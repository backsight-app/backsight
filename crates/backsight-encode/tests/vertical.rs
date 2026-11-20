//! Vertical clips (testing.md, tier 1): a synthetic 1080p x264 + AAC clip is cropped to 9:16 and
//! checked with `ffprobe`: the size, the copied audio, the length, and no `.part` left behind;
//! burned-in captions change the picture's lower part only.

use backsight_encode::{
    Catalog,
    burn::Captions,
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

/// The luma of the frame at `secs` of `clip`, as `ffmpeg` decodes it.
fn luma_at(clip: &Path, secs: &str) -> Vec<u8> {
    let out = Command::new(tool("ffmpeg.exe"))
        .args(["-v", "error", "-ss", secs, "-i"])
        .arg(clip)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "gray", "-"])
        .output()
        .expect("run ffmpeg");
    assert!(out.status.success(), "ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
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
    let out = make_vertical(&src, &dst, &container, venc, &values, None).unwrap();
    assert_eq!(out, dst);
    assert!(!d.join("clip (vertical).mp4.part").exists(), "the .part is renamed");

    let v = ffprobe(&out);
    let streams = v["streams"].as_array().unwrap();
    let video = streams.iter().find(|s| s["codec_type"] == "video").unwrap();
    assert_eq!((video["width"].as_u64(), video["height"].as_u64()), (Some(594), Some(1056)));
    assert_eq!(video["codec_name"], "h264");
    let audio = streams.iter().find(|s| s["codec_type"] == "audio").expect("audio copied");
    assert_eq!(audio["codec_name"], "aac");
    let audio_secs: f64 = audio["duration"].as_str().unwrap().parse().unwrap();
    assert!((1.8..2.2).contains(&audio_secs), "audio from the start, got {audio_secs} s");
    let secs: f64 = v["format"]["duration"].as_str().unwrap().parse().unwrap();
    assert!((1.8..2.2).contains(&secs), "about 2 s long, got {secs}");

    // A second one never replaces the first. This one has captions from 0.5 s on.
    let srt = d.join("clip.srt");
    fs::write(&srt, "1\n00:00:00,500 --> 00:00:02,000\nThat was close!\n").unwrap();
    let mut captions = Captions::load(&srt).unwrap();
    let again = make_vertical(&src, &dst, &container, venc, &values, Some(&mut captions)).unwrap();
    assert_eq!(again, d.join("clip (vertical) (2).mp4"));
    assert!(captions.burned());
    let (plain, burned) = (luma_at(&out, "1"), luma_at(&again, "1"));
    let rows = |frame: &[u8], from: usize, to: usize| frame[from * 594..to * 594].to_vec();
    let differ = |a: &[u8], b: &[u8]| a.iter().zip(b).filter(|(x, y)| x.abs_diff(**y) > 40).count();
    assert_eq!(differ(&rows(&plain, 0, 470), &rows(&burned, 0, 470)), 0, "the top is untouched");
    assert!(differ(&rows(&plain, 475, 898), &rows(&burned, 475, 898)) > 2000, "the caption shows");
    assert_eq!(differ(&luma_at(&out, "0.1"), &luma_at(&again, "0.1")), 0, "not before its time");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn captions_count_from_the_start_of_a_clip_whose_clock_starts_late() {
    let d = dir("late");
    let src = make_clip(&d);
    let late = d.join("late.mkv");
    let ok = Command::new(tool("ffmpeg.exe"))
        .args(["-v", "error", "-i"])
        .arg(&src)
        .args(["-c", "copy", "-output_ts_offset", "5"])
        .arg(&late)
        .status()
        .unwrap();
    assert!(ok.success());
    let srt = d.join("late.srt");
    fs::write(
        &srt,
        "1
00:00:00,500 --> 00:00:02,000
That was close!
",
    )
    .unwrap();
    let mut captions = Captions::load(&srt).unwrap();

    let c = Catalog::get();
    let container = Catalog::clip_container(c.container("mp4").unwrap());
    let venc = c.video_encoder("x264").unwrap();
    let values = resolve(&c.video_options(venc), None);
    make_vertical(&late, &d.join("late (vertical).mp4"), &container, venc, &values, Some(&mut captions)).unwrap();
    assert!(captions.burned(), "the caption at 0.5 s of the file shows");
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
    let values = resolve(&c.video_options(venc), None);
    assert!(make_vertical(&src, &dst, &container, venc, &values, None).is_err());
    let left: Vec<_> = fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(left, vec![std::ffi::OsString::from("broken.mp4")]);
    let _ = fs::remove_dir_all(&d);
}
