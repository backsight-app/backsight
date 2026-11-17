# Audio

All audio goes through WASAPI in shared mode, so capture never takes a device away from games or voice chat.

## Sources

| Setting | Source | API |
| --- | --- | --- |
| Desktop audio | Default render endpoint, loopback | `IAudioClient::Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK \| EVENTCALLBACK)` |
| Game audio only (replaces desktop audio; needs Desktop audio on) | The foreground process on the primary monitor once its window has kept focus for 3 s, with its children. Windows shell processes such as `explorer.exe` are skipped | `ActivateAudioInterfaceAsync` with `AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`, `PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE` |
| Microphone | Chosen capture endpoint, or the Windows default (the setting `"default"`, the first choice on the Audio page) | `IMMDeviceEnumerator` and event-driven capture |

Each source runs on its own thread, woken by the WASAPI event (10 ms periods). All three share one thread loop and packet pump in `audio/wasapi.rs` and differ only in how they open the `IAudioClient`: `wasapi::Loopback`, `microphone::Microphone` and `process_loopback::ProcessLoopback` have the same `start(.., on_audio)`, `stats()` and `stop()`. A source that can't open (no device, microphone blocked in Windows privacy settings) keeps delivering silence, retries every 500 ms and puts a plain-language `problem` in its stats. `ProcessLoopback::start` fails up front only on Windows older than 2004; it never falls back to desktop audio.

## Timing and sync

Game-only audio pins the process across window switches and automatic settings restarts. Focus the game on the primary monitor for 3 s to select it; until then the session records no desktop sound and Home asks you to focus the game. If the target exits, restart capture with the new game focused. Home shows a warning and the recording has silence where game audio is unavailable.

- Every buffer is stamped with the QPC position WASAPI reports (`GetBuffer`'s `qpcPosition`), so audio and video share one clock.
- **Loopback goes silent when nothing plays:** WASAPI simply stops delivering. The engine fills the gap with silence so audio never drifts ahead of video.
- Each source is delivered as 48 kHz float stereo. Every source asks WASAPI for that format directly (`AUTOCONVERTPCM`), so Windows does the resampling and channel conversion; process loopback has no `GetMixFormat` and needs an explicit format anyway.
- Clock drift between a device and QPC is corrected by the pure `audio/timeline.rs`: a chunk more than 10 ms late gets silence before it, a chunk more than 10 ms early is trimmed. Together with silence fill this keeps sample `n` at `start + n / 48000` on the QPC clock, which is what the encoder's sample-count timestamps assume.

## Tracks

- **Single track (default):** desktop and mic are mixed into one track. This plays correctly everywhere.
- **Separate tracks:** track 1 is everything mixed (so players still sound right), followed by each enabled source in desktop/game, then microphone order. The same tracks go into recordings and Instant Replay clips.

The session feeds the pure `audio/mixer.rs` with its enabled sources:

- `Mixer::new(start, &[SourceOptions], separate)` puts frame `n` of every track at `start + n / 48000`. Sources are listed in track order (desktop or game, then the mic); `SourceOptions` sets `muted` and `push_to_talk` for the recording.
- `push(source, samples, time)` adds a source's interleaved stereo chunk at its QPC time. A chunk within 2 frames of where the source's last one ended is joined on, so rounding to the grid never opens one-sample gaps. The part of a chunk that was already mixed is dropped.
- `mix_to(now, out)` emits chunks of up to 1024 frames as `out(Track::Mix, samples, time)`, then, with separate tracks, `out(Track::Source(i), ..)` for each source with the same time and length. It mixes as far as every source has delivered, and always up to 200 ms before `now`. A source that is late or missing there is silence, so a dead source never holds up the recording. The 200 ms sits above the timeline's 100 ms fill slack, so a quiet loopback source's silence arrives in time. After a gap of over a second (the caller stalled, or the PC slept) the time jumps forward, as the timeline does.
- **Clipping:** the sum is hard-clamped to [-1, 1] (each separate track too). It's the simplest choice that keeps the encoder's integer conversion from wrapping. A soft limiter is a recorded shortcut if loud overlaps sound harsh.
- **Allocation:** every buffer (1 s of pending audio per source, one chunk of output, 32 queued events) is sized in `Mixer::new` and never grows. A test drives late, early, oversized and stalled input and checks the capacities stay the same.

## Microphone controls

- **Mute or unmute** flips the mic's gain between 0 and 1 over 10 ms (480 samples) to avoid clicks: `Mixer::set_muted(source, muted, time)`.
- **Push to talk:** assigning a push-to-talk key enables this mode. The mic opens while the key is held, with a 150 ms release tail so word endings aren't cut: `Mixer::key(source, down, time)`. The edges ramp over 10 ms.
- Both take the event's QPC time, so a change lands on the exact sample even though mixing runs behind. An event for audio that has already been mixed takes effect at the next sample.

## Device changes

Each WASAPI thread registers an `IMMNotificationClient` (`audio/devices.rs`) for its endpoint. The callback only sets a flag; the thread then drops its stream and opens again, while its `Timeline` carries on, so a switch is a few tens of milliseconds of silence on the same clock, never a break or a session restart.

| Source | Reopens when |
| --- | --- |
| Desktop audio | The Windows default output (console role) changes |
| Microphone on "Windows default" (`"default"`) | The default microphone (console role) changes |
| A chosen microphone | That device becomes active again (plugged back in, re-enabled) |
| Game audio only | Never: it follows the process, not an endpoint |

If an endpoint fails or is unplugged, the source retries every 500 ms and supplies silence for the gap. A missing chosen microphone is never swapped for another: that would record the wrong voice without saying so; Home shows the problem instead.

`audio::devices::microphones()` lists the active capture endpoints (`MicDevice { id, name }`) for the Settings dropdown; `id` is what `audio.microphone_device` stores. The setting `"default"` (also `""` for the engine) means the Windows default. If a chosen microphone is not plugged in, the dropdown still lists it as "Chosen microphone (not plugged in)" so your choice isn't lost.
