//! Xbox Game Bar's custom keys, from `HKCU\Software\Microsoft\Windows\CurrentVersion\GameDVR`:
//! `VK<Name>` holds a virtual-key code and `VKM<Name>` its modifiers. A missing pair means Game
//! Bar's default key. See platform-notes.md, "Other apps' hotkeys".

use super::{
    Saved,
    vk::{RegValue, hotkey, reg_values},
};
use std::collections::BTreeMap;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

/// Game Bar's shortcut names (the part after `VK` / `VKM`).
const NAMES: [&str; 8] = [
    "ToggleGameBar",
    "ToggleRecording",
    "TakeScreenshot",
    "SaveHistoricalVideo",
    "ToggleMicrophoneCapture",
    "ToggleCameraCapture",
    "ToggleBroadcast",
    "ToggleRecordingIndicator",
];

pub fn read() -> Option<Saved> {
    let dvr = reg_values(HKEY_CURRENT_USER, r"Software\Microsoft\Windows\CurrentVersion\GameDVR").unwrap_or_default();
    let store = reg_values(HKEY_CURRENT_USER, r"System\GameConfigStore").unwrap_or_default();
    let policy = reg_values(HKEY_LOCAL_MACHINE, r"SOFTWARE\Policies\Microsoft\Windows\GameDVR").unwrap_or_default();
    // shortcut: these three switches come from third-party guides and mainly stop capture; Win+G
    // may still open Game Bar with them off. Check on a PC where turning capture off is fine.
    let off = [(&dvr, "AppCaptureEnabled"), (&store, "GameDVR_Enabled"), (&policy, "AllowGameDVR")]
        .iter()
        .any(|(values, name)| values.get(*name) == Some(&RegValue::Dword(0)));
    let mut saved = parse(&dvr);
    saved.off = off;
    Some(saved)
}

/// The custom keys among the GameDVR values; Game Bar always exists on Windows, so no values
/// still gives `Some` (every key at its default).
pub(crate) fn parse(values: &BTreeMap<String, RegValue>) -> Saved {
    let dword = |name: String| match values.get(&name) {
        Some(RegValue::Dword(v)) => Some(*v),
        _ => None,
    };
    let mut keys = BTreeMap::new();
    for name in NAMES {
        let Some(vk) = dword(format!("VK{name}")) else { continue };
        // Modifiers as `VirtualKeyModifiers`: Ctrl 1, Alt 2, Shift 4, Win 8.
        // shortcut: that bit order is inferred, not documented for this value; settle it by
        // changing a Game Bar key and reading the registry.
        let m = dword(format!("VKM{name}")).unwrap_or(0);
        let key = if vk == 0 { None } else { hotkey(m & 1 != 0, m & 2 != 0, m & 4 != 0, m & 8 != 0, vk) };
        keys.insert(name.to_owned(), key);
    }
    Saved { keys, off: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_test_pc_has_only_defaults() {
        let values = BTreeMap::from([
            ("KGLToGCSUpdatedRevision".to_owned(), RegValue::Dword(1)),
            ("KGLRevision".to_owned(), RegValue::Dword(1)),
            ("LastGameActivity".to_owned(), RegValue::Text("x".into())),
        ]);
        let saved = parse(&values);
        assert!(saved.keys.is_empty());
        assert!(!saved.off);
    }

    #[test]
    fn reads_custom_keys() {
        let values = BTreeMap::from([
            ("VKSaveHistoricalVideo".to_owned(), RegValue::Dword(0x79)),
            ("VKMSaveHistoricalVideo".to_owned(), RegValue::Dword(1 | 2)),
            ("VKTakeScreenshot".to_owned(), RegValue::Dword(0x2C)),
            ("VKMTakeScreenshot".to_owned(), RegValue::Dword(2 | 8)),
            ("VKToggleRecording".to_owned(), RegValue::Dword(0)),
            ("VKToggleBroadcast".to_owned(), RegValue::Dword(0x42)),
            ("VKMToggleBroadcast".to_owned(), RegValue::Dword(4)),
        ]);
        let saved = parse(&values);
        let k = |id: &str| saved.keys[id].as_ref().map(ToString::to_string);
        assert_eq!(k("SaveHistoricalVideo").as_deref(), Some("Ctrl+Alt+F10"));
        assert_eq!(k("TakeScreenshot").as_deref(), Some("Alt+Win+PrintScreen"));
        assert_eq!(k("ToggleRecording"), None);
        assert_eq!(k("ToggleBroadcast").as_deref(), Some("Shift+B"));
        assert!(!saved.keys.contains_key("ToggleGameBar"));
    }

    /// Shows what this PC has with --nocapture; never fails.
    #[test]
    fn reads_this_pc() {
        match read() {
            Some(s) => println!("Game Bar: off={} keys={:?}", s.off, s.keys),
            None => println!("Game Bar: not found"),
        }
    }
}
