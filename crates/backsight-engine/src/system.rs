//! Small Windows calls: installed memory, the local time, the clipboard, playing a sound and
//! background priority.

use std::{os::windows::ffi::OsStrExt, path::Path, time::Duration};
use windows::{
    Win32::{
        Foundation::{GlobalFree, HANDLE},
        Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT},
        System::{
            Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoTaskMemFree, CoUninitialize},
            DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
            SystemInformation::{GetLocalTime, GlobalMemoryStatusEx, MEMORYSTATUSEX},
            Threading::{
                GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN, THREAD_MODE_BACKGROUND_END,
            },
        },
        UI::{
            Shell::{SHOpenFolderAndSelectItems, SHParseDisplayName, ShellExecuteW},
            WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, MB_ICONINFORMATION, MB_OK, MessageBoxW, SW_SHOWNORMAL,
                WS_EX_NOACTIVATE, WS_POPUP,
            },
        },
    },
    core::{PCWSTR, w},
};

/// While alive, the calling thread runs in Windows' background mode: low CPU, disk and memory
/// priority, so slow work after a save never takes time from a game.
pub struct Background(std::marker::PhantomData<*const ()>);

pub fn background_mode() -> Background {
    // SAFETY: GetCurrentThread's pseudo-handle is always valid for the calling thread.
    if let Err(e) = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) } {
        tracing::warn!("background priority not set: {e}");
    }
    Background(std::marker::PhantomData)
}

impl Drop for Background {
    fn drop(&mut self) {
        // SAFETY: as above, on the same thread (`Background` is not `Send`).
        let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_END) };
    }
}

/// Starts playing a WAV file held in memory and returns at once. Windows reads `wav` while it
/// plays, hence `'static`; a newer sound stops the one playing.
pub fn play_wav(wav: &'static [u8]) {
    // SAFETY: with SND_MEMORY the "name" is the WAV image itself, which outlives the playback.
    let ok = unsafe { PlaySoundW(PCWSTR(wav.as_ptr().cast()), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT) };
    if !ok.as_bool() {
        tracing::debug!("a sound didn't play (no output device?)");
    }
}

/// Installed physical memory in bytes, or `None` if Windows won't say.
pub fn physical_memory() -> Option<u64> {
    let mut m = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    // SAFETY: `m` is a valid MEMORYSTATUSEX with `dwLength` set, as the call requires.
    unsafe { GlobalMemoryStatusEx(&mut m) }.ok().map(|()| m.ullTotalPhys)
}

/// The local time as `2026-11-08 14.03.07`, for file names (they can't contain `:`).
pub fn local_time_stamp() -> String {
    // SAFETY: no arguments; returns a SYSTEMTIME by value.
    let t = unsafe { GetLocalTime() };
    format!("{:04}-{:02}-{:02} {:02}.{:02}.{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

/// Puts a CF_DIB image on the clipboard.
// shortcut: if EmptyClipboard succeeds but SetClipboardData fails (out of memory), the old clipboard is gone; use delayed rendering if this is ever reported.
pub fn copy_dib(data: &[u8]) -> windows::core::Result<()> {
    copy_data(8, data)
}

pub fn copy_path(path: &Path) -> windows::core::Result<()> {
    let text: Vec<_> = path.as_os_str().encode_wide().chain([0]).flat_map(u16::to_le_bytes).collect();
    copy_data(13, &text)
}

/// Open a file with its Windows association, or select it in File Explorer.
/// Call on a blocking worker.
pub fn open_path(path: &Path, reveal: bool) -> windows::core::Result<()> {
    let path: Vec<_> = dunce::simplified(path).as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: COM is initialized and released on this thread, and the path remains alive and
    // NUL-terminated for both shell calls. The parsed item is freed after use.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).ok()?;
        let result = if reveal {
            let mut item = std::ptr::null_mut();
            SHParseDisplayName(PCWSTR(path.as_ptr()), None, &mut item, 0, None).and_then(|()| {
                let result = SHOpenFolderAndSelectItems(item, None, 0);
                CoTaskMemFree(Some(item.cast()));
                result
            })
        } else {
            let code =
                ShellExecuteW(None, w!("open"), PCWSTR(path.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
            if code.0 as isize > 32 {
                Ok(())
            } else {
                Err(windows::core::Error::new(
                    windows::Win32::Foundation::E_FAIL,
                    format!("Windows shell error {}", code.0 as isize),
                ))
            }
        };
        CoUninitialize();
        result
    }
}

/// Moves files to the Recycle Bin. Windows asks first if one can't go there (a drive without a
/// Recycle Bin) and would be deleted for good. Call on a blocking worker.
pub fn recycle(paths: &[std::path::PathBuf]) -> Result<(), String> {
    use windows::Win32::UI::Shell::{
        FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_SILENT, FOF_WANTNUKEWARNING, SHFILEOPSTRUCTW,
        SHFileOperationW,
    };
    if paths.is_empty() {
        return Ok(());
    }
    // A list of NUL-terminated paths, ended by one more NUL.
    let mut from: Vec<u16> =
        paths.iter().flat_map(|p| dunce::simplified(p).as_os_str().encode_wide().chain([0])).collect();
    from.push(0);
    let mut op = SHFILEOPSTRUCTW {
        hwnd: Default::default(),
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        pTo: PCWSTR::null(),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_WANTNUKEWARNING | FOF_SILENT).0 as u16,
        fAnyOperationsAborted: false.into(),
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: PCWSTR::null(),
    };
    // SAFETY: `from` is double-NUL-terminated and outlives the call; `op` is fully initialized.
    let code = unsafe { SHFileOperationW(&mut op) };
    match (code, op.fAnyOperationsAborted.as_bool()) {
        (0, false) => Ok(()),
        (0, true) => Err("Nothing more was removed: the removal was cancelled.".into()),
        (code, _) => Err(format!("Windows shell error {code:#x}")),
    }
}

/// A plain message box, for the start modes that have no window of their own (`--report`).
/// Returns when the user closes it.
pub fn message_box(title: &str, text: &str) {
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (title, text) = (wide(title), wide(text));
    // SAFETY: both strings are NUL-terminated and outlive the call; no owner window.
    unsafe { MessageBoxW(None, PCWSTR(text.as_ptr()), PCWSTR(title.as_ptr()), MB_OK | MB_ICONINFORMATION) };
}

fn copy_data(format: u32, data: &[u8]) -> windows::core::Result<()> {
    // SAFETY: movable global memory is filled before ownership transfers to Windows.
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, data.len())?;
        let ptr = GlobalLock(memory);
        if ptr.is_null() {
            let error = windows::core::Error::from_thread();
            let _ = GlobalFree(Some(memory));
            return Err(error);
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr.cast(), data.len());
        let _ = GlobalUnlock(memory);
        // EmptyClipboard needs a real owner even though the settings window is disposable.
        let owner = match CreateWindowExW(
            WS_EX_NOACTIVATE,
            w!("STATIC"),
            w!("Backsight clipboard"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            None,
            None,
        ) {
            Ok(w) => w,
            Err(e) => {
                let _ = GlobalFree(Some(memory));
                return Err(e);
            }
        };
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(Some(owner)).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let result = if opened {
            EmptyClipboard().and_then(|()| SetClipboardData(format, Some(HANDLE(memory.0))).map(|_| ()))
        } else {
            Err(windows::core::Error::new(windows::Win32::Foundation::E_FAIL, "Another app is using the clipboard."))
        };
        if opened {
            let _ = CloseClipboard();
        }
        if result.is_err() {
            let _ = GlobalFree(Some(memory));
        }
        let _ = DestroyWindow(owner);
        result
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reports_memory_and_a_file_name_safe_time() {
        assert!(super::physical_memory().unwrap() > 1 << 30);
        let stamp = super::local_time_stamp();
        assert_eq!(stamp.len(), 19, "{stamp}");
        assert!(!stamp.contains(':'), "{stamp}");
    }
}
