<script lang="ts">
  import { computePosition, flip, offset, shift } from "@floating-ui/dom";
  import { onMount } from "svelte";

  let { label, anchor, actions, close }: {
    label: string;
    anchor: Element | { getBoundingClientRect: () => DOMRect };
    actions: { label: string; run: () => void }[];
    close: (restoreFocus: boolean) => void;
  } = $props();
  let menu: HTMLDivElement;

  onMount(() => {
    menu.querySelector("button")?.focus({ preventScroll: true });
    computePosition(anchor, menu, {
      strategy: "fixed",
      placement: "bottom-start",
      middleware: [offset(4), flip({ padding: 8 }), shift({ padding: 8, crossAxis: true })],
    }).then(({ x, y }) => Object.assign(menu.style, { left: `${x}px`, top: `${y}px` }));
  });

  function keys(e: KeyboardEvent) {
    if (e.key === "Tab") {
      close(true);
      return;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      close(true);
      return;
    }
    const buttons = [...menu.querySelectorAll("button")];
    const index = buttons.indexOf(document.activeElement as HTMLButtonElement);
    const next = {
      ArrowDown: (index + 1) % buttons.length,
      ArrowUp: (index + buttons.length - 1) % buttons.length,
      Home: 0,
      End: buttons.length - 1,
    }[e.key];
    if (next === undefined) return;
    e.preventDefault();
    buttons[next].focus();
  }
</script>

<svelte:window
  onpointerdown={(e) => { if (!menu.contains(e.target as Node)) close(false); }}
  onblur={() => close(false)}
  onresize={() => close(true)}
/>

<div class="menu" bind:this={menu} role="menu" aria-label={label} tabindex="-1" onkeydown={keys}>
  <div class="label" aria-hidden="true" title={label}>{label}</div>
  {#each actions as action (action.label)}
    <button type="button" role="menuitem" tabindex="-1" onclick={() => { close(true); action.run(); }}>
      {action.label}
    </button>
  {/each}
</div>

<style>
  .menu {
    position: fixed;
    top: 8px;
    left: 8px;
    z-index: 100;
    display: grid;
    width: min(244px, calc(100vw - 16px));
    max-height: calc(100vh - 16px);
    overflow: auto;
    padding: 5px;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    background: var(--bg-elev);
    box-shadow: 0 10px 26px rgb(0 0 0 / 0.42);
  }
  .label {
    overflow: hidden;
    padding: 7px 10px;
    border-bottom: 1px solid var(--border);
    color: var(--text-dim);
    font-size: 12px;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  button {
    padding: 8px 10px;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text);
    text-align: left;
    cursor: pointer;
  }
  button:hover, button:focus-visible {
    background: var(--bg-hover);
  }
</style>
