//! The media library (testing.md, tier 1): scan a folder of tiny generated clips and
//! screenshots, then check metadata, thumbnails, the cache and hover previews with `ffprobe`.

use backsight_encode::{
    Catalog,
    catalog::resolve,
    mux::{Muxer, Track, needs_global_header},
    session::{AudioSession, VideoSession},
};
use backsight_engine::library::{self, MediaKind};
use serde_json::Value;
use std::{
    fs,
    os::windows::fs::FileTimesExt,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant, SystemTime},
};

const FPS: u32 = 30;
const RATE: u32 = 48_000;

fn tool(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/ffmpeg/bin").join(name)
}

fn ffprobe(path: &Path, args: &[&str]) -> Value {
    let out = Command::new(tool("ffprobe.exe"))
        .args(["-v", "error", "-of", "json"])
        .args(args)
        .arg(path)
        .output()
        .expect("run ffprobe");
    assert!(out.status.success(), "ffprobe failed: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

/// `seconds` of a moving 256×144 gradient with a stereo AAC tone, like a tiny saved clip.
fn make_clip(path: &Path, container_id: &str, seconds: u32) {
    let c = Catalog::get();
    let container = c.container(container_id).unwrap();
    let gh = needs_global_header(container);
    let venc = c.video_encoder("x264").unwrap();
    let mut video = VideoSession::open(venc, &resolve(&c.video_options(venc), None), 256, 144, FPS, gh).unwrap();
    let aenc = c.audio_encoder("aac").unwrap();
    let mut audio = AudioSession::open(aenc, &resolve(&aenc.options, None), RATE, 2, gh).unwrap();
    let mut mux = Muxer::create(path, container, &video.params(), &[&audio.params()]).unwrap();
    let silence = vec![0f32; (RATE / FPS) as usize * 2];
    for i in 0..(seconds * FPS) as usize {
        for (n, (data, stride)) in video.planes_mut().unwrap().into_iter().flatten().enumerate() {
            for (row, line) in data.chunks_mut(stride).enumerate() {
                line.iter_mut().enumerate().for_each(|(x, p)| *p = if n == 0 { (x + row + i * 4) as u8 } else { 128 });
            }
        }
        video.send(i as i64, &mut |p| mux.write(Track::Video, &p).unwrap()).unwrap();
        audio.push(&silence, &mut |p| mux.write(Track::Audio(0), &p).unwrap()).unwrap();
    }
    video.finish(&mut |p| mux.write(Track::Video, &p).unwrap()).unwrap();
    audio.finish(&mut |p| mux.write(Track::Audio(0), &p).unwrap()).unwrap();
    mux.finish().unwrap();
}

/// Sets when a file was "recorded", so the newest-first order is known.
fn set_created(path: &Path, minutes_ago: u64) {
    let t = SystemTime::now() - Duration::from_secs(minutes_ago * 60);
    let f = fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_times(fs::FileTimes::new().set_created(t)).unwrap();
}

fn mtime(path: &Path) -> SystemTime {
    fs::metadata(path).unwrap().modified().unwrap()
}

#[test]
fn library_scans_probes_caches_and_previews() {
    let root = std::env::temp_dir().join(format!("backsight-library-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let (videos, shots, cache) = (root.join("videos"), root.join("shots"), root.join("cache"));
    fs::create_dir_all(videos.join("Some Game")).unwrap();
    fs::create_dir_all(&shots).unwrap();

    let short = videos.join("short.mp4");
    let long = videos.join("Some Game").join("long.mkv");
    let png = shots.join("shot.png");
    let jxr = shots.join("hdr.jxr");
    make_clip(&short, "mp4", 2);
    make_clip(&long, "mkv", 8);
    let made = Command::new(tool("ffmpeg.exe"))
        .args(["-v", "error", "-f", "lavfi", "-i", "color=red:s=64x32", "-frames:v", "1"])
        .arg(&png)
        .status()
        .unwrap();
    assert!(made.success());
    fs::write(&jxr, b"not really JPEG XR").unwrap();
    fs::write(videos.join("saving.mp4.part"), b"half a clip").unwrap();
    fs::write(shots.join("notes.txt"), b"not media").unwrap();
    for (i, p) in [&long, &png, &short, &jxr].into_iter().enumerate() {
        set_created(p, i as u64 + 1); // long is newest
    }

    let started = Instant::now();
    let lib = library::scan(Some(&videos), Some(&shots), &cache);
    let first = started.elapsed();
    let names: Vec<&str> = lib.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["long.mkv", "shot.png", "short.mp4", "hdr.jxr"], "newest first, no .part or .txt");
    assert_eq!(lib.video_dir.as_deref(), Some(videos.as_path()));

    let mp4 = &lib.items[2];
    assert_eq!((mp4.kind, mp4.ext.as_str(), mp4.path.as_path()), (MediaKind::Video, "mp4", short.as_path()));
    assert_eq!(mp4.bytes, fs::metadata(&short).unwrap().len());
    assert!(mp4.duration_ms.is_some_and(|d| d.abs_diff(2000) < 150), "{:?}", mp4.duration_ms);
    assert_eq!((mp4.width, mp4.height, mp4.fps), (Some(256), Some(144), Some(30.0)));
    assert_eq!((mp4.video_codec.as_deref(), mp4.audio_tracks), (Some("H.264"), Some(1)));
    let thumb = mp4.thumbnail.clone().expect("a video thumbnail");
    let info = ffprobe(&thumb, &["-show_entries", "stream=codec_name,width,height"]);
    let s = &info["streams"][0];
    assert_eq!(
        (s["codec_name"].as_str(), s["width"].as_i64(), s["height"].as_i64()),
        (Some("mjpeg"), Some(256), Some(144))
    );

    let shot = &lib.items[1];
    assert_eq!((shot.kind, shot.width, shot.height), (MediaKind::Screenshot, Some(64), Some(32)));
    assert_eq!((shot.duration_ms, shot.video_codec.as_deref(), shot.audio_tracks), (None, None, None));
    assert_eq!(shot.thumbnail.as_deref(), Some(png.as_path()), "the WebView shows screenshots directly");
    assert_eq!(lib.items[3].thumbnail, None, "a WebView can't show JPEG XR");

    // A rescan reuses the cache: a planted duration comes back, and no thumbnail is remade.
    let index = cache.join("library.json");
    let thumb_time = mtime(&thumb);
    let mut planted: Value = serde_json::from_str(&fs::read_to_string(&index).unwrap()).unwrap();
    planted[short.to_str().unwrap()]["info"]["duration_ms"] = 1234.into();
    fs::write(&index, planted.to_string()).unwrap();
    let started = Instant::now();
    let again = library::scan(Some(&videos), Some(&shots), &cache);
    let second = started.elapsed();
    assert_eq!(again.items[2].duration_ms, Some(1234));
    assert_eq!(mtime(&thumb), thumb_time);
    println!("scan of 4 files: first {first:?}, cached {second:?}");

    // A broken cache is rebuilt.
    fs::write(&index, "{ not json").unwrap();
    let rebuilt = library::scan(Some(&videos), Some(&shots), &cache);
    assert_eq!(rebuilt.items, lib.items);

    // Hover previews: four 1.5 s snippets of a long video, the whole of a short one.
    let started = Instant::now();
    let resolved_long = fs::canonicalize(&long).unwrap();
    let preview = library::preview(&long, &resolved_long, &cache).unwrap();
    println!("preview of an 8 s 256x144 clip: {:?}", started.elapsed());
    let info = ffprobe(&preview, &["-show_entries", "stream=codec_type,codec_name,width,height:format=duration"]);
    let streams = info["streams"].as_array().unwrap();
    assert_eq!(streams.len(), 1, "muted: {info}");
    let s = &streams[0];
    assert_eq!(
        (s["codec_name"].as_str(), s["width"].as_i64(), s["height"].as_i64()),
        (Some("h264"), Some(256), Some(144))
    );
    let duration: f64 = info["format"]["duration"].as_str().unwrap().parse().unwrap();
    assert!((duration - 6.0).abs() < 0.2, "duration {duration}");
    let preview_time = mtime(&preview);
    assert_eq!(library::preview(&long, &long, &cache).unwrap(), preview, "cached");
    assert_eq!(mtime(&preview), preview_time);
    let alias = long.with_file_name("missing-alias.mp4");
    assert!(library::preview(&alias, &resolved_long, &cache).unwrap().is_file());

    let short_preview = library::preview(&short, &short, &cache).unwrap();
    let info = ffprobe(&short_preview, &["-show_entries", "format=duration"]);
    let duration: f64 = info["format"]["duration"].as_str().unwrap().parse().unwrap();
    assert!((duration - 2.0).abs() < 0.2, "duration {duration}");

    // Deleting a video prunes its thumbnail and preview.
    let long_thumb = lib.items[0].thumbnail.clone().unwrap();
    fs::remove_file(&long).unwrap();
    let pruned = library::scan(Some(&videos), Some(&shots), &cache);
    assert_eq!(pruned.items.len(), 3);
    assert!(!long_thumb.exists() && !preview.exists(), "pruned");
    assert!(short_preview.exists() && thumb.exists(), "kept");

    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn missing_folders_give_an_empty_library() {
    let root = std::env::temp_dir().join(format!("backsight-library-missing-{}", std::process::id()));
    let lib = library::scan(Some(&root.join("nope")), None, &root.join("cache"));
    assert!(lib.items.is_empty());
    let _ = fs::remove_dir_all(&root);
}
