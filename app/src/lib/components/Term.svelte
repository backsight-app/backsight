<script lang="ts">
  // A word you can hover (or focus with Tab) to get a plain-language explanation.
  import type { Snippet } from "svelte";
  import { autoUpdate, computePosition, flip, offset, shift } from "@floating-ui/dom";
  import { glossary } from "../content";
  import Icon from "./Icon.svelte";
  import TermCard from "./TermCard.svelte";

  let { id, children }: { id: string; children?: Snippet } = $props();

  const entry = $derived(glossary[id]);
  const tipId = `tip-${Math.random().toString(36).slice(2, 9)}`;
  let open = $state(false);
  let anchor: HTMLElement;
  let tip: HTMLElement | undefined = $state();
  let timer: ReturnType<typeof setTimeout> | undefined;

  function show() {
    clearTimeout(timer);
    timer = setTimeout(() => (open = true), 150);
  }
  function hide() {
    clearTimeout(timer);
    open = false;
  }

  $effect(() => {
    if (!open || !tip) return;
    const el = tip;
    return autoUpdate(anchor, el, () =>
      computePosition(anchor, el, {
        placement: "bottom-start",
        strategy: "fixed",
        middleware: [offset(10), flip({ padding: 12 }), shift({ padding: 12 })],
      }).then(({ x, y }) => Object.assign(el.style, { left: `${x}px`, top: `${y}px` })),
    );
  });
</script>

<span
  class="term"
  role="button"
  tabindex="0"
  data-term={id}
  aria-describedby={open ? tipId : undefined}
  bind:this={anchor}
  onmouseenter={show}
  onmouseleave={hide}
  onfocus={show}
  onblur={hide}
  onkeydown={(e) => e.key === "Escape" && hide()}
>
  <span class="word">{#if children}{@render children()}{:else}{entry?.term ?? id}{/if}</span>
  <Icon name="help" size={14} />
</span>

{#if open && entry}
  <div class="tip" role="tooltip" id={tipId} bind:this={tip}>
    <TermCard {entry} />
  </div>
{/if}

<style>
  .term {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    cursor: help;
    border-radius: 4px;
    outline: none;
  }
  .word {
    text-decoration: underline dotted var(--text-faint);
    text-underline-offset: 4px;
    text-decoration-thickness: 1.5px;
  }
  .term :global(svg) {
    color: var(--text-faint);
    transition: color 0.15s;
  }
  .term:hover :global(svg),
  .term:focus-visible :global(svg) {
    color: var(--accent-2);
  }
  .term:focus-visible {
    box-shadow: 0 0 0 2px var(--focus);
  }
  .tip {
    position: fixed;
    z-index: 100;
    width: 340px;
    padding: 14px 16px;
    border-radius: var(--radius);
    border: 1px solid var(--border-strong);
    background: var(--bg-pop);
    box-shadow: var(--shadow);
    animation: pop 0.14s ease-out;
    pointer-events: none;
  }
  @keyframes pop {
    from {
      opacity: 0;
      transform: translateY(-4px);
    }
  }
</style>
