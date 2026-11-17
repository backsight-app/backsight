//! The rules that keep Backsight answering at once while something slow runs (opening an encoder
//! takes seconds on some PCs). Plain logic, no Windows or Tauri calls.
//!
//! - [`Gate`]: a slow hotkey command (start or stop recording, turn Instant Replay on or off) runs
//!   one at a time. A press of the same key while it runs is dropped, never queued, so no stale
//!   press acts seconds later. Dropped, not "cancel": a second press is nearly always impatience,
//!   and stopping a recording the user believes is running loses their footage.
//! - [`Latest`]: saved settings are applied in the background, newest only.
//! - [`Shown`]: the same problem pop-up isn't shown again within [`REPEAT_GAP`].

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering::SeqCst},
    },
    time::{Duration, Instant},
};

/// Lets one command through at a time.
#[derive(Default)]
pub struct Gate(Arc<AtomicBool>);

/// The command that got through; the gate opens again when this is dropped.
pub struct Pass(Arc<AtomicBool>);

impl Gate {
    /// `None` while an earlier press is still being carried out: drop this one.
    pub fn enter(&self) -> Option<Pass> {
        (!self.0.swap(true, SeqCst)).then(|| Pass(self.0.clone()))
    }
}

impl Drop for Pass {
    fn drop(&mut self) {
        self.0.store(false, SeqCst);
    }
}

/// A stop press this soon after a recording started is the same burst of presses as the one that
/// started it (a start takes 0.2 s once the encoder is warm), not a wish to stop.
pub const STOP_GRACE: Duration = Duration::from_secs(1);

/// Whether a recording running for `time` (`None`: not recording) is too new to stop.
pub fn just_started(time: Option<Duration>) -> bool {
    time.is_some_and(|t| t < STOP_GRACE)
}

/// The newest value waiting for one background worker. Values offered while the worker is busy
/// replace each other: nothing waits in line.
pub struct Latest<T> {
    next: Option<T>,
    running: bool,
}

impl<T> Default for Latest<T> {
    fn default() -> Self {
        Self { next: None, running: false }
    }
}

impl<T> Latest<T> {
    /// Keeps `value` in place of any older one still waiting. `true`: no worker is running, so the
    /// caller starts one.
    pub fn offer(&mut self, value: T) -> bool {
        self.next = Some(value);
        !std::mem::replace(&mut self.running, true)
    }

    /// The worker's next value. `None` ends the worker (the next `offer` starts a new one).
    pub fn take(&mut self) -> Option<T> {
        let value = self.next.take();
        self.running = value.is_some();
        value
    }
}

/// How long the same problem pop-up stays unrepeated: a burst of presses, and a few times the
/// 3.5 s a card is on screen. A try after that gets its explanation again.
pub const REPEAT_GAP: Duration = Duration::from_secs(10);

/// The pop-ups shown lately, by title and body.
#[derive(Default)]
pub struct Shown(Vec<(String, String, Instant)>);

impl Shown {
    /// Whether this pop-up was shown within [`REPEAT_GAP`]. If not, it counts as shown now.
    pub fn repeat(&mut self, title: &str, body: &str, now: Instant) -> bool {
        self.0.retain(|(.., at)| now.saturating_duration_since(*at) < REPEAT_GAP);
        let repeat = self.0.iter().any(|(t, b, _)| t == title && b == body);
        if !repeat {
            self.0.push((title.into(), body.into(), now));
        }
        repeat
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stop_press_in_the_same_burst_as_the_start_is_ignored() {
        assert!(!just_started(None), "not recording: the press starts one");
        assert!(just_started(Some(Duration::from_millis(30))));
        assert!(!just_started(Some(STOP_GRACE)));
    }

    #[test]
    fn presses_while_a_command_runs_are_dropped_not_queued() {
        let gate = Gate::default();
        let first = gate.enter();
        assert!(first.is_some(), "the first press runs");
        // The key is hit again and again while the encoder opens.
        assert_eq!((0..10).filter(|_| gate.enter().is_some()).count(), 0);
        drop(first);
        // Nothing was queued: the next press is a fresh command, and only one.
        let next = gate.enter();
        assert!(next.is_some() && gate.enter().is_none());
    }

    #[test]
    fn a_gate_opens_again_even_if_its_command_panics() {
        let gate = Gate::default();
        let pass = gate.enter().unwrap();
        let crashed = std::thread::spawn(move || {
            let _pass = pass;
            panic!("the command failed");
        });
        assert!(crashed.join().is_err());
        assert!(gate.enter().is_some());
    }

    #[test]
    fn only_the_newest_save_is_applied() {
        let mut saves = Latest::default();
        assert!(saves.offer(1), "nothing running: start a worker");
        assert_eq!(saves.take(), Some(1));
        // Three more saves arrive while the first is being applied.
        assert!(!saves.offer(2) && !saves.offer(3) && !saves.offer(4), "no second worker");
        assert_eq!(saves.take(), Some(4), "the ones in between are skipped");
        assert_eq!(saves.take(), None, "the worker ends");
        assert!(saves.offer(5), "a later save starts a new worker");
        assert_eq!(saves.take(), Some(5));
    }

    #[test]
    fn the_same_problem_is_shown_once_in_a_while() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut shown = Shown::default();
        assert!(!shown.repeat("Couldn't start recording", "why", at(0)));
        assert!((1..20).all(|i| shown.repeat("Couldn't start recording", "why", at(i * 200))), "a burst of presses");
        assert!(!shown.repeat("Couldn't start recording", "another reason", at(4000)), "new words are shown");
        assert!(!shown.repeat("Couldn't save the clip", "why", at(4000)));
        assert!(shown.repeat("Couldn't start recording", "why", at(9_999)));
        assert!(!shown.repeat("Couldn't start recording", "why", at(10_000)), "a later try is told again");
        assert_eq!(shown.0.len(), 3, "old entries are forgotten");
    }
}
