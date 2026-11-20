//! User settings for Backsight: the schema, the defaults and how they are saved.
//!
//! Defaults live in `content/default-settings.json` so the UI and the engine share
//! one source of truth. Settings are stored as TOML in `%APPDATA%\Backsight\config`.

mod hotkey;

pub use hotkey::{Hotkey, HotkeyError, conflicts};

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

const DEFAULTS_JSON: &str = include_str!("../../../content/default-settings.json");

/// Bump when a field is added, renamed or removed, and migrate older files in [`Settings::load_from`].
pub const SCHEMA_VERSION: u32 = 2;

/// Values for one encoder, keyed by catalog option id. Missing keys use the catalog default.
pub type EncoderOptions = BTreeMap<String, toml::Value>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub version: u32,
    pub general: General,
    pub video: Video,
    pub replay: Replay,
    pub audio: Audio,
    pub screenshot: Screenshot,
    /// Hotkey per action in canonical form (`"Alt+F10"`). An empty string means unbound.
    pub hotkeys: BTreeMap<Action, String>,
    /// Missing from files written before setup existed: then setup shows once.
    #[serde(default = "default_setup")]
    pub setup: Setup,
}

/// First-run setup (the encoder check and the NVIDIA overlay hotkey check).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Setup {
    /// Finished or skipped; until then the window opens on setup.
    pub done: bool,
}

fn default_setup() -> Setup {
    let defaults: serde_json::Value = serde_json::from_str(DEFAULTS_JSON).expect("valid JSON");
    serde_json::from_value(defaults["setup"].clone()).expect("a setup block in content/default-settings.json")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct General {
    /// Empty means `Videos\Backsight`.
    pub video_folder: String,
    /// Empty means `Pictures\Backsight`.
    pub screenshot_folder: String,
    pub start_with_windows: bool,
    pub minimize_to_tray: bool,
    /// Backsight's own pop-up for every action (ADR 0010).
    pub notifications: bool,
    /// A short sound for every action. Missing from older files: then the default.
    #[serde(default = "default_sounds")]
    pub sounds: bool,
    /// A red dot with the time on screen while recording, and a red tray icon.
    #[serde(default = "default_recording_indicator")]
    pub recording_indicator: bool,
    /// Where the pop-ups sit: their window's top-left as a share (0–1) of the monitor's width
    /// and height, so it fits any resolution. `None`: the top-right corner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub popup_position: Option<[f64; 2]>,
    /// Where the recording dot sits, like `popup_position`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dot_position: Option<[f64; 2]>,
    /// Saved files start with the game's name instead of "Backsight".
    #[serde(default = "default_name_after_game")]
    pub name_after_game: bool,
    /// Saved files go in a folder named after the game, inside the clips or screenshot folder.
    #[serde(default = "default_folder_per_game")]
    pub folder_per_game: bool,
}

/// `general.<key>` from `content/default-settings.json`, for a field added after version 1.
fn general_default(key: &str) -> bool {
    let defaults: serde_json::Value = serde_json::from_str(DEFAULTS_JSON).expect("valid JSON");
    defaults["general"][key].as_bool().expect("a boolean default in content/default-settings.json")
}

fn default_sounds() -> bool {
    general_default("sounds")
}

fn default_recording_indicator() -> bool {
    general_default("recording_indicator")
}

fn default_name_after_game() -> bool {
    general_default("name_after_game")
}

fn default_folder_per_game() -> bool {
    general_default("folder_per_game")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Video {
    /// Container id from `content/encoders.json`.
    pub container: String,
    /// Video encoder id from `content/encoders.json`.
    pub encoder: String,
    /// `"source"` or a height such as `"1080p"`.
    pub resolution: String,
    pub fps: u32,
    pub capture_cursor: bool,
    #[serde(default = "default_video_hdr")]
    pub hdr: bool,
    /// Start a new file every N minutes; 0 disables splitting.
    pub split_minutes: u32,
    /// Rewrap MKV recordings into MP4 once they finish.
    pub remux_to_mp4: bool,
    /// `"monitor"` or `"window"` (the game's window), from `content/encoders.json`.
    #[serde(default = "default_capture_target")]
    pub capture_target: String,
    /// The monitor to record (`wgc::Monitor::id`); empty means the main monitor.
    #[serde(default = "default_capture_monitor")]
    pub capture_monitor: String,
    /// Per-encoder values, so switching encoders and back keeps your tweaks.
    pub encoder_options: BTreeMap<String, EncoderOptions>,
}

/// `video.<key>` from `content/default-settings.json`, for a field added after version 1.
fn video_default(key: &str) -> String {
    let defaults: serde_json::Value = serde_json::from_str(DEFAULTS_JSON).expect("valid JSON");
    defaults["video"][key].as_str().expect("a string default in content/default-settings.json").to_owned()
}

fn default_video_hdr() -> bool {
    let defaults: serde_json::Value = serde_json::from_str(DEFAULTS_JSON).expect("valid JSON");
    defaults["video"]["hdr"].as_bool().expect("a video HDR default in content/default-settings.json")
}

fn default_capture_target() -> String {
    video_default("capture_target")
}

fn default_capture_monitor() -> String {
    video_default("capture_monitor")
}

fn default_clip_shape() -> String {
    let defaults: serde_json::Value = serde_json::from_str(DEFAULTS_JSON).expect("valid JSON");
    defaults["replay"]["clip_shape"].as_str().expect("a clip shape default in content/default-settings.json").to_owned()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Replay {
    pub enabled: bool,
    pub length_seconds: u32,
    /// `"wide"`, `"vertical"` or `"both"`: whether saving a clip also makes a 9:16 copy (ADR 0015).
    /// Missing from older files (then the default); anything unknown means wide.
    #[serde(default = "default_clip_shape")]
    pub clip_shape: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Audio {
    pub encoder: String,
    pub desktop: bool,
    pub microphone: bool,
    /// Endpoint id, or `"default"` to follow the Windows default microphone.
    pub microphone_device: String,
    pub game_audio_only: bool,
    pub separate_tracks: bool,
    /// Write captions next to each saved clip (ADR 0016). Missing from older files (then the default).
    #[serde(default = "default_audio_captions")]
    pub captions: bool,
    pub encoder_options: BTreeMap<String, EncoderOptions>,
}

fn default_audio_captions() -> bool {
    let defaults: serde_json::Value = serde_json::from_str(DEFAULTS_JSON).expect("valid JSON");
    defaults["audio"]["captions"].as_bool().expect("an audio captions default in content/default-settings.json")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Screenshot {
    /// Screenshot format id from `content/encoders.json`.
    pub format: String,
    /// 1–100, used by lossy formats only.
    pub quality: u8,
    pub capture_cursor: bool,
    pub copy_to_clipboard: bool,
}

/// Everything a hotkey can trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    SaveReplay,
    ToggleReplay,
    ToggleRecording,
    PauseRecording,
    Screenshot,
    ScreenshotWindow,
    ScreenshotRegion,
    ToggleMic,
    PushToTalk,
    AddMarker,
    OpenApp,
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("could not read or write the settings file: {0}")]
    Io(#[from] io::Error),
    #[error("the settings file is not valid: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("could not serialize settings: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("the settings file was written by a newer Backsight (schema {0}); update Backsight to use it")]
    TooNew(u32),
    #[error("no user profile folder was found")]
    NoConfigDir,
}

impl Default for Settings {
    fn default() -> Self {
        serde_json::from_str(DEFAULTS_JSON).expect("content/default-settings.json matches the schema")
    }
}

/// `%APPDATA%\Backsight`: settings in `config`, logs in `logs`, caches beside them.
pub fn app_dir() -> Result<PathBuf, SettingsError> {
    let config = Settings::default_path()?;
    Ok(config.ancestors().nth(2).ok_or(SettingsError::NoConfigDir)?.to_path_buf())
}

impl General {
    /// Where clips go: `video_folder`, or `Videos\Backsight` when it's empty.
    pub fn video_dir(&self) -> Option<PathBuf> {
        if !self.video_folder.is_empty() {
            return Some(PathBuf::from(&self.video_folder));
        }
        directories::UserDirs::new()?.video_dir().map(|d| d.join("Backsight"))
    }

    /// Where screenshots go: `screenshot_folder`, or `Pictures\Backsight` when it's empty.
    pub fn screenshot_dir(&self) -> Option<PathBuf> {
        if !self.screenshot_folder.is_empty() {
            return Some(PathBuf::from(&self.screenshot_folder));
        }
        directories::UserDirs::new()?.picture_dir().map(|d| d.join("Backsight"))
    }
}

impl Settings {
    /// `%APPDATA%\Backsight\config\settings.toml`.
    pub fn default_path() -> Result<PathBuf, SettingsError> {
        directories::ProjectDirs::from("", "", "Backsight")
            .map(|dirs| dirs.config_dir().join("settings.toml"))
            .ok_or(SettingsError::NoConfigDir)
    }

    pub fn load() -> Result<Self, SettingsError> {
        Self::load_from(&Self::default_path()?)
    }

    pub fn save(&self) -> Result<(), SettingsError> {
        self.save_to(&Self::default_path()?)
    }

    /// A missing file yields the defaults. A broken file is an error, never silently replaced.
    pub fn load_from(path: &Path) -> Result<Self, SettingsError> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };
        let mut settings: Self = toml::from_str(&text)?;
        if settings.version > SCHEMA_VERSION {
            return Err(SettingsError::TooNew(settings.version));
        }
        // Older files load (new fields take defaults); the next save stamps them current, so an
        // older build that meets the file later refuses it instead of overwriting it.
        settings.version = SCHEMA_VERSION;
        Ok(settings)
    }

    pub fn save_to(&self, path: &Path) -> Result<(), SettingsError> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        // Write, flush to disk, then rename: a crash or power cut mid-save never leaves a
        // half-written or empty settings file (NTFS may write the rename before the data).
        let tmp = path.with_extension("toml.tmp");
        let mut file = fs::File::create(&tmp)?;
        io::Write::write_all(&mut file, toml::to_string_pretty(self)?.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Backsight used to be called Clipwise. On the first start under the new name, copies the old
    /// `%APPDATA%\Clipwise\config` files over (the old folder stays as it is) and keeps saving
    /// into the old `Videos\Clipwise` and `Pictures\Clipwise` folders, so nothing seems lost.
    /// Returns whether anything was copied.
    pub fn adopt_old_name() -> Result<bool, SettingsError> {
        let Some(old) = directories::ProjectDirs::from("", "", "Clipwise") else { return Ok(false) };
        let user = directories::UserDirs::new();
        let old_videos = user.as_ref().and_then(|u| u.video_dir()).map(|d| d.join("Clipwise"));
        let old_pictures = user.as_ref().and_then(|u| u.picture_dir()).map(|d| d.join("Clipwise"));
        Self::adopt_old_name_from(old.config_dir(), &Self::default_path()?, old_videos, old_pictures)
    }

    fn adopt_old_name_from(
        old_config: &Path,
        new_path: &Path,
        old_videos: Option<PathBuf>,
        old_pictures: Option<PathBuf>,
    ) -> Result<bool, SettingsError> {
        let old_path = old_config.join("settings.toml");
        if new_path.exists() || !old_path.is_file() {
            return Ok(false);
        }
        let new_config = new_path.parent().ok_or(SettingsError::NoConfigDir)?;
        fs::create_dir_all(new_config)?;
        // Backups first, settings.toml last (copy beside, then rename): a crash midway leaves no
        // new settings file, so the next start copies again.
        for entry in fs::read_dir(old_config)?.flatten() {
            let name = entry.file_name();
            if name != "settings.toml" && entry.file_type().is_ok_and(|t| t.is_file()) {
                fs::copy(entry.path(), new_config.join(&name))?;
            }
        }
        // Pinned folders and the copy land in one atomic write, so a crash can't leave a copy
        // without them (the next start would then skip it). A file that doesn't load, or needs
        // no pin, is copied exactly; the normal load reports a broken one.
        if let Ok(mut settings) = Self::load_from(&old_path) {
            let mut changed = false;
            for (folder, old) in [
                (&mut settings.general.video_folder, old_videos),
                (&mut settings.general.screenshot_folder, old_pictures),
            ] {
                if let Some(old) = old.filter(|d| folder.is_empty() && d.is_dir()) {
                    *folder = old.to_string_lossy().into_owned();
                    changed = true;
                }
            }
            if changed {
                settings.save_to(new_path)?;
                return Ok(true);
            }
        }
        let tmp = new_path.with_extension("toml.tmp");
        fs::copy(&old_path, &tmp)?;
        fs::rename(&tmp, new_path)?;
        Ok(true)
    }

    /// Parses every bound hotkey. Unbound actions (empty strings) are skipped.
    pub fn parsed_hotkeys(&self) -> Result<BTreeMap<Action, Hotkey>, (Action, HotkeyError)> {
        self.hotkeys
            .iter()
            .filter(|(_, combo)| !combo.trim().is_empty())
            .map(|(action, combo)| combo.parse().map(|hk| (*action, hk)).map_err(|e| (*action, e)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("backsight-test-{}-{name}", std::process::id())).join("settings.toml")
    }

    #[test]
    fn old_name_settings_are_copied_once_and_keep_the_old_folders() {
        let root = temp_path("adopt").parent().unwrap().to_path_buf();
        let _ = fs::remove_dir_all(&root);
        let old = root.join("Clipwise/config");
        let videos = root.join("Videos/Clipwise");
        fs::create_dir_all(&old).unwrap();
        fs::create_dir_all(&videos).unwrap();
        let mut s = Settings::default();
        s.general.start_with_windows = true;
        s.save_to(&old.join("settings.toml")).unwrap();
        fs::write(old.join("settings.backup.toml"), "kept").unwrap();
        fs::create_dir_all(old.join("some folder")).unwrap(); // only files are copied
        let new = root.join("Backsight/config/settings.toml");
        let missing = root.join("Pictures/Clipwise");

        assert!(Settings::adopt_old_name_from(&old, &new, Some(videos.clone()), Some(missing.clone())).unwrap());
        let got = Settings::load_from(&new).unwrap();
        assert!(got.general.start_with_windows);
        assert_eq!(got.general.video_folder, videos.to_string_lossy());
        assert_eq!(got.general.screenshot_folder, "", "a folder that never existed isn't pinned");
        assert_eq!(fs::read_to_string(new.with_file_name("settings.backup.toml")).unwrap(), "kept");
        assert!(old.join("settings.toml").is_file(), "the old file stays");
        // Second start: the new file exists, nothing is copied again.
        assert!(!Settings::adopt_old_name_from(&old, &new, Some(videos), Some(missing)).unwrap());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_folder_the_user_picked_is_kept() {
        let root = temp_path("adopt-custom").parent().unwrap().to_path_buf();
        let _ = fs::remove_dir_all(&root);
        let old = root.join("old");
        let videos = root.join("Videos/Clipwise");
        fs::create_dir_all(&videos).unwrap();
        let mut s = Settings::default();
        s.general.video_folder = r"D:\Clips".to_owned();
        s.save_to(&old.join("settings.toml")).unwrap();
        let text = fs::read_to_string(old.join("settings.toml")).unwrap();
        let new = root.join("new/settings.toml");
        assert!(Settings::adopt_old_name_from(&old, &new, Some(videos), None).unwrap());
        assert_eq!(fs::read_to_string(&new).unwrap(), text, "copied as it was");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_unreadable_old_settings_file_is_copied_untouched() {
        let root = temp_path("adopt-broken").parent().unwrap().to_path_buf();
        let _ = fs::remove_dir_all(&root);
        let old = root.join("old");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("settings.toml"), "version = 999\n").unwrap();
        let new = root.join("new/settings.toml");
        assert!(Settings::adopt_old_name_from(&old, &new, Some(root.clone()), None).unwrap());
        assert_eq!(fs::read_to_string(&new).unwrap(), "version = 999\n");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_from_before_new_general_fields_still_loads() {
        let path = temp_path("old-general");
        let mut text = toml::to_string_pretty(&Settings::default()).unwrap();
        text = text
            .replace(
                "sounds = true
",
                "",
            )
            .replace(
                "recording_indicator = true
",
                "",
            );
        text = text.replacen("hdr = false\n", "", 1).replacen("captions = false\n", "", 1);
        text = text.replace("name_after_game = true\n", "").replace("folder_per_game = false\n", "");
        text = text.replace("capture_target = \"monitor\"\n", "").replace("capture_monitor = \"\"\n", "");
        text = text.replace("clip_shape = \"wide\"\n", "");
        assert!(!text.contains("sounds") && !text.contains("_game") && !text.contains("capture_target"), "{text}");
        assert!(!text.contains("clip_shape"), "{text}");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        let loaded = Settings::load_from(&path).unwrap();
        assert!(!loaded.video.hdr && !loaded.audio.captions);
        assert!(loaded.general.sounds && loaded.general.recording_indicator, "defaults filled in");
        assert_eq!(loaded.general.popup_position, None, "no saved spot: the corner");
        assert!(loaded.general.name_after_game && !loaded.general.folder_per_game, "file naming defaults");
        assert_eq!((loaded.video.capture_target.as_str(), loaded.video.capture_monitor.as_str()), ("monitor", ""));
        assert_eq!(loaded, Settings::default(), "every missing field takes its default");
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_file_from_before_setup_loads_and_shows_setup_once() {
        let path = temp_path("old-setup");
        let text = toml::to_string_pretty(&Settings::default()).unwrap();
        let text = text.split("[setup]").next().unwrap().to_owned();
        assert!(!text.contains("done"), "{text}");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        assert!(!Settings::load_from(&path).unwrap().setup.done);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    /// A frozen file from the version-1 build. Unlike the tests above it isn't built from today's
    /// defaults, so a schema change that would break an installed file fails here.
    #[test]
    fn a_frozen_version_1_file_loads_with_its_values_and_is_stamped_current() {
        let path = temp_path("frozen-v1");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, include_str!("../tests/fixtures/settings-v1.toml")).unwrap();
        let s = Settings::load_from(&path).unwrap();
        assert_eq!(s.version, SCHEMA_VERSION, "stamped so an older build won't overwrite it later");
        assert_eq!(s.general.video_folder, "D:\\Clips");
        assert_eq!((s.video.fps, s.replay.length_seconds, s.setup.done), (30, 300, true));
        assert_eq!(s.video.encoder_options["nvenc-h264"]["bitrate"], toml::Value::Integer(40_000));
        assert_eq!(s.hotkeys[&Action::SaveReplay], "Ctrl+F10");
        assert_eq!(s.hotkeys[&Action::OpenApp], "Alt+Shift+C");
        assert!(s.parsed_hotkeys().is_ok());
        s.save_to(&path).unwrap();
        assert!(fs::read_to_string(&path).unwrap().starts_with("version = 2\n"));
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn defaults_parse_and_bind_every_action() {
        let s = Settings::default();
        assert_eq!(s.version, SCHEMA_VERSION);
        assert_eq!(s.hotkeys.len(), 11, "every Action has an entry in default-settings.json");
    }

    #[test]
    fn default_hotkeys_are_valid_and_conflict_free() {
        let s = Settings::default();
        let parsed = s.parsed_hotkeys().expect("default hotkeys parse");
        assert!(conflicts(&parsed).is_empty());
        assert_eq!(parsed.len(), s.hotkeys.values().filter(|c| !c.is_empty()).count());
        assert!(!parsed.is_empty());
    }

    #[test]
    fn an_unreadable_settings_file_is_an_error_not_the_defaults() {
        let dir = temp_path("unreadable");
        fs::create_dir_all(&dir).unwrap(); // a folder where the file should be
        assert!(matches!(Settings::load_from(&dir), Err(SettingsError::Io(_))));
        fs::remove_dir_all(dir.parent().unwrap()).ok();
    }

    #[test]
    fn folders_default_under_the_user_folders_and_settings_under_app_data() {
        let mut g = Settings::default().general;
        g.video_folder = r"D:\Clips".into();
        g.screenshot_folder = r"D:\Shots".into();
        assert_eq!((g.video_dir(), g.screenshot_dir()), (Some(r"D:\Clips".into()), Some(r"D:\Shots".into())));
        g.video_folder.clear();
        g.screenshot_folder.clear();
        let (videos, shots) = (g.video_dir().unwrap(), g.screenshot_dir().unwrap());
        assert!(videos.ends_with("Backsight") && videos.is_absolute(), "{videos:?}");
        assert!(shots.ends_with("Backsight") && shots.is_absolute() && shots != videos, "{shots:?}");
        let app = app_dir().unwrap();
        assert!(app.ends_with("Backsight") && app.is_absolute(), "{app:?}");
        assert_eq!(Settings::default_path().unwrap(), app.join("config").join("settings.toml"));
    }

    #[test]
    fn cleared_hotkeys_save_load_and_never_conflict() {
        let path = temp_path("cleared");
        let mut s = Settings::default();
        s.hotkeys.values_mut().for_each(String::clear);
        s.save_to(&path).unwrap();
        let loaded = Settings::load_from(&path).unwrap();
        assert_eq!(loaded.hotkeys.len(), 11);
        assert!(loaded.hotkeys.values().all(String::is_empty), "no key stays no key, not the default");
        assert!(loaded.parsed_hotkeys().unwrap().is_empty());
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn save_then_load_round_trips() {
        let path = temp_path("roundtrip");
        let mut s = Settings::default();
        s.replay.length_seconds = 300;
        s.video
            .encoder_options
            .entry("nvenc-h264".into())
            .or_default()
            .insert("bitrate".into(), toml::Value::Integer(40_000));
        s.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path).unwrap(), s);
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_missing_file_and_the_defaults_are_the_current_version() {
        assert_eq!(Settings::load_from(&temp_path("nofile")).unwrap().version, SCHEMA_VERSION);
        assert_eq!(Settings::default().version, SCHEMA_VERSION);
    }

    #[test]
    fn missing_file_gives_defaults() {
        assert_eq!(Settings::load_from(&temp_path("missing")).unwrap(), Settings::default());
    }

    #[test]
    fn newer_schema_is_rejected_not_overwritten() {
        let path = temp_path("too-new");
        let s = Settings { version: SCHEMA_VERSION + 1, ..Settings::default() };
        s.save_to(&path).unwrap();
        let before = fs::read(&path).unwrap();
        assert!(matches!(Settings::load_from(&path), Err(SettingsError::TooNew(v)) if v == SCHEMA_VERSION + 1));
        assert_eq!(fs::read(&path).unwrap(), before, "loading a newer file leaves it untouched");
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}
