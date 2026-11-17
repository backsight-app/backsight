//! First-run setup: which graphics cards this PC has and which encoders work on it.
//!
//! Encoders are tested in a separate process (`backsight.exe --probe-encoders <ids>`), so a
//! graphics driver that never returns can't freeze Backsight: the process writes one JSON line per
//! encoder as it finishes, and one that takes too long is killed.

use crate::capture::d3d;
use backsight_encode::{
    catalog::Catalog,
    probe::{self, ProbeResult, Problem},
};
use serde::Serialize;
use std::{
    io::{self, BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

/// The encoder used when the chosen one doesn't work here: NVENC H.264, else the CPU.
const BEST: &str = "nvenc-h264";
const FALLBACK: &str = "x264";

#[derive(Debug, Clone, Serialize)]
pub struct GraphicsCard {
    pub name: String,
    /// `nvidia`, `amd`, `intel` or `other`.
    pub vendor: &'static str,
}

pub fn graphics_cards() -> Vec<GraphicsCard> {
    let cards = d3d::graphics_cards().unwrap_or_else(|e| {
        tracing::warn!("couldn't list graphics cards: {e}");
        Vec::new()
    });
    let vendor =
        |id| ["nvidia", "amd", "intel"].into_iter().find(|v| d3d::pci_vendor(v) == Some(id)).unwrap_or("other");
    cards.into_iter().map(|(id, name)| GraphicsCard { name, vendor: vendor(id) }).collect()
}

/// How long one hardware encoder may take in the test process before it counts as stuck. Opening
/// NVENC can take 6 s on an Optimus laptop (the NVIDIA card wakes up first) and over 10 s on
/// the first test after starting from a slow drive, so 10 s called it stuck.
pub const PROBE_LIMIT: Duration = Duration::from_secs(30);
/// CPU encoders touch no driver, so they're slow rather than stuck: SVT-AV1 can take 15 s to test.
const CPU_PROBE_LIMIT: Duration = Duration::from_secs(60);
/// How long the test process may take to start. Starting touches no driver, but a virus scanner
/// checking a new copy of Backsight took 13 s once, so this doesn't count against an encoder.
const START_LIMIT: Duration = Duration::from_secs(30);
/// The test process's first line, once it's running.
const READY: &str = "ready";
/// Starts a line the test process sends for the log.
const NOTE: &str = "note ";

/// Which catalog encoders work here (seconds the first time; working ones are cached per graphics
/// driver after that). `spawn` starts a test process for the given ids.
pub fn check_encoders(spawn: impl FnMut(&[&str]) -> io::Result<ProbeRun>) -> Vec<ProbeResult> {
    let path = match crate::library::cache_dir() {
        Ok(dir) => dir.join("encoder-check.json"),
        Err(e) => {
            tracing::warn!("encoder check not cached: {e}");
            std::env::temp_dir().join("backsight-encoder-check.json")
        }
    };
    let drivers = d3d::drivers_key().unwrap_or_else(|e| {
        tracing::warn!("no graphics driver versions: {e}");
        String::new()
    });
    let catalog = Catalog::get();
    let cpu = |id: &str| catalog.video_encoder(id).is_some_and(|e| !e.hardware);
    let limit = |id: &str| if cpu(id) { CPU_PROBE_LIMIT } else { PROBE_LIMIT };
    probe::probe_video_encoders_cached(&path, &drivers, |ids| probe_in_children(ids, limit, spawn))
}

/// A running test process: its answer lines, and the process, killed when this drops.
pub struct ProbeRun {
    lines: Receiver<String>,
    child: Option<Child>,
}

impl ProbeRun {
    /// Starts `command` with its output read line by line on a helper thread.
    pub fn start(mut command: Command) -> io::Result<Self> {
        let mut child = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?;
        crate::job::kill_on_exit(&child);
        let out = child.stdout.take().expect("stdout is piped");
        let (tx, lines) = mpsc::channel();
        // Ends with the process. One stuck in a driver call may never end; it keeps only this idle thread.
        thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Self { lines, child: Some(child) })
    }
}

impl Drop for ProbeRun {
    fn drop(&mut self) {
        // Not waited for: a process stuck in the driver may take its time to go.
        if let Some(child) = &mut self.child {
            let _ = child.kill();
        }
    }
}

/// Tests `ids` in test processes, answers in the same order. An encoder that takes longer than
/// its `limit`, or ends its process without an answer, doesn't work here; the ones after it go to
/// a new process. `None` for encoders no process could be started for.
pub fn probe_in_children(
    ids: &[&str],
    limit: impl Fn(&str) -> Duration,
    mut spawn: impl FnMut(&[&str]) -> io::Result<ProbeRun>,
) -> Vec<Option<ProbeResult>> {
    let mut out = Vec::with_capacity(ids.len());
    while out.len() < ids.len() {
        let rest = &ids[out.len()..];
        let started = spawn(rest).and_then(|run| match run.lines.recv_timeout(START_LIMIT) {
            Ok(line) if line == READY => Ok(run),
            other => Err(io::Error::other(format!("no ready line: {other:?}"))),
        });
        let run = match started {
            Ok(run) => run,
            Err(e) => {
                tracing::warn!("encoder check: couldn't start the test, {} encoders untested: {e}", rest.len());
                break;
            }
        };
        for &id in rest {
            tracing::info!("encoder check: testing {id}");
            let started = Instant::now();
            match answer(&run.lines, id, started + limit(id)) {
                Ok(r) => {
                    let ms = started.elapsed().as_millis();
                    match &r.problem {
                        None => tracing::info!("encoder check: {id} works ({ms} ms)"),
                        Some(p) => tracing::info!("encoder check: {id} doesn't work here ({ms} ms): {}", p.detail),
                    }
                    out.push(Some(r));
                }
                Err(e) => {
                    let r = gave_up(id, e, limit(id));
                    tracing::warn!("encoder check: {id} {}", r.problem.as_ref().expect("a problem").detail);
                    out.push(Some(r));
                    break; // `run` drops: the process is killed and the rest get a new one
                }
            }
        }
    }
    out.resize(ids.len(), None);
    out
}

/// The answer about `id`, skipping anything else the process prints.
fn answer(lines: &Receiver<String>, id: &str, deadline: Instant) -> Result<ProbeResult, RecvTimeoutError> {
    loop {
        let line = lines.recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
        match serde_json::from_str::<ProbeResult>(&line) {
            Ok(r) if r.encoder_id == id => return Ok(r),
            _ if line.starts_with(NOTE) => tracing::info!("encoder check: {}", &line[NOTE.len()..]),
            _ => tracing::debug!("encoder check: ignored {line:?}"),
        }
    }
}

/// `id` didn't answer: it got stuck (`Timeout`) or took the process down (`Disconnected`).
fn gave_up(id: &str, why: RecvTimeoutError, limit: Duration) -> ProbeResult {
    let (reason, detail) = match why {
        RecvTimeoutError::Timeout => (
            "This encoder froze in Backsight's test, so it's switched off. Updating your graphics driver may fix it.",
            format!("stopped responding: no answer in {} s, test stopped", limit.as_secs_f32()),
        ),
        RecvTimeoutError::Disconnected => (
            "This encoder crashed Backsight's test, so it's switched off. Updating your graphics driver may fix it.",
            "the test process ended without an answer".to_owned(),
        ),
    };
    ProbeResult {
        encoder_id: id.to_owned(),
        problem: Some(Problem { reason: reason.into(), detail, stuck: true }),
        max_bframes: None,
    }
}

/// The test process's work (`backsight.exe --probe-encoders <ids>`): a ready line, then one JSON
/// line per encoder, in order, as each finishes. It hangs on the encoders in `stuck`
/// (comma-separated) instead, so tests can fake a driver that never returns.
pub fn probe_child(ids: &[String], stuck: &str, mut out: impl Write) -> io::Result<()> {
    writeln!(out, "{READY}")?;
    out.flush()?;
    for id in ids {
        if stuck.split(',').any(|s| s == id) {
            loop {
                thread::park();
            }
        }
        let result = probe::probe_encoder(id);
        if let Some(note) = probe::take_note() {
            writeln!(out, "{NOTE}{id}: {note}")?; // before the answer, so it's read
        }
        let line = serde_json::to_string(&result).expect("plain data");
        writeln!(out, "{line}")?;
        out.flush()?;
    }
    Ok(())
}

/// A better encoder than the chosen one, which failed the check.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pick {
    pub encoder: &'static str,
    /// The chosen one can't work at all (no graphics card from its maker): switch without asking.
    /// Otherwise the failure may be passing (other apps holding every NVENC session), so the
    /// switch is only offered.
    pub automatic: bool,
}

/// What to do when `current` failed the check; `None` keeps it. A working choice is never
/// overridden. The shell marks the encoder of a healthy running session as working first.
pub fn pick_encoder(current: &str, results: &[ProbeResult], cards: &[GraphicsCard]) -> Option<Pick> {
    let works = |id: &str| results.iter().any(|r| r.encoder_id == id && r.problem.is_none());
    if works(current) {
        return None;
    }
    let encoder = if works(BEST) { BEST } else { FALLBACK };
    let vendor = Catalog::get().video_encoder(current).map(|e| e.vendor.as_str());
    let automatic = vendor.is_none_or(|v| v != "cpu" && !cards.iter().any(|c| c.vendor == v));
    (encoder != current).then_some(Pick { encoder, automatic })
}

#[cfg(test)]
mod tests {
    use super::*;
    use backsight_encode::probe::Problem;

    fn result(id: &str, ok: bool) -> ProbeResult {
        let problem = (!ok).then(|| Problem { reason: "needs a card".into(), detail: "failed".into(), stuck: false });
        ProbeResult { encoder_id: id.into(), problem, max_bframes: None }
    }

    fn card(vendor: &'static str) -> GraphicsCard {
        GraphicsCard { name: vendor.into(), vendor }
    }

    #[test]
    fn keeps_a_working_choice_and_replaces_a_broken_one() {
        let rtx = [card("nvidia"), card("intel")];
        let nvidia = [result("nvenc-h264", true), result("nvenc-av1", false), result("x264", true)];
        assert_eq!(pick_encoder("nvenc-h264", &nvidia, &rtx), None, "fresh install on NVIDIA");
        assert_eq!(pick_encoder("x265", &[result("x265", true)], &rtx), None, "the user's own choice");
        let offered = Pick { encoder: "nvenc-h264", automatic: false };
        assert_eq!(pick_encoder("nvenc-av1", &nvidia, &rtx), Some(offered), "an RTX 30 card has no AV1 encoder");
        let no_nvidia = [result("nvenc-h264", false), result("qsv-h264", true), result("x264", true)];
        let switched = Pick { encoder: "x264", automatic: true };
        assert_eq!(pick_encoder("nvenc-h264", &no_nvidia, &[card("intel")]), Some(switched), "no NVIDIA card");
    }

    #[test]
    fn a_busy_nvidia_card_is_only_offered_a_switch() {
        // Every NVENC session taken (Discord, OBS): the card is there, so the failure may pass.
        let busy = [result("nvenc-h264", false), result("x264", true)];
        let pick = pick_encoder("nvenc-h264", &busy, &[card("nvidia")]).unwrap();
        assert_eq!(pick, Pick { encoder: "x264", automatic: false });
    }

    fn ok(id: &str) -> String {
        serde_json::to_string(&result(id, true)).unwrap()
    }

    /// A fake test process: answers `script` lines in order; `"stuck"` hangs, `"crash"` ends it.
    fn fake(script: Vec<String>) -> io::Result<ProbeRun> {
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in script {
                match line.as_str() {
                    "stuck" => loop {
                        thread::park();
                    },
                    "crash" => return,
                    _ => tx.send(line).unwrap(),
                }
            }
        });
        Ok(ProbeRun { lines, child: None })
    }

    #[test]
    fn a_stuck_or_crashing_encoder_fails_and_the_rest_get_a_new_process() {
        let ids = ["a", "stuck", "b", "crash", "c"];
        let mut spawned = Vec::new();
        let results = probe_in_children(
            &ids,
            |_| Duration::from_millis(100),
            |rest| {
                spawned.push(rest.join(" "));
                let mut script = vec![READY.into(), "noise".into(), ok("other")];
                script.extend(rest.iter().map(|&id| if id == "stuck" || id == "crash" { id.into() } else { ok(id) }));
                fake(script)
            },
        );
        assert_eq!(spawned, ["a stuck b crash c", "b crash c", "c"]);
        let worked: Vec<_> = results.iter().map(|r| r.as_ref().unwrap().problem.is_none()).collect();
        assert_eq!(worked, [true, false, true, false, true]);
        let reason = |i: usize| results[i].as_ref().unwrap().problem.as_ref().unwrap().reason.clone();
        assert!(reason(1).contains("froze") && reason(3).contains("crashed"));
    }

    #[test]
    fn encoders_without_a_process_are_untested() {
        let mut calls = 0;
        let results = probe_in_children(
            &["a", "stuck", "b"],
            |_| Duration::from_millis(50),
            |rest| {
                calls += 1;
                match calls {
                    1 => fake(
                        [READY.into()]
                            .into_iter()
                            .chain(rest.iter().map(|&id| if id == "stuck" { id.into() } else { ok(id) }))
                            .collect(),
                    ),
                    2 => fake(vec!["crash".into()]), // ends before it's ready
                    _ => Err(io::Error::other("blocked")),
                }
            },
        );
        assert_eq!(calls, 2);
        assert!(results[0].as_ref().unwrap().problem.is_none());
        assert!(results[1].as_ref().unwrap().problem.is_some());
        assert_eq!(results[2], None);
        assert!(probe_in_children(&[], |_| Duration::ZERO, |_| unreachable!()).is_empty());
    }

    #[test]
    fn the_test_process_answers_line_by_line() {
        let mut out = Vec::new();
        probe_child(&["x264".into(), "nope".into()], "nvenc-hevc", &mut out).unwrap();
        let out = String::from_utf8(out).unwrap();
        let (ready, rest) = out.split_once('\n').unwrap();
        assert_eq!(ready, READY);
        let lines: Vec<ProbeResult> = rest.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].problem.is_none(), "{:?}", lines[0]);
        assert!(lines[1].problem.is_some());
    }

    /// Shows this PC's cards with --nocapture.
    #[test]
    fn lists_this_pcs_graphics_cards() {
        println!("{:?}", graphics_cards());
    }
}
