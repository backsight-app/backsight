<p align="center">
  <img src="assets/brand/banner.png" alt="Backsight" width="100%">
</p>

<p align="center">
  <a href="https://github.com/backsight-app/backsight/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/backsight-app/backsight?style=flat-square&color=7C5CFF"></a>
  <a href="https://github.com/backsight-app/backsight/releases"><img alt="Downloads" src="https://img.shields.io/github/downloads/backsight-app/backsight/total?style=flat-square&color=7C5CFF"></a>
  <a href="https://github.com/backsight-app/backsight/actions/workflows/ci.yml"><img alt="CI" src="https://img.shields.io/github/actions/workflow/status/backsight-app/backsight/ci.yml?branch=main&style=flat-square&label=CI"></a>
  <a href="LICENSE"><img alt="License: GPL-3.0-or-later" src="https://img.shields.io/badge/license-GPL--3.0--or--later-2E3A5C?style=flat-square"></a>
  <img alt="Platform: Windows 10 and 11" src="https://img.shields.io/badge/platform-Windows%2010%20%7C%2011-22D3EE?style=flat-square">
  <img alt="Built with Rust and Tauri" src="https://img.shields.io/badge/built%20with-Rust%20%2B%20Tauri%202-DEA584?style=flat-square">
</p>

<p align="center">
  <a href="https://github.com/backsight-app/backsight/releases/latest"><strong>Download</strong></a> ·
  <a href="docs/README.md">Documentation</a> ·
  <a href="CHANGELOG.md">Changelog</a> ·
  <a href="docs/ROADMAP.md">Roadmap</a> ·
  <a href="https://github.com/backsight-app/backsight/issues/new/choose">Report an issue</a> ·
  <a href="SECURITY.md">Security</a>
</p>

Backsight is a Windows application for instant replay, screen recording, and screenshots. It captures with Windows Graphics Capture and uses GPU encoding where available, without injecting into games.

## Features

- Save recent gameplay from a configurable replay buffer or record sessions with pause, split, and chapter markers.
- Capture a monitor, game window, or screen region; save screenshots in PNG, JPEG, WebP, or JPEG XR formats.
- Use supported NVIDIA, AMD, or Intel hardware encoders, or software encoders through FFmpeg.
- Capture HDR video and create vertical clips with optional on-device captions.
- Configure global hotkeys, audio tracks, microphone controls, and per-game capture settings.
- Browse recordings in the built-in library, with thumbnails, previews, and filters.

## Screenshots

| Home | Library |
| --- | --- |
| ![Backsight home screen](docs/assets/screenshots/home.png) | ![Backsight library](docs/assets/screenshots/library.png) |

| Video settings | Hotkeys |
| --- | --- |
| ![Video settings](docs/assets/screenshots/encoder-picker.png) | ![Hotkey settings](docs/assets/screenshots/hotkeys.png) |

## Requirements

- Windows 10 version 2004 (build 19041) or later, or Windows 11.
- WebView2. Windows 11 includes it; the installer installs it if needed.
- A compatible GPU for hardware encoding. CPU encoding is also available. AMD support is available but has not yet been verified.
- AVX2-capable CPU for on-device captions.

## Install

Download and run `Backsight_<version>_x64-setup.exe` from the [latest release](https://github.com/backsight-app/backsight/releases/latest). The installer is not Authenticode-signed; Windows SmartScreen may require selecting **More info** and **Run anyway**. See the [installation guide](docs/install.md) for details.

## Build from source

See [development setup](docs/DEVELOPMENT.md) for prerequisites and build instructions. The build script checks the toolchain, downloads the pinned FFmpeg build, and creates the installer.

## Documentation and project information

The [documentation index](docs/README.md) links to user guides, privacy information, architecture and platform references, development instructions, and project records.

- [Contributing](CONTRIBUTING.md) · [Changelog](CHANGELOG.md) · [Roadmap](docs/ROADMAP.md)
- [Security policy](SECURITY.md) · [Privacy statement](docs/privacy.md)
- [License](LICENSE) · [Third-party notices](THIRD-PARTY-NOTICES.txt)

## License and acknowledgments

Backsight is licensed under [GPL-3.0-or-later](LICENSE). The bundled FFmpeg build is GPL-licensed; its corresponding source is published in the [FFmpeg release](https://github.com/backsight-app/backsight/releases/tag/ffmpeg-8.1.3-slim-r1). See [ADR 0005](docs/adr/0005-license-gpl.md) for the licensing decision.

Backsight uses Rust, Tauri, Svelte, FFmpeg, whisper.cpp, and other open-source components. Their licenses and required notices are listed in [THIRD-PARTY-NOTICES.txt](THIRD-PARTY-NOTICES.txt), also available in the app. NVIDIA, GeForce, AMD, Radeon, Intel, Discord, Steam, OBS, and Xbox are trademarks of their respective owners. Backsight is not affiliated with or endorsed by those companies or any game publisher.

## Privacy

Backsight has no account, analytics, or telemetry. Network access is used for update checks and the optional captions-model download. Problem reports are stored locally and are not sent automatically. Read the [privacy statement](docs/privacy.md).
