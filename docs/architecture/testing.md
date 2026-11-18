# Testing strategy

Capture software fails in ways that only show up on real hardware or after long sessions: audio slowly drifting, washed-out colours, a file that won't open after a crash. Tests are organized so those failures are caught on purpose, not by users.

## Three tiers

| Tier | Runs where | When | Covers |
| --- | --- | --- | --- |
| **1. CI** | GitHub Actions `windows-latest` (**no GPU**) | Every push and PR | Pure logic, content integrity, software encoders, muxing, file validation |
| **2. Hardware check** | A real PC with a GPU (`scripts/hardware-check.ps1`) | Before merging capture or encode changes, and before releases | NVENC, AMF and QSV, WGC capture, frame cost, A/V sync on real devices |
| **3. Release checklist** | A person, about 15 minutes | Before every release | Real games, anti-cheat titles, hybrid laptops, the installer |

CI can't see a GPU, so every pipeline stage has a **CPU stand-in**: synthetic frames instead of WGC, `libx264` instead of NVENC, generated tones instead of WASAPI. The hardware check runs the same scenarios with the real components.

## What is tested

| Area | Test | Tier |
| --- | --- | --- |
| Content | Every glossary reference exists; defaults valid and compatible | 1 |
| Settings | Atomic save, round trip, newer schema refused | 1 |
| Replay buffer | Window kept, keyframe start, byte ceiling, empty-buffer clip | 1 |
| Hotkeys | Parsing, normalization, conflicts; matcher on synthetic Raw Input (left/right modifiers, held keys, stuck Alt after Alt-Tab, push-to-talk) | 1 |
| Frame pacing | Bursts and gaps, 144 FPS in, 60 out: steady output with the expected duplicates or drops | 1 |
| Option mapping | Catalog-to-FFmpeg translation per encoder family | 1 |
| File validity | `ffprobe` on saved clips: opens, expected streams and codecs, starts on a keyframe, duration within one GOP (`clip_files.rs`, `save_clip.rs`; NVENC in `replay_session.rs`) | 1 (x264), 2 (NVENC) |
| A/V sync | Flash and beep every second; flash-to-beep offset under 1 frame at start and under 2 frames after 30 min (`av_sync.rs`) | 1 (synthetic), 2 (real devices) |
| Crash safety | Kill mid-recording; the file opens and plays to the last fragment (`crash_kill.rs`; a kill inside the first GOP leaves an empty file, see platform-notes.md) | 1 |
| Recovery | A killed `.recording.part` is found (not while still written) and copied to a new file without replacing one; empty, corrupt, foreign and clip `.part` files stay byte-identical (`recovery.rs`) | 1 |
| Drive failing mid-write | Another handle locks the file being written, as a full or unplugged drive makes writes fail: a clip save fails with a plain error and leaves no file or `.part` (`write_failure.rs`); an MKV to MP4 conversion locked past its header blames the drive, keeps the MKV byte-identical and leaves nothing else (`remux.rs`); a recovery whose copy can't be written keeps the recording as it was (`recovery.rs`). A real small virtual disk needs an admin prompt (`diskpart`), so it stays manual | 1 |
| Markers | `ffprobe -show_chapters` finds markers at their times in every container with chapters and after MKV to MP4 (`chapters.rs`); markers survive pause and split (`recording_cuts.rs`) | 1 |
| Hot-path allocations | Counting allocator over a 600-frame synthetic run asserts zero allocations per frame after warm-up: pacer, x264, AAC, replay push, recording fan-out (`hot_path_allocations.rs`; FFmpeg's own allocator and the GPU path are outside it) | 1 |
| Diagnostics counters | Dropped frames, encoder queue depth, audio drift and silence fills are exposed and logged | 1 (values), 2 (real) |
| Colour | Test pattern (100% bars, grey steps, 0-255 ramp) encoded and decoded as BT.709 limited: largest delta E, ramp black level and slope. BT.601, full-range and squeezed-twice controls must be caught (`colour.rs`) | 1 (x264 controls), 2 (NVENC, GPU converter) |
| Encoder quality | Generated 1080p60 reference through x264 and NVENC H.264/HEVC; PSNR-Y and SSIM above floors, calibrated between good and bad controls that rerun every time (`quality.rs`). **Thresholds only, never byte- or bitrate-exact goldens**: hardware output changes with every driver | 2 |
| HDR | 10-bit PQ pattern: BT.2020/PQ metadata and decoded patch values (see below) | 2 |
| Frame cost | PresentMon A/B in a GPU-bound scene: WGC alone vs replay on | 2 |
| Benchmarks | `criterion` on pure hot paths (below) | local, CI compare |
| UI | `npm run build`; screenshots from `npm run screenshots`, reviewed by eye when views change | 1, manual |

## Test media

Nothing large is committed. Inputs are generated at test time by FFmpeg's built-in sources: `testsrc2` and `smptehdbars` for colour, a flash frame once per second plus `sine` beeps for sync, and a filter graph (`testsrc2`, `gradients`, `testsrc2` with seeded grain, 80 frames each) as the quality reference. `colour.rs` draws its own pattern.

## Hardware check

```powershell
./scripts/hardware-check.ps1
```

It prints the GPU, driver and Windows build, lists available encoders, runs the ignored hardware tests in release, and writes `target/hardware-report.md` (or under `CARGO_TARGET_DIR`) to attach to PRs, with a short checklist for what needs a person (other Windows audio mix formats, playing a clip in Photos, a browser and Discord).

The ignored tests: `capture_to_clip` (WGC, GPU conversion, NVENC zero copy), `replay_session` (replay end to end with desktop audio and counters), `loopback` (WASAPI), `raw_input` (hotkeys), `colour`, and `quality` (about 80 s). They play a beep and press Ctrl+Alt+Shift+F24.

Options:

- `-QualifyM1`: a 125-second replay test requesting a 120-second 1080p60 clip. It checks the real height and frame rate, requires saving in under one second, and prints `BACKSIGHT_M1_METRICS` (save time, test-process private working set, buffer bytes). The working set includes the test harness, so subtracting buffer bytes only estimates the 150 MB engine budget.
- `-ReportOnly -ReplayOffCsv base.csv -ReplayOnCsv on.csv`: compare two PresentMon captures without running tests. CSVs need `MsBetweenPresents` in ms, one game process, one swap chain and at least 30 s of intervals. Cost is `100 x (1 - on FPS / baseline FPS)`. Supplied captures never qualify a budget by themselves; matching workload and repeatability still need review. Without captures the report marks FPS cost pending, never pass.
- `./scripts/hardware-metrics.check.ps1`: offline parser checks (no GPU).

Quit Backsight first so two captures don't compete.

### Colour and quality

CPU-only checks (no desktop capture needed):

```powershell
cargo test --locked --release -p backsight-engine --test colour -- x264_colour_controls_are_told_apart --exact
cargo test --locked --release -p backsight-encode --test quality -- x264_quality_floors_catch_wrong_matrix --exact --ignored --nocapture
./scripts/hardware-metrics.check.ps1
```

GPU checks:

```powershell
cargo test --locked --release -p backsight-engine --test colour -- the_gpu_path_keeps_bt709_limited_colour --exact --ignored --nocapture
cargo test --locked --release -p backsight-encode --test quality -- encoders_at_their_defaults_clear_calibrated_quality_floors --exact --ignored --nocapture
```

**Colour pass criteria:** largest delta E at most 3.0, ramp black 13-19, slope 0.83-0.89 (limited range is 16 and 219/255 = 0.859), and every file tagged `tv`/`bt709`. Measured: the GPU path (2560x1440 BGRA, video processor, 1080p NV12, NVENC) scored delta E 0.39 and black 16.05; the BT.709 control 0.55. The BT.601 matrix (18.62), full range (6.95) and squeezed-twice (11.68) controls are all caught.

**Quality floors:** PSNR-Y 37.0 dB and SSIM 0.934 (VMAF 89 only when the linked FFmpeg has libvmaf; Backsight's slim build has none, so that floor is skipped with a message). The floors sit midway between the best bad control and the worst sample that must pass, and hold for this reference only. Bad controls (1 Mbit/s CBR, BT.601 matrix in a BT.709 file, half size stretched to 1080p) must fail at least one floor; the good control (x264 CRF 10) and every default must clear all. Measured: NVENC H.264 and HEVC defaults reach about 40.9-41.0 dB and SSIM 0.9435-0.9454; x264 defaults 39.11 dB and SSIM 0.9379. SSIM separates least (bad controls 0.913-0.931) because film grain dominates it, so PSNR-Y does most of the catching. AMF, Quick Sync and AV1 NVENC were not available for this measurement.

### HDR

`hdr_video.rs` (ignored) records a few seconds of every monitor with HDR video on (NVENC HEVC, MP4 and MKV, clip and recording). A monitor with Windows HDR on must give HEVC Main 10, `yuv420p10le`, BT.2020 and SMPTE 2084 at 60 FPS; an SDR monitor gives standard video with the Home notice. `BACKSIGHT_HDR_OUT=<folder>` keeps the files and a JPEG XR and PNG screenshot. `game_window_records_hdr_on_an_hdr_monitor` does the same in Game window mode (run it once on each kind of monitor). The shader maths is checked against a CPU formula on the GPU and on WARP by `capture::hdr` unit tests (tier 1).

**Calibrated check:** `calibrated_pattern_comes_back_with_the_drawn_codes` covers each monitor in turn with its own borderless D3D11 flip-model window (`tests/support/pattern_window.rs`) for about 6 seconds, records it, decodes the last frame and compares each patch centre with the expected limited-range Y, Cb, Cr. It prints every patch and keeps going after a miss.

- **HDR monitor:** HDR10 swap chain. Patches: PQ grey at 0, 100, 203, 400 and 1000 nits, plus BT.2020 red, green and blue at the 203-nit signal. The clip must be `yuv420p10le` BT.2020 PQ. Each patch must decode to within **3% of its own light plus 0.02 nits**. Raw codes are too strict for the primaries: PQ is so steep near black that the tiny leakage in DWM's FP16 maths shifts codes by 15.
- **SDR monitor:** sRGB swap chain; black, greys, white and BT.709 primaries. Backsight must give the Home notice and BT.709 limited `yuv420p`, within **+/-2 8-bit codes**.
- A part is skipped with a message, not failed, when no monitor of that kind is present.

Run with nothing else on the screens and the mouse out of the way (the cursor isn't recorded):

```powershell
$env:LIBCLANG_PATH="$env:APPDATA\Python\Python312\site-packages\clang\native"
cargo test -p backsight-engine --test hdr_video calibrated -- --ignored --nocapture --test-threads=1
```

`BACKSIGHT_HDR_MONITOR=2` checks only "Monitor 2" (the numbers on Backsight's Video page). A miss on the BT.2020 primaries alone, with greys right, points at DWM clipping colours outside BT.709 before capture (platform-notes.md, "HDR test pattern"). `calibrated_pattern_maths` (tier 1) checks the expected codes against the `capture::hdr` reference.

### Long recording and A/V sync

`a_long_recording_keeps_audio_and_video_together` in `replay_session.rs` (ignored; skipped by `hardware-check.ps1`; `BACKSIGHT_LONG_MINUTES` shortens it, `BACKSIGHT_LONG_MONITOR` and `BACKSIGHT_LONG_OUT` pick the monitor and output). It checks the length, no video gap over 0.1 s, both tracks running the full length, no skipped frames and no audio timeline jumps. It cannot show drift, because the timeline pins audio to the QPC clock and desktop audio is mostly silence fill.

Drift needs the **flash-to-beep test**: play a generated video with 2 white frames and a 50 ms 1 kHz beep every second full screen while the test runs, then measure beep-to-flash offsets in the recording. Result on a high refresh-rate screen over 5 minutes: 301 of 301 pairs matched, the beep leading by a steady 14.3 ms, or 31.0 ms when a flash landed one recorded frame later, the same mix every minute, so no drift. The offset includes the player's and the panel's own delay.

Use a one-minute flash-to-beep run for recording PRs and the 30-minute run for release qualification.

## Performance budgets and measured results

| Budget | Limit | Measured |
| --- | --- | --- |
| Game FPS cost of replay (1080p60) over the WGC-only floor | under 5% | recording plus replay 1.6% over capture-only on a GPU-bound game (12 rotated runs); WGC alone is about 2-3% over no capture |
| Save a 2-minute 1080p60 clip | under 1 s | 194-202 ms |
| Engine RAM | under 150 MB plus the replay buffer | 128 MiB private working set minus buffer bytes (a process-memory proxy) |
| NVENC default preset | P1 | encode engine 17% at 1080p P1 vs 33% at P4 on a quiet desktop (1440p source: 30% vs 52%); within 0.15 dB PSNR-Y and 0.001 SSIM of P4. With a game running, 1080p P1 used about 9% of the encode engine. Game FPS at P1 not yet measured |

The budget is measured over the **WGC floor**, not over "nothing running": Windows Graphics Capture alone costs a little and hard rule 1 forbids anything cheaper, so the budget covers only Backsight's own conversion, encoding and buffering. The baseline is WGC capture alone, run by the `gpu_cost` harness with `BACKSIGHT_EXP_MODE=capture` and the replay run's settings file (same fps, cursor setting and capture interval `1 / (2 x fps)`).

Also check distinct-frame cadence when comparing presets: a replay that decodes the right frame count can still repeat frames. The exclusive-fullscreen gate for recording cost is still open.

### Repeating the frame-cost measurement

Use a truly GPU-bound scene (a repeatable in-game benchmark in exclusive fullscreen on the primary monitor, `AllowsTearing = 1`, GPU busy above 99%, frame times not quantized to the refresh rate), with catalog defaults (1080p60, NVENC H.264, fragmented MP4, 2-minute replay, desktop audio). Quit Backsight, plug in a laptop's charger, close other apps and disable frame generation. Use an elevated PowerShell with the normal FFmpeg and Rust setup, and the PresentMon console and `nvidia-smi` on PATH (or pass `-PresentMon` the full path). Start the game and warm up the scene. Use an absolute `CARGO_TARGET_DIR` if reusing a build cache.

Run the kinds in a rotated order so thermal drift lands on all of them (use unused run numbers for repeats; `-DryRun` prints the plan; `-KeepMedia` keeps recorded media, otherwise it is deleted):

```powershell
./scripts/recording-cost.ps1 -Kind record -Run 1 -DryRun
./scripts/recording-cost.ps1 -Kind plain  -Run 1   # nothing capturing
./scripts/recording-cost.ps1 -Kind base   -Run 1   # WGC only
./scripts/recording-cost.ps1 -Kind replay -Run 1
./scripts/recording-cost.ps1 -Kind record -Run 1   # replay plus recording
# then runs 2 and 3, with the order rotated (e.g. record, replay, base, plain)
```

Run one line at a time. At the prompt, switch to the game and start the benchmark within 20 seconds; PresentMon measures the next 60 seconds (`-Seconds` accepts 35 or more). Keep the game focused on the primary monitor and the scene moving until the recording run saves its replay (the harness waits at least 125 s so the buffer fills). Capture runs need at least 10 new frames per second across successive 5-second samples. Check each `gpu.csv` for near-saturation; a run without sustained GPU-bound gameplay cannot qualify the budget. Use matching gameplay slices if the trace includes menus or multiple swap chains.

```powershell
$out = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { './target' }
. ./scripts/hardware-metrics.ps1
foreach ($n in 1..3) {
    Compare-PresentMon "$out/m2-base-$n/frames.csv"   "$out/m2-record-$n/frames.csv"
    Compare-PresentMon "$out/m2-replay-$n/frames.csv" "$out/m2-record-$n/frames.csv"
    Compare-PresentMon "$out/m2-plain-$n/frames.csv"  "$out/m2-base-$n/frames.csv"
    Select-String -LiteralPath "$out/m2-record-$n/harness.log" -Pattern '^BACKSIGHT_M2_RECORDING '
}
```

Base vs record is the budget (recording plus replay over the WGC floor); replay vs record isolates recording; plain vs base is the WGC floor. The recording receipt samples the process's private working set and buffer bytes before saving, then times the save with recording still running, and probes the saved clip for the same 2-minute 1080p60 output.

For a plain replay-on/off comparison: record the same scene twice with `PresentMon.exe --process_name game.exe --v1_metrics --timed 60 --terminate_after_timed --output_file base.csv` (then `on.csv` with replay on), and pass both to `hardware-check.ps1 -ReportOnly` as above.

## Benchmarks

`criterion` benchmarks cover the pure, GPU-free hot paths in `crates/backsight-engine/benches/hot_paths.rs`. CI compiles them with the normal run (`--all-targets`). Run them by hand before and after touching these paths:

```powershell
cargo bench -p backsight-engine --bench hot_paths
```

| Benchmark | What it does | Time (one PC) |
|---|---|---|
| `pacer/144hz_source_to_60fps_1s` | 1 s of a 144 Hz source into a 60 FPS pacer | 401 ns |
| `replay/push_encoded_warm_1s_of_video_and_audio` | 60 video packets (20 Mbps) plus audio into a full 2-minute buffer | 214 us |
| `replay/clip_2min` | `clip()` over a full 2-minute buffer | 180 us |
| `recording/send_drain_256_packets` | 256 x 41 KB `Feed::send` calls drained by a second thread | 480 us |
| `hotkeys/press_release_cycle` | 4 press, auto-repeat and release rounds through `Matcher` | 66 ns |
| `audio/mix_2_sources_separate_tracks_1s` | 1 s of desktop audio and the microphone (10 ms packets) mixed into three tracks | not measured yet |

### Performance check in CI (`.github/workflows/bench.yml`)

On every pull request and push to `main`, a Windows runner (no GPU) checks out the base commit beside the change and runs these benchmarks in quick mode (1 s warm-up, 3 s measuring, 30 samples) four times: base, change, base, change. Both sides share one runner, so machine speed cancels out. `scripts/bench-compare.ps1` takes the faster run of each side (a busy neighbour only slows a run), writes a table to the job summary and fails if a checked benchmark is more than 30% slower than base. `recording/send_drain_256_packets` is shown but never fails, since its time depends on thread scheduling. The first run takes about 10 minutes with a cold cache; later runs reuse it.

If it fails, run `cargo bench -p backsight-engine --bench hot_paths` before and after on your own PC. The check is only a net for big regressions; a failure you can't reproduce locally is noise, so push again.

The zero-allocation tests (`hot_path_allocations.rs`) run in the normal Rust job under `cargo test --workspace` and again as a named step. They are hard gates: no retry, no threshold.

## Release checklist (tier 3)

Screenshot slice: `cargo test -p backsight-engine --test screenshot -- --ignored --nocapture` needs an unlocked desktop and replaces the clipboard. It saves a native-resolution PNG, verifies `CF_DIB` availability, keeps the saved file while another thread holds the clipboard, and saves with copying disabled while the clipboard is busy. `region_shot_crops_the_frozen_monitor` (same command) checks a centre crop's size and that a click saves nothing. Unit checks cover PNG/JPEG/WebP encoding, malformed input, DIB row orientation and non-overwriting filenames. Pasting into Paint or Discord, real app hotkey delivery, active-window capture, the region picker window (cancel, Esc focus, negative and mixed-DPI monitors) and HDR stay manual.

- [ ] Instant Replay in a borderless DX12 game, a Vulkan game, and an anti-cheat title (e.g. one using EAC); clips play in Windows Photos, Discord and a browser
- [ ] Recording for 30+ minutes; audio still in sync at the end
- [ ] Hybrid laptop: NVENC and Quick Sync both work
- [ ] Hotkeys work with the game focused; mouse side-button binding works
- [ ] Closing the window leaves no `msedgewebview2.exe` running from Backsight
- [ ] Unplug the headset mid-recording; the recording continues
- [ ] Fresh install and uninstall; clips and settings survive uninstall
