<script lang="ts">
  import OptionList from "../lib/components/OptionList.svelte";
  import Row from "../lib/components/Row.svelte";
  import Select from "../lib/components/Select.svelte";
  import Toggle from "../lib/components/Toggle.svelte";
  import { audioEncoder, catalog, container } from "../lib/content";
  import { app } from "../lib/settings.svelte";
  import { isTauri } from "@tauri-apps/api/core";
  import { invoke } from "../lib/uiwatch";

  const a = $derived(app.settings.audio);
  const box = $derived(container(app.settings.video.container));
  const encoder = $derived(audioEncoder(a.encoder));
  const encoderChoices = $derived(
    catalog.audioEncoders.map((e: any) => ({
      value: e.id,
      label: e.label,
      glossary: e.glossary,
      disabled: !box?.audioEncoders.includes(e.id),
      disabledReason: `${box?.label} can't hold this. Try MKV or MOV.`,
    })),
  );
  type Mic = { id: string; name: string };
  let devices: Mic[] = $state([]);
  $effect(() => {
    if (isTauri()) invoke<Mic[]>("list_microphones").then((d) => (devices = d), () => (devices = []));
  });
  const mics = $derived([
    { value: "default", label: "Windows default microphone" },
    ...devices.map((d) => ({ value: d.id, label: d.name })),
    // Keep a chosen mic selectable while it's unplugged, so opening this page doesn't change it.
    ...(a.microphone_device && a.microphone_device !== "default" && !devices.some((d) => d.id === a.microphone_device)
      ? [{ value: a.microphone_device, label: "Chosen microphone (not plugged in)" }]
      : []),
  ]);

  // Captions (ADR 0016): the model download, then the on/off setting.
  type Captions = { installed: boolean; downloading: boolean; cpu_supported: boolean; download_bytes: number };
  type Progress = { done: number; total: number; finished: boolean; error: string | null };
  let cap: Captions | null = $state(null);
  let percent = $state(0);
  let capError = $state("");
  let stopping = false; // a stop the user asked for isn't shown as a problem
  const mb = $derived(cap ? Math.round(cap.download_bytes / 1e6) : 0);
  async function refreshCaptions() {
    try {
      cap = await invoke<Captions>("captions_status");
    } catch (e) {
      capError = String(e);
    }
  }
  $effect(() => {
    if (!isTauri()) return;
    refreshCaptions();
    let unlisten: (() => void) | undefined;
    let gone = false;
    import("@tauri-apps/api/event")
      .then(({ listen }) =>
        listen<Progress>("captions-download", (e) => {
          const p = e.payload;
          percent = p.total ? Math.floor((100 * p.done) / p.total) : 0;
          if (!p.finished) {
            if (cap) cap.downloading = true;
            return;
          }
          if (p.error && !stopping) capError = p.error;
          stopping = false;
          refreshCaptions();
        }),
      )
      .then((u) => (gone ? u() : (unlisten = u)));
    return () => {
      gone = true;
      unlisten?.();
    };
  });
  async function downloadCaptions() {
    capError = "";
    percent = 0;
    try {
      await invoke("download_captions");
      if (cap) cap.downloading = true;
    } catch (e) {
      capError = String(e);
    }
  }
  function stopDownload() {
    stopping = true;
    invoke("stop_captions_download");
  }
  async function removeCaptions() {
    capError = "";
    try {
      await invoke("remove_captions");
      a.captions = false;
    } catch (e) {
      capError = String(e);
    }
    refreshCaptions();
  }
</script>

<div class="stack">
  <section class="card">
    <header><h2>Sources</h2></header>
    <Row term="desktop-audio">
      <Toggle label="Desktop audio" checked={a.desktop} onchange={(v) => (a.desktop = v)} />
    </Row>
    <Row term="app-audio">
      <Toggle label="Game audio only" checked={a.game_audio_only} disabled={!a.desktop} onchange={(v) => (a.game_audio_only = v)} />
    </Row>
    <Row term="microphone">
      <Toggle label="Microphone" checked={a.microphone} onchange={(v) => (a.microphone = v)} />
    </Row>
    {#if a.microphone}
      <Row term="microphone" label="Which microphone">
        <Select name="mic" label="Microphone" value={a.microphone_device} choices={mics} onchange={(v) => (a.microphone_device = v)} />
      </Row>
    {/if}
    <Row term="audio-tracks">
      <Toggle label="Separate audio tracks" checked={a.separate_tracks} onchange={(v) => (a.separate_tracks = v)} />
    </Row>
  </section>

  {#if cap || capError}
    <section class="card">
      <header><h2>Captions</h2></header>
      {#if cap && !cap.cpu_supported}
        <Row term="captions" note="This PC's processor is too old to make captions." />
      {:else if cap?.installed}
        <Row term="captions" note={a.captions && !(a.separate_tracks && a.microphone) ? "Turn on Microphone and Separate audio tracks for clean captions." : ""}>
          <Toggle label="Captions" checked={a.captions} onchange={(v) => (a.captions = v)} />
          <button type="button" class="btn" onclick={removeCaptions}>Remove</button>
        </Row>
      {:else if cap?.downloading}
        <Row term="captions" label="Downloading captions">
          <progress max="100" value={percent} aria-label="Captions download"></progress>
          <span class="value">{percent}%</span>
          <button type="button" class="btn" onclick={stopDownload}>Stop</button>
        </Row>
      {:else if cap}
        <Row term="captions">
          <button type="button" class="btn primary" onclick={downloadCaptions}>Download captions ({mb} MB)</button>
        </Row>
      {/if}
      {#if capError}<p class="cap-error" role="alert">{capError}</p>{/if}
    </section>
  {/if}

  <section class="card">
    <header><h2>Format</h2></header>
    <Row term="audio-encoder">
      <Select name="audio-encoder" label="Audio format" value={a.encoder} choices={encoderChoices} onchange={(v) => (a.encoder = v)} />
    </Row>
    {#if encoder}
      <OptionList options={encoder.options} store={a.encoder_options} key={encoder.id} />
    {/if}
  </section>
</div>

<style>
  progress {
    width: 140px;
    accent-color: var(--accent);
  }
  .value {
    min-width: 40px;
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .cap-error {
    font-size: 13px;
    color: var(--warn);
    padding: 0 0 12px;
  }
</style>
