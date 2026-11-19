//! OBS Studio's saved keys: the active profile's `basic.ini` [Hotkeys] (values are JSON like
//! `{"bindings":[{"key":"OBS_KEY_C","alt":true}]}`) and the active scene collection's `"hotkeys"`
//! objects. OBS has no default keys, so only what's saved counts. See
//! platform-notes.md, "Other apps' hotkeys".

use super::{
    Saved,
    vk::{env_dir, ini_section, keep_bound, read_text},
};
use backsight_config::Hotkey;
use serde_json::{Map, Value};
use std::path::PathBuf;

pub fn read() -> Option<Saved> {
    // shortcut: a portable OBS keeps `config\obs-studio` next to its exe, and a moved settings
    // folder ([Locations] Configuration) moves user.ini too; neither is read here.
    let base = env_dir("APPDATA")?.join("obs-studio");
    let Some(user) = read_text(&base.join("user.ini")).or_else(|| read_text(&base.join("global.ini"))) else {
        tracing::debug!("no OBS settings");
        return None;
    };
    let basic = ini_section(&user, "Basic");
    let get = |name: &str| basic.iter().find(|(k, _)| *k == name).map(|(_, v)| *v).filter(|v| !v.is_empty());
    // OBS 31+ can keep profiles and scene collections elsewhere: global.ini [Locations] names the
    // folder that holds `obs-studio` (backslashes doubled).
    let global = read_text(&base.join("global.ini")).unwrap_or_default();
    let locations = ini_section(&global, "Locations");
    let folder = |name: &str, rest: &str| {
        locations.iter().find(|(k, v)| *k == name && !v.is_empty()).map_or_else(
            || base.join(rest),
            |(_, v)| PathBuf::from(v.replace(r"\\", r"\")).join("obs-studio").join(rest),
        )
    };
    let profile =
        get("ProfileDir").and_then(|dir| read_text(&folder("Profiles", r"basic\profiles").join(dir).join("basic.ini")));
    // Older OBS wrote the file name without `.json`.
    let scenes = get("SceneCollectionFile").and_then(|file| {
        let dir = folder("SceneCollections", r"basic\scenes");
        read_text(&dir.join(file)).or_else(|| read_text(&dir.join(format!("{file}.json"))))
    });
    Some(parse(profile.as_deref().unwrap_or_default(), scenes.as_deref().unwrap_or_default()))
}

/// Keys from the profile's `basic.ini` text and the scene collection's JSON text (either may be
/// empty). Ids are OBS's hotkey names, e.g. `OBSBasic.StartRecording` or `ReplayBuffer.Save`.
pub(crate) fn parse(profile_ini: &str, scenes_json: &str) -> Saved {
    let mut saved = Saved { keys: Default::default(), off: false };
    for (name, value) in ini_section(profile_ini, "Hotkeys") {
        // OBS writes newlines inside a value as `\n`.
        let json = serde_json::from_str::<Value>(value)
            .or_else(|_| serde_json::from_str(&value.replace("\\n", "\n").replace("\\r", "")));
        if let Ok(Value::Object(obj)) = json {
            // `{"bindings": [..]}` is the ini key's own; output hotkeys nest: `{"ReplayBuffer.Save": [..]}`.
            hotkeys(&mut saved, &obj, Some(name));
        }
    }
    if let Ok(scenes) = serde_json::from_str::<Value>(scenes_json) {
        walk(&mut saved, &scenes);
    }
    saved
}

/// Every `"hotkeys": {id: [bindings]}` object in a scene collection, sources included.
fn walk(saved: &mut Saved, v: &Value) {
    match v {
        Value::Object(obj) => {
            if let Some(Value::Object(h)) = obj.get("hotkeys") {
                hotkeys(saved, h, None);
            }
            obj.values().for_each(|child| walk(saved, child));
        }
        Value::Array(items) => items.iter().for_each(|child| walk(saved, child)),
        _ => {}
    }
}

fn hotkeys(saved: &mut Saved, obj: &Map<String, Value>, ini_name: Option<&str>) {
    for (name, bindings) in obj {
        let Some(bindings) = bindings.as_array() else { continue };
        let id = if name == "bindings" { ini_name.unwrap_or(name) } else { name };
        if bindings.is_empty() {
            keep_bound(saved, id, None);
        }
        for b in bindings {
            keep_bound(saved, id, binding(b));
        }
    }
}

/// `{"key":"OBS_KEY_C","alt":true,"shift":..,"control":..,"command":..}` to a hotkey (`command` is
/// the Windows key).
fn binding(b: &Value) -> Option<Hotkey> {
    let key = obs_key(b["key"].as_str()?)?;
    let on = |m: &str| b[m].as_bool().unwrap_or(false);
    let mods = [("control", "Ctrl+"), ("alt", "Alt+"), ("shift", "Shift+"), ("command", "Win+")];
    let prefix: String = mods.iter().filter(|(m, _)| on(m)).map(|(_, p)| *p).collect();
    format!("{prefix}{key}").parse().ok()
}

/// An `OBS_KEY_*` name to Backsight's key name; names Backsight shares (letters, digits, F-keys,
/// `HOME`, `PAGEUP`, …) pass through for the hotkey parser to check.
fn obs_key(name: &str) -> Option<String> {
    let n = name.strip_prefix("OBS_KEY_")?;
    let mapped = match n {
        "PRINT" => "PrintScreen",
        "RETURN" => "Enter",
        "ENTER" => "NumpadEnter",
        "NUMASTERISK" => "NumpadMultiply",
        "NUMPLUS" => "NumpadAdd",
        "NUMMINUS" => "NumpadSubtract",
        "NUMPERIOD" => "NumpadDecimal",
        "NUMSLASH" => "NumpadDivide",
        "ASCIITILDE" | "QUOTELEFT" => "Backquote",
        "PLUS" => "Equal",
        "APOSTROPHE" => "Quote",
        "MOUSE3" => "Mouse3",
        "MOUSE4" => "Mouse4",
        "MOUSE5" => "Mouse5",
        _ => {
            return Some(
                n.strip_prefix("NUM").filter(|d| d.len() == 1).map_or_else(|| n.into(), |d| format!("Numpad{d}")),
            );
        }
    };
    Some(mapped.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(saved: &Saved, id: &str) -> Option<String> {
        saved.keys[id].as_ref().map(ToString::to_string)
    }

    #[test]
    fn reads_profile_hotkeys() {
        let ini = r#"[Output]
Mode=Simple

[Hotkeys]
OBSBasic.StartRecording={"bindings":[{"key":"OBS_KEY_F9","alt":true}]}
OBSBasic.StopRecording={\n    "bindings": [\n        {\n            "control": true,\n            "shift": true,\n            "key": "OBS_KEY_NUM5"\n        }\n    ]\n}
ReplayBuffer={"ReplayBuffer.Save":[{"key":"OBS_KEY_PRINT","command":true}]}
OBSBasic.Screenshot={"bindings":[]}
OBSBasic.ResetStats={"bindings":[{"key":"OBS_KEY_MOUSE1"},{"key":"OBS_KEY_ASCIITILDE","alt":true}]}
"#;
        let saved = parse(ini, "");
        assert_eq!(k(&saved, "OBSBasic.StartRecording").as_deref(), Some("Alt+F9"));
        assert_eq!(k(&saved, "OBSBasic.StopRecording").as_deref(), Some("Ctrl+Shift+Numpad5"));
        assert_eq!(k(&saved, "ReplayBuffer.Save").as_deref(), Some("Win+PrintScreen"));
        assert_eq!(k(&saved, "OBSBasic.Screenshot"), None);
        assert_eq!(k(&saved, "OBSBasic.ResetStats").as_deref(), Some("Alt+Backquote"), "left click can't be bound");
        assert!(!saved.keys.contains_key("ReplayBuffer"));
    }

    /// Like the test PC's: a profile without [Hotkeys] and a scene collection with empty arrays.
    #[test]
    fn the_test_pc_has_nothing_bound() {
        let scenes = r#"{"name":"Untitled","hotkeys":{"OBSBasic.SelectScene":[]},
            "sources":[{"name":"Mic","hotkeys":{"libobs.mute":[],"libobs.push-to-talk":[]}}]}"#;
        let saved = parse("[Output]\nMode=Simple\n", scenes);
        assert!(saved.keys.values().all(Option::is_none));
        assert!(saved.keys.contains_key("libobs.mute"));
    }

    #[test]
    fn scene_and_source_hotkeys() {
        let scenes = r#"{"sources":[
            {"name":"Mic","hotkeys":{"libobs.mute":[{"key":"OBS_KEY_M","alt":true}]}},
            {"name":"Desktop","hotkeys":{"libobs.mute":[]}}]}"#;
        let saved = parse("", scenes);
        assert_eq!(k(&saved, "libobs.mute").as_deref(), Some("Alt+M"), "an unbound copy doesn't hide it");
    }

    #[test]
    fn key_names() {
        for (obs, ours) in [
            ("OBS_KEY_A", "A"),
            ("OBS_KEY_0", "0"),
            ("OBS_KEY_F24", "F24"),
            ("OBS_KEY_HOME", "Home"),
            ("OBS_KEY_PAGEUP", "PageUp"),
            ("OBS_KEY_SCROLLLOCK", "ScrollLock"),
            ("OBS_KEY_NUM0", "Numpad0"),
            ("OBS_KEY_MINUS", "Minus"),
            ("OBS_KEY_MOUSE4", "Mouse4"),
        ] {
            let b = serde_json::json!({ "key": obs });
            assert_eq!(binding(&b).map(|h| h.to_string()).as_deref(), Some(ours), "{obs}");
        }
        for obs in ["OBS_KEY_F35", "OBS_KEY_MOUSE2", "OBS_KEY_ESCAPE", "OBS_KEY_SHIFT", "C"] {
            assert_eq!(binding(&serde_json::json!({ "key": obs })), None, "{obs}");
        }
    }

    /// Shows what this PC has with --nocapture; never fails.
    #[test]
    fn reads_this_pc() {
        match read() {
            Some(s) => println!("OBS: off={} keys={:?}", s.off, s.keys),
            None => println!("OBS: not found"),
        }
    }
}
