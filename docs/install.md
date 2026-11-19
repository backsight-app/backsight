# Install

## Install

Download `Backsight_<version>_x64-setup.exe` from the [latest release](https://github.com/backsight-app/backsight/releases/latest) and run it.

- Per-user install to `%LOCALAPPDATA%\Backsight`, Start menu shortcut, no admin rights.
- A running Backsight (0.1.7 or later) is closed by the installer after confirmation.
- WebView2 is downloaded from Microsoft when missing (included in Windows 11).

## Warnings

The installer is not Authenticode-signed. Updates are signed separately and verified before they install.

| Warning | Action |
| --- | --- |
| Browser: "isn't commonly downloaded" | Keep the file |
| SmartScreen: "Windows protected your PC" | **More info → Run anyway** |
| Smart App Control (Windows 11) | Blocks unsigned apps; no per-app exception |
| Antivirus | Some products flag screen capture or global hotkeys; allow Backsight |

## First start

Backsight tests the encoders, recommends a setting and starts Instant Replay (main monitor, 1080p60). Closing the window keeps it in the tray. Default keys: Alt+C clip, Alt+R record, Alt+S screenshot; all keys are on the Hotkeys page.

When another app uses the same keys, Home offers **Give Backsight other keys** (fallbacks: Alt+X clip, Ctrl+Alt+X replay on/off, Alt+N record, Alt+Shift+N pause).

Output: clips and recordings in `Videos\Backsight`, screenshots in `Pictures\Backsight`.

## Updates

Checked at start, hourly and when the window opens. Installed automatically shortly after start when no game is in front and nothing is recording; otherwise Home shows **Restart to update**. Manual check: General → Updates → **Check**. Settings, clips and screenshots are kept. See [privacy](privacy.md).

## Uninstall

Windows Settings → Apps. Clips, screenshots, settings and logs are kept.

## Known limits

- Verified on NVIDIA and Intel Quick Sync with Windows 11; AMD encoders and Windows 10 not yet verified on hardware.
- Game-window capture can't record exclusive full screen; use borderless or windowed.
- Users are responsible for what they record and share.

## Problem reports

Home → **Save a problem report**, or the Start menu entry **Backsight problem report** (works when Backsight hangs). The file is written to the desktop and never sent. The Windows user name is removed; settings and the names of foreground programs remain, so review it before attaching it to an [issue](https://github.com/backsight-app/backsight/issues). Security issues: [SECURITY.md](../SECURITY.md).
