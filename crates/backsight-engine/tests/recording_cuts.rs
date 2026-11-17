//! Pause, resume and split files (testing.md, tier 1 synthetic): a white flash frame and a beep
//! once per second go through x264 and AAC from the encoders' first sample, as when a session
//! starts just for a recording, into a recording's feed and writer. The recording is paused,
//! resumed on a requested keyframe and split; FFmpeg then checks every part: it starts on a
//! keyframe with sound at its start, keeps each flash within a frame of its beep, has no gaps,
//! and lost exactly the paused time. Also checks that separate tracks have the same packet
//! timeline across both cuts. Everything is generated.

use backsight_encode::{
    Catalog,
    catalog::resolve,
    mux::{Muxer, needs_global_header},
    session::{AudioSession, VideoSession},
};
use backsight_engine::{recording, replay::StreamKind};
use serde_json::Value;
use std::{
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

const FPS: u32 = 60;
const RATE: u32 = 48_000;
const SECONDS: u32 = 12;
/// The engine clock (QPC µs) at the first video frame.
const START_US: i64 = 987_654_321;
/// Desktop audio starts this much before the first captured frame.
const AUDIO_LEAD: u32 = 370 * RATE / 1000;
const PHASE_MS: u32 = 250;
const BEEP_MS: u32 = 50;
/// Paused from 3.6 s to 6.1 s of capture; a new file after every 4 s of video.
const PAUSE_FRAME: u32 = 216;
const RESUME_FRAME: u32 = 366;
const SPLIT: Duration = Duration::from_secs(4);
/// Markers, pressed on capture frames: on flashes before and after the pause and in the second
/// part, and one while paused.
const MARKS: [u32; 4] = [75, 300, 435, 615];

fn exe(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../third_party/ffmpeg/bin/{name}.exe"))
}

fn ffprobe(path: &Path, args: &[&str]) -> Value {
    let out = Command::new(exe("ffprobe")).args(["-v", "error", "-of", "json"]).args(args).arg(path).output().unwrap();
    assert!(out.status.success(), "ffprobe failed: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

/// Flash and beep start times (seconds) in a decoded file, from FFmpeg's own filters.
fn decode_events(path: &Path) -> (Vec<f64>, Vec<f64>) {
    let out = Command::new(exe("ffmpeg"))
        .args(["-hide_banner", "-nostats", "-i"])
        .arg(path)
        .args(["-vf", "signalstats,metadata=print:key=lavfi.signalstats.YAVG"])
        .args(["-af", "silencedetect=noise=-20dB:duration=0.1", "-f", "null", "-"])
        .output()
        .expect("run ffmpeg");
    let log = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "ffmpeg failed: {log}");
    let value = |line: &str, key: &str| -> Option<f64> {
        let rest = &line[line.find(key)? + key.len()..];
        rest.split(|c: char| c.is_whitespace() || c == '|').next()?.parse().ok()
    };
    let (mut flashes, mut beeps) = (Vec::new(), Vec::new());
    let (mut pts, mut lit) = (0.0, false);
    for line in log.lines() {
        if let Some(t) = value(line, "pts_time:") {
            pts = t;
        } else if let Some(y) = value(line, "YAVG=") {
            if y > 128.0 && !lit {
                flashes.push(pts);
            }
            lit = y > 128.0;
        } else if let Some(t) = value(line, "silence_end: ") {
            beeps.push(t);
        }
    }
    (flashes, beeps)
}

#[test]
fn paused_and_split_recordings_keep_sync_and_start_on_keyframes() {
    let c = Catalog::get();
    for (id, tracks) in [("mp4-fragmented", 1), ("mp4-fragmented", 3), ("mkv", 3)] {
        let container = c.container(id).unwrap(); // recordings use the chosen container as is
        let gh = needs_global_header(container);
        let venc = c.video_encoder("x264").unwrap();
        let mut video = VideoSession::open(venc, &resolve(&c.video_options(venc), None), 256, 144, FPS, gh).unwrap();
        let aenc = c.audio_encoder("aac").unwrap();
        let options = resolve(&aenc.options, None);
        let mut audio: Vec<_> = (0..tracks).map(|_| AudioSession::open(aenc, &options, RATE, 2, gh).unwrap()).collect();
        let audio_origin = START_US - i64::from(AUDIO_LEAD) * 1_000_000 / i64::from(RATE);

        let dir = std::env::temp_dir().join(format!("backsight-cuts-{}-{id}-{tracks}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let vp = video.params();
        let ap: Vec<_> = audio.iter().map(AudioSession::params).collect();
        let mut n = 0;
        let mut next = move || {
            n += 1;
            Muxer::create(
                &dir.join(format!("part {n}.{}", container.ext)),
                container,
                &vp,
                &ap.iter().collect::<Vec<_>>(),
            )
        };
        let first = next().unwrap();
        let (mut feed, sink) = recording::channel(1 << 16); // generous: encoding outruns the writer here
        feed.expect_audio_tracks(tracks);
        feed.split_every(SPLIT);
        let writer = std::thread::spawn(move || recording::write_parts(sink, "can't write", first, next));

        let mut written = 0u32;
        let mut push_audio = |frames: u32, audio: &mut [AudioSession], feed: &mut recording::Feed| {
            let mut chunk = Vec::with_capacity(frames as usize * 2);
            for n in written..written + frames {
                let t = (i64::from(n) - i64::from(AUDIO_LEAD)).rem_euclid(i64::from(RATE)) as u32;
                let on = (PHASE_MS * RATE / 1000..(PHASE_MS + BEEP_MS) * RATE / 1000).contains(&t);
                let s = if on { (n as f32 / RATE as f32 * 1000.0 * std::f32::consts::TAU).sin() * 0.5 } else { 0.0 };
                chunk.extend([s, s]);
            }
            written += frames;
            for (i, encoder) in audio.iter_mut().enumerate() {
                encoder
                    .push(&chunk, &mut |p| {
                        let (pts, dts) = (p.pts_us + audio_origin, p.dts_us + audio_origin);
                        feed.send(StreamKind::Audio(i as u8), pts, dts, p.duration_us, p.keyframe, p.data).unwrap()
                    })
                    .unwrap();
            }
        };
        let at = |frame: u32| START_US + i64::from(frame) * 1_000_000 / i64::from(FPS);
        push_audio(AUDIO_LEAD, &mut audio, &mut feed);
        let mut resume_key_out = None;
        for i in 0..SECONDS * FPS {
            if MARKS.contains(&i) {
                feed.mark(at(i));
            }
            if i == PAUSE_FRAME {
                feed.pause(at(i));
            }
            if i == RESUME_FRAME {
                feed.resume(at(i));
                video.request_keyframe();
            }
            let flash = i % FPS == PHASE_MS * FPS / 1000;
            for (n, (data, _)) in video.planes_mut().unwrap().into_iter().flatten().enumerate() {
                data.fill(match n {
                    0 if flash => 235,
                    0 => 16,
                    _ => 128,
                });
            }
            video
                .send(i.into(), &mut |p| {
                    if p.keyframe && p.pts_us + START_US == at(RESUME_FRAME) {
                        resume_key_out = Some(i); // the frame being sent when the keyframe came out
                    }
                    feed.send(
                        StreamKind::Video,
                        p.pts_us + START_US,
                        p.dts_us + START_US,
                        p.duration_us,
                        p.keyframe,
                        p.data,
                    )
                    .unwrap()
                })
                .unwrap();
            push_audio(RATE / FPS, &mut audio, &mut feed);
            if feed.queued_bytes() > 8 << 20 {
                let deadline = Instant::now() + Duration::from_secs(10);
                while feed.queued_bytes() > 8 << 20 {
                    assert!(Instant::now() < deadline, "recording writer stalled");
                    std::thread::yield_now();
                }
            }
        }
        let out = resume_key_out.expect("x264 made the requested keyframe");
        println!("{id}: the requested keyframe came out {} frames after it was asked for", out - RESUME_FRAME);
        drop(feed); // stop recording (the encoders' last frames are left out, as in a session)
        let (parts, unfinished, last, markers_lost) = writer.join().unwrap();
        assert!(!markers_lost);
        assert!(unfinished.is_empty());
        let parts: Vec<_> = parts.into_iter().chain([last.unwrap()]).collect();
        assert_eq!(parts.len(), 2, "{id}: 3.6 s + the 2 s after the resume make 4 s; the next keyframe splits");

        let frame = 1.0 / f64::from(FPS);
        let mut shown = 0.0;
        // Each marker: where it is in the whole recording, and whether a flash is shown there.
        let mut marks = Vec::new();
        for (k, part) in parts.iter().enumerate() {
            let chapters = ffprobe(part, &["-show_chapters"]);
            let chapters = chapters["chapters"].as_array().unwrap();
            assert_eq!(chapters[0]["tags"]["title"], "Start", "{id} part {k}");
            let part_marks: Vec<f64> =
                chapters[1..].iter().map(|c| c["start_time"].as_str().unwrap().parse().unwrap()).collect();
            let part_start = shown;
            let info = ffprobe(part, &["-show_entries", "stream=codec_type,start_time:format=duration"]);
            let streams = info["streams"].as_array().unwrap();
            assert_eq!(streams.iter().filter(|s| s["codec_type"] == "audio").count(), tracks);
            let start = |kind: &str| -> f64 {
                let s = streams.iter().find(|s| s["codec_type"] == kind).unwrap_or_else(|| panic!("no {kind} track"));
                s["start_time"].as_str().unwrap().parse().unwrap()
            };
            let (v, a) = (start("video"), start("audio"));
            println!("{id} part {k}: video starts {v}, audio {a}");
            assert!((0.0..0.022).contains(&a), "{id} part {k}: sound at its start, audio starts at {a}");
            assert!(v < 0.1, "{id} part {k}: video starts at {v}");
            if tracks > 1 {
                let audio_packets =
                    ffprobe(part, &["-select_streams", "a", "-show_entries", "packet=stream_index,pts,duration"]);
                let packets = audio_packets["packets"].as_array().unwrap();
                let timeline = |index| {
                    packets
                        .iter()
                        .filter(|p| p["stream_index"] == index)
                        .map(|p| (p["pts"].as_i64().unwrap(), p["duration"].as_i64()))
                        .collect::<Vec<_>>()
                };
                let mix = timeline(1);
                assert!(!mix.is_empty());
                for index in 2..=tracks {
                    assert_eq!(timeline(index), mix, "{id} part {k}: audio track {index} lost alignment");
                }
            }
            let first = ffprobe(
                part,
                &["-select_streams", "v:0", "-read_intervals", "%+#1", "-show_entries", "frame=key_frame"],
            );
            assert_eq!(first["frames"][0]["key_frame"], 1, "{id} part {k}: starts on a keyframe");

            let packets = ffprobe(part, &["-select_streams", "v:0", "-show_entries", "packet=pts_time"]);
            let mut pts: Vec<f64> = packets["packets"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p["pts_time"].as_str().unwrap().parse().unwrap())
                .collect();
            pts.sort_by(f64::total_cmp);
            let gap = pts.windows(2).map(|w| w[1] - w[0]).fold(0.0, f64::max);
            assert!(gap < 3.5 * frame, "{id} part {k}: a {gap} s gap between frames");
            shown += pts.len() as f64 * frame;

            let (flashes, beeps) = decode_events(part);
            assert!(flashes.len() >= 3, "{id} part {k}: only {} flashes: {flashes:?}", flashes.len());
            let offsets_ms: Vec<f64> = flashes
                .iter()
                .map(|f| {
                    beeps.iter().map(|b| b - f).min_by(|a, b| a.abs().total_cmp(&b.abs())).expect("a beep") * 1000.0
                })
                .collect();
            println!(
                "{id} part {k}: flashes {flashes:?}, beep minus flash {offsets_ms:.1?} ms, markers {part_marks:?}"
            );
            for m in part_marks {
                marks.push((part_start + m, flashes.iter().any(|f| (f - m).abs() < 1.5 * frame)));
            }
            assert!(offsets_ms.iter().all(|o| o.abs() < frame * 1000.0), "{id} part {k}: off by more than a frame");
            if k == 0 {
                // Flashes at 3.25 s and 6.25 s of capture: 3.6 s to 6.1 s is gone, less the few
                // frames x264 had already decoded-ordered before the pause (its B-frames).
                let seam = flashes.windows(2).map(|w| w[1] - w[0]).find(|d| *d < 0.9).expect("the pause seam");
                assert!(
                    // + 0.1 s: a resume may start on a keyframe shown up to RESUME_SLACK before the press.
                    (0.5 - frame..0.6 + 6.0 * frame).contains(&seam),
                    "{id}: {seam} s between flashes across the pause"
                );
            }
        }
        // Everything but the pause, up to the frames still inside x264 at the stop.
        let expected = f64::from(SECONDS) - 2.5;
        assert!((expected - 1.0..expected + 2.0 * frame).contains(&shown), "{id}: {shown} s of video");
        // Pressed on a flash, a marker shows that flash: it moved with the pause and the split.
        // Pressed while paused, it is where the pause cut the recording (3.6 s, give or take the
        // frames x264 had decoded before it).
        println!("{id}: markers (seconds into the recording, on a flash) {marks:.3?}");
        let on_flash: Vec<_> = marks.iter().map(|m| m.1).collect();
        assert_eq!(on_flash, [true, false, true, true], "{id}: {marks:?}");
        assert!((3.6 - frame..3.6 + 6.0 * frame).contains(&marks[1].0), "{id}: {marks:?}");
        assert!(marks.windows(2).all(|w| w[0].0 < w[1].0), "{id}: in order: {marks:?}");
        std::fs::remove_dir_all(parts[0].parent().unwrap()).unwrap();
    }
}
