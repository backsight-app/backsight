# Development

## Prerequisites

| Tool | Version | Notes |
| --- | --- | --- |
| Windows | 10 2004+ or 11 | WebView2 ships with Windows 11 |
| Rust | stable, MSVC target | `rustup default stable-x86_64-pc-windows-msvc` |
| Visual Studio Build Tools | 2022 | "Desktop development with C++" workload and a Windows 11 SDK |
| Node.js | 22+ | |
| libclang | 18+ | Needed by `bindgen` for the FFmpeg bindings. See below |
| FFmpeg dev files | 8.1.3 GPL shared, slim | Fetched by `scripts/fetch-ffmpeg.ps1` |
| GitHub CLI (`gh`) | any | Signed in (`gh auth login`); fetches the FFmpeg build from the release assets |

### FFmpeg and libclang

Run `./scripts/fetch-ffmpeg.ps1` once after cloning (it uses `gh` to download the release asset). It installs Backsight's slim FFmpeg, about 44 MB of DLLs with only what Backsight uses, built and checked by `.github/workflows/ffmpeg.yml` and published with its sources as a release asset. CI, the tests, the desktop build and the installer all use this one build. It has no libvmaf, so the quality check (`quality.rs`) reports PSNR and SSIM without VMAF; for VMAF, install the full build from the commented fallback in the script into a separate checkout, and never build the installer there. The script stamps what it installs (`third_party/ffmpeg/.sha256`) and refuses a folder holding another build: move that folder aside (for a link, `cmd /c rmdir` it) and run the script again. To add an encoder, format or filter, add it to that workflow's lists and publish a new build. It puts headers, import libraries and DLLs in `third_party/ffmpeg/` (gitignored). `.cargo/config.toml` already points `rsmpeg` there with `FFMPEG_INCLUDE_DIR`, `FFMPEG_LIBS_DIR` and `FFMPEG_LINK_MODE=dynamic`, `bundle.resources` in `tauri.conf.json` copies every `third_party/ffmpeg/bin/*.dll` next to `backsight.exe` (so `tauri dev`, `tauri build` and the installer find them), and the `backsight-encode` build script declares a copy as a search path, so Cargo puts them on `PATH` for `cargo test`, `bench` and `run`. A test program started directly (as `scripts/recording-cost.ps1` does) needs `third_party/ffmpeg/bin` on `PATH`.

`bindgen` needs `libclang.dll`. Either install LLVM (`winget install LLVM.LLVM`, needs admin, found automatically) or, without admin:

```powershell
python -m pip install --user libclang
$env:LIBCLANG_PATH = "$env:APPDATA\Python\Python312\site-packages\clang\native"   # add to your profile
```

## Everyday commands

### Installer

`cd app; npm run tauri build` builds the release app and an NSIS installer at
`<target>/release/bundle/nsis/Backsight_<version>_x64-setup.exe` (the Tauri CLI downloads NSIS on first use).
It installs per user (no admin) into `%LOCALAPPDATA%\Backsight` with a Start menu shortcut and the FFmpeg DLLs.
Uninstalling preserves app data. Before installing, upgrading or uninstalling, choose **Quit Backsight**
from the tray menu and retry. A silent `setup.exe /S` exits with code 1 while Backsight is running. The same is
true of `uninstall.exe /S` only when you pass `_?=<install folder>` (last on the command line); without it NSIS copies
the uninstaller to `%TEMP%` and runs that copy, so the call returns at once with exit code 0 whatever happens
(NSIS Users Manual, 3.2.2). Upgrading skips the folder page and replaces the installed copy; `/D=<folder>` still
wins. The installer records the files it installs in `backsight-installed-files.txt` and deletes exactly those later.
Test installs in a separate Windows account or VM because the installer uses per-user
registry entries and shortcuts. See [ADR 0011](adr/0011-nsis-installer.md). Installers are unsigned.

### Building from source on a clean PC

Run `./scripts/build.ps1`. It checks the prerequisites above (Rust MSVC toolchain, C++ build tools, Node 22+, libclang), tells you in plain words what is missing, then runs `fetch-ffmpeg.ps1`, `npm ci` and `npm run tauri build`. It still needs `gh` signed in for the FFmpeg download.

### Desktop launcher

Run `./scripts/build-desktop.ps1` from PowerShell to build the release application,
copy its FFmpeg DLLs into `dist-desktop/Backsight/`, and create a **Backsight** desktop
shortcut. Open the shortcut or `Launch Backsight.bat` in the repository root.
Keep the DLLs beside `backsight.exe` when moving the application folder.

The desktop artwork is `assets/brand/backsight-desktop.svg`: a film-reel frame
around Backsight's rewind ring and recording dot. `npm run assets` renders its PNG;
`npx tauri icon ../assets/brand/backsight-desktop-1024.png -o src-tauri/icons/desktop`
(from `app/`) regenerates the application icons. These local builds are unsigned.

```powershell
# Run the app (starts Vite and the Tauri shell with hot reload)
cd app; npm ci; npm run tauri dev

# Build the UI first: the Tauri crate needs app/dist
cd app; npm run build

# Rust checks (run from the repo root)
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Benchmarks (replay buffer, recording feed, pacer, hotkeys)
cargo bench -p backsight-engine

# On a PC with a GPU: the hardware tests, written up as target/hardware-report.md
# (quit Backsight from its tray menu first)
./scripts/hardware-check.ps1

# UI only, in a browser (preview mode, nothing is saved)
cd app; npm run dev      # http://localhost:1420

# Re-render brand PNGs after editing assets/brand/*.svg
cd app; npm run assets

# Refresh README screenshots (with `npm run dev` running)
cd app; npm run screenshots

# Regenerate app icons after changing the mark
cd app; npx tauri icon ../assets/brand/backsight-mark-1024.png -o src-tauri/icons
```

The asset scripts drive the Microsoft Edge that ships with Windows through `playwright-core`, so they don't download a browser.

## Repository layout

```
.
├── app/                      Desktop app
│   ├── src/                  Svelte UI
│   │   ├── lib/components/   Term, Select, Row, OptionList, Toggle, HotkeyInput…
│   │   ├── lib/              content.ts, settings.svelte.ts, hotkeys.ts, library.ts
│   │   ├── pages/            One file per sidebar section (Home, Library, Instant Replay,
│   │   │                     Video, Audio, Screenshots, Hotkeys, General, Diagnostics)
│   │   └── Picker.svelte     The area picker for region screenshots (its own small window)
│   ├── src-tauri/            Tauri shell (tray, window lifecycle, pop-ups, IPC)
│   └── scripts/              Asset and screenshot rendering
├── crates/
│   ├── backsight-config/      Settings schema, persistence, hotkeys
│   ├── backsight-encode/      Encoder catalog, FFmpeg encode and mux, remux, screenshot encoding
│   └── backsight-engine/      Capture, audio, replay buffer, recording, recovery, screenshots, hotkeys
├── scripts/                  fetch-ffmpeg, hardware-check, build-desktop (PowerShell)
├── content/                  Shared data: glossary, encoders, default settings
├── assets/brand/             Logo, banner and social preview (SVG sources and PNGs)
└── docs/                     Architecture, ADRs, roadmap, design
```

## Conventions

- **Commits:** [Conventional Commits](https://www.conventionalcommits.org/) (`feat(engine): …`, `fix(ui): …`, `docs: …`).
- **Branches:** `feat/…`, `fix/…`, `docs/…` off `main`. CI must pass before merge.
- **Rust:** `cargo fmt`, Clippy clean, `unsafe` only in platform modules with a `// SAFETY:` comment per block.
- **Known shortcuts** are marked `// shortcut: <limit>, <when to upgrade>` so they can be listed later.
- **New settings** follow [settings-and-content.md](architecture/settings-and-content.md); new explanations follow [brand-and-voice.md](design/brand-and-voice.md).
- **Decisions** that are hard to reverse get an ADR.
- **Errors that reach users** are plain language and say what to do next ("Update your NVIDIA driver"), never a raw HRESULT alone. Log the raw detail.
- **Defaults stay light:** a fresh install must be able to pick what to capture and start, with no setup. Quality extras (look-ahead, two-pass, adaptive quantization) are off by default and stay in Advanced settings. Recording defaults to 1080p60, never upscaled.
- **New logic ships with a test** in the right tier ([testing.md](architecture/testing.md)). Before writing Windows or FFmpeg code, read [platform-notes.md](reference/platform-notes.md) and add what you learn to it.

## Hard rules

Breaking one of these needs a new ADR.

1. **No injection, no hooks.** Never add DLL injection, `Present`/swap-chain hooking, or `SetWindowsHookEx`. Capture uses Windows.Graphics.Capture (ADR 0004); hotkeys use Raw Input (ADR 0007). This keeps users safe from anti-cheat.
2. **Not OBS.** No plugin system, no scripting runtime, no scene or source graph, no overlay inside the game process. Backsight's own pop-ups are ordinary windows that never take focus (ADR 0010). Features are built in.
3. **The UI never touches Windows or FFmpeg.** It reads and writes settings and sends commands. All platform code is in engine crates.
4. **Platform code stays at the edges.** `windows`-crate calls live in small modules (`capture/wgc.rs`, `audio/wasapi.rs`, `input/raw_input.rs`). Logic such as pacing, the replay buffer, hotkey matching and option mapping stays pure Rust with unit tests.
5. **`content/` is the single source of truth** for encoders, options, defaults and explanations. Never hard-code an option list in Rust or Svelte. Every option users see needs a glossary entry; `cargo test` enforces this. View-only filters that aren't saved settings (the Library's buckets in `app/src/lib/library.ts`) are exempt.
6. **Never destroy user data.** Settings are saved atomically (temp file, then rename). A settings file that fails to parse or comes from a newer version is never overwritten. Recordings are written crash-safe; remux output goes to `.part` first.
7. **Store encoded packets, not raw frames,** in the replay buffer. The byte ceiling always holds.
8. **No per-frame heap allocation** in the capture → encode → mux hot path. Use pools.

## Troubleshooting

- **`LNK1104: cannot open file …build_script_build….exe`:** the checkout path is so long that build outputs pass Windows' 260-character limit. Clone to a shorter path, or set `CARGO_TARGET_DIR` to a short folder such as `C:\Users\<you>\cargo-target`.
- **`Unable to find libclang`:** set `LIBCLANG_PATH` (see [FFmpeg and libclang](#ffmpeg-and-libclang)).
- **`FFmpeg include dir … doesn't exits` or `STATUS_DLL_NOT_FOUND`:** run `./scripts/fetch-ffmpeg.ps1`.
- **"Start with Windows" points at the wrong exe after testing:** switching it on writes the running exe into the per-user Run key (`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, value `Backsight`). Release builds re-point it at startup; dev builds don't, so switch it off again after trying it in `tauri dev`.
- **The window is blank in `tauri dev`:** port 1420 is taken; stop the other process (Vite uses `strictPort`).
