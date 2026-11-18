//! The Backsight desktop shell: tray icon, the settings window and the UI bridge.
//!
//! The window is disposable. Closing it destroys the WebView to free its memory,
//! while the tray icon (and, from M1, the engine) keeps running.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod captions;
mod cli;
mod probe_child;
mod report;
mod respond;
mod setup;
mod uiwatch;
mod update;
mod webview_check;

use backsight_config::{Action, Hotkey, Settings};
use backsight_engine::{
    hotkeys::Event,
    input::raw_input::Hotkeys,
    library::{self, Library},
    screenshot::{self, Frozen, Saved},
    session::{Clip, ReplaySession},
    sounds::{self, Cue},
    vertical::{self, Shape},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Mutex, MutexGuard, PoisonError, TryLockError,
        atomic::{AtomicBool, Ordering::Relaxed},
    },
};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, RunEvent, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder, WindowEvent,
    async_runtime::{self, Sender, spawn_blocking},
    image::Image,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

/// The engine lives here, not in the (disposable) window.
#[derive(Default)]
struct Engine {
    /// Held by whatever starts, stops or replaces the session (applying saved settings, the record
    /// and replay keys, the watcher's restarts, startup, quitting): one at a time, for seconds while
    /// an encoder opens. Nothing the user waits on takes it: saving settings and the hotkeys hand
    /// their work to a thread that does. Hotkey callbacks never take it: `start_hotkeys` joins them.
    apply: Mutex<()>,
    /// Held for moments only, never while a session opens (see `Replay::starting`).
    replay: Mutex<Replay>,
    /// Held while the settings file is read and written, so saves reach `to_apply` in file order.
    settings_file: Mutex<()>,
    /// The newest saved settings the engine doesn't run on yet (`apply_saved`).
    to_apply: Mutex<respond::Latest<Settings>>,
    /// The settings the engine was last set up with; a save restarts only what differs from them.
    applied: Mutex<Option<Settings>>,
    /// One press of the record key, or of the replay toggle, is carried out at a time.
    record_gate: respond::Gate,
    replay_gate: respond::Gate,
    /// The record press being carried out is starting a recording (not stopping or restarting).
    record_starting: AtomicBool,
    /// Problem pop-ups shown lately, so a repeated press doesn't stack the same card.
    shown: Mutex<respond::Shown>,
    /// `quit` was asked for: the tray icon and windows are gone, a worker is finishing up.
    quit_started: AtomicBool,
    /// `shut_down` ran (or is running, while this is held).
    shut_down: Mutex<bool>,
    mic_key: Mutex<MicKey>,
    // Dropping `Hotkeys` joins its thread, whose callback can take `mic_key`.
    hotkeys: Mutex<Option<Hotkeys>>,
    /// Returned by `get_status` while `replay` is busy (starting takes seconds).
    last_status: Mutex<Option<Status>>,
    /// A `get_status` / `get_diagnostics` call is running: the next poll doesn't wait behind it.
    status_busy: AtomicBool,
    diagnostics_busy: AtomicBool,
    screenshot_busy: AtomicBool,
    /// The frozen monitor while the region picker is open.
    region: Mutex<Option<Frozen>>,
    /// The last window destroyed was the picker or the hud: closing it must not quit Backsight.
    helper_closed_last: AtomicBool,
    /// The pop-ups and recording dot are being moved (`hud_arrange`).
    arranging: AtomicBool,
    /// The pop-ups (0) and dot (1) were dragged since moving started.
    dragged: [AtomicBool; 2],
    /// Pop-ups waiting for the hud window to take them (`hud_poll`).
    hud_queue: Mutex<Vec<Notice>>,
    /// The tray icon shows a recording.
    tray_recording: AtomicBool,
    replay_save_busy: AtomicBool,
    quitting: AtomicBool,
    /// Files still being finished (stopped recordings, clips, recovery); quitting waits for them.
    finishing: Mutex<Finishing>,
    /// The running hotkey matcher holds the push-to-talk key.
    ptt_live: AtomicBool,
    /// Why the hotkeys couldn't start; shown on Home until a start works.
    hotkey_problem: Mutex<Option<String>>,
    /// When the picker window was built; it stays hidden while its frozen frame loads.
    picker_opened: Mutex<Option<std::time::Instant>>,
    /// The running recording's stop-on-its-own reason was already notified.
    recording_problem_told: AtomicBool,
    /// A hotkey box is listening for a new combo: presses go to it, not to the live hotkeys.
    hotkeys_paused: AtomicBool,
    /// The recording dot is turned off in settings. Kept here so the polls don't read the file.
    hide_indicator: AtomicBool,
}

#[derive(Default)]
struct Finishing {
    threads: Vec<std::thread::JoinHandle<()>>,
    /// Quitting took the list: nothing new is started, since nobody would wait for it.
    closed: bool,
}

/// Runs `work` on a thread that quitting waits for. Returns `false`, without running it, once
/// quitting has taken the list.
fn spawn_finishing(engine: &Engine, work: impl FnOnce() + Send + 'static) -> bool {
    let mut finishing = lock(&engine.finishing);
    if finishing.closed {
        return false;
    }
    finishing.threads.retain(|t| !t.is_finished());
    finishing.threads.push(std::thread::spawn(work));
    true
}

#[derive(Default)]
struct Replay {
    session: Option<ReplaySession>,
    /// A session is being opened by whoever holds `apply`; `session` is empty meanwhile.
    starting: bool,
    /// Why the last start failed. A running session reports its own failure through `error()`.
    start_error: Option<String>,
    /// Turned on or off with the toggle hotkey or tray: other settings changes keep that choice.
    // shortcut: the toggle lasts until restart; it doesn't change `replay.enabled`
    toggled: Option<bool>,
    /// Settings changed while recording; applied when the recording stops, so it isn't cut.
    pending_apply: bool,
    /// Replacing the matcher would lose release edges for the recording's active binding.
    pending_hotkeys: bool,
    mic_muted: bool,
    /// The running session records the mic in push-to-talk mode.
    ptt: bool,
    /// The replay from before each move to another monitor (or, window mode, to the other kind
    /// of monitor for HDR), which starts a new session: the next clip saved saves these too.
    before_move: Vec<Kept>,
    /// Counts the sessions started, so a kept clip of the one still running isn't saved twice.
    session_number: u64,
}

/// A clip snapshot kept from before a move (`Replay::before_move`).
struct Kept {
    /// When it moved, and the replay length then.
    at: std::time::Instant,
    length: std::time::Duration,
    /// `Replay::session_number` of the session it was taken from.
    from: u64,
    clip: Clip,
}

/// Clips kept from before moves, at most: a monitor that keeps sleeping and waking would
/// otherwise add a pile of short files to every clip saved.
const KEPT_CLIPS: usize = 2;

/// Trims the clips from before a move to what a clip saved at `now` holds, so together with the
/// running replay they never hold more than the replay length, nor more RAM than its buffer may
/// still take (`room`, [`ReplaySession::replay_room`]). The oldest is trimmed from its start first,
/// a keyframe at a time, and goes only when not even its last part fits.
fn keep_recent(before_move: &mut Vec<Kept>, now: std::time::Instant, room: Option<usize>) {
    before_move.retain_mut(|k| {
        let left = k.length.saturating_sub(now.saturating_duration_since(k.at));
        k.clip.keep_last(left);
        !left.is_zero()
    });
    let mut bytes: usize = before_move.iter().map(|k| k.clip.bytes()).sum();
    while before_move.len() > KEPT_CLIPS {
        bytes -= before_move.remove(0).clip.bytes();
    }
    while let Some(r) = room.filter(|&r| bytes > r && !before_move.is_empty()) {
        let first = &mut before_move[0].clip;
        let had = first.bytes();
        let trimmed = had > bytes - r && first.keep_bytes(had - (bytes - r)) && first.bytes() < had;
        bytes -= if trimmed { had - first.bytes() } else { before_move.remove(0).clip.bytes() };
    }
}

/// Takes the kept clips for a save. One kept from the session still `running` (its number), saved
/// while the move is under way, is in that session's own clip already.
fn kept_to_save(before_move: &mut Vec<Kept>, running: Option<u64>) -> Vec<Clip> {
    std::mem::take(before_move).into_iter().filter(|k| Some(k.from) != running).map(|k| k.clip).collect()
}

#[derive(Default)]
struct MicKey {
    down: bool,
    control: Option<Box<dyn Fn(bool) + Send>>,
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the Home page shows. The UI polls it once a second.
#[derive(Serialize, Clone)]
struct Status {
    /// "on", "off" or "failed".
    state: &'static str,
    message: String,
    buffer_seconds: f64,
    buffer_bytes: usize,
    dropped_frames: u64,
    late_frames: u64,
    recording: bool,
    recording_seconds: f64,
    recording_paused: bool,
    /// Why the recording stopped on its own; empty otherwise.
    recording_message: String,
    pending_apply: bool,
    audio_problem: Option<String>,
    /// The mic is recorded but muted with the hotkey.
    mic_muted: bool,
    /// The program whose sound game-only audio records, e.g. `game.exe`.
    game_audio_from: Option<String>,
    /// A missing monitor, or window capture waiting for or losing the game's window.
    capture_notice: Option<&'static str>,
    /// The hotkeys couldn't start, so pressing them does nothing.
    hotkey_problem: Option<String>,
    /// The game Backsight is watching, as files are filed under it.
    game: Option<String>,
    /// A newer version is ready to install (ADR 0014).
    update: Option<String>,
}

/// Replaces the session with a new one. The caller holds `apply` and not `replay`: opening the
/// encoder takes seconds, and the hotkeys, the recording dot and Home must not wait for it.
fn start_replay(engine: &Engine, settings: &Settings, preserve_target: bool) {
    let (old, muted, target, window) = {
        let mut replay = lock(&engine.replay);
        let target = replay
            .session
            .as_ref()
            .filter(|_| preserve_target && settings.audio.desktop && settings.audio.game_audio_only)
            .and_then(ReplaySession::game_audio_target);
        // Window mode captures the same game again straight away, if its window is still open.
        let window = replay.session.as_ref().and_then(ReplaySession::captured_window);
        lock(&engine.mic_key).control = None;
        replay.start_error = None;
        replay.starting = true;
        (replay.session.take(), replay.mic_muted, target, window)
    };
    drop(old); // two captures would fight over the GPU
    // Start with PTT closed. A release may arrive while the capture is starting.
    let started = ReplaySession::start_with_audio_state(settings, muted, false, target, window);
    let mut replay = lock(&engine.replay);
    replay.starting = false;
    match started {
        Ok(s) => {
            if replay.mic_muted != muted {
                s.set_mic_muted(replay.mic_muted); // the mute key was pressed while it opened
            }
            let mut key = lock(&engine.mic_key);
            if let Some(control) = s.mic_key_control() {
                control(key.down);
                key.control = Some(Box::new(control));
            }
            replay.session = Some(s);
            replay.session_number += 1;
            replay.ptt = settings.audio.microphone && push_to_talk(settings);
        }
        Err(e) => {
            tracing::error!("Instant Replay did not start: {e}");
            replay.start_error = Some(e);
        }
    }
}

fn stop_replay(engine: &Engine, replay: &mut Replay) {
    lock(&engine.mic_key).control = None;
    replay.session = None;
    replay.start_error = None;
    replay.before_move.clear(); // turned off: the replay goes, as it does for the running session
}

pub(crate) fn recording(replay: &Replay) -> bool {
    replay.session.as_ref().is_some_and(|s| s.recording_time().is_some())
}

/// Nothing to lose by restarting now: Setup is finished, nothing records and no game is in front
/// (an update found at start installs only then).
pub(crate) fn idle_for_update(app: &AppHandle) -> bool {
    let setup_done = Settings::load().is_ok_and(|s| s.setup.done);
    let engine = app.state::<Engine>();
    let replay = lock(&engine.replay);
    setup_done && !recording(&replay) && replay.session.as_ref().is_none_or(|s| s.game_name().is_none())
}

/// Runs replay the way `settings` and the toggle hotkey say. The caller holds `apply`, so no
/// recording starts between the check and the restart.
fn apply_replay(engine: &Engine, settings: &Settings) {
    let mut replay = lock(&engine.replay);
    if recording(&replay) {
        replay.pending_apply = true; // restarting now would cut the recording
        return;
    }
    replay.pending_apply = false;
    if replay.toggled.unwrap_or(settings.replay.enabled) {
        drop(replay);
        start_replay(engine, settings, true);
    } else {
        stop_replay(engine, &mut replay);
    }
}

/// A "saved" pop-up's body: where to watch it, then the file name.
fn in_library(name: &str) -> String {
    format!("Watch it in Backsight's Library. ({name})")
}

/// The pop-up for a save press: `now` is the running replay's clip, `earlier` the ones kept from
/// before a monitor change (`Replay::before_move`). `making`: a vertical clip is being made of it.
fn saved_notice(
    earlier: &[Result<PathBuf, String>],
    now: Result<PathBuf, String>,
    making: bool,
) -> (&'static str, String) {
    let name = |p: &PathBuf| in_library(&p.file_name().unwrap_or_default().to_string_lossy());
    let kept = earlier.iter().rev().find_map(|r| r.as_ref().ok());
    let lost = earlier.iter().rev().find_map(|r| r.as_ref().err());
    let (title, body) = match (now, kept) {
        (Ok(path), None) => ("Clip saved", name(&path)),
        (Ok(path), Some(_)) => (
            "Clips saved",
            format!(
                "{} Instant Replay restarted when your monitor changed, so what came before is a separate clip.",
                name(&path)
            ),
        ),
        (Err(_), Some(path)) => {
            ("Clip saved", format!("{} It's what Instant Replay had before your monitor changed.", name(path)))
        }
        // A kept clip's error is the real one: the new session's is usually that it is starting.
        (Err(e), None) => return ("Couldn't save the clip", lost.cloned().unwrap_or(e)),
    };
    // The wide file is about to become the vertical clip, so "watch it" would be early.
    let body = if making { "Making the vertical clip now.".to_owned() } else { body };
    match lost {
        Some(e) => (title, format!("{body} The part from before your monitor changed wasn't saved. {e}")),
        None => (title, body),
    }
}

/// The notification for a finished recording, and every file it saved.
fn stopped_notice(stopped: backsight_engine::session::Stopped) -> (&'static str, String, Vec<PathBuf>) {
    let name = |p: &std::path::Path| p.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let mut parts = stopped.earlier_parts;
    let parts_note = |n: usize| if n > 0 { format!(" and {n} earlier part(s)") } else { String::new() };
    let unfinished = backsight_engine::recording::unfinished_note(&stopped.unfinished_parts);
    let (title, body, parts) = match (stopped.file, stopped.stopped_early) {
        (Ok(path), Some(why)) => {
            let body = format!("{why} ({}{})", name(&path), parts_note(parts.len()));
            parts.push(path);
            ("Recording stopped early", body, parts)
        }
        (Ok(path), None) => {
            let files = format!("{}{}", name(&path), parts_note(parts.len()));
            parts.push(path);
            let (title, body) = if unfinished.is_some() {
                ("Part of the recording wasn't finished", files)
            } else {
                ("Recording saved", in_library(&files))
            };
            (title, body, parts)
        }
        (Err(e), _) if parts.is_empty() => ("Couldn't save the recording", e, parts),
        (Err(e), _) => ("Couldn't save the last part", format!("{e} (earlier parts are saved)"), parts),
    };
    let markers = stopped.markers_lost.then_some(backsight_engine::recording::MARKERS_LOST);
    let body = unfinished.as_deref().into_iter().chain(markers).chain([body.as_str()]).collect::<Vec<_>>().join(" ");
    (title, body, parts)
}

/// A recording that stopped on its own and the user was told. Not told yet (the watcher checks
/// once a second): the press means "stop", nothing more.
fn recording_stopped_itself(recording: bool, failed: bool, told: bool) -> bool {
    recording && failed && told
}

/// Shown the moment the record key is heard, when the recording can't start at once.
const STARTING: (&str, &str) =
    ("Starting the recording", "This takes a few seconds. You'll hear the sound again when it's recording.");

/// The record key: starts or stops a recording. A recording that already stopped on its own is
/// finished and a new one started, in the same press. On its own thread, with `pass` keeping it
/// to one press at a time: `apply` may be busy for seconds, and so may opening a session.
fn toggle_recording(app: &AppHandle, _pass: respond::Pass) {
    let engine = app.state::<Engine>();
    // Before waiting for `apply`: the key was heard, even if the recording takes a while to start.
    let cued = {
        let replay = lock(&engine.replay);
        !recording(&replay) && (replay.starting || replay.session.is_none())
    };
    if cued {
        notify(app, STARTING.0, STARTING.1);
    }
    let apply = lock(&engine.apply);
    if engine.quitting.load(Relaxed) {
        return;
    }
    let mut replay = lock(&engine.replay);
    let stopped_itself = replay.session.as_ref().is_some_and(|s| {
        recording_stopped_itself(
            s.recording_time().is_some(),
            s.recording_error().is_some() || s.error().is_some(),
            engine.recording_problem_told.load(Relaxed),
        )
    });
    if !stopped_itself && respond::just_started(replay.session.as_ref().and_then(ReplaySession::recording_time)) {
        drop(replay);
        drop(apply);
        notify(app, "Already recording", "Press again to stop.");
        return;
    }
    let stopping = replay.session.as_mut().and_then(ReplaySession::begin_stop_recording);
    let stopped = stopping.is_some();
    let pending = stopped && std::mem::take(&mut replay.pending_apply);
    let replay_off = replay.session.as_ref().is_some_and(|s| !s.replay_on());
    drop(replay);
    if let Some(stopping) = stopping {
        // On its own thread: with markers, finishing copies the whole file. Registered while
        // `apply` is held, so a Quit right after waits for it; it can't be refused, as quitting
        // is checked above under `apply`, which quitting takes before closing the list.
        if !spawn_finishing(&engine, finish_recording(app.clone(), stopping)) {
            tracing::warn!("a stopped recording wasn't finished: Backsight was already quitting");
        }
        // The dot goes before a restart that takes seconds. On another thread: the tray waits for
        // the main thread, which waits for `apply` while Windows signs out (`shut_down`).
        refresh_indicators_soon(app);
        // Settings saved meanwhile, or replay off (the pipeline only ran for the recording).
        match pending.then(Settings::load) {
            Some(Ok(settings)) => apply_replay(&engine, &settings),
            _ if replay_off => stop_replay(&engine, &mut lock(&engine.replay)),
            _ => {}
        }
    }
    let notice = (!stopped || stopped_itself).then(|| {
        engine.record_starting.store(true, Relaxed);
        let notice = start_recording(app, cued);
        engine.record_starting.store(false, Relaxed);
        notice
    });
    engine.recording_problem_told.store(false, Relaxed);
    let mut replay = lock(&engine.replay);
    let apply_hotkeys = !recording(&replay) && std::mem::take(&mut replay.pending_hotkeys);
    drop(replay);
    if apply_hotkeys && let Ok(settings) = Settings::load() {
        start_hotkeys(app, &settings);
    }
    drop(apply);
    if let Some((title, body)) = notice {
        notify(app, title, &body);
    }
    refresh_indicators(app);
}

/// Starts a recording; returns its notice. The caller holds `apply`. `cued`: the user was
/// already told it is starting.
fn start_recording(app: &AppHandle, cued: bool) -> (&'static str, String) {
    let engine = app.state::<Engine>();
    let settings = match Settings::load() {
        Ok(s) => s,
        Err(e) => return ("Couldn't start recording", format!("Backsight couldn't read your settings: {e}")),
    };
    let mut replay = lock(&engine.replay);
    let replay_wanted = replay.toggled.unwrap_or(settings.replay.enabled);
    if replay.session.as_ref().is_some_and(|s| s.error().is_some()) {
        stop_replay(&engine, &mut replay); // a failed session records nothing: start a fresh one
    }
    let fresh = replay.session.is_none();
    drop(replay);
    if fresh {
        if !cued {
            notify(app, STARTING.0, STARTING.1);
        }
        start_replay(&engine, &settings, false);
    }
    let mut replay = lock(&engine.replay);
    if fresh
        && !replay_wanted
        && let Some(s) = &replay.session
    {
        s.set_replay(false);
    }
    let started = match replay.session.as_mut() {
        Some(s) => {
            s.set_split(std::time::Duration::from_secs(u64::from(settings.video.split_minutes) * 60));
            s.start_recording()
        }
        None => Err(replay.start_error.clone().unwrap_or_default()),
    };
    match started {
        Ok(()) => {
            let key = settings.hotkeys.get(&Action::ToggleRecording).filter(|k| !k.is_empty());
            (
                "Recording started",
                key.map_or("Press the record hotkey again to stop.".into(), |k| format!("Press {k} again to stop.")),
            )
        }
        Err(e) => {
            if !replay_wanted {
                stop_replay(&engine, &mut replay); // nothing should keep running for a recording that didn't start
            }
            ("Couldn't start recording", e)
        }
    }
}

/// Saves what plays of recordings cut off by a crash or power loss. The unfinished originals
/// are kept, renamed so they aren't offered again (ADR 0008).
fn recover_recordings(app: &AppHandle, folder: &std::path::Path) {
    let found = backsight_engine::recovery::find_interrupted(folder);
    if found.is_empty() {
        return;
    }
    let (mut saved, mut failed) = (Vec::new(), None);
    for f in &found {
        match backsight_engine::recovery::recover(&f.path, Some(&app.state::<Engine>().quitting)) {
            Ok(rec) => saved.push(rec),
            // Quitting: the original is recovered at the next start, so nothing to tell.
            Err(e) if e == backsight_encode::remux::CANCELLED => return,
            Err(e) => {
                tracing::warn!("recovering {} failed: {e}", f.path.display());
                failed = Some(e);
            }
        }
    }
    if !saved.is_empty() {
        let _ = app.emit("library-changed", ());
        let kept = backsight_engine::recovery::kept_originals_note(&saved).unwrap_or_default();
        notify(
            app,
            "Unfinished recording saved",
            &format!(
                "Backsight saved {} recording(s) that were cut off. {} {kept}",
                saved.len(),
                backsight_engine::recovery::RECOVERED_NOTE
            ),
        );
    }
    if let Some(e) = failed {
        notify(app, "Couldn't save an unfinished recording", &e);
    }
}

/// One Backsight pop-up (ADR 0010).
#[derive(Serialize, Clone, Debug, PartialEq)]
struct Notice {
    /// Picks the pop-up's symbol and colour: see [`kind_of`].
    kind: &'static str,
    title: String,
    body: String,
}

/// The pop-up kind and sound for a notice, from its title (every title is written in this file).
fn kind_of(title: &str) -> (&'static str, Cue) {
    match title {
        "Recording started" | "Recording resumed" => ("record", Cue::Start),
        // A different sound from "started": heard at once, it mustn't sound like recording already.
        "Starting the recording" | "Still getting ready" | "Still finishing" | "Already recording" => {
            ("info", Cue::Marker)
        }
        "Recording saved" | "Clip saved" | "Unfinished recording saved" | "Vertical clip saved" => {
            ("saved", Cue::Saved)
        }
        "Screenshot saved" | "Screenshot saved; copy failed" => ("screenshot", Cue::Screenshot),
        "Recording paused" => ("pause", Cue::Pause),
        "Marker added" => ("marker", Cue::Marker),
        "Microphone on" => ("mic-on", Cue::MicOn),
        "Microphone muted" => ("mic-off", Cue::MicOff),
        "Not recording" | "Still saving the last clip" | "Instant Replay is back on" => ("info", Cue::Quiet),
        _ => ("problem", Cue::Problem),
    }
}

/// Tells the user what just happened: a Backsight pop-up and a sound, each unless turned off.
/// The same problem or reminder shown moments ago isn't stacked again (`respond::Shown`); its
/// sound still plays, so every press gets an answer.
fn notify(app: &AppHandle, title: &str, body: &str) {
    let (kind, cue) = kind_of(title);
    let engine = app.state::<Engine>();
    // Both default to on, and a settings error is exactly what to show.
    let settings = Settings::load().ok();
    if settings.as_ref().is_none_or(|s| s.general.sounds) {
        sounds::play(cue);
    }
    let repeat =
        matches!(kind, "problem" | "info") && lock(&engine.shown).repeat(title, body, std::time::Instant::now());
    if !repeat && settings.is_none_or(|s| s.general.notifications) {
        lock(&engine.hud_queue).push(Notice { kind, title: title.into(), body: body.into() });
        open_hud(app, HudPart::Popups);
    }
}

/// Backsight's two on-screen windows (ADR 0010): the pop-ups and the recording dot. Each can be
/// moved anywhere (`hud_arrange`) and opens where it was left.
#[derive(Clone, Copy, PartialEq, Debug)]
enum HudPart {
    Popups,
    Dot,
}

impl HudPart {
    const ALL: [Self; 2] = [Self::Popups, Self::Dot];

    fn label(self) -> &'static str {
        match self {
            Self::Popups => "hud",
            Self::Dot => "dot",
        }
    }

    /// Logical size of its window. The pop-ups' holds three cards.
    fn size(self) -> (f64, f64) {
        match self {
            Self::Popups => (360.0, 400.0),
            Self::Dot => (220.0, 48.0),
        }
    }

    fn saved(self, g: &backsight_config::General) -> Option<[f64; 2]> {
        match self {
            Self::Popups => g.popup_position,
            Self::Dot => g.dot_position,
        }
    }
}

/// Top-left of a hud window on a monitor (logical `x, y, width, height`), kept fully on it. A saved
/// spot is a share of the room the window can move in (0 = left/top edge, 1 = right/bottom edge),
/// so a spot by an edge stays by that edge on any screen size. By default: the top-right corner,
/// pop-ups under the dot.
fn hud_spot(part: HudPart, saved: Option<[f64; 2]>, monitor: (f64, f64, f64, f64)) -> (f64, f64) {
    const MARGIN: f64 = 16.0;
    let (mx, my, mw, mh) = monitor;
    let (w, h) = part.size();
    let (x, y) = match saved.filter(|s| s.iter().all(|v| v.is_finite())) {
        Some([fx, fy]) => (mx + fx * (mw - w), my + fy * (mh - h)),
        None if part == HudPart::Popups => (mx + mw - w - MARGIN, my + MARGIN + HudPart::Dot.size().1),
        None => (mx + mw - w - MARGIN, my + MARGIN),
    };
    (x.clamp(mx, (mx + mw - w).max(mx)), y.clamp(my, (my + mh - h).max(my)))
}

/// Opens a hud window if it isn't open, on the monitor the game is on. An ordinary topmost window
/// that never takes focus, ignores the mouse (except while being moved) and is left out of
/// capture (ADR 0010). It polls with `hud_poll` and closes itself with `hud_idle`. Runs on the
/// main thread: a worker waiting on it for the monitor list or the window handle would hang
/// quitting, which joins finishing workers.
fn open_hud(app: &AppHandle, part: HudPart) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || open_hud_now(&handle, part));
}

fn open_hud_now(app: &AppHandle, part: HudPart) {
    let engine = app.state::<Engine>();
    if app.get_webview_window(part.label()).is_some() || engine.quitting.load(Relaxed) {
        return;
    }
    let (width, height) = part.size();
    let saved = Settings::load().ok().and_then(|s| part.saved(&s.general));
    let origin = backsight_engine::capture::wgc::monitor_origin(backsight_engine::capture::wgc::foreground_monitor());
    let monitors = app.available_monitors().unwrap_or_default();
    let monitor = origin
        .ok()
        .and_then(|(x, y)| monitors.iter().find(|m| (m.position().x, m.position().y) == (x, y)))
        .or(monitors.first());
    // shortcut: logical position from this monitor's scale; on mixed-DPI setups Windows may place it a little off.
    let (x, y) = monitor.map_or((16.0, 16.0), |m| {
        let scale = m.scale_factor();
        let (p, s) = (m.position().to_logical::<f64>(scale), m.size().to_logical::<f64>(scale));
        hud_spot(part, saved, (p.x, p.y, s.width, s.height))
    });
    let url = if part == HudPart::Dot { "index.html?hud=dot" } else { "index.html?hud" };
    // Built visible and unfocused: Windows then shows it without activating it (SW_SHOWNOACTIVATE),
    // so a full-screen game never loses focus. Showing a hidden window later would activate it.
    let built = WebviewWindowBuilder::new(app, part.label(), WebviewUrl::App(url.into()))
        .title("Backsight")
        .inner_size(width, height)
        .position(x, y)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .focusable(false)
        .build();
    match built {
        Ok(hud) => {
            let _ = hud.set_ignore_cursor_events(!engine.arranging.load(Relaxed));
            if let Err(e) = hud.hwnd().map(|h| backsight_engine::capture::wgc::exclude_from_capture(h.0 as isize)) {
                tracing::warn!("Backsight's pop-ups may show up in recordings: {e}");
            }
        }
        Err(e) => tracing::warn!("couldn't open the pop-up window: {e}"),
    }
}

/// Where a hud window is now, as `hud_spot` takes it back: a share of the room it can move in on
/// its monitor.
fn hud_share(window: &tauri::WebviewWindow) -> Option<[f64; 2]> {
    let p = window.outer_position().ok()?;
    let size = window.outer_size().ok()?;
    let m = window.current_monitor().ok()??;
    let (mp, ms) = (m.position(), m.size());
    let share = |v: i32, start: i32, room: i64| {
        let s = f64::from(v - start) / (room.max(1) as f64);
        (s.clamp(0.0, 1.0) * 1e4).round() / 1e4
    };
    Some([
        share(p.x, mp.x, i64::from(ms.width) - i64::from(size.width)),
        share(p.y, mp.y, i64::from(ms.height) - i64::from(size.height)),
    ])
}

/// What the hud shows: new pop-ups, and the recording dot (`None` while the engine is busy:
/// keep showing the last one).
#[derive(Serialize)]
struct Hud {
    notices: Vec<Notice>,
    recording: Option<HudRecording>,
    /// Being moved: show a sample and let the mouse drag it.
    arranging: bool,
    /// The window's middle is in the left or bottom half of its monitor: line up toward that edge.
    left: bool,
    bottom: bool,
}

#[derive(Serialize, Clone, Copy)]
struct HudRecording {
    /// Show the dot: recording, with the recording dot turned on.
    on: bool,
    seconds: f64,
    paused: bool,
    /// The mic is recorded in push-to-talk mode; `mic_open` while its key is held.
    push_to_talk: bool,
    mic_open: bool,
    mic_muted: bool,
}

/// The recording dot's state, or `None` if the engine is busy.
fn hud_recording(engine: &Engine, indicator: bool) -> Option<HudRecording> {
    let mic_open = lock(&engine.mic_key).down;
    let replay = engine.replay.try_lock().ok()?;
    let session = replay.session.as_ref();
    // A recording that stopped on its own still exists until the record key is pressed.
    let time = session
        .filter(|s| s.recording_error().is_none() && s.error().is_none())
        .and_then(ReplaySession::recording_time);
    Some(HudRecording {
        on: indicator && time.is_some(),
        seconds: time.map_or(0.0, |t| t.as_secs_f64()),
        paused: session.is_some_and(ReplaySession::recording_paused),
        push_to_talk: replay.ptt,
        mic_open: replay.ptt && mic_open,
        mic_muted: replay.mic_muted && session.is_some_and(ReplaySession::records_mic),
    })
}

/// A hud window's poll, a few times a second while it is open.
#[tauri::command]
fn hud_poll(window: tauri::WebviewWindow, engine: tauri::State<Engine>) -> Hud {
    let indicator = !engine.hide_indicator.load(Relaxed);
    let popups = window.label() == HudPart::Popups.label();
    let notices = if popups { std::mem::take(&mut *lock(&engine.hud_queue)) } else { Vec::new() };
    // Under half of the room to move in means the window's middle is left of (or above) the screen's.
    let [x, y] = hud_share(&window).unwrap_or([1.0, 0.0]);
    Hud {
        notices,
        recording: hud_recording(&engine, indicator),
        arranging: engine.arranging.load(Relaxed),
        left: x < 0.5,
        bottom: y > 0.5,
    }
}

/// A hud window has nothing left to show: close it to free its memory, unless a pop-up just
/// arrived or it is being moved.
#[tauri::command]
fn hud_idle(window: tauri::WebviewWindow, engine: tauri::State<Engine>) {
    let waiting = window.label() == HudPart::Popups.label() && !lock(&engine.hud_queue).is_empty();
    if !waiting && !engine.arranging.load(Relaxed) {
        let _ = window.destroy();
    }
}

/// The mouse went down on a hud window that is being moved: let Windows drag it.
#[tauri::command]
fn hud_drag(window: tauri::WebviewWindow, engine: tauri::State<Engine>) {
    if engine.arranging.load(Relaxed) {
        let dot = window.label() == HudPart::Dot.label();
        engine.dragged[usize::from(dot)].store(true, Relaxed);
        let _ = window.start_dragging();
    }
}

/// Where each hud window was left; `None` if it wasn't dragged.
#[derive(Serialize)]
struct HudSpots {
    popups: Option<[f64; 2]>,
    dot: Option<[f64; 2]>,
}

/// Starts (`on`) or ends moving the pop-ups and recording dot. Ending returns where each was left
/// for the settings page to save (it owns the settings, so a save from here would be overwritten).
/// Async: opening windows from a sync command deadlocks on Windows.
#[tauri::command]
async fn hud_arrange(app: AppHandle, on: bool) -> HudSpots {
    let engine = app.state::<Engine>();
    engine.arranging.store(on, Relaxed);
    let mut spots = HudSpots { popups: None, dot: None };
    for part in HudPart::ALL {
        let dot = part == HudPart::Dot;
        // Only a window that was dragged gets a saved spot: Move then Done keeps the default.
        let dragged = engine.dragged[usize::from(dot)].swap(false, Relaxed);
        let Some(window) = app.get_webview_window(part.label()) else {
            if on {
                open_hud(&app, part);
            }
            continue;
        };
        if !on && dragged {
            *(if dot { &mut spots.dot } else { &mut spots.popups }) = hud_share(&window);
        }
        let _ = window.set_ignore_cursor_events(!on);
    }
    spots
}

/// `icon` with a red dot in its bottom-right corner: the tray icon while recording.
fn recording_icon(icon: &Image<'_>) -> Image<'static> {
    let (w, h) = (icon.width() as usize, icon.height() as usize);
    let mut rgba = icon.rgba().to_vec();
    let r = w.min(h) as f32 * 0.3;
    let (cx, cy) = (w as f32 - r - 0.5, h as f32 - r - 0.5);
    for y in 0..h {
        for x in 0..w {
            let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            let px = &mut rgba[(y * w + x) * 4..][..4];
            if d <= r - 1.0 {
                px.copy_from_slice(&[0xE5, 0x2B, 0x2B, 0xFF]);
            } else if d <= r + 0.5 {
                px.copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // a ring that shows on any taskbar
            }
        }
    }
    Image::new_owned(rgba, w as u32, h as u32)
}

/// Keeps the tray icon and the hud's recording dot in step with the recording. Cheap when
/// nothing changed; called every second and right after the record and pause keys.
/// [`refresh_indicators`] for a caller holding `apply`, which mustn't wait for the main thread.
fn refresh_indicators_soon(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || refresh_indicators(&app));
}

fn refresh_indicators(app: &AppHandle) {
    let engine = app.state::<Engine>();
    let Some(rec) = hud_recording(&engine, !engine.hide_indicator.load(Relaxed)) else { return };
    if engine.tray_recording.swap(rec.on, Relaxed) != rec.on
        && let (Some(tray), Some(icon)) = (app.tray_by_id("main"), app.default_window_icon())
    {
        let _ = tray.set_icon(Some(if rec.on { recording_icon(icon) } else { icon.clone() }));
        let _ = tray.set_tooltip(Some(if rec.on { "Backsight: recording" } else { "Backsight" }));
    }
    if rec.on {
        open_hud(app, HudPart::Dot);
    }
}

/// Clears the busy flag even if taking the screenshot panics.
struct ScreenshotDone(AppHandle);
impl Drop for ScreenshotDone {
    fn drop(&mut self) {
        self.0.state::<Engine>().screenshot_busy.store(false, Relaxed);
    }
}

fn settings_error(e: impl std::fmt::Display) -> String {
    format!("Couldn't read your settings: {e}")
}

/// Tells the user how a screenshot went: the same notification for every kind.
fn report_screenshot<E>(app: &AppHandle, settings: &Result<Settings, E>, result: Result<Saved, String>) {
    let (title, body) = match result {
        Ok(saved) => {
            let _ = app.emit("library-changed", ());
            match saved.clipboard_error {
                Some(e) => ("Screenshot saved; copy failed", e),
                None => (
                    "Screenshot saved",
                    if settings.as_ref().is_ok_and(|s| s.screenshot.copy_to_clipboard) {
                        "Image copied. Paste it with Ctrl+V.".into()
                    } else {
                        saved.path.display().to_string()
                    },
                ),
            }
        }
        Err(e) => {
            tracing::error!("Screenshot failed: {e}");
            ("Couldn't take screenshot", e)
        }
    };
    notify(app, title, &body);
}

/// Opens the region picker over the frozen monitor: an ordinary borderless, topmost window of
/// ours, never an overlay inside the game. It stays hidden until the picture is drawn
/// (`region_show`).
fn open_picker(app: &AppHandle, frozen: Frozen) -> tauri::Result<()> {
    let position = PhysicalPosition::new(frozen.origin.0, frozen.origin.1);
    let size = PhysicalSize::new(frozen.width, frozen.height);
    *lock(&app.state::<Engine>().region) = Some(frozen);
    let picker = WebviewWindowBuilder::new(app, "picker", WebviewUrl::App("index.html?picker".into()))
        .title("Backsight: pick an area")
        .visible(false)
        .decorations(false)
        .shadow(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .build()?;
    *lock(&app.state::<Engine>().picker_opened) = Some(std::time::Instant::now());
    // Keep the frozen picture out of a running replay or recording.
    if let Err(e) = picker.hwnd().map(|h| backsight_engine::capture::wgc::exclude_from_capture(h.0 as isize)) {
        tracing::warn!("the area picker may show up in recordings: {e}");
    }
    // Moving onto a monitor with another scale resizes the window (WM_DPICHANGED) and may nudge
    // it, so size it after the move and then place it again. All in physical pixels.
    picker.set_position(position)?;
    picker.set_size(size)?;
    picker.set_position(position)
}

/// This build and the newer one `scripts/build-desktop.ps1` left waiting beside it in
/// `dist-desktop/next`, each as its `build.txt` line. `current` is `None` in a dev build.
#[derive(Serialize)]
struct LocalBuilds {
    /// Backsight's version number, shown on Home.
    version: &'static str,
    current: Option<String>,
    next: Option<String>,
    /// Why the last press of Update didn't install `next`, written by `apply-update.ps1`.
    error: Option<String>,
}

/// The commit this build was made from (`build.rs`), empty in a development build.
const BUILD: &str = env!("BACKSIGHT_BUILD");
/// This start copied the settings saved under the old name Clipwise (ADR 0013).
static ADOPTED_OLD_NAME: AtomicBool = AtomicBool::new(false);

fn app_folder() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(PathBuf::from)
}

/// This build's `build.txt` line: a local test build's own file first (written at build time, so it
/// reads like the `next` one), else the stamp compiled in. `None` in a development build.
fn this_build() -> Option<String> {
    app_folder()
        .and_then(|d| std::fs::read_to_string(d.join("build.txt")).ok())
        .map(|s| s.trim().to_owned())
        .or_else(|| Some(BUILD.to_owned()))
        .filter(|b| !b.is_empty())
}

#[tauri::command(async)]
fn local_builds() -> LocalBuilds {
    let read = |p: PathBuf| std::fs::read_to_string(p).ok().map(|s| s.trim().to_owned());
    let dir = app_folder();
    LocalBuilds {
        version: env!("CARGO_PKG_VERSION"),
        current: this_build(),
        next: dir.as_ref().filter(|_| LOCAL_UPDATE).and_then(|d| read(d.join("../next/build.txt"))),
        error: dir.filter(|_| LOCAL_UPDATE).and_then(|d| read(d.join("../next/update-error.txt"))),
    }
}

/// Only the desktop test build (`scripts/build-desktop.ps1`) can update from `../next` (ADR 0009).
const LOCAL_UPDATE: bool = cfg!(feature = "local-update");

/// Installers and public builds carry no code that starts an update script.
#[cfg(not(feature = "local-update"))]
#[tauri::command(async)]
fn install_local_build() -> Result<(), String> {
    Err("This copy of Backsight can't update itself. Install the new version instead.".into())
}

/// Starts the waiting build's `apply-update.ps1`, which copies it over this one once Backsight has
/// closed and opens it again, then quits (finishing any recording first, like Quit).
#[cfg(feature = "local-update")]
#[tauri::command(async)]
fn install_local_build(app: AppHandle) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // Only a local test build (it has a build.txt) updates itself this way.
    if !app_folder().is_some_and(|d| d.join("build.txt").is_file()) {
        return Err("This copy of Backsight can't update itself. Install the new version instead.".into());
    }
    let script = app_folder().map(|d| d.join("../next/apply-update.ps1")).filter(|p| p.exists());
    let script = script.ok_or("The new build is gone. Build it again, then press Update.")?;
    std::process::Command::new(windows_dir().join(r"System32\WindowsPowerShell\v1.0\powershell.exe"))
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(script)
        .args(["-WaitPid", &std::process::id().to_string()])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("Backsight couldn't start the update. Try again. ({e})"))?;
    quit(&app);
    Ok(())
}

/// The frozen monitor as a BMP file, for the picker to show.
#[tauri::command(async)]
fn region_image(engine: tauri::State<Engine>) -> Result<tauri::ipc::Response, &'static str> {
    lock(&engine.region).as_ref().map(|f| tauri::ipc::Response::new(f.bmp())).ok_or("The area picker has closed.")
}

/// The picker drew the picture: show it and take the keyboard, so Esc reaches it.
#[tauri::command]
fn region_show(window: WebviewWindow) {
    let _ = window.show();
    let _ = window.set_focus();
}

/// A finished drag in the picker, in its CSS pixels; see `screenshot::take_region`.
#[derive(Deserialize)]
struct Pick {
    start: [f64; 2],
    end: [f64; 2],
    view: [f64; 2],
}

/// Closes the picker, then saves the picked area, or nothing when the user cancelled (`None`).
#[tauri::command]
async fn region_pick(app: AppHandle, pick: Option<Pick>) {
    let frozen = lock(&app.state::<Engine>().region).take();
    if let Some(picker) = app.get_webview_window("picker") {
        let _ = picker.destroy();
    }
    let (Some(frozen), Some(pick)) = (frozen, pick) else { return };
    let _ = spawn_blocking(move || {
        let settings = Settings::load();
        let result = settings
            .as_ref()
            .map_err(settings_error)
            .and_then(|s| screenshot::take_region(s, &frozen, pick.start, pick.end, pick.view));
        // `None`: a click without a drag cancels.
        if let Some(result) = result.transpose() {
            report_screenshot(&app, &settings, result);
        }
    })
    .await;
}

fn start_hotkeys(app: &AppHandle, settings: &Settings) {
    // Parse first: a bad binding keeps the hotkeys that work instead of leaving none, and Home says so.
    let bindings = match hotkey_bindings(settings) {
        Ok(b) => b,
        Err(problem) => {
            tracing::warn!("hotkeys kept as they were: {problem}");
            *lock(&app.state::<Engine>().hotkey_problem) = Some(problem);
            return;
        }
    };
    let engine = app.state::<Engine>();
    let mut slot = lock(&engine.hotkeys);
    *slot = None; // Windows keeps one Raw Input target per process: stop the old one first
    // The old matcher can no longer deliver the release for a held key.
    set_mic_key(&engine, false);
    let handle = app.clone();
    let actions = app.state::<Sender<Action>>().inner().clone();
    // A combo shared with an earlier action is ignored by the matcher, so it never opens the mic.
    let has_ptt = bindings.contains_key(&Action::PushToTalk)
        && !backsight_config::conflicts(&bindings).iter().any(|&(_, later)| later == Action::PushToTalk);
    engine.ptt_live.store(false, Relaxed);
    match Hotkeys::start(bindings, move |event| match event {
        _ if hotkey_ignored(&handle.state::<Engine>(), &event) => {}
        Event::Pressed(Action::PushToTalk) => set_mic_key(&handle.state::<Engine>(), true),
        Event::Released(Action::PushToTalk) => set_mic_key(&handle.state::<Engine>(), false),
        Event::Pressed(action) => {
            if let Err(e) = actions.try_send(action) {
                tracing::warn!("hotkey action skipped: {e}");
            }
        }
        Event::Released(_) => {}
    }) {
        Ok(h) => {
            *slot = Some(h);
            engine.ptt_live.store(has_ptt, Relaxed);
            *lock(&engine.hotkey_problem) = None;
        }
        Err(e) => {
            tracing::error!("hotkeys not started: {e}");
            drop(slot);
            *lock(&engine.hotkey_problem) = Some(HOTKEYS_FAILED.into());
            notify(app, "Hotkeys aren't working", HOTKEYS_FAILED);
        }
    }
}

/// The hotkeys in `settings`, or what to tell the user when one (a hand-edited file) can't be read.
fn hotkey_bindings(settings: &Settings) -> Result<BTreeMap<Action, Hotkey>, String> {
    settings.parsed_hotkeys().map_err(|(action, e)| {
        tracing::warn!("{action:?} hotkey is invalid: {e}");
        let combo = settings.hotkeys.get(&action).map_or("", String::as_str);
        format!(
            "One of your hotkeys (\"{combo}\") isn't valid, so no hotkey changes were applied. Set it again on Backsight's Hotkeys page."
        )
    })
}

/// Raw Input isn't exclusive, so no other program can take it: a refusal is Windows itself.
const HOTKEYS_FAILED: &str = "Windows wouldn't let Backsight listen for your hotkeys, so pressing them does nothing. Quit Backsight from its icon near the clock (click ^ if you don't see it) and open it again. If that doesn't help, restart your computer.";

/// Keys pressed into a hotkey box aren't hotkeys. Releases still pass, so a held key lets go.
fn hotkey_ignored(engine: &Engine, event: &Event<Action>) -> bool {
    matches!(event, Event::Pressed(_)) && engine.hotkeys_paused.load(Relaxed)
}

/// A hotkey box starts (`on`) or stops listening for a new combo.
#[tauri::command]
fn pause_hotkeys(engine: tauri::State<Engine>, on: bool) {
    engine.hotkeys_paused.store(on, Relaxed);
}

fn set_mic_key(engine: &Engine, down: bool) {
    let mut key = lock(&engine.mic_key);
    key.down = down;
    if let Some(control) = &key.control {
        control(down);
    }
}

/// Finishes a stopped recording and tells the user, then converts its MKV files to MP4 on a
/// thread of its own: quitting waits only for the finish, never for a conversion.
fn finish_recording(app: AppHandle, stopping: backsight_engine::session::Stopping) -> impl FnOnce() + Send + 'static {
    let saving = backsight_engine::shutdown::saving();
    move || {
        let (title, body, saved) = stopped_notice(stopping.finish());
        let mkvs = mark_for_mp4(saved);
        drop(saving); // the conversion resumes at the next start, so it never holds shutdown
        let _ = app.emit("library-changed", ());
        notify(&app, title, &body);
        std::thread::spawn(move || convert_to_mp4(&app, &mkvs));
    }
}

/// The MKV files among `saved` to convert to MP4, each marked first (while quitting still waits),
/// so a conversion cut off later finishes at the next start.
fn mark_for_mp4(saved: Vec<PathBuf>) -> Vec<PathBuf> {
    let remux = Settings::load().is_ok_and(|s| s.video.remux_to_mp4);
    let mkvs: Vec<_> =
        saved.into_iter().filter(|p| remux && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("mkv"))).collect();
    mkvs.iter().for_each(|mkv| backsight_engine::recovery::mark_conversion(mkv));
    mkvs
}

/// Makes a clip's vertical copy on its own thread (ADR 0015): never waited for by the next save
/// or by quitting (the next start picks it up again), and says when it is there. With captions on,
/// the clip it is made from is captioned first and the captions are burned into the picture.
fn make_vertical(app: &AppHandle, job: vertical::Job, settings: Settings) {
    let app = app.clone();
    std::thread::spawn(move || {
        let (title, body) = match job.run(&settings, |source| captions::wait_for(source, &settings)) {
            Ok(path) => ("Vertical clip saved", in_library(&path.file_name().unwrap_or_default().to_string_lossy())),
            // Quitting leaves the job for the next start, so there's nothing to tell.
            Err(_) if app.state::<Engine>().quitting.load(Relaxed) => return,
            Err(e) => ("Couldn't make the vertical clip", e),
        };
        let _ = app.emit("library-changed", ());
        notify(&app, title, &body);
    });
}

/// Converts recordings' MKV files to MP4 (the MKV is always kept), telling the user if one can't be.
fn convert_to_mp4(app: &AppHandle, mkvs: &[PathBuf]) {
    for mkv in mkvs {
        match backsight_engine::recovery::finish_conversion(mkv) {
            Ok(_) => {
                let _ = app.emit("library-changed", ());
            }
            Err(e) => {
                tracing::warn!("remux of {} failed: {e}", mkv.display());
                let body = if e.starts_with(backsight_encode::remux::CANT_WRITE) {
                    "Your recording is kept as MKV. It plays in VLC. The MP4 couldn't be written: the drive may be full. Free up some space; the next start tries the MP4 again."
                } else {
                    "Your recording is kept as MKV. It plays in VLC."
                };
                notify(app, "Couldn't convert to MP4", body);
            }
        }
    }
}

/// Tells the user once when a recording stops on its own (a full or slow drive, lost capture):
/// the window is usually closed while playing, so `get_status` alone would go unseen.
fn watch_recording(app: &AppHandle) {
    let engine = app.state::<Engine>();
    let mut revival = Revival::default();
    let (mut tick, mut moved_before) = (0u32, false);
    // Last tick's answer: a session that could move to another monitor needs their list.
    let mut list_monitors = false;
    while !engine.quitting.load(Relaxed) {
        std::thread::sleep(std::time::Duration::from_secs(1));
        tick = tick.wrapping_add(1);
        // Never waits: starting or stopping holds `replay` for seconds.
        refresh_indicators(app);
        // Every 5 s, asked before the lock: Windows can take a while to list the monitors.
        let check = tick.is_multiple_of(5);
        let listed = if check && list_monitors { backsight_engine::capture::wgc::monitors() } else { Vec::new() };
        let Ok(mut replay) = engine.replay.try_lock() else { continue };
        let room = replay.session.as_ref().map(ReplaySession::replay_room);
        keep_recent(&mut replay.before_move, std::time::Instant::now(), room);
        // Never cutting a recording.
        let movable = |s: &ReplaySession| s.error().is_none() && s.recording_time().is_none();
        list_monitors = replay.session.as_ref().is_some_and(|s| movable(s) && s.follows_monitor());
        if replay.starting {
            continue; // nothing to judge until the session is open
        }
        let moved = check.then(|| replay.session.as_ref().is_some_and(|s| movable(s) && s.monitor_moved(&listed)));
        // Some(on): healthy, with the replay on or off by choice; None: stopped on its own.
        let replay_on = match &replay.session {
            Some(s) => s.error().is_none().then(|| s.replay_on()),
            None => replay.start_error.is_none().then_some(false),
        };
        let sample = replay.session.as_ref().filter(|s| s.error().is_none()).map(|s| {
            let d = s.diagnostics();
            backsight_engine::overload::Sample {
                encoded: d.encoded_frames,
                late: d.late_frames,
                skipped: d.pacer.skipped,
            }
        });
        let problem = replay
            .session
            .as_ref()
            .filter(|s| s.recording_time().is_some())
            .and_then(|s| s.recording_error().or_else(|| s.error()));
        // A failed session's recording is finished first; the restart follows on a later tick.
        let failed_recording =
            replay.session.as_ref().is_some_and(|s| s.recording_time().is_some() && s.error().is_some());
        let replay_failure = match &replay.session {
            Some(s) if s.recording_time().is_none() => s.error(),
            Some(_) => None,
            None => replay.start_error.clone().filter(|_| revival.restarted()),
        };
        drop(replay);
        if revival.back(replay_on) {
            notify(app, "Instant Replay is back on", "Saving clips works again.");
        }
        // Twice in a row, so a monitor blinking out for a moment doesn't restart the replay. The
        // move starts a new session; the replay so far is kept for the next save (`before_move`).
        if let Some(moves) = moved
            && std::mem::replace(&mut moved_before, moves)
            && moves
        {
            tracing::info!(
                "the monitor to capture changed (plugged in or out, or the game's HDR): restarting Instant Replay"
            );
            revival.moving(std::time::Instant::now());
            restart_replay(&engine, false);
        }
        if let Some(warning) = setup::overload_tick(sample) {
            notify(app, "Clips may stutter", &warning);
        }
        if failed_recording {
            // Its "Recording stopped early" pop-up says why.
            finish_failed_recording(app, &engine);
        } else if let Some(problem) = problem
            && !engine.recording_problem_told.swap(true, Relaxed)
        {
            notify(app, "Recording stopped", &problem);
        }
        // Sorted: Windows may list the same monitors in another order.
        let monitors: Vec<String> = match replay_failure {
            Some(_) => {
                let mut ids: Vec<String> =
                    backsight_engine::capture::wgc::monitors().into_iter().map(|m| m.id).collect();
                ids.sort_unstable();
                ids
            }
            None => Vec::new(),
        };
        match (revival.step(replay_failure.is_some(), &monitors, std::time::Instant::now()), replay_failure) {
            (Some(Revive::Restart { tell }), Some(why)) => {
                tracing::warn!("Instant Replay stopped, restarting it: {why}");
                if tell {
                    notify(app, "Instant Replay stopped", &format!("{why} Backsight is turning it back on."));
                }
                restart_replay(&engine, true);
            }
            (Some(Revive::GiveUp), Some(why)) => {
                tracing::error!("Instant Replay gave up restarting: {why}");
                notify(
                    app,
                    "Instant Replay is off",
                    &format!(
                        "{why} Backsight couldn't turn it back on. Turn Instant Replay off and on again, or restart Backsight."
                    ),
                );
            }
            (Some(Revive::GiveUpChanging), Some(why)) => {
                tracing::error!("Instant Replay gave up restarting while the monitors kept changing: {why}");
                notify(
                    app,
                    "Instant Replay is off",
                    &format!(
                        "{why} Your monitors kept changing, and Backsight couldn't turn it back on. Once they're settled, turn Instant Replay off and on again."
                    ),
                );
            }
            _ => {}
        }
    }
}

/// Finishes the recording of a session that failed, so Instant Replay can be restarted.
fn finish_failed_recording(app: &AppHandle, engine: &Engine) {
    let apply = lock(&engine.apply);
    if engine.quitting.load(Relaxed) {
        return;
    }
    let mut replay = lock(&engine.replay);
    if !replay.session.as_ref().is_some_and(|s| s.error().is_some()) {
        return;
    }
    let Some(stopping) = replay.session.as_mut().and_then(ReplaySession::begin_stop_recording) else { return };
    let apply_hotkeys = std::mem::take(&mut replay.pending_hotkeys);
    drop(replay);
    if !spawn_finishing(engine, finish_recording(app.clone(), stopping)) {
        tracing::warn!("a stopped recording wasn't finished: Backsight was already quitting");
    }
    if apply_hotkeys && let Ok(settings) = Settings::load() {
        start_hotkeys(app, &settings);
    }
    drop(apply); // the tray waits for the main thread, which may wait for `apply` (`shut_down`)
    refresh_indicators(app);
}

/// Restarts Instant Replay the way the settings say, unless something changed meanwhile: it
/// `failed`, or (`!failed`) runs fine but should move to another monitor.
fn restart_replay(engine: &Engine, failed: bool) {
    let _apply = lock(&engine.apply);
    if engine.quitting.load(Relaxed) {
        return;
    }
    // Asked before the lock: Windows can take a while to list the monitors.
    let monitors = if failed { Vec::new() } else { backsight_engine::capture::wgc::monitors() };
    let mut replay = lock(&engine.replay);
    let still = match &replay.session {
        Some(s) if failed => s.error().is_some(),
        Some(s) => s.error().is_none() && s.monitor_moved(&monitors),
        None => failed && replay.start_error.is_some(),
    };
    if !still || recording(&replay) {
        return; // turned off, restarted or recording since the check
    }
    // A move keeps what the replay has so far: the next save writes it as a clip of its own
    // (the new session may record another size or HDR, so it can't join the new one's).
    if !failed
        && let Some(s) = &replay.session
        && let Ok(clip) = s.prepare_clip()
    {
        let (at, length, from) = (std::time::Instant::now(), s.replay_length(), replay.session_number);
        replay.before_move.push(Kept { at, length, from, clip });
        keep_recent(&mut replay.before_move, at, None); // the new session's buffer starts empty
    }
    drop(replay);
    match Settings::load() {
        Ok(s) => apply_replay(engine, &s),
        Err(e) => {
            let mut replay = lock(&engine.replay);
            stop_replay(engine, &mut replay);
            replay.start_error = Some(format!("Backsight couldn't read your settings: {e}"));
        }
    }
}

/// Restarts tried for Instant Replay that stopped on its own (a monitor unplugged, a driver
/// reset, an encoder error).
const REPLAY_RESTARTS: u32 = 3;
/// Wait after each restart before the next (or giving up), so a monitor or a driver that resets
/// the display several times has a moment.
const RESTART_GAPS: [std::time::Duration; REPLAY_RESTARTS as usize] =
    [std::time::Duration::from_secs(5), std::time::Duration::from_secs(15), std::time::Duration::from_secs(60)];
/// Running this long after a restart counts as working again: a later failure starts a fresh count.
const RESTART_RESET: std::time::Duration = std::time::Duration::from_secs(60);
/// Fresh rounds of restarts that changed monitors may start, each after a longer wait
/// (`RESTART_GAPS`), until Instant Replay works again: a monitor that keeps blinking, with a
/// failure that lasts, would otherwise restart it every few seconds forever.
const MONITOR_ROUNDS: u32 = 3;

#[derive(Debug, PartialEq)]
enum Revive {
    /// Restart; `tell` the user on the first one.
    Restart { tell: bool },
    /// Tell the user it stays off.
    GiveUp,
    /// Tell the user it stays off, though the monitors keep changing (`MONITOR_ROUNDS`).
    GiveUpChanging,
}

/// Decides, once a second, what to do about Instant Replay that stopped on its own.
#[derive(Default)]
struct Revival {
    restarts: u32,
    last: Option<std::time::Instant>,
    gave_up: bool,
    /// The user was told it stays off.
    told_off: bool,
    /// The monitors connected at the last restart: a change (one plugged back in) is worth
    /// another round of restarts, even after giving up, up to `MONITOR_ROUNDS`.
    monitors: Vec<String>,
    /// Rounds started by a monitor change since it last worked.
    rounds: u32,
    /// The user was told it stays off while the monitors keep changing.
    told_changing: bool,
}

impl Revival {
    /// A restart of ours is under way or failed: its start error counts as a failure.
    fn restarted(&self) -> bool {
        self.restarts > 0
    }

    /// Our own restart onto another monitor: if it fails to start, that's retried like a failure.
    fn moving(&mut self, now: std::time::Instant) {
        self.restarts = self.restarts.max(1);
        self.last = Some(now);
    }

    /// Whether to say Instant Replay is back after being told it stays off: once, when it runs
    /// again (`Some(true)`). Turned off by choice (`Some(false)`) means there's nothing to say.
    fn back(&mut self, replay_on: Option<bool>) -> bool {
        match replay_on {
            Some(on) => std::mem::take(&mut self.told_off) && on,
            None => false,
        }
    }

    /// `monitors` only matters while `failed`.
    fn step(&mut self, failed: bool, monitors: &[String], now: std::time::Instant) -> Option<Revive> {
        let since = self.last.map(|t| now.saturating_duration_since(t));
        if !failed {
            if since.is_some_and(|d| d >= RESTART_RESET) {
                *self = Self { told_off: self.told_off, ..Self::default() };
            }
            return None;
        }
        // Each round waits longer before it starts, and there are only a few, so monitors that
        // keep changing can't restart in a loop.
        let round_gap = RESTART_GAPS[self.rounds.min(REPLAY_RESTARTS - 1) as usize];
        if self.restarts > 0
            && monitors != self.monitors
            && self.rounds < MONITOR_ROUNDS
            && since.is_none_or(|d| d >= round_gap)
        {
            // A fresh round, as after the first restart: the user was already told it stopped.
            *self = Self {
                restarts: 1,
                last: Some(now),
                gave_up: false,
                monitors: monitors.to_vec(),
                rounds: self.rounds + 1,
                ..std::mem::take(self)
            };
            return Some(Revive::Restart { tell: false });
        }
        let gap = RESTART_GAPS[self.restarts.saturating_sub(1).min(REPLAY_RESTARTS - 1) as usize];
        if self.gave_up || since.is_some_and(|d| d < gap) {
            return None;
        }
        if self.restarts >= REPLAY_RESTARTS {
            self.gave_up = true;
            // A later round ends quietly (the user knows it's off), except the last one, which
            // says it stops trying while the monitors keep changing.
            if self.rounds >= MONITOR_ROUNDS {
                self.told_off = true;
                return (!std::mem::replace(&mut self.told_changing, true)).then_some(Revive::GiveUpChanging);
            }
            return (!std::mem::replace(&mut self.told_off, true)).then_some(Revive::GiveUp);
        }
        self.restarts += 1;
        self.last = Some(now);
        self.monitors = monitors.to_vec();
        Some(Revive::Restart { tell: self.restarts == 1 })
    }
}

fn set_replay(engine: &Engine, desired: Option<bool>) {
    let _apply = lock(&engine.apply);
    if engine.quitting.load(Relaxed) {
        return;
    }
    // One lock for the check and the action, so two presses can't both start a session.
    let mut replay = lock(&engine.replay);
    let active = replay.session.as_ref().is_some_and(|session| session.replay_on()) || replay.start_error.is_some();
    let running = replay.session.is_some() || replay.start_error.is_some();
    match replay_step(desired, active, recording(&replay), running) {
        ReplayStep::Nothing => {}
        ReplayStep::FlipBuffer => {
            // Keep the pipeline (and the recording) running; only the buffer changes.
            let session = replay.session.as_ref().expect("recording");
            let on = !session.replay_on();
            session.set_replay(on);
            replay.toggled = Some(on);
        }
        ReplayStep::Stop => {
            stop_replay(engine, &mut replay);
            replay.toggled = Some(false);
        }
        ReplayStep::Start => {
            replay.toggled = Some(true);
            drop(replay);
            match Settings::load() {
                Ok(s) => start_replay(engine, &s, false),
                Err(e) => {
                    lock(&engine.replay).start_error = Some(format!("Backsight couldn't read your settings: {e}"));
                }
            }
        }
    }
}

/// What a press of the replay toggle (or a settings-driven on/off) does.
#[derive(Debug, PartialEq)]
enum ReplayStep {
    Nothing,
    /// A recording is running: its pipeline stays, only the replay buffer turns on or off.
    FlipBuffer,
    Stop,
    Start,
}

/// `desired` is `None` for a plain toggle. `active`: replay is on or has a start error to show.
/// `running`: there is a session or a start error.
fn replay_step(desired: Option<bool>, active: bool, recording: bool, running: bool) -> ReplayStep {
    if desired.is_some_and(|on| on == active) {
        ReplayStep::Nothing
    } else if recording {
        ReplayStep::FlipBuffer
    } else if running {
        ReplayStep::Stop
    } else {
        ReplayStep::Start
    }
}

fn on_hotkey(app: &AppHandle, action: Action) {
    let engine = app.state::<Engine>();
    if engine.quitting.load(Relaxed) {
        return;
    }
    match action {
        Action::Screenshot | Action::ScreenshotWindow => {
            if engine.screenshot_busy.swap(true, Relaxed) {
                return;
            }
            let app = app.clone();
            spawn_blocking(move || {
                let _done = ScreenshotDone(app.clone());
                let settings = Settings::load();
                let result = settings.as_ref().map_err(settings_error).and_then(|s| match action {
                    Action::ScreenshotWindow => screenshot::take_window(s),
                    _ => screenshot::take(s),
                });
                report_screenshot(&app, &settings, result);
            });
        }
        Action::ScreenshotRegion => {
            if let Some(picker) = app.get_webview_window("picker") {
                let loading = lock(&engine.picker_opened).is_some_and(|t| t.elapsed().as_secs() < 5);
                if picker.is_visible().unwrap_or(true) {
                    let _ = picker.set_focus();
                } else if loading {
                    // Still loading its frozen frame (a second press too soon): let it finish.
                } else {
                    // Its page never showed it (a WebView crash): close it so the next press works.
                    // Not reopened here, so its Destroyed event can't clear a new picker's frame.
                    let _ = picker.destroy();
                    notify(app, "Couldn't open the area picker", "Press the region screenshot key again.");
                }
                return;
            }
            if engine.screenshot_busy.swap(true, Relaxed) {
                return;
            }
            let app = app.clone();
            spawn_blocking(move || {
                let _done = ScreenshotDone(app.clone());
                let settings = Settings::load();
                let opened =
                    settings.as_ref().map_err(settings_error).and_then(screenshot::freeze).and_then(|frozen| {
                        open_picker(&app, frozen).map_err(|e| {
                            *lock(&app.state::<Engine>().region) = None;
                            format!("Backsight couldn't open the area picker. Restart Backsight and try again. ({e})")
                        })
                    });
                if let Err(e) = opened {
                    report_screenshot(&app, &settings, Err(e));
                }
            });
        }
        Action::SaveReplay => {
            if engine.replay_save_busy.swap(true, Relaxed) {
                notify(app, "Still saving the last clip", "Press again in a moment to save another.");
                return;
            }
            struct Done(AppHandle);
            impl Drop for Done {
                fn drop(&mut self) {
                    self.0.state::<Engine>().replay_save_busy.store(false, Relaxed);
                }
            }
            let done = Done(app.clone());
            let (earlier, prepared) = {
                let mut replay = lock(&engine.replay);
                let room = replay.session.as_ref().map(ReplaySession::replay_room);
                keep_recent(&mut replay.before_move, std::time::Instant::now(), room);
                let running = replay.session.is_some().then_some(replay.session_number);
                let earlier = kept_to_save(&mut replay.before_move, running);
                let prepared = match (&replay.session, &replay.start_error) {
                    _ if replay.starting => Err("Instant Replay is still starting. Try again in a moment.".into()),
                    (Some(s), _) => s.prepare_clip(),
                    (None, Some(e)) => Err(e.clone()),
                    (None, None) => Err("Instant Replay is off. Turn it on to save a clip.".into()),
                };
                (earlier, prepared)
            };
            let app = app.clone();
            let saving = backsight_engine::shutdown::saving();
            // Quitting (and Windows shutting down) waits for the clip's file.
            let started = spawn_finishing(&engine, move || {
                let (_done, _saving) = (done, saving);
                let earlier: Vec<_> = earlier.into_iter().map(Clip::save).collect();
                let now = prepared.and_then(Clip::save);
                if now.is_ok() || earlier.iter().any(Result::is_ok) {
                    let _ = app.emit("library-changed", ());
                }
                let settings = lock(&app.state::<Engine>().applied).clone();
                let shape = settings.as_ref().map_or(Shape::Wide, |s| Shape::from_setting(&s.replay.clip_shape));
                let mut making = false;
                for path in earlier.iter().chain([&now]).filter_map(|r| r.as_ref().ok()) {
                    let job = vertical::Job::after_save(path, shape);
                    making |= job.is_some() && matches!(shape, Shape::Vertical);
                    // A vertical job captions the clip itself, before it burns the captions in.
                    match (job, &settings) {
                        (Some(job), Some(settings)) => make_vertical(&app, job, settings.clone()),
                        (None, Some(settings)) => captions::after_save(path, settings),
                        _ => {}
                    }
                }
                let (title, body) = saved_notice(&earlier, now, making);
                notify(&app, title, &body);
            });
            if !started {
                tracing::warn!("clip not saved: Backsight was already quitting");
            }
        }
        // Both can take seconds (an encoder opens), so each runs on its own thread and this
        // returns at once: the keys after it never wait. A press while one is still being carried
        // out is dropped, not queued (`respond::Gate`).
        Action::ToggleReplay => {
            if let Some(pass) = engine.replay_gate.enter() {
                let app = app.clone();
                std::thread::spawn(move || {
                    let _pass = pass;
                    set_replay(&app.state::<Engine>(), None);
                });
            } else {
                notify(app, "Still finishing", "Backsight is busy with the last press. Press again in a moment.");
            }
        }
        Action::ToggleRecording => match engine.record_gate.enter() {
            Some(pass) => {
                let app = app.clone();
                std::thread::spawn(move || toggle_recording(&app, pass));
            }
            // Every dropped press is answered, so a press is never silently lost.
            None if engine.record_starting.load(Relaxed) => {
                notify(app, "Still getting ready", "One press is enough. Recording starts in a moment.")
            }
            None => notify(app, "Still finishing", "Backsight is busy with the last press. Press again in a moment."),
        },
        Action::AddMarker => {
            let marked = lock(&engine.replay).session.as_ref().map(|s| (s.add_marker(), s.recording_splits()));
            match marked {
                // With split parts the time since the press isn't a time in any one file.
                Some((Ok(_), true)) => notify(app, "Marker added", "It shows as a chapter in the part being recorded."),
                Some((Ok(at), false)) => {
                    let s = at.as_secs();
                    notify(app, "Marker added", &format!("At {}:{:02} in the recording.", s / 60, s % 60));
                }
                Some((Err(e), _)) => notify(app, "Couldn't add a marker", &e),
                None => notify(app, "Not recording", "Start a recording first, then add markers."),
            }
        }
        Action::ToggleMic => {
            let mut replay = lock(&engine.replay);
            replay.mic_muted = !replay.mic_muted;
            if let Some(s) = &replay.session {
                s.set_mic_muted(replay.mic_muted);
            }
            let muted = replay.mic_muted;
            drop(replay);
            match muted {
                true => notify(app, "Microphone muted", "Press the same key to unmute."),
                false => notify(app, "Microphone on", "Your voice is recorded again."),
            }
        }
        Action::PauseRecording => {
            let paused = {
                let mut replay = lock(&engine.replay);
                replay.session.as_mut().filter(|s| s.recording_time().is_some()).map(|s| {
                    if s.recording_paused() {
                        s.resume_recording();
                    } else {
                        s.pause_recording();
                    }
                    s.recording_paused()
                })
            };
            match paused {
                Some(true) => notify(app, "Recording paused", "Press the same key again to carry on."),
                Some(false) => notify(app, "Recording resumed", "Recording again from here."),
                None => notify(app, "Not recording", "Start a recording first, then pause it."),
            }
        }
        Action::OpenApp => show_main_window(app),
        Action::PushToTalk => {} // handled by the hotkey callback itself
    }
}

fn push_to_talk(settings: &Settings) -> bool {
    settings.hotkeys.get(&Action::PushToTalk).is_some_and(|key| !key.trim().is_empty())
}

/// Which parts of the engine a settings change affects: (session, hotkeys).
fn restart_needed(old: &Settings, new: &Settings) -> (bool, bool) {
    // Recording-only fields, which `ReplaySession::start` doesn't read, must not clear the buffer.
    // A new field restarts until it's listed here.
    let replay_video = |s: &Settings| {
        let mut v = s.video.clone();
        (v.split_minutes, v.remux_to_mp4) = (0, false);
        v
    };
    let session = old.replay != new.replay
        || old.general.video_folder != new.general.video_folder
        || replay_video(old) != replay_video(new)
        || old.audio != new.audio
        || push_to_talk(old) != push_to_talk(new);
    (session, old.hotkeys != new.hotkeys)
}

// `(async)` here and on the other commands that read files or start programs: a plain sync
// command runs on the main thread, which runs every window, so a slow disk would freeze them.
#[tauri::command(async)]
fn get_settings() -> Result<Settings, String> {
    Settings::load().map_err(|e| e.to_string())
}

/// Saves the file and returns at once; the engine catches up in the background (`apply_saved`).
#[tauri::command(async)]
fn save_settings(app: AppHandle, settings: Settings) -> Result<(), String> {
    let engine = app.state::<Engine>();
    let _file = lock(&engine.settings_file);
    if engine.quitting.load(Relaxed) {
        return Err("Backsight is closing. Reopen it to save your settings.".into());
    }
    for folder in [&settings.general.video_folder, &settings.general.screenshot_folder] {
        // A relative folder would land somewhere else when Windows starts Backsight at sign-in.
        // Untrimmed: Windows would read " D:\Clips" as a relative path.
        if !folder.is_empty() && !std::path::Path::new(folder).is_absolute() {
            return Err(format!("Pick a full folder path, like D:\\Clips, instead of \"{folder}\"."));
        }
    }
    // A broken or newer file is never overwritten (a missing one loads as the defaults).
    let old = Settings::load().map_err(|e| {
        format!(
            "Backsight didn't save, so your settings file is kept as it is. Fix or delete it, then try again. ({e})"
        )
    })?;
    settings.save().map_err(|e| e.to_string())?;
    engine.hide_indicator.store(!settings.general.recording_indicator, Relaxed);
    allow_library_files(&app, &settings); // before returning: the Library reloads after a save
    let autostart = (old.general.start_with_windows != settings.general.start_with_windows)
        .then_some(settings.general.start_with_windows);
    if lock(&engine.to_apply).offer(settings) {
        let app = app.clone();
        std::thread::spawn(move || apply_saved(&app));
    }
    autostart.map_or(Ok(()), backsight_engine::autostart::set)
}

/// Brings the engine in line with the saved settings, newest only: saves that arrive while one is
/// being applied (a session restart takes seconds) replace each other instead of waiting in line.
fn apply_saved(app: &AppHandle) {
    let engine = app.state::<Engine>();
    loop {
        let _apply = lock(&engine.apply);
        // Taken under `apply`: whatever was saved while waiting for it is skipped.
        let Some(settings) = lock(&engine.to_apply).take() else { return };
        if engine.quitting.load(Relaxed) {
            continue;
        }
        // `None`: startup couldn't read the file, so nothing runs yet and everything is new.
        let old = lock(&engine.applied).replace(settings.clone());
        let (session, hotkeys) = old.as_ref().map_or((true, true), |old| restart_needed(old, &settings));
        if old.as_ref().is_some_and(|old| old.replay.enabled != settings.replay.enabled) {
            lock(&engine.replay).toggled = None; // an explicit choice overrides the hotkey
        }
        if session {
            apply_replay(&engine, &settings);
        }
        let mut replay = lock(&engine.replay);
        if let Some(s) = replay.session.as_mut() {
            s.set_naming(&settings.general); // file names need no restart
        }
        let ptt = old.as_ref().is_some_and(push_to_talk) || push_to_talk(&settings);
        if hotkeys && recording(&replay) && (replay.pending_hotkeys || ptt) {
            // Replacing the matcher would drop the release edge for the recording's active binding.
            replay.pending_hotkeys = true;
        }
        let apply_hotkeys = hotkeys && !replay.pending_hotkeys;
        drop(replay);
        if apply_hotkeys {
            start_hotkeys(app, &settings);
        }
    }
}

/// The library folders, absolute.
fn library_dirs(settings: &Settings) -> [Option<PathBuf>; 2] {
    [settings.general.video_dir(), settings.general.screenshot_dir()]
        .map(|d| d.and_then(|d| std::path::absolute(d).ok()))
}

/// Home's Record and Screenshot tiles: the same as pressing that key, queued with the hotkeys.
#[tauri::command]
fn press_action(actions: tauri::State<Sender<Action>>, action: Action) -> Result<(), String> {
    match action {
        Action::ToggleRecording | Action::Screenshot => actions
            .try_send(action)
            .map_err(|_| "Backsight is still busy with the last key press. Try again in a moment.".into()),
        _ => Err("Only Record and Screenshot can be pressed from Home.".into()),
    }
}

/// Free and total bytes on the drive that holds the clips folder (Home's side menu), `None` when
/// Windows can't say. A folder not made yet counts as its nearest existing parent.
/// Off the main thread: a sleeping or network drive can take seconds to answer.
#[tauri::command]
async fn clips_space() -> Option<(u64, u64)> {
    use windows::{Win32::Storage::FileSystem::GetDiskFreeSpaceExW, core::HSTRING};
    let settings = load_settings().ok()?;
    let [videos, _] = library_dirs(&settings);
    spawn_blocking(move || {
        let dir = videos?.ancestors().find(|d| d.is_dir())?.to_owned();
        let (mut free, mut total) = (0u64, 0u64);
        // SAFETY: the path is a valid wide string and both outputs live across the call.
        unsafe { GetDiskFreeSpaceExW(&HSTRING::from(dir.as_os_str()), Some(&mut free), Some(&mut total), None) }
            .ok()?;
        Some((free, total))
    })
    .await
    .ok()
    .flatten()
}

/// Lets the WebView load thumbnails, screenshots and previews through the asset protocol.
// shortcut: a folder you switch away from stays readable until restart (Tauri's scope can only add a
// permanent forbid); fine while it only holds your own clips.
fn allow_library_files(app: &AppHandle, settings: &Settings) {
    let scope = app.asset_protocol_scope();
    for dir in library_dirs(settings).into_iter().chain([library::cache_dir().ok()]).flatten() {
        // Tauri matches the file's real path (case as on disk, mapped drives as UNC, links
        // resolved), so grant that too; the folder as written covers one created later.
        for dir in [std::fs::canonicalize(&dir).ok(), Some(dir)].into_iter().flatten() {
            if let Err(e) = scope.allow_directory(&dir, true) {
                tracing::warn!("the window can't show files in {}: {e}", dir.display());
            }
        }
    }
}

/// Connected monitors for the Video page; "Main monitor" is added by the page.
#[tauri::command]
async fn list_monitors() -> Result<Vec<backsight_engine::capture::wgc::Monitor>, String> {
    spawn_blocking(backsight_engine::capture::wgc::monitors).await.map_err(|e| e.to_string())
}

/// Plugged-in microphones for the Audio page; "Windows default" is added by the page.
#[tauri::command]
async fn list_microphones() -> Result<Vec<backsight_engine::audio::devices::MicDevice>, String> {
    spawn_blocking(backsight_engine::audio::devices::microphones).await.map_err(|e| e.to_string())?
}

/// Settings for the Library commands, with an error that says what to do.
fn load_settings() -> Result<Settings, String> {
    Settings::load().map_err(|e| {
        tracing::warn!("settings not loaded: {e}");
        format!("Backsight couldn't read your settings file. Fix or delete it, then restart Backsight. ({e})")
    })
}

/// Every saved video and screenshot, newest first. The first scan of new files reads each one.
#[tauri::command]
async fn list_library() -> Result<Library, String> {
    let settings = load_settings()?;
    let cache = library::cache_dir()?;
    let [videos, shots] = library_dirs(&settings);
    spawn_blocking(move || library::scan(videos.as_deref(), shots.as_deref(), &cache)).await.map_err(|e| e.to_string())
}

/// The hover preview MP4 of a library video; a second or two the first time.
#[tauri::command]
async fn library_preview(path: PathBuf) -> Result<PathBuf, String> {
    let settings = load_settings()?;
    let cache = library::cache_dir()?;
    spawn_blocking(move || {
        let resolved = library::resolve_file(&path, &library_dirs(&settings))?;
        library::preview(&path, &resolved, &cache)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn library_action(action: String, path: Option<PathBuf>) -> Result<Option<PathBuf>, String> {
    use backsight_engine::system;

    let roots = library_dirs(&load_settings()?);
    spawn_blocking(move || {
        if matches!(action.as_str(), "clips" | "screenshots") {
            let dir = roots[usize::from(action == "screenshots")]
                .as_ref()
                .and_then(|dir| std::fs::canonicalize(dir).ok())
                .filter(|dir| dir.is_dir())
                .ok_or("That folder is missing. Check the save folders in General.")?;
            system::open_path(&dir, false).map_err(|e| {
                tracing::warn!("Library folder couldn't open: {e}");
                "Windows couldn't open that folder."
            })?;
            return Ok(None);
        }
        let path = path.ok_or("Choose a file in the Library first.")?;
        let real = library::resolve_file(&path, &roots)?;
        let result = match action.as_str() {
            "view" => return Ok(Some(real)),
            "open" => system::open_path(&real, false),
            "reveal" => system::open_path(&real, true),
            "copy_path" => system::copy_path(&path),
            _ => return Err("That Library action isn't available.".into()),
        };
        result.map_err(|e| {
            tracing::warn!("Library {action} failed: {e}");
            match action.as_str() {
                "copy_path" => "Couldn't copy the file path. Close any clipboard dialog and try again.",
                "reveal" => "Windows couldn't show that file in its folder.",
                _ => "Windows couldn't open that file. Check which app is set to open it.",
            }
        })?;
        Ok(None)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Runs the same replay commands as the hotkeys, without blocking the window.
#[tauri::command]
async fn replay_menu_action(app: AppHandle, action: String) -> Result<(), String> {
    spawn_blocking(move || match action.as_str() {
        "on" | "off" => {
            set_replay(&app.state::<Engine>(), Some(action == "on"));
            Ok(())
        }
        "save" => {
            on_hotkey(&app, Action::SaveReplay);
            Ok(())
        }
        _ => Err("That replay action isn't available.".into()),
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Never waits for replay: if its lock is busy or a session is starting, returns the last status. Off the main thread, and
/// one at a time: `diagnostics()` takes session locks the video thread holds across GPU calls, so a
/// stuck driver would freeze the window, or hold one more worker with every poll.
#[tauri::command(async)]
fn get_status(engine: tauri::State<Engine>) -> Status {
    let ptt_live = engine.ptt_live.load(Relaxed);
    if engine.status_busy.swap(true, Relaxed) {
        return lock(&engine.last_status).clone().unwrap_or_else(|| status(&Replay::default(), ptt_live));
    }
    let mut status = match engine.replay.try_lock() {
        // Starting: keep showing the last status until the session is open or has failed.
        Ok(replay) if !replay.starting => status(&replay, ptt_live),
        Err(TryLockError::Poisoned(e)) => status(&e.into_inner(), ptt_live),
        _ => lock(&engine.last_status).clone().unwrap_or_else(|| status(&Replay::default(), ptt_live)),
    };
    status.hotkey_problem = lock(&engine.hotkey_problem).clone();
    *lock(&engine.last_status) = Some(status.clone());
    engine.status_busy.store(false, Relaxed);
    status
}

/// What the Diagnostics page shows: the session's own counters, read only.
#[derive(Serialize)]
struct Diag {
    buffer_seconds: f64,
    buffer_bytes: usize,
    /// Average size of what the buffer holds, video and audio together; `None` while it is empty.
    buffer_kbps: Option<u32>,
    encoded_frames: u64,
    encoder_queue: u64,
    dropped_frames: u64,
    repeated_frames: u64,
    skipped_frames: u64,
    late_frames: u64,
    audio: Option<DiagAudio>,
    audio_problem: Option<String>,
    /// `None` when not recording.
    recording: Option<DiagRecording>,
}

#[derive(Serialize)]
struct DiagRecording {
    written_kbps: Option<u32>,
    queued_bytes: usize,
    queue_limit: usize,
}

#[derive(Serialize)]
struct DiagAudio {
    silence_frames: u64,
    trimmed_frames: u64,
    jumped_frames: u64,
    discontinuities: u64,
    device_errors: u64,
}

/// Never waits for replay: an error while its lock is busy (the page keeps what it had), `None`
/// when nothing is running. Off the main thread and one at a time, like `get_status`.
#[tauri::command(async)]
fn get_diagnostics(engine: tauri::State<Engine>) -> Result<Option<Diag>, &'static str> {
    if engine.diagnostics_busy.swap(true, Relaxed) {
        return Err("busy");
    }
    let d = diagnostics(&engine);
    engine.diagnostics_busy.store(false, Relaxed);
    d
}

fn diagnostics(engine: &Engine) -> Result<Option<Diag>, &'static str> {
    let replay = match engine.replay.try_lock() {
        Ok(replay) => replay,
        Err(TryLockError::Poisoned(e)) => e.into_inner(),
        Err(TryLockError::WouldBlock) => return Err("busy"),
    };
    if replay.starting {
        return Err("busy");
    }
    let Some(session) = &replay.session else { return Ok(None) };
    let d = session.diagnostics();
    let secs = d.buffer.as_secs_f64();
    Ok(Some(Diag {
        buffer_seconds: secs,
        buffer_bytes: d.buffer_bytes,
        buffer_kbps: (secs >= 1.0).then(|| (d.buffer_bytes as f64 * 8.0 / 1000.0 / secs) as u32),
        encoded_frames: d.encoded_frames,
        encoder_queue: d.encoder_queue,
        dropped_frames: d.pacer.dropped,
        repeated_frames: d.pacer.repeated,
        skipped_frames: d.pacer.skipped,
        late_frames: d.late_frames,
        audio: d.audio.map(|a| DiagAudio {
            silence_frames: a.timeline.silence_frames,
            trimmed_frames: a.timeline.trimmed_frames,
            jumped_frames: a.timeline.jumped_frames,
            discontinuities: a.discontinuities,
            device_errors: a.device_errors,
        }),
        audio_problem: d.audio_problem,
        recording: d.recording.map(|r| DiagRecording {
            written_kbps: r.written_kbps,
            queued_bytes: r.queued_bytes,
            queue_limit: r.queue_limit,
        }),
    }))
}

fn status(replay: &Replay, ptt_live: bool) -> Status {
    let mut status = Status {
        state: "off",
        message: String::new(),
        buffer_seconds: 0.0,
        buffer_bytes: 0,
        dropped_frames: 0,
        late_frames: 0,
        recording: false,
        recording_seconds: 0.0,
        recording_paused: false,
        recording_message: String::new(),
        pending_apply: replay.pending_apply || replay.pending_hotkeys,
        audio_problem: None,
        mic_muted: false,
        game_audio_from: None,
        capture_notice: None,
        hotkey_problem: None,
        game: None,
        update: update::ready(),
    };
    if let Some(session) = &replay.session
        && let Some(t) = session.recording_time()
    {
        status.recording = true;
        status.recording_seconds = t.as_secs_f64();
        status.recording_paused = session.recording_paused();
        status.recording_message = session.recording_error().or_else(|| session.error()).unwrap_or_default();
    }
    if let Some(session) = &replay.session {
        let d = session.diagnostics();
        status.audio_problem = d.audio_problem.or_else(|| {
            (replay.ptt && !ptt_live).then(|| {
                "Your push-to-talk key isn't working, so the microphone stays closed. Check it on Backsight's Hotkeys page.".into()
            })
        });
        status.mic_muted = replay.mic_muted && session.records_mic();
        status.game_audio_from = session.game_audio_target().map(|t| t.name().to_owned());
        status.capture_notice = session.capture_notice();
        status.game = session.game_name();
        if !session.replay_on() {
            return status;
        }
        status.buffer_seconds = d.buffer.as_secs_f64();
        status.buffer_bytes = d.buffer_bytes;
        status.dropped_frames = d.pacer.dropped;
        status.late_frames = d.late_frames;
        match session.error() {
            Some(e) => (status.state, status.message) = ("failed", e),
            None => status.state = "on",
        }
    } else if let Some(e) = &replay.start_error {
        (status.state, status.message) = ("failed", e.clone());
    }
    status
}

/// "Keep running in the tray when the window closes", `None` when the settings can't be read.
fn minimize_to_tray() -> Option<bool> {
    Settings::load().ok().map(|s| s.general.minimize_to_tray)
}

/// The main window closed while pop-ups are still open: quit now when the user doesn't want the
/// tray, because the pop-ups' own closing would never ask to exit. An unreadable file keeps running.
fn quits_with_main_window(others_open: bool, minimize_to_tray: Option<bool>) -> bool {
    others_open && !minimize_to_tray.unwrap_or(true)
}

/// The last window closed (which asks to exit): stay in the tray, or when only a pop-up was last.
fn keeps_running(minimize_to_tray: Option<bool>, helper_closed_last: bool) -> bool {
    minimize_to_tray.unwrap_or(true) || helper_closed_last
}

/// Shows the main window, recreating it if it was closed.
fn show_main_window(app: &AppHandle) {
    if app.state::<Engine>().quit_started.load(Relaxed) {
        return; // it would vanish again in a moment
    }
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    if let Some(config) = app.config().app.windows.first()
        && let Err(e) = WebviewWindowBuilder::from_config(app, config).and_then(|b| b.build())
    {
        eprintln!("could not open the Backsight window: {e}");
    }
}

/// A crash report saved by the last run that the user hasn't dismissed (`crash.rs`).
#[tauri::command(async)]
fn crash_notice() -> Option<String> {
    backsight_engine::crash::pending(&backsight_engine::crash::reports_dir()?)
}

#[tauri::command(async)]
fn crash_dismiss() {
    if let Some(dir) = backsight_engine::crash::reports_dir() {
        backsight_engine::crash::dismiss(&dir);
    }
}

#[tauri::command(async)]
fn crash_open_folder() {
    open_folder(backsight_engine::crash::reports_dir());
}

/// Opens the folder with Backsight's logs, for sending them along with a problem.
#[tauri::command(async)]
fn open_logs_folder() {
    open_folder(backsight_config::app_dir().ok().map(|d| d.join("logs")));
}

/// General's About links: the license notices installed beside Backsight, or its source code page
/// (`bundle.homepage` in tauri.conf.json, the one place that address is kept).
#[tauri::command(async)]
fn about_open(app: AppHandle, what: String) -> Result<(), String> {
    match what.as_str() {
        "licenses" => {
            let path = app.path().resource_dir().map_err(|e| e.to_string())?.join("THIRD-PARTY-NOTICES.txt");
            if !path.is_file() {
                return Err("The licenses file is missing. Reinstall Backsight to get it back.".into());
            }
            backsight_engine::system::open_path(&path, false).map_err(|e| {
                tracing::warn!("licenses couldn't open: {e}");
                "Windows couldn't open the licenses file. Check which app is set to open .txt files.".to_owned()
            })
        }
        "source" => {
            let url = app.config().bundle.homepage.clone().ok_or("No source code address is set.")?;
            // explorer.exe opens a web address in the default browser, without a console window.
            std::process::Command::new(windows_dir().join("explorer.exe")).arg(url).spawn().map(drop).map_err(|e| {
                tracing::warn!("source page couldn't open: {e}");
                "Windows couldn't open your web browser.".to_owned()
            })
        }
        _ => Err("That link isn't available.".into()),
    }
}

fn open_folder(dir: Option<PathBuf>) {
    if let Some(dir) = dir {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::process::Command::new(windows_dir().join("explorer.exe")).arg(dir).spawn();
    }
}

/// The Windows folder. Programs are started by full path: a bare name is looked up in Backsight's
/// own (user-writable) folder first.
pub(crate) fn windows_dir() -> PathBuf {
    std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
}

/// The installer's "close Backsight and continue" (windows/installer.nsi).
const QUIT_FLAG: &str = "--quit";

/// Quits without holding up the window or Windows' messages: the tray icon and the windows go at
/// once, a worker finishes a running recording and the files still being written, then the
/// process ends.
// shortcut: opening Backsight again in the seconds the worker needs does nothing; show a "closing" note if people hit it.
fn quit(app: &AppHandle) {
    let engine = app.state::<Engine>();
    if engine.quit_started.swap(true, Relaxed) {
        return;
    }
    engine.quitting.store(true, Relaxed);
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_visible(false);
    }
    for window in app.webview_windows().values() {
        let _ = window.hide();
    }
    let app = app.clone();
    std::thread::spawn(move || {
        shut_down(&app.state::<Engine>());
        app.exit(0);
    });
}

/// Like `quit`, but on the calling thread and without ending the process: the updater starts the
/// installer and ends it once this returns.
pub(crate) fn close_for_update(app: &AppHandle) {
    let engine = app.state::<Engine>();
    engine.quit_started.store(true, Relaxed);
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_visible(false);
    }
    for window in app.webview_windows().values() {
        let _ = window.hide();
    }
    shut_down(&engine);
}

/// Finishes a running recording's file instead of leaving a .part behind, and waits for the files
/// still being written. Once: `quit`'s worker does it, and the `Exit` event finds it done (or
/// waits for it, or does it itself at sign-out and shutdown).
fn shut_down(engine: &Engine) {
    let mut done = lock(&engine.shut_down);
    if std::mem::replace(&mut *done, true) {
        return;
    }
    // At sign-out and shutdown this runs inside WM_ENDSESSION, after Windows decided to go
    // ahead: a running recording never holds shutdown off (it's crash-safe by default and
    // recovered at the next start). This only asks for time to finish what it can.
    let _saving = backsight_engine::shutdown::saving();
    // Set first too: whoever holds `apply` sees it as soon as it looks.
    engine.quitting.store(true, Relaxed);
    // First: a job woken by `captions::stop` must already see that Backsight is quitting.
    vertical::stopping();
    captions::stop();
    let stopping = {
        let _apply = lock(&engine.apply);
        let mut replay = lock(&engine.replay);
        // Stopped like the hotkey does, so its MKV is marked for conversion at the next start.
        let stopping = replay.session.as_mut().and_then(ReplaySession::begin_stop_recording);
        stop_replay(engine, &mut replay);
        stopping
    };
    if let Some(stopping) = stopping {
        mark_for_mp4(stopped_notice(stopping.finish()).2);
    }
    // A stopped recording or a clip may still be finishing (faststart, the marker copy):
    // the process ends after this, which would cut its file off.
    let threads = {
        let mut finishing = lock(&engine.finishing);
        finishing.closed = true; // anything that got in first is waited for below
        std::mem::take(&mut finishing.threads)
    };
    for t in threads {
        let _ = t.join();
    }
}

fn main() {
    if let Some(code) = cli::run() {
        std::process::exit(code);
    }
    webview_check::ensure_installed();
    let app = tauri::Builder::default()
        .manage(Engine::default())
        .plugin(tauri_plugin_dialog::init())
        // `--quit` comes from the installer: close the running copy the way Quit does (a running
        // recording is finished first). Any other second start shows the window.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if args.iter().any(|a| a == QUIT_FLAG) { quit(app) } else { show_main_window(app) }
        }))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .on_page_load(|webview, _| uiwatch::watch_webview(webview))
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            get_status,
            clips_space,
            press_action,
            replay_menu_action,
            get_diagnostics,
            list_microphones,
            captions::captions_status,
            captions::download_captions,
            captions::stop_captions_download,
            captions::remove_captions,
            list_monitors,
            list_library,
            library_preview,
            library_action,
            region_image,
            region_show,
            region_pick,
            hud_poll,
            hud_idle,
            hud_drag,
            hud_arrange,
            local_builds,
            install_local_build,
            update::check_update,
            update::install_update,
            setup::encoder_check,
            setup::setup_check,
            setup::overload_status,
            setup::lighter_video,
            setup::nvidia_overlay_keys,
            crash_notice,
            crash_dismiss,
            crash_open_folder,
            open_logs_folder,
            about_open,
            pause_hotkeys,
            report::save_problem_report,
            uiwatch::ui_log,
            uiwatch::ui_heartbeat
        ])
        .setup(|app| {
            // Nothing was running to quit: don't start one.
            if std::env::args().any(|a| a == QUIT_FLAG) {
                std::process::exit(0);
            }
            update::check_in_background(app.handle().clone());
            let (actions, mut queued) = async_runtime::channel::<Action>(32);
            app.manage(actions);
            let handle = app.handle().clone();
            async_runtime::spawn(async move {
                while let Some(action) = queued.recv().await {
                    let app = handle.clone();
                    // Keep action order, including taking a save snapshot before the next toggle.
                    if let Err(e) = spawn_blocking(move || on_hotkey(&app, action)).await {
                        tracing::error!("hotkey action failed: {e}");
                    }
                }
            });
            // Here, not before the builder: a second launch exits while the single-instance plugin
            // loads, before `setup`, and must not rotate the running instance's log.
            // Crash rules first: the log scrubs its lines with them.
            // Before anything reads settings, the crash rules included (they scrub the clip folders):
            // carry over what the app saved under its old name.
            let adopted = Settings::adopt_old_name();
            ADOPTED_OLD_NAME.store(matches!(adopted, Ok(true)), Relaxed);
            let build = this_build().unwrap_or_default();
            backsight_engine::crash::install(&build);
            let _ = backsight_engine::logging::init();
            let version = backsight_engine::crash::version_line(&build);
            tracing::info!("Backsight {version}");
            match adopted {
                Ok(true) => tracing::info!("copied the settings saved under the old name Clipwise"),
                Ok(false) => {}
                Err(e) => tracing::warn!("couldn't copy the settings saved under the old name: {e}"),
            }
            if !cfg!(debug_assertions)
                && let Err(e) = backsight_engine::autostart::adopt_old_name()
            {
                tracing::warn!("{e}");
            }
            uiwatch::start(app.handle(), version); // `setup` runs on the main thread, which it watches
            let open = MenuItem::with_id(app, "open", "Open Backsight", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit Backsight", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit])?;
            let mut tray = TrayIconBuilder::with_id("main")
                .tooltip("Backsight")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_main_window(app),
                    "quit" => crate::quit(app),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left, button_state: MouseButtonState::Up, ..
                    } = event
                    {
                        show_main_window(tray.app_handle());
                    }
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;
            captions::start(app.handle());
            // Start with Windows passes --minimized: only the tray icon then.
            if !std::env::args().any(|a| a == "--minimized") {
                show_main_window(app.handle());
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build the Backsight app");

    app.run(|app, event| {
        // Start after `build()`: tao registers Raw Input in `EventLoop::new` and Tauri again in
        // `build()`, both before `setup`, and the last registration wins. Never call
        // `set_device_event_filter`.
        if let RunEvent::Ready = event {
            let app = app.clone();
            std::thread::spawn(move || {
                let engine = app.state::<Engine>();
                let _apply = lock(&engine.apply);
                if engine.quitting.load(Relaxed) {
                    return;
                }
                let file = lock(&engine.settings_file); // a save may be in progress
                // A settings file that fails to load starts nothing; the UI shows the error.
                let Ok(mut settings) = Settings::load() else { return };
                // Here, not in `setup` on the main thread: resolving a folder on an offline network
                // drive can take a long time.
                allow_library_files(&app, &settings);
                engine.hide_indicator.store(!settings.general.recording_indicator, Relaxed);
                // Only refreshes an entry that exists (points it at this exe after an update or a move).
                // A stored `true` may be the old default, so it never creates one. When the entry is gone
                // (removed by hand, or uninstalled), the registry wins and the setting turns off.
                // Dev builds never touch it.
                if !cfg!(debug_assertions) {
                    // Just copied from Clipwise: the old entry may already be gone (its uninstaller
                    // removes it), so the copied setting wins this once.
                    let adopted = ADOPTED_OLD_NAME.load(Relaxed) && settings.general.start_with_windows;
                    if backsight_engine::autostart::is_enabled() || adopted {
                        if let Err(e) = backsight_engine::autostart::set(settings.general.start_with_windows) {
                            tracing::warn!("{e}");
                        }
                    } else if settings.general.start_with_windows {
                        settings.general.start_with_windows = false;
                        if let Err(e) = settings.save() {
                            tracing::warn!("{e}");
                        }
                    }
                }
                *lock(&engine.applied) = Some(settings.clone());
                drop(file);
                // Before the hotkeys: a clip saved from here on has its own job.
                let verticals = settings.general.video_dir().map(|f| vertical::unfinished(&f)).unwrap_or_default();
                verticals.into_iter().for_each(|job| make_vertical(&app, job, settings.clone()));
                start_hotkeys(&app, &settings);
                apply_replay(&engine, &settings);
                // Not under the startup lock: copying a long recording takes a while.
                // Registered in `finishing`, so quitting mid-copy doesn't leave a half-written file.
                if let Some(folder) = settings.general.video_dir() {
                    // Not in `finishing`: quitting never waits for a conversion, and the next start resumes it.
                    let mkvs = backsight_engine::recovery::unfinished_conversions(&folder);
                    if settings.video.remux_to_mp4 {
                        let handle = app.clone();
                        std::thread::spawn(move || convert_to_mp4(&handle, &mkvs));
                    } else {
                        // Turned off since: nothing will finish them, so their leftovers go (the MKVs stay).
                        mkvs.iter().for_each(|mkv| backsight_engine::recovery::drop_conversion(mkv));
                    }
                    let handle = app.clone();
                    spawn_finishing(&engine, move || recover_recordings(&handle, &folder));
                }
                drop(_apply);
                // With Instant Replay on, starting it just paid for the slow first encoder open.
                if !settings.replay.enabled {
                    std::thread::spawn(move || backsight_engine::session::warm_up(&settings));
                }
                watch_recording(&app);
            });
            return;
        }
        if let RunEvent::Exit = event {
            shut_down(&app.state::<Engine>());
            return;
        }
        // Comes before the `ExitRequested` that the last window's close sends.
        if let RunEvent::WindowEvent { label, event: WindowEvent::Destroyed, .. } = &event {
            let engine = app.state::<Engine>();
            uiwatch::forget(label);
            if label == "picker" {
                *lock(&engine.region) = None; // closed some other way, such as Alt+F4
            }
            engine.helper_closed_last.store(["picker", "hud", "dot"].contains(&label.as_str()), Relaxed);
            // A pop-up that arrived while the idle hud was closing found it still open.
            // So did one that was closing when moving started.
            let moving = engine.arranging.load(Relaxed);
            if label == "hud" && (moving || !lock(&engine.hud_queue).is_empty()) {
                open_hud(app, HudPart::Popups);
            }
            if label == "dot" && moving {
                open_hud(app, HudPart::Dot);
            }
            if label == "main" {
                engine.hotkeys_paused.store(false, Relaxed); // closed while a hotkey box listened
            }
            // The settings page that was moving the hud is gone: let the mouse through again.
            if label == "main" && engine.arranging.swap(false, Relaxed) {
                for part in HudPart::ALL {
                    if let Some(w) = app.get_webview_window(part.label()) {
                        let _ = w.set_ignore_cursor_events(true);
                    }
                }
            }
            // Closed because its WebView2 failed: open a new one (after this event, once it's gone).
            if label == "main" && uiwatch::recreating() {
                let handle = app.clone();
                let posted = app.run_on_main_thread(move || {
                    show_main_window(&handle);
                    uiwatch::recreated();
                });
                if posted.is_err() {
                    uiwatch::recreated();
                }
                return;
            }
            // Quit-on-close must not wait for a pop-up to fade: nothing else would ask to exit then.
            let others = app.webview_windows().keys().any(|l| l != "main");
            if label == "main" && quits_with_main_window(others, minimize_to_tray()) {
                quit(app);
            }
            return;
        }
        // Closing the last window asks to exit with no code; `quit` passes an explicit one when
        // its worker is done, so the exit asked for here never finishes files on this thread.
        if let RunEvent::ExitRequested { code: None, api, .. } = event {
            api.prevent_exit();
            if !uiwatch::recreating()
                && !keeps_running(minimize_to_tray(), app.state::<Engine>().helper_closed_last.load(Relaxed))
            {
                quit(app);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_to_talk_release_reaches_the_gate_while_replay_is_locked() {
        let engine = Engine::default();
        let (events, edges) = std::sync::mpsc::channel();
        lock(&engine.mic_key).control = Some(Box::new(move |down| events.send(down).unwrap()));
        set_mic_key(&engine, true);
        assert!(edges.recv().unwrap());

        std::thread::scope(|scope| {
            let replay = lock(&engine.replay);
            scope.spawn(|| set_mic_key(&engine, false));
            let release = edges.recv_timeout(std::time::Duration::from_secs(1));
            drop(replay);
            assert_eq!(release, Ok(false));
        });
        assert!(!lock(&engine.mic_key).down);
    }

    #[test]
    fn an_unfinished_earlier_part_is_told() {
        let stopped = |unfinished_parts| backsight_engine::session::Stopped {
            file: Ok(PathBuf::from("c 3.mp4")),
            earlier_parts: vec![PathBuf::from("c 1.mp4")],
            unfinished_parts,
            stopped_early: None,
            markers_lost: false,
        };
        let (title, body, _) = stopped_notice(stopped(vec![]));
        assert_eq!(
            (title, body.as_str()),
            ("Recording saved", "Watch it in Backsight's Library. (c 3.mp4 and 1 earlier part(s))")
        );
        let lost = stopped_notice(backsight_engine::session::Stopped { markers_lost: true, ..stopped(vec![]) });
        assert!(lost.1.starts_with("Your markers couldn't be added"), "{}", lost.1);
        let both = stopped_notice(backsight_engine::session::Stopped { markers_lost: true, ..stopped(vec![2]) }).1;
        assert!(
            both.starts_with("Part 2 of") && both.contains("markers") && both.ends_with("earlier part(s)"),
            "{both}"
        );
        let (title, body, saved) = stopped_notice(stopped(vec![2]));
        assert_eq!(title, "Part of the recording wasn't finished");
        assert!(
            body.starts_with("Part 2 of your recording couldn't be finished.")
                && body.ends_with("c 3.mp4 and 1 earlier part(s)")
        );
        assert_eq!(saved.len(), 2);
    }

    #[test]
    fn hud_windows_open_where_they_were_left_and_stay_on_screen() {
        let monitor = (1920.0, 0.0, 2560.0, 1440.0); // a second monitor, right of the first
        assert_eq!(hud_spot(HudPart::Dot, None, monitor), (1920.0 + 2560.0 - 220.0 - 16.0, 16.0));
        assert_eq!(hud_spot(HudPart::Popups, None, monitor), (1920.0 + 2560.0 - 360.0 - 16.0, 64.0), "under the dot");
        assert_eq!(
            hud_spot(HudPart::Dot, Some([0.5, 0.25]), monitor),
            (1920.0 + (2560.0 - 220.0) / 2.0, (1440.0 - 48.0) / 4.0)
        );
        // Pushed against the bottom-right edge: still against it on a smaller screen.
        let laptop = (0.0, 0.0, 1536.0, 864.0);
        assert_eq!(hud_spot(HudPart::Popups, Some([1.0, 1.0]), monitor), (1920.0 + 2560.0 - 360.0, 1440.0 - 400.0));
        assert_eq!(hud_spot(HudPart::Popups, Some([1.0, 1.0]), laptop), (1536.0 - 360.0, 864.0 - 400.0));
        // Edited by hand: still fully on screen.
        assert_eq!(hud_spot(HudPart::Dot, Some([-1.0, f64::NAN]), monitor), hud_spot(HudPart::Dot, None, monitor));
        assert_eq!(hud_spot(HudPart::Dot, Some([-1.0, 7.0]), monitor), (1920.0, 1440.0 - 48.0));
    }

    #[test]
    fn every_title_gets_its_own_pop_up_and_sound() {
        assert_eq!(kind_of("Recording started"), ("record", Cue::Start));
        assert_eq!(kind_of("Clip saved"), ("saved", Cue::Saved));
        assert_eq!(kind_of("Screenshot saved"), ("screenshot", Cue::Screenshot));
        assert_eq!(kind_of("Microphone muted"), ("mic-off", Cue::MicOff));
        assert_eq!(kind_of("Not recording"), ("info", Cue::Quiet));
        // Any title not listed is a problem: a new failure message gets the warning, never silence.
        assert_eq!(kind_of("Couldn't save the clip"), ("problem", Cue::Problem));
        assert_eq!(kind_of("Recording stopped early"), ("problem", Cue::Problem));
    }

    #[test]
    fn the_recording_tray_icon_has_a_red_dot_in_the_corner() {
        let icon = Image::new_owned(vec![0x40; 32 * 32 * 4], 32, 32);
        let red = recording_icon(&icon);
        let px = |x: usize, y: usize| &red.rgba()[(y * 32 + x) * 4..][..4];
        assert_eq!(px(25, 25), [0xE5, 0x2B, 0x2B, 0xFF], "red near the corner");
        assert_eq!(px(4, 4), [0x40; 4], "the rest is untouched");
    }

    #[test]
    fn instant_replay_restarts_a_few_times_then_says_it_stays_off() {
        let t0 = std::time::Instant::now();
        let at = |s: u64| t0 + std::time::Duration::from_secs(s);
        let mut r = Revival::default();
        assert_eq!(r.step(false, &[], at(0)), None, "running fine");
        assert_eq!(r.step(true, &[], at(1)), Some(Revive::Restart { tell: true }), "told once");
        assert_eq!(r.step(true, &[], at(2)), None, "waits between restarts");
        assert_eq!(r.step(true, &[], at(6)), Some(Revive::Restart { tell: false }));
        assert_eq!(r.step(true, &[], at(11)), None, "waits longer each time");
        assert_eq!(r.step(true, &[], at(21)), Some(Revive::Restart { tell: false }));
        assert_eq!(r.step(true, &[], at(80)), None);
        assert_eq!(r.step(true, &[], at(81)), Some(Revive::GiveUp));
        assert_eq!(r.step(true, &[], at(200)), None, "gave up once, quietly after that");

        // A restart that keeps working a minute starts a fresh count.
        let mut r = Revival::default();
        r.step(true, &[], at(0));
        assert!(r.restarted());
        assert_eq!(r.step(false, &[], at(59)), None);
        assert!(r.restarted());
        r.step(false, &[], at(60));
        assert!(!r.restarted());
        assert_eq!(r.step(true, &[], at(61)), Some(Revive::Restart { tell: true }));
    }

    #[test]
    fn back_on_is_said_only_when_it_runs_again_not_after_turning_it_off() {
        let mut r = Revival { told_off: true, ..Revival::default() };
        assert!(!r.back(None), "still stopped");
        assert!(!r.back(Some(false)), "turned off by the user");
        assert!(!r.back(Some(true)), "turned on later by the user: nothing to say");
        let mut r = Revival { told_off: true, ..Revival::default() };
        assert!(r.back(Some(true)));
        assert!(!r.back(Some(true)), "said once");
    }

    /// Runs the watcher's once-a-second revival against a fake monitor for `seconds`: unplugged
    /// during `gone`, and capture starts work from `ready` seconds after it's back. Returns
    /// (starts tried, "stopped" pop-ups, "stays off" pop-ups, "back on" pop-ups, first second
    /// replay runs again after the monitor is back).
    fn unplug(gone: std::ops::Range<u64>, ready: u64, seconds: u64) -> (u32, u32, u32, u32, Option<u64>) {
        let t0 = std::time::Instant::now();
        let mut r = Revival::default();
        let (mut running, mut starts, mut stopped, mut off, mut back, mut again) = (true, 0, 0, 0, 0, None);
        for s in 0..seconds {
            let plugged = !gone.contains(&s);
            let monitors: Vec<String> = if plugged { vec![r"\\?\DISPLAY#ONE".into()] } else { Vec::new() };
            running &= plugged; // its capture item closed
            back += u32::from(r.back(running.then_some(true)));
            match r.step(!running, &monitors, t0 + std::time::Duration::from_secs(s)) {
                Some(Revive::Restart { tell }) => {
                    starts += 1;
                    stopped += u32::from(tell);
                    running = plugged && s >= gone.end + ready;
                }
                Some(Revive::GiveUp | Revive::GiveUpChanging) => off += 1,
                None => {}
            }
            if running && s >= gone.end {
                again.get_or_insert(s);
            }
        }
        (starts, stopped, off, back, again)
    }

    #[test]
    fn instant_replay_comes_back_when_its_monitor_is_plugged_back_in() {
        // Back within a minute: told once, running again soon after, never "stays off".
        let (starts, stopped, off, back, again) = unplug(10..40, 0, 200);
        assert_eq!((stopped, off, back), (1, 0, 0));
        assert!(again.is_some_and(|s| s <= 45), "back at {again:?}");
        assert!(starts <= 4, "{starts} starts");

        // Gone for ten minutes: it gives up once, then comes back when the monitor does.
        let (starts, stopped, off, back, again) = unplug(10..610, 0, 700);
        assert_eq!((stopped, off, back), (1, 1, 1), "one pop-up each");
        assert!(again.is_some_and(|s| s <= 615), "back at {again:?}");
        assert!(starts <= 5, "no restarts while it's gone after giving up: {starts}");

        // Windows lists the monitor a few seconds before capture works on it.
        let (_, stopped, off, back, again) = unplug(10..610, 4, 700);
        assert_eq!((stopped, off, back), (1, 1, 1));
        assert!(again.is_some_and(|s| s <= 630), "back about 20 s after replugging: {again:?}");
    }

    #[test]
    fn monitors_that_keep_changing_never_restart_in_a_loop() {
        let t0 = std::time::Instant::now();
        let mut r = Revival::default();
        let (mut starts, mut last, mut told) = (0, 0, Vec::new());
        for s in 0..3600u64 {
            // A monitor blinking every few seconds while capture keeps failing to start.
            let monitors = vec![format!("monitor {}", s / 3)];
            match r.step(true, &monitors, t0 + std::time::Duration::from_secs(s)) {
                Some(Revive::Restart { .. }) => (starts, last) = (starts + 1, s),
                Some(other) => told.push(other),
                None => {}
            }
        }
        assert!(starts <= REPLAY_RESTARTS * (1 + MONITOR_ROUNDS), "a few rounds, then it stops: {starts}");
        assert!(last < 300, "nothing after the last round: {last}");
        assert_eq!(told, [Revive::GiveUpChanging], "told once, and why");

        // Working again for a minute starts afresh: a later unplug gets its rounds again.
        r.step(false, &[], t0 + std::time::Duration::from_secs(3600 + 60));
        assert_eq!(r.step(true, &[], t0 + std::time::Duration::from_secs(3700)), Some(Revive::Restart { tell: true }));
    }

    #[test]
    fn the_replay_from_before_a_move_is_saved_with_the_next_clip() {
        use backsight_engine::replay::{Packet, StreamKind};
        let (t0, s) = (std::time::Instant::now(), std::time::Duration::from_secs);
        // A minute of video kept by a move (`restart_replay`): a keyframe every 10 s, 1000 bytes a second.
        let kept = |at, from| {
            let second = |t: i64| Packet {
                stream: StreamKind::Video,
                pts_us: t * 1_000_000,
                dts_us: t * 1_000_000,
                duration_us: 0,
                keyframe: t % 10 == 0,
                data: vec![0; 1000].into(),
            };
            let clip = Clip::new((0..60).map(second).collect(), |_| Ok(PathBuf::from("before.mp4")));
            Kept { at, length: s(60), from, clip }
        };
        let mut before_move = vec![kept(t0, 0), kept(t0 + s(60), 1)];
        keep_recent(&mut before_move, t0 + s(60), None);
        assert_eq!(before_move.len(), 1, "older than the replay length: nothing of it would be in a clip");
        keep_recent(&mut before_move, t0 + s(80), None);
        assert_eq!(before_move[0].clip.bytes(), 50_000, "the last 40 s, from the keyframe before them");
        keep_recent(&mut before_move, t0 + s(80), Some(49_000));
        assert_eq!(
            before_move[0].clip.bytes(),
            40_000,
            "trimmed from its start, a keyframe at a time, to fit the RAM left"
        );
        keep_recent(&mut before_move, t0 + s(80), Some(5_000));
        assert!(before_move.is_empty(), "not even its last 10 s fit");
        // A monitor that keeps sleeping and waking: only the newest few are kept.
        let mut before_move: Vec<_> = (0..5).map(|i| kept(t0 + s(i), i)).collect();
        keep_recent(&mut before_move, t0 + s(5), None);
        assert_eq!(before_move.iter().map(|k| k.from).collect::<Vec<_>>(), [3, 4]);

        // A save during the move, before the new session took over: the running one has it all.
        assert!(kept_to_save(&mut before_move, Some(4)).len() == 1 && before_move.is_empty(), "taken, once");
        let mut before_move = vec![kept(t0, 4)];
        let earlier: Vec<_> = kept_to_save(&mut before_move, Some(5)).into_iter().map(Clip::save).collect();
        let clip = |name: &str| Ok(PathBuf::from(name));
        let (title, body) = saved_notice(&earlier, clip("now.mp4"), false);
        assert_eq!(title, "Clips saved");
        assert!(body.contains("now.mp4") && body.contains("separate clip"), "{body}");

        assert_eq!(saved_notice(&[], clip("a.mp4"), false).0, "Clip saved");
        let starting = "Instant Replay is still starting. Try again in a moment.".to_owned();
        let (title, body) = saved_notice(&[clip("before.mp4")], Err(starting.clone()), false);
        assert!(title == "Clip saved" && body.contains("before.mp4"), "the new session isn't up yet: {body}");
        let full = "Backsight couldn't save the clip. Check that the drive has free space.".to_owned();
        assert_eq!(saved_notice(&[Err(full.clone())], Err(starting), false), ("Couldn't save the clip", full.clone()));
        let (title, body) = saved_notice(&[Err(full.clone())], clip("now.mp4"), false);
        assert!(title == "Clip saved" && body.contains("now.mp4") && body.ends_with(&full), "{body}");
        let (_, body) = saved_notice(&[clip("before.mp4")], clip("now.mp4"), true);
        assert_eq!(body, "Making the vertical clip now.");
    }

    #[test]
    fn a_failed_move_to_the_returning_monitor_is_retried() {
        let t0 = std::time::Instant::now();
        let at = |s: u64| t0 + std::time::Duration::from_secs(s);
        let monitors = [r"\\?\DISPLAY#ONE".to_owned(), r"\\?\DISPLAY#TWO".to_owned()];
        let mut r = Revival::default();
        r.moving(at(100));
        assert!(r.restarted(), "its start error counts as a failure");
        assert_eq!(r.step(true, &monitors, at(101)), None, "not straight away");
        assert_eq!(r.step(true, &monitors, at(105)), Some(Revive::Restart { tell: false }));
    }

    #[test]
    fn a_hotkey_box_listening_stops_presses_but_not_releases() {
        let engine = Engine::default();
        assert!(!hotkey_ignored(&engine, &Event::Pressed(Action::ToggleRecording)));
        engine.hotkeys_paused.store(true, Relaxed);
        assert!(hotkey_ignored(&engine, &Event::Pressed(Action::ToggleRecording)));
        assert!(hotkey_ignored(&engine, &Event::Pressed(Action::PushToTalk)));
        assert!(!hotkey_ignored(&engine, &Event::Released(Action::PushToTalk)));
    }

    #[test]
    fn every_command_is_a_permission_and_each_window_names_its_own() {
        let names = |text: &str, from: &str, to: &str| -> Vec<String> {
            let start = text.find(from).unwrap() + from.len();
            let list = &text[start..start + text[start..].find(to).unwrap()];
            let mut v: Vec<String> = list
                .split(',')
                .map(|s| s.trim().trim_matches('"').rsplit("::").next().unwrap().to_owned())
                .filter(|s| !s.is_empty())
                .collect();
            v.sort();
            v
        };
        let handler = names(include_str!("main.rs"), "generate_handler![", "]");
        assert_eq!(handler, names(include_str!("../build.rs"), "let commands = &[", "]"));
        let mut granted: Vec<String> = ["default", "hud", "picker"]
            .iter()
            .flat_map(|c| {
                let text =
                    std::fs::read_to_string(format!("{}/capabilities/{c}.json", env!("CARGO_MANIFEST_DIR"))).unwrap();
                names(&text, "\"permissions\": [", "]")
            })
            .filter_map(|p| p.strip_prefix("allow-").map(|c| c.replace('-', "_")))
            .collect();
        granted.sort();
        granted.dedup();
        assert_eq!(granted, handler, "each command is granted to some window");
        let picker =
            std::fs::read_to_string(format!("{}/capabilities/picker.json", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let hud = std::fs::read_to_string(format!("{}/capabilities/hud.json", env!("CARGO_MANIFEST_DIR"))).unwrap();
        assert!(!picker.contains("install") && !hud.contains("install") && !hud.contains("save"));
    }

    #[test]
    fn only_affected_parts_restart() {
        let old = Settings::default();
        assert_eq!(restart_needed(&old, &old), (false, false));

        let mut new = old.clone();
        new.screenshot.quality = 50;
        new.general.notifications = !new.general.notifications;
        assert_eq!(restart_needed(&old, &new), (false, false));

        // Recording-only fields keep the replay buffer.
        new.video.split_minutes += 5;
        new.video.remux_to_mp4 = !new.video.remux_to_mp4;
        assert_eq!(restart_needed(&old, &new), (false, false));

        new.replay.length_seconds += 30;
        assert_eq!(restart_needed(&old, &new), (true, false));

        let mut new = old.clone();
        new.video.fps += 1;
        assert_eq!(restart_needed(&old, &new), (true, false));

        let mut new = old.clone();
        new.hotkeys.insert(Action::SaveReplay, "Ctrl+F9".into());
        assert_eq!(restart_needed(&old, &new), (false, true));
    }

    #[test]
    fn audio_sources_and_push_to_talk_mode_restart_the_session() {
        let old = Settings::default();
        let mut new = old.clone();
        new.audio.microphone = !old.audio.microphone;
        assert_eq!(restart_needed(&old, &new), (true, false));

        new.audio = old.audio.clone();
        new.audio.microphone_device = "another endpoint".into();
        assert_eq!(restart_needed(&old, &new), (true, false));

        new.audio = old.audio.clone();
        new.audio.game_audio_only = !old.audio.game_audio_only;
        assert_eq!(restart_needed(&old, &new), (true, false));

        new.audio = old.audio.clone();
        new.audio.separate_tracks = !old.audio.separate_tracks;
        assert_eq!(restart_needed(&old, &new), (true, false));

        new.audio = old.audio.clone();
        new.hotkeys.insert(Action::PushToTalk, "   ".into());
        assert_eq!(restart_needed(&old, &new), (false, true));

        new.hotkeys.insert(Action::PushToTalk, "Ctrl+F8".into());
        assert_eq!(restart_needed(&old, &new), (true, true));
        assert_eq!(restart_needed(&new, &old), (true, true));

        let mut rebound = new.clone();
        rebound.hotkeys.insert(Action::PushToTalk, "Shift+F8".into());
        assert_eq!(restart_needed(&new, &rebound), (false, true));
    }

    #[test]
    fn a_bad_hotkey_is_told_and_no_hotkey_is_dropped_silently() {
        let mut s = Settings::default();
        assert_eq!(hotkey_bindings(&s).unwrap().len(), s.parsed_hotkeys().unwrap().len());
        s.hotkeys.insert(Action::SaveReplay, "Ctrl+Banana".into());
        let problem = hotkey_bindings(&s).unwrap_err();
        assert!(problem.contains("Ctrl+Banana") && problem.contains("Hotkeys page"), "{problem}");
        // An unbound action (empty) is fine, not a problem.
        s.hotkeys.insert(Action::SaveReplay, " ".into());
        assert!(hotkey_bindings(&s).is_ok());
    }

    #[test]
    fn closing_windows_quits_or_stays_in_the_tray() {
        // The main window closes with pop-ups still open: only "don't keep running" quits.
        assert!(quits_with_main_window(true, Some(false)));
        assert!(!quits_with_main_window(true, Some(true)));
        assert!(!quits_with_main_window(true, None), "an unreadable file keeps running");
        assert!(!quits_with_main_window(false, Some(false)), "the last window's own exit request decides");
        // The last window closed.
        assert!(keeps_running(Some(true), false));
        assert!(keeps_running(None, false));
        assert!(!keeps_running(Some(false), false));
        assert!(keeps_running(Some(false), true), "a pop-up closing last never quits");
    }

    #[test]
    fn the_replay_toggle_picks_one_step() {
        use ReplayStep::*;
        // (desired, active, recording, running)
        assert_eq!(replay_step(None, true, false, true), Stop);
        assert_eq!(replay_step(None, false, false, false), Start);
        assert_eq!(replay_step(None, false, false, true), Stop, "a failed start is cleared, not retried");
        assert_eq!(replay_step(None, true, true, true), FlipBuffer, "a recording keeps running");
        assert_eq!(replay_step(None, false, true, true), FlipBuffer);
        assert_eq!(replay_step(Some(true), true, false, true), Nothing);
        assert_eq!(replay_step(Some(false), false, false, false), Nothing);
        assert_eq!(replay_step(Some(true), false, false, false), Start);
        assert_eq!(replay_step(Some(false), true, false, true), Stop);
    }

    #[test]
    fn a_recording_that_stopped_itself_needs_to_have_been_told() {
        assert!(recording_stopped_itself(true, true, true));
        assert!(!recording_stopped_itself(true, true, false), "the press means stop");
        assert!(!recording_stopped_itself(true, false, true));
        assert!(!recording_stopped_itself(false, true, true));
    }

    #[test]
    fn giving_up_a_conversion_removes_only_its_own_part_and_mark() {
        let d = tempfile::tempdir().unwrap();
        let file = |name: &str| {
            let p = d.path().join(name);
            std::fs::write(&p, b"x").unwrap();
            p
        };
        let mkv = file("Backsight 2026-11-10 14.03.07.mkv");
        let part = file("Backsight 2026-11-10 14.03.07.mp4.part");
        let other = file("Backsight 2026-11-10 14.03.07 (2).mp4.part");
        let clip = file("Backsight 2026-11-10 14.03.07.mp4");
        let mark = file("Backsight 2026-11-10 14.03.07.mkv.to-mp4.part");
        std::fs::write(&mark, part.to_str().unwrap()).unwrap();
        assert_eq!(backsight_engine::recovery::unfinished_conversions(d.path()), std::slice::from_ref(&mkv));
        backsight_engine::recovery::drop_conversion(&mkv);
        assert!(!part.exists() && !mark.exists());
        assert!(mkv.exists() && other.exists() && clip.exists());
        assert!(backsight_engine::recovery::unfinished_conversions(d.path()).is_empty());

        // A mark naming something else (not a `.part`, or elsewhere) removes nothing but itself.
        let keep = file("Backsight 2026-11-10 14.03.07.txt");
        std::fs::write(&mark, keep.to_str().unwrap()).unwrap();
        backsight_engine::recovery::drop_conversion(&mkv);
        assert!(keep.exists() && !mark.exists());
        // No mark at all: nothing happens.
        backsight_engine::recovery::drop_conversion(&mkv);
        assert!(mkv.exists());
    }
}
