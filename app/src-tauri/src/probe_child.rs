//! `backsight.exe --probe-encoders [ids…]`: tests video encoders in a separate process (see `cli.rs`
//! and the engine's `setup` module), so a stuck graphics driver can't freeze Backsight. No ids
//! tests every catalog encoder.

/// Exit code of the probe process.
pub fn run() -> i32 {
    let mut ids: Vec<String> = std::env::args().skip(2).collect();
    if ids.is_empty() {
        let catalog = backsight_encode::catalog::Catalog::get();
        ids = catalog.video_encoders.iter().map(|e| e.id.clone()).collect();
    }
    // Fakes a driver that never returns, e.g. BACKSIGHT_TEST_STUCK_PROBE=nvenc-hevc.
    let stuck = std::env::var("BACKSIGHT_TEST_STUCK_PROBE").unwrap_or_default();
    match backsight_engine::setup::probe_child(&ids, &stuck, std::io::stdout().lock()) {
        Ok(()) => 0,
        Err(_) => 1, // the app stopped listening
    }
}

/// `backsight.exe --tune-encoder <video json> <width> <height>`: one speed test (the engine's
/// `tune` module), answered as one JSON line.
pub fn tune() -> i32 {
    let args: Vec<String> = std::env::args().skip(2).collect();
    match backsight_engine::tune::child(&args, std::io::stdout().lock()) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}
