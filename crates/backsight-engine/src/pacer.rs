//! Frame pacing: turns Windows.Graphics.Capture's irregular frames into the constant
//! frame rate the encoder needs.
//!
//! WGC only delivers a frame when the screen changes. The pacer owns one output slot per
//! `1 / fps` on the QPC clock. A slot gets the newest frame that arrived since the last
//! slot; with none it repeats the previous one, and extra frames in between are dropped.
//! Times are raw QPC ticks, so the pacer is pure and testable without Windows.

/// Counters for the diagnostics page.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PacerStats {
    /// Frames replaced by a newer one before their slot came up (the game outran the target).
    pub dropped: u64,
    /// Slots that re-sent the previous frame (nothing changed on screen, or capture delivered
    /// no new frame in time).
    pub repeated: u64,
    /// Slots skipped after a long stall instead of sending a burst of repeats.
    pub skipped: u64,
}

/// One output frame: send the newest texture with this presentation timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    /// Index in units of `1 / fps`, counted from the first captured frame.
    pub pts: u64,
    /// `true` when a new frame arrived for this slot, `false` for a repeat.
    pub fresh: bool,
}

#[derive(Debug)]
pub struct Pacer {
    fps: u64,
    qpc_freq: u64,
    start: Option<u64>,
    next_slot: u64,
    fresh: bool,
    stats: PacerStats,
}

impl Pacer {
    pub fn new(fps: u32, qpc_freq: u64) -> Self {
        assert!(fps > 0 && qpc_freq > 0, "fps and QPC frequency must be positive");
        Pacer { fps: fps.into(), qpc_freq, start: None, next_slot: 0, fresh: false, stats: PacerStats::default() }
    }

    pub fn stats(&self) -> PacerStats {
        self.stats
    }

    /// The first frame's arrival time: slot 0's time on the QPC clock.
    pub fn start(&self) -> Option<u64> {
        self.start
    }

    /// A captured frame arrived at QPC time `now`. It replaces any frame not yet sent.
    pub fn frame_arrived(&mut self, now: u64) {
        self.start.get_or_insert(now);
        if self.fresh {
            self.stats.dropped += 1;
        }
        self.fresh = true;
    }

    /// When the next slot is due, or `None` before the first frame (nothing to send yet).
    pub fn next_deadline(&self) -> Option<u64> {
        self.start.map(|s| self.slot_time(s, self.next_slot))
    }

    /// The next slot due at `now`, if any. Call until it returns `None`.
    pub fn poll(&mut self, now: u64) -> Option<Slot> {
        let start = self.start?;
        if now < self.slot_time(start, self.next_slot) {
            return None;
        }
        // After a stall (sleep, debugger, a hung GPU, an encoder falling behind) jump to the
        // current slot instead of bursting repeats; timestamps stay on the wall clock so audio
        // stays in sync. About 100 ms behind is enough: catching up later shows old pictures
        // against current sound.
        let current = (now - start) * self.fps / self.qpc_freq;
        if current.saturating_sub(self.next_slot) >= (self.fps / 10).max(2) {
            self.stats.skipped += current - self.next_slot;
            self.next_slot = current;
        }
        let slot = Slot { pts: self.next_slot, fresh: std::mem::take(&mut self.fresh) };
        if !slot.fresh {
            self.stats.repeated += 1;
        }
        self.next_slot += 1;
        Some(slot)
    }

    /// Computed from the start each time so slot times never drift.
    fn slot_time(&self, start: u64, n: u64) -> u64 {
        start + n * self.qpc_freq / self.fps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FREQ: u64 = 10_000_000; // QPC frequency on most Windows 10+ machines

    /// Feeds arrivals in time order and polls exactly at each deadline, like the pacer thread.
    fn simulate(p: &mut Pacer, arrivals: &[u64], end: u64) -> Vec<Slot> {
        let mut out = Vec::new();
        let mut i = 0;
        loop {
            match (p.next_deadline(), arrivals.get(i).copied()) {
                (Some(d), a) if d <= end && a.is_none_or(|a| d < a) => {
                    while let Some(s) = p.poll(d) {
                        out.push(s);
                    }
                }
                (_, Some(a)) => {
                    p.frame_arrived(a);
                    i += 1;
                }
                _ => return out,
            }
        }
    }

    fn evenly(fps: u64, from: u64, to: u64) -> Vec<u64> {
        (0..).map(|k| from + k * FREQ / fps).take_while(|t| *t < to).collect()
    }

    fn assert_steady(slots: &[Slot], count: u64) {
        assert_eq!(slots.len() as u64, count);
        assert!(slots.iter().enumerate().all(|(i, s)| s.pts == i as u64), "pts has gaps");
    }

    #[test]
    fn nothing_is_sent_before_the_first_frame() {
        let mut p = Pacer::new(60, FREQ);
        assert_eq!((p.next_deadline(), p.poll(123)), (None, None));
    }

    #[test]
    fn faster_game_drops_frames_to_the_target_rate() {
        let mut p = Pacer::new(60, FREQ);
        let slots = simulate(&mut p, &evenly(144, 0, FREQ), FREQ - 1);
        assert_steady(&slots, 60);
        assert!(slots.iter().all(|s| s.fresh));
        // The last frame still waits for slot 60, so it is not dropped yet.
        assert_eq!(p.stats(), PacerStats { dropped: 144 - 60 - 1, repeated: 0, skipped: 0 });
    }

    #[test]
    fn slower_game_repeats_frames() {
        let mut p = Pacer::new(60, FREQ);
        let slots = simulate(&mut p, &evenly(30, 0, FREQ), FREQ - 1);
        assert_steady(&slots, 60);
        assert_eq!(slots.iter().filter(|s| s.fresh).count(), 30);
        assert_eq!(p.stats().repeated, 30);
    }

    #[test]
    fn static_screen_keeps_the_output_steady() {
        let mut p = Pacer::new(60, FREQ);
        let slots = simulate(&mut p, &evenly(60, 0, FREQ / 2), FREQ - 1);
        assert_steady(&slots, 60);
        assert_eq!(p.stats().repeated, 30);
    }

    #[test]
    fn a_burst_sends_only_the_newest_frame() {
        let mut p = Pacer::new(60, FREQ);
        let slots = simulate(&mut p, &[0, 10, 20, 30, 40], FREQ / 60 - 1);
        assert_eq!(slots, vec![Slot { pts: 0, fresh: true }]);
        // The first arrival opens slot 0 at once; the four after it land in slot 1.
        assert_eq!(simulate(&mut p, &[], FREQ / 60), vec![Slot { pts: 1, fresh: true }]);
        assert_eq!(p.stats().dropped, 3);
    }

    #[test]
    fn a_long_stall_skips_ahead_instead_of_bursting() {
        let mut p = Pacer::new(60, FREQ);
        p.frame_arrived(0);
        assert_eq!(p.poll(0), Some(Slot { pts: 0, fresh: true }));
        p.frame_arrived(5 * FREQ);
        assert_eq!(p.poll(5 * FREQ), Some(Slot { pts: 300, fresh: true }));
        assert_eq!(p.poll(5 * FREQ), None);
        assert_eq!(p.stats().skipped, 299);
    }

    #[test]
    fn falling_behind_by_a_tenth_of_a_second_skips_ahead() {
        let mut p = Pacer::new(60, FREQ);
        p.frame_arrived(0);
        assert_eq!(p.poll(0), Some(Slot { pts: 0, fresh: true }));
        // 5 slots late (83 ms): still catches up one slot at a time.
        assert_eq!(p.poll(6 * FREQ / 60), Some(Slot { pts: 1, fresh: false }));
        // Now 7 slots behind the clock (slot 8 is due): jump instead of sending old slots.
        assert_eq!(p.poll(8 * FREQ / 60 + 1), Some(Slot { pts: 8, fresh: false }));
        assert_eq!(p.stats().skipped, 6);
    }

    #[test]
    fn slot_times_do_not_drift_over_an_hour() {
        let mut p = Pacer::new(60, FREQ);
        p.frame_arrived(7);
        p.next_slot = 60 * 3600;
        assert_eq!(p.next_deadline(), Some(7 + 3600 * FREQ));
    }
}
