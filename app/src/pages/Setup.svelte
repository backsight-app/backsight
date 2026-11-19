<script lang="ts">
  // First-run setup: the engine checks the PC, this page says what it found in plain words.
  // Starting before the check ends is fine: Backsight works with its defaults.
  import { invoke } from "../lib/uiwatch";
  import { onMount } from "svelte";
  import mark from "../../../assets/brand/backsight-mark.svg";
  import Kbd from "../lib/components/Kbd.svelte";
  import OverlayClashes from "../lib/components/OverlayClashes.svelte";
  import Term from "../lib/components/Term.svelte";
  import { fixCompatibility, formatDuration, vendorLabel, videoEncoder } from "../lib/content";
  import { ACTIONS } from "../lib/hotkeys";
  import { clashesFor, takenClashes, type Scan } from "../lib/overlays";
  import { app } from "../lib/settings.svelte";

  type Check = {
    cards: { name: string; vendor: string }[];
    pick: { encoder: string; automatic: boolean } | null;
    scan: Scan;
  };

  let check = $state<Check | null>(null);
  // After "Give Backsight other keys" the card stays up to say how it went.
  let swapped = $state(false);
  // A failed check keeps the apps found before.
  function onswap(r: Scan) {
    if (check && !r.failed) check.scan = r;
    swapped = true;
  }
  let failed = $state("");
  let switchedFrom = $state("");
  // The chosen encoder failed the check but may only be busy: offer the switch, don't make it.
  let offer = $state<{ from: string; to: string } | null>(null);
  // The check usually takes a few seconds; after 5 the page says so (you can start any time).
  let slow = $state(false);

  const label = (action: string) => ACTIONS.find((a) => a.id === action)?.label ?? action;
  const encoderLabel = (id: string) => videoEncoder(id)?.label ?? id;

  function switchTo(id: string) {
    app.settings.video.encoder = id;
    fixCompatibility(app.settings);
  }

  onMount(() => {
    const timer = setTimeout(() => (slow = true), 5000);
    // Starting early closes this page but the check still finishes: a choice that can't work here is still replaced.
    const asked = app.settings.video.encoder;
    invoke<Check>("setup_check", { settings: $state.snapshot(app.settings) }).then(
      (c) => {
        check = c;
        // Only a choice that doesn't work on this PC is replaced, and only if nobody changed it meanwhile.
        if (!c.pick || app.settings.video.encoder !== asked) return;
        if (c.pick.automatic) {
          switchedFrom = asked;
          switchTo(c.pick.encoder);
        } else {
          offer = { from: asked, to: c.pick.encoder };
        }
      },
      (e) => (failed = String(e)),
    );
    return () => clearTimeout(timer);
  });

  const hotkeys = $derived(app.settings.hotkeys as Record<string, string>);
  const clashes = $derived(check ? [...clashesFor(hotkeys, check.scan.apps), ...takenClashes(hotkeys, check.scan.taken)] : []);

  const encoder = $derived(videoEncoder(app.settings.video.encoder));
  const recorder = $derived(
    !encoder
      ? ""
      : !encoder.hardware
        ? "Your processor will do the recording. That works, but it can cost a few FPS in demanding games."
        : encoder.vendor === "nvidia"
          ? "Your NVIDIA card will do the recording, so games stay fast."
          : `Your ${vendorLabel[encoder.vendor]} graphics will do the recording, so games stay fast.`,
  );
  // NVIDIA and Intel Quick Sync are tested; AMD isn't yet (on hold), so an AMD-only PC gets a note.
  const untested = $derived(
    check && !check.cards.some((c) => c.vendor === "nvidia")
      ? [...new Set(check.cards.filter((c) => c.vendor === "amd").map((c) => vendorLabel[c.vendor]))]
      : [],
  );
  const main = ["save-replay", "toggle-recording", "screenshot"];

  function finish() {
    app.settings.setup.done = true;
    app.setupOpen = false;
  }
</script>

{#snippet enc(id: string)}<Term id={videoEncoder(id)?.glossary ?? id}>{encoderLabel(id)}</Term>{/snippet}

<div class="backdrop">
  <div class="panel" role="dialog" aria-modal="true" aria-labelledby="setup-title">
    <header>
      <img src={mark} alt="" width="44" height="44" />
      <div>
        <h1 id="setup-title">Let's get you set up</h1>
        <p class="muted">Backsight checks your PC in the background. You can start right away.</p>
      </div>
    </header>

    <section>
      <h2>1. What records your games</h2>
      {#if failed}
        <p>Backsight couldn't check your PC, so it keeps its usual settings.</p>
      {:else if !check}
        <p class="muted" role="status">Checking your graphics card. This takes a few seconds…</p>
        {#if slow}
          <p class="note">
            This is taking longer than usual. You can start now: Backsight uses its usual settings and finishes the check
            in the background.
          </p>
        {/if}
      {:else}
        <p>{recorder}</p>
        {#if switchedFrom}
          <p class="muted">
            {@render enc(switchedFrom)} doesn't work on this PC, so Backsight picked {@render enc(app.settings.video.encoder)} instead.
          </p>
        {/if}
        {#if offer && app.settings.video.encoder === offer.from}
          <div class="actions">
            <span>
              {@render enc(offer.from)} didn't start in the test. It may just be busy right now. If your clips don't save,
              switch:
            </span>
            <button type="button" class="btn" onclick={() => switchTo(offer!.to)}>Use {encoderLabel(offer.to)}</button>
          </div>
        {/if}
        {#if check.cards.length}
          <p class="muted">Found: {check.cards.map((c) => c.name).join(", ")}.</p>
        {/if}
        {#if untested.length}
          <p class="note">
            Backsight is tested on NVIDIA and Intel graphics so far. It may work on your {untested.join(" and ")} graphics, but
            that hasn't been checked yet. If something doesn't work, click Save a problem report on Home.
          </p>
        {/if}
      {/if}
    </section>

    <section>
      <h2>2. Hotkeys</h2>
      {#if !check && !failed}
        <p class="muted">Checking your hotkeys…</p>
      {:else if check && (swapped || clashes.length)}
        <OverlayClashes {clashes} {onswap} />
      {:else}
        {#if app.settings.replay.enabled}
          <p>
            Instant Replay is on: Backsight keeps the last {formatDuration(app.settings.replay.length_seconds)}. Press
            <Kbd combo={app.settings.hotkeys["save-replay"]} /> to save it as a clip.
          </p>
        {/if}
        <div class="keys">
          {#each main as action}
            <span>{label(action)} <Kbd combo={app.settings.hotkeys[action]} /></span>
          {/each}
        </div>
      {/if}
    </section>

    <footer>
      <p class="muted">You can close the window later. Backsight keeps running near the clock.</p>
      <button type="button" class="btn primary" onclick={finish}>Start using Backsight</button>
    </footer>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 50;
    display: grid;
    place-items: center;
    padding: 24px;
    background: rgb(5 8 15 / 0.75);
    overflow-y: auto;
  }
  .panel {
    display: grid;
    gap: 22px;
    width: min(640px, 100%);
    padding: 28px 30px;
    border-radius: 18px;
    border: 1px solid var(--border-strong);
    background: var(--bg-card);
  }
  header {
    display: flex;
    gap: 16px;
    align-items: center;
  }
  header div {
    flex: 1;
  }
  h1 {
    font-size: 22px;
    font-weight: 700;
  }
  h2 {
    margin-bottom: 8px;
    font-size: 15px;
    font-weight: 650;
  }
  section {
    display: grid;
    gap: 8px;
  }
  .note {
    padding: 10px 12px;
    border-radius: var(--radius-sm);
    background: rgb(212 178 91 / 0.1);
    color: #e8cf8a;
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
    color: var(--text-dim);
  }
  footer {
    display: flex;
    gap: 16px;
    align-items: center;
    justify-content: flex-end;
  }
</style>
