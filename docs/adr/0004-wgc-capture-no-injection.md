# 0004. Windows.Graphics.Capture, no game injection

- **Status:** Accepted
- **Date:** 2026-11-07

## Context

Options for getting game frames on Windows:

1. **Injection** (OBS "Game Capture"): hook the game's `Present` call. Fast, but it puts a DLL inside the game process, which anti-cheat systems (Vanguard, EAC, BattlEye) may block or flag.
2. **DXGI Desktop Duplication:** whole monitors only, one adapter, awkward on hybrid laptops.
3. **Windows.Graphics.Capture (WGC):** the modern, supported API behind Xbox Game Bar. Monitor or window, GPU textures, cross-adapter support, cursor and border control.

## Decision

Use **WGC** for all capture. No injection, no hooks. Minimum OS is Windows 10 2004 (build 19041); Windows 11 adds borderless capture.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Game-capture injection | Anti-cheat risk; per-graphics-API hook code (D3D9/11/12, Vulkan, OpenGL) |
| Desktop Duplication as primary | Monitor-only, no window capture, extra work on hybrid GPUs |

## Consequences

- Safe with anti-cheat, the same as Game Bar.
- Modern flip-model and borderless games capture perfectly. Some old exclusive-fullscreen titles may show black in *window* capture; we fall back to monitor capture and explain why in the UI.
- On Windows 10 a thin yellow border shows around captured windows (an OS rule). Monitor capture avoids it.
