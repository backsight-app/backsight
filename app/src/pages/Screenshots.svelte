<script lang="ts">
  import Kbd from "../lib/components/Kbd.svelte";
  import Row from "../lib/components/Row.svelte";
  import Select from "../lib/components/Select.svelte";
  import Toggle from "../lib/components/Toggle.svelte";
  import { catalog } from "../lib/content";
  import { app } from "../lib/settings.svelte";

  const s = $derived(app.settings.screenshot);
  const format = $derived(catalog.screenshotFormats.find((f: any) => f.id === s.format));
  const choices = catalog.screenshotFormats.map((f: any) => ({
    value: f.id,
    label: f.label,
    glossary: f.glossary,
    hint: f.hdr ? "HDR" : f.lossy ? undefined : "Lossless",
  }));
</script>

<div class="stack">
  <section class="card">
    <header><h2>Format</h2></header>
    <Row term="screenshot-format">
      <Select name="screenshot-format" label="Screenshot format" value={s.format} choices={choices} onchange={(v) => (s.format = v)} />
    </Row>
    {#if format?.lossy}
      <Row term="screenshot-quality">
        <input
          type="range"
          min={catalog.limits.screenshotQuality.min}
          max={catalog.limits.screenshotQuality.max}
          value={s.quality}
          aria-label="Image quality"
          oninput={(e) => (s.quality = Number(e.currentTarget.value))}
        />
        <span class="value">{s.quality}</span>
      </Row>
    {/if}
  </section>

  <section class="card">
    <header><h2>Behavior</h2></header>
    <Row term="copy-to-clipboard">
      <Toggle label="Copy to clipboard" checked={s.copy_to_clipboard} onchange={(v) => (s.copy_to_clipboard = v)} />
    </Row>
    <Row term="capture-cursor">
      <Toggle label="Show cursor" checked={s.capture_cursor} onchange={(v) => (s.capture_cursor = v)} />
    </Row>
  </section>

  <section class="card">
    <header><h2>Hotkeys</h2><a class="link" href="#hotkeys" aria-label="Change hotkeys">Change</a></header>
    <div class="keys">
      <span>Whole screen</span><Kbd combo={app.settings.hotkeys["screenshot"]} />
      <span>Active window</span><Kbd combo={app.settings.hotkeys["screenshot-window"]} />
      <span>Pick an area</span><Kbd combo={app.settings.hotkeys["screenshot-region"]} />
    </div>
  </section>
</div>

<style>
  .value {
    min-width: 36px;
    text-align: right;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .keys {
    display: grid;
    grid-template-columns: 1fr auto;
    align-items: center;
    gap: 12px;
    padding: 6px 0 16px;
    color: var(--text-dim);
  }
  .link {
    font-size: 13px;
    color: var(--accent-2);
    text-decoration: none;
  }
</style>
