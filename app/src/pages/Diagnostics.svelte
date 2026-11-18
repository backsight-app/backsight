<script lang="ts">
  // Read-only counters from the running session (see `get_diagnostics` in main.rs).
  // Not the logging invoke: "busy" every second while a session starts is expected, not a problem.
  import { invoke, isTauri } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import Icon from "../lib/components/Icon.svelte";
  import Row from "../lib/components/Row.svelte";

  type Diag = {
    buffer_seconds: number;
    buffer_bytes: number;
    buffer_kbps: number | null;
    encoded_frames: number;
    encoder_queue: number;
    dropped_frames: number;
    repeated_frames: number;
    skipped_frames: number;
    late_frames: number;
    audio: {
      silence_frames: number;
      trimmed_frames: number;
      jumped_frames: number;
      discontinuities: number;
      device_errors: number;
    } | null;
    audio_problem: string | null;
    recording: { written_kbps: number | null; queued_bytes: number; queue_limit: number } | null;
  };

  let d = $state<Diag | null>(null);
  onMount(() => {
    if (!isTauri()) return;
    // The command fails while the engine is busy; keep the last numbers then.
    const poll = () => invoke<Diag | null>("get_diagnostics").then((v) => (d = v), () => {});
    poll();
    const id = setInterval(poll, 1000);
    return () => clearInterval(id);
  });

  const n = (v: number) => v.toLocaleString();
  const mb = (bytes: number) => `${(bytes / 1048576).toFixed(1)} MB`;
  const video = $derived(
    d
      ? [
          ["diag-buffer-length", `${d.buffer_seconds.toFixed(1)} s`],
          ["diag-buffer-memory", mb(d.buffer_bytes)],
          ["diag-buffer-rate", d.buffer_kbps === null ? "Not enough yet" : `${n(d.buffer_kbps)} kbps`],
          ["diag-encoded-frames", n(d.encoded_frames)],
          ["diag-encoder-queue", n(d.encoder_queue)],
          ["diag-dropped-frames", n(d.dropped_frames)],
          ["diag-repeated-frames", n(d.repeated_frames)],
          ["diag-skipped-frames", n(d.skipped_frames)],
          ["diag-late-frames", n(d.late_frames)],
          [
            "diag-written-rate",
            !d.recording ? "—" : d.recording.written_kbps === null ? "Not enough yet" : `${n(d.recording.written_kbps)} kbps`,
          ],
          [
            "diag-writer-backlog",
            d.recording ? `${mb(d.recording.queued_bytes)} of ${mb(d.recording.queue_limit)}` : "—",
          ],
        ]
      : [],
  );
  const audio = $derived(
    d?.audio
      ? [
          ["diag-silence-fills", n(d.audio.silence_frames)],
          ["diag-audio-trimmed", n(d.audio.trimmed_frames)],
          ["diag-audio-jumps", n(d.audio.jumped_frames)],
          ["diag-audio-glitches", n(d.audio.discontinuities)],
          ["diag-audio-device-errors", n(d.audio.device_errors)],
        ]
      : [],
  );
</script>

<div class="stack">
  {#if !d}
    <div class="notice info" role="status">
      <Icon name="info" size={18} />
      <span>{isTauri() ? "Nothing is running yet. Turn on Instant Replay to see live numbers here." : "Live numbers show in the Backsight app."}</span>
    </div>
  {:else}
    {#if d.audio_problem}
      <div class="notice" role="status"><Icon name="alert" size={18} /><span>{d.audio_problem}</span></div>
    {/if}
    <section class="card">
      <header><h2>Video and replay</h2></header>
      {#each video as [term, value]}
        <Row {term}><span class="value">{value}</span></Row>
      {/each}
    </section>
    {#if audio.length}
      <section class="card">
        <header><h2>Audio</h2></header>
        {#each audio as [term, value]}
          <Row {term}><span class="value">{value}</span></Row>
        {/each}
      </section>
    {/if}
  {/if}
  {#if isTauri()}
    <div class="notice info">
      <Icon name="info" size={18} />
      <span>Something wrong? Attach the files in the logs folder to a new issue at github.com/backsight-app/backsight/issues. Your name and file names are taken out.</span>
      <button type="button" class="btn" onclick={() => invoke("open_logs_folder")}>Open logs folder</button>
    </div>
  {/if}
</div>

<style>
  .value {
    font-variant-numeric: tabular-nums;
    font-weight: 600;
  }
</style>
