//! "Tune for this PC": how fast the chosen video encoder runs here at the recording's size and
//! frame rate, and the settings, stepped down as [`overload::lighter`] does, that leave a game room.
//!
//! The speed test runs in a separate process (`backsight.exe --tune-encoder`), like the encoder
//! check, so a graphics driver that never returns can't freeze Backsight.

use crate::{capture::d3d, overload, session::output_size, setup::ProbeRun};
use backsight_config::Video;
use backsight_encode::{
    catalog::{Catalog, resolve},
    session::VideoSession,
};
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::Path,
    time::{Duration, Instant},
};

/// The encoder must run this many times faster than the recording's frame rate when nothing else
/// uses it: a game shares the graphics card (or the processor), and replay must never fall behind.
pub const HEADROOM: f64 = 2.0;
/// A guard only: `lighter` always runs out (NVENC p7, 2160p120 on a 4K screen takes 6 presets, 3
/// resolutions and a few frame rates).
const MAX_STEPS: usize = 32;
/// Changed whenever the test itself changes, so earlier answers aren't reused.
const TEST_VERSION: u32 = 2;
/// How long one speed test may take before the encoder counts as stuck (opening NVENC can take
/// seconds on a laptop whose NVIDIA card is asleep).
pub const TEST_LIMIT: Duration = Duration::from_secs(30);
/// Frames encoded before timing starts (the first ones are slow), and the timed part's limits.
const WARM_UP: i64 = 10;
const TIMED_FRAMES: i64 = 240;
const TIMED_FOR: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tuned {
    /// The settings to record with: the ones given, or lighter ones.
    pub video: Video,
    /// Frames per second the encoder managed with `video`, alone.
    pub speed: f64,
    /// What was changed, in plain words, in order; empty when the given settings had room.
    pub changes: Vec<String>,
    /// `false` when even the lightest settings leave less than [`HEADROOM`].
    pub enough: bool,
}

/// Steps `video` down until the encoder has [`HEADROOM`]. `measure` gives frames per second for
/// some settings at an output size; `source` is the recorded monitor's size; `working_hardware`
/// lists hardware encoders that passed the encoder check (a CPU encoder may move to one).
pub fn pick(
    video: &Video,
    working_hardware: &[String],
    source: (u32, u32),
    mut measure: impl FnMut(&Video, (u32, u32)) -> Result<f64, String>,
) -> Result<Tuned, String> {
    // Several steps of one setting (preset P3, P2, P1) are told as where it ended up.
    fn setting(change: &str) -> &str {
        match change.find(" is now ") {
            Some(i) => &change[..i],
            None if change.ends_with(" FPS now.") => "FPS",
            None => change.split(' ').next().unwrap_or(change),
        }
    }
    let (mut video, mut changes, mut steps) = (video.clone(), Vec::<String>::new(), 0);
    loop {
        let speed = measure(&video, output_size(source, &video.resolution))?;
        let enough = speed >= f64::from(video.fps) * HEADROOM;
        let lighter =
            if enough || steps >= MAX_STEPS { None } else { overload::lighter(&video, working_hardware, source) };
        match lighter {
            Some((next, change)) => {
                steps += 1;
                changes.retain(|c| setting(c) != setting(&change));
                changes.push(change);
                video = next;
            }
            None => return Ok(Tuned { video, speed, changes, enough }),
        }
    }
}

/// [`pick`], remembered per graphics driver, monitor size and settings: the same question gets the
/// same answer until a driver update (or a settings change) asks it again. Not remembered when the
/// driver versions can't be read (an update could never ask again).
pub fn pick_cached(
    cache: &Path,
    video: &Video,
    working_hardware: &[String],
    source: (u32, u32),
    measure: impl FnMut(&Video, (u32, u32)) -> Result<f64, String>,
) -> Result<Tuned, String> {
    let drivers = d3d::drivers_key().unwrap_or_default();
    if drivers.is_empty() {
        return pick(video, working_hardware, source, measure);
    }
    pick_cached_for(cache, &drivers, video, working_hardware, source, measure)
}

fn pick_cached_for(
    cache: &Path,
    drivers: &str,
    video: &Video,
    working_hardware: &[String],
    source: (u32, u32),
    measure: impl FnMut(&Video, (u32, u32)) -> Result<f64, String>,
) -> Result<Tuned, String> {
    // The encoder's options as they resolve now: an update may change a default.
    let c = Catalog::get();
    let options =
        c.video_encoder(&video.encoder).map(|e| resolve(&c.video_options(e), video.encoder_options.get(&e.id)));
    let key = format!(
        "{TEST_VERSION}|{drivers}|{source:?}|{working_hardware:?}|{options:?}|{}",
        serde_json::to_string(video).expect("plain data")
    );
    #[derive(Serialize, Deserialize)]
    struct Saved {
        key: String,
        tuned: Tuned,
    }
    if let Ok(saved) = fs::read(cache)
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice::<Saved>(&b).map_err(|e| e.to_string()))
        && saved.key == key
    {
        return Ok(saved.tuned);
    }
    let tuned = pick(video, working_hardware, source, measure)?;
    let saved = serde_json::to_vec(&Saved { key, tuned: tuned.clone() }).expect("plain data");
    if let Err(e) = fs::write(cache, saved) {
        tracing::warn!("tune result not cached: {e}");
    }
    Ok(tuned)
}

/// The last saved answer ended at `video` with room to spare, but on other graphics drivers than
/// now: after a driver update, the test should run again. `false` with no saved answer (setup runs
/// it), unreadable drivers, or an answer about other settings (the user's own choice since then,
/// which a retest must not undo).
pub fn drivers_changed(cache: &Path, video: &Video) -> bool {
    drivers_changed_for(cache, &d3d::drivers_key().unwrap_or_default(), video)
}

fn drivers_changed_for(cache: &Path, now: &str, video: &Video) -> bool {
    #[derive(Deserialize)]
    struct Saved {
        key: String,
        tuned: Tuned,
    }
    let saved = fs::read(cache).ok().and_then(|b| serde_json::from_slice::<Saved>(&b).ok());
    // The key starts "<test version>|<drivers>|".
    !now.is_empty()
        && saved.is_some_and(|s| {
            s.tuned.enough && s.tuned.video == *video && s.key.split('|').nth(1).is_some_and(|d| d != now)
        })
}

/// One answer from the test process.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Answer {
    Speed(f64),
    Error(String),
}

/// Frames per second for `video` at `size`, measured in a test process `spawn` starts with the
/// arguments [`child`] reads.
pub fn measure_in_child(
    video: &Video,
    size: (u32, u32),
    spawn: impl FnOnce(&[String]) -> io::Result<ProbeRun>,
) -> Result<f64, String> {
    measure_within(video, size, spawn, TEST_LIMIT)
}

fn measure_within(
    video: &Video,
    size: (u32, u32),
    spawn: impl FnOnce(&[String]) -> io::Result<ProbeRun>,
    limit: Duration,
) -> Result<f64, String> {
    use std::sync::mpsc::RecvTimeoutError;
    let args = [serde_json::to_string(video).expect("plain data"), size.0.to_string(), size.1.to_string()];
    let run = spawn(&args).map_err(|e| format!("couldn't start the speed test: {e}"))?;
    let deadline = Instant::now() + limit;
    loop {
        // `run` drops on return, which kills a test process that is still going.
        let line = run.lines.recv_timeout(deadline.saturating_duration_since(Instant::now())).map_err(|e| match e {
            RecvTimeoutError::Timeout => {
                format!("{} froze in the speed test: no answer in {} s", video.encoder, limit.as_secs_f32())
            }
            RecvTimeoutError::Disconnected => format!("{} crashed the speed test", video.encoder),
        })?;
        match serde_json::from_str::<Answer>(&line) {
            Ok(Answer::Speed(fps)) => return Ok(fps),
            Ok(Answer::Error(e)) => return Err(e),
            Err(_) => tracing::debug!("speed test: ignored {line:?}"),
        }
    }
}

/// The test process's work (`backsight.exe --tune-encoder <video json> <width> <height>`): one
/// JSON line, the speed or why there is none.
pub fn child(args: &[String], mut out: impl io::Write) -> io::Result<()> {
    let answer = match args {
        [video, w, h] => match (serde_json::from_str::<Video>(video), w.parse(), h.parse()) {
            (Ok(video), Ok(w), Ok(h)) => measure(&video, (w, h)).map_or_else(Answer::Error, Answer::Speed),
            _ => Answer::Error("bad speed test arguments".into()),
        },
        _ => Answer::Error("bad speed test arguments".into()),
    };
    writeln!(out, "{}", serde_json::to_string(&answer).expect("plain data"))?;
    out.flush()
}

/// Encodes moving test frames with `video`'s encoder and options at `size`, as fast as it goes,
/// and returns frames per second. The frames are drawn first, and the clock runs throughout:
/// encoders that work on other threads (x264, NVENC) would otherwise work for free meanwhile.
// shortcut: frames come from system memory, so hardware encoders also pay an upload the real
// zero-copy session doesn't (a slightly low answer); SDR only. Feed D3D11 frames if a card is
// stepped down that keeps up in real recordings.
pub fn measure(video: &Video, size: (u32, u32)) -> Result<f64, String> {
    let c = Catalog::get();
    let venc = c.video_encoder(&video.encoder).ok_or("unknown encoder")?;
    let values = resolve(&c.video_options(venc), video.encoder_options.get(&venc.id));
    let mut session = VideoSession::open(venc, &values, size.0, size.1, video.fps, true)?;
    let frames: Vec<[Vec<u8>; 3]> = (0..FRAMES).map(|t| draw(&mut session, t)).collect::<Result<_, _>>()?;
    let mut packets = 0u64;
    let mut send = |session: &mut VideoSession, pts: i64| -> Result<(), String> {
        for (plane, drawn) in session.planes_mut()?.into_iter().zip(&frames[pts as usize % frames.len()]) {
            if let Some((data, _)) = plane {
                data.copy_from_slice(drawn);
            }
        }
        session.send(pts, &mut |_| packets += 1)
    };
    for pts in 0..WARM_UP {
        send(&mut session, pts)?;
    }
    let began = Instant::now();
    let mut pts = WARM_UP;
    while pts < WARM_UP + TIMED_FRAMES && began.elapsed() < TIMED_FOR {
        send(&mut session, pts)?;
        pts += 1;
    }
    session.finish(&mut |_| packets += 1)?;
    let secs = began.elapsed().as_secs_f64().max(1e-6);
    if packets == 0 {
        return Err("the encoder produced nothing".into());
    }
    Ok((pts - WARM_UP) as f64 / secs)
}

/// Frames drawn before the test, played in a loop.
const FRAMES: i64 = 24;

/// Frame `t`'s planes: a detailed textured scene panning diagonally, like a game camera moving,
/// with a small part that changes completely each frame (effects, a minimap). The texture moves
/// with the scene, so encoders find it in earlier frames as they do in real footage.
// shortcut: a stand-in for game footage, not checked against a real recording's encode speed;
// compare with a real game's late frames if a PC is stepped down that copes in practice.
fn draw(session: &mut VideoSession, t: i64) -> Result<[Vec<u8>; 3], String> {
    let t = t as usize;
    // Fine detail fixed to the scene: a hash of the scene position, 0-15.
    let texture = |u: usize, v: usize| ((u.wrapping_mul(73_856_093) ^ v.wrapping_mul(19_349_663)) >> 7 & 15) as u8;
    Ok(session.planes_mut()?.map(|plane| {
        let Some((data, stride)) = plane else { return Vec::new() };
        let rows = data.len() / stride;
        let mut out = vec![0; data.len()];
        for (i, px) in out.iter_mut().enumerate() {
            let (x, y) = (i % stride, i / stride);
            let (u, v) = (x + t * 6, y + t * 3);
            // The bottom-right sixteenth changes every frame.
            let (u, v) = if x * 4 > stride * 3 && y * 4 > rows * 3 { (u * 31 + t * 977, v * 17) } else { (u, v) };
            *px = (((u / 24 + v / 16) & 7) as u8 * 24 + 40).wrapping_add(texture(u, v));
        }
        out
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use backsight_config::Settings;

    fn video() -> Video {
        let mut v = Settings::default().video;
        (v.encoder, v.resolution, v.fps) = ("x264".into(), "1080p".into(), 60);
        v
    }

    #[test]
    fn settings_with_room_are_kept() {
        let tuned = pick(&video(), &[], (1920, 1080), |_, _| Ok(500.0)).unwrap();
        assert_eq!((tuned.video, tuned.changes.len(), tuned.enough), (video(), 0, true));
    }

    #[test]
    fn a_slow_encoder_steps_down_until_it_has_room() {
        // Too slow at first (60 FPS needs 120), fast enough after one step.
        let mut sizes = Vec::new();
        let tuned = pick(&video(), &[], (2560, 1440), |_, size| {
            sizes.push(size);
            Ok(if sizes.len() == 1 { 90.0 } else { 200.0 })
        })
        .unwrap();
        assert!(tuned.enough);
        assert_eq!(tuned.changes.len(), 1, "{:?}", tuned.changes);
        assert_ne!(tuned.video, video());
        assert_eq!(sizes[0], (1920, 1080), "measured at the recorded size, not the monitor's");
    }

    #[test]
    fn a_cpu_encoder_moves_to_a_working_graphics_card_first() {
        let tuned = pick(&video(), &["nvenc-h264".into()], (1920, 1080), |v, _| {
            Ok(if v.encoder == "x264" { 40.0 } else { 900.0 })
        })
        .unwrap();
        assert_eq!(tuned.video.encoder, "nvenc-h264");
        assert!(tuned.enough && tuned.changes[0].contains("graphics card"), "{:?}", tuned.changes);
    }

    #[test]
    fn nothing_light_enough_says_so_and_ends_at_the_lightest() {
        let tuned = pick(&video(), &[], (1920, 1080), |_, _| Ok(10.0)).unwrap();
        assert!(!tuned.enough);
        assert!(tuned.changes.len() >= 3, "preset, resolution and frame rate: {:?}", tuned.changes);
        assert_eq!(
            tuned.changes.iter().filter(|c| c.contains(" is now ")).count(),
            1,
            "one line per setting: {:?}",
            tuned.changes
        );
        assert_eq!(tuned.changes.iter().filter(|c| c.ends_with(" FPS now.")).count(), 1, "{:?}", tuned.changes);
        assert!(overload::lighter(&tuned.video, &[], (1920, 1080)).is_none(), "every step was taken");
    }

    #[test]
    fn a_failed_test_on_a_step_gives_no_answer() {
        let mut calls = 0;
        let r = pick(&video(), &[], (1920, 1080), |_, _| {
            calls += 1;
            if calls == 1 { Ok(10.0) } else { Err("stuck".into()) }
        });
        assert_eq!(r, Err("stuck".into()), "no half-tuned settings");
    }

    fn lines(script: &[&str]) -> std::sync::mpsc::Receiver<String> {
        let (tx, rx) = std::sync::mpsc::channel();
        for line in script {
            tx.send((*line).into()).unwrap();
        }
        if !script.contains(&"hang") {
            drop(tx); // the process ended
        } else {
            std::mem::forget(tx);
        }
        rx
    }

    #[test]
    fn the_test_process_answer_is_read_past_other_output() {
        let run = |script: &[&str]| {
            let rx = lines(script);
            measure_within(&video(), (320, 180), |_| Ok(ProbeRun::fake(rx)), Duration::from_millis(100))
        };
        assert_eq!(run(&["noise", r#"{"speed":250.0}"#]), Ok(250.0));
        assert_eq!(run(&[r#"{"error":"open failed"}"#]), Err("open failed".into()));
        assert!(run(&["noise"]).unwrap_err().contains("crashed"));
        assert!(run(&["hang"]).unwrap_err().contains("froze"));
    }

    #[test]
    fn the_answer_is_cached_until_something_changes() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("tune.json");
        let mut calls = 0;
        let mut ask = |drivers: &str, v: &Video| {
            pick_cached_for(&cache, drivers, v, &[], (1920, 1080), |_, _| {
                calls += 1;
                Ok(500.0)
            })
            .unwrap()
        };
        ask("10de:1", &video());
        ask("10de:1", &video());
        let mut other = video();
        other.fps = 30;
        ask("10de:1", &other);
        ask("10de:2", &other);
        assert_eq!(calls, 3, "the same question again comes from the cache; new settings or a driver update ask again");
        // The last answer: `other`, with room (500 frames/s).
        assert!(!drivers_changed_for(&cache, "10de:2", &other), "answered on these drivers");
        assert!(drivers_changed_for(&cache, "10de:3", &other), "a driver update");
        assert!(!drivers_changed_for(&cache, "10de:3", &video()), "about other settings: the user's choice");
        assert!(!drivers_changed_for(&cache, "", &other), "unreadable drivers ask nothing");
        assert!(!drivers_changed_for(&dir.path().join("none.json"), "10de:3", &other), "no answer yet: setup asks");
        pick_cached_for(&cache, "10de:4", &other, &[], (1920, 1080), |_, _| Ok(10.0)).unwrap();
        let slow = |_: &Video, _| Ok(10.0);
        let lightest = pick(&other, &[], (1920, 1080), slow).unwrap().video;
        assert!(!drivers_changed_for(&cache, "10de:3", &lightest), "no room then: nothing to keep");
    }

    /// This PC's speeds at 1080p60, with --nocapture.
    #[test]
    #[ignore = "needs the encoders; takes a few seconds each"]
    fn measures_this_pcs_encoders() {
        for id in ["nvenc-h264", "nvenc-hevc", "nvenc-av1", "qsv-h264", "x264"] {
            let mut v = video();
            v.encoder = id.into();
            println!("{id}: {:?} frames/s", measure(&v, (1920, 1080)));
        }
    }

    #[test]
    fn the_test_process_answers_one_line() {
        let mut out = Vec::new();
        child(&["{}".into()], &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().starts_with(r#"{"error""#));
        let mut out = Vec::new();
        let args = [serde_json::to_string(&video()).unwrap(), "320".into(), "180".into()];
        child(&args, &mut out).unwrap();
        let answer: Answer = serde_json::from_slice(&out).unwrap();
        assert!(matches!(answer, Answer::Speed(fps) if fps > 0.0), "{answer:?}");
    }
}
