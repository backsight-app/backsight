# Instant Replay buffer

Implemented in [`crates/backsight-engine/src/replay.rs`](../../crates/backsight-engine/src/replay.rs) with unit tests.

## Idea

Keep **encoded packets**, not raw frames, in RAM. A raw 1080p frame is about 3 MB; an encoded one at 20 Mbps is about 40 KB. Two minutes of replay is roughly 300 MB instead of over 20 GB.

Packet bytes are appended to a ring of fixed-size chunks (2 MB at most, `max_bytes / 64` for small ceilings). Emptied chunks are kept as spares and reused, so once the buffer is full pushing a packet allocates nothing (`tests/hot_path_allocations.rs`). A chunk that a clip being saved still holds is never written to: the next packet starts a fresh chunk instead. A packet bigger than a chunk gets a chunk of its own (a `shortcut:` in the code).

Saving a clip copies `Arc` handles to those chunks (pointers, not bytes) under a short lock, then `ReplaySession::save_clip` muxes them to disk **without re-encoding**. Saving takes well under a second and costs no GPU.

## Invariants

1. The first **video** packet is always a keyframe, so every saved clip plays from its first frame. Audio for that keyframe's time may come before it (see below).
2. It holds **at least** `window` of video plus one spare GOP (keyframe interval), and at most two extra GOPs. With the default 2-second keyframes, a 2:00 replay holds 2:02–2:04.
3. A **byte ceiling** wins over the window, so a runaway bitrate (constant-quality mode on a noisy scene) can never exhaust RAM. The engine sets it from installed physical memory: the lesser of 2 GB and 25% of RAM.
4. Video before the first keyframe is discarded: it can't be decoded. Audio that arrives before it is kept (at most one window of it), and trimmed to the keyframe's decode time when the keyframe arrives.

**Why audio needs care:** the video encoder runs behind audio (look-ahead and B-frames hold about 27 frames in NVENC), so the audio for a keyframe's time reaches the buffer *before* the keyframe does. Clips therefore pick audio by timestamp, not by position, and the buffer keeps that earlier audio. Lengths (the window, a clip's cutoff) are measured from the newest **video** timestamp, so a 2:00 clip holds 2:00 of video even though audio runs ahead.

## Trimming

On every push the buffer looks at the **third** keyframe. If everything from that keyframe on still covers the window, the first GOP is dropped. Repeat. Only whole GOPs are ever removed, which keeps invariant 1 true, and the spare GOP in front of a full-length clip's keyframe holds the audio that arrived before it.

```
time ─────────────────────────────────────────────────────────▶
     K . . . . . K . . . . . K . . . . . K . . . . . K . . (now)
     └─ dropped ─┘└──────────────── window ──────────────┘
```

## Saving a clip

1. `clip(length)` finds the last keyframe at or before `now − length`. It returns that keyframe, every video packet after it, and every audio packet from the keyframe's **decode** time on. Decode time, not display time: they differ by the B-frame delay, and fragmented MP4 measures every track from its decode start.
2. `session::write_clip` muxes them into a `.part` file, rebased to start at zero, and renames it. Nothing is re-encoded.
3. A notification and an engine event tell the UI where the file is (step 8).

Audio starts within one AAC frame (21 ms) of the clip's start, and stays within 1 ms of where it belongs relative to video (`tests/save_clip.rs`).

## Memory estimate shown in the UI

`MB ≈ (video kbps + audio kbps × tracks) × seconds ÷ 8000`. The Instant Replay page shows this live; constant-quality modes say "depends on the scene" instead of guessing.

## Future work

- Index keyframe positions if profiling shows the per-push scan (about one GOP, roughly 200 packets) matters. It is marked as a `shortcut:` in the code.
- Optional spill to disk for very long replays (20 min+) on low-RAM machines.
