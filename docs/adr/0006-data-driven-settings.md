# 0006. One data file drives options, UI and explanations

- **Status:** Accepted
- **Date:** 2026-11-07

## Context

Backsight exposes about a dozen encoders, each with 5–12 options, plus containers, audio formats and screenshot formats. Every one needs a label, an explanation, valid values, a default, an FFmpeg mapping, and Simple or Advanced visibility. Hand-coding these in both Rust and the UI would drift within weeks.

## Decision

Describe all of it as data in `content/encoders.json`, with explanations in `content/glossary.en.json` and defaults in `content/default-settings.json`. Rust embeds the files with `include_str!`; the UI imports them at build time. Tests enforce referential integrity (see [settings-and-content.md](../architecture/settings-and-content.md)).

## Consequences

- Adding an option is a data change plus a glossary entry; the UI renders it automatically.
- Explanations can be reviewed and translated without touching code.
- Logic that can't be expressed as data (FFmpeg value translation) lives in one small, tested function per encoder family.
