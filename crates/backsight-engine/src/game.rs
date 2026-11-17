//! Which game is being played, as far as Backsight can tell: the program that has kept focus for a
//! few seconds. Game-only audio records its sound, and saved files are named after it. Window
//! queries are in `capture::foreground`; the logic here is pure and tested.

use crate::capture::{
    foreground::{self, Focus, Program},
    wgc::{CLOCK_HZ, now},
};
use backsight_config::General;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering::Relaxed},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

/// The file name start when no game is known (the desktop, or a program without a usable name).
pub const FALLBACK_NAME: &str = "Backsight";

/// A window must stay focused this long before its program is picked, so the Start menu, Alt+Tab
/// or a launcher passing through doesn't become the game.
const STEADY_FOCUS: u64 = 3 * CLOCK_HZ;
const POLL: Duration = Duration::from_millis(100);
/// Longer names are cut: a path stays well under Windows' 260-character limit.
const MAX_NAME: usize = 60;

/// Whether `exe` (a file name such as `Battle.net.exe`) is a game launcher or store
/// (`content/game-launchers.json`). A launcher never counts as the game: it would name files, get
/// game-only audio and, in window mode, hold the capture so the windowed game it starts is never
/// recorded.
pub fn is_launcher(exe: &str) -> bool {
    static LIST: OnceLock<Vec<String>> = OnceLock::new();
    LIST.get_or_init(|| serde_json::from_str(include_str!("../../../content/game-launchers.json")).unwrap_or_default())
        .iter()
        .any(|l| l.eq_ignore_ascii_case(exe))
}

/// Invisible formatting characters (Unicode category Cf) that can hide or reverse text in a file
/// name, such as U+202E (right-to-left override) or U+200B (zero-width space).
fn is_format(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206F}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}')
}

/// Turns what a program calls itself into a safe file and folder name, or `None` when nothing
/// usable is left: characters Windows forbids, invisible formatting characters and trademark
/// signs go, spaces are collapsed, the length is capped, and device names such as `CON` are
/// refused.
pub fn clean_name(raw: &str) -> Option<String> {
    let kept: String = raw
        .chars()
        .filter(|&c| !matches!(c, '™' | '®' | '©') && !is_format(c))
        .map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { ' ' } else { c })
        .collect();
    let mut name = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some((cut, _)) = name.char_indices().nth(MAX_NAME) {
        name.truncate(cut);
    }
    // Windows drops trailing dots and spaces, which would make the folder another name.
    let name = name.trim_end_matches(['.', ' ']);
    let device = name.split('.').next().unwrap_or_default().trim_end().to_ascii_uppercase();
    let port = device.strip_prefix("COM").or_else(|| device.strip_prefix("LPT"));
    let reserved = matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || port.is_some_and(|n| {
            let mut n = n.chars();
            matches!((n.next(), n.next()), (Some('0'..='9' | '¹' | '²' | '³'), None))
        });
    (!name.is_empty() && !reserved).then(|| name.to_owned())
}

/// Where a new file goes and how its name starts: `root\<game>` with "folder per game", and the
/// game's name instead of "Backsight" with "name files after the game". `game` is a
/// [`clean_name`]; without one the file goes in `root`, named "Backsight …".
pub fn place(root: &Path, game: Option<&str>, general: &General) -> (PathBuf, String) {
    let folder = match game {
        Some(game) if general.folder_per_game => root.join(game),
        _ => root.to_path_buf(),
    };
    let name = game.filter(|_| general.name_after_game).unwrap_or(FALLBACK_NAME);
    (folder, name.to_owned())
}

/// The [`clean_name`] of a program, if it has one.
pub fn program_name(program: &Program) -> Option<String> {
    clean_name(&program.label).or_else(|| clean_name(&Path::new(&program.exe).file_stem()?.to_string_lossy()))
}

/// Settles on a key (a PID, or `None` for the desktop) once it has been seen for [`STEADY_FOCUS`].
#[derive(Default)]
struct Steady {
    candidate: Option<(Option<u32>, u64)>,
    settled: Option<Option<u32>>,
}

impl Steady {
    /// `seen` is `None` when focus says nothing (another monitor, Backsight itself): the candidate
    /// starts over and the settled key stays. Returns the key when a new one settles.
    fn observe(&mut self, seen: Option<Option<u32>>, time: u64) -> Option<Option<u32>> {
        let Some(key) = seen else {
            self.candidate = None;
            return None;
        };
        match self.candidate {
            Some((k, since)) if k == key => {
                if time.saturating_sub(since) >= STEADY_FOCUS && self.settled != Some(key) {
                    self.settled = Some(key);
                    return Some(key);
                }
            }
            _ => self.candidate = Some((key, time)),
        }
        None
    }

    /// Forgets the settled key, so the same one settles again after another [`STEADY_FOCUS`]:
    /// for a program whose details couldn't be read this time.
    fn retry(&mut self) {
        self.settled = None;
        self.candidate = None;
    }
}

/// Watches focus on a background thread while capture runs; dropping it stops the thread.
pub struct GameWatch {
    current: Arc<Mutex<Option<Program>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl GameWatch {
    /// Watches windows on `monitor` (a raw `HMONITOR`), or on every monitor with `None`.
    /// `on_poll` runs on the watch thread after every look (10 times a second) with the current
    /// game; window capture follows the game's window there.
    pub fn start(
        monitor: Option<isize>,
        mut on_poll: impl FnMut(Option<&Program>) + Send + 'static,
    ) -> std::io::Result<Self> {
        let current = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let (out, quit) = (current.clone(), stop.clone());
        let thread = thread::Builder::new().name("backsight-game-watch".into()).spawn(move || {
            let mut steady = Steady::default();
            let mut window = 0;
            while !quit.load(Relaxed) {
                let focus = foreground::focus(monitor);
                let seen = focus.map(|f| match f {
                    Focus::Window { window: w, pid } => {
                        window = w;
                        Some(pid)
                    }
                    Focus::Shell => None,
                });
                // Read before taking the lock: it opens the process and reads its file.
                let mut settled =
                    steady.observe(seen, now()).map(|pid| pid.map(|pid| (pid, foreground::program(window, pid))));
                if let Some(Some((_, Some(p)))) = &settled
                    && is_launcher(&p.exe)
                {
                    // Like Backsight's own window: the game before it stays the game.
                    tracing::info!("{} is a game launcher, not the game", p.exe);
                    settled = None;
                }
                if let Some(Some((pid, None))) = settled {
                    // Gone, the shell, or not readable right now: try again after steady focus.
                    tracing::debug!("no program details for pid {pid}");
                    steady.retry();
                }
                let mut current = crate::session::lock(&out);
                if let Some(settled) = settled {
                    let program = settled.and_then(|(_, program)| program);
                    match &program {
                        Some(p) => tracing::info!("game in front: {} ({})", p.exe, p.label),
                        None => tracing::info!("no game in front"),
                    }
                    *current = program;
                } else if let (Some(p), Some(Focus::Window { window: w, pid })) = (current.as_mut(), focus) {
                    // The game opened another window (a launcher's game, a restart): follow it.
                    if p.pid == pid {
                        p.window = w;
                    }
                }
                let program = current.clone();
                drop(current);
                on_poll(program.as_ref());
                thread::park_timeout(POLL);
            }
        })?;
        Ok(Self { current, stop, thread: Some(thread) })
    }

    /// The program that last kept focus, or `None` when it was the desktop or nothing yet.
    pub fn current(&self) -> Option<Program> {
        crate::session::lock(&self.current).clone()
    }

    /// [`Self::current`] for another thread.
    pub(crate) fn reader(&self) -> impl Fn() -> Option<Program> + Send + 'static {
        let current = self.current.clone();
        move || crate::session::lock(&current).clone()
    }

    /// The [`clean_name`] of [`Self::current`].
    pub fn name(&self) -> Option<String> {
        self.current().as_ref().and_then(program_name)
    }
}

impl Drop for GameWatch {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(t) = self.thread.take() {
            t.thread().unpark();
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_lose_what_windows_forbids() {
        assert_eq!(clean_name("Tomb Raider").as_deref(), Some("Tomb Raider"));
        assert_eq!(clean_name("  Shadow of the Tomb Raider™ ").as_deref(), Some("Shadow of the Tomb Raider"));
        assert_eq!(clean_name("Half-Life: Alyx").as_deref(), Some("Half-Life Alyx"));
        assert_eq!(clean_name(r#"a<b>c"d/e\f|g?h*i"#).as_deref(), Some("a b c d e f g h i"));
        assert_eq!(clean_name("Tabs\tand\nnew lines").as_deref(), Some("Tabs and new lines"));
        assert_eq!(clean_name("Microsoft® Edge").as_deref(), Some("Microsoft Edge"));
        assert_eq!(clean_name("Ends with dots...").as_deref(), Some("Ends with dots"));
        assert_eq!(clean_name("Spiel über Ärger 日本").as_deref(), Some("Spiel über Ärger 日本"), "letters stay");
        assert_eq!(clean_name("Evil\u{202E}4pm.exe").as_deref(), Some("Evil4pm.exe"), "no hidden reversal");
        assert_eq!(clean_name("Zero\u{200B}Width\u{FEFF}").as_deref(), Some("ZeroWidth"));
    }

    #[test]
    fn empty_and_device_names_are_refused() {
        for raw in [
            "",
            "   ",
            "???",
            "...",
            "CON",
            "nul",
            "Com1",
            "LPT9",
            "con.txt",
            "aux ",
            "COM¹",
            "lpt³",
            "CONIN$",
            "conout$.x",
            "\u{200B}",
        ] {
            assert_eq!(clean_name(raw), None, "{raw:?}");
        }
        assert_eq!(clean_name("Console").as_deref(), Some("Console"));
        assert_eq!(clean_name("COM10").as_deref(), Some("COM10"));
    }

    #[test]
    fn long_names_are_cut_on_a_character() {
        let name = clean_name(&"é".repeat(100)).unwrap();
        assert_eq!(name.chars().count(), MAX_NAME);
        let name = clean_name(&format!("{} tail", "a".repeat(59))).unwrap();
        assert_eq!(name, "a".repeat(59), "the cut space is trimmed");
    }

    #[test]
    fn files_go_where_the_settings_say() {
        let root = Path::new(r"C:\Videos\Backsight");
        let mut g = backsight_config::Settings::default().general;
        assert!(g.name_after_game && !g.folder_per_game, "fresh install: named, one folder");
        assert_eq!(place(root, Some("Tomb Raider"), &g), (root.to_path_buf(), "Tomb Raider".into()));
        assert_eq!(place(root, None, &g), (root.to_path_buf(), "Backsight".into()));
        g.folder_per_game = true;
        assert_eq!(place(root, Some("Tomb Raider"), &g), (root.join("Tomb Raider"), "Tomb Raider".into()));
        assert_eq!(place(root, None, &g), (root.to_path_buf(), "Backsight".into()), "no game: no folder");
        g.name_after_game = false;
        assert_eq!(place(root, Some("Tomb Raider"), &g), (root.join("Tomb Raider"), "Backsight".into()));
    }

    #[test]
    fn launchers_are_never_the_game() {
        for exe in
            ["Battle.net.exe", "steam.exe", "STEAMWEBHELPER.EXE", "EpicGamesLauncher.exe", "RiotClientServices.exe"]
        {
            assert!(is_launcher(exe), "{exe}");
        }
        for exe in ["Wow.exe", "Overwatch.exe", "cs2.exe", "Discord.exe", ""] {
            assert!(!is_launcher(exe), "{exe}");
        }
        let list: Vec<String> = serde_json::from_str(include_str!("../../../content/game-launchers.json")).unwrap();
        assert!(list.iter().all(|e| e.to_ascii_lowercase().ends_with(".exe")), "file names, as Windows reports them");
    }

    #[test]
    fn a_program_settles_after_steady_focus() {
        let s = CLOCK_HZ;
        let mut steady = Steady::default();
        assert_eq!(steady.observe(Some(Some(7)), 0), None);
        assert_eq!(steady.observe(Some(Some(7)), 2 * s), None, "not yet");
        assert_eq!(steady.observe(Some(Some(7)), 3 * s), Some(Some(7)));
        assert_eq!(steady.observe(Some(Some(7)), 9 * s), None, "settles once");
        // Alt+Tab through another window for a moment changes nothing.
        assert_eq!(steady.observe(Some(Some(9)), 10 * s), None);
        assert_eq!(steady.observe(Some(Some(7)), 11 * s), None);
        // Backsight's own window or another monitor: the game stays, the new candidate restarts.
        assert_eq!(steady.observe(Some(None), 12 * s), None);
        assert_eq!(steady.observe(None, 13 * s), None);
        assert_eq!(steady.observe(Some(None), 14 * s), None);
        assert_eq!(steady.observe(Some(None), 16 * s), None, "only 2 s since the restart");
        assert_eq!(steady.observe(Some(None), 17 * s), Some(None), "the desktop settles too");
        // A program whose details couldn't be read is tried again after another steady stretch.
        assert_eq!(steady.observe(Some(Some(5)), 20 * s), None);
        assert_eq!(steady.observe(Some(Some(5)), 23 * s), Some(Some(5)));
        steady.retry();
        assert_eq!(steady.observe(Some(Some(5)), 24 * s), None);
        assert_eq!(steady.observe(Some(Some(5)), 27 * s), Some(Some(5)), "settles again");
    }
}
