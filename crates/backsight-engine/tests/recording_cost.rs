//! Tier-2 cost harness with recording on: the `gpu_cost` replay session plus a
//! running recording, for the PresentMon game-FPS runs that repeat the replay-only measurements
//! (testing.md, "Repeating the frame-cost measurement"). After the run it samples the process's private
//! working set, times saving a replay clip while the recording still runs, stops the recording
//! and prints one `BACKSIGHT_M2_RECORDING` line.
//! Start with scripts/recording-cost.ps1 -Kind record so the runner owns the media directory
//! and can clean up even if this process is killed.

#[path = "support/cost.rs"]
mod cost;
#[path = "support/replay_probe.rs"]
mod replay_probe;

use backsight_config::Settings;
use backsight_engine::session::ReplaySession;
use serde_json::json;
use std::{
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

/// This process's private working set (the method in `replay_session.rs`), in bytes.
fn private_working_set() -> u64 {
    let query = format!(
        "(Get-CimInstance Win32_PerfFormattedData_PerfProc_Process -Filter 'IDProcess={}').WorkingSetPrivate",
        std::process::id()
    );
    let out =
        Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", &query]).output().unwrap();
    assert!(out.status.success(), "memory query failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().parse().expect("one private working set value")
}

#[test]
#[ignore = "experiment"]
fn run_recording_session() {
    let Ok(path) = std::env::var("BACKSIGHT_EXP") else {
        println!("skipped: set BACKSIGHT_EXP to a settings file (see this file's header)");
        return;
    };
    let seconds: u64 = std::env::var("BACKSIGHT_EXP_SECONDS").unwrap().parse().unwrap();
    let run = cost::run_dir().expect("Start the recording benchmark with scripts/recording-cost.ps1.");
    let mut settings = Settings::load_from(Path::new(&path)).unwrap();
    let dir = run.join("media");
    assert!(dir.is_dir(), "The runner must create the media directory.");
    settings.general.video_folder = dir.to_string_lossy().into_owned();
    let mut session = ReplaySession::start(&settings).unwrap();
    session.start_recording().unwrap();
    let started = Instant::now();
    println!("BACKSIGHT_COST_READY");
    cost::check_capture(&run, Duration::from_secs(seconds), || cost::fresh_frames(&session));
    // Give the two-minute replay buffer time to fill even after a shorter FPS measurement.
    std::thread::sleep(Duration::from_secs(125).saturating_sub(started.elapsed()));

    // Sample before saving: a clip snapshot retains chunks outside the live buffer.
    let private_working_set_bytes = private_working_set();
    let before_save = session.diagnostics();
    let started = Instant::now();
    let clip = session.save_clip().unwrap();
    let save_ms = started.elapsed().as_secs_f64() * 1000.0;
    let diagnostics = session.diagnostics();
    let recorded = session.recording_time().unwrap_or_default();
    let stopped = session.stop_recording().expect("a recording was running");
    assert_eq!(stopped.stopped_early, None);
    let file = stopped.file.unwrap();
    let probe = replay_probe::ffprobe(
        &clip,
        &["-select_streams", "v:0", "-show_entries", "stream=width,height,r_frame_rate,avg_frame_rate:format=duration"],
    );
    replay_probe::require_two_minute_1080p60(&probe);
    let video = &probe["streams"][0];
    let parts: Vec<_> = stopped.earlier_parts.iter().chain([&file]).collect();
    let recording_bytes: u64 = parts.iter().map(|p| std::fs::metadata(p).unwrap().len()).sum();
    assert_eq!(session.error(), None);
    println!("EXP {path}: {diagnostics:?}");
    println!(
        "BACKSIGHT_M2_RECORDING {}",
        json!({
            "receipt": "real device", "recorded_s": recorded.as_secs_f64(),
            "save_ms_while_recording": (save_ms * 1000.0).round() / 1000.0,
            "private_working_set_bytes": private_working_set_bytes, "buffer_bytes": before_save.buffer_bytes,
            "buffer_s": before_save.buffer.as_secs_f64(),
            "saved_clip_s": replay_probe::duration(&probe),
            "saved_clip_width": video["width"], "saved_clip_height": video["height"],
            "saved_clip_fps": video["r_frame_rate"], "saved_clip_avg_fps": video["avg_frame_rate"],
            "recording_bytes": recording_bytes, "recording_parts": parts.len(),
            "recording_mbps": (recording_bytes as f64 * 8.0 / recorded.as_secs_f64().max(1.0) / 1e4).round() / 100.0,
        })
    );
}
