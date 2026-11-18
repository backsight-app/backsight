# 0016. Caption the microphone on the PC with whisper.cpp, after the save

- **Status:** Accepted
- **Date:** 2026-11-18

## Context

People want what they said in a clip written out as captions: for viewers with the sound off, and for sites that show text on the picture. The words come from the microphone, which Backsight already records as its own track when separate tracks are on.

Constraints: nothing leaves the PC (`docs/privacy.md`), so no online speech service. No hooks or injection (hard rule 1). Saving a clip stays instant and the capture path allocates nothing per frame (rule 8). A saved clip is never rewritten (rule 6). The app stays light for people who never use captions. Windows 10 2004 and later, on ordinary laptops while a game runs.

Windows' own speech tools don't fit: dictation is an online service that only listens to a live microphone, Live Captions has no programming interface, and the old offline SAPI recognizer is deprecated and far less accurate.

## Decision

**Engine.** [whisper.cpp](https://github.com/ggml-org/whisper.cpp) (MIT) through the `whisper-rs` crate (Unlicense), built from source with CMake, **CPU only**, `GGML_NATIVE=OFF` so one build runs on any CPU with AVX2 (Backsight checks the CPU first and says so plainly on older ones). The multilingual **base** model, 5-bit (`ggml-base-q5_1.bin`, 59.7 MB, about 99 languages, detected per clip), and the **Silero VAD** model (`ggml-silero-v6.2.0.bin`, 0.9 MB), which finds the parts with speech so Whisper reads only those and doesn't invent words in the silence.

**Off by default, opt-in download.** The models are not in the installer (it would more than quadruple it for everyone). A **Download captions** button fetches both files (about 61 MB) from a release of Backsight's own GitHub repository, checks their size and SHA-256 against values built into the app, and only then renames them into `%APPDATA%\Backsight\models`. This is the only network use besides updates, and only when the button is pressed; `docs/privacy.md` lists it.

**After the save, in a separate process.** When a clip has been saved, a queue (`backsight_captions::Queue`) runs `backsight.exe --captions <clip> <track> <language>`, one clip at a time. The process starts at below-normal priority, reads the models, then puts itself in Windows' background mode (lowest CPU, disk and memory priority, for every thread whisper.cpp starts). Whisper runs on a quarter of the CPU's threads, at most two. A separate process, rather than a thread, because background mode then covers whisper.cpp's own threads, its 300 MB of memory goes back to Windows when it ends, and a crash in native code can't take the replay buffer with it. Quitting Backsight kills it; a job running longer than 15 minutes is killed as stuck.

**Speech stretches.** The VAD's speech stretches are joined with 0.1 s of silence between them and transcribed in one go; times are mapped back to the clip piece by piece, and a caption is split wherever the clip pauses for a second or more. (whisper-rs's per-state call skips whisper.cpp's built-in VAD step, so this mapping lives in Backsight's pure code, `cues.rs`, with unit tests.)

**Output.** A SubRip file next to the clip with the same name (`clip.mp4` → `clip.srt`), which VLC, MPC and the Windows Media Player pick up and YouTube accepts. Lines of at most 42 characters, two per caption, each caption up at least a second. Written to `.srt.part`, then renamed without ever replacing a file of that name. The clip itself is never touched: adding a subtitle track would mean rewriting a saved file, and MP4 needs `mov_text`, which the slim FFmpeg doesn't have. Captions burned into the picture are for vertical clips only (ADR 0015's job writes a new file anyway) and are decided separately.

**Which track.** The microphone is the last audio track when separate tracks are on. Without separate tracks the only track is the mix, so game voices may be captioned too; the setting's explanation says to turn separate tracks on for clean captions.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Windows dictation (`Windows.Media.SpeechRecognition`) | Online service, live microphone only, needs the user's online-speech switch |
| Windows 11 Live Captions | No interface for apps; live only; Windows 11 only |
| SAPI 5 desktop recognizer | Deprecated, much less accurate, needs a speech language pack |
| Vosk (Kaldi) | Less accurate on casual speech, one model per language, no punctuation in the small models |
| sherpa-onnx with Parakeet v3 | More accurate, but a 15 MB runtime plus a 650 MB model, 25 languages |
| Whisper on the GPU (Vulkan or CUDA) | Competes with the game and the video encoder; CUDA needs hundreds of MB of DLLs, Vulkan a build-time SDK |
| Whisper `tiny` / `small` | `tiny` (32 MB) misses too much; `small` (190 MB) is about three times slower. Possible later choices |
| Model in the installer | 61 MB more for everyone, most of whom never turn captions on |
| A thread in the main process | Background mode and the 300 MB would stay with Backsight; a native crash would end the replay buffer |
| A subtitle track inside the clip | Rewrites a saved clip; MP4 needs `mov_text`, not in the slim FFmpeg |

## Consequences

- Building needs CMake (part of Visual Studio's C++ tools; `scripts/build.ps1` finds it). The first build of whisper.cpp adds a few minutes. The app grows by about 1.5 MB.
- Measured on a test laptop (Core i5-12500H) with other programs keeping its CPU about 75% busy: 71 s of mostly talking took 48 s in the background process, a minute with a few words 28 s; with one of the two threads at normal priority, 22–33 s and 12 s. Background mode roughly doubles the time on a busy PC; that is the price of leaving the game alone. Peak memory of the captions process about 300 MB. Details in `docs/reference/platform-notes.md`.
- Whisper is good at clear speech and weaker at slang, names and speech over loud game sound. Captions are a help, not a transcript.
- The model release must stay published, and must never be the repository's "latest" release (publish it as a pre-release), or installed copies could no longer find their updates. Changing a model means a new release name and new hashes in `model.rs`.
- Old CPUs without AVX2 get a plain message and no captions, never a crash.
