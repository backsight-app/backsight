//! Real sessions with the capture choices: a missing monitor falls back with a notice, and window
//! mode runs without failing whatever is in front (nothing is focused or touched).
use backsight_config::Settings;
use backsight_engine::{capture::target, session::ReplaySession};
use std::time::Duration;

#[path = "support/replay_probe.rs"]
mod replay_probe;

fn settings() -> Settings {
    let mut s = Settings::default();
    s.audio.desktop = false; // video only: nothing here is about sound
    s
}

#[test]
#[ignore = "needs a desktop and an NVIDIA GPU"]
fn a_missing_monitor_records_the_main_one_and_says_so() {
    let mut s = settings();
    s.video.capture_monitor = r"\\?\DISPLAY#GONE#0#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}".into();
    let session = ReplaySession::start(&s).unwrap();
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(session.capture_notice(), Some(target::MONITOR_GONE));
    assert_eq!(session.error(), None);
    assert!(session.diagnostics().encoded_frames > 0, "the main monitor is recorded");
}

#[test]
#[ignore = "needs a desktop and an NVIDIA GPU"]
fn window_mode_waits_or_follows_without_failing() {
    let mut s = settings();
    s.video.capture_target = "window".into();
    let session = ReplaySession::start(&s).unwrap();
    std::thread::sleep(Duration::from_secs(5));
    let d = session.diagnostics();
    let notice = session.capture_notice();
    println!("notice: {notice:?}, frames: {}, from the window: {}", d.encoded_frames, d.window_frames);
    assert_eq!(session.error(), None);
    // Waiting for a game records black frames, so a clip saved now is a real video either way.
    assert!(d.encoded_frames > 0);
    match notice {
        // Something kept focus for 3 s: its window, not the black picture, is being recorded.
        None => assert!(d.window_frames > 0, "the window sent a picture"),
        Some(n) => assert!([target::WAITING_FOR_WINDOW, target::WINDOW_MINIMIZED].contains(&n), "{n}"),
    }
    let clip = session.save_clip().unwrap();
    let info = replay_probe::ffprobe(&clip, &["-show_streams", "-select_streams", "v:0", "-count_frames"]);
    let frames: u64 = info["streams"][0]["nb_read_frames"].as_str().and_then(|n| n.parse().ok()).unwrap_or(0);
    assert!(frames > 0, "the clip has video: {info}");
    std::fs::remove_file(clip).unwrap();
}
