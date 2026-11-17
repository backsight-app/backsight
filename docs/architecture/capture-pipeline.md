# Capture pipeline

From pixels on screen to packets in a file, with the GPU doing nearly all the work.

## 1. Choosing the GPU

The D3D11 device is created on the **adapter that will encode**, so captured textures go to the encoder without leaving that GPU.

- Enumerate adapters with `IDXGIFactory6::EnumAdapterByGpuPreference(HIGH_PERFORMANCE)`.
- Map the chosen encoder's vendor (`nvidia`, `amd`, `intel` in `content/encoders.json`) to the adapter's PCI vendor id (`0x10DE`, `0x1002`, `0x8086`).
- **Hybrid laptops** (an Intel chip plus an NVIDIA card): Windows composites the desktop on one GPU while the game may render on the other. WGC copies frames across adapters for us, so we always pick the encoder's adapter and accept that one copy. Quick Sync on the iGPU is a valid choice that keeps the dGPU free for the game.

## 2. Capture: Windows.Graphics.Capture (WGC)

| What | API |
| --- | --- |
| Monitor or window target | `IGraphicsCaptureItemInterop::CreateForMonitor` / `CreateForWindow`. Instant Replay and recording capture the monitor picked on the Video page (the main one if it is gone), or the game's window in window mode; window capture is also used for the active-window screenshot |
| Frames | `Direct3D11CaptureFramePool::CreateFreeThreaded`, 2 buffers, `B8G8R8A8UIntNormalized` (standard video; HDR video uses `R16G16B16A16Float`) |
| Cursor | `GraphicsCaptureSession.IsCursorCaptureEnabled` |
| No yellow border | `IsBorderRequired = false` (Windows 11; the call is skipped where Windows lacks it) |
| Frame cap | `MinUpdateInterval` (Windows 11 24H2+, set only when Windows has it); our pacer also drops extra frames |

Why WGC and not injection or NvFBC: see [ADR 0001](../adr/0001-no-shadowplay-integration.md) and [ADR 0004](../adr/0004-wgc-capture-no-injection.md).

**Pacing.** WGC only delivers a frame when the screen changes. The encoder needs constant frame rate, so a pacer ticks at the target FPS on the QPC clock: it re-sends the last texture when nothing new arrived and drops extra frames when the game outruns the target.

## 3. Convert and scale on the GPU

`ID3D11VideoProcessor` converts BGRA to **NV12** (8-bit) and scales to the output resolution in one blit. The output texture and the input views are reused, so nothing is allocated per frame. For HDR video a compute shader (`capture/hdr.rs`) writes 10-bit P010 instead, since the video processor can't.

Color: BT.709 limited range (SDR). It is written into the stream's color metadata so players show the right colors. HDR video is BT.2020 PQ.

## 4. Encode with FFmpeg (libavcodec)

Encoders are configured entirely from the catalog. Each catalog option has an `ffmpeg` field naming the AVOption it maps to.

| Encoder family | GPU input path | Notes |
| --- | --- | --- |
| NVENC (`h264_nvenc`, `hevc_nvenc`, `av1_nvenc`) | `AV_PIX_FMT_D3D11` frames from an `AVHWFramesContext` built on **our** `ID3D11Device` | Zero-copy. Rate-control value `cq` maps to `rc=vbr` + `cq=N` + `b=0` |
| AMF (`*_amf`) | `AV_PIX_FMT_D3D11` | Zero-copy path in code, not checked on AMD hardware yet. `cq` maps to `rc=qvbr` |
| Quick Sync (`*_qsv`) | Planned: `av_hwframe_map` D3D11 → QSV (derived device). Today frames are copied to system memory like the CPU encoders | Not checked on Intel hardware yet |
| CPU (`libx264`, `libx265`, `libsvtav1`, `libaom-av1`) | Copy NV12 to a staging texture, map, feed `yuv420p` | One GPU→CPU copy per frame |

Mapping rules that are not 1:1 live in one function per family in `backsight-encode` (`ffmpeg_options(family, resolved) -> AVDictionary`) with unit tests, for example `keyint_sec = 0` means 2 seconds, and seconds become frames (`g = fps × keyint_sec`).

**Encoder probe.** `backsight_encode::probe` tries each catalog encoder on a small test frame and returns a plain-language reason when one fails ("Needs an RTX 40-series GPU"). `setup::check_encoders` runs it in child processes with a time limit, so a frozen driver can't hang the app, and the setup page shows the results. Working results and freezes are cached per GPU driver version; other failures are tried again each time.

**First run.** A fresh install uses the encoder in `content/default-settings.json` (NVENC H.264, 1080p60). If the check finds it doesn't work here, `setup::pick_encoder` falls back to x264 (CPU). It switches without asking when the PC has no graphics card from the encoder's maker, and otherwise only offers the switch. A working choice is never overridden.

## 5. Mux (libavformat)

- The container comes from the catalog (`muxer`, `muxerOptions`). Crash-safe MP4 and MOV use `movflags=frag_keyframe+empty_moov+delay_moov+default_base_moof` (`delay_moov` keeps tracks in sync; see platform-notes.md). Replay clips use the container's `clipMuxerOptions` (none, so plain MP4/MOV without faststart) instead: they are written from RAM in milliseconds through a `.part` file, Windows (Explorer, Photos) shows no length for fragmented files, and faststart's second pass would add about 2 s to a 2-minute clip.
- **Recordings** are written by a thread of their own, fed from the encoders through a bounded queue of reused buffers (up to 64 MB). While a recording is running the file is named `<final name>.recording.part`; stopping renames it. If the disk can't keep up, the recording stops and keeps what was written ([ADR 0008](../adr/0008-interrupted-recording-files.md) covers recovery).
- **Pause and resume** leave the same span out of sound and picture. Video resumes on the next keyframe, which the encoder is asked for, and audio resumes within one packet of it.
- **Convert to MP4 when done** (on by default; it only applies to MKV recordings) rewraps the MKV as MP4 after you stop, without re-encoding. The engine side is `backsight_encode::remux`. `remux_to_mp4_report` copies every video and audio stream and the chapters (`+faststart`) into `<name>.mp4.part`, then renames it to `<name>.mp4` or `<name> (2).mp4` so nothing is replaced. It drops streams MP4 can't hold (subtitles, attachments) and lists them. `remux_checked` is what the app calls: it keeps the MP4 only if nothing was dropped and the length matches the MKV within a second. **The MKV is always kept.** On any failure the `.part` is removed and a notification says the recording stays as MKV. Track titles and languages are not copied yet. Before converting, the app leaves an empty `<name>.mkv.to-mp4.part` mark (`recovery::mark_conversion`), removed once the conversion returns. Quitting or shutting down during the conversion leaves the mark, and the next start tries the conversion again using an unused name (`recovery::unfinished_conversions`, `finish_conversion`). Existing partial files are kept because the mark cannot identify which conversion or clip wrote them.
- **Split recordings** open a new file on the first keyframe after N minutes (Settings → Video; the value is read when the recording starts). Each part is named by the time it starts.
- **Markers** (Alt+F7) become chapters named "Start", "Marker 1", "Marker 2"… in MP4, MOV and MKV, at the hotkey's time (moved back by the time paused). MPEG-TS and FLV can't hold chapters, and Backsight says so. A crash-safe MP4 or MOV writes its index first, so a recording with markers is copied once more when it stops, as plain MP4/MOV with the chapters in place. Markers are written only when the file is finished, so an interrupted recording loses them.
- **Interrupted recordings** (a crash, a kill, a power loss) are found at the next start by name, copied into a normal file with FFmpeg stream copy, and checked before they are published. The original is renamed `<final name>.recovered.part` and never deleted.

File names: `Backsight <YYYY-MM-DD HH.MM.SS>.<ext>`, with ` (2)`, ` (3)` added if that name is taken. Clips and recordings go into the video folder (Settings → General, picked with the "Choose…" button). Names can use the foreground program's name, with optional per-game folders.

## 6. Screenshots

- **Implemented:** the whole-screen hotkey (Alt+F1 by default) takes a one-shot WGC screenshot of the monitor showing the focused window (the primary monitor if there is none), including when replay is off. Capture and saving run off the input thread; repeated presses during a save are ignored. It honors the screenshot cursor setting.
- **Encode:** the linked FFmpeg build writes PNG, JPEG or WebP at native resolution; JPEG/WebP honor screenshot quality. A complete `.part` is flushed and closed before a same-directory, no-replace rename publishes it. Existing screenshots are never overwritten.
- **Clipboard:** with Copy to clipboard enabled (the default), the same opaque image pixels are copied as `CF_DIB`, so Ctrl+V pastes an image rather than a filename. The file is saved first; clipboard failure keeps it and reports a separate warning. The Library refreshes after saving.
- **Active window** (`screenshot::take_window`, hotkey action `ScreenshotWindow`): a WGC window item for the focused top-level window, title bar included, without the shadow or windows covering it, at the window's physical pixel size. No focus, the desktop (`Progman`/`WorkerW`), a taskbar, a minimized window or a Backsight window falls back to the whole monitor. Same save and clipboard path as the whole-screen shot.
- **Region** (`screenshot::freeze` then `take_region`, hotkey action `ScreenshotRegion`, Ctrl+Alt+F1): one WGC capture of the monitor under the mouse, frozen in memory. The app opens a disposable, hidden, borderless, topmost window of its own (label `picker`, never an in-game overlay) over that monitor in physical pixels, sends it the frame as an uncompressed BMP, and shows it once the picture is drawn. Dragging picks an area; mouse-up or Enter saves it, Esc, right-click or switching away cancels, and a drag without area cancels too. Rust crops the frozen frame (no second capture) using image ÷ view as the scale, so the crop matches what was shown at any DPI or text size; then the same save, clipboard and notification path as the other shots. Closing the picker never quits Backsight.
- **Still planned:** capture-texture reuse and an additional PNG clipboard format. HDR screenshots (JPEG XR) are covered in platform-notes.

## Risks and how we handle them

| Risk | Mitigation |
| --- | --- |
| Exclusive-fullscreen legacy games (D3D9) show black in window capture | Video already captures the whole monitor. The window screenshot falls back to the monitor when it can't capture a window |
| Encoder overload (CPU encoders, high res) | The Diagnostics page shows the encoder queue, which keeps growing when the encoder can't keep up. Home shows a plain-English warning with a one-click "Use a lighter setting" fix |
| Driver resets (`DXGI_ERROR_DEVICE_REMOVED`) | Any capture or encoder failure stops the session, keeps the replay buffer and shows a plain-language reason on Home. Rebuilding the device automatically is not done yet |
| HDR color errors | Golden-image tests on a known HDR test pattern (M3) |
