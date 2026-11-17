# 0002. Rust engine with a Tauri 2 + Svelte settings window

- **Status:** Accepted
- **Date:** 2026-11-07

## Context

Backsight runs all day in the background next to games. It must be small, never leak or crash, and talk to COM-heavy Windows APIs (WGC, D3D11, WASAPI, Raw Input). Its UI must be friendlier than OBS, which means rich tooltips, explanation panes and good typography.

## Decision

- **Engine in Rust**, using Microsoft's official [`windows`](https://crates.io/crates/windows) crate for Win32 and WinRT. Memory safety matters in a process that runs for weeks; Cargo workspaces give a clean crate-per-responsibility layout.
- **UI in Tauri 2 (stable) + Svelte 5.** The window is a WebView2 page that is **destroyed when closed** and rebuilt from config when reopened, so it costs nothing while you play. Tauri 3 is alpha as of this writing; we stay on 2.x.
- One process: the tray, the engine and (when open) the window.

## Alternatives considered

| Option | Why not |
| --- | --- |
| C++ with Qt (OBS's stack) | Mature, but slower to build safely; Qt adds ~40 MB; UI polish is harder |
| C# with WPF or WinUI 3 | Good Windows integration, but GC pauses in a 60 FPS pipeline need care; WinUI 3 packaging is painful; more resident memory |
| Electron | Bundles its own Chromium (~150 MB on disk and in RAM) |
| Rust with a native UI (egui, Slint) | Lighter while open, but rich explanatory UI and accessibility are much more work |

## Consequences

- With the window open, WebView2 uses about 80–150 MB. With it closed, nothing.
- Svelte compiles to small vanilla JS (the whole UI is about 47 KB gzipped today).
- Engine and UI talk only through Tauri commands and events, which keeps the boundary explicit.
- A panic in the engine takes down the tray too. If that proves a problem, the engine can move to a child process behind the same command interface.
