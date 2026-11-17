//! Microphone and game-only audio on a real PC (testing.md, tier 2): capture ~2 s and check
//! the stream is continuous on the QPC clock. Run with
//! `cargo test -p backsight-engine --test audio_sources -- --ignored --nocapture`.

use backsight_engine::{
    audio::{
        microphone::Microphone,
        process_loopback::ProcessLoopback,
        wasapi::{CHANNELS, LoopbackStats, RATE},
    },
    capture::wgc::{CLOCK_HZ, now},
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Default)]
struct Seen {
    first: Option<u64>,
    frames: u64,
    /// Largest difference between a chunk's time and where the previous chunks ended.
    worst_gap: u64,
    peak: f32,
}

type Shared = Arc<Mutex<Seen>>;

fn recorder(seen: &Shared) -> impl FnMut(&[f32], u64) + Send + 'static {
    let s = seen.clone();
    move |samples, time| {
        let mut s = s.lock().unwrap();
        let first = *s.first.get_or_insert(time);
        let expected = first + s.frames * CLOCK_HZ / RATE as u64;
        s.worst_gap = s.worst_gap.max(time.abs_diff(expected));
        s.frames += samples.len() as u64 / CHANNELS as u64;
        s.peak = samples.iter().fold(s.peak, |p, x| p.max(x.abs()));
    }
}

/// Checks a run from `started` to `stopped` was one continuous stream with real timestamps.
fn check(seen: &Shared, stats: &LoopbackStats, started: u64, stopped: u64) {
    let s = seen.lock().unwrap();
    let ms = |ticks: u64| ticks / (CLOCK_HZ / 1000);
    let covered = s.frames * 1000 / RATE as u64;
    println!(
        "{covered} ms of audio over {} ms, peak {:.4}, worst gap {} ms, {stats:?}",
        ms(stopped - started),
        s.peak,
        ms(s.worst_gap)
    );
    assert!(stats.last_error.is_none() && stats.problem.is_none(), "{stats:?}");
    assert!(ms(s.first.unwrap().abs_diff(started)) < 50, "stream starts at the capture start");
    assert!(s.worst_gap < CLOCK_HZ / 1000, "chunks follow each other within 1 ms");
    let behind = ms(stopped - started).abs_diff(covered);
    assert!(behind < 200, "stream covers the run: {covered} ms of {} ms", ms(stopped - started));
    // Bad packet timestamps would show as trimmed or jumped frames.
    assert_eq!((stats.timeline.trimmed_frames, stats.timeline.jumped_frames), (0, 0), "{stats:?}");
}

#[test]
#[ignore = "needs a microphone, allowed in Windows privacy settings"]
fn microphone_is_continuous() {
    let seen = Shared::default();
    let started = now();
    let mic = Microphone::start("default", recorder(&seen));
    std::thread::sleep(Duration::from_millis(2000));
    let stats = mic.stats();
    let stopped = now();
    drop(mic);

    check(&seen, &stats, started, stopped);
    // The mic delivers packets all the time, so only the start-up latency is filled.
    assert!(stats.timeline.silence_frames < RATE as u64 / 5, "{stats:?}");
}

#[test]
#[ignore = "needs Windows 10 2004 or newer and an audio output device"]
fn process_loopback_of_a_silent_process_is_continuous() {
    let seen = Shared::default();
    let started = now();
    // This test process plays nothing: the stream must still be continuous silence.
    let game = ProcessLoopback::start(std::process::id(), recorder(&seen)).expect("supported Windows");
    std::thread::sleep(Duration::from_millis(2000));
    let stats = game.stats();
    let stopped = now();
    drop(game);

    check(&seen, &stats, started, stopped);
    assert_eq!(seen.lock().unwrap().peak, 0.0, "a process that plays nothing is silent");
}
