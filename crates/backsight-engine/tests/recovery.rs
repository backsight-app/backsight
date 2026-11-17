//! Interrupted recordings (testing.md, tier 1): a child process (this test binary, re-run)
//! records x264 and AAC the way a recording does, as `….recording.part`, and is killed with
//! TerminateProcess. `recovery` must find it (and not while it is still being written), recover
//! what plays into a new file without replacing anything, keep the original, keep the bytes of
//! corrupt and empty files (renamed to `.unrecoverable.part`), leave unrelated files alone, and
//! leave a good recording as it was when the copy can't be written. Also the crash-safe MKV.

use backsight_encode::{
    Catalog,
    catalog::resolve,
    mux::{Muxer, Track, needs_global_header},
    session::{AudioSession, VideoSession},
};
use backsight_engine::recovery::{self, find_interrupted, recover};
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const FPS: u32 = 30;
const RATE: u32 = 48_000;
const READY: &str = "BACKSIGHT CHILD READY";
const NAME: &str = "Backsight 2026-11-10 14.03.07";

fn ffprobe(path: &Path, args: &[&str]) -> Value {
    let exe = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/ffmpeg/bin/ffprobe.exe");
    let out = Command::new(exe).args(["-v", "error", "-of", "json"]).args(args).arg(path).output().unwrap();
    assert!(out.status.success(), "ffprobe failed: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("backsight-recovery-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// Starts the child recording `<dir>/<NAME>.<ext>` in container `id`. With `stop_after`, it
/// stops writing after that many frames, says so, and waits to be killed.
fn spawn_child(dir: &Path, id: &str, stop_after: Option<u32>) -> Child {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", "child_records_until_killed", "--ignored", "--nocapture"])
        .env("BACKSIGHT_RECOVERY_DIR", dir)
        .env("BACKSIGHT_RECOVERY_CONTAINER", id)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(n) = stop_after {
        cmd.env("BACKSIGHT_RECOVERY_STOP_AFTER", n.to_string());
    }
    cmd.spawn().unwrap()
}

/// Waits until the child's file holds `bytes`, then kills it mid-write.
fn kill_after(mut child: Child, part: &Path, bytes: u64) {
    let started = Instant::now();
    while fs::metadata(part).map_or(0, |m| m.len()) < bytes {
        assert!(started.elapsed() < Duration::from_secs(60), "the child never wrote {bytes} bytes");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(find_interrupted(part.parent().unwrap()).is_empty(), "a recording being written isn't interrupted");
    child.kill().unwrap(); // TerminateProcess
    child.wait().unwrap();
}

fn video_seconds(path: &Path) -> f64 {
    let v = ffprobe(path, &["-select_streams", "v:0", "-count_frames", "-show_entries", "stream=nb_read_frames"]);
    v["streams"][0]["nb_read_frames"].as_str().unwrap().parse::<f64>().unwrap() / f64::from(FPS)
}

#[test]
fn a_killed_recording_is_recovered_next_to_the_files_it_must_not_touch() {
    for (id, ext) in [("mp4-fragmented", "mp4"), ("mkv", "mkv")] {
        let d = dir(id);
        let part = d.join(format!("{NAME}.{ext}.recording.part"));
        // Files recovery must leave alone: another app's and a clip's unfinished files and, for
        // MP4, a finished file under the recording's own name, so the recovered one needs another.
        let mut untouched = vec![
            (d.join(format!("holiday.{ext}.part")), b"another app's download".as_slice()),
            (d.join(format!("{NAME} (5).{ext}.part")), b"a clip being saved".as_slice()),
            (d.join(format!("holiday.{ext}.recording.part")), b"not a name Backsight gives".as_slice()),
        ];
        let collision = ext == "mp4";
        if collision {
            untouched.push((d.join(format!("{NAME}.{ext}")), b"precious".as_slice()));
        }
        for (p, bytes) in &untouched {
            fs::write(p, bytes).unwrap();
        }
        // The copy of an earlier recovery cut off by a crash, named in its mark: removed, not
        // stepped around (a clip owning the name makes it ` (2)`).
        let stale = d.join(if collision { format!("{NAME} (2).{ext}.part") } else { format!("{NAME}.{ext}.part") });
        let mark = d.join(format!("{NAME}.{ext}{}", recovery::RECOVERING_PART));
        fs::write(&stale, b"cut off").unwrap();
        fs::write(&mark, stale.to_str().unwrap()).unwrap();
        kill_after(spawn_child(&d, id, None), &part, 256 << 10);
        let killed = fs::read(&part).unwrap();

        let found = find_interrupted(&d);
        assert_eq!(found.len(), 1, "{id}: {found:?}");
        assert_eq!(
            (found[0].path.as_path(), found[0].name.as_str()),
            (part.as_path(), format!("{NAME}.{ext}").as_str())
        );
        assert_eq!(found[0].bytes, killed.len() as u64);

        let r = recover(&part, None).unwrap();
        // The name the recording would have had, unless a file has it: nothing is replaced.
        let name = if collision { format!("{NAME} (2).{ext}") } else { format!("{NAME}.{ext}") };
        assert_eq!(r.path, d.join(name), "{id}");
        assert_eq!(r.original, d.join(format!("{NAME}.{ext}{}", recovery::RECOVERED_PART)));
        assert_eq!(fs::read(&r.original).unwrap(), killed, "{id}: the original is kept as it was");
        assert_eq!(r.original_bytes, killed.len() as u64, "{id}");
        assert!(!part.exists());
        assert!(!stale.exists() && !mark.exists(), "{id}: a cut-off recovery's leftovers are removed");
        let seconds = video_seconds(&r.path);
        println!("{id}: killed at {} KB, recovered {seconds:.2} s", killed.len() >> 10);
        assert!(seconds >= 2.0, "{id}: only {seconds} s recovered");
        let info = ffprobe(&r.path, &["-show_entries", "stream=codec_type"]);
        let kinds: Vec<_> = info["streams"].as_array().unwrap().iter().map(|s| s["codec_type"].clone()).collect();
        assert_eq!(kinds, ["video", "audio"], "{id}");

        for (p, bytes) in &untouched {
            assert_eq!(fs::read(p).unwrap(), *bytes, "{id}: {} was touched", p.display());
        }
        assert!(find_interrupted(&d).is_empty(), "{id}: a recovered recording isn't offered again");
        let names = fs::read_dir(&d).unwrap().count();
        assert_eq!(names, untouched.len() + 2, "{id}: only the recovered file was added");
        fs::remove_dir_all(&d).unwrap();
    }
}

#[test]
fn empty_corrupt_and_foreign_files_are_explained_and_kept() {
    let d = dir("bad");
    // Killed inside the first GOP: crash-safe MP4 has written nothing yet.
    let empty = d.join(format!("{NAME}.mp4.recording.part"));
    let mut child = spawn_child(&d, "mp4-fragmented", Some(FPS));
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    while !line.contains(READY) {
        line.clear();
        assert!(out.read_line(&mut line).unwrap() > 0, "the child exited early");
    }
    child.kill().unwrap();
    child.wait().unwrap();
    let corrupt = d.join("Backsight 2026-11-10 15.00.00.mkv.recording.part");
    fs::write(&corrupt, b"not a video at all, just bytes".repeat(1000)).unwrap();
    let foreign = d.join("Backsight 2026-11-10 15.00.00.mp4.part");
    fs::write(&foreign, b"a .part Backsight didn't mark").unwrap();
    let before: Vec<_> = [&empty, &corrupt, &foreign].map(|p| fs::read(p).unwrap()).into();
    assert!(before[0].is_empty(), "nothing reaches the disk before the first fragment");

    let found: Vec<_> = find_interrupted(&d).into_iter().map(|i| i.path).collect();
    assert_eq!(found, [empty.clone(), corrupt.clone()], "oldest first, only Backsight's recordings");
    let e = recover(&empty, None).unwrap_err();
    assert!(e.contains("empty"), "{e}");
    let e = recover(&corrupt, None).unwrap_err();
    assert!(e.contains("nothing it can play"), "{e}");
    let e = recover(&foreign, None).unwrap_err();
    assert!(e.contains("only recovers recordings it was writing"), "{e}");
    // Set aside under a name startup doesn't offer again, bytes untouched.
    let aside = |p: &std::path::Path| {
        let name = p.file_name().unwrap().to_str().unwrap().replace(".recording.part", ".unrecoverable.part");
        p.with_file_name(name)
    };
    let after: Vec<_> = [aside(&empty), aside(&corrupt), foreign.clone()].map(|p| fs::read(p).unwrap()).into();
    assert_eq!(before, after, "contents left exactly as they were");
    assert!(find_interrupted(&d).is_empty(), "not offered again");
    assert_eq!(fs::read_dir(&d).unwrap().count(), 3, "no copies or .part files left behind");
    fs::remove_dir_all(&d).unwrap();
}

/// Runs icacls on `dir` (a test folder of our own).
fn icacls(dir: &Path, args: &[&str]) {
    let out = Command::new("icacls").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "icacls failed: {}", String::from_utf8_lossy(&out.stdout));
}

#[test]
fn a_copy_that_cant_be_written_leaves_a_good_recording_as_it_was() {
    let d = dir("cant-write");
    let part = d.join(format!("{NAME}.mp4.recording.part"));
    kill_after(spawn_child(&d, "mp4-fragmented", None), &part, 256 << 10);
    let killed = fs::read(&part).unwrap();

    // New files in the folder can be created but not written, like a drive that fills up.
    icacls(&d, &["/deny", "*S-1-1-0:(OI)(IO)(WD,AD)"]);
    let e = recover(&part, None);
    icacls(&d, &["/reset", "/T"]);
    let e = e.unwrap_err();
    assert!(e.contains("couldn't write the recovered video"), "{e}");
    assert_eq!(fs::read(&part).unwrap(), killed, "the original keeps its name and bytes");
    assert_eq!(fs::read_dir(&d).unwrap().count(), 1, "no copy, .part or set-aside file left");
    assert_eq!(find_interrupted(&d).len(), 1, "still offered for recovery");

    // Once it can be written, recovery works, and a stray unfinished marker copy of ours goes.
    let chapters = d.join(format!("{NAME}.mp4.chapters.part"));
    fs::write(&chapters, b"an unfinished marker copy").unwrap();
    // Quitting mid-copy leaves everything as it was, for the next start.
    let quitting = std::sync::atomic::AtomicBool::new(true);
    assert_eq!(recover(&part, Some(&quitting)).unwrap_err(), backsight_encode::remux::CANCELLED);
    assert_eq!(fs::read(&part).unwrap(), killed, "a cancelled copy leaves the original as it was");
    assert_eq!(fs::read_dir(&d).unwrap().count(), 2, "no copy left behind");
    let r = recover(&part, None).unwrap();
    assert_eq!(r.path, d.join(format!("{NAME} (2).mp4")), "the marker copy still held the name");
    assert!(!chapters.exists(), "the stray .chapters.part is removed");
    assert_eq!(fs::read(&r.original).unwrap(), killed);
    assert_eq!(fs::read_dir(&d).unwrap().count(), 2, "the recovered file and the original");
    fs::remove_dir_all(&d).unwrap();
}

/// The process the tests above kill. Ignored unless they start it.
#[test]
#[ignore = "run by the recovery tests as their child process"]
fn child_records_until_killed() {
    let Some(dir) = std::env::var_os("BACKSIGHT_RECOVERY_DIR") else { return };
    let id = std::env::var("BACKSIGHT_RECOVERY_CONTAINER").unwrap();
    let stop_after: Option<u32> = std::env::var("BACKSIGHT_RECOVERY_STOP_AFTER").ok().map(|n| n.parse().unwrap());
    let c = Catalog::get();
    let container = c.container(&id).unwrap();
    let gh = needs_global_header(container);
    let venc = c.video_encoder("x264").unwrap();
    let mut video = VideoSession::open(venc, &resolve(&c.video_options(venc), None), 256, 144, FPS, gh).unwrap();
    let aenc = c.audio_encoder("aac").unwrap();
    let mut audio = AudioSession::open(aenc, &resolve(&aenc.options, None), RATE, 2, gh).unwrap();
    let path = Path::new(&dir).join(format!("{NAME}.{}", container.ext));
    let mut mux = Muxer::create_recording(&path, container, &video.params(), &[&audio.params()]).unwrap();
    mux.keep_part_on_failure();
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
