# Privacy

Short version: everything Backsight does stays on your PC.

## What goes online

Backsight makes no network connections, with three exceptions:

1. **Updates.** An installed Backsight asks GitHub whether a newer version exists: when it starts, every hour while it runs, when you open its window, and when you press **Check** under Updates on the General page. The question sends nothing about you or your PC; it only downloads a small file that names the newest version. When there is one, Backsight downloads it and checks its signature. GitHub sees your internet address, as any website would.
2. **The captions download.** Captions are off until you turn them on. When you press **Download captions**, Backsight downloads its speech models (about 61 MB) from Backsight's own releases on GitHub, once, and checks that they arrived unchanged. GitHub sees your internet address, as any website would. Nothing else is ever sent: the captions are made on your PC, and your voice and clips never leave it.
3. **The installer's WebView2 setup.** Backsight's window needs Microsoft's WebView2 runtime. Windows 11 already has it. If your PC doesn't, the installer may download it from Microsoft. WebView2 is part of Microsoft Edge, so Microsoft may collect diagnostic data from it according to your Windows privacy settings; Backsight adds nothing to that.

There is no account, no analytics, no crash reporting and no advertising.

## What stays on your PC

Your settings, clips, captions, screenshots, the Library's thumbnails, the captions models and Backsight's log all live in folders on your PC (settings, models and log in `%APPDATA%\Backsight`, clips in `Videos\Backsight`, screenshots in `Pictures\Backsight`). Backsight never uploads them.

## Problem reports

**Save a problem report** writes a file to your desktop. Nothing is sent anywhere. Your Windows user name is removed from it, but it includes your settings and the names of the programs that were in front while Backsight ran (for example a game's name). **Read it before you share it**, and don't paste it into a public issue unless you are happy for everyone to see it.

## Questions

Open an issue at https://github.com/backsight-app/backsight/issues. For security problems, see [SECURITY.md](../SECURITY.md).
