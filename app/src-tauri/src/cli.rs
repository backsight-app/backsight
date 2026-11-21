//! Special start modes: `backsight.exe --<mode>` does one job and exits, before any window opens.
//! Each mode lives in its own module.

/// The exit code when a mode ran, `None` for a normal start.
pub fn run() -> Option<i32> {
    match std::env::args().nth(1)?.as_str() {
        // The setup check's encoder test, in its own process so a stuck driver can't freeze Backsight.
        "--probe-encoders" => Some(crate::probe_child::run()),
        // Setup's speed test of the chosen encoder, in its own process for the same reason.
        "--tune-encoder" => Some(crate::probe_child::tune()),
        // A problem report on the desktop, also when Backsight itself is frozen.
        "--report" => Some(crate::report::run()),
        // Captions for a saved clip, in their own low-priority process (ADR 0016).
        backsight_captions::CHILD_FLAG => Some(backsight_captions::child_process()),
        _ => None,
    }
}
