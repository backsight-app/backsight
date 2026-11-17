//! Crash safety (testing.md, tier 1): a child process (this test binary, re-run) records x264
//! and AAC into crash-safe MP4 and is killed with TerminateProcess, so nothing gets finished
//! or cleaned up. What it wrote must open and play up to the last whole fragment.
//!
//! The file keeps its `.part` name after a crash. Finding and recovering a recording's
//! (`….recording.part`) is `backsight_engine::recovery`, tested with the same kind of child in
//! `backsight-engine/tests/recovery.rs`. Crash-safe MP4 uses `delay_moov`, so the header is written with the
//! first fragment: a kill inside the first GOP leaves a file with nothing playable in it.

use backsight_encode::{
    Catalog,
    catalog::resolve,
    mux::{Muxer, Track, needs_global_header},
    session::{AudioSession, VideoSession},
};
use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

const FPS: u32 = 30;
const RATE: u32 = 48_000;
const READY: &str = "BACKSIGHT CHILD READY";

fn ffmpeg_tool(name: &str, args: &[&str], file: &Path) -> Output {
    let exe = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/ffmpeg/bin").join(name);
    Command::new(exe).args(["-v", "error"]).args(args).arg(file).output().expect("run an FFmpeg tool")
}

fn fragments(part: &Path) -> usize {
    std::fs::read(part).map_or(0, |bytes| bytes.windows(4).filter(|w| w == b"moof").count())
}

/// Starts the child recording into `path` (it writes `path.part`). With `stop_after`, it
/// stops writing after that many frames and says so on stdout, then waits to be killed.
fn spawn_child(path: &Path, stop_after: Option<u32>) -> Child {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", "child_records_until_killed", "--ignored", "--nocapture"])
        .env("BACKSIGHT_CRASH_FILE", path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(n) = stop_after {
        cmd.env("BACKSIGHT_CRASH_STOP_AFTER", n.to_string());
    }
    cmd.spawn().unwrap()
}

fn temp_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("backsight-crash-{}-{name}.mp4", std::process::id()))
}

fn part_of(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.part", path.display()))
}

#[test]
fn killed_mid_recording_plays_up_to_the_last_fragment() {
    let path = temp_path("mid");
    let part = part_of(&path);
    let mut child = spawn_child(&path, None);
    let started = Instant::now();
    while fragments(&part) < 4 {
        assert!(started.elapsed() < Duration::from_secs(60), "the child never wrote four fragments");
        std::thread::sleep(Duration::from_millis(5));
    }
    child.kill().unwrap(); // TerminateProcess, mid-write
    child.wait().unwrap();
    let written = fragments(&part);
    let size = std::fs::metadata(&part).unwrap().len();

    let probe = ffmpeg_tool(
        "ffprobe",
        &["-of", "json", "-count_frames", "-show_entries", "stream=codec_type,codec_name,nb_read_frames"],
        &part,
    );
    assert!(probe.status.success(), "the killed file doesn't open: {}", String::from_utf8_lossy(&probe.stderr));
    let info: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
    let streams = info["streams"].as_array().unwrap();
    let frames = |kind: &str| -> u64 {
        let s = streams.iter().find(|s| s["codec_type"] == kind).unwrap_or_else(|| panic!("no {kind} track"));
        s["nb_read_frames"].as_str().unwrap().parse().unwrap()
    };
    let (video, audio) = (frames("video"), frames("audio"));
    let audio_s = audio as f64 * 1024.0 / f64::from(RATE);
    // Fragments start on keyframes; the last one in the file may be cut off.
    let packets =
        ffmpeg_tool("ffprobe", &["-select_streams", "v:0", "-show_entries", "packet=flags", "-of", "csv"], &part);
    let flags = String::from_utf8_lossy(&packets.stdout);
    let last_fragment =
        flags.lines().collect::<Vec<_>>().iter().rposition(|l| l.contains('K')).expect("a keyframe") as u64;
    println!(
        "killed with {written} fragments in {size} B: {video} video frames (last fragment from {last_fragment}), {audio_s:.2} s of audio"
    );
    assert!(video >= last_fragment, "only {video} frames decode; {last_fragment} are before the last fragment");
    // Each fragment is one two-second GOP; only the last one written may be cut off.
    let gop = 2 * u64::from(FPS);
    assert!(
        last_fragment >= (written as u64).saturating_sub(2) * gop,
        "the demuxer stops at frame {last_fragment}, but {written} fragments were written"
    );
    let whole_s = last_fragment as f64 / f64::from(FPS);
    assert!(audio_s >= whole_s - 0.5, "audio stops at {audio_s:.2} s, video at {whole_s:.2} s");
    std::fs::remove_file(part).unwrap();
}

#[test]
fn killed_in_the_first_gop_leaves_nothing_playable() {
    let path = temp_path("first-gop");
    let part = part_of(&path);
    let mut child = spawn_child(&path, Some(FPS)); // one second into a two-second GOP
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    while !line.contains(READY) {
        line.clear();
        assert!(out.read_line(&mut line).unwrap() > 0, "the child exited early");
    }
    child.kill().unwrap();
    child.wait().unwrap();
    let size = std::fs::metadata(&part).map_or(0, |m| m.len());
    let probe = ffmpeg_tool("ffprobe", &[], &part);
    println!("killed in the first GOP: {size} B, ffprobe: {}", String::from_utf8_lossy(&probe.stderr).trim());
    // delay_moov holds the header back until the first fragment, so nothing is lost but this second.
    assert_eq!(fragments(&part), 0);
    assert!(!probe.status.success(), "a file without its first fragment has no header");
    let _ = std::fs::remove_file(part);
}

/// The process the tests above kill. Ignored unless they start it.
#[test]
#[ignore = "run by the crash tests as their child process"]
fn child_records_until_killed() {
    let Some(path) = std::env::var_os("BACKSIGHT_CRASH_FILE") else { return };
    let stop_after: Option<u32> = std::env::var("BACKSIGHT_CRASH_STOP_AFTER").ok().map(|n| n.parse().unwrap());
    let c = Catalog::get();
    let container = c.container("mp4-fragmented").unwrap();
    let gh = needs_global_header(container);
    let venc = c.video_encoder("x264").unwrap();
    let mut video = VideoSession::open(venc, &resolve(&c.video_options(venc), None), 256, 144, FPS, gh).unwrap();
    let aenc = c.audio_encoder("aac").unwrap();
    let mut audio = AudioSession::open(aenc, &resolve(&aenc.options, None), RATE, 2, gh).unwrap();
    let mut mux = Muxer::create(Path::new(&path), container, &video.params(), &[&audio.params()]).unwrap();
    let tone: Vec<f32> = (0..RATE / FPS * 2).map(|i| ((i / 2) as f32 * 0.06).sin() * 0.3).collect();
    for i in 0..100_000u32 {
        if Some(i) == stop_after {
            println!("{READY}");
            std::thread::sleep(Duration::from_secs(60));
            return;
        }
        for (n, (data, stride)) in video.planes_mut().unwrap().into_iter().flatten().enumerate() {
            for (row, line) in data.chunks_mut(stride).enumerate() {
                line.iter_mut()
                    .enumerate()
                    .for_each(|(x, p)| *p = if n == 0 { (x + row * 2) as u8 ^ i as u8 } else { 128 });
            }
        }
        video.send(i.into(), &mut |p| mux.write(Track::Video, &p).unwrap()).unwrap();
        audio.push(&tone, &mut |p| mux.write(Track::Audio(0), &p).unwrap()).unwrap();
    }
}
