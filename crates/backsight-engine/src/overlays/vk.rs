//! Shared by the settings readers: Windows virtual-key codes and .NET key names to Backsight's key
//! names, plus small helpers for reading files, INI text and the registry. Everything here only
//! reads.

use super::Saved;
use backsight_config::Hotkey;
use std::{collections::BTreeMap, io::Read, path::Path};
use windows::{
    Win32::{
        Foundation::{ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS},
        System::Registry::{
            HKEY, KEY_READ, REG_DWORD, REG_EXPAND_SZ, REG_SZ, REG_VALUE_TYPE, RegCloseKey, RegEnumValueW, RegOpenKeyExW,
        },
    },
    core::{PCWSTR, PWSTR},
};

/// Settings files bigger than this are read only this far.
const MAX_FILE: u64 = 8 << 20;

/// Backsight's name for a virtual-key code that isn't a modifier; `None` for keys Backsight can't
/// bind (Esc, Caps Lock, left and right mouse buttons, …).
pub(crate) fn key_name(vk: u32) -> Option<String> {
    let name = match vk {
        0x30..=0x39 | 0x41..=0x5A => return char::from_u32(vk).map(String::from),
        0x60..=0x69 => return Some(format!("Numpad{}", vk - 0x60)),
        0x70..=0x87 => return Some(format!("F{}", vk - 0x6F)),
        0x04 => "Mouse3",
        0x05 => "Mouse4",
        0x06 => "Mouse5",
        0x08 => "Backspace",
        0x09 => "Tab",
        0x0D => "Enter",
        0x13 => "Pause",
        0x20 => "Space",
        0x21 => "PageUp",
        0x22 => "PageDown",
        0x23 => "End",
        0x24 => "Home",
        0x25 => "Left",
        0x26 => "Up",
        0x27 => "Right",
        0x28 => "Down",
        0x2C => "PrintScreen",
        0x2D => "Insert",
        0x2E => "Delete",
        0x6A => "NumpadMultiply",
        0x6B => "NumpadAdd",
        0x6D => "NumpadSubtract",
        0x6E => "NumpadDecimal",
        0x6F => "NumpadDivide",
        0x90 => "NumLock",
        0x91 => "ScrollLock",
        0xBA => "Semicolon",
        0xBB => "Equal",
        0xBC => "Comma",
        0xBD => "Minus",
        0xBE => "Period",
        0xBF => "Slash",
        0xC0 => "Backquote",
        0xDB => "BracketLeft",
        0xDC => "Backslash",
        0xDD => "BracketRight",
        0xDE => "Quote",
        _ => return None,
    };
    Some(name.into())
}

/// `Ctrl`, `Alt`, `Shift` or `Win` for a modifier's virtual-key code, either side.
fn modifier(vk: u32) -> Option<&'static str> {
    match vk {
        0x10 | 0xA0 | 0xA1 => Some("Shift"),
        0x11 | 0xA2 | 0xA3 => Some("Ctrl"),
        0x12 | 0xA4 | 0xA5 => Some("Alt"),
        0x5B | 0x5C => Some("Win"),
        _ => None,
    }
}

/// Virtual-key codes held together (modifiers and one key, any order) to a hotkey; `None` when
/// they aren't one Backsight could bind (two main keys, no main key, an unknown key).
pub(crate) fn combo(vks: &[u32]) -> Option<Hotkey> {
    let parts: Option<Vec<String>> =
        vks.iter().map(|&vk| modifier(vk).map(String::from).or_else(|| key_name(vk))).collect();
    parts?.join("+").parse().ok()
}

/// A hotkey from modifier flags and a virtual-key code.
pub(crate) fn hotkey(ctrl: bool, alt: bool, shift: bool, win: bool, vk: u32) -> Option<Hotkey> {
    Some(Hotkey { ctrl, alt, shift, win, key: key_name(vk)? })
}

/// A .NET `Keys` name (as ShareX and Greenshot save them) to Backsight's key name or modifier.
pub(crate) fn dotnet_key(name: &str) -> Option<String> {
    let n = name.trim();
    let mapped = match n.to_ascii_lowercase().as_str() {
        "control" | "ctrl" | "controlkey" | "lcontrolkey" | "rcontrolkey" => "Ctrl",
        "alt" | "menu" | "lmenu" | "rmenu" => "Alt",
        "shift" | "shiftkey" | "lshiftkey" | "rshiftkey" => "Shift",
        "win" | "lwin" | "rwin" => "Win",
        "snapshot" | "printscreen" | "print" => "PrintScreen",
        "back" => "Backspace",
        "return" => "Enter",
        "next" => "PageDown",
        "prior" => "PageUp",
        "scroll" => "ScrollLock",
        "multiply" => "NumpadMultiply",
        "add" => "NumpadAdd",
        "subtract" => "NumpadSubtract",
        "decimal" => "NumpadDecimal",
        "divide" => "NumpadDivide",
        "oemtilde" | "oem3" => "Backquote",
        "oemminus" => "Minus",
        "oemplus" => "Equal",
        "oemopenbrackets" | "oem4" => "BracketLeft",
        "oemclosebrackets" | "oem6" => "BracketRight",
        "oempipe" | "oem5" => "Backslash",
        // .NET `OemBackslash` is VK 226, the extra key beside left Shift on some keyboards, which
        // Backsight has no name for.
        "oembackslash" | "oem102" => return None,
        "oemsemicolon" | "oem1" => "Semicolon",
        "oemquotes" | "oem7" => "Quote",
        "oemcomma" => "Comma",
        "oemperiod" => "Period",
        "oemquestion" | "oem2" => "Slash",
        "mbutton" => "Mouse3",
        "xbutton1" => "Mouse4",
        "xbutton2" => "Mouse5",
        lower => {
            // `D0`..`D9` are the digit row, `NumPad0`.. the keypad; the rest match Backsight's names.
            if let Some(d) = lower.strip_prefix('d').filter(|d| d.len() == 1 && d.as_bytes()[0].is_ascii_digit()) {
                return Some(d.into());
            }
            if let Some(d) = lower.strip_prefix("numpad") {
                return Some(format!("Numpad{d}"));
            }
            return Some(n.into());
        }
    };
    Some(mapped.into())
}

/// Puts a key into the map, but never lets an unbound copy of the same id hide a bound one.
// shortcut: apps that bind one action twice (two push-to-talk keys) keep only the first key here;
// give `Saved` a list per id if those clashes matter.
pub(crate) fn keep_bound(saved: &mut Saved, id: &str, key: Option<Hotkey>) {
    let slot = saved.keys.entry(id.to_owned()).or_insert(None);
    if slot.is_none() {
        *slot = key;
    }
}

/// A file's bytes, at most `MAX_FILE` of them; `None` when it can't be opened. Rust opens files on
/// Windows sharing read, write and delete, so the app that owns the file is never blocked.
pub(crate) fn read_capped(path: &Path) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    match std::fs::File::open(path).and_then(|f| f.take(MAX_FILE).read_to_end(&mut bytes)) {
        Ok(_) => Some(bytes),
        Err(e) => {
            tracing::debug!("can't read {}: {e}", path.display());
            None
        }
    }
}

/// A text file (UTF-8, byte-order mark dropped).
pub(crate) fn read_text(path: &Path) -> Option<String> {
    let bytes = read_capped(path)?;
    let text = String::from_utf8_lossy(&bytes);
    Some(text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned())
}

/// The `key=value` lines of one INI section, in file order; section names ignore case.
pub(crate) fn ini_section<'a>(text: &'a str, section: &str) -> Vec<(&'a str, &'a str)> {
    let mut inside = false;
    let mut out = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            inside = name.trim().eq_ignore_ascii_case(section);
        } else if inside
            && !line.starts_with([';', '#'])
            && let Some((k, v)) = line.split_once('=')
        {
            out.push((k.trim(), v.trim()));
        }
    }
    out
}

/// One registry value: a number or text (other kinds are skipped).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RegValue {
    Dword(u32),
    Text(String),
}

/// Every number and short text value under a registry key; `None` when the key is missing.
pub(crate) fn reg_values(root: HKEY, path: &str) -> Option<BTreeMap<String, RegValue>> {
    let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
    let mut key = HKEY::default();
    // SAFETY: `wide` is NUL-terminated and outlives the call; `key` is a live out-pointer.
    if unsafe { RegOpenKeyExW(root, PCWSTR(wide.as_ptr()), None, KEY_READ, &mut key) } != ERROR_SUCCESS {
        return None;
    }
    let mut values = BTreeMap::new();
    let mut name = [0u16; 256];
    let mut data = [0u8; 2048];
    for i in 0.. {
        let (mut name_len, mut kind, mut data_len) = (name.len() as u32, 0u32, data.len() as u32);
        // SAFETY: every buffer is a live local whose size is passed with it.
        let err = unsafe {
            RegEnumValueW(
                key,
                i,
                Some(PWSTR(name.as_mut_ptr())),
                &mut name_len,
                None,
                Some(&mut kind),
                Some(data.as_mut_ptr()),
                Some(&mut data_len),
            )
        };
        match err {
            ERROR_SUCCESS => {
                let name = String::from_utf16_lossy(&name[..name_len as usize]);
                let data = &data[..data_len as usize];
                let value = match REG_VALUE_TYPE(kind) {
                    REG_DWORD if data.len() == 4 => {
                        RegValue::Dword(u32::from_le_bytes(data.try_into().unwrap_or_default()))
                    }
                    REG_SZ | REG_EXPAND_SZ => RegValue::Text(utf16_le(data).trim_end_matches('\0').to_owned()),
                    _ => continue,
                };
                values.insert(name, value);
            }
            ERROR_MORE_DATA => {} // long values (blobs, paths lists) aren't needed
            ERROR_NO_MORE_ITEMS => break,
            e => {
                tracing::debug!("reading registry key {path} stopped: win32 error {}", e.0);
                break;
            }
        }
    }
    // SAFETY: `key` was opened above and is closed once.
    let _ = unsafe { RegCloseKey(key) };
    Some(values)
}

/// UTF-16LE bytes to text (an odd last byte is dropped).
pub(crate) fn utf16_le(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect();
    String::from_utf16_lossy(&units)
}

/// A folder from an environment variable such as `APPDATA`.
pub(crate) fn env_dir(var: &str) -> Option<std::path::PathBuf> {
    std::env::var_os(var).map(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_name_is_one_backsight_knows() {
        for vk in 0..=0xFF {
            if let Some(name) = key_name(vk) {
                let hk: Hotkey = name.parse().unwrap_or_else(|e| panic!("vk {vk:#x} -> {name}: {e}"));
                assert_eq!(hk.key, name, "vk {vk:#x} name is canonical");
            }
        }
    }

    #[test]
    fn combos_take_either_side_modifiers_in_any_order() {
        assert_eq!(combo(&[164, 67]).unwrap().to_string(), "Alt+C");
        assert_eq!(combo(&[0x5B, 161, 0x2C]).unwrap().to_string(), "Shift+Win+PrintScreen");
        assert_eq!(combo(&[0xC0, 162]).unwrap().to_string(), "Ctrl+Backquote");
        assert_eq!(combo(&[0x70, 0x71]), None, "two main keys");
        assert_eq!(combo(&[0x10, 0x11]), None, "no main key");
        assert_eq!(combo(&[0x1B]), None, "Esc");
        assert_eq!(combo(&[0x05]).unwrap().to_string(), "Mouse4");
    }

    #[test]
    fn dotnet_names() {
        let k = |s: &str| dotnet_key(s).unwrap();
        assert_eq!(k("Control"), "Ctrl");
        assert_eq!(k("PrintScreen"), "PrintScreen");
        assert_eq!(k("D5"), "5");
        assert_eq!(k("NumPad3"), "Numpad3");
        assert_eq!(k("Oemtilde"), "Backquote");
        assert_eq!(k("Next"), "PageDown");
        assert_eq!(k("F11"), "F11");
        assert_eq!(k("XButton2"), "Mouse5");
    }

    #[test]
    fn every_key_backsight_names_has_one_code() {
        // Digits, letters, the keypad's digits, F1-F24 and every named key but NumpadEnter, which
        // shares Enter's code.
        let names: Vec<_> = (0..=0xFF).filter_map(key_name).collect();
        assert_eq!(names.len(), 10 + 26 + 10 + 24 + 37);
        assert_eq!(names.iter().collect::<std::collections::HashSet<_>>().len(), names.len());
    }

    #[test]
    fn a_dotnet_letter_d_is_a_key_not_a_digit() {
        assert_eq!(dotnet_key("D").as_deref(), Some("D"));
    }

    #[test]
    fn files_text_and_folders_are_read() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.ini");
        std::fs::write(&p, "\u{feff}[A]\nx=1").unwrap();
        assert_eq!(read_text(&p).as_deref(), Some("[A]\nx=1"), "byte-order mark dropped");
        assert_eq!(read_capped(&d.path().join("gone.ini")), None);
        assert_eq!(utf16_le(&[b'h', 0, b'i', 0, 7]), "hi");
        assert!(env_dir("SystemRoot").is_some_and(|p| p.join("explorer.exe").is_file()));
        assert_eq!(env_dir("NO_SUCH_VARIABLE_HERE"), None);
    }

    #[test]
    fn registry_numbers_and_text_are_read() {
        use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
        let v = reg_values(HKEY_LOCAL_MACHINE, r"SOFTWARE\Microsoft\Windows NT\CurrentVersion").unwrap();
        assert_eq!(v.get("CurrentMajorVersionNumber"), Some(&RegValue::Dword(10)));
        assert!(matches!(v.get("CurrentBuildNumber"), Some(RegValue::Text(t)) if t.parse::<u32>().is_ok()));
        assert_eq!(reg_values(HKEY_LOCAL_MACHINE, r"SOFTWARE\No Such Key Here"), None);
    }

    #[test]
    fn ini_sections() {
        let text = "[A]\nx=1\n[Hotkeys]\n; note\nOBSBasic.Screenshot = {\"bindings\":[]}\n[b]\ny=2";
        assert_eq!(ini_section(text, "hotkeys"), vec![("OBSBasic.Screenshot", "{\"bindings\":[]}")]);
        assert!(ini_section(text, "missing").is_empty());
    }
}
