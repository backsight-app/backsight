//! The real `backsight.exe --probe-encoders`: an encoder whose driver never returns is given up on
//! in time, and the encoders after it are still tested.

use backsight_engine::setup::{PROBE_LIMIT, ProbeRun, probe_in_children};
use std::{
    process::Command,
    time::{Duration, Instant},
};

#[test]
fn a_stuck_encoder_is_given_up_and_the_rest_still_tested() {
    let exe = env!("CARGO_BIN_EXE_backsight");
    // A new build's first start can take seconds (virus scan); that's not what's timed here.
    let warm = Command::new(exe).args(["--probe-encoders", "x264"]).output().expect("starts");
    assert!(String::from_utf8_lossy(&warm.stdout).starts_with("ready"), "{warm:?}");
    let limit = Duration::from_secs(3);
    // nvenc-h264 answers fast either way: it works here, and fails at once without an NVIDIA card (CI).
    let ids = ["x264", "nvenc-hevc", "nvenc-h264"];
    let started = Instant::now();
    let results = probe_in_children(
        &ids,
        // Only the stuck one gets the short limit: opening NVENC alone takes ~6 s on a busy laptop.
        |id| if id == "nvenc-hevc" { limit } else { PROBE_LIMIT },
        |rest| {
            let mut command = Command::new(exe);
            command.arg("--probe-encoders").args(rest).env("BACKSIGHT_TEST_STUCK_PROBE", "nvenc-hevc");
            ProbeRun::start(command)
        },
    );
    let took = started.elapsed();
    let problem = |i: usize| results[i].as_ref().expect("tested").problem.as_ref();
    assert!(problem(0).is_none(), "x264: {:?}", problem(0));
    assert!(problem(1).is_some_and(|p| p.detail.contains("stopped responding")), "{:?}", problem(1));
    let tested = !problem(2).is_some_and(|p| p.detail.contains("stopped responding"));
    assert!(tested, "nvenc-h264, in a new process: {:?}", problem(2));
    // The stuck one costs the short limit, not the full one; the other two take a few seconds.
    assert!(took >= limit && took < PROBE_LIMIT, "gave up after {took:?}");
}
