<script lang="ts">
  import { isTauri } from "@tauri-apps/api/core";
  import { untrack } from "svelte";
  import HotkeyInput from "../lib/components/HotkeyInput.svelte";
  import Icon from "../lib/components/Icon.svelte";
  import Kbd from "../lib/components/Kbd.svelte";
  import Term from "../lib/components/Term.svelte";
  import { ACTIONS } from "../lib/hotkeys";
  import { clashesFor, same, takenClashes, type Scan } from "../lib/overlays";
  import { app } from "../lib/settings.svelte";
  import { loadScan } from "../lib/setup";

  const keys = $derived(app.settings.hotkeys);
  const groups = [...new Set(ACTIONS.map((a) => a.group))];
  const labelOf = (id: string) => ACTIONS.find((a) => a.id === id)?.label ?? id;

  // For each action, the other action already using the same combo (if any).
  const conflicts = $derived.by(() => {
    const out: Record<string, string> = {};
    for (const a of ACTIONS) {
      const other = ACTIONS.find((b) => b.id !== a.id && keys[a.id] && keys[b.id] && same(keys[a.id]) === same(keys[b.id]));
      if (other) out[a.id] = other.id;
    }
    return out;
  });

  // Other apps' keys, read when the page opens once the saved hotkeys are in, and again after each
  // key you type, so a key another program holds is found; clashes follow the keys as you edit.
  let scan = $state<Scan | null>(null);
  let seq = 0;
  const rescan = () => {
    const mine = ++seq;
    loadScan($state.snapshot(app.settings)).then((r) => mine === seq && (scan = r)); // only the newest scan counts
  };
  let scanned = false;
  $effect(() => {
    if (!scanned && (app.loaded || !isTauri())) {
      scanned = true;
      untrack(rescan);
    }
  });
  const clashes = $derived(scan ? clashesFor(keys as Record<string, string>, scan.apps) : []);
  const taken = $derived(scan ? takenClashes(keys as Record<string, string>, scan.taken).map((t) => t.action) : []);
  // What another app's use of this key means, by how that app takes keys (overlays.ts `App.passes`).
  function shared(action: string) {
    const mine = clashes.filter((c) => c.action === action);
    if (!mine.length) return "";
    const names = [...new Set(mine.map((c) => c.app))].join(" and ");
    if (mine.some((c) => c.passes === "hidden")) return `Also used by ${names}. Backsight may not see this key.`;
    if (mine.every((c) => c.passes === "both")) return `Also used by ${names}, so one press may do two things.`;
    return `Also used by ${names}.`;
  }
  const clashOf = (appId: string, key: string) => clashes.find((c) => c.app_id === appId && same(c.key) === same(key));
  const sentence = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);
</script>

<div class="stack">
  <p class="intro muted">
    Click a box and press the keys you want, or pick No key to turn one off. Every <Term id="hotkey">hotkey</Term> works while you're in a game.
    If a key does nothing, another program (for example an overlay) may be taking it.
  </p>

  {#each groups as group}
    <section class="card">
      <header><h2>{group}</h2></header>
      {#each ACTIONS.filter((a) => a.group === group) as action}
        <div class="row">
          <div class="text">
            <strong>{action.label}</strong>
            <p>{action.desc}</p>
            {#if conflicts[action.id]}
              <p class="conflict">
                <Icon name="alert" size={14} />
                Same as “{labelOf(conflicts[action.id])}”. <Term id="hotkey-conflict">Why this matters</Term>
              </p>
            {/if}
            {#if taken.includes(action.id)}
              <p class="conflict">
                <Icon name="alert" size={14} />
                Another program on this PC already uses this key, so Backsight won't react to it. Pick a different key.
              </p>
            {:else if shared(action.id)}
              <p class="clash"><Icon name="alert" size={14} /> {shared(action.id)}</p>
            {/if}
          </div>
          <div class="key">
            <HotkeyInput
              label={action.label}
              value={keys[action.id]}
              conflict={conflicts[action.id]}
              onchange={(combo) => {
                keys[action.id] = combo;
                rescan();
              }}
            />
            <!-- No key at all, e.g. when another app keeps this one. -->
            <button
              type="button"
              class="btn"
              aria-label={`${action.label}: no key`}
              disabled={!keys[action.id]}
              onclick={() => (keys[action.id] = "")}>No key</button
            >
          </div>
        </div>
      {/each}
    </section>
  {/each}

  <section class="card">
    <header>
      <div class="text">
        <h2>Other apps on this PC</h2>
        {#if scan?.apps.length}
          <p>Apps with hotkeys of their own. A key Backsight also uses is marked, so you can change one of them.</p>
        {/if}
      </div>
    </header>
    {#if !scan}
      <p class="muted">Looking for other apps…</p>
    {:else if !scan.apps.length}
      <p class="muted">No other apps with hotkeys found.</p>
    {:else}
      {#each scan.apps as other}
        <div class="row other">
          <div class="text">
            <strong>{other.name}</strong>
            <p>
              {other.id === "windows" ? "" : other.running ? "Running · " : "Not running · "}{other.saved ? "Keys from its settings" : "Its usual keys"}{other.scope ===
              "in-game"
                ? " · Only in games"
                : ""}
            </p>
          </div>
          <ul>
            {#each other.keys.filter((k) => k.key) as k}
              {@const hit = clashOf(other.id, k.key)}
              <li class:hit>
                <Kbd combo={k.key} />
                <span>
                  {sentence(k.does)}
                  {#if hit}<span class="also">Also “{labelOf(hit.action)}” in Backsight</span>{/if}
                </span>
              </li>
            {/each}
          </ul>
        </div>
      {/each}
    {/if}
  </section>
</div>

<style>
  .intro :global(.term) {
    color: var(--text);
  }
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
  .key {
    display: flex;
    gap: 8px;
  }
  .text {
    display: grid;
    gap: 3px;
  }
  .text p {
    font-size: 13px;
    color: var(--text-dim);
  }
  .text p.conflict,
  .text p.clash {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--rec);
  }
  .text p.clash {
    color: var(--warn);
  }
  .other {
    display: grid;
    grid-template-columns: 160px 1fr;
    align-items: start;
  }
  .other ul {
    display: grid;
    gap: 4px;
    margin: 0;
    padding: 0;
    list-style: none;
    font-size: 13px;
    color: var(--text-dim);
  }
  .other li {
    display: grid;
    /* Wide enough for Alt + Win + PrintScreen. */
    grid-template-columns: 200px 1fr;
    align-items: center;
    gap: 10px;
    padding: 3px 8px;
    border-radius: var(--radius-sm);
  }
  .other li.hit {
    background: rgb(212 178 91 / 0.1);
    color: #e8cf8a;
  }
  .other .also {
    display: block;
    font-weight: 500;
  }
</style>
