//! Holding off Windows shutdown or sign-out while a clip or recording is being saved, so the file
//! isn't cut off mid-write. See platform-notes.md, "Shutdown and sign-out".
//!
//! A hidden top-level window of our own, on its own thread, answers `WM_QUERYENDSESSION` with
//! FALSE and registers a reason with `ShutdownBlockReasonCreate` while any [`Saving`] is alive.
//! Its own window and thread, because the reason can only be set from the thread that created the
//! window, and Tauri's windows come and go (and tao owns their message loop).

use std::{
    sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering::SeqCst},
    },
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::{
            LibraryLoader::GetModuleHandleW,
            Shutdown::{ShutdownBlockReasonCreate, ShutdownBlockReasonDestroy},
        },
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG, PostMessageW, RegisterClassW, WM_APP,
            WM_ENDSESSION, WM_QUERYENDSESSION, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
        },
    },
    core::w,
};

/// What Windows shows on its "apps are preventing shutdown" screen. Short: users glance at it.
const REASON: windows::core::PCWSTR = w!("Backsight is saving your clip or recording.");
/// Sent to the window when [`Saving`]s come or go: set or remove the reason.
const WM_APP_SYNC: u32 = WM_APP + 1;
/// The longest a forced shutdown is held after the user clicks "Shut down anyway" (Windows allows
/// a window with a reason 30 seconds to answer `WM_ENDSESSION`).
const ENDSESSION_WAIT: Duration = Duration::from_secs(25);

static BUSY: AtomicUsize = AtomicUsize::new(0);
/// The window, as an integer (`HWND` isn't `Send`); 0 if it couldn't be made.
static WINDOW: OnceLock<isize> = OnceLock::new();

/// While alive, Windows shutdown and sign-out wait (the user sees why and can still go ahead).
pub struct Saving(());

/// Holds off shutdown until the returned guard is dropped. Never fails: without the window,
/// saving goes on as before.
pub fn saving() -> Saving {
    BUSY.fetch_add(1, SeqCst);
    sync();
    Saving(())
}

impl Drop for Saving {
    fn drop(&mut self) {
        BUSY.fetch_sub(1, SeqCst);
        sync();
    }
}

fn sync() {
    let hwnd = *WINDOW.get_or_init(start);
    if hwnd != 0 {
        // SAFETY: posting to our own window; if it's gone the call fails harmlessly.
        let _ = unsafe { PostMessageW(Some(HWND(hwnd as _)), WM_APP_SYNC, WPARAM(0), LPARAM(0)) };
    }
}

/// Starts the window's thread and returns the window once it exists.
fn start() -> isize {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name("backsight-shutdown".into()).spawn(move || {
        // SAFETY: plain Win32 calls on this thread; the class name and wndproc are 'static.
        let hwnd = unsafe {
            let instance = GetModuleHandleW(None).map(Into::into).unwrap_or_default();
            let class = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: instance,
                lpszClassName: w!("BacksightShutdown"),
                ..Default::default()
            };
            RegisterClassW(&class);
            // Top-level (a message-only window gets no WM_QUERYENDSESSION), never shown.
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class.lpszClassName,
                w!("Backsight"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                None,
            )
        };
        let hwnd = match hwnd {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!("shutdown window not created, saves can be cut off at shutdown: {e}");
                let _ = tx.send(0);
                return;
            }
        };
        let _ = tx.send(hwnd.0 as isize);
        let mut msg = MSG::default();
        // SAFETY: standard message loop on this thread's queue; it runs for the life of the process.
        while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
            // SAFETY: dispatches the message GetMessageW just filled in.
            unsafe { DispatchMessageW(&msg) };
        }
    });
    if let Err(e) = spawned {
        tracing::warn!("shutdown thread not started: {e}");
        return 0;
    }
    rx.recv().unwrap_or(0)
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let busy = BUSY.load(SeqCst) > 0;
    match msg {
        // FALSE holds shutdown; the reason alone doesn't (platform-notes.md).
        WM_QUERYENDSESSION if busy => LRESULT(0),
        // Shutting down anyway: the process may end once this returns, so let the save finish first.
        WM_ENDSESSION if wparam.0 != 0 => {
            let since = Instant::now();
            while BUSY.load(SeqCst) > 0 && since.elapsed() < ENDSESSION_WAIT {
                std::thread::sleep(Duration::from_millis(50));
            }
            LRESULT(0)
        }
        WM_APP_SYNC => {
            // SAFETY: on the thread that created `hwnd`, as both calls require.
            let done = unsafe {
                if busy { ShutdownBlockReasonCreate(hwnd, REASON) } else { ShutdownBlockReasonDestroy(hwnd) }
            };
            if let Err(e) = done {
                tracing::debug!("shutdown block reason (busy: {busy}) not changed: {e}");
            }
            LRESULT(0)
        }
        // SAFETY: called by Windows for our window with valid arguments.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::{System::Shutdown::ShutdownBlockReasonQuery, UI::WindowsAndMessaging::SendMessageW};

    fn reason_set(hwnd: HWND) -> bool {
        let mut len = 0u32;
        // SAFETY: asks only for the length; Windows writes `len`.
        unsafe { ShutdownBlockReasonQuery(hwnd, None, &mut len) }.is_ok()
    }

    fn eventually(f: impl Fn() -> bool) -> bool {
        let since = Instant::now();
        while since.elapsed() < Duration::from_secs(5) {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn holds_shutdown_only_while_saving() {
        let outer = saving();
        let hwnd = HWND(*WINDOW.get().unwrap() as _);
        assert!(!hwnd.is_invalid());
        assert!(eventually(|| reason_set(hwnd)), "reason registered");
        // SAFETY: a synchronous message to our own window, as Windows sends at shutdown.
        let answer = |h| unsafe { SendMessageW(h, WM_QUERYENDSESSION, Some(WPARAM(0)), Some(LPARAM(0))) };
        assert_eq!(answer(hwnd), LRESULT(0), "shutdown waits");
        let inner = saving();
        drop(outer);
        assert_eq!(answer(hwnd), LRESULT(0), "still one save going");
        drop(inner);
        assert!(eventually(|| !reason_set(hwnd)), "reason removed");
        assert_ne!(answer(hwnd), LRESULT(0), "shutdown goes ahead");
    }
}
