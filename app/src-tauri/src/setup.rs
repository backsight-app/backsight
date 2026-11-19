//! Commands for first-run setup, the encoder list and the "can't keep up" warning: the engine
//! checks, the UI shows.

use backsight_config::{Settings, Video};
use backsight_encode::probe::ProbeResult;
use backsight_engine::{
    capture::{target, wgc},
    overlays,
    overload::{self, OverloadWatch, Sample},
    setup,
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

/// Watches the running session for an encoder that can't keep up; holds the warning while it lasts.
static OVERLOAD: Mutex<(OverloadWatch, Option<String>)> = Mutex::new((OverloadWatch::new(), None));

/// One reading a second (`None` without a session). Returns the warning for a pop-up the first
/// time the encoder falls behind in a session; later swings only update Home.
pub fn overload_tick(sample: Option<Sample>) -> Option<String> {
    let mut state = OVERLOAD.lock().unwrap_or_else(PoisonError::into_inner);
    if !state.0.sample(sample)? {
        state.1 = None;
        return None;
    }
    let encoder = Settings::load().map(|s| s.video.encoder).unwrap_or_default();
    let warning = overload::message(&encoder);
    tracing::warn!("encoder {encoder} can't keep up");
    state.1 = Some(warning.clone());
    state.0.first_warning().then_some(warning)
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
        let catalog = backsight_encode::catalog::Catalog::get();
        let hardware = |id: &str| catalog.video_encoder(id).is_some_and(|e| e.hardware);
        let working: Vec<String> = encoders(&app)
            .into_iter()
            .filter(|r| r.problem.is_none() && hardware(&r.encoder_id))
            .map(|r| r.encoder_id)
            .collect();
        // The session sizes its video from the chosen monitor in both modes (the main one when
        // that's unplugged), so the lighter step starts from the same size.
        let monitors = wgc::monitors();
        let ids: Vec<_> = monitors.iter().map(|m| (m.id.as_str(), m.primary)).collect();
        let monitor = target::pick_monitor(&video.capture_monitor, &ids)
            .map_or_else(wgc::primary_monitor, |(i, _)| HMONITOR(monitors[i].handle as _));
        let source = wgc::monitor_size(monitor).unwrap_or((1920, 1080));
        let lighter = overload::lighter(&video, &working, source);
        if lighter.is_some() {
            let mut state = OVERLOAD.lock().unwrap_or_else(PoisonError::into_inner);
            state.0.wait_for_new_session();
            state.1 = None;
        }
        lighter.map(|(video, change)| Lighter { video, change })
    })
    .await
    .map_err(|e| e.to_string())
}
