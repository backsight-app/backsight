<script lang="ts" module>
  import { invoke, isTauri } from "@tauri-apps/api/core";

  // While a box listens, the live hotkeys ignore presses (Alt+R would start a recording). Turned
  // back on a moment later: the hotkeys see a press before this page does.
  let resume: ReturnType<typeof setTimeout> | undefined;
  function pauseHotkeys(on: boolean) {
    if (!isTauri()) return;
    clearTimeout(resume);
    const send = () => invoke("pause_hotkeys", { on }).catch(() => {});
    if (on) send();
    else resume = setTimeout(send, 300);
  }
</script>

<script lang="ts">
  // Click, press the keys you want, done. Esc cancels, Backspace clears.
  import { comboFromKeyboard, comboFromMouse } from "../hotkeys";
  import Kbd from "./Kbd.svelte";

  let {
    value,
    onchange,
    label,
    conflict = "",
  }: { value: string; onchange: (v: string) => void; label: string; conflict?: string } = $props();

  let recording = $state(false);
  let button: HTMLButtonElement;

  $effect(() => {
    if (!recording) return;
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      const bare = !e.ctrlKey && !e.altKey && !e.shiftKey && !e.metaKey;
      if (e.key === "Escape") return (recording = false);
      if (bare && (e.key === "Backspace" || e.key === "Delete")) {
        onchange("");
        return (recording = false);
      }
      const combo = comboFromKeyboard(e);
      if (combo) {
        onchange(combo);
        recording = false;
      }
    };
    const onMouse = (e: MouseEvent) => {
      const combo = comboFromMouse(e);
      if (combo) {
        e.preventDefault();
        onchange(combo);
        recording = false;
      } else if (!button.contains(e.target as Node)) {
        recording = false;
      }
    };
    addEventListener("keydown", onKey, true);
    addEventListener("mousedown", onMouse, true);
    pauseHotkeys(true);
    return () => {
      removeEventListener("keydown", onKey, true);
      removeEventListener("mousedown", onMouse, true);
      pauseHotkeys(false);
    };
  });
</script>

<button
  bind:this={button}
  type="button"
  class="hotkey"
  class:recording
  class:conflict={!!conflict}
  aria-label={recording
    ? `${label}: press a key combo. Escape cancels, Backspace clears.`
    : `${label}: ${value || "no key"}. Press to change.`}
  onclick={() => (recording = true)}
  onblur={() => (recording = false)}
>
  {#if recording}
    <span class="dot"></span>
    <span>Press a key combo…</span>
    <span class="help">Esc cancels · Backspace clears</span>
  {:else}
    <Kbd combo={value} />
  {/if}
</button>

<style>
  .hotkey {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    min-width: 230px;
    height: 38px;
    padding: 0 12px;
    border-radius: var(--radius-sm);
    border: 1px solid var(--border-strong);
    background: var(--bg-input);
    color: var(--text);
    font: inherit;
    cursor: pointer;
  }
  .hotkey:hover {
    border-color: var(--text-faint);
  }
  .hotkey:focus-visible,
  .hotkey.recording {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--focus);
  }
  .hotkey.conflict {
    border-color: var(--rec);
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--rec);
    animation: blink 1s infinite;
  }
  .help {
    margin-left: auto;
    font-size: 11px;
    color: var(--text-faint);
  }
  @keyframes blink {
    50% {
      opacity: 0.25;
    }
  }
</style>
