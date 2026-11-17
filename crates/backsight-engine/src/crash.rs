//! Local crash reports. If Backsight panics (on any thread) or hits a native crash, a plain text
//! report is saved in `%APPDATA%\Backsight\crash-reports`. Nothing is ever sent anywhere: whoever
//! uses the PC decides whether to share it. Every report is scrubbed first ([`scrub`]) so it names
//! no user, PC, address, clip or clip folder. The newest 20 are kept.
//!
//! The panic hook runs, then the previous hook, then the process does what it did before (in a
//! release build `panic = "abort"`). Native crashes use `SetUnhandledExceptionFilter`, which only
//! sees this process; there is no injection and no hook into any other process.

use backsight_encode::Catalog;
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

/// Reports kept; older ones are deleted when a new one is written.
pub const KEEP: usize = 20;
/// Log lines copied into a report, read from the last [`LOG_TAIL`] bytes of the log.
const LOG_LINES: usize = 200;
const LOG_TAIL: u64 = 64 * 1024;
/// Words used as placeholders; a user name equal to one is left alone so it can't nest.
const PLACEHOLDERS: &[&str] = &["user", "pc", "file", "host", "email", "ip", "clips", "screenshots", "folder"];

/// What gets removed from a report. Build it with [`Rules::new`].
#[derive(Default)]
pub struct Rules {
    /// Every spelling of the profile folder (`C:\Users\Avery`, doubled backslashes, 8.3, `file:` form).
    profile: Vec<String>,
    /// The Windows user name and the profile folder's own name.
    names: Vec<String>,
    /// This PC's names (short and full).
    pcs: Vec<String>,
    /// Folders with the label that replaces them, longest spelling first.
    folders: Vec<(String, &'static str)>,
    /// Lower-case extensions of the user's own files.
    extensions: Vec<String>,
}

impl Rules {
    /// `short` maps a path to its 8.3 form, when Windows has one (`None` in tests).
    pub fn new(
        profile: &str,
        user: &str,
        pcs: &[String],
        folders: &[(String, &'static str)],
        extensions: Vec<String>,
        short: &dyn Fn(&str) -> Option<String>,
    ) -> Rules {
        let by_length = |v: &mut Vec<String>| {
            v.sort_by_key(|s| std::cmp::Reverse(s.len()));
            v.dedup();
        };
        let mut r = Rules { extensions, ..Rules::default() };
        if !profile.is_empty() {
            r.profile = path_forms(profile, short);
            let leaf = profile.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().unwrap_or("");
            r.names.push(leaf.to_owned());
        }
        r.names.push(user.to_owned());
        r.names.retain(|n| !n.is_empty() && !PLACEHOLDERS.contains(&n.to_lowercase().as_str()));
        r.pcs = pcs.iter().filter(|p| !p.is_empty()).cloned().collect();
        for (path, label) in folders {
            if !path.trim().is_empty() {
                r.folders.extend(path_forms(path, short).into_iter().map(|f| (f, *label)));
            }
        }
        r.folders.sort_by_key(|(s, _)| std::cmp::Reverse(s.len()));
        r.folders.dedup();
        by_length(&mut r.profile);
        r
    }
}

/// The ways a path shows up in text: as is, with `/`, with doubled `\` (Rust's `{:?}`), in 8.3
/// form, and percent-encoded (`file:///` links).
fn path_forms(path: &str, short: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
    let path = path.trim().trim_end_matches(['\\', '/']);
    let mut bases = vec![path.to_owned()];
    bases.extend(short(path));
    let mut forms = Vec::new();
    for b in bases {
        let slash = b.replace('\\', "/");
        forms.push(percent_encode(&slash));
        forms.push(b.replace('\\', "\\\\"));
        forms.push(slash);
        forms.push(b);
    }
    forms.retain(|f| f.len() > 2);
    forms.sort_by_key(|s| std::cmp::Reverse(s.len()));
    forms.dedup();
    forms
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) => (b as char).to_string(),
            b => format!("%{b:02X}"),
        })
        .collect()
}

/// What is known at crash time, filled in at start so the hook does as little as possible.
struct Context {
    dir: PathBuf,
    version: String,
    windows: String,
    rules: Rules,
}

static CONTEXT: OnceLock<Context> = OnceLock::new();
static GPU: Mutex<String> = Mutex::new(String::new());
static BUSY: AtomicBool = AtomicBool::new(false);

/// Remembers the graphics card name for reports (called when the capture device is made).
pub fn note_gpu(name: &str) {
    if let Ok(mut g) = GPU.lock() {
        *g = name.to_owned();
    }
}

/// Local time as `2026-11-12 14:03:59`, also for the window's "stopped responding" reports.
pub use platform::local_time;

/// The folder reports go in.
pub fn reports_dir() -> Option<PathBuf> {
    Some(backsight_config::app_dir().ok()?.join("crash-reports"))
}

/// `built 2026-11-12 14:03, commit abc1234, plus uncommitted changes` from the `build.txt` line
/// `2026-11-12 14:03, abc1234 commit subject (plus uncommitted changes)`: no commit subject.
pub fn build_label(line: &str) -> String {
    let line = line.trim();
    let Some((date, rest)) = line.split_once(", ") else { return line.chars().take(40).collect() };
    let hash = rest.split_whitespace().next().unwrap_or("");
    let dirty = if rest.contains("uncommitted") { ", plus uncommitted changes" } else { "" };
    format!("built {date}, commit {hash}{dirty}")
}

/// The version line reports carry: `0.1.0 (built 2026-11-12 14:03, commit abc1234)`. `build` is the
/// stamp the app was compiled with (the `build.txt` shape), empty for a development build.
pub fn version_line(build: &str) -> String {
    match build.trim() {
        "" => format!("{} (development build)", env!("CARGO_PKG_VERSION")),
        b => format!("{} ({})", env!("CARGO_PKG_VERSION"), build_label(b)),
    }
}

/// The version line [`install`] was given, for the other reports.
pub fn installed_version() -> Option<&'static str> {
    CONTEXT.get().map(|c| c.version.as_str())
}

/// What to remove from text about this PC: its user, PC names, clip and screenshot folders and
/// the extensions of the user's own files. [`install`] uses it; `--report` too, without hooks.
pub fn this_pc_rules() -> Rules {
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    let mut folders = Vec::new();
    if let Ok(s) = backsight_config::Settings::load() {
        let general = &s.general;
        folders.extend(general.video_dir().map(|p| (p.to_string_lossy().into_owned(), "<clips folder>")));
        folders.extend(general.screenshot_dir().map(|p| (p.to_string_lossy().into_owned(), "<screenshots folder>")));
    } else {
        // A broken or newer settings file is when people send reports: still hide its folders.
        let raw = backsight_config::Settings::default_path().ok().and_then(|p| std::fs::read_to_string(p).ok());
        let general =
            raw.as_deref().and_then(|r| r.parse::<toml::Table>().ok()).and_then(|t| t.get("general").cloned());
        for (key, name) in [("video_folder", "<clips folder>"), ("screenshot_folder", "<screenshots folder>")] {
            if let Some(dir) =
                general.as_ref().and_then(|g| g.get(key)).and_then(|v| v.as_str()).filter(|d| !d.is_empty())
            {
                folders.push((dir.to_owned(), name));
            }
        }
    }
    let mut pcs = vec![env("COMPUTERNAME")];
    pcs.extend(platform::pc_names());
    let c = Catalog::get();
    let mut extensions: Vec<String> = c
        .containers
        .iter()
        .map(|x| x.ext.to_lowercase())
        .chain(c.screenshot_formats.iter().map(|x| x.ext.to_lowercase()))
        .chain(["part".to_owned()])
        .collect();
    extensions.sort();
    extensions.dedup();
    Rules::new(&env("USERPROFILE"), &env("USERNAME"), &pcs, &folders, extensions, &platform::short_path)
}

/// For example `Windows 11 Home 25H2 (build 26200)`.
pub fn windows_version() -> String {
    platform::windows_version()
}

/// Installs the panic hook and the native-crash filter. Call once, early, before logging starts
/// (log lines are scrubbed with what this reads). `build` is the build stamp, see [`version_line`].
// shortcut: the clip and screenshot folders are read once here; a folder changed later is only
// hidden after the next start. Re-read the settings in `write_report` if that matters.
pub fn install(build: &str) {
    let Some(dir) = reports_dir() else { return };
    let version = version_line(build);
    let ctx = Context { dir, version, windows: platform::windows_version(), rules: this_pc_rules() };
    if CONTEXT.set(ctx).is_err() {
        return;
    }
    let previous = std::panic::take_hook();
    // shortcut: a development build unwinds, so a panic that is caught and recovered from also
    // leaves a report and Home says Backsight "closed unexpectedly"; release builds abort, where it's true.
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("unnamed");
        let backtrace = std::backtrace::Backtrace::force_capture();
        write_report("panic", &format!("thread: {name}\n{info}\n\nbacktrace:\n{backtrace}"));
        previous(info);
    }));
    platform::install_native_filter();
}

/// Writes one report; never panics and never writes two at once (a crash inside a crash).
fn write_report(kind: &str, detail: &str) {
    let Some(ctx) = CONTEXT.get() else { return };
    if BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    let gpu = GPU.lock().map(|g| g.clone()).unwrap_or_default();
    let gpu = if gpu.is_empty() { "not started yet".to_owned() } else { gpu };
    let log = read_tail(&ctx.dir.join("../logs/backsight.log"), LOG_TAIL);
    let stamp = platform::local_time();
    let text = format!(
        "Backsight crash report ({kind})\nThis file stays on this PC unless you send it. Personal details were removed.\n\n\
         time: {stamp}\nBacksight: {}\nWindows: {}\ngraphics card: {gpu}\n\n{detail}\n\nlast log lines:\n{}\n",
        ctx.version,
        ctx.windows,
        last_lines(&log, LOG_LINES),
    );
    let text = scrub(&text, &ctx.rules);
    if fs::create_dir_all(&ctx.dir).is_ok() {
        // Sortable name, so the newest is last; the thread id keeps two crashes in one second apart.
        let name = format!("crash-{}-{:?}.txt", stamp.replace([' ', ':'], "-"), std::thread::current().id());
        let name: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.')).collect();
        // Written under another name and renamed, so a crash while writing leaves no half report.
        let (part, done) = (ctx.dir.join(format!("{name}.part")), ctx.dir.join(&name));
        if fs::File::create(&part).and_then(|mut f| f.write_all(text.as_bytes())).is_ok() {
            let _ = fs::rename(&part, done);
        } else {
            let _ = fs::remove_file(&part);
        }
        prune(&ctx.dir, KEEP);
    }
    BUSY.store(false, Ordering::SeqCst);
}

/// The last `max` bytes of a file as text, starting at a whole line.
pub fn read_tail(path: &Path, max: u64) -> String {
    let read = || -> std::io::Result<String> {
        let mut f = fs::File::open(path)?;
        let len = f.metadata()?.len();
        f.seek(SeekFrom::Start(len.saturating_sub(max)))?;
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes)?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        Ok(if len > max { text.split_once('\n').map_or(String::new(), |(_, rest)| rest.to_owned()) } else { text })
    };
    read().unwrap_or_default()
}

/// The last `n` lines of `text`.
pub fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Report files in `dir`, oldest first (the names sort by time). Unfinished `.part` files don't count.
pub fn list(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("crash-") && n.ends_with(".txt"))
        })
        .collect();
    files.sort();
    files
}

/// Deletes the oldest reports until `keep` remain, and unfinished ones left by a crash mid-write.
pub fn prune(dir: &Path, keep: usize) {
    let files = list(dir);
    for old in &files[..files.len().saturating_sub(keep)] {
        let _ = fs::remove_file(old);
    }
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        if e.file_name().to_str().is_some_and(|n| n.starts_with("crash-") && n.ends_with(".txt.part")) {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// The newest report the user hasn't dismissed yet, as a file name.
pub fn pending(dir: &Path) -> Option<String> {
    let newest = list(dir).pop()?.file_name()?.to_str()?.to_owned();
    let seen = fs::read_to_string(dir.join("seen.txt")).unwrap_or_default();
    (seen.trim() != newest).then_some(newest)
}

/// Marks the newest report as seen.
pub fn dismiss(dir: &Path) {
    if let Some(name) = list(dir).pop().and_then(|p| p.file_name()?.to_str().map(str::to_owned)) {
        let _ = fs::write(dir.join("seen.txt"), name);
    }
}

/// Removes what could point at one person: their profile folder and Windows user name
/// (`<user>`), this PC's name (`<pc>`), network hosts (`<host>`), the clip and screenshot
/// folders, e-mail addresses, IP addresses, and the names of their own files (`<file>.mp4`).
pub fn scrub(text: &str, rules: &Rules) -> String {
    let mut s = text.to_owned();
    for (f, label) in &rules.folders {
        s = replace_ci(&s, f, label, false);
    }
    for p in &rules.profile {
        s = replace_ci(&s, p, "<user>", false);
    }
    for n in &rules.names {
        s = replace_ci(&s, n, "<user>", true);
    }
    for p in &rules.pcs {
        s = replace_ci(&s, p, "<pc>", true);
    }
    s = s.lines().map(|l| scrub_line(&scrub_unc(l), &rules.extensions)).collect::<Vec<_>>().join("\n");
    s
}

/// `text` with what [`install`] knows about this PC removed, as in reports; unchanged before
/// [`install`]. A trailing line break is kept.
pub fn scrub_installed(text: &str) -> String {
    CONTEXT.get().map_or_else(|| text.to_owned(), |ctx| scrub_keeping_end(text, &ctx.rules))
}

/// [`scrub`], keeping a trailing line break (it joins lines without one).
fn scrub_keeping_end(text: &str, rules: &Rules) -> String {
    let (body, end) = text.strip_suffix('\n').map_or((text, ""), |b| (b, "\n"));
    scrub(body, rules) + end
}

/// Case-insensitive (Unicode, letter by letter) replace; with `whole`, only where letters and
/// digits don't touch it.
fn replace_ci(s: &str, find: &str, with: &str, whole: bool) -> String {
    let hay: Vec<(usize, char)> = s.char_indices().collect();
    let needle: Vec<char> = find.chars().collect();
    if needle.is_empty() {
        return s.to_owned();
    }
    let same = |a: char, b: char| a == b || a.to_lowercase().eq(b.to_lowercase());
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let (mut out, mut i) = (String::new(), 0);
    while i < hay.len() {
        let end = i + needle.len();
        let hit = end <= hay.len()
            && needle.iter().zip(&hay[i..end]).all(|(n, (_, h))| same(*n, *h))
            && !(whole && (i > 0 && word(hay[i - 1].1) || hay.get(end).is_some_and(|(_, c)| word(*c))));
        if hit {
            out.push_str(with);
            i = end;
        } else {
            out.push(hay[i].1);
            i += 1;
        }
    }
    out
}

/// `\\HOST\share` (or the doubled-backslash spelling) becomes `\\<host>\share`.
fn scrub_unc(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let (mut out, mut i) = (String::new(), 0);
    while i < chars.len() {
        let run = chars[i..].iter().take_while(|c| **c == '\\').count();
        let starts_word = i == 0 || chars[i - 1].is_whitespace() || "\"'(=[,".contains(chars[i - 1]);
        if matches!(run, 2 | 4) && starts_word {
            let host_len = chars[i + run..].iter().take_while(|c| c.is_alphanumeric() || "-_.".contains(**c)).count();
            let host: String = chars[i + run..i + run + host_len].iter().collect();
            if host_len > 0 && host != "." {
                out.push_str(&"\\".repeat(run));
                out.push_str("<host>");
                i += run + host_len;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

const DELIMITERS: &str = "\"'(),;<>[]=";

fn scrub_line(line: &str, extensions: &[String]) -> String {
    let mut line = scrub_files(line, extensions);
    if line.contains('@') || line.chars().any(|c| c.is_ascii_digit()) {
        line = line
            .split_inclusive(|c: char| c.is_whitespace() || DELIMITERS.contains(c))
            .map(|tok| {
                let cut = |c: char| c.is_whitespace() || DELIMITERS.contains(c);
                let word = tok.trim_end_matches(cut);
                let word = word.trim_end_matches(['.', ':']);
                let tail = &tok[word.len()..];
                if is_email(word) {
                    format!("<email>{tail}")
                } else if is_ip(word) {
                    format!("<ip>{tail}")
                } else if let Some((key, _)) =
                    word.split_once(':').filter(|(k, v)| k.chars().all(char::is_alphabetic) && is_ip(v))
                {
                    format!("{key}:<ip>{tail}") // addr:10.1.1.1
                } else {
                    tok.to_owned()
                }
            })
            .collect();
    }
    line
}

fn is_email(w: &str) -> bool {
    w.split_once('@').is_some_and(|(a, b)| !a.is_empty() && b.contains('.') && !b.ends_with('.'))
}

/// An IPv4 address (with or without `:port`) or an IPv6 address.
fn is_ip(w: &str) -> bool {
    let w = w.trim_start_matches('[');
    let colons = w.matches(':').count();
    let hex = !w.is_empty() && w.chars().all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.');
    // `fe80::1`, `::1`, a full address. Letters only (`Add::add`) or a time (`14:03:07`) don't count.
    if hex && colons >= 2 && w.chars().any(|c| c.is_ascii_digit()) && (w.contains("::") || colons >= 7) {
        return true;
    }
    let host = w.split(':').next().unwrap_or("");
    let parts: Vec<&str> = host.split('.').collect();
    parts.len() == 4 && parts.iter().all(|p| !p.is_empty() && p.len() <= 3 && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Replaces the name of any clip-like file (`My Game 2026.mp4`) with `<file>`, keeping the
/// extension. The name starts after the last `\`, `/` or quote before it, else after the last
/// space-separated word.
fn scrub_files(line: &str, extensions: &[String]) -> String {
    let lower = line.to_ascii_lowercase();
    let (mut out, mut at) = (String::new(), 0);
    let bytes = lower.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'.'
            && let Some(ext) =
                extensions.iter().filter(|e| lower[i + 1..].starts_with(e.as_str())).max_by_key(|e| e.len())
            && bytes.get(i + 1 + ext.len()).is_none_or(|b| !b.is_ascii_alphanumeric())
        {
            let before = &lower[at..i];
            let start = at + before.rfind(['\\', '/', '"', '\'']).or_else(|| before.rfind(' ')).map_or(0, |p| p + 1);
            if start < i {
                out.push_str(&line[at..start]);
                out.push_str("<file>");
                at = i;
            }
            i += 1 + ext.len();
            continue;
        }
        i += 1;
    }
    out.push_str(&line[at..]);
    out
}

/// Windows calls, kept small.
mod platform {
    use std::{fmt::Write, sync::OnceLock};
    use windows::{
        Win32::{
            Storage::FileSystem::GetShortPathNameW,
            System::{
                Diagnostics::Debug::{EXCEPTION_POINTERS, SetUnhandledExceptionFilter},
                Registry::{HKEY, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW},
                SystemInformation::{
                    COMPUTER_NAME_FORMAT, ComputerNameDnsFullyQualified, ComputerNameDnsHostname, GetComputerNameExW,
                    GetLocalTime,
                },
            },
        },
        core::{PCWSTR, PWSTR, w},
    };

    pub fn local_time() -> String {
        // SAFETY: no arguments; returns a SYSTEMTIME by value.
        let t = unsafe { GetLocalTime() };
        format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
    }

    fn reg(name: PCWSTR) -> Option<String> {
        let mut text = [0u16; 128];
        let mut bytes = size_of_val(&text) as u32;
        let key: HKEY = HKEY_LOCAL_MACHINE;
        // SAFETY: `text` is writable for `bytes` bytes; both outlive the call.
        let e = unsafe {
            RegGetValueW(
                key,
                w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"),
                name,
                RRF_RT_REG_SZ,
                None,
                Some(text.as_mut_ptr().cast()),
                Some(&mut bytes),
            )
        };
        e.ok().ok()?;
        Some(String::from_utf16_lossy(&text[..bytes as usize / 2]).trim_end_matches('\0').to_owned())
    }

    /// For example `Windows 11 Home 25H2 (build 26200)`; gaps are left out. Windows 11 still has
    /// "Windows 10" in `ProductName`, so the build number decides (22000 and up is Windows 11).
    pub fn windows_version() -> String {
        let build = reg(w!("CurrentBuildNumber"));
        let mut s = reg(w!("ProductName")).unwrap_or_else(|| "Windows".into());
        if build.as_deref().and_then(|b| b.parse::<u32>().ok()).is_some_and(|b| b >= 22000) {
            s = s.replace("Windows 10", "Windows 11");
        }
        if let Some(v) = reg(w!("DisplayVersion")) {
            let _ = write!(s, " {v}");
        }
        if let Some(b) = build {
            let _ = write!(s, " (build {b})");
        }
        s
    }

    fn computer_name(kind: COMPUTER_NAME_FORMAT) -> Option<String> {
        let mut buf = [0u16; 300];
        let mut len = buf.len() as u32;
        // SAFETY: `buf` is writable for `len` UTF-16 units; both outlive the call.
        unsafe { GetComputerNameExW(kind, Some(PWSTR(buf.as_mut_ptr())), &mut len) }.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }

    /// The PC's full host name and DNS name (`COMPUTERNAME` is cut to 15 characters).
    pub fn pc_names() -> Vec<String> {
        [ComputerNameDnsHostname, ComputerNameDnsFullyQualified].into_iter().filter_map(computer_name).collect()
    }

    /// The 8.3 form of an existing path (`C:\Users\LONGNA~1`).
    pub fn short_path(path: &str) -> Option<String> {
        let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
        let mut buf = [0u16; 520];
        // SAFETY: `wide` is NUL-terminated and `buf` is writable; both outlive the call.
        let n = unsafe { GetShortPathNameW(PCWSTR(wide.as_ptr()), Some(&mut buf)) } as usize;
        (n > 0 && n < buf.len()).then(|| String::from_utf16_lossy(&buf[..n]))
    }

    type Filter = unsafe extern "system" fn(*const EXCEPTION_POINTERS) -> i32;
    static PREVIOUS: OnceLock<Option<Filter>> = OnceLock::new();

    /// Writes a short report for a crash that is not a Rust panic (access violation and the
    /// like), then calls the filter that was there before, or lets Windows carry on as before.
    // shortcut: allocates and takes locks inside the filter, which can fail if the crash was in the allocator; a pre-allocated buffer would be needed then.
    unsafe extern "system" fn on_native_crash(info: *const EXCEPTION_POINTERS) -> i32 {
        // SAFETY: Windows passes a valid pointer (or null) to the filter.
        let record = unsafe { info.as_ref().and_then(|i| i.ExceptionRecord.as_ref()) };
        if let Some(r) = record {
            let thread = std::thread::current();
            super::write_report(
                "native crash",
                &format!(
                    "thread: {}\nexception code: {:#010x}\naddress: {:#x}",
                    thread.name().unwrap_or("unnamed"),
                    r.ExceptionCode.0,
                    r.ExceptionAddress as usize
                ),
            );
        }
        match PREVIOUS.get().copied().flatten() {
            // SAFETY: the pointer came from SetUnhandledExceptionFilter and is passed on unchanged.
            Some(previous) => unsafe { previous(info) },
            None => 0, // EXCEPTION_CONTINUE_SEARCH: Windows handles it as before
        }
    }

    pub fn install_native_filter() {
        // SAFETY: registers a function that stays valid for the whole process.
        let previous = unsafe { SetUnhandledExceptionFilter(Some(on_native_crash)) };
        let _ = PREVIOUS.set(previous);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exts() -> Vec<String> {
        ["mp4", "mkv", "ts", "flv", "webp", "jxr", "png", "part"].map(String::from).to_vec()
    }

    fn rules(profile: &str, user: &str, pcs: &[&str], folders: &[(&str, &'static str)]) -> Rules {
        let pcs: Vec<String> = pcs.iter().map(|s| s.to_string()).collect();
        let folders: Vec<_> = folders.iter().map(|(p, l)| (p.to_string(), *l)).collect();
        Rules::new(profile, user, &pcs, &folders, exts(), &|p| {
            (p == r"C:\Users\Morgan Longname").then(|| r"C:\Users\MORGAN~1".to_owned())
        })
    }

    fn plain() -> Rules {
        rules(r"C:\Users\Avery", "Avery", &["PC1"], &[])
    }

    #[test]
    fn profile_and_user_name_become_user() {
        let s = scrub(r"open C:\Users\Avery\AppData\Roaming\x and c:/users/avery/y; owner Avery.", &plain());
        assert!(!s.to_lowercase().contains("avery"), "{s}");
        assert!(s.contains(r"<user>\AppData\Roaming\x"));
        assert!(s.contains("<user>/y"));
    }

    #[test]
    fn a_log_line_is_scrubbed_and_keeps_its_line_break() {
        let s = scrub_keeping_end("INFO saved C:\\Users\\Avery\\Videos\\my game.mp4 on PC1\n", &plain());
        assert!(s.ends_with('\n') && !s.contains("Avery") && !s.contains("PC1") && !s.contains("my game"), "{s}");
    }

    #[test]
    fn user_name_inside_a_word_stays() {
        assert_eq!(scrub("Averycat Avery", &plain()), "Averycat <user>");
    }

    #[test]
    fn every_spelling_of_the_profile_goes() {
        let r = rules(r"C:\Users\Morgan Longname", "Morgan Longname", &["PC1"], &[]);
        for text in [
            r"C:\Users\Morgan Longname\x",
            r"C:\\Users\\Morgan Longname\\x", // Rust's {:?}
            "C:/Users/Morgan Longname/x",
            "file:///C:/Users/Morgan%20Longname/x",
            r"C:\Users\MORGAN~1\x", // 8.3
            r"c:\users\morgan longname\x",
        ] {
            let s = scrub(text, &r);
            assert!(s.contains("<user>"), "{text} -> {s}");
            assert!(!s.to_lowercase().contains("morgan"), "{text} -> {s}");
        }
    }

    #[test]
    fn short_and_non_ascii_user_names() {
        let r = rules(r"C:\Users\Al", "Al", &[], &[]);
        assert_eq!(scrub(r"C:\Users\Al\x and Al", &r), r"<user>\x and <user>");
        let r = rules(r"C:\Users\Ünal", "Ünal", &[], &[]);
        assert_eq!(scrub(r"c:\users\ünal\x and ÜNAL.", &r), r"<user>\x and <user>.");
    }

    #[test]
    fn profile_folder_name_differing_from_the_user_name_goes() {
        let r = rules(r"C:\Users\avery.DESKTOP-1", "avery", &[], &[]);
        assert_eq!(scrub("see avery.DESKTOP-1 here", &r), "see <user> here");
    }

    #[test]
    fn user_named_like_a_placeholder_does_not_nest() {
        let r = rules(r"C:\Users\User", "User", &[], &[]);
        assert_eq!(scrub(r"C:\Users\User\x", &r), r"<user>\x");
    }

    #[test]
    fn pc_names_short_and_full_and_unc_hosts() {
        let r = rules(r"C:\Users\Avery", "Avery", &["DESKTOP-ABCDEFG", "DESKTOP-ABCDEFGHIJ"], &[]);
        assert_eq!(scrub("DESKTOP-ABCDEFGHIJ and DESKTOP-ABCDEFG", &r), "<pc> and <pc>");
        assert_eq!(scrub(r"open \\NAS01\share\a and \\?\C:\x", &r), r"open \\<host>\share\a and \\?\C:\x");
        assert_eq!(scrub(r#"path "\\\\NAS01\\share""#, &r), r#"path "\\\\<host>\\share""#);
    }

    #[test]
    fn email_and_ip_forms() {
        let r = plain();
        assert_eq!(scrub("mail me@example.com from 192.168.1.20:5000 ok", &r), "mail <email> from <ip> ok");
        assert_eq!(scrub("write a@b.com. Then 10.0.0.5.", &r), "write <email>. Then <ip>.");
        assert_eq!(scrub("ip=1.2.3.4 peer=[fe80::1]:80 lo ::1", &r), "ip=<ip> peer=[<ip>]:80 lo <ip>");
        assert_eq!(scrub("user=me@x.org, addr:10.1.1.1;", &r), "user=<email>, addr:<ip>;");
    }

    #[test]
    fn versions_times_and_paths_are_not_addresses() {
        let r = plain();
        let text = "ffmpeg 8.1.0 build 26200.1 at 14:03:07 std::io::Add::add";
        assert_eq!(scrub(text, &r), text);
    }

    #[test]
    fn clip_and_screenshot_folders_are_hidden() {
        let r = rules(
            r"C:\Users\Avery",
            "Avery",
            &[],
            &[
                (r"D:\Riley clips", "<clips folder>"),
                (r"C:\Users\Avery\OneDrive - Employer\Pictures", "<screenshots folder>"),
            ],
        );
        let s = scrub(r"saved D:\Riley clips\run 1\a.mp4 and C:\Users\Avery\OneDrive - Employer\Pictures\x.png", &r);
        assert_eq!(s, r"saved <clips folder>\run 1\<file>.mp4 and <screenshots folder>\<file>.png");
        let s = scrub(r#"{"D:\\Riley clips"}"#, &r);
        assert!(!s.contains("Riley"), "{s}");
    }

    #[test]
    fn clip_file_names_go_for_every_catalog_extension() {
        let r = plain();
        let s = scrub(r"saved D:\Clips\Elden Ring 2026-11-08 14.03.07.mp4 and 'x'", &r);
        assert_eq!(s, r"saved D:\Clips\<file>.mp4 and 'x'");
        let s = scrub(r#"failed "C:\Users\Avery\Videos\Backsight\boss fight.MKV": no space"#, &r);
        assert_eq!(s, r#"failed "<user>\Videos\Backsight\<file>.MKV": no space"#);
        for ext in ["ts", "flv", "webp", "jxr", "png"] {
            let s = scrub(&format!(r"D:\a\my run.{ext} done"), &r);
            assert_eq!(s, format!(r"D:\a\<file>.{ext} done"), "{ext}");
        }
        // Not a media file, or an extension that only starts like one.
        assert_eq!(scrub("see settings.toml and a.mp4x and std::ts", &r), "see settings.toml and a.mp4x and std::ts");
    }

    #[test]
    fn extensions_come_from_the_catalog() {
        let c = Catalog::get();
        for ext in ["ts", "flv", "webp", "jxr"] {
            assert!(
                c.containers.iter().any(|x| x.ext == ext) || c.screenshot_formats.iter().any(|x| x.ext == ext),
                "{ext}"
            );
        }
    }

    #[test]
    fn build_label_drops_the_commit_subject() {
        assert_eq!(
            build_label("2026-11-12 14:03, abc1234 fix: my private note"),
            "built 2026-11-12 14:03, commit abc1234"
        );
        assert_eq!(
            build_label("2026-11-12 14:03, abc1234 x (plus uncommitted changes)"),
            "built 2026-11-12 14:03, commit abc1234, plus uncommitted changes"
        );
    }

    #[test]
    fn the_version_line_names_the_build_or_says_development() {
        let v = env!("CARGO_PKG_VERSION");
        assert_eq!(version_line(""), format!("{v} (development build)"));
        assert_eq!(
            version_line(
                "2026-11-12 14:03, abc1234 x
"
            ),
            format!("{v} (built 2026-11-12 14:03, commit abc1234)")
        );
    }

    #[test]
    fn last_lines_keeps_the_tail() {
        assert_eq!(last_lines("a\nb\nc\nd", 2), "c\nd");
        assert_eq!(last_lines("a", 5), "a");
    }

    #[test]
    fn read_tail_starts_at_a_whole_line() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("log");
        fs::write(&p, "first line\nsecond line\nthird line\n").unwrap();
        assert_eq!(read_tail(&p, 1000), "first line\nsecond line\nthird line\n");
        assert_eq!(read_tail(&p, 20), "third line\n");
        assert_eq!(read_tail(&dir.path().join("missing"), 20), "");
    }

    #[test]
    fn keeps_the_newest_tracks_dismissal_and_ignores_part_files() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..25 {
            fs::write(dir.path().join(format!("crash-2026-11-{:02}.txt", i + 1)), "x").unwrap();
        }
        fs::write(dir.path().join("seen.txt"), "").unwrap();
        fs::write(dir.path().join("crash-2026-12-31.txt.part"), "half").unwrap();
        assert_eq!(list(dir.path()).len(), 25); // the .part isn't a report
        prune(dir.path(), KEEP);
        let files = list(dir.path());
        assert_eq!(files.len(), 20);
        assert!(!dir.path().join("crash-2026-12-31.txt.part").exists());
        assert!(files[0].ends_with("crash-2026-11-06.txt"));
        assert_eq!(pending(dir.path()).as_deref(), Some("crash-2026-11-25.txt"));
        dismiss(dir.path());
        assert_eq!(pending(dir.path()), None);
        fs::write(dir.path().join("crash-2026-12-01.txt"), "x").unwrap();
        assert_eq!(pending(dir.path()).as_deref(), Some("crash-2026-12-01.txt"));
    }
}
