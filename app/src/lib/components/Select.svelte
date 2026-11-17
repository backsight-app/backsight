<script lang="ts">
  // A dropdown whose options explain themselves: hover or arrow onto a choice to
  // see what it's good and bad at before you pick it.
  import { autoUpdate, computePosition, flip, offset, shift, size } from "@floating-ui/dom";
  import { glossary, type Choice } from "../content";
  import Icon from "./Icon.svelte";
  import TermCard from "./TermCard.svelte";

  let {
    value,
    choices,
    onchange,
    name = "",
    label = "",
  }: { value: any; choices: Choice[]; onchange: (v: any) => void; name?: string; label?: string } = $props();

  const uid = `sel-${Math.random().toString(36).slice(2, 9)}`;
  let open = $state(false);
  let active = $state(0);
  let button: HTMLButtonElement;
  let popup: HTMLDivElement | undefined = $state();
  let list: HTMLDivElement | undefined = $state();

  const selected = $derived(choices.find((c) => c.value === value));
  const hasInfo = $derived(choices.some((c) => c.glossary));
  const activeChoice = $derived(choices[active]);
  const activeEntry = $derived(activeChoice?.glossary ? glossary[activeChoice.glossary] : undefined);

  function openList() {
    active = Math.max(0, choices.findIndex((c) => c.value === value));
    open = true;
  }
  function close(refocus = true) {
    open = false;
    if (refocus) button?.focus();
  }
  function pick(i: number) {
    if (choices[i] && !choices[i].disabled) {
      onchange(choices[i].value);
      close();
    }
  }
  function move(step: number, from = active) {
    let i = from;
    for (let n = 0; n < choices.length; n++) {
      i = (i + step + choices.length) % choices.length;
      if (!choices[i].disabled) return (active = i);
    }
  }
  function onkey(e: KeyboardEvent) {
    const actions: Record<string, () => void> = {
      ArrowDown: () => move(1),
      ArrowUp: () => move(-1),
      Home: () => move(1, -1),
      End: () => move(-1, choices.length),
      Enter: () => pick(active),
      " ": () => pick(active),
      Escape: () => close(),
      Tab: () => close(false),
    };
    if (!actions[e.key]) return;
    if (e.key !== "Tab") e.preventDefault();
    actions[e.key]();
  }

  $effect(() => {
    if (!open || !popup) return;
    const el = popup;
    list?.focus();
    const stop = autoUpdate(button, el, () =>
      computePosition(button, el, {
        placement: "bottom-start",
        strategy: "fixed",
        middleware: [
          offset(6),
          flip({ padding: 12 }),
          shift({ padding: 12 }),
          size({ padding: 12, apply: ({ availableHeight }) => { el.style.maxHeight = `${Math.min(460, availableHeight)}px`; } }),
        ],
      }).then(({ x, y }) => Object.assign(el.style, { left: `${x}px`, top: `${y}px` })),
    );
    const outside = (e: PointerEvent) => {
      if (!el.contains(e.target as Node) && !button.contains(e.target as Node)) close(false);
    };
    addEventListener("pointerdown", outside, true);
    return () => {
      stop();
      removeEventListener("pointerdown", outside, true);
    };
  });

  $effect(() => {
    if (open) document.getElementById(`${uid}-${active}`)?.scrollIntoView({ block: "nearest" });
  });
</script>

<button
  bind:this={button}
  type="button"
  class="select"
  aria-haspopup="listbox"
  aria-expanded={open}
  aria-label={label ? `${label}: ${selected?.label ?? "not set"}` : undefined}
  data-select={name}
  onclick={() => (open ? close() : openList())}
  onkeydown={(e) => {
    if (["ArrowDown", "ArrowUp", "Enter", " "].includes(e.key)) {
      e.preventDefault();
      openList();
    }
  }}
>
  <span class="current">{selected?.label ?? "Choose…"}</span>
  {#if selected?.hint}<span class="badge">{selected.hint}</span>{/if}
  <Icon name="chevron" size={16} />
</button>

{#if open}
  <div class="popup" class:with-info={hasInfo} bind:this={popup}>
    <div
      class="options"
      role="listbox"
      tabindex="-1"
      aria-label={label}
      aria-activedescendant={`${uid}-${active}`}
      bind:this={list}
      onkeydown={onkey}
    >
      {#each choices as c, i}
        {#if c.group && c.group !== choices[i - 1]?.group}
          <div class="group" role="presentation">{c.group}</div>
        {/if}
        <div
          id={`${uid}-${i}`}
          role="option"
          tabindex="-1"
          class="option"
          class:active={i === active}
          class:disabled={c.disabled}
          aria-selected={c.value === value}
          aria-disabled={c.disabled}
          data-value={c.value}
          onpointerenter={() => (active = i)}
          onclick={() => pick(i)}
          onkeydown={onkey}
        >
          <span class="option-text">
            <span class="option-label">{c.label}</span>
            {#if c.disabled ? c.disabledReason : c.hint}
              <span class="option-hint">{c.disabled ? c.disabledReason : c.hint}</span>
            {/if}
          </span>
          {#if c.value === value}<Icon name="check" size={16} />{/if}
        </div>
      {/each}
    </div>
    {#if hasInfo}
      <div class="info" aria-live="polite">
        {#if activeEntry}
          <TermCard entry={activeEntry} />
        {:else}
          <p class="muted">{activeChoice?.hint ?? ""}</p>
        {/if}
      </div>
    {/if}
  </div>
{/if}

<style>
  .select {
    display: inline-flex;
    align-items: center;
    gap: 10px;
    min-width: 260px;
    height: 38px;
    padding: 0 12px 0 14px;
    border-radius: var(--radius-sm);
    border: 1px solid var(--border-strong);
    background: var(--bg-input);
    color: var(--text);
    font: inherit;
    cursor: pointer;
    transition: border-color 0.15s, background 0.15s;
  }
  .select:hover {
    border-color: var(--text-faint);
  }
  .select[aria-expanded="true"],
  .select:focus-visible {
    border-color: var(--accent);
    outline: none;
    box-shadow: 0 0 0 3px var(--focus);
  }
  .current {
    flex: 1;
    text-align: left;
    font-weight: 500;
  }
  .badge {
    font-size: 11px;
    padding: 2px 8px;
    border-radius: 99px;
    background: var(--accent-soft);
    color: var(--accent-2);
    white-space: nowrap;
  }
  .select :global(svg) {
    color: var(--text-faint);
  }
  .popup {
    position: fixed;
    z-index: 90;
    display: grid;
    grid-template-columns: minmax(260px, max-content);
    border-radius: var(--radius);
    border: 1px solid var(--border-strong);
    background: var(--bg-pop);
    box-shadow: var(--shadow);
    overflow: hidden;
    animation: drop 0.14s ease-out;
  }
  .popup.with-info {
    grid-template-columns: 280px 340px;
  }
  @keyframes drop {
    from {
      opacity: 0;
      transform: translateY(-4px);
    }
  }
  .options {
    overflow-y: auto;
    padding: 6px;
    outline: none;
    min-height: 0;
  }
  .group {
    padding: 10px 10px 4px;
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--text-faint);
  }
  .option {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 10px;
    border-radius: 7px;
    cursor: pointer;
    outline: none;
  }
  .option.active {
    background: var(--bg-hover);
  }
  .option[aria-selected="true"] .option-label {
    color: var(--accent-2);
  }
  .option.disabled {
    cursor: not-allowed;
    opacity: 0.45;
  }
  .option-text {
    flex: 1;
    display: grid;
    gap: 1px;
  }
  .option-label {
    font-weight: 500;
  }
  .option-hint {
    font-size: 12px;
    color: var(--text-faint);
  }
  .option :global(svg) {
    color: var(--accent-2);
  }
  .info {
    padding: 16px 18px;
    border-left: 1px solid var(--border);
    background: var(--bg-elev);
    overflow-y: auto;
    min-height: 0;
  }
</style>
