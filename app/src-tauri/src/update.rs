//! The Update button for installed copies (ADR 0014): asks GitHub Releases for a newer signed
//! installer only when pressed, then installs it the way Quit closes Backsight.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Duration;

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

/// The newer release, or `None` when this copy is the newest.
#[tauri::command]
pub async fn check_update(app: AppHandle) -> Result<Option<Available>, String> {
    installed()?;
    let update =
        app.updater_builder().timeout(Duration::from_secs(30)).build().map_err(plain)?.check().await.map_err(plain)?;
    Ok(update.map(|u| Available { version: u.version, notes: u.body }))
}

/// Downloads and checks the newest release, then closes Backsight (finishing a running recording
/// first, like Quit) and starts its installer, which opens Backsight again.
#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), String> {
    installed()?;
    // One at a time: two finished downloads would start two installers.
    static RUNNING: AtomicBool = AtomicBool::new(false);
    if RUNNING.swap(true, Relaxed) {
        return Err("An update is already downloading.".into());
    }
    let result = download_and_install(&app).await;
    RUNNING.store(false, Relaxed);
    result
}

async fn download_and_install(app: &AppHandle) -> Result<(), String> {
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
                done += chunk as u64;
                // About every 1%, not every chunk.
                if total.is_none_or(|t| done - shown >= t / 100) {
                    shown = done;
                    let _ = app.emit("update-progress", Progress { done, total });
                }
            },
            || {},
        )
        .await
        .map_err(plain)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_plain_and_a_bad_signature_never_reads_as_a_network_problem() {
        assert!(plain(Error::Network("503".into())).starts_with("Couldn't reach GitHub"));
        assert!(plain(Error::SignatureUtf8("x".into())).contains("safety check"));
        assert!(plain(Error::MissingSignedVersion).contains("nothing was installed"));
        assert!(plain(Error::EmptyEndpoints).starts_with("Backsight couldn't update"));
    }
}
