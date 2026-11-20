//! NVIDIA's overlay (NVIDIA App, GeForce Experience): is it on, and which keys are saved. See
//! platform-notes.md, "Other apps' hotkeys".

use super::{Saved, processes, vk::combo};
use backsight_config::Hotkey;
use std::collections::BTreeMap;
use windows::{
    Win32::{
        Foundation::{ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS},
        System::Registry::{HKEY, HKEY_CURRENT_USER, KEY_READ, RegCloseKey, RegEnumValueW, RegOpenKeyExW},
    },
    core::{PWSTR, w},
};

/// The overlay's processes, lower case: NVIDIA App, then GeForce Experience.
const OVERLAY_EXES: [&str; 2] = ["nvidia overlay.exe", "nvidia share.exe"];

pub fn read() -> Option<Saved> {
    let running = processes();
    saved(read_nvspcaps(), OVERLAY_EXES.iter().any(|x| running.contains(*x)))
}

/// Off unless the overlay runs or `IsShadowPlayEnabledUser` is set; `None` (defaults) when on
/// with no saved keys.
pub(super) fn saved(values: Option<BTreeMap<String, u32>>, overlay_running: bool) -> Option<Saved> {
    // shortcut: the on/off signals were only seen with the overlay off (platform-notes.md); check
    // them with it on, on a PC where toggling NVIDIA's overlay is fine, before trusting a "no clash".
    let on = overlay_running || values.as_ref().and_then(|v| v.get("IsShadowPlayEnabledUser")).is_some_and(|v| *v != 0);
    if !on {
        return Some(Saved { keys: BTreeMap::new(), off: true });
    }
    let keys = saved_keys(&values?);
    (!keys.is_empty()).then_some(Saved { keys, off: false })
}

/// NVIDIA's saved keys: `<Name>HKeyCount` holds how many keys a shortcut has, and `<Name>HKey0`,
/// `<Name>HKey1`, … one Windows virtual-key code each (`GFEOverlayHKeyV2Count` → `GFEOverlayHKeyV20`).
/// A count of 0 (or a key Backsight can't bind) maps to `None`.
fn saved_keys(values: &BTreeMap<String, u32>) -> BTreeMap<String, Option<Hotkey>> {
    values
        .iter()
        .filter_map(|(name, &count)| {
            let base = name.strip_suffix("Count").filter(|b| b.contains("HKey"))?;
            let vks: Vec<u32> =
                (0..count.min(5)).map(|i| values.get(&format!("{base}{i}")).copied()).collect::<Option<_>>()?;
            Some((base.to_owned(), combo(&vks)))
        })
        .collect()
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
        put("ChatPTTHKey", &[18, 86]);
        put("CustomOverlayBHKey", &[]);
        v.insert("IsShadowPlayEnabledUser".into(), 0);
        v
    }

    #[test]
    fn reads_nvidia_keys_from_virtual_key_codes() {
        let keys: BTreeMap<_, _> = saved_keys(&nvidia_app_defaults())
            .into_iter()
            .map(|(n, k)| (n, k.map(|k| k.to_string()).unwrap_or_default()))
            .collect();
        assert_eq!(keys["DVRHKey"], "Alt+F10");
        assert_eq!(keys["IRToggleHKey"], "Alt+Shift+F10");
        assert_eq!(keys["GFEOverlayHKeyV2"], "Alt+Z");
        assert_eq!(keys["MicToggleHKey"], "Ctrl+Alt+M");
        assert_eq!(keys["MicPTTHKey"], "Backquote");
        assert_eq!(keys["ChatPTTHKey"], "Alt+V");
        assert_eq!(keys["CustomOverlayBHKey"], "", "an unset shortcut is unbound");
    }

    #[test]
    fn the_overlay_is_off_unless_running_or_turned_on() {
        let off = saved(Some(nvidia_app_defaults()), false).unwrap();
        assert!(off.off && off.keys.is_empty());
        assert!(saved(None, false).unwrap().off, "no NVIDIA settings and no overlay: off");

        let on = saved(Some(nvidia_app_defaults()), true).unwrap();
        assert!(!on.off && on.keys.contains_key("DVRHKey"));

        let mut flag_on = nvidia_app_defaults();
        flag_on.insert("IsShadowPlayEnabledUser".into(), 1);
        assert!(!saved(Some(flag_on), false).unwrap().off);

        assert_eq!(saved(None, true), None, "running with nothing saved: its usual keys");
    }
}
