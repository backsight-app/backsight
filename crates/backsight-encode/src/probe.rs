//! Encoder probe: which catalog encoders work on this PC.
//!
//! Each video encoder encodes one black 256×144 frame with its defaults. The UI greys out
//! the ones that fail and shows the catalog's plain-language `requires` text.

use crate::{
    catalog::Catalog,
    ffmpeg::capture_log,
    options::{limit_bframes, without_bframes},
    session::{VideoSession, default_values},
};
use backsight_config::EncoderOptions;
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeResult {
    pub encoder_id: String,
    /// `None` when the encoder works.
    pub problem: Option<Problem>,
    /// Set when the encoder works only with fewer B-frames than the probe's 2: `Some(0)` for HEVC on
    /// a GTX 10-series card. [`probe_video_encoders_cached`] hands it to the session options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bframes: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Problem {
    /// Shown to the user, e.g. "Needs an NVIDIA RTX 40-series graphics card or newer."
    pub reason: String,
    /// FFmpeg's error, for the log.
    pub detail: String,
    /// The test froze or crashed instead of answering. Cached like a success (until the driver
    /// changes) so later checks don't wait for it again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stuck: bool,
}

const CPU_ENCODER_BROKEN: &str = "This encoder is missing from this copy of Backsight. Reinstall Backsight to fix it.";

#[derive(Serialize, Deserialize)]
struct Cache {
    key: String,
    results: Vec<ProbeResult>,
    /// The Backsight process that wrote it: froze/crashed results are only trusted by that run.
    #[serde(default)]
    pid: u32,
}

/// Encoder results, cached in `path` under `drivers` (the engine's `drivers_key()`). `probe` tests
/// the ids it's given and answers in the same order (`None`: couldn't be tested). Working encoders
/// are trusted until a driver, FFmpeg, Backsight or the catalog's encoder list changes. Failed ones
/// are tried again every time: the failure may have been temporary (another app holding every
/// NVENC session, a GPU reset), and failing is fast. An encoder that froze or crashed the test is
/// kept like a success for the rest of this run of Backsight (waiting out its time limit again won't
/// help soon), and tested again after a restart: one slow test mustn't switch it off for good.
/// Limits found on the way (see [`ProbeResult::max_bframes`]) apply to every session from here on.
pub fn probe_video_encoders_cached(
    path: &Path,
    drivers: &str,
    probe: impl FnOnce(&[&str]) -> Vec<Option<ProbeResult>>,
) -> Vec<ProbeResult> {
    let key = format!(
        "backsight {} ffmpeg {} probe 2 drivers {drivers}",
        env!("CARGO_PKG_VERSION"),
        crate::ffmpeg::version()
    );
    let ids: Vec<&str> = Catalog::get().video_encoders.iter().map(|e| e.id.as_str()).collect();
    cached(path, &key, &ids, probe)
}

fn cached(
    path: &Path,
    key: &str,
    ids: &[&str],
    probe: impl FnOnce(&[&str]) -> Vec<Option<ProbeResult>>,
) -> Vec<ProbeResult> {
    let saved = fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Cache>(&t).ok());
    let valid = saved
        .as_ref()
        .filter(|c| c.key == key && c.results.iter().map(|r| r.encoder_id.as_str()).eq(ids.iter().copied()));
    let trusted = |id: &str| {
        valid.and_then(|c| {
            c.results.iter().find(|r| {
                r.encoder_id == id && r.problem.as_ref().is_none_or(|p| p.stuck && c.pid == std::process::id())
            })
        })
    };
    let todo: Vec<&str> = ids.iter().copied().filter(|id| trusted(id).is_none()).collect();
    let mut fresh = probe(&todo).into_iter();
    let mut untested = false;
    let results: Vec<ProbeResult> = ids
        .iter()
        .map(|&id| match trusted(id) {
            Some(r) => r.clone(),
            None => fresh.next().flatten().unwrap_or_else(|| {
                // Not greyed out (nothing says it fails), but not cached as working either.
                untested = true;
                ProbeResult { encoder_id: id.to_owned(), problem: None, max_bframes: None }
            }),
        })
        .collect();
    for r in &results {
        if let Some(max) = r.max_bframes {
            limit_bframes(&r.encoder_id, max);
        }
    }
    if untested || valid.is_some_and(|c| c.results == results && c.pid == std::process::id()) {
        return results;
    }
    let cache = Cache { key: key.to_owned(), results, pid: std::process::id() };
    // A cache that can't be written only costs a re-probe next start; write-then-rename keeps a
    // crash from leaving half a file.
    let tmp = path.with_extension("tmp");
    let _ = path.parent().map(fs::create_dir_all);
    if fs::write(&tmp, serde_json::to_string(&cache).expect("plain data")).is_ok() {
        let _ = fs::rename(&tmp, path);
    }
    cache.results
}

/// Tests one catalog encoder (the probe process calls this for each id it's given).
pub fn probe_encoder(id: &str) -> ProbeResult {
    match Catalog::get().video_encoder(id) {
        Some(e) => probe_one(e),
        None => ProbeResult {
            encoder_id: id.to_owned(),
            problem: Some(Problem {
                reason: CPU_ENCODER_BROKEN.into(),
                detail: "not in the catalog".into(),
                stuck: false,
            }),
            max_bframes: None,
        },
    }
}

// shortcut: tries each encoder's default configuration only; probe the exact settings (bit depth,
// B-frames) when the UI lets users pick 10-bit or B-frames on hardware that may lack them.
fn probe_one(e: &crate::catalog::VideoEncoder) -> ProbeResult {
    probe_with(e, |values| {
        // FFmpeg's warnings say more than its error code; keep them with the failure.
        let (result, log) = capture_log(|| encode_one_frame(e, values));
        result.map_err(|err| if log.is_empty() { err } else { format!("{err} [{}]", log.join(" | ")) })
    })
}

/// Tests `e` with `open` (open an encoder with these values, encode a frame). NVENC rejects what a
/// card lacks instead of clamping it (HEVC B-frames on Pascal), so a failure with B-frames on is
/// retried without them: if that works, the encoder works and `max_bframes` says what it can't do.
fn probe_with(e: &crate::catalog::VideoEncoder, open: impl Fn(&EncoderOptions) -> Result<(), String>) -> ProbeResult {
    let mut values = default_values(e);
    if e.family == "nvenc" {
        // Fresh installs use no B-frames; test with them anyway, so the card's limit is known
        // before someone turns them on in Advanced settings.
        values.insert("bframes".into(), toml::Value::Integer(2));
    }
    let (problem, max_bframes) = match open(&values) {
        Ok(()) => (None, None),
        Err(first) => match without_bframes(e, &values).map(|v| open(&v)) {
            // Without B-frames it opened: try them once more, so a passing failure (every NVENC
            // session busy for a moment) doesn't turn them off until the next driver update.
            Some(Ok(())) if open(&values).is_ok() => (None, None),
            Some(Ok(())) => {
                set_note(format!("works only without B-frames; with them: {first}"));
                (None, Some(0))
            }
            _ => {
                let reason = e.requires.clone().unwrap_or_else(|| CPU_ENCODER_BROKEN.into());
                (Some(Problem { reason, detail: first, stuck: false }), None)
            }
        },
    };
    ProbeResult { encoder_id: e.id.clone(), problem, max_bframes }
}

thread_local! {
    static NOTE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

fn set_note(note: String) {
    NOTE.with(|n| *n.borrow_mut() = Some(note));
}

/// What the last test on this thread found worth logging though the encoder works (why it's
/// limited), taken once.
pub fn take_note() -> Option<String> {
    NOTE.with(|n| n.borrow_mut().take())
}

fn encode_one_frame(encoder: &crate::catalog::VideoEncoder, values: &EncoderOptions) -> Result<(), String> {
    let mut s = VideoSession::open_exact(encoder, values, (256, 144), 30)?;
    for (i, plane) in s.planes_mut()?.into_iter().flatten().enumerate() {
        plane.0.fill(if i == 0 { 16 } else { 128 }); // limited-range black
    }
    let mut packets = 0;
    s.send(0, &mut |_| packets += 1)?;
    s.finish(&mut |_| packets += 1)?;
    if packets == 0 { Err("opened but produced no packet".into()) } else { Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_encoders_work_and_failures_explain_themselves() {
        crate::ffmpeg::errors_only_logging();
        let results: Vec<_> = Catalog::get().video_encoders.iter().map(probe_one).collect();
        assert_eq!(results.len(), Catalog::get().video_encoders.len());
        for r in &results {
            let enc = Catalog::get().video_encoder(&r.encoder_id).unwrap();
            if !enc.hardware {
                assert!(r.problem.is_none(), "{} failed: {:?}", r.encoder_id, r.problem);
            }
            if let Some(p) = &r.problem {
                assert!(!p.reason.is_empty() && !p.detail.is_empty(), "{}", r.encoder_id);
            }
        }
        // Shows what this machine supports when run with --nocapture.
        for r in &results {
            println!("{:<12} {}", r.encoder_id, r.problem.as_ref().map_or("ok", |p| p.reason.as_str()));
        }
    }

    #[test]
    fn cache_keeps_successes_and_retries_failures() {
        let path = std::env::temp_dir().join(format!("backsight-probe-{}.json", std::process::id()));
        let _ = fs::remove_file(&path);
        let ok = |id: &str| ProbeResult { encoder_id: id.into(), problem: None, max_bframes: None };
        let failed = |id: &str| ProbeResult {
            encoder_id: id.into(),
            problem: Some(Problem { reason: "needs a GPU".into(), detail: "busy".into(), stuck: false }),
            max_bframes: None,
        };
        // What each check asked the probe to test. `nv` fails unless `nv`; `None` can't be tested.
        let mut asked: Vec<Vec<String>> = Vec::new();
        let mut run = |key: &str, ids: &[&str], nv: Option<bool>| {
            cached(&path, key, ids, |todo| {
                asked.push(todo.iter().map(|s| s.to_string()).collect());
                todo.iter()
                    .map(|&id| match (id, nv) {
                        ("nv", None) => None,
                        ("nv", Some(false)) => Some(failed(id)),
                        _ => Some(ok(id)),
                    })
                    .collect()
            })
        };
        assert_eq!(run("k1", &["x264", "nv"], Some(false)), [ok("x264"), failed("nv")], "first start probes all");
        assert_eq!(run("k1", &["x264", "nv"], Some(true)), [ok("x264"), ok("nv")], "NVENC was only busy");
        assert_eq!(run("k1", &["x264", "nv"], Some(false)), [ok("x264"), ok("nv")], "a success is kept");
        assert_eq!(run("k2", &["x264", "nv"], Some(false)), [ok("x264"), failed("nv")], "driver update");
        run("k2", &["x264", "nv", "av1"], Some(false)); // the catalog gained an encoder
        fs::write(&path, "{ not json").unwrap();
        run("k2", &["x264", "nv", "av1"], Some(false));
        assert_eq!(run("k3", &["x264", "nv"], None), [ok("x264"), ok("nv")], "untested isn't greyed out");
        run("k3", &["x264", "nv"], Some(true)); // ...nor kept as working
        drop(run);
        let asked: Vec<String> = asked.iter().map(|a| a.join(" ")).collect();
        let expected = ["x264 nv", "nv", "", "x264 nv", "x264 nv av1", "x264 nv av1", "x264 nv", "x264 nv"];
        assert_eq!(asked, expected);
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn cache_keeps_frozen_encoders_and_what_they_learned() {
        let path = std::env::temp_dir().join(format!("backsight-probe-stuck-{}.json", std::process::id()));
        let _ = fs::remove_file(&path);
        let froze = ProbeResult {
            encoder_id: "nv".into(),
            problem: Some(Problem { reason: "froze".into(), detail: "no answer".into(), stuck: true }),
            max_bframes: None,
        };
        let no_b = ProbeResult { encoder_id: "hevc".into(), problem: None, max_bframes: Some(0) };
        let answers = vec![Some(froze.clone()), Some(no_b.clone())];
        let ids = ["nv", "hevc"];
        assert_eq!(cached(&path, "k1", &ids, |_| answers.clone()), [froze.clone(), no_b.clone()]);
        // Both survive the cache file, so the next check tests nothing.
        assert_eq!(
            cached(&path, "k1", &ids, |todo| {
                assert!(todo.is_empty(), "tested {todo:?}");
                vec![]
            }),
            [froze.clone(), no_b.clone()]
        );
        // The next run of Backsight tests the frozen one again: one slow test isn't for good.
        let other_run =
            fs::read_to_string(&path).unwrap().replace(&format!("\"pid\":{}", std::process::id()), "\"pid\":1");
        fs::write(&path, other_run).unwrap();
        cached(&path, "k1", &ids, |todo| {
            assert_eq!(todo, ["nv"], "only the frozen one");
            vec![Some(froze.clone())]
        });
        // Still frozen: now this run's, so it isn't tested again in this run.
        cached(&path, "k1", &ids, |todo| {
            assert!(todo.is_empty(), "tested {todo:?}");
            vec![]
        });
        // A new driver tests them again.
        cached(&path, "k2", &ids, |todo| {
            assert_eq!(todo, ids, "a new driver tests everything");
            vec![None; todo.len()]
        });
        // An older cache, from before `max_bframes` existed, parses and is tested again (new key).
        let old = r#"{"key":"k1","results":[{"encoder_id":"hevc","problem":null}]}"#;
        assert!(serde_json::from_str::<Cache>(old).is_ok());
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn nvenc_that_refuses_bframes_is_retried_without_them() {
        let nv = |id| Catalog::get().video_encoder(id).unwrap().clone();
        let bf = |v: &EncoderOptions| v["bframes"].as_integer().unwrap();
        // A Pascal card: opens only without B-frames.
        let pascal =
            |v: &EncoderOptions| if bf(v) > 0 { Err("open failed: Generic error".to_string()) } else { Ok(()) };
        let hevc = probe_with(&nv("nvenc-hevc"), pascal);
        assert_eq!((hevc.problem, hevc.max_bframes), (None, Some(0)));
        assert!(take_note().is_some_and(|n| n.contains("Generic error")), "why, for the log");
        // A passing failure (every NVENC session busy for a moment) doesn't turn B-frames off.
        let calls = std::cell::Cell::new(0);
        let busy_once = |_: &EncoderOptions| {
            calls.set(calls.get() + 1);
            if calls.get() == 1 { Err("busy".to_string()) } else { Ok(()) }
        };
        let hevc = probe_with(&nv("nvenc-hevc"), busy_once);
        assert_eq!((hevc.problem, hevc.max_bframes, take_note()), (None, None, None));
        // A newer card needs no retry.
        let opened = std::cell::RefCell::new(Vec::new());
        let turing = |v: &EncoderOptions| {
            opened.borrow_mut().push(bf(v));
            Ok(())
        };
        let hevc = probe_with(&nv("nvenc-hevc"), turing);
        assert_eq!((hevc.problem, hevc.max_bframes, opened.take()), (None, None, vec![2]));
        // Broken for other reasons: still a problem, with the first error.
        let broken = |_: &EncoderOptions| Err("no device".to_string());
        let hevc = probe_with(&nv("nvenc-hevc"), broken);
        assert_eq!(hevc.problem.map(|p| p.detail), Some("no device".into()));
        assert_eq!(hevc.max_bframes, None);
        // Not NVENC: no retry, since B-frames aren't what it refused.
        let calls = std::cell::Cell::new(0);
        let x264 = probe_with(&nv("x264"), |_| {
            calls.set(calls.get() + 1);
            Err("no".into())
        });
        assert!(x264.problem.is_some() && calls.get() == 1);
    }
}
