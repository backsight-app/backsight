use serde_json::Value;
use std::{path::Path, process::Command};

pub fn ffprobe(path: &Path, args: &[&str]) -> Value {
    let exe = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party/ffmpeg/bin/ffprobe.exe");
    let out = Command::new(exe).args(["-v", "error", "-of", "json"]).args(args).arg(path).output().unwrap();
    assert!(out.status.success(), "ffprobe failed: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

pub fn duration(info: &Value) -> f64 {
    info["format"]["duration"].as_str().unwrap().parse().unwrap()
}

pub fn require_two_minute_1080p60(info: &Value) {
    let seconds = duration(info);
    // The same tolerance as the replay_session test: the clip starts at a keyframe and may include a preceding GOP.
    assert!((119.95..122.1).contains(&seconds), "expected a two-minute replay clip, got {seconds} s");
    let stream = &info["streams"][0];
    assert_eq!(stream["width"], 1920, "requires 1920x1080 output");
    assert_eq!(stream["height"], 1080, "1080p requires a source at least 1080 pixels high");
    assert_eq!(stream["r_frame_rate"], "60/1", "requires 60 FPS video");
}

#[test]
fn replay_receipt_requires_two_minutes_at_1080p60() {
    let clip = |seconds: &str, width: u32, height: u32, fps: &str| {
        serde_json::json!({
            "format": {"duration": seconds},
            "streams": [{"width": width, "height": height, "r_frame_rate": fps}]
        })
    };
    require_two_minute_1080p60(&clip("120.1", 1920, 1080, "60/1"));
    for invalid in [clip("30", 1920, 1080, "60/1"), clip("120", 1280, 720, "60/1"), clip("120", 1920, 1080, "30/1")] {
        assert!(std::panic::catch_unwind(|| require_two_minute_1080p60(&invalid)).is_err());
    }
}
