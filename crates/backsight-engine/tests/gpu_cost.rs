//! Tier-2 frame-cost harness: run a replay session from a settings file for a while, so
//! game-FPS cost can be measured per configuration without the tray app taking focus.
//! BACKSIGHT_EXP=<settings.toml> BACKSIGHT_EXP_SECONDS=<n> cargo test --release -p backsight-engine --test gpu_cost -- --ignored --nocapture
//! BACKSIGHT_EXP_MODE=capture (WGC only, the frame-cost baseline) or convert (WGC + BGRA→NV12)
//! skips encoding; both take fps, cursor and resolution from the same settings file as replay.
// shortcut: NVIDIA adapter and primary monitor only, upgrade when AMD/Intel get frame-cost runs.

#[path = "support/cost.rs"]
mod cost;
#[path = "support/scene.rs"]
mod scene;

use backsight_config::Settings;
use backsight_engine::{
    capture::{
        convert::Converter,
        d3d::Gpu,
        wgc::{self, Capture},
    },
    session::{ReplaySession, output_size},
};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::Duration,
};

#[test]
#[ignore = "experiment"]
fn run_session() {
    let Ok(path) = std::env::var("BACKSIGHT_EXP") else {
        println!("skipped: set BACKSIGHT_EXP to a settings file (see this file's header)");
        return;
    };
    let seconds: u64 = std::env::var("BACKSIGHT_EXP_SECONDS").unwrap().parse().unwrap();
    let mode = std::env::var("BACKSIGHT_EXP_MODE").unwrap_or_default();
    let settings = Settings::load_from(Path::new(&path)).unwrap();
    if mode.is_empty() {
        let session = ReplaySession::start(&settings).unwrap();
        println!("BACKSIGHT_COST_READY");
        if let Some(run) = cost::run_dir() {
            cost::check_capture(&run, Duration::from_secs(seconds), || cost::fresh_frames(&session));
        } else {
            std::thread::sleep(Duration::from_secs(seconds));
        }
        println!("EXP {path}: {:?}", session.diagnostics());
        assert_eq!(session.error(), None);
        return;
    }
    let gpu = Arc::new(Gpu::open("nvidia").unwrap());
    let monitor = wgc::primary_monitor();
    let input = wgc::monitor_size(monitor).unwrap();
    let fps = settings.video.fps;
    let size = output_size(input, &settings.video.resolution);
    let conv = (mode == "convert").then(|| Mutex::new(Converter::new(&gpu, input, size, fps).unwrap()));
    let frames = Arc::new(AtomicU64::new(0));
    let (f, g) = (frames.clone(), gpu.clone());
    let capture = Capture::monitor(
        &gpu,
        monitor,
        settings.video.capture_cursor,
        Some(Duration::from_secs(1) / (2 * fps)), // same interval as session.rs
        move |frame| {
            let Ok(frame) = frame else { return };
            if let Some(c) = &conv {
                c.lock().unwrap().convert_from(&g, frame.texture, frame.size).unwrap();
            }
            f.fetch_add(1, Relaxed);
        },
    )
    .unwrap();
    println!("BACKSIGHT_COST_READY");
    if let Some(run) = cost::run_dir() {
        cost::check_capture(&run, Duration::from_secs(seconds), || frames.load(Relaxed));
    } else {
        std::thread::sleep(Duration::from_secs(seconds));
    }
    drop(capture);
    println!("EXP mode {mode}: {} frames delivered", frames.load(Relaxed));
}

/// Settings from a line of `key=value` words: `encoder`, `fps`, `resolution`, `hdr`, `monitor` and
/// `audio` (desktop sound on or off) are video and audio settings, anything else an option of the
/// encoder (`bitrate=20000 preset=p1 bframes=0`). What isn't named keeps the fresh-install default.
fn spec_settings(spec: &str) -> Settings {
    let mut s = Settings::default();
    let mut options = backsight_config::EncoderOptions::new();
    for pair in spec.split_whitespace() {
        let (key, v) = pair.split_once('=').expect("key=value");
        match key {
            "encoder" => s.video.encoder = v.into(),
            "fps" => s.video.fps = v.parse().unwrap(),
            "resolution" => s.video.resolution = v.into(),
            "hdr" => s.video.hdr = v.parse().unwrap(),
            "audio" => s.audio.desktop = v.parse().unwrap(),
            // A number from `list_monitors`, so a line stays free of device paths.
            "monitor" => s.video.capture_monitor = wgc::monitors()[v.parse::<usize>().unwrap()].id.clone(),
            _ => {
                let value = v.parse().map(toml::Value::Integer).or_else(|_| v.parse().map(toml::Value::Boolean));
                options.insert(key.into(), value.unwrap_or_else(|_| toml::Value::String(v.into())));
            }
        }
    }
    s.video.encoder_options.insert(s.video.encoder.clone(), options);
    s
}

#[test]
fn a_spec_line_sets_video_settings_and_encoder_options() {
    let s = spec_settings("encoder=nvenc-hevc fps=30 hdr=true bitrate=5000 preset=p4 lookahead=true");
    assert_eq!((s.video.encoder.as_str(), s.video.fps, s.video.hdr), ("nvenc-hevc", 30, true));
    let options = &s.video.encoder_options["nvenc-hevc"];
    assert_eq!(options["bitrate"], toml::Value::Integer(5000));
    assert_eq!(options["preset"], toml::Value::String("p4".into()));
    assert_eq!(options["lookahead"], toml::Value::Boolean(true));
}

/// One row of `scripts/replay-cost-matrix.ps1`: Instant Replay with the settings in
/// BACKSIGHT_COST_SPEC (see [`spec_settings`]) for BACKSIGHT_EXP_SECONDS or until the file
/// BACKSIGHT_COST_STOP exists, then what came out.
#[test]
#[ignore = "experiment"]
fn run_spec() {
    let Ok(spec) = std::env::var("BACKSIGHT_COST_SPEC") else {
        println!("skipped: set BACKSIGHT_COST_SPEC (see scripts/replay-cost-matrix.ps1)");
        return;
    };
    let seconds: u64 = std::env::var("BACKSIGHT_EXP_SECONDS").unwrap().parse().unwrap();
    let session = ReplaySession::start(&spec_settings(&spec)).unwrap();
    println!("BACKSIGHT_COST_READY");
    // The script ends the run by creating this file, once it has its samples.
    let stop = std::env::var_os("BACKSIGHT_COST_STOP").map(std::path::PathBuf::from);
    let began = std::time::Instant::now();
    while began.elapsed() < Duration::from_secs(seconds) && !stop.as_ref().is_some_and(|f| f.exists()) {
        std::thread::sleep(Duration::from_millis(100));
    }
    let d = session.diagnostics();
    assert_eq!(session.error(), None);
    // The buffer holds sound too (192 Kbps by default), so `kbps` is a little over the video's.
    let kbps = d.buffer_bytes as f64 * 8.0 / 1000.0 / d.buffer.as_secs_f64().max(0.001);
    println!(
        "COST_RESULT kbps={kbps:.0} slots={} repeated={} dropped={} late={} queue={} notice={:?}",
        d.encoded_frames,
        d.pacer.repeated,
        d.pacer.dropped,
        d.late_frames,
        d.encoder_queue,
        session.capture_notice()
    );
}

/// The numbers `monitor=` takes, and whether Windows shows each monitor in HDR right now.
#[test]
#[ignore = "experiment"]
fn list_monitors() {
    for (i, m) in wgc::monitors().iter().enumerate() {
        let hdr = wgc::monitor_hdr_enabled(windows::Win32::Graphics::Gdi::HMONITOR(m.handle as _));
        println!("MONITOR {i}: {} primary={} hdr={hdr:?}", m.label, m.primary);
    }
}

/// The stand-in game for the cost runs: BACKSIGHT_SCENE_SECONDS, BACKSIGHT_SCENE_LOAD (shader loop
/// count, default 40), BACKSIGHT_SCENE_VSYNC=1 for one picture per refresh instead of flat out,
/// and BACKSIGHT_SCENE_MONITOR (a number from `list_monitors`; the main monitor without it).
#[test]
#[ignore = "experiment"]
fn moving_scene() {
    let Ok(seconds) = std::env::var("BACKSIGHT_SCENE_SECONDS") else {
        println!("skipped: set BACKSIGHT_SCENE_SECONDS (see scripts/replay-cost-matrix.ps1)");
        return;
    };
    let load = std::env::var("BACKSIGHT_SCENE_LOAD").map_or(40.0, |v| v.parse().unwrap());
    let vsync = std::env::var("BACKSIGHT_SCENE_VSYNC").is_ok_and(|v| v == "1");
    let monitor =
        std::env::var("BACKSIGHT_SCENE_MONITOR").ok().map(|v| wgc::monitors()[v.parse::<usize>().unwrap()].handle);
    scene::run(monitor, Duration::from_secs(seconds.parse().unwrap()), load, vsync);
}
