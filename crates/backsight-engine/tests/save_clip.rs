//! Saving a clip (testing.md, tier 1): x264 and AAC packets go through the replay buffer on a
//! QPC-like clock, with the video encoder's look-ahead putting video behind audio as on real
//! hardware, and B-frames putting decode time before display time. The saved clip must start
//! on a keyframe, map engine time to file time identically on both tracks (A/V sync), and
//! cover the requested length.

use backsight_encode::{
    Catalog,
    catalog::resolve,
    mux::needs_global_header,
    session::{AudioSession, Encoded, VideoSession},
};
use backsight_engine::{
    replay::{ReplayBuffer, StreamKind},
    session::write_clip,
};
use serde_json::Value;
use std::{path::Path, process::Command, time::Duration};

const FPS: u32 = 30;
const RATE: u32 = 48_000;
const SECONDS: u32 = 6;
const CLIP: Duration = Duration::from_secs(2);
/// The engine clock at the first frame: QPC microseconds, far from zero.
const START_US: i64 = 123_456_789;

pub fn ffprobe(path: &Path, args: &[&str]) -> Value {
    let exe = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party/ffmpeg/bin/ffprobe.exe");
    let out =
        Command::new(exe).args(["-v", "error", "-of", "json"]).args(args).arg(path).output().expect("run ffprobe");
    assert!(out.status.success(), "ffprobe failed: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

fn push(buf: &mut ReplayBuffer, stream: StreamKind, p: Encoded) {
    buf.push_encoded(stream, START_US, p);
}

#[test]
fn a_clip_from_the_buffer_starts_on_a_keyframe_with_audio_in_sync() {
    let c = Catalog::get();
    for id in ["mp4-fragmented", "mov-fragmented", "mp4", "mkv"] {
        let container = &Catalog::clip_container(c.container(id).unwrap()); // what a save writes
        let gh = needs_global_header(container);
        let venc = c.video_encoder("x264").unwrap();
        let mut video = VideoSession::open(venc, &resolve(&c.video_options(venc), None), 256, 144, FPS, gh).unwrap();
        let aenc = c.audio_encoder("aac").unwrap();
        let mut audio = AudioSession::open(aenc, &resolve(&aenc.options, None), RATE, 2, gh).unwrap();

        let mut buf = ReplayBuffer::new(CLIP, usize::MAX);
        let tone = vec![0.25f32; (RATE / FPS * 2) as usize];
        for i in 0..SECONDS * FPS {
            for (n, (data, stride)) in video.planes_mut().unwrap().into_iter().flatten().enumerate() {
                for (row, line) in data.chunks_mut(stride).enumerate() {
                    line.iter_mut()
                        .enumerate()
                        .for_each(|(x, p)| *p = if n == 0 { (x + row) as u8 ^ i as u8 } else { 128 });
                }
            }
            video.send(i.into(), &mut |p| push(&mut buf, StreamKind::Video, p)).unwrap();
            audio.push(&tone, &mut |p| push(&mut buf, StreamKind::Audio(0), p)).unwrap();
        }

        let path = std::env::temp_dir().join(format!("backsight-save-{}-{id}.{}", std::process::id(), container.ext));
        let clip = buf.clip(CLIP);
        for packet in &clip {
            let expected = match packet.stream {
                StreamKind::Video => 1_000_000 / i64::from(FPS),
                StreamKind::Audio(_) => 1024 * 1_000_000 / i64::from(RATE),
            };
            assert!((packet.duration_us - expected).abs() <= 1, "{id}: packet duration {}", packet.duration_us);
        }
        let key = &clip[0];
        let first_audio = clip.iter().find(|p| p.stream == StreamKind::Audio(0)).unwrap();
        assert!(key.dts_us < key.pts_us, "x264 uses B-frames by default, which this test relies on");
        let saved = write_clip(&path, container, &video.params(), Some(&audio.params()), &clip).unwrap();
        let info = ffprobe(&saved, &["-show_entries", "stream=codec_type,start_time:format=duration"]);
        let start = |kind: &str| -> f64 {
            let s = info["streams"].as_array().unwrap().iter().find(|s| s["codec_type"] == kind).unwrap();
            s["start_time"].as_str().unwrap().parse().unwrap()
        };
        let (v, a) = (start("video"), start("audio"));
        println!("{id}: video starts {v}, audio {a}");
        // In sync: the file puts the first audio packet and the keyframe as far apart as they
        // were on the engine clock (within timestamp rounding).
        let want = (first_audio.pts_us - key.pts_us) as f64 / 1e6;
        assert!(((a - v) - want).abs() < 0.001, "{id}: audio is {:.1} ms off", ((a - v) - want) * 1000.0);
        assert!((0.0..0.022).contains(&a), "{id}: audio starts within one AAC frame of the file, at {a}");
        let duration: f64 = info["format"]["duration"].as_str().unwrap().parse().unwrap();
        // At least the requested length, at most one 2 s GOP more.
        assert!((1.95..4.1).contains(&duration), "{id}: duration {duration}");
        let first =
            ffprobe(&saved, &["-select_streams", "v:0", "-read_intervals", "%+#1", "-show_entries", "frame=key_frame"]);
        assert_eq!(first["frames"][0]["key_frame"], 1, "{id}: starts on a keyframe");
        let packets =
            ffprobe(&saved, &["-select_streams", "v:0", "-show_packets", "-show_entries", "packet=duration_time"]);
        for packet in packets["packets"].as_array().unwrap() {
            let duration: f64 = packet["duration_time"].as_str().unwrap().parse().unwrap();
            assert!((duration - 1.0 / f64::from(FPS)).abs() < 0.001, "{id}: video packet duration {duration}");
        }
        std::fs::remove_file(saved).unwrap();
    }
}
