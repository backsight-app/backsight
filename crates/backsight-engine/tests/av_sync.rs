//! A/V sync test pattern (testing.md, tier 1 synthetic): a white flash frame and a beep once
//! per second go through x264 and AAC, the replay buffer and `write_clip`; FFmpeg decodes the
//! saved clip and the flash-to-beep offset must be under one frame. Audio starts before video
//! and the video encoder runs behind, as with WASAPI and NVENC. Everything is generated.

use backsight_encode::{
    Catalog,
    catalog::resolve,
    mux::needs_global_header,
    session::{AudioSession, VideoSession},
};
use backsight_engine::{
    replay::{ReplayBuffer, StreamKind},
    session::write_clip,
};
use std::{path::Path, process::Command, time::Duration};

const FPS: u32 = 60;
const RATE: u32 = 48_000;
const SECONDS: u32 = 8;
const CLIP: Duration = Duration::from_secs(4);
/// The engine clock (QPC µs) at the first video frame.
const START_US: i64 = 987_654_321;
/// Desktop audio starts this much before the first captured frame.
const AUDIO_LEAD: u32 = 370 * RATE / 1000;
/// The flash and the beep come a quarter second after each whole second of video, away from
/// the 2 s keyframes, so every clip starts with silence before its first beep.
const PHASE_MS: u32 = 250;
const BEEP_MS: u32 = 50;

/// Flash and beep start times (seconds) in the decoded clip, from FFmpeg's own filters.
fn decode_events(path: &Path) -> (Vec<f64>, Vec<f64>) {
    let exe = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party/ffmpeg/bin/ffmpeg.exe");
    let out = Command::new(exe)
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
fn flashes_and_beeps_stay_within_a_frame_in_saved_clips() {
    let c = Catalog::get();
    for id in ["mp4-fragmented", "mov-fragmented", "mp4", "mkv"] {
        let container = &Catalog::clip_container(c.container(id).unwrap()); // what a save writes
        let gh = needs_global_header(container);
        let venc = c.video_encoder("x264").unwrap();
        let mut video = VideoSession::open(venc, &resolve(&c.video_options(venc), None), 256, 144, FPS, gh).unwrap();
        let aenc = c.audio_encoder("aac").unwrap();
        let mut audio = AudioSession::open(aenc, &resolve(&aenc.options, None), RATE, 2, gh).unwrap();
        let mut buf = ReplayBuffer::new(CLIP, usize::MAX);
        let audio_origin = START_US - i64::from(AUDIO_LEAD) * 1_000_000 / i64::from(RATE);

        // Sample `n` plays `n - AUDIO_LEAD` samples after the first frame.
        let mut written = 0u32;
        let mut push_audio = |frames: u32, buf: &mut ReplayBuffer| {
            let mut chunk = Vec::with_capacity(frames as usize * 2);
            for n in written..written + frames {
                let t = (i64::from(n) - i64::from(AUDIO_LEAD)).rem_euclid(i64::from(RATE)) as u32;
                let on = (PHASE_MS * RATE / 1000..(PHASE_MS + BEEP_MS) * RATE / 1000).contains(&t);
                let s = if on { (n as f32 / RATE as f32 * 1000.0 * std::f32::consts::TAU).sin() * 0.5 } else { 0.0 };
                chunk.extend([s, s]);
            }
            written += frames;
            audio.push(&chunk, &mut |p| buf.push_encoded(StreamKind::Audio(0), audio_origin, p)).unwrap();
        };
        push_audio(AUDIO_LEAD, &mut buf);
        for i in 0..SECONDS * FPS {
            let flash = i % FPS == PHASE_MS * FPS / 1000;
            for (n, (data, _)) in video.planes_mut().unwrap().into_iter().flatten().enumerate() {
                data.fill(match n {
                    0 if flash => 235,
                    0 => 16,
                    _ => 128,
                });
            }
            video.send(i.into(), &mut |p| buf.push_encoded(StreamKind::Video, START_US, p)).unwrap();
            push_audio(RATE / FPS, &mut buf);
        }

        let path = std::env::temp_dir().join(format!("backsight-sync-{}-{id}.{}", std::process::id(), container.ext));
        let saved = write_clip(&path, container, &video.params(), Some(&audio.params()), &buf.clip(CLIP)).unwrap();
        let (flashes, beeps) = decode_events(&saved);
        // Release checklist: BACKSIGHT_KEEP_CLIPS=<dir> keeps a copy to play in Photos, a browser and Discord.
        if let Ok(dir) = std::env::var("BACKSIGHT_KEEP_CLIPS") {
            std::fs::copy(&saved, Path::new(&dir).join(format!("sync-{id}.{}", container.ext))).unwrap();
        }
        std::fs::remove_file(&saved).unwrap();
        assert!(flashes.len() >= 3, "{id}: only {} flashes found: {flashes:?}", flashes.len());
        let offsets_ms: Vec<f64> = flashes
            .iter()
            .map(|f| beeps.iter().map(|b| b - f).min_by(|a, b| a.abs().total_cmp(&b.abs())).expect("a beep") * 1000.0)
            .collect();
        println!("{id}: flashes {flashes:?}, beep minus flash {offsets_ms:.1?} ms");
        let frame_ms = 1000.0 / f64::from(FPS);
        assert!(offsets_ms.iter().all(|o| o.abs() < frame_ms), "{id}: off by more than a frame: {offsets_ms:?} ms");
    }
}
