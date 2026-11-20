#![no_main]
//! Greenshot's ini, and registry values (AMD, Game Bar, NVIDIA) given as `name=value` lines (a
//! number is a DWORD, anything else text). Also the UTF-16 decoding of registry text.
use backsight_engine::fuzz;
use libfuzzer_sys::fuzz_target;
use std::collections::BTreeMap;

fuzz_target!(|data: &[u8]| {
    let _ = fuzz::utf16_le(data);
    let Ok(text) = std::str::from_utf8(data) else { return };
    let mut values = fuzz::Values::new();
    let mut numbers = BTreeMap::new();
    for line in text.lines() {
        let (k, v) = line.split_once('=').unwrap_or((line, ""));
        match v.parse::<u32>() {
            Ok(n) => {
                numbers.insert(k.to_owned(), n);
                values.insert(k.to_owned(), Ok(n));
            }
            Err(_) => {
                values.insert(k.to_owned(), Err(v.to_owned()));
            }
        }
    }
    let all = [
        Some(fuzz::greenshot(text)),
        Some(fuzz::amd(&values)),
        Some(fuzz::gamebar(&values)),
        fuzz::nvidia(&numbers, false),
        fuzz::nvidia(&numbers, true),
    ];
    for hk in all.iter().flatten().flat_map(|s| s.keys.values().flatten()) {
        assert_eq!(hk.to_string().parse().as_ref(), Ok(hk));
    }
});
