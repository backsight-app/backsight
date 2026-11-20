//! Global hotkeys through Raw Input on a message-only window: no hooks, no injection, so
//! nothing for anti-cheat to flag (ADR 0007). Gotchas are in `docs/reference/platform-notes.md`.

use crate::hotkeys::{Event, Input, Matcher, Modifier};
use backsight_config::{Action, Hotkey};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering::Relaxed},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::{
            LibraryLoader::GetModuleHandleW,
            RemoteDesktop::{
                NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
            },
            Threading::GetCurrentThreadId,
        },
        UI::{
            Input::{KeyboardAndMouse::*, *},
            WindowsAndMessaging::*,
        },
    },
    core::{Result, w},
};

/// Posted to our own window when the session locks or unlocks (key-ups are lost meanwhile).
const WM_APP_RESET: u32 = WM_APP + 1;

/// The key a scan code names, in `backsight_config::Hotkey`'s canonical names. Scan codes are
/// physical positions, like the UI's `KeyboardEvent.code`, so both agree on every layout.
/// `e0` is the `RI_KEY_E0` flag (right-hand Ctrl/Alt, arrows and the block above them).
pub fn key_from_scan(code: u16, e0: bool) -> Option<Input> {
    use Input::{Key, Modifier as M};
    use Modifier::*;
    const ROW_Q: [&str; 10] = ["Q", "W", "E", "R", "T", "Y", "U", "I", "O", "P"];
    const ROW_A: [&str; 9] = ["A", "S", "D", "F", "G", "H", "J", "K", "L"];
    const ROW_Z: [&str; 7] = ["Z", "X", "C", "V", "B", "N", "M"];
    const DIGITS: [&str; 10] = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"];
    const F1_10: [&str; 10] = ["F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10"];
    const F13_23: [&str; 11] = ["F13", "F14", "F15", "F16", "F17", "F18", "F19", "F20", "F21", "F22", "F23"];
    // Numpad 7 8 9 - 4 5 6 + 1 2 3 0 . (0x47..=0x53); with E0 the same codes are the edit keys.
    const PAD: [&str; 13] = [
        "Numpad7",
        "Numpad8",
        "Numpad9",
        "NumpadSubtract",
        "Numpad4",
        "Numpad5",
        "Numpad6",
        "NumpadAdd",
        "Numpad1",
        "Numpad2",
        "Numpad3",
        "Numpad0",
        "NumpadDecimal",
    ];
    const EDIT: [&str; 13] =
        ["Home", "Up", "PageUp", "", "Left", "", "Right", "", "End", "Down", "PageDown", "Insert", "Delete"];
    let at = |table: &[&'static str], first: u16| table.get((code - first) as usize).copied();
    let key = match (code, e0) {
        (0x1D, false) => return Some(M(LCtrl)),
        (0x1D, true) => return Some(M(RCtrl)),
        (0x38, false) => return Some(M(LAlt)),
        (0x38, true) => return Some(M(RAlt)),
        (0x2A, false) => return Some(M(LShift)),
        (0x36, false) => return Some(M(RShift)), // E0 36 is only ever a fake Shift
        (0x5B, true) => return Some(M(LWin)),
        (0x5C, true) => return Some(M(RWin)),
        (0x02..=0x0B, false) => at(&DIGITS, 0x02),
        (0x10..=0x19, false) => at(&ROW_Q, 0x10),
        (0x1E..=0x26, false) => at(&ROW_A, 0x1E),
        (0x2C..=0x32, false) => at(&ROW_Z, 0x2C),
        (0x3B..=0x44, false) => at(&F1_10, 0x3B),
        (0x64..=0x6E, false) => at(&F13_23, 0x64),
        (0x47..=0x53, false) => at(&PAD, 0x47),
        (0x47..=0x53, true) => at(&EDIT, 0x47).filter(|k| !k.is_empty()),
        (0x0C, false) => Some("Minus"),
        (0x0D, false) => Some("Equal"),
        (0x0E, false) => Some("Backspace"),
        (0x0F, false) => Some("Tab"),
        (0x1A, false) => Some("BracketLeft"),
        (0x1B, false) => Some("BracketRight"),
        (0x1C, false) => Some("Enter"),
        (0x1C, true) => Some("NumpadEnter"),
        (0x27, false) => Some("Semicolon"),
        (0x28, false) => Some("Quote"),
        (0x29, false) => Some("Backquote"),
        (0x2B, false) => Some("Backslash"),
        (0x33, false) => Some("Comma"),
        (0x34, false) => Some("Period"),
        (0x35, false) => Some("Slash"),
        (0x35, true) => Some("NumpadDivide"),
        (0x37, false) => Some("NumpadMultiply"),
        (0x37, true) | (0x54, false) => Some("PrintScreen"), // 0x54: PrintScreen while Alt is down
        (0x39, false) => Some("Space"),
        (0x45, false) => Some("NumLock"),
        (0x46, false) => Some("ScrollLock"),
        (0x46, true) => Some("Pause"), // Ctrl+Pause arrives as Break (E0 46)
        (0x57, false) => Some("F11"),
        (0x58, false) => Some("F12"),
        (0x76, false) => Some("F24"),
        _ => None,
    };
    key.map(Key)
}

/// Mouse button transitions in one `RAWMOUSE.usButtonFlags`, as `(button, down)`. Names match
/// the UI's: the wheel button is `Mouse3`, back is `Mouse4`, forward is `Mouse5`.
pub fn mouse_buttons(flags: u16) -> impl Iterator<Item = (Input, bool)> {
    const BUTTONS: [(u32, u32, &str); 3] = [
        (RI_MOUSE_MIDDLE_BUTTON_DOWN, RI_MOUSE_MIDDLE_BUTTON_UP, "Mouse3"),
        (RI_MOUSE_BUTTON_4_DOWN, RI_MOUSE_BUTTON_4_UP, "Mouse4"),
        (RI_MOUSE_BUTTON_5_DOWN, RI_MOUSE_BUTTON_5_UP, "Mouse5"),
    ];
    let flags = u32::from(flags);
    BUTTONS.into_iter().flat_map(move |(down, up, name)| {
        [
            (flags & down != 0).then_some((Input::Key(name), true)),
            (flags & up != 0).then_some((Input::Key(name), false)),
        ]
        .into_iter()
        .flatten()
    })
}

/// The virtual key `GetAsyncKeyState` knows a mouse button by (names as in [`mouse_buttons`]).
fn mouse_virtual_key(name: &str) -> u16 {
    match name {
        "Mouse3" => VK_MBUTTON.0,
        "Mouse4" => VK_XBUTTON1.0,
        _ => VK_XBUTTON2.0,
    }
}

/// The hotkey listener thread; dropping it stops the thread. To change bindings, start a new one.
pub struct Hotkeys {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
    /// Set when dropped: a thread left running (see `Drop`) must not act on old bindings.
    retired: Arc<AtomicBool>,
}

impl Hotkeys {
    /// Calls `on_event` on the listener thread for every matched press and release, whichever
    /// app has focus. Fails if Windows refuses the Raw Input registration.
    ///
    /// Windows keeps one Raw Input target per device type per process and the last registration
    /// wins, so run one `Hotkeys` at a time. Tauri's event loop (tao) registers too, so start this **after** Tauri's `build()`,
    /// and never call `set_device_event_filter`, which re-registers or removes it.
    pub fn start(
        bindings: BTreeMap<Action, Hotkey>,
        mut on_event: impl FnMut(Event<Action>) + Send + 'static,
    ) -> Result<Self> {
        let (ready, started) = mpsc::channel();
        let retired = Arc::new(AtomicBool::new(false));
        let gone = retired.clone();
        let thread = std::thread::Builder::new()
            .name("backsight-hotkeys".into())
            .spawn(move || {
                // Only listen to the mouse when a binding uses it: a gaming mouse reports 8000 times a second.
                let mouse = bindings.values().any(|h| h.key.starts_with("Mouse"));
                let on_event = move |e| {
                    if !gone.load(Relaxed) {
                        on_event(e);
                    }
                };
                run(Matcher::new(&bindings).with_loose(Action::PushToTalk), mouse, on_event, ready)
            })
            .expect("spawn the hotkey thread");
        let thread_id = started.recv().expect("the hotkey thread reports its start")?;
        Ok(Hotkeys { thread_id, thread: Some(thread), retired })
    }
}

/// How long dropping [`Hotkeys`] keeps trying to hand the thread its quit message.
const QUIT_TRIES_FOR: Duration = Duration::from_secs(2);

impl Drop for Hotkeys {
    fn drop(&mut self) {
        self.retired.store(true, Relaxed);
        let Some(t) = self.thread.take() else { return };
        // The post fails when the thread's queue is full (10,000 messages, e.g. a fast mouse while
        // it is busy): retry, and never wait on a thread that didn't get the message.
        let deadline = Instant::now() + QUIT_TRIES_FOR;
        // SAFETY: posts to a thread with a message queue; if it already exited the call fails harmlessly.
        while unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) }.is_err() && !t.is_finished()
        {
            if Instant::now() >= deadline {
                // shortcut: its Raw Input mouse registration stays on its window (a new listener
                // without mouse bindings registers only the keyboard); events are ignored, but a
                // fast mouse keeps it busy. Remove the registration (RIDEV_REMOVE) if this is ever seen.
                tracing::warn!("the hotkey thread didn't take its quit message; left running");
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = t.join();
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: called by Windows for our window with valid arguments.
    unsafe {
        if msg == WM_WTSSESSION_CHANGE {
            // Sent, not posted, so it lands here; hand it to the loop that owns the matcher.
            let _ = PostMessageW(Some(hwnd), WM_APP_RESET, WPARAM(0), LPARAM(0));
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }
}

/// The window, and whether lock/unlock notifications were registered.
fn open_window(mouse: bool) -> Result<(HWND, bool)> {
    // SAFETY: plain Win32 calls; the class name and wndproc are 'static.
    unsafe {
        let instance = GetModuleHandleW(None)?.into();
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            lpszClassName: w!("BacksightHotkeys"),
            ..Default::default()
        };
        RegisterClassW(&class); // fails harmlessly when a previous listener registered it
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class.lpszClassName,
            None,
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance),
            None,
        )?;
        let device = |usage| RAWINPUTDEVICE {
            usUsagePage: 0x01, // generic desktop
            usUsage: usage,
            dwFlags: RIDEV_INPUTSINK | RIDEV_DEVNOTIFY, // input while unfocused; WM_INPUT_DEVICE_CHANGE
            hwndTarget: hwnd,
        };
        let devices = [device(0x06), device(0x02)];
        RegisterRawInputDevices(&devices[..1 + mouse as usize], size_of::<RAWINPUTDEVICE>() as u32)?;
        // Can fail early at boot before Remote Desktop Services is up; hotkeys still work without
        // lock resets, and the stale-modifier check covers most of what they're for.
        let session = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION).is_ok();
        Ok((hwnd, session))
    }
}

/// A held modifier older than this is checked against the real key state before a hotkey
/// matches: its key-up may have been made on the secure desktop (Ctrl+Alt+Del, UAC), which we
/// never see. Younger ones are trusted, so a fast press isn't lost when this thread lags.
const STALE_MODIFIER_MS: u32 = 500;

/// While a key that fired an action is held, this timer checks it is really still down: its
/// key-up is lost when it's let go on the secure desktop (UAC, Ctrl+Alt+Del) or over an
/// elevated window, and push-to-talk would stay on.
const HELD_TIMER: usize = 1;
const HELD_POLL_MS: u32 = 250;

fn virtual_key(m: Modifier) -> VIRTUAL_KEY {
    match m {
        Modifier::LCtrl => VK_LCONTROL,
        Modifier::RCtrl => VK_RCONTROL,
        Modifier::LAlt => VK_LMENU,
        Modifier::RAlt => VK_RMENU,
        Modifier::LShift => VK_LSHIFT,
        Modifier::RShift => VK_RSHIFT,
        Modifier::LWin => VK_LWIN,
        Modifier::RWin => VK_RWIN,
    }
}

fn run(
    mut matcher: Matcher<Action>,
    mouse: bool,
    mut on_event: impl FnMut(Event<Action>),
    ready: mpsc::Sender<Result<u32>>,
) {
    let (hwnd, session) = match open_window(mouse) {
        // SAFETY: no arguments.
        Ok(opened) => {
            let _ = ready.send(Ok(unsafe { GetCurrentThreadId() }));
            opened
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let mut msg = MSG::default();
    // When each modifier last went down, in message time (ms).
    let mut since = [0u32; Modifier::ALL.len()];
    // The virtual key each key last arrived with: the one Windows tracks it by on the current
    // layout (scan code 0x10 is VK_A on AZERTY). Bounded by the number of key names.
    let mut vks: Vec<(&'static str, u16)> = Vec::with_capacity(16);
    let mut polling = false;
    // SAFETY: standard message loop on this thread's queue; GetMessageW returns 0 on WM_QUIT
    // and -1 on error, both of which end the loop.
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
        match msg.message {
            WM_INPUT => {
                for (input, down, vk) in read_input(msg.lParam).into_iter().flatten() {
                    match input {
                        Input::Modifier(m) if down => since[m as usize] = msg.time,
                        Input::Key(key) if down => {
                            match vks.iter_mut().find(|(k, _)| *k == key) {
                                Some(known) => known.1 = vk,
                                None => vks.push((key, vk)),
                            }
                            // SAFETY: GetAsyncKeyState only reads the global key state.
                            matcher.forget_released_modifiers(|m| {
                                msg.time.wrapping_sub(since[m as usize]) < STALE_MODIFIER_MS
                                    || unsafe { GetAsyncKeyState(virtual_key(m).0.into()) } < 0
                            })
                        }
                        _ => {}
                    }
                    if let Some(event) = matcher.input(input, down) {
                        on_event(event);
                    }
                }
            }
            WM_TIMER if msg.wParam.0 == HELD_TIMER => {
                // A key with no known virtual key counts as down: never end push-to-talk early.
                let is_down = |key: &str| match vks.iter().find(|(k, _)| *k == key).map_or(0, |(_, vk)| *vk) {
                    0 => true,
                    // SAFETY: GetAsyncKeyState only reads the global key state.
                    vk => (unsafe { GetAsyncKeyState(vk.into()) }) < 0,
                };
                matcher.forget_released_keys(is_down).for_each(&mut on_event);
            }
            WM_INPUT_DEVICE_CHANGE | WM_APP_RESET => matcher.reset().for_each(&mut on_event),
            _ => {}
        }
        if matcher.holds_key() != polling {
            polling = !polling;
            // SAFETY: a timer on our own window, from the thread that owns it.
            unsafe {
                if polling {
                    SetTimer(Some(hwnd), HELD_TIMER, HELD_POLL_MS, None);
                } else {
                    let _ = KillTimer(Some(hwnd), HELD_TIMER);
                }
            }
        }
        // SAFETY: as above; DefWindowProc must see WM_INPUT to free the input data.
        unsafe { DispatchMessageW(&msg) };
    }
    // SAFETY: our own window, on the thread that created it; unregister before destroying it.
    unsafe {
        if session {
            let _ = WTSUnRegisterSessionNotification(hwnd);
        }
        let _ = DestroyWindow(hwnd);
    }
}

/// Up to three `(input, down, virtual key)` transitions in one `WM_INPUT` (a mouse report can
/// carry several).
fn read_input(lparam: LPARAM) -> [Option<(Input, bool, u16)>; 3] {
    let mut raw = RAWINPUT::default();
    let mut size = size_of::<RAWINPUT>() as u32;
    // SAFETY: `raw` is large enough for keyboard and mouse input, the only devices registered.
    let read = unsafe {
        GetRawInputData(
            HRAWINPUT(lparam.0 as _),
            RID_INPUT,
            Some(&mut raw as *mut RAWINPUT as _),
            &mut size,
            size_of::<RAWINPUTHEADER>() as u32,
        )
    };
    let mut out = [None; 3];
    if read == u32::MAX {
        return out;
    }
    // SAFETY: the header says which union member Windows filled in.
    unsafe {
        if raw.header.dwType == RIM_TYPEKEYBOARD.0 {
            let k = raw.data.keyboard;
            // VKey 0xFF marks the fake keys inside escape sequences (Pause, PrintScreen with NumLock).
            if k.VKey != 0xFF {
                let (flags, down) = (u32::from(k.Flags), u32::from(k.Flags) & RI_KEY_BREAK == 0);
                let key = if flags & RI_KEY_E1 != 0 {
                    (k.MakeCode == 0x1D).then_some(Input::Key("Pause"))
                } else {
                    key_from_scan(k.MakeCode, flags & RI_KEY_E0 != 0)
                };
                out[0] = key.map(|key| (key, down, k.VKey));
            }
        } else if raw.header.dwType == RIM_TYPEMOUSE.0 {
            let buttons = mouse_buttons(raw.data.mouse.Anonymous.Anonymous.usButtonFlags);
            for (slot, (input, down)) in out.iter_mut().zip(buttons) {
                let Input::Key(name) = input else { continue };
                *slot = Some((input, down, mouse_virtual_key(name)));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use Input::{Key, Modifier as M};

    #[test]
    fn scan_codes_name_the_same_keys_as_the_ui() {
        let cases = [
            ((0x44, false), Key("F10")),
            ((0x3B, false), Key("F1")),
            ((0x57, false), Key("F11")),
            ((0x76, false), Key("F24")),
            ((0x1E, false), Key("A")),
            ((0x32, false), Key("M")),
            ((0x0B, false), Key("0")),
            ((0x4C, false), Key("Numpad5")),
            ((0x4B, true), Key("Left")),
            ((0x52, true), Key("Insert")),
            ((0x1C, true), Key("NumpadEnter")),
            ((0x35, true), Key("NumpadDivide")),
            ((0x37, true), Key("PrintScreen")),
            ((0x29, false), Key("Backquote")),
            ((0x38, false), M(Modifier::LAlt)),
            ((0x38, true), M(Modifier::RAlt)),
            ((0x1D, true), M(Modifier::RCtrl)),
            ((0x36, false), M(Modifier::RShift)),
            ((0x5B, true), M(Modifier::LWin)),
        ];
        for ((code, e0), want) in cases {
            assert_eq!(key_from_scan(code, e0), Some(want), "{code:#x} e0={e0}");
        }
        assert_eq!(key_from_scan(0x4C, true), None, "no edit key there");
        assert_eq!(key_from_scan(0x01, false), None, "Esc can't be bound");
    }

    #[test]
    fn every_scan_code_name_is_a_valid_hotkey_key() {
        for code in 0..=0x7F {
            for e0 in [false, true] {
                if let Some(Key(name)) = key_from_scan(code, e0) {
                    let hk: Hotkey = name.parse().unwrap_or_else(|e| panic!("{name}: {e}"));
                    assert_eq!(hk.key, name, "{name} is canonical");
                }
            }
        }
    }

    #[test]
    fn one_mouse_report_can_hold_several_buttons() {
        let flags = (RI_MOUSE_BUTTON_4_DOWN | RI_MOUSE_BUTTON_5_UP) as u16;
        assert_eq!(mouse_buttons(flags).collect::<Vec<_>>(), [(Key("Mouse4"), true), (Key("Mouse5"), false)]);
        assert_eq!(mouse_buttons(0).count(), 0, "plain movement");
        assert_eq!(mouse_buttons(RI_MOUSE_LEFT_BUTTON_DOWN as u16).count(), 0, "left button can't be bound");
    }
}
