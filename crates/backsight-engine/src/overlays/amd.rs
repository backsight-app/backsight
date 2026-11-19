//! AMD Software: Adrenalin's saved keys, from `Software\AMD\DVR` (the user's copy first, then the
//! machine's). Values are text like `Alt,R`, `Ctrl+Shift,E` or `Ctrl+Alt,VK_UP`; `None` or empty
//! is unbound. `HotkeysDisabled` = 1 turns every key off. See platform-notes.md, "Other apps' hotkeys".

use super::{
    Saved,
    vk::{RegValue, dotnet_key, reg_values},
};
use backsight_config::Hotkey;
use std::collections::BTreeMap;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

const KEY: &str = r"Software\AMD\DVR";

pub fn read() -> Option<Saved> {
    let user = reg_values(HKEY_CURRENT_USER, KEY);
    let machine = reg_values(HKEY_LOCAL_MACHINE, KEY);
    if user.is_none() && machine.is_none() {
        tracing::debug!("no AMD hotkey settings");
        return None;
    }
    let mut values = machine.unwrap_or_default();
    values.extend(user.unwrap_or_default());
    Some(parse(&values))
}

pub(crate) fn parse(values: &BTreeMap<String, RegValue>) -> Saved {
    let keys = values
        .iter()
        .filter(|(name, _)| name.ends_with("Hotkey") || *name == "ToggleUpscaling")
        .filter_map(|(name, v)| match v {
            RegValue::Text(t) => Some((name.clone(), hotkey(t))),
            RegValue::Dword(_) => None,
        })
        .collect();
    Saved { keys, off: values.get("HotkeysDisabled") == Some(&RegValue::Dword(1)) }
}

/// `Mods,Key` to a hotkey; `None` when unbound or not a key Backsight could bind.
fn hotkey(text: &str) -> Option<Hotkey> {
    let (mods, key) = text.rsplit_once(',').unwrap_or(("", text));
    let key = key.trim();
    if key.is_empty() || key.eq_ignore_ascii_case("none") {
        return None;
    }
    // `VK_UP`, `VK_OEM_3`, … are Windows key names; `dotnet_key` knows the same spellings.
    let key = dotnet_key(&key.strip_prefix("VK_").unwrap_or(key).replace('_', ""))?;
    let mods = mods.trim();
    let mods = if mods.eq_ignore_ascii_case("none") { "" } else { mods };
    format!("{mods}+{key}").parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(pairs: &[(&str, &str)]) -> BTreeMap<String, RegValue> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), RegValue::Text((*v).to_owned()))).collect()
    }

    #[test]
    fn reads_adrenalin_keys() {
        let mut v = text(&[
            ("ToggleRsHotkey", "Alt,R"),
            ("ToggleDvrRecordingHotkey", "Ctrl+Shift,E"),
            ("RotationUpHotkey", "Ctrl+Alt,VK_UP"),
            ("SaveInstantReplayHotkey", "Shift+Ctrl,S"),
            ("ToggleRsPerfUiHotkey", "Ctrl+Shift+Alt,O"),
            ("ToggleUpscaling", "Alt,U"),
            ("ToggleCameraHotkey", "None"),
            ("ToggleStreamingHotkey", ""),
            ("SomeOtherText", "Alt,X"),
        ]);
        v.insert("HotkeysDisabled".into(), RegValue::Dword(0));
        let saved = parse(&v);
        let k = |id: &str| saved.keys[id].as_ref().map(ToString::to_string);
        assert_eq!(k("ToggleRsHotkey").as_deref(), Some("Alt+R"));
        assert_eq!(k("ToggleDvrRecordingHotkey").as_deref(), Some("Ctrl+Shift+E"));
        assert_eq!(k("RotationUpHotkey").as_deref(), Some("Ctrl+Alt+Up"));
        assert_eq!(k("SaveInstantReplayHotkey").as_deref(), Some("Ctrl+Shift+S"));
        assert_eq!(k("ToggleRsPerfUiHotkey").as_deref(), Some("Ctrl+Alt+Shift+O"));
        assert_eq!(k("ToggleUpscaling").as_deref(), Some("Alt+U"));
        assert_eq!(k("ToggleCameraHotkey"), None);
        assert_eq!(k("ToggleStreamingHotkey"), None);
        assert!(!saved.keys.contains_key("SomeOtherText"));
        assert!(!saved.off);
    }

    #[test]
    fn hotkeys_disabled_turns_everything_off() {
        let mut v = text(&[("ToggleRsHotkey", "Alt,R")]);
        v.insert("HotkeysDisabled".into(), RegValue::Dword(1));
        assert!(parse(&v).off);
    }

    #[test]
    fn single_keys_and_oem_names() {
        assert_eq!(hotkey("None,F11").map(|h| h.to_string()).as_deref(), Some("F11"));
        assert_eq!(hotkey(",VK_OEM_3").map(|h| h.to_string()).as_deref(), Some("Backquote"));
        assert_eq!(hotkey("Alt,VK_SNAPSHOT").map(|h| h.to_string()).as_deref(), Some("Alt+PrintScreen"));
        assert_eq!(hotkey("Alt,VK_ESCAPE"), None);
    }

    /// Shows what this PC has with --nocapture; never fails.
    #[test]
    fn reads_this_pc() {
        match read() {
            Some(s) => println!("AMD: off={} keys={:?}", s.off, s.keys),
            None => println!("AMD: not found"),
        }
    }
}
