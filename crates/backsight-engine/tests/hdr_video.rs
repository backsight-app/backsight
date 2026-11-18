//! HDR video on real monitors (testing.md, tier 2). Every connected monitor gets an HDR session
//! (NVENC HEVC, MP4 and MKV): an HDR monitor must give 10-bit BT.2020 PQ clips and recordings,
//! an SDR one standard video with a plain notice for Home. Run with
//! `cargo test -p backsight-engine --test hdr_video -- --ignored --nocapture --test-threads=1`.
//! Files go to a temp folder and are deleted, unless `BACKSIGHT_HDR_OUT` names a folder to keep them in.

use backsight_config::Settings;
use backsight_engine::{
    capture::{
        target::{HDR_WINDOW, SDR_MONITOR, WAITING_FOR_WINDOW},
        wgc,
    },
    session::ReplaySession,
};
use std::{path::PathBuf, time::Duration};

#[path = "support/replay_probe.rs"]
mod replay_probe;
use replay_probe::ffprobe;

#[path = "support/pattern_window.rs"]
mod pattern_window;

fn folder(name: &str) -> (PathBuf, bool) {
    match std::env::var_os("BACKSIGHT_HDR_OUT") {
        Some(dir) => (PathBuf::from(dir).join(name), true),
        None => (std::env::temp_dir().join(format!("backsight-{name}-{}", std::process::id())), false),
    }
}

fn settings(dir: &std::path::Path, monitor: &str, container: &str, hdr: bool) -> Settings {
    let mut settings = Settings::default();
    settings.general.video_folder = dir.to_string_lossy().into_owned();
    settings.replay.length_seconds = 3;
    settings.audio.desktop = false;
    settings.audio.microphone = false;
    settings.video.encoder = "nvenc-hevc".into();
    settings.video.container = container.into();
    settings.video.resolution = "source".into();
    settings.video.capture_monitor = monitor.into();
    settings.video.hdr = hdr;
    settings
}

fn video_stream(path: &std::path::Path) -> serde_json::Value {
    let info = ffprobe(
        path,
        &[
            "-show_streams",
            "-show_frames",
            "-read_intervals",
            "%+#1",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,profile,pix_fmt,width,height,color_range,color_space,color_transfer,color_primaries:frame=side_data_list",
        ],
    );
    println!("{}: {}", path.display(), info);
    info["streams"][0].clone()
}

#[test]
#[ignore = "needs an NVIDIA GPU, an unlocked desktop and a monitor with Windows HDR on"]
fn hdr_monitor_gives_pq_clips_and_sdr_monitor_standard_video() {
    let mut hdr_monitors = 0;
    for monitor in wgc::monitors() {
        for container in ["mp4", "mkv"] {
            let (dir, keep) =
                folder(&format!("hdr-{container}-{}", monitor.label.replace([' ', '(', ')', '×', ','], "")));
            let mut session = ReplaySession::start(&settings(&dir, &monitor.id, container, true)).unwrap();
            // Each monitor is connected and checked: no notice means HDR, any other is a failure.
            let hdr = session.capture_notice().is_none();
            assert!(hdr || session.capture_notice() == Some(SDR_MONITOR), "{:?}", session.capture_notice());
            println!("{} {container}: HDR {hdr}, notice {:?}", monitor.label, session.capture_notice());
            hdr_monitors += usize::from(hdr);
            session.start_recording().unwrap();
            std::thread::sleep(Duration::from_secs(3));
            let d = session.diagnostics();
            println!(
                "{} {container}: {} frames encoded, {} late, pacer {:?}",
                monitor.label, d.encoded_frames, d.late_frames, d.pacer
            );
            assert!(d.encoded_frames >= 150, "a few seconds at 60 FPS: {} frames", d.encoded_frames);
            let clip = session.save_clip().unwrap();
            let recording = session.stop_recording().unwrap().file.unwrap();
            assert_eq!(session.error(), None);
            drop(session);
            for file in [&clip, &recording] {
                let stream = video_stream(file);
                assert_eq!(stream["codec_name"], "hevc");
                if !hdr {
                    assert_eq!((&stream["pix_fmt"], &stream["color_transfer"]), (&"yuv420p".into(), &"bt709".into()));
                    continue;
                }
                assert_eq!(stream["profile"], "Main 10");
                assert_eq!(stream["pix_fmt"], "yuv420p10le");
                assert_eq!(stream["color_range"], "tv");
                assert_eq!(stream["color_space"], "bt2020nc");
                assert_eq!(stream["color_transfer"], "smpte2084");
                assert_eq!(stream["color_primaries"], "bt2020");
            }
            if !keep {
                std::fs::remove_dir_all(&dir).unwrap();
            }
        }
    }
    assert!(hdr_monitors > 0, "no monitor has Windows HDR on");
}

/// Game window mode with HDR video on records whatever window is kept in front, with something
/// bright in it such as Notepad (nothing is focused or touched): HDR video when that window's monitor shows HDR, standard video with
/// [`HDR_WINDOW`] otherwise. A start that checked the other kind of monitor asks for one restart
/// (`monitor_moved`), as the app does. Run once with a window in front on the HDR monitor and once
/// on an SDR one: `cargo test -p backsight-engine --test hdr_video game_window -- --ignored --nocapture`.
#[test]
#[ignore = "needs an NVIDIA GPU, an unlocked desktop and a bright window (Notepad) kept in front"]
fn game_window_records_hdr_on_an_hdr_monitor() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let (dir, keep) = folder("hdr-window");
    let mut s = settings(&dir, "", "mp4", true);
    s.video.capture_target = "window".into();
    let mut session = ReplaySession::start(&s).unwrap();
    std::thread::sleep(Duration::from_secs(5));
    if session.monitor_moved(&wgc::monitors()) {
        println!("the window in front is on the other kind of monitor: restarting, as the app does");
        let window = session.captured_window();
        drop(session);
        session = ReplaySession::start_with_audio_state(&s, false, false, None, window).unwrap();
        std::thread::sleep(Duration::from_secs(5));
        assert!(!session.monitor_moved(&wgc::monitors()), "a second restart: the window in front moved meanwhile?");
    }
    let notice = session.capture_notice();
    assert_ne!(notice, Some(WAITING_FOR_WINDOW), "keep a window in front for the whole test");
    assert!(session.diagnostics().window_frames > 0, "the window sent a picture, not just black frames");
    let front = wgc::foreground_window().expect("a window in front");
    let hdr = wgc::monitor_hdr_enabled(wgc::window_monitor(front.0 as isize)).unwrap();
    println!("window in front on an {} monitor, notice {notice:?}", if hdr { "HDR" } else { "SDR" });
    assert_eq!(notice, (!hdr).then_some(HDR_WINDOW));
    let clip = session.save_clip().unwrap();
    assert_eq!(session.error(), None);
    drop(session);
    let stream = video_stream(&clip);
    let want = if hdr { ("yuv420p10le", "smpte2084") } else { ("yuv420p", "bt709") };
    assert_eq!((stream["pix_fmt"].as_str(), stream["color_transfer"].as_str()), (Some(want.0), Some(want.1)));
    // `window_frames` counts frames that arrived, not ones that reached the video.
    let black = if hdr { 64.0 } else { 16.0 };
    let brightest = measure(&clip, hdr).iter().map(|p| p[0]).fold(0.0, f64::max);
    assert!(brightest > black * 1.5, "the clip ends on the window, not black (Y {brightest})");
    if !keep {
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

/// The usual setting on an HDR monitor: SDR video. Keeps the files with
/// `BACKSIGHT_HDR_OUT` so a frame can be looked at; only the tags are checked here.
#[test]
#[ignore = "needs an NVIDIA GPU and an unlocked desktop"]
fn sdr_video_on_every_monitor() {
    for monitor in wgc::monitors() {
        let (dir, keep) = folder(&format!("sdr-{}", monitor.label.replace([' ', '(', ')', '×', ','], "")));
        let session = ReplaySession::start(&settings(&dir, &monitor.id, "mp4", false)).unwrap();
        std::thread::sleep(Duration::from_secs(3));
        let clip = session.save_clip().unwrap();
        assert_eq!(session.error(), None);
        drop(session);
        let stream = video_stream(&clip);
        assert_eq!(stream["pix_fmt"], "yuv420p");
        assert_eq!(stream["color_transfer"], "bt709");
        if !keep {
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }
}

/// ST 2084 (PQ) signal, 0 to 1, for absolute light in nits.
fn pq(nits: f64) -> f64 {
    let y = (nits / 10_000.0).powf(0.159_301_757_812_5);
    ((0.835_937_5 + 18.851_562_5 * y) / (1.0 + 18.687_5 * y)).powf(78.843_75)
}

/// The calibrated pattern: two rows of four flat patches, as R, G, B signals (0 to 1) for the
/// swap chain. HDR: PQ greys, then the BT.2020 primaries at the 203-nit signal and one more grey.
/// SDR: sRGB greys and the BT.709 primaries.
fn patches(hdr: bool) -> [(&'static str, [f64; 3]); 8] {
    if hdr {
        let (g, p) = (|nits| [pq(nits); 3], pq(203.0));
        [
            ("0 nits", g(0.0)),
            ("100 nits", g(100.0)),
            ("203 nits", g(203.0)),
            ("1000 nits", g(1000.0)),
            ("BT.2020 red", [p, 0.0, 0.0]),
            ("BT.2020 green", [0.0, p, 0.0]),
            ("BT.2020 blue", [0.0, 0.0, p]),
            ("400 nits", g(400.0)),
        ]
    } else {
        [
            ("black", [0.0; 3]),
            ("1/3 grey", [1.0 / 3.0; 3]),
            ("2/3 grey", [2.0 / 3.0; 3]),
            ("white", [1.0; 3]),
            ("red", [1.0, 0.0, 0.0]),
            ("green", [0.0, 1.0, 0.0]),
            ("blue", [0.0, 0.0, 1.0]),
            ("half grey", [0.5; 3]),
        ]
    }
}

/// The swap chain's 10-bit codes for a signal.
fn code(signal: f64) -> u32 {
    (signal * 1023.0).round() as u32
}

/// What Backsight should write for a patch: limited-range Y, Cb, Cr, as 10-bit BT.2020 codes for
/// HDR, 8-bit BT.709 for SDR. PQ survives the trip (DWM decodes it to linear scRGB, Backsight
/// encodes it again), so the drawn signal is the expected signal. SDR passes through 8 bits.
fn expected(hdr: bool, rgb: [f64; 3]) -> [f64; 3] {
    let mut s = rgb.map(|x| f64::from(code(x)) / 1023.0);
    if !hdr {
        s = s.map(|x| (x * 255.0).round() / 255.0);
    }
    let (kr, kb) = if hdr { (0.2627, 0.0593) } else { (0.2126, 0.0722) };
    let y = kr * s[0] + (1.0 - kr - kb) * s[1] + kb * s[2];
    let (cb, cr) = ((s[2] - y) / (2.0 * (1.0 - kb)), (s[0] - y) / (2.0 * (1.0 - kr)));
    if hdr {
        [64.0 + 876.0 * y, 512.0 + 896.0 * cb, 512.0 + 896.0 * cr]
    } else {
        [16.0 + 219.0 * y, 128.0 + 224.0 * cb, 128.0 + 224.0 * cr]
    }
}

/// Largest allowed difference of a patch's mean from [`expected`]. SDR: 2 of 219 8-bit steps, in
/// output codes. HDR is compared in light instead ([`hdr_off`]): PQ is so steep near black that
/// 0.01 nits of green in a 200-nit red (DWM's FP16 maths) moves its codes by 15.
const HDR_TOLERANCE: f64 = 1.0;
const SDR_TOLERANCE: f64 = 2.0;

/// R, G, B light in nits from limited-range 10-bit BT.2020 PQ Y, Cb, Cr.
fn nits([y, cb, cr]: [f64; 3]) -> [f64; 3] {
    let (y, cb, cr) = ((y - 64.0) / 876.0, (cb - 512.0) / 896.0, (cr - 512.0) / 896.0);
    let (r, b) = (y + 1.4746 * cr, y + 1.8814 * cb);
    let g = (y - 0.2627 * r - 0.0593 * b) / 0.6780;
    // A negative signal (chroma pushed past the gamut) decodes to negative light, so it counts as
    // an error instead of vanishing into 0.
    [r, g, b].map(|e: f64| {
        let p = e.abs().min(1.0).powf(1.0 / 78.843_75);
        let n = 10_000.0 * ((p - 0.835_937_5).max(0.0) / (18.851_562_5 - 18.6875 * p)).powf(1.0 / 0.159_301_757_812_5);
        n.copysign(e)
    })
}

/// An HDR patch's worst channel error as a share of what that channel is allowed: 3% of its own
/// light (about 3 PQ steps near 1000 nits) plus 0.02 nits, so a primary's dark channels and
/// black stay tight. 1.0 is the limit.
fn hdr_off(want: [f64; 3], got: [f64; 3]) -> f64 {
    let (want, got) = (nits(want), nits(got));
    want.iter().zip(got).map(|(w, g)| (g - w).abs() / (0.03 * w.abs() + 0.02)).fold(0.0, f64::max)
}

/// The pattern as packed R10G10B10A2 pixels (red in the low bits, opaque).
fn pattern_pixels(hdr: bool, (w, h): (u32, u32)) -> Vec<u32> {
    let patches = patches(hdr);
    (0..h)
        .flat_map(|y| (0..w).map(move |x| (y * 2 / h * 4 + x * 4 / w) as usize))
        .map(|i| {
            let [r, g, b] = patches[i].1.map(code);
            r | g << 10 | b << 20 | 3 << 30
        })
        .collect()
}

/// Mean Y, Cb, Cr over the centre half of every patch in the clip's last frame.
fn measure(clip: &std::path::Path, hdr: bool) -> Vec<[f64; 3]> {
    let stream = video_stream(clip);
    let (w, h) = (stream["width"].as_u64().unwrap() as usize, stream["height"].as_u64().unwrap() as usize);
    let format = if hdr { "yuv420p10le" } else { "yuv420p" };
    assert_eq!(stream["pix_fmt"], format, "the decoder hands the planes over unconverted");
    let exe = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party/ffmpeg/bin/ffmpeg.exe");
    let out = std::process::Command::new(exe)
        .args(["-v", "error", "-sseof", "-0.5", "-i"])
        .arg(clip)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", format, "-"])
        .output()
        .unwrap();
    assert!(out.status.success(), "ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr));
    let samples: Vec<f64> = if hdr {
        out.stdout.as_chunks::<2>().0.iter().map(|b| f64::from(u16::from_le_bytes(*b))).collect()
    } else {
        out.stdout.iter().map(|&b| f64::from(b)).collect()
    };
    assert_eq!(samples.len(), w * h * 3 / 2, "one decoded frame");
    let (y, uv) = samples.split_at(w * h);
    let (u, v) = uv.split_at(w * h / 4);
    let mean = |plane: &[f64], stride: usize, col: usize, row: usize| {
        let rows = plane.len() / stride;
        let (x0, x1) = ((4 * col + 1) * stride / 16, (4 * col + 3) * stride / 16);
        let (y0, y1) = ((4 * row + 1) * rows / 8, (4 * row + 3) * rows / 8);
        (y0..y1).flat_map(|r| &plane[r * stride + x0..r * stride + x1]).sum::<f64>() / ((x1 - x0) * (y1 - y0)) as f64
    };
    (0..8).map(|i| [mean(y, w, i % 4, i / 4), mean(u, w / 2, i % 4, i / 4), mean(v, w / 2, i % 4, i / 4)]).collect()
}

#[test]
fn calibrated_pattern_maths() {
    assert!((pq(10_000.0) - 1.0).abs() < 1e-12 && pq(0.0) < 1e-6);
    // BT.2408: 203-nit HDR reference white is a 58% PQ signal.
    assert!((pq(203.0) - 0.58).abs() < 0.005);
    // The same codes `capture::hdr` works out from scRGB, within the swap chain's 10-bit rounding:
    // 80 and 1000 nits grey, BT.2020 red at 80 nits.
    let near = |got: [f64; 3], want: [f64; 3]| got.iter().zip(want).all(|(g, w)| (g - w).abs() < 1.0);
    assert!(near(expected(true, [pq(80.0); 3]), [490.0, 512.0, 512.0]));
    assert!(near(expected(true, [pq(1000.0); 3]), [723.0, 512.0, 512.0]));
    assert!(near(expected(true, [pq(80.0), 0.0, 0.0]), [176.0, 451.0, 730.0]));
    // In light, colours included: grey and the primaries decode to what was drawn (within the
    // swap chain's 10-bit rounding).
    let close = |got: [f64; 3], want: [f64; 3]| got.iter().zip(want).all(|(g, w)| (g - w).abs() <= 0.01 * w + 0.01);
    assert!(close(nits(expected(true, [pq(1000.0); 3])), [1000.0; 3]));
    let red = expected(true, [pq(203.0), 0.0, 0.0]);
    assert!(close(nits(red), [203.0, 0.0, 0.0]), "{:?}", nits(red));
    assert!(close(nits(expected(true, [0.0, pq(203.0), 0.0])), [0.0, 203.0, 0.0]));
    assert!(close(nits(expected(true, [0.0, 0.0, pq(203.0)])), [0.0, 0.0, 203.0]));
    // The HDR test monitor's 2026-11-16 red (0.013 nits of green in 205 nits of red, 15 codes off) and a red 2%
    // dim pass; 5% dim, 1 nit of green in it, or black lifted to 0.03 nits don't.
    let off = |got| hdr_off(red, got);
    assert!(off([213.0, 435.0, 762.0]) < HDR_TOLERANCE);
    assert!(off(expected(true, [pq(203.0 * 0.98), 0.0, 0.0])) < HDR_TOLERANCE);
    assert!(off(expected(true, [pq(203.0 * 0.95), 0.0, 0.0])) > HDR_TOLERANCE);
    assert!(off(expected(true, [pq(203.0), pq(1.0), 0.0])) > HDR_TOLERANCE);
    assert!(hdr_off(expected(true, [0.0; 3]), expected(true, [pq(0.03); 3])) > HDR_TOLERANCE);
    assert_eq!(expected(false, [0.0; 3]), [16.0, 128.0, 128.0]);
    assert_eq!(expected(false, [1.0; 3]), [235.0, 128.0, 128.0]);
    // Patch i fills column i % 4 of row i / 4.
    let pixels = pattern_pixels(true, (8, 4));
    assert_eq!(pixels[0], 3 << 30, "0 nits, opaque");
    let c = code(pq(400.0));
    assert_eq!(pixels[3 * 8 + 7], c | c << 10 | c << 20 | 3 << 30, "400 nits, bottom right");
    assert_eq!(pixels[2 * 8] & 0x3ff, code(pq(203.0)), "BT.2020 red");
    assert_eq!(pixels[2 * 8] >> 10 & 0xfffff, 0);
}

/// The calibrated check: a window of known PQ patches covers each monitor (or the one numbered
/// `BACKSIGHT_HDR_MONITOR`, as in "Monitor 2"), Backsight records it with HDR video on, and the
/// decoded 10-bit codes must match the drawn ones. A monitor without Windows HDR gets sRGB
/// patches and must give the matching standard video instead. Takes over each screen for about
/// 6 seconds. See testing.md, "HDR".
#[test]
#[ignore = "needs an NVIDIA GPU and an unlocked desktop; covers each monitor with a test pattern"]
fn calibrated_pattern_comes_back_with_the_drawn_codes() {
    let only = std::env::var("BACKSIGHT_HDR_MONITOR").ok().map(|n| format!("Monitor {n} "));
    let monitors: Vec<_> =
        wgc::monitors().into_iter().filter(|m| only.as_ref().is_none_or(|n| m.label.starts_with(n))).collect();
    assert!(!monitors.is_empty(), "no monitor matches BACKSIGHT_HDR_MONITOR={only:?}");
    let (mut failures, mut checked) = (Vec::new(), [0, 0]);
    for monitor in monitors {
        let (dir, keep) = folder(&format!("pattern-{}", monitor.label.replace([' ', '(', ')', '×', ','], "")));
        let window = pattern_window::PatternWindow::open(monitor.handle, pattern_pixels);
        let hdr = window.hdr;
        window.show_for(Duration::from_secs(1));
        let mut s = settings(&dir, &monitor.id, "mp4", true);
        s.video.capture_cursor = false;
        let session = ReplaySession::start(&s).unwrap();
        let notice = session.capture_notice();
        window.show_for(Duration::from_secs(4));
        let clip = session.save_clip();
        assert_eq!(session.error(), None);
        drop(session);
        drop(window);
        let clip = clip.unwrap();
        assert_eq!(notice, (!hdr).then_some(SDR_MONITOR), "{}: Backsight and DXGI disagree on HDR", monitor.label);

        let tolerance = if hdr { HDR_TOLERANCE } else { SDR_TOLERANCE };
        let unit = if hdr { "of the allowed light error" } else { "codes" };
        println!(
            "{} ({}), tolerance {tolerance} {unit}:",
            monitor.label,
            if hdr { "HDR, 10-bit" } else { "SDR, 8-bit" }
        );
        for ((name, rgb), got) in patches(hdr).into_iter().zip(measure(&clip, hdr)) {
            let want = expected(hdr, rgb);
            let worst = if hdr {
                hdr_off(want, got)
            } else {
                got.iter().zip(want).map(|(g, w)| (g - w).abs()).fold(0.0, f64::max)
            };
            let line = format!(
                "  {name:<14} want Y {:6.1} Cb {:6.1} Cr {:6.1}  got {:6.1} {:6.1} {:6.1}  off {worst:.1}",
                want[0], want[1], want[2], got[0], got[1], got[2]
            );
            println!("{line}");
            if worst > tolerance {
                failures.push(format!("{}:{line}", monitor.label));
            }
        }
        checked[usize::from(hdr)] += 1;
        if !keep {
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }
    if checked[1] == 0 {
        println!("HDR part skipped: no monitor checked has Windows HDR on. Turn it on in Windows display settings.");
    }
    if checked[0] == 0 {
        println!("SDR part skipped: every monitor checked has Windows HDR on.");
    }
    assert!(failures.is_empty(), "patches off by more than the tolerance:\n{}", failures.join("\n"));
}

/// A JPEG XR (HDR) and a PNG screenshot of the monitor in front, to compare video frames with.
#[test]
#[ignore = "needs an unlocked desktop; only useful with BACKSIGHT_HDR_OUT"]
fn screenshots_to_compare_with() {
    let (dir, keep) = folder("shots");
    let mut settings = Settings::default();
    settings.general.screenshot_folder = dir.to_string_lossy().into_owned();
    settings.screenshot.copy_to_clipboard = false;
    for format in ["jxr", "png"] {
        settings.screenshot.format = format.into();
        println!("{}", backsight_engine::screenshot::take(&settings).unwrap().path.display());
    }
    if !keep {
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
