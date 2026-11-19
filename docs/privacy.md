# Privacy

Backsight works entirely on the PC. It has no account, analytics, telemetry, crash upload or advertising.

## Network connections

Backsight connects to the internet in three cases only:

1. **Updates.** An installed copy asks GitHub whether a newer version exists: at start, every hour while running, when its window opens, and when **Check** under Updates on the General page is pressed. The request sends nothing about the user or the PC; it downloads a small file naming the newest version. When one exists, Backsight downloads it and verifies its signature before installing. GitHub sees the PC's IP address, as with any website.
2. **Captions models.** Captions are off by default. **Download captions** on the Audio page downloads the speech models (about 61 MB) once from a release of Backsight's GitHub repository and verifies their size and SHA-256. GitHub sees the PC's IP address. Captions are then made on the PC; audio and clips never leave it.
3. **WebView2 setup.** Backsight's window needs Microsoft's WebView2 runtime, which Windows 11 includes. On a PC without it, the installer downloads it from Microsoft. WebView2 is part of Microsoft Edge, so Microsoft may collect diagnostic data from it according to the Windows privacy settings; Backsight adds nothing to that.

## Local data

Settings, clips, recordings, captions, screenshots, Library thumbnails, the captions models and logs are stored on the PC: settings, models, cache and logs in `%APPDATA%\Backsight`, clips in `Videos\Backsight`, screenshots in `Pictures\Backsight` (or the folders chosen in the settings). Backsight never uploads them.

## Problem reports

**Save a problem report** writes a file to the desktop; nothing is sent. The Windows user name is removed, but the report includes the settings and the names of programs that were in front while Backsight ran, such as games. Review it before sharing, and attach it to a public issue only if its contents may be public.

## Contact

Questions: <https://github.com/backsight-app/backsight/issues>. Security issues: [SECURITY.md](../SECURITY.md).
