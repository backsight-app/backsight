<script lang="ts">
  // One setting: an explained label on the left, its control on the right.
  import type { Snippet } from "svelte";
  import { glossary } from "../content";
  import Term from "./Term.svelte";

  let {
    term,
    label,
    note = "",
    children,
  }: { term: string; label?: string; note?: string; children?: Snippet } = $props();

  const entry = $derived(glossary[term]);
</script>

<div class="row">
  <div class="text">
    <Term id={term}>{label ?? entry?.term ?? term}</Term>
    {#if entry?.short}<p class="short">{entry.short}</p>{/if}
    {#if note}<p class="note">{note}</p>{/if}
  </div>
  <div class="control">{@render children?.()}</div>
</div>

<style>
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 24px;
    padding: 14px 0;
    border-top: 1px solid var(--border);
  }
  .row:first-of-type {
    border-top: none;
  }
  .text {
    display: grid;
    gap: 3px;
    font-weight: 600;
  }
  .short,
  .note {
    margin: 0;
    font-weight: 400;
    font-size: 13px;
    color: var(--text-dim);
  }
  .note {
    color: var(--warn);
  }
  .control {
    display: flex;
    align-items: center;
    gap: 10px;
    flex: none;
  }
</style>
