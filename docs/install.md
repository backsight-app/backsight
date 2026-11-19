# Installing Backsight

## Install

Download `Backsight_<version>_x64-setup.exe` from the [Releases page](https://github.com/backsight-app/backsight/releases/latest) and run it. Backsight installs per user into `%LOCALAPPDATA%\Backsight` with a Start menu shortcut; no admin rights are needed. If Backsight is already running, the installer offers to close it (installed copies from 0.1.7 on).

Backsight is free software (GPL-3.0-or-later) with no warranty, and is not affiliated with any game or company. Its source code is in the same repository as the releases.

## Windows and antivirus warnings

The installer is not Authenticode-signed. Update downloads are signed separately and checked by Backsight before they install.

- **Browser download warning** ("isn't commonly downloaded"): choose Keep. In Edge: **…** next to the download, **Keep**, **Show more**, **Keep anyway**.
- **"Windows protected your PC"** (SmartScreen): **More info**, then **Run anyway**.
- **"Smart App Control blocked this app"** (Windows 11): Smart App Control does not run unsigned apps. There is no per-app exception.
- **Antivirus:** some products flag screen recording and global hotkeys. Allow Backsight in the antivirus if it is blocked.

The installer downloads Microsoft's WebView2 runtime when it is missing (Windows 11 includes it), which needs an internet connection.

## First start

Backsight tests the graphics card's encoders for a few seconds and recommends a setting. Instant Replay starts at once, capturing the main monitor at 1080p60. Closing the window leaves Backsight running in the notification area; quit from its icon's menu.

| Key | Action |
| --- | --- |
| Alt+C | Save the last 2 minutes as a clip |
| Alt+R | Start or stop recording |
| Alt+S | Screenshot |

Home lists all keys; change them on the Hotkeys page. Existing settings keep their keys after an update. When another app on the PC (the NVIDIA app, AMD Software, Discord, SteelSeries GG and others) uses the same keys, Home offers **Give Backsight other keys**; Alt+R, which is also NVIDIA's and AMD's overlay key, then moves to Alt+N.

Clips and recordings go to `Videos\Backsight`, screenshots to `Pictures\Backsight`.

## Updates

Installed copies check GitHub for a new version at start, every hour and when the window opens. An update found shortly after start, while no game is in front and nothing is recording, installs by itself. Otherwise Home shows **Update ready** with **Restart to update**; **Check** under Updates on the General page checks on demand. Every update is verified against Backsight's signing key before it runs. Settings, clips and screenshots are kept. See the [privacy statement](privacy.md) for what these checks send.

## Uninstall

Uninstall from Windows Settings > Apps. Clips, screenshots, settings and logs are kept.

## Known limits

- Verified on NVIDIA GPUs and Intel Quick Sync with Windows 11. AMD encoders and Windows 10 are not yet verified on hardware. Reports from those PCs are welcome.
- Games in exclusive full screen can't be captured in game-window mode; Home asks for borderless or windowed mode.
- Users are responsible for what they record and share, including other people's voices and copyrighted material.

## Reporting a problem

Create a problem report with Home's **Save a problem report**, or from the Start menu entry **Backsight problem report** (works when Backsight is unresponsive). The report is written to the desktop; nothing is sent. The Windows user name is removed, but the report lists settings and the names of programs that were in front, such as games. Review it before sharing.

Open an issue at <https://github.com/backsight-app/backsight/issues> with what you did, what you expected and what happened, and attach the report if you are comfortable with its contents. Report security issues privately as described in [SECURITY.md](../SECURITY.md).
