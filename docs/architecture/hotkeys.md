# Hotkeys

Hotkeys must fire while a fullscreen game has focus, support mouse side buttons and push-to-talk, and never trip anti-cheat. Decision record: [ADR 0007](../adr/0007-raw-input-hotkeys.md).

## Mechanism: Raw Input

- A message-only window on the input thread calls `RegisterRawInputDevices` for keyboard (`0x01/0x06`) and mouse (`0x01/0x02`) with `RIDEV_INPUTSINK`, so it receives input even when it isn't focused.
- `WM_INPUT` gives key-down, key-up and mouse-button events. The thread tracks modifier state itself (left and right Ctrl, Alt, Shift and Win) and matches the result against the parsed bindings.
- A combo must match exactly (Alt+Shift+F10 is not Alt+F10), but push-to-talk bound with no modifiers also fires while modifiers are held, so it works while you hold Shift to sprint or Ctrl to crouch. An exact combo on the same key wins. Other actions without modifiers match exactly (Ctrl+F12 doesn't fire F12).
- Key repeat is ignored: an action fires once per press. Push-to-talk uses down and up.
- A key-up let go on the secure desktop (UAC, Ctrl+Alt+Del) or over an elevated window never arrives. While a key that fired an action is held, a 250 ms timer checks it with `GetAsyncKeyState` and reports the release itself, so push-to-talk can't stay on.
- No hooks (`SetWindowsHookEx`) and no DLL injection, so there is nothing for anti-cheat to flag.

**Limitation:** Raw Input can't swallow a key, so the game also sees the press. That is why the defaults use Alt plus F-keys, which games rarely bind. If a game runs **as administrator**, Windows blocks its input from reaching non-elevated apps, so hotkeys don't fire while it has focus. Backsight doesn't detect this; running Backsight as administrator too works around it.

## Binding format

Canonical strings such as `Ctrl+Alt+F10` or `Mouse4`, in the order Ctrl, Alt, Shift, Win, then the key. They are parsed and validated in [`crates/backsight-config/src/hotkey.rs`](../../crates/backsight-config/src/hotkey.rs); the UI builds the same names from `KeyboardEvent.code` in [`app/src/lib/hotkeys.ts`](../../app/src/lib/hotkeys.ts). An empty string means unbound.

## Defaults

| Action | Default | Why |
| --- | --- | --- |
| Save Instant Replay | Alt+F10 | Same as ShadowPlay, so muscle memory carries over |
| Turn Instant Replay on or off | Alt+Shift+F10 | ShadowPlay-compatible |
| Start or stop recording | Alt+F9 | ShadowPlay-compatible |
| Pause or resume recording | Alt+Shift+F9 | |
| Screenshot: whole screen | Alt+F1 | ShadowPlay-compatible |
| Screenshot: active window | Alt+Shift+F1 | |
| Screenshot: pick an area | Ctrl+Alt+F1 | |
| Mute or unmute mic | Alt+F8 | |
| Push to talk | (unbound) | Usually a mouse button; let the user choose |
| Add a marker | Alt+F7 | |
| Open Backsight | Alt+Shift+C | Avoids Alt+Z, the NVIDIA overlay key |

If the NVIDIA overlay is turned on, its Alt+F10, Alt+F9 and Alt+F1 also fire. Setup warns about the clash; change the keys on one side (Hotkeys page, or the NVIDIA App).

## Conflicts

The UI marks two actions with the same combo in red and explains why it matters. The engine runs the same check with `conflicts()` and ignores the later binding instead of firing both.
