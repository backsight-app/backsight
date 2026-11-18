//! NVIDIA's own in-game overlay (NVIDIA App, GeForce Experience): is it on, and do its keys clash
//! with Backsight's? Read only: Backsight never changes NVIDIA's settings. See platform-notes.md,
//! "NVIDIA overlay".

use backsight_config::{Action, Hotkey, Settings};
use serde::Serialize;
use std::collections::BTreeMap;
use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
            },
            Registry::{HKEY, HKEY_CURRENT_USER, KEY_READ, RegCloseKey, RegEnumValueW, RegOpenKeyExW},
        },
    },
    core::{PWSTR, w},
};

/// The overlay's processes: NVIDIA App, then GeForce Experience.
const OVERLAY_EXES: [&str; 2] = ["NVIDIA Overlay.exe", "NVIDIA Share.exe"];

/// NVIDIA App's default keys, for when its overlay runs but its saved keys can't be read.
const DEFAULT_KEYS: [(&str, &str); 8] = [
    ("GFEOverlay", "Alt+Z"),
    ("Manual", "Alt+F9"),
    ("DVR", "Alt+F10"),
    ("IRToggle", "Alt+Shift+F10"),
    ("Screenshot", "Alt+F1"),
    ("AnselUI", "Alt+F2"),
    ("FSUI", "Alt+F3"),
    ("PMOCOverlay", "Alt+R"),
];

/// A Backsight hotkey that NVIDIA's overlay also uses.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Clash {
    pub action: Action,
    pub key: String,
    /// What the key does in NVIDIA's overlay, e.g. "saves NVIDIA's Instant Replay".
    pub nvidia: &'static str,
}

/// One of NVIDIA's overlay shortcuts.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OverlayKey {
    pub key: String,
    /// What the key does in NVIDIA's overlay, e.g. "saves NVIDIA's Instant Replay".
    pub nvidia: &'static str,
}

/// Backsight hotkeys that NVIDIA's overlay also uses; empty when the overlay is off or missing.
pub fn clashes(settings: &Settings) -> Vec<Clash> {
    find_clashes(settings, &overlay_hotkeys())
}

/// NVIDIA's overlay shortcuts, for the UI to compare with the hotkeys as they're edited; empty
/// when the overlay is off or missing.
pub fn overlay_keys() -> Vec<OverlayKey> {
    overlay_hotkeys().iter().map(|(name, k)| OverlayKey { key: k.to_string(), nvidia: use_of(name) }).collect()
}

fn overlay_hotkeys() -> Vec<(String, Hotkey)> {
    // shortcut: the on/off signals were only seen with the overlay off (platform-notes.md); check
    // them with it on, on a PC where toggling NVIDIA's overlay is fine, before trusting a "no clash".
    let values = read_nvspcaps().unwrap_or_default();
    if !overlay_running() && values.get("IsShadowPlayEnabledUser").is_none_or(|v| *v == 0) {
        return Vec::new();
    }
    let keys = saved_keys(&values);
    if !keys.is_empty() {
        return keys;
    }
    DEFAULT_KEYS.iter().filter_map(|(n, k)| Some(((*n).to_owned(), k.parse().ok()?))).collect()
}

fn find_clashes(settings: &Settings, nvidia: &[(String, Hotkey)]) -> Vec<Clash> {
    let mut out = Vec::new();
    for (action, combo) in &settings.hotkeys {
        let Ok(hk) = combo.parse::<Hotkey>() else { continue };
        if let Some((name, _)) = nvidia.iter().find(|(_, k)| *k == hk) {
            out.push(Clash { action: *action, key: hk.to_string(), nvidia: use_of(name) });
        }
    }
    out
}

/// NVIDIA's saved keys: `<Name>HKeyCount` holds how many keys a shortcut has, and `<Name>HKey0`,
/// `<Name>HKey1`, … one Windows virtual-key code each (`GFEOverlayHKeyV2Count` → `GFEOverlayHKeyV20`).
fn saved_keys(values: &BTreeMap<String, u32>) -> Vec<(String, Hotkey)> {
    values
        .iter()
        .filter_map(|(name, &count)| {
            let base = name.strip_suffix("Count").filter(|b| b.contains("HKey"))?;
            let vks: Option<Vec<u32>> = (0..count.min(5)).map(|i| values.get(&format!("{base}{i}")).copied()).collect();
            Some((base.to_owned(), combo(&vks?)?))
        })
        .collect()
}

/// Virtual-key codes to a Backsight hotkey; `None` for keys Backsight can't bind.
fn combo(vks: &[u32]) -> Option<Hotkey> {
    let parts: Option<Vec<String>> = vks
        .iter()
        .map(|&vk| match vk {
            0x10 => Some("Shift".into()),
            0x11 => Some("Ctrl".into()),
            0x12 => Some("Alt".into()),
            0x5B | 0x5C => Some("Win".into()),
            0x30..=0x39 | 0x41..=0x5A => char::from_u32(vk).map(String::from),
            0x60..=0x69 => Some(format!("Numpad{}", vk - 0x60)),
            0x70..=0x87 => Some(format!("F{}", vk - 0x6F)),
            0xC0 => Some("Backquote".into()),
            _ => None,
        })
        .collect();
    parts?.join("+").parse().ok()
}

fn use_of(name: &str) -> &'static str {
    match name.split("HKey").next().unwrap_or_default() {
        "GFEOverlay" => "opens NVIDIA's overlay",
        "Manual" => "starts and stops NVIDIA's recording",
        "DVR" => "saves NVIDIA's Instant Replay",
        "IRToggle" => "turns NVIDIA's Instant Replay on and off",
        "Screenshot" => "takes an NVIDIA screenshot",
        "AnselUI" => "opens NVIDIA's photo mode",
        "FSUI" => "opens NVIDIA's game filters",
        "PMOCOverlay" | "PMOCOverlayCycle" => "shows NVIDIA's performance overlay",
        "MicToggle" => "turns NVIDIA's microphone on and off",
        "MicPTT" => "is NVIDIA's push-to-talk key",
        _ => "is an NVIDIA overlay shortcut",
    }
}

/// Every 4-byte value under NVIDIA's overlay settings key, as numbers; `None` when it's missing.
fn read_nvspcaps() -> Option<BTreeMap<String, u32>> {
    let mut key = HKEY::default();
    let path = w!(r"Software\NVIDIA Corporation\Global\ShadowPlay\NVSPCAPS");
    // SAFETY: `path` is a static NUL-terminated string and `key` a live out-pointer.
    if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path, None, KEY_READ, &mut key) } != ERROR_SUCCESS {
        return None;
    }
    let mut values = BTreeMap::new();
    let mut name = [0u16; 256];
    for i in 0.. {
        let (mut name_len, mut data, mut data_len) = (name.len() as u32, [0u8; 4], 4u32);
        // SAFETY: every buffer is a live local whose size is passed with it.
        let err = unsafe {
            RegEnumValueW(
                key,
                i,
                Some(PWSTR(name.as_mut_ptr())),
                &mut name_len,
                None,
                None,
                Some(data.as_mut_ptr()),
                Some(&mut data_len),
            )
        };
        match err {
            ERROR_SUCCESS if data_len == 4 => {
                values.insert(String::from_utf16_lossy(&name[..name_len as usize]), u32::from_le_bytes(data));
            }
            ERROR_SUCCESS | ERROR_MORE_DATA => {} // longer values (paths) aren't needed
            ERROR_NO_MORE_ITEMS => break,
            e => {
                tracing::debug!("reading NVIDIA's overlay settings stopped: win32 error {}", e.0);
                break;
            }
        }
    }
    // SAFETY: `key` was opened above and is closed once.
    let _ = unsafe { RegCloseKey(key) };
    Some(values)
}

fn overlay_running() -> bool {
    // SAFETY: the snapshot handle is closed below; `entry.dwSize` is set as the calls require.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return false };
        let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut found = false;
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        while more && !found {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            let exe = String::from_utf16_lossy(&entry.szExeFile[..len]);
            found = OVERLAY_EXES.iter().any(|x| x.eq_ignore_ascii_case(&exe));
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As read from NVIDIA App 11 with its default keys.
    fn nvidia_app_defaults() -> BTreeMap<String, u32> {
        let mut v = BTreeMap::new();
        let mut put = |name: &str, vks: &[u32]| {
            v.insert(format!("{name}Count"), vks.len() as u32);
            for (i, vk) in vks.iter().enumerate() {
                v.insert(format!("{name}{i}"), *vk);
            }
        };
        put("DVRHKey", &[18, 121]);
        put("ManualHKey", &[18, 120]);
        put("IRToggleHKey", &[18, 16, 121]);
        put("ScreenshotHKey", &[18, 112]);
        put("GFEOverlayHKeyV2", &[18, 90]);
        put("MicToggleHKey", &[17, 18, 77]);
        put("MicPTTHKey", &[192]);
        put("CustomOverlayBHKey", &[]);
        v.insert("IsShadowPlayEnabledUser".into(), 0);
        v
    }

    #[test]
    fn reads_nvidia_keys_from_virtual_key_codes() {
        let keys: BTreeMap<_, _> =
            saved_keys(&nvidia_app_defaults()).into_iter().map(|(n, k)| (n, k.to_string())).collect();
        assert_eq!(keys["DVRHKey"], "Alt+F10");
        assert_eq!(keys["IRToggleHKey"], "Alt+Shift+F10");
        assert_eq!(keys["GFEOverlayHKeyV2"], "Alt+Z");
        assert_eq!(keys["MicToggleHKey"], "Ctrl+Alt+M");
        assert_eq!(keys["MicPTTHKey"], "Backquote");
        assert!(!keys.contains_key("CustomOverlayBHKey"), "an unset shortcut is no key");
    }

    fn nvidia_defaults() -> Vec<(String, Hotkey)> {
        DEFAULT_KEYS.iter().map(|(n, k)| ((*n).to_owned(), k.parse().unwrap())).collect()
    }

    #[test]
    fn previous_backsight_defaults_clash_with_nvidia_defaults() {
        let previous: BTreeMap<Action, String> =
            serde_json::from_str(include_str!("../../../content/previous-default-hotkeys.json")).unwrap();
        let mut s = Settings::default();
        s.hotkeys.extend(previous);
        let clashes = find_clashes(&s, &saved_keys(&nvidia_app_defaults()));
        let found: Vec<_> = clashes.iter().map(|c| (c.action, c.key.as_str(), c.nvidia)).collect();
        assert!(found.contains(&(Action::SaveReplay, "Alt+F10", "saves NVIDIA's Instant Replay")), "{found:?}");
        assert!(found.contains(&(Action::ToggleRecording, "Alt+F9", "starts and stops NVIDIA's recording")));
        assert!(found.contains(&(Action::Screenshot, "Alt+F1", "takes an NVIDIA screenshot")));
        assert!(found.contains(&(Action::ToggleReplay, "Alt+Shift+F10", "turns NVIDIA's Instant Replay on and off")));
    }

    #[test]
    fn defaults_clash_only_on_alt_r_and_the_fallback_keys_clear_it() {
        // Alt+R records in Backsight and opens NVIDIA's performance overlay; the rest are clear.
        let s = Settings::default();
        let found: Vec<_> = find_clashes(&s, &nvidia_defaults()).iter().map(|c| (c.action, c.key.clone())).collect();
        assert_eq!(found, [(Action::ToggleRecording, "Alt+R".to_owned())]);
        assert!(find_clashes(&s, &saved_keys(&nvidia_app_defaults())).is_empty());

        // The clash card moves a key whose default is NVIDIA's (or taken) to its fallback key.
        let fallback: BTreeMap<Action, String> =
            serde_json::from_str(include_str!("../../../content/fallback-hotkeys.json")).unwrap();
        let mut s = Settings::default();
        s.hotkeys.extend(fallback);
        assert!(find_clashes(&s, &nvidia_defaults()).is_empty());
        assert!(find_clashes(&s, &saved_keys(&nvidia_app_defaults())).is_empty());
        let parsed = s.parsed_hotkeys().expect("fallback keys parse");
        assert!(backsight_config::conflicts(&parsed).is_empty(), "fallback keys don't clash with the defaults");
    }

    #[test]
    fn a_cleared_key_never_clashes() {
        let mut s = Settings::default();
        s.hotkeys.values_mut().for_each(String::clear);
        assert!(find_clashes(&s, &nvidia_defaults()).is_empty());
    }

    /// Shows what this PC has with --nocapture; never fails on a PC without NVIDIA.
    #[test]
    fn reads_this_pc_without_failing() {
        println!("overlay running: {}", overlay_running());
        println!("clashes: {:?}", clashes(&Settings::default()));
    }
}
