# Changelog

Each release lists **Major** and **Minor** changes. Versions are `0.1.<build>`; the newest section is also the release's notes on the [Releases page](https://github.com/backsight-app/backsight/releases).

## 0.1.19 - 2026-11-20

### Major

- Optional early updates: General -> Updates -> Get new versions early.

### Minor

- New versions reach everyone about two days after release, once early testers have tried them.

## 0.1.18 - 2026-11-20

### Major

- The window no longer freezes for a few seconds when Backsight starts recording a new game's sound.
- Backsight can no longer hang while closing or changing its hotkeys.

### Minor

- A hotkey listener being replaced ignores keys, so an old binding never fires after you change it.

## 0.1.17 - 2026-11-20

### Major

- Damaged or unusual video files (no picture or sound, broken time stamps) are refused plainly instead of crashing Backsight or using gigabytes of memory for thumbnails, previews, captions and repairs.
- Fixed a rare memory-safety problem in game-only audio when Windows was slow to start it.
- Fixed a rare problem when screen or game-window capture stopped while a picture was arriving.

### Minor

- Freeze reports read the stuck part of Backsight correctly.
- The Windows Ease of Access box is no longer treated as a game.
- A deep check (fuzzing, mutation testing, a 3-hour run) added about 400 tests; memory and graphics memory stay steady over long sessions.

## 0.1.16 - 2026-11-19

### Major

- A recording left running while the PC sleeps no longer shows one frozen picture for the whole sleep: the gap is cut out like a pause.
- Automatic updates never stop a recording that started while the update was downloading.
- A finished clip is kept if its final rename fails, instead of being deleted.
- "Give Backsight other keys" checks the new keys first, keeps a key's old binding if another program holds the new one, and shows "Done" with the new keys.

### Minor

- Captions: no error pop-up when quitting mid-job or when sound and microphone are both off; long clips get more time; messages name the final vertical clip.
- A pop-up or recording dot whose page keeps crashing is no longer rebuilt forever.
- Hotkeys still work after their key was let go in an administrator window.
- Less background work while another program is in front of the game; less graphics memory held after switching game windows.
- Library skips Windows system folders when the clips folder is a drive root.
- Release pipeline hardened: actions pinned to exact versions, only the newest main is released, FFmpeg checked on every release.

## 0.1.14 - 2026-11-19

### Major

- No app changes.

### Minor

- README banner: current tagline and default clip key (Alt+C).
- Shorter release page header.

## 0.1.13 - 2026-11-19

### Major

- No app changes: repository and documentation release.

### Minor

- README rewritten: features, requirements, hotkeys, build steps, documentation index and credits.
- Release notes now list Major and Minor changes; the changelog uses the same layout.
- Documentation index and install guide condensed.

## 0.1.12 - 2026-11-19

### Major

- Hotkey clash detection: finds about 50 overlay, chat, recording and screenshot apps (NVIDIA app, AMD Software, Discord, Steam, OBS, ShareX, Xbox Game Bar, PowerToys and more), reads their saved keys where possible, and detects keys another program holds. Home and Setup offer **Give Backsight other keys**; the Hotkeys page lists every app's keys. Read only ([ADR 0017](docs/adr/0017-other-apps-hotkeys-read-only.md)).

### Minor

- Fallback keys: Alt+X, Ctrl+Alt+X, Alt+N, Alt+Shift+N (Alt+V is NVIDIA's chat push-to-talk).

## 0.1.11 - 2026-11-19

### Major

- Intel Quick Sync zero-copy encoding (H.264, HEVC): 60 FPS on Iris Xe.
- Quiet updates: no installer window; the first start of a new version opens the window.

### Minor

- Library: thumbnails for HDR (JPEG XR) screenshots; **Remove damaged originals** for files kept after crash recovery.
- Captions prompt includes "Backsight" and the game name for correct spelling.
- Fixed: blank thumbnails on new clips; Home's Screenshot tile capturing Backsight itself; "Use a lighter setting" sizing the wrong monitor; Windows shell panels (ShellHost) detected as a game; Open folder errors not logged.

## 0.1.8 - 2026-11-18

### Major

- Burned-in captions on vertical clips.
- Replay survives a move to another monitor; the kept footage is saved as an extra clip.

### Minor

- Notice for exclusive full screen in game-window mode.
- HDR window capture falls back to SDR after three failures; monitor-change restarts are capped.
- Fixed: browsers taking over game-window capture; resized still windows; full-drive MP4 conversion message; vertical-clip job recovery and duplicates; caption rendering edges.
- Problem reports and crash text point to GitHub issues.

## 0.1.7 - 2026-11-18

### Major

- Redesign: new Home (game, replay timeline, Record and Screenshot tiles, recent captures), dark theme, bundled Inter and Sora fonts.
- Automatic updates: checked at start, hourly and when the window opens; installed automatically when idle ([ADR 0014](docs/adr/0014-updates-from-github-releases.md)).

### Minor

- Installer offers to close a running Backsight.
- Side menu shows new Library items and free space on the clips drive.

## 0.1.4 to 0.1.6 - 2026-11-18

### Major

- Vertical clips (9:16) for Shorts, TikTok and Reels ([ADR 0015](docs/adr/0015-vertical-clips.md)).
- On-device captions with whisper.cpp, `.srt` per clip, opt-in 61 MB model ([ADR 0016](docs/adr/0016-on-device-captions.md)).
- New default hotkeys: Alt+C clip, Alt+R record, Alt+S screenshot (full table in the README).

### Minor

- **No key** option for every action.
- NVIDIA overlay clash notice with **Give Backsight other keys**.

## 0.1.3 - 2026-11-18

First public release. Covers all development up to this point; hotkeys named in older notes predate 0.1.6.

### Major

- Instant Replay: RAM ring buffer, keyframe-aligned trimming, clip saved without re-encoding in about 0.25 s.
- Recording: start, stop, pause, split every N minutes, markers as chapters, optional MKV-to-MP4 remux.
- Screenshots: screen, active window and region; PNG, JPEG, WebP; copied to the clipboard.
- Capture: Windows.Graphics.Capture, no injection or hooks; zero-copy GPU conversion to NVENC, AMF or Quick Sync. Monitor or game-window capture.
- HDR: 10-bit HEVC video (NVIDIA) and JPEG XR screenshots.
- Audio: desktop or game-only sound, microphone with mute and push-to-talk, separate tracks, device changes followed live.
- Crash recovery for interrupted recordings.
- Library: thumbnails, hover previews, tabs and filters.
- Installer (NSIS, per user, no admin) with a slim FFmpeg 8.1 build.

### Minor

- First-run setup tests the encoders and recommends a setting.
- Encoder catalog: NVENC, AMF, Quick Sync (H.264, HEVC, AV1), x264, x265, SVT-AV1, AOM; AAC, Opus, FLAC, ALAC, PCM; 8 containers.
- Clips named after the game, optional per-game folders.
- Own pop-ups, sounds and recording dot that show over games and during Do Not Disturb.
- Overload warning with **Use a lighter setting**.
- Diagnostics page with live engine counters; local crash and problem reports, never sent.
- Start with Windows (tray only); Simple and Advanced settings; Reset to defaults per page.
- Light defaults: 1080p60, NVENC preset P1, no B-frames, still-screen frame-rate reduction.
- Measured cost: about 3-5% FPS with Instant Replay on (Tomb Raider 2013); automated performance check in CI.
