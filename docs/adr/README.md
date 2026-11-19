# Architecture decision records

Each ADR records one decision: the context, what we chose, what we gave up. New ADRs copy [template.md](template.md) and take the next number. Superseded ADRs stay, marked as superseded.

| # | Decision | Status |
| --- | --- | --- |
| [0001](0001-no-shadowplay-integration.md) | Build our own capture stack instead of integrating ShadowPlay | Accepted |
| [0002](0002-rust-engine-tauri-ui.md) | Rust engine with a Tauri 2 + Svelte settings window | Accepted |
| [0003](0003-ffmpeg-for-encoding.md) | FFmpeg (libav*) for encoding and muxing | Accepted |
| [0004](0004-wgc-capture-no-injection.md) | Windows.Graphics.Capture, no game injection | Accepted |
| [0005](0005-license-gpl.md) | License under GPL-3.0-or-later | Accepted |
| [0006](0006-data-driven-settings.md) | One data file drives options, UI and explanations | Accepted |
| [0007](0007-raw-input-hotkeys.md) | Raw Input for global hotkeys | Accepted |
| [0008](0008-interrupted-recording-files.md) | Mark unfinished recordings by name and recover them by copy | Accepted |
| [0009](0009-local-test-build-updates.md) | Local test builds update from a folder, not the internet | Accepted (public releases: ADR 0014) |
| [0010](0010-own-pop-ups-and-sounds.md) | Backsight's own pop-ups, recording dot and sounds | Accepted |
| [0011](0011-nsis-installer.md) | Ship a per-user NSIS installer | Accepted |
| [0012](0012-window-and-monitor-capture.md) | Capture a chosen monitor or the game's window, at a fixed output size | Accepted |
| [0013](0013-rename-to-backsight.md) | Rename the app from Clipwise to Backsight | Accepted |
| [0014](0014-updates-from-github-releases.md) | Installed copies update from GitHub Releases | Accepted (amended 2026-11-18: automatic checks) |
| [0015](0015-vertical-clips.md) | Make vertical clips from the saved clip, in the background | Accepted |
| [0016](0016-on-device-captions.md) | Caption the microphone on the PC with whisper.cpp, after the save | Accepted |
| [0017](0017-other-apps-hotkeys-read-only.md) | Find other apps' hotkeys by reading, never by hooking | Accepted |
