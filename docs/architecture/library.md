# Media library

The Library page lists every saved video and screenshot, newest first. The engine returns one flat list; tabs, filters, totals and sorting by other fields happen in the UI.

Implemented in [`crates/backsight-engine/src/library.rs`](../../crates/backsight-engine/src/library.rs) (scan, cache, pruning) and [`crates/backsight-encode/src/media.rs`](../../crates/backsight-encode/src/media.rs) (the FFmpeg part: probe, thumbnail, preview). Tested by `crates/backsight-engine/tests/library.rs`.

## What is scanned

- The video folder (`General::video_dir()`, default `Videos\Backsight`) and the screenshot folder (`General::screenshot_dir()`, default `Pictures\Backsight`), recursively. Symlinked folders are not followed; a file found through both folders is listed once.
- A file is a **video** if its extension is a container's `ext` in `content/encoders.json`, a **screenshot** if it is a screenshot format's `ext`. Everything else is ignored, including `.part` files (saves in progress) and files that can't be read.
- `created_ms` is the file's creation time (modified time when Windows has none). A missing folder gives an empty list.

## The cache

Everything lives in `%APPDATA%\Backsight\cache`:

| File | What |
| --- | --- |
| `library.json` | Probe results per path, valid while the file's size and modified time match. Written atomically (temp file, then rename); a broken one is rebuilt. |
| `library\<key>.jpg` | A video's thumbnail (the keyframe at about 10% in) or a JPEG XR screenshot's, at most 480 px each side. |
| `library\<key>.mp4` | A video's hover preview, made on first request. |

`<key>` is an FNV-1a hash of path, size and modified time, so an edited file gets new ones. After each scan, thumbnails and previews of files that are gone or changed are deleted, then the oldest previews (and after them thumbnails) until the folder is under 500 MB.

New files are probed and thumbnailed on up to four threads. Measured on a desktop CPU (release build): 1,000 small clips take 7.6 s the first time and 0.12 s once cached.

Screenshots need no thumbnail: the UI shows the file itself. JPEG XR (the HDR format) can't be shown by a WebView, so it gets a JPEG thumbnail, decoded through Windows Imaging Component and tone-mapped with the same highlight roll-off as the screenshot picker and clipboard.

A file that can't be read or thumbnailed is remembered as such until it changes, except in its first two minutes: right after a save, antivirus or the save itself may still hold it, so each scan tries again. Home rescans a new clip without a thumbnail twice, 3 s apart.

## Hover previews

A muted H.264 MP4 at up to 640×360, 30 fps, made of four 1.5-second snippets centred at 20, 40, 60 and 80% of the video (the whole video when it is 6 s or shorter). It is decoded in software, scaled with swscale and encoded with the catalog's x264 encoder (`VideoSession`) into the catalog's plain `mp4` container (`Muxer`, faststart), written to `.part` and renamed. One preview is made at a time. A 30 s 1080p60 clip takes about 1 s.

## Tauri side

- `list_library` and `library_preview` are async commands that run the work on a blocking thread. `library_preview` only accepts paths inside the two folders.
- `library-changed` is emitted after a clip is saved, so an open Library page can refresh.
- The WebView loads files through the asset protocol (`convertFileSrc`). `tauri.conf.json` enables it and allows `asset:` / `http://asset.localhost` in `img-src` and `media-src`; the folders are granted at start and after every settings save.
