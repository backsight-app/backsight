//! Other apps' hotkeys: which overlay, chat, recording and screenshot apps this PC has, what keys
//! they use, and which of Backsight's keys another program holds (the UI matches the rest). Read only: Backsight never changes
//! another app's settings, never opens another process, and never hooks or sends keys. See
//! platform-notes.md, "Other apps' hotkeys", and ADR 0017.
//!
//! `content/overlays.json` lists each app's detection signals and default keys; a reader per app
//! (`overlays/<reader>.rs`) returns the keys saved in its settings, merged over the defaults.

pub mod amd;
pub mod gamebar;
pub mod greenshot;
pub mod magpie;
mod nvidia;
pub mod obs;
pub mod powertoys;
pub mod sharex;
pub mod vk;

use backsight_config::{Action, Hotkey, Settings};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    sync::OnceLock,
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_HOTKEY_ALREADY_REGISTERED, ERROR_SUCCESS},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
            },
            Registry::{
                HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegCloseKey,
                RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
            },
        },
        UI::Input::KeyboardAndMouse::{
            MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey, UnregisterHotKey,
        },
    },
    core::{PCWSTR, PWSTR},
};

/// What an app's reader found in its settings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Saved {
    /// Key id (the app's own name for the shortcut) to its key; `None` = unbound or turned off.
    /// Ids not in the map keep their default.
    pub keys: BTreeMap<String, Option<Hotkey>>,
    /// The app's hotkeys are all off.
    pub off: bool,
}

/// Whether Backsight still sees a key the app also uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Passes {
    /// The app only watches keys: both apps act on one press.
    Both,
    /// The app takes the key (RegisterHotKey or a swallowing hook): Backsight may never see it.
    Hidden,
    Unknown,
}

/// Where the app's keys work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    Always,
    /// Only inside games (in-game overlays).
    InGame,
}

/// What this PC has: the apps found, and Backsight keys another program holds.
#[derive(Debug, Clone, Serialize)]
pub struct Scan {
    pub apps: Vec<App>,
    pub taken: Vec<Taken>,
}

/// An app found on this PC (installed or running) with at least one key.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct App {
    pub id: String,
    pub name: String,
    pub running: bool,
    /// The keys were read from the app's settings (otherwise they're its usual keys).
    pub saved: bool,
    pub passes: Passes,
    pub scope: Scope,
    pub keys: Vec<AppKey>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AppKey {
    pub key: String,
    /// What the key does in that app, e.g. "saves a Discord clip".
    pub does: String,
}

/// A Backsight key that another program already holds (RegisterHotKey), and that no listed app
/// explains. Backsight's key then doesn't work at all.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Taken {
    pub action: Action,
    pub key: String,
}

/// One app in `content/overlays.json`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    id: String,
    name: String,
    #[serde(default)]
    processes: Vec<String>,
    /// Uninstall-key DisplayName prefixes.
    #[serde(default)]
    uninstall: Vec<String>,
    /// Store package name prefixes.
    #[serde(default)]
    packages: Vec<String>,
    /// Program files (with `%ENV%`); a config folder alone doesn't count.
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    reader: Option<String>,
    passes: Passes,
    scope: Scope,
    keys: Vec<EntryKey>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryKey {
    id: String,
    key: Option<String>,
    does: String,
}

fn entries() -> &'static [Entry] {
    static LIST: OnceLock<Vec<Entry>> = OnceLock::new();
    LIST.get_or_init(|| {
        serde_json::from_str(include_str!("../../../../content/overlays.json")).unwrap_or_else(|e| {
            tracing::error!("content/overlays.json doesn't parse: {e}");
            Vec::new()
        })
    })
}

/// Scans this PC: the apps found, then a RegisterHotKey probe of Backsight's keys. Reads the
/// registry, files and the process list, so call it off the UI thread, and not while a game plays.
pub fn scan(settings: &Settings) -> Scan {
    let apps = apps();
    let taken = probe(bound(settings).collect())
        .into_iter()
        .map(|(action, hk)| Taken { action, key: hk.to_string() })
        .filter(|t| !explained(&apps, &t.key))
        .collect();
    Scan { apps, taken }
}

/// A held key that a listed app could be holding: one that is running (or has no process to look
/// for, like Windows) and doesn't only watch keys (`both` apps never hold one).
fn explained(apps: &[App], key: &str) -> bool {
    let has_process = |id: &str| entries().iter().any(|e| e.id == id && !e.processes.is_empty());
    apps.iter()
        .any(|a| a.passes != Passes::Both && (a.running || !has_process(&a.id)) && a.keys.iter().any(|k| k.key == key))
}

fn bound(settings: &Settings) -> impl Iterator<Item = (Action, Hotkey)> + '_ {
    settings.hotkeys.iter().filter_map(|(action, combo)| Some((*action, combo.parse().ok()?)))
}

fn apps() -> Vec<App> {
    let here = Here::read();
    entries()
        .iter()
        .filter_map(|e| {
            let running = here.detect(e)?;
            let saved = e.reader.as_deref().and_then(reader).and_then(|read| read());
            Some(app(e, running, saved))
        })
        .filter(|a| !a.keys.is_empty())
        .collect()
}

/// An app's keys: what its settings say, over its defaults.
fn app(e: &Entry, running: bool, saved: Option<Saved>) -> App {
    let read = saved.is_some();
    let saved = saved.unwrap_or_default();
    let mut keys = Vec::new();
    if !saved.off {
        for k in &e.keys {
            let hk = match saved.keys.get(&k.id) {
                Some(hk) => hk.clone(),
                None => k.key.as_deref().and_then(|s| s.parse().ok()),
            };
            keys.extend(hk.map(|hk| AppKey { key: hk.to_string(), does: k.does.clone() }));
        }
        for (id, hk) in &saved.keys {
            if !e.keys.iter().any(|k| k.id == *id) {
                keys.extend(
                    hk.as_ref().map(|hk| AppKey { key: hk.to_string(), does: format!("is a shortcut in {}", e.name) }),
                );
            }
        }
    }
    App { id: e.id.clone(), name: e.name.clone(), running, saved: read, passes: e.passes, scope: e.scope, keys }
}

/// The reader named in overlays.json.
fn reader(name: &str) -> Option<fn() -> Option<Saved>> {
    let read: fn() -> Option<Saved> = match name {
        "nvidia" => nvidia::read,
        "amd" => amd::read,
        "obs" => obs::read,
        "sharex" => sharex::read,
        "greenshot" => greenshot::read,
        "magpie" => magpie::read,
        "gamebar" => gamebar::read,
        "powertoys" => powertoys::read,
        "snipping" => snipping,
        _ => return None,
    };
    Some(read)
}

/// PrintScreen opens Snipping Tool unless `PrintScreenKeyForSnippingEnabled` is 0.
fn snipping() -> Option<Saved> {
    // shortcut: a missing value keeps the default (on, as on Windows 11 22621+); Windows 10 PCs
    // then show PrintScreen as Snipping Tool's. Check the build if Windows 10 reports come in.
    let on = reg_dword(HKEY_CURRENT_USER, r"Control Panel\Keyboard", "PrintScreenKeyForSnippingEnabled")? != 0;
    let key = if on { "PrintScreen".parse().ok() } else { None };
    Some(Saved { keys: BTreeMap::from([("PrintScreenKeyForSnippingEnabled".to_owned(), key)]), off: false })
}

/// What's installed and running, read once per scan.
#[derive(Default)]
struct Here {
    /// Running exe names, lower case.
    processes: HashSet<String>,
    /// Uninstall DisplayNames, lower case, with their install folder.
    programs: Vec<(String, Option<String>)>,
    /// Store package names, lower case, with their folder.
    packages: Vec<(String, Option<String>)>,
}

const UNINSTALL: [(HKEY, &str); 3] = [
    (HKEY_LOCAL_MACHINE, r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"),
    (HKEY_LOCAL_MACHINE, r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall"),
    (HKEY_CURRENT_USER, r"Software\Microsoft\Windows\CurrentVersion\Uninstall"),
];
const PACKAGES: &str =
    r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\Repository\Packages";

impl Here {
    fn read() -> Self {
        let mut programs = Vec::new();
        for (root, base) in UNINSTALL {
            for sub in subkeys(root, base) {
                let path = format!(r"{base}\{sub}");
                if let Some(name) = reg_string(root, &path, "DisplayName") {
                    programs.push((name.to_lowercase(), reg_string(root, &path, "InstallLocation")));
                }
            }
        }
        let packages = subkeys(HKEY_CURRENT_USER, PACKAGES)
            .into_iter()
            .map(|p| {
                let folder = reg_string(HKEY_CURRENT_USER, &format!(r"{PACKAGES}\{p}"), "PackageRootFolder");
                (p.to_lowercase(), folder)
            })
            .collect();
        Here { processes: processes(), programs, packages }
    }

    /// `Some(running)` when the app is installed or running.
    fn detect(&self, e: &Entry) -> Option<bool> {
        let running = e.processes.iter().any(|p| self.processes.contains(&p.to_lowercase()));
        // The folder is checked only for a name match: a listing on an offline drive is slow.
        let prefixed = |list: &[(String, Option<String>)], prefixes: &[String]| {
            prefixes
                .iter()
                .any(|p| list.iter().any(|(n, folder)| n.starts_with(&p.to_lowercase()) && still_there(folder.clone())))
        };
        let installed = prefixed(&self.programs, &e.uninstall)
            || prefixed(&self.packages, &e.packages)
            || e.paths.iter().any(|p| expand(p).is_some_and(|p| p.is_file()));
        (running || installed).then_some(running)
    }
}

/// A listing's folder is still there, or the listing names none (a stale entry left by an
/// uninstall doesn't count).
fn still_there(folder: Option<String>) -> bool {
    folder.map(|f| f.trim().trim_matches('"').to_owned()).is_none_or(|f| f.is_empty() || PathBuf::from(f).exists())
}

/// `%VAR%\rest` with each variable filled in; `None` when one isn't set.
fn expand(path: &str) -> Option<PathBuf> {
    let mut out = String::new();
    for (i, part) in path.split('%').enumerate() {
        if i % 2 == 1 {
            out.push_str(&std::env::var(part).ok()?);
        } else {
            out.push_str(part);
        }
    }
    Some(out.into())
}

/// Running exe names, lower case. Toolhelp names only: never opens another process.
fn processes() -> HashSet<String> {
    let mut out = HashSet::new();
    // SAFETY: the snapshot handle is closed below; `entry.dwSize` is set as the calls require.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        while more {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            out.insert(String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase());
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
    }
    out
}

/// The keys another program holds with RegisterHotKey. Each key is held for a moment on a
/// short-lived thread and released at once. Can't see hooks, Raw Input listeners or in-game
/// overlays; mouse keys are skipped.
fn probe(keys: Vec<(Action, Hotkey)>) -> Vec<(Action, Hotkey)> {
    // Two scans at once would see each other's registration as another program's.
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    std::thread::spawn(move || keys.into_iter().filter(|(_, hk)| held(hk)).collect()).join().unwrap_or_default()
}

fn held(hk: &Hotkey) -> bool {
    const ID: i32 = 0xB5;
    let Some(vk) = vk_code(&hk.key) else { return false };
    // Windows keeps plain F12 for debuggers, so registering it always fails, yet presses still reach
    // Backsight's Raw Input.
    if vk == 0x7B && !(hk.ctrl || hk.alt || hk.shift || hk.win) {
        return false;
    }
    let mods = [(hk.ctrl, MOD_CONTROL), (hk.alt, MOD_ALT), (hk.shift, MOD_SHIFT), (hk.win, MOD_WIN)]
        .into_iter()
        .filter(|(on, _)| *on)
        .fold(MOD_NOREPEAT, |m, (_, f)| m | f);
    // SAFETY: a thread hotkey (no window), unregistered on the same thread right away.
    match unsafe { RegisterHotKey(None, ID, mods, vk) } {
        Ok(()) => {
            let _ = unsafe { UnregisterHotKey(None, ID) };
            false
        }
        Err(e) => e.code() == ERROR_HOTKEY_ALREADY_REGISTERED.to_hresult(),
    }
}

/// A Backsight key name's virtual-key code; `None` for mouse buttons, and for NumpadEnter (it
/// shares Enter's code).
fn vk_code(key: &str) -> Option<u32> {
    (0x07..=0xFF).find(|&v| vk::key_name(v).is_some_and(|n| n == key))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn reg_dword(root: HKEY, path: &str, name: &str) -> Option<u32> {
    let (path, name) = (wide(path), wide(name));
    let (mut data, mut len) = (0u32, 4u32);
    // SAFETY: NUL-terminated strings and a 4-byte buffer whose size is passed with it.
    let err = unsafe {
        RegGetValueW(
            root,
            PCWSTR(path.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut data).cast()),
            Some(&mut len),
        )
    };
    (err == ERROR_SUCCESS).then_some(data)
}

/// A REG_SZ (or expanded REG_EXPAND_SZ) value; `None` when missing or over 2 KB.
fn reg_string(root: HKEY, path: &str, name: &str) -> Option<String> {
    let (path, name) = (wide(path), wide(name));
    let mut buf = [0u16; 1024];
    let mut len = size_of_val(&buf) as u32;
    // SAFETY: NUL-terminated strings and a buffer whose size in bytes is passed with it.
    let err = unsafe {
        RegGetValueW(
            root,
            PCWSTR(path.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
    };
    if err != ERROR_SUCCESS {
        return None;
    }
    let chars = &buf[..(len as usize / 2).min(buf.len())];
    let end = chars.iter().position(|&c| c == 0).unwrap_or(chars.len());
    Some(String::from_utf16_lossy(&chars[..end]))
}

fn subkeys(root: HKEY, path: &str) -> Vec<String> {
    let path = wide(path);
    let mut key = HKEY::default();
    // SAFETY: `path` is NUL-terminated and `key` a live out-pointer.
    if unsafe { RegOpenKeyExW(root, PCWSTR(path.as_ptr()), None, KEY_READ, &mut key) } != ERROR_SUCCESS {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut name = [0u16; 256];
    for i in 0.. {
        let mut len = name.len() as u32;
        // SAFETY: `name` is a live buffer whose length is passed with it.
        let err = unsafe { RegEnumKeyExW(key, i, Some(PWSTR(name.as_mut_ptr())), &mut len, None, None, None, None) };
        if err != ERROR_SUCCESS {
            break; // ERROR_NO_MORE_ITEMS, or a name over 255 characters (key names can't be)
        }
        out.push(String::from_utf16_lossy(&name[..len as usize]));
    }
    // SAFETY: `key` was opened above and is closed once.
    let _ = unsafe { RegCloseKey(key) };
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Backsight key that a detected app also uses. The UI finds these itself (`clashesFor` in
    /// app/src/lib/overlays.ts); this copy tests the app list.
    #[derive(Debug, PartialEq)]
    struct Clash {
        action: Action,
        key: String,
        app: String,
        app_id: String,
        does: String,
        passes: Passes,
    }

    fn find_clashes(settings: &Settings, apps: &[App]) -> Vec<Clash> {
        let mut out = Vec::new();
        for (action, hk) in bound(settings) {
            let key = hk.to_string();
            for app in apps {
                for k in app.keys.iter().filter(|k| k.key == key) {
                    out.push(Clash {
                        action,
                        key: key.clone(),
                        app: app.name.clone(),
                        app_id: app.id.clone(),
                        does: k.does.clone(),
                        passes: app.passes,
                    });
                }
            }
        }
        out
    }

    fn entry(id: &str) -> &'static Entry {
        entries().iter().find(|e| e.id == id).unwrap()
    }

    /// Every listed app with its default keys, as if all were installed.
    fn all_defaults() -> Vec<App> {
        entries().iter().map(|e| app(e, false, None)).collect()
    }

    fn json(text: &str) -> BTreeMap<Action, String> {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn the_app_list_parses_and_every_key_is_a_hotkey() {
        let list: Vec<Entry> = serde_json::from_str(include_str!("../../../../content/overlays.json")).unwrap();
        assert_eq!(list.len(), entries().len());
        let mut ids = HashSet::new();
        for e in &list {
            assert!(ids.insert(&e.id), "app id {} twice", e.id);
            let mut key_ids = HashSet::new();
            for k in &e.keys {
                assert!(key_ids.insert(&k.id), "{}: key id {} twice", e.id, k.id);
                if let Some(key) = &k.key {
                    let hk: Hotkey = key.parse().unwrap_or_else(|err| panic!("{}: {key}: {err}", e.id));
                    assert_eq!(hk.to_string(), *key, "{}: write keys the canonical way", e.id);
                }
                assert!(!k.does.is_empty() && !k.does.ends_with('.'), "{}: {}", e.id, k.id);
            }
            if let Some(r) = &e.reader {
                assert!(reader(r).is_some(), "{}: no reader called {r}", e.id);
            }
        }
    }

    #[test]
    fn backsight_defaults_clash_with_exactly_these_apps() {
        let found: Vec<_> = find_clashes(&Settings::default(), &all_defaults())
            .into_iter()
            .map(|c| (c.action, c.key, c.app_id))
            .collect();
        let expected = [
            (Action::SaveReplay, "Alt+C", "discord"),
            (Action::SaveReplay, "Alt+C", "discord-ptb"),
            (Action::SaveReplay, "Alt+C", "discord-canary"),
            (Action::ToggleRecording, "Alt+R", "nvidia"),
            (Action::ToggleRecording, "Alt+R", "amd"),
            (Action::PauseRecording, "Alt+Shift+R", "nvidia"),
            (Action::Screenshot, "Alt+S", "steelseries"),
            (Action::ScreenshotWindow, "Alt+Shift+S", "magpie"),
            (Action::ScreenshotRegion, "Ctrl+Alt+S", "bandicam"),
            (Action::ScreenshotRegion, "Ctrl+Alt+S", "lossless-scaling"),
            (Action::OpenApp, "Alt+Shift+O", "intel-graphics"),
        ]
        .map(|(a, k, id)| (a, k.to_owned(), id.to_owned()));
        assert_eq!(found, expected);
    }

    #[test]
    fn previous_backsight_defaults_clash_with_nvidia_defaults() {
        let mut s = Settings::default();
        s.hotkeys.extend(json(include_str!("../../../../content/previous-default-hotkeys.json")));
        let nvidia = [app(entry("nvidia"), true, None)];
        let found: Vec<_> = find_clashes(&s, &nvidia).into_iter().map(|c| (c.action, c.key, c.does)).collect();
        for (action, key, does) in [
            (Action::SaveReplay, "Alt+F10", "saves NVIDIA's Instant Replay"),
            (Action::ToggleRecording, "Alt+F9", "starts and stops NVIDIA's recording"),
            (Action::Screenshot, "Alt+F1", "takes an NVIDIA screenshot"),
            (Action::ToggleReplay, "Alt+Shift+F10", "turns NVIDIA's Instant Replay on and off"),
        ] {
            assert!(found.contains(&(action, key.to_owned(), does.to_owned())), "{found:?}");
        }
    }

    #[test]
    fn the_fallback_keys_clear_every_app() {
        // The clash card moves a key whose default is taken to its fallback key.
        let fallback = json(include_str!("../../../../content/fallback-hotkeys.json"));
        let mut s = Settings::default();
        s.hotkeys.values_mut().for_each(String::clear);
        s.hotkeys.extend(fallback.clone());
        assert_eq!(find_clashes(&s, &all_defaults()), []);

        let mut s = Settings::default();
        s.hotkeys.extend(fallback);
        let parsed = s.parsed_hotkeys().expect("fallback keys parse");
        assert!(backsight_config::conflicts(&parsed).is_empty(), "fallback keys don't clash with the defaults");
    }

    #[test]
    fn the_ui_gets_plain_names() {
        // The shapes `Scan` has in app/src/lib/overlays.ts.
        let taken = Taken { action: Action::ScreenshotRegion, key: "Ctrl+Alt+S".into() };
        let taken = serde_json::to_value(Scan { apps: vec![], taken: vec![taken] }).unwrap()["taken"][0].clone();
        assert_eq!(taken, serde_json::json!({ "action": "screenshot-region", "key": "Ctrl+Alt+S" }));
        let bandicam = serde_json::to_value(app(entry("bandicam"), true, None)).unwrap();
        assert_eq!(bandicam["id"], "bandicam");
        assert_eq!(bandicam["running"], true);
        assert_eq!(bandicam["saved"], false);
        assert_eq!(bandicam["passes"], "unknown");
        assert_eq!(bandicam["scope"], "always");
        assert!(
            bandicam["name"].is_string()
                && bandicam["keys"][0]["key"].is_string()
                && bandicam["keys"][0]["does"].is_string()
        );
        assert_eq!(serde_json::to_value(app(entry("steam"), false, None)).unwrap()["scope"], "in-game");
    }

    #[test]
    fn a_cleared_key_never_clashes() {
        let mut s = Settings::default();
        s.hotkeys.values_mut().for_each(String::clear);
        assert!(find_clashes(&s, &all_defaults()).is_empty());
    }

    #[test]
    fn saved_keys_replace_the_defaults() {
        let hk = |s: &str| s.parse::<Hotkey>().ok();
        let saved = Saved {
            keys: BTreeMap::from([
                ("SAVE_CLIP".to_owned(), hk("Alt+K")),
                ("SAVE_SCREENSHOT".to_owned(), None),
                ("PUSH_TO_TALK".to_owned(), hk("Mouse4")),
                ("SOMETHING_NEW".to_owned(), hk("Ctrl+F5")),
            ]),
            off: false,
        };
        let discord = app(entry("discord"), true, Some(saved));
        assert!(discord.saved && discord.running);
        let keys: Vec<_> = discord.keys.iter().map(|k| (k.key.as_str(), k.does.as_str())).collect();
        assert_eq!(
            keys,
            [
                ("Alt+K", "saves a Discord clip if Discord Clips is on"),
                ("Shift+Backquote", "unlocks Discord's overlay"),
                ("Ctrl+Backquote", "opens Discord's soundboard"),
                ("Mouse4", "is Discord's push-to-talk key"),
                ("Ctrl+F5", "is a shortcut in Discord"),
            ]
        );

        let off = app(entry("nvidia"), false, Some(Saved { keys: BTreeMap::new(), off: true }));
        assert!(off.keys.is_empty(), "an app whose keys are off has none");
        assert!(!app(entry("nvidia"), false, None).saved);
    }

    #[test]
    fn detection_needs_a_live_signal() {
        let here = Here {
            processes: HashSet::from(["discord.exe".to_owned()]),
            programs: vec![
                ("steam".to_owned(), None),
                ("obs studio 33.0.1".to_owned(), None),
                ("teamspeak 3 client".to_owned(), Some(r"C:\No Such Folder\TeamSpeak".to_owned())),
            ],
            packages: vec![("microsoft.xboxgamingoverlay_7.326.0_x64__8wekyb3d8bbwe".to_owned(), None)],
        };
        assert_eq!(here.detect(entry("discord")), Some(true));
        assert_eq!(here.detect(entry("obs")), Some(false));
        assert_eq!(here.detect(entry("steam")), Some(false));
        assert_eq!(here.detect(entry("gamebar")), Some(false));
        // Entries checked here have no program-file paths, so this PC's disk can't change the answer.
        assert!(entry("snipping-tool").paths.is_empty());
        assert_eq!(here.detect(entry("snipping-tool")), None);
        assert!(entry("teamspeak").paths.is_empty());
        assert_eq!(here.detect(entry("teamspeak")), None, "a name match whose folder is gone doesn't count");
        assert!(still_there(None) && still_there(Some(String::new())));
        assert!(!still_there(Some(r"C:\No Such Folder\ShareX".into())), "a stale listing doesn't count");
        assert!(expand(r"%SystemRoot%\explorer.exe").is_some_and(|p| p.is_file()));
        assert_eq!(expand(r"%NO_SUCH_VARIABLE_HERE%\x.exe"), None);
    }

    #[test]
    fn backsight_keys_have_virtual_key_codes() {
        let codes: Vec<_> = ["A", "0", "F1", "F24", "Numpad9", "PrintScreen", "Backquote", "Space"].map(vk_code).into();
        assert_eq!(codes, [0x41, 0x30, 0x70, 0x87, 0x69, 0x2C, 0xC0, 0x20].map(Some));
        assert_eq!(vk_code("Mouse4"), None, "mouse keys aren't probed");
        assert!(!held(&"F12".parse().unwrap()), "plain F12 is reserved for debuggers, not held");
    }

    #[test]
    fn only_apps_that_can_hold_a_key_explain_a_held_key() {
        let running: Vec<App> = entries().iter().map(|e| app(e, true, None)).collect();
        assert!(!explained(&running, "Alt+C"), "Discord only watches Alt+C, so something else holds it");
        assert!(explained(&running, "Alt+R"), "NVIDIA's overlay can hold Alt+R");
        assert!(!explained(&running, "Ctrl+Alt+F11"));
        let idle = all_defaults();
        assert!(!explained(&idle, "Alt+R"), "an app that isn't running holds nothing");
        assert!(explained(&idle, "Win+PrintScreen"), "Windows has no process to look for");
    }

    #[test]
    fn the_probe_returns_without_panicking() {
        let keys: Vec<_> =
            bound(&Settings::default()).chain([(Action::PushToTalk, "Mouse4".parse().unwrap())]).collect();
        let taken = probe(keys);
        println!("held by another program: {taken:?}");
    }

    /// Shows what this PC has with --nocapture; never fails.
    #[test]
    fn reads_this_pc() {
        let scan = scan(&Settings::default());
        for a in &scan.apps {
            let keys: Vec<_> = a.keys.iter().map(|k| k.key.as_str()).collect();
            println!("{} (running {}, saved {}): {}", a.name, a.running, a.saved, keys.join(", "));
        }
        println!("taken: {:?}", scan.taken);
    }
}
