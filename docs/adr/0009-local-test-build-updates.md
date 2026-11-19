# 0009: Local test builds update from a folder, not the internet

- Status: accepted
- Date: 2026-11-11

## Context

Local test builds run from a desktop shortcut (`dist-desktop/Backsight`). Such a copy goes stale when nothing rebuilds it, so testers end up checking an old build; it needs an Update button. A download would need a token or a public release location, and local builds have neither.

## Decision

`scripts/build-desktop.ps1` puts each build in `dist-desktop/next` with a one-line `build.txt` (time and commit). If that Backsight isn't running it installs the build right away. Otherwise Home shows "A newer build is ready" with an **Update** button. The button starts `next/apply-update.ps1` and quits. The script waits for Backsight to close (finishing a recording first), copies the files over and opens it again. Home also shows "This build: …".

Running `build-desktop.ps1` after a change keeps the newest build one click away in the test copy.

## Consequences

- Nothing goes online, there are no keys or secrets, and dev builds are unaffected (no `build.txt`).
- This only works on the PC that builds it. Public releases need signed downloads (Tauri updater plus a public release location), which needs a new ADR. *2026-11-17: done in [ADR 0014](0014-updates-from-github-releases.md).*
- Each file is copied beside the old one and then swapped in, so a failed copy never leaves a broken `backsight.exe`. A failure is logged to `%TEMP%\backsight-update.log`, shown on Home, and the waiting build stays for another try. If Backsight doesn't close within 5 minutes, nothing is installed and no second copy is started.
- Update is disabled while recording.
- Run from a linked `git worktree`, the script builds that checkout but uses the main checkout's FFmpeg and `dist-desktop`, so the desktop copy still gets the build. Two builds at once would mix files (noted as a `shortcut:`).
- Tauri and the dialog plugin are pinned to the same major.minor on the Rust and npm sides, because `tauri build` refuses a mismatch. A mismatch once kept the local build stale.
