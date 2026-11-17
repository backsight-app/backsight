# Privacy

Short version: everything Backsight does stays on your PC.

## What goes online

Backsight makes no network connections, with two exceptions:

1. **Updates.** When you press **Check** under Updates on the General page, Backsight asks GitHub for the newest version and downloads it. It never checks by itself in the background. GitHub sees your internet address, as any website would.
2. **The installer's WebView2 setup.** Backsight's window needs Microsoft's WebView2 runtime. Windows 11 already has it. If your PC doesn't, the installer may download it from Microsoft. WebView2 is part of Microsoft Edge, so Microsoft may collect diagnostic data from it according to your Windows privacy settings; Backsight adds nothing to that.

There is no account, no analytics, no crash reporting and no advertising.

## What stays on your PC

Your settings, clips, screenshots, the Library's thumbnails and Backsight's log all live in folders on your PC (settings and log in `%APPDATA%\Backsight`, clips in `Videos\Backsight`, screenshots in `Pictures\Backsight`). Backsight never uploads them.

## Problem reports

**Save a problem report** writes a file to your desktop. Nothing is sent anywhere. Your Windows user name is removed from it, but it includes your settings and the names of the programs that were in front while Backsight ran (for example a game's name). **Read it before you share it**, and don't paste it into a public issue unless you are happy for everyone to see it.

## Questions

Open an issue at https://github.com/backsight-app/backsight/issues. For security problems, see [SECURITY.md](../SECURITY.md).
