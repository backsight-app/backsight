<script lang="ts">
  import { isTauri } from "@tauri-apps/api/core";
  import { invoke } from "../lib/uiwatch";
  import { open } from "@tauri-apps/plugin-dialog";
  import { onDestroy } from "svelte";
  import mark from "../../../assets/brand/backsight-mark.svg";
  import { version, bundle } from "../../src-tauri/tauri.conf.json";
  import Row from "../lib/components/Row.svelte";
  import Toggle from "../lib/components/Toggle.svelte";
  import { app } from "../lib/settings.svelte";

  const g = $derived(app.settings.general);

  // Cancelling returns null and leaves the folder as it was. An empty folder means the default.
  async function choose(key: "video_folder" | "screenshot_folder") {
    const dir = await open({ directory: true, defaultPath: g[key] || undefined });
    if (typeof dir === "string") g[key] = dir;
  }

  // Moving the pop-ups and dot: Rust shows both as draggable samples; Done saves where they were left.
  let moving = $state(false);
  async function move(on: boolean) {
    moving = on;
    const spots = await invoke<{ popups: number[] | null; dot: number[] | null }>("hud_arrange", { on });
    if (spots.popups) g.popup_position = spots.popups;
    if (spots.dot) g.dot_position = spots.dot;
  }
  // About: the licenses file beside Backsight, or the source code page (bundle.homepage).
  let aboutError = $state("");
  const openAbout = (what: "licenses" | "source") =>
    invoke("about_open", { what }).then(
      () => (aboutError = ""),
      (e) => (aboutError = String(e)),
    );

  // Updates (ADR 0014): Backsight also checks by itself; Install closes Backsight and the installer reopens it.
  let update = $state<{ version: string; notes: string | null } | null>(null);
  let updateState = $state<"idle" | "checking" | "current" | "installing">("idle");
  let updateError = $state("");
  let percent = $state<number | null>(null);
  async function checkUpdate() {
    updateState = "checking";
    updateError = "";
    try {
      update = await invoke<typeof update>("check_update");
      updateState = update ? "idle" : "current";
    } catch (e) {
      updateError = String(e);
      updateState = "idle";
    }
  }
  async function installUpdate() {
    updateState = "installing";
    updateError = "";
    percent = null;
    const { listen } = await import("@tauri-apps/api/event");
    const unlisten = await listen<{ done: number; total: number | null }>("update-progress", (e) => {
      if (e.payload.total) percent = Math.floor((100 * e.payload.done) / e.payload.total);
    });
    try {
      await invoke("install_update");
    } catch (e) {
      updateError = String(e);
      updateState = "idle";
    } finally {
      unlisten();
    }
  }

  // Leaving the page keeps the spots too.
  // shortcut: closing the window while moving drops the new spots (Rust just ends moving); save them in onCloseRequested if people hit it.
  onDestroy(() => {
    if (moving) move(false);
  });
</script>

<div class="stack">
  <section class="card">
    <header><h2>Files</h2></header>
    <Row term="output-folder" label="Clips and recordings">
      <label class="text-input">
        <input value={g.video_folder} placeholder="Videos\Backsight" aria-label="Clips and recordings folder" onchange={(e) => (g.video_folder = e.currentTarget.value.trim())} />
      </label>
      {#if isTauri()}<button type="button" class="btn" aria-label="Choose clips and recordings folder" onclick={() => choose("video_folder")}>Choose…</button>{/if}
    </Row>
    <Row term="output-folder" label="Screenshots">
      <label class="text-input">
        <input value={g.screenshot_folder} placeholder="Pictures\Backsight" aria-label="Screenshots folder" onchange={(e) => (g.screenshot_folder = e.currentTarget.value.trim())} />
      </label>
      {#if isTauri()}<button type="button" class="btn" aria-label="Choose screenshots folder" onclick={() => choose("screenshot_folder")}>Choose…</button>{/if}
    </Row>
    <Row term="name-after-game">
      <Toggle label="Name files after the game" checked={g.name_after_game} onchange={(v) => (g.name_after_game = v)} />
    </Row>
    <Row term="folder-per-game">
      <Toggle label="A folder per game" checked={g.folder_per_game} onchange={(v) => (g.folder_per_game = v)} />
    </Row>
  </section>

  <section class="card">
    <header><h2>Startup</h2></header>
    <Row term="start-with-windows">
      <Toggle label="Start with Windows" checked={g.start_with_windows} onchange={(v) => (g.start_with_windows = v)} />
    </Row>
    <Row term="minimize-to-tray">
      <Toggle label="Keep running in the tray" checked={g.minimize_to_tray} onchange={(v) => (g.minimize_to_tray = v)} />
    </Row>
  </section>

  <section class="card">
    <header><h2>Letting you know</h2></header>
    <Row term="notifications">
      <Toggle label="Pop-ups" checked={g.notifications} onchange={(v) => (g.notifications = v)} />
    </Row>
    <Row term="sounds">
      <Toggle label="Sounds" checked={g.sounds} onchange={(v) => (g.sounds = v)} />
    </Row>
    <Row term="recording-indicator">
      <Toggle label="Recording dot" checked={g.recording_indicator} onchange={(v) => (g.recording_indicator = v)} />
    </Row>
    {#if isTauri()}
      <Row term="hud-position">
        <button type="button" class="btn" class:primary={moving} onclick={() => move(!moving)}>{moving ? "Done" : "Move"}</button>
        {#if !moving && (g.popup_position || g.dot_position)}
          <button type="button" class="btn" onclick={() => ((g.popup_position = null), (g.dot_position = null))}>Back to corner</button>
        {/if}
      </Row>
    {/if}
  </section>

  {#if isTauri()}
    <section class="card">
      <header><h2>Setup</h2></header>
      <Row term="first-run-setup" label="Check this PC again">
        <button type="button" class="btn" onclick={() => (app.setupOpen = true)}>Run setup</button>
      </Row>
    </section>

    <section class="card">
      <header><h2>Updates</h2></header>
      <Row term="updates" label="Newest version">
        {#if update}
          <button type="button" class="btn primary" disabled={updateState === "installing"} onclick={installUpdate}>
            {updateState === "installing" ? (percent === null ? "Downloading…" : `Downloading… ${percent}%`) : `Install ${update.version}`}
          </button>
        {:else}
          <button type="button" class="btn" disabled={updateState === "checking"} onclick={checkUpdate}>
            {updateState === "checking" ? "Checking…" : "Check"}
          </button>
        {/if}
      </Row>
      <div aria-live="polite">
        {#if updateState === "current"}<p class="update-note">You have the newest version.</p>{/if}
        {#if updateState === "installing"}<p class="update-note">{percent === null ? "Downloading…" : `Downloading… ${percent}%`} Backsight closes and reopens when it's done.</p>{/if}
        {#if update && updateState !== "installing"}<p class="update-note">Backsight {update.version} is ready. Installing closes and reopens Backsight; a recording in progress is saved first.</p>{/if}
        {#if update?.notes}<pre class="update-note notes">{update.notes}</pre>{/if}
        {#if updateError}<p class="update-note" role="alert">{updateError}</p>{/if}
      </div>
    </section>
  {/if}

  <section class="about">
    <img src={mark} alt="" width="48" height="48" />
    <div>
      <strong>Backsight {version}</strong>
      <p>Instant replay, recording and screenshots for Windows. Lightweight, and it explains itself.</p>
      <p>{bundle.copyright}. Free software under {bundle.license}, with no warranty.</p>
      <p>Source code: <span class="url">{bundle.homepage}</span></p>
      {#if isTauri()}
        <div class="links">
          <button type="button" class="btn" onclick={() => openAbout("licenses")}>Licenses</button>
          <button type="button" class="btn" onclick={() => openAbout("source")}>Source code</button>
        </div>
        {#if aboutError}<p role="alert">{aboutError}</p>{/if}
      {/if}
    </div>
  </section>
</div>

<style>
  .about {
    display: flex;
    gap: 16px;
    align-items: center;
    padding: 18px 22px;
    border-radius: var(--radius);
    border: 1px dashed var(--border-strong);
  }
  .about p {
    font-size: 13px;
    color: var(--text-dim);
  }
  .about .url {
    user-select: text;
  }
  .update-note {
    font-size: 13px;
    color: var(--text-dim);
    padding: 0 22px 12px;
  }
  .notes {
    white-space: pre-wrap;
    font-family: inherit;
    max-height: 160px;
    overflow: auto;
  }
  .links {
    display: flex;
    gap: 8px;
    margin-top: 10px;
  }
</style>
