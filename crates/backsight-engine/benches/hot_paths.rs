//! Benchmarks for the pure, GPU-free hot paths. Run: `cargo bench -p backsight-engine --bench hot_paths`.

use backsight_config::Hotkey;
use backsight_encode::session::Encoded;
use backsight_engine::{
    audio::mixer::{Mixer, SourceOptions},
    hotkeys::{Input, Matcher, Modifier},
    pacer::Pacer,
    recording,
    replay::{ReplayBuffer, StreamKind},
};
use criterion::{Criterion, criterion_group, criterion_main};
use std::{
    collections::BTreeMap,
    hint::black_box,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering::Relaxed},
    },
    time::Duration,
};

const QPC_FREQ: u64 = 10_000_000;

/// One second of a 144 Hz source feeding a 60 FPS pacer: a frame arrival every tick, a poll after each.
fn pacer(c: &mut Criterion) {
    c.bench_function("pacer/144hz_source_to_60fps_1s", |b| {
        b.iter(|| {
            let mut p = Pacer::new(60, QPC_FREQ);
            let mut slots = 0u32;
            for i in 0..144u64 {
                let now = i * QPC_FREQ / 144;
                p.frame_arrived(now);
                while let Some(s) = p.poll(now) {
                    black_box(s);
                    slots += 1;
                }
            }
            black_box(slots)
        })
    });
}

/// 20 Mbps video at 60 FPS (keyframe every 2 s) plus audio packets.
struct Mix {
    video: Vec<u8>,
    key: Vec<u8>,
    audio: Vec<u8>,
}

impl Mix {
    fn new() -> Self {
        // 20 Mbps = 2.5 MB/s; a keyframe is about 10x a delta frame.
        let delta = 2_500_000 / (60 + 9 * 60 / 120);
        Mix { video: vec![7; delta], key: vec![7; delta * 10], audio: vec![3; 400] }
    }

    /// Pushes `frames` video frames and their audio, starting at frame `from`.
    fn push(&self, buf: &mut ReplayBuffer, from: i64, frames: i64) {
        for f in from..from + frames {
            let pts = f * 1_000_000 / 60;
            let key = f % 120 == 0;
            let data = if key { &self.key } else { &self.video };
            let e = Encoded { data, pts_us: pts, dts_us: pts, duration_us: 16_667, keyframe: key };
            buf.push_encoded(StreamKind::Video, 0, e);
            if f % 5 == 0 {
                let a = Encoded { data: &self.audio, pts_us: pts, dts_us: pts, duration_us: 21_333, keyframe: false };
                buf.push_encoded(StreamKind::Audio(0), 0, a);
            }
        }
    }
}

fn replay(c: &mut Criterion) {
    let mix = Mix::new();
    let window = Duration::from_secs(120);
    // Warm and at the window: every push now also trims the oldest packets.
    let mut buf = ReplayBuffer::new(window, 512 << 20);
    mix.push(&mut buf, 0, 60 * 130);
    let mut next = 60 * 130;
    c.bench_function("replay/push_encoded_warm_1s_of_video_and_audio", |b| {
        b.iter(|| {
            mix.push(&mut buf, next, 60);
            next += 60;
        })
    });
    c.bench_function("replay/clip_2min", |b| b.iter(|| black_box(buf.clip(window))));
}

/// 256 packets (a few seconds of 60 FPS video) sent, then written by a thread that discards them.
fn recording_round_trip(c: &mut Criterion) {
    let (mut feed, sink) = recording::channel(0);
    let written = Arc::new(AtomicUsize::new(0));
    let w = written.clone();
    let writer = std::thread::spawn(move || {
        recording::drain(sink, "bench", |item| {
            black_box(item);
            w.fetch_add(1, Relaxed);
            Ok(())
        })
    });
    let data = vec![5u8; 41_000];
    let (mut sent, mut pts) = (0usize, 0i64);
    c.bench_function("recording/send_drain_256_packets", |b| {
        b.iter(|| {
            for _ in 0..256 {
                feed.send(StreamKind::Video, pts, pts, 16_667, pts == 0, &data).unwrap();
                pts += 16_667;
            }
            sent += 256;
            while written.load(Relaxed) < sent {
                std::hint::spin_loop();
            }
        })
    });
    drop(feed);
    writer.join().unwrap().unwrap();
}

/// Modifier + key presses that hit, miss and auto-repeat.
fn hotkeys(c: &mut Criterion) {
    let mut bindings = BTreeMap::new();
    for (i, hk) in ["Alt+F10", "Alt+F9", "Ctrl+Shift+F8", "F12", "Alt+F11", "Ctrl+Alt+P"].iter().enumerate() {
        bindings.insert(i as u8, hk.parse::<Hotkey>().unwrap());
    }
    let mut m = Matcher::new(&bindings);
    c.bench_function("hotkeys/press_release_cycle", |b| {
        b.iter(|| {
            for (mods, key) in
                [(Some(Modifier::LAlt), "F10"), (None, "W"), (Some(Modifier::LCtrl), "F7"), (None, "F12")]
            {
                if let Some(md) = mods {
                    m.input(Input::Modifier(md), true);
                }
                black_box(m.input(Input::Key(key), true));
                black_box(m.input(Input::Key(key), true)); // auto-repeat
                black_box(m.input(Input::Key(key), false));
                if let Some(md) = mods {
                    m.input(Input::Modifier(md), false);
                }
            }
        })
    });
}

/// One second of desktop audio and the microphone (10 ms packets) mixed into three tracks.
fn audio_mixer(c: &mut Criterion) {
    let sources = [SourceOptions::default(); 2];
    let mut m = Mixer::new(0, &sources, true);
    let packet = vec![0.25f32; 960];
    let mut t = 0u64;
    c.bench_function("audio/mix_2_sources_separate_tracks_1s", |b| {
        b.iter(|| {
            for _ in 0..100 {
                m.push(0, &packet, t);
                m.push(1, &packet, t);
                t += QPC_FREQ / 100;
                m.mix_to(t, |track, s, time| {
                    black_box((track, s, time));
                });
            }
        })
    });
}

criterion_group!(benches, pacer, replay, recording_round_trip, hotkeys, audio_mixer);
criterion_main!(benches);
