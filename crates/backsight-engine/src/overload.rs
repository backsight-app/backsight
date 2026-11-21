//! Spots a video encoder that can't keep up with real time, and offers one lighter setting.
//!
//! Pure logic: the shell feeds [`OverloadWatch`] the session's counters once a second.

use crate::session::output_size;
use backsight_config::Video;
use backsight_encode::catalog::{Catalog, OptionKind};

/// Running totals from `ReplaySession::diagnostics`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sample {
    pub encoded: u64,
    pub late: u64,
    pub skipped: u64,
}

/// Seconds in a row with many late frames before warning, and with almost none before the
/// warning clears. The gap between the two (hysteresis) keeps it from flapping.
const BAD_SECONDS: u32 = 5;
const GOOD_SECONDS: u32 = 10;

#[derive(Debug, Default)]
pub struct OverloadWatch {
    last: Option<Sample>,
    bad: u32,
    good: u32,
    overloaded: bool,
    /// The pop-up was shown for this session; later swings only update Home.
    told: bool,
    /// A lighter setting was picked: quiet until the session restarts with it.
    waiting: bool,
}

impl OverloadWatch {
    /// For a `static`.
    pub const fn new() -> Self {
        Self { last: None, bad: 0, good: 0, overloaded: false, told: false, waiting: false }
    }

    /// One reading a second; `None` while no session runs. Returns the new state when it changes.
    pub fn sample(&mut self, now: Option<Sample>) -> Option<bool> {
        let was = self.overloaded;
        match (now, self.last) {
            // Counters only grow within a session: a smaller one is a new session.
            (Some(s), Some(last)) if self.waiting && s.encoded >= last.encoded => self.last = Some(s),
            (Some(s), Some(last)) if s.encoded >= last.encoded => {
                let skipped = s.skipped.saturating_sub(last.skipped);
                let behind = s.late.saturating_sub(last.late) + skipped;
                let slots = s.encoded - last.encoded + skipped;
                // At least 10% of a second's frames late: behind. Under 2%: fine.
                if slots >= 10 && behind * 10 >= slots {
                    (self.bad, self.good) = (self.bad + 1, 0);
                } else if behind * 50 <= slots {
                    (self.bad, self.good) = (0, self.good + 1);
                } else {
                    (self.bad, self.good) = (0, 0);
                }
                if self.bad >= BAD_SECONDS {
                    self.overloaded = true;
                } else if self.good >= GOOD_SECONDS {
                    self.overloaded = false;
                }
                self.last = Some(s);
            }
            (s, _) => *self = Self { last: s, ..Self::default() },
        }
        (self.overloaded != was).then_some(self.overloaded)
    }

    pub fn overloaded(&self) -> bool {
        self.overloaded
    }

    /// `true` the first time it's asked in a session: the pop-up shows once per session.
    pub fn first_warning(&mut self) -> bool {
        !std::mem::replace(&mut self.told, true)
    }

    /// A lighter setting was picked: clear the warning and stay quiet until a new session starts
    /// (while recording, the change waits for the recording to stop).
    pub fn wait_for_new_session(&mut self) {
        (self.overloaded, self.bad, self.good, self.waiting) = (false, 0, 0, true);
    }
}

/// When a lighter setting that is due may be applied: applying restarts the session, which
/// empties Instant Replay, so never mid-recording and only once what it holds is safe on disk: a
/// clip was saved or a recording stopped. (Not "the game closed": the game watched is the program
/// in front, which a look at another window changes too.)
#[derive(Debug, Default)]
pub struct SafeMoment {
    /// Clips saved and the video settings that fell behind, when it became due.
    due: Option<(u64, Video)>,
    recorded: bool,
}

impl SafeMoment {
    /// For a `static`.
    pub const fn new() -> Self {
        Self { due: None, recorded: false }
    }

    /// A lighter setting than `video` is due; `clips` counts clips saved so far.
    pub fn due(&mut self, clips: u64, video: Video) {
        if self.due.is_none() {
            (self.due, self.recorded) = (Some((clips, video)), false);
        }
    }

    /// Nothing is due any more (the user picked a setting).
    pub fn cancel(&mut self) {
        self.due = None;
    }

    /// One reading a second: at the safe moment, once, the settings that fell behind (lighten
    /// them only if they are still the ones in use).
    pub fn tick(&mut self, recording: bool, clips: u64) -> Option<Video> {
        let (at, _) = self.due.as_ref()?;
        if recording {
            self.recorded = true;
            return None;
        }
        (clips > *at || self.recorded).then(|| self.due.take().expect("due").1)
    }
}

/// The warning for an encoder that falls behind, in plain words.
pub fn message(encoder_id: &str) -> String {
    let hardware = Catalog::get().video_encoder(encoder_id).is_some_and(|e| e.hardware);
    if hardware {
        "Your graphics card can't keep up. Pick a faster preset, a lower resolution or a lower frame rate.".into()
    } else {
        "Your processor can't keep up. Pick a faster preset or switch to your graphics card.".into()
    }
}

/// `video` one step lighter, and what changed in plain words; `None` when nothing is left to lower.
/// In order: a working hardware encoder instead of the CPU, a faster preset, a lower resolution,
/// a lower frame rate. `working_hardware` lists hardware encoder ids that passed the probe;
/// `source` is the captured monitor's size (a resolution that wouldn't shrink it is skipped).
pub fn lighter(video: &Video, working_hardware: &[String], source: (u32, u32)) -> Option<(Video, String)> {
    let c = Catalog::get();
    let venc = c.video_encoder(&video.encoder)?;
    let mut v = video.clone();

    if !venc.hardware {
        let holds =
            |codec: &str| c.container(&video.container).is_some_and(|k| k.video_codecs.iter().any(|x| x == codec));
        let working: Vec<_> = c.video_encoders.iter().filter(|e| working_hardware.contains(&e.id)).collect();
        let pick = working.iter().find(|e| e.codec == venc.codec).or_else(|| working.iter().find(|e| holds(&e.codec)));
        if let Some(e) = pick {
            v.encoder = e.id.clone();
            return Some((v, format!("Switched to {}, so your graphics card does the work.", e.label)));
        }
    }

    // Every speed option is explained by the "preset" entry (AMF calls it `quality`). Choices are
    // listed fastest first; numeric speeds (SVT-AV1, AOM) are faster when higher.
    let options = c.video_options(venc);
    if let Some(opt) = options.iter().find(|o| o.glossary == "preset") {
        let saved = video.encoder_options.get(&venc.id).and_then(|s| s.get(&opt.id));
        let current = saved.unwrap_or(&opt.default);
        let faster = match opt.kind {
            OptionKind::Select => opt
                .choices
                .iter()
                .position(|ch| &ch.value == current)
                .and_then(|i| i.checked_sub(1))
                .map(|i| (opt.choices[i].value.clone(), opt.choices[i].label.clone())),
            OptionKind::Number => current.as_integer().zip(opt.max).and_then(|(now, max)| {
                let n = (now + 2).min(max as i64);
                (n > now).then(|| (toml::Value::Integer(n), n.to_string()))
            }),
            OptionKind::Toggle => None,
        };
        if let Some((value, label)) = faster {
            v.encoder_options.entry(venc.id.clone()).or_default().insert(opt.id.clone(), value);
            return Some((v, format!("{} is now {label}.", opt.label)));
        }
    }

    // Never upscaled: on a 1080p screen "source" and 1440p both record 1080p, so the next real step is 720p.
    let pixels = |r: &str| {
        let (w, h) = output_size(source, r);
        u64::from(w) * u64::from(h)
    };
    let now = pixels(&video.resolution);
    let lower = c.resolutions.iter().filter_map(|ch| ch.value.as_str()).find(|r| pixels(r) < now);
    if let Some(r) = lower {
        v.resolution = r.into();
        return Some((v, format!("Recording at {r} now.")));
    }

    let fps =
        c.frame_rates.iter().filter_map(|ch| ch.value.as_integer()).filter(|&f| f < i64::from(video.fps)).max()?;
    v.fps = fps as u32;
    Some((v, format!("Recording at {fps} FPS now.")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use backsight_config::Settings;

    #[test]
    fn a_lighter_setting_waits_for_a_safe_moment() {
        let video = Settings::default().video;
        let mut m = SafeMoment::new();
        assert_eq!(m.tick(false, 0), None, "nothing due");
        // Not while playing on, nor while recording; after the recording stops, yes, once.
        m.due(3, video.clone());
        assert_eq!(m.tick(false, 3), None);
        assert_eq!(m.tick(true, 3), None, "never mid-recording");
        assert_eq!(m.tick(false, 3), Some(video.clone()), "the recording stopped");
        assert_eq!(m.tick(false, 3), None, "once");
        // After the next clip; a clip saved while recording still waits for the stop.
        m.due(3, video.clone());
        assert!(m.tick(false, 4).is_some());
        m.due(4, video.clone());
        assert_eq!(m.tick(true, 5), None);
        assert!(m.tick(false, 5).is_some());
        // The first settings that fell behind are the ones asked about; a cancel clears it.
        let mut other = video.clone();
        other.fps = 30;
        m.due(5, video.clone());
        m.due(5, other);
        assert_eq!(m.tick(false, 6), Some(video.clone()));
        m.due(6, video);
        m.cancel();
        assert_eq!(m.tick(false, 9), None);
    }

    fn feed(w: &mut OverloadWatch, late_per_second: u64, seconds: u32, at: &mut Sample) -> Vec<bool> {
        (0..seconds)
            .filter_map(|_| {
                at.encoded += 60;
                at.late += late_per_second;
                w.sample(Some(*at))
            })
            .collect()
    }

    #[test]
    fn warns_after_five_bad_seconds_and_clears_after_ten_good_ones() {
        let mut w = OverloadWatch::default();
        let mut at = Sample::default();
        assert_eq!(w.sample(Some(at)), None, "the first reading only sets the baseline");
        assert_eq!(feed(&mut w, 30, 4, &mut at), [] as [bool; 0], "four bad seconds: not yet");
        assert_eq!(feed(&mut w, 30, 1, &mut at), [true]);
        assert_eq!(feed(&mut w, 0, 9, &mut at), [] as [bool; 0], "nine good seconds: still warning");
        assert_eq!(feed(&mut w, 0, 1, &mut at), [false]);
    }

    #[test]
    fn short_spikes_and_borderline_seconds_do_not_flap() {
        let mut w = OverloadWatch::default();
        let mut at = Sample::default();
        w.sample(Some(at));
        for _ in 0..10 {
            // A loading screen: two bad seconds, then fine.
            assert!(feed(&mut w, 40, 2, &mut at).is_empty());
            assert!(feed(&mut w, 0, 3, &mut at).is_empty());
        }
        feed(&mut w, 30, 5, &mut at);
        assert!(w.overloaded());
        // Between 2% and 10% late keeps the warning: it isn't fixed yet.
        assert!(feed(&mut w, 3, 30, &mut at).is_empty());
        assert!(w.overloaded());
    }

    #[test]
    fn skipped_frames_count_and_a_new_session_starts_over() {
        let mut w = OverloadWatch::default();
        let mut at = Sample::default();
        w.sample(Some(at));
        for _ in 0..5 {
            // A stalled encoder: few frames out, a second's worth skipped.
            at.encoded += 20;
            at.skipped += 60;
            w.sample(Some(at));
        }
        assert!(w.overloaded());
        assert_eq!(w.sample(Some(Sample::default())), Some(false), "restarted with new settings");
        assert_eq!(w.sample(None), None);
    }

    #[test]
    fn idle_seconds_are_not_overload() {
        let mut w = OverloadWatch::default();
        let mut at = Sample::default();
        w.sample(Some(at));
        for _ in 0..10 {
            at.late += 1; // one late frame of one: too few frames to judge
            at.encoded += 1;
            assert_eq!(w.sample(Some(at)), None);
        }
    }

    #[test]
    fn the_pop_up_shows_once_per_session_and_a_lighter_pick_waits_for_the_restart() {
        let mut w = OverloadWatch::default();
        let mut at = Sample::default();
        w.sample(Some(at));
        assert_eq!(feed(&mut w, 30, 5, &mut at), [true]);
        assert!(w.first_warning());
        feed(&mut w, 0, 10, &mut at);
        assert_eq!(feed(&mut w, 30, 5, &mut at), [true], "a heavy scene again: Home shows it");
        assert!(!w.first_warning(), "but no second pop-up");
        w.wait_for_new_session();
        assert!(!w.overloaded());
        assert!(feed(&mut w, 30, 20, &mut at).is_empty(), "the old session is still slow: quiet");
        w.sample(Some(Sample::default())); // restarted with the lighter setting
        let mut at = Sample::default();
        assert_eq!(feed(&mut w, 30, 5, &mut at), [true], "still too slow: warns again");
        assert!(w.first_warning(), "new session, new pop-up");
    }

    fn video(encoder: &str) -> Video {
        Video { encoder: encoder.into(), ..Settings::default().video }
    }

    const FULL_HD: (u32, u32) = (1920, 1080);

    #[test]
    fn lighter_moves_a_cpu_encoder_to_a_working_graphics_card() {
        let (v, said) = lighter(&video("x264"), &["nvenc-hevc".into(), "nvenc-h264".into()], FULL_HD).unwrap();
        assert_eq!(v.encoder, "nvenc-h264", "same codec first");
        assert!(said.contains("NVIDIA"), "{said}");
        let (v, _) = lighter(&video("x265"), &["nvenc-h264".into(), "nvenc-hevc".into()], FULL_HD).unwrap();
        assert_eq!(v.encoder, "nvenc-hevc");
        // FLV holds only H.264: an HEVC card can't take over.
        let flv = Video { container: "flv".into(), ..video("x264") };
        assert_eq!(lighter(&flv, &["nvenc-hevc".into()], FULL_HD).unwrap().0.encoder, "x264");
    }

    #[test]
    fn lighter_steps_preset_then_resolution_then_frame_rate() {
        let (v, said) = lighter(&video("x264"), &[], FULL_HD).unwrap();
        assert_eq!(v.encoder_options["x264"]["preset"].as_str(), Some("superfast"), "{said}");
        let mut v = video("x264");
        v.encoder_options.entry("x264".into()).or_default().insert("preset".into(), "ultrafast".into());
        let (v, _) = lighter(&v, &[], FULL_HD).unwrap();
        assert_eq!(v.resolution, "720p");
        let (v, said) = lighter(&v, &[], FULL_HD).unwrap();
        assert_eq!((v.fps, said.as_str()), (30, "Recording at 30 FPS now."));
        assert!(lighter(&v, &[], FULL_HD).is_none(), "nothing left to lower");
    }

    #[test]
    fn lighter_skips_resolutions_that_would_not_shrink_the_picture() {
        let mut v = video("x264");
        v.encoder_options.entry("x264".into()).or_default().insert("preset".into(), "ultrafast".into());
        v.resolution = "source".into();
        assert_eq!(lighter(&v, &[], FULL_HD).unwrap().0.resolution, "720p", "1080p would change nothing");
        assert_eq!(lighter(&v, &[], (2560, 1440)).unwrap().0.resolution, "1080p");
    }

    #[test]
    fn lighter_speeds_up_numeric_presets_and_hardware_presets() {
        let (v, _) = lighter(&video("svt-av1"), &[], FULL_HD).unwrap();
        let opt = Catalog::get().video_options(Catalog::get().video_encoder("svt-av1").unwrap());
        let default = opt.iter().find(|o| o.id == "preset").unwrap().default.as_integer().unwrap();
        assert_eq!(v.encoder_options["svt-av1"]["preset"].as_integer(), Some(default + 2));
        let max = opt.iter().find(|o| o.id == "preset").unwrap().max.unwrap() as i64;
        let mut v = video("svt-av1");
        v.encoder_options.entry("svt-av1".into()).or_default().insert("preset".into(), max.into());
        assert_eq!(lighter(&v, &[], FULL_HD).unwrap().0.resolution, "720p", "already the fastest");
        let mut v = video("nvenc-h264");
        v.encoder_options.entry("nvenc-h264".into()).or_default().insert("preset".into(), "p4".into());
        let (v, _) = lighter(&v, &[], FULL_HD).unwrap();
        assert_eq!(v.encoder_options["nvenc-h264"]["preset"].as_str(), Some("p3"));
        // P1, the default, is already the fastest: the resolution goes down instead.
        let (v, _) = lighter(&video("nvenc-h264"), &[], FULL_HD).unwrap();
        assert_eq!(v.resolution, "720p");
        let mut v = video("amf-h264");
        v.encoder_options.entry("amf-h264".into()).or_default().insert("quality".into(), "balanced".into());
        let (v, said) = lighter(&v, &[], FULL_HD).unwrap();
        assert_eq!(v.encoder_options["amf-h264"]["quality"].as_str(), Some("speed"), "{said}");
        // AMD and Intel default to their fastest too: the resolution goes down instead.
        assert_eq!(lighter(&video("amf-h264"), &[], FULL_HD).unwrap().0.resolution, "720p");
        assert_eq!(lighter(&video("qsv-h264"), &[], FULL_HD).unwrap().0.resolution, "720p");
    }

    #[test]
    fn the_message_names_what_is_too_slow() {
        assert!(message("x264").contains("processor"));
        assert!(message("nvenc-h264").contains("graphics card"));
    }
}
