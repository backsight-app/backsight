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
