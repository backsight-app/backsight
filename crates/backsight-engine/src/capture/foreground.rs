//! The program in front: the focused window, its process and a friendly name for it. Only reads
//! window and process state; nothing is injected or hooked (ADR 0004). The steady-focus logic
//! that decides which program counts as "the game" is pure Rust in `crate::game`.

use super::wgc::is_shell_class;
use std::{
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    sync::Arc,
};
use windows::{
    Win32::{
        Foundation::{HANDLE, HWND, WAIT_TIMEOUT},
        Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromWindow},
        Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW},
        System::Threading::{
            OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            QueryFullProcessImageNameW, WaitForSingleObject,
        },
        UI::WindowsAndMessaging::{
            GetClassNameW, GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
        },
    },
    core::{HSTRING, PCWSTR},
};

/// Windows shell processes whose windows take focus but are never a game.
// shortcut: UWP apps (ApplicationFrameHost.exe owns their window) count as the shell; resolve the
// hosted app's process if users ask for Store games.
const SHELL: [&str; 12] = [
    "explorer.exe",
    "startmenuexperiencehost.exe",
    "shellexperiencehost.exe",
    "shellhost.exe", // newer Windows 11 shell pieces (quick settings, flyouts)
    "searchhost.exe",
    "searchapp.exe",
    "applicationframehost.exe",
    "textinputhost.exe",
    "lockapp.exe",
    "pickerhost.exe",         // the file-open and save boxes of Store apps
    "easeofaccessdialog.exe", // the Ease of Access box (e.g. Sticky Keys after pressing Shift 5 times)
    "backsight.exe",
];

/// Programs that run games written for them (Minecraft runs in Java), whose own name is no help.
const HOSTS: [&str; 4] = ["java.exe", "javaw.exe", "python.exe", "pythonw.exe"];

/// What has focus right now, from cheap window queries only (one small string for the class name).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// An ordinary visible window of another process.
    Window { window: isize, pid: u32 },
    /// The desktop or a taskbar.
    Shell,
}

/// A program that held focus: the game, as far as Backsight can tell.
#[derive(Clone)]
pub struct Program {
    pub pid: u32,
    /// Held so the process can be watched and its PID isn't reused while we know it.
    pub process: Arc<OwnedHandle>,
    /// The file name, e.g. `game.exe`.
    pub exe: String,
    /// What the program calls itself (its file description, product name or window title), raw.
    pub label: String,
    /// The focused top-level window, as a raw `HWND`.
    pub window: isize,
    /// When the process started, in FILETIME ticks; 0 if Windows didn't say.
    pub started: u64,
}

impl Program {
    /// Whether the process `parent` (from [`parent`]) is `self`, started while `self` ran: a game
    /// launched from the launcher (or chat) captured first. The start times rule out a reused
    /// parent PID; a child of a process that exited long ago never matches.
    pub fn started(&self, child_parent: u32, child_started: u64) -> bool {
        child_parent != 0 && child_parent == self.pid && self.started != 0 && child_started >= self.started
    }
}

/// The time now in FILETIME ticks, the clock of [`Program::started`].
pub fn now() -> u64 {
    // SAFETY: no arguments; always succeeds.
    let t = unsafe { windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime() };
    u64::from(t.dwHighDateTime) << 32 | u64::from(t.dwLowDateTime)
}

/// The parent PID and file name of `pid`, from a process snapshot (a few ms: only for the game
/// watcher's take-over decision, never per frame or before a screenshot).
pub fn parent(pid: u32) -> Option<(u32, String)> {
    use windows::Win32::{
        Foundation::CloseHandle,
        System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
        },
    };
    // SAFETY: the snapshot handle is closed below; `entry.dwSize` is set as the calls require.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut entries = Vec::new();
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        while more {
            entries.push((entry.th32ProcessID, entry.th32ParentProcessID, entry.szExeFile));
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
        let ppid = entries.iter().find(|e| e.0 == pid)?.1;
        let name = entries.iter().find(|e| e.0 == ppid).map(|e| {
            let len = e.2.iter().position(|&c| c == 0).unwrap_or(e.2.len());
            String::from_utf16_lossy(&e.2[..len])
        });
        Some((ppid, name.unwrap_or_default()))
    }
}

/// When `process` started, in FILETIME ticks.
fn start_time(process: &OwnedHandle) -> Option<u64> {
    use windows::Win32::{Foundation::FILETIME, System::Threading::GetProcessTimes};
    let mut t = [FILETIME::default(); 4];
    let [created, exited, kernel, user] = &mut t;
    // SAFETY: the handle has PROCESS_QUERY_LIMITED_INFORMATION, which GetProcessTimes accepts;
    // the four out-pointers are valid locals.
    unsafe { GetProcessTimes(HANDLE(process.as_raw_handle()), created, exited, kernel, user) }.ok()?;
    Some(u64::from(t[0].dwHighDateTime) << 32 | u64::from(t[0].dwLowDateTime))
}

/// The focused window, or `None` when focus says nothing about the game: no focus (a switch in
/// progress), an invisible or minimized window, a window on another monitor than `monitor` (a
/// raw `HMONITOR`; `None` means any), or one of Backsight's own.
pub fn focus(monitor: Option<isize>) -> Option<Focus> {
    let mut class = [0u16; 64];
    let mut pid = 0;
    // SAFETY: these queries only read window state and write to valid local buffers; a window
    // closing meanwhile only makes them fail.
    unsafe {
        let window = GetForegroundWindow();
        if window.is_invalid() || !IsWindowVisible(window).as_bool() || IsIconic(window).as_bool() {
            return None;
        }
        if monitor.is_some_and(|m| MonitorFromWindow(window, MONITOR_DEFAULTTONULL).0 as isize != m) {
            return None;
        }
        let len = GetClassNameW(window, &mut class);
        if len > 0 && is_shell_class(&String::from_utf16_lossy(&class[..len as usize])) {
            return Some(Focus::Shell);
        }
        GetWindowThreadProcessId(window, Some(&mut pid));
        (pid != 0 && pid != std::process::id()).then_some(Focus::Window { window: window.0 as isize, pid })
    }
}

/// The program behind a focused window, or `None` for the Windows shell (Explorer, the Start
/// menu…) or a process that is gone or won't tell its file.
pub fn program(window: isize, pid: u32) -> Option<Program> {
    // SAFETY: OpenProcess returns an owned handle on success, closed when the OwnedHandle drops.
    let access = PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION;
    let process = unsafe { OwnedHandle::from_raw_handle(OpenProcess(access, false, pid).ok()?.0) };
    let mut path = [0u16; 1024];
    let mut len = path.len() as u32;
    // SAFETY: the handle has PROCESS_QUERY_LIMITED_INFORMATION; `path` and `len` are valid locals.
    unsafe {
        QueryFullProcessImageNameW(
            HANDLE(process.as_raw_handle()),
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(path.as_mut_ptr()),
            &mut len,
        )
        .ok()?;
    }
    let path = String::from_utf16_lossy(&path[..len as usize]);
    let file = std::path::Path::new(&path);
    let exe = file.file_name()?.to_string_lossy().into_owned();
    if SHELL.iter().any(|s| exe.eq_ignore_ascii_case(s)) {
        return None;
    }
    let mut confirmed = 0;
    // SAFETY: the PID output is a valid local; a window gone meanwhile simply fails the query.
    unsafe { GetWindowThreadProcessId(HWND(window as _), Some(&mut confirmed)) };
    if confirmed != pid || !alive(&process) {
        return None;
    }
    let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    // A host runs games it says nothing about ("Java(TM) Platform SE binary"): only its window
    // title names the game. Elsewhere the title is never used: it can carry an FPS counter, a
    // level, a document or a chat name.
    let label = if HOSTS.iter().any(|h| exe.eq_ignore_ascii_case(h)) {
        title(window)
    } else {
        [version_string(&path, "FileDescription"), version_string(&path, "ProductName")]
            .into_iter()
            .flatten()
            .find(|s| !s.trim().is_empty())
    }
    .unwrap_or(stem);
    let started = start_time(&process).unwrap_or(0);
    Some(Program { pid, process: Arc::new(process), exe, label, window, started })
}

/// The program in front right now, on any monitor: for screenshots, where the moment counts.
pub fn program_now() -> Option<Program> {
    match focus(None)? {
        Focus::Window { window, pid } => program(window, pid),
        Focus::Shell => None,
    }
}

impl Program {
    /// Whether its window is still open and still this program's (a closed window's handle can be
    /// reused by another program).
    pub fn window_open(&self) -> bool {
        let mut pid = 0;
        // SAFETY: the PID output is a valid local; a stale handle simply fails the query.
        unsafe { GetWindowThreadProcessId(HWND(self.window as _), Some(&mut pid)) };
        pid == self.pid && alive(&self.process)
    }
}

/// Whether the process is still running.
pub fn alive(process: &OwnedHandle) -> bool {
    // SAFETY: the handle stays open through this nonblocking wait and has SYNCHRONIZE access.
    unsafe { WaitForSingleObject(HANDLE(process.as_raw_handle()), 0) == WAIT_TIMEOUT }
}

fn title(window: isize) -> Option<String> {
    let mut text = [0u16; 256];
    // SAFETY: `text` is a valid local buffer; a stale handle returns 0.
    let len = unsafe { GetWindowTextW(HWND(window as _), &mut text) };
    (len > 0).then(|| String::from_utf16_lossy(&text[..len as usize]))
}

/// One string from the file's version resource (`FileDescription`, `ProductName`), in the first
/// language it lists.
fn version_string(path: &str, key: &str) -> Option<String> {
    let path = HSTRING::from(path);
    // SAFETY: `path` is a valid null-terminated string for both calls; `data` has the size the
    // first call asked for, and VerQueryValueW returns pointers into `data`, read while it lives.
    unsafe {
        let size = GetFileVersionInfoSizeW(&path, None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(&path, None, size, data.as_mut_ptr().cast()).ok()?;
        let (mut ptr, mut len) = (std::ptr::null_mut(), 0u32);
        let lang =
            if VerQueryValueW(data.as_ptr().cast(), &HSTRING::from(r"\VarFileInfo\Translation"), &mut ptr, &mut len)
                .as_bool()
                && len >= 4
            {
                let pair = ptr as *const u16;
                format!("{:04x}{:04x}", *pair, *pair.add(1))
            } else {
                "040904b0".into() // US English, Unicode: what most files use
            };
        let sub = HSTRING::from(format!(r"\StringFileInfo\{lang}\{key}"));
        if !VerQueryValueW(data.as_ptr().cast(), PCWSTR(sub.as_ptr()), &mut ptr, &mut len).as_bool() || len == 0 {
            return None;
        }
        // `len` counts characters, the terminating null included.
        let text = std::slice::from_raw_parts(ptr as *const u16, len as usize);
        let end = text.iter().position(|&c| c == 0).unwrap_or(text.len());
        Some(String::from_utf16_lossy(&text[..end]))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn notepad_describes_itself() {
        let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
        let label = super::version_string(&format!(r"{windir}\System32\notepad.exe"), "FileDescription");
        assert!(label.is_some_and(|l| l.contains("Notepad")));
    }

    #[test]
    fn only_a_program_the_captured_one_started_takes_over() {
        use super::Program;
        use std::{os::windows::io::AsHandle, sync::Arc};
        // Kept running: a process that exited is no longer in the snapshot.
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/C", "pause"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let handle = Arc::new(child.as_handle().try_clone_to_owned().unwrap());
        let prog = |pid, started| Program {
            pid,
            process: handle.clone(),
            exe: String::new(),
            label: String::new(),
            window: 0,
            started,
        };
        let launcher = prog(10, 100);
        assert!(launcher.started(10, 150), "the game the launcher started");
        assert!(!launcher.started(30, 150), "a browser opened mid-game");
        assert!(!launcher.started(10, 50), "older than the launcher: a reused parent PID");
        assert!(!launcher.started(0, 150), "parent unknown");
        assert!(!prog(10, 0).started(10, 150), "launcher's start time unknown");
        // The real snapshot: this test's child names this process (and its file) as its parent.
        let (ppid, name) = super::parent(child.id()).unwrap();
        assert_eq!(ppid, std::process::id());
        assert!(name.to_ascii_lowercase().ends_with(".exe"), "{name}");
        child.kill().unwrap();
    }
}
