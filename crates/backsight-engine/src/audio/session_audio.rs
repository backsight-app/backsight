//! Connects the WASAPI sources and the shared QPC mixer to the session's packet sink.

use super::{
    microphone::Microphone,
    mixer::{CHUNK_FRAMES, Mixer, SourceOptions},
    process_loopback::ProcessLoopback,
    wasapi::{CHANNELS, Loopback, LoopbackStats, RATE},
};
use crate::{
    capture::{
        foreground::{Program, alive},
        wgc::{CLOCK_HZ, now},
    },
    session::{Shared, lock},
};
use backsight_config::{Action, Settings};
use backsight_encode::session::{AudioSession, Encoded};
use std::{
    collections::VecDeque,
    os::windows::io::OwnedHandle,
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::Duration,
};

const NO_TARGET: &str = "Game-only audio is waiting for a game. Keep the game in front for a few seconds.";
const LOST_TARGET: &str = "The game closed. Backsight will record the sound of the next game you play.";
const ENCODER_STOPPED: &str =
    "Audio encoding stopped. Restart capture; if it keeps happening, pick another format on Backsight's Audio page.";

pub(crate) struct SessionAudio {
    mixer: Arc<Mutex<Mixer>>,
    mic_index: Option<usize>,
    desktop: Option<Loopback>,
    mic: Option<Microphone>,
    game: Arc<Mutex<Option<Game>>>,
    thread: Option<JoinHandle<()>>,
}

/// A process identity retained across settings rebuilds, including after the process exits.
#[derive(Clone)]
pub struct GameAudioTarget {
    process: Arc<OwnedHandle>,
    pid: u32,
    name: String,
}

impl GameAudioTarget {
    /// The program's file name, e.g. `game.exe`, for Home to say whose sound is recorded.
    pub fn name(&self) -> &str {
        &self.name
    }
}

enum Game {
    Pending,
    Pinned(GameAudioTarget, Result<ProcessLoopback, String>),
}

pub(crate) fn track_count(settings: &Settings) -> usize {
    let sources = usize::from(settings.audio.desktop) + usize::from(settings.audio.microphone);
    usize::from(sources != 0) + if settings.audio.separate_tracks { sources } else { 0 }
}

impl SessionAudio {
    pub(crate) fn start(
        shared: Arc<Shared>,
        settings: &Settings,
        in_front: impl Fn() -> Option<Program> + Send + 'static,
        encoders: Vec<AudioSession>,
        muted: bool,
        key_down: bool,
        target: Option<GameAudioTarget>,
    ) -> std::io::Result<Self> {
        let audio = &settings.audio;
        let output = usize::from(audio.desktop);
        let mic_index = audio.microphone.then_some(output);
        let ptt = settings.hotkeys.get(&Action::PushToTalk).is_some_and(|binding| !binding.trim().is_empty());
        let mut sources = [SourceOptions::default(); 2];
        if let Some(index) = mic_index {
            sources[index] = SourceOptions { muted, push_to_talk: ptt };
        }
        let start = now();
        let mut mixer = Mixer::new(start, &sources[..output + usize::from(audio.microphone)], audio.separate_tracks);
        if let Some(index) = mic_index.filter(|_| key_down) {
            mixer.key(index, true, start);
        }
        let mixer = Arc::new(Mutex::new(mixer));
        let game = Arc::new(Mutex::new(
            (audio.desktop && audio.game_audio_only)
                .then(|| target.map_or(Game::Pending, |target| start_game(target, &mixer))),
        ));
        let desktop = (audio.desktop && !audio.game_audio_only).then(|| Loopback::start(input(&mixer, 0)));
        let mic = mic_index.map(|index| Microphone::start(&audio.microphone_device, input(&mixer, index)));
        let (mix, game_state) = (mixer.clone(), game.clone());
        let thread = thread::Builder::new().name("backsight-audio-encode".into()).spawn(move || {
            let mut tracks = EncodeTracks::new(encoders);
            let mut chunk = MixScratch::new(&tracks);
            let mut publish = |offset, packets: &[Encoded<'_>]| shared.push_audio(offset, packets);
            let mut next_target_check = 0;
            while !shared.stopped() {
                let time = now();
                if time >= next_target_check {
                    select_game(&mut lock(&game_state), &mix, &in_front);
                    next_target_check = time + CLOCK_HZ / 10;
                }
                let target = lock(&mix).mix_target(time);
                while !shared.stopped() {
                    let mixed = chunk.take(&mut lock(&mix), target);
                    if !mixed {
                        break;
                    }
                    if let Err(e) = tracks.push(&chunk, &mut publish) {
                        shared.fail(ENCODER_STOPPED, &e);
                        return;
                    }
                }
                thread::park_timeout(Duration::from_millis(10));
            }
            // shortcut: up to LATENCY_MS of audio a quiet source was still owed is not mixed on
            // stop, so a file can end ~0.2 s before its video; mix up to the stop time if users notice.
            if let Err(e) = tracks.finish(&mut publish) {
                shared.fail(ENCODER_STOPPED, &e);
            }
        })?;
        Ok(Self { mixer, mic_index, desktop, mic, game, thread: Some(thread) })
    }

    pub(crate) fn game_target(&self) -> Option<GameAudioTarget> {
        match lock(&self.game).as_ref() {
            Some(Game::Pinned(target, _)) => Some(target.clone()),
            _ => None,
        }
    }

    pub(crate) fn has_mic(&self) -> bool {
        self.mic_index.is_some()
    }

    pub(crate) fn set_mic_muted(&self, muted: bool) {
        if let Some(i) = self.mic_index {
            lock(&self.mixer).set_muted(i, muted, now());
        }
    }

    pub(crate) fn mic_key_control(&self) -> Option<impl Fn(bool) + Send + 'static> {
        let index = self.mic_index?;
        let mixer = self.mixer.clone();
        Some(move |down| lock(&mixer).key(index, down, now()))
    }

    pub(crate) fn diagnostics(&self) -> (Option<LoopbackStats>, Option<String>) {
        let mic = self.mic.as_ref().map(Microphone::stats);
        let game = lock(&self.game);
        let (output, target_problem) = match game.as_ref() {
            None => (self.desktop.as_ref().map(Loopback::stats), None),
            Some(Game::Pending) => (None, Some(NO_TARGET)),
            Some(Game::Pinned(target, source)) => (
                source.as_ref().ok().map(ProcessLoopback::stats),
                if alive(&target.process) { source.as_ref().err().map(String::as_str) } else { Some(LOST_TARGET) },
            ),
        };
        let problem = target_problem
            .or_else(|| output.as_ref().and_then(|s| s.problem))
            .or_else(|| mic.as_ref().and_then(|s| s.problem))
            .map(str::to_owned);
        (output.or(mic), problem)
    }

    /// Called after the shared session stop flag is set.
    pub(crate) fn stop(&mut self) {
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
        if let Some(source) = &mut self.desktop {
            source.stop();
        }
        if let Some(source) = &mut self.mic {
            source.stop();
        }
        if let Some(Game::Pinned(_, Ok(source))) = lock(&self.game).as_mut() {
            source.stop();
        }
    }
}

fn input(mixer: &Arc<Mutex<Mixer>>, index: usize) -> impl FnMut(&[f32], u64) + Send + 'static {
    let mixer = mixer.clone();
    move |samples, time| lock(&mixer).push(index, samples, time)
}

/// Pins the game once one has kept focus for a few seconds (`crate::game::GameWatch`). When the
/// pinned game has exited, waits for the next one instead of staying on the dead process.
fn select_game(game: &mut Option<Game>, mixer: &Arc<Mutex<Mixer>>, in_front: &impl Fn() -> Option<Program>) {
    if let Some(Game::Pinned(target, source)) = game
        && !alive(&target.process)
    {
        if let Ok(source) = source {
            source.stop();
        }
        *game = Some(Game::Pending);
    }
    if !matches!(game, Some(Game::Pending)) {
        return;
    }
    if let Some(p) = in_front() {
        *game = Some(start_game(GameAudioTarget { pid: p.pid, process: p.process, name: p.exe }, mixer));
    }
}

fn start_game(target: GameAudioTarget, mixer: &Arc<Mutex<Mixer>>) -> Game {
    if !alive(&target.process) {
        return Game::Pinned(target, Err(LOST_TARGET.into()));
    }
    let process = target.process.clone();
    let mut push = input(mixer, 0);
    let source = ProcessLoopback::start(target.pid, move |samples, time| {
        if alive(&process) {
            push(samples, time);
        }
    });
    Game::Pinned(target, source)
}

struct MixScratch {
    frame_limit: usize,
    samples: Vec<f32>,
    at: u64,
}

impl MixScratch {
    fn new(tracks: &EncodeTracks) -> Self {
        let frame_limit = tracks.encoders[0].frame_size().min(CHUNK_FRAMES);
        let samples = Vec::with_capacity(frame_limit * usize::from(CHANNELS) * tracks.encoders.len());
        Self { frame_limit, samples, at: 0 }
    }

    fn take(&mut self, mixer: &mut Mixer, target: i64) -> bool {
        self.samples.clear();
        mixer.mix_chunk(target, self.frame_limit, |_, samples, at| {
            self.at = at;
            assert!(samples.len() <= self.samples.capacity() - self.samples.len());
            self.samples.extend_from_slice(samples);
        })
    }
}

// shortcut: bound codec bursts/skew to eight packets per track and 40 KiB per packet (enough for
// a raw 4608-frame stereo FLAC block plus headers), revisit when adding codecs or channel layouts.
const PACKET_SLOTS: usize = 8;
const PACKET_BYTES: usize = 40 * 1024;
const MAX_TRACKS: usize = 3;

struct PendingPacket {
    bytes: Vec<u8>,
    metadata: Encoded<'static>,
}

impl PendingPacket {
    fn encoded(&self) -> Encoded<'_> {
        Encoded { data: &self.bytes, ..self.metadata }
    }
}

struct PacketQueue {
    slots: [PendingPacket; PACKET_SLOTS],
    len: usize,
}

impl PacketQueue {
    fn new() -> Self {
        let slots = std::array::from_fn(|_| PendingPacket {
            bytes: Vec::with_capacity(PACKET_BYTES),
            metadata: Encoded { data: &[], pts_us: 0, dts_us: 0, duration_us: 0, keyframe: false },
        });
        Self { slots, len: 0 }
    }

    fn collect(&mut self, encode: impl FnOnce(&mut dyn FnMut(Encoded)) -> Result<(), String>) -> Result<(), String> {
        let mut overflow = false;
        encode(&mut |packet| {
            // FLAC can emit a side-data-only packet when flushed.
            if packet.data.is_empty() {
                return;
            }
            if self.len == PACKET_SLOTS || packet.data.len() > PACKET_BYTES {
                overflow = true;
                return;
            }
            let slot = &mut self.slots[self.len];
            slot.bytes.clear();
            slot.bytes.extend_from_slice(packet.data);
            slot.metadata = Encoded { data: &[], ..packet };
            self.len += 1;
        })?;
        if overflow {
            return Err("audio encoder exceeded its packet staging capacity".into());
        }
        Ok(())
    }
}

struct EncodeTracks {
    encoders: Vec<AudioSession>,
    pending: Vec<PacketQueue>,
    samples: u64,
    // A mixer jump (e.g. sleep) changes the QPC origin for future samples. Packets delayed by
    // the encoder still use the earlier origin, including a codec frame straddling the jump.
    origins: VecDeque<(i64, u64)>,
}

impl EncodeTracks {
    fn new(encoders: Vec<AudioSession>) -> Self {
        assert!((1..=MAX_TRACKS).contains(&encoders.len()));
        let pending = encoders.iter().map(|_| PacketQueue::new()).collect();
        Self { encoders, pending, samples: 0, origins: VecDeque::with_capacity(16) }
    }

    fn push(&mut self, chunk: &MixScratch, sink: &mut impl FnMut(i64, &[Encoded<'_>])) -> Result<(), String> {
        let elapsed = self.samples * CLOCK_HZ / u64::from(RATE);
        let origin = chunk.at.saturating_sub(elapsed);
        if self.origins.back().is_none_or(|&(_, previous)| previous.abs_diff(origin) > 1) {
            if self.origins.len() == self.origins.capacity() {
                return Err("too many audio timeline jumps before the encoder returned a packet".into());
            }
            let start_us = (self.samples * 1_000_000 + u64::from(RATE) / 2) / u64::from(RATE);
            self.origins.push_back((start_us as i64, origin));
        }
        let stride = chunk.samples.len() / self.encoders.len();
        assert!(stride > 0 && chunk.samples.len().is_multiple_of(self.encoders.len()));
        self.samples += (stride / usize::from(CHANNELS)) as u64;
        for ((encoder, queue), samples) in
            self.encoders.iter_mut().zip(&mut self.pending).zip(chunk.samples.chunks_exact(stride))
        {
            queue.collect(|receive| encoder.push(samples, receive))?;
        }
        self.publish(sink)
    }

    fn finish(&mut self, sink: &mut impl FnMut(i64, &[Encoded<'_>])) -> Result<(), String> {
        for (encoder, queue) in self.encoders.iter_mut().zip(&mut self.pending) {
            queue.collect(|receive| encoder.finish(receive))?;
        }
        self.publish(sink)?;
        if self.pending.iter().any(|queue| queue.len != 0) {
            return Err("audio encoder ended with an incomplete track group".into());
        }
        Ok(())
    }

    fn publish(&mut self, sink: &mut impl FnMut(i64, &[Encoded<'_>])) -> Result<(), String> {
        while self.pending.iter().all(|queue| queue.len > 0) {
            let first = self.pending[0].slots[0].encoded();
            let mut group = [first; MAX_TRACKS];
            let packets = &mut group[..self.pending.len()];
            for (packet, queue) in packets.iter_mut().zip(&self.pending) {
                *packet = queue.slots[0].encoded();
                if (packet.pts_us, packet.dts_us, packet.duration_us) != (first.pts_us, first.dts_us, first.duration_us)
                {
                    return Err("audio encoders returned different track timestamps".into());
                }
            }
            let position = self.origins.iter().rposition(|&(start, _)| start <= first.pts_us).unwrap_or(0);
            let &(_, origin) =
                self.origins.get(position).ok_or("audio encoder returned a packet without a clock origin")?;
            sink((origin / 10) as i64, packets);
            self.origins.drain(..position);
            for queue in &mut self.pending {
                queue.slots.rotate_left(1);
                queue.len -= 1;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backsight_encode::{Catalog, catalog::resolve};
    use std::{os::windows::io::AsHandle, process::Command};

    #[test]
    fn an_exited_pin_is_retained_without_starting_loopback() {
        let mut child = Command::new("cmd.exe").args(["/C", "exit"]).spawn().unwrap();
        let target = GameAudioTarget {
            process: Arc::new(child.as_handle().try_clone_to_owned().unwrap()),
            pid: child.id(),
            name: "child".into(),
        };
        child.wait().unwrap();
        let mixer = Arc::new(Mutex::new(Mixer::new(now(), &[SourceOptions::default()], false)));
        let Game::Pinned(retained, Err(problem)) = start_game(target.clone(), &mixer) else {
            panic!("exited process should stay pinned without a loopback source");
        };
        assert!(Arc::ptr_eq(&retained.process, &target.process));
        assert_eq!(retained.pid, target.pid);
        assert_eq!(problem, LOST_TARGET);
    }

    #[test]
    fn game_only_audio_waits_for_the_next_game_after_the_pinned_one_exits() {
        let mut child = Command::new("cmd.exe").args(["/C", "exit"]).spawn().unwrap();
        let process = Arc::new(child.as_handle().try_clone_to_owned().unwrap());
        let target = GameAudioTarget { process, pid: child.id(), name: "old".into() };
        child.wait().unwrap();
        let mixer = Arc::new(Mutex::new(Mixer::new(now(), &[SourceOptions::default()], false)));
        let mut game = Some(Game::Pinned(target, Err(LOST_TARGET.into())));
        // Nothing in front yet: back to waiting, not stuck on the dead process.
        select_game(&mut game, &mixer, &|| None);
        assert!(matches!(game, Some(Game::Pending)));
        // The next game that holds focus is picked up (a live one stays pinned).
        let mut next =
            Command::new("cmd.exe").args(["/C", "pause"]).stdin(std::process::Stdio::piped()).spawn().unwrap();
        let program = Program {
            pid: next.id(),
            process: Arc::new(next.as_handle().try_clone_to_owned().unwrap()),
            exe: "new.exe".into(),
            label: String::new(),
            window: 0,
            started: 0,
        };
        select_game(&mut game, &mixer, &|| Some(program.clone()));
        let Some(Game::Pinned(pinned, _)) = &mut game else { panic!("next game should be pinned") };
        assert_eq!(pinned.name, "new.exe");
        if let Some(Game::Pinned(_, Ok(source))) = &mut game {
            source.stop();
        }
        next.kill().unwrap();
        next.wait().unwrap();
    }

    #[test]
    fn a_packet_burst_waits_for_matching_source_packets() {
        let codec = &Catalog::get().audio_encoders[0];
        let values = resolve(&codec.options, None);
        let encoders = (0..2).map(|_| AudioSession::open(codec, &values, RATE, CHANNELS, true).unwrap()).collect();
        let mut tracks = EncodeTracks::new(encoders);
        tracks.origins.push_back((0, 10_000));
        let packet = |pts_us| Encoded { data: &[1], pts_us, dts_us: pts_us, duration_us: 20, keyframe: true };
        tracks.pending[0]
            .collect(|sink| {
                sink(packet(0));
                sink(packet(20));
                Ok(())
            })
            .unwrap();
        tracks.publish(&mut |_, _| panic!("the source track has no packets yet")).unwrap();
        for pts in [0, 20] {
            tracks.pending[1]
                .collect(|sink| {
                    sink(packet(pts));
                    Ok(())
                })
                .unwrap();
            let mut groups = 0;
            tracks
                .publish(&mut |offset, group| {
                    assert_eq!(offset, 1000);
                    assert_eq!(group.len(), 2);
                    assert!(group.iter().all(|p| p.pts_us == pts));
                    groups += 1;
                })
                .unwrap();
            assert_eq!(groups, 1);
        }
    }

    #[test]
    fn mixer_encoders_keep_qpc_and_track_order_across_a_stall() {
        const START: u64 = 60 * CLOCK_HZ;
        let catalog = Catalog::get();
        for codec in &catalog.audio_encoders {
            let values = resolve(&codec.options, None);
            let encoders = (0..3).map(|_| AudioSession::open(codec, &values, RATE, CHANNELS, true).unwrap()).collect();
            let mut tracks = EncodeTracks::new(encoders);
            let mut chunk = MixScratch::new(&tracks);
            let mut mixer = Mixer::new(START, &[SourceOptions::default(); 2], true);
            let mut packets = Vec::new();
            let mut publish = |offset, group: &[Encoded<'_>]| {
                assert_eq!(group.len(), 3);
                for (index, packet) in group.iter().enumerate() {
                    packets.push((index, offset, offset + packet.pts_us, packet.duration_us));
                }
            };
            let mut mix_to = |mixer: &mut Mixer, time| {
                let target = mixer.mix_target(time);
                while chunk.take(mixer, target) {
                    tracks.push(&chunk, &mut publish).unwrap();
                }
            };
            let tone = [0.25; 1000 * CHANNELS as usize];
            let silence = [0.0; 1000 * CHANNELS as usize];
            // Feed uneven 1000-frame chunks, including unaligned Opus frame boundaries.
            let feed = |mixer: &mut Mixer, mix_to: &mut dyn FnMut(&mut Mixer, u64), origin| {
                for i in 0..25 {
                    let at = origin + i * 1000 * CLOCK_HZ / u64::from(RATE);
                    mixer.push(0, &tone, at);
                    mixer.push(1, &silence, at);
                    mix_to(mixer, at);
                }
            };
            feed(&mut mixer, &mut mix_to, START);
            let wake = START + 5 * CLOCK_HZ;
            mix_to(&mut mixer, wake);
            feed(&mut mixer, &mut mix_to, wake);
            tracks.finish(&mut publish).unwrap();
            let (groups, remainder) = packets.as_chunks::<3>();
            assert!(remainder.is_empty(), "{}", codec.id);
            let mut last_pts = i64::MIN;
            let mut offsets = Vec::new();
            for group in groups {
                let (_, offset, pts, duration) = group[0];
                assert_eq!(
                    group,
                    &[(0, offset, pts, duration), (1, offset, pts, duration), (2, offset, pts, duration)],
                    "{}",
                    codec.id
                );
                assert!(pts > last_pts, "{}", codec.id);
                last_pts = pts;
                if offsets.last() != Some(&offset) {
                    offsets.push(offset);
                }
            }
            assert_eq!(offsets.len(), 2, "{}", codec.id);
            assert_eq!(offsets[0], (START / 10) as i64);
            assert!(last_pts >= (wake / 10) as i64, "{}", codec.id);
        }
    }
}
