<script lang="ts">
  import type { GlossaryEntry } from "../content";
  import Icon from "./Icon.svelte";

  let { entry }: { entry: GlossaryEntry } = $props();
</script>

<div class="termcard">
  <div class="title">{entry.term}</div>
  <div class="short">{entry.short}</div>
  <p class="eli5">{entry.eli5}</p>
  {#if entry.pros?.length}
    <ul class="pros">
      {#each entry.pros as pro}<li><Icon name="check" size={15} /><span>{pro}</span></li>{/each}
    </ul>
  {/if}
  {#if entry.cons?.length}
    <ul class="cons">
      {#each entry.cons as con}<li><Icon name="alert" size={15} /><span>{con}</span></li>{/each}
    </ul>
  {/if}
  {#if entry.pickIf}
    <div class="pick"><Icon name="bulb" size={15} /><span>{entry.pickIf}</span></div>
  {/if}
</div>

<style>
  .termcard {
    display: grid;
    gap: 8px;
    font-size: 13px;
    line-height: 1.5;
  }
  .title {
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--accent-2);
  }
  .short {
    font-size: 15px;
    font-weight: 600;
    color: var(--text);
    line-height: 1.35;
  }
  .eli5 {
    margin: 0;
    color: var(--text-dim);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 4px;
  }
  li,
  .pick {
    display: flex;
    gap: 8px;
    align-items: flex-start;
  }
  li :global(svg),
  .pick :global(svg) {
    margin-top: 2px;
  }
  .pros li {
    color: var(--text);
  }
  .pros :global(svg) {
    color: var(--ok);
  }
  .cons li {
    color: var(--text);
  }
  .cons :global(svg) {
    color: var(--warn);
  }
  .pick {
    margin-top: 2px;
    padding: 8px 10px;
    border-radius: var(--radius-sm);
    background: var(--accent-soft);
    color: var(--text);
  }
  .pick :global(svg) {
    color: var(--accent-2);
  }
</style>
