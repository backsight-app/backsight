# Hotkeys

Hotkeys must fire while a fullscreen game has focus, support mouse side buttons and push-to-talk, and never trip anti-cheat. Decision record: [ADR 0007](../adr/0007-raw-input-hotkeys.md).

## Mechanism: Raw Input

- A message-only window on the input thread calls `RegisterRawInputDevices` for keyboard (`0x01/0x06`) and mouse (`0x01/0x02`) with `RIDEV_INPUTSINK`, so it receives input even when it isn't focused.
- `WM_INPUT` gives key-down, key-up and mouse-button events. The thread tracks modifier state itself (left and right Ctrl, Alt, Shift and Win) and matches the result against the parsed bindings.
- A combo must match exactly (Alt+Shift+F10 is not Alt+F10), but push-to-talk bound with no modifiers also fires while modifiers are held, so it works while you hold Shift to sprint or Ctrl to crouch. An exact combo on the same key wins. Other actions without modifiers match exactly (Ctrl+F12 doesn't fire F12).
- Key repeat is ignored: an action fires once per press. Push-to-talk uses down and up.
- A key-up let go on the secure desktop (UAC, Ctrl+Alt+Del) or over an elevated window never arrives. While a key that fired an action is held, a 250 ms timer checks it with `GetAsyncKeyState` and reports the release itself, so push-to-talk can't stay on.
- No hooks (`SetWindowsHookEx`) and no DLL injection, so there is nothing for anti-cheat to flag.

**Limitation:** Raw Input can't swallow a key, so the game also sees the press. The defaults are Alt plus a letter, which is quicker to reach than an F-key; the game sees the letter too, so the letters avoid W, A, D, Q, E, R and F (movement, abilities, reload, use). Anyone whose game uses one of them can change it or pick No key. If a game runs **as administrator**, Windows blocks its input from reaching non-elevated apps, so hotkeys don't fire while it has focus. Backsight doesn't detect this; running Backsight as administrator too works around it.

## Binding format

Canonical strings such as `Ctrl+Alt+F10` or `Mouse4`, in the order Ctrl, Alt, Shift, Win, then the key. They are parsed and validated in [`crates/backsight-config/src/hotkey.rs`](../../crates/backsight-config/src/hotkey.rs); the UI builds the same names from `KeyboardEvent.code` in [`app/src/lib/hotkeys.ts`](../../app/src/lib/hotkeys.ts). An empty string means unbound.

## Defaults

| Action | Default | Why |
| --- | --- | --- |
| Save Instant Replay | Alt+X | Next to the thumb on Alt; Alt+C is Discord's clip key |
| Turn Instant Replay on or off | Alt+Shift+X | Same key with Shift |
| Start or stop recording | Alt+V | V for video; Alt+R is NVIDIA's and AMD's overlay key |
| Pause or resume recording | Alt+Shift+V | Same key with Shift |
| Screenshot: whole screen | Alt+S | S for shot |
| Screenshot: active window | Alt+Shift+S | Same key with Shift |
| Screenshot: pick an area | Ctrl+Alt+S | Same key with Ctrl |
| Mute or unmute mic | Alt+T | T for talk; Ctrl+Alt+M and Ctrl+Shift+M are NVIDIA's and Discord's |
| Push to talk | (no key) | Usually a mouse button; let the user choose |
| Add a marker | Alt+B | B for bookmark |
| Open Backsight | Alt+Shift+C | Avoids Alt+Z, the NVIDIA overlay key |

The first releases used NVIDIA's own keys as defaults (Alt+F10, Alt+F9, Alt+F1 and neighbours), and with NVIDIA's overlay on, its keys won. Existing settings keep their keys; `content/previous-default-hotkeys.json` lists the old set, and Home's NVIDIA clash card ("Give Backsight other keys") moves a clashing key to today's default, or the whole old set when none was changed. Why these keys and what else was checked: [platform-notes.md, "Default hotkeys"](../reference/platform-notes.md#default-hotkeys).

Every action can have no key (an empty string): the Hotkeys page has a No key button beside each box, and Backspace while a box listens does the same. An empty key is saved as `""`, loads as no key (not the default), and never counts as a clash.

## Conflicts

The UI marks two actions with the same combo in red and explains why it matters. The engine runs the same check with `conflicts()` and ignores the later binding instead of firing both.
