//! The encoder catalog: one data file drives the engine, the settings UI and the glossary.

use backsight_config::EncoderOptions;
use serde::Deserialize;
use std::{collections::BTreeMap, sync::OnceLock};

const CATALOG_JSON: &str = include_str!("../../../content/encoders.json");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub containers: Vec<Container>,
    pub video_codecs: Vec<VideoCodec>,
    pub families: BTreeMap<String, Vec<OptionDef>>,
    pub video_encoders: Vec<VideoEncoder>,
    pub audio_encoders: Vec<AudioEncoder>,
    pub screenshot_formats: Vec<ScreenshotFormat>,
    pub resolutions: Vec<Choice>,
    pub frame_rates: Vec<Choice>,
    /// What video capture records: the monitor or the game's window.
    pub capture_targets: Vec<Choice>,
    /// Shape of saved clips: wide, vertical or both (ADR 0015).
    pub clip_shapes: Vec<Choice>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Container {
    pub id: String,
    pub label: String,
    pub ext: String,
    /// FFmpeg muxer name (`-f`).
    pub muxer: String,
    #[serde(default)]
    pub muxer_options: BTreeMap<String, String>,
    pub crash_safe: bool,
    /// Muxer options for replay clips, when they differ. Clips are written from RAM in
    /// milliseconds (through a `.part` file), so they skip the fragmented crash-safe layout,
    /// which Windows (Explorer, Photos) shows no length for, and faststart's second pass.
    #[serde(default)]
    pub clip_muxer_options: Option<BTreeMap<String, String>>,
    pub glossary: String,
    pub video_codecs: Vec<String>,
    pub audio_encoders: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct VideoCodec {
    pub id: String,
    pub label: String,
    pub glossary: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VideoEncoder {
    pub id: String,
    pub label: String,
    /// FFmpeg encoder name, e.g. `h264_nvenc`.
    pub ffmpeg: String,
    pub codec: String,
    pub family: String,
    pub vendor: String,
    pub hardware: bool,
    /// What the PC needs for this hardware encoder, shown when the probe finds it missing.
    pub requires: Option<String>,
    pub glossary: String,
    /// Can encode HDR (10-bit, BT.2020 PQ).
    #[serde(default)]
    pub hdr: bool,
    #[serde(default)]
    pub overrides: BTreeMap<String, OptionOverride>,
}

#[derive(Debug, Deserialize)]
pub struct AudioEncoder {
    pub id: String,
    pub label: String,
    pub ffmpeg: String,
    pub glossary: String,
    pub options: Vec<OptionDef>,
}

#[derive(Debug, Deserialize)]
pub struct ScreenshotFormat {
    pub id: String,
    pub label: String,
    pub ext: String,
    /// FFmpeg encoder. JPEG XR uses Windows Imaging Component.
    #[serde(default)]
    pub ffmpeg: Option<String>,
    pub glossary: String,
    pub lossy: bool,
    pub hdr: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OptionDef {
    pub id: String,
    pub label: String,
    pub glossary: String,
    pub kind: OptionKind,
    pub default: toml::Value,
    /// FFmpeg AVOption name the engine maps this option to.
    pub ffmpeg: String,
    #[serde(default)]
    pub choices: Vec<Choice>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    pub unit: Option<String>,
    #[serde(default)]
    pub advanced: bool,
    pub show_if: Option<ShowIf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OptionKind {
    Select,
    Number,
    Toggle,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Choice {
    pub value: toml::Value,
    pub label: String,
    pub glossary: Option<String>,
    pub hint: Option<String>,
}

/// Only show an option while another option has one of these values.
#[derive(Debug, Clone, Deserialize)]
pub struct ShowIf {
    pub option: String,
    #[serde(rename = "in")]
    pub values: Vec<toml::Value>,
}

/// Per-encoder tweaks to a family option (e.g. HEVC has different profiles than H.264).
#[derive(Debug, Clone, Deserialize)]
pub struct OptionOverride {
    pub default: Option<toml::Value>,
    pub choices: Option<Vec<Choice>>,
    /// The encoder has no such setting in FFmpeg (e.g. `av1_nvenc` has no `profile`).
    #[serde(default)]
    pub omit: bool,
}

impl Catalog {
    pub fn get() -> &'static Catalog {
        static CATALOG: OnceLock<Catalog> = OnceLock::new();
        CATALOG.get_or_init(|| serde_json::from_str(CATALOG_JSON).expect("content/encoders.json matches the schema"))
    }

    pub fn video_encoder(&self, id: &str) -> Option<&VideoEncoder> {
        self.video_encoders.iter().find(|e| e.id == id)
    }

    pub fn audio_encoder(&self, id: &str) -> Option<&AudioEncoder> {
        self.audio_encoders.iter().find(|e| e.id == id)
    }

    pub fn container(&self, id: &str) -> Option<&Container> {
        self.containers.iter().find(|c| c.id == id)
    }

    /// How a replay clip is saved: `c` with its `clipMuxerOptions`, if any.
    pub fn clip_container(c: &Container) -> Container {
        let mut clip = c.clone();
        if let Some(o) = &c.clip_muxer_options {
            clip.muxer_options = o.clone();
        }
        clip
    }

    /// The family options for an encoder with its overrides applied.
    pub fn video_options(&self, encoder: &VideoEncoder) -> Vec<OptionDef> {
        let mut options = self.families.get(&encoder.family).cloned().unwrap_or_default();
        options.retain(|o| !encoder.overrides.get(&o.id).is_some_and(|ov| ov.omit));
        for opt in &mut options {
            if let Some(ov) = encoder.overrides.get(&opt.id) {
                if let Some(choices) = &ov.choices {
                    opt.choices = choices.clone();
                }
                if let Some(default) = &ov.default {
                    opt.default = default.clone();
                }
            }
        }
        options
    }
}

/// Merges saved values over the defaults. Values that are unknown, the wrong type,
/// out of range or not a listed choice fall back to the default, because the
/// settings file is hand-editable.
pub fn resolve(options: &[OptionDef], saved: Option<&EncoderOptions>) -> EncoderOptions {
    options
        .iter()
        .map(|opt| {
            let value = saved.and_then(|s| s.get(&opt.id)).filter(|v| opt.accepts(v)).unwrap_or(&opt.default);
            (opt.id.clone(), value.clone())
        })
        .collect()
}

impl OptionDef {
    pub fn accepts(&self, value: &toml::Value) -> bool {
        match self.kind {
            OptionKind::Toggle => value.is_bool(),
            OptionKind::Select => self.choices.iter().any(|c| &c.value == value),
            OptionKind::Number => {
                let n = match value {
                    toml::Value::Integer(i) => *i as f64,
                    toml::Value::Float(f) => *f,
                    _ => return false,
                };
                self.min.is_none_or(|min| n >= min) && self.max.is_none_or(|max| n <= max)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backsight_config::Settings;
    use std::collections::BTreeSet;

    const GLOSSARY_JSON: &str = include_str!("../../../content/glossary.en.json");

    fn glossary_ids() -> BTreeSet<String> {
        let map: BTreeMap<String, serde_json::Value> = serde_json::from_str(GLOSSARY_JSON).unwrap();
        map.into_keys().collect()
    }

    fn all_options(c: &Catalog) -> Vec<OptionDef> {
        let video = c.video_encoders.iter().flat_map(|e| c.video_options(e));
        let audio = c.audio_encoders.iter().flat_map(|e| e.options.clone());
        video.chain(audio).collect()
    }

    #[test]
    fn amd_and_intel_default_to_their_fastest_preset() {
        let c = Catalog::get();
        for (id, option, fastest) in [("amf-h264", "quality", "speed"), ("qsv-h264", "preset", "veryfast")] {
            let enc = c.video_encoder(id).unwrap();
            let def = c.video_options(enc).into_iter().find(|o| o.id == option).unwrap();
            assert_eq!(def.default.as_str(), Some(fastest), "{id}");
        }
    }

    #[test]
    fn every_glossary_reference_exists() {
        let c = Catalog::get();
        let known = glossary_ids();
        let mut refs: Vec<&str> = Vec::new();
        refs.extend(c.containers.iter().map(|x| x.glossary.as_str()));
        refs.extend(c.video_codecs.iter().map(|x| x.glossary.as_str()));
        refs.extend(c.video_encoders.iter().map(|x| x.glossary.as_str()));
        refs.extend(c.audio_encoders.iter().map(|x| x.glossary.as_str()));
        refs.extend(c.screenshot_formats.iter().map(|x| x.glossary.as_str()));
        // Every resolution and frame-rate choice must name an entry; a missing key fails as "<none>".
        refs.extend(
            c.resolutions
                .iter()
                .chain(&c.frame_rates)
                .chain(&c.capture_targets)
                .chain(&c.clip_shapes)
                .map(|ch| ch.glossary.as_deref().unwrap_or("<none>")),
        );
        let options = all_options(c);
        for opt in &options {
            refs.push(&opt.glossary);
            refs.extend(opt.choices.iter().filter_map(|ch| ch.glossary.as_deref()));
        }
        let missing: Vec<_> = refs.into_iter().filter(|r| !known.contains(*r)).collect();
        assert!(missing.is_empty(), "missing glossary entries: {missing:?}");
    }

    #[test]
    fn every_default_is_valid_for_its_option() {
        for opt in all_options(Catalog::get()) {
            assert!(opt.accepts(&opt.default), "default of `{}` is not an allowed value", opt.id);
        }
    }

    #[test]
    fn encoders_and_containers_reference_known_ids() {
        let c = Catalog::get();
        for e in &c.video_encoders {
            assert_eq!(e.hardware, e.requires.is_some(), "{}: hardware encoders say what they need", e.id);
        }
        let codecs: BTreeSet<_> = c.video_codecs.iter().map(|x| x.id.as_str()).collect();
        for e in &c.video_encoders {
            assert!(codecs.contains(e.codec.as_str()), "{} has unknown codec", e.id);
            assert!(c.families.contains_key(&e.family), "{} has unknown family", e.id);
        }
        for k in &c.containers {
            assert!(k.video_codecs.iter().all(|v| codecs.contains(v.as_str())), "{}", k.id);
            assert!(k.audio_encoders.iter().all(|a| c.audio_encoder(a).is_some()), "{}", k.id);
        }
    }

    /// Also proves the FFmpeg link works: a missing DLL fails the whole test binary.
    #[test]
    fn linked_ffmpeg_has_every_catalog_encoder_and_muxer() {
        use rsmpeg::{avcodec::AVCodec, avformat::AVOutputFormat};
        use std::ffi::CString;
        let c = Catalog::get();
        let encoders = c.video_encoders.iter().map(|e| &e.ffmpeg);
        for name in encoders.chain(c.audio_encoders.iter().map(|a| &a.ffmpeg)) {
            let cname = CString::new(name.as_str()).unwrap();
            assert!(AVCodec::find_encoder_by_name(&cname).is_some(), "encoder {name}");
        }
        for k in &c.containers {
            let cname = CString::new(k.muxer.as_str()).unwrap();
            assert!(AVOutputFormat::guess_format(Some(&cname), None, None).is_some(), "muxer {}", k.muxer);
        }
    }

    #[test]
    fn default_settings_point_at_a_compatible_combination() {
        let (s, c) = (Settings::default(), Catalog::get());
        let encoder = c.video_encoder(&s.video.encoder).expect("default video encoder exists");
        let container = c.container(&s.video.container).expect("default container exists");
        assert!(container.video_codecs.contains(&encoder.codec));
        assert!(container.audio_encoders.contains(&s.audio.encoder));
        assert!(c.screenshot_formats.iter().any(|f| f.id == s.screenshot.format));
        assert!(c.resolutions.iter().any(|ch| ch.value.as_str() == Some(s.video.resolution.as_str())));
        assert!(c.frame_rates.iter().any(|ch| ch.value.as_integer() == Some(i64::from(s.video.fps))));
        assert!(c.capture_targets.iter().any(|ch| ch.value.as_str() == Some(s.video.capture_target.as_str())));
        assert!(c.clip_shapes.iter().any(|ch| ch.value.as_str() == Some(s.replay.clip_shape.as_str())));
    }

    #[test]
    fn clips_never_use_the_fragmented_layout() {
        let c = Catalog::get();
        for from in &c.containers {
            let clip = Catalog::clip_container(from);
            assert!(clip.muxer_options.values().all(|v| !v.contains("frag")), "{}", from.id);
        }
        assert!(Catalog::clip_container(c.container("mp4-fragmented").unwrap()).muxer_options.is_empty());
    }

    #[test]
    fn resolve_keeps_valid_values_and_rejects_bad_ones() {
        let c = Catalog::get();
        let options = c.video_options(c.video_encoder("nvenc-hevc").unwrap());
        let saved = EncoderOptions::from([
            ("bitrate".into(), toml::Value::Integer(40_000)),
            ("preset".into(), toml::Value::String("p99".into())),
            ("bframes".into(), toml::Value::Integer(99)),
            ("profile".into(), toml::Value::String("main10".into())),
            ("unknown".into(), toml::Value::Boolean(true)),
        ]);
        let r = resolve(&options, Some(&saved));
        assert_eq!(r["bitrate"], toml::Value::Integer(40_000));
        assert_eq!(r["preset"], options.iter().find(|o| o.id == "preset").unwrap().default);
        assert_eq!(r["bframes"], options.iter().find(|o| o.id == "bframes").unwrap().default);
        assert_eq!(r["profile"], toml::Value::String("main10".into()));
        assert!(!r.contains_key("unknown"));
    }
}
