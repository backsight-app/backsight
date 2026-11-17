# Roadmap

Each milestone ends with something you can use, and a definition of done that is measured, not assumed.

✅ done · 🟡 partly done · ⏸️ on hold

| Milestone | Status |
| --- | --- |
| M0 Foundation | ✅ |
| M1 Instant Replay end to end | ✅ |
| M2 Recording and screenshots | ✅ |
| M3 Every GPU, every case | 🟡 NVIDIA verified; one hands-on check open; AMD and Intel on hold |
| M4 Ship it | 🟡 installer, crash reports and CI checks done; signing and auto-update on hold |

On hold: microphone switching mid-recording, the NVIDIA overlay clash check, AMD and Intel testing, signed builds with public auto-update. Later ideas: quick trim, per-game settings, translations.

## M0 · Foundation ✅

- Repository structure, docs, ADRs, CI, brand assets
- Settings schema with atomic save, validation and hotkey parsing (`backsight-config`)
- Encoder catalog: 13 video encoders, 7 audio formats, 8 containers, 4 screenshot formats (`backsight-encode`)
- Plain-language glossary with integrity tests
- Instant Replay ring buffer (`backsight-engine::replay`)
- Tauri shell (tray, disposable window) and the full settings UI with tooltips and hotkey editor

## M1 · Instant Replay works end to end ✅

- `scripts/fetch-ffmpeg.ps1`: downloads the pinned FFmpeg 8.1 GPL shared build and verifies its SHA-256
- Encoder probe (`backsight-encode::probe`): tries each encoder on a black frame and caches the result per graphics driver
- WGC monitor capture, GPU NV12 conversion, NVENC (H.264, HEVC) with zero-copy D3D11 frames
- Desktop audio loopback to AAC, with silence fill and QPC sync
- Raw Input hotkeys: save replay, toggle replay
- Save clip: buffer snapshot, then mux to crash-safe MP4 or MKV; tray notification
- Live status on the Home page; Library page with thumbnails, hover previews and filters ([library.md](architecture/library.md))
- Diagnostics counters (dropped frames, encoder queue depth, audio drift, silence fills) and `tracing` logs in `%APPDATA%\Backsight\logs`
- Tests ([testing.md](architecture/testing.md)): pacer and hotkey scenarios, option mapping, `ffprobe` file validation, A/V sync, crash-kill recovery, zero allocations per frame
- `scripts/hardware-check.ps1`: the same scenarios on a real GPU, plus PresentMon frame cost, as a Markdown report

**Done when:** on a test PC at 1080p60, pressing Alt+F10 in a game saves a playable 2-minute clip with sound in under 1 s, at under 5% FPS cost over WGC capture alone, with engine RAM under 150 MB plus the buffer. Met; measurements are in [testing.md](architecture/testing.md).

## M2 · Recording and screenshots ✅

- Start and stop recording (Alt+F9), pause and resume (Alt+Shift+F9), split files every N minutes, markers (Alt+F7) as chapters, MKV to MP4 conversion (the MKV is kept)
- Recordings cut off by a crash or power loss are saved at the next start; the unfinished original is kept ([ADR 0008](adr/0008-interrupted-recording-files.md))
- Microphone (chosen device or Windows default), mute, push-to-talk; game-only audio (process loopback); separate tracks
- Screenshots: whole screen (Alt+F1), active window (Alt+Shift+F1), region overlay (Ctrl+Alt+F1); PNG, JPEG, WebP; copy to clipboard
- Folder pickers and "Start with Windows" (off by default)
- Backsight's own pop-ups, a movable recording dot, red tray icon and sounds, shown over games and Do Not Disturb ([ADR 0010](adr/0010-own-pop-ups-and-sounds.md))
- Diagnostics page: replay buffer length, size and bitrate, recording bitrate and writer backlog, frame and audio counters, each explained in plain language
- `criterion` benchmarks for the pacer, replay buffer, recording feed and hotkeys
- Hardware checks for colour (range and matrix errors) and encoder quality (PSNR/SSIM floors)
- NVENC preset P1 is the default: far less encoder work than P4 or P5 at the same game speed and near-identical picture quality. On a still screen NVENC rests instead of re-encoding identical frames (see platform-notes)

Measured: a 30-minute recording has full length and no video gaps; flash-and-beep sync shows no drift; replay plus recording costs about 1.6% FPS over capture-only in a GPU-bound game.

Open or dropped checks:
- ⏸️ Switching or unplugging a microphone during a recording (untested with a physical microphone switch)
- Dropped: the area picker at Windows scaling above 100% (not worth reconfiguring a test machine)
- The exclusive-fullscreen recording-cost gate is still open ([testing.md](architecture/testing.md))

## M3 · Every GPU, every case 🟡

- ✅ First-run setup recommends an encoder and explains unavailable choices
- ✅ Home warns when encoding falls behind; "Use a lighter setting" switches to hardware encoding
- ✅ Game-window capture and monitor selection ([ADR 0012](adr/0012-window-and-monitor-capture.md))
- ✅ File names can use the foreground program's name, with optional per-game folders
- ✅ Setup warns about NVIDIA overlay hotkey clashes (a check with the overlay on is ⏸️ on hold)
- 🟡 Instant Replay should resume by itself after the monitor is unplugged and replugged: still to verify by hand
- ✅ HDR (opt-in): 10-bit HEVC (P010, BT.2020 PQ) on NVIDIA for a whole monitor or a game window, JPEG XR screenshots, tone-mapped Library thumbnails. An SDR monitor gets standard video with a note on Home. Verified with a calibrated test-pattern window (see "Calibrated HDR check" in [testing.md](architecture/testing.md))
- ⏸️ AMD (AMF) and Intel (Quick Sync) encoding: the engine has the code path, but it has never run on that hardware. Until it has, Backsight is verified on NVIDIA only, and first-run setup says so plainly when it finds an AMD or Intel card

## M4 · Ship it 🟡

- ✅ NSIS installer bundling a slim FFmpeg build (about 44 MB of DLLs); per-user install; the uninstaller keeps clips; builds carry no personal names or paths
- ✅ Local crash reports stay on the PC unless shared manually
- ✅ Automated performance regression check on GitHub ([testing.md](architecture/testing.md))
- 🟡 FFmpeg source: published with every build; a public release needs it where anyone can reach it (GPL, [ADR 0005](adr/0005-license-gpl.md))
- ⏸️ Signed builds and public auto-update (`tauri-plugin-updater`). An Update button for local builds exists ([ADR 0009](adr/0009-local-test-build-updates.md))

## Later ⏸️

- Quick trim and share in the Library
- Per-game profiles
- Translations (`glossary.<lang>.json`)
