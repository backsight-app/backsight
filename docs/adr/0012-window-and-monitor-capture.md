# 0012. Capture a chosen monitor or the game's window, at a fixed output size

- **Status:** Accepted
- **Date:** 2026-11-12

## Context

Until M3 video capture was always the whole primary monitor. Users with two monitors want the one the game is on, and some want only the game's window (no notifications, no second app beside it). The rules stand: Windows.Graphics.Capture only (ADR 0004), no hooks, and a fresh install stays basic. A window can resize, minimize or close at any time, while an encoder and the replay buffer can't change size mid-stream.

## Decision

`video.capture_target` is `"monitor"` (default) or `"window"`, and `video.capture_monitor` names a monitor by its display device interface path (`EnumDisplayDevicesW` with `EDD_GET_DEVICE_INTERFACE_NAME`), empty for the main monitor. A saved monitor that isn't connected falls back to the main one with a plain notice on Home; the saved choice is kept, so it comes back when the monitor does.

The output size is fixed when capture starts, from the chosen monitor and the resolution setting. Every picture of another shape (a window, or a monitor whose resolution changed) is scaled on the GPU into that size with its shape kept and black bars (`ID3D11VideoProcessor` destination rectangle), never stretched.

Window mode captures the window of the program the game watcher settled on (the same 3-second steady focus as game-only audio and file names), with `CreateForWindow`. For privacy it then **sticks with that program** until its window closes: a new window of the same program is followed, but another program (Alt+Tab to a chat or browser, which window capture would record even when covered) only takes over when its window covers its whole monitor, as full-screen and borderless games do, or when the captured program, or a listed game launcher (`content/game-launchers.json`) after the capture began, started it: a windowed game launched from there (`target::should_capture`, amended 2026-11-15 for a Battle.net launcher case). A minimized window repeats its last picture; a closed one waits for the next program kept in front; a capture or converter error is retried after 2 s. Each case shows a plain message on Home instead of stopping capture.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Store the monitor's `\\.\DISPLAYn` name | Windows renumbers these when monitors are plugged in another order. |
| Restart the encoder at the window's size | Clears the replay buffer and splits the recording on every resize. |
| Stretch to the output size | Distorts the picture of any window that isn't the monitor's shape. |
| A window picker | More UI for a basic setup; the program you keep in front is what you want almost always. |

## Consequences

- A small window in window mode is scaled up to the output size (never above what the resolution setting allows). Black bars show when the shapes differ.
- The display interface path changes if a monitor moves to another port; then the notice asks to pick it again.
- While no game has focus yet, window mode records nothing; Home says so.
