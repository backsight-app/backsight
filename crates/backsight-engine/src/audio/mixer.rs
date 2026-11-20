//! Mixes the audio sources (desktop or game audio, the microphone) into the recorded tracks on
//! the shared QPC sample grid: frame `n` of every track plays at `start + n / 48000`, the same
//! grid the sources' [`Timeline`](super::timeline::Timeline)s keep and the encoder assumes.
//!
//! Sources push 48 kHz float stereo chunks stamped with their QPC time; [`Mixer::mix_to`]
//! emits the mixed track (and, with separate tracks, one track per source) up to the point
//! every source has delivered, but never later than [`LATENCY_MS`] behind `now`: a source that
//! is late or has stopped contributes silence there instead of holding up the recording.
//!
//! Each source has a gain gate for mute and push-to-talk, ramped linearly over [`RAMP_FRAMES`]
//! so switching never clicks. Mute and key events carry QPC times, so they land on the exact
//! sample even though mixing runs [`LATENCY_MS`] behind.
//!
//! Pure and allocation-free after [`Mixer::new`] (hard rule 8): every buffer is sized up front
//! and never grows. Times are in 100 ns QPC units.

use super::wasapi::{CHANNELS, RATE};
use crate::capture::wgc::CLOCK_HZ;
use std::collections::VecDeque;

const CH: usize = CHANNELS as usize;
/// A source may lag this far behind `now` before its missing span is mixed as silence. Above
/// the timeline's 100 ms fill slack, so a quiet loopback source's silence fill still makes it.
pub const LATENCY_MS: u64 = 200;
/// Mute and push-to-talk fade over 10 ms.
pub const RAMP_FRAMES: u64 = RATE as u64 / 100;
/// After the push-to-talk key is released the mic stays open 150 ms, so word endings aren't cut.
pub const PTT_TAIL_FRAMES: u64 = RATE as u64 * 150 / 1000;
/// Audio buffered per source, ahead of what has been mixed. Also the stall limit: a longer gap
/// to mix in one go (the PC slept) jumps forward instead of emitting the silence.
// shortcut: anything pushed more than 1 s past the mixed point is dropped (only after the caller
// stalled); raise it, or mix on a timer, if a stalled encoder thread ever loses audio.
const PENDING_FRAMES: usize = RATE as usize;
/// Frames emitted per `out` call.
pub(crate) const CHUNK_FRAMES: usize = 1024;
/// A chunk landing within this many frames of where the source's last chunk ended is joined
/// on: rounding each chunk's time to the grid must not open one-sample gaps.
const SNAP_FRAMES: i64 = 2;
/// Mute or key events waiting to be reached by the mix. When full, the oldest applies early.
const EVENTS: usize = 32;

/// Per-source settings fixed for the recording (changing push-to-talk restarts it).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SourceOptions {
    /// Starts muted.
    pub muted: bool,
    /// Only heard while the push-to-talk key is held, plus [`PTT_TAIL_FRAMES`].
    pub push_to_talk: bool,
}

/// Which track an emitted chunk belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Track {
    /// Every source mixed: the default track, and track 1 with separate tracks.
    Mix,
    /// One source on its own (by index in [`Mixer::new`]'s list), with separate tracks.
    Source(usize),
}

#[derive(Debug, Clone, Copy)]
enum Event {
    Mute(bool),
    Key(bool),
}

/// A source's gain over time: 0 or 1, ramped.
#[derive(Debug)]
struct Gate {
    gain: f32,
    muted: bool,
    push_to_talk: bool,
    held: bool,
    /// With push-to-talk and the key up, the mic is open until this frame.
    open_until: i64,
    events: VecDeque<(i64, Event)>,
}

impl Gate {
    fn new(o: SourceOptions) -> Self {
        let gain = if o.muted || o.push_to_talk { 0.0 } else { 1.0 };
        let events = VecDeque::with_capacity(EVENTS);
        Gate { gain, muted: o.muted, push_to_talk: o.push_to_talk, held: false, open_until: 0, events }
    }

    fn queue(&mut self, frame: i64, e: Event) {
        if self.events.len() == EVENTS {
            let (f, e) = self.events.pop_front().unwrap();
            self.apply(f, e);
        }
        self.events.push_back((frame, e));
    }

    fn apply(&mut self, frame: i64, e: Event) {
        match e {
            Event::Mute(m) => self.muted = m,
            Event::Key(down) => {
                // A release keeps the mic open for the tail; a press during it just continues.
                if self.held && !down {
                    self.open_until = frame + PTT_TAIL_FRAMES as i64;
                }
                self.held = down;
            }
        }
    }

    fn target(&mut self, frame: i64) -> f32 {
        while let Some(&(f, e)) = self.events.front().filter(|(f, _)| *f <= frame) {
            self.events.pop_front();
            self.apply(f, e);
        }
        let closed = self.push_to_talk && !self.held && frame >= self.open_until;
        if self.muted || closed { 0.0 } else { 1.0 }
    }

    /// The gain for `frame`, one ramp step closer to the target.
    fn step(&mut self, frame: i64) -> f32 {
        let target = self.target(frame);
        let step = 1.0 / RAMP_FRAMES as f32;
        self.gain = if target > self.gain { (self.gain + step).min(target) } else { (self.gain - step).max(target) };
        self.gain
    }
}

#[derive(Debug)]
struct Source {
    /// Interleaved samples from the mixer's `emitted` frame on; gaps are zeros.
    pending: Vec<f32>,
    /// The grid frame right after the last chunk pushed.
    next: i64,
    gate: Gate,
    /// This source's separate track for the current chunk (no capacity without separate tracks).
    track: Vec<f32>,
}

/// Mixes sources into tracks. See the module docs.
#[derive(Debug)]
pub struct Mixer {
    start: u64,
    /// Frames emitted so far: frame `emitted` is the next one out.
    emitted: i64,
    sources: Vec<Source>,
    separate: bool,
    mix: Vec<f32>,
}

impl Mixer {
    /// A mixer whose frame 0 plays at QPC time `start`, for `sources` in track order (desktop
    /// or game, then the microphone). With `separate`, each source also gets its own track.
    pub fn new(start: u64, sources: &[SourceOptions], separate: bool) -> Self {
        let track = if separate { CHUNK_FRAMES * CH } else { 0 };
        let sources = sources
            .iter()
            .map(|&o| Source {
                pending: Vec::with_capacity(PENDING_FRAMES * CH),
                next: 0,
                gate: Gate::new(o),
                track: Vec::with_capacity(track),
            })
            .collect();
        Mixer { start, emitted: 0, sources, separate, mix: Vec::with_capacity(CHUNK_FRAMES * CH) }
    }

    /// The grid frame nearest QPC time `time` (negative before `start`).
    fn frame_at(&self, time: u64) -> i64 {
        let ticks = time as i128 - self.start as i128;
        (ticks * RATE as i128 + CLOCK_HZ as i128 / 2).div_euclid(CLOCK_HZ as i128) as i64
    }

    /// QPC time of grid frame `frame`.
    fn time_of(&self, frame: i64) -> u64 {
        (self.start as i128 + frame as i128 * CLOCK_HZ as i128 / RATE as i128) as u64
    }

    /// Adds interleaved stereo `samples` from `source`, the first frame captured at QPC `time`.
    /// The part that is already mixed (the source was late) is dropped.
    pub fn push(&mut self, source: usize, samples: &[f32], time: u64) {
        let mut at = self.frame_at(time);
        let emitted = self.emitted;
        let s = &mut self.sources[source];
        if (at - s.next).abs() <= SNAP_FRAMES {
            at = s.next;
        }
        s.next = at + (samples.len() / CH) as i64;
        // Frames into `pending`, possibly negative; drop what was mixed already or doesn't fit.
        let offset = at - emitted;
        let skip = (-offset).max(0) as usize * CH;
        let from = offset.max(0) as usize * CH;
        let end = (from + samples.len().saturating_sub(skip)).min(PENDING_FRAMES * CH);
        if skip >= samples.len() || from >= end {
            return;
        }
        if s.pending.len() < end {
            s.pending.resize(end, 0.0);
        }
        s.pending[from..end].copy_from_slice(&samples[skip..skip + end - from]);
    }

    /// Mutes or unmutes `source` from QPC time `time`, ramped over [`RAMP_FRAMES`].
    pub fn set_muted(&mut self, source: usize, muted: bool, time: u64) {
        let at = self.frame_at(time);
        self.sources[source].gate.queue(at, Event::Mute(muted));
    }

    /// The push-to-talk key went down or up at QPC time `time` (from Raw Input). Ignored for
    /// a source without push-to-talk.
    pub fn key(&mut self, source: usize, down: bool, time: u64) {
        let at = self.frame_at(time);
        self.sources[source].gate.queue(at, Event::Key(down));
    }

    /// Emits everything every source has delivered, and at least everything up to
    /// [`LATENCY_MS`] before `now`, as `out(track, samples, time)` chunks: interleaved stereo in
    /// [-1, 1] whose first frame plays at QPC `time`. Each chunk goes out as [`Track::Mix`],
    /// then (with separate tracks) as each [`Track::Source`], all with the same time and length.
    /// After a stall of over a second the time jumps forward instead.
    pub fn mix_to(&mut self, now: u64, mut out: impl FnMut(Track, &[f32], u64)) {
        let target = self.mix_target(now);
        while self.mix_chunk(target, CHUNK_FRAMES, &mut out) {}
    }

    /// Snapshots the ready span so the caller can release the lock between chunks.
    pub(crate) fn mix_target(&mut self, now: u64) -> i64 {
        let deadline = self.frame_at(now.saturating_sub(LATENCY_MS * CLOCK_HZ / 1000));
        let ready = self.sources.iter().map(|s| self.emitted + (s.pending.len() / CH) as i64).min();
        let target = ready.map_or(deadline, |r| r.max(deadline));
        if target - self.emitted > PENDING_FRAMES as i64 {
            self.jump(target);
        }
        target
    }

    pub(crate) fn mix_chunk(
        &mut self,
        target: i64,
        frame_limit: usize,
        mut out: impl FnMut(Track, &[f32], u64),
    ) -> bool {
        if self.emitted >= target {
            return false;
        }
        let n = ((target - self.emitted) as usize).min(frame_limit).min(CHUNK_FRAMES);
        assert!(n > 0);
        self.chunk(n);
        let time = self.time_of(self.emitted);
        out(Track::Mix, &self.mix, time);
        if self.separate {
            for (i, s) in self.sources.iter().enumerate() {
                out(Track::Source(i), &s.track, time);
            }
        }
        self.emitted += n as i64;
        true
    }

    /// Mixes the next `n` frames into `mix` and the sources' `track`s, consuming `pending`.
    fn chunk(&mut self, n: usize) {
        self.mix.clear();
        self.mix.resize(n * CH, 0.0);
        for s in &mut self.sources {
            s.track.clear();
            let have = s.pending.len().min(n * CH);
            for (f, frame) in self.mix.as_chunks_mut::<CH>().0.iter_mut().enumerate() {
                let g = s.gate.step(self.emitted + f as i64);
                for (c, m) in frame.iter_mut().enumerate() {
                    let i = f * CH + c;
                    let v = if i < have { s.pending[i] * g } else { 0.0 };
                    *m += v;
                    if self.separate {
                        s.track.push(v.clamp(-1.0, 1.0));
                    }
                }
            }
            s.pending.drain(..have);
        }
        // Hard clamp: the simplest way to keep a loud sum in range. Floats don't wrap; the clamp
        // stops the encoder's integer conversion from doing so.
        // shortcut: loud overlaps clip hard; add a soft limiter if mixed clips sound harsh.
        for v in &mut self.mix {
            *v = v.clamp(-1.0, 1.0);
        }
    }

    /// Skips straight to frame `to` (the caller stalled): drops what was buffered and settles
    /// each gate on its state there.
    fn jump(&mut self, to: i64) {
        for s in &mut self.sources {
            s.pending.clear();
            s.gate.gain = s.gate.target(to);
        }
        self.emitted = to;
    }

    #[cfg(test)]
    fn capacity(&self) -> usize {
        self.mix.capacity()
            + self
                .sources
                .iter()
                .map(|s| s.pending.capacity() + s.track.capacity() + s.gate.events.capacity())
                .sum::<usize>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = CLOCK_HZ / 1000;
    const T0: u64 = 1_000_000 * MS;
    /// A 10 ms WASAPI packet, in frames.
    const PACKET: usize = 480;
    const DESKTOP: usize = 0;
    const MIC: usize = 1;

    fn tone(frames: usize, v: f32) -> Vec<f32> {
        vec![v; frames * CH]
    }

    /// QPC time of grid frame `f`.
    fn at(f: u64) -> u64 {
        T0 + f * CLOCK_HZ / RATE as u64
    }

    /// Runs `mix_to(now)` and collects each track's samples and chunk times.
    #[derive(Default)]
    struct Out {
        mix: Vec<f32>,
        times: Vec<(Track, u64, usize)>,
        tracks: [Vec<f32>; 2],
    }

    fn mix(m: &mut Mixer, now: u64, o: &mut Out) {
        m.mix_to(now, |t, s, time| {
            o.times.push((t, time, s.len()));
            match t {
                Track::Mix => o.mix.extend_from_slice(s),
                Track::Source(i) => o.tracks[i].extend_from_slice(s),
            }
        });
    }

    fn two(separate: bool) -> Mixer {
        Mixer::new(T0, &[SourceOptions::default(); 2], separate)
    }

    /// Left-channel samples of `o.mix` in frames `range`.
    fn left(o: &Out, range: std::ops::Range<usize>) -> Vec<f32> {
        range.map(|f| o.mix[f * CH]).collect()
    }

    #[test]
    fn loud_sums_are_clamped() {
        let mut m = two(false);
        m.push(DESKTOP, &[0.8, -0.8, 0.3, 0.0], T0);
        m.push(MIC, &[0.8, -0.8, 0.3, 2.0], T0);
        let mut o = Out::default();
        mix(&mut m, T0, &mut o);
        assert_eq!(o.mix, [1.0, -1.0, 0.6, 1.0]);
    }

    #[test]
    fn mixing_waits_for_every_source_until_the_deadline() {
        let mut m = two(false);
        let mut o = Out::default();
        m.push(DESKTOP, &tone(PACKET, 0.25), T0);
        mix(&mut m, at(PACKET as u64), &mut o);
        assert!(o.mix.is_empty(), "the mic hasn't delivered and the deadline isn't reached");
        m.push(MIC, &tone(PACKET, 0.5), T0);
        mix(&mut m, at(PACKET as u64), &mut o);
        assert_eq!(o.mix, tone(PACKET, 0.75), "both delivered: mixed without waiting");
    }

    #[test]
    fn a_missing_source_is_silence_after_the_latency() {
        let mut m = two(false);
        let mut o = Out::default();
        // The mic never delivers. Desktop audio flows for a second.
        for i in 0..100 {
            m.push(DESKTOP, &tone(PACKET, 0.5), at(i * PACKET as u64));
            mix(&mut m, at((i + 1) * PACKET as u64), &mut o);
        }
        // Mixed up to exactly 200 ms behind now, with the desktop's audio alone.
        assert_eq!(o.mix.len() / CH, 100 * PACKET - 200 * 48);
        assert!(o.mix.iter().all(|v| *v == 0.5));
    }

    #[test]
    fn a_late_source_keeps_only_what_arrives_in_time() {
        let mut m = two(false);
        let mut o = Out::default();
        m.push(DESKTOP, &tone(48_000, 0.25), T0);
        // At 300 ms the mic hasn't delivered: the first 100 ms go out with the desktop alone.
        mix(&mut m, T0 + 300 * MS, &mut o);
        assert_eq!(o.mix.len() / CH, 4800);
        // The mic's first 200 ms arrive now: the first half is too late, the second half mixes.
        m.push(MIC, &tone(9600, 0.5), T0);
        mix(&mut m, T0 + 400 * MS, &mut o);
        assert_eq!(o.mix.len() / CH, 9600);
        assert_eq!(left(&o, 0..4800), vec![0.25; 4800]);
        assert_eq!(left(&o, 4800..9600), vec![0.75; 4800]);
        // A mic gap of 10 ms after that is silence; the desktop carries on.
        m.push(MIC, &tone(PACKET, 0.5), at(9600 + PACKET as u64));
        mix(&mut m, T0 + 500 * MS, &mut o);
        assert_eq!(left(&o, 9600..9600 + PACKET), vec![0.25; PACKET]);
        assert_eq!(left(&o, 9600 + PACKET..9600 + 2 * PACKET), vec![0.75; PACKET]);
    }

    #[test]
    fn chunks_off_the_grid_by_rounding_stay_continuous() {
        // The source's timeline started 2.5008 frames after the mixer's, and its chunk times are
        // rounded down to 100 ns (481 frames is not a whole number of ticks), so each chunk on
        // its own rounds to frame +2 or +3. No gaps or overlaps may appear.
        let mut m = Mixer::new(T0, &[SourceOptions::default()], false);
        let src_start = T0 + 521;
        let mut o = Out::default();
        for i in 0..90u64 {
            m.push(0, &tone(481, 0.5), src_start + i * 481 * CLOCK_HZ / RATE as u64);
        }
        mix(&mut m, T0, &mut o);
        assert_eq!(o.mix.len(), (3 + 90 * 481) * CH);
        let zeros: Vec<usize> = (0..o.mix.len() / CH).filter(|f| o.mix[f * CH] == 0.0).collect();
        assert_eq!(zeros, [0, 1, 2]);
    }

    #[test]
    fn mute_ramps_linearly_over_10_ms() {
        let mut m = Mixer::new(T0, &[SourceOptions::default()], false);
        m.push(0, &tone(4000, 1.0), T0);
        m.set_muted(0, true, at(1000));
        m.set_muted(0, false, at(3000));
        let mut o = Out::default();
        mix(&mut m, T0, &mut o);
        let l = left(&o, 0..4000);
        assert!(l[..1000].iter().all(|v| *v == 1.0));
        for k in 0..RAMP_FRAMES as usize {
            let want = 1.0 - (k + 1) as f32 / RAMP_FRAMES as f32;
            assert!((l[1000 + k] - want).abs() < 1e-4, "down {k}: {}", l[1000 + k]);
            assert!((l[3000 + k] - (1.0 - want)).abs() < 1e-4, "up {k}: {}", l[3000 + k]);
        }
        assert!(l[1480..3000].iter().all(|v| *v == 0.0));
        assert!(l[3480..].iter().all(|v| *v == 1.0));
    }

    #[test]
    fn starting_muted_is_silent_from_the_first_sample() {
        let mut m = Mixer::new(T0, &[SourceOptions { muted: true, push_to_talk: false }], false);
        m.push(0, &tone(PACKET, 1.0), T0);
        let mut o = Out::default();
        mix(&mut m, T0, &mut o);
        assert_eq!(o.mix, tone(PACKET, 0.0));
    }

    #[test]
    fn push_to_talk_opens_while_held_plus_a_tail() {
        let ptt = SourceOptions { muted: false, push_to_talk: true };
        let mut m = Mixer::new(T0, &[ptt], false);
        m.push(0, &tone(20_000, 1.0), T0);
        m.key(0, false, at(500)); // a release without a press opens nothing
        m.key(0, true, at(1000));
        m.key(0, false, at(5000));
        let mut o = Out::default();
        mix(&mut m, T0, &mut o);
        let l = left(&o, 0..20_000);
        assert_eq!(PTT_TAIL_FRAMES, 7200, "150 ms");
        let close = 5000 + PTT_TAIL_FRAMES as usize; // 12 200
        assert!(l[..1000].iter().all(|v| *v == 0.0), "closed before the key");
        assert!((l[1000] - 1.0 / RAMP_FRAMES as f32).abs() < 1e-6, "ramps up from the press");
        assert!(l[1480..close].iter().all(|v| *v == 1.0), "open while held and for 150 ms after");
        assert!(l[close] < 1.0 && l[close] > 0.99, "then ramps down");
        assert!(l[close + 480..].iter().all(|v| *v == 0.0));
    }

    #[test]
    fn separate_tracks_line_up_with_the_mix() {
        let mut m = two(true);
        let mut o = Out::default();
        for i in 0..50u64 {
            m.push(DESKTOP, &tone(PACKET, 0.25), at(i * PACKET as u64));
            if i % 3 != 0 {
                m.push(MIC, &tone(PACKET, 0.5), at(i * PACKET as u64));
            }
            // The deadline is exactly the end of this packet: the mic's missing ones are silence.
            mix(&mut m, at((i + 1) * PACKET as u64) + LATENCY_MS * MS, &mut o);
        }
        // Every chunk went out as mix, desktop, mic with the same time and length.
        for c in o.times.chunks(3) {
            assert_eq!([c[0].0, c[1].0, c[2].0], [Track::Mix, Track::Source(0), Track::Source(1)]);
            assert!(c.iter().all(|t| (t.1, t.2) == (c[0].1, c[0].2)));
        }
        assert_eq!(o.times[0].1, T0);
        assert_eq!((o.tracks[0].len(), o.tracks[1].len()), (o.mix.len(), o.mix.len()));
        // And sample for sample the mix is the sum of the tracks.
        for (i, v) in o.mix.iter().enumerate() {
            assert_eq!(*v, o.tracks[0][i] + o.tracks[1][i], "sample {i}");
        }
        assert_eq!(o.tracks[1][..PACKET * CH], tone(PACKET, 0.0)[..], "the mic's missing packet");
    }

    #[test]
    fn a_stall_jumps_forward() {
        let mut m = two(false);
        let mut o = Out::default();
        m.push(DESKTOP, &tone(PACKET, 0.5), T0);
        // The caller didn't mix for an hour (the PC slept).
        let later = T0 + 3600 * 1000 * MS;
        mix(&mut m, later, &mut o);
        assert!(o.times.is_empty(), "no hour of silence");
        m.push(DESKTOP, &tone(PACKET, 0.5), later - 200 * MS);
        mix(&mut m, later + 10 * MS, &mut o);
        assert_eq!(o.times[0].1, later - 200 * MS);
        assert_eq!(o.mix, tone(PACKET, 0.5));
    }

    #[test]
    fn a_second_of_nothing_is_mixed_as_silence_and_only_more_jumps() {
        let mut m = Mixer::new(T0, &[SourceOptions::default()], false);
        let mut o = Out::default();
        mix(&mut m, at(PENDING_FRAMES as u64) + LATENCY_MS * MS, &mut o);
        assert_eq!(o.mix, tone(PENDING_FRAMES, 0.0), "exactly the buffer: no jump yet");
    }

    #[test]
    fn audio_far_past_the_buffer_is_dropped() {
        let mut m = Mixer::new(T0, &[SourceOptions::default()], false);
        m.push(0, &tone(PACKET, 1.0), at(2 * PENDING_FRAMES as u64));
        assert!(m.sources[0].pending.is_empty(), "not kept");
        let mut o = Out::default();
        mix(&mut m, T0, &mut o);
        assert!(o.mix.is_empty());
    }

    #[test]
    fn buffers_never_grow_after_warm_up() {
        let mut m =
            Mixer::new(T0, &[SourceOptions::default(), SourceOptions { muted: false, push_to_talk: true }], true);
        let cap = m.capacity();
        let packet = tone(PACKET, 0.3);
        let big = tone(3 * 48_000, 0.3);
        let mut now = T0;
        for i in 0..2000u64 {
            now += 10 * MS;
            m.push(DESKTOP, &packet, now - 10 * MS);
            match i % 7 {
                0 => m.push(MIC, &packet, now - 150 * MS),              // late
                1 => m.push(MIC, &big, now),                            // too long and ahead
                2 => m.key(MIC, i % 2 == 0, now),                       // key events
                3 => (0..40).for_each(|_| m.set_muted(MIC, true, now)), // overflow the event queue
                _ => m.push(MIC, &packet, now - 10 * MS),
            }
            if i % 500 == 499 {
                now += 5000 * MS; // a stall
            }
            m.mix_to(now, |_, s, _| assert!(s.len() <= CHUNK_FRAMES * CH));
        }
        assert_eq!(m.capacity(), cap);
    }
}
