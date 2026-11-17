//! "Start with Windows": a per-user entry in the registry Run key. No admin rights needed.

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows::core::{PCWSTR, w};

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const VALUE_NAME: &str = "Backsight";

/// The Run command: the exe path in quotes (so spaces are safe) plus the minimized flag.
fn command_line(exe: &str) -> String {
    format!("\"{exe}\" --minimized")
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Turn "start with Windows" on or off for the current user.
pub fn set(enabled: bool) -> Result<(), String> {
    set_named(VALUE_NAME, enabled)
}

/// Whether the "Backsight" Run entry exists.
pub fn is_enabled() -> bool {
    is_enabled_named(VALUE_NAME)
}

/// Backsight used to be called Clipwise: removes that name's entry and moves it to this exe.
/// Returns whether there was one.
pub fn adopt_old_name() -> Result<bool, String> {
    if !is_enabled_named("Clipwise") {
        return Ok(false);
    }
    set(true)?;
    set_named("Clipwise", false)?;
    Ok(true)
}

fn set_named(name: &str, enabled: bool) -> Result<(), String> {
    let name_w = wide(name);
    let fail = |code: u32| {
        tracing::warn!("autostart registry call failed: win32 error {code}");
        "Backsight could not change the Start with Windows setting. Try again, or check that your account may edit its own startup list.".to_string()
    };
    if enabled {
        let exe = std::env::current_exe()
            .map_err(|e| {
                tracing::warn!("current_exe failed: {e}");
                "Backsight could not find its own file, so it can't start with Windows.".to_string()
            })?
            .to_string_lossy()
            .into_owned();
        let data = wide(&command_line(&exe));
        // SAFETY: all pointers come from live, NUL-terminated UTF-16 buffers; the byte length matches `data`.
        let err = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                PCWSTR(name_w.as_ptr()),
                REG_SZ.0,
                Some(data.as_ptr().cast()),
                (data.len() * 2) as u32,
            )
        };
        if err != ERROR_SUCCESS {
            return Err(fail(err.0));
        }
    } else {
        // SAFETY: the key and name pointers are valid NUL-terminated UTF-16 for the call.
        let err = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, PCWSTR(name_w.as_ptr())) };
        if err != ERROR_SUCCESS && err != ERROR_FILE_NOT_FOUND {
            return Err(fail(err.0));
        }
    }
    Ok(())
}

fn is_enabled_named(name: &str) -> bool {
    let name_w = wide(name);
    // SAFETY: valid NUL-terminated names; no data buffer requested, so only existence is checked.
    unsafe {
        RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, PCWSTR(name_w.as_ptr()), RRF_RT_REG_SZ, None, None, None)
            == ERROR_SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_quotes_paths_with_spaces() {
        assert_eq!(
            command_line(r"C:\Program Files\Backsight\backsight.exe"),
            r#""C:\Program Files\Backsight\backsight.exe" --minimized"#
        );
        assert_eq!(command_line(r"C:\a\b.exe"), r#""C:\a\b.exe" --minimized"#);
    }

    #[test]
    #[ignore = "writes to the real per-user registry (under a test value name)"]
    fn registry_round_trip() {
        let name = "BacksightAutostartTest";
        set_named(name, false).unwrap(); // deleting a missing value is fine
        assert!(!is_enabled_named(name));
        set_named(name, true).unwrap();
        assert!(is_enabled_named(name));
        set_named(name, false).unwrap();
        assert!(!is_enabled_named(name));
    }
}
