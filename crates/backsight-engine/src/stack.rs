//! Another thread's call stack in this process, as `module+offset` lines, for "stopped responding"
//! reports. Own process only: the thread is paused for a moment, its stack is unwound with the
//! unwind tables every x64 module carries, and it carries on. Nothing is injected anywhere.
//!
//! Decode an offset with the build's PDB (`backsight.pdb` next to the build), for example in
//! WinDbg: `ln backsight+0x1234`.

use std::fmt::Write;

/// A thread whose stack can be read later; take it on that thread with [`Watched::this_thread`].
#[derive(Clone, Copy, Debug)]
pub struct Watched {
    id: u32,
    /// The thread's stack range: unwinding never reads outside it.
    low: usize,
    high: usize,
}

/// The most frames read; a frozen main thread is a few dozen deep.
const MAX_FRAMES: usize = 64;

impl Watched {
    /// The calling thread.
    pub fn this_thread() -> Self {
        let (id, low, high) = platform::this_thread();
        Self { id, low, high }
    }

    /// The thread's return addresses, innermost first. Pauses the thread while reading.
    pub fn capture(&self) -> Result<Vec<usize>, String> {
        if self.id == platform::this_thread().0 {
            return Err("a thread can't read its own stack this way".into());
        }
        platform::capture(self)
    }
}

/// One line per address: `backsight.exe+0x1a2b3`, or the bare address outside any module.
pub fn describe(addresses: &[usize]) -> String {
    let mut out = String::new();
    for (i, &a) in addresses.iter().enumerate() {
        let _ = match platform::module_of(a) {
            Some((name, base)) => writeln!(out, "{i:>2} {}", module_line(&name, a - base)),
            None => writeln!(out, "{i:>2} {a:#x}"),
        };
    }
    out
}

/// `name+0xoffset`, the form WinDbg's `ln` takes (without `.exe`/`.dll` there).
fn module_line(name: &str, offset: usize) -> String {
    format!("{name}+{offset:#x}")
}

#[cfg(target_arch = "x86_64")]
mod platform {
    use super::{MAX_FRAMES, Watched};
    use windows::{
        Win32::{
            Foundation::{CloseHandle, HANDLE, HMODULE},
            System::{
                Diagnostics::Debug::{
                    CONTEXT, CONTEXT_CONTROL_AMD64, CONTEXT_INTEGER_AMD64, GetThreadContext, RtlLookupFunctionEntry,
                    RtlVirtualUnwind, UNW_FLAG_NHANDLER,
                },
                LibraryLoader::{
                    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                    GetModuleFileNameW, GetModuleHandleExW,
                },
                Threading::{
                    GetCurrentThreadId, GetCurrentThreadStackLimits, OpenThread, ResumeThread, SuspendThread,
                    THREAD_GET_CONTEXT, THREAD_SUSPEND_RESUME,
                },
            },
        },
        core::PCWSTR,
    };

    pub fn this_thread() -> (u32, usize, usize) {
        let (mut low, mut high) = (0usize, 0usize);
        // SAFETY: both pointers are valid for writes for the duration of the call.
        unsafe { GetCurrentThreadStackLimits(&mut low, &mut high) };
        // SAFETY: no arguments.
        (unsafe { GetCurrentThreadId() }, low, high)
    }

    /// Closes the thread handle however `capture` returns.
    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: the handle came from OpenThread and is closed once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    // shortcut: unwinds while the thread is paused, and RtlLookupFunctionEntry can take a loader lock
    // the paused thread might hold (it is very rarely inside the loader); copying the stack first,
    // as Chromium's sampler does, would remove that if a report ever goes missing.
    pub fn capture(w: &Watched) -> Result<Vec<usize>, String> {
        // SAFETY: plain handle request for a thread of this process; the result is checked.
        let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT, false, w.id) }
            .map_err(|e| format!("couldn't open the thread: {e}"))?;
        let thread = Handle(thread);
        // Allocated before the pause: the paused thread may hold the heap lock.
        let mut frames = Vec::with_capacity(MAX_FRAMES);
        // SAFETY: the handle has THREAD_SUSPEND_RESUME; every successful pause is resumed below.
        if unsafe { SuspendThread(thread.0) } == u32::MAX {
            return Err("couldn't pause the thread".into());
        }
        let walked = walk(&thread, w, &mut frames);
        // SAFETY: undoes the SuspendThread above on the same handle.
        unsafe { ResumeThread(thread.0) };
        walked.map(|()| frames).map_err(String::from)
    }

    /// Runs while the thread is paused: no allocation, no locks of ours.
    fn walk(thread: &Handle, w: &Watched, frames: &mut Vec<usize>) -> Result<(), &'static str> {
        let mut ctx = CONTEXT { ContextFlags: CONTEXT_CONTROL_AMD64 | CONTEXT_INTEGER_AMD64, ..Default::default() };
        // SAFETY: `ctx` is a 16-byte-aligned CONTEXT (the type's own alignment) with its flags
        // set; GetThreadContext also waits for the pause to take effect.
        unsafe { GetThreadContext(thread.0, &mut ctx) }.map_err(|_| "couldn't read the thread's registers")?;
        let on_stack = |sp: u64| (w.low as u64..w.high as u64).contains(&sp);
        while frames.len() < MAX_FRAMES && ctx.Rip != 0 && on_stack(ctx.Rsp) {
            frames.push(ctx.Rip as usize);
            let before = ctx.Rsp;
            let mut base = 0u64;
            // SAFETY: looks up a static table; `base` is writable.
            let entry = unsafe { RtlLookupFunctionEntry(ctx.Rip, &mut base, None) };
            if entry.is_null() {
                // A leaf function: the return address is on top of the stack, which was checked
                // to be inside the paused thread's stack range.
                // SAFETY: `Rsp` is inside the thread's stack, which stays mapped while it's paused.
                ctx.Rip = unsafe { (ctx.Rsp as *const u64).read_unaligned() };
                ctx.Rsp += 8;
            } else {
                let (mut handler, mut frame) = (std::ptr::null_mut(), 0u64);
                // SAFETY: `entry` belongs to the module at `base` and to `Rip`; RtlVirtualUnwind only
                // reads the paused thread's stack (inside its range) and updates `ctx`.
                unsafe {
                    RtlVirtualUnwind(UNW_FLAG_NHANDLER, base, ctx.Rip, entry, &mut ctx, &mut handler, &mut frame, None)
                };
            }
            if ctx.Rsp <= before {
                break; // a broken frame would loop
            }
        }
        Ok(())
    }

    /// The file name and load address of the module holding `address`.
    pub fn module_of(address: usize) -> Option<(String, usize)> {
        let mut module = HMODULE::default();
        let flags = GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
        // SAFETY: with FROM_ADDRESS the "name" is any address in the process; `module` is writable.
        unsafe { GetModuleHandleExW(flags, PCWSTR(address as *const u16), &mut module) }.ok()?;
        let mut buf = [0u16; 520];
        // SAFETY: `buf` is writable; the module is loaded (the address is in it).
        let n = unsafe { GetModuleFileNameW(Some(module), &mut buf) } as usize;
        let path = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
        // Only the file name: the folder can hold the user's name.
        let name = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_owned();
        Some((name, module.0 as usize))
    }
}

#[cfg(not(target_arch = "x86_64"))]
mod platform {
    use super::Watched;

    pub fn this_thread() -> (u32, usize, usize) {
        (0, 0, 0)
    }

    pub fn capture(_: &Watched) -> Result<Vec<usize>, String> {
        Err("reading another thread's stack is only built for 64-bit Intel and AMD Windows".into())
    }

    pub fn module_of(_: usize) -> Option<(String, usize)> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn module_lines_read_like_windbg_takes_them() {
        assert_eq!(module_line("backsight.exe", 0x1a2b), "backsight.exe+0x1a2b");
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn reads_a_blocked_threads_stack_down_to_this_program() {
        let (tx, rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let blocked = std::thread::spawn(move || {
            tx.send(Watched::this_thread()).unwrap();
            let _ = done_rx.recv(); // waits in Windows, several calls deep in this program
        });
        let watched = rx.recv().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100)); // let it reach the wait
        let frames = watched.capture().unwrap();
        done_tx.send(()).unwrap();
        blocked.join().unwrap();
        let text = describe(&frames);
        let exe = std::env::current_exe().unwrap();
        let exe = exe.file_name().unwrap().to_string_lossy();
        assert!(frames.len() >= 3, "{text}");
        assert!(text.contains("ntdll.dll+0x"), "the wait itself is in Windows:\n{text}");
        assert!(text.contains(&format!("{exe}+0x")), "the unwind reached this program:\n{text}");
    }

    #[test]
    fn a_thread_cant_read_itself() {
        assert!(Watched::this_thread().capture().is_err());
    }
}
