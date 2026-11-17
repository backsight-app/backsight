<script lang="ts">
  import HotkeyInput from "../lib/components/HotkeyInput.svelte";
  import Icon from "../lib/components/Icon.svelte";
  import Term from "../lib/components/Term.svelte";
  import { ACTIONS } from "../lib/hotkeys";
  import { app } from "../lib/settings.svelte";

  const keys = $derived(app.settings.hotkeys);
  const groups = [...new Set(ACTIONS.map((a) => a.group))];
  const labelOf = (id: string) => ACTIONS.find((a) => a.id === id)?.label ?? id;

  // For each action, the other action already using the same combo (if any).
  const conflicts = $derived.by(() => {
    const out: Record<string, string> = {};
    for (const a of ACTIONS) {
      const other = ACTIONS.find((b) => b.id !== a.id && keys[a.id] && keys[a.id] === keys[b.id]);
      if (other) out[a.id] = other.id;
    }
    return out;
  });
</script>

<div class="stack">
  <p class="intro muted">
    Click a box and press the keys you want. Every <Term id="hotkey">hotkey</Term> works while you're in a game.
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
          </div>
          <HotkeyInput
            label={action.label}
            value={keys[action.id]}
            conflict={conflicts[action.id]}
            onchange={(combo) => (keys[action.id] = combo)}
          />
        </div>
      {/each}
    </section>
  {/each}
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
  .text {
    display: grid;
    gap: 3px;
  }
  .text p {
    font-size: 13px;
    color: var(--text-dim);
  }
  .text p.conflict {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--rec);
  }
</style>
