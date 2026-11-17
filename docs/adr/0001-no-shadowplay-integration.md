# 0001. Build our own capture stack instead of integrating ShadowPlay

- **Status:** Accepted
- **Date:** 2026-11-07

## Context

The goal is ShadowPlay's features (instant replay, recording, screenshots) without opening or even enabling the NVIDIA overlay. So we looked at whether ShadowPlay itself could be driven from our app.

- NVIDIA publishes **no API for ShadowPlay** (GeForce Experience / NVIDIA App "Instant Replay"). Its hotkeys only work while the overlay is enabled, which is exactly what we want to avoid.
- ShadowPlay captures through **NvFBC** (the NVIDIA Capture SDK). NVIDIA deprecated NvFBC on Windows 10 and later; the last supported Windows 10 build is 1803. On consumer GeForce cards it was licensed only to NVIDIA's own software. The community workaround (a "magic" key from GeForce Experience) is unsupported and legally gray.
- The building block that matters, **NVENC**, is fully public: the NVIDIA Video Codec SDK, also exposed through FFmpeg as `h264_nvenc`, `hevc_nvenc` and `av1_nvenc`.

## Decision

Build our own pipeline from public Windows and vendor APIs: Windows.Graphics.Capture for frames, WASAPI for audio, and NVENC, AMF and Quick Sync through FFmpeg for encoding. Backsight works the same whether or not NVIDIA software is installed, and it supports AMD and Intel GPUs too.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Automate ShadowPlay (send its hotkeys, watch its folder) | Requires the overlay running; fragile across driver updates; NVIDIA-only |
| NvFBC with the leaked key | Unsupported on Windows 10+, license-restricted, could break any driver update |
| OBS as a backend via obs-websocket | Ships the whole of OBS; defeats "lightweight"; settings UI would fight OBS's |

## Consequences

- We own the capture and encode code, so quality and performance are ours to get right. The capture pipeline doc sets budgets, and M1's definition of done measures them.
- Same feature set on NVIDIA, AMD and Intel.
- No dependency on the NVIDIA overlay, so the user can keep it disabled.
