#![no_main]
//! settings.toml text (hand-edited or damaged) and hotkey strings: never panics, and whatever
//! loads saves and loads back the same, so saving never loses a setting.
use backsight_config::{Hotkey, Settings};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|text: &str| {
    for part in text.lines() {
        if let Ok(hk) = part.parse::<Hotkey>() {
            assert_eq!(hk.to_string().parse::<Hotkey>().as_ref(), Ok(&hk), "{part:?}");
        }
    }
    // What crash reports read from a settings file that doesn't load.
    let _ = text.parse::<toml::Table>();
    // What `Settings::load_from` does with the text.
    let Ok(settings) = toml::from_str::<Settings>(text) else { return };
    let saved = toml::to_string_pretty(&settings).expect("loaded settings save");
    let again: Settings = toml::from_str(&saved).expect("saved settings load");
    assert_eq!(toml::to_string_pretty(&again).unwrap(), saved);
});
