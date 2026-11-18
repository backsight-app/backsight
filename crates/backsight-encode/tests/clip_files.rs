//! File validity (testing.md, tier 1): encode a synthetic clip with x264 and each audio
//! encoder, mux it into each catalog container, and check the result with `ffprobe`.

use backsight_encode::{
    Catalog,
    catalog::resolve,
    mux::{Muxer, Track, needs_global_header},
    session::{AudioSession, Encoded, VideoSession},
};
use serde_json::Value;
use std::{path::PathBuf, process::Command};

const FPS: u32 = 30;
const RATE: u32 = 48_000;

fn ffprobe(path: &PathBuf, args: &[&str]) -> Value {
    let exe = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/ffmpeg/bin/ffprobe.exe");
    let out =
        Command::new(exe).args(["-v", "error", "-of", "json"]).args(args).arg(path).output().expect("run ffprobe");
    assert!(out.status.success(), "ffprobe failed: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

/// Encodes `seconds` of a moving gradient and a 440 Hz tone and returns the saved file.
fn make_clip(container_id: &str, audio_id: &str, seconds: u32) -> PathBuf {
    let c = Catalog::get();
    let container = c.container(container_id).unwrap();
    let gh = needs_global_header(container);
    let venc = c.video_encoder("x264").unwrap();
    let mut video = VideoSession::open(venc, &resolve(&c.video_options(venc), None), 256, 144, FPS, gh).unwrap();
    let aenc = c.audio_encoder(audio_id).unwrap();
    let mut audio = AudioSession::open(aenc, &resolve(&aenc.options, None), RATE, 2, gh).unwrap();

    let path = std::env::temp_dir().join(format!(
        "backsight-test-{}-{container_id}-{audio_id}.{}",
        std::process::id(),
        container.ext
    ));
    let mut mux = Muxer::create(&path, container, &video.params(), &[&audio.params()]).unwrap();
    let samples_per_frame = (RATE / FPS) as usize;
    let mut tone = vec![0f32; samples_per_frame * 2];
    for i in 0..(seconds * FPS) as usize {
        for (n, plane) in video.planes_mut().unwrap().into_iter().flatten().enumerate() {
            let (data, stride) = plane;
            for (row, line) in data.chunks_mut(stride).enumerate() {
                line.iter_mut().enumerate().for_each(|(x, p)| *p = if n == 0 { (x + row + i * 4) as u8 } else { 128 });
            }
        }
        video.send(i as i64, &mut |p| mux.write(Track::Video, &p).unwrap()).unwrap();
        for (k, s) in tone.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let t = (i * samples_per_frame + k) as f32 / RATE as f32;
            s.fill((t * 440.0 * std::f32::consts::TAU).sin() * 0.5);
        }
        audio.push(&tone, &mut |p| mux.write(Track::Audio(0), &p).unwrap()).unwrap();
    }
    video.finish(&mut |p| mux.write(Track::Video, &p).unwrap()).unwrap();
    audio.finish(&mut |p| mux.write(Track::Audio(0), &p).unwrap()).unwrap();
    let saved = mux.finish().unwrap();
    assert!(!PathBuf::from(format!("{}.part", saved.display())).exists(), "part file renamed");
    saved
}

#[test]
fn every_h264_aac_container_gives_a_valid_clip() {
    let c = Catalog::get();
    let containers = c
        .containers
        .iter()
        .filter(|k| k.video_codecs.iter().any(|v| v == "h264") && k.audio_encoders.iter().any(|a| a == "aac"));
    for k in containers {
        let path = make_clip(&k.id, "aac", 2);
        let info = ffprobe(&path, &["-show_entries", "stream=codec_type,codec_name:format=duration"]);
        let streams: Vec<_> = info["streams"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| (s["codec_type"].as_str().unwrap().to_owned(), s["codec_name"].as_str().unwrap().to_owned()))
            .collect();
        assert_eq!(streams, [("video".into(), "h264".into()), ("audio".into(), "aac".into())], "{}", k.id);
        let duration: f64 = info["format"]["duration"].as_str().unwrap().parse().unwrap();
        assert!((duration - 2.0).abs() < 0.15, "{}: duration {duration}", k.id);
        let first =
            ffprobe(&path, &["-select_streams", "v:0", "-read_intervals", "%+#1", "-show_entries", "frame=key_frame"]);
        assert_eq!(first["frames"][0]["key_frame"], 1, "{}: starts on a keyframe", k.id);
        std::fs::remove_file(path).unwrap();
    }
}

#[test]
fn every_audio_encoder_writes_a_playable_track() {
    for a in &Catalog::get().audio_encoders {
        let path = make_clip("mkv", &a.id, 1);
        let info =
            ffprobe(&path, &["-select_streams", "a:0", "-show_entries", "stream=codec_name,sample_rate,channels"]);
        let s = &info["streams"][0];
        assert_eq!((s["sample_rate"].as_str(), s["channels"].as_i64()), (Some("48000"), Some(2)), "{}", a.id);
        std::fs::remove_file(path).unwrap();
    }
}

#[test]
#[ignore = "needs NVIDIA HEVC Main 10 support; synthetic input, no desktop capture"]
fn p010_hdr_signaling_survives_replay_and_recording_containers() {
    let c = Catalog::get();
    let venc = c.video_encoder("nvenc-hevc").unwrap();
    let values = resolve(&c.video_options(venc), None);
    for id in ["mp4-fragmented", "mkv"] {
        let record = c.container(id).unwrap();
        let clip = Catalog::clip_container(record);
        let mut video =
            VideoSession::open_hdr(venc, &values, (256, 144), FPS, needs_global_header(&clip), None).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut writers = [("recording", record), ("replay", &clip)].map(|(name, container)| {
            let path = dir.path().join(format!("{name}.{}", container.ext));
            Muxer::create(&path, container, &video.params(), &[]).unwrap()
        });
        let mut write = |p: Encoded<'_>| {
            for mux in &mut writers {
                mux.write(Track::Video, &p).unwrap();
            }
        };
        for i in 0..FPS {
            let [Some((y, _)), Some((uv, _)), None] = video.planes_mut().unwrap() else {
                panic!("expected P010");
            };
            for pixel in y.as_chunks_mut::<2>().0 {
                *pixel = (723_u16 << 6).to_le_bytes(); // ~1,000 nits
            }
            for pixel in uv.as_chunks_mut::<2>().0 {
                *pixel = (512_u16 << 6).to_le_bytes();
            }
            video.send(i.into(), &mut write).unwrap();
        }
        video.finish(&mut write).unwrap();
        for mux in writers {
            let path = mux.finish().unwrap();
            let info = ffprobe(&path, &["-show_streams", "-show_frames", "-read_intervals", "%+#3"]);
            for entry in [&info["streams"][0], &info["frames"][0]] {
                assert_eq!(entry["pix_fmt"], "yuv420p10le");
                assert_eq!(entry["color_primaries"], "bt2020");
                assert_eq!(entry["color_space"], "bt2020nc");
                assert_eq!(entry["color_transfer"], "smpte2084");
                assert_eq!(entry["color_range"], "tv");
            }
            assert_eq!(info["streams"][0]["profile"], "Main 10");
        }
    }
}

#[test]
fn speech_reading_gives_16_khz_mono_from_the_start() {
    use backsight_encode::media::{SPEECH_RATE, read_speech};
    let clip = make_clip("mp4", "aac", 2);
    let speech = read_speech(&clip, None).unwrap();
    let seconds = speech.len() as f32 / SPEECH_RATE as f32;
    assert!((1.9..2.2).contains(&seconds), "{seconds} s");
    // The 440 Hz tone at half volume, folded to mono: loud, not silence or noise.
    let rms = (speech.iter().map(|s| s * s).sum::<f32>() / speech.len() as f32).sqrt();
    assert!(rms > 0.2, "rms {rms}");
    assert_eq!(read_speech(&clip, Some(0)).unwrap().len(), speech.len(), "track 0 is the only, and last, one");
    assert!(read_speech(&clip, Some(1)).is_err(), "no second sound track");
    let _ = std::fs::remove_file(clip);
}
