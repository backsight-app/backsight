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
    sync::{LazyLock, mpsc},
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

/// Where "Report it" files a new issue: the bug report form (the repository allows no blank issues).
const NEW_ISSUE: &str = "https://github.com/backsight-app/backsight/issues/new?template=bug_report.yml";
/// GitHub refuses an issue text over 65 536 characters: a longer report keeps its start and end.
const MAX_SHARE: usize = 60_000;
const SHARE_HEAD: usize = 4_000;

/// When this run started: a report written since then is offered at the next start, not as "last time".
pub static STARTED: LazyLock<SystemTime> = LazyLock::new(SystemTime::now);

/// The report on offer (`crash::pending`) and the reports folder, if `name` (from [`crash_notice`])
/// is still exactly that file name, so no other file can be named.
fn offered(name: &str) -> Option<(PathBuf, PathBuf)> {
    let dir = crash::reports_dir()?;
    let path = crash::pending(&dir, *STARTED)?;
    (path.file_name()? == name).then_some((path, dir))
}

/// The file name of a crash or "got stuck" report (`crash-*` or `stuck-*`) from an earlier run
/// that the user hasn't been asked about.
#[tauri::command(async)]
pub fn crash_notice() -> Option<String> {
    let path = crash::pending(&crash::reports_dir()?, *STARTED)?;
    Some(path.file_name()?.to_string_lossy().into_owned())
}

/// Stops offering report `name`.
#[tauri::command(async)]
pub fn crash_dismiss(name: String) {
    if let Some((_, dir)) = offered(&name) {
        crash::dismiss(&dir, &name);
    }
}

/// Report `name` as it would be shared: scrubbed again with this PC's rules (reports from older
/// versions too) and short enough for an issue. `None`: it's no longer on offer.
#[tauri::command(async)]
pub fn crash_report_text(name: String) -> Option<String> {
    let (path, _) = offered(&name)?;
    Some(share_text(&fs::read_to_string(path).ok()?, &crash::this_pc_rules()))
}

/// Copies report `name` to the clipboard, opens GitHub's bug report form in the browser (the user
/// pastes it into the Log box), then stops offering it.
#[tauri::command(async)]
pub fn crash_report_send(name: String) -> Result<(), String> {
    let (path, dir) = offered(&name).ok_or("Backsight couldn't find the report.")?;
    let report = share_text(
        &fs::read_to_string(&path).map_err(|_| "Backsight couldn't read the report.")?,
        &crash::this_pc_rules(),
    );
    system::copy_text(&report).map_err(|_| "Backsight couldn't copy the report. Please try again.")?;
    let title = if name.starts_with("stuck-") { "Backsight got stuck" } else { "Backsight closed unexpectedly" };
    system::open_path(Path::new(&issue_url(title, &report)), false)
        .map_err(|_| "Backsight couldn't open your web browser.")?;
    crash::dismiss(&dir, &name);
    Ok(())
}

/// The scrubbed report, its middle cut out if it's longer than an issue may be.
fn share_text(text: &str, rules: &Rules) -> String {
    let text = crash::scrub(text.trim_end(), rules);
    if text.len() <= MAX_SHARE {
        return text;
    }
    let head = &text[..text.floor_char_boundary(SHARE_HEAD)];
    let tail = &text[text.ceil_char_boundary(text.len() - (MAX_SHARE - SHARE_HEAD - 100))..];
    format!("{head}\n\n[... part of the report was cut here to fit ...]\n\n{tail}")
}

/// The bug report form's link with `title`, and its version, Windows and graphics card fields
/// filled in from the report's header lines when it has them. Short: the report itself is pasted.
fn issue_url(title: &str, report: &str) -> String {
    let mut url = format!("{NEW_ISSUE}&title={}", url_encode(title));
    // The first line is the report's own title ("Backsight: a step got stuck"), not a field.
    let field = |prefix: &str| report.lines().skip(1).find_map(|l| l.strip_prefix(prefix)).map(str::trim);
    for (id, prefix) in [("version", "Backsight: "), ("windows", "Windows: "), ("gpu", "graphics card: ")] {
        if let Some(v) = field(prefix).filter(|v| !v.is_empty() && !["not started yet", "unknown"].contains(v)) {
            // 60 characters each keeps the link well under 2000 characters.
            let _ = write!(url, "&{id}={}", url_encode(&v.chars().take(60).collect::<String>()));
        }
    }
    url
}

/// Percent-encodes everything but letters, digits and `-._~`.
fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
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
    fn a_shared_report_hides_the_user_and_fits_github() {
        let rules = Rules::new(r"C:\Users\Avery", "avery", &[], &[], vec!["mp4".to_owned()], &|_| None);
        let text = share_text(
            "Backsight crash report (panic)\n\nBacksight: 0.1.13 (built y)\nWindows: Windows 11 & co #1\n\
             graphics card: not started yet\n\ncrash in C:\\Users\\Avery\\AppData\\x\n",
            &rules,
        );
        assert!(!text.contains("Avery") && text.contains("<user>"), "{text}");

        // The form's fields from the header; no report in the link.
        let url = issue_url("Backsight closed unexpectedly", &text);
        assert_eq!(
            url,
            format!(
                "{NEW_ISSUE}&title=Backsight%20closed%20unexpectedly&version=0.1.13%20%28built%20y%29\
                 &windows=Windows%2011%20%26%20co%20%231"
            )
        );
        // A stuck report's title line isn't a version; long header lines keep the link short.
        let url = issue_url("Backsight got stuck", "Backsight: a step got stuck\nBacksight: 1.0\nWindows: é");
        assert!(url.ends_with("&title=Backsight%20got%20stuck&version=1.0&windows=%C3%A9"), "{url}");
        let wide = format!("x\nBacksight: {0}\nWindows: {0}\ngraphics card: {0}", "€".repeat(500));
        assert!(issue_url("Backsight got stuck", &wide).len() < 2000);

        // Too long for an issue: the start and the end are kept.
        let long: String = (0..2000).map(|i| format!("log line {i} é\n")).collect();
        let huge = share_text(&long.repeat(5), &rules);
        assert!(huge.len() <= MAX_SHARE && huge.starts_with("log line 0 ") && huge.ends_with("log line 1999 é"));
        assert!(huge.contains("was cut here"));
    }

    #[test]
    fn a_report_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let first = write_new(dir.path(), "one").unwrap();
        let second = write_new(dir.path(), "two").unwrap();
        assert_ne!(first, second);
        assert_eq!(fs::read_to_string(&first).unwrap(), "\u{feff}one");
        assert_eq!(fs::read_to_string(&second).unwrap(), "\u{feff}two");
        let name = |p: &Path| p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(!name(&first).contains(" ("), "{}", name(&first));
        assert!(name(&second).ends_with(" (2).txt"), "{}", name(&second));
        assert!(write_new(&dir.path().join("gone"), "x").is_err(), "a missing folder is an error, not a retry");
    }

    #[test]
    fn files_are_read_up_to_their_limit() {
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("big.log");
        fs::write(&big, "a line of the log\n".repeat(8_000)).unwrap(); // 144 000 bytes
        assert_eq!(read_all(&big, LOG_TAIL).len(), 144_000, "a log is copied whole up to 300 KB");
        let tail = read_all(&big, FILE_TAIL).len();
        assert!((90_000..=102_400).contains(&tail), "other files: the last 100 KB, got {tail}");
        fs::write(dir.path().join("empty.txt"), " \n").unwrap();
        assert_eq!(read_all(&dir.path().join("empty.txt"), FILE_TAIL), "(empty)");
        assert_eq!(read_all(&dir.path().join("gone.txt"), FILE_TAIL), "(not there)");
    }

    #[test]
    fn the_report_asks_windows_and_lists_its_parts() {
        assert_eq!(ask(|| 5), Some(5));
        let titles: Vec<_> = gather().into_iter().map(|(t, _)| t).take(4).collect();
        assert_eq!(titles, ["Backsight", "Windows", "Graphics cards", "Monitors"]);
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
