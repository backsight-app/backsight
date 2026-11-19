//! ShareX's saved keys, from `Documents\ShareX\HotkeysConfig.json` (or the folder
//! `PersonalPath.cfg` points to): each hotkey's `HotkeyInfo.Hotkey` is .NET key flags as text, key
//! first (`"PrintScreen, Control"`), with `Win` a separate flag; `"None"` = unbound. Ids are task
//! names (`TaskSettings.Job`, e.g. `RectangleRegion`). `ApplicationConfig.json` `DisableHotkeys`
//! turns every key off. See platform-notes.md, "Other apps' hotkeys".

use super::{
    Saved,
    vk::{RegValue, dotnet_key, env_dir, keep_bound, read_text, reg_values},
};
use backsight_config::Hotkey;
use serde_json::Value;
use std::path::PathBuf;
use windows::Win32::System::Registry::HKEY_CURRENT_USER;

pub fn read() -> Option<Saved> {
    let dir = folder()?;
    let Some(hotkeys) = read_text(&dir.join("HotkeysConfig.json")) else {
        tracing::debug!("no ShareX hotkeys");
        return None;
    };
    parse(&hotkeys, read_text(&dir.join("ApplicationConfig.json")).as_deref().unwrap_or_default())
}

/// `Documents\ShareX`, or the absolute folder its `PersonalPath.cfg` names.
// shortcut: a relative or `%VAR%` path in PersonalPath.cfg (portable setups) isn't followed.
fn folder() -> Option<PathBuf> {
    // `Shell Folders` holds the Documents path already expanded (it may live in OneDrive).
    let docs = reg_values(HKEY_CURRENT_USER, r"Software\Microsoft\Windows\CurrentVersion\Explorer\Shell Folders")
        .and_then(|v| match v.get("Personal") {
            Some(RegValue::Text(p)) if !p.is_empty() => Some(PathBuf::from(p)),
            _ => None,
        })
        .or_else(|| Some(env_dir("USERPROFILE")?.join("Documents")))?;
    let default = docs.join("ShareX");
    let redirect = read_text(&default.join("PersonalPath.cfg"))
        .and_then(|t| t.lines().map(str::trim).find(|l| !l.is_empty()).map(PathBuf::from))
        .filter(|p| p.is_absolute());
    Some(redirect.unwrap_or(default))
}

/// Keys from `HotkeysConfig.json` text and `ApplicationConfig.json` text (may be empty).
pub(crate) fn parse(hotkeys_json: &str, app_json: &str) -> Option<Saved> {
    let config: Value = serde_json::from_str(hotkeys_json).ok()?;
    let mut saved = Saved { keys: Default::default(), off: false };
    for h in config["Hotkeys"].as_array()? {
        let Some(job) = h["TaskSettings"]["Job"].as_str() else { continue };
        let info = &h["HotkeyInfo"];
        keep_bound(&mut saved, job, hotkey(info["Hotkey"].as_str().unwrap_or("None"), info["Win"] == true));
    }
    saved.off = serde_json::from_str::<Value>(app_json).is_ok_and(|a| a["DisableHotkeys"] == true);
    Some(saved)
}

/// `"PrintScreen, Shift, Control"` plus the Win flag to a hotkey.
fn hotkey(flags: &str, win: bool) -> Option<Hotkey> {
    if flags.trim().eq_ignore_ascii_case("none") {
        return None;
    }
    let mut parts: Vec<String> = flags.split(',').map(dotnet_key).collect::<Option<_>>()?;
    if win {
        parts.push("Win".into());
    }
    parts.join("+").parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The test PC's HotkeysConfig.json, trimmed to the fields that matter.
    const TEST_PC: &str = r#"{"Hotkeys":[
        {"HotkeyInfo":{"Hotkey":"PrintScreen, Control","Win":false},"TaskSettings":{"Description":"","Job":"RectangleRegion"}},
        {"HotkeyInfo":{"Hotkey":"PrintScreen","Win":false},"TaskSettings":{"Job":"PrintScreen"}},
        {"HotkeyInfo":{"Hotkey":"PrintScreen, Alt","Win":false},"TaskSettings":{"Job":"ActiveWindow"}},
        {"HotkeyInfo":{"Hotkey":"PrintScreen, Shift","Win":false},"TaskSettings":{"Job":"ScreenRecorder"}},
        {"HotkeyInfo":{"Hotkey":"PrintScreen, Shift, Control","Win":false},"TaskSettings":{"Job":"ScreenRecorderGIF"}}
    ],"DarkTheme":true}"#;

    #[test]
    fn reads_the_test_pc_config() {
        let saved = parse(TEST_PC, r#"{"DisableHotkeys":false}"#).unwrap();
        let k = |id: &str| saved.keys[id].as_ref().map(ToString::to_string);
        assert_eq!(k("RectangleRegion").as_deref(), Some("Ctrl+PrintScreen"));
        assert_eq!(k("PrintScreen").as_deref(), Some("PrintScreen"));
        assert_eq!(k("ActiveWindow").as_deref(), Some("Alt+PrintScreen"));
        assert_eq!(k("ScreenRecorder").as_deref(), Some("Shift+PrintScreen"));
        assert_eq!(k("ScreenRecorderGIF").as_deref(), Some("Ctrl+Shift+PrintScreen"));
        assert!(!saved.off);
    }

    #[test]
    fn win_flag_none_and_disabled() {
        let json = r#"{"Hotkeys":[
            {"HotkeyInfo":{"Hotkey":"D1, Alt","Win":true},"TaskSettings":{"Job":"CustomRegion"}},
            {"HotkeyInfo":{"Hotkey":"None","Win":false},"TaskSettings":{"Job":"LastRegion"}}]}"#;
        let saved = parse(json, r#"{"DisableHotkeys":true}"#).unwrap();
        assert_eq!(saved.keys["CustomRegion"].as_ref().unwrap().to_string(), "Alt+Win+1");
        assert_eq!(saved.keys["LastRegion"], None);
        assert!(saved.off);
        assert!(parse("not json", "").is_none());
    }

    /// Shows what this PC has with --nocapture; never fails.
    #[test]
    fn reads_this_pc() {
        match read() {
            Some(s) => println!("ShareX: off={} keys={:?}", s.off, s.keys),
            None => println!("ShareX: not found"),
        }
    }
}
