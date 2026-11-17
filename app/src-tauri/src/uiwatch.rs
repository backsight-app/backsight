//! What happens inside the windows: their errors go to the log, and a window that stops answering
//! is noticed and reported.
//!
//! - Each page sends its JavaScript errors and failed commands here (`ui_log`) and says it is alive
//!   every 2 s (`ui_heartbeat`); a visible page that goes quiet for 10 s is logged.
//! - A watchdog thread asks the main thread (which runs every window) to do nothing every 2 s. When
//!   that isn't done within 10 s, Windows shows "Not responding": a report with the main thread's
//!   stack goes in the logs folder (`window-freeze-*.txt`), once per freeze.
//! - A WebView2 process that crashes or hangs is logged, and the window is made again.
//!
//! `BACKSIGHT_TEST_FREEZE_MAIN=15` blocks the main thread once for 15 s, about 6 s after start,
//! so the report can be tested.

use backsight_engine::{crash, stack::Watched};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed},
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager, WebviewWindow};

/// How often the watchdog asks the main thread to answer.
const TICK: Duration = Duration::from_secs(2);
/// No answer for this long is a freeze (Windows says "Not responding" after 5 s).
const FROZEN: Duration = Duration::from_secs(10);
/// A tick this late means the watchdog itself was held up (the PC slept): start counting again.
const LATE_TICK: Duration = Duration::from_secs(6);
/// Window messages per minute that reach the log; an error repeating every frame would flood it.
const LOG_PER_MINUTE: u32 = 30;
const LOG_MESSAGE_MAX: usize = 4000;
/// Freeze reports kept in the logs folder.
const KEEP_REPORTS: usize = 5;
/// Windows made again after a WebView2 failure, per run; a page that crashes every time stops there.
const MAX_RECREATES: u32 = 3;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// An error or warning from a window's page, for the log. Off the main thread: it writes a file.
#[tauri::command(async)]
pub fn ui_log(window: WebviewWindow, level: String, message: String) {
    static GATE: Mutex<LogGate> = Mutex::new(LogGate { since: None, count: 0 });
    let label = window.label();
    match lock(&GATE).allow(Instant::now()) {
        Gate::Pass => {}
        Gate::FirstHeld => {
            tracing::warn!("window {label}: more window messages this minute were left out");
            return;
        }
        Gate::Held => return,
    }
    // One log line per message: a newline from the window can't fake another entry.
    let message: String = message.chars().take(LOG_MESSAGE_MAX).map(|c| if c.is_control() { ' ' } else { c }).collect();
    if level == "error" {
        tracing::error!("window {label}: {message}");
    } else {
        tracing::warn!("window {label}: {message}");
    }
}

#[derive(Debug, PartialEq)]
enum Gate {
    Pass,
    /// The first message over the limit: say that some are left out.
    FirstHeld,
    Held,
}

/// At most [`LOG_PER_MINUTE`] messages in each minute.
struct LogGate {
    since: Option<Instant>,
    count: u32,
}

impl LogGate {
    fn allow(&mut self, now: Instant) -> Gate {
        if self.since.is_none_or(|s| now.duration_since(s) >= Duration::from_secs(60)) {
            (self.since, self.count) = (Some(now), 0);
        }
        self.count += 1;
        match self.count {
            n if n <= LOG_PER_MINUTE => Gate::Pass,
            n if n == LOG_PER_MINUTE + 1 => Gate::FirstHeld,
            _ => Gate::Held,
        }
    }
}

/// When each page last said it is alive.
#[derive(Clone, Copy)]
struct Page {
    last: Instant,
    /// Hidden pages' timers are slowed down by WebView2, so only visible ones are watched.
    visible: bool,
    /// Already logged as not answering.
    stalled: bool,
}

static PAGES: Mutex<Option<HashMap<String, Page>>> = Mutex::new(None);

/// A window's page saying it is still running (every 2 s, and when it is shown or hidden).
#[tauri::command]
pub fn ui_heartbeat(window: WebviewWindow, visible: bool) {
    let (label, now) = (window.label(), Instant::now());
    let mut pages = lock(&PAGES);
    let pages = pages.get_or_insert_default();
    if let Some(p) = pages.get(label).filter(|p| p.stalled) {
        tracing::info!("window {label}: the page answers again after {} s", now.duration_since(p.last).as_secs());
    }
    pages.insert(label.to_owned(), Page { last: now, visible, stalled: false });
}

/// Visible pages quiet for [`FROZEN`] that weren't logged yet; marks them logged.
fn newly_stalled(pages: &mut HashMap<String, Page>, now: Instant) -> Vec<(String, Duration)> {
    let mut out = Vec::new();
    for (label, p) in pages.iter_mut() {
        let quiet = now.saturating_duration_since(p.last);
        if p.visible && !p.stalled && quiet >= FROZEN {
            p.stalled = true;
            out.push((label.clone(), quiet));
        }
    }
    out.sort();
    out
}

/// A window was destroyed: stop watching its page and its WebView.
pub fn forget(label: &str) {
    if let Some(pages) = lock(&PAGES).as_mut() {
        pages.remove(label);
    }
    if let Some(hooked) = lock(&HOOKED).as_mut() {
        hooked.remove(label);
    }
}

/// What the watchdog does on one tick.
#[derive(Debug, PartialEq)]
enum Tick {
    /// The last request was answered (or there was none): send the next one.
    Post,
    /// Still waiting for an answer.
    Wait,
    /// No answer for this long: write the report (once per freeze).
    Froze(Duration),
    /// Answered after a reported freeze that lasted this long; send the next one.
    Answered(Duration),
}

/// The watchdog's timing decisions, kept apart from the threads so they can be tested.
#[derive(Default)]
struct MainWatch {
    /// When the request still waiting for an answer was sent.
    pending_since: Option<Instant>,
    last_tick: Option<Instant>,
    reported: bool,
}

impl MainWatch {
    /// `answered`: the main thread ran the last request.
    fn tick(&mut self, now: Instant, answered: bool) -> Tick {
        let late = self.last_tick.is_some_and(|t| now.saturating_duration_since(t) > LATE_TICK);
        self.last_tick = Some(now);
        let Some(since) = self.pending_since.filter(|_| !answered) else {
            let froze = self.pending_since.map(|s| now.saturating_duration_since(s));
            self.pending_since = Some(now);
            return match (std::mem::take(&mut self.reported), froze) {
                (true, Some(d)) => Tick::Answered(d),
                _ => Tick::Post,
            };
        };
        if late && !self.reported {
            self.pending_since = Some(now); // the PC slept: the main thread gets a fresh 10 s
            return Tick::Wait;
        }
        let waited = now.saturating_duration_since(since);
        if !self.reported && waited >= FROZEN {
            self.reported = true;
            return Tick::Froze(waited);
        }
        Tick::Wait
    }
}

/// Starts the watchdog. Call on the main thread (in `setup`): it watches the calling thread.
/// `version` is the line reports carry ([`crash::version_line`]).
pub fn start(app: &AppHandle, version: String) {
    let main = Watched::this_thread();
    let app = app.clone();
    let started = std::thread::Builder::new().name("backsight-window-watch".into()).spawn(move || {
        watch(&app, &main, &version);
    });
    if let Err(e) = started {
        tracing::warn!("the window watchdog didn't start: {e}");
    }
}

fn watch(app: &AppHandle, main: &Watched, version: &str) {
    let served = Arc::new(AtomicU64::new(0));
    let mut posted = 0u64;
    let mut watch = MainWatch::default();
    let mut test_freeze =
        std::env::var("BACKSIGHT_TEST_FREEZE_MAIN").ok().and_then(|s| s.trim().parse::<u64>().ok()).filter(|&s| s > 0);
    loop {
        std::thread::sleep(TICK);
        // Quitting blocks the main thread on purpose while files are finished.
        if app.state::<crate::Engine>().quitting.load(Relaxed) {
            return;
        }
        let now = Instant::now();
        match watch.tick(now, served.load(Relaxed) == posted) {
            Tick::Wait => continue,
            Tick::Froze(waited) => {
                freeze_report(main, waited, version);
                continue;
            }
            Tick::Answered(d) => tracing::info!("the window answers again after {} s", d.as_secs()),
            Tick::Post => {}
        }
        // Pages can only answer while the main thread does, so they are checked only now.
        if let Some(pages) = lock(&PAGES).as_mut() {
            for (label, quiet) in newly_stalled(pages, now) {
                tracing::warn!("window {label}: the page hasn't answered for {} s", quiet.as_secs());
            }
        }
        posted += 1;
        let (served, n) = (served.clone(), posted);
        let freeze = if posted == 3 { test_freeze.take() } else { None };
        let sent = app.run_on_main_thread(move || {
            if let Some(secs) = freeze {
                tracing::warn!("test (BACKSIGHT_TEST_FREEZE_MAIN): blocking the window's main thread for {secs} s");
                std::thread::sleep(Duration::from_secs(secs));
            }
            served.store(n, Relaxed);
        });
        if sent.is_err() {
            return; // the event loop has ended
        }
    }
}

/// Writes `window-freeze-<time>.txt` in the logs folder: scrubbed, through a `.part` file.
fn freeze_report(main: &Watched, waited: Duration, version: &str) {
    // Logged first: if reading the stack ever hangs, the log still says what happened.
    tracing::error!(
        "the window stopped responding: its main thread hasn't answered for {} s; saving a report",
        waited.as_secs()
    );
    let stack = match main.capture() {
        Ok(frames) => backsight_engine::stack::describe(&frames),
        Err(e) => format!("not read: {e}\n"),
    };
    let Some(dir) = backsight_config::app_dir().ok().map(|d| d.join("logs")) else { return };
    let now = Instant::now();
    let pages: String = lock(&PAGES)
        .as_ref()
        .map(|pages| {
            let mut lines: Vec<String> = pages
                .iter()
                .map(|(label, p)| {
                    let seen = now.saturating_duration_since(p.last).as_secs();
                    let shown = if p.visible { "shown" } else { "hidden" };
                    format!("{label}: last sign of life {seen} s ago ({shown})\n")
                })
                .collect();
            lines.sort();
            lines.concat()
        })
        .unwrap_or_default();
    let log = crash::last_lines(&crash::read_tail(&dir.join("backsight.log"), 64 * 1024), 100);
    let time = crash::local_time();
    let text = format!(
        "Backsight: the window stopped responding\nThis file stays on this PC unless you send it. Personal details were removed.\n\n\
         time: {time}\nBacksight: {version}\nno answer for: {} s\n\n\
         main thread stack (decode with this build's backsight.pdb):\n{stack}\nwindow pages:\n{pages}\nlast log lines:\n{log}\n",
        waited.as_secs(),
    );
    let name = format!("window-freeze-{}.txt", time.replace([' ', ':'], "-"));
    match backsight_engine::watchdog::save_report(
        &dir,
        "window-freeze-",
        &name,
        &crash::scrub_installed(&text),
        KEEP_REPORTS,
    ) {
        Ok(()) => tracing::error!("freeze report saved: {name}"),
        Err(e) => tracing::error!("the freeze report couldn't be saved: {e}"),
    }
}

/// WebViews with a process-failure handler, by window label.
static HOOKED: Mutex<Option<HashSet<String>>> = Mutex::new(None);
/// The main window is being made again: closing it must not quit Backsight.
static RECREATING: AtomicBool = AtomicBool::new(false);
static RECREATED: AtomicU32 = AtomicU32::new(0);

/// The main window is being made again after a WebView2 failure (see `main.rs`'s window events).
pub fn recreating() -> bool {
    RECREATING.load(Relaxed)
}

/// The replacement main window is open.
pub fn recreated() {
    RECREATING.store(false, Relaxed);
}

/// Watches a window's WebView2 processes; call on each page load (repeats are skipped).
pub fn watch_webview(webview: &tauri::Webview) {
    let label = webview.label().to_owned();
    if !lock(&HOOKED).get_or_insert_default().insert(label.clone()) {
        return;
    }
    let app = webview.app_handle().clone();
    let hooked = webview.with_webview(move |w| {
        let failed = move |kind: Option<i32>| on_process_failed(&app, &label, kind);
        if let Err(e) = platform::on_process_failed(&w.controller(), Box::new(failed)) {
            tracing::warn!("WebView2 crashes won't be noticed in this window: {e}");
        }
    });
    if let Err(e) = hooked {
        tracing::warn!("WebView2 crashes won't be noticed in this window: {e}");
    }
}

/// What to do about a failed WebView2 process.
#[derive(Debug, PartialEq)]
enum Recovery {
    /// WebView2 restarts it by itself (GPU, utility and helper processes, an embedded frame).
    Log,
    /// The page is gone or hung: make the window again.
    Recreate,
    /// Made again too often this run: leave it.
    GiveUp,
}

/// `kind` is `COREWEBVIEW2_PROCESS_FAILED_KIND`: 0 browser process exited, 1 render process exited,
/// 2 render process unresponsive; the rest WebView2 recovers by itself.
fn recovery(kind: Option<i32>, recreated: u32) -> Recovery {
    match kind {
        Some(0..=2) if recreated >= MAX_RECREATES => Recovery::GiveUp,
        Some(0..=2) => Recovery::Recreate,
        _ => Recovery::Log,
    }
}

fn kind_name(kind: Option<i32>) -> &'static str {
    match kind {
        Some(0) => "the WebView2 browser process ended",
        Some(1) => "the page's process ended",
        Some(2) => "the page stopped responding",
        Some(3) => "an embedded frame's process ended",
        Some(4) => "a WebView2 utility process ended",
        Some(5) => "the WebView2 sandbox helper ended",
        Some(6) => "the WebView2 graphics process ended",
        _ => "a WebView2 process ended",
    }
}

/// Runs on the main thread, inside WebView2's event: the window is changed afterwards, not here.
fn on_process_failed(app: &AppHandle, label: &str, kind: Option<i32>) {
    let what = kind_name(kind);
    match recovery(kind, RECREATED.load(Relaxed)) {
        Recovery::Log => tracing::warn!("window {label}: {what} (WebView2 restarts it)"),
        Recovery::GiveUp => {
            tracing::error!("window {label}: {what}; it was made again {MAX_RECREATES} times already, so it's left")
        }
        Recovery::Recreate => {
            if label == "main" && RECREATING.swap(true, Relaxed) {
                return; // already being made again (a hung page reports again and again)
            }
            // One browser-process crash reaches every window; only the main window's remakes count.
            if label == "main" {
                RECREATED.fetch_add(1, Relaxed);
            }
            tracing::error!("window {label}: {what}; making the window again");
            let (app2, label2) = (app.clone(), label.to_owned());
            let posted = app.run_on_main_thread(move || {
                // Pop-ups and the area picker reopen when next needed; `main.rs` reopens the main
                // window when this one's `Destroyed` arrives.
                if let Some(w) = app2.get_webview_window(&label2) {
                    if w.destroy().is_err() && label2 == "main" {
                        recreated();
                    }
                } else if label2 == "main" {
                    recreated();
                }
            });
            if posted.is_err() && label == "main" {
                recreated();
            }
        }
    }
}

/// The WebView2 COM call, kept small.
mod platform {
    use webview2_com::{
        Microsoft::Web::WebView2::Win32::{COREWEBVIEW2_PROCESS_FAILED_KIND, ICoreWebView2Controller},
        ProcessFailedEventHandler,
    };

    /// Calls `failed` with the failure kind whenever one of the WebView's processes fails.
    pub fn on_process_failed(
        controller: &ICoreWebView2Controller,
        failed: Box<dyn Fn(Option<i32>) + Send>,
    ) -> webview2_com::Result<()> {
        let handler = ProcessFailedEventHandler::create(Box::new(move |_, args| {
            let kind = args.and_then(|a| {
                let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
                // SAFETY: `kind` is writable; `a` is the live event-args object WebView2 passed.
                unsafe { a.ProcessFailedKind(&mut kind) }.ok().map(|()| kind.0)
            });
            failed(kind);
            Ok(())
        }));
        let mut token = 0i64;
        // SAFETY: called on the main thread (inside `with_webview`), where the controller lives;
        // `token` is writable. The handler stays registered for the WebView's life.
        unsafe { controller.CoreWebView2()?.add_ProcessFailed(&handler, &mut token)? };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_main_thread_that_answers_gets_a_request_every_tick() {
        let (mut w, t0) = (MainWatch::default(), Instant::now());
        assert_eq!(w.tick(t0, true), Tick::Post);
        assert_eq!(w.tick(t0 + TICK, true), Tick::Post);
        assert_eq!(w.tick(t0 + TICK * 2, true), Tick::Post);
    }

    #[test]
    fn ten_seconds_without_an_answer_is_one_report_then_answered_again() {
        let (mut w, t0) = (MainWatch::default(), Instant::now());
        assert_eq!(w.tick(t0, true), Tick::Post);
        for i in 1..5 {
            assert_eq!(w.tick(t0 + TICK * i, false), Tick::Wait, "tick {i}");
        }
        assert_eq!(w.tick(t0 + TICK * 5, false), Tick::Froze(FROZEN));
        for i in 6..20 {
            assert_eq!(w.tick(t0 + TICK * i, false), Tick::Wait, "one report per freeze, tick {i}");
        }
        assert_eq!(w.tick(t0 + TICK * 20, true), Tick::Answered(TICK * 20));
        assert_eq!(w.tick(t0 + TICK * 21, true), Tick::Post);
        for i in 22..26 {
            assert_eq!(w.tick(t0 + TICK * i, false), Tick::Wait, "tick {i}");
        }
        assert_eq!(w.tick(t0 + TICK * 26, false), Tick::Froze(FROZEN), "a second freeze is reported too");
    }

    #[test]
    fn a_pc_that_slept_is_not_a_freeze() {
        let (mut w, t0) = (MainWatch::default(), Instant::now());
        assert_eq!(w.tick(t0, true), Tick::Post);
        // The watchdog itself slept for an hour: the main thread gets a fresh 10 s.
        let woke = t0 + Duration::from_secs(3600);
        assert_eq!(w.tick(woke, false), Tick::Wait);
        for i in 1..5 {
            assert_eq!(w.tick(woke + TICK * i, false), Tick::Wait, "tick {i}");
        }
        assert_eq!(w.tick(woke + TICK * 5, false), Tick::Froze(FROZEN));
    }

    #[test]
    fn the_log_takes_thirty_window_messages_a_minute() {
        let (mut g, t0) = (LogGate { since: None, count: 0 }, Instant::now());
        for _ in 0..LOG_PER_MINUTE {
            assert_eq!(g.allow(t0), Gate::Pass);
        }
        assert_eq!(g.allow(t0), Gate::FirstHeld);
        assert_eq!(g.allow(t0 + Duration::from_secs(30)), Gate::Held);
        assert_eq!(g.allow(t0 + Duration::from_secs(60)), Gate::Pass);
    }

    #[test]
    fn only_visible_quiet_pages_are_logged_once() {
        let t0 = Instant::now();
        let page = |visible| Page { last: t0, visible, stalled: false };
        let mut pages: HashMap<String, Page> =
            [("main".to_owned(), page(true)), ("hud".to_owned(), page(false))].into_iter().collect();
        assert!(newly_stalled(&mut pages, t0 + TICK).is_empty());
        assert_eq!(newly_stalled(&mut pages, t0 + FROZEN), [("main".to_owned(), FROZEN)]);
        assert!(newly_stalled(&mut pages, t0 + FROZEN * 2).is_empty());
    }

    #[test]
    fn a_dead_or_hung_page_is_made_again_a_few_times() {
        for kind in [0, 1, 2] {
            assert_eq!(recovery(Some(kind), 0), Recovery::Recreate);
            assert_eq!(recovery(Some(kind), MAX_RECREATES), Recovery::GiveUp);
        }
        for kind in [3, 4, 5, 6, 7, 8, 9] {
            assert_eq!(recovery(Some(kind), 0), Recovery::Log, "kind {kind}");
        }
        assert_eq!(recovery(None, 0), Recovery::Log);
    }
}
