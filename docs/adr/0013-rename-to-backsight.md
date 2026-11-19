# 0013. Rename the app from Clipwise to Backsight

- **Status:** Accepted
- **Date:** 2026-11-16

## Context

Before going public we found other software already called Clipwise (a paid video app and a Windows writing tool). A sound-alike such as "Clipwize" carries the same risk. Renaming is cheapest before the first public release. Existing test installs already keep settings in `%APPDATA%\Clipwise`, clips in `Videos\Clipwise`, screenshots in `Pictures\Clipwise`, a `Clipwise` "start with Windows" entry and, for installer users, a `Clipwise` entry in Add/Remove Programs.

## Decision

The app, its crates, files, art and docs are called **Backsight** (a surveyor's look back over the shoulder, as instant replay is). The bundle identifier becomes `app.backsight.desktop`.

Existing users keep everything, and nothing of theirs is deleted:
- First start: `Settings::adopt_old_name` copies `%APPDATA%\Clipwise\config` into `%APPDATA%\Backsight\config` (the old folder stays). If the copied settings load and the clip or screenshot folder was the default, it is pinned to the old `Videos\Clipwise` or `Pictures\Clipwise` folder when that exists, so the Library still shows every old clip. A file that doesn't load is copied untouched.
- `autostart::adopt_old_name` moves a `Clipwise` Run entry to the new exe.
- The installer's `NSIS_HOOK_PREINSTALL` silently runs the old Clipwise uninstaller (program files and shortcuts only; it never deletes settings or clips) and asks the user to quit Clipwise first if it is running.

The FFmpeg release keeps its old name and location until it is republished under the new name (`scripts/fetch-ffmpeg.ps1`, `FFMPEG-SOURCE.txt`, `.github/workflows/ffmpeg.yml`). *2026-11-17: republished as `ffmpeg-8.1.3-slim-r1` in `backsight-app/backsight`, which the fetch script and `FFMPEG-SOURCE.txt` name.*

## Alternatives considered

| Option | Why not |
| --- | --- |
| Keep Clipwise | Possible rename request after release; costlier later |
| Clipwize | Sounds the same, so the same risk |
| Move `Videos\Clipwise` to `Videos\Backsight` | Touches user files; fails if a clip is open; pinning is safer |

## Consequences

Old clips keep their "Clipwise …" file names and folder. Left behind on purpose (nothing deletes them): `%APPDATA%\Clipwise` (old settings, logs, crash reports, caches) and `%LOCALAPPDATA%\app.clipwise.desktop` (the old window's data, so one-time notices such as the NVIDIA key clash show once more). The installer moves the old "Start with Windows" entry before the old uninstaller removes it, and the first start trusts the copied setting. The migration code can go once no installed copy older than the rename remains.
