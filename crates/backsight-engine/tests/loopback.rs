//! Desktop audio on a real device (testing.md, tier 2): capture while Windows plays a beep,
//! and check the stream is continuous on the QPC clock and actually holds the sound. Run with
//! `cargo test -p backsight-engine --test loopback -- --ignored --nocapture`.

use backsight_engine::{
    audio::wasapi::{CHANNELS, Loopback, RATE},
    capture::wgc::{CLOCK_HZ, now},
};
use std::{
    process::Command,
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
    /// QPC time of the first sample louder than the noise floor.
    loud_at: Option<u64>,
}

#[test]
#[ignore = "needs an audio output device; plays a short beep"]
fn loopback_is_continuous_and_hears_the_desktop() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let s = seen.clone();
    let started = now();
    let loopback = Loopback::start(move |samples, time| {
        let mut s = s.lock().unwrap();
        let first = *s.first.get_or_insert(time);
        let expected = first + s.frames * CLOCK_HZ / RATE as u64;
        s.worst_gap = s.worst_gap.max(time.abs_diff(expected));
        s.frames += samples.len() as u64 / CHANNELS as u64;
        s.peak = samples.iter().fold(s.peak, |p, x| p.max(x.abs()));
        if s.loud_at.is_none()
            && let Some(i) = samples.iter().position(|x| x.abs() > 0.01)
        {
            s.loud_at = Some(time + (i / CHANNELS as usize) as u64 * CLOCK_HZ / RATE as u64);
        }
    });
    std::thread::sleep(Duration::from_millis(500)); // silence first: loopback delivers nothing
    let before_beep = now();
    let beep = Command::new("powershell").args(["-NoProfile", "-Command", "[console]::beep(880, 700)"]).status();
    let after_beep = now();
    std::thread::sleep(Duration::from_millis(1500));
    let stats = loopback.stats();
    let stopped = now();
    drop(loopback);

    let s = seen.lock().unwrap();
    let ms = |ticks: u64| ticks / (CLOCK_HZ / 1000);
    let covered = s.frames * 1000 / RATE as u64;
    println!(
        "{covered} ms of audio over {} ms, peak {:.3}, worst gap {} ms, {stats:?}",
        ms(stopped - started),
        s.peak,
        ms(s.worst_gap)
    );
    assert!(beep.is_ok_and(|b| b.success()), "the beep didn't play");
    assert!(stats.last_error.is_none(), "{stats:?}");
    assert!(ms(s.first.unwrap().abs_diff(started)) < 50, "stream starts at the capture start");
    assert!(s.worst_gap < CLOCK_HZ / 1000, "chunks follow each other within 1 ms");
    // Silence is filled up to 100 ms before now, so the stream ends 100–150 ms short of the stop.
    let behind = ms(stopped - started).abs_diff(covered);
    assert!(behind < 200, "stream covers the run: {covered} ms of {} ms", ms(stopped - started));
    assert!(s.peak > 0.01, "heard nothing: is the output muted or the beep routed elsewhere?");
    // The real sync check: the beep's samples carry QPC times from while PowerShell was playing it.
    let loud = s.loud_at.unwrap();
    println!("beep heard {} ms after PowerShell started", ms(loud - before_beep));
    assert!((before_beep..after_beep).contains(&loud), "the beep's audio is stamped outside the time it played");
}
