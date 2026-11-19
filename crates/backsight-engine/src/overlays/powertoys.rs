//! PowerToys' saved keys: each module's `%LOCALAPPDATA%\Microsoft\PowerToys\<Module>\settings.json`
//! holds hotkey objects `{"win":..,"ctrl":..,"alt":..,"shift":..,"code":67}` (Settings.UI.Library
//! HotkeySettings.cs); they're found by shape, wherever they sit. Ids are
//! `<ModuleFolder>.<property path>` without the `properties` and `value` wrappers, e.g.
//! `ColorPicker.ActivationShortcut`. A module turned off in the top `settings.json` `"enabled"` map
//! has all its keys unbound, its usual keys too (many modules start off). Command Palette keeps its
//! `Hotkey` in its own Store package folder. See platform-notes.md, "Other apps' hotkeys".

use super::{
    Saved,
    vk::{env_dir, hotkey, read_text},
};
use serde_json::Value;

pub fn read() -> Option<Saved> {
    let root = env_dir("LOCALAPPDATA")?.join(r"Microsoft\PowerToys");
    let Some(general) = read_text(&root.join("settings.json")) else {
        tracing::debug!("no PowerToys settings");
        return None;
    };
    let mut modules: Vec<(String, String)> = std::fs::read_dir(&root)
        .ok()?
        .filter_map(|e| {
            let e = e.ok()?;
            let text = read_text(&e.path().join("settings.json"))?;
            Some((e.file_name().to_string_lossy().into_owned(), text))
        })
        .collect();
    let cmdpal =
        env_dir("LOCALAPPDATA")?.join(r"Packages\Microsoft.CommandPalette_8wekyb3d8bbwe\LocalState\settings.json");
    modules.extend(read_text(&cmdpal).map(|text| ("CmdPal".to_owned(), text)));
    let defaults: Vec<&str> = super::entries()
        .iter()
        .filter(|e| e.reader.as_deref() == Some("powertoys"))
        .flat_map(|e| e.keys.iter().map(|k| k.id.as_str()))
        .collect();
    parse(&general, &modules, &defaults)
}

/// Keys from the top `settings.json` text and each module's (folder name, `settings.json` text).
/// `defaults` are the listed key ids (`<Module>.<property>`), unbound when their module is off.
pub(crate) fn parse(general_json: &str, modules: &[(String, String)], defaults: &[&str]) -> Option<Saved> {
    let general: Value = serde_json::from_str(general_json).ok()?;
    let enabled = general["enabled"].as_object();
    // The `enabled` names mostly match folder names, sometimes only the module's own `name`;
    // spaces and case differ ("Measure Tool", "PowerToys Run").
    let squash = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase();
    let mut saved = Saved { keys: Default::default(), off: false };
    for (folder, text) in modules {
        let Ok(module) = serde_json::from_str::<Value>(text) else { continue };
        let names = [squash(folder), module["name"].as_str().map(squash).unwrap_or_default()];
        let on = enabled
            .and_then(|m| m.iter().find(|(k, _)| names.contains(&squash(k))))
            .is_none_or(|(_, v)| v.as_bool() != Some(false));
        let mut found = Vec::new();
        walk(&module, folder, &mut found);
        for (id, key) in found {
            saved.keys.insert(id, if on { key } else { None });
        }
    }
    let off = |module: &str| {
        enabled.is_some_and(|m| m.iter().any(|(k, v)| squash(k) == squash(module) && v.as_bool() == Some(false)))
    };
    for id in defaults {
        if id.split_once('.').is_some_and(|(module, _)| off(module)) {
            saved.keys.insert((*id).to_owned(), None);
        }
    }
    Some(saved)
}

/// Every hotkey-shaped object under `v`, with its dotted path.
fn walk(v: &Value, path: &str, out: &mut Vec<(String, Option<backsight_config::Hotkey>)>) {
    match v {
        Value::Object(obj) => {
            let flag = |k: &str| obj.get(k).and_then(Value::as_bool);
            let flags = ["win", "ctrl", "alt", "shift"].map(flag);
            if let (Some(code), true) = (obj.get("code").and_then(Value::as_u64), flags.iter().all(Option::is_some)) {
                let [win, ctrl, alt, shift] = flags.map(|f| f.unwrap_or(false));
                let key = u32::try_from(code).ok().filter(|c| *c != 0).and_then(|c| hotkey(ctrl, alt, shift, win, c));
                out.push((path.to_owned(), key));
                return;
            }
            for (k, child) in obj {
                let child_path =
                    if k == "properties" || k == "value" { path.to_owned() } else { format!("{path}.{k}") };
                walk(child, &child_path, out);
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                walk(child, &format!("{path}.{i}"), out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modules(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter().map(|(a, b)| ((*a).to_owned(), (*b).to_owned())).collect()
    }

    #[test]
    fn reads_module_hotkeys_and_respects_disabled_modules() {
        let general = r#"{"enabled":{"ColorPicker":true,"Measure Tool":false,"PowerToys Run":true,"ZoomIt":false},"theme":"system"}"#;
        let mods = modules(&[
            (
                "ColorPicker",
                r#"{"properties":{"ActivationShortcut":{"win":true,"ctrl":false,"alt":false,"shift":true,"code":67,"key":"C"},"changecursor":false},"name":"ColorPicker","version":"2.1"}"#,
            ),
            (
                "Measure Tool",
                r#"{"properties":{"ActivationShortcut":{"win":true,"ctrl":true,"alt":false,"shift":true,"code":77,"key":"M"}},"name":"Measure Tool"}"#,
            ),
            (
                "PowerToys Run",
                r#"{"properties":{"open_powerlauncher":{"win":false,"ctrl":false,"alt":true,"shift":false,"code":32,"key":"Space"}},"name":"PowerToys Run"}"#,
            ),
            (
                "FancyZones",
                r#"{"properties":{"fancyzones_editor_hotkey":{"value":{"win":true,"ctrl":false,"alt":false,"shift":true,"code":192,"key":"`"}},"x":{"win":false,"ctrl":false,"alt":false,"shift":false,"code":0,"key":""}},"name":"FancyZones"}"#,
            ),
            ("Broken", "not json"),
        ]);
        let saved =
            parse(general, &mods, &["Measure Tool.ActivationShortcut", "ZoomIt.ToggleKey", "Peek.ActivationShortcut"])
                .unwrap();
        let k = |id: &str| saved.keys[id].as_ref().map(ToString::to_string);
        assert_eq!(k("ColorPicker.ActivationShortcut").as_deref(), Some("Shift+Win+C"));
        assert_eq!(k("Measure Tool.ActivationShortcut"), None, "module off");
        assert_eq!(k("PowerToys Run.open_powerlauncher").as_deref(), Some("Alt+Space"));
        assert_eq!(k("FancyZones.fancyzones_editor_hotkey").as_deref(), Some("Shift+Win+Backquote"));
        assert_eq!(k("FancyZones.x"), None, "code 0 is unbound");
        assert_eq!(k("ZoomIt.ToggleKey"), None, "a module that's off and never set up keeps no usual keys");
        assert!(!saved.keys.contains_key("Peek.ActivationShortcut"), "a module not listed keeps its usual key");
        assert_eq!(saved.keys.len(), 6);
    }

    /// Shows what this PC has with --nocapture; never fails.
    #[test]
    fn reads_this_pc() {
        match read() {
            Some(s) => println!("PowerToys: off={} keys={:?}", s.off, s.keys),
            None => println!("PowerToys: not found"),
        }
    }
}
