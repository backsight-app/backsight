#![no_main]
//! The JSON settings readers: PowerToys (general, then folder/settings pairs), ShareX (hotkeys,
//! app config) and Magpie. Parts are separated by NULs; the first byte picks the reader.
use backsight_engine::fuzz;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&pick, rest)) = data.split_first() else { return };
    let Ok(text) = std::str::from_utf8(rest) else { return };
    let parts: Vec<&str> = text.split('\0').collect();
    let saved = match pick % 3 {
        0 => {
            let modules: Vec<(String, String)> =
                parts[1..].chunks(2).map(|p| (p[0].to_owned(), p.get(1).copied().unwrap_or("").to_owned())).collect();
            fuzz::powertoys(parts[0], &modules, &["ColorPicker.ActivationShortcut", "AlwaysOnTop.Hotkey", "x"])
        }
        1 => fuzz::sharex(parts[0], parts.get(1).copied().unwrap_or("")),
        _ => fuzz::magpie(text),
    };
    for hk in saved.iter().flat_map(|s| s.keys.values().flatten()) {
        assert_eq!(hk.to_string().parse().as_ref(), Ok(hk));
    }
});
