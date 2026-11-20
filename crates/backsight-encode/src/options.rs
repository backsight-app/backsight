//! Catalog option values to FFmpeg AVOptions.
//!
//! Most options map 1:1 through the catalog's `ffmpeg` field. The rules that are not
//! 1:1 (rate control, keyframe seconds, look-ahead) live here, per encoder family.
//! See `docs/reference/platform-notes.md` for why each rule exists.

use crate::catalog::{AudioEncoder, Catalog, OptionDef, VideoEncoder};
use backsight_config::EncoderOptions;
use std::{collections::BTreeMap, sync::Mutex};
use toml::Value;

/// FFmpeg option name and value pairs, in the order they are applied.
pub type FfmpegOptions = Vec<(String, String)>;

/// Keyframe interval used when the setting is 0 ("Auto").
const AUTO_KEYINT_SEC: i64 = 2;
/// Frames NVENC looks ahead when look-ahead is on (FFmpeg's own default, 0, means off).
const LOOKAHEAD_FRAMES: i64 = 20;

/// Encoders that take fewer B-frames than their default on this PC (id → most), learned from the
/// encoder check or a refused open. NVENC HEVC on a GTX 10-series card takes none.
static BFRAME_LIMITS: Mutex<BTreeMap<String, u32>> = Mutex::new(BTreeMap::new());

/// Remembers that `encoder_id` can't use more than `max` B-frames here; [`video_options`] clamps to it.
pub fn limit_bframes(encoder_id: &str, max: u32) {
    BFRAME_LIMITS.lock().unwrap_or_else(|e| e.into_inner()).insert(encoder_id.to_owned(), max);
}

/// These `values` with B-frames off, when an NVENC encoder was asked for some (the one thing to
/// retry with when it refuses to open: NVENC rejects what the card lacks instead of clamping it).
pub fn without_bframes(encoder: &VideoEncoder, values: &EncoderOptions) -> Option<EncoderOptions> {
    let on = values.get("bframes").and_then(Value::as_integer).is_some_and(|n| n > 0);
    (encoder.family == "nvenc" && on).then(|| {
        let mut v = values.clone();
        v.insert("bframes".into(), Value::Integer(0));
        v
    })
}

/// The FFmpeg options for a video encoder, given resolved values (see [`crate::catalog::resolve`]).
pub fn video_options(encoder: &VideoEncoder, values: &EncoderOptions, fps: u32) -> FfmpegOptions {
    let family = encoder.family.as_str();
    // Only NVENC and AMF have an `rc` option; the others infer rate control from what is set.
    let has_rc = matches!(family, "nvenc" | "amf");
    let rc = values.get("rc").and_then(Value::as_str).unwrap_or_default();

    let mut out = FfmpegOptions::new();
    for def in Catalog::get().video_options(encoder).iter().filter(|d| is_visible(d, values)) {
        let Some(v) = values.get(&def.id) else { continue };
        let value = match def.id.as_str() {
            "rc" if !has_rc => continue,
            "rc" => match (family, rc) {
                ("nvenc", "cq") => "vbr",
                ("amf", "cq") => "qvbr",
                ("amf", "vbr") => "vbr_peak",
                _ => rc,
            }
            .to_string(),
            "keyint_sec" => {
                let sec = v.as_integer().filter(|s| *s > 0).unwrap_or(AUTO_KEYINT_SEC);
                (i64::from(fps) * sec).to_string()
            }
            "lookahead" => if v.as_bool() == Some(true) { LOOKAHEAD_FRAMES } else { 0 }.to_string(),
            "tune" if v.as_str() == Some("none") => continue,
            // Zero latency means no B-frames; x265 refuses to open with both.
            "bframes" if family == "x26x" && values.get("tune").and_then(Value::as_str) == Some("zerolatency") => {
                "0".to_string()
            }
            _ => ffmpeg_value(def, v),
        };
        out.push((def.ffmpeg.clone(), value));
    }

    match (family, rc) {
        // Constant quality: NVENC is `rc=vbr` + `cq=N` + `b=0`; libaom needs `b=0` next to `crf`.
        ("nvenc" | "aom", "cq") => out.push(("b".into(), "0".into())),
        // Without an `rc` option, CBR means max rate and buffer equal to the bitrate.
        (_, "cbr") if !has_rc => {
            if let Some((_, b)) = out.iter().find(|(k, _)| k == "b").cloned() {
                out.push(("maxrate".into(), b.clone()));
                out.push(("bufsize".into(), b));
            }
        }
        _ => {}
    }
    if let Some(max) = BFRAME_LIMITS.lock().unwrap_or_else(|e| e.into_inner()).get(&encoder.id) {
        for (_, v) in out.iter_mut().filter(|(k, v)| k == "bf" && v.parse::<u32>().is_ok_and(|n| n > *max)) {
            *v = max.to_string();
        }
    }
    out
}

/// The FFmpeg options for an audio encoder. Audio options all map 1:1.
pub fn audio_options(encoder: &AudioEncoder, values: &EncoderOptions) -> FfmpegOptions {
    encoder
        .options
        .iter()
        .filter(|d| is_visible(d, values))
        .filter_map(|d| values.get(&d.id).map(|v| (d.ffmpeg.clone(), ffmpeg_value(d, v))))
        .collect()
}

/// Options hidden by `showIf` don't apply (for example the bitrate in constant-quality mode).
fn is_visible(def: &OptionDef, values: &EncoderOptions) -> bool {
    def.show_if.as_ref().is_none_or(|s| values.get(&s.option).is_some_and(|v| s.values.contains(v)))
}

fn ffmpeg_value(def: &OptionDef, v: &Value) -> String {
    let scale = if def.unit.as_deref() == Some("Kbps") { 1000 } else { 1 };
    match v {
        Value::Boolean(b) => u8::from(*b).to_string(),
        Value::Integer(i) => (i * scale).to_string(),
        Value::Float(f) => (f * scale as f64).to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{catalog::resolve, ffmpeg};

    fn values(encoder: &VideoEncoder, overrides: &[(&str, Value)]) -> EncoderOptions {
        let mut v = resolve(&Catalog::get().video_options(encoder), None);
        v.extend(overrides.iter().map(|(k, val)| (k.to_string(), val.clone())));
        v
    }

    fn mapped(encoder_id: &str, overrides: &[(&str, Value)]) -> FfmpegOptions {
        let enc = Catalog::get().video_encoder(encoder_id).unwrap();
        video_options(enc, &values(enc, overrides), 60)
    }

    fn get<'a>(opts: &'a FfmpegOptions, key: &str) -> Option<&'a str> {
        opts.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    #[test]
    fn a_learned_bframe_limit_clamps_that_encoder_only() {
        // A copy under its own id, so the limit can't leak into the other tests.
        let mut enc = Catalog::get().video_encoder("nvenc-hevc").unwrap().clone();
        enc.id = "test-pascal-hevc".into();
        let v = values(&enc, &[("bframes", 2.into())]);
        assert_eq!(get(&video_options(&enc, &v, 60), "bf"), Some("2"));
        limit_bframes(&enc.id, 0);
        assert_eq!(get(&video_options(&enc, &v, 60), "bf"), Some("0"));
        assert_eq!(get(&video_options(&enc, &v, 60), "g"), Some("120"), "only B-frames are clamped");
        assert_eq!(get(&mapped("nvenc-h264", &[("bframes", 2.into())]), "bf"), Some("2"), "other encoders keep theirs");
        // Fresh installs use none: on NVENC two B-frames cost about a quarter more encoder time
        // (HEVC) and 3D work a game would feel (platform-notes.md, "What Instant Replay costs").
        assert_eq!(get(&mapped("nvenc-hevc", &[]), "bf"), Some("0"));

        assert_eq!(without_bframes(&enc, &v).unwrap()["bframes"], Value::Integer(0));
        assert!(without_bframes(&enc, &values(&enc, &[("bframes", 0.into())])).is_none());
        let x264 = Catalog::get().video_encoder("x264").unwrap();
        assert!(without_bframes(x264, &values(x264, &[])).is_none(), "NVENC only");
    }

    #[test]
    fn nvenc_constant_quality_is_vbr_with_cq_and_zero_bitrate() {
        let o = mapped("nvenc-h264", &[("rc", "cq".into()), ("cq", 19.into())]);
        assert_eq!((get(&o, "rc"), get(&o, "cq"), get(&o, "b")), (Some("vbr"), Some("19"), Some("0")));
        assert_eq!(get(&o, "maxrate"), None, "hidden options are not sent");
    }

    #[test]
    fn bitrates_are_kbps_in_the_catalog_and_bits_in_ffmpeg() {
        let o = mapped("nvenc-hevc", &[("rc", "vbr".into())]);
        assert_eq!((get(&o, "b"), get(&o, "maxrate")), (Some("20000000"), Some("30000000")));
        assert_eq!(get(&o, "cq"), None);
    }

    #[test]
    fn keyframe_seconds_become_frames_with_auto_as_two_seconds() {
        assert_eq!(get(&mapped("x264", &[("keyint_sec", 0.into())]), "g"), Some("120"));
        assert_eq!(get(&mapped("x264", &[("keyint_sec", 5.into())]), "g"), Some("300"));
    }

    #[test]
    fn lookahead_toggle_becomes_a_frame_count() {
        assert_eq!(get(&mapped("nvenc-h264", &[("lookahead", true.into())]), "rc-lookahead"), Some("20"));
        assert_eq!(get(&mapped("nvenc-h264", &[("lookahead", false.into())]), "rc-lookahead"), Some("0"));
    }

    #[test]
    fn families_without_rc_option_infer_rate_control() {
        let o = mapped("qsv-h264", &[("rc", "cbr".into())]);
        assert_eq!(get(&o, "rc"), None);
        assert_eq!((get(&o, "maxrate"), get(&o, "bufsize")), (Some("20000000"), Some("20000000")));
        let o = mapped("x264", &[("rc", "cq".into())]);
        assert_eq!((get(&o, "crf"), get(&o, "b")), (Some("21"), None));
        assert_eq!(get(&mapped("aom-av1", &[("rc", "cq".into())]), "b"), Some("0"));
        assert_eq!(get(&mapped("nvenc-h264", &[("rc", "cbr".into())]), "bufsize"), None, "NVENC has its own rc");
    }

    #[test]
    fn zero_latency_turns_x26x_b_frames_off() {
        assert_eq!(get(&mapped("x264", &[]), "bf"), Some("2"));
        assert_eq!(get(&mapped("x264", &[("tune", "zerolatency".into())]), "bf"), Some("0"));
    }

    #[test]
    fn amf_rate_control_names() {
        assert_eq!(get(&mapped("amf-h264", &[("rc", "cq".into())]), "rc"), Some("qvbr"));
        assert_eq!(get(&mapped("amf-h264", &[("rc", "vbr".into())]), "rc"), Some("vbr_peak"));
        assert_eq!(get(&mapped("x264", &[("tune", "none".into())]), "tune"), None);
        assert_eq!(get(&mapped("x264", &[("tune", "film".into())]), "tune"), Some("film"));
    }

    #[test]
    fn audio_bitrate_in_bits() {
        let aac = Catalog::get().audio_encoder("aac").unwrap();
        let o = audio_options(aac, &resolve(&aac.options, None));
        assert_eq!(get(&o, "b"), Some("192000"));
        // A hand-edited settings file may hold a decimal bitrate.
        let def = aac.options.iter().find(|d| d.ffmpeg == "b").unwrap();
        assert_eq!(ffmpeg_value(def, &Value::Float(2.5)), "2500");
    }

    /// Every option and choice the catalog offers is a name and value the linked FFmpeg accepts.
    /// Hardware encoders are only allocated, not opened, so this needs no GPU.
    #[test]
    fn every_mapped_option_is_accepted_by_ffmpeg() {
        ffmpeg::errors_only_logging();
        let c = Catalog::get();
        let mut failures = Vec::new();
        for enc in &c.video_encoders {
            for def in c.video_options(enc) {
                let tries: Vec<Value> = match def.kind {
                    crate::catalog::OptionKind::Select => def.choices.iter().map(|ch| ch.value.clone()).collect(),
                    _ => vec![def.default.clone()],
                };
                for v in tries {
                    let opts = video_options(enc, &values(enc, &[(&def.id, v.clone())]), 60);
                    if let Err(e) = ffmpeg::check_options(&enc.ffmpeg, &opts) {
                        failures.push(format!("{} {}={v}: {e}", enc.id, def.id));
                    }
                }
            }
        }
        for enc in &c.audio_encoders {
            if let Err(e) = ffmpeg::check_options(&enc.ffmpeg, &audio_options(enc, &resolve(&enc.options, None))) {
                failures.push(format!("{}: {e}", enc.id));
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    /// x264 and x265 take string options that FFmpeg only checks on open, so open them for real.
    #[test]
    fn cpu_encoders_open_with_every_preset_and_tune() {
        ffmpeg::errors_only_logging();
        let c = Catalog::get();
        let mut failures = Vec::new();
        for enc in c.video_encoders.iter().filter(|e| e.family == "x26x") {
            for def in c.video_options(enc).iter().filter(|d| d.id == "preset" || d.id == "tune") {
                for ch in &def.choices {
                    let v = values(enc, &[(&def.id, ch.value.clone())]);
                    if let Err(e) = crate::session::VideoSession::open(enc, &v, 256, 144, 60, true) {
                        failures.push(format!("{} {}={}: {e}", enc.id, def.id, ch.value));
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }
}
