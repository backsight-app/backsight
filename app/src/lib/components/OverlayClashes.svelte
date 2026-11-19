<script lang="ts">
  // Other apps on this PC share some of Backsight's keys: says which, and offers other keys (Setup and Home).
  import defaults from "../../../../content/default-settings.json";
  import fallback from "../../../../content/fallback-hotkeys.json";
  import previous from "../../../../content/previous-default-hotkeys.json";
  import spares from "../../../../content/spare-hotkeys.json";
  import { ACTIONS } from "../hotkeys";
  import { useOtherKeys, type App, type Clash } from "../overlays";
  import { app } from "../settings.svelte";
  import { loadScan } from "../setup";
  import Kbd from "./Kbd.svelte";
  import Term from "./Term.svelte";

  // `onswap`: after new keys are picked, so the page can probe them (a program may hold one).
  let { clashes, onswap }: { clashes: Clash[]; onswap?: () => void } = $props();
  let swapped = $state<string[]>([]);
  let unswapped = $state<string[]>([]);
  // The engine's setup check marks a key some program reserved with app_id "taken".
  // A taken key never reaches Backsight, so "both act" lines for it would contradict that.
  const takenActions = $derived(new Set(clashes.filter((c) => c.app_id === "taken").map((c) => c.action)));
  const list = $derived(
    clashes
      .filter((c) => !(c.passes === "both" && takenActions.has(c.action)))
      .map((c) => (c.app_id === "taken" ? { ...c, passes: "taken" } : c)),
  );

  const label = (action: string) => ACTIONS.find((a) => a.id === action)?.label ?? action;
  async function swap() {
    // Every app's keys, so a default that is one of them (Alt+R) isn't picked.
    const known: App[] = [
      { id: "", name: "", running: true, saved: false, passes: "", scope: "", keys: list.filter((c) => c.passes !== "taken") },
    ];
    const apps = await loadScan($state.snapshot(app.settings)).then(
      (s) => [...s.apps, ...known],
      () => known,
    );
    const keys = app.settings.hotkeys as Record<string, string>;
    const r = useOtherKeys(keys, list, defaults.hotkeys, previous, fallback, spares, apps);
    swapped = r.moved;
    unswapped = r.stuck;
    onswap?.();
  }
</script>

{#if !swapped.length && !unswapped.length}
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
    <button type="button" class="btn primary" onclick={swap}>Give Backsight other keys</button>
    <span class="muted">Or change the key in that app. Backsight never changes other apps' settings.</span>
  </div>
{:else}
  {#if swapped.length}<p>Done. Backsight has new keys. You can change them any time in Hotkeys.</p>{/if}
  {#if unswapped.length}
    <p class="note">
      {unswapped.map(label).join(", ")} still {unswapped.length === 1 ? "shares its key" : "share their keys"} with
      another app, because no free key was left. Pick a new one in Hotkeys.
    </p>
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
