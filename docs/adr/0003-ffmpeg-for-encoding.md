# 0003. FFmpeg (libav*) for encoding and muxing

- **Status:** Accepted
- **Date:** 2026-11-07

## Context

We need NVENC, AMF and Quick Sync in H.264, HEVC and AV1; x264, x265, SVT-AV1 and AOM on the CPU; AAC, Opus, FLAC, ALAC and PCM audio; and MP4, fragmented MP4, MKV, MOV, MPEG-TS, WebM and FLV containers. All with fine-grained options (presets, tuning, multipass, look-ahead, AQ, B-frames).

## Decision

Link **FFmpeg's libraries** (libavcodec, libavformat, libavutil, libswresample) through the [`rsmpeg`](https://crates.io/crates/rsmpeg) bindings (FFmpeg 8 feature). Ship a **win64 GPL shared** build pinned by version and SHA-256 in `scripts/fetch-ffmpeg.ps1`. Since M2 that is Backsight's own slim FFmpeg 8.1.3, built and published with its sources by `.github/workflows/ffmpeg.yml` (M1 used a ready-made full build). D3D11 textures go straight into hardware encoders through `AVHWFramesContext` (see [capture-pipeline.md](../architecture/capture-pipeline.md)).

## Alternatives considered

| Option | Why not |
| --- | --- |
| Vendor SDKs directly (NVIDIA Video Codec SDK, AMF, oneVPL), like Sunshine | Three integrations to maintain, plus our own muxers; only worth it for a codec feature FFmpeg lacks |
| Media Foundation | Built in, but few codecs and options, inconsistent vendor MFTs, no AV1 software encoder, weak muxer control |
| GStreamer | Capable, but a heavy runtime and a second plugin system to ship |
| Pipe raw frames to `ffmpeg.exe` | Copies every frame through the CPU and a pipe; no zero-copy; process management pain |

## Consequences

- One API for every encoder, and the catalog's `ffmpeg` field maps options 1:1 in most cases.
- About 44 MB of DLLs in the installer. That is the biggest part of our download size, and acceptable.
- Building needs **libclang** (for `bindgen`) and the FFmpeg dev files; [DEVELOPMENT.md](../DEVELOPMENT.md) covers setup.
- The GPL build forces the GPL license question: see [ADR 0005](0005-license-gpl.md).
