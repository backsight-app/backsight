//! Desktop audio: WASAPI loopback of the default output device, in shared mode so games and
//! voice chat keep their devices. The capture thread and packet pump here are shared with the
//! microphone and game-only sources. Gotchas are in `docs/reference/platform-notes.md`.

use super::{
    devices::{Follow, Watch},
    timeline::{Placement, Timeline, TimelineStats},
};
use crate::capture::wgc::{CLOCK_HZ, now};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_FAILED},
        Media::{Audio::*, Multimedia::WAVE_FORMAT_IEEE_FLOAT},
        System::{
            Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize},
            Threading::{CreateEventW, WaitForSingleObject},
        },
    },
    core::{Error, HSTRING, Result},
};

/// What every source delivers: 48 kHz interleaved stereo `f32`, whatever the device uses
/// (Windows converts with `AUTOCONVERTPCM`).
pub const RATE: u32 = 48_000;
pub const CHANNELS: u8 = 2;

/// WASAPI's own buffer; packets arrive every 10 ms, so this rides out a 200 ms stall.
const BUFFER_100NS: i64 = 2_000_000;
/// Wake at least this often, to fill silence when loopback goes quiet.
const WAIT_MS: u32 = 20;
/// 10 ms of stereo silence, written in pieces instead of allocating.
static ZEROS: [f32; 960] = [0.0; 960];

/// Counters for the diagnostics page.
#[derive(Debug, Default, Clone)]
pub struct LoopbackStats {
    pub timeline: TimelineStats,
    /// Packets WASAPI flagged as following a glitch (not counting the normal one after each start).
    pub discontinuities: u64,
    /// Failed opens or lost devices (unplugged, audio service restarted); each retries after 500 ms.
    pub device_errors: u64,
    /// The last device error, for the log.
    pub last_error: Option<String>,
    /// What is wrong right now and what to do, in plain words for the UI; `None` while working.
    pub problem: Option<&'static str>,
}

/// One audio capture thread: desktop loopback here, and also the inside of
/// [`Microphone`](super::microphone::Microphone) and
/// [`ProcessLoopback`](super::process_loopback::ProcessLoopback). Dropping it stops the thread.
pub struct Loopback {
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<LoopbackStats>>,
    thread: Option<JoinHandle<()>>,
}

impl Loopback {
    /// Calls `on_audio(samples, time)` with continuous audio from now on: gaps (nothing
    /// playing, a device change) are filled with silence. `time` is the QPC time of the first
    /// frame in [`CLOCK_HZ`] ticks, the same clock as video frames. After a stall of over a
    /// second (the PC slept) `time` jumps forward instead, like the video pacer's skipped slots.
    ///
    /// `on_audio` must not panic: the thread would end and the stream stop. Dropping the
    /// `Loopback` waits for the thread, which can take as long as one Windows audio call.
    ///
    /// Follows the Windows default output: when it changes, capture moves to the new device.
    pub fn start(on_audio: impl FnMut(&[f32], u64) + Send + 'static) -> Self {
        let explain = |_: &Error| "No speakers or headphones found. Plug some in; Backsight keeps trying.";
        let follow = Follow { flow: eRender, device: HSTRING::new() };
        Self::spawn("backsight-loopback", Some(follow), Device::loopback, explain, on_audio)
    }

    /// Runs `open` on a new MTA thread and pumps its packets to `on_audio` as [`Loopback::start`]
    /// describes, reopening every 500 ms after a failure, and at once when `follow`'s endpoint
    /// changes (a new default, the chosen device plugged back in). `explain` turns a failure into [`LoopbackStats::problem`].
    pub(super) fn spawn(
        name: &str,
        follow: Option<Follow>,
        open: impl FnMut() -> Result<Device> + Send + 'static,
        explain: fn(&Error) -> &'static str,
        on_audio: impl FnMut(&[f32], u64) + Send + 'static,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Mutex::new(LoopbackStats::default()));
        let (s, st) = (stop.clone(), stats.clone());
        let thread = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || run(&s, &st, follow, open, explain, on_audio))
            .expect("spawn the audio thread");
        Loopback { stop, stats, thread: Some(thread) }
    }

    pub fn stats(&self) -> LoopbackStats {
        self.stats.lock().expect("audio thread never panics holding the lock").clone()
    }
}

impl Loopback {
    /// Stops and joins the thread; [`Loopback::stats`] keeps the final counts afterwards.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Writes `frames` of silence starting at `time`, unless `stop` is set (a long gap must not
/// hold up shutdown).
fn silence(stop: &AtomicBool, on_audio: &mut impl FnMut(&[f32], u64), mut time: u64, mut frames: u64) {
    while frames > 0 && !stop.load(Ordering::Relaxed) {
        let n = frames.min(ZEROS.len() as u64 / CHANNELS as u64);
        on_audio(&ZEROS[..(n * CHANNELS as u64) as usize], time);
        time += n * CLOCK_HZ / RATE as u64;
        frames -= n;
    }
}

/// Writes the gap part of a placement: time moves past `jump`, then `silence` zero frames.
fn gap(stop: &AtomicBool, on_audio: &mut impl FnMut(&[f32], u64), start: u64, p: Placement) {
    silence(stop, on_audio, start + p.jump * CLOCK_HZ / RATE as u64, p.silence);
}

/// Writes one packet of `frames` that the timeline placed at `start`: its gap, then the frames
/// it keeps, as silence when `data` is `None` (flagged silent).
fn deliver(
    stop: &AtomicBool,
    on_audio: &mut impl FnMut(&[f32], u64),
    start: u64,
    p: Placement,
    data: Option<&[f32]>,
    frames: u64,
) {
    gap(stop, on_audio, start, p);
    let (kept, at) = (frames - p.skip, start + (p.jump + p.silence) * CLOCK_HZ / RATE as u64);
    match data {
        None => silence(stop, on_audio, at, kept),
        Some(all) if kept > 0 => on_audio(&all[p.skip as usize * CHANNELS as usize..], at),
        Some(_) => {}
    }
}

fn run(
    stop: &AtomicBool,
    stats: &Mutex<LoopbackStats>,
    follow: Option<Follow>,
    mut open: impl FnMut() -> Result<Device>,
    explain: fn(&Error) -> &'static str,
    mut on_audio: impl FnMut(&[f32], u64),
) {
    // SAFETY: first COM call on this thread; MTA as WASAPI expects (platform-notes.md).
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    // Set by Windows' device notifications; the timeline below carries on across the reopen,
    // so the switch is a short silence, not a break in the recording.
    let reopen = Arc::new(AtomicBool::new(false));
    let watch = follow.and_then(|f| {
        Watch::register(f, reopen.clone())
            .inspect_err(|e| tracing::warn!("device notifications unavailable, only retrying on failure: {e}"))
            .ok()
    });
    let mut timeline = Timeline::new(RATE, now());
    let mut discontinuities = 0;
    let mut publish = |timeline: &Timeline, discontinuities, error: Option<Error>| {
        let mut s = stats.lock().expect("never poisoned");
        s.timeline = timeline.stats();
        s.discontinuities = discontinuities;
        s.problem = error.as_ref().map(explain);
        if let Some(e) = error {
            s.device_errors += 1;
            s.last_error = Some(format!("{e} ({:#010x})", e.code().0));
        }
    };
    'open: while !stop.load(Ordering::Relaxed) {
        let result = open()
            .and_then(|d| d.pump(stop, &reopen, &mut timeline, &mut discontinuities, &mut on_audio, &mut publish));
        let Err(e) = result else { continue }; // stopped, or the device changed
        publish(&timeline, discontinuities, Some(e));
        // No device (unplugged, audio service restarting): keep the stream going with silence
        // and try again shortly, or as soon as a device comes back.
        for _ in 0..10 {
            if stop.load(Ordering::Relaxed) {
                break 'open;
            }
            if reopen.swap(false, Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
            let t = timeline.next_time();
            let p = timeline.fill_to(now());
            gap(stop, &mut on_audio, t, p);
        }
    }
    drop(watch); // unregister before COM goes away
    // SAFETY: balances CoInitializeEx above; every COM object of this thread is dropped by now.
    unsafe { CoUninitialize() };
}

/// A started capture stream: the client, its capture service and the event WASAPI signals.
pub(super) struct Device {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: HANDLE,
}

impl Device {
    fn loopback() -> Result<Self> {
        // SAFETY: COM calls on live objects from the MTA audio thread.
        let client = unsafe {
            let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?.Activate(CLSCTX_ALL, None)?
        };
        Self::start(client, AUDCLNT_STREAMFLAGS_LOOPBACK)
    }

    /// Initializes `client` in shared, event-driven mode for [`RATE`]/[`CHANNELS`] float, with
    /// Windows converting from the device's own format, plus `flags`; then starts it.
    pub(super) fn start(client: IAudioClient, flags: u32) -> Result<Self> {
        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_IEEE_FLOAT as u16,
            nChannels: CHANNELS.into(),
            nSamplesPerSec: RATE,
            nAvgBytesPerSec: RATE * CHANNELS as u32 * 4,
            nBlockAlign: CHANNELS as u16 * 4,
            wBitsPerSample: 32,
            cbSize: 0,
        };
        let flags = flags
            | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        // SAFETY: COM calls on live objects from an MTA thread; `format` outlives Initialize.
        unsafe {
            client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER_100NS, 0, &format, None)?;
            let capture = client.GetService()?; // before the event, so a failure can't leak it
            let device = Device { capture, client, event: CreateEventW(None, false, false, None)? };
            device.client.SetEventHandle(device.event)?;
            device.client.Start()?;
            Ok(device)
        }
    }

    /// Reads packets until `stop` or `reopen` (`Ok`) or a device error (`Err`, e.g. unplugged).
    fn pump(
        &self,
        stop: &AtomicBool,
        reopen: &AtomicBool,
        timeline: &mut Timeline,
        discontinuities: &mut u64,
        on_audio: &mut impl FnMut(&[f32], u64),
        publish: &mut impl FnMut(&Timeline, u64, Option<Error>),
    ) -> Result<()> {
        let mut first_packet = true;
        while !stop.load(Ordering::Relaxed) && !reopen.swap(false, Ordering::Relaxed) {
            // SAFETY: a live event handle owned by `self`.
            if unsafe { WaitForSingleObject(self.event, WAIT_MS) } == WAIT_FAILED {
                return Err(Error::from_thread());
            }
            // SAFETY: COM calls on the live capture client. GetBuffer's data stays valid until
            // ReleaseBuffer, and holds `frames` frames in the format given to Initialize.
            unsafe {
                while self.capture.GetNextPacketSize()? > 0 {
                    let (mut data, mut frames, mut flags, mut qpc) = (std::ptr::null_mut(), 0, 0, 0);
                    self.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))?;
                    let flag = |f: _AUDCLNT_BUFFERFLAGS| flags & f.0 as u32 != 0;
                    // The first packet after Start is always flagged; only later ones are glitches.
                    *discontinuities += (flag(AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY) && !first_packet) as u64;
                    first_packet = false;
                    let start = timeline.next_time();
                    // `qpc` is already in 100 ns units. Without a usable stamp, assume the packet follows on.
                    let time = timeline.packet_time(qpc, flag(AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR), now());
                    let p = timeline.place(time, frames.into());
                    let audible = !flag(AUDCLNT_BUFFERFLAGS_SILENT);
                    let samples = audible
                        .then(|| std::slice::from_raw_parts(data as *const f32, frames as usize * CHANNELS as usize));
                    deliver(stop, on_audio, start, p, samples, frames.into());
                    self.capture.ReleaseBuffer(frames)?;
                }
            }
            let t = timeline.next_time();
            let p = timeline.fill_to(now());
            gap(stop, on_audio, t, p);
            publish(timeline, *discontinuities, None);
        }
        Ok(())
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: stops the stream before its event closes; both are owned by `self`.
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_comes_in_whole_frames_with_advancing_times() {
        let mut got = Vec::new();
        silence(&AtomicBool::new(false), &mut |s: &[f32], t| got.push((s.len(), t)), 1000, 1000);
        assert_eq!(got, [(960, 1000), (960, 1000 + 100_000), (80, 1000 + 200_000)]);
    }

    #[test]
    fn stopping_interrupts_a_long_silence() {
        let stop = AtomicBool::new(false);
        let mut calls = 0;
        silence(
            &stop,
            &mut |_: &[f32], _| {
                calls += 1;
                if calls == 3 {
                    stop.store(true, Ordering::Relaxed);
                }
            },
            0,
            48_000 * 3600,
        );
        assert_eq!(calls, 3, "an hour of silence stops after the third piece");
    }

    /// `(samples, time, all zero)` for each call `deliver` makes.
    fn delivered(p: Placement, data: Option<&[f32]>, frames: u64) -> Vec<(usize, u64, bool)> {
        let mut got = Vec::new();
        let mut on_audio = |s: &[f32], t| got.push((s.len(), t, s.iter().all(|x| *x == 0.0)));
        deliver(&AtomicBool::new(false), &mut on_audio, 0, p, data, frames);
        got
    }

    #[test]
    fn a_silent_packet_keeps_its_time_as_silence() {
        let loud = [0.5; 480 * 2];
        let none = Placement { jump: 0, silence: 0, skip: 0 };
        assert_eq!(delivered(none, Some(&loud), 480), [(960, 0, false)]);
        // Flagged silent: the same frames at the same time, all zero.
        assert_eq!(delivered(none, None, 480), [(960, 0, true)]);
        // After 5 ms of gap silence the packet follows on, silent or not.
        let gap = Placement { jump: 0, silence: 240, skip: 0 };
        assert_eq!(delivered(gap, None, 480), [(480, 0, true), (960, 50_000, true)]);
        assert_eq!(delivered(gap, Some(&loud), 480), [(480, 0, true), (960, 50_000, false)]);
    }

    #[test]
    fn overlapping_frames_are_trimmed_from_the_front() {
        let p = Placement { jump: 0, silence: 0, skip: 80 };
        assert_eq!(delivered(p, Some(&[0.5; 480 * 2]), 480), [(800, 0, false)]);
        assert_eq!(delivered(p, None, 480), [(800, 0, true)]);
        let all = Placement { jump: 0, silence: 0, skip: 480 };
        assert_eq!(delivered(all, Some(&[0.5; 480 * 2]), 480), []);
    }

    #[test]
    fn a_device_switch_becomes_silence_on_the_same_timeline() {
        const MS: u64 = CLOCK_HZ / 1000;
        let t0 = 1000 * CLOCK_HZ;
        let stop = AtomicBool::new(false);
        let loud = [0.5; 480 * 2];
        let mut timeline = Timeline::new(RATE, t0);
        // (time, frames, silent) for every write, as the pump and retry loop make them.
        let mut got = Vec::new();
        let mut on_audio = |s: &[f32], t| got.push((t, s.len() as u64 / 2, s[0] == 0.0));
        let packet = |timeline: &mut Timeline, on_audio: &mut dyn FnMut(&[f32], u64), at| {
            let start = timeline.next_time();
            let p = timeline.place(at, 480);
            deliver(&stop, &mut |s: &[f32], t| on_audio(s, t), start, p, Some(&loud), 480);
        };
        // The old device plays 100 ms, then the default changes and the new one takes 35 ms to
        // open, with wakes in between (they stay 100 ms behind, so they write nothing yet).
        (0..10).for_each(|i| packet(&mut timeline, &mut on_audio, t0 + i * 10 * MS));
        for wake in [110, 130] {
            let (t, p) = (timeline.next_time(), timeline.fill_to(t0 + wake * MS));
            gap(&stop, &mut on_audio, t, p);
        }
        (0..10).for_each(|i| packet(&mut timeline, &mut on_audio, t0 + (135 + i * 10) * MS));

        let mut at = t0;
        for &(time, frames, _) in &got {
            assert_eq!(time, at, "every write starts where the last one ended");
            at += frames * CLOCK_HZ / RATE as u64;
        }
        assert_eq!(at, t0 + 235 * MS);
        let silent: u64 = got.iter().filter(|w| w.2).map(|w| w.1).sum();
        assert_eq!(silent, 35 * 48, "the switch is exactly the gap, as silence");
        assert_eq!(timeline.stats().jumped_frames, 0);
    }

    /// Hardware tier: loopback of the default output delivers 48 000 frames per second of QPC
    /// time whatever the device's own rate (44.1 kHz, say), so `AUTOCONVERTPCM`
    /// really resamples. A silent stream of our own keeps the device running without sound.
    #[test]
    #[ignore = "needs audio devices"]
    fn loopback_delivers_48k_whatever_the_device_rate() {
        use windows::Win32::System::Com::CoTaskMemFree;
        // SAFETY: test-thread COM setup and calls on live objects; the mix format outlives
        // Initialize, and each GetBuffer is released before the next.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).unwrap();
            let endpoint = enumerator.GetDefaultAudioEndpoint(eRender, eConsole).unwrap();
            let render: IAudioClient = endpoint.Activate(CLSCTX_ALL, None).unwrap();
            let mix = render.GetMixFormat().unwrap();
            let device_rate = (*mix).nSamplesPerSec;
            render.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, BUFFER_100NS, 0, mix, None).unwrap();
            CoTaskMemFree(Some(mix as _));
            render.Start().unwrap();
            let device = Device::loopback().unwrap();
            // Frames between the first and the last packet over their QPC stamps, so queued
            // packets and wake-up timing don't skew it.
            let (mut frames, mut first, mut last, mut previous) = (0u64, None, 0, 0);
            let end = now() + 3 * CLOCK_HZ;
            while now() < end {
                std::thread::sleep(Duration::from_millis(10));
                while device.capture.GetNextPacketSize().unwrap() > 0 {
                    let (mut data, mut n, mut flags, mut qpc) = (std::ptr::null_mut(), 0, 0, 0);
                    device.capture.GetBuffer(&mut data, &mut n, &mut flags, None, Some(&mut qpc)).unwrap();
                    device.capture.ReleaseBuffer(n).unwrap();
                    // The previous packet's frames fill the time up to this packet's stamp.
                    frames += previous;
                    previous = u64::from(n);
                    first.get_or_insert(qpc);
                    last = qpc;
                }
            }
            let rate = frames * CLOCK_HZ / (last - first.unwrap());
            let _ = render.Stop();
            println!("device mix {device_rate} Hz, loopback delivered {rate} frames/s");
            assert!((47_500..48_500).contains(&rate), "{rate}");
        }
    }
}
