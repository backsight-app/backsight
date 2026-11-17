<script lang="ts" module>
  // Hotkeys name letter keys by where they sit on a US keyboard (the engine matches that spot), so
  // AZERTY's A key is stored as "Q". Show the letter printed on this keyboard instead.
  let letters = $state<Record<string, string>>({});
  type LayoutMap = { forEach(f: (value: string, code: string) => void): void };
  (navigator as Navigator & { keyboard?: { getLayoutMap?: () => Promise<LayoutMap> } }).keyboard
    ?.getLayoutMap?.()
    .then((map) => {
      const found: Record<string, string> = {};
      map.forEach((value, code) => {
        if (/^Key[A-Z]$/.test(code) && /^\p{L}$/u.test(value)) found[code.slice(3)] = value.toUpperCase();
      });
      letters = found;
    })
    .catch(() => {});
</script>

<script lang="ts">
  let { combo }: { combo: string } = $props();
</script>

{#if combo}
  <span class="combo">
    {#each combo.split("+") as key, i}
      {#if i}<span class="plus">+</span>{/if}<kbd>{letters[key] ?? key}</kbd>
    {/each}
  </span>
{:else}
  <span class="unset">Not set</span>
{/if}

<style>
  .combo {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
  kbd {
    min-width: 26px;
    padding: 3px 8px;
    border-radius: 6px;
    border: 1px solid var(--border-strong);
    border-bottom-width: 2px;
    background: var(--bg-input);
    font: 600 12px/1.4 var(--font);
    color: var(--text);
    text-align: center;
  }
  .plus {
    color: var(--text-faint);
    font-size: 12px;
  }
  .unset {
    color: var(--text-faint);
    font-style: italic;
  }
</style>
