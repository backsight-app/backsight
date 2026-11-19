//! Greenshot's saved keys, from `%APPDATA%\Greenshot\Greenshot.ini` ([Core] `RegionHotkey=Ctrl +
//! PrintScreen`, … ; `None` = unbound). See platform-notes.md, "Other apps' hotkeys".

use super::{
    Saved,
    vk::{dotnet_key, env_dir, ini_section, read_text},
};
use backsight_config::Hotkey;

const IDS: [&str; 5] = ["RegionHotkey", "WindowHotkey", "FullscreenHotkey", "LastregionHotkey", "IEHotkey"];

pub fn read() -> Option<Saved> {
    // shortcut: a portable Greenshot keeps its ini next to its exe and isn't read here.
    let path = env_dir("APPDATA")?.join(r"Greenshot\Greenshot.ini");
    let text = read_text(&path)?;
    Some(parse(&text))
}

pub(crate) fn parse(ini: &str) -> Saved {
    let keys = ini_section(ini, "Core")
        .into_iter()
        .filter(|(k, _)| IDS.iter().any(|id| id.eq_ignore_ascii_case(k)))
        .map(|(k, v)| (k.to_owned(), hotkey(v)))
        .collect();
    Saved { keys, off: false }
}

/// `Ctrl + Shift + PrintScreen` (.NET key names joined by ` + `) to a hotkey.
fn hotkey(text: &str) -> Option<Hotkey> {
    let parts: Option<Vec<String>> = text.split('+').map(dotnet_key).collect();
    parts?.join("+").parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_greenshot_ini() {
        let ini = "\u{feff}; Greenshot core configuration\n[Core]\nLanguage=en-US\nRegionHotkey=Ctrl + PrintScreen\n\
                   WindowHotkey=Alt + PrintScreen\nFullscreenHotkey=PrintScreen\nLastregionHotkey=None\n\
                   IEHotkey=Shift + Ctrl + PrintScreen\n[Editor]\nRegionHotkey=F1\n";
        let saved = parse(ini);
        let k = |id: &str| saved.keys[id].as_ref().map(ToString::to_string);
        assert_eq!(k("RegionHotkey").as_deref(), Some("Ctrl+PrintScreen"));
        assert_eq!(k("WindowHotkey").as_deref(), Some("Alt+PrintScreen"));
        assert_eq!(k("FullscreenHotkey").as_deref(), Some("PrintScreen"));
        assert_eq!(k("LastregionHotkey"), None);
        assert_eq!(k("IEHotkey").as_deref(), Some("Ctrl+Shift+PrintScreen"));
        assert_eq!(saved.keys.len(), 5);
    }

    /// Shows what this PC has with --nocapture; never fails.
    #[test]
    fn reads_this_pc() {
        match read() {
            Some(s) => println!("Greenshot: off={} keys={:?}", s.off, s.keys),
            None => println!("Greenshot: not found"),
        }
    }
}
