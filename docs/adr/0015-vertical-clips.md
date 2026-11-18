# 0015. Make vertical clips from the saved clip, in the background

- **Status:** Accepted
- **Date:** 2026-11-17

## Context

People post clips to phone-first sites (YouTube Shorts, TikTok, Reels) that want a tall 9:16 picture. Today every clip is as wide as the screen. A "Clip shape" setting adds **Wide** (today's behaviour, the default), **Vertical** (only a 9:16 clip) and **Both**.

Constraints: the replay buffer stores encoded packets, never raw frames (hard rule 7), and the capture → encode → mux path allocates nothing per frame (rule 8), so a second live encode at a second size is out: it would double the GPU encoder's work all game long for clips that are rarely saved. Saving a clip must stay instant and must never wait for, or be blocked by, anything slow. Files are written crash-safe (rule 6).

## Decision

**After the save, from the saved file.** The wide clip is saved exactly as before. Then a background job (`backsight_engine::vertical`) decodes it, crops it and encodes the crop into `<wide name> (vertical).<ext>` next to it. The job never holds the save: the next clip can be saved while it runs. Jobs run one at a time, on a thread in Windows' background mode (lower CPU, disk and memory priority), with the decoder on that one thread, so a game keeps the rest of the PC. Windows shutdown is not held for it.

**The picture.** The crop is the centre of the frame, exactly 9:16, as tall as the clip (its height rounded down to a multiple of 32, so the width is a whole even number). It is kept at that size up to 1920 pixels tall and scaled down to 1080×1920 above that: never enlarged. A 1080p clip gives 594×1056, a 1440p clip 810×1440, a 4K clip 1080×1920, and a portrait monitor's 1080×1920 stays as it is. Enlarging would only add blur and bytes; the sites scale it for the phone anyway.

**Encoder.** The user's own video encoder with their settings (NVENC by default), fed from system memory, so the job uses the hardware encoder they already chose and adds no new options. HDR clips are encoded as HDR with the same encoder. **Audio** is copied unchanged (no re-encode), every track.

**Files.** The vertical clip is written to `<name> (vertical).<ext>.part` and renamed when finished (no-replace rename, ` (2)` if the name is taken), like every clip. With **Vertical**, the wide clip is first renamed to `<name> (vertical).<ext>.source.part`, a working file the user never sees in the Library. When the vertical clip is finished the working file is removed. If the vertical clip can't be made, the working file is renamed back to the wide clip's name, so footage is never lost; the user is told the clip was kept wide. If Backsight quits or crashes mid-job, the next start finds the leftover `(vertical).<ext>.part` or `.source.part` files and makes those vertical clips again (`vertical::unfinished`).

**Recordings stay wide.** Recordings can be hours long, so the job would run for a long time while people play, and a 9:16 crop of a whole session is rarely what anyone posts. Clips are short by design. Cutting a vertical clip out of a recording is a later feature.

**Library.** A vertical clip is an ordinary video file, so the Library lists it with no new bookkeeping; any video taller than it is wide gets a "Vertical" badge.

**Errors.** Plain language, saying what to do: "Backsight couldn't make the vertical clip. The wide clip is saved. Try another encoder on the Video page." The raw FFmpeg error is logged.

## Alternatives considered

| Option | Why not |
| --- | --- |
| A second live encode at 9:16 | Doubles encoder work all the time for clips rarely saved; a second buffer; rules 7 and 8. |
| Crop inside the save, before the clip is published | The save would wait seconds to minutes; a failure could cost the clip. |
| libavfilter (`crop`, `scale`) graph | Works (the slim build has both), but the crop is a pointer offset (`av_frame_apply_cropping`) and swscale already scales the Library previews: less code, one fewer library to drive. |
| Always 1080×1920, enlarging small crops | Blurry and bigger for no gain on a 1080p clip; the sites rescale anyway. |
| Delete the wide clip after a Vertical save | Backsight never deletes a file it has shown the user. The working-file rename keeps the wide picture out of sight until it can be dropped safely. |
| Vertical recordings too | Long jobs during play; see above. |

## Consequences

- Cost: one CPU core for about as long as the clip, plus a little hardware-encoder time. Measured on a test PC: 20 s of noisy 1080p60 at 20 Mbps decoded on one thread in 17 s, and in 18 s with the crop and NVENC encode added, so a 2-minute clip takes about 2 minutes, at background priority. Memory stays near 300 MB for the job. Nothing runs when the shape is Wide.
- A clip saved as Vertical shows up a little later than a wide one: the pop-up says when it is ready.
- The job uses the encoder of the moment (settings at save time); a broken encoder now fails the vertical clip, never the wide save.
- Smart framing (following the action instead of the centre) can replace the fixed centre crop later without changing the files.
