//! Keeps a captured audio stream continuous on the QPC clock, so sample `n` always plays at
//! `start + n / rate` and audio stays in sync with video for hours.
//!
//! WASAPI loopback delivers nothing while nothing plays, and a device clock drifts a little
//! from QPC. The timeline compares each chunk's QPC time with how much has been written:
//! a gap gets silence, an overlap (device clock running ahead) is trimmed. After a stall of
//! over a second (the PC slept: QPC keeps counting through sleep) the stream jumps forward
//! instead, like the video pacer's skipped slots. Pure, so it is tested without Windows.
//! Times are in 100 ns QPC units, as WASAPI and WGC report them.

/// Counters for the diagnostics page.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TimelineStats {
    /// Silent frames inserted (nothing playing, a device change, or the device clock running slow).
    pub silence_frames: u64,
    /// Captured frames dropped because they overlapped what was already written.
    pub trimmed_frames: u64,
    /// Frames jumped over after stalls (sleep, a frozen thread) instead of writing silence.
    pub jumped_frames: u64,
}

/// Where a captured chunk goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// Leave out this many frames of time first (only after a stall): the next write's time jumps.
    pub jump: u64,
    /// Then write this many silent frames.
    pub silence: u64,
    /// Then skip this many frames at the start of the chunk (it may be the whole chunk).
    pub skip: u64,
}

const CLOCK_HZ: u64 = 10_000_000;
/// Timing differences below this are left alone: packet timestamps jitter by a few ms.
const TOLERANCE_MS: u64 = 10;
/// Drift just past the tolerance is corrected this much at a time, not all the way back to 0:
/// a shorter silence or cut is harder to hear. Anything further off (sound resuming after a
/// pause, a device change) is corrected in one go.
const DRIFT_STEP_MS: u64 = 5;
/// [`Timeline::fill_to`] stays this far behind `now`, so a late packet still fits after it.
const FILL_SLACK_MS: u64 = 100;
/// A gap longer than this in one go means the thread stalled; jump instead of writing silence.
/// Normal quiet is filled on every 20 ms wake, so it never comes close.
const STALL_MS: u64 = 1000;
/// A packet stamp this far from `now` is a glitch (virtual cables, USB and Bluetooth can send one).
const BAD_STAMP_MS: u64 = 1000;

// shortcut: drift is corrected by inserting or trimming 5 ms at a time. A measured loopback ran
// 250 ppm slow (47 988 frames/s), which makes one 5 ms silence every 20 s (at most 6 per
// 2-minute clip; audio stays within 10 ms). Stretch with swresample's soft compensation if
// long recordings sound it.
#[derive(Debug)]
pub struct Timeline {
    rate: u64,
    start: u64,
    /// Frames (per channel) written since `start`.
    written: u64,
    stats: TimelineStats,
}

impl Timeline {
    /// A stream whose first frame plays at QPC time `start`.
    pub fn new(rate: u32, start: u64) -> Self {
        assert!(rate > 0, "sample rate must be positive");
        Timeline { rate: rate.into(), start, written: 0, stats: TimelineStats::default() }
    }

    pub fn stats(&self) -> TimelineStats {
        self.stats
    }

    /// QPC time of the next frame to be written.
    pub fn next_time(&self) -> u64 {
        self.start + self.written * CLOCK_HZ / self.rate
    }

    fn frames_at(&self, time: u64) -> u64 {
        time.saturating_sub(self.start) * self.rate / CLOCK_HZ
    }

    fn tolerance(&self) -> u64 {
        self.rate * TOLERANCE_MS / 1000
    }

    /// A gap of `frames`: silence, or a jump after a stall.
    fn gap(&mut self, frames: u64) -> Placement {
        let p = if frames > self.rate * STALL_MS / 1000 {
            Placement { jump: frames, silence: 0, skip: 0 }
        } else {
            Placement { jump: 0, silence: frames, skip: 0 }
        };
        self.stats.jumped_frames += p.jump;
        self.stats.silence_frames += p.silence;
        self.written += frames;
        p
    }

    /// The time to place a packet at: its stamp `qpc`, unless `error` flags it or it is more than
    /// a second from `now`. Then the packet is assumed to follow on, so one bad stamp can't
    /// drop everything until the clock catches up (a future stamp) or jump the stream (a past one).
    pub fn packet_time(&self, qpc: u64, error: bool, now: u64) -> u64 {
        if error || qpc.abs_diff(now) > BAD_STAMP_MS * CLOCK_HZ / 1000 { self.next_time() } else { qpc }
    }

    /// A chunk of `frames` captured at QPC time `time`: what to write. The caller leaves out
    /// `jump` frames of time, writes `silence` zero frames, then `frames - skip` of the chunk.
    pub fn place(&mut self, time: u64, frames: u64) -> Placement {
        // Signed: a chunk may start before `start` (captured just before the stream began).
        let due = (time as i64 - self.start as i64) * self.rate as i64 / CLOCK_HZ as i64;
        let ahead = due - self.written as i64; // > 0: a gap, < 0: an overlap
        let tolerance = self.tolerance();
        let step = self.rate * DRIFT_STEP_MS / 1000;
        let off = ahead.unsigned_abs();
        let fix = if off < tolerance + step { step } else { off };
        let mut p = Placement { jump: 0, silence: 0, skip: 0 };
        if ahead > tolerance as i64 {
            p = self.gap(fix);
        } else if ahead < -(tolerance as i64) {
            p.skip = fix.min(frames);
            self.stats.trimmed_frames += p.skip;
        }
        self.written += frames - p.skip;
        p
    }

    /// What to write when no chunk arrived for a while (`skip` is 0): catches up to shortly
    /// before `now`. Without this, a replay saved while nothing plays would have no sound.
    pub fn fill_to(&mut self, now: u64) -> Placement {
        let due = self.frames_at(now.saturating_sub(FILL_SLACK_MS * CLOCK_HZ / 1000));
        self.gap(due.saturating_sub(self.written))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    const MS: u64 = CLOCK_HZ / 1000;
    /// A 10 ms WASAPI packet.
    const PACKET: u64 = 480;
    const T0: u64 = 1_000_000 * MS;

    fn none() -> Placement {
        Placement { jump: 0, silence: 0, skip: 0 }
    }

    fn silent(frames: u64) -> Placement {
        Placement { jump: 0, silence: frames, skip: 0 }
    }

    #[test]
    fn steady_packets_pass_through() {
        let mut t = Timeline::new(RATE, T0);
        for i in 0..1000u64 {
            // ±3 ms jitter on the timestamps changes nothing.
            let jitter = if i % 4 == 1 { 3 * MS } else { 0 };
            assert_eq!(t.place(T0 + i * 10 * MS + jitter, PACKET), none(), "packet {i}");
        }
        assert_eq!(t.next_time(), T0 + 10_000 * MS);
        assert_eq!(t.stats(), TimelineStats::default());
    }

    #[test]
    fn a_pause_in_playback_becomes_silence() {
        let mut t = Timeline::new(RATE, T0);
        t.place(T0, PACKET);
        // Nothing plays for half a second (and no wake filled it), then sound resumes.
        assert_eq!(t.place(T0 + 510 * MS, PACKET), silent(24_000));
        assert_eq!(t.next_time(), T0 + 520 * MS);
    }

    #[test]
    fn silence_is_filled_while_nothing_arrives_and_playback_resumes_cleanly() {
        let mut t = Timeline::new(RATE, T0);
        // Silent from the start; the thread wakes every 20 ms and fills up to 100 ms before now.
        for i in 1..=50 {
            assert_eq!(t.fill_to(T0 + i * 20 * MS).jump, 0);
        }
        assert_eq!(t.next_time(), T0 + 900 * MS);
        assert_eq!(t.fill_to(T0 + 1000 * MS), none(), "nothing twice");
        // Sound resumes with a packet stamped 40 ms ago, inside the slack: no overlap.
        assert_eq!(t.place(T0 + 960 * MS, PACKET), silent(60 * 48));
        // While packets flow, filling does nothing.
        assert_eq!(t.fill_to(T0 + 1000 * MS), none());
        assert_eq!(t.stats().silence_frames, 960 * 48);
    }

    #[test]
    fn a_stall_jumps_ahead_instead_of_writing_an_hour_of_silence() {
        let hour = 3600 * 1000 * MS;
        let mut t = Timeline::new(RATE, T0);
        t.place(T0, PACKET);
        // The PC slept for an hour; QPC kept counting.
        assert_eq!(t.place(T0 + 10 * MS + hour, PACKET), Placement { jump: 3600 * 48_000, silence: 0, skip: 0 });
        assert_eq!(t.next_time(), T0 + 20 * MS + hour, "back on the clock");
        // The same when a wake with no packet notices the stall first.
        let mut t = Timeline::new(RATE, T0);
        assert_eq!(t.fill_to(T0 + 100 * MS + hour).jump, 3600 * 48_000);
        assert_eq!((t.stats().silence_frames, t.stats().jumped_frames), (0, 3600 * 48_000));
    }

    #[test]
    fn a_stamp_far_from_now_is_replaced_by_the_next_expected_time() {
        let mut t = Timeline::new(RATE, T0);
        t.place(T0, PACKET);
        let now = T0 + 20 * MS;
        assert_eq!(t.packet_time(T0 + 10 * MS, false, now), T0 + 10 * MS, "a good stamp is kept");
        assert_eq!(t.packet_time(T0 + 10 * MS, true, now), t.next_time(), "flagged");
        for bad in [now + 5000 * MS, now - 5000 * MS] {
            let time = t.packet_time(bad, false, now);
            assert_eq!(time, t.next_time(), "bad stamp falls back to the clock");
            assert_eq!(t.place(time, PACKET), none(), "and nothing is lost or jumped");
        }
    }

    #[test]
    fn a_device_clock_running_fast_is_trimmed_back() {
        let mut t = Timeline::new(RATE, T0);
        // The device delivers 481 frames per 10 ms of QPC time (+0.2 %): after 60 packets it is
        // 60 frames ahead (1.25 ms, tolerated); by about 500 packets it passes 10 ms and 5 ms is trimmed,
        // then 5 ms again every 240 packets.
        let mut trims = 0;
        for i in 0..2000u64 {
            let p = t.place(T0 + i * 10 * MS, PACKET + 1);
            assert_eq!(p.silence, 0);
            trims += (p.skip > 0) as u32;
            let drift = t.written as i64 - t.frames_at(T0 + (i + 1) * 10 * MS) as i64;
            assert!(drift.unsigned_abs() <= t.tolerance() + PACKET + 1, "drift {drift} at {i}");
        }
        assert!((6..=8).contains(&trims), "{trims} trims");
    }

    #[test]
    fn a_device_clock_running_slow_gets_silence() {
        let mut t = Timeline::new(RATE, T0);
        let mut fills = 0;
        for i in 0..2000u64 {
            let p = t.place(T0 + i * 10 * MS, PACKET - 1);
            assert_eq!(p.skip, 0);
            fills += (p.silence > 0) as u32;
        }
        assert!((6..=8).contains(&fills), "{fills} fills");
    }

    #[test]
    fn a_two_minute_clip_at_the_measured_drift_gets_a_few_short_silences() {
        // A measured loopback: 47 988 frames per QPC second (250 ppm slow), 10 ms packets.
        let mut t = Timeline::new(RATE, T0);
        let (mut fills, mut longest) = (0, 0);
        for i in 0..12_000u64 {
            let p = t.place(T0 + i * PACKET * CLOCK_HZ / 47_988, PACKET);
            fills += (p.silence > 0) as u32;
            longest = longest.max(p.silence);
            let off = t.frames_at(T0 + (i + 1) * PACKET * CLOCK_HZ / 47_988).abs_diff(t.written);
            // Until the next packet corrects it, drift may pass the tolerance by a frame.
            assert!(off <= t.tolerance() + 1, "{off} frames off sync at packet {i}");
        }
        assert_eq!(longest, 5 * 48, "each silence is 5 ms");
        // 30 ms of drift: the first silence after 40 s (past 10 ms), then one every 20 s.
        assert_eq!(fills, 4, "silences");
    }

    #[test]
    fn audio_from_before_the_start_is_dropped() {
        let mut t = Timeline::new(RATE, T0);
        assert_eq!(t.place(T0 - 50 * MS, PACKET).skip, PACKET, "entirely before the start");
        // A 20 ms packet from 15 ms before the start: its last 5 ms are kept.
        assert_eq!(t.place(T0 - 15 * MS, 2 * PACKET).skip, 15 * 48, "the part before the start");
        assert_eq!(t.next_time(), T0 + 5 * MS);
        // 5 ms early is within tolerance: kept whole.
        let mut t = Timeline::new(RATE, T0);
        assert_eq!(t.place(T0 - 5 * MS, PACKET), none());
    }
}
