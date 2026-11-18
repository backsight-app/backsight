//! Instant Replay end to end on real hardware (testing.md, tier 2): the default settings
//! (NVENC H.264, fragmented MP4, 60 fps, desktop audio) capture for a few seconds, then a saved
//! clip must have both tracks, start on a keyframe with audio at its start, and save fast.
//! Recordings are checked the same way, including pause, resume and split files.
//! (Whether the session maps audio and video onto one clock is the A/V sync pattern test.) Run with
//! `cargo test -p backsight-engine --test replay_session -- --ignored --nocapture`.

use backsight_config::Settings;
use backsight_engine::{capture::wgc, session::ReplaySession};
use std::{
    process::Command,
    time::{Duration, Instant},
};

#[path = "support/replay_probe.rs"]
mod replay_probe;
use replay_probe::ffprobe;

#[test]
#[ignore = "needs an NVIDIA GPU, an unlocked desktop and an audio output"]
fn saves_a_clip_with_sound_from_a_running_session() {
    let qualify = std::env::var("BACKSIGHT_QUALIFY_M1").as_deref() == Ok("1");
    let (length, seconds) = if qualify { (120, 125) } else { (3, 5) };
    let dir = std::env::temp_dir().join(format!("backsight-session-{}", std::process::id()));
    let mut settings = Settings::default();
    settings.general.video_folder = dir.to_string_lossy().into_owned();
    settings.replay.length_seconds = length;
    if qualify {
        settings.video.resolution = "1080p".into();
    }
    let session = ReplaySession::start(&settings).unwrap();
    std::thread::sleep(Duration::from_secs(seconds));

    // Sample before saving: a clip snapshot retains chunks outside the live buffer.
    let memory = qualify.then(|| {
        let query = format!(
            "(Get-CimInstance Win32_PerfFormattedData_PerfProc_Process -Filter 'IDProcess={}').WorkingSetPrivate",
            std::process::id()
        );
        let out = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &query])
            .output()
            .expect("query test-process private working set");
        assert!(out.status.success(), "memory query failed: {}", String::from_utf8_lossy(&out.stderr));
        let bytes: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().expect("one private working set value");
        (bytes, session.diagnostics().buffer_bytes)
    });

    let started = Instant::now();
    let saved = session.save_clip().unwrap();
    let save_ms = started.elapsed().as_secs_f64() * 1000.0;
    println!("saved {} in {save_ms:.3} ms", saved.display());
    if let Some((private_working_set_bytes, buffer_bytes)) = memory {
        println!(
            "BACKSIGHT_M1_METRICS {}",
            serde_json::json!({
                "resolution_setting": "1080p", "fps_setting": settings.video.fps,
                "requested_clip_seconds": length, "capture_seconds": seconds,
                "save_ms": save_ms, "private_working_set_bytes": private_working_set_bytes,
                "buffer_bytes": buffer_bytes,
                "scope": "test process before save; not engine-only RAM qualification"
            })
        );
    }
    let second = session.save_clip().unwrap();
    assert_ne!(saved, second, "a second save in the same second gets its own name");
    let d = session.diagnostics();
    println!("{d:#?}");
    assert_eq!(session.error(), None);
    assert!(d.encoded_frames >= (seconds - 1) * 60, "encoded only {} frames in {seconds} s", d.encoded_frames);
    // The diagnostics counters move as they should on real hardware.
    assert!(d.encoder_queue < 60, "{} frames stuck in the encoder", d.encoder_queue);
    assert!(
        d.buffer >= Duration::from_secs(length.into()) && d.buffer_bytes > 0,
        "buffer {:?}, {} B",
        d.buffer,
        d.buffer_bytes
    );
    let audio = d.audio.as_ref().expect("desktop audio is on by default");
    assert!(audio.last_error.is_none(), "{audio:?}");
    drop(session);

    let info = ffprobe(&saved, &["-show_entries", "stream=codec_type,codec_name,start_time:format=duration"]);
    let streams = info["streams"].as_array().unwrap();
    let start = |kind: &str| -> f64 {
        let s = streams.iter().find(|s| s["codec_type"] == kind).unwrap_or_else(|| panic!("no {kind} track"));
        s["start_time"].as_str().unwrap().parse().unwrap()
    };
    let (v, a) = (start("video"), start("audio"));
    println!("video starts {v}, audio {a}");
    // Both tracks start at the keyframe's decode time: audio within one AAC frame of it, video
    // a whole number of frames later (the B-frame delay). The A/V sync pattern test measures
    // sync itself.
    assert!((0.0..0.022).contains(&a), "audio starts at {a}");
    let frames = v * 60.0;
    assert!(v < 0.2 && (frames - frames.round()).abs() < 0.1, "video starts at {v}");
    let duration: f64 = info["format"]["duration"].as_str().unwrap().parse().unwrap();
    assert!((f64::from(length) - 0.05..f64::from(length) + 2.1).contains(&duration), "duration {duration}");
    if qualify {
        let video = ffprobe(
            &saved,
            &["-select_streams", "v:0", "-show_entries", "stream=width,height,r_frame_rate:format=duration"],
        );
        replay_probe::require_two_minute_1080p60(&video);
        assert!(save_ms < 1000.0, "two-minute clip save took {save_ms:.3} ms");
    }
    let first =
        ffprobe(&saved, &["-select_streams", "v:0", "-read_intervals", "%+#1", "-show_entries", "frame=key_frame"]);
    assert_eq!(first["frames"][0]["key_frame"], 1, "starts on a keyframe");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "needs an NVIDIA GPU, an unlocked desktop and an audio output"]
fn records_while_replay_is_toggled_and_starts_on_a_keyframe() {
    let dir = std::env::temp_dir().join(format!("backsight-recording-{}", std::process::id()));
    let mut settings = Settings::default();
    settings.general.video_folder = dir.to_string_lossy().into_owned();
    let mut session = ReplaySession::start(&settings).unwrap();
    std::thread::sleep(Duration::from_secs(3));
    session.start_recording().unwrap();
    assert!(session.save_clip().is_ok(), "replay keeps working while recording");
    std::thread::sleep(Duration::from_secs(2));
    session.set_replay(false); // turning replay off must not stop the recording
    assert!(session.save_clip().is_err());
    std::thread::sleep(Duration::from_secs(2));
    session.set_replay(true);
    std::thread::sleep(Duration::from_secs(1));
    assert!(session.recording_time().unwrap() >= Duration::from_secs(5));
    let stopped = session.stop_recording().expect("a recording was running");
    assert_eq!(stopped.stopped_early, None);
    let path = stopped.file.unwrap();
    assert_eq!(session.error(), None);
    drop(session);

    assert_eq!(path.extension().unwrap(), "mp4");
    let info = ffprobe(&path, &["-show_entries", "stream=codec_type,start_time:format=duration"]);
    let streams = info["streams"].as_array().unwrap();
    assert!(streams.iter().any(|s| s["codec_type"] == "audio"), "has sound");
    let duration: f64 = info["format"]["duration"].as_str().unwrap().parse().unwrap();
    println!("recorded {} ({duration} s)", path.display());
    // 5 s of pressing, plus up to one 2 s GOP from before the press.
    assert!((4.9..7.6).contains(&duration), "duration {duration}");
    let first =
        ffprobe(&path, &["-select_streams", "v:0", "-read_intervals", "%+#1", "-show_entries", "frame=key_frame"]);
    assert_eq!(first["frames"][0]["key_frame"], 1, "starts on a keyframe");
    // No gaps: video packets are one frame apart all the way through.
    let packets = ffprobe(&path, &["-select_streams", "v:0", "-show_entries", "packet=dts_time"]);
    let mut dts: Vec<f64> = packets["packets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["dts_time"].as_str().unwrap().parse().unwrap())
        .collect();
    dts.sort_by(f64::total_cmp);
    let gap = dts.windows(2).map(|w| w[1] - w[0]).fold(0.0, f64::max);
    assert!(gap < 0.1, "largest gap between video packets: {gap} s");
}

#[test]
#[ignore = "needs an NVIDIA GPU, an unlocked desktop and an audio output"]
fn records_three_tracks_even_when_the_microphone_is_missing() {
    let dir = std::env::temp_dir().join(format!("backsight-audio-tracks-{}", std::process::id()));
    let mut settings = Settings::default();
    settings.general.video_folder = dir.to_string_lossy().into_owned();
    settings.audio.microphone = true;
    settings.audio.microphone_device = "backsight-test-missing-endpoint".into();
    settings.audio.separate_tracks = true;
    let mut session = ReplaySession::start(&settings).unwrap();
    session.start_recording().unwrap();
    std::thread::sleep(Duration::from_secs(4));
    assert!(session.diagnostics().audio_problem.is_some(), "the missing microphone is reported");
    assert_eq!(session.error(), None, "video keeps recording");
    let stopped = session.stop_recording().unwrap();
    assert_eq!(stopped.stopped_early, None);
    let save = session.prepare_clip().unwrap();
    drop(session);
    let replay = save.save().unwrap();
    for path in [stopped.file.unwrap(), replay] {
        let info = ffprobe(&path, &["-select_streams", "a", "-show_entries", "stream=codec_type"]);
        assert_eq!(info["streams"].as_array().unwrap().len(), 3, "{}", path.display());
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "needs an NVIDIA GPU, an unlocked desktop and an audio output; takes about 75 s"]
fn pauses_and_splits_a_recording_on_keyframes() {
    let dir = std::env::temp_dir().join(format!("backsight-pause-split-{}", std::process::id()));
    let mut settings = Settings::default();
    settings.general.video_folder = dir.to_string_lossy().into_owned();
    settings.video.split_minutes = 1;
    let mut session = ReplaySession::start(&settings).unwrap();
    std::thread::sleep(Duration::from_secs(3));
    session.start_recording().unwrap();
    std::thread::sleep(Duration::from_secs(3));
    session.pause_recording();
    assert!(session.recording_paused());
    std::thread::sleep(Duration::from_secs(5));
    session.resume_recording();
    assert!(!session.recording_paused());
    std::thread::sleep(Duration::from_secs(2));
    session.pause_recording(); // a short one, inside the encoder's delay
    std::thread::sleep(Duration::from_millis(30));
    session.resume_recording();
    std::thread::sleep(Duration::from_secs(60)); // past the one-minute split
    session.pause_recording();
    std::thread::sleep(Duration::from_secs(2));
    let time = session.recording_time().unwrap();
    assert!((Duration::from_secs(64)..Duration::from_secs(68)).contains(&time), "recording time {time:?}");
    let stopped = session.stop_recording().expect("a recording was running"); // stop while paused
    assert_eq!(stopped.stopped_early, None);
    let last = stopped.file.unwrap();
    assert_eq!(session.error(), None);
    drop(session);

    let parts: Vec<_> = stopped.earlier_parts.iter().chain([&last]).collect();
    assert_eq!(parts.len(), 2, "{parts:?}");
    assert!(parts.windows(2).all(|w| w[0] < w[1]), "part names sort in order: {parts:?}");
    let mut total = 0.0;
    for part in &parts {
        let info = ffprobe(part, &["-show_entries", "stream=codec_type,start_time:format=duration"]);
        let streams = info["streams"].as_array().unwrap();
        let start = |kind: &str| -> f64 {
            let s = streams.iter().find(|s| s["codec_type"] == kind).unwrap_or_else(|| panic!("no {kind} track"));
            s["start_time"].as_str().unwrap().parse().unwrap()
        };
        let (v, a) = (start("video"), start("audio"));
        assert!((0.0..0.022).contains(&a), "{}: audio starts at {a}", part.display());
        assert!(v < 0.2, "{}: video starts at {v}", part.display());
        let first =
            ffprobe(part, &["-select_streams", "v:0", "-read_intervals", "%+#1", "-show_entries", "frame=key_frame"]);
        assert_eq!(first["frames"][0]["key_frame"], 1, "{} starts on a keyframe", part.display());
        let packets = ffprobe(part, &["-select_streams", "v:0", "-show_entries", "packet=pts_time"]);
        let mut pts: Vec<f64> = packets["packets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["pts_time"].as_str().unwrap().parse().unwrap())
            .collect();
        pts.sort_by(f64::total_cmp);
        let gap = pts.windows(2).map(|w| w[1] - w[0]).fold(0.0, f64::max);
        assert!(gap < 0.1, "{}: largest gap between frames {gap} s", part.display());
        let duration: f64 = info["format"]["duration"].as_str().unwrap().parse().unwrap();
        println!("{} ({duration} s)", part.display());
        total += duration;
    }
    // 65 s recorded (5 s and 2 s paused), plus up to a 2 s GOP from before the press.
    assert!((64.5..68.0).contains(&total), "{total} s in all");
    // The reported time counts that lead-in too, so marker times match the file.
    assert!((total - time.as_secs_f64()).abs() < 0.5, "recording time {time:?}, file {total} s");
    std::fs::remove_dir_all(dir).unwrap();
}

/// A long real recording (30 minutes unless `BACKSIGHT_LONG_MINUTES` says otherwise)
/// keeps every frame, never skips a frame or jumps the audio timeline, and has both tracks the
/// full length. Track ends can't show drift (the timeline pins audio to the QPC clock); the
/// timeline counters can. Writes several GB to the temp folder, removed even on failure.
/// `scripts/hardware-check.ps1` skips it. For the flash-to-beep check, `BACKSIGHT_LONG_MONITOR`
/// picks the monitor whose label contains it ("1920") and `BACKSIGHT_LONG_OUT` keeps a copy.
// shortcut: no flash-to-beep check on real devices; the desktop audio here is mostly silence
// fill, so add a played beep pattern when a real-device sync test is written.
#[test]
#[ignore = "needs an NVIDIA GPU, an unlocked desktop, an audio output and 30 minutes"]
fn a_long_recording_keeps_audio_and_video_together() {
    let minutes: u64 = std::env::var("BACKSIGHT_LONG_MINUTES").map_or(30, |m| m.parse().unwrap());
    struct Remove(std::path::PathBuf);
    impl Drop for Remove {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let dir = Remove(std::env::temp_dir().join(format!("backsight-long-{}", std::process::id())));
    let dir = &dir.0;
    let mut settings = Settings::default();
    settings.general.video_folder = dir.to_string_lossy().into_owned();
    settings.video.split_minutes = 0;
    if let Ok(want) = std::env::var("BACKSIGHT_LONG_MONITOR") {
        let monitors = backsight_engine::capture::wgc::monitors();
        let m =
            monitors.iter().find(|m| m.label.contains(&want)).unwrap_or_else(|| panic!("no {want} in {monitors:?}"));
        settings.video.capture_monitor = m.id.clone();
    }
    let mut session = ReplaySession::start(&settings).unwrap();
    std::thread::sleep(Duration::from_secs(3));
    session.start_recording().unwrap();
    std::thread::sleep(Duration::from_secs(minutes * 60));
    let diag = session.diagnostics();
    let audio = diag.audio.as_ref().expect("desktop audio stats");
    println!("pacer {:?}, late {}, audio {:?}", diag.pacer, diag.late_frames, audio.timeline);
    assert_eq!(diag.pacer.skipped, 0, "skipped frames");
    assert_eq!(audio.timeline.jumped_frames, 0, "the audio timeline jumped (a stall or drift)");
    let stopped = session.stop_recording().expect("a recording was running");
    assert_eq!(stopped.stopped_early, None);
    let path = stopped.file.unwrap();
    assert_eq!(session.error(), None);
    drop(session);
    if let Ok(out) = std::env::var("BACKSIGHT_LONG_OUT") {
        std::fs::copy(&path, &out).unwrap();
    }

    let info = ffprobe(&path, &["-show_entries", "stream=codec_type,start_time,duration:format=duration"]);
    let streams = info["streams"].as_array().unwrap();
    let span = |kind: &str| -> (f64, f64) {
        let s = streams.iter().find(|s| s["codec_type"] == kind).unwrap_or_else(|| panic!("no {kind} track"));
        let f = |k: &str| s[k].as_str().unwrap().parse::<f64>().unwrap();
        (f("start_time"), f("start_time") + f("duration"))
    };
    let ((v0, v1), (a0, a1)) = (span("video"), span("audio"));
    let total: f64 = info["format"]["duration"].as_str().unwrap().parse().unwrap();
    println!("{} ({total} s): video {v0}..{v1}, audio {a0}..{a1}", path.display());
    let wanted = (minutes * 60) as f64;
    assert!((wanted - 1.0..wanted + 3.0).contains(&total), "{total} s for {wanted} s pressed");
    // The ends differ by when each track's last packet landed before the stop (up to 0.2 s).
    assert!((a1 - v1).abs() < 0.2, "audio ends {:.4} s from video", a1 - v1);
    let packets = ffprobe(&path, &["-select_streams", "v:0", "-show_entries", "packet=dts_time"]);
    let mut dts: Vec<f64> = packets["packets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["dts_time"].as_str().unwrap().parse().unwrap())
        .collect();
    dts.sort_by(f64::total_cmp);
    let gap = dts.windows(2).map(|w| w[1] - w[0]).fold(0.0, f64::max);
    assert!(gap < 0.1, "largest gap between video packets: {gap} s");
}

/// On a still screen most frames are left out and the encoder hands over what it holds (session.rs,
/// `still_slot`). Nothing of that may show in a clip: every frame decodes, no gap reaches 0.1 s
/// and sound and picture end together, with 0 to 4 B-frames. The test moves the mouse pointer on
/// the captured monitor between still spells (bursts of motion, single moves, two moves). Every run
/// after a rest has no gap in it, so this checks rests end to end but not the decode-time clamp
/// (a gap comes only from the pacer's stall jump; the unit model in backsight-encode covers that).
/// Leave the screen alone otherwise.
#[test]
#[ignore = "needs an NVIDIA GPU, an unlocked desktop and an audio output"]
fn a_still_screen_rests_the_encoder_and_the_clip_plays_through() {
    use windows::Win32::{
        Foundation::POINT,
        Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFO},
        UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos},
    };
    let monitors = wgc::monitors();
    let monitor = match std::env::var("BACKSIGHT_STILL_MONITOR") {
        Ok(i) => monitors[i.parse::<usize>().unwrap()].clone(),
        Err(_) => monitors.iter().find(|m| m.primary).unwrap().clone(),
    };
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: `info` is a valid local with `cbSize` set.
    assert!(unsafe { GetMonitorInfoW(HMONITOR(monitor.handle as _), &mut info) }.as_bool());
    let r = info.rcMonitor;
    let (cx, cy) = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
    let mut before = POINT::default();
    // SAFETY: `before` is a valid local.
    unsafe { GetCursorPos(&mut before) }.unwrap();
    let mut step = 0;
    let mut move_pointer = || {
        step += 1;
        // SAFETY: plain call; a refused move only leaves the pointer where it is.
        let _ = unsafe { SetCursorPos(cx + (step % 40) * 4, cy) };
    };
    for bframes in [0, 1, 2, 3, 4] {
        let dir = std::env::temp_dir().join(format!("backsight-still-{}-{bframes}", std::process::id()));
        let mut settings = Settings::default();
        settings.general.video_folder = dir.to_string_lossy().into_owned();
        settings.replay.length_seconds = 15;
        // Anything animating on the main monitor (an animated border, a video) keeps the encoder busy: pick a quieter monitor by number, as in `list_monitors`.
        settings.video.capture_monitor = monitor.id.clone();
        settings.video.capture_cursor = true;
        let options = settings.video.encoder_options.entry(settings.video.encoder.clone()).or_default();
        options.insert("bframes".into(), toml::Value::Integer(bframes));
        let session = ReplaySession::start(&settings).unwrap();
        let sleep = |ms| std::thread::sleep(Duration::from_millis(ms));
        sleep(2_000);
        for _ in 0..4 {
            for _ in 0..30 {
                move_pointer();
                sleep(16);
            }
            sleep(500);
            move_pointer();
            sleep(400);
            move_pointer();
            sleep(17);
            move_pointer();
            sleep(400);
        }
        sleep(1_000);
        let saved = session.save_clip().unwrap();
        let d = session.diagnostics();
        assert_eq!(session.error(), None);
        println!(
            "{bframes} B-frames: {} of {} frames left out, {} in the encoder",
            d.rested_frames, d.encoded_frames, d.encoder_queue
        );
        assert!(d.encoder_queue < 60, "{} frames stuck in the encoder", d.encoder_queue);
        assert!(d.rested_frames > 0, "{bframes} B-frames: the encoder never rested: was something moving on screen?");
        drop(session);

        let ffmpeg = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party/ffmpeg/bin/ffmpeg.exe");
        let out =
            Command::new(ffmpeg).args(["-v", "error", "-i"]).arg(&saved).args(["-f", "null", "-"]).output().unwrap();
        let errors = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success() && errors.trim().is_empty(), "the clip doesn't decode cleanly: {errors}");

        let info = ffprobe(&saved, &["-show_entries", "stream=codec_type,start_time,duration,r_frame_rate"]);
        let end = |kind: &str| -> f64 {
            let streams = info["streams"].as_array().unwrap();
            let s = streams.iter().find(|s| s["codec_type"] == kind).unwrap_or_else(|| panic!("no {kind} track"));
            let f = |k: &str| s[k].as_str().unwrap().parse::<f64>().unwrap();
            f("start_time") + f("duration")
        };
        let (v, a) = (end("video"), end("audio"));
        let video = info["streams"].as_array().unwrap().iter().find(|s| s["codec_type"] == "video").unwrap();
        // The Library shows this; a still desktop's even 15-a-second spacing mustn't read as 15 FPS.
        assert_eq!(video["r_frame_rate"], "60/1", "{bframes} B-frames: frame rate");
        assert!((a - v).abs() < 0.2, "{bframes} B-frames: audio ends {:.4} s from video", a - v);
        let packets = ffprobe(&saved, &["-select_streams", "v:0", "-show_entries", "packet=dts_time,pts_time"]);
        let packets = packets["packets"].as_array().unwrap();
        let time = |p: &serde_json::Value, k: &str| p[k].as_str().unwrap().parse::<f64>().unwrap();
        let mut pts: Vec<f64> = packets.iter().map(|p| time(p, "pts_time")).collect();
        pts.sort_by(f64::total_cmp);
        let gap = pts.windows(2).map(|w| w[1] - w[0]).fold(0.0, f64::max);
        assert!(gap < 0.1, "{bframes} B-frames: largest gap between pictures {gap} s");
        assert!(packets.windows(2).all(|w| time(&w[0], "dts_time") < time(&w[1], "dts_time")), "decode times rise");
        assert!(packets.iter().all(|p| time(p, "dts_time") <= time(p, "pts_time")), "decoded before shown");
        std::fs::remove_dir_all(dir).unwrap();
    }
    // SAFETY: plain call.
    let _ = unsafe { SetCursorPos(before.x, before.y) };
}
