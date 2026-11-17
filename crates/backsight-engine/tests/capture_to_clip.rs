//! End to end on a real GPU (testing.md, tier 2): capture the primary monitor with WGC,
//! convert to NV12 on the GPU, hand the texture to NVENC without a CPU copy, mux an MP4 and
//! check it with ffprobe. Run on a PC with an NVIDIA GPU with
//! `cargo test -p backsight-engine --test capture_to_clip -- --ignored --nocapture`.

use backsight_encode::{
    Catalog,
    catalog::resolve,
    mux::{Muxer, Track, needs_global_header},
    session::VideoSession,
};
use backsight_engine::{
    capture::{
        convert::Converter,
        d3d::Gpu,
        wgc::{self, Capture},
    },
    pacer::Pacer,
};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const FPS: u32 = 30;
const SECONDS: u32 = 3;

fn ffmpeg_tool(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/ffmpeg/bin").join(name)
}

/// Runs an FFmpeg tool as `name -v error <before> <file> <after>` and returns its stdout.
fn run(name: &str, before: &[&str], file: &Path, after: &[&str]) -> Vec<u8> {
    let out =
        Command::new(ffmpeg_tool(name)).args(["-v", "error"]).args(before).arg(file).args(after).output().unwrap();
    assert!(out.status.success(), "{name} failed: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

#[test]
#[ignore = "needs an NVIDIA GPU and an unlocked desktop"]
fn captures_the_screen_into_an_nvenc_clip_without_a_cpu_copy() {
    let gpu = Arc::new(Gpu::open("nvidia").unwrap());
    assert_eq!(gpu.vendor_id, 0x10DE, "needs an NVIDIA GPU, got {}", gpu.name);
    let monitor = wgc::primary_monitor();
    let input = wgc::monitor_size(monitor).unwrap();
    let output = (input.0 & !1, input.1 & !1); // NV12 needs even sizes
    println!("{} capturing {input:?} as {output:?}", gpu.name);

    let state =
        Arc::new(Mutex::new((Converter::new(&gpu, input, output, FPS).unwrap(), Pacer::new(FPS, wgc::CLOCK_HZ))));
    let (g, s) = (gpu.clone(), state.clone());
    let interval = Some(Duration::from_secs(1) / (2 * FPS));
    let capture = Capture::monitor(&gpu, monitor, true, interval, move |frame| {
        let Ok(frame) = frame else { return }; // the stall check below reports it
        let mut s = s.lock().unwrap();
        // Converting WGC's own textures also checks they accept a video processor input view.
        match s.0.convert_from(&g, frame.texture, frame.size) {
            Ok(true) => s.1.frame_arrived(frame.time),
            Ok(false) => {}
            Err(e) => eprintln!("convert failed: {e}"),
        }
    })
    .unwrap();

    let c = Catalog::get();
    let container = c.container("mp4").unwrap();
    let enc = c.video_encoder("nvenc-h264").unwrap();
    assert!(VideoSession::takes_d3d11(enc));
    let values = resolve(&c.video_options(enc), None);
    let mut video =
        VideoSession::open_d3d11(enc, &values, output, FPS, needs_global_header(container), gpu.for_ffmpeg()).unwrap();
    let path = std::env::temp_dir().join(format!("backsight-e2e-{}.mp4", std::process::id()));
    let mut mux = Muxer::create(&path, container, &video.params(), &[]).unwrap();

    let started = Instant::now();
    let mut sent = 0;
    while sent < FPS * SECONDS {
        assert!(started.elapsed() < Duration::from_secs(10 + SECONDS as u64), "capture stalled after {sent} frames");
        let mut s = state.lock().unwrap();
        let Some(slot) = s.1.poll(wgc::now()) else {
            drop(s);
            std::thread::sleep(Duration::from_millis(1));
            continue;
        };
        s.0.copy_to_encoder(&gpu, video.next_gpu_frame().unwrap());
        drop(s);
        video.send(slot.pts as i64, &mut |p| mux.write(Track::Video, &p).unwrap()).unwrap();
        sent += 1;
    }
    drop(capture);
    video.finish(&mut |p| mux.write(Track::Video, &p).unwrap()).unwrap();
    let saved = mux.finish().unwrap();
    let (conv, pacer) = &*state.lock().unwrap();
    println!("pacer: {:?}, input views made: {}", pacer.stats(), conv.views_made());
    // WGC hands back the same few pool textures: one view each (none on the copy fallback).
    assert!(conv.views_made() <= 3, "{} input views: the cache isn't reused", conv.views_made());

    let info: Value = serde_json::from_slice(&run(
        "ffprobe.exe",
        &["-of", "json", "-show_entries", "stream=codec_name,width,height,nb_frames:format=duration"],
        &saved,
        &[],
    ))
    .unwrap();
    let v = &info["streams"][0];
    assert_eq!(v["codec_name"], "h264");
    assert_eq!((v["width"].as_u64(), v["height"].as_u64()), (Some(output.0.into()), Some(output.1.into())));
    assert_eq!(v["nb_frames"].as_str(), Some((FPS * SECONDS).to_string().as_str()));
    let duration: f64 = info["format"]["duration"].as_str().unwrap().parse().unwrap();
    assert!((duration - SECONDS as f64).abs() < 0.5, "duration {duration}");

    // A wrong texture slice or a missed copy encodes a flat (black or green) picture; a desktop
    // has text and edges. A broken UV plane (zeros, or never copied) tints it green or pink,
    // while a desktop's chroma averages near 128. Decode the last frame and check both.
    let yuv = run(
        "ffmpeg.exe",
        &["-sseof", "-0.5", "-i"],
        &saved,
        &["-frames:v", "1", "-pix_fmt", "yuv420p", "-f", "rawvideo", "-"],
    );
    let luma = (output.0 * output.1) as usize;
    assert_eq!(yuv.len(), luma * 3 / 2);
    let (y, uv) = yuv.split_at(luma);
    let (lo, hi) = (y.iter().min().unwrap(), y.iter().max().unwrap());
    println!("last frame luma {lo}..{hi}");
    assert!(hi - lo > 64, "flat picture (luma {lo}..{hi}): the GPU copy into FFmpeg's frame is wrong");
    let mean = |p: &[u8]| p.iter().map(|&x| u64::from(x)).sum::<u64>() / p.len() as u64;
    let (u, v) = uv.split_at(luma / 4);
    let (u, v) = (mean(u), mean(v));
    println!("last frame mean chroma U {u}, V {v}");
    assert!((32..224).contains(&u) && (32..224).contains(&v), "tinted picture (U {u}, V {v}): the UV plane is wrong");
    std::fs::remove_file(saved).unwrap();
}
