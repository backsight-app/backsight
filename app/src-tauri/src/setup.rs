//! Commands for first-run setup, the encoder list and the "can't keep up" warning: the engine
//! checks, the UI shows.

use backsight_config::{Settings, Video};
use backsight_encode::probe::ProbeResult;
use backsight_engine::{
    capture::{target, wgc},
    overlays,
    overload::{self, OverloadWatch, SafeMoment, Sample},
    setup, tune,
};
use serde::Serialize;
use std::sync::{Mutex, PoisonError};
use tauri::{AppHandle, Manager, async_runtime::spawn_blocking};
use windows::Win32::Graphics::Gdi::HMONITOR;

/// One encoder check at a time: they share the cache file.
static CHECKING: Mutex<()> = Mutex::new(());

/// Which encoders work here. Successes come from the cache (kept per graphics driver); failures
/// are tried again on every call, so a passing failure doesn't stick until restart.
pub fn encoders(app: &AppHandle) -> Vec<ProbeResult> {
    let _one = CHECKING.lock().unwrap_or_else(PoisonError::into_inner);
    let mut results = setup::check_encoders(probe_process);
    // The check opens a new encoder session, and NVIDIA's consumer cards allow only a few at once:
    // with replay holding one (and Discord or OBS others) it can fail on an encoder that works.
    if let Some(running) = running_encoder(app) {
        for r in results.iter_mut().filter(|r| r.encoder_id == running) {
            r.problem = None;
        }
    }
    results
}

/// Starts `backsight.exe --probe-encoders <ids>` (see `probe_child.rs`) without a console window.
fn probe_process(ids: &[&str]) -> std::io::Result<setup::ProbeRun> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command.arg("--probe-encoders").args(ids).creation_flags(CREATE_NO_WINDOW);
    setup::ProbeRun::start(command)
}

/// The encoder a healthy session runs on; `None` when none runs, or it's busy or about to change.
fn running_encoder(app: &AppHandle) -> Option<String> {
    let engine = app.state::<crate::Engine>();
    let replay = engine.replay.try_lock().ok()?;
    if replay.pending_apply || !replay.session.as_ref().is_some_and(|s| s.error().is_none()) {
        return None;
    }
    drop(replay);
    Settings::load().ok().map(|s| s.video.encoder)
}

/// Which encoders work on this PC, for the encoder list.
#[tauri::command]
pub async fn encoder_check(app: AppHandle) -> Result<Vec<ProbeResult>, String> {
    spawn_blocking(move || encoders(&app)).await.map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct SetupCheck {
    cards: Vec<setup::GraphicsCard>,
    encoders: Vec<ProbeResult>,
    /// The encoder to switch to, when the chosen one doesn't work here.
    pick: Option<setup::Pick>,
    /// Other apps' hotkeys and keys another program holds; the page matches them to the hotkeys.
    scan: overlays::Scan,
}

/// Everything the setup page shows, for the settings it has now.
#[tauri::command]
pub async fn setup_check(app: AppHandle, settings: Settings) -> Result<SetupCheck, String> {
    spawn_blocking(move || {
        let encoders = encoders(&app);
        let cards = setup::graphics_cards();
        SetupCheck {
            pick: setup::pick_encoder(&settings.video.encoder, &encoders, &cards),
            cards,
            encoders,
            scan: overlays::scan(&settings),
        }
    })
    .await
    .map_err(|e| e.to_string())
}

/// Other apps' hotkeys, and Backsight keys another program already holds, for Home, Setup and Hotkeys
/// to compare with the hotkeys as they change. Reads the registry, files and the process list,
/// so off the UI thread.
#[tauri::command]
pub async fn overlay_scan(settings: Settings) -> Result<overlays::Scan, String> {
    spawn_blocking(move || overlays::scan(&settings)).await.map_err(|e| e.to_string())
}

/// Watches the running session for an encoder that can't keep up; holds the warning while it lasts,
/// and the lighter setting that is due until a safe moment.
static OVERLOAD: Mutex<(OverloadWatch, Option<String>, SafeMoment)> =
    Mutex::new((OverloadWatch::new(), None, SafeMoment::new()));

pub struct Tick {
    /// For a pop-up: the first time the encoder falls behind in a session.
    pub warning: Option<String>,
    /// Now is the moment to lighten these settings, the ones that fell behind ([`step_down`]).
    pub step_down: Option<Video>,
}

/// One reading a second (`None` without a session), with what the session is doing: a recording,
/// how many clips were saved so far, and whether saved settings wait for the recording to stop
/// (then the file isn't what runs, so no step is marked). Later swings only update Home.
pub fn overload_tick(sample: Option<Sample>, recording: bool, clips: u64, pending: bool) -> Tick {
    let mut state = OVERLOAD.lock().unwrap_or_else(PoisonError::into_inner);
    let step_down = state.2.tick(recording, clips);
    let mut warning = None;
    match state.0.sample(sample) {
        Some(true) => {
            let video = Settings::load().unwrap_or_default().video;
            let mut text = overload::message(&video.encoder);
            // Without the encoder check (it starts processes), so a CPU encoder's move to the card isn't promised.
            if !pending && overload::lighter(&video, &[], source_size(&video)).is_some() {
                text.push_str(" Backsight lightens a setting by itself after your next clip or recording.");
            }
            tracing::warn!("encoder {} can't keep up", video.encoder);
            state.1 = Some(text.clone());
            if !pending {
                state.2.due(clips, video);
            }
            warning = state.0.first_warning().then_some(text);
        }
        Some(false) => state.1 = None,
        None => {}
    }
    Tick { warning, step_down }
}

/// Lightens the video settings one step after the encoder fell behind, at a safe moment
/// ([`SafeMoment`]), and says so. Runs on its own thread: the save restarts the session.
pub fn step_down(app: &AppHandle, behind: &Video) {
    // Before loading: the encoder check behind it can take seconds.
    let working = working_hardware(app);
    let Ok(mut settings) = Settings::load() else { return };
    if settings.video != *behind {
        tracing::info!("step down skipped: the video settings changed since they fell behind");
        return;
    }
    let Some((video, change)) = overload::lighter(&settings.video, &working, source_size(&settings.video)) else {
        tracing::info!("step down: no lighter setting left");
        return;
    };
    settings.video = video;
    // Quiet from before the save: the session it restarts must not count as the old one.
    let quiet = |on: bool| {
        let mut state = OVERLOAD.lock().unwrap_or_else(PoisonError::into_inner);
        if on {
            state.0.wait_for_new_session();
            state.1 = None;
        } else {
            state.0 = OverloadWatch::new(); // nothing changed: watch again
        }
    };
    quiet(true);
    match crate::save_by_engine(app, settings) {
        Ok(()) => {
            tracing::info!("step down: {change}");
            let text = format!("Clips were stuttering, so Backsight lightened a setting: {change}");
            remember_change(&text);
            crate::notify(app, "Setting lightened", &format!("{text} You can change it back in Video."));
        }
        Err(e) => {
            quiet(false);
            tracing::warn!("step down not saved: {e}");
        }
    }
}

/// The last setting Backsight lightened by itself, for Home until dismissed: a pop-up can be missed
/// or turned off. `at` is milliseconds since 1970 (the page writes the date).
#[derive(Serialize, serde::Deserialize)]
pub struct AutoChange {
    at: u64,
    text: String,
}

fn auto_change_file() -> Option<std::path::PathBuf> {
    backsight_engine::library::cache_dir().ok().map(|d| d.join("auto-change.json"))
}

/// Adds `text` (what changed) to Home's note: changes not dismissed yet stay listed. A note only:
/// a failed write is logged.
fn remember_change(text: &str) {
    let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    let text = match auto_change() {
        Some(earlier) => format!("{} {text}", earlier.text),
        None => text.into(),
    };
    let json = serde_json::to_vec(&AutoChange { at, text }).expect("plain data");
    let Some(file) = auto_change_file() else { return };
    let part = file.with_extension("json.part");
    if let Err(e) = std::fs::write(&part, json).and_then(|()| std::fs::rename(&part, &file)) {
        tracing::warn!("automatic change not kept for Home: {e}");
    }
}

/// The automatic changes Home shows, until dismissed.
#[tauri::command(async)]
pub fn auto_change() -> Option<AutoChange> {
    serde_json::from_slice(&std::fs::read(auto_change_file()?).ok()?).ok()
}

/// Dismisses the note Home showed (`at`), not one written since.
#[tauri::command(async)]
pub fn auto_change_dismiss(at: u64) {
    if auto_change().is_some_and(|c| c.at == at)
        && let Some(f) = auto_change_file()
    {
        let _ = std::fs::remove_file(f);
    }
}

/// The warning Home shows while the encoder can't keep up.
#[tauri::command]
pub fn overload_status() -> Option<String> {
    OVERLOAD.lock().unwrap_or_else(PoisonError::into_inner).1.clone()
}

#[derive(Serialize)]
pub struct Lighter {
    video: Video,
    /// What changed, in plain words.
    change: String,
}

/// `video` one step lighter, for Home's "Use a lighter setting" button; `None` when nothing is left.
/// The warning clears and stays quiet until the session restarts with the change.
#[tauri::command]
pub async fn lighter_video(app: AppHandle, video: Video) -> Result<Option<Lighter>, String> {
    spawn_blocking(move || {
        let lighter = overload::lighter(&video, &working_hardware(&app), source_size(&video));
        if lighter.is_some() {
            let mut state = OVERLOAD.lock().unwrap_or_else(PoisonError::into_inner);
            state.0.wait_for_new_session();
            state.1 = None;
            state.2.cancel();
        }
        lighter.map(|(video, change)| Lighter { video, change })
    })
    .await
    .map_err(|e| e.to_string())
}

/// Hardware encoders that passed the encoder check (a CPU encoder may step over to one).
fn working_hardware(app: &AppHandle) -> Vec<String> {
    let catalog = backsight_encode::catalog::Catalog::get();
    let hardware = |id: &str| catalog.video_encoder(id).is_some_and(|e| e.hardware);
    encoders(app).into_iter().filter(|r| r.problem.is_none() && hardware(&r.encoder_id)).map(|r| r.encoder_id).collect()
}

/// The size of the monitor `video` records. The session sizes its video from the chosen monitor
/// in both modes (the main one when that's unplugged), so lighter steps start from the same size.
fn source_size(video: &Video) -> (u32, u32) {
    let monitors = wgc::monitors();
    let ids: Vec<_> = monitors.iter().map(|m| (m.id.as_str(), m.primary)).collect();
    let monitor = target::pick_monitor(&video.capture_monitor, &ids)
        .map_or_else(wgc::primary_monitor, |(i, _)| HMONITOR(monitors[i].handle as _));
    wgc::monitor_size(monitor).unwrap_or((1920, 1080))
}

/// Setup's "tune for this PC": how fast the chosen encoder runs here, and lighter settings when it
/// lacks room for a game. `working` lists the encoders the setup check found working (hardware ones
/// are steps a CPU encoder may take). Remembered per graphics driver, so it's seconds only the
/// first time. Instant Replay pauses while it measures; nothing is measured while recording.
#[tauri::command]
pub async fn tune_check(app: AppHandle, video: Video, working: Vec<String>) -> Result<tune::Tuned, String> {
    spawn_blocking(move || run_tune(&app, &video, working)).await.map_err(|e| e.to_string())?
}

/// After a graphics driver update (the speed test's saved answer, about the settings in use and
/// with room then, is from other drivers), tests again once Backsight is idle, and lightens the
/// settings if they no longer have room.
pub fn tune_after_driver_update(app: &AppHandle) {
    let Ok(cache) = backsight_engine::library::cache_dir().map(|d| d.join("tune.json")) else { return };
    let Ok(video) = Settings::load().map(|s| s.video) else { return };
    if !tune::drivers_changed(&cache, &video) {
        return;
    }
    // Idle: setup done, not recording, no game in front. Checked for a few minutes after start.
    let idle = || crate::idle_for_update(app);
    if !(0..18).any(|_| {
        std::thread::sleep(std::time::Duration::from_secs(10));
        idle()
    }) {
        return; // the next start asks again
    }
    let working = working_hardware(app); // may start the encoder check: seconds
    let unchanged = || Settings::load().ok().filter(|s| s.video == video);
    if !idle() || unchanged().is_none() {
        return;
    }
    tracing::info!("graphics drivers changed: speed test again");
    match run_tune(app, &video, working) {
        // Fresh settings: anything else changed during the test is kept.
        Ok(t)
            if !t.changes.is_empty()
                && let Some(mut settings) = unchanged() =>
        {
            settings.video = t.video;
            match crate::save_by_engine(app, settings) {
                Ok(()) => {
                    let text = format!(
                        "After a graphics driver update, your PC records slower, so Backsight lightened your video settings: {}",
                        t.changes.join(" ")
                    );
                    remember_change(&text);
                    crate::notify(app, "Setting lightened", &format!("{text} You can change them back in Video."));
                }
                Err(e) => tracing::warn!("tune after driver update not saved: {e}"),
            }
        }
        _ => {}
    }
}

fn run_tune(app: &AppHandle, video: &Video, working: Vec<String>) -> Result<tune::Tuned, String> {
    {
        let cache = backsight_engine::library::cache_dir()?.join("tune.json");
        let catalog = backsight_encode::catalog::Catalog::get();
        let working: Vec<String> =
            working.into_iter().filter(|id| catalog.video_encoder(id).is_some_and(|e| e.hardware)).collect();
        let source = source_size(video);
        // One at a time: they share the cache file, and two at once would slow each other down.
        static TUNING: Mutex<()> = Mutex::new(());
        let _one = TUNING.lock().unwrap_or_else(PoisonError::into_inner);
        // Paused at the first measurement (none when the answer is remembered), until the end.
        let mut paused = None;
        let measure = |v: &Video, size: (u32, u32)| {
            if paused.is_none() {
                paused = Some(crate::ReplayPause::start(app).ok_or("a recording is running")?);
            }
            let fps = tune::measure_in_child(v, size, tune_process);
            tracing::info!("speed test: {} {}x{} at {} FPS: {fps:?} frames/s", v.encoder, size.0, size.1, v.fps);
            fps
        };
        let tuned = tune::pick_cached(&cache, video, &working, source, measure);
        drop(paused);
        match &tuned {
            Ok(t) => tracing::info!("tune: {:.0} frames/s, enough {}, changes {:?}", t.speed, t.enough, t.changes),
            Err(e) => tracing::warn!("tune skipped: {e}"),
        }
        tuned
    }
}

/// Starts `backsight.exe --tune-encoder <args>` (see `probe_child.rs`) without a console window.
fn tune_process(args: &[String]) -> std::io::Result<setup::ProbeRun> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command.arg("--tune-encoder").args(args).creation_flags(CREATE_NO_WINDOW);
    setup::ProbeRun::start(command)
}
