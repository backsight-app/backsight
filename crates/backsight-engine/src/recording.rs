//! Continuous recording: the running session's encoded packets, written to a file by a thread
//! of its own. No second encoder: a recording starts on the newest keyframe the replay buffer
//! holds, then takes every packet after it.
//!
//! The encoder threads hand packets over through [`Feed::send`], which copies the bytes into a
//! pool of reused buffers (no allocation per packet once warm, hard rule 8, DEVELOPMENT.md) and never
//! blocks. A disk that falls behind fills the queue or the byte budget, and the recording stops
//! with what was written so far instead of losing packets or growing memory without limit.
//!
//! The feed also makes the cuts: pauses and split files. Video decides where a cut falls (a
//! file or a resume must start on a keyframe), and audio follows by timestamp:
//! - Pause at time P: video ends before the first frame decoded at or after P that is shown
//!   after every earlier frame (so nothing kept needs it); call its decode time D. Audio ends at D.
//! - Resume at R: video starts again at the first keyframe K shown at or after R. Both tracks
//!   lose the same span, about K's decode time minus D, so they stay in sync. The span is a whole
//!   number of audio packets, so the audio carries on without a gap or overlap; audio resumes
//!   within one packet of K's decode time.
//! - Split: a new file starts at the first keyframe once a part has `split_every` of video,
//!   with the audio from that keyframe's decode time on, as a saved clip does.
//!
//! Cuts are decided by timestamp, whichever track arrives first. Audio usually arrives ahead of
//! video (the video encoder holds frames back), so each audio packet waits in `held` until video
//! has passed its time and the cut it falls under is known. When audio lags (no B-frames, a slow
//! audio thread), each video packet waits in `waiting` until audio has arrived one packet past
//! its decode time, so a resume or split sees every audio packet around its keyframe.
//!
//! Markers are cut the same way: one at time M is placed once video has been decoded past M, in
//! the part being written then, moved by the time paused so far. One added while paused lands
//! where the pause cut the file.

use crate::{
    replay::{Bytes, Packet, StreamKind},
    session::{lock, track},
};
use backsight_encode::{mux::Muxer, session::Encoded};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{
            AtomicU64, AtomicUsize,
            Ordering::{Acquire, Relaxed, Release},
        },
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    time::Duration,
};

/// How far before a resume press its keyframe may be shown and still be used.
const RESUME_SLACK_US: i64 = 100_000;
/// A longer gap between video packets (sleep, a stalled capture) is cut out like a pause.
const MAX_GAP_US: i64 = 5_000_000;
/// Packets waiting for the disk (about 9 s of 60 FPS video plus audio), besides the backlog.
const MAX_PACKETS: usize = 1024;
/// Bytes waiting for the disk, counted as the buffers' capacity.
const MAX_QUEUED: usize = 64 << 20;
/// Audio waiting for video to reach its time: about 5 s of AAC, where video runs well under a
/// second behind.
const MAX_HELD: usize = 256;
/// Video waiting for audio to reach its time: about a second at 60 FPS, where audio lags by tens
/// of milliseconds. A full queue lets the oldest go on, so a dead audio source can't stall video.
const MAX_WAITING: usize = 64;
/// Free buffers kept for reuse, in bytes; beyond this they are freed.
const POOL_BYTES: usize = 16 << 20;
/// A new buffer's capacity at least: most packets fit without growing it later.
const MIN_BUFFER: usize = 64 << 10;

pub const DISK_TOO_SLOW: &str = "Your drive couldn't keep up, so the recording stopped. What was recorded so far is saved. Record to a faster drive (an SSD) on Backsight's General page, or lower the bitrate on Backsight's Video page.";

/// One packet on its way to the file. Timestamps are on the engine clock (QPC, microseconds),
/// minus the time paused so far.
pub struct Item {
    pub stream: StreamKind,
    pub pts_us: i64,
    pub dts_us: i64,
    pub duration_us: i64,
    pub keyframe: bool,
    /// The first packet of the next file (a split).
    pub new_part: bool,
    data: Data,
    /// Bytes counted against `MAX_QUEUED`.
    counted: usize,
}

enum Data {
    /// Copied from an encoder into a pool buffer.
    Pooled(Vec<u8>),
    /// The replay buffer's own bytes (the backlog a recording starts with).
    Shared(Bytes),
    /// Not a packet: a marker at `pts_us` in the current part.
    Marker,
}

impl Item {
    pub fn data(&self) -> &[u8] {
        match &self.data {
            Data::Pooled(v) => v,
            Data::Shared(b) => b,
            Data::Marker => &[],
        }
    }

    /// A marker (a chapter start at `pts_us`) rather than a packet.
    pub fn is_marker(&self) -> bool {
        matches!(self.data, Data::Marker)
    }

    fn encoded(&self) -> Encoded<'_> {
        Encoded {
            data: self.data(),
            pts_us: self.pts_us,
            dts_us: self.dts_us,
            duration_us: self.duration_us,
            keyframe: self.keyframe,
        }
    }
}

/// Free buffers, newest on top so the same few stay warm.
struct Pool {
    bufs: Vec<Vec<u8>>,
    bytes: usize,
}

fn give(pool: &Mutex<Pool>, buf: Vec<u8>) {
    let mut p = lock(pool);
    if p.bytes + buf.capacity() <= POOL_BYTES && p.bufs.len() < p.bufs.capacity() {
        p.bytes += buf.capacity();
        p.bufs.push(buf);
    }
}

/// The encoder side of a recording.
pub struct Feed {
    items: SyncSender<Item>,
    pool: Arc<Mutex<Pool>>,
    /// Buffers allocated so far (the pool was empty).
    made: usize,
    queued: Arc<AtomicUsize>,
    /// Packet bytes the writer has written to the file so far.
    written: Arc<AtomicU64>,
    /// Set by the writer when the file can't be written.
    failed: Arc<OnceLock<String>>,
    /// Audio waiting for video to pass its time, oldest first.
    held: VecDeque<Item>,
    /// Video waiting for audio to reach its time, in decode order.
    waiting: VecDeque<Item>,
    /// End of the newest packet from each audio track. Video waits for the slowest track.
    audio_seen: Vec<Option<i64>>,
    cuts: Cuts,
}

/// The writer side: what [`drain`] needs.
pub struct Sink {
    pub items: Receiver<Item>,
    pool: Arc<Mutex<Pool>>,
    queued: Arc<AtomicUsize>,
    written: Arc<AtomicU64>,
    failed: Arc<OnceLock<String>>,
}

/// A recording's channel, with room for `backlog` packets on top of the live limit.
pub fn channel(backlog: usize) -> (Feed, Sink) {
    let (items_tx, items) = mpsc::sync_channel(MAX_PACKETS + backlog);
    let pool = Arc::new(Mutex::new(Pool { bufs: Vec::with_capacity(MAX_PACKETS + MAX_HELD + MAX_WAITING), bytes: 0 }));
    let queued = Arc::new(AtomicUsize::new(0));
    let written = Arc::new(AtomicU64::new(0));
    let failed = Arc::new(OnceLock::new());
    let feed = Feed {
        items: items_tx,
        pool: pool.clone(),
        made: 0,
        queued: queued.clone(),
        written: written.clone(),
        failed: failed.clone(),
        held: VecDeque::with_capacity(MAX_HELD),
        waiting: VecDeque::with_capacity(MAX_WAITING),
        audio_seen: vec![None],
        cuts: Cuts::new(),
    };
    (feed, Sink { items, pool, queued, written, failed })
}

/// Where the cuts fall; see the module docs.
struct Cuts {
    /// Pause and resume times (engine clock, µs) video hasn't reached yet, oldest first. They
    /// alternate: while `open`, the front is a pause; otherwise a resume. It starts with a resume
    /// at the beginning of time: the first keyframe opens the file.
    presses: VecDeque<i64>,
    /// The last press was a pause.
    paused: bool,
    /// Video is being written.
    open: bool,
    /// Decode time of the first video packet left out at the last pause.
    ended_at: Option<i64>,
    /// Audio from this time on belongs to the current stretch (a resume or a part).
    audio_from: i64,
    /// End of the last audio written, and the length of an audio packet.
    audio_end: Option<i64>,
    audio_dur: i64,
    /// Time removed so far by pauses.
    removed_us: i64,
    /// Newest video decode and display times seen, and the end of the newest frame written.
    video_dts: i64,
    seen_pts: i64,
    written_end: i64,
    /// Video per part before a split; 0 never splits.
    split_us: i64,
    /// Display time (minus `removed_us`) of the current part's first keyframe.
    part_start: Option<i64>,
    /// Marker times video hasn't been decoded past yet, oldest first.
    marks: VecDeque<i64>,
}

impl Cuts {
    fn new() -> Self {
        let mut presses = VecDeque::with_capacity(16);
        presses.push_back(i64::MIN);
        Cuts {
            presses,
            paused: false,
            open: false,
            ended_at: None,
            audio_from: i64::MIN,
            audio_end: None,
            audio_dur: 0,
            removed_us: 0,
            video_dts: i64::MIN,
            seen_pts: i64::MIN,
            written_end: i64::MIN,
            split_us: 0,
            part_start: None,
            marks: VecDeque::with_capacity(16),
        }
    }

    /// Where a marker at `at` goes, before `removed_us` is taken off: there while video is being
    /// written, or where the pause cut the file while paused.
    fn marker_time(&self, at: i64) -> i64 {
        match self.ended_at {
            Some(ended) if !self.open => ended,
            _ => at,
        }
    }

    /// Whether a video packet (in decode order) is written, and if so whether it starts a part.
    /// `held` is the audio waiting, oldest first.
    fn video(&mut self, pts: i64, dts: i64, duration: i64, key: bool, held: &VecDeque<Item>) -> Option<bool> {
        // Shown after every frame before it (an I or P frame): ending just before it leaves no
        // kept frame waiting for one that is cut.
        let anchor = pts > self.seen_pts;
        // shortcut: a video stall over MAX_GAP_US that isn't a sleep also cuts the audio of that
        // span; check that audio jumped too if stalls ever happen while the PC is awake.
        let jumped = anchor && self.seen_pts != i64::MIN && pts - self.seen_pts > MAX_GAP_US;
        self.seen_pts = self.seen_pts.max(pts);
        let prev = std::mem::replace(&mut self.video_dts, dts);
        if self.open && anchor && self.presses.front().is_some_and(|&p| dts >= p) {
            self.presses.pop_front();
            self.open = false;
            // A pause due on the first frame after a sleep still ends before the gap.
            self.ended_at = Some(if jumped { dts.min(prev + duration.max(1)) } else { dts });
        } else if self.open && jumped {
            // The PC slept: cut the gap out like a pause, ending where the next frame was due, so
            // the file doesn't show one frame for the whole time. With B-frames this frame's
            // decode time is still from before the gap, and a resume there could remove nothing:
            // resume at a keyframe decoded after it instead (the one after this, at worst).
            // shortcut: no keyframe is asked for here, so up to one keyframe interval after wake
            // is lost; ask the encoder for one if that shows in real clips.
            self.open = false;
            self.ended_at = Some(dts.min(prev + duration.max(1)));
            self.presses.push_front(if dts - prev > MAX_GAP_US { dts } else { pts + RESUME_SLACK_US + 1 });
        }
        if !self.open {
            // The keyframe asked for at a resume can belong to a frame captured just before the
            // press (the video thread runs up to ~50 ms behind), so accept it.
            if !key || !self.presses.front().is_some_and(|&r| pts >= r.saturating_sub(RESUME_SLACK_US)) {
                return None;
            }
            self.presses.pop_front();
            self.open = true;
            self.audio_from = dts;
            if let Some(end) = self.ended_at {
                // At most this much, or a frame shown before the pause would be shown after K.
                let most = (dts - end).min(pts.saturating_sub(self.written_end)).max(0);
                let removed = match self.audio_end {
                    None => most,
                    Some(audio_end) => {
                        // The newest audio packet that can follow the last one written seamlessly:
                        // the one playing at `limit`. Video waited for it to arrive.
                        let limit = audio_end + most;
                        let from = match held.iter().rev().find(|a| a.pts_us <= limit) {
                            Some(a) if a.pts_us + a.duration_us > limit => a.pts_us,
                            // shortcut: that packet was already left out (frames shown before the
                            // pause cut `most` short) or audio stopped arriving; assume packets
                            // follow each other evenly (µs rounding drifts ~1 µs per packet
                            // skipped). Keep left-out audio's times if long pauses show a seam.
                            _ => audio_end + most / self.audio_dur.max(1) * self.audio_dur.max(1),
                        };
                        self.audio_from = from;
                        from - audio_end
                    }
                };
                self.removed_us += removed;
            }
        }
        let shifted = pts - self.removed_us;
        let split = key && self.split_us > 0 && self.part_start.is_some_and(|s| shifted - s >= self.split_us);
        if split {
            // A new file: its audio starts at its keyframe's decode time, like a saved clip.
            self.audio_from = self.audio_from.max(dts);
        }
        if split || self.part_start.is_none() {
            self.part_start = Some(shifted);
        }
        self.written_end = self.written_end.max(pts + duration.max(1));
        Some(split)
    }

    /// Audio before this time can be decided once video has reached decode time `dts`. While
    /// paused, one more audio packet waits: a resume may start that far back.
    fn audio_horizon(&self, dts: i64) -> i64 {
        if self.open { dts } else { dts.saturating_sub(self.audio_dur) }
    }

    /// Whether audio at `pts` is written, once video has passed it.
    fn keeps_audio(&self, pts: i64) -> bool {
        pts >= self.audio_from && (self.open || self.ended_at.is_some_and(|d| pts < d))
    }

    /// Whether held audio is written when the recording stops: video hasn't reached it, so a
    /// pause video hasn't reached yet still applies.
    fn keeps_audio_at_stop(&self, pts: i64) -> bool {
        self.keeps_audio(pts) && (!self.open || self.presses.front().is_none_or(|&p| pts < p))
    }
}

/// Where a packet's bytes come from.
enum Src<'a> {
    Copy(&'a [u8]),
    Share(&'a Bytes),
}

impl Feed {
    /// Queues a copy of one encoder packet. An error is the plain-language reason the recording
    /// must stop.
    pub fn send(
        &mut self,
        stream: StreamKind,
        pts_us: i64,
        dts_us: i64,
        duration_us: i64,
        keyframe: bool,
        data: &[u8],
    ) -> Result<(), String> {
        self.accept(stream, pts_us, dts_us, duration_us, keyframe, Src::Copy(data))
    }

    /// Queues a packet from the replay buffer, sharing its bytes. Not limited like live packets:
    /// [`channel`] makes room for the backlog, which can be a long GOP at a high bitrate.
    pub fn send_packet(&mut self, p: &Packet) -> Result<(), String> {
        self.accept(p.stream, p.pts_us, p.dts_us, p.duration_us, p.keyframe, Src::Share(&p.data))
    }

    /// Pauses at `at_us` on the engine clock; packets before it (still inside the encoders) are
    /// written. Does nothing if already paused.
    pub fn pause(&mut self, at_us: i64) {
        if !self.cuts.paused {
            self.cuts.paused = true;
            self.cuts.presses.push_back(at_us);
        }
    }

    /// Resumes at the first keyframe shown at or after `at_us`. Does nothing if not paused.
    pub fn resume(&mut self, at_us: i64) {
        if self.cuts.paused {
            self.cuts.paused = false;
            self.cuts.presses.push_back(at_us);
        }
    }

    /// Adds a marker at `at_us` on the engine clock, placed once video reaches it (module docs).
    pub fn mark(&mut self, at_us: i64) {
        self.cuts.marks.push_back(at_us);
    }

    /// Starts a new file at the first keyframe after each `every` of recorded video; zero never.
    pub fn split_every(&mut self, every: Duration) {
        self.cuts.split_us = every.as_micros().try_into().unwrap_or(i64::MAX);
    }

    /// Sets the number of synchronized tracks before any backlog or live packets arrive.
    pub fn expect_audio_tracks(&mut self, count: usize) {
        self.audio_seen.resize(count, None);
    }

    /// Bytes waiting for the disk (the writer's backlog), not counting either track waiting for
    /// the other.
    pub fn queued_bytes(&self) -> usize {
        let held: usize = self.held.iter().chain(&self.waiting).map(|a| a.counted).sum();
        // Acquire pairs with the writer's Release: bytes counted as written are seen once their item has left.
        self.queued.load(Acquire).saturating_sub(held)
    }

    /// Packet bytes the writer has written to the file so far (all parts).
    pub fn written_bytes(&self) -> u64 {
        self.written.load(Relaxed)
    }

    /// The most bytes [`queued_bytes`](Self::queued_bytes) can reach before the recording stops.
    pub const fn queue_limit() -> usize {
        MAX_QUEUED
    }

    fn accept(
        &mut self,
        stream: StreamKind,
        pts: i64,
        dts: i64,
        duration: i64,
        key: bool,
        src: Src,
    ) -> Result<(), String> {
        if let Some(e) = self.failed.get() {
            return Err(e.clone());
        }
        let item = self.item(stream, pts, dts, duration, key, src)?;
        if stream == StreamKind::Video {
            // Audio stopped or lags by more than the queue: the oldest goes on without it.
            let oldest = if self.waiting.len() == MAX_WAITING { self.waiting.pop_front() } else { None };
            self.waiting.push_back(item);
            if let Some(v) = oldest {
                self.release_video(v)?;
            }
        } else if let StreamKind::Audio(index) = stream {
            self.cuts.audio_dur = duration;
            if let Some(seen) = self.audio_seen.get_mut(usize::from(index)) {
                *seen = Some(pts + duration);
            }
            self.hold_audio(item)?;
        }
        // One audio packet past the decode time: a resume looks that far ahead.
        let audio_started = self.audio_seen.iter().any(Option::is_some);
        while let Some(v) = self.waiting.front()
            && (!audio_started
                || self.audio_seen.iter().all(|seen| seen.is_some_and(|t| t >= v.dts_us + self.cuts.audio_dur)))
        {
            let v = self.waiting.pop_front().expect("just checked");
            self.release_video(v)?;
        }
        Ok(())
    }

    /// Writes or drops a video packet now that audio has reached its time.
    fn release_video(&mut self, mut item: Item) -> Result<(), String> {
        // Audio before this packet's decode time falls under the cuts as they are now.
        let before = self.cuts.audio_horizon(item.dts_us);
        while self.held.front().is_some_and(|a| a.pts_us < before) {
            let a = self.held.pop_front().expect("just checked");
            if let Err(e) = self.decide_audio(a) {
                self.discard(item);
                return Err(e);
            }
        }
        let written = self.cuts.video(item.pts_us, item.dts_us, item.duration_us, item.keyframe, &self.held);
        // Markers video has been decoded past belong before this packet, which may start a part.
        while let Some(at) = self.cuts.marks.front().copied().filter(|&m| m <= item.dts_us) {
            self.cuts.marks.pop_front();
            if let Err(e) = self.queue_marker(at) {
                self.discard(item);
                return Err(e);
            }
        }
        match written {
            Some(new_part) => {
                item.new_part = new_part;
                self.queue(item)
            }
            None => {
                self.discard(item);
                Ok(())
            }
        }
    }

    fn hold_audio(&mut self, item: Item) -> Result<(), String> {
        if item.pts_us < self.cuts.audio_horizon(self.cuts.video_dts) && self.held.is_empty() {
            return self.decide_audio(item); // late audio: video already passed its time
        }
        if self.held.len() == MAX_HELD {
            // shortcut: video stalled for seconds while audio kept coming; the oldest audio is
            // decided as if video had passed it. Fine for the rare stall, a larger hold if not.
            let a = self.held.pop_front().expect("full");
            self.decide_audio(a)?;
        }
        // A short codec frame can emit several packets from one track before the next track.
        let i = self.held.partition_point(|a| a.pts_us <= item.pts_us);
        self.held.insert(i, item);
        Ok(())
    }

    fn decide_audio(&mut self, item: Item) -> Result<(), String> {
        if self.cuts.keeps_audio(item.pts_us) {
            self.cuts.audio_end = Some(item.pts_us + item.duration_us);
            self.queue(item)
        } else {
            self.discard(item);
            Ok(())
        }
    }

    fn item(
        &mut self,
        stream: StreamKind,
        pts: i64,
        dts: i64,
        duration: i64,
        key: bool,
        src: Src,
    ) -> Result<Item, String> {
        let (data, counted) = match src {
            Src::Share(b) => (Data::Shared(b.clone()), 0),
            Src::Copy(bytes) => {
                let mut buf = {
                    let mut p = lock(&self.pool);
                    let buf = p.bufs.pop();
                    if let Some(b) = &buf {
                        p.bytes -= b.capacity();
                    }
                    buf
                }
                .unwrap_or_else(|| {
                    self.made += 1;
                    Vec::with_capacity(bytes.len().max(MIN_BUFFER))
                });
                buf.clear();
                buf.extend_from_slice(bytes); // reuses the buffer's capacity once warm
                let counted = buf.capacity();
                if self.queued.fetch_add(counted, Relaxed) + counted > MAX_QUEUED {
                    self.queued.fetch_sub(counted, Relaxed);
                    give(&self.pool, buf);
                    return Err(DISK_TOO_SLOW.into());
                }
                (Data::Pooled(buf), counted)
            }
        };
        Ok(Item {
            stream,
            pts_us: pts,
            dts_us: dts,
            duration_us: duration,
            keyframe: key,
            new_part: false,
            data,
            counted,
        })
    }

    fn queue(&mut self, mut item: Item) -> Result<(), String> {
        item.pts_us -= self.cuts.removed_us;
        item.dts_us -= self.cuts.removed_us;
        match self.items.try_send(item) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(item)) => {
                self.discard(item);
                Err(DISK_TOO_SLOW.into())
            }
            // The writer is gone; it set `failed` first unless it panicked.
            Err(TrySendError::Disconnected(item)) => {
                self.discard(item);
                Err(self.failed.get().cloned().unwrap_or_else(|| DISK_TOO_SLOW.into()))
            }
        }
    }

    fn queue_marker(&mut self, at: i64) -> Result<(), String> {
        let t = self.cuts.marker_time(at);
        let marker = Item {
            stream: StreamKind::Video,
            pts_us: t,
            dts_us: t,
            duration_us: 0,
            keyframe: false,
            new_part: false,
            data: Data::Marker,
            counted: 0,
        };
        self.queue(marker)
    }

    fn discard(&self, item: Item) {
        self.queued.fetch_sub(item.counted, Relaxed);
        if let Data::Pooled(buf) = item.data {
            give(&self.pool, buf);
        }
    }
}

impl Drop for Feed {
    /// The recording stops: audio still waiting for video goes in if it belongs before the end,
    /// so stopping while paused ends the file at the pause.
    fn drop(&mut self) {
        while let Some(v) = self.waiting.pop_front() {
            let _ = self.release_video(v);
        }
        // Video never reached these (the encoder still held it): the file ends before them, and
        // the muxer moves them to its end.
        while let Some(at) = self.cuts.marks.pop_front() {
            let _ = self.queue_marker(at);
        }
        while let Some(a) = self.held.pop_front() {
            if self.cuts.keeps_audio_at_stop(a.pts_us) {
                let _ = self.queue(a);
            } else {
                self.discard(a);
            }
        }
    }
}

/// Average data rate over the last few seconds of a growing byte count.
#[derive(Default)]
pub struct RateMeter {
    /// (seconds since the recording started, bytes written), oldest first.
    samples: VecDeque<(f64, u64)>,
}

impl RateMeter {
    const WINDOW_S: f64 = 5.0;

    /// Adds a reading and returns kbps over the window, or `None` until it spans a second.
    pub fn add(&mut self, at_s: f64, bytes: u64) -> Option<u32> {
        // Nobody asked for a while (page closed): readings that old aren't "the last few seconds".
        if self.samples.back().is_some_and(|&(t, _)| at_s - t > Self::WINDOW_S) {
            self.samples.clear();
        }
        self.samples.push_back((at_s, bytes));
        // Keep one sample at or before the window's start so the span is the full window.
        while self.samples.len() > 2 && self.samples[1].0 <= at_s - Self::WINDOW_S {
            self.samples.pop_front();
        }
        let (t0, b0) = *self.samples.front()?;
        let span = at_s - t0;
        (span >= 1.0).then(|| (bytes.saturating_sub(b0) as f64 * 8.0 / 1000.0 / span) as u32)
    }
}

/// The writer loop: writes every item until the feed is dropped, returning each buffer for
/// reuse. After the first write error it only drains (so the feed sees `failed` and stops) and
/// returns that error, with `user` as the message the feed reports.
pub fn drain(sink: Sink, user: &str, mut write: impl FnMut(&Item) -> Result<(), String>) -> Result<(), String> {
    let mut result = Ok(());
    for item in &sink.items {
        if result.is_ok() {
            match write(&item) {
                Ok(()) => {
                    if !item.is_marker() {
                        sink.written.fetch_add(item.data().len() as u64, Relaxed);
                    }
                }
                Err(e) => {
                    let _ = sink.failed.set(user.to_owned());
                    result = Err(e);
                }
            }
        }
        let counted = item.counted;
        if let Data::Pooled(buf) = item.data {
            give(&sink.pool, buf); // before the count drops, so an empty queue means all are back
        }
        sink.queued.fetch_sub(counted, Release);
    }
    result
}

/// What [`write_parts`] wrote: the finished earlier parts, the numbers (from 1) of earlier parts
/// that couldn't be finished, the last part or the raw reason it failed, then whether any part
/// lost its markers ([`MARKERS_LOST`]).
pub type Parts = (Vec<PathBuf>, Vec<usize>, Result<PathBuf, String>, bool);

/// Told after a recording whose markers couldn't be added (the video itself is fine).
pub const MARKERS_LOST: &str = "Your markers couldn't be added to the saved video, which plays fine without them.";

/// The recording thread: [`drain`] into `first`, finishing each part and opening the next
/// (from `next`) at a split. Unfinished parts keep their `.recording.part` file, for `recovery`.
pub fn write_parts(sink: Sink, user: &str, mut first: Muxer, mut next: impl FnMut() -> Result<Muxer, String>) -> Parts {
    first.keep_part_on_failure();
    let mut mux = Some(first);
    let mut finishing = Vec::new();
    let written = drain(sink, user, |i| {
        if i.new_part
            && let Some(done) = mux.take()
        {
            // Finishing can take as long as copying the file (MP4's faststart, or a fragmented
            // part with markers), so it runs beside the next part: the queue never waits for it.
            let finish = std::thread::Builder::new()
                .name("backsight-finish-part".into())
                .spawn(move || done.finish_noting_markers());
            finishing.push(finish.map_err(|e| e.to_string())?);
            let mut m = next()?;
            m.keep_part_on_failure();
            mux = Some(m);
        }
        let m = mux.as_mut().ok_or("no file to write to")?;
        if i.is_marker() {
            m.add_marker(i.pts_us);
            return Ok(());
        }
        m.write(track(i.stream), &i.encoded())
    });
    let last =
        written.and_then(|()| mux.take().ok_or_else(|| "no file to finish".to_string())?.finish_noting_markers());
    let (parts, unfinished, earlier_lost) = finished_parts(finishing.into_iter().map(|f| f.join()));
    let markers_lost = earlier_lost || last.as_ref().is_ok_and(|(_, lost)| *lost);
    (parts, unfinished, last.map(|(path, _)| path), markers_lost)
}

/// The earlier parts' finishing threads' results: the finished files, the numbers (from 1) of the
/// parts that couldn't be finished, and whether any lost its markers.
fn finished_parts(
    results: impl Iterator<Item = std::thread::Result<Result<(PathBuf, bool), String>>>,
) -> (Vec<PathBuf>, Vec<usize>, bool) {
    let (mut parts, mut unfinished, mut markers_lost) = (Vec::new(), Vec::new(), false);
    for (i, result) in results.enumerate() {
        match result {
            Ok(Ok((path, lost))) => {
                markers_lost |= lost;
                parts.push(path);
            }
            Ok(Err(e)) => {
                tracing::error!("recording part {} couldn't be finished: {e}", i + 1);
                unfinished.push(i + 1);
            }
            Err(_) => {
                tracing::error!("finishing recording part {} panicked", i + 1);
                unfinished.push(i + 1);
            }
        }
    }
    (parts, unfinished, markers_lost)
}

/// Tells the user which earlier parts of a split recording couldn't be finished (numbers from 1).
/// Their `.recording.part` files are recovered at the next start.
pub fn unfinished_note(parts: &[usize]) -> Option<String> {
    let names = match parts {
        [] => return None,
        [one] => format!("Part {one} of your recording"),
        [rest @ .., last] => {
            format!(
                "Parts {} and {last} of your recording",
                rest.iter().map(usize::to_string).collect::<Vec<_>>().join(", ")
            )
        }
    };
    Some(format!(
        "{names} couldn't be finished. Backsight saves what it can of {} the next time it starts.",
        if parts.len() == 1 { "it" } else { "them" }
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn audio_starting_exactly_at_a_pause_is_left_out() {
        let mut cuts = super::Cuts::new();
        cuts.audio_from = 0;
        cuts.ended_at = Some(1_000);
        assert!(cuts.keeps_audio(999));
        assert!(!cuts.keeps_audio(1_000));
    }

    #[test]
    fn unfinished_parts_are_named() {
        assert_eq!(super::unfinished_note(&[]), None);
        let one = super::unfinished_note(&[2]).unwrap();
        assert!(one.starts_with("Part 2 of your recording couldn't be finished. Backsight saves"), "{one}");
        assert!(one.ends_with("of it the next time it starts."), "{one}");
        let many = super::unfinished_note(&[1, 3, 4]).unwrap();
        assert!(many.starts_with("Parts 1, 3 and 4 of your recording couldn't"), "{many}");
    }

    #[test]
    fn rate_meter_averages_over_its_window() {
        let mut m = super::RateMeter::default();
        assert_eq!(m.add(0.0, 0), None);
        assert_eq!(m.add(0.5, 100_000), None); // under a second of history
        assert_eq!(m.add(1.0, 125_000), Some(1000)); // 125 kB in 1 s = 1000 kbps
        // 10 s of 250 kB/s: only the last 5 s count, and an earlier burst is forgotten.
        for t in 2..=10 {
            m.add(t as f64, 125_000 + (t - 1) * 250_000);
        }
        assert_eq!(m.add(11.0, 125_000 + 10 * 250_000), Some(2000));
        // A stall shows as a falling rate.
        assert_eq!(m.add(16.0, 125_000 + 10 * 250_000), Some(0));
        // Reopened after a long gap: no average over the gap, only new readings.
        assert_eq!(m.add(100.0, 9_000_000), None);
        assert_eq!(m.add(101.0, 9_125_000), Some(1000));
    }

    use super::*;

    const F: i64 = 16_667; // 60 fps
    const A: i64 = 21_333; // one AAC frame

    fn video(feed: &mut Feed, key: bool, size: usize) -> Result<(), String> {
        feed.send(StreamKind::Video, 0, 0, 0, key, &vec![1; size])
    }

    /// What reached the writer: (stream, pts, dts, keyframe, new part, original pts).
    type Written = (StreamKind, i64, i64, bool, bool, i64);
    type Out = Vec<Written>;

    fn collect(sink: Sink) -> std::thread::JoinHandle<Out> {
        std::thread::spawn(move || drain_all(sink).0)
    }

    /// The packets, and the markers as (part number, time in that part's timeline).
    fn drain_all(sink: Sink) -> (Out, Vec<(usize, i64)>) {
        let (mut out, mut marks) = (Vec::new(), Vec::new());
        let mut part = 0;
        drain(sink, "x", |i| {
            if i.is_marker() {
                marks.push((part, i.pts_us));
                return Ok(());
            }
            part += usize::from(i.new_part);
            let original = i64::from_le_bytes(i.data()[..8].try_into().unwrap());
            out.push((i.stream, i.pts_us, i.dts_us, i.keyframe, i.new_part, original));
            Ok(())
        })
        .unwrap();
        (out, marks)
    }

    /// A synthetic session: 60 fps video with two B-frames (decode order I0 P3 B1 B2 P6 B4 B5 …,
    /// closed GOPs of `gop` frames) that comes out of the encoder `lag` frames late, and AAC
    /// packets that come out one frame after their time. `presses` alternate pause and resume
    /// (each pressed twice: the second does nothing), at their time on the engine clock. Each
    /// packet's data is its pts.
    fn run(frames: i64, gop: i64, lag: i64, feed: &mut Feed, presses: &[i64]) {
        run_with(frames, gop, lag, true, A, feed, presses);
    }

    /// [`run`], with or without B-frames (then decode time is display time), and audio for time
    /// t sent at t + `audio_at`.
    fn run_with(frames: i64, gop: i64, lag: i64, b_frames: bool, audio_at: i64, feed: &mut Feed, presses: &[i64]) {
        run_sleeping(frames, gop, lag, b_frames, audio_at, feed, presses, (frames, 0));
    }

    /// [`run_with`], with the PC asleep for `sleep.1` µs before frame `sleep.0` is captured: every
    /// time from then on (capture, encoder output, decode times as NVENC derives them from earlier
    /// inputs) is that much later.
    #[allow(clippy::too_many_arguments)] // test helper; each argument is a different knob
    fn run_sleeping(
        frames: i64,
        gop: i64,
        lag: i64,
        b_frames: bool,
        audio_at: i64,
        feed: &mut Feed,
        presses: &[i64],
        sleep: (i64, i64),
    ) {
        let clock = |t: i64| if t >= sleep.0 * F { t + sleep.1 } else { t };
        let mut order = Vec::new(); // display indexes in decode order
        let mut i = 0;
        while i < frames {
            if !b_frames
                || i % gop == 0
                || i + 3 > frames
                || (i + 3) % gop == 0
                || (i + 2) % gop == 0
                || (i + 1) % gop == 0
            {
                order.push(i); // keyframes and the frames just before them: no B-frames
                i += 1;
            } else {
                order.extend([i + 2, i, i + 1]);
                i += 3;
            }
        }
        // Packets with their send time: video decode slot k leaves at (k + lag) frames.
        let delay = if b_frames { 1 } else { 0 };
        let mut packets: Vec<(i64, StreamKind, i64, i64, bool)> = order
            .iter()
            .enumerate()
            .map(|(k, &n)| {
                let (sent, dts) = (clock((k as i64 + lag) * F), clock((k as i64 - delay) * F));
                (sent, StreamKind::Video, clock(n * F), dts, n % gop == 0)
            })
            .collect();
        let mut t = -10 * A;
        while t < frames * F {
            packets.push((clock(t) + audio_at, StreamKind::Audio(0), clock(t), clock(t), true));
            t += A;
        }
        packets.sort_by_key(|p| p.0);
        let mut next = 0;
        for (at, stream, pts, dts, key) in packets {
            while next < presses.len() && presses[next] <= at {
                for _ in 0..2 {
                    if next % 2 == 0 { feed.pause(presses[next]) } else { feed.resume(presses[next]) }
                }
                next += 1;
            }
            let dur = if stream == StreamKind::Video { F } else { A };
            feed.send(stream, pts, dts, dur, key, &pts.to_le_bytes()).unwrap();
        }
    }

    fn of(out: &[Written], s: StreamKind) -> Vec<(i64, i64, bool, bool, i64)> {
        out.iter().filter(|p| p.0 == s).map(|p| (p.1, p.2, p.3, p.4, p.5)).collect()
    }

    /// Every part starts on a keyframe with audio from its decode time, video decode times run
    /// one frame apart, nothing is written twice, and display order never goes back across a cut.
    fn check_parts(out: &Out) -> Vec<Out> {
        let mut parts: Vec<Out> = Vec::new();
        for p in out {
            if p.4 || parts.is_empty() {
                parts.push(Vec::new());
            }
            parts.last_mut().unwrap().push(*p);
        }
        let mut seen = std::collections::HashSet::new();
        for p in out {
            assert!(seen.insert((p.0, p.5)), "written twice: {p:?}");
        }
        for part in &parts {
            let first = part[0];
            assert!(first.0 == StreamKind::Video && first.3, "a part starts on a keyframe: {first:?}");
            let audio = of(part, StreamKind::Audio(0));
            assert!(
                audio[0].0 >= first.2 && audio[0].0 < first.2 + A,
                "sound at the start: {:?} vs {first:?}",
                audio[0]
            );
            for w in audio.windows(2) {
                assert_eq!(w[1].0 - w[0].0, A, "audio carries on seamlessly, even across a pause: {w:?}");
            }
            let video = of(part, StreamKind::Video);
            for w in video.windows(2) {
                let step = w[1].1 - w[0].1;
                assert!((F..F + A).contains(&step), "video decode times one frame apart: {w:?}");
            }
            let mut shown: Vec<i64> = video.iter().map(|v| v.0).collect();
            shown.sort();
            for w in shown.windows(2) {
                assert!(w[1] - w[0] >= F && w[1] - w[0] <= 3 * F, "video shown with a gap: {w:?}");
            }
        }
        parts
    }

    /// The time removed from each packet, which must be the same for both tracks at any moment.
    fn removed(p: &(StreamKind, i64, i64, bool, bool, i64)) -> i64 {
        p.5 - p.1
    }

    #[test]
    fn a_plain_recording_starts_on_a_keyframe_with_its_audio() {
        let (mut feed, sink) = channel(4096); // room enough that the test never races the writer
        let writer = collect(sink);
        run(300, 120, 5, &mut feed, &[]);
        drop(feed);
        let out = writer.join().unwrap();
        let parts = check_parts(&out);
        assert_eq!(parts.len(), 1);
        assert_eq!(of(&out, StreamKind::Video).len(), 300);
        assert!(out.iter().all(|p| removed(p) == 0));
        // Audio from the keyframe's decode time (one frame early) to the end, beyond the last video.
        let audio = of(&out, StreamKind::Audio(0));
        assert!(audio[0].0 >= -F && audio.last().unwrap().0 >= 299 * F);
    }

    #[test]
    fn a_pause_removes_the_same_span_from_both_tracks() {
        let (mut feed, sink) = channel(4096); // room enough that the test never races the writer
        let writer = collect(sink);
        let (pause, resume) = (100 * F + 5_000, 150 * F + 5_000);
        run(400, 60, 6, &mut feed, &[pause, resume]);
        drop(feed);
        let out = writer.join().unwrap();
        check_parts(&out);
        // Nothing captured while paused, up to the keyframe the resume waits for (frame 180), give
        // or take the frames and audio packet around each cut.
        let video = of(&out, StreamKind::Video);
        assert!(video.iter().all(|v| v.4 < pause + 3 * F || v.4 >= 180 * F), "paused video written");
        assert!(video.iter().any(|v| v.4 == 180 * F) && video.iter().any(|v| v.4 >= 99 * F && v.4 < pause));
        let audio = of(&out, StreamKind::Audio(0));
        assert!(audio.iter().all(|a| a.4 < pause + 3 * F || a.4 >= 179 * F - A), "paused audio written");
        // One shift before, one after, the same for audio and video.
        let shift = removed(out.iter().find(|p| p.5 == 180 * F && p.0 == StreamKind::Video).unwrap());
        assert!(shift > 0);
        for p in &out {
            let r = removed(p);
            assert!(r == 0 || r == shift, "{p:?} moved by {r}");
            assert_eq!(r == shift, p.5 >= 150 * F, "{p:?}");
        }
    }

    const HOUR: i64 = 3_600_000_000;

    #[test]
    fn a_sleep_is_cut_out_of_both_tracks_like_a_pause() {
        for b_frames in [false, true] {
            let (mut feed, sink) = channel(4096);
            let writer = collect(sink);
            run_sleeping(400, 60, 6, b_frames, A, &mut feed, &[], (100, HOUR));
            drop(feed);
            let out = writer.join().unwrap();
            check_parts(&out); // no hour-long frame, audio seamless
            let video = of(&out, StreamKind::Video);
            assert!(video.iter().any(|v| v.4 >= 96 * F && v.4 < 100 * F), "kept up to the sleep");
            assert!(video.iter().any(|v| v.4 == HOUR + 180 * F), "resumes within a keyframe or two");
            // The same span gone from both tracks, the hour and at most a keyframe interval more.
            let shift = removed(out.iter().find(|p| p.5 >= HOUR).unwrap());
            assert!((HOUR..HOUR + 130 * F).contains(&shift), "removed {shift}");
            for p in &out {
                assert_eq!(removed(p), if p.5 >= HOUR { shift } else { 0 }, "{p:?} (b-frames: {b_frames})");
            }
        }
    }

    #[test]
    fn a_resume_pressed_just_before_a_sleep_is_not_doubled() {
        let (mut feed, sink) = channel(4096);
        let writer = collect(sink);
        // Paused at 50, resumed at 99: the keyframe it waits for (120) comes after the sleep.
        run_sleeping(400, 60, 6, true, A, &mut feed, &[50 * F + 7, 99 * F + 7], (100, HOUR));
        drop(feed);
        let out = writer.join().unwrap();
        check_parts(&out);
        let video = of(&out, StreamKind::Video);
        assert!(video.iter().all(|v| v.4 < 53 * F || v.4 >= HOUR + 120 * F), "paused span written");
        assert!(video.iter().any(|v| v.4 == HOUR + 120 * F), "resumes on the first keyframe after waking");
        let mut shifts: Vec<i64> = out.iter().map(removed).collect();
        shifts.sort();
        shifts.dedup();
        assert_eq!(shifts.len(), 2, "one cut for the pause and the sleep: {shifts:?}");
    }

    #[test]
    fn a_short_pause_inside_the_encoder_delay_still_cuts_by_time() {
        let (mut feed, sink) = channel(4096); // room enough that the test never races the writer
        let writer = collect(sink);
        // Paused and resumed before video for either time has left the encoder (lag 30 frames).
        let (pause, resume) = (100 * F + 1, 110 * F + 1);
        run(300, 60, 30, &mut feed, &[pause, resume]);
        drop(feed);
        let out = writer.join().unwrap();
        check_parts(&out);
        let video = of(&out, StreamKind::Video);
        assert!(video.iter().all(|v| v.4 <= 104 * F || v.4 >= 120 * F), "resumes on the next keyframe");
        assert!(video.iter().any(|v| v.4 == 120 * F));
    }

    #[test]
    fn repeated_pauses_add_up_and_stop_while_paused_ends_at_the_pause() {
        let (mut feed, sink) = channel(4096); // room enough that the test never races the writer
        let writer = collect(sink);
        let presses = [40 * F + 7, 70 * F + 7, 130 * F + 7, 150 * F + 7, 250 * F + 7];
        run(400, 60, 4, &mut feed, &presses);
        drop(feed); // still paused since frame 250
        let out = writer.join().unwrap();
        check_parts(&out);
        let mut shifts: Vec<i64> = out.iter().map(removed).collect();
        shifts.dedup();
        let mut sorted = shifts.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 3, "three stretches: {sorted:?}");
        let last = out.iter().map(|p| p.5).max().unwrap();
        assert!(last < 250 * F + 3 * F, "nothing after the last pause, got {last}");
    }

    #[test]
    fn splits_start_each_part_on_a_keyframe_without_gaps_or_duplicates() {
        let (mut feed, sink) = channel(4096);
        feed.split_every(Duration::from_micros(150 * F as u64));
        let writer = collect(sink);
        run(600, 60, 8, &mut feed, &[200 * F + 3, 260 * F + 3]);
        drop(feed);
        let out = writer.join().unwrap();
        let parts = check_parts(&out);
        // Frames 0–179 (a split is due at 150, the next keyframe is 180). Then 180–201 until the
        // pause, and from the resume at 300 the keyframes at 360 and 420 come before that part
        // holds 150 frames; 480 splits.
        let starts: Vec<i64> = parts.iter().map(|p| p[0].5).collect();
        assert_eq!(starts, [0, 180 * F, 480 * F]);
        // Every frame up to the pause and after the resume is somewhere.
        let video = of(&out, StreamKind::Video).len() as i64;
        assert_eq!(video, 202 + 300, "{video} frames");
    }

    #[test]
    fn markers_follow_pauses_and_splits() {
        let (mut feed, sink) = channel(4096);
        feed.split_every(Duration::from_micros(150 * F as u64));
        let writer = std::thread::spawn(move || drain_all(sink));
        // Parts start at frames 0, 180 and 480 (see the split test); paused from 200 to the
        // keyframe at 300. One marker in each stretch, one while paused, one past the end.
        let marks = [100 * F + 5, 190 * F + 5, 230 * F, 400 * F, 500 * F, 10_000 * F];
        marks.iter().for_each(|&m| feed.mark(m));
        run(600, 60, 8, &mut feed, &[200 * F + 3, 260 * F + 3]);
        drop(feed);
        let (out, got) = writer.join().unwrap();
        let shift = removed(out.iter().find(|p| p.5 == 300 * F && p.0 == StreamKind::Video).unwrap());
        assert!(shift > 0);
        let (parts, times): (Vec<_>, Vec<_>) = got.iter().copied().unzip();
        assert_eq!(parts, [0, 1, 1, 1, 2, 2], "{got:?}");
        assert_eq!(times[0], 100 * F + 5);
        assert_eq!(times[1], 190 * F + 5);
        // Where the pause cut the file: just after the last frame decoded before it.
        let last_before = out.iter().filter(|p| p.0 == StreamKind::Video && p.5 < 210 * F).map(|p| p.2).max().unwrap();
        assert!((last_before..=last_before + F).contains(&times[2]), "{got:?} vs {last_before}");
        assert_eq!(times[3], 400 * F - shift, "moved by the paused time");
        assert_eq!(times[4], 500 * F - shift);
        assert_eq!(times[5], 10_000 * F - shift, "the muxer moves it to the file's end");
    }

    /// No B-frames (AMF and QSV by default), two frames of encoder lag, and audio arriving 50 ms
    /// after the video for its time.
    const LATE_AUDIO: i64 = 2 * F + 50_000;

    /// Paused at `pause`, resumed at `resume` on the keyframe shown at frame `key`, with audio
    /// behind video: both tracks still cut by time.
    fn late_audio_pause(pause: i64, resume: i64, key: i64) {
        let (mut feed, sink) = channel(4096); // room enough that the test never races the writer
        let writer = collect(sink);
        run_with(400, 60, 2, false, LATE_AUDIO, &mut feed, &[pause, resume]);
        drop(feed);
        let out = writer.join().unwrap();
        check_parts(&out);
        // The first frame left out is the first decoded (and shown) at or after the pause.
        let left_out = (pause + F - 1) / F;
        let (ended_at, key_dts) = (left_out * F, key * F);
        let video = of(&out, StreamKind::Video);
        assert!(video.iter().all(|v| v.4 < left_out * F || v.4 >= key * F), "paused video written");
        assert!(video.iter().any(|v| v.4 == (left_out - 1) * F) && video.iter().any(|v| v.4 == key * F));
        // Both tracks lose the same span from the cut on (none if shorter than an audio packet).
        let shift = removed(out.iter().find(|p| p.5 == key * F && p.0 == StreamKind::Video).unwrap());
        for p in &out {
            let after = if p.0 == StreamKind::Video { p.5 >= key * F } else { p.5 >= ended_at };
            assert_eq!(removed(p), if after { shift } else { 0 }, "{p:?}");
        }
        let audio = of(&out, StreamKind::Audio(0));
        let last_before = audio.iter().rfind(|a| a.4 < ended_at).unwrap().4;
        assert!(last_before + A >= ended_at, "sound up to the pause kept, got {last_before} for {ended_at}");
        let first_after = audio.iter().find(|a| a.4 >= ended_at).unwrap().4;
        assert!((key_dts - A..key_dts + A).contains(&first_after), "sound from the keyframe on: {first_after}");
    }

    #[test]
    fn late_audio_and_a_short_pause_cut_by_time() {
        // Resumed on the keyframe at 120: from these pauses on, it leaves the encoder before
        // audio from before the pause has arrived.
        for frame in 108..=119 {
            late_audio_pause(frame * F + 1, 121 * F, 120);
        }
    }

    #[test]
    fn late_audio_and_a_long_pause_cut_by_time() {
        late_audio_pause(100 * F + 5_000, 250 * F + 5_000, 300);
    }

    #[test]
    fn late_audio_ends_each_split_part_at_the_next_keyframe() {
        let (mut feed, sink) = channel(4096);
        feed.split_every(Duration::from_micros(150 * F as u64));
        let writer = collect(sink);
        // Four parts fit in the byte budget even if the writer hasn't run yet.
        run_with(550, 60, 2, false, LATE_AUDIO, &mut feed, &[]);
        drop(feed);
        let out = writer.join().unwrap();
        let parts = check_parts(&out);
        let starts: Vec<i64> = parts.iter().map(|p| p[0].5).collect();
        assert_eq!(starts, [0, 180 * F, 360 * F, 540 * F]);
        // No audio lost between parts: each part's sound runs up to the next one's.
        let audio = of(&out, StreamKind::Audio(0));
        for w in audio.windows(2) {
            assert_eq!(w[1].4 - w[0].4, A, "audio lost at a split: {w:?}");
        }
        assert!(out.iter().all(|p| removed(p) == 0));
    }

    #[test]
    fn split_waits_for_every_audio_track_even_when_packets_arrive_in_batches() {
        let (mut feed, sink) = channel(0);
        feed.expect_audio_tracks(2);
        feed.split_every(Duration::from_micros((2 * F) as u64));
        let writer = collect(sink);
        let (video, mix, mic) = (StreamKind::Video, StreamKind::Audio(0), StreamKind::Audio(1));
        for (stream, pts) in
            [(mix, 0), (mic, 0), (video, 0), (mix, F), (mix, 2 * F), (video, 2 * F), (mic, F), (mic, 2 * F)]
        {
            feed.send(stream, pts, pts, F, true, &pts.to_le_bytes()).unwrap();
        }
        drop(feed);
        let out = writer.join().unwrap();
        let (before, after) = out.split_at(out.iter().position(|p| p.4).unwrap());
        for track in [mix, mic] {
            assert_eq!(of(before, track).iter().map(|p| p.4).collect::<Vec<_>>(), [0, F]);
            assert_eq!(of(after, track).iter().map(|p| p.4).collect::<Vec<_>>(), [2 * F]);
        }
    }

    #[test]
    fn video_waits_for_late_audio_only_so_long() {
        let (mut feed, sink) = channel(4096);
        let writer = collect(sink);
        let send = |f: &mut Feed, s, pts: i64, key| f.send(s, pts, pts, F, key, &pts.to_le_bytes()).unwrap();
        send(&mut feed, StreamKind::Audio(0), 0, true);
        for n in 0..MAX_WAITING as i64 + 10 {
            send(&mut feed, StreamKind::Video, n * F, n == 0); // the audio source died
        }
        assert_eq!(feed.waiting.len(), MAX_WAITING);
        drop(feed);
        let video = of(&writer.join().unwrap(), StreamKind::Video).len();
        assert_eq!(video, MAX_WAITING + 10, "every frame written");
    }

    #[test]
    fn audio_waits_for_the_first_keyframe_and_keeps_its_time_order() {
        let (mut feed, sink) = channel(4096); // room enough that the test never races the writer
        let writer = collect(sink);
        let send = |f: &mut Feed, s, pts: i64, key| f.send(s, pts, pts, F, key, &pts.to_le_bytes()).unwrap();
        send(&mut feed, StreamKind::Audio(0), -2 * A, true);
        send(&mut feed, StreamKind::Video, -F, false); // can't be decoded: skipped
        send(&mut feed, StreamKind::Audio(0), 0, true);
        send(&mut feed, StreamKind::Video, 0, true);
        send(&mut feed, StreamKind::Video, F, false);
        drop(feed);
        let got: Vec<_> = writer.join().unwrap().iter().map(|p| (p.0, p.5)).collect();
        let (v, a) = (StreamKind::Video, StreamKind::Audio(0));
        assert_eq!(got, [(v, 0), (a, 0), (v, F)], "audio before the keyframe's decode time dropped");
    }

    #[test]
    fn late_audio_goes_straight_in_and_audio_at_videos_time_waits() {
        let (mut feed, sink) = channel(16);
        let send = |f: &mut Feed, s, pts: i64, key| f.send(s, pts, pts, F, key, &pts.to_le_bytes()).unwrap();
        send(&mut feed, StreamKind::Video, 0, true); // no audio yet: video goes on alone
        send(&mut feed, StreamKind::Video, F, false);
        send(&mut feed, StreamKind::Audio(0), 0, true); // video already passed it
        assert_eq!(sink.items.try_iter().count(), 3);
        send(&mut feed, StreamKind::Audio(0), F, true); // not before video's decode time: held
        assert_eq!(sink.items.try_iter().count(), 0);
    }

    #[test]
    fn a_stalled_disk_stops_the_feed_instead_of_growing() {
        let (mut feed, _sink) = channel(0); // nobody drains: the disk is stuck
        video(&mut feed, true, 1).unwrap();
        let sent = (0..MAX_PACKETS * 2).take_while(|_| video(&mut feed, false, 1).is_ok()).count();
        assert_eq!(sent, MAX_PACKETS - 1);
        assert_eq!(video(&mut feed, false, 1).unwrap_err(), DISK_TOO_SLOW);
    }

    #[test]
    fn the_backlog_may_exceed_the_live_limits() {
        let (mut feed, sink) = channel(MAX_PACKETS * 2);
        let big: Bytes = vec![0; MAX_QUEUED].into();
        for i in 0..MAX_PACKETS as i64 * 2 {
            let p = Packet {
                stream: StreamKind::Video,
                pts_us: i,
                dts_us: i,
                duration_us: 1,
                keyframe: i == 0,
                data: big.clone(),
            };
            feed.send_packet(&p).unwrap();
        }
        assert_eq!(feed.queued_bytes(), 0, "shared bytes aren't copied");
        video(&mut feed, false, 1).unwrap(); // the live limit still has its room
        drop(feed);
        assert_eq!(sink.items.iter().count(), MAX_PACKETS * 2 + 1);
    }

    #[test]
    fn the_byte_budget_holds_too() {
        let (mut feed, _sink) = channel(0);
        video(&mut feed, true, Feed::queue_limit() / 2).unwrap();
        assert!(video(&mut feed, false, Feed::queue_limit() / 2 + 1).is_err());
    }

    #[test]
    fn buffers_are_reused_once_written_and_the_pool_stays_small() {
        let (mut feed, sink) = channel(0);
        let writer = std::thread::spawn(move || drain(sink, "x", |_| Ok(())));
        for i in 0..MAX_PACKETS * 4 {
            video(&mut feed, i == 0, if i % 50 == 0 { 1 << 20 } else { 100 }).unwrap();
            while feed.queued_bytes() > 0 {
                std::thread::yield_now(); // let the writer keep up
            }
        }
        assert!((1..=2).contains(&feed.made), "made {} buffers", feed.made);
        let p = lock(&feed.pool);
        assert!(p.bytes <= POOL_BYTES);
        assert_eq!(p.bytes, p.bufs.iter().map(Vec::capacity).sum::<usize>(), "the pool counts its bytes");
        drop(p);
        let big = (0..MAX_PACKETS * 4).filter(|i| i % 50 == 0).count();
        assert_eq!(feed.written_bytes(), (big << 20) as u64 + (MAX_PACKETS * 4 - big) as u64 * 100);
        drop(feed);
        writer.join().unwrap().unwrap();
    }

    #[test]
    fn the_pool_keeps_buffers_only_within_its_limits() {
        let pool = Mutex::new(Pool { bufs: Vec::with_capacity(2), bytes: 0 });
        give(&pool, Vec::with_capacity(POOL_BYTES + 1));
        assert!(lock(&pool).bufs.is_empty(), "a buffer over the byte limit is freed");
        for _ in 0..3 {
            give(&pool, Vec::with_capacity(10));
        }
        let p = lock(&pool);
        assert_eq!(p.bufs.len(), 2, "no more buffers than it has room for");
        assert_eq!(p.bytes, p.bufs.iter().map(Vec::capacity).sum::<usize>());
    }

    #[test]
    fn earlier_parts_are_counted_from_one_with_lost_markers_noted() {
        let part = |name: &str, lost| Ok(Ok((PathBuf::from(name), lost)));
        let panicked = Err(Box::new("boom") as Box<dyn std::any::Any + Send>);
        let results = [part("1", false), Ok(Err("disk".to_string())), panicked, part("4", true)];
        let (parts, unfinished, lost) = finished_parts(results.into_iter());
        assert_eq!(parts, [PathBuf::from("1"), PathBuf::from("4")]);
        assert_eq!(unfinished, [2, 3]);
        assert!(lost, "part 4 lost its markers");
        assert!(!finished_parts([part("1", false)].into_iter()).2);
    }

    #[test]
    fn audio_still_held_at_stop_ends_at_a_pause_video_has_not_reached() {
        let mut c = Cuts::new();
        (c.open, c.audio_from, c.presses) = (true, 0, VecDeque::from([100]));
        assert!(c.keeps_audio_at_stop(50));
        assert!(!c.keeps_audio_at_stop(100), "from the pause on");
        assert!(!c.keeps_audio_at_stop(150));
        c.presses.clear();
        assert!(c.keeps_audio_at_stop(150), "no pause pending");
    }

    #[test]
    fn a_sleep_ends_the_cut_where_the_next_frame_was_due() {
        let none = VecDeque::new();
        let started = || {
            let mut c = Cuts::new();
            assert_eq!(c.video(0, 0, F, true, &none), Some(false));
            assert_eq!(c.video(F, F, F, false, &none), Some(false));
            c
        };
        // Decoded after the gap: resume from that frame's decode time.
        let mut c = started();
        assert_eq!(c.video(HOUR, HOUR, F, false, &none), None);
        assert_eq!((c.ended_at, c.presses.front()), (Some(2 * F), Some(&HOUR)));
        // A B-frame decoded before the gap (or exactly at its limit): resume just after it is shown.
        for dts in [2 * F, F + MAX_GAP_US] {
            let mut c = started();
            assert_eq!(c.video(HOUR, dts, F, false, &none), None);
            assert_eq!((c.ended_at, c.presses.front()), (Some(2 * F), Some(&(HOUR + RESUME_SLACK_US + 1))));
        }
        // A gap of exactly the limit is not a sleep.
        let mut c = started();
        assert_eq!(c.video(F + MAX_GAP_US, F + MAX_GAP_US, F, false, &none), Some(false));
        // A pause due on the first frame after the sleep ends before the gap too.
        let mut c = started();
        c.presses.push_back(10 * F);
        assert_eq!(c.video(HOUR, HOUR, F, false, &none), None);
        assert_eq!(c.ended_at, Some(2 * F));
    }

    /// Paused at 2F (the frame shown at 2F is the first left out), with `audio_end`, then resumed
    /// at 5F; returns the cuts after the keyframe at 10F (decoded at 6F) and `held`.
    fn resumed(audio_end: Option<i64>, held: &VecDeque<Item>) -> Cuts {
        let mut c = Cuts::new();
        let none = VecDeque::new();
        c.video(0, 0, F, true, &none);
        c.video(F, F, F, false, &none);
        c.presses.push_back(2 * F);
        assert_eq!(c.video(2 * F, 2 * F, F, false, &none), None);
        (c.audio_end, c.audio_dur) = (audio_end, A);
        c.presses.push_back(5 * F);
        assert_eq!(c.video(10 * F, 6 * F, F, true, held), Some(false));
        c
    }

    fn held_audio(pts: i64) -> VecDeque<Item> {
        let a = Item {
            stream: StreamKind::Audio(0),
            pts_us: pts,
            dts_us: pts,
            duration_us: A,
            keyframe: true,
            new_part: false,
            data: Data::Marker,
            counted: 0,
        };
        VecDeque::from([a])
    }

    #[test]
    fn a_resume_removes_the_time_from_the_pause_to_the_keyframes_decode_time() {
        // From the pause (2F) to the keyframe's decode time (6F); its display time (10F) is later.
        assert_eq!(resumed(None, &VecDeque::new()).removed_us, 4 * F);
        // Frames shown after the pause cap it: P5 (decoded at F) was shown up to 6F, so a
        // keyframe shown at 9F can move back by 3F at most.
        let mut c = Cuts::new();
        let none = VecDeque::new();
        c.video(0, 0, F, true, &none);
        c.video(5 * F, F, F, false, &none);
        c.presses.push_back(2 * F);
        assert_eq!(c.video(2 * F, 2 * F, F, false, &none), Some(false), "B2 is shown before P5");
        assert_eq!(c.video(6 * F, 3 * F, F, false, &none), None);
        c.presses.push_back(8 * F);
        assert_eq!(c.video(9 * F, 9 * F, F, true, &none), Some(false));
        assert_eq!(c.removed_us, 3 * F);
    }

    #[test]
    fn a_resume_continues_audio_seamlessly_from_the_packet_playing_at_the_cut() {
        // 4F (the time removed without audio) after the last audio written ends.
        let (end, most) = (2 * F, 4 * F);
        let limit = end + most;
        // A held packet plays across the limit: audio resumes with it.
        let c = resumed(Some(end), &held_audio(limit - A + 1));
        assert_eq!((c.audio_from, c.removed_us), (limit - A + 1, most - A + 1));
        // The one held ends right at the limit, so the packet playing then was left out: whole
        // packets after the last one written.
        let c = resumed(Some(end), &held_audio(limit - A));
        let whole = most / A * A;
        assert_eq!((c.audio_from, c.removed_us), (end + whole, whole));
    }

    #[test]
    fn a_write_error_stops_the_feed_with_the_users_message() {
        let (mut feed, sink) = channel(0);
        let writer = std::thread::spawn(move || drain(sink, "Disk full.", |_| Err("ENOSPC".into())));
        video(&mut feed, true, 1).unwrap();
        let mut result = Ok(());
        for _ in 0..1000 {
            result = video(&mut feed, false, 1);
            if result.is_err() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(result.unwrap_err(), "Disk full.");
        drop(feed);
        assert_eq!(writer.join().unwrap().unwrap_err(), "ENOSPC");
    }
}
