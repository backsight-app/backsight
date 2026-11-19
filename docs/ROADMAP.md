# Roadmap

Each milestone ends with something usable and a definition of done that is measured, not assumed.

✅ done · 🟡 partly done · ⏸️ on hold

| Milestone | Status |
| --- | --- |
| M0 Foundation | ✅ |
| M1 Instant Replay end to end | ✅ |
| M2 Recording and screenshots | ✅ (two hand checks open) |
| M3 Every GPU, every case | 🟡 NVIDIA and Intel Quick Sync verified; AMD untested |
| M4 Ship it | 🟡 public releases, signed auto-update and FFmpeg source done; installer code signing open |

On hold: Authenticode signing of the installer, AMD encoder testing (no hardware), microphone switching mid-recording (hand check), the NVIDIA overlay clash check with the overlay on, the exclusive-fullscreen recording-cost gate. Later: see [Later](#later-).

Releases: every version since v0.1.3 is on [GitHub Releases](https://github.com/backsight-app/backsight/releases), built and signed by CI as `0.1.<run>`. Changes per version: [CHANGELOG.md](../CHANGELOG.md).

## M0 · Foundation ✅

- Repository structure, docs, ADRs, CI, brand assets
- Settings schema with atomic save, validation and hotkey parsing (`backsight-config`)
- Encoder catalog: 13 video encoders, 7 audio formats, 8 containers, 4 screenshot formats (`backsight-encode`)
- Plain-language glossary with integrity tests
- Instant Replay ring buffer (`backsight-engine::replay`)
- Tauri shell (tray, disposable window) and the full settings UI with tooltips and hotkey editor

## M1 · Instant Replay end to end ✅

- `scripts/fetch-ffmpeg.ps1`: downloads the pinned FFmpeg 8.1.3 slim GPL build and verifies its SHA-256
- Encoder probe (`backsight-encode::probe`): tries each encoder on a black frame and caches the result per graphics driver
- WGC monitor capture, GPU NV12 conversion, NVENC (H.264, HEVC) with zero-copy D3D11 frames
- Desktop audio loopback to AAC, with silence fill and QPC sync
- Raw Input hotkeys: save clip, toggle Instant Replay
- Save clip: buffer snapshot, then mux to crash-safe MP4 or MKV
- Live status on Home; Library with thumbnails, hover previews and filters ([library.md](architecture/library.md))
- Diagnostics counters (dropped frames, encoder queue depth, audio drift, silence fills) and `tracing` logs in `%APPDATA%\Backsight\logs`
- Tests ([testing.md](architecture/testing.md)): pacer and hotkey scenarios, option mapping, `ffprobe` file validation, A/V sync, crash-kill recovery, zero allocations per frame
- `scripts/hardware-check.ps1`: the same scenarios on a real GPU, plus PresentMon frame cost, as a Markdown report

**Done when:** on a test PC at 1080p60, the save-clip key in a game saves a playable 2-minute clip with sound in under 1 s, at under 5% FPS cost over WGC capture alone, with engine RAM under 150 MB plus the buffer. Met; measurements in [testing.md](architecture/testing.md).

## M2 · Recording and screenshots ✅

- Start and stop recording (Alt+R), pause and resume (Alt+Shift+R), split files every N minutes, markers (Alt+B) as chapters, MKV to MP4 conversion (the MKV is kept)
- Recordings cut off by a crash or power loss are recovered at the next start; the unfinished original is kept ([ADR 0008](adr/0008-interrupted-recording-files.md))
- Microphone (chosen device or Windows default), mute (Alt+T), push-to-talk; game-only audio (process loopback); separate tracks
- Screenshots: whole screen (Alt+S), active window (Alt+Shift+S), region (Ctrl+Alt+S); PNG, JPEG, WebP; copy to clipboard
- Folder pickers and Start with Windows (off by default)
- Backsight's own pop-ups, movable recording dot, red tray icon and sounds, shown over games and with Do Not Disturb on ([ADR 0010](adr/0010-own-pop-ups-and-sounds.md))
- Diagnostics page: replay buffer length, size and bitrate, recording bitrate and writer backlog, frame and audio counters, each explained
- `criterion` benchmarks for the pacer, replay buffer, recording feed and hotkeys
- Hardware checks for colour (range and matrix errors) and encoder quality (PSNR/SSIM floors)
- NVENC preset P1 by default: far less encoder work than P4 or P5 at the same game speed and near-identical quality. On a still screen NVENC rests instead of re-encoding identical frames (platform-notes)

Measured: a 30-minute recording has full length and no video gaps; the flash-and-beep test shows no drift; replay plus recording costs about 1.6% FPS over capture alone in a GPU-bound game.

Open or dropped checks:
- ⏸️ Switching or unplugging a microphone during a recording: not yet checked by hand with a physical device change
- ⏸️ Exclusive-fullscreen recording-cost gate ([testing.md](architecture/testing.md))
- Dropped: the region picker at Windows scaling above 100% (not worth reconfiguring a test machine)

## M3 · Every GPU, every case 🟡

- ✅ First-run setup recommends an encoder and explains unavailable choices
- ✅ Home warns when encoding falls behind; **Use a lighter setting** switches to hardware encoding
- ✅ Game-window capture and monitor selection ([ADR 0012](adr/0012-window-and-monitor-capture.md))
- ✅ Game-window mode: a web browser never takes over the capture; a notice for games in exclusive full screen; an HDR window capture that keeps failing falls back to standard video for that game (0.1.8; covered by unit tests, not yet seen in a live game)
- ✅ File names from the foreground program, with optional per-game folders
- ✅ Setup warns about NVIDIA overlay hotkey clashes and offers fallback keys. ⏸️ A check with the overlay switched on is still open
- ✅ Instant Replay resumes after its monitor is unplugged and replugged (checked by hand). A move to another monitor keeps the replay so far, and restarts are capped (0.1.8)
- ✅ HDR (opt-in): 10-bit HEVC (P010, BT.2020 PQ) on NVIDIA for a whole monitor or a game window, JPEG XR screenshots, tone-mapped Library thumbnails. An SDR monitor gets standard video with a note on Home. Verified with a calibrated test-pattern window ("Calibrated check" in [testing.md](architecture/testing.md))
- ✅ Intel Quick Sync (H.264, HEVC): zero-copy from D3D11 textures; replay, recording, pause and split, and quality floors verified on Intel Iris Xe (hybrid laptop). Not yet run on an Intel-only PC; HDR stays NVIDIA-only
- ⏸️ AMD (AMF): the code path exists but has never run on AMD hardware. First-run setup says so when it finds an AMD card

## M4 · Ship it 🟡

- ✅ NSIS installer with a slim FFmpeg build (about 44 MB of DLLs); per-user install; the uninstaller keeps clips and settings; builds carry no personal names or paths ([ADR 0011](adr/0011-nsis-installer.md))
- ✅ Local crash reports stay on the PC unless shared manually
- ✅ Automated performance regression check on GitHub ([testing.md](architecture/testing.md))
- ✅ Public repository [backsight-app/backsight](https://github.com/backsight-app/backsight) since 2026-11-18; every green push to `main` is released by CI as `0.1.<run>` (v0.1.3 to v0.1.8 so far)
- ✅ FFmpeg corresponding source published as the release [`ffmpeg-8.1.3-slim-r1`](https://github.com/backsight-app/backsight/releases/tag/ffmpeg-8.1.3-slim-r1) (GPL, [ADR 0005](adr/0005-license-gpl.md))
- ✅ Automatic updates from GitHub Releases (`tauri-plugin-updater`), signed with minisign: checks at start, hourly and when the window opens; installs by itself when idle shortly after start, otherwise **Restart to update** on Home ([ADR 0014](adr/0014-updates-from-github-releases.md)). Quiet install without an installer window from 0.1.9 on
- ✅ The installer offers to close a running Backsight (0.1.7)
- ⏸️ Authenticode signing of the installer (SmartScreen warns on first run). A free open-source signing service is being looked into

## Shipped since M4

- ✅ Vertical clips: Clip shape Wide, Vertical or Both; a 9:16 centre crop made in the background after the save ([ADR 0015](adr/0015-vertical-clips.md), 0.1.5)
- ✅ On-device captions with whisper.cpp: opt-in 61 MB model download, a `.srt` per clip ([ADR 0016](adr/0016-on-device-captions.md), 0.1.5); captions burned into vertical clips (0.1.8)
- ✅ New default hotkeys (Alt+C, Alt+R, Alt+S family) with NVIDIA-clash fallbacks, and a No key option (0.1.6)
- ✅ Redesign: neutral dark theme, violet accent, bundled Inter and Sora fonts, rebuilt Home (0.1.7)

## Later ⏸️

- Quick trim and share in the Library
- Per-game profiles
- Translations (`glossary.<lang>.json`)
- Captions for recordings and for clips saved before captions were turned on
- Vertical clips cut from recordings; framing that follows the action instead of the centre
