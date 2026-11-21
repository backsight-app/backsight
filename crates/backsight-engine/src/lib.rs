//! The Backsight engine: the always-running core behind the tray icon.
//!
//! A [`session::ReplaySession`] runs Instant Replay end to end: [`capture`] (Windows.Graphics.Capture
//! and GPU conversion) and [`audio`] (WASAPI loopback) feed the encoders, whose packets go into
//! the [`replay`] buffer, paced by the frame [`pacer`]. [`input`] turns Raw Input into
//! [`hotkeys`] actions, [`library`] lists saved clips and screenshots, [`recovery`] finds and
//! recovers interrupted recordings, and [`logging`] writes the log. See
//! `docs/architecture/overview.md`.

pub mod audio;
pub mod autostart;
pub mod capture;
pub mod crash;
pub mod game;
pub mod hotkeys;
pub mod input;
mod job;
pub mod library;
pub mod logging;
pub mod overlays;
pub mod overload;
pub mod pacer;
pub mod recording;
pub mod recovery;
pub mod replay;
pub mod screenshot;
pub mod session;
pub mod setup;
pub mod shutdown;
pub mod sounds;
pub mod stack;
pub mod system;
pub mod tune;
pub mod vertical;
pub mod watchdog;

/// The parsers of other apps' settings, for `fuzz/` only (cargo-fuzz builds with `--cfg fuzzing`).
#[cfg(fuzzing)]
#[doc(hidden)]
pub mod fuzz {
    pub use crate::overlays::Saved;
    use crate::overlays::{amd, gamebar, greenshot, magpie, obs, powertoys, sharex, vk};
    use std::collections::BTreeMap;

    /// A registry key's values: a number, or text.
    pub type Values = BTreeMap<String, Result<u32, String>>;

    fn reg(values: &Values) -> BTreeMap<String, vk::RegValue> {
        let value = |v: &Result<u32, String>| match v {
            Ok(n) => vk::RegValue::Dword(*n),
            Err(t) => vk::RegValue::Text(t.clone()),
        };
        values.iter().map(|(k, v)| (k.clone(), value(v))).collect()
    }

    pub fn obs(profile_ini: &str, scenes_json: &str) -> Saved {
        obs::parse(profile_ini, scenes_json)
    }
    pub fn powertoys(general: &str, modules: &[(String, String)], defaults: &[&str]) -> Option<Saved> {
        powertoys::parse(general, modules, defaults)
    }
    pub fn sharex(hotkeys: &str, app: &str) -> Option<Saved> {
        sharex::parse(hotkeys, app)
    }
    pub fn greenshot(ini: &str) -> Saved {
        greenshot::parse(ini)
    }
    pub fn magpie(json: &str) -> Option<Saved> {
        magpie::parse(json)
    }
    pub fn amd(values: &Values) -> Saved {
        amd::parse(&reg(values))
    }
    pub fn gamebar(values: &Values) -> Saved {
        gamebar::parse(&reg(values))
    }
    pub fn nvidia(values: &BTreeMap<String, u32>, overlay_running: bool) -> Option<Saved> {
        crate::overlays::nvidia_saved(values, overlay_running)
    }
    pub fn utf16_le(bytes: &[u8]) -> String {
        vk::utf16_le(bytes)
    }
    pub fn recovery_names(name: &str) {
        crate::recovery::fuzz_names(name);
    }
}
