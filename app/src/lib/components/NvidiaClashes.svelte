<script lang="ts">
  // The NVIDIA overlay shares some of Backsight's keys: says which, and offers other keys (Setup and Home).
  import defaults from "../../../../content/default-settings.json";
  import alternatives from "../../../../content/hotkey-alternatives.json";
  import { ACTIONS } from "../hotkeys";
  import { useOtherKeys, type Clash } from "../nvidia";
  import { app } from "../settings.svelte";
  import Kbd from "./Kbd.svelte";
  import Term from "./Term.svelte";

  let { clashes }: { clashes: Clash[] } = $props();
  let swapped = $state<string[]>([]);
  let unswapped = $state<string[]>([]);

  const label = (action: string) => ACTIONS.find((a) => a.id === action)?.label ?? action;
  function swap() {
    const r = useOtherKeys(app.settings.hotkeys as Record<string, string>, clashes, defaults.hotkeys, alternatives);
    swapped = r.moved;
    unswapped = r.stuck;
  }
</script>

{#if !swapped.length && !unswapped.length}
  <p>
    The <Term id="nvidia-overlay">NVIDIA overlay</Term> uses some of the same keys, so one press would do two things:
  </p>
  <ul>
    {#each clashes as c}
      <li><Kbd combo={c.key} /> {label(c.action)} in Backsight, and {c.nvidia}.</li>
    {/each}
  </ul>
  <div class="actions">
    <button type="button" class="btn primary" onclick={swap}>Give Backsight other keys</button>
    <span class="muted">Or turn the overlay off in the NVIDIA app. Backsight never changes NVIDIA's settings.</span>
  </div>
{:else}
  {#if swapped.length}<p>Done. Backsight has new keys. You can change them any time in Hotkeys.</p>{/if}
  {#if unswapped.length}
    <p class="note">
      {unswapped.map(label).join(", ")} still {unswapped.length === 1 ? "shares its key" : "share their keys"} with
      NVIDIA, because the usual other key is taken. Pick a new one in Hotkeys.
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
    background: rgb(245 158 11 / 0.12);
    color: #fcd34d;
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
