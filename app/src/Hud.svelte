<script lang="ts">
  // Backsight's own pop-ups (`?hud`) and recording dot (`?hud=dot`), ADR 0010: each a transparent,
  // click-through window that never takes focus, where the user put it. Rust queues pop-ups; this
  // page takes them a few times a second, fades each out after a few seconds, and asks Rust to
  // close the window once nothing is left to show, so it costs no memory between actions. While
  // being moved (General page) it shows a sample the mouse can drag.
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";

  const dot = new URLSearchParams(location.search).get("hud") === "dot";

  type Notice = { kind: string; title: string; body: string };
  type Recording = {
    on: boolean;
    seconds: number;
    paused: boolean;
    push_to_talk: boolean;
    mic_open: boolean;
    mic_muted: boolean;
  };
  type Shown = Notice & { id: number; leaving: boolean };

  const SHOW_MS = 3500;
  const MAX = 3;
  let cards = $state<Shown[]>([]);
  let rec = $state<Recording | null>(null);
  let arranging = $state(false);
  let left = $state(false);
  let bottom = $state(false);
  // What the dot shows while being moved and not recording.
  const SAMPLE: Recording = { on: true, seconds: 0, paused: false, push_to_talk: false, mic_open: false, mic_muted: false };
  let next = 0;
  let idleSince = 0;

  function add(n: Notice) {
    const id = next++;
    cards = [{ ...n, id, leaving: false }, ...cards].slice(0, MAX);
    setTimeout(() => (cards = cards.map((c) => (c.id === id ? { ...c, leaving: true } : c))), SHOW_MS);
    setTimeout(() => (cards = cards.filter((c) => c.id !== id)), SHOW_MS + 300);
  }

  async function poll() {
    try {
      const hud = await invoke<{ notices: Notice[]; recording: Recording | null; arranging: boolean; left: boolean; bottom: boolean }>("hud_poll");
      hud.notices.forEach(add);
      if (hud.recording) rec = hud.recording;
      ({ arranging, left, bottom } = hud);
    } catch {
      // Rust is closing this window.
    }
    const busy = arranging || (dot ? rec?.on : cards.length > 0);
    if (busy) idleSince = 0;
    else if (!idleSince) idleSince = Date.now();
    else if (Date.now() - idleSince > 1000) invoke("hud_idle").catch(() => {});
  }

  onMount(() => {
    poll();
    const timer = setInterval(poll, 200);
    return () => clearInterval(timer);
  });

  const time = (s: number) => {
    const t = Math.floor(s);
    const h = Math.floor(t / 3600);
    const mm = String(Math.floor(t / 60) % 60).padStart(h ? 2 : 1, "0");
    return `${h ? h + ":" : ""}${mm}:${String(t % 60).padStart(2, "0")}`;
  };

  // Windows moves the window while the button is held.
  const drag = (e: MouseEvent) => {
    if (arranging && e.button === 0) invoke("hud_drag").catch(() => {});
  };

  const shown = $derived(rec?.on ? rec : arranging ? SAMPLE : null);
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="hud" class:left class:bottom class:arranging onmousedown={drag}>
  {#if dot && shown}
    <div class="rec" class:paused={shown.paused} role="status" aria-label={shown.paused ? "Recording paused" : "Recording"}>
      <span class="dot"></span>
      <span class="label">{shown.paused ? "Paused" : "Rec"}</span>
      <span class="time">{time(shown.seconds)}</span>
      {#if shown.mic_muted}
        <span class="mic off" title="Microphone muted">
          <svg viewBox="0 0 24 24"><path d="M12 15a3 3 0 0 0 3-3V6a3 3 0 0 0-6 0v6a3 3 0 0 0 3 3Zm5-3a5 5 0 0 1-10 0M12 17v4M4 4l16 16" /></svg>
        </span>
      {:else if shown.push_to_talk}
        <span class="mic" class:open={shown.mic_open} title={shown.mic_open ? "Push-to-talk: mic open" : "Push-to-talk: mic closed"}>
          <svg viewBox="0 0 24 24"><path d="M12 15a3 3 0 0 0 3-3V6a3 3 0 0 0-6 0v6a3 3 0 0 0 3 3Zm5-3a5 5 0 0 1-10 0M12 17v4" /></svg>
        </span>
      {/if}
    </div>
  {/if}

  {#if !dot && arranging}
    <div class="card info sample">
      <span class="icon" aria-hidden="true"><svg viewBox="0 0 24 24"><path d="M12 3v18M3 12h18M12 3l-3 3M12 3l3 3M12 21l-3-3M12 21l3-3M3 12l3-3M3 12l3 3M21 12l-3-3M21 12l-3 3" /></svg></span>
      <div class="text">
        <strong>Pop-ups appear here</strong>
        <span>Drag this card where you want them, then press Done in Backsight.</span>
      </div>
    </div>
  {/if}

  {#each dot ? [] : cards as c (c.id)}
    <div class="card {c.kind}" class:leaving={c.leaving} role="status">
      <span class="icon" aria-hidden="true">
        <svg viewBox="0 0 24 24">
          {#if c.kind === "record"}<circle cx="12" cy="12" r="6" class="fill" />
          {:else if c.kind === "saved"}<path d="M5 12.5l4.5 4.5L19 7.5" />
          {:else if c.kind === "screenshot"}<path d="M4 8h3l2-3h6l2 3h3v11H4z" /><circle cx="12" cy="13" r="3.5" />
          {:else if c.kind === "pause"}<path d="M9 6v12M15 6v12" />
          {:else if c.kind === "marker"}<path d="M6 21V4h10l-2 4 2 4H6" />
          {:else if c.kind === "mic-on"}<path d="M12 15a3 3 0 0 0 3-3V6a3 3 0 0 0-6 0v6a3 3 0 0 0 3 3Zm5-3a5 5 0 0 1-10 0M12 17v4" />
          {:else if c.kind === "mic-off"}<path d="M12 15a3 3 0 0 0 3-3V6a3 3 0 0 0-6 0v6a3 3 0 0 0 3 3Zm5-3a5 5 0 0 1-10 0M12 17v4M4 4l16 16" />
          {:else if c.kind === "problem"}<path d="M12 4l9 16H3zM12 10v4M12 17v.5" />
          {:else}<circle cx="12" cy="12" r="9" /><path d="M12 11v6M12 7.5v.5" />{/if}
        </svg>
      </span>
      <div class="text">
        <strong>{c.title}</strong>
        {#if c.body}<span>{c.body}</span>{/if}
      </div>
    </div>
  {/each}
</div>

<style>
  :global(html),
  :global(body) {
    background: transparent !important;
  }
  .hud {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 8px;
    height: 100%;
    padding: 8px 12px;
    --slide: 24px;
  }
  /* Line up toward the nearest screen edges: newest pop-up nearest the edge. */
  .hud.left {
    align-items: flex-start;
    --slide: -24px;
  }
  .hud.bottom {
    flex-direction: column-reverse;
  }
  /* The whole window catches the mouse while being moved, so show all of it. */
  .hud.arranging {
    outline: 2px dashed var(--accent);
    outline-offset: -2px;
    border-radius: 12px;
    background: rgb(124 92 255 / 0.12);
    cursor: move;
  }
  .rec {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 12px 6px 10px;
    border-radius: 999px;
    background: rgb(11 15 26 / 0.85);
    border: 1px solid rgb(255 77 109 / 0.5);
    font: 600 13px var(--font);
    color: var(--text);
    box-shadow: 0 4px 16px rgb(0 0 0 / 0.4);
  }
  .dot {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: var(--rec);
    animation: pulse 1.6s ease-in-out infinite;
  }
  .paused .dot {
    background: var(--warn);
    animation: none;
  }
  .paused {
    border-color: rgb(251 191 36 / 0.5);
  }
  .label {
    text-transform: uppercase;
    letter-spacing: 0.06em;
    font-size: 11px;
    color: var(--text-dim);
  }
  .time {
    font-variant-numeric: tabular-nums;
  }
  .mic {
    display: grid;
    width: 18px;
    height: 18px;
    color: var(--text-faint);
  }
  .mic.open {
    color: var(--ok);
  }
  .mic.off {
    color: var(--rec);
  }
  .mic svg {
    width: 18px;
    height: 18px;
  }
  .card {
    display: flex;
    gap: 12px;
    align-items: flex-start;
    /* Fluid so it never runs past the window's right edge, whatever size Windows gives the window. */
    width: 100%;
    max-width: 320px;
    padding: 12px 14px;
    border-radius: var(--radius);
    background: rgb(20 27 48 / 0.96);
    border: 1px solid var(--border-strong);
    border-left: 3px solid var(--tone);
    box-shadow: var(--shadow);
    animation: slide-in 0.22s ease-out;
    transition:
      opacity 0.3s,
      transform 0.3s;
    --tone: var(--accent);
  }
  .card.leaving {
    opacity: 0;
    transform: translateX(var(--slide));
  }
  .icon {
    flex: none;
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border-radius: 50%;
    background: color-mix(in srgb, var(--tone) 18%, transparent);
    color: var(--tone);
  }
  svg {
    width: 18px;
    height: 18px;
    fill: none;
    stroke: currentColor;
    stroke-width: 2;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .fill {
    fill: currentColor;
    stroke: none;
  }
  .text {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  strong {
    font: 600 14px var(--font-display);
  }
  .text span {
    color: var(--text-dim);
    font-size: 12.5px;
    overflow-wrap: anywhere;
  }
  .record {
    --tone: var(--rec);
  }
  .saved,
  .mic-on {
    --tone: var(--ok);
  }
  .screenshot,
  .marker {
    --tone: var(--accent-2);
  }
  .pause,
  .problem {
    --tone: var(--warn);
  }
  .mic-off,
  .info {
    --tone: var(--text-faint);
  }
  @keyframes slide-in {
    from {
      opacity: 0;
      transform: translateX(var(--slide));
    }
  }
  @keyframes pulse {
    50% {
      opacity: 0.35;
    }
  }
</style>
