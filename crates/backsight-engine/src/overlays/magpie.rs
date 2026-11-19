//! Magpie's saved keys, from `%LOCALAPPDATA%\Magpie\config\v5\config.json` (older versions:
//! `v4`..`v2`, then `config\config.json`). `"shortcuts"` (`"hotkeys"` before v0.10) maps `scale`,
//! `windowedModeScale`, `toolbar` (`overlay` before v0.12) and `takeScreenshot` to a number: low
//! byte = virtual-key code, 0x100 Win, 0x200 Ctrl, 0x400 Alt, 0x800 Shift (Magpie's
//! AppSettings.cpp, EncodeShortcut). See platform-notes.md, "Other apps' hotkeys".

use super::{
    Saved,
    vk::{env_dir, hotkey, read_text},
};
use serde_json::Value;

pub fn read() -> Option<Saved> {
    // shortcut: a portable Magpie keeps `config\config.json` next to its exe and isn't read here.
    let config = env_dir("LOCALAPPDATA")?.join(r"Magpie\config");
    let newest_first = (2..=5).rev().map(|v| config.join(format!("v{v}")).join("config.json"));
    let Some(text) = newest_first.chain([config.join("config.json")]).find_map(|p| read_text(&p)) else {
        tracing::debug!("no Magpie settings");
        return None;
    };
    parse(&text)
}

pub(crate) fn parse(json: &str) -> Option<Saved> {
    let root: Value = serde_json::from_str(json).ok()?;
    let shortcuts = root.get("shortcuts").or_else(|| root.get("hotkeys")).and_then(Value::as_object);
    let mut saved = Saved { keys: Default::default(), off: false };
    for (name, value) in shortcuts.into_iter().flatten() {
        let id = if name == "overlay" { "toolbar" } else { name.as_str() };
        // Magpie ignores numbers above 0xfff and keeps its default, so they're skipped here too.
        let Some(v) = value.as_u64().filter(|v| *v <= 0xFFF) else { continue };
        let vk = (v & 0xFF) as u32;
        let key =
            if vk == 0 { None } else { hotkey(v & 0x200 != 0, v & 0x400 != 0, v & 0x800 != 0, v & 0x100 != 0, vk) };
        saved.keys.insert(id.to_owned(), key);
    }
    Some(saved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_magpie_defaults() {
        // Alt+Shift+A, Alt+Shift+Q, Alt+Shift+D, Alt+Shift+S.
        let json = r#"{"language":"","shortcuts":{"scale":3137,"windowedModeScale":3153,"toolbar":3140,"takeScreenshot":3155}}"#;
        let saved = parse(json).unwrap();
        let k = |id: &str| saved.keys[id].as_ref().map(ToString::to_string);
        assert_eq!(k("scale").as_deref(), Some("Alt+Shift+A"));
        assert_eq!(k("windowedModeScale").as_deref(), Some("Alt+Shift+Q"));
        assert_eq!(k("toolbar").as_deref(), Some("Alt+Shift+D"));
        assert_eq!(k("takeScreenshot").as_deref(), Some("Alt+Shift+S"));
    }

    #[test]
    fn older_names_unbound_and_win_ctrl() {
        let json = r#"{"hotkeys":{"scale":0,"overlay":889,"takeScreenshot":5000}}"#;
        let saved = parse(json).unwrap();
        assert_eq!(saved.keys["scale"], None);
        assert_eq!(saved.keys["toolbar"].as_ref().unwrap().to_string(), "Ctrl+Win+F10"); // 0x300 | 0x79
        assert!(!saved.keys.contains_key("takeScreenshot"));
        assert!(parse("{").is_none());
    }

    /// Shows what this PC has with --nocapture; never fails.
    #[test]
    fn reads_this_pc() {
        match read() {
            Some(s) => println!("Magpie: off={} keys={:?}", s.off, s.keys),
            None => println!("Magpie: not found"),
        }
    }
}
