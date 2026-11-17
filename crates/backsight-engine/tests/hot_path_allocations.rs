//! Hot-path allocations (testing.md, tier 1; hard rule 8, DEVELOPMENT.md): a counting global allocator
//! wraps 600 frames of the CI stand-in for the session's hot path after warm-up, and every
//! frame must make zero Rust heap allocations. Per frame that is the pacer, writing an x264
//! frame from system memory, encoding it, a frame's worth of AAC, and both encoders' packets
//! into the replay buffer and a recording's feed (paused and resumed on a requested keyframe
//! midway), as `session.rs` does on its video and audio threads. The recording's writer thread
//! isn't counted. A second measured pass uses synthetic 96/192 KiB video packets after growing
//! the recording buffers from small packets. Both passes hold the writer for 12-frame bursts;
//! writer observations prove resumed video was delivered, not merely accepted by the feed.
//!
//! FFmpeg allocates with its own `av_malloc` (the C runtime), outside Rust's allocator, so this
//! doesn't see it: frame and packet references inside `send_frame`/`receive_packet` and x264's
//! own buffers. The GPU path (WGC copy, conversion, NVENC) needs a GPU and isn't covered.
//! The global allocator is why this test has its own binary.

use backsight_encode::{
    Catalog,
    catalog::resolve,
    mux::needs_global_header,
    session::{AudioSession, Encoded, VideoSession},
};
use backsight_engine::{
    pacer::Pacer,
    recording,
    replay::{ReplayBuffer, StreamKind},
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    sync::{
        Arc,
        atomic::{
            AtomicBool, AtomicU64,
            Ordering::{Relaxed, SeqCst},
        },
    },
    time::{Duration, Instant},
};

struct Counting;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// Only this test's thread is counted (not the test harness, nor threads FFmpeg starts).
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

fn count() {
    if COUNTING.try_with(Cell::get).unwrap_or(false) {
        ALLOCATIONS.fetch_add(1, Relaxed);
    }
}

// SAFETY: forwards every call unchanged to the system allocator.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: the caller's contract is passed straight through.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: as above.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        // SAFETY: as above.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: as above.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const FPS: u32 = 60;
const RATE: u32 = 48_000;
const QPC_HZ: u64 = 10_000_000;
/// Long enough for the buffer to reach its steady size (window + up to two 2 s GOPs), so its
/// packet queue and chunk ring stop growing.
const WARM_UP: u32 = 20 * FPS;
const MEASURED: u32 = 600;
const BURST: u32 = 12;

fn wait_until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "recording writer stalled");
        std::thread::yield_now();
    }
}

fn drain_burst(feed: &recording::Feed, ready: &AtomicBool) {
    ready.store(true, SeqCst);
    wait_until(|| feed.queued_bytes() == 0);
    ready.store(false, SeqCst);
}

#[test]
fn the_hot_path_allocates_nothing_per_frame_after_warm_up() {
    let c = Catalog::get();
    let container = c.container("mp4-fragmented").unwrap();
    let gh = needs_global_header(container);
    let venc = c.video_encoder("x264").unwrap();
    let mut video = VideoSession::open(venc, &resolve(&c.video_options(venc), None), 320, 180, FPS, gh).unwrap();
    let aenc = c.audio_encoder("aac").unwrap();
    let mut audio = AudioSession::open(aenc, &resolve(&aenc.options, None), RATE, 2, gh).unwrap();
    let mut pacer = Pacer::new(FPS, QPC_HZ);
    // A 1 MB ceiling gives 16 KB chunks, so the run goes through a dozen of them.
    let mut buf = ReplayBuffer::new(Duration::from_secs(4), 1 << 20);
    let mut tone = vec![0f32; (RATE / FPS * 2) as usize];
    let frame_ticks = QPC_HZ / u64::from(FPS);
    let (mut feed, sink) = recording::channel(0);
    let ready = Arc::new(AtomicBool::new(false));
    let writer_ready = ready.clone();
    let writer = std::thread::spawn(move || {
        let mut video = Vec::new();
        recording::drain(sink, "x", |p| {
            wait_until(|| writer_ready.load(SeqCst));
            if p.stream == StreamKind::Video {
                video.push(p.pts_us);
            }
            Ok(())
        })
        .unwrap();
        video
    });

    let mut frame = |i: u32| {
        // Paused for a second in the warm-up (a pause holds an audio buffer more) and again in
        // the middle of the measured frames.
        let at_us = i64::from(i) * 1_000_000 / i64::from(FPS);
        if i == WARM_UP / 2 || i == WARM_UP + MEASURED / 3 {
            feed.pause(at_us);
        }
        if i == WARM_UP / 2 + FPS || i == WARM_UP + MEASURED / 3 + FPS {
            feed.resume(at_us);
            video.request_keyframe();
        }
        let now = 1_000_000_000 + u64::from(i) * frame_ticks;
        pacer.frame_arrived(now);
        let slot = pacer.poll(now + frame_ticks / 2).expect("a slot per frame");
        for (n, (data, stride)) in video.planes_mut().unwrap().into_iter().flatten().enumerate() {
            for (row, line) in data.chunks_mut(stride).enumerate() {
                line.iter_mut()
                    .enumerate()
                    .for_each(|(x, p)| *p = if n == 0 { (x * 3 + row + i as usize) as u8 } else { 128 });
            }
        }
        video
            .send(slot.pts as i64, &mut |p| {
                buf.push_encoded(StreamKind::Video, 0, p);
                feed.send(StreamKind::Video, p.pts_us, p.dts_us, p.duration_us, p.keyframe, p.data).unwrap();
            })
            .unwrap();
        for (k, s) in tone.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let t = (i * RATE / FPS + k as u32) as f32 / RATE as f32;
            s.fill((t * 440.0 * std::f32::consts::TAU).sin() * 0.3);
        }
        audio
            .push(&tone, &mut |p| {
                buf.push_encoded(StreamKind::Audio(0), 0, p);
                feed.send(StreamKind::Audio(0), p.pts_us, p.dts_us, p.duration_us, p.keyframe, p.data).unwrap();
            })
            .unwrap();
        if (i + 1).is_multiple_of(BURST) {
            drain_burst(&feed, &ready);
        }
    };

    (0..WARM_UP).for_each(&mut frame);
    COUNTING.set(true);
    (WARM_UP..WARM_UP + MEASURED).for_each(&mut frame);
    COUNTING.set(false);
    let allocations = ALLOCATIONS.load(Relaxed);
    drop(frame);
    ready.store(true, SeqCst);
    drop(feed);
    let written = writer.join().unwrap();
    // Both resumes must reach the writer, well past each cut and encoder delay.
    for end in [WARM_UP, WARM_UP + MEASURED] {
        let pauses = if end == WARM_UP { 1 } else { 2 };
        let last_second = i64::from(end - pauses * FPS - FPS) * 1_000_000 / i64::from(FPS);
        assert!(
            written.iter().any(|&pts| pts >= last_second && pts < last_second + 1_000_000),
            "no video reached the writer after resume before frame {end}"
        );
    }

    println!("{allocations} Rust allocations in {MEASURED} frames; buffer holds {} B", buf.bytes());
    assert_eq!(allocations, 0, "the hot path allocated {allocations} times in {MEASURED} frames");
    large_packets_reuse_grown_buffers_in_bursts();
}

fn large_packets_reuse_grown_buffers_in_bursts() {
    let (mut feed, sink) = recording::channel(0);
    let ready = Arc::new(AtomicBool::new(false));
    let writer_ready = ready.clone();
    let writer = std::thread::spawn(move || {
        let mut frames = Vec::new();
        recording::drain(sink, "x", |p| {
            wait_until(|| writer_ready.load(SeqCst));
            if p.stream == StreamKind::Video {
                frames.push(u32::from_le_bytes(p.data()[..4].try_into().unwrap()));
            }
            Ok(())
        })
        .unwrap();
        frames
    });
    let mut replay = ReplayBuffer::new(Duration::from_secs(1), 32 << 20);
    let mut payload = vec![0x5a; 192 << 10];
    let audio = [0u8; 1024];
    let pause = WARM_UP + MEASURED / 3;
    let resume = pause + FPS;
    let warm_pause = WARM_UP / 2;
    let warm_resume = warm_pause + FPS;
    let mut peak_queued = 0;
    let mut frame = |i: u32| {
        let pts = i64::from(i) * 1_000_000 / i64::from(FPS);
        if i == pause || i == warm_pause {
            feed.pause(pts);
        }
        if i == resume || i == warm_resume {
            feed.resume(pts);
        }
        // First populate the pool below its 64 KiB minimum, then force growth of many
        // simultaneous buffers before measuring alternating large packet sizes.
        let size = if i < 2 * BURST {
            16 << 10
        } else if i.is_multiple_of(2) {
            192 << 10
        } else {
            96 << 10
        };
        payload[..4].copy_from_slice(&i.to_le_bytes());
        let packet = Encoded {
            data: &payload[..size],
            pts_us: pts,
            dts_us: pts,
            duration_us: 1_000_000 / i64::from(FPS),
            keyframe: i.is_multiple_of(30) || i == resume || i == warm_resume,
        };
        replay.push_encoded(StreamKind::Video, 0, packet);
        feed.send(StreamKind::Video, pts, pts, packet.duration_us, packet.keyframe, packet.data).unwrap();
        feed.send(StreamKind::Audio(0), pts, pts, packet.duration_us, false, &audio).unwrap();
        if (i + 1).is_multiple_of(BURST) {
            peak_queued = peak_queued.max(feed.queued_bytes());
            drain_burst(&feed, &ready);
        }
    };
    (0..WARM_UP).for_each(&mut frame);
    ALLOCATIONS.store(0, Relaxed);
    COUNTING.set(true);
    (WARM_UP..WARM_UP + MEASURED).for_each(&mut frame);
    COUNTING.set(false);
    let allocations = ALLOCATIONS.load(Relaxed);
    drop(frame);
    ready.store(true, SeqCst);
    drop(feed);
    let written = writer.join().unwrap();
    let expected: Vec<_> = (0..WARM_UP + MEASURED)
        .filter(|&i| (i < pause || i >= resume) && (i < warm_pause || i >= warm_resume))
        .collect();
    assert_eq!(written, expected, "every unpaused synthetic frame, including resumed video, reaches the writer");
    assert!(peak_queued >= (BURST as usize - 2) * (96 << 10), "large packets must actually queue together");
    println!("{allocations} Rust allocations in {MEASURED} synthetic frames; peak queued {peak_queued} B");
    assert_eq!(allocations, 0, "large-packet burst reuse allocated {allocations} times");
}
