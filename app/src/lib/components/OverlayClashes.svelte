<script lang="ts">
  // Other apps on this PC share some of Backsight's keys: says which, and offers other keys (Setup and Home).
  import defaults from "../../../../content/default-settings.json";
  import fallback from "../../../../content/fallback-hotkeys.json";
  import previous from "../../../../content/previous-default-hotkeys.json";
  import spares from "../../../../content/spare-hotkeys.json";
  import { ACTIONS } from "../hotkeys";
  import { same, takenClashes, useOtherKeys, type App, type Clash, type Scan } from "../overlays";
  import { app } from "../settings.svelte";
  import { loadScan } from "../setup";
  import { onDestroy } from "svelte";
  import Kbd from "./Kbd.svelte";
  import Term from "./Term.svelte";

  // `onswap`: after new keys are picked, with the scan of them; the page keeps this card up to show the result.
  let { clashes, onswap }: { clashes: Clash[]; onswap?: (scan: Scan) => void } = $props();
  let swapped = $state<string[]>([]);
  let unswapped = $state<string[]>([]);
  // Moved to a key another program turned out to hold: back on the old key, or (when another
  // action took the old one meanwhile) still on the held one.
  let reverted = $state<string[]>([]);
  let held = $state<string[]>([]);
  let busy = $state(false);
  let done = $state(false);
  let failed = $state(false);
  // Dismissed (or left) while the new keys are checked: drop the result, keys unchanged.
  let gone = false;
  onDestroy(() => (gone = true));
  // A taken key never reaches Backsight, so "both act" lines for it would contradict that.
  const takenActions = $derived(new Set(clashes.filter((c) => c.passes === "taken").map((c) => c.action)));
  const list = $derived(clashes.filter((c) => !(c.passes === "both" && takenActions.has(c.action))));

  const label = (action: string) => ACTIONS.find((a) => a.id === action)?.label ?? action;
  async function swap() {
    busy = true;
    // Every app's keys, so a default that is one of them (Alt+R) isn't picked.
    const known: App[] = [
      { id: "", name: "", running: true, saved: false, passes: "", scope: "", keys: list.filter((c) => c.passes !== "taken") },
    ];
    const before = await loadScan($state.snapshot(app.settings));
    const settings = $state.snapshot(app.settings);
    const keys = settings.hotkeys as Record<string, string>;
    const original = { ...keys };
    const r = useOtherKeys(keys, list, defaults.hotkeys, previous, fallback, spares, [...before.apps, ...known]);
    // Probe the new keys (one another program holds didn't really move) before using them: the
    // page hides this card once the keys clash with nothing, unless `onswap` already ran.
    const after = await loadScan(settings);
    busy = false;
    if (gone) return;
    const holds = new Set(takenClashes(keys, after.taken).map((c) => c.action).filter((a) => r.moved.includes(a)));
    const free = (a: string) => !Object.entries(keys).some(([b, k]) => b !== a && k && same(k) === same(original[a]));
    for (const a of holds) if (free(a)) keys[a] = original[a];
    swapped = r.moved.filter((a) => !holds.has(a));
    unswapped = r.stuck;
    reverted = [...holds].filter((a) => keys[a] === original[a]);
    held = [...holds].filter((a) => keys[a] !== original[a]);
    failed = !!after.failed;
    done = true;
    onswap?.(after);
    app.settings.hotkeys = keys;
  }
  const names = (actions: string[]) => actions.map(label).join(", ");
</script>

{#if !done}
  <p>Some <Term id="overlay">other apps</Term> on this PC use the same keys as Backsight:</p>
  <ul>
    {#each list as c}
      <li>
        {#if c.passes === "both"}
          <Kbd combo={c.key} /> {label(c.action)} in Backsight, and {c.does}.
        {:else if c.passes === "hidden"}
          {label(c.action)}: <Kbd combo={c.key} /> {c.does}. Backsight may not see this key.
        {:else if c.passes === "taken"}
          {label(c.action)}: another program on this PC already uses <Kbd combo={c.key} />, so Backsight won't react to it.
        {:else}
          {label(c.action)}: <Kbd combo={c.key} /> {c.does}. One press may do both, or only {c.app}'s.
        {/if}
      </li>
    {/each}
  </ul>
  <div class="actions">
    <button type="button" class="btn primary" onclick={swap} disabled={busy}>
      {busy ? "Checking the new keys…" : "Give Backsight other keys"}
    </button>
    <span class="muted">Or change the key in that app. Backsight never changes other apps' settings.</span>
  </div>
{:else}
  {#if swapped.length}<p>Done. Backsight has new keys. You can change them any time in Hotkeys.</p>{/if}
  {#if unswapped.length}
    <p class="note">
      {names(unswapped)} still {unswapped.length === 1 ? "shares its key" : "share their keys"} with
      another app, because no free key was left. Pick a new one in Hotkeys.
    </p>
  {/if}
  {#if reverted.length}
    <p class="note">
      Another program on this PC holds the new key picked for {names(reverted)}, so
      {reverted.length === 1 ? "it keeps its old key" : "they keep their old keys"}. Pick a new one in Hotkeys.
    </p>
  {/if}
  {#if held.length}
    <p class="note">
      Another program on this PC holds the new key for {names(held)}, so Backsight won't react to it. Pick a new one in
      Hotkeys.
    </p>
  {/if}
  {#if failed}
    <p class="note">Backsight couldn't check whether another program holds the new keys. If one does nothing, pick another in Hotkeys.</p>
  {/if}
  <div class="keys">
    {#each swapped as action}
      <span>{label(action)} <Kbd combo={app.settings.hotkeys[action]} /></span>
    {/each}
  </div>
{/if}

<style>
  .note {
    padding: 10px 12px;
    border-radius: var(--radius-sm);
    background: rgb(212 178 91 / 0.1);
    color: #e8cf8a;
  }
  ul {
    display: grid;
    gap: 6px;
    margin: 0;
    padding-left: 18px;
  }
  .actions,
  .keys {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 12px;
  }
  .keys span {
    display: flex;
    align-items: center;
    gap: 8px;
  }
</style>
