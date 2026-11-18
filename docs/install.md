# Installing Backsight

Backsight keeps the last 2 minutes of your game ready to save, and can also record and take screenshots.

## How to install

Download the installer ("Backsight_..._x64-setup.exe") from the Releases page, then double-click it. It puts Backsight in your Start menu and sets up everything it needs. You don't need admin rights.

Backsight is free software (GPL-3.0-or-later) and comes with no warranty. It is not affiliated with any game or company. Its source code is on the same page as the installer.

## If Windows warns you

1. Your browser may say the file "isn't commonly downloaded". Choose Keep. In Edge: click the three dots next to the download, then Keep, then Show more, then Keep anyway.
2. "Windows protected your PC": click "More info", then "Run anyway". It appears because Backsight isn't signed with a paid certificate, not because something is wrong.
3. "Smart App Control blocked this app" (Windows 11 only): Windows won't run unsigned apps on that PC. There's no simple way around that one.
4. Some antivirus programs complain, because Backsight records the screen and listens for its keyboard shortcuts. If yours blocks it, allow Backsight in the antivirus.

If your PC doesn't have Microsoft's WebView2 (Windows 11 does), the installer may download it, which needs an internet connection the first time.

## First start

Backsight checks your graphics card for a few seconds. You can start right away. It records your main screen.

When you close the window, Backsight keeps running. Its icon is near the clock (click the ^ arrow if you don't see it). Right-click the icon to quit.

- Alt+X: save a clip of the last 2 minutes
- Alt+V: start or stop a recording
- Alt+S: screenshot

Backsight's Home page shows your keys. You can change them on the Hotkeys page, or pick No key to turn one off. If you used Backsight before these keys came in, you keep your old keys; if the NVIDIA app uses the same ones, Home offers to give Backsight the new ones.

When a clip is saved you hear a short sound and see "Clip saved" in a corner of the screen. Watch it in Backsight's Library.

## Good to know

- It has been tested on NVIDIA graphics with Windows 11. AMD and Intel graphics, and Windows 10, are only lightly tried. If you have one of those, your feedback is extra useful.
- Clips go to Videos\Backsight and screenshots to Pictures\Backsight. Uninstalling keeps them.
- To update, open the General page and press Check under Updates, then Install; your settings and clips stay. Backsight goes online only when you press those buttons.
- You are responsible for what you record. Check that you have the right to record other people's voices and copyrighted video before you share a clip.

## If something goes wrong: send a problem report

Make the report in one of these ways. The second also works when Backsight is stuck:

- On Backsight's Home page, click "Save a problem report".
- Start menu: open "Backsight problem report".

A report file appears on your desktop. Your Windows user name is removed from it, but it still lists your settings and the names of the programs that were in front, such as games. Read it before you share it, and don't paste it into a public post unless you are happy for everyone to see it.

Then open an issue at https://github.com/backsight-app/backsight/issues and tell us what you did, what you expected, and what happened instead. Attach the report only if you are happy with what it says. Security problems: see SECURITY.md in the repository, not a public issue.

More about privacy: docs/privacy.md in the repository.
