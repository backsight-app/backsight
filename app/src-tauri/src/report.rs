//! Problem reports: `backsight.exe --report` (see `cli.rs`) and Home's "Save a problem report".
//!
//! One plain text file on the Desktop with everything a helper needs: build, Windows, graphics
//! cards and drivers, monitors, settings, both logs and every report file Backsight left behind.
//! It only reads files, so `--report` works while the main Backsight is frozen (it is its own
//! process and takes no lock and talks to nothing). The whole text is scrubbed like crash reports.

use backsight_engine::{
    capture::{d3d, wgc},
    crash::{self, Rules},
    setup, system,
};
use std::{
    fmt::Write as _,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, SystemTime},
};

/// How much of each log is copied (the end of it).
const LOG_TAIL: u64 = 300 * 1024;
/// How much of every other report file is copied, and how many files at most.
const FILE_TAIL: u64 = 100 * 1024;
const MAX_FILES: usize = 25;
/// How long a Windows question (graphics cards, monitors) may take before the report goes on without it.
const ASK_SECONDS: u64 = 5;

/// Exit code of `--report`.
pub fn run() -> i32 {
    let title = "Backsight problem report";
    match save() {
        Ok(path) => {
            // Explorer with the file selected; if that fails, say where it is.
            if system::open_path(&path, true).is_err() {
                system::message_box(title, &format!("The report is saved on your Desktop:\n{}", path.display()));
            }
            0
        }
        Err(e) => {
            system::message_box(title, &e);
            1
        }
    }
}

/// Saves a problem report; returns where it went.
#[tauri::command]
pub async fn save_problem_report() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let path = save()?;
        let _ = system::open_path(&path, true);
        Ok(path.to_string_lossy().into_owned())
    })
    .await
    .map_err(|_| "Backsight couldn't make the report. Please try again.".to_owned())?
}

/// Collects, scrubs and writes the report on the Desktop; the error is plain words.
fn save() -> Result<PathBuf, String> {
    let desktop = directories::UserDirs::new()
        .and_then(|d| d.desktop_dir().map(Path::to_path_buf))
        .ok_or("Backsight couldn't find your Desktop folder, so it couldn't save the report.")?;
    let text = assemble(&gather(), &crash::this_pc_rules());
    write_new(&desktop, &text).map_err(|e| {
        tracing::warn!("problem report not saved: {e}");
        "Backsight couldn't save the report on your Desktop. Check that the Desktop isn't full or locked, then try again."
            .to_owned()
    })
}

/// Writes `Backsight problem report <date time>.txt` in `dir`, under another name if it exists.
fn write_new(dir: &Path, text: &str) -> std::io::Result<PathBuf> {
    let stamp: String = system::local_time_stamp().chars().take(16).collect(); // no seconds
    for n in 1.. {
        let suffix = if n == 1 { String::new() } else { format!(" ({n})") };
        let path = dir.join(format!("Backsight problem report {stamp}{suffix}.txt"));
        match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                // A BOM, so Notepad on Windows 10 reads "×" and other characters right.
                f.write_all(b"\xEF\xBB\xBF")?;
                f.write_all(text.as_bytes())?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

/// Runs `f` on its own thread and gives up after [`ASK_SECONDS`]: a stuck graphics driver must
/// not freeze the report. (The thread is left behind; the process is short-lived.)
// shortcut: from Home's button this runs inside Backsight, so a stuck driver call leaves one stuck
// thread per press; start `backsight.exe --report` instead if that ever matters.
fn ask<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(ASK_SECONDS)).ok()
}

/// The report as (title, text) parts, not scrubbed yet.
fn gather() -> Vec<(String, String)> {
    let mut parts = Vec::new();
    let build = crate::this_build().unwrap_or_default();
    parts.push(("Backsight".to_owned(), crash::version_line(&build)));
    parts.push(("Windows".to_owned(), crash::windows_version()));

    let cards = ask(|| (setup::graphics_cards(), d3d::drivers_key()));
    parts.push((
        "Graphics cards".to_owned(),
        match cards {
            Some((cards, drivers)) => {
                let mut s = String::new();
                for c in cards {
                    let _ = writeln!(s, "{} ({})", c.name, c.vendor);
                }
                let _ = write!(
                    s,
                    "driver versions (vendor id:version): {}",
                    drivers.unwrap_or_else(|e| format!("unknown ({e})"))
                );
                s
            }
            None => format!("Windows didn't answer within {ASK_SECONDS} seconds (the graphics driver may be stuck)."),
        },
    ));

    let monitors = ask(wgc::monitors).map_or_else(
        || format!("Windows didn't answer within {ASK_SECONDS} seconds."),
        |m| m.iter().map(|m| m.label.as_str()).collect::<Vec<_>>().join("\n"),
    );
    parts.push(("Monitors".to_owned(), monitors));

    let Ok(app_dir) = backsight_config::app_dir() else {
        parts.push(("Files".to_owned(), "Backsight's data folder wasn't found.".to_owned()));
        return parts;
    };
    // The file as it is, so a settings file that doesn't parse shows up too.
    let settings = backsight_config::Settings::default_path().ok().map(|p| read_all(&p, FILE_TAIL));
    parts.push(("settings.toml".to_owned(), settings.unwrap_or_else(|| "not found".to_owned())));

    let logs = app_dir.join("logs");
    for name in ["backsight.log", "backsight.old.log", "install.log"] {
        parts.push((format!("logs\\{name} (end of the file)"), read_all(&logs.join(name), LOG_TAIL)));
    }
    // Everything else the logs folder holds that looks like a report (freeze reports, window errors),
    // then the crash reports; newest first.
    let mut files = extra_files(&logs);
    files.extend(extra_files(&app_dir.join("crash-reports")));
    files.sort_by_key(|(_, t)| std::cmp::Reverse(*t));
    for (path, _) in files.into_iter().take(MAX_FILES) {
        let dir = path.parent().and_then(Path::file_name).map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        parts.push((format!("{dir}\\{name}"), read_all(&path, FILE_TAIL)));
    }
    parts
}

/// Report-like files in `dir` with their modified time: `.txt` files and names with "report" in
/// them, except the "seen" mark and unfinished `.part` files.
fn extra_files(dir: &Path) -> Vec<(PathBuf, SystemTime)> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_lowercase();
            (n.ends_with(".txt") || n.contains("report")) && n != "seen.txt" && !n.ends_with(".part")
        })
        .map(|e| (e.path(), e.metadata().and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH)))
        .collect()
}

/// The end of a file, or a short note when it can't be read.
fn read_all(path: &Path, max: u64) -> String {
    match fs::metadata(path) {
        Ok(_) => {
            let t = crash::read_tail(path, max);
            if t.trim().is_empty() { "(empty)".to_owned() } else { t }
        }
        Err(_) => "(not there)".to_owned(),
    }
}

/// The finished text: a header, every part under its title, all scrubbed with `rules`.
fn assemble(parts: &[(String, String)], rules: &Rules) -> String {
    let mut text = String::from(
        "Backsight problem report\nThis file stays on this PC unless you send it. Personal details were removed.\n",
    );
    for (title, body) in parts {
        let _ = write!(text, "\n===== {title} =====\n{}\n", body.trim_end());
    }
    crash::scrub(&text, rules) + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_names_no_user_pc_or_folder() {
        let rules = Rules::new(
            r"C:\Users\Avery Longname",
            "avery",
            &["DESKTOP-AVERY1".to_owned()],
            &[(r"D:\Riley clips".to_owned(), "<clips folder>")],
            vec!["mp4".to_owned()],
            &|_| None,
        );
        let parts = vec![
            ("Windows".to_owned(), "Windows 11 on DESKTOP-AVERY1".to_owned()),
            (
                "logs".to_owned(),
                "INFO saved D:\\Riley clips\\Elden Ring 1.mp4\nopen C:\\Users\\Avery Longname\\AppData\\Roaming\\Backsight\nuser avery on desktop-avery1".to_owned(),
            ),
        ];
        let text = assemble(&parts, &rules);
        for secret in ["avery", "Avery", "AVERY", "Longname", "Riley", "Elden"] {
            assert!(!text.contains(secret), "{secret} survived:\n{text}");
        }
        assert!(text.contains("<user>") && text.contains("<pc>") && text.contains("<clips folder>"));
        assert!(text.contains("===== Windows =====") && text.contains("Windows 11"));
    }

    #[test]
    fn a_report_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let first = write_new(dir.path(), "one").unwrap();
        let second = write_new(dir.path(), "two").unwrap();
        assert_ne!(first, second);
        assert_eq!(fs::read_to_string(first).unwrap(), "\u{feff}one");
        assert_eq!(fs::read_to_string(second).unwrap(), "\u{feff}two");
    }

    #[test]
    fn only_report_like_files_are_picked_up() {
        let dir = tempfile::tempdir().unwrap();
        for n in [
            "backsight.log",
            "backsight.old.log",
            "seen.txt",
            "freeze.txt",
            "window-report.json",
            "other.log",
            "x.txt.part",
        ] {
            fs::write(dir.path().join(n), "x").unwrap();
        }
        let mut found: Vec<String> = extra_files(dir.path())
            .iter()
            .map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        found.sort();
        assert_eq!(found, ["freeze.txt", "window-report.json"]);
    }
}
