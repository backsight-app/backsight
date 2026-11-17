//! Start-up check for the WebView2 runtime. The installer puts it on the PC, but the no-install
//! folder can't, and without it Tauri fails with a dialog nobody can act on. If it is missing,
//! say in plain words what to install, then exit.
//!
//! Detection is Microsoft's documented registry check: the runtime's version is the `pv` value
//! under its Edge Update client key; missing, empty or `0.0.0.0` means "not installed".

const CLIENT_KEY_64: &str = r"SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
const CLIENT_KEY_32: &str = r"SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
/// Microsoft's own "Evergreen Bootstrapper" download (the same link the installer uses).
const DOWNLOAD_URL: &str = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";

fn is_usable(version: &str) -> bool {
    let v = version.trim();
    !v.is_empty() && v != "0.0.0.0"
}

/// Returns when the runtime is there. Otherwise tells the user what to install and exits.
pub fn ensure_installed() {
    // A fixed-version runtime has no registry key; trust whoever pointed WebView2 at it.
    if std::env::var_os("WEBVIEW2_BROWSER_EXECUTABLE_FOLDER").is_some() {
        return;
    }
    if platform::versions().iter().any(|v| is_usable(v)) {
        return;
    }
    tracing::error!("WebView2 runtime not found");
    if platform::ask_to_open_download() {
        // explorer.exe opens a web address in the default browser, without a console window.
        let _ = std::process::Command::new(crate::windows_dir().join("explorer.exe")).arg(DOWNLOAD_URL).spawn();
    }
    std::process::exit(1);
}

/// Windows calls, kept small.
mod platform {
    use windows::{
        Win32::{
            System::Registry::{HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW},
            UI::WindowsAndMessaging::{IDOK, MB_ICONINFORMATION, MB_OKCANCEL, MB_SETFOREGROUND, MessageBoxW},
        },
        core::{PCWSTR, w},
    };

    fn read_pv(root: HKEY, key: &str) -> Option<String> {
        let key_w: Vec<u16> = key.encode_utf16().chain(Some(0)).collect();
        let mut text = [0u16; 64];
        let mut bytes = size_of_val(&text) as u32;
        // SAFETY: `key_w` is NUL-terminated; `text` is writable for `bytes` bytes; both outlive the call.
        let e = unsafe {
            RegGetValueW(
                root,
                PCWSTR(key_w.as_ptr()),
                w!("pv"),
                RRF_RT_REG_SZ,
                None,
                Some(text.as_mut_ptr().cast()),
                Some(&mut bytes),
            )
        };
        e.ok().ok()?;
        Some(String::from_utf16_lossy(&text[..bytes as usize / 2]).trim_end_matches('\0').to_owned())
    }

    /// Every runtime version the registry lists (per machine, 64- and 32-bit views, and per user).
    pub fn versions() -> Vec<String> {
        [
            (HKEY_LOCAL_MACHINE, super::CLIENT_KEY_64),
            (HKEY_LOCAL_MACHINE, super::CLIENT_KEY_32),
            (HKEY_CURRENT_USER, super::CLIENT_KEY_32),
        ]
        .into_iter()
        .filter_map(|(root, key)| read_pv(root, key))
        .collect()
    }

    /// True when the user chose OK, which means "open the download page".
    pub fn ask_to_open_download() -> bool {
        // SAFETY: both strings are NUL-terminated literals; no owner window.
        let answer = unsafe {
            MessageBoxW(
                None,
                w!(
                    "Backsight needs a Microsoft part called the WebView2 Runtime to show its window, and this PC doesn't have it yet.\n\nClick OK to download it. Run the file you get, wait until it finishes, then start Backsight again.\n\nClick Cancel to close this."
                ),
                w!("Backsight: one more thing to install"),
                MB_OKCANCEL | MB_ICONINFORMATION | MB_SETFOREGROUND,
            )
        };
        answer == IDOK
    }
}

#[cfg(test)]
mod tests {
    use super::is_usable;

    #[test]
    fn empty_and_zero_versions_count_as_missing() {
        assert!(!is_usable(""));
        assert!(!is_usable(" "));
        assert!(!is_usable("0.0.0.0"));
        assert!(is_usable("131.0.2903.112"));
    }
}
