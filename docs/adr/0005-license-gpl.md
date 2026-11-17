# 0005. License under GPL-3.0-or-later

- **Status:** Accepted (2026-11-08)
- **Date:** 2026-11-07

## Context

The FFmpeg build we ship (ADR 0003) includes x264 and x265, which are GPL. Distributing an app linked against a GPL FFmpeg means the app must be distributed under GPL-compatible terms.

## Decision

License Backsight as **GPL-3.0-or-later**, as OBS does (GPL-2.0-or-later).

## Alternatives considered

| Option | Trade-off |
| --- | --- |
| LGPL FFmpeg build plus a proprietary or permissive app | Allows closed source, but drops x264 and x265 (software H.264 would fall back to the weaker OpenH264). Hardware encoders, SVT-AV1 and AOM are unaffected |
| Offer x264 and x265 as an optional separate download | Keeps the app LGPL-compatible, but adds friction and complexity |

## Consequences

- Anyone may use, study and modify Backsight; derivatives must stay GPL.
- To go closed-source later, switch to an LGPL FFmpeg build and remove the software x264 and x265 options from `content/encoders.json`. Nothing else in the architecture depends on this choice.
