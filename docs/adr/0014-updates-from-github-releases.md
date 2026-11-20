# 0014: Installed copies update from GitHub Releases

- Status: accepted; amended 2026-11-18 (automatic checks, see Consequences), 2026-11-20 (early channel)
- Date: 2026-11-17

## Context

People who install Backsight need new versions without hunting for a download. ADR 0009 covers only the build folder on the PC that builds it. An update that runs code from the internet must prove it came from the project, and Backsight promises not to go online by itself (`docs/privacy.md`).

## Decision

Installed copies get an **Update** button on the General page. Pressing it asks GitHub for `releases/latest/download/latest.json` (`tauri-plugin-updater`, called only from Rust commands, so the window needs no `updater:` permission). If a newer version exists, Backsight shows it with its notes. Pressing **Install** downloads the installer and checks its minisign signature against the public key in `tauri.conf.json`. Backsight then closes the way Quit does, finishing a running recording, and starts the installer in quiet mode (`/S /UPDATE /R`, amended 2026-11-18: passive mode flashed a progress window). The installer waits up to 15 s for the old Backsight to end, installs over it, and opens it again. It passes on the old copy's arguments, so a copy started with Windows (`--minimized`) would come back as a tray icon only; the first start of a new version (`last-version.txt` in the app folder) opens the window anyway, so the update is seen. The install mode comes from the old copy's settings, so the quiet install starts with updates from 0.1.9.

Every push to `main` that passes CI in the public repository becomes a release (`.github/workflows/release.yml`): version `0.1.<N>` (amended 2026-11-19: the newest `v0.1.N` tag plus one, so failed or skipped runs leave no gap; before, the run number), stamped into `tauri.conf.json` and `Cargo.toml`, built and signed by `tauri-action`. The notes are the commit subjects since the last tag (amended 2026-11-19: the top CHANGELOG.md section when CHANGELOG.md changed since the last release; only the newest `main` is released). The private signing key exists only as the `TAURI_SIGNING_PRIVATE_KEY` secret and an offline backup.

## Consequences

- ~~No background checks.~~ Changed 2026-11-18 (owner, after a friend found no way to update without hunting): installed copies check by themselves when Backsight starts, every hour, and when its window opens. Home then says "Update ready" with one **Restart to update** button. An update found in the first minutes after Backsight starts (before any game, nothing recording) installs straight away. Privacy page updated.
- A tampered or wrongly signed download is refused before anything is closed: "This download didn't pass its safety check, so nothing was installed."
- Losing the private key means existing installs can never update again. Leaking it lets anyone who can also serve `latest.json` from the repository push an update.
- Only a copy the installer put there (an `uninstall.exe` beside it) updates itself. The installer always installs to its own folder, so a copied or test folder would otherwise end up as two copies.
- Local builds have no key, so they don't make update files (`createUpdaterArtifacts` is switched on only in the release workflow). The desktop test build keeps ADR 0009's local Update.
- Only working code may reach `main`: every push there ships to every installed copy.
- If the installer can't start after Backsight has closed for it, Backsight restarts itself instead of staying half closed.

## Amendment 2026-11-20: early channel

Every push to `main` reaching all installed copies at once meant one bad build broke everyone. New versions are now published first as GitHub prereleases, which `releases/latest` skips, and listed on an `early` channel: a release with the fixed tag `early` whose asset `latest.json` names the newest prerelease. A daily Promote workflow marks a prerelease as latest for everyone once it is 48 hours old, unless a line of its notes starts with `HOLD`. General -> Updates -> **Get new versions early** (off by default) points the updater at `releases/download/early/latest.json` instead of the default endpoint; the setting is read at every check, so switching it applies to the next one. Releasing steps: [`docs/releasing.md`](../releasing.md).
