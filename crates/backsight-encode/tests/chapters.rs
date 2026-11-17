//! Markers as chapters (testing.md, tier 1): a synthetic x264 + AAC file gets markers while it is
//! written, in every container that keeps chapters, and `ffprobe -show_chapters` finds them at
//! their times after the file is finished, after a fragmented file's final copy, and after the
//! MKV → MP4 remux.

use backsight_encode::{
    Catalog,
    catalog::resolve,
    mux::{Muxer, Track, holds_chapters, needs_global_header},
    remux::remux_to_mp4,
    session::{AudioSession, Encoded, VideoSession},
};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const FPS: u32 = 30;
const RATE: u32 = 48_000;

fn ffprobe(path: &Path, args: &[&str]) -> Value {
    let exe = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/ffmpeg/bin/ffprobe.exe");
    let out = Command::new(exe).args(["-v", "error", "-of", "json"]).args(args).arg(path).output().unwrap();
    assert!(out.status.success(), "ffprobe failed: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

/// Chapter (start, end, title) in seconds.
pub fn chapters(path: &Path) -> Vec<(f64, f64, String)> {
    let v = ffprobe(path, &["-show_chapters"]);
    v["chapters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            let t = |k: &str| c[k].as_str().unwrap().parse::<f64>().unwrap();
            (t("start_time"), t("end_time"), c["tags"]["title"].as_str().unwrap_or_default().to_string())
        })
        .collect()
}

/// Encodes 3 s starting at `origin_us` on the packets' clock, with markers at 1 s and 2 s, into
/// `<dir>/rec.<ext>`.
fn record(dir: &Path, id: &str, origin_us: i64) -> PathBuf {
    let c = Catalog::get();
    let container = c.container(id).unwrap();
    let gh = needs_global_header(container);
    let venc = c.video_encoder("x264").unwrap();
    let mut video = VideoSession::open(venc, &resolve(&c.video_options(venc), None), 256, 144, FPS, gh).unwrap();
    let aenc = c.audio_encoder("aac").unwrap();
    let mut audio = AudioSession::open(aenc, &resolve(&aenc.options, None), RATE, 2, gh).unwrap();
    let path = dir.join(format!("rec.{}", container.ext));
    let mut mux = Muxer::create_recording(&path, container, &video.params(), &[&audio.params()]).unwrap();
    let tone = vec![0.25f32; (RATE / FPS * 2) as usize];
    fn shifted(mut p: Encoded<'_>, by: i64) -> Encoded<'_> {
        p.pts_us += by;
        p.dts_us += by;
        p
    }
    for i in 0..3 * FPS as usize {
        if i == FPS as usize || i == 2 * FPS as usize {
            mux.add_marker(origin_us + i as i64 * 1_000_000 / i64::from(FPS));
        }
        for (n, (data, _)) in video.planes_mut().unwrap().into_iter().flatten().enumerate() {
            data.fill(if n == 0 { (i * 4) as u8 } else { 128 });
        }
        video.send(i as i64, &mut |p| mux.write(Track::Video, &shifted(p, origin_us)).unwrap()).unwrap();
        audio.push(&tone, &mut |p| mux.write(Track::Audio(0), &shifted(p, origin_us)).unwrap()).unwrap();
    }
    video.finish(&mut |p| mux.write(Track::Video, &shifted(p, origin_us)).unwrap()).unwrap();
    audio.finish(&mut |p| mux.write(Track::Audio(0), &shifted(p, origin_us)).unwrap()).unwrap();
    mux.finish().unwrap()
}

/// "Start" at zero, then "Marker 1" at 1 s and "Marker 2" at 2 s (give or take the audio
/// priming the file starts with), each running to the next and the last to the end.
fn assert_markers(path: &Path, what: &str) {
    let got = chapters(path);
    println!("{what}: {got:?}");
    let titles: Vec<_> = got.iter().map(|c| c.2.as_str()).collect();
    assert_eq!(titles, ["Start", "Marker 1", "Marker 2"], "{what}: {got:?}");
    let near = |a: f64, b: f64| (a - b).abs() < 0.05;
    assert!(near(got[0].0, 0.0) && near(got[1].0, 1.0) && near(got[2].0, 2.0), "{what}: {got:?}");
    assert!(got.windows(2).all(|w| w[0].1 == w[1].0) && got[2].1 >= 2.9, "{what}: {got:?}");
}

#[test]
fn markers_are_chapters_in_every_container_that_has_them() {
    let c = Catalog::get();
    for container in c.containers.iter().filter(|c| holds_chapters(c) && c.video_codecs.iter().any(|v| v == "h264")) {
        let id = &container.id;
        let d = std::env::temp_dir().join(format!("backsight-chapters-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        // Packets carry the engine clock; the file starts at zero.
        let file = record(&d, id, 5_000_000_000);
        assert_markers(&file, id);
        let names: Vec<_> = fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names, [file.file_name().unwrap()], "{id}: only the finished file is left");
        // A fragmented file with markers is copied unfragmented at the end: its chapter track
        // would start at zero (platform-notes.md).
        let fragments = fs::read(&file).unwrap().windows(4).any(|w| w == b"moof");
        assert!(!fragments, "{id}: unfragmented");
        if id == "mkv" {
            let mp4 = remux_to_mp4(&file).unwrap();
            assert_markers(&mp4, "mkv → mp4");
        }
        fs::remove_dir_all(&d).unwrap();
    }
}

#[test]
fn containers_without_chapters_are_known() {
    let c = Catalog::get();
    let without: Vec<_> = c.containers.iter().filter(|c| !holds_chapters(c)).map(|c| c.id.as_str()).collect();
    assert_eq!(without, ["mpegts", "flv"]);
}
