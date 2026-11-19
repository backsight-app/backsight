# 0011. Ship a per-user NSIS installer

- **Status:** Accepted
- **Date:** 2026-11-11

## Context

Until now Backsight ran only from a folder that `scripts/build-desktop.ps1` fills (ADR 0009). M4 needs something a normal person can double-click. The app needs the FFmpeg DLLs beside `backsight.exe`, must not ask for admin rights (it is a tray app that a user installs for themselves), and must never delete clips, screenshots or settings. The window is a WebView2 page, which Windows 11 includes and some Windows 10 PCs lack.

## Decision

Use Tauri's NSIS bundler (`bundle.targets: ["nsis"]`), configured in `app/src-tauri/tauri.conf.json`:

- **Per-user:** `installMode: currentUser`. It installs to `%LOCALAPPDATA%\Backsight`, needs no admin prompt and adds a Start menu shortcut and an entry in "Installed apps".
- **FFmpeg DLLs:** `bundle.resources` is `{ "../../third_party/ffmpeg/bin/*.dll": "" }`. Whatever DLLs `fetch-ffmpeg.ps1` leaves there go next to `backsight.exe`; no names are listed. `tauri-build` also copies them next to the built exe, so `tauri dev` and `tauri build --no-bundle` work. `ffmpeg.exe`, `ffprobe.exe` and `ffplay.exe` are not shipped. `crates/backsight-encode/build.rs` copies the DLLs into its `OUT_DIR` and declares it a search path, so Cargo puts them on `PATH` for `cargo test`, `bench` and `run` (Rust 1.100 gives each test binary its own folder).
- **WebView2:** `downloadBootstrapper`, silent. The installer only gains the small bootstrapper step; it does nothing on Windows 11 and on Windows 10 PCs that already have WebView2, and fetches the runtime from Microsoft only when it is missing. Microsoft's "Distribute your app and the WebView2 Runtime" says the bootstrapper (`MicrosoftEdgeWebview2Setup.exe /silent /install`) installs per-user when it is not run elevated, and per-machine only if it is elevated or a per-machine Edge Updater is already present. Our installer is not elevated, so no admin prompt is added and no change was needed.
- **Upgrades replace in place.** When the install-location key exists and still points at an installed `backsight.exe`, the folder page is skipped, so an upgrade overwrites the installed copy instead of leaving an orphan. An explicit `/D=<folder>` still wins. Uninstalling removes the key.
- **Autostart:** "Start with Windows" writes the running exe's path (`autostart.rs`). On every release launch the app re-writes an existing entry so it follows a move to another folder. It never creates one at startup (a stored `true` may be the old default); when the entry is gone the registry wins and the setting turns off, so after an uninstall and reinstall the user turns it on again. The uninstaller deletes the `Backsight` Run value only when it is exactly this install's `"<exe>" --minimized` command, so a portable copy's entry survives. The single-instance lock is keyed by the app identifier, not the path.
- **Uninstall keeps user data.** The custom NSIS template omits Tauri's app-data deletion option. It deletes exactly the file names the installer recorded in `backsight-installed-files.txt` (plain names in `$INSTDIR`, no wildcards), so DLLs that an older version shipped and a newer one dropped are removed on upgrade and on uninstall. Known limit: the very first installer wrote no list, so upgrading from it removes nothing old; few machines have it. It also removes the shortcuts and the install-location registry key.
- **Quit from the tray before setup.** Install and uninstall check for the current user's Backsight process and ask the user to quit from the tray and retry. A silent `setup.exe /S` exits with code 1; `uninstall.exe /S` does so only with `_?=<folder>`, because otherwise NSIS runs a copy from `%TEMP%` and the first process returns 0 at once. Upgrades replace the installed files in place so setup never calls an older uninstaller that could force Backsight to close. *Amended 2026-11-18 (0.1.7): when the installed copy is 0.1.7 or newer, an interactive install offers "Backsight is open. Close it and continue?" and runs `backsight.exe --quit`, waiting up to 60 s before falling back to the message.*

## Alternatives considered

| Option | Why not |
| --- | --- |
| WiX / MSI | Per-user MSI needs more setup, larger tooling, and gives us nothing NSIS does not for a single app. |
| `offlineInstaller` or `fixedRuntime` WebView2 | Adds about 127 MB or 180 MB to the installer to help only older Windows 10 PCs. |
| `embedBootstrapper` | Same need for internet on those PCs, adds about 1.8 MB to every download. |
| `skip` | The app would not start on Windows 10 PCs without WebView2, with no explanation. |
| Per-machine install (`both` or `perMachine`) | Needs admin; Backsight has no machine-wide parts. |
| Deleting `%APPDATA%\Backsight\logs` on uninstall | Logs are what a user sends when something broke. Keep when unsure. |

## Consequences

- Building the installer needs the FFmpeg DLLs in `third_party/ffmpeg/bin` (the build fails clearly without the link libraries anyway) and NSIS, which the Tauri CLI downloads on first use.
- A smaller FFmpeg build shrinks the installer automatically.
- The installer is unsigned, so SmartScreen warns on first run. Signing and auto-update are separate M4 items. *2026-11-18: auto-update done ([ADR 0014](0014-updates-from-github-releases.md), update files signed with minisign); Authenticode signing of the installer is still open.*
- An installed copy and a portable `dist-desktop` copy share the same autostart entry name and the same single-instance lock. Whichever copy launched last owns the entry; uninstalling the installed copy leaves it alone unless it points at the installed copy. Running both at once opens the first one's window.
- The FFmpeg DLLs are GPL builds. The installer carries Backsight's `LICENSE.txt`, every bundled library's license as a flat `LICENSE-<library>-<file>.txt`, `FFMPEG-BUILDINFO.txt` and `FFMPEG-SOURCE.txt` (where the corresponding source is). Before a public release the source must be publicly reachable: the release that holds it lives in this repository, so the repository itself must be public by then (see ADR 0005). *2026-11-18: the repository is public and the source is on the release `ffmpeg-8.1.3-slim-r1`.*
