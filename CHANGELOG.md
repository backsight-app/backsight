# Changelog

All notable changes are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## Unreleased

### Added

- Library: "Remove damaged originals" moves the originals kept after crash recovery (`.recovered.part`) to the Recycle Bin, after a confirmation.
- Thumbnails for HDR (JPEG XR) screenshots, tone-mapped like the clipboard copy.

### Changed

- Intel Quick Sync encodes straight from GPU textures, with no copy through system memory, and keeps up at 60 FPS. Verified on Intel Iris Xe (H.264 and HEVC): replay, recording, pause and split, quality floors.
- Updates install in quiet mode, without an installer window. Takes effect for updates from 0.1.9 on, because the running copy chooses the mode.
- The first start of a new version opens the window, also when Backsight starts with Windows, so an update is visible.
- Captions are prompted with "Backsight" and the game's name, so both are spelled correctly.

### Fixed

- New clips sometimes kept a blank thumbnail; a file that can't be read yet is retried during its first two minutes, and Home's saved-clip card follows the clip (including its vertical copy).
- Home's Screenshot tile captured Backsight's own window; it now captures the monitor behind it.
- "Use a lighter setting" sized the main monitor instead of the monitor being captured.
- Open folder failures are logged instead of being ignored.
- Windows' quick settings and other shell panels (ShellHost) were treated as a game.

## 0.1.8 - 2026-11-18

### Added

- Captions on vertical clips: with captions on, the words are burned into the vertical clip's picture (white text with a black outline, above the phone apps' buttons). With Clip shape set to Both, the wide clip keeps its `.srt`.
- A move to another monitor keeps the replay recorded so far; the next saved clip also writes it as a separate file. Kept footage never exceeds the replay length or the replay's RAM ceiling.
- Game-window mode shows a notice when the game runs in exclusive full screen, which window capture can't record, and asks for borderless or windowed mode.

### Changed

- Problem-report, crash and Diagnostics text points to GitHub issues.
- Pause follows record to its fallback key (Alt+Shift+V when recording moves to Alt+V).

### Fixed

- Game-window mode never hands the capture to a web browser, including one the game opens for a store page or log-in.
- An HDR game-window capture that fails three times in a row falls back to standard video for that game, with a notice on Home, instead of retrying forever.
- A still window that is resized once records its new picture.
- Monitor changes start at most three rounds of restarts; then Instant Replay stays off with a notice.
- A failed MKV-to-MP4 conversion on a full drive says the drive may be full. The MKV is kept unchanged.
- Vertical clips: jobs are found again after the video folder changes or the drive is unplugged, matched regardless of path case, and resumed jobs get captions. A job that crashed Backsight is not retried; its clip stays wide. A job record is never overwritten when damaged. No error pop-up while quitting; the job resumes at the next start.
- Burned-in captions: smoother text edges, the newest of overlapping captions is shown, and the `.srt` is kept if drawing fails.

## 0.1.7 - 2026-11-18

### Added

- Redesign: dark neutral greys, one violet accent, bundled Inter and Sora fonts. Home shows the detected game, the replay with a Save clip button and timeline, Record and Screenshot tiles, the microphone and recent captures. The side menu shows new Library items and free space on the clips drive.
- Automatic updates: installed copies check at start, every hour and when the window opens. Home shows **Update ready** with **Restart to update**. An update found shortly after start, before a game or recording, installs by itself ([ADR 0014](docs/adr/0014-updates-from-github-releases.md), [privacy](docs/privacy.md)).
- The installer offers to close a running Backsight instead of stopping, when the installed copy is 0.1.7 or newer.

## 0.1.4 to 0.1.6 - 2026-11-18

### Added

- Vertical clips (0.1.5): the Clip shape setting on the Instant Replay page saves each clip Wide (default), Vertical (9:16 only) or Both. The vertical clip is cropped from the centre in the background after the save; if it can't be made, the clip is kept wide. Recordings stay wide ([ADR 0015](docs/adr/0015-vertical-clips.md)).
- Captions (0.1.5): optional on-device speech-to-text writes a `.srt` beside each saved clip. Off by default; needs a one-time 61 MB model download from the Audio page ([ADR 0016](docs/adr/0016-on-device-captions.md)).
- A No key option for every hotkey. Home says when an action has no key.

### Changed

- New default hotkeys (0.1.6): Alt+C save clip, Alt+Shift+C replay on/off, Alt+R record, Alt+Shift+R pause, Alt+S screenshot, Alt+Shift+S window, Ctrl+Alt+S region, Alt+T microphone mute, Alt+B marker, Alt+Shift+O open Backsight. The first defaults were NVIDIA's keys, which the NVIDIA overlay takes first. Existing settings keep their keys; when they clash with NVIDIA's, Home's **Give Backsight other keys** moves them.
- Alt+R is also the NVIDIA and AMD performance-overlay key; with NVIDIA's overlay on, Home points this out and **Give Backsight other keys** moves recording to Alt+V. Alt+C is also Discord's clip key.

## 0.1.3 - 2026-11-18

Versions are numbered 0.1.<build>; each one on the Releases page lists its own changes.

First public release: Instant Replay, recording and screenshots, with an explanation for every setting. Defaults to 1080p60, runs in the tray when the window is closed, and recovers recordings cut off by a crash or power loss. Capture uses Windows.Graphics.Capture, with no hooks or injection. Updates are installed on request from the General page; Backsight makes no other network connections.

The entries below cover development up to this release. Hotkeys named here are the defaults of the time (changed in 0.1.6).

### Added

- HDR when you record just the game window. On an HDR monitor the video is HDR; on a normal monitor it is normal video and Home says why. Dragging the game to the other kind of monitor restarts the capture once and keeps the same game, and another app in front doesn't take over.
- Problem reports (Home button, desktop shortcut, `--report`), a "got stuck" report when an engine step hangs, and an encoder test that runs on its own so setup can't freeze.
- The installer brings the WebView2 setup.
- Opt-in HDR video (NVIDIA HEVC, whole monitor) and HDR screenshots saved as JPEG XR.
- Backsight-style Library menus for videos, screenshots and save folders. Open them with a right-click, the keyboard or the More actions button.
- First-run setup recommends an encoder, shows the main hotkeys and warns about NVIDIA overlay hotkey clashes. Video explains why unavailable encoders are disabled.
- Home warns when encoding falls behind and offers “Use a lighter setting.”
- Clips can use the foreground game's name, with optional per-game folders. Video can capture a chosen monitor or only the game window.
- Local crash reports and a dismissible Home notice. Reports are shared only by hand.
- An automated performance regression check runs on GitHub.
- Whole-screen screenshot hotkey (Alt+F1): save PNG/JPEG/WebP and also copy the image for Ctrl+V by default; clipboard failure preserves the saved file.
- Recording (Alt+F9): starts on the replay buffer's latest keyframe with no second encoder, works with Instant Replay on or off, shows a red "Recording" timer on Home, and quitting finishes the file. A full or slow disk stops the recording and keeps what was written.
- Pause and resume a recording (Alt+Shift+F9) without starting a new file; sound and picture skip the same span. "Split every N minutes" starts each new part on a keyframe.
- Active-window screenshot (Alt+Shift+F1).
- Region screenshot (Ctrl+Alt+F1): the screen under the mouse freezes, you drag a box, and that part is saved and copied like any screenshot. Esc or right-click cancels.
- "Convert to MP4 when done": an MKV recording is rewrapped as MP4 after you stop, without re-encoding; the MKV is kept.
- Start with Windows (off by default); started this way, Backsight opens only its tray icon.
- Record microphone and game-only audio in recordings and Instant Replay. Mute and push-to-talk use a short fade, and separate tracks keep the mix plus each enabled source.
- Benchmarks for the replay buffer, recording feed, pacer and hotkeys.
- Markers (Alt+F7) are saved as chapters in recordings, and kept through pause, split and the MP4 conversion.
- Recordings cut off by a crash or power loss are saved at the next start; the unfinished original is kept.
- Pick a microphone on the Audio page; desktop sound and the "Windows default" mic follow device changes without restarting.
- Diagnostics page with the engine's live counters, each explained.
- "Choose…" buttons for the video and screenshot folders.
- Backsight's own pop-ups and sounds for every action, a recording dot and a red tray icon; they show over games and while Do Not Disturb is on, and you can move them. Windows notifications are no longer used.
- "Reset to defaults" on each settings page.
- Diagnostics shows the recording's written bitrate and writer backlog.
- Installer (about 12 MB) with a slim FFmpeg build; upgrades keep your files, uninstall keeps your clips.
- Update button on Home for local test builds ([ADR 0009](docs/adr/0009-local-test-build-updates.md)).
- Foundation: Cargo workspace (`backsight-config`, `backsight-encode`, `backsight-engine`) and Tauri 2 shell with a tray icon and a disposable window
- Settings schema with atomic saves, schema versioning and hotkey validation
- Encoder catalog: NVENC, AMF and Quick Sync (H.264, HEVC, AV1), x264, x265, SVT-AV1 and AOM; AAC, Opus, FLAC, ALAC and PCM; 8 containers; 4 screenshot formats
- Plain-language glossary (about 90 terms) with integrity tests
- Instant Replay ring buffer with keyframe-aligned trimming and a RAM ceiling
- Settings UI: hover explanations, explained dropdowns, Simple and Advanced modes, hotkey editor with conflict detection
- Brand: logo, app icons, README banner, social preview
- Architecture docs, seven ADRs, roadmap, CI
- Hard rules for contributors, a three-tier testing strategy, platform API notes, and an explicit out-of-scope list
- Instant Replay, end to end: Alt+F10 (the default at the time) saves the last stretch of screen and desktop sound as a clip
- FFmpeg 8.1 toolchain: `scripts/fetch-ffmpeg.ps1` downloads the pinned build and checks its SHA-256; a test confirms every encoder and container in the catalog exists in it
- Encoder probe: Backsight tries each encoder once with a black frame, remembers the result per graphics driver, and a failure comes with a plain-language "requires" message (for example, which card or driver is needed)
- Screen capture with Windows.Graphics.Capture and no injection or hooks, converted to video on the GPU and handed to NVENC without copying through system memory (zero copy)
- Desktop audio loopback with silence filling for gaps, kept on the same clock as the video so sound and picture stay in sync
- Hotkeys through Raw Input, so Alt+F10 (save clip) and the replay on/off toggle work while a game has focus
- Saving a clip copies the already-encoded buffer to disk without re-encoding, so it is quick (about 15 ms in our test) and costs no GPU; files are written crash-safe and never overwrite an existing clip
- Live status on the Home page: whether Instant Replay is running or why it stopped, how much is buffered, and dropped or late frames
- Library page: saved clips and screenshots newest first, with thumbnails, a muted hover preview for videos, tabs (All, Screenshots, Videos) and filters (date, length, size, resolution, format)
- Logs in `%APPDATA%\Backsight\logs` and diagnostics counters (late frames, encoder queue, audio gaps and silence fills) to help find problems
- Tests: frame pacing, hotkey matching, option mapping, `ffprobe` file checks, audio and video sync, a killed recording still playing, and zero allocations per frame; `scripts/hardware-check.ps1` runs the GPU checks and writes a Markdown report
- Performance, measured in Tomb Raider (2013) on a test PC: saving a 2-minute clip takes about 0.25 s; with Instant Replay on, games run about 3–5% slower than with screen capture alone, inside the 5% budget
- Lighter capture path: video is converted straight from the captured screen image, without copying it first, and Windows delivers at most twice the recording frame rate
- Lightweight defaults for a fresh install: recording at 1080p60, and the encoder's advanced quality extras (look-ahead, two-pass, adaptive quantization) off until you turn them on in Advanced settings (an existing settings file that never changed these options picks up the new, lighter values); Instant Replay uses about 75 MB of memory plus the clip buffer
- `gpu_cost` test harness for measuring Backsight's FPS cost in a real game
- Replay clips save as a regular MP4 (or MOV) that shows its length in File Explorer and Photos
- Resolution and frame-rate choices each come with a plain explanation
- Opening Backsight a second time keeps the running copy's log

### Changed

- Clips recorded in game-window mode are named after the game you recorded.
- A game started from a listed launcher (for example Battle.net) takes over the game-window capture from the launcher. Launchers themselves never count as the game, and screenshots aren't named after them.
- A still screen is now lighter on NVIDIA's video chip even when the "B-frames" option in Advanced video settings is turned on.
- Instant Replay rests NVIDIA's video encoder on a still screen (about 15 pictures a second instead of 60), so a still desktop uses much less of the graphics card. Clips of a still desktop are variable frame rate.
- NVIDIA encoders default to no B-frames: lighter on the graphics card for a slightly bigger file.
- HDR video sets itself up and never stops a start; settings save instantly; hotkeys, saving and Quit never wait for the encoder.
- AMD and Intel graphics now start on their fastest preset, like NVIDIA, so games lose fewer FPS.
- A PC without an NVIDIA card now switches to a working encoder even if you skip setup, and after "Reset to defaults" on Video.
- "Split recordings every" rounds to whole minutes instead of silently blocking saves.
- Instant Replay works the graphics card much less: NVIDIA encoders now default to their fastest preset, P1, instead of P5. Our quality test scores it practically the same, and on a quiet desktop it did about half of P4's encoder work (P4, tried first, already did 40% less than P5 with the same game speed). Your saved choice is kept if you picked one.
- Built with the newest Rust (1.100) and up-to-date libraries: Tauri 2.12, the Windows library Tauri now uses, and newer GitHub build steps. FFmpeg stays at 8.1.3, the newest version the Rust FFmpeg library supports.

### Fixed

- Game-window mode no longer saves a video with no picture when no game has been found yet: it records black until the game shows up, and Home says so.
- A paused or still game window no longer records black after the capture restarts.
- Windows' file open and save box no longer counts as a game.
- Instant Replay comes back when its monitor is plugged back in. HEVC works on older NVIDIA cards (no B-frames). Messages name Backsight's real pages.
- Saving a clip after a long gap in the video (for example sleep) starts the replay over instead of making a broken clip. A failed session no longer makes clips.
- On a PC without NVIDIA, "Skip" and "Video reset" switch to an encoder that works. AMD and Intel get faster presets.
- A bad audio timestamp no longer silences a sound source, and game-only sound follows the next game.
- If Instant Replay fails to start, Backsight tries again. Hotkeys pause while you are choosing a new one.
- Settings are stamped with the current version, and after a recovery Backsight says which original files it kept.
- A briefly locked file no longer stops a save, and video that falls behind catches up sooner.
- Home notice buttons match the other settings buttons and keep their labels on one line. The overload warning no longer repeats itself.
- Preserve sound before pause and split boundaries when audio reaches the recorder after video. Waiting is bounded so a stopped audio source cannot hold video indefinitely.

### Checked

- The HDR colour test now passes on a real HDR monitor as well as on a normal screen.
