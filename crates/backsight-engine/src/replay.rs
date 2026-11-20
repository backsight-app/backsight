//! Instant Replay: a rolling window of already-encoded packets kept in RAM.
//!
//! Packets arrive from the encoders in decode order. The buffer always starts on a
//! video keyframe so any saved clip is playable from its first frame, and it holds
//! at least `window` of video. Packet bytes go into a ring of fixed-size chunks that are
//! reused, so a running session allocates nothing per packet (hard rule 8, DEVELOPMENT.md). Saving a
//! clip copies `Arc` handles to those chunks, never the bytes, then the muxer writes them out
//! without re-encoding. Design notes: `docs/architecture/replay-buffer.md`.

use backsight_encode::session::Encoded;
use std::{collections::VecDeque, ops::Deref, sync::Arc, time::Duration};

/// A jump in video timestamps longer than this (sleep, a long hang) starts the buffer over.
const GAP_US: i64 = 5_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamKind {
    Video,
    Audio(u8),
}

#[derive(Debug, Clone)]
pub struct Packet {
    pub stream: StreamKind,
    /// Presentation and decode timestamps in microseconds on the engine clock (QPC).
    pub pts_us: i64,
    pub dts_us: i64,
    /// Duration in microseconds; 0 means unknown.
    pub duration_us: i64,
    /// Only meaningful for video: the packet can be decoded on its own.
    pub keyframe: bool,
    pub data: Bytes,
}

/// A packet's bytes: a shared handle to the chunk that holds them. Cloning copies the handle.
#[derive(Debug, Clone)]
pub struct Bytes {
    chunk: Arc<Vec<u8>>,
    start: usize,
    end: usize,
}

impl Deref for Bytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.chunk[self.start..self.end]
    }
}

impl From<Vec<u8>> for Bytes {
    fn from(v: Vec<u8>) -> Self {
        Bytes { end: v.len(), chunk: Arc::new(v), start: 0 }
    }
}

/// A packet in the buffer: its bytes are `len` bytes at `start` in chunk number `chunk`.
#[derive(Debug)]
struct Entry {
    stream: StreamKind,
    pts_us: i64,
    dts_us: i64,
    duration_us: i64,
    keyframe: bool,
    chunk: u64,
    start: usize,
    len: usize,
}

impl Entry {
    fn is_video_keyframe(&self) -> bool {
        self.stream == StreamKind::Video && self.keyframe
    }
}

#[derive(Debug)]
pub struct ReplayBuffer {
    window_us: i64,
    max_bytes: usize,
    packets: VecDeque<Entry>,
    /// The chunks the packets live in, oldest first; the last one is being filled.
    chunks: VecDeque<Arc<Vec<u8>>>,
    /// The number of `chunks[0]`; chunk numbers only grow.
    first_chunk: u64,
    /// Emptied chunks kept for reuse.
    spare: Vec<Arc<Vec<u8>>>,
    chunk_size: usize,
    /// Capacity of `chunks` and of `spare`: together, the RAM the buffer takes.
    held: usize,
    spare_bytes: usize,
    /// The newest video timestamp. Clips and the window are measured in video, which runs
    /// behind audio by the encoder's delay.
    latest_pts_us: i64,
    /// A video keyframe has arrived; before that only audio is held.
    started: bool,
}

impl ReplayBuffer {
    /// `max_bytes` is a hard RAM ceiling that wins over `window`, so a runaway
    /// bitrate (e.g. constant-quality on a noisy scene) can never exhaust memory.
    pub fn new(window: Duration, max_bytes: usize) -> Self {
        Self {
            window_us: window.as_micros() as i64,
            max_bytes,
            packets: VecDeque::new(),
            chunks: VecDeque::new(),
            first_chunk: 0,
            spare: Vec::new(),
            // 2 MB holds a few keyframes even at high bitrates; small ceilings get small chunks.
            chunk_size: (max_bytes / 64).clamp(4 << 10, 2 << 20),
            held: 0,
            spare_bytes: 0,
            latest_pts_us: i64::MIN,
            started: false,
        }
    }

    /// Changes how much the buffer keeps. Zero keeps only the newest GOP or two, enough for a
    /// recording to start on a keyframe while Instant Replay is off.
    pub fn set_window(&mut self, window: Duration) {
        self.window_us = window.as_micros() as i64;
        self.trim();
    }

    /// Copies an encoder's packet into the buffer. `offset_us` moves its timestamps (zero at
    /// the encoder's first input) onto the engine clock.
    pub fn push_encoded(&mut self, stream: StreamKind, offset_us: i64, p: Encoded) {
        let (pts_us, dts_us) = (p.pts_us + offset_us, p.dts_us + offset_us);
        if stream == StreamKind::Video && self.started && pts_us > self.latest_pts_us + GAP_US {
            // Video stopped for a while (the PC slept): a clip across the gap would show one
            // frozen picture for all of it, so start over from the next keyframe.
            tracing::info!(
                "replay buffer cleared after a {} s gap in video",
                (pts_us - self.latest_pts_us) / 1_000_000
            );
            self.started = false;
            self.drop_front_while(|e| e.pts_us < pts_us - GAP_US);
        }
        if !self.started {
            match stream {
                // Nothing before the first keyframe is decodable.
                StreamKind::Video if !p.keyframe => return,
                // The video encoder runs behind, so audio for the keyframe's time came first: keep it.
                // (Until then the buffer holds only audio, in time order.)
                StreamKind::Video => {
                    self.started = true;
                    self.drop_front_while(|e| e.pts_us < dts_us);
                }
                // Until video starts, hold at most one window of audio.
                StreamKind::Audio(_) => {
                    let oldest = pts_us - self.window_us;
                    self.drop_front_while(|e| e.pts_us < oldest);
                }
            }
        }
        if stream == StreamKind::Video {
            self.latest_pts_us = self.latest_pts_us.max(pts_us);
        }
        let (chunk, start) = self.store(p.data);
        self.packets.push_back(Entry {
            stream,
            pts_us,
            dts_us,
            duration_us: p.duration_us,
            keyframe: p.keyframe,
            chunk,
            start,
            len: p.data.len(),
        });
        self.trim();
    }

    /// Adds a packet, copying its bytes like [`Self::push_encoded`].
    pub fn push(&mut self, p: Packet) {
        let encoded = Encoded {
            data: &p.data,
            pts_us: p.pts_us,
            dts_us: p.dts_us,
            duration_us: p.duration_us,
            keyframe: p.keyframe,
        };
        self.push_encoded(p.stream, 0, encoded);
    }

    /// RAM the buffer takes (whole chunks, spares included), for the UI's readout. Within the
    /// ceiling unless a single GOP is bigger than it.
    pub fn bytes(&self) -> usize {
        self.held + self.spare_bytes
    }

    /// How much more RAM the buffer may take before its ceiling.
    pub fn room(&self) -> usize {
        self.max_bytes.saturating_sub(self.bytes())
    }

    /// How much video the buffer covers right now.
    pub fn duration(&self) -> Duration {
        match self.keyframe(0) {
            Some(first) => Duration::from_micros((self.latest_pts_us - self.packets[first].pts_us).max(0) as u64),
            None => Duration::ZERO,
        }
    }

    /// The packets for a clip of (at least) the last `length`: a video keyframe first, then
    /// the video after it and the audio from its decode time on. Returns everything held if
    /// the buffer is shorter than `length`.
    ///
    /// Audio and video reach the buffer with different encoder delays, so audio for the
    /// keyframe's time may sit before it and older audio after it: audio is picked by
    /// timestamp. It starts at the keyframe's decode time, not its display time (they differ
    /// by the B-frame delay), because fragmented MP4 starts every track's decode time at zero;
    /// sharing that origin keeps the tracks in sync in every container.
    pub fn clip(&self, length: Duration) -> Vec<Packet> {
        let cutoff = self.latest_pts_us.saturating_sub(length.as_micros() as i64);
        let last_before = self.packets.iter().rposition(|p| p.is_video_keyframe() && p.pts_us <= cutoff);
        let Some(start) = last_before.or_else(|| self.keyframe(0)) else { return Vec::new() };
        let key = &self.packets[start];
        let rest = self.packets.iter().enumerate().filter(|&(i, p)| match p.stream {
            StreamKind::Video => i > start,
            StreamKind::Audio(_) => p.pts_us >= key.dts_us,
        });
        std::iter::once(key).chain(rest.map(|(_, p)| p)).map(|e| self.packet(e)).collect()
    }

    fn packet(&self, e: &Entry) -> Packet {
        let chunk = self.chunks[(e.chunk - self.first_chunk) as usize].clone();
        let data = Bytes { chunk, start: e.start, end: e.start + e.len };
        Packet {
            stream: e.stream,
            pts_us: e.pts_us,
            dts_us: e.dts_us,
            duration_us: e.duration_us,
            keyframe: e.keyframe,
            data,
        }
    }

    /// Appends `data` to the newest chunk, or starts a new one (a spare if there is one) when
    /// that is full or a clip being saved holds it. Returns the chunk's number and the offset.
    fn store(&mut self, data: &[u8]) -> (u64, usize) {
        let fits = |c: &mut Arc<Vec<u8>>| Arc::get_mut(c).is_some_and(|v| v.capacity() - v.len() >= data.len());
        if !self.chunks.back_mut().is_some_and(fits) {
            if data.len() <= self.chunk_size && self.spare.is_empty() && self.bytes() + self.chunk_size > self.max_bytes
            {
                // At the ceiling: drop the oldest GOP now, so its chunks become the spare instead
                // of allocating one and freeing them after.
                self.trim_to(self.max_bytes - self.chunk_size);
            }
            let spare = if data.len() <= self.chunk_size { self.spare.pop() } else { None };
            let chunk = match spare {
                Some(c) => {
                    self.spare_bytes -= c.capacity();
                    c
                }
                // shortcut: a packet bigger than a chunk gets a chunk of its own (not kept as a
                // spare); fine for rare huge keyframes, raise the chunk size if they get common.
                None => Arc::new(Vec::with_capacity(data.len().max(self.chunk_size))),
            };
            self.held += chunk.capacity();
            self.chunks.push_back(chunk);
        }
        let number = self.first_chunk + self.chunks.len() as u64 - 1;
        let v = Arc::get_mut(self.chunks.back_mut().expect("just ensured")).expect("not shared");
        let start = v.len();
        v.extend_from_slice(data);
        (number, start)
    }

    /// Drops whole GOPs from the front while the rest still covers the window with one GOP to
    /// spare, or while the byte ceiling is exceeded (keeping at least one GOP).
    ///
    /// The spare GOP holds the audio for the first keyframe a full-length clip starts on: the
    /// video encoder runs behind audio, so that audio arrives before the keyframe does.
    fn trim(&mut self) {
        self.trim_to(self.max_bytes);
    }

    /// [`Self::trim`] with `max_bytes` as the ceiling.
    fn trim_to(&mut self, max_bytes: usize) {
        // Over the ceiling (an oversized packet took a new chunk while spares were kept): free
        // spares before video.
        while self.bytes() > max_bytes
            && let Some(c) = self.spare.pop()
        {
            self.spare_bytes -= c.capacity();
        }
        while let Some(next_key) = self.keyframe(1) {
            let clip_start = self.keyframe(2).map(|k| self.packets[k].pts_us);
            let rest_covers_window = clip_start.is_some_and(|t| t <= self.latest_pts_us - self.window_us);
            // `held`, not `bytes()`: released chunks become spares only within the ceiling.
            if !rest_covers_window && self.held <= max_bytes {
                break;
            }
            self.packets.drain(..next_key);
            self.release_chunks();
        }
    }

    fn drop_front_while(&mut self, mut old: impl FnMut(&Entry) -> bool) {
        while self.packets.front().is_some_and(&mut old) {
            self.packets.pop_front();
        }
        self.release_chunks();
    }

    /// Frees the chunks before the oldest packet's. One is kept as a spare if nothing else
    /// holds it (a clip being saved), it is the standard size, and the spares stay within the
    /// ceiling and no bigger than what is in use, so a bitrate drop can't strand RAM.
    fn release_chunks(&mut self) {
        let newest = self.first_chunk + self.chunks.len() as u64;
        let in_use = self.packets.front().map_or(newest, |e| e.chunk);
        // Judge spares against what stays in use once all of these are gone: deciding per chunk,
        // with `held` still near the ceiling, freed a chunk per GOP only to allocate it again.
        let released: usize =
            self.chunks.iter().take(in_use.saturating_sub(self.first_chunk) as usize).map(|c| c.capacity()).sum();
        let held_after = self.held - released;
        let limit = held_after.min(self.max_bytes.saturating_sub(held_after));
        while self.first_chunk < in_use {
            let Some(mut c) = self.chunks.pop_front() else { break };
            self.first_chunk += 1;
            self.held -= c.capacity();
            if c.capacity() == self.chunk_size
                && self.spare_bytes + c.capacity() <= limit
                && let Some(v) = Arc::get_mut(&mut c)
            {
                v.clear();
                self.spare_bytes += c.capacity();
                self.spare.push(c);
            }
        }
    }

    /// Position of the `n`th video keyframe (0 is the front).
    // shortcut: linear scan of two GOPs (~4 s of packets) per push; index keyframes if profiling shows it matters.
    fn keyframe(&self, n: usize) -> Option<usize> {
        self.packets.iter().enumerate().filter(|(_, p)| p.is_video_keyframe()).nth(n).map(|(i, _)| i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME_US: i64 = 16_667; // 60 fps

    /// 60 fps video with a keyframe every `gop` frames plus one audio packet per frame.
    fn feed(buf: &mut ReplayBuffer, frames: i64, gop: i64, size: usize) {
        for i in 0..frames {
            let pts = i * FRAME_US;
            let data: Bytes = vec![0; size].into();
            buf.push(Packet {
                stream: StreamKind::Video,
                pts_us: pts,
                dts_us: pts,
                duration_us: FRAME_US,
                keyframe: i % gop == 0,
                data: data.clone(),
            });
            buf.push(Packet {
                stream: StreamKind::Audio(0),
                pts_us: pts,
                dts_us: pts,
                duration_us: FRAME_US,
                keyframe: true,
                data,
            });
        }
    }

    #[test]
    fn keeps_at_least_the_window_and_starts_on_a_keyframe() {
        let mut buf = ReplayBuffer::new(Duration::from_secs(10), usize::MAX);
        feed(&mut buf, 60 * 60, 120, 100); // one minute, keyframe every 2 s
        let held = buf.duration();
        assert!(held >= Duration::from_secs(10), "held {held:?}");
        assert!(held < Duration::from_secs(14), "trimmed to within two GOPs of the window, held {held:?}");
        assert!(buf.clip(Duration::from_secs(10)).len() < buf.packets.len(), "a full-length clip has a GOP before it");
        assert!(buf.packets.front().unwrap().is_video_keyframe());
    }

    #[test]
    fn before_the_first_keyframe_keeps_only_audio_for_its_time() {
        let mut buf = ReplayBuffer::new(Duration::from_secs(10), usize::MAX);
        let packet = |stream, pts_us, keyframe| Packet {
            stream,
            pts_us,
            dts_us: pts_us,
            duration_us: FRAME_US,
            keyframe,
            data: vec![1].into(),
        };
        buf.push(packet(StreamKind::Video, 0, false));
        assert_eq!(buf.bytes(), 0, "video before the first keyframe can't be decoded");
        for t in [0, 50_000, 100_000] {
            buf.push(packet(StreamKind::Audio(0), t, true));
        }
        assert!(buf.clip(Duration::from_secs(10)).is_empty(), "no clip without video");
        buf.push(packet(StreamKind::Video, 50_000, true));
        let got: Vec<_> = buf.clip(Duration::from_secs(10)).iter().map(|p| (p.stream, p.pts_us)).collect();
        let (v, a) = (StreamKind::Video, StreamKind::Audio(0));
        assert_eq!(got, [(v, 50_000), (a, 50_000), (a, 100_000)], "audio before the keyframe's time dropped");
        assert_eq!(buf.packets.len(), 3);

        let mut buf = ReplayBuffer::new(Duration::from_secs(1), usize::MAX);
        for t in 0..300 {
            buf.push(packet(StreamKind::Audio(0), t * 10_000, true));
        }
        assert_eq!(buf.packets.len(), 101, "without video, at most one window of audio");
    }

    #[test]
    fn a_jump_in_video_time_starts_the_buffer_over() {
        let mut buf = ReplayBuffer::new(Duration::from_secs(120), usize::MAX);
        let packet = |stream, pts_us, keyframe| Packet {
            stream,
            pts_us,
            dts_us: pts_us,
            duration_us: FRAME_US,
            keyframe,
            data: vec![1].into(),
        };
        feed(&mut buf, 60 * 60, 120, 10); // a minute, keyframe every 2 s
        let held = buf.packets.len();
        let hiccup = buf.latest_pts_us + 1_000_000;
        buf.push(packet(StreamKind::Video, hiccup, false));
        assert_eq!(buf.packets.len(), held + 1, "a short stall keeps the buffer");

        // The PC sleeps for an hour. Sound comes back first; the first picture isn't a keyframe.
        let wake = hiccup + 3_600_000_000;
        buf.push(packet(StreamKind::Audio(0), wake, true));
        buf.push(packet(StreamKind::Video, wake, false));
        assert!(buf.clip(Duration::from_secs(120)).is_empty(), "nothing from before the sleep");
        for i in 1..300 {
            buf.push(packet(StreamKind::Video, wake + i * FRAME_US, i % 120 == 0));
            buf.push(packet(StreamKind::Audio(0), wake + i * FRAME_US, true));
        }
        let clip = buf.clip(Duration::from_secs(120));
        assert!(
            clip[0].keyframe && clip[0].pts_us == wake + 120 * FRAME_US,
            "starts on the first keyframe after waking"
        );
        assert!(clip.iter().all(|p| p.pts_us >= wake), "no footage from before the sleep");
        assert!(buf.duration() < Duration::from_secs(5), "held {:?}", buf.duration());
    }

    #[test]
    fn only_a_video_gap_over_the_limit_starts_over() {
        let mut buf = ReplayBuffer::new(Duration::from_secs(120), usize::MAX);
        let packet = |stream, pts_us, keyframe| Packet {
            stream,
            pts_us,
            dts_us: pts_us,
            duration_us: FRAME_US,
            keyframe,
            data: vec![1].into(),
        };
        feed(&mut buf, 600, 120, 10);
        let held = buf.packets.len();
        // Audio far ahead (one bad stamp) leaves the video alone, and so does a gap of exactly
        // the limit.
        buf.push(packet(StreamKind::Audio(0), buf.latest_pts_us + 2 * GAP_US, true));
        buf.push(packet(StreamKind::Video, buf.latest_pts_us + GAP_US, false));
        assert_eq!(buf.packets.len(), held + 2);
        // After a sleep, audio from up to the limit before the new video is kept for its keyframe.
        let wake = buf.latest_pts_us + 3_600_000_000;
        buf.push(packet(StreamKind::Audio(0), wake - GAP_US, true));
        buf.push(packet(StreamKind::Audio(0), wake, true));
        buf.push(packet(StreamKind::Video, wake, false));
        let left: Vec<_> = buf.packets.iter().map(|p| p.pts_us).collect();
        assert_eq!(left, [wake - GAP_US, wake]);
    }

    #[test]
    fn room_is_what_the_ceiling_leaves() {
        let mut buf = ReplayBuffer::new(Duration::from_secs(10), 1 << 20);
        assert_eq!(buf.room(), 1 << 20);
        feed(&mut buf, 1, 1, 10);
        assert!(buf.bytes() > 0);
        assert_eq!(buf.room(), (1 << 20) - buf.bytes());
    }

    #[test]
    fn a_huge_keyframe_at_the_ceiling_frees_spare_chunks_first() {
        let ceiling = 4 << 20; // 64 KB chunks
        let mut buf = ReplayBuffer::new(Duration::from_secs(2), ceiling);
        let mut i = 0;
        while i < 60 * 4 || buf.spare_bytes == 0 {
            feed_encoded(&mut buf, i..i + 1, 50_000, 3_000);
            i += 1;
            assert!(i < 60 * 20, "the window trims, leaving spares");
        }
        assert_eq!(buf.bytes(), buf.held + buf.spare_bytes, "spares count");
        let (huge, pts_us) = (vec![1; 3 << 20], i * FRAME_US); // a chunk of its own
        buf.push_encoded(
            StreamKind::Video,
            0,
            Encoded { data: &huge, pts_us, dts_us: pts_us, duration_us: 0, keyframe: true },
        );
        assert!(buf.bytes() <= ceiling, "{} B held", buf.bytes());
        assert_eq!(buf.spare_bytes, buf.spare.iter().map(|c| c.capacity()).sum::<usize>());
    }

    #[test]
    fn clip_starts_on_a_keyframe_at_or_before_the_requested_length() {
        let mut buf = ReplayBuffer::new(Duration::from_secs(30), usize::MAX);
        feed(&mut buf, 60 * 40, 120, 10);
        let clip = buf.clip(Duration::from_secs(5));
        let first = clip.first().unwrap();
        assert!(first.stream == StreamKind::Video && first.keyframe);
        let span = buf.latest_pts_us - first.pts_us;
        assert!((5_000_000..7_000_000).contains(&span), "span {span}");
    }

    #[test]
    fn clip_takes_audio_by_time_not_by_arrival() {
        let mut buf = ReplayBuffer::new(Duration::from_secs(30), usize::MAX);
        let packet = |stream, pts_us, keyframe| Packet {
            stream,
            pts_us,
            dts_us: pts_us,
            duration_us: FRAME_US,
            keyframe,
            data: vec![0].into(),
        };
        // The video encoder runs 3 frames behind audio: audio up to 50 ms arrives before the
        // keyframe for time 0, and the keyframe at 100 ms arrives after audio for 150 ms.
        buf.push(packet(StreamKind::Video, -100_000, true));
        for t in [-50_000, 0, 50_000] {
            buf.push(packet(StreamKind::Audio(0), t, true));
        }
        buf.push(packet(StreamKind::Video, 0, false));
        buf.push(packet(StreamKind::Audio(0), 100_000, true));
        buf.push(packet(StreamKind::Video, 50_000, false));
        buf.push(packet(StreamKind::Audio(0), 150_000, true));
        buf.push(packet(StreamKind::Video, 100_000, true));
        buf.push(packet(StreamKind::Audio(0), 200_000, true));

        let got: Vec<_> = buf.clip(Duration::ZERO).iter().map(|p| (p.stream, p.pts_us)).collect();
        let (v, a) = (StreamKind::Video, StreamKind::Audio(0));
        assert_eq!(got, [(v, 100_000), (a, 100_000), (a, 150_000), (a, 200_000)]);
        let all: Vec<_> = buf.clip(Duration::from_secs(1)).iter().map(|p| (p.stream, p.pts_us)).collect();
        assert_eq!(all[0], (v, -100_000));
        assert_eq!(all.len(), 10, "every packet once: {all:?}");
    }

    #[test]
    fn clips_hold_the_full_length_of_video_when_video_runs_behind_audio() {
        let mut buf = ReplayBuffer::new(Duration::from_secs(3), usize::MAX);
        let lag = 27; // frames NVENC holds back (look-ahead and B-frames)
        for i in 0..60 * 10 {
            let packet = |stream, pts_us, keyframe| Packet {
                stream,
                pts_us,
                dts_us: pts_us,
                duration_us: FRAME_US,
                keyframe,
                data: vec![0].into(),
            };
            buf.push(packet(StreamKind::Audio(0), i * FRAME_US, true));
            if i >= lag {
                let v = i - lag;
                buf.push(packet(StreamKind::Video, v * FRAME_US, v % 120 == 0));
            }
        }
        let clip = buf.clip(Duration::from_secs(3));
        let video: Vec<_> = clip.iter().filter(|p| p.stream == StreamKind::Video).map(|p| p.pts_us).collect();
        let span = video.last().unwrap() - video[0];
        assert!(span >= 3_000_000 - FRAME_US, "only {span} µs of video in a 3 s clip");
    }

    #[test]
    fn short_buffer_returns_everything() {
        let mut buf = ReplayBuffer::new(Duration::from_secs(30), usize::MAX);
        feed(&mut buf, 60 * 3, 120, 10);
        assert_eq!(buf.clip(Duration::from_secs(30)).len(), buf.packets.len());
    }

    #[test]
    fn byte_ceiling_wins_over_window() {
        let mut buf = ReplayBuffer::new(Duration::from_secs(120), 50_000);
        feed(&mut buf, 60 * 60, 120, 100); // ~24 KB per GOP
        assert!(buf.bytes() <= 50_000, "bytes {}", buf.bytes());
        assert!(buf.packets.front().unwrap().is_video_keyframe());
    }

    /// Video of `key` bytes per keyframe and `delta` per other frame, plus 400-byte audio, through
    /// [`ReplayBuffer::push_encoded`].
    fn feed_encoded(buf: &mut ReplayBuffer, frames: std::ops::Range<i64>, key: usize, delta: usize) {
        let (big, small, audio) = (vec![1; key], vec![2; delta], vec![3; 400]);
        for i in frames {
            let pts_us = i * FRAME_US;
            let keyframe = i % 120 == 0;
            let data = if keyframe { &big } else { &small };
            buf.push_encoded(
                StreamKind::Video,
                0,
                Encoded { data, pts_us, dts_us: pts_us, duration_us: FRAME_US, keyframe },
            );
            buf.push_encoded(
                StreamKind::Audio(0),
                0,
                Encoded { data: &audio, pts_us, dts_us: pts_us, duration_us: FRAME_US, keyframe },
            );
        }
    }

    fn chunk_ptrs(b: &ReplayBuffer) -> Vec<*const u8> {
        b.chunks.iter().chain(&b.spare).map(|c| c.as_ptr()).collect()
    }

    #[test]
    fn chunks_are_reused_and_spares_stay_small() {
        let mut buf = ReplayBuffer::new(Duration::from_secs(4), usize::MAX);
        feed_encoded(&mut buf, 0..60 * 30, 50_000, 3_000);
        let known = chunk_ptrs(&buf);
        feed_encoded(&mut buf, 60 * 30..60 * 90, 50_000, 3_000);
        assert!(buf.first_chunk > 6, "the test runs through several chunks");
        assert!(chunk_ptrs(&buf).iter().all(|p| known.contains(p)), "steady state allocates no chunk");
        assert!(buf.spare_bytes <= buf.held, "spares ({} B) never outgrow the chunks in use", buf.spare_bytes);

        // A bitrate drop frees chunks instead of keeping them all as spares.
        feed_encoded(&mut buf, 60 * 90..60 * 120, 600, 600);
        assert!(buf.spare_bytes <= buf.held, "{} B spare, {} B in use", buf.spare_bytes, buf.held);
    }

    #[test]
    fn at_the_ceiling_chunks_are_still_reused() {
        let ceiling = 4 << 20; // 64 KB chunks; ~1.6 MB per 2 s GOP, so the ceiling trims, not the window
        let mut buf = ReplayBuffer::new(Duration::from_secs(120), ceiling);
        feed_encoded(&mut buf, 0..60 * 30, 50_000, 12_000);
        let known = chunk_ptrs(&buf);
        for i in 60 * 30..60 * 90 {
            feed_encoded(&mut buf, i..i + 1, 50_000, 12_000);
            assert!(buf.bytes() <= ceiling, "{} B held with a {ceiling} B ceiling", buf.bytes());
        }
        assert!(buf.duration() < Duration::from_secs(10), "the ceiling, not the window, is what trims");
        assert!(chunk_ptrs(&buf).iter().all(|p| known.contains(p)), "steady state at the ceiling allocates no chunk");
    }

    #[test]
    fn a_clip_being_saved_keeps_its_bytes_while_the_buffer_moves_on() {
        let mut buf = ReplayBuffer::new(Duration::from_secs(2), 1 << 20);
        feed_encoded(&mut buf, 0..60 * 5, 9_000, 900);
        let clip = buf.clip(Duration::from_secs(2));
        feed_encoded(&mut buf, 60 * 5..60 * 15, 7_000, 700); // trims every packet in `clip`
        let first = buf.packets.front().unwrap().pts_us;
        assert!(clip.iter().all(|p| p.pts_us < first), "the clip's packets have left the buffer");
        for p in &clip {
            let want = match (p.stream, p.keyframe) {
                (StreamKind::Audio(_), _) => (3, 400),
                (_, true) => (1, 9_000),
                (_, false) => (2, 900),
            };
            assert!(p.data.iter().all(|&b| b == want.0) && p.data.len() == want.1, "a chunk a clip holds was reused");
        }
    }

    #[test]
    fn the_ceiling_counts_spare_chunks_and_big_packets() {
        let ceiling = 2_000_000; // 31 KB chunks: every keyframe gets a chunk of its own
        let mut buf = ReplayBuffer::new(Duration::from_secs(120), ceiling);
        for i in 0..60 * 60 {
            feed_encoded(&mut buf, i..i + 1, 60_000, 4_000);
            assert!(buf.bytes() <= ceiling, "{} B held with a {ceiling} B ceiling at frame {i}", buf.bytes());
        }
        assert!(buf.duration() > Duration::from_secs(2), "still holds most of the ceiling");
        assert!(buf.packets.front().unwrap().is_video_keyframe());
    }

    #[test]
    fn a_zero_window_keeps_two_gops_and_still_starts_on_a_keyframe() {
        let mut buf = ReplayBuffer::new(Duration::from_secs(10), 1 << 30);
        feed(&mut buf, 600, 60, 100); // 10 s at 60 FPS, 1 s GOPs
        buf.set_window(Duration::ZERO);
        assert!(buf.duration() <= Duration::from_secs(2), "{:?}", buf.duration());
        let clip = buf.clip(Duration::ZERO);
        assert!(clip[0].keyframe && clip[0].stream == StreamKind::Video);
    }
}
