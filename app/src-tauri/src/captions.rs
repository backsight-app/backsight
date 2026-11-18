//! The captions download for the Audio page (ADR 0016): status, download with progress, stop,
//! remove; and the queue that captions saved clips, each in its own process (`backsight-captions`).

use backsight_captions::{Job, Outcome, Queue, model};
use backsight_config::Settings;
use serde::Serialize;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use tauri::{AppHandle, Emitter};

static QUEUE: OnceLock<Queue> = OnceLock::new();

/// Starts the captions queue, once, at app start.
pub fn start(app: &AppHandle) {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return tracing::warn!("captions are off: Backsight can't find its own program file: {e}"),
    };
    let app = app.clone();
    QUEUE.get_or_init(|| {
        Queue::start(
            move |job| backsight_captions::command(&exe, job).spawn(),
            move |job, outcome| {
                match &outcome {
                    Outcome::Failed(e) => {
                        tracing::warn!("captions for {} failed: {e}", job.clip.display());
                        crate::notify(&app, "Couldn't make captions", e);
                    }
                    other => tracing::info!("captions for {}: {other:?}", job.clip.display()),
                }
                let _ = app.emit("library-changed", ());
            },
        )
    });
}

/// Kills a running captions process; at quit.
pub fn stop() {
    if let Some(queue) = QUEUE.get() {
        queue.stop();
    }
}

/// Captions `clip` in the background when the setting is on and the models are there.
pub fn after_save(clip: &Path, settings: &Settings) {
    let a = &settings.audio;
    if !a.captions || !backsight_captions::cpu_supported() || !model::dir().is_ok_and(|d| model::installed(&d)) {
        return;
    }
    // The microphone is the last track with separate tracks; otherwise the first track, the mix.
    let track = if a.separate_tracks && a.microphone { None } else { Some(0) };
    if let Some(queue) = QUEUE.get() {
        queue.push(Job { clip: clip.to_path_buf(), track, language: "auto".into() });
    }
}

static DOWNLOADING: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);

#[derive(Serialize)]
pub struct Status {
    /// Both model files are there.
    installed: bool,
    /// A download is running (the window may have been closed and opened since it started).
    downloading: bool,
    /// The CPU can run them (AVX2); captions stay off when it can't.
    cpu_supported: bool,
    download_bytes: u64,
}

/// The `captions-download` event: progress, then once `finished` (with `error` if it failed).
#[derive(Clone, Serialize)]
struct Progress {
    done: u64,
    total: u64,
    finished: bool,
    error: Option<String>,
}

#[tauri::command]
pub fn captions_status() -> Result<Status, String> {
    Ok(Status {
        installed: model::installed(&model::dir()?),
        downloading: DOWNLOADING.load(Relaxed),
        cpu_supported: backsight_captions::cpu_supported(),
        download_bytes: model::download_size(),
    })
}

/// Starts the download on its own thread and returns; the page follows `captions-download`.
#[tauri::command]
pub fn download_captions(app: AppHandle) -> Result<(), String> {
    let dir = model::dir()?;
    if DOWNLOADING.swap(true, Relaxed) {
        return Ok(()); // already running: the page just follows it
    }
    STOP.store(false, Relaxed);
    std::thread::spawn(move || {
        let mut sent = 0;
        let result = model::download(
            &dir,
            |done, total| {
                // About every megabyte: enough for a smooth bar, few events.
                if done - sent >= 1 << 20 {
                    sent = done;
                    let _ = app.emit("captions-download", Progress { done, total, finished: false, error: None });
                }
            },
            &STOP,
        );
        DOWNLOADING.store(false, Relaxed);
        match &result {
            Ok(()) => tracing::info!("captions models downloaded to {}", dir.display()),
            Err(e) => tracing::warn!("captions download failed: {e}"),
        }
        let total = model::download_size();
        let done = if result.is_ok() { total } else { sent };
        let _ = app.emit("captions-download", Progress { done, total, finished: true, error: result.err() });
    });
    Ok(())
}

#[tauri::command]
pub fn stop_captions_download() {
    STOP.store(true, Relaxed);
}

/// Deletes the models (not any captions already made).
#[tauri::command]
pub fn remove_captions() -> Result<(), String> {
    if DOWNLOADING.load(Relaxed) {
        return Err("The captions download is still running. Stop it first.".into());
    }
    let dir = model::dir()?;
    model::remove(&dir).map_err(|e| {
        tracing::warn!("captions models not removed from {}: {e}", dir.display());
        format!(
            "Backsight couldn't remove captions from {}. Close other programs using it and try again.",
            dir.display()
        )
    })
}
