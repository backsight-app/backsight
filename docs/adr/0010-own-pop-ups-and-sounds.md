# 0010. Backsight's own pop-ups, recording dot and sounds

- **Status:** Accepted
- **Date:** 2026-11-11

## Context

Backsight told the user what a hotkey did (recording started, clip saved, screenshot taken) only through Windows notifications. In practice testers saw none:

- Windows only shows a notification from an app registered under its identifier. The unpackaged desktop build (ADR 0009) isn't, so Windows dropped every one silently. Dev builds only worked because the notification plugin borrows PowerShell's identifier there.
- Windows 11 turns on Do Not Disturb by itself while a game runs, so even a registered app's notifications don't pop up while playing, which is exactly when hotkeys are pressed.

A game recorder needs feedback that works while playing. Hard rule 2 (DEVELOPMENT.md) says "no overlay inside the game process". That rule exists to keep Backsight out of the game process (with rule 1, no injection); it never meant "nothing of ours on screen". The region picker (M2) is already an ordinary topmost window of ours.

## Decision

Backsight shows its own feedback, in three ways, each with a setting in General:

1. **Pop-ups** (the existing `notifications` setting): a small Backsight-styled card in the top-right corner of the monitor the game is on, for every action. It is an ordinary borderless, transparent, topmost window (`hud`) that never takes focus (created visible with `focused(false)` and `focusable(false)`, so Windows shows it with `SW_SHOWNOACTIVATE` and `WS_EX_NOACTIVATE`), ignores the mouse, and is left out of screen capture (`WDA_EXCLUDEFROMCAPTURE`), so it never appears in clips, recordings or screenshots. It is destroyed when idle, so it costs no memory between actions. Windows notifications are no longer used.
2. **Recording dot** (`recording_indicator`): while recording, the same window shows a red dot with the time, and a microphone symbol for push-to-talk or a muted mic. The tray icon also turns red.
3. **Sounds** (`sounds`): a short tone per action, made in code and played with `PlaySoundW` from memory. Sounds work in every game, including exclusive full screen, where no window of ours can show.

Nothing is injected or hooked: rule 1 stands, and rule 2 now reads "no overlay inside the game process".

## Alternatives considered

| Option | Why not |
| --- | --- |
| Register the identifier with Windows (Start-menu shortcut or `AppUserModelId` registry key) | Fixes the silent drop, but Do Not Disturb still hides every pop-up while playing |
| Draw inside the game (Present hook) | Injection: anti-cheat risk (ADR 0004) |
| Sounds only | No way to see that a recording is still running, or whether push-to-talk is open |

## Consequences

- Feedback shows over windowed and borderless games and on the desktop, and is heard in exclusive full screen.
- A WebView window is created per burst of pop-ups (about 0.2 s) and kept for the whole recording while the recording dot is on; it is destroyed afterwards.
- The sounds go to the default output device, which desktop-audio capture records: a sound played during a recording (a screenshot, a marker) is in the file. Turning sounds off avoids it; excluding our own process from loopback would fix it properly.
- The hud is placed in logical pixels from its monitor's scale; on mixed-DPI setups it may sit slightly off the corner.
