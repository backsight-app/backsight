# 0014: Installed copies update from GitHub Releases, only when asked

- Status: accepted
- Date: 2026-11-17

## Context

People who install Backsight need new versions without hunting for a download. ADR 0009 covers only the build folder on the PC that builds it. An update that runs code from the internet must prove it came from the project, and Backsight promises not to go online by itself (`docs/privacy.md`).

## Decision

Installed copies get an **Update** button on the General page. Pressing it asks GitHub for `releases/latest/download/latest.json` (`tauri-plugin-updater`, called only from Rust commands, so the window needs no `updater:` permission). If a newer version exists, Backsight shows it with its notes. Pressing **Install** downloads the installer and checks its minisign signature against the public key in `tauri.conf.json`. Backsight then closes the way Quit does, finishing a running recording, and starts the installer in passive mode (`/P /UPDATE /R`). The installer waits up to 15 s for the old Backsight to end, installs over it, and opens it again.

Every push to `main` that passes CI in the public repository becomes a release (`.github/workflows/release.yml`): version `0.1.<run number>`, stamped into `tauri.conf.json` and `Cargo.toml`, built and signed by `tauri-action`. The notes are the commit subjects since the last tag. The private signing key exists only as the `TAURI_SIGNING_PRIVATE_KEY` secret and an offline backup.

## Consequences

- No background checks. Backsight goes online only when someone presses Update.
- A tampered or wrongly signed download is refused before anything is closed: "This download didn't pass its safety check, so nothing was installed."
- Losing the private key means existing installs can never update again. Leaking it lets anyone who can also serve `latest.json` from the repository push an update.
- Only a copy the installer put there (an `uninstall.exe` beside it) updates itself. The installer always installs to its own folder, so a copied or test folder would otherwise end up as two copies.
- Local builds have no key, so they don't make update files (`createUpdaterArtifacts` is switched on only in the release workflow). The desktop test build keeps ADR 0009's local Update.
- Only working code may reach `main`: every push there ships to everyone who presses Update.
- If the installer can't start after Backsight has closed for it, Backsight restarts itself instead of staying half closed.
