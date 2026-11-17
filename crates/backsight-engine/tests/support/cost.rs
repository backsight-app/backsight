//! Checks shared by the replay, capture-only and recording benchmark runs.
use backsight_engine::{capture::wgc, session::ReplaySession};
use std::{
    fs, io,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};
use windows::Win32::{
    Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromWindow},
    UI::WindowsAndMessaging::GetWindowThreadProcessId,
};

pub fn run_dir() -> Option<PathBuf> {
    std::env::var_os("BACKSIGHT_COST_RUN").map(PathBuf::from)
}

pub fn fresh_frames(session: &ReplaySession) -> u64 {
    let d = session.diagnostics();
    d.encoded_frames.saturating_sub(d.pacer.repeated)
}

fn require_primary_game(game_pid: u32) {
    let window = wgc::foreground_window().expect("Keep the benchmark game in the foreground.");
    let mut pid = 0;
    // SAFETY: these only query the window; pid is a valid out pointer.
    let monitor = unsafe {
        GetWindowThreadProcessId(window, Some(&mut pid));
        MonitorFromWindow(window, MONITOR_DEFAULTTONULL)
    };
    assert!(
        pid == game_pid && monitor == wgc::primary_monitor(),
        "Keep the benchmark game in the foreground on the primary monitor."
    );
}

fn active(frames: u64, elapsed: Duration) -> bool {
    // Even a modest gameplay scene must supply new frames, not paced repeats of the desktop.
    frames as f64 / elapsed.as_secs_f64() >= 10.0
}

/// Sample only the window bracketed by the runner's two markers.
pub fn check_capture(run: &Path, timeout: Duration, frames: impl Fn() -> u64) {
    let start = Instant::now();
    let game_pid = loop {
        match fs::read_to_string(run.join("game.pid")) {
            Ok(pid) => break pid.trim().parse().expect("game process ID"),
            Err(e) if e.kind() == io::ErrorKind::NotFound && start.elapsed() < timeout => {
                thread::sleep(Duration::from_millis(100));
            }
            Err(e) => panic!("Did not receive the benchmark start: {e}"),
        }
    };
    require_primary_game(game_pid);
    let started = Instant::now();
    let mut sample = started;
    let mut previous = frames();
    println!("BACKSIGHT_COST_MEASURING");
    loop {
        thread::sleep(Duration::from_secs(1));
        require_primary_game(game_pid);
        let done = run.join("presentmon.done").try_exists().unwrap();
        let elapsed = sample.elapsed();
        if elapsed >= Duration::from_secs(5) || done {
            let current = frames();
            let fresh = current.saturating_sub(previous);
            assert!(
                active(fresh, elapsed),
                "Only {fresh} new capture frames in {elapsed:.2?}; keep the gameplay scene moving."
            );
            sample = Instant::now();
            previous = current;
        }
        if done {
            break;
        }
        assert!(started.elapsed() < timeout, "PresentMon did not finish the benchmark in time.");
    }
}

#[test]
fn sustained_activity_requires_new_capture_frames() {
    assert!(!active(0, Duration::from_secs(5)));
    assert!(!active(49, Duration::from_secs(5)));
    assert!(active(50, Duration::from_secs(5)));
}
