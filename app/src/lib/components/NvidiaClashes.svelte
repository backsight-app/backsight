<script lang="ts">
  // The NVIDIA overlay shares some of Backsight's keys: says which, and offers other keys (Setup and Home).
  import defaults from "../../../../content/default-settings.json";
  import fallback from "../../../../content/fallback-hotkeys.json";
  import previous from "../../../../content/previous-default-hotkeys.json";
  import { ACTIONS } from "../hotkeys";
  import { useOtherKeys, type Clash, type OverlayKey } from "../nvidia";
  import { app } from "../settings.svelte";
  import { invoke } from "../uiwatch";
  import Kbd from "./Kbd.svelte";
  import Term from "./Term.svelte";

  let { clashes }: { clashes: Clash[] } = $props();
  let swapped = $state<string[]>([]);
  let unswapped = $state<string[]>([]);

  const label = (action: string) => ACTIONS.find((a) => a.id === action)?.label ?? action;
  async function swap() {
    // NVIDIA's own keys, so a default that is one of them (Alt+R) isn't picked.
    const overlay = await invoke<OverlayKey[]>("nvidia_overlay_keys").catch(() => clashes);
    const keys = app.settings.hotkeys as Record<string, string>;
    const r = useOtherKeys(keys, clashes, defaults.hotkeys, previous, fallback, overlay);
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
      NVIDIA, because its usual key is taken. Pick a new one in Hotkeys.
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
