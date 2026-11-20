#![no_main]
//! OBS's profile `basic.ini` and scene collection JSON, separated by a NUL.
use backsight_engine::fuzz;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|text: &str| {
    let (ini, json) = text.split_once('\0').unwrap_or((text, text));
    check(&fuzz::obs(ini, json));
});

fn check(saved: &fuzz::Saved) {
    for hk in saved.keys.values().flatten() {
        assert_eq!(hk.to_string().parse().as_ref(), Ok(hk));
    }
}
