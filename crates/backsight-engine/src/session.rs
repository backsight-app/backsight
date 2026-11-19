//! Instant Replay and recording end to end: screen capture → GPU conversion → video encoder,
//! WASAPI sources → mixer → audio encoders, both into the replay buffer (and, while recording, to a file
//! writer), and saving clips from it. One pipeline serves both: recording never starts a second
//! encoder, and turning replay off while recording only shrinks the buffer.
//!
//! Threads: WGC callbacks keep the newest frame, one video thread paces, converts it into the
//! converter's NV12 texture and encodes it, and the audio worker mixes and encodes. Both push
//! packets stamped on the QPC clock in microseconds, so a clip lines audio up with video.
//! Any failure stops the session; saving a clip from it is refused, because its footage is old.

pub use crate::audio::session_audio::GameAudioTarget;
use crate::{
    audio::{
        session_audio::{SessionAudio, track_count},
        wasapi::{self, LoopbackStats},
    },
    capture::{
        convert::{self, Converter, Downloader},
        d3d::Gpu,
        foreground::{self, Program},
        hdr,
        target::{self, HdrFallback, SizeSettle},
        wgc::{self, Capture, Frame},
    },
    game::{self, GameWatch},
    pacer::{Pacer, PacerStats},
    recording::{self, Feed},
    replay::{Packet, ReplayBuffer, StreamKind},
    system,
    watchdog::{Step, Watch},
};
use backsight_config::{General, Settings};
use backsight_encode::{
    Catalog,
    catalog::{Container, resolve},
    mux::{Muxer, Track, free_name, holds_chapters, needs_global_header},
    session::{AudioSession, Encoded, StreamParams, VideoSession},
};
use std::{
    fmt::Display,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering::Relaxed},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use windows::{Graphics::Capture::Direct3D11CaptureFrame, Win32::Graphics::Direct3D11::ID3D11Texture2D};

const UNKNOWN_CHOICE: &str = "Your settings name an encoder or file format this version of Backsight doesn't have. Pick one on Backsight's Video page.";
const NO_GPU: &str = "Backsight couldn't open your graphics card. Update your graphics driver, then restart Backsight.";
const NO_CAPTURE: &str = "Backsight couldn't start capturing your screen. Restart Backsight; if that doesn't help, update Windows and your graphics driver.";
const NO_CONVERTER: &str = "Your graphics card couldn't prepare frames for the video encoder. Update your graphics driver, then restart Backsight.";
const NO_AUDIO_ENCODER: &str = "The audio encoder didn't start. Pick another audio format on Backsight's Audio page.";
/// The screen counts as still once nothing new has been captured for this long. Shorter gaps (a
/// game drawing fewer pictures than the video's frame rate) are encoded as always.
const STILL_AFTER_US: i64 = 100_000;
/// On a still screen, how often the same picture is encoded again: gaps in the video stay under
/// a tenth of a second, so sound and picture end together in every clip and recording.
const STILL_REPEAT_US: i64 = 66_000;
const CAPTURE_LOST: &str = "Screen capture stopped, usually because the monitor was disconnected.";
const VIDEO_STOPPED: &str = "The video encoder stopped, often after a graphics driver update or crash. If it keeps happening, update your graphics driver.";
const NO_FOLDER: &str =
    "Windows didn't tell Backsight where your Videos folder is. Pick a folder for clips on Backsight's General page.";
const UNKNOWN_AUDIO: &str =
    "Your settings name an audio format this version of Backsight doesn't have. Pick one on Backsight's Audio page.";
const NO_VIDEO_FIT: &str = "The chosen file format can't hold this video encoder's format. Pick another file format on Backsight's Video page.";
const NO_AUDIO_FIT: &str =
    "The chosen file format can't hold this audio format. Pick another audio format on Backsight's Audio page.";
const NOTHING_YET: &str = "There's nothing to save yet: Instant Replay has only just started.";
const REPLAY_OFF: &str = "Instant Replay is off. Turn it on to save a clip.";
const REPLAY_STOPPED: &str =
    "Instant Replay stopped, so it has no new footage to save. Backsight is restarting it: try again in a few seconds.";
const WRITER_CRASHED: &str = "The recording stopped unexpectedly. What was recorded so far is kept next to your videos as a file ending in .recording.part, which Backsight can recover.";
const NO_RECORDING_TO_MARK: &str = "Markers go into a recording. Start recording first, then add a marker.";
const NO_MARKERS_IN_FORMAT: &str =
    "This file format can't hold markers. To use them, pick another file format on Backsight's Video page.";

/// Counters for the diagnostics page and the log.
#[derive(Debug, Clone, Default)]
pub struct Diagnostics {
    /// Dropped (the game outran the target frame rate), repeated and skipped frames.
    pub pacer: PacerStats,
    /// Frames handed to the video encoder, or left out because the screen hadn't changed.
    pub encoded_frames: u64,
    /// Frames inside the encoder that haven't come out as packets yet (look-ahead, B-frames).
    /// A number that keeps growing means the encoder can't keep up.
    pub encoder_queue: u64,
    /// Frames encoded more than one frame late: the GPU or the encoder is too slow.
    pub late_frames: u64,
    /// Frames left out because the screen hadn't changed (counted in `encoded_frames` too).
    pub rested_frames: u64,
    /// Window mode: frames the game's window sent (none while the video is black, waiting for one).
    pub window_frames: u64,
    /// Silence fills, drift corrections and device errors for the output source, or mic if absent.
    pub audio: Option<LoopbackStats>,
    /// Current source or target problem, in plain language.
    pub audio_problem: Option<String>,
    pub buffer_bytes: usize,
    pub buffer: Duration,
    /// The running recording's file and writer; `None` when not recording.
    pub recording: Option<RecordingStats>,
}

/// What the recording writer is doing.
#[derive(Debug, Clone, Default)]
pub struct RecordingStats {
    /// Packet bytes written to the file so far.
    pub written_bytes: u64,
    /// Average written rate over the last few seconds; `None` until there is enough to tell.
    pub written_kbps: Option<u32>,
    /// Bytes waiting for the writer thread.
    pub queued_bytes: usize,
    /// The recording stops when the queue reaches this.
    pub queue_limit: usize,
}

/// Instant Replay, running. Dropping it stops capture and encoding and discards the buffer.
pub struct ReplaySession {
    shared: Arc<Shared>,
    capture: Option<Capture>,
    audio_capture: Option<SessionAudio>,
    video_thread: Option<JoinHandle<()>>,
    /// The layout clips are saved in (see `Catalog::clip_container`).
    container: Container,
    /// The container chosen in settings, crash-safe by default: recordings use it.
    record_container: Container,
    recording: Option<Recording>,
    /// Instant Replay is on; off while the pipeline runs only for a recording.
    replay_on: AtomicBool,
    /// A recording starts a new file after this much video; zero never.
    split: Duration,
    video: StreamParams,
    audio: Vec<StreamParams>,
    length: Duration,
    /// `None` when Windows has no Videos folder and the settings name none.
    folder: Option<PathBuf>,
    /// The program in front, for file names and game-only audio.
    game: GameWatch,
    /// How files are named and filed (`name_after_game`, `folder_per_game`).
    naming: General,
    /// What Home should say about capture (a missing monitor, the game window), if anything.
    capture_notice: Arc<Mutex<Option<&'static str>>>,
    /// Monitor mode: the id of the monitor captured, and the one saved in settings.
    monitor: Option<(String, String)>,
    /// Window mode: the game's window is on a monitor that does or doesn't show HDR, unlike the
    /// one checked at start (`target::window_hdr_restart`), so a restart records it right.
    hdr_moved: Arc<AtomicBool>,
    /// Window mode: the program whose window is (or was last) captured; never one that failed.
    window_owner: Arc<Mutex<Option<Program>>>,
    /// The video is HDR.
    hdr: bool,
}

struct Recording {
    /// Finishes the file once the feed closes; returns the earlier parts and the last one.
    writer: JoinHandle<recording::Parts>,
    started: Instant,
    /// Set while paused.
    paused_since: Option<Instant>,
    paused_total: Duration,
    /// Split into parts, as read when it started.
    splits: bool,
    /// Turns the written byte count into a rate, one reading per diagnostics poll.
    rate: Mutex<recording::RateMeter>,
}

impl Recording {
    fn time(&self) -> Duration {
        let paused = self.paused_total + self.paused_since.map_or(Duration::ZERO, |p| p.elapsed());
        self.started.elapsed().saturating_sub(paused)
    }
}

/// How a recording ended.
pub struct Stopped {
    /// The finished file (the last part when the recording was split), or why there is none.
    pub file: Result<PathBuf, String>,
    /// The parts finished before the last one, oldest first, when `video.split_minutes` is set.
    pub earlier_parts: Vec<PathBuf>,
    /// Earlier parts (numbered from 1) that couldn't be finished: [`recording::unfinished_note`].
    pub unfinished_parts: Vec<usize>,
    /// Why the recording stopped before it was asked to (a slow or full drive).
    pub stopped_early: Option<String>,
    /// Markers were added but couldn't be kept: [`recording::MARKERS_LOST`].
    pub markers_lost: bool,
}

/// A recording whose file is still being finished.
pub struct Stopping {
    time: Duration,
    writer: JoinHandle<recording::Parts>,
    stopped_early: Option<String>,
    /// Written so far, for the watchdog's limit.
    bytes: u64,
}

impl Stopping {
    pub fn finish(self) -> Stopped {
        let _watch = Watch::start(Step::FinishRecording { bytes: self.bytes });
        let (earlier_parts, unfinished_parts, file, markers_lost) = self
            .writer
            .join()
            .unwrap_or_else(|_| (Vec::new(), Vec::new(), Err(user_error(WRITER_CRASHED, &"writer panicked")), false));
        let stopped_early = self.stopped_early;
        match &file {
            Ok(path) => tracing::info!(
                "recording saved: {} ({:?}, {} earlier parts)",
                path.display(),
                self.time,
                earlier_parts.len()
            ),
            Err(e) => tracing::error!("recording failed: {e}"),
        }
        Stopped { file, earlier_parts, unfinished_parts, stopped_early, markers_lost }
    }
}

/// What the capture callback, the video thread and the audio thread share.
pub(crate) struct Shared {
    input: Mutex<Input>,
    replay: Mutex<ReplayBuffer>,
    stop: AtomicBool,
    /// The first failure, in plain language.
    error: Mutex<Option<String>>,
    frames: AtomicU64,
    video_packets: AtomicU64,
    /// Slots not encoded because the screen was still (they count in `frames` too).
    rested: AtomicU64,
    /// Frames from window capture (see [`Diagnostics::window_frames`]).
    window_frames: AtomicU64,
    late: AtomicU64,
    /// The running recording's feed; taken (closing the file) to stop it. Lock after `replay`.
    recorder: Mutex<Option<Feed>>,
    /// Why the recording stopped on its own.
    record_error: Mutex<Option<String>>,
    /// The video thread asks the encoder for a keyframe (a recording resumes on it).
    want_key: AtomicBool,
}

struct Input {
    converter: VideoConverter,
    pacer: Pacer,
    /// Waits for a new capture size to hold before rebuilding the converter for it.
    settle: SizeSettle,
    /// The newest captured frame, converted when its slot comes up. A newer one replaces it
    /// unconverted, so a game faster than the output rate costs no extra conversions.
    held: Option<Held>,
    /// The newest frame skipped while the capture size settles; the video thread uses it once the
    /// size has held (`SizeSettle::due`), for a still window that sends no frame after a resize.
    resized: Option<Held>,
    /// Window mode: the newest capture that sent a frame (numbered from 1). An older one's frames,
    /// arriving while it is being replaced, are ignored.
    window_source: u64,
}

/// A captured frame kept out of WGC's pool (two buffers) for at most one slot: a newer frame or
/// its conversion hands it back, so WGC always has a buffer free to capture into.
struct Held {
    texture: ID3D11Texture2D,
    size: (u32, u32),
    /// Keeps `texture` out of the pool until dropped.
    _frame: Option<Direct3D11CaptureFrame>,
}

enum VideoConverter {
    Sdr(Box<Converter>),
    Hdr(hdr::Converter),
}

impl VideoConverter {
    fn new(gpu: &Gpu, source: (u32, u32), size: (u32, u32), fps: u32, hdr: bool) -> windows::core::Result<Self> {
        if hdr {
            hdr::Converter::new(gpu, source, size).map(Self::Hdr)
        } else {
            Converter::new(gpu, source, size, fps).map(|c| Self::Sdr(Box::new(c)))
        }
    }
}

impl Input {
    /// Rebuilds the converter for a `source`-sized capture, scaled into the encoder's `size`
    /// keeping its shape (black bars, `target::fit`): a resized window or a new monitor resolution.
    fn resize(
        &mut self,
        gpu: &Gpu,
        source: (u32, u32),
        size: (u32, u32),
        fps: u32,
    ) -> Result<(), (&'static str, String)> {
        // The video processor shares the immediate context with NVENC's `send` on the video thread.
        let _device = gpu.lock();
        let hdr = matches!(self.converter, VideoConverter::Hdr(_));
        self.converter = VideoConverter::new(gpu, source, size, fps, hdr).map_err(|e| (NO_CONVERTER, e.to_string()))?;
        tracing::info!("capture size changed to {source:?}");
        Ok(())
    }
}

impl ReplaySession {
    /// Starts capturing the primary monitor and the configured audio sources into the replay buffer.
    /// Errors are plain-language messages for the UI; the raw detail goes to the log.
    pub fn start(settings: &Settings) -> Result<Self, String> {
        Self::start_with_audio_state(settings, false, false, None, None)
    }

    /// Applies microphone state and restores the selected game before audio capture starts.
    /// `window` ([`Self::captured_window`] of the session before): window mode captures it again
    /// straight away if it's still open, and checks its monitor for HDR, not the one in front.
    pub fn start_with_audio_state(
        settings: &Settings,
        muted: bool,
        key_down: bool,
        target: Option<GameAudioTarget>,
        window: Option<Program>,
    ) -> Result<Self, String> {
        let mut tried_hdr = false;
        let first = Self::open(
            settings,
            muted,
            key_down,
            target.clone(),
            window.clone(),
            HdrFallback::NotNeeded,
            &mut tried_hdr,
        );
        match first {
            // An HDR step that failed after the encoder (the converter's shader on another card,
            // a 16-bit capture) gets one standard start, as an encoder that won't do HDR does.
            // HDR video stays wanted, so window mode's Home line says why it's standard.
            Err(e) if tried_hdr => {
                tracing::warn!("HDR video didn't start ({e}): trying standard video");
                Self::open(settings, muted, key_down, target, window, HdrFallback::Encoder, &mut tried_hdr)
            }
            started => started,
        }
    }

    /// `forced`: why HDR is already given up (`NotNeeded` to decide here). `tried_hdr` is set
    /// once the encoder opened in HDR, so a later failure may be HDR's.
    #[allow(clippy::too_many_arguments)] // two callers, both in `start_with_audio_state`
    fn open(
        settings: &Settings,
        muted: bool,
        key_down: bool,
        target: Option<GameAudioTarget>,
        window: Option<Program>,
        forced: HdrFallback,
        tried_hdr: &mut bool,
    ) -> Result<Self, String> {
        let _watch = Watch::start(Step::CaptureStart);
        let began = Instant::now(); // where a slow start goes is logged with "replay starting"
        let c = Catalog::get();
        let (Some(container), Some(venc)) =
            (c.container(&settings.video.container), c.video_encoder(&settings.video.encoder))
        else {
            return Err(user_error(UNKNOWN_CHOICE, &format!("{:?}", settings.video)));
        };
        let record_container = container.clone();
        let container = Catalog::clip_container(container);
        let fps = settings.video.fps;
        let hdr = settings.video.hdr;
        let window_mode = settings.video.capture_target == "window";
        // HDR never stops a start: what can't be HDR records standard video and Home says why.
        let hdr_fallback = match forced {
            HdrFallback::NotNeeded => target::hdr_settings(hdr, venc.hdr, &record_container.ext),
            forced => forced,
        };
        if hdr && hdr_fallback != HdrFallback::NotNeeded {
            tracing::info!(
                "HDR video is on, but these settings can't record it ({hdr_fallback:?}): recording standard video"
            );
        }
        let hdr = hdr && hdr_fallback == HdrFallback::NotNeeded;

        // The UI keeps these compatible; a hand-edited settings file may not.
        if !container.video_codecs.contains(&venc.codec) {
            return Err(user_error(NO_VIDEO_FIT, &format!("{} with {}", container.id, venc.id)));
        }
        let global_header = needs_global_header(&container);
        let gpu = Arc::new(Gpu::open(&venc.vendor).map_err(|e| user_error(NO_GPU, &e))?);
        let monitors = wgc::monitors();
        let ids: Vec<_> = monitors.iter().map(|m| (m.id.as_str(), m.primary)).collect();
        let picked = target::pick_monitor(&settings.video.capture_monitor, &ids);
        let (monitor, monitor_gone) = match picked {
            Some((i, gone)) => (windows::Win32::Graphics::Gdi::HMONITOR(monitors[i].handle as _), gone),
            None => (wgc::primary_monitor(), false),
        };
        let captured_monitor = picked
            .filter(|_| !window_mode)
            .map(|(i, _)| (monitors[i].id.clone(), settings.video.capture_monitor.clone()));
        if monitor_gone {
            tracing::warn!("monitor {:?} isn't connected; capturing the main one", settings.video.capture_monitor);
        }
        // An SDR monitor (or Windows HDR switched off) records standard video and says so on Home.
        // Window mode checks the game's monitor when a restart brought its window (still open,
        // same program), else the monitor in front, normally the game's; a game found on the
        // other kind restarts once it's captured.
        let window = window.filter(|p| window_mode && p.window_open());
        let hdr_fallback = match &window {
            Some(p) if hdr && hdr_window_failed(&p.exe) => HdrFallback::WindowCapture,
            _ => hdr_fallback,
        };
        let hdr = hdr && hdr_fallback == HdrFallback::NotNeeded;
        let hdr_monitor = match &window {
            Some(p) => wgc::window_monitor(p.window),
            None if window_mode => wgc::foreground_monitor(),
            None => monitor,
        };
        let hdr_fallback = match hdr.then(|| wgc::monitor_hdr_enabled(hdr_monitor)) {
            None => hdr_fallback,
            Some(Ok(true)) => HdrFallback::NotNeeded,
            Some(Ok(false)) => {
                tracing::info!("HDR video is on, but the monitor isn't in HDR mode: recording standard video");
                HdrFallback::SdrMonitor
            }
            Some(Err(e)) => {
                tracing::warn!("couldn't tell whether the monitor shows HDR ({e}): recording standard video");
                HdrFallback::Unknown
            }
        };
        let hdr = hdr && hdr_fallback == HdrFallback::NotNeeded;
        // The output size comes from the monitor in window mode too, so it never changes.
        let source = wgc::monitor_size(monitor).map_err(|e| user_error(NO_CAPTURE, &e))?;
        let size = output_size(source, &settings.video.resolution);
        if hdr && (size.0 < 2 || size.1 < 2) {
            return Err("Choose a larger video resolution on Backsight's Video page.".into());
        }
        let gpu_ready = began.elapsed();
        let values = resolve(&c.video_options(venc), settings.video.encoder_options.get(&venc.id));
        let system_memory = || -> Result<_, String> {
            let download = Downloader::new(&gpu, size).map_err(|e| user_error(NO_CONVERTER, &e))?;
            Ok((VideoSession::open(venc, &values, size.0, size.1, fps, global_header), Some(download)))
        };
        // An encoder that won't start in HDR (an old driver, a card without 10-bit) gets a standard start.
        let hdr_video =
            hdr.then(|| VideoSession::open_hdr(venc, &values, size, fps, global_header, Some(gpu.for_ffmpeg())));
        let (video, download, hdr_fallback) = match hdr_video {
            Some(Ok(video)) => (Ok(video), None, hdr_fallback),
            failed => {
                let hdr_fallback = match failed {
                    Some(Err(e)) => {
                        tracing::warn!("{} didn't start in HDR ({e}): recording standard video", venc.id);
                        HdrFallback::Encoder
                    }
                    _ => hdr_fallback,
                };
                let (video, download) = if VideoSession::takes_d3d11(venc) {
                    or_system_memory(
                        VideoSession::open_d3d11(venc, &values, size, fps, global_header, gpu.for_ffmpeg()),
                        system_memory,
                    )?
                } else {
                    system_memory()?
                };
                (video, download, hdr_fallback)
            }
        };
        let hdr = hdr && hdr_fallback == HdrFallback::NotNeeded;
        *tried_hdr = hdr;
        let encoder_failed = format!(
            "{} didn't start. Pick another encoder on Backsight's Video page, or update your graphics driver.",
            venc.label
        );
        let video = video.map_err(|e| user_error(&encoder_failed, &e))?;
        let encoder_ready = began.elapsed();
        let converter = VideoConverter::new(&gpu, source, size, fps, hdr).map_err(|e| user_error(NO_CONVERTER, &e))?;
        let converter_ready = began.elapsed();
        let count = track_count(settings);
        let audio = if count != 0 {
            let aenc = c
                .audio_encoder(&settings.audio.encoder)
                .ok_or_else(|| user_error(UNKNOWN_AUDIO, &settings.audio.encoder))?;
            if !container.audio_encoders.contains(&aenc.id) || !record_container.audio_encoders.contains(&aenc.id) {
                return Err(user_error(NO_AUDIO_FIT, &format!("{} with {}", container.id, aenc.id)));
            }
            let values = resolve(&aenc.options, settings.audio.encoder_options.get(&aenc.id));
            (0..count)
                .map(|_| {
                    AudioSession::open(aenc, &values, wasapi::RATE, wasapi::CHANNELS, global_header)
                        .map_err(|e| user_error(NO_AUDIO_ENCODER, &e))
                })
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };

        let length = Duration::from_secs(settings.replay.length_seconds.into());
        let shared = Arc::new(Shared {
            input: Mutex::new(Input {
                converter,
                pacer: Pacer::new(fps, wgc::CLOCK_HZ),
                settle: SizeSettle::default(),
                held: None,
                resized: None,
                window_source: 0,
            }),
            replay: Mutex::new(ReplayBuffer::new(length, replay_ceiling(system::physical_memory()))),
            stop: AtomicBool::new(false),
            error: Mutex::new(None),
            frames: AtomicU64::new(0),
            video_packets: AtomicU64::new(0),
            rested: AtomicU64::new(0),
            window_frames: AtomicU64::new(0),
            late: AtomicU64::new(0),
            recorder: Mutex::new(None),
            record_error: Mutex::new(None),
            want_key: AtomicBool::new(false),
        });
        if window_mode {
            // Until a game window is captured the video is black, not missing: a clip or recording
            // made meanwhile is still a valid file with its sound in step. The game's first frame
            // replaces it.
            let black = convert::black(&gpu, source, hdr).map_err(|e| user_error(NO_CONVERTER, &e))?;
            let mut input = lock(&shared.input);
            input.held = Some(Held { texture: black, size: source, _frame: None });
            input.pacer.frame_arrived(wgc::now());
        }
        tracing::info!(
            "replay starting: {} on {}, {source:?} → {size:?} at {fps} fps, {}, {} s (graphics card ready after {gpu_ready:.1?}, video encoder after {encoder_ready:.1?}, converter after {converter_ready:.1?}, sound encoders after {:.1?})",
            venc.id,
            gpu.name,
            container.id,
            length.as_secs(),
            began.elapsed()
        );
        // What decides the cost per frame, for reading a "it uses a lot of GPU" report from the log.
        tracing::info!(
            "replay path: {} video, encoder fed from {}, monitor on {}",
            if hdr { "HDR" } else { "standard" },
            if download.is_some() { "system memory (a GPU-to-RAM copy per frame)" } else { "GPU textures" },
            wgc::monitor_adapter(monitor).as_deref().unwrap_or("an unknown graphics card")
        );
        // MinUpdateInterval is a minimum gap; the real one adds the wait for the game's next frame,
        // so cap at twice the output rate: games up to 2× fps aren't throttled, faster ones are.
        let min_interval = Duration::from_secs(1) / (2 * fps);
        let cursor = settings.video.capture_cursor;
        let notice = target::start_notice(window_mode, monitor_gone, hdr_fallback);
        let capture_notice = Arc::new(Mutex::new(notice));
        let hdr_moved = Arc::new(AtomicBool::new(false));
        let window_owner = Arc::new(Mutex::new(None));
        let game = if window_mode {
            let follow = follow_window(
                shared.clone(),
                gpu.clone(),
                capture_notice.clone(),
                cursor,
                min_interval,
                size,
                fps,
                (settings.video.hdr, hdr_fallback),
                hdr_moved.clone(),
                (window, window_owner.clone()),
            );
            GameWatch::start(None, follow)
        } else {
            GameWatch::start(Some(monitor.0 as isize), |_| {})
        };
        let mut session = ReplaySession {
            shared: shared.clone(),
            capture: None,
            audio_capture: None,
            video_thread: None,
            container,
            record_container,
            recording: None,
            replay_on: AtomicBool::new(true),
            split: Duration::from_secs(u64::from(settings.video.split_minutes) * 60),
            video: video.params(),
            audio: audio.iter().map(AudioSession::params).collect(),
            length,
            folder: settings.general.video_dir(),
            game: game.map_err(|e| user_error(NO_CAPTURE, &e))?,
            naming: settings.general.clone(),
            capture_notice,
            monitor: captured_monitor,
            hdr_moved,
            window_owner,
            hdr,
        };
        // From here on, an early return drops `session`, which stops whatever already started.
        let (s, g) = (shared.clone(), gpu.clone());
        session.video_thread = Some(
            std::thread::Builder::new()
                .name("backsight-video".into())
                .spawn(move || encode_video(&s, &g, video, download, size, fps))
                .map_err(|e| user_error(NO_CAPTURE, &e))?,
        );
        let (s, g) = (shared.clone(), gpu.clone());
        if !window_mode {
            let on_frame = move |frame: windows::core::Result<Frame<'_>>| match frame {
                Ok(frame) => {
                    if let Err((user, e)) = s.frame_arrived(&g, frame, size, fps, None) {
                        s.fail(user, &e);
                    }
                }
                Err(e) => s.fail(CAPTURE_LOST, &e),
            };
            let capture = if hdr {
                Capture::hdr_monitor(&gpu, monitor, cursor, Some(min_interval), on_frame)
            } else {
                Capture::monitor(&gpu, monitor, cursor, Some(min_interval), on_frame)
            };
            session.capture = Some(capture.map_err(|e| user_error(NO_CAPTURE, &e))?);
        }
        if !audio.is_empty() {
            session.audio_capture = Some(
                SessionAudio::start(shared, settings, session.game.reader(), audio, muted, key_down, target)
                    .map_err(|e| user_error(NO_AUDIO_ENCODER, &e))?,
            );
        }
        tracing::info!(
            "replay started: capture and sound running {:.1?} after the start was asked for",
            began.elapsed()
        );
        Ok(session)
    }

    /// Saves the last `length` of the buffer (starting on a keyframe) into the video folder and
    /// returns the file. Refused after a failure: the buffer then holds only old footage.
    pub fn save_clip(&self) -> Result<PathBuf, String> {
        self.prepare_clip()?.save()
    }

    /// Snapshots the replay now. [`Clip::save`] writes it without holding the session lock.
    pub fn prepare_clip(&self) -> Result<Clip, String> {
        if !self.replay_on() {
            return Err(REPLAY_OFF.into());
        }
        if let Some(e) = self.error() {
            tracing::warn!("clip refused, the session failed earlier: {e}");
            return Err(REPLAY_STOPPED.into());
        }
        let started = Instant::now();
        let packets = lock(&self.shared.replay).clip(self.length);
        if packets.is_empty() {
            return Err(NOTHING_YET.into());
        }
        let (folder, name) = self.place()?;
        let cant_save = format!(
            "Backsight couldn't save the clip in {}. Check that the drive has free space, or pick another folder on Backsight's General page.",
            folder.display()
        );
        let (container, video, audio) = (self.container.clone(), self.video.clone(), self.audio.clone());
        Ok(Clip::new(packets, move |packets| {
            let _watch = Watch::start(Step::SaveClip { bytes: packets.iter().map(|p| p.data.len() as u64).sum() });
            fs::create_dir_all(&folder).map_err(|e| user_error(&cant_save, &e))?;
            let mux = open_named_mux(&folder, &name, &container, &video, &audio, false)
                .map_err(|e| user_error(&cant_save, &e))?;
            let saved = finish_clip(mux, packets).map_err(|e| user_error(&cant_save, &e))?;
            tracing::info!("saved {} ({} packets) in {:?}", saved.display(), packets.len(), started.elapsed());
            Ok(saved)
        }))
    }

    /// How much more RAM the replay buffer may take before its ceiling: what clips kept from
    /// before a move may hold beside it.
    pub fn replay_room(&self) -> usize {
        lock(&self.shared.replay).room()
    }

    /// The folder and file name start for a file saved now: after the game, if the settings say
    /// so. In window mode that is the captured window's program, not whatever is in front.
    fn place(&self) -> Result<(PathBuf, String), String> {
        let root = self.folder.as_deref().ok_or(NO_FOLDER)?;
        let captured =
            lock(&self.window_owner).as_ref().filter(|p| !game::is_launcher(&p.exe)).and_then(game::program_name);
        Ok(game::place(root, captured.or_else(|| self.game.name()).as_deref(), &self.naming))
    }

    /// Applies new `name_after_game` and `folder_per_game` settings without a restart. A
    /// recording already running keeps its folder and name.
    pub fn set_naming(&mut self, general: &General) {
        self.naming = general.clone();
    }

    /// What Home should say about capture: the chosen monitor is missing, or window mode is
    /// waiting for, or lost, the game's window. `None` when all is well.
    pub fn capture_notice(&self) -> Option<&'static str> {
        let waiting = self.recording.is_some() && self.hdr_moved.load(Relaxed);
        lock(&self.capture_notice).or_else(|| target::hdr_after_recording(waiting, self.hdr))
    }

    /// Whether to restart on another monitor (see [`target::should_move`]), e.g. the saved one is
    /// plugged back in, or (window mode) to switch between HDR and standard video for the game's
    /// monitor. `monitors` is [`wgc::monitors`], asked for by the caller before it locks the
    /// session: Windows can take a while to answer.
    pub fn monitor_moved(&self, monitors: &[wgc::Monitor]) -> bool {
        self.hdr_moved.load(Relaxed)
            || self.monitor.as_ref().is_some_and(|(captured, saved)| {
                let ids: Vec<_> = monitors.iter().map(|m| (m.id.as_str(), m.primary)).collect();
                target::should_move(captured, saved, &ids)
            })
    }

    /// Monitor mode: [`Self::monitor_moved`] looks at the monitor list. Window mode doesn't.
    pub fn follows_monitor(&self) -> bool {
        self.monitor.is_some()
    }

    /// Window mode: the program whose window is (or was last) captured, for the next start.
    pub fn captured_window(&self) -> Option<Program> {
        lock(&self.window_owner).clone()
    }

    /// Keeps the selected game when applying unrelated settings to a running session.
    pub fn game_audio_target(&self) -> Option<GameAudioTarget> {
        self.audio_capture.as_ref().and_then(SessionAudio::game_target)
    }

    /// The game in front (or last in front), as its folder name; `None` for the desktop. Home shows it.
    pub fn game_name(&self) -> Option<String> {
        self.game.name()
    }

    /// How much a saved clip holds (`replay.length_seconds`).
    pub fn replay_length(&self) -> Duration {
        self.length
    }

    pub fn replay_on(&self) -> bool {
        self.replay_on.load(Relaxed)
    }

    /// Turns Instant Replay on or off without stopping the pipeline (or a recording). Off keeps
    /// only the newest keyframe or two, so a recording can still start cleanly.
    pub fn set_replay(&self, on: bool) {
        self.replay_on.store(on, Relaxed);
        lock(&self.shared.replay).set_window(if on { self.length } else { Duration::ZERO });
    }

    /// How often the next recording starts a new file (`video.split_minutes`); zero never. A
    /// recording already running keeps the value it started with.
    pub fn set_split(&mut self, every: Duration) {
        self.split = every;
    }

    /// Starts writing everything from the newest keyframe on (up to a GOP before the press) to a
    /// new file in the video folder. Does nothing if a recording is already running.
    pub fn start_recording(&mut self) -> Result<(), String> {
        if self.recording.is_some() {
            return Ok(());
        }
        if let Some(e) = self.error() {
            return Err(e);
        }
        let (folder, name) = self.place()?;
        let cant = format!(
            "Backsight couldn't start a recording in {}. Check that the drive has free space, or pick another folder on Backsight's General page.",
            folder.display()
        );
        fs::create_dir_all(&folder).map_err(|e| user_error(&cant, &e))?;
        let new_file = {
            let (folder, container) = (folder.clone(), self.record_container.clone());
            let (video, audio) = (self.video.clone(), self.audio.clone());
            // Parts are named by the time they start, so they sort in order.
            move || open_named_mux(&folder, &name, &container, &video, &audio, true)
        };
        let mux = new_file().map_err(|e| user_error(&cant, &e))?;
        let part = mux.part().to_path_buf();
        let cant_write = if self.record_container.crash_safe {
            format!(
                "Backsight couldn't keep writing the recording: the drive may be full or unplugged. What was recorded so far is the newest file ending in .recording.part in {}, which Backsight can recover.",
                folder.display()
            )
        } else {
            format!(
                "Backsight couldn't keep writing the recording: the drive may be full or unplugged. The unfinished file in {} may not play; a crash-safe file format (on Backsight's Video page) keeps what was recorded.",
                folder.display()
            )
        };
        *lock(&self.shared.record_error) = None;
        // Under the buffer's lock, so no packet falls between the backlog and the live feed. The
        // backlog shares the buffer's bytes and gets room of its own, however long the GOP.
        let (sink, lead_in) = {
            let replay = lock(&self.shared.replay);
            let backlog = replay.clip(Duration::ZERO);
            // The file starts at the backlog's keyframe, before the press: count that in the
            // recording's time so marker times match the file.
            let lead_in = backlog.first().map_or(0, |p| engine_now_us() - p.pts_us);
            let lead_in = Duration::from_micros(lead_in.max(0) as u64);
            let (mut feed, sink) = recording::channel(backlog.len());
            feed.expect_audio_tracks(self.audio.len());
            feed.split_every(self.split);
            // On an error `mux` is dropped unfinished, which removes its .part file.
            backlog.iter().try_for_each(|p| feed.send_packet(p)).map_err(|e| user_error(&cant, &e))?;
            *lock(&self.shared.recorder) = Some(feed);
            (sink, lead_in)
        };
        let writer = std::thread::Builder::new().name("backsight-recording".into()).spawn(move || {
            let (parts, unfinished, last, markers_lost) = recording::write_parts(sink, &cant_write, mux, new_file);
            (parts, unfinished, last.map_err(|e| user_error(&cant_write, &e)), markers_lost)
        });
        let writer = match writer {
            Ok(w) => w,
            Err(e) => {
                lock(&self.shared.recorder).take(); // the unstarted writer dropped `mux` and its .part
                return Err(user_error(&cant, &e));
            }
        };
        tracing::info!("recording started: {}", part.display());
        let started = Instant::now().checked_sub(lead_in).unwrap_or_else(Instant::now);
        self.recording = Some(Recording {
            writer,
            started,
            paused_since: None,
            paused_total: Duration::ZERO,
            splits: !self.split.is_zero(),
            rate: Mutex::default(),
        });
        Ok(())
    }

    /// Pauses the recording: what is captured from now on is left out. Packets still inside the
    /// encoders from before the press are written. Does nothing if not recording or paused.
    pub fn pause_recording(&mut self) {
        let Some(r) = self.recording.as_mut().filter(|r| r.paused_since.is_none()) else { return };
        r.paused_since = Some(Instant::now());
        if let Some(feed) = lock(&self.shared.recorder).as_mut() {
            feed.pause(engine_now_us());
        }
        tracing::info!("recording paused");
    }

    /// Resumes a paused recording on the next keyframe, which the encoder is asked for now.
    /// Does nothing if not paused.
    pub fn resume_recording(&mut self) {
        let Some(r) = self.recording.as_mut() else { return };
        let Some(since) = r.paused_since.take() else { return };
        r.paused_total += since.elapsed();
        if let Some(feed) = lock(&self.shared.recorder).as_mut() {
            feed.resume(engine_now_us());
        }
        self.shared.want_key.store(true, Relaxed);
        tracing::info!("recording resumed after {:?}", since.elapsed());
    }

    /// Marks "now" in the running recording (a chapter in the file, in whichever part is being
    /// written; moved back by the paused time, or to the pause if paused). Returns the recording's
    /// length at the marker; an error is a plain-language reason there's none.
    pub fn add_marker(&self) -> Result<Duration, String> {
        let r = self.recording.as_ref().ok_or(NO_RECORDING_TO_MARK)?;
        if !holds_chapters(&self.record_container) {
            return Err(NO_MARKERS_IN_FORMAT.into());
        }
        match lock(&self.shared.recorder).as_mut() {
            Some(feed) => feed.mark(engine_now_us()),
            None => return Err(self.recording_error().unwrap_or_else(|| NO_RECORDING_TO_MARK.into())),
        }
        let at = r.time();
        tracing::info!("marker added at {at:?}");
        Ok(at)
    }

    /// Whether the running recording is paused.
    pub fn recording_paused(&self) -> bool {
        self.recording.as_ref().is_some_and(|r| r.paused_since.is_some())
    }

    /// Whether the running recording is split into parts (the split setting when it started).
    pub fn recording_splits(&self) -> bool {
        self.recording.as_ref().is_some_and(|r| r.splits)
    }

    /// Stops the recording (paused or not) and finishes its file; `None` if none was running.
    pub fn stop_recording(&mut self) -> Option<Stopped> {
        self.begin_stop_recording().map(Stopping::finish)
    }

    /// Closes the recording's feed and returns at once; [`Stopping::finish`] waits for the file.
    /// Finishing can take as long as copying it (markers in a crash-safe MP4), so callers that
    /// hold a lock finish it after letting go.
    pub fn begin_stop_recording(&mut self) -> Option<Stopping> {
        let recording = self.recording.take()?;
        // Closes the feed: the writer finishes the file.
        let bytes = lock(&self.shared.recorder).take().map_or(0, |feed| feed.written_bytes());
        // Taken now: a recording started while this one finishes clears and reuses the slot.
        let stopped_early = lock(&self.shared.record_error).take();
        Some(Stopping { time: recording.time(), writer: recording.writer, stopped_early, bytes })
    }

    /// How much the current recording holds (paused time left out); `None` when not recording.
    pub fn recording_time(&self) -> Option<Duration> {
        self.recording.as_ref().map(Recording::time)
    }

    /// Why the current recording stopped on its own (it still needs [`Self::stop_recording`]).
    pub fn recording_error(&self) -> Option<String> {
        lock(&self.shared.record_error).clone()
    }

    /// The failure that stopped the session, in plain language.
    pub fn error(&self) -> Option<String> {
        lock(&self.shared.error).clone()
    }

    /// Whether this session records a microphone.
    pub fn records_mic(&self) -> bool {
        self.audio_capture.as_ref().is_some_and(SessionAudio::has_mic)
    }

    /// Applies the microphone mute gate to both the mix and the separate mic track.
    pub fn set_mic_muted(&self, muted: bool) {
        if let Some(audio) = &self.audio_capture {
            audio.set_mic_muted(muted);
        }
    }

    /// Lets the input thread close the mic gate while the session is busy saving or stopping.
    pub fn mic_key_control(&self) -> Option<impl Fn(bool) + Send + 'static> {
        self.audio_capture.as_ref().and_then(SessionAudio::mic_key_control)
    }

    pub fn diagnostics(&self) -> Diagnostics {
        let s = &self.shared;
        let pacer = lock(&s.input).pacer.stats();
        let (buffer_bytes, buffer) = {
            let r = lock(&s.replay);
            (r.bytes(), r.duration())
        };
        // In this order: a slot counted between the two reads can only make the queue look longer.
        let rested_frames = s.rested.load(Relaxed);
        let encoded_frames = s.frames.load(Relaxed);
        let (audio, audio_problem) = self.audio_capture.as_ref().map(SessionAudio::diagnostics).unwrap_or_default();
        let recording = self.recording.as_ref().and_then(|r| {
            let feed = lock(&self.shared.recorder);
            let feed = feed.as_ref()?;
            let written_bytes = feed.written_bytes();
            let written_kbps = lock(&r.rate).add(r.started.elapsed().as_secs_f64(), written_bytes);
            Some(RecordingStats {
                written_bytes,
                written_kbps,
                queued_bytes: feed.queued_bytes(),
                queue_limit: Feed::queue_limit(),
            })
        });
        Diagnostics {
            pacer,
            encoded_frames,
            encoder_queue: encoded_frames.saturating_sub(rested_frames + s.video_packets.load(Relaxed)),
            late_frames: s.late.load(Relaxed),
            rested_frames,
            window_frames: s.window_frames.load(Relaxed),
            audio,
            audio_problem,
            buffer_bytes,
            buffer,
            recording,
        }
    }
}

/// A replay snapshot ([`ReplaySession::prepare_clip`]). It holds its packets' memory until it is
/// saved or dropped.
pub struct Clip {
    packets: Vec<Packet>,
    write: Box<WriteClip>,
}

/// Saves a clip's packets as a file.
type WriteClip = dyn FnOnce(&[Packet]) -> Result<PathBuf, String> + Send;

impl Clip {
    /// `packets` start on a video keyframe, as [`ReplayBuffer::clip`] gives them; `write` saves them.
    pub fn new(
        packets: Vec<Packet>,
        write: impl FnOnce(&[Packet]) -> Result<PathBuf, String> + Send + 'static,
    ) -> Self {
        Clip { packets, write: Box::new(write) }
    }

    /// The packets' bytes.
    pub fn bytes(&self) -> usize {
        self.packets.iter().map(|p| p.data.len()).sum()
    }

    /// Keeps (at least) the last `length`, picked as [`ReplayBuffer::clip`] does: a keyframe first,
    /// the video after it and the audio from its decode time on. Allocates nothing.
    pub fn keep_last(&mut self, length: Duration) {
        let video = |p: &Packet| p.stream == StreamKind::Video;
        let latest = self.packets.iter().filter(|p| video(p)).map(|p| p.pts_us).max().unwrap_or(0);
        let cutoff = latest.saturating_sub(length.as_micros() as i64);
        let Some(start) = self.packets.iter().rposition(|p| video(p) && p.keyframe && p.pts_us <= cutoff) else {
            return;
        };
        if start == 0 {
            return; // starts there already
        }
        let (key_dts, mut i) = (self.packets[start].dts_us, 0);
        self.packets.retain(|p| {
            i += 1;
            if video(p) { i > start } else { p.pts_us >= key_dts }
        });
        // The keyframe goes first, ahead of the audio kept from before it.
        let key = self.packets.iter().position(video).expect("the keyframe is kept");
        self.packets[..=key].rotate_right(1);
    }

    /// Keeps the newest part that fits in about `max` bytes, from a keyframe on ([`Clip::keep_last`]).
    /// `false` when not even the part from the last keyframe fits.
    pub fn keep_bytes(&mut self, max: usize) -> bool {
        let video = |p: &Packet| p.stream == StreamKind::Video;
        let latest = self.packets.iter().filter(|p| video(p)).map(|p| p.pts_us).max().unwrap_or(0);
        let (mut sum, mut from) = (0, None);
        for p in self.packets.iter().rev() {
            sum += p.data.len();
            if sum > max {
                break;
            }
            if video(p) && p.keyframe {
                from = Some(p.pts_us);
            }
        }
        let Some(from) = from else { return false };
        self.keep_last(Duration::from_micros(latest.saturating_sub(from) as u64));
        true
    }

    pub fn save(self) -> Result<PathBuf, String> {
        (self.write)(&self.packets)
    }
}

impl Drop for ReplaySession {
    fn drop(&mut self) {
        let watch = Watch::start(Step::CaptureStop);
        self.shared.stop.store(true, Relaxed);
        self.capture = None; // a callback already running finishes on its own Arc
        if let Some(audio) = &mut self.audio_capture {
            audio.stop();
        }
        if let Some(t) = self.video_thread.take() {
            let _ = t.join();
        }
        drop(watch); // finishing the recording has its own limit
        let _ = self.stop_recording(); // finishes the file; the result is logged
        tracing::info!("replay stopped: {:?}", self.diagnostics());
    }
}

impl Shared {
    pub(crate) fn stopped(&self) -> bool {
        self.stop.load(Relaxed)
    }

    /// Records the first failure and stops capture and encoding; `raw` goes to the log only.
    pub(crate) fn fail(&self, user: &str, raw: &dyn Display) {
        let message = user_error(user, raw);
        lock(&self.error).get_or_insert(message);
        self.stop.store(true, Relaxed);
    }

    /// WGC callback: keeps the frame for the video thread to convert at its slot (see [`Held`]).
    /// An error is the plain-language message and the raw detail; the caller decides whether it
    /// ends the session. `window` (window mode) is the capture's number and a flag that is true
    /// until its first frame is kept.
    fn frame_arrived(
        &self,
        gpu: &Gpu,
        frame: Frame,
        size: (u32, u32),
        fps: u32,
        window: Option<(u64, &AtomicBool)>,
    ) -> Result<(), (&'static str, String)> {
        if self.stopped() {
            return Ok(()); // after a failure, until the session is dropped
        }
        let mut input = lock(&self.input);
        let current = match &input.converter {
            VideoConverter::Sdr(c) => c.input_size(),
            VideoConverter::Hdr(c) => c.input_size(),
        };
        // A new window's first frame is used at once: there is no picture of it to repeat, and a
        // still window may send no other.
        if let Some((source, _)) = window {
            if source < input.window_source {
                return Ok(()); // the window being replaced: its frame must not take the new one's place
            }
            input.window_source = source;
        }
        // Cleared only once this frame is kept: a failed converter rebuild sent no picture
        // (`target::HdrWindowTries` counts it).
        let first = window.is_some_and(|(_, f)| f.load(Relaxed));
        let kept = || Held { texture: frame.texture.clone(), size: frame.size, _frame: frame.handle.cloned() };
        if first {
            input.settle.settled(); // the old window's size change no longer matters
            match &mut input.converter {
                VideoConverter::Sdr(c) => c.forget_views(),
                VideoConverter::Hdr(c) => c.forget_views(),
            }
        } else if !input.settle.observe(current, frame.size, frame.time) {
            // Still resizing: the pacer repeats the last picture; this one waits in case it's the last.
            input.resized = Some(kept());
            return Ok(());
        }
        input.resized = None;
        if frame.size != current {
            input.resize(gpu, frame.size, size, fps)?;
        }
        // Replacing a frame its slot hasn't taken hands it back unconverted (the pacer counts it
        // as dropped).
        input.held = Some(kept());
        if let Some((_, f)) = window {
            f.store(false, Relaxed);
        }
        input.pacer.frame_arrived(frame.time);
        Ok(())
    }

    /// `offset_us` moves the encoder's timestamps (zero at its first input) onto the QPC clock.
    fn push(&self, stream: StreamKind, offset_us: i64, p: Encoded) {
        if stream == StreamKind::Video {
            self.video_packets.fetch_add(1, Relaxed);
        }
        self.push_packets(offset_us, [(stream, p)]);
    }

    /// A clip snapshot or recording stop sees all tracks for a timestamp together.
    pub(crate) fn push_audio(&self, offset_us: i64, packets: &[Encoded<'_>]) {
        self.push_packets(offset_us, packets.iter().enumerate().map(|(index, &p)| (StreamKind::Audio(index as u8), p)));
    }

    fn push_packets<'a>(&self, offset_us: i64, packets: impl IntoIterator<Item = (StreamKind, Encoded<'a>)>) {
        let mut replay = lock(&self.replay);
        let mut recorder = lock(&self.recorder);
        for (stream, p) in packets {
            replay.push_encoded(stream, offset_us, p);
            if let Some(feed) = recorder.as_mut()
                && let Err(e) =
                    feed.send(stream, p.pts_us + offset_us, p.dts_us + offset_us, p.duration_us, p.keyframe, p.data)
            {
                *recorder = None; // the writer finishes the file with what it has
                tracing::error!("recording stopped early: {e}");
                lock(&self.record_error).get_or_insert(e);
            }
        }
    }
}

#[derive(Debug, PartialEq)]
enum Still {
    Encode,
    /// The same picture again, as a keyframe because one is due.
    Keyframe,
    /// The same picture again: nothing to encode.
    Rest,
}

/// What to do with a pacer slot at `slot_us`. `fresh_us`: the newest slot with a new picture;
/// `sent_us`: the newest slot encoded; `key_us`: the newest keyframe sent or asked for;
/// `want_key`: a recording starts or resumes and waits for a keyframe; `key_gap_us`: the user's
/// keyframe interval, which the encoder counts in frames and so stretches on a still screen.
fn still_slot(fresh_us: i64, sent_us: i64, key_us: i64, slot_us: i64, want_key: bool, key_gap_us: i64) -> Still {
    match () {
        _ if want_key || slot_us - fresh_us < STILL_AFTER_US => Still::Encode,
        _ if slot_us - key_us >= key_gap_us => Still::Keyframe,
        _ if slot_us - sent_us >= STILL_REPEAT_US => Still::Encode,
        _ => Still::Rest,
    }
}

/// The video thread: one frame per pacer slot, converted on the GPU and encoded. On a still
/// screen most slots are left out: players hold a picture until the next one's time, so the
/// video looks the same and the encoder (most of Instant Replay's GPU use on a desktop) rests.
fn encode_video(
    s: &Shared,
    gpu: &Gpu,
    mut video: VideoSession,
    download: Option<Downloader>,
    size: (u32, u32),
    fps: u32,
) {
    let frame_ticks = wgc::CLOCK_HZ / u64::from(fps);
    let mut start_us = None;
    // Leaving slots out needs an encoder that can hand over the frames it holds back (B-frames,
    // its own queue) when the pictures stop coming, or they would come out late: NVENC so far.
    let can_rest = video.can_rest();
    let key_gap_us = video.keyframe_interval_us();
    let (mut fresh_us, mut sent_us, mut key_us) = (0, 0, 0);
    let mut holding = false; // the encoder may hold frames sent since its last rest
    while !s.stopped() {
        let now = wgc::now();
        let mut input = lock(&s.input);
        // A still window resized once sends no frame after the settle time: use the last one skipped.
        if input.settle.due(now)
            && let Some(resized) = input.resized.take()
        {
            input.settle.settled();
            // A failed rebuild keeps the old converter and picture; the window's next frame tries again.
            let old = match input.resize(gpu, resized.size, size, fps) {
                Ok(()) => {
                    input.pacer.frame_arrived(now);
                    input.held.replace(resized)
                }
                Err((_, raw)) => {
                    tracing::warn!("the resized capture's last picture couldn't be used: {raw}");
                    Some(resized)
                }
            };
            drop(input);
            drop(old); // after the lock, as in `release_held`
            continue;
        }
        let due = input.pacer.next_deadline();
        let Some(slot) = input.pacer.poll(now) else {
            drop(input);
            // Nothing captured yet, or the next slot isn't due: sleep until it is.
            let wait = due.map_or(50_000, |d| d.saturating_sub(now));
            std::thread::sleep(Duration::from_micros(wait / 10));
            continue;
        };
        if due.is_some_and(|d| now > d + frame_ticks) {
            s.late.fetch_add(1, Relaxed);
        }
        let start = *start_us.get_or_insert_with(|| input.pacer.start().map_or(0, |t| t as i64 / 10));
        // Released at the end of the loop, after `input`: never give a frame back while holding the
        // lock the capture callback waits on.
        let held = input.held.take();
        let slot_us = slot.pts as i64 * 1_000_000 / i64::from(fps);
        if held.is_some() || slot.pts == 0 {
            fresh_us = slot_us;
        }
        if can_rest {
            match still_slot(fresh_us, sent_us, key_us, slot_us, s.want_key.load(Relaxed), key_gap_us) {
                Still::Rest => {
                    drop(input);
                    if std::mem::take(&mut holding) {
                        let _device = download.is_none().then(|| gpu.lock());
                        if let Err(e) = video.rest(&mut |p| s.push(StreamKind::Video, start, p)) {
                            return s.fail(VIDEO_STOPPED, &e);
                        }
                    }
                    s.rested.fetch_add(1, Relaxed);
                    s.frames.fetch_add(1, Relaxed);
                    continue;
                }
                Still::Keyframe => {
                    video.request_keyframe();
                    key_us = slot_us; // the packet may come out a few frames later
                }
                Still::Encode => {}
            }
        }
        if let Some(held) = &held {
            let _device = gpu.lock(); // other threads use the immediate context too (converter rebuilds)
            let frame = Frame { texture: &held.texture, size: held.size, time: 0, handle: None };
            let converted = match &mut input.converter {
                VideoConverter::Sdr(c) => c.convert_from(gpu, frame.texture, frame.size).map(|_| ()),
                VideoConverter::Hdr(c) => c.convert_from(gpu, &frame),
            };
            if let Err(e) = converted {
                return s.fail(VIDEO_STOPPED, &e);
            }
        }
        let converted = match &input.converter {
            VideoConverter::Sdr(c) => match &download {
                None => video.next_gpu_frame().map(|frame| c.copy_to_encoder(gpu, frame)),
                Some(d) => {
                    video.planes_mut().and_then(|planes| d.download(gpu, c.output(), planes).map_err(|e| e.to_string()))
                }
            },
            VideoConverter::Hdr(c) => video.next_gpu_frame().map(|frame| c.copy_to_encoder(gpu, frame)),
        };
        let gpu_encoder = download.is_none();
        drop(input);
        if let Err(e) = converted {
            return s.fail(VIDEO_STOPPED, &e);
        }
        s.frames.fetch_add(1, Relaxed);
        if s.want_key.swap(false, Relaxed) {
            video.request_keyframe(); // a recording resumes on it
        }
        let device = gpu_encoder.then(|| gpu.lock());
        let sent = video.send(slot.pts as i64, &mut |p| {
            if p.keyframe {
                key_us = key_us.max(p.pts_us);
            }
            s.push(StreamKind::Video, start, p)
        });
        (sent_us, holding) = (slot_us, true);
        drop(device);
        if let Err(e) = sent {
            return s.fail(VIDEO_STOPPED, &e);
        }
    }
}

/// Window mode: called by the game watcher 10 times a second. Captures the game's window and
/// keeps it until it closes (`target::should_capture`: another program only takes over when it
/// fills its monitor or the captured program started it), and keeps `notice` saying what's going
/// on. A closed or minimized window, or a capture error, never stops the session: the pacer
/// repeats the last picture meanwhile, and a failed capture is tried again after `RETRY`. HDR
/// capture that keeps failing (`target::HdrWindowTries`) asks for a restart in standard video.
#[allow(clippy::too_many_arguments)] // one caller; each argument is a different kind of thing
fn follow_window(
    s: Arc<Shared>,
    gpu: Arc<Gpu>,
    notice: Arc<Mutex<Option<&'static str>>>,
    cursor: bool,
    min_interval: Duration,
    size: (u32, u32),
    fps: u32,
    (hdr_wanted, hdr_fallback): (bool, HdrFallback),
    hdr_moved: Arc<AtomicBool>,
    (mut carried, captured_out): (Option<Program>, Arc<Mutex<Option<Program>>>),
) -> impl FnMut(Option<&Program>) + Send + 'static {
    const RETRY: Duration = Duration::from_secs(2);
    const OK: u8 = 0;
    const CLOSED: u8 = 1;
    const FAILED: u8 = 2;
    // The captured (pid, window), its capture, how it ended (OK while running) and whether it
    // has yet to send a picture.
    type Captured = ((u32, isize), Capture, Arc<AtomicU8>, Arc<AtomicBool>);
    let mut current: Option<Captured> = None;
    // A capture that failed: the same (pid, window) is tried again after RETRY, and meanwhile it
    // still counts as captured, so a failure never lets another program's window in.
    let mut failed: Option<((u32, isize), Instant)> = None;
    let mut lost = false;
    // The captured (or failed) program, for `Program::started` in `target::should_capture`.
    // `captured_out` gets only one that captured, for the next session (`carried`, tried first),
    // or one whose HDR capture was given up, so the restart records it in standard video.
    let mut owner: Option<Program> = None;
    // When the current capture began, in FILETIME ticks like `Program::started`.
    let mut began = 0;
    // The pool's format is the session's, so frames fit its converter wherever the window goes.
    let hdr = hdr_wanted && hdr_fallback == HdrFallback::NotNeeded;
    // The captured window's monitor (a raw HMONITOR) when its HDR was last checked, and what
    // that check said (`None`: Windows didn't say). Checked again only when the monitor changes:
    // MonitorFromWindow is cheap, the HDR check isn't.
    let (mut checked_monitor, mut window_hdr) = (None, None);
    // Numbers each capture started, for `Input::window_source`.
    let mut sources = 0u64;
    // HDR capture failures in a row; once given up, the failed window isn't tried again here.
    let (mut hdr_tries, mut hdr_given_up) = (target::HdrWindowTries::default(), false);
    // The window frame count when it last changed, and when: a game in front that sends none
    // for `EXCLUSIVE_QUIET` may be in exclusive full screen.
    let (mut last_frames, mut last_frame_at) = (0, Instant::now());
    // The last `launched` answer, keyed by (pid, start time, owner pid and start, began): the process
    // snapshot behind it costs a few ms, too much to repeat 10 times a second.
    let mut launch_cache = None;
    move |game| {
        if s.stopped() {
            release_held(&s);
            current = None;
            return;
        }
        // The program whose HDR capture is given up, taken when it fails: `owner` may change below.
        let mut give_up = None;
        if let Some((id, _, end, fresh)) = &current {
            match end.load(Relaxed) {
                OK => {}
                CLOSED => {
                    tracing::info!("the captured game window closed");
                    release_held(&s);
                    // Not carried into a restart, nor naming clips any more.
                    *lock(&captured_out) = None;
                    (current, lost, failed) = (None, true, None);
                    // Nothing left to restart for: the next window is checked when captured.
                    (checked_monitor, window_hdr) = (None, None);
                    hdr_moved.store(false, Relaxed);
                }
                _ => {
                    release_held(&s);
                    if hdr && hdr_tries.failed(*id, !fresh.load(Relaxed)) {
                        give_up = lock(&captured_out).clone(); // set when this capture started
                    }
                    failed = Some((*id, Instant::now() + RETRY));
                    current = None;
                }
            }
        }
        let captured = current.as_ref().map(|(c, ..)| *c).or(failed.map(|(id, _)| id));
        let mut wanted = |p: &Program| match failed {
            Some((id, until)) if id == (p.pid, p.window) => Instant::now() >= until,
            _ => {
                // Started by the captured program, or by a listed launcher since this capture began
                // (launchers are never captured themselves). Only asked when it could matter.
                let mut launched = || {
                    let key = (p.pid, p.started, owner.as_ref().map(|o| (o.pid, o.started)), began);
                    if let Some((k, answer)) = launch_cache
                        && k == key
                    {
                        return answer;
                    }
                    // A failed snapshot isn't cached: asked again next time.
                    let Some((ppid, exe)) = foreground::parent(p.pid) else { return false };
                    let answer = owner.as_ref().is_some_and(|o| o.started(ppid, p.started))
                        || (game::is_launcher(&exe) && p.started >= began);
                    launch_cache = Some((key, answer));
                    answer
                };
                let other = captured.is_some_and(|(pid, _)| pid != p.pid);
                let browser = other && game::is_browser(&p.exe);
                let covers = !browser && wgc::covers_monitor(p.window);
                let launched = other && !browser && !covers && launched();
                target::should_capture(captured, (p.pid, p.window), covers, launched, browser)
            }
        };
        // Checked again: the window may have closed (and its handle gone to another program)
        // since the start.
        let carried = carried.take().filter(Program::window_open);
        // Once HDR is given up nothing more is captured: the pool is HDR, and the restart is due.
        if let Some(p) = carried.as_ref().or(game).filter(|p| !hdr_given_up && p.window != 0 && wanted(p)) {
            let end = Arc::new(AtomicU8::new(OK));
            let (shared, g, e) = (s.clone(), gpu.clone(), end.clone());
            let hwnd = windows::Win32::Foundation::HWND(p.window as _);
            sources += 1;
            let (source, fresh) = (sources, Arc::new(AtomicBool::new(true)));
            let f = fresh.clone();
            let on_frame = move |frame: windows::core::Result<Frame<'_>>| {
                let failure = match frame {
                    Ok(frame) => match shared.frame_arrived(&g, frame, size, fps, Some((source, &f))) {
                        Ok(()) => {
                            shared.window_frames.fetch_add(1, Relaxed);
                            return;
                        }
                        Err((_, raw)) => (FAILED, raw),
                    },
                    Err(err) if err.code() == wgc::ITEM_CLOSED => (CLOSED, err.to_string()),
                    Err(err) => (FAILED, err.to_string()),
                };
                if e.swap(failure.0, Relaxed) == OK && failure.0 == FAILED {
                    tracing::warn!("window capture stopped, trying again shortly: {}", failure.1);
                }
            };
            let started = if hdr {
                Capture::hdr_window(&gpu, hwnd, cursor, Some(min_interval), on_frame)
            } else {
                Capture::window(&gpu, hwnd, cursor, Some(min_interval), on_frame)
            };
            match started {
                Ok(capture) => {
                    tracing::info!("capturing the window {:#x} of {}", p.window, p.exe);
                    // A new window gets its own 2 s before it can count as exclusive full screen.
                    (last_frames, last_frame_at) = (s.window_frames.load(Relaxed), Instant::now());
                    // The old picture goes, unless this window's first frame already took its place.
                    let held = {
                        let mut input = lock(&s.input);
                        // The old window's capture is dropped next: its late frames are ignored now.
                        input.window_source = input.window_source.max(source);
                        let old = input.resized.take();
                        (if fresh.load(Relaxed) { input.held.take() } else { None }, old)
                    };
                    drop(held); // after the lock, as in `release_held`
                    (current, lost, failed) = (Some(((p.pid, p.window), capture, end, fresh)), false, None);
                    owner = Some(p.clone());
                    began = foreground::now();
                    *lock(&captured_out) = Some(p.clone());
                    checked_monitor = None;
                }
                Err(e) => {
                    let id = (p.pid, p.window);
                    if failed.is_none_or(|(f, _)| f != id) {
                        tracing::warn!("the window {:#x} of {} can't be captured yet: {e}", p.window, p.exe);
                    }
                    // A window not ready yet (a game still launching) isn't an HDR failure.
                    if hdr && wgc::window_ready(hwnd) && hdr_tries.failed(id, false) {
                        give_up = Some(p.clone());
                    }
                    failed = Some((id, Instant::now() + RETRY));
                    owner = Some(p.clone());
                }
            }
        }
        if let Some(p) = give_up
            && !std::mem::replace(&mut hdr_given_up, true)
        {
            tracing::warn!("HDR capture of the window of {} kept failing: restarting in standard video for it", p.exe);
            lock(&HDR_WINDOW_FAILED).push(p.exe.clone());
            *lock(&captured_out) = Some(p); // the restart captures it straight away
        }
        // A game on (or dragged to) the other kind of monitor asks for a restart (`monitor_moved`);
        // dragged back before it happens, the restart is called off. Never once HDR is given up.
        if hdr_given_up {
            hdr_moved.store(true, Relaxed);
        } else if hdr_wanted && let Some(((_, w), ..)) = &current {
            let monitor = wgc::window_monitor(*w);
            if checked_monitor != Some(monitor.0 as isize) {
                checked_monitor = Some(monitor.0 as isize);
                window_hdr = wgc::monitor_hdr_enabled(monitor)
                    .inspect_err(|e| tracing::warn!("couldn't tell whether the game's monitor shows HDR: {e}"))
                    .ok();
                // Unknown never restarts: the new start couldn't tell either.
                let restart = window_hdr.is_some_and(|h| target::window_hdr_restart(hdr_wanted, hdr_fallback, h));
                if hdr_moved.swap(restart, Relaxed) != restart {
                    tracing::info!(
                        "the game's monitor {} HDR: {}",
                        if window_hdr == Some(true) { "shows" } else { "doesn't show" },
                        if restart { "restarting for it once no recording runs" } else { "no restart needed now" }
                    );
                }
            }
        }
        let minimized = current.as_ref().is_some_and(|((_, w), ..)| wgc::is_minimized(*w));
        let frames = s.window_frames.load(Relaxed);
        // A minimized window sends nothing either; its quiet time starts once it is back.
        if frames != last_frames || current.is_none() || minimized {
            (last_frames, last_frame_at) = (frames, Instant::now());
        }
        let exclusive = current.as_ref().is_some_and(|((_, w), ..)| {
            last_frame_at.elapsed() >= target::EXCLUSIVE_QUIET && wgc::exclusive_full_screen(*w)
        });
        // No HDR line while a restart for the game's monitor is due: it would be about to change
        // (while a recording holds the restart off, `ReplaySession::capture_notice` says so).
        let hdr_note =
            target::window_hdr_notice(hdr_wanted, hdr_fallback, window_hdr).filter(|_| !hdr_moved.load(Relaxed));
        *lock(&notice) = match current {
            None if hdr_given_up => Some(target::HDR_WINDOW_FAILING),
            _ => target::window_notice(current.is_some(), lost, minimized, exclusive, hdr_note),
        };
    }
}

/// Programs whose HDR window capture kept failing (`follow_window`): later sessions record them in
/// standard video until Backsight restarts.
static HDR_WINDOW_FAILED: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn hdr_window_failed(exe: &str) -> bool {
    lock(&HDR_WINDOW_FAILED).iter().any(|e| e.eq_ignore_ascii_case(exe))
}

/// Gives back the frame the video thread hasn't converted yet, before its window capture (and frame
/// pool) is dropped: Microsoft doesn't say a frame stays usable after its pool closes.
fn release_held(s: &Shared) {
    let held = {
        let mut input = lock(&s.input);
        (input.held.take(), input.resized.take())
    };
    drop(held); // after the lock: never release a frame while holding what the capture callback waits on
}

/// Muxes `packets` (from [`ReplayBuffer::clip`]) into `path` through a `.part` file.
pub fn write_clip(
    path: &Path,
    container: &Container,
    video: &StreamParams,
    audio: Option<&StreamParams>,
    packets: &[Packet],
) -> backsight_encode::session::Result<PathBuf> {
    finish_clip(Muxer::create(path, container, video, audio.as_slice())?, packets)
}

fn open_named_mux(
    folder: &Path,
    name: &str,
    container: &Container,
    video: &StreamParams,
    audio: &[StreamParams],
    recording: bool,
) -> backsight_encode::session::Result<Muxer> {
    // Replay saves and recording splits can open files at the same instant. The .part
    // reserves the name before the lock is released, so packet writing can run independently.
    static OUTPUT_NAME: Mutex<()> = Mutex::new(());
    let _reserve = lock(&OUTPUT_NAME);
    let path = clip_path(folder, name, &container.ext, &system::local_time_stamp());
    let audio: Vec<_> = audio.iter().collect();
    if recording {
        // Named so that one left behind by a crash can be recovered (`recovery`).
        Muxer::create_recording(&path, container, video, &audio)
    } else {
        Muxer::create(&path, container, video, &audio)
    }
}

fn finish_clip(mut mux: Muxer, packets: &[Packet]) -> backsight_encode::session::Result<PathBuf> {
    for p in packets {
        mux.write(
            track(p.stream),
            &Encoded {
                data: &p.data,
                pts_us: p.pts_us,
                dts_us: p.dts_us,
                duration_us: p.duration_us,
                keyframe: p.keyframe,
            },
        )?;
    }
    mux.finish()
}

pub(crate) fn track(stream: StreamKind) -> Track {
    match stream {
        StreamKind::Video => Track::Video,
        StreamKind::Audio(i) => Track::Audio(i.into()),
    }
}

/// The encoded size for the `resolution` setting: `"source"`, or a height such as `"1080p"`
/// (never upscaled), keeping the aspect ratio. NV12 needs even sizes.
pub fn output_size(source: (u32, u32), resolution: &str) -> (u32, u32) {
    // "1080p" means the short side, so a portrait screen keeps its full width.
    let short = source.0.min(source.1);
    let target = resolution.strip_suffix('p').and_then(|h| h.parse::<u32>().ok()).filter(|&h| h < short);
    let scale = |side: u32, t: u32| (u64::from(side) * u64::from(t) / u64::from(short)) as u32;
    let (w, h) = match target {
        Some(t) => (scale(source.0, t), scale(source.1, t)),
        None => source,
    };
    (w & !1, h & !1)
}

/// The replay buffer's RAM ceiling: the lesser of 2 GB and a quarter of installed memory.
fn replay_ceiling(installed: Option<u64>) -> usize {
    const MAX: u64 = 2 << 30;
    installed.map_or(MAX, |ram| MAX.min(ram / 4)) as usize
}

/// `<folder>\<name> <stamp>.<ext>`, adding ` (2)`, ` (3)` … when that name is taken, so a
/// second save in the same second never replaces the first.
fn clip_path(folder: &Path, name: &str, ext: &str, stamp: &str) -> PathBuf {
    free_name(folder, &format!("{name} {stamp}"), ext, None)
}

/// The engine clock (QPC) in microseconds, the unit packet timestamps use.
fn engine_now_us() -> i64 {
    (wgc::now() / 10) as i64
}

/// Opens the chosen video encoder once and closes it again. The first open in a process is the
/// slow one (6 to 9 s for NVENC on an Optimus laptop, see platform-notes.md). Done once after
/// startup when Instant Replay is off, so the first press of the record key doesn't pay for it.
/// Nothing stays open: the graphics card goes back to sleep and idle use stays at zero.
// shortcut: warms only the encoder chosen at startup, and can't keep a sleeping card awake; if
// "replay starting" still shows seconds for the video encoder after "encoder warmed up", open the
// device when a game comes to the front instead.
pub fn warm_up(settings: &Settings) {
    let began = Instant::now();
    let c = Catalog::get();
    let Some(venc) = c.video_encoder(&settings.video.encoder) else { return };
    let values = resolve(&c.video_options(venc), settings.video.encoder_options.get(&venc.id));
    let (size, fps) = ((1280, 720), settings.video.fps);
    let opened = if VideoSession::takes_d3d11(venc) {
        Gpu::open(&venc.vendor).map_err(|e| e.to_string()).and_then(|gpu| {
            VideoSession::open_d3d11(venc, &values, size, fps, false, gpu.for_ffmpeg())
                .map(drop)
                .map_err(|e| e.to_string())
        })
    } else {
        VideoSession::open(venc, &values, size.0, size.1, fps, false).map(drop).map_err(|e| e.to_string())
    };
    match opened {
        Ok(()) => tracing::info!("encoder warmed up: {} opened and closed in {:.1?}", venc.id, began.elapsed()),
        Err(e) => {
            tracing::info!("encoder warm-up skipped: {} didn't open after {:.1?} ({e})", venc.id, began.elapsed())
        }
    }
}

/// The encoder opened on GPU textures, or, when that fails, `system_memory()`'s session and
/// downloader: the encoder probe only checked system memory (an AMD driver may take one and
/// not the other). Generic so the choice is tested without a GPU.
fn or_system_memory<V, D, E: Display>(
    gpu: Result<V, E>,
    system_memory: impl FnOnce() -> Result<(Result<V, E>, Option<D>), String>,
) -> Result<(Result<V, E>, Option<D>), String> {
    match gpu {
        Ok(video) => Ok((Ok(video), None)),
        Err(e) => {
            tracing::warn!("the encoder didn't open on GPU textures ({e}); feeding it from system memory");
            system_memory()
        }
    }
}

/// Logs the raw detail and returns the plain-language message for the UI.
pub(crate) fn user_error(user: &str, raw: &dyn Display) -> String {
    tracing::error!("{user} ({raw})");
    user.to_owned()
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_still_screen_is_encoded_a_few_times_a_second() {
        let (s, slot) = (1_000_000, 1_000_000 / 60);
        assert_eq!(still_slot(0, 0, 0, slot, false, 2 * s), Still::Encode, "a picture just changed");
        assert_eq!(still_slot(0, 6 * slot, 0, 7 * slot, false, 2 * s), Still::Rest, "still for over 0.1 s");
        assert_eq!(still_slot(0, 6 * slot, 0, 7 * slot, true, 2 * s), Still::Encode, "a recording starts");
        assert_eq!(still_slot(0, 2 * s - slot, 0, 2 * s, false, 2 * s), Still::Keyframe, "a keyframe is due");
        // A game drawing 30 pictures a second into 60 FPS video never rests the encoder.
        assert!(
            (1..600)
                .all(|n| still_slot((n - n % 2) * slot, (n - 1) * slot, 0, n * slot, false, 2 * s) == Still::Encode)
        );
        // A still screen for 10 s at 60 FPS and at 30: about 15 pictures a second, a keyframe every
        // 2 s, and never 0.1 s without a picture (what the real-GPU tests allow).
        for fps in [60, 30] {
            let (mut sent, mut key, mut count, mut keys, mut gap) = (0, 0, 0, 0, 0);
            for n in 1..10 * fps {
                let at = n * s / fps;
                match still_slot(0, sent, key, at, false, 2 * s) {
                    Still::Rest => continue,
                    Still::Keyframe => (key, keys) = (at, keys + 1),
                    Still::Encode => {}
                }
                (gap, sent, count) = (gap.max(at - sent), at, count + 1);
            }
            assert!((140..=160).contains(&count) && keys == 4, "{fps} FPS: {count} pictures, {keys} keyframes");
            assert!(gap < s / 10, "{fps} FPS: {gap} us");
        }
    }

    #[test]
    fn an_encoder_that_fails_on_gpu_textures_falls_back_to_system_memory() {
        let system = || Ok((Ok::<_, String>("system"), Some("downloader")));
        let (video, download) = or_system_memory(Err("AMF: unsupported surface".to_owned()), system).unwrap();
        assert_eq!((video, download), (Ok("system"), Some("downloader")));
        // Zero copy when it opens: no download step, and system memory isn't tried.
        let (video, download) = or_system_memory(Ok::<_, String>("gpu"), || -> Result<(_, Option<()>), _> {
            panic!("system memory opened too")
        })
        .unwrap();
        assert_eq!((video, download), (Ok("gpu"), None));
        // Both fail: the system-memory error is the one reported.
        let (video, _) =
            or_system_memory(Err("gpu".to_owned()), || Ok((Err::<(), _>("system".to_owned()), Some(())))).unwrap();
        assert_eq!(video, Err("system".to_owned()));
    }

    #[test]
    fn every_content_resolution_is_understood() {
        // Anything output_size can't parse silently records at source size.
        for ch in &Catalog::get().resolutions {
            let v = ch.value.as_str().expect("resolution values are strings");
            assert!(v == "source" || output_size((7680, 4320), v) != (7680, 4320), "{v} is not understood");
        }
    }

    #[test]
    #[ignore = "needs an NVIDIA GPU and an unlocked desktop"]
    fn keeps_pace_converting_at_slot_time_and_refuses_clips_after_a_failure() {
        let dir = std::env::temp_dir().join(format!("backsight-failed-{}", std::process::id()));
        let mut settings = Settings::default();
        settings.general.video_folder = dir.to_string_lossy().into_owned();
        settings.audio.desktop = false;
        let session = ReplaySession::start(&settings).unwrap();
        std::thread::sleep(Duration::from_secs(3));
        // Frames are converted on the video thread at their slot; every slot still gets encoded.
        let d = session.diagnostics();
        assert!(d.encoded_frames >= u64::from(settings.video.fps) * 5 / 2, "{d:?}");
        assert_eq!(session.error(), None);
        session.save_clip().unwrap();

        session.shared.fail(CAPTURE_LOST, &"test");
        assert_eq!(session.prepare_clip().err().as_deref(), Some(REPLAY_STOPPED), "no old footage after a failure");
        drop(session);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn output_size_keeps_aspect_never_upscales_and_stays_even() {
        assert_eq!(output_size((2560, 1440), "source"), (2560, 1440));
        assert_eq!(output_size((2560, 1440), "1080p"), (1920, 1080));
        assert_eq!(output_size((1366, 768), "720p"), (1280, 720));
        assert_eq!(output_size((1920, 1080), "1440p"), (1920, 1080), "no upscaling");
        assert_eq!(output_size((1365, 767), "source"), (1364, 766), "NV12 needs even sizes");
        assert_eq!(output_size((1080, 1920), "1080p"), (1080, 1920), "portrait 1080p stays as is");
        assert_eq!(output_size((1440, 2560), "1080p"), (1080, 1920), "portrait scales by its short side");
    }

    #[test]
    fn a_kept_clip_is_trimmed_from_a_keyframe() {
        let packet = |stream, s: f64, keyframe| Packet {
            stream,
            pts_us: (s * 1e6) as i64,
            dts_us: (s * 1e6) as i64,
            duration_us: 0,
            keyframe,
            data: vec![0; 10].into(),
        };
        // Video each second (keyframes at 0, 4 and 8 s), each after audio for half a second later:
        // the encoders' delays mix the order.
        let packets = (0..10)
            .flat_map(|t| {
                let t = f64::from(t);
                [packet(StreamKind::Audio(0), t + 0.5, false), packet(StreamKind::Video, t, t % 4.0 == 0.0)]
            })
            .collect();
        let mut clip = Clip::new(packets, |p| Ok(PathBuf::from(p.len().to_string())));
        clip.keep_last(Duration::from_secs(20));
        assert_eq!(clip.bytes(), 200, "longer than it is: all of it");
        clip.keep_last(Duration::from_secs(3));
        let first = &clip.packets[0];
        assert!(first.stream == StreamKind::Video && first.keyframe && first.pts_us == 4_000_000, "{first:?}");
        assert!(clip.packets.iter().all(|p| p.pts_us >= 4_000_000), "nothing from before the keyframe");
        assert_eq!(clip.packets.iter().filter(|p| p.stream == StreamKind::Video).count(), 6);
        assert_eq!(clip.bytes(), 120, "6 video packets and the audio from 4.5 s on");
        assert_eq!(clip.save(), Ok(PathBuf::from("12")));
    }

    #[test]
    fn replay_ceiling_is_two_gb_or_a_quarter_of_ram() {
        assert_eq!(replay_ceiling(Some(16 << 30)), 2 << 30);
        assert_eq!(replay_ceiling(Some(4 << 30)), 1 << 30);
        assert_eq!(replay_ceiling(None), 2 << 30);
    }

    #[test]
    fn clip_path_never_reuses_a_taken_name() {
        let dir = std::env::temp_dir().join(format!("backsight-names-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let first = clip_path(&dir, "Backsight", "mp4", "2026-11-08 14.03.07");
        assert_eq!(first.file_name().unwrap(), "Backsight 2026-11-08 14.03.07.mp4");
        fs::write(&first, b"x").unwrap();
        let second = clip_path(&dir, "Backsight", "mp4", "2026-11-08 14.03.07");
        assert_eq!(second.file_name().unwrap(), "Backsight 2026-11-08 14.03.07 (2).mp4");
        fs::write(dir.join("Backsight 2026-11-08 14.03.07 (2).mp4.part"), b"x").unwrap();
        let third = clip_path(&dir, "Backsight", "mp4", "2026-11-08 14.03.07");
        assert_eq!(third.file_name().unwrap(), "Backsight 2026-11-08 14.03.07 (3).mp4", "a save in progress");
        fs::write(dir.join("Backsight 2026-11-08 14.03.07 (3).mp4.recording.part"), b"x").unwrap();
        let fourth = clip_path(&dir, "Backsight", "mp4", "2026-11-08 14.03.07");
        assert_eq!(fourth.file_name().unwrap(), "Backsight 2026-11-08 14.03.07 (4).mp4", "a recording in progress");
        let game = clip_path(&dir, "Tomb Raider", "mp4", "2026-11-08 14.03.07");
        assert_eq!(game.file_name().unwrap(), "Tomb Raider 2026-11-08 14.03.07.mp4");
        fs::remove_dir_all(dir).unwrap();
    }
}
