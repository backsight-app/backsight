//! What the Library can read: probe, thumbnail and hover preview for other apps' formats (VP8/VP9
//! WebM, MPEG-4 Part 2, MPEG-2), AV1 and HDR (10-bit PQ) clips. AV1 and HDR files are made with
//! the slim FFmpeg's own encoders; the others come from `tests/fixtures/media` (the slim build
//! has no encoder for them), each a couple of seconds of `testsrc2` at 128x72, 10 fps.

use backsight_encode::media::{preview, probe, read_speech, thumbnail};
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

/// `seconds` of 256x144 x264 at `fps`, a keyframe every second, through [`Muxer`]: noise whose
/// brightness steps up by 10 each second, and two AAC tracks of a tone, the second starting
/// 0.5 s late. With `gap`, the picture skips a second after its first half second.
fn stepped_clip(dir: &Path, fps: u32, seconds: usize, gap: bool) -> PathBuf {
    use backsight_encode::{
        Catalog,
        catalog::resolve,
        mux::{Muxer, Track, needs_global_header},
        session::{AudioSession, Encoded, VideoSession},
    };
    const RATE: u32 = 48_000;
    let c = Catalog::get();
    let container = c.container("mp4").unwrap();
    let gh = needs_global_header(container);
    let venc = c.video_encoder("x264").unwrap();
    let mut values = resolve(&c.video_options(venc), None);
    values.insert("keyint_sec".into(), 1.into());
    let mut video = VideoSession::open(venc, &values, 256, 144, fps, gh).unwrap();
    let aenc = c.audio_encoder("aac").unwrap();
    let open_audio = || AudioSession::open(aenc, &resolve(&aenc.options, None), RATE, 2, gh).unwrap();
    let (mut a0, mut a1) = (open_audio(), open_audio());
    let file = dir.join(format!("stepped-{fps}-{gap}.mp4"));
    let mut mux = Muxer::create(&file, container, &video.params(), &[&a0.params(), &a1.params()]).unwrap();
    fn late<'a>(p: &Encoded<'a>) -> Encoded<'a> {
        Encoded { pts_us: p.pts_us + 500_000, dts_us: p.dts_us + 500_000, ..*p }
    }
    let tone = vec![0.25f32; (RATE / fps * 2) as usize];
    for i in 0..seconds * fps as usize {
        let level = 10 * (i / fps as usize);
        for (n, (data, stride)) in video.planes_mut().unwrap().into_iter().flatten().enumerate() {
            for (row, line) in data.chunks_mut(stride).enumerate() {
                for (x, p) in line.iter_mut().enumerate() {
                    let noise = (x as u32).wrapping_mul(0x9E37_79B1) ^ (row as u32).wrapping_mul(0x85EB_CA77);
                    *p = if n == 0 { 16 + level as u8 + ((noise >> 27) as u8) } else { 128 };
                }
            }
        }
        let skipped = if gap && i >= fps as usize / 2 { fps as i64 } else { 0 };
        video.send(i as i64 + skipped, &mut |p| mux.write(Track::Video, &p).unwrap()).unwrap();
        a0.push(&tone, &mut |p| mux.write(Track::Audio(0), &p).unwrap()).unwrap();
        a1.push(&tone, &mut |p| mux.write(Track::Audio(1), &late(&p)).unwrap()).unwrap();
    }
    video.finish(&mut |p| mux.write(Track::Video, &p).unwrap()).unwrap();
    a0.finish(&mut |p| mux.write(Track::Audio(0), &p).unwrap()).unwrap();
    a1.finish(&mut |p| mux.write(Track::Audio(1), &late(&p)).unwrap()).unwrap();
    mux.finish().unwrap()
}

fn packets_and_seconds(file: &Path) -> (u64, f64) {
    let j = ffprobe(file, &["-count_packets", "-show_entries", "stream=nb_read_packets:format=duration"]);
    let n = j["streams"][0]["nb_read_packets"].as_str().unwrap().parse().unwrap();
    (n, j["format"]["duration"].as_str().unwrap().parse().unwrap())
}

#[test]
fn probe_counts_sound_tracks_and_reads_the_frame_rate() {
    let d = dir("probe-tracks");
    let clip = stepped_clip(&d, 30, 2, false);
    let info = probe(&clip).unwrap();
    assert_eq!((info.audio_tracks, info.fps, info.video_codec.as_deref()), (2, Some(30.0), Some("h264")));

    // Sound only: no video details, not the sound track's.
    let sound = d.join("sound.mp4");
    ffmpeg(&["-i", clip.to_str().unwrap(), "-map", "0:a", "-c", "copy", sound.to_str().unwrap()]);
    let info = probe(&sound).unwrap();
    assert_eq!((info.audio_tracks, info.video_codec, info.width, info.fps), (2, None, None, None));

    // NTSC's 30000/1001.
    let ntsc = d.join("ntsc.mp4");
    let src = fixture("vp8.webm");
    ffmpeg(&["-i", src.to_str().unwrap(), "-r", "30000/1001", "-c:v", "libx264", ntsc.to_str().unwrap()]);
    let fps = probe(&ntsc).unwrap().fps.unwrap();
    assert!((fps - 29.97).abs() < 0.01, "{fps}");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn the_thumbnail_is_the_keyframe_a_tenth_in_at_its_quality() {
    let d = dir("thumb-seek");
    let clip = stepped_clip(&d, 30, 20, false);
    let thumb = d.join("t.jpg");
    thumbnail(&clip, &thumb).unwrap();
    // 2 s in, about 41 (brightness 20 up, plus noise); the first frame would be about 18.
    let [r, g, b] = average_rgb(&thumb);
    assert!((33..=47).contains(&r) && r.abs_diff(g) < 4 && r.abs_diff(b) < 4, "{r},{g},{b}");
    // The same frame at JPEG scale 5, the way FFmpeg's `-q:v 5` writes it.
    let reference = d.join("ref.jpg");
    ffmpeg(&["-ss", "2", "-i", clip.to_str().unwrap(), "-frames:v", "1", "-q:v", "5", reference.to_str().unwrap()]);
    let (ours, theirs) = (fs::metadata(&thumb).unwrap().len(), fs::metadata(&reference).unwrap().len());
    assert!(ours.abs_diff(theirs) * 5 < theirs, "{ours} vs {theirs} bytes");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn previews_take_one_frame_per_slot() {
    let d = dir("preview-slots");
    // 60 fps: every other frame; 30 fps: all of them; 15 fps: each once, no repeats.
    // With a second missing after 0.5 s (a variable frame rate), the slots start again after it.
    for (fps, gap, packets) in [(60, false, 60), (30, false, 60), (15, false, 30), (60, true, 60)] {
        let clip = stepped_clip(&d, fps, 2, gap);
        let prev = d.join(format!("p{fps}-{gap}.mp4"));
        preview(&clip, &prev).unwrap();
        let (n, secs) = packets_and_seconds(&prev);
        assert_eq!(n, packets, "{fps} fps, gap {gap}");
        assert!((secs - packets as f64 / 30.0).abs() < 0.1, "{fps} fps: {secs} s");
    }
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn speech_keeps_a_late_track_in_time() {
    let d = dir("speech");
    let clip = stepped_clip(&d, 30, 2, false);
    let early = read_speech(&clip, Some(0)).unwrap();
    let late = read_speech(&clip, Some(1)).unwrap();
    let lead = late.len() as i64 - early.len() as i64;
    assert!((7_000..=9_000).contains(&lead), "0.5 s of silence first: {lead} samples");
    assert!(late[..7_000].iter().all(|s| s.abs() < 1e-3), "silence before the late track");
    // The sound as FFmpeg decodes it: every sample, the resampler's last ones too.
    let raw = ffmpeg(&[
        "-i",
        clip.to_str().unwrap(),
        "-map",
        "0:a:0",
        "-ac",
        "1",
        "-ar",
        "16000",
        "-c:a",
        "pcm_f32le",
        "-f",
        "rawvideo",
        "-",
    ]);
    assert_eq!(early.len(), raw.len() / 4);
    // MPEG-TS starts its clock at about 1.4 s: the sound still counts from the file's start.
    let ts = d.join("clip.ts");
    ffmpeg(&["-i", clip.to_str().unwrap(), "-map", "0", "-c", "copy", ts.to_str().unwrap()]);
    let from_ts = read_speech(&ts, Some(0)).unwrap();
    assert!(from_ts.len().abs_diff(early.len()) < 1_600, "{} vs {}", from_ts.len(), early.len());
    let _ = fs::remove_dir_all(&d);
}
