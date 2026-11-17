//! Notices an engine step that got stuck: starting or stopping capture, saving a clip, finishing a
//! recording, converting one to MP4, recovering one. The step holds a [`Watch`] while it runs; one
//! still running past its limit is logged and gets one "got stuck" report in the logs folder
//! (`stuck-*.txt`) with the stuck thread's stack and the last log lines. Nothing is stopped.
//!
//! Costs nothing per frame: a step registers once, and one thread looks at the running steps
//! once a second.
//!
//! `BACKSIGHT_TEST_STUCK_STEP=save` makes that step hang for 5 s against a 2 s limit, so the report
//! can be tested. Steps: `capture`, `stop`, `save`, `recording`, `convert`, `recover`
//! (comma-separated for several).

use crate::{crash, stack::Watched};
use std::{
    marker::PhantomData,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak},
    time::{Duration, Instant},
};

/// How often the running steps are looked at.
const TICK: Duration = Duration::from_secs(1);
/// A look this much later than the one before means the PC slept (or the watchdog was held up):
/// that gap doesn't count against any step.
const LATE: Duration = Duration::from_secs(5);
/// A slow USB hard drive writes about 20 MB/s; a step that writes gets 1 s per 5 MB on top.
const SLOW_DISK_BYTES_PER_SEC: u64 = 5_000_000;
/// "Got stuck" reports kept in the logs folder.
const KEEP_REPORTS: usize = 5;
const REPORT_PREFIX: &str = "stuck-";
/// The test hook's limit and hang.
const TEST_LIMIT: Duration = Duration::from_secs(2);
const TEST_HANG: Duration = Duration::from_secs(5);

/// A long-running engine step.
#[derive(Clone, Copy, Debug)]
pub enum Step {
    CaptureStart,
    CaptureStop,
    /// `bytes`: what the step writes (or reads and writes), for its limit.
    SaveClip {
        bytes: u64,
    },
    FinishRecording {
        bytes: u64,
    },
    Convert {
        bytes: u64,
    },
    Recover {
        bytes: u64,
    },
}

impl Step {
    /// The name `BACKSIGHT_TEST_STUCK_STEP` takes.
    pub fn key(self) -> &'static str {
        match self {
            Self::CaptureStart => "capture",
            Self::CaptureStop => "stop",
            Self::SaveClip { .. } => "save",
            Self::FinishRecording { .. } => "recording",
            Self::Convert { .. } => "convert",
            Self::Recover { .. } => "recover",
        }
    }

    /// What the step does, in the report and the log.
    pub fn name(self) -> &'static str {
        match self {
            Self::CaptureStart => "starting capture",
            Self::CaptureStop => "stopping capture",
            Self::SaveClip { .. } => "saving a clip",
            Self::FinishRecording { .. } => "finishing a recording",
            Self::Convert { .. } => "converting a recording to MP4",
            Self::Recover { .. } => "recovering an interrupted recording",
        }
    }

    /// How long the step may run before it counts as stuck: generous, so a slow disk never trips it.
    pub fn limit(self) -> Duration {
        let (base, bytes) = match self {
            Self::CaptureStart | Self::CaptureStop => (30, 0),
            Self::SaveClip { bytes } => (60, bytes),
            Self::FinishRecording { bytes } | Self::Recover { bytes } => (120, bytes),
            Self::Convert { bytes } => (600, bytes),
        };
        Duration::from_secs(base + bytes / SLOW_DISK_BYTES_PER_SEC)
    }
}

/// Held while a step runs, on the thread doing it (so it can't be sent to another thread: the
/// report reads this thread's stack). Dropping it ends the step.
pub struct Watch {
    dog: Arc<Watchdog>,
    id: u64,
    _same_thread: PhantomData<*const ()>,
}

impl Watch {
    /// Starts watching `step` on the calling thread.
    pub fn start(step: Step) -> Self {
        let test = test_steps().split(',').any(|s| s.trim().eq_ignore_ascii_case(step.key()));
        let watch = global().watch(step.key(), step.name(), if test { TEST_LIMIT } else { step.limit() });
        if test {
            tracing::warn!("test (BACKSIGHT_TEST_STUCK_STEP): {} hangs for {} s", step.name(), TEST_HANG.as_secs());
            std::thread::sleep(TEST_HANG);
        }
        watch
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let ended = lock(&self.dog.board).remove(self.id);
        if let Some(r) = ended.filter(|r| r.reported) {
            tracing::info!("{} finished after all, after {} s", r.name, r.started.elapsed().as_secs());
        }
    }
}

fn test_steps() -> &'static str {
    static STEPS: OnceLock<String> = OnceLock::new();
    STEPS.get_or_init(|| std::env::var("BACKSIGHT_TEST_STUCK_STEP").unwrap_or_default())
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A step that is running.
struct Running {
    id: u64,
    key: &'static str,
    name: &'static str,
    limit: Duration,
    thread: Watched,
    started: Instant,
    /// Time run so far, sleep left out.
    counted: Duration,
    /// When `counted` was last brought up to date.
    last: Instant,
    reported: bool,
}

/// A step past its limit, to report.
struct Stuck {
    id: u64,
    key: &'static str,
    name: &'static str,
    ran: Duration,
    limit: Duration,
    thread: Watched,
}

/// The running steps' deadlines, kept apart from the threads so they can be tested.
#[derive(Default)]
struct Board {
    running: Vec<Running>,
    next_id: u64,
}

impl Board {
    fn add(&mut self, key: &'static str, name: &'static str, limit: Duration, thread: Watched, now: Instant) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.running.push(Running {
            id,
            key,
            name,
            limit,
            thread,
            started: now,
            counted: Duration::ZERO,
            last: now,
            reported: false,
        });
        id
    }

    fn remove(&mut self, id: u64) -> Option<Running> {
        let i = self.running.iter().position(|r| r.id == id)?;
        Some(self.running.swap_remove(i))
    }

    /// Steps that went past their limit since the last look; each step is returned once.
    fn tick(&mut self, now: Instant) -> Vec<Stuck> {
        let mut out = Vec::new();
        for r in &mut self.running {
            let gap = now.saturating_duration_since(r.last);
            if gap < LATE {
                r.counted += gap;
            }
            r.last = now;
            if !r.reported && r.counted >= r.limit {
                r.reported = true;
                out.push(Stuck {
                    id: r.id,
                    key: r.key,
                    name: r.name,
                    ran: r.counted,
                    limit: r.limit,
                    thread: r.thread,
                });
            }
        }
        out
    }
}

struct Watchdog {
    board: Mutex<Board>,
    /// Where reports go; `None` when there is no profile folder.
    dir: Option<PathBuf>,
}

/// The app's watchdog, started by the first step.
fn global() -> &'static Arc<Watchdog> {
    static DOG: OnceLock<Arc<Watchdog>> = OnceLock::new();
    DOG.get_or_init(|| Watchdog::spawn(TICK, backsight_config::app_dir().ok().map(|d| d.join("logs"))))
}

impl Watchdog {
    /// Starts its thread, which ends when the watchdog is dropped.
    fn spawn(tick: Duration, dir: Option<PathBuf>) -> Arc<Self> {
        let dog = Arc::new(Self { board: Mutex::default(), dir });
        let weak = Arc::downgrade(&dog);
        let started = std::thread::Builder::new().name("backsight-step-watch".into()).spawn(move || run(&weak, tick));
        if let Err(e) = started {
            tracing::warn!("the stuck-step watchdog didn't start: {e}");
        }
        dog
    }

    fn watch(self: &Arc<Self>, key: &'static str, name: &'static str, limit: Duration) -> Watch {
        let id = lock(&self.board).add(key, name, limit, Watched::this_thread(), Instant::now());
        Watch { dog: self.clone(), id, _same_thread: PhantomData }
    }
}

fn run(dog: &Weak<Watchdog>, tick: Duration) {
    loop {
        std::thread::sleep(tick);
        let Some(dog) = dog.upgrade() else { return };
        let stuck = lock(&dog.board).tick(Instant::now());
        for s in &stuck {
            report(&dog, s);
        }
    }
}

/// Logs the stuck step and writes `stuck-<time>-<step>.txt`: scrubbed, through a `.part` file.
fn report(dog: &Watchdog, s: &Stuck) {
    // Logged first: if reading the stack ever hangs, the log still says what happened.
    tracing::error!(
        "{} got stuck: still running after {} s (limit {} s); saving a report",
        s.name,
        s.ran.as_secs(),
        s.limit.as_secs()
    );
    let stack = match s.thread.capture() {
        Ok(frames) => crate::stack::describe(&frames),
        Err(e) => format!("not read: {e}\n"),
    };
    let Some(dir) = &dog.dir else { return };
    let others: String = {
        let board = lock(&dog.board);
        let now = Instant::now();
        let mut lines: Vec<String> = board
            .running
            .iter()
            .filter(|r| r.id != s.id)
            .map(|r| format!("{}: running for {} s\n", r.name, now.saturating_duration_since(r.started).as_secs()))
            .collect();
        lines.sort();
        lines.concat()
    };
    let others = if others.is_empty() { "none\n".to_owned() } else { others };
    let log = crash::last_lines(&crash::read_tail(&dir.join("backsight.log"), 64 * 1024), 100);
    let time = crash::local_time();
    let text = format!(
        "Backsight: a step got stuck\nThis file stays on this PC unless you send it. Personal details were removed.\n\n\
         time: {time}\nBacksight: {}\nstep: {}\nrunning for: {} s (limit {} s)\n\n\
         stuck thread's stack (decode with this build's backsight.pdb):\n{stack}\nother steps running:\n{others}\n\
         last log lines:\n{log}\n",
        crash::installed_version().unwrap_or("unknown"),
        s.name,
        s.ran.as_secs(),
        s.limit.as_secs(),
    );
    let name = format!("{REPORT_PREFIX}{}-{}.txt", time.replace([' ', ':'], "-"), s.key);
    match save_report(dir, REPORT_PREFIX, &name, &crash::scrub_installed(&text), KEEP_REPORTS) {
        Ok(()) => tracing::error!("stuck report saved: {name}"),
        Err(e) => tracing::error!("the stuck report couldn't be saved: {e}"),
    }
}

/// Writes the report `name` in `dir` through a `.part` file, then keeps the newest `keep` files
/// whose names start with `prefix` (the names sort by time). Shared with the window's freeze reports.
pub fn save_report(dir: &Path, prefix: &str, name: &str, text: &str, keep: usize) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let part = dir.join(format!("{name}.part"));
    std::fs::write(&part, text).inspect_err(|_| {
        let _ = std::fs::remove_file(&part);
    })?;
    std::fs::rename(&part, dir.join(name))?;
    let mut old: Vec<_> = std::fs::read_dir(dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(prefix)))
        .collect();
    old.sort(); // `.part` leftovers sort next to their report
    for p in &old[..old.len().saturating_sub(keep)] {
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEC: Duration = Duration::from_secs(1);

    fn board_with(limit: Duration, t0: Instant) -> (Board, u64) {
        let mut b = Board::default();
        let id = b.add("save", "saving a clip", limit, Watched::this_thread(), t0);
        (b, id)
    }

    #[test]
    fn a_step_past_its_limit_is_reported_once() {
        let t0 = Instant::now();
        let (mut b, _) = board_with(SEC * 3, t0);
        assert!(b.tick(t0 + SEC).is_empty());
        assert!(b.tick(t0 + SEC * 2).is_empty());
        let stuck = b.tick(t0 + SEC * 3);
        assert_eq!(stuck.len(), 1);
        assert_eq!((stuck[0].name, stuck[0].ran), ("saving a clip", SEC * 3));
        for i in 4..20 {
            assert!(b.tick(t0 + SEC * i).is_empty(), "one report per stuck step, tick {i}");
        }
    }

    #[test]
    fn a_step_that_ends_in_time_is_not_reported() {
        let t0 = Instant::now();
        let (mut b, id) = board_with(SEC * 3, t0);
        assert!(b.tick(t0 + SEC * 2).is_empty());
        assert!(b.remove(id).is_some_and(|r| !r.reported));
        assert!(b.tick(t0 + SEC * 10).is_empty());
        assert!(b.remove(id).is_none());
    }

    #[test]
    fn a_pc_that_slept_does_not_count() {
        let t0 = Instant::now();
        let (mut b, _) = board_with(SEC * 3, t0);
        assert!(b.tick(t0 + SEC).is_empty());
        let woke = t0 + Duration::from_secs(3600);
        assert!(b.tick(woke).is_empty(), "the hour asleep isn't counted");
        assert!(b.tick(woke + SEC).is_empty());
        assert_eq!(b.tick(woke + SEC * 2).len(), 1, "1 s before the sleep and 2 s after");
    }

    #[test]
    fn each_of_two_stuck_steps_is_reported() {
        let t0 = Instant::now();
        let (mut b, _) = board_with(SEC * 2, t0);
        b.add("convert", "converting a recording to MP4", SEC * 4, Watched::this_thread(), t0 + SEC);
        assert_eq!(b.tick(t0 + SEC * 2).iter().map(|s| s.key).collect::<Vec<_>>(), ["save"]);
        assert!(b.tick(t0 + SEC * 4).is_empty());
        assert_eq!(b.tick(t0 + SEC * 5).iter().map(|s| s.key).collect::<Vec<_>>(), ["convert"]);
    }

    #[test]
    fn limits_grow_with_what_a_step_writes() {
        assert_eq!(Step::CaptureStart.limit(), SEC * 30);
        assert_eq!(Step::SaveClip { bytes: 0 }.limit(), SEC * 60);
        assert_eq!(Step::SaveClip { bytes: 2_000_000_000 }.limit(), SEC * 460);
        assert_eq!(Step::Convert { bytes: 50_000_000_000 }.limit(), SEC * 10_600);
    }

    #[test]
    fn a_step_held_past_a_tiny_limit_writes_exactly_one_report() {
        let dir = tempfile::tempdir().unwrap();
        let dog = Watchdog::spawn(Duration::from_millis(10), Some(dir.path().to_owned()));
        {
            let _w = dog.watch("save", "saving a clip", Duration::from_millis(50));
            std::thread::sleep(Duration::from_millis(500)); // stuck for ten times its limit
        }
        std::thread::sleep(Duration::from_millis(100));
        drop(dog);
        let reports: Vec<PathBuf> = std::fs::read_dir(dir.path()).unwrap().flatten().map(|e| e.path()).collect();
        assert_eq!(reports.len(), 1, "{reports:?}");
        let name = reports[0].file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("stuck-") && name.ends_with("-save.txt"), "{name}");
        let text = std::fs::read_to_string(&reports[0]).unwrap();
        assert!(text.contains("step: saving a clip"), "{text}");
        #[cfg(target_arch = "x86_64")]
        assert!(text.contains("ntdll.dll+0x"), "the stuck thread's stack is in the report:\n{text}");
    }

    #[test]
    fn only_the_newest_reports_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("window-freeze-1.txt"), "").unwrap();
        for i in 0..8 {
            save_report(dir.path(), "stuck-", &format!("stuck-{i}.txt"), "x", 3).unwrap();
        }
        let mut names: Vec<String> =
            std::fs::read_dir(dir.path()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into()).collect();
        names.sort();
        assert_eq!(names, ["stuck-5.txt", "stuck-6.txt", "stuck-7.txt", "window-freeze-1.txt"]);
    }
}
