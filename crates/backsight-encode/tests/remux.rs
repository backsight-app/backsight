//! MKV to MP4 (testing.md, tier 1): a synthetic x264 + AAC recording is rewrapped and checked
//! with `ffprobe`; an existing MP4 is never replaced; a broken input leaves no `.part`.

use backsight_encode::{
    Catalog,
    catalog::resolve,
    mux::{Muxer, Track, needs_global_header},
    remux::{copy, remux_checked, remux_to_mp4, remux_to_mp4_report},
    session::{AudioSession, VideoSession},
};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const FPS: u32 = 30;
const RATE: u32 = 48_000;

fn tool(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/ffmpeg/bin").join(name)
}

fn ffprobe(path: &Path, args: &[&str]) -> Value {
    let out = Command::new(tool("ffprobe.exe")).args(["-v", "error", "-of", "json"]).args(args).arg(path).output();
    let out = out.expect("run ffprobe");
    assert!(out.status.success(), "ffprobe failed: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

/// A fresh, empty folder for one test.
fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("backsight-remux-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// Encodes 3 s of a moving gradient and a tone into `<dir>/rec.mkv`.
fn make_mkv(dir: &Path) -> PathBuf {
    make_mkv_with(dir, None)
}

/// [`make_mkv`], writing the `n`th audio packet `extra` more times when `dup` is `(n, extra)`:
/// Matroska takes the repeated timestamp, MP4 refuses it.
/// Hard-to-compress luma, so a clip is big enough (about 1 MB) to be written out mid-copy.
fn noise(x: usize, row: usize, i: usize) -> u8 {
    let mut h = (x as u32).wrapping_mul(0x9E37_79B1)
        ^ (row as u32).wrapping_mul(0x85EB_CA77)
        ^ (i as u32).wrapping_mul(0xC2B2_AE3D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    (h >> 24) as u8
}

fn make_mkv_with(dir: &Path, dup: Option<(usize, usize)>) -> PathBuf {
    let c = Catalog::get();
    let container = c.container("mkv").unwrap();
    let gh = needs_global_header(container);
    let venc = c.video_encoder("x264").unwrap();
    let mut video = VideoSession::open(venc, &resolve(&c.video_options(venc), None), 256, 144, FPS, gh).unwrap();
    let aenc = c.audio_encoder("aac").unwrap();
    let mut audio = AudioSession::open(aenc, &resolve(&aenc.options, None), RATE, 2, gh).unwrap();

    let mut mux = Muxer::create(&dir.join("rec.mkv"), container, &video.params(), &[&audio.params()]).unwrap();
    let tone = vec![0.25f32; (RATE / FPS * 2) as usize];
    let mut audio_packets = 0;
    for i in 0..3 * FPS as usize {
        for (n, (data, stride)) in video.planes_mut().unwrap().into_iter().flatten().enumerate() {
            for (row, line) in data.chunks_mut(stride).enumerate() {
                line.iter_mut().enumerate().for_each(|(x, p)| *p = if n == 0 { noise(x, row, i) } else { 128 });
            }
        }
        video.send(i as i64, &mut |p| mux.write(Track::Video, &p).unwrap()).unwrap();
        audio
            .push(&tone, &mut |p| {
                mux.write(Track::Audio(0), &p).unwrap();
                audio_packets += 1;
                if let Some((n, extra)) = dup
                    && n == audio_packets
                {
                    (0..extra).for_each(|_| mux.write(Track::Audio(0), &p).unwrap());
                }
            })
            .unwrap();
    }
    video.finish(&mut |p| mux.write(Track::Video, &p).unwrap()).unwrap();
    audio.finish(&mut |p| mux.write(Track::Audio(0), &p).unwrap()).unwrap();
    mux.finish().unwrap()
}

fn part_files(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "part"))
        .collect()
}

const ENTRIES: &str =
    "stream=codec_name,codec_type,width,height,sample_rate,channels,nb_read_packets,start_time:format=duration";

fn probe(path: &Path) -> Value {
    ffprobe(path, &["-count_packets", "-show_entries", ENTRIES])
}

#[test]
fn remux_keeps_streams_duration_and_the_keyframe_start() {
    let d = dir("keeps");
    let mkv = make_mkv(&d);
    let before = fs::read(&mkv).unwrap();

    let mp4 = remux_to_mp4(&mkv).unwrap();
    assert_eq!(mp4, d.join("rec.mp4"));
    assert!(part_files(&d).is_empty(), "the .part file was renamed");
    assert_eq!(fs::read(&mkv).unwrap(), before, "the MKV is untouched");

    let (a, b) = (probe(&mkv), probe(&mp4));
    for key in ["codec_name", "codec_type", "width", "height", "sample_rate", "channels", "nb_read_packets"] {
        let pick = |v: &Value| v["streams"].as_array().unwrap().iter().map(|s| s[key].clone()).collect::<Vec<_>>();
        assert_eq!(pick(&a), pick(&b), "stream {key}");
    }
    assert_eq!(b["streams"][0]["codec_name"], "h264");
    assert_eq!(b["streams"][1]["codec_name"], "aac");
    let secs = |v: &Value| -> f64 { v["format"]["duration"].as_str().unwrap().parse().unwrap() };
    assert!((secs(&a) - secs(&b)).abs() <= 1.0 / f64::from(FPS), "duration {} vs {}", secs(&a), secs(&b));

    let first =
        ffprobe(&mp4, &["-select_streams", "v:0", "-read_intervals", "%+#1", "-show_entries", "frame=key_frame"]);
    assert_eq!(first["frames"][0]["key_frame"], 1, "starts on a keyframe");

    // +faststart: the index comes before the media data.
    let bytes = fs::read(&mp4).unwrap();
    let find = |tag: &[u8]| bytes.windows(4).position(|w| w == tag).unwrap();
    assert!(find(b"moov") < find(b"mdat"), "moov is at the front");
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn salvage_leaves_out_a_refused_packet_and_copies_the_rest() {
    let d = dir("refused");
    let mkv = make_mkv_with(&d, Some((20, 1)));
    let out = d.join("out.mp4");
    let options = Default::default();
    let c = copy(&mkv, &out, "mp4", &options, &[], true, None).unwrap();
    assert_eq!((c.refused, c.cut_off.as_deref()), (1, None), "{c:?}");
    let packets =
        |p: &Path, s: usize| probe(p)["streams"][s]["nb_read_packets"].as_str().unwrap().parse::<u64>().unwrap();
    assert_eq!(packets(&out, 0), packets(&mkv, 0), "all video");
    assert_eq!(packets(&out, 1), packets(&mkv, 1) - 1, "all audio but the repeat");
    // Without salvage the same packet is an error, and not one about the drive.
    let e = copy(&mkv, &out, "mp4", &options, &[], false, None).unwrap_err();
    assert!(!e.starts_with(backsight_encode::remux::CANT_WRITE), "{e}");
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn salvage_stops_after_100_refused_packets_in_a_row() {
    let d = dir("refused-run");
    let mkv = make_mkv_with(&d, Some((20, 120)));
    let c = copy(&mkv, &d.join("out.mp4"), "mp4", &Default::default(), &[], true, None).unwrap();
    assert_eq!(c.refused, 100, "{c:?}");
    assert!(c.cut_off.as_deref().is_some_and(|w| w.starts_with("packets couldn't be copied")), "{c:?}");
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn remux_never_replaces_an_existing_file() {
    let d = dir("exists");
    let mkv = make_mkv(&d);
    fs::write(d.join("rec.mp4"), b"precious").unwrap();

    assert_eq!(remux_to_mp4(&mkv).unwrap(), d.join("rec (2).mp4"));
    assert_eq!(remux_to_mp4(&mkv).unwrap(), d.join("rec (3).mp4"));
    assert_eq!(fs::read(d.join("rec.mp4")).unwrap(), b"precious");
    assert!(part_files(&d).is_empty());
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn remux_leaves_no_part_file_for_a_broken_input() {
    let d = dir("broken");
    let mkv = d.join("broken.mkv");
    fs::write(&mkv, b"this is not a matroska file").unwrap();

    let err = remux_to_mp4(&mkv).unwrap_err();
    assert!(err.contains("broken.mkv"), "{err}");
    assert!(fs::read_dir(&d).unwrap().flatten().all(|e| e.path() == mkv), "nothing but the input remains");
    assert_eq!(fs::read(&mkv).unwrap(), b"this is not a matroska file");
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn remux_drops_streams_mp4_cannot_hold_and_says_so() {
    let d = dir("subs");
    let mkv = make_mkv(&d);
    fs::write(d.join("s.srt"), "1\n00:00:00,000 --> 00:00:01,000\nhello\n").unwrap();
    let with_subs = d.join("subs.mkv");
    let ok = Command::new(tool("ffmpeg.exe"))
        .args(["-v", "error", "-i"])
        .arg(&mkv)
        .arg("-i")
        .arg(d.join("s.srt"))
        .args(["-map", "0", "-map", "1", "-c", "copy", "-c:s", "srt"])
        .arg(&with_subs)
        .status()
        .unwrap();
    assert!(ok.success());

    let r = remux_to_mp4_report(&with_subs, &mut |_| {}).unwrap();
    assert_eq!(r.skipped.len(), 1, "{:?}", r.skipped);
    assert!(r.skipped[0].contains("stream 2"), "{:?}", r.skipped);
    let kinds: Vec<_> = probe(&r.path)["streams"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["codec_type"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(kinds, ["video", "audio"]);
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn remux_checked_keeps_the_mkv_and_a_good_mp4() {
    let d = dir("checked");
    let mkv = make_mkv(&d);
    // A leftover `.part` (another conversion's, or one cut off) is skipped, never overwritten.
    let mut leftover = mkv.with_extension("mp4").into_os_string();
    leftover.push(".part");
    fs::write(&leftover, b"x").unwrap();
    let mut told = Vec::new();
    let mp4 = remux_checked(&mkv, &mut |p| told.push(p.to_path_buf())).unwrap();
    let stem = mkv.file_stem().unwrap().to_str().unwrap();
    assert_eq!(mp4.file_name().unwrap().to_str().unwrap(), format!("{stem} (2).mp4"));
    assert_eq!(told, [d.join(format!("{stem} (2).mp4.part"))], "told the .part it reserved");
    assert_eq!(fs::read(&leftover).unwrap(), b"x");
    assert!(mkv.exists(), "the MKV is never deleted");
    assert_eq!(probe(&mp4)["streams"].as_array().unwrap().len(), 2);

    // A broken input fails and is kept.
    let bad = d.join("bad.mkv");
    fs::write(&bad, b"not a video").unwrap();
    assert!(remux_checked(&bad, &mut |_| {}).is_err());
    assert!(bad.exists());
}

#[test]
fn a_conversion_whose_drive_fills_up_keeps_the_mkv_and_leaves_no_mp4() {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::{Foundation::HANDLE, Storage::FileSystem::LockFile};
    let d = dir("drive-full");
    let mkv = make_mkv(&d);
    let before = fs::read(&mkv).unwrap();
    // As a drive filling up mid-copy: another handle locks the reserved `.part` from 4 KiB on, so
    // the first time FFmpeg's write buffer goes to disk, partway through the packets, it fails.
    assert!(before.len() > 256 * 1024, "the clip outgrows FFmpeg's write buffer: {} bytes", before.len());
    let mut lock = None;
    let e = remux_checked(&mkv, &mut |p| {
        let f = fs::File::open(p).unwrap();
        // SAFETY: `f` is an open file handle; the range lies past its end, which LockFile allows.
        unsafe { LockFile(HANDLE(f.as_raw_handle()), 4096, 0, u32::MAX, u32::MAX >> 1) }.unwrap();
        lock = Some(f);
    })
    .unwrap_err();
    drop(lock);
    println!("{e}");
    assert!(e.starts_with(backsight_encode::remux::CANT_WRITE), "the drive is blamed: {e}");
    assert!(e.contains("writing the mp4 file failed"), "it failed mid-copy, not at the end: {e}");
    assert_eq!(fs::read(&mkv).unwrap(), before, "the MKV is untouched");
    let left: Vec<_> = fs::read_dir(&d).unwrap().flatten().map(|e| e.path()).collect();
    assert_eq!(left, [mkv], "no MP4 and no .part");
    fs::remove_dir_all(&d).unwrap();
}
