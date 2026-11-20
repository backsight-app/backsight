//! Updates for installed copies (ADR 0014): asks GitHub Releases for a newer signed installer when
//! Backsight starts, every hour, when the window opens and when Check is pressed, then installs it
//! the way Quit closes Backsight.

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::{Error, UpdaterExt};

/// What the Update button offers.
#[derive(Serialize)]
pub struct Available {
    version: String,
    notes: Option<String>,
}

/// Download progress for the window: bytes so far and the total when GitHub says it.
#[derive(Clone, Serialize)]
struct Progress {
    done: u64,
    total: Option<u64>,
}

/// The newer version the last check found: Home shows "Update ready" with it.
static READY: Mutex<Option<String>> = Mutex::new(None);

pub fn ready() -> Option<String> {
    crate::lock(&READY).clone()
}

/// The newer release, or `None` when this copy is the newest.
#[tauri::command]
pub async fn check_update(app: AppHandle) -> Result<Option<Available>, String> {
    installed()?;
    let update =
        app.updater_builder().timeout(Duration::from_secs(30)).build().map_err(plain)?.check().await.map_err(plain)?;
    *crate::lock(&READY) = update.as_ref().map(|u| u.version.clone());
    Ok(update.map(|u| Available { version: u.version, notes: u.body }))
}

/// Checks now and then every hour. An update found in the first minutes after Backsight starts
/// installs straight away when nothing would be lost (`crate::idle_for_update`); otherwise Home
/// offers it.
pub fn check_in_background(app: AppHandle) {
    if installed().is_err() {
        return;
    }
    let started = Instant::now();
    let spawned = std::thread::Builder::new().name("backsight-update-check".into()).spawn(move || {
        loop {
            if let Ok(Some(found)) = tauri::async_runtime::block_on(check_update(app.clone()))
                && started.elapsed() < Duration::from_secs(180)
                && crate::idle_for_update(&app)
                && first_try(&found.version)
            {
                tracing::info!("update {}: found at start, installing", found.version);
                let _ = tauri::async_runtime::block_on(install(app.clone(), true));
            }
            std::thread::sleep(Duration::from_secs(3600));
        }
    });
    if let Err(e) = spawned {
        tracing::warn!("update check didn't start: {e}");
    }
}

/// Installing at start is tried once per version: a broken installer must not restart Backsight
/// over and over. A marker in the temp folder remembers the try.
fn first_try(version: &str) -> bool {
    let marker = std::env::temp_dir().join(format!("backsight-auto-update-{version}"));
    std::fs::OpenOptions::new().write(true).create_new(true).open(marker).is_ok()
}

/// Downloads and checks the newest release, then closes Backsight (finishing a running recording
/// first, like Quit) and starts its installer, which opens Backsight again.
#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), String> {
    install(app, false).await
}

/// `auto`: started by the background check, so it gives up when a recording began meanwhile.
async fn install(app: AppHandle, auto: bool) -> Result<(), String> {
    installed()?;
    // One at a time: two finished downloads would start two installers.
    static RUNNING: AtomicBool = AtomicBool::new(false);
    if RUNNING.swap(true, Relaxed) {
        // Maybe the automatic one, which gives up if a recording started: pressing again works then.
        return Err("An update is already downloading. Try again in a minute.".into());
    }
    let result = download_and_install(&app, auto).await;
    RUNNING.store(false, Relaxed);
    result
}

async fn download_and_install(app: &AppHandle, auto: bool) -> Result<(), String> {
    let closing = Arc::new(AtomicBool::new(false));
    let hook_app = app.clone();
    let hook_closing = closing.clone();
    let update = app
        .updater_builder()
        // The whole download: a 15 MB installer on a slow line.
        .timeout(Duration::from_secs(600))
        .on_before_exit(move || {
            hook_closing.store(true, Relaxed);
            crate::close_for_update(&hook_app);
            hook_app.cleanup_before_exit();
        })
        .build()
        .map_err(plain)?
        .check()
        .await
        .map_err(plain)?
        .ok_or("Backsight is already up to date.")?;
    let mut done = 0u64;
    let mut shown = 0u64;
    let bytes = update
        .download(
            |chunk, total| {
                if progressed(&mut done, &mut shown, chunk, total) {
                    let _ = app.emit("update-progress", Progress { done, total });
                }
            },
            || {},
        )
        .await
        .map_err(plain)?;
    // The download can take minutes; a recording started meanwhile must not be stopped unasked.
    // shortcut: a recording starting between this check and `install` is still finished and saved
    // by `close_for_update` (like Quit); holding `apply` here would deadlock with `shut_down`, which
    // takes it. Fix with a "closing for update" flag that starting checks, if it ever bites.
    if auto && !crate::idle_for_update(app) {
        tracing::info!("update {}: downloaded, but Backsight is busy now; Home offers it", update.version);
        return Err("Backsight is busy; the update waits.".into());
    }
    tracing::info!("update {}: downloaded {} bytes, installing", update.version, bytes.len());
    // On success this never returns: the process ends once the installer has started.
    let result = update.install(bytes);
    let err = result.err().map(plain).unwrap_or_default();
    if closing.load(Relaxed) {
        // Backsight already stopped recording and capture; start it again rather than leave it
        // half closed.
        tracing::error!("update: the installer didn't start after closing ({err}); restarting");
        app.restart();
    }
    Err(err)
}

/// Adds a downloaded `chunk` to `done`; whether to show it: about every 1%, not every chunk.
fn progressed(done: &mut u64, shown: &mut u64, chunk: usize, total: Option<u64>) -> bool {
    *done += chunk as u64;
    let due = total.is_none_or(|t| *done - *shown >= t / 100);
    if due {
        *shown = *done;
    }
    due
}

/// Only a copy the installer put there updates itself: the installer always installs to its own
/// folder, so a copy elsewhere (a desktop test build, a copied folder) would end up as two.
fn installed() -> Result<(), String> {
    match crate::app_folder() {
        Some(dir) if dir.join("uninstall.exe").is_file() => Ok(()),
        _ => Err("This copy wasn't installed with Backsight's installer, so it can't update itself. Download the newest installer from GitHub instead.".into()),
    }
}

/// Plain words for the window; the raw error goes to the log.
fn plain(e: Error) -> String {
    tracing::warn!("update failed: {e}");
    match e {
        Error::Reqwest(_) | Error::Network(_) => "Couldn't reach GitHub. Check your internet and try again.",
        Error::ReleaseNotFound | Error::TargetNotFound(_) | Error::TargetsNotFound(_) => {
            "GitHub has no Backsight release for this PC yet. Try again later."
        }
        Error::Minisign(_)
        | Error::Base64(_)
        | Error::SignatureUtf8(_)
        | Error::SignedVersionMismatch { .. }
        | Error::MissingSignedVersion => "This download didn't pass its safety check, so nothing was installed.",
        _ => "Backsight couldn't update. Try again later, or download the newest version from GitHub.",
    }
    .to_owned()
}

/// This version starts for the first time (the version that last started is kept in
/// `last-version.txt`). A first install also counts; no record at all (a folder that can't be
/// written) never does, so the window doesn't open at every sign-in.
pub(crate) fn first_start_of(version: &str) -> bool {
    let Ok(dir) = backsight_config::app_dir() else { return false };
    first_start_in(&dir.join("last-version.txt"), version)
}

/// [`first_start_of`] with the record kept in `file`.
fn first_start_in(file: &std::path::Path, version: &str) -> bool {
    let last = std::fs::read_to_string(file).unwrap_or_default();
    last.trim() != version && std::fs::write(file, version).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_plain_and_a_bad_signature_never_reads_as_a_network_problem() {
        assert!(plain(Error::Network("503".into())).starts_with("Couldn't reach GitHub"));
        assert!(plain(Error::SignatureUtf8("x".into())).contains("safety check"));
        assert!(plain(Error::MissingSignedVersion).contains("nothing was installed"));
        assert!(plain(Error::EmptyEndpoints).starts_with("Backsight couldn't update"));
        assert!(plain(Error::ReleaseNotFound).contains("no Backsight release"));
    }

    #[test]
    fn an_update_found_is_ready_and_installs_at_start_once_per_version() {
        *crate::lock(&READY) = Some("9.8.7".into());
        assert_eq!(ready().as_deref(), Some("9.8.7"));
        let version = format!("0.0.0-test-{}", std::process::id());
        let marker = std::env::temp_dir().join(format!("backsight-auto-update-{version}"));
        let _ = std::fs::remove_file(&marker);
        assert!(first_try(&version), "the first time");
        assert!(!first_try(&version), "never again");
        std::fs::remove_file(&marker).unwrap();
        assert!(installed().is_err(), "a test build isn't an installed copy");
    }

    #[test]
    fn progress_shows_about_every_percent() {
        let (mut done, mut shown) = (0, 0);
        let mut step = |chunk, total| progressed(&mut done, &mut shown, chunk, total);
        assert!(!step(5, Some(1000)), "0.5%");
        assert!(step(5, Some(1000)), "1%");
        assert!(!step(9, Some(1000)), "0.9% more");
        assert!(step(1, Some(1000)), "2%");
        assert!(step(1, None), "no total: every chunk");
        assert_eq!((done, shown), (21, 21));
    }

    #[test]
    fn a_version_counts_as_new_once_and_never_without_a_record() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("last-version.txt");
        assert!(first_start_in(&file, "0.1.9"), "first install");
        assert!(!first_start_in(&file, "0.1.9"), "the next sign-in");
        assert!(first_start_in(&file, "0.2.0"), "after an update");
        assert!(!first_start_in(&dir.path().join("gone").join("last-version.txt"), "0.2.0"), "can't be written");
    }
}
