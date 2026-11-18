//! Vertical clips (testing.md, tier 1): a synthetic 1080p x264 + AAC clip is cropped to 9:16 and
//! checked with `ffprobe`: the size, the copied audio, the length, and no `.part` left behind.

use backsight_encode::{
    Catalog,
    catalog::resolve,
    mux::{Muxer, Track, needs_global_header},
    session::{AudioSession, VideoSession},
    vertical::make_vertical,
};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn tool(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/ffmpeg/bin").join(name)
}

fn ffprobe(path: &Path) -> Value {
    let out = Command::new(tool("ffprobe.exe"))
        .args(["-v", "error", "-of", "json", "-show_streams", "-show_format"])
        .arg(path)
        .output()
        .expect("run ffprobe");
    assert!(out.status.success(), "ffprobe failed: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("backsight-vertical-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// 2 s of a moving 1080p gradient and a tone, saved as a clip is: x264 and AAC through [`Muxer`].
fn make_clip(dir: &Path) -> PathBuf {
    const FPS: u32 = 30;
    const RATE: u32 = 48_000;
    let c = Catalog::get();
    let container = Catalog::clip_container(c.container("mp4").unwrap());
    let gh = needs_global_header(&container);
    let venc = c.video_encoder("x264").unwrap();
    let mut video = VideoSession::open(venc, &resolve(&c.video_options(venc), None), 1920, 1080, FPS, gh).unwrap();
    let aenc = c.audio_encoder("aac").unwrap();
    let mut audio = AudioSession::open(aenc, &resolve(&aenc.options, None), RATE, 2, gh).unwrap();
    let mut mux = Muxer::create(&dir.join("clip.mp4"), &container, &video.params(), &[&audio.params()]).unwrap();
    let tone = vec![0.25f32; (RATE / FPS * 2) as usize];
    for i in 0..2 * FPS as usize {
        for (n, (data, stride)) in video.planes_mut().unwrap().into_iter().flatten().enumerate() {
            for (row, line) in data.chunks_mut(stride).enumerate() {
                line.iter_mut().enumerate().for_each(|(x, p)| *p = if n == 0 { (x + row + i * 4) as u8 } else { 128 });
            }
        }
        video.send(i as i64, &mut |p| mux.write(Track::Video, &p).unwrap()).unwrap();
        audio.push(&tone, &mut |p| mux.write(Track::Audio(0), &p).unwrap()).unwrap();
    }
    video.finish(&mut |p| mux.write(Track::Video, &p).unwrap()).unwrap();
    audio.finish(&mut |p| mux.write(Track::Audio(0), &p).unwrap()).unwrap();
    mux.finish().unwrap()
}

#[test]
fn a_wide_clip_becomes_a_9_16_clip_with_its_audio() {
    let d = dir("wide");
    let src = make_clip(&d);

    let c = Catalog::get();
    let container = Catalog::clip_container(c.container("mp4").unwrap());
    let venc = c.video_encoder("x264").unwrap();
    let values = resolve(&c.video_options(venc), None);
    let dst = d.join("clip (vertical).mp4");
    let out = make_vertical(&src, &dst, &container, venc, &values).unwrap();
    assert_eq!(out, dst);
    assert!(!d.join("clip (vertical).mp4.part").exists(), "the .part is renamed");

    let v = ffprobe(&out);
    let streams = v["streams"].as_array().unwrap();
    let video = streams.iter().find(|s| s["codec_type"] == "video").unwrap();
    assert_eq!((video["width"].as_u64(), video["height"].as_u64()), (Some(594), Some(1056)));
    assert_eq!(video["codec_name"], "h264");
    let audio = streams.iter().find(|s| s["codec_type"] == "audio").expect("audio copied");
    assert_eq!(audio["codec_name"], "aac");
    let secs: f64 = v["format"]["duration"].as_str().unwrap().parse().unwrap();
    assert!((1.8..2.2).contains(&secs), "about 2 s long, got {secs}");

    // A second one never replaces the first.
    let again = make_vertical(&src, &dst, &container, venc, &values).unwrap();
    assert_eq!(again, d.join("clip (vertical) (2).mp4"));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_broken_clip_leaves_nothing() {
    let d = dir("broken");
    let src = d.join("broken.mp4");
    fs::write(&src, b"not a video").unwrap();
    let c = Catalog::get();
    let container = Catalog::clip_container(c.container("mp4").unwrap());
    let venc = c.video_encoder("x264").unwrap();
    let dst = d.join("broken (vertical).mp4");
    assert!(make_vertical(&src, &dst, &container, venc, &resolve(&c.video_options(venc), None)).is_err());
    let left: Vec<_> = fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(left, vec![std::ffi::OsString::from("broken.mp4")]);
    let _ = fs::remove_dir_all(&d);
}
