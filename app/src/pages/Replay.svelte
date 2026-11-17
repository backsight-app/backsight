<script lang="ts">
  import Icon from "../lib/components/Icon.svelte";
  import Kbd from "../lib/components/Kbd.svelte";
  import Row from "../lib/components/Row.svelte";
  import Toggle from "../lib/components/Toggle.svelte";
  import { catalog, estimateKbps, formatDuration, megabytes } from "../lib/content";
  import { app } from "../lib/settings.svelte";

  const r = $derived(app.settings.replay);
  const rate = $derived(estimateKbps(app.settings));
  const ram = $derived(rate.video === null ? null : megabytes(rate.video + rate.audio, r.length_seconds));
</script>

<div class="stack">
  <section class="card">
    <Row term="instant-replay">
      <Toggle label="Instant Replay" checked={r.enabled} onchange={(v) => (r.enabled = v)} />
    </Row>
    <Row term="replay-length">
      <input
        type="range"
        min={catalog.limits.replayLength.min}
        max={catalog.limits.replayLength.max}
        step={catalog.limits.replayLength.step}
        value={r.length_seconds}
        aria-label="Replay length"
        oninput={(e) => (r.length_seconds = Number(e.currentTarget.value))}
      />
      <span class="value">{formatDuration(r.length_seconds)}</span>
    </Row>
  </section>

  <section class="meter">
    <Icon name="memory" size={26} />
    <div>
      {#if ram === null}
        <strong>Memory use depends on the scene</strong>
        <p>Constant-quality mode has no fixed bitrate, so busy scenes use more RAM. Backsight caps it so your PC never runs short.</p>
      {:else}
        <strong>≈ {ram} MB of RAM while Instant Replay is on</strong>
        <p>{formatDuration(r.length_seconds)} at {(rate.video! / 1000).toFixed(1)} Mbps video + {rate.audio} kbps audio. Lower the bitrate or length to use less.</p>
      {/if}
    </div>
  </section>

  <section class="card">
    <header><h2>Hotkeys</h2><a class="link" href="#hotkeys">Change</a></header>
    <div class="keys">
      <span>Save the replay</span><Kbd combo={app.settings.hotkeys["save-replay"]} />
      <span>Turn Instant Replay on or off</span><Kbd combo={app.settings.hotkeys["toggle-replay"]} />
    </div>
  </section>
</div>

<style>
  .value {
    min-width: 72px;
    text-align: right;
    font-variant-numeric: tabular-nums;
    font-weight: 600;
  }
  .meter {
    display: flex;
    gap: 16px;
    align-items: flex-start;
    padding: 18px 22px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: linear-gradient(135deg, rgb(124 92 255 / 0.12), rgb(34 211 238 / 0.06)), var(--bg-card);
  }
  .meter :global(svg) {
    color: var(--accent-2);
  }
  .meter p {
    margin-top: 3px;
    font-size: 13px;
    color: var(--text-dim);
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
