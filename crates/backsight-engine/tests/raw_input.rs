//! Hotkeys on the real desktop (testing.md, tier 2): inject key presses with `SendInput` and
//! check the Raw Input listener reports them, whichever window has focus. Uses Ctrl+Alt+Shift+F24,
//! which no app binds. Run with `cargo test -p backsight-engine --test raw_input -- --ignored
//! --test-threads=1`: one process gets one Raw Input listener, so two tests can't run at once.

use backsight_config::Action;
use backsight_engine::{hotkeys::Event, input::raw_input::Hotkeys};
use std::{collections::BTreeMap, sync::mpsc, time::Duration};
use windows::Win32::UI::Input::KeyboardAndMouse::*;

/// Sends one key by scan code; `extended` sets the E0 prefix (right Ctrl).
fn key(scan: u16, extended: bool, down: bool) {
    let mut flags = KEYEVENTF_SCANCODE;
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wScan: scan, dwFlags: flags, ..Default::default() } },
    };
    // SAFETY: one well-formed INPUT.
    assert_eq!(unsafe { SendInput(&[input], size_of::<INPUT>() as i32) }, 1, "SendInput blocked (UIPI?)");
}

const RCTRL: (u16, bool) = (0x1D, true);
const LALT: (u16, bool) = (0x38, false);
const LSHIFT: (u16, bool) = (0x2A, false);
const F24: (u16, bool) = (0x76, false);
const F23: (u16, bool) = (0x6E, false);

/// Releases every key the test uses when dropped, so a failed assert can't leave Ctrl, Alt and
/// Shift held down for the whole desktop.
struct ReleaseAll;

impl Drop for ReleaseAll {
    fn drop(&mut self) {
        for (scan, e0) in [F24, F23, LSHIFT, LALT, RCTRL] {
            key(scan, e0, false);
        }
    }
}

fn press(keys: &[(u16, bool)], down: bool) {
    for &(scan, e0) in keys {
        key(scan, e0, down);
    }
}

#[test]
#[ignore = "needs an interactive desktop; injects Ctrl+Alt+Shift+F24"]
fn hotkeys_fire_once_per_press_from_any_window() {
    let bindings = BTreeMap::from([
        (Action::SaveReplay, "Ctrl+Alt+Shift+F24".parse().unwrap()),
        (Action::PushToTalk, "Ctrl+Alt+Shift+F23".parse().unwrap()),
    ]);
    let (tx, rx) = mpsc::channel();
    let hotkeys = Hotkeys::start(bindings, move |e| tx.send(e).unwrap()).unwrap();
    let next = || rx.recv_timeout(Duration::from_secs(2));
    let _release = ReleaseAll;

    press(&[RCTRL, LALT, LSHIFT], true);
    press(&[F24, F24, F24], true); // the extra downs are what auto-repeat looks like
    press(&[F24], false);
    press(&[F23], true);
    press(&[F23], false);
    press(&[LSHIFT, LALT, RCTRL], false);
    press(&[F24], true); // no modifiers: nothing
    press(&[F24], false);
    // Input is handled in order, so this sentinel proves the bare F24 above fired nothing.
    press(&[RCTRL, LALT, LSHIFT, F23], true);
    press(&[F23, LSHIFT, LALT, RCTRL], false);

    assert_eq!(next(), Ok(Event::Pressed(Action::SaveReplay)), "right Ctrl counts as Ctrl");
    assert_eq!(next(), Ok(Event::Released(Action::SaveReplay)), "repeats are ignored");
    assert_eq!(next(), Ok(Event::Pressed(Action::PushToTalk)));
    assert_eq!(next(), Ok(Event::Released(Action::PushToTalk)));
    assert_eq!(next(), Ok(Event::Pressed(Action::PushToTalk)), "bare F24 fired nothing");
    assert_eq!(next(), Ok(Event::Released(Action::PushToTalk)));
    assert!(rx.recv_timeout(Duration::from_millis(200)).is_err(), "nothing else fired");
    drop(hotkeys);
}

#[test]
#[ignore = "needs an interactive desktop; injects Ctrl+Alt+Shift+F24"]
fn modifiers_held_for_a_while_still_count() {
    // Past STALE_MODIFIER_MS the listener re-checks each modifier with GetAsyncKeyState;
    // genuinely held keys must pass that check.
    let bindings = BTreeMap::from([(Action::SaveReplay, "Ctrl+Alt+Shift+F24".parse().unwrap())]);
    let (tx, rx) = mpsc::channel();
    let _hotkeys = Hotkeys::start(bindings, move |e| tx.send(e).unwrap()).unwrap();
    let _release = ReleaseAll;
    press(&[RCTRL, LALT, LSHIFT], true);
    std::thread::sleep(Duration::from_millis(700));
    press(&[F24], true);
    press(&[F24, LSHIFT, LALT, RCTRL], false);
    assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(Event::Pressed(Action::SaveReplay)));
}

#[test]
#[ignore = "needs an interactive desktop; injects Shift+F23"]
fn push_to_talk_held_with_shift_stays_on_until_released() {
    // A bare binding fires with Shift held, and the held-key timer (250 ms) checks the key with
    // GetAsyncKeyState: a key that is really down must not be released early.
    let bindings = BTreeMap::from([(Action::PushToTalk, "F23".parse().unwrap())]);
    let (tx, rx) = mpsc::channel();
    let _hotkeys = Hotkeys::start(bindings, move |e| tx.send(e).unwrap()).unwrap();
    let _release = ReleaseAll;
    press(&[LSHIFT, F23], true);
    assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(Event::Pressed(Action::PushToTalk)));
    assert!(rx.recv_timeout(Duration::from_millis(800)).is_err(), "still held: no release");
    press(&[F23, LSHIFT], false);
    assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(Event::Released(Action::PushToTalk)));
}
