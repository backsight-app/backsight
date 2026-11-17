//! What the Library can read: probe, thumbnail and hover preview for other apps' formats (VP8/VP9
//! WebM, MPEG-4 Part 2, MPEG-2), AV1 and HDR (10-bit PQ) clips. AV1 and HDR files are made with
//! the slim FFmpeg's own encoders; the others come from `tests/fixtures/media` (the slim build
//! has no encoder for them), each a couple of seconds of `testsrc2` at 128x72, 10 fps.

use backsight_encode::media::{preview, probe, thumbnail};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn tool(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/ffmpeg/bin").join(name)
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("backsight-media-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media").join(name)
}

fn ffmpeg(args: &[&str]) -> Vec<u8> {
    let out = Command::new(tool("ffmpeg.exe")).args(["-v", "error", "-y"]).args(args).output().expect("run ffmpeg");
    assert!(out.status.success(), "ffmpeg {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

fn ffprobe(path: &Path, args: &[&str]) -> Value {
    let out = Command::new(tool("ffprobe.exe")).args(["-v", "error", "-of", "json"]).args(args).arg(path).output();
    let out = out.expect("run ffprobe");
    assert!(out.status.success(), "ffprobe failed: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

/// The thumbnail's average colour (decoded back with FFmpeg, scaled down to one pixel).
fn average_rgb(jpeg: &Path) -> [u8; 3] {
    let raw = ffmpeg(&["-i", jpeg.to_str().unwrap(), "-vf", "scale=1:1", "-pix_fmt", "rgb24", "-f", "rawvideo", "-"]);
    raw[..3].try_into().unwrap()
}

/// Brightest minus darkest 8x8 grey cell of the thumbnail: a picture, not a blank frame.
fn contrast(jpeg: &Path) -> u8 {
    let raw = ffmpeg(&["-i", jpeg.to_str().unwrap(), "-vf", "scale=8:8", "-pix_fmt", "gray", "-f", "rawvideo", "-"]);
    raw.iter().max().unwrap() - raw.iter().min().unwrap()
}

/// Checks probe, thumbnail and preview of one file; returns the thumbnail's path.
fn check(file: &Path, codec: &str, size: (u32, u32), seconds: f64) -> PathBuf {
    let info = probe(file).unwrap();
    assert_eq!(info.video_codec.as_deref(), Some(codec), "{info:?}");
    assert_eq!((info.width, info.height), (Some(size.0), Some(size.1)));
    let ms = info.duration_ms.expect("a length");
    assert!(ms.abs_diff((seconds * 1000.0) as u64) < 300, "{ms} ms");

    let out = dir(&format!("{codec}-{}", file.file_name().unwrap().to_string_lossy()));
    let thumb = out.join("t.jpg");
    thumbnail(file, &thumb).unwrap_or_else(|e| panic!("thumbnail of {codec}: {e}"));
    let j = ffprobe(&thumb, &["-show_entries", "stream=codec_name,width,height"]);
    let s = &j["streams"][0];
    assert_eq!(s["codec_name"].as_str(), Some("mjpeg"));
    assert_eq!((s["width"].as_u64(), s["height"].as_u64()), (Some(size.0.into()), Some(size.1.into())));

    let prev = out.join("p.mp4");
    preview(file, &prev).unwrap_or_else(|e| panic!("preview of {codec}: {e}"));
    let j = ffprobe(&prev, &["-show_entries", "stream=codec_name"]);
    assert_eq!(j["streams"][0]["codec_name"].as_str(), Some("h264"));
    thumb
}

fn assert_picture(thumb: &Path) {
    let c = contrast(thumb);
    assert!(c > 60, "the thumbnail looks blank (contrast {c})");
}

#[test]
fn vp8_webm() {
    assert_picture(&check(&fixture("vp8.webm"), "vp8", (128, 72), 1.0));
}

#[test]
fn vp9_webm() {
    assert_picture(&check(&fixture("vp9.webm"), "vp9", (128, 72), 1.0));
}

#[test]
fn mpeg4_part2_mp4() {
    assert_picture(&check(&fixture("mpeg4.mp4"), "mpeg4", (128, 72), 1.0));
}

#[test]
fn mpeg2_transport_stream() {
    assert_picture(&check(&fixture("mpeg2.ts"), "mpeg2video", (128, 72), 1.0));
}

#[test]
fn av1_mp4() {
    let out = dir("av1-src");
    let file = out.join("red.mp4");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=red:s=256x144:r=30:d=2",
        "-c:v",
        "libaom-av1",
        "-cpu-used",
        "8",
        "-pix_fmt",
        "yuv420p",
        file.to_str().unwrap(),
    ]);
    let thumb = check(&file, "av1", (256, 144), 2.0);
    let [r, g, b] = average_rgb(&thumb);
    assert!(r > 200 && g < 60 && b < 60, "red came out as {r},{g},{b}");
}

/// A 2 s clip of one PQ-coded BT.2020 colour (each channel's PQ value, 0..1), 10-bit HEVC.
fn hdr_clip(name: &str, rgb: [f64; 3]) -> PathBuf {
    let file = dir(name).join(format!("{name}.mp4"));
    let [r, g, b] = rgb.map(|v| (v * 255.0).round() as u8);
    let graph = format!(
        "color=c=0x{r:02x}{g:02x}{b:02x}:s=128x72:r=30:d=2,format=rgb24,\
         scale=out_color_matrix=bt2020:out_range=tv,format=yuv420p10le"
    );
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        &graph,
        "-c:v",
        "libx265",
        "-preset",
        "ultrafast",
        "-x265-params",
        "log-level=error:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:range=limited",
        "-color_primaries",
        "bt2020",
        "-color_trc",
        "smpte2084",
        "-colorspace",
        "bt2020nc",
        "-color_range",
        "tv",
        file.to_str().unwrap(),
    ]);
    file
}

#[test]
fn hdr_thumbnails_are_not_washed_out() {
    // PQ 0.58 is 203 nits, the usual "paper white": it should come out near white. PQ 0.5 is
    // 92 nits, a mid-bright grey. Read as plain SDR, 0.5 would be a flat 128.
    for (signal, low, high) in [(0.58, 225, 250), (0.5, 165, 195), (0.3, 35, 70)] {
        let file = hdr_clip(&format!("hdr-{signal}"), [signal; 3]);
        let thumb = check(&file, "hevc", (128, 72), 2.0);
        let [r, g, b] = average_rgb(&thumb);
        let ok = |v: u8| (low..=high).contains(&v);
        assert!(ok(r) && ok(g) && ok(b), "PQ {signal} came out as {r},{g},{b}, wanted {low}..{high}");
    }
}

#[test]
fn hdr_thumbnails_read_colours_with_the_bt2020_formula() {
    // Greys come out the same whatever formula turns YUV into RGB; a colour doesn't. Read with
    // the SD (BT.601) one, this green comes out about 35 levels brighter.
    let file = hdr_clip("hdr-green", [0.0, 0.45, 0.0]);
    let [r, g, b] = average_rgb(&check(&file, "hevc", (128, 72), 2.0));
    println!("PQ green 0.45 came out as {r},{g},{b}");
    assert!((140..=172).contains(&g) && r < 40 && b < 40, "PQ green 0.45 came out as {r},{g},{b}");
}
