<script lang="ts">
  // Renders catalog options (content/encoders.json) for one encoder.
  import { isVisible, resolved, type OptionDef } from "../content";
  import { app } from "../settings.svelte";
  import Row from "./Row.svelte";
  import Select from "./Select.svelte";
  import Toggle from "./Toggle.svelte";

  // `limits`: options this PC caps (option id → most), shown at the cap and switched off.
  let {
    options,
    store,
    key,
    limits = {},
  }: { options: OptionDef[]; store: Record<string, any>; key: string; limits?: Record<string, number> } = $props();

  const values = $derived(resolved(options, store[key]));
  const shown = $derived(options.filter((o) => isVisible(o, values, app.advanced)));

  function set(id: string, value: any) {
    (store[key] ??= {})[id] = value;
  }
  function clamp(opt: OptionDef, raw: string) {
    const n = Number(raw);
    if (Number.isNaN(n)) return opt.default;
    return Math.min(opt.max ?? n, Math.max(opt.min ?? n, n));
  }
</script>

{#each shown as opt (opt.id)}
  <Row term={opt.glossary} label={opt.label} note={opt.id in limits ? (opt.limitedNote ?? "") : ""}>
    {#if opt.kind === "select"}
      <Select name={opt.id} label={opt.label} value={values[opt.id]} choices={opt.choices ?? []} onchange={(v) => set(opt.id, v)} />
    {:else if opt.kind === "toggle"}
      <Toggle label={opt.label} checked={values[opt.id]} onchange={(v) => set(opt.id, v)} />
    {:else}
      {#if opt.unit === "Kbps"}<span class="hint">≈ {(values[opt.id] / 1000).toFixed(1)} Mbps</span>{/if}
      <label class="number">
        <input
          type="number"
          min={opt.min}
          max={opt.max}
          step={opt.step}
          value={limits[opt.id] ?? values[opt.id]}
          disabled={opt.id in limits}
          aria-label={opt.label}
          onchange={(e) => {
            const v = clamp(opt, e.currentTarget.value);
            e.currentTarget.value = String(v);
            set(opt.id, v);
          }}
        />
        {#if opt.unit}<span class="unit">{opt.unit}</span>{/if}
      </label>
    {/if}
  </Row>
{/each}

<style>
  .hint {
    font-size: 12px;
    color: var(--text-faint);
  }
</style>
