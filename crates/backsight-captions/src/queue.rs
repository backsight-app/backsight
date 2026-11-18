//! The captions queue: one worker thread runs one captions process at a time, in the order the
//! clips were saved. Stopping (on quit) kills the running process and drops the rest; a killed
//! job leaves no captions file, only its clip.

use crate::{Job, Outcome};
use std::{
    io::{self, Read},
    process::Child,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering::Relaxed},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

/// A job still running after this is stuck; it is killed. A 2-minute clip of talking takes
/// about a minute and a half in background mode on a busy laptop CPU.
const LIMIT: Duration = Duration::from_secs(15 * 60);

pub struct Queue {
    jobs: Mutex<Option<mpsc::Sender<Job>>>,
    stop: Arc<AtomicBool>,
}

impl Queue {
    /// Starts the worker. `spawn` starts the captions process for a job (see [`crate::command`])
    /// with stdout piped; `done` hears how each job ended, on the worker thread.
    pub fn start(
        spawn: impl Fn(&Job) -> io::Result<Child> + Send + 'static,
        done: impl Fn(&Job, Outcome) + Send + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<Job>();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = thread::Builder::new().name("captions".into()).spawn(move || {
            for job in rx {
                if stopped.load(Relaxed) {
                    break;
                }
                let outcome = run_child(&spawn, &job, &stopped, LIMIT);
                done(&job, outcome);
            }
        });
        if let Err(e) = worker {
            tracing::warn!("captions: no worker thread, captions are off: {e}");
        }
        Queue { jobs: Mutex::new(Some(tx)), stop }
    }

    /// Adds a clip; ignored after [`Queue::stop`] (or without a worker), then `false`.
    pub fn push(&self, job: Job) -> bool {
        self.jobs.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|tx| tx.send(job).is_ok())
    }

    /// Kills the running captions process and forgets the clips still waiting.
    pub fn stop(&self) {
        self.stop.store(true, Relaxed);
        self.jobs.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}

/// Runs one job's process to its end, or kills it on `stop` or after `limit`.
fn run_child(spawn: &impl Fn(&Job) -> io::Result<Child>, job: &Job, stop: &AtomicBool, limit: Duration) -> Outcome {
    const CANT_START: &str = "Backsight couldn't start making captions. Restart Backsight and try again.";
    let mut child = match spawn(job) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("captions: can't start the process: {e}");
            return Outcome::Failed(CANT_START.into());
        }
    };
    let started = Instant::now();
    // The answer is one short line (plus a few log lines), far below a pipe's buffer, so the
    // process never waits on us while we poll.
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if stop.load(Relaxed) || started.elapsed() > limit => {
                let _ = child.kill();
                let _ = child.wait();
                let why = if stop.load(Relaxed) { "stopped" } else { "took too long, stopped" };
                tracing::warn!("captions for {}: {why}", job.clip.display());
                return Outcome::Failed("Making captions took too long and was stopped.".into());
            }
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(e) => {
                tracing::warn!("captions: can't wait for the process: {e}");
                return Outcome::Failed(CANT_START.into());
            }
        }
    }
    let mut output = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut output);
    }
    let outcome = Outcome::parse(&output, &job.clip);
    let lines: Vec<&str> = output.lines().collect();
    for line in lines.iter().take(lines.len().saturating_sub(1)) {
        tracing::info!("captions process: {line}");
    }
    tracing::info!("captions for {} in {:.1?}: {outcome:?}", job.clip.display(), started.elapsed());
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    fn job(name: &str) -> Job {
        Job { clip: format!(r"C:\v\{name}.mp4").into(), track: None, language: "auto".into() }
    }

    /// A process that prints a log line and `answer`, or with `None` runs until killed (a loop
    /// inside cmd itself, so killing cmd ends it; `ping` as a timer ended early on some runs).
    fn fake(answer: Option<&str>) -> io::Result<Child> {
        let script = answer.map_or("for /l %i in (0,0,1) do @rem".into(), |a| format!("echo log line & echo {a}"));
        Command::new("cmd").args(["/c", &script]).stdin(Stdio::null()).stdout(Stdio::piped()).spawn()
    }

    #[test]
    fn jobs_run_one_at_a_time_in_order() {
        let (tx, rx) = mpsc::channel();
        let q = Queue::start(
            |j: &Job| fake(Some(if j.clip.ends_with("b.mp4") { "error: Try again." } else { "ok" })),
            move |j: &Job, o| tx.send((j.clip.clone(), o)).unwrap(),
        );
        for name in ["a", "b", "c"] {
            q.push(job(name));
        }
        let got: Vec<_> = (0..3).map(|_| rx.recv_timeout(Duration::from_secs(20)).unwrap()).collect();
        assert_eq!(got[0], (job("a").clip, Outcome::Written(r"C:\v\a.srt".into())));
        assert_eq!(got[1], (job("b").clip, Outcome::Failed("Try again.".into())));
        assert_eq!(got[2].0, job("c").clip);
    }

    #[test]
    fn stop_kills_the_running_process_and_drops_the_rest() {
        let (tx, rx) = mpsc::channel();
        let q = Queue::start(|_: &Job| fake(None), move |j: &Job, o| tx.send((j.clip.clone(), o)).unwrap());
        q.push(job("long"));
        q.push(job("never"));
        thread::sleep(Duration::from_millis(500));
        let asked = Instant::now();
        q.stop();
        let (clip, outcome) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(asked.elapsed() < Duration::from_secs(2), "killed, not waited for");
        assert_eq!(clip, job("long").clip);
        assert!(matches!(outcome, Outcome::Failed(_)));
        assert!(rx.recv_timeout(Duration::from_secs(1)).is_err(), "the waiting job never ran");
        q.push(job("after"));
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    }

    #[test]
    fn a_stuck_process_is_killed_after_the_limit_and_a_failed_start_is_plain() {
        let no = AtomicBool::new(false);
        let started = Instant::now();
        let o = run_child(&|_: &Job| fake(None), &job("stuck"), &no, Duration::from_millis(300));
        assert!(matches!(o, Outcome::Failed(_)) && started.elapsed() < Duration::from_secs(5));
        let o = run_child(&|_: &Job| Err(io::Error::other("nope")), &job("x"), &no, LIMIT);
        assert!(matches!(o, Outcome::Failed(ref why) if why.starts_with("Backsight couldn't start")));
    }
}
