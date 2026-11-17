# 0008. Mark unfinished recordings by name and recover them by copy

- **Status:** Accepted
- **Date:** 2026-11-10

## Context

A recording is written for minutes or hours. If Backsight crashes, is killed or Windows shuts down, the file is left unfinished. Crash-safe MP4 and MKV still play up to the last fragment or cluster (platform-notes.md, "What a crash leaves behind"), so it is worth recovering. Until now a recording and a clip both used `<name>.part`, and other apps (browsers, sync tools) use `.part` too. Recovery must touch only files Backsight is sure it wrote, and never destroy anything (hard rule 6, DEVELOPMENT.md). Once users have files named this way, changing the scheme means old ones are no longer found, so this is hard to reverse.

## Decision

A recording in progress is named `<final name>.recording.part`, for example `Backsight 2026-11-10 14.03.07.mp4.recording.part` (`mux::RECORDING_PART`, `Muxer::create_recording`). Clips keep `<name>.part`, which their `Muxer` removes on failure. A recording that fails while running keeps its file (`keep_part_on_failure`).

`recovery::find_interrupted(folder)` lists only files named `Backsight <…>.<a catalog extension>.recording.part`, and skips any that another handle still has open: a recording being written, by this or another Backsight.

*Amended 2026-11-12 (M3, game names in file names):* files may start with the game's name and sit in a folder per game, so recovery recognises `<name> YYYY-MM-DD HH.MM.SS[ (n)].<a catalog extension>.recording.part`, in the video folder and the folders directly inside it. The `.recording.part` ending plus the exact time stamp still marks the file as Backsight's; older `Backsight …` files match too.

`recovery::recover(path)` refuses any other name. It copies the streams of what plays (FFmpeg stream copy, stopping at the first unreadable packet) into `<final name>.part`, which it first creates with create-new so it never overwrites anything. It checks the copy has packets, video and a length, then publishes it with a no-replace rename to the final name, or `… (2)` if that is taken. After that it renames the original to `<final name>.recovered.part`, so it isn't offered again, and removes a stray `<final name>.chapters.part` (Backsight's own unfinished marker copy, left by a crash while adding markers) if nothing has it open. Nothing else is deleted. A write error ends the copy as an error, never as a shorter file: if the copy can't be written (a full disk, say) but the original still opens with video, the unfinished copy is removed and the original keeps its name and bytes, to be tried again. An empty, corrupt or unrecoverable file is kept byte-for-byte and renamed to `<final name>.unrecoverable.part`, with a plain explanation.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Recover every `.part` in the video folder | Other apps' downloads and syncs use `.part`; we could rename or read files that aren't ours. |
| A sidecar file (`….backsight`) next to each recording | Two files to keep in step, an extra write before recording starts, and stale sidecars to clean up. The name alone is created in the same step as the file. |
| Rename the interrupted file in place (just drop `.recording.part`) | It skips verification, keeps a cut-off last fragment that some players choke on, and MKV without its index seeks badly. Copying gives a clean file and keeps the original. |
| Delete the original after a verified recovery | Rule 6: no new automatic deletion. The UI can offer to delete `.recovered.part` files. |

## Consequences

- Markers in an interrupted recording are lost: they are written when the file is finished.
- `.recovered.part` originals stay in the video folder until the user removes them; the UI should offer that.
- The ownership check depends on the `Backsight <stamp>` naming. If users can rename recordings in future, keep the `.recording.part` suffix and loosen the prefix check together.
- A recovered recording needs free space equal to the copy until the user removes the original.
