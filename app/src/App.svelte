<script lang="ts">
  import { onMount } from "svelte";
  import { isTauri } from "@tauri-apps/api/core";
  import { invoke } from "./lib/uiwatch";
  import mark from "../../assets/brand/backsight-mark.svg";
  import Icon from "./lib/components/Icon.svelte";
  import ActionMenu from "./lib/components/ActionMenu.svelte";
  import Term from "./lib/components/Term.svelte";
  import SettingsProblem from "./lib/SettingsProblem.svelte";
  import { app, loadSettings, resetSection, scheduleSave, type ReplayStatus } from "./lib/settings.svelte";
  import { feed, markSeen, startFeed } from "./lib/feed.svelte";
  import { formatBytes, newSince } from "./lib/library";
  import Audio from "./pages/Audio.svelte";
  import Diagnostics from "./pages/Diagnostics.svelte";
  import General from "./pages/General.svelte";
  import Home from "./pages/Home.svelte";
  import Hotkeys from "./pages/Hotkeys.svelte";
  import Library from "./pages/Library.svelte";
  import Replay from "./pages/Replay.svelte";
  import Screenshots from "./pages/Screenshots.svelte";
  import Setup from "./pages/Setup.svelte";
  import Video from "./pages/Video.svelte";

  const pages = [
    { id: "home", label: "Home", icon: "home", subtitle: "Everything Backsight is set to do, at a glance.", component: Home, group: "Capture" },
    { id: "replay", label: "Instant Replay", icon: "replay", subtitle: "Always-on rewind. Save the last few minutes with one key.", component: Replay, section: "replay", group: "Capture" },
    { id: "video", label: "Video", icon: "video", subtitle: "How your clips and recordings are compressed and saved.", component: Video, section: "video", group: "Capture" },
    { id: "audio", label: "Audio & captions", icon: "mic", subtitle: "What you hear, and what ends up in your clips.", component: Audio, section: "audio", group: "Capture" },
    { id: "screenshots", label: "Screenshots", icon: "camera", subtitle: "Grab a moment as a picture.", component: Screenshots, section: "screenshot", group: "Capture" },
    { id: "library", label: "Library", icon: "library", subtitle: "Everything you've saved, newest first.", component: Library, group: "Yours" },
    { id: "hotkeys", label: "Hotkeys", icon: "keyboard", subtitle: "Shortcuts that work even while you're in a game.", component: Hotkeys, section: "hotkeys", group: "Yours" },
    { id: "general", label: "General", icon: "sliders", subtitle: "Where files go and how Backsight starts.", component: General, section: "general", group: "Yours" },
    { id: "diagnostics", label: "Diagnostics", icon: "bolt", subtitle: "Live numbers that show how well Backsight is keeping up.", component: Diagnostics, group: "Yours" },
  ] as const;

  // Resetting asks for a second click on the same page, so a stray click changes nothing.
  let armed = $state<string | null>(null);
  let disarm: ReturnType<typeof setTimeout> | undefined;
  function reset(p: (typeof pages)[number]) {
    if (!("section" in p)) return;
    clearTimeout(disarm);
    if (armed === p.id) {
      resetSection(p.section);
      armed = null;
    } else {
      armed = p.id;
      disarm = setTimeout(() => (armed = null), 4000);
    }
  }

  const statusText = {
    preview: "Preview: changes aren't saved",
    loading: "Loading settings…",
    saving: "Saving…",
    saved: "All changes saved",
    error: "Couldn't save",
  };

  let current = $state(location.hash.slice(1) || "home");
  const page = $derived(pages.find((p) => p.id === current) ?? pages[0]);
  let menuButton = $state<HTMLButtonElement>();
  type Anchor = Element | { getBoundingClientRect: () => DOMRect };
  let menu = $state.raw<{ anchor: Anchor; opener: HTMLElement | null } | null>(null);
  let replay = $state<ReplayStatus | null>(null);
  let menuFeedback = $state("");
  // Opening Library clears its "new" count; files saved while it is open don't count either.
  $effect(() => {
    if (current === "library" && feed.lib) markSeen();
  });
  const fresh = $derived(feed.lib && current !== "library" ? newSince(feed.lib.items, feed.seen) : 0);
  // About 10 minutes of 4K recording; below it a long recording may stop.
  const low = $derived(!!feed.space && feed.space[0] < 5 * 1024 ** 3);
  const used = $derived(feed.space && feed.space[1] ? 1 - feed.space[0] / feed.space[1] : 0);
  const setupVisible = $derived(app.loaded && (app.setupOpen || !app.settings.setup?.done));
  $effect(() => { if (setupVisible) closeMenu(false); });

  function closeMenu(restore: boolean) {
    const opener = menu?.opener;
    menu = null;
    if (restore) opener?.focus({ preventScroll: true });
  }

  function showMenu(anchor: Anchor) {
    if (setupVisible) return;
    const opener = document.activeElement instanceof HTMLElement && document.activeElement !== document.body
      ? document.activeElement : menuButton ?? null;
    const opened = { anchor, opener };
    replay = null;
    menuFeedback = "";
    menu = opened;
    if (isTauri()) {
      void invoke<ReplayStatus>("get_status").then((status) => {
        if (menu === opened) replay = status;
      }, () => {});
    }
  }

  function context(e: MouseEvent) {
    if (e.defaultPrevented || !menuButton || setupVisible) return;
    if ((e.target as Element).closest("input, textarea, select, [contenteditable], button, a, video, audio, dialog, [role=menu], [role=listbox]")) return;
    e.preventDefault();
    const point = new DOMRect(e.clientX, e.clientY, 0, 0);
    showMenu({ getBoundingClientRect: () => point });
  }

  function menuCommand(command: string, args: Record<string, string>) {
    void invoke(command, args).catch((e) => (menuFeedback = String(e)));
  }

  const actions = $derived([
    { label: "Open Library", run: () => { location.hash = "library"; } },
    ...(isTauri() ? [
      { label: "Open clips folder", run: () => menuCommand("library_action", { action: "clips" }) },
      { label: "Open screenshots folder", run: () => menuCommand("library_action", { action: "screenshots" }) },
    ] : []),
    ...(replay ? [{
      label: replay.state === "on" ? "Turn Instant Replay off" : replay.state === "failed" ? "Clear replay error" : "Turn Instant Replay on",
      run: () => menuCommand("replay_menu_action", { action: replay?.state === "off" ? "on" : "off" }),
    }] : []),
    ...(replay?.state === "on" && replay.buffer_seconds > 0 ? [{
      label: "Save clip",
      run: () => menuCommand("replay_menu_action", { action: "save" }),
    }] : []),
  ]);

  onMount(() => {
    const onHash = () => {
      closeMenu(false);
      menuFeedback = "";
      current = location.hash.slice(1) || "home";
    };
    addEventListener("hashchange", onHash);
    loadSettings();
    startFeed();
    return () => removeEventListener("hashchange", onHash);
  });

  // Auto-save: any change to settings is written shortly after.
  $effect(() => {
    JSON.stringify(app.settings);
    scheduleSave();
  });
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="shell" inert={setupVisible} oncontextmenu={context}>
  <aside class="sidebar">
    <div class="brand">
      <img src={mark} alt="" width="24" height="24" />
      <span class="wordmark">Backsight</span>
    </div>
    <nav aria-label="Sections">
      {#each pages as p, i}
        {#if p.group !== pages[i - 1]?.group}<span class="group">{p.group}</span>{/if}
        <a href={`#${p.id}`} class:active={p.id === page.id} aria-current={p.id === page.id ? "page" : undefined}>
          <Icon name={p.icon} size={17} />
          <span class="label">{p.label}</span>
          {#if p.id === "library" && fresh}<span class="badge" title="Saved since you last opened Library">{fresh} new</span>{/if}
        </a>
      {/each}
    </nav>
    <div class="sidebar-foot">
      <button class="btn app-menu" type="button" bind:this={menuButton} aria-haspopup="menu" aria-expanded={!!menu}
        onkeydown={(e) => {
          if (!e.altKey && !e.ctrlKey && !e.metaKey && (e.key === "ContextMenu" ||
            (e.shiftKey && e.key === "F10" && !Object.values(app.settings.hotkeys).some((key) => String(key).trim().toLowerCase() === "shift+f10")))) {
            e.preventDefault();
            showMenu(e.currentTarget);
          }
        }}
        onclick={(e) => showMenu(e.currentTarget)}>Backsight menu</button>
      {#if feed.space}
        <div class="space" class:low title={low ? "The drive that holds your clips is nearly full: a recording may stop. Delete old clips or pick another folder on the General page." : "Free space on the drive that holds your clips folder"}>
          <span>Clips folder</span><span class="free">{low ? "Only " : ""}{formatBytes(feed.space[0])} free</span>
          <div class="bar" role="meter" aria-label="Clips drive used" aria-valuemin="0" aria-valuemax="100" aria-valuenow={Math.round(used * 100)}>
            <div style:width={`${Math.round(used * 100)}%`}></div>
          </div>
        </div>
      {/if}
      <span class="status" class:error={app.status === "error"} title={app.error} role="status">
        <span class="status-dot"></span>{statusText[app.status]}
      </span>
    </div>
  </aside>

  <main>
    <header class="page-head" class:hidden={page.id === "home"}>
      <div>
        <h1>{page.label}</h1>
        <p class="muted">{page.subtitle}</p>
      </div>
      {#if "section" in page}
        <button type="button" class="reset" class:armed={armed === page.id} disabled={!app.loaded && app.status !== "preview"} onclick={() => reset(page)}>
          {armed === page.id ? `Click again to reset ${page.label}` : "Reset to defaults"}
        </button>
      {/if}
      {#if "section" in page}
        <div class="mode" role="group" aria-label="Settings detail">
          <Term id="advanced-mode">Detail</Term>
          <div class="segmented">
            <button type="button" aria-pressed={!app.advanced} onclick={() => (app.advanced = false)}>Simple</button>
            <button type="button" aria-pressed={app.advanced} onclick={() => (app.advanced = true)}>Advanced</button>
          </div>
        </div>
      {/if}
    </header>
    <div class="page">
      {#if menuFeedback}<div class="notice" role="status">{menuFeedback}</div>{/if}
      <SettingsProblem />
      <page.component />
    </div>
  </main>
</div>
{#if menu}
  {#key menu}
    <ActionMenu label="Backsight" anchor={menu.anchor} {actions} close={closeMenu} />
  {/key}
{/if}
{#if setupVisible}
  <Setup />
{/if}

<style>
  .app-menu { align-self: flex-start; margin-bottom: 8px; }
  .shell {
    display: grid;
    grid-template-columns: 216px 1fr;
    height: 100%;
  }
  .sidebar {
    display: flex;
    flex-direction: column;
    gap: 18px;
    padding: 20px 12px 16px;
    border-right: 1px solid var(--border);
    background: var(--bg-elev);
    overflow-y: auto;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 4px 10px 6px;
  }
  .wordmark {
    font: 700 16px/1 var(--font-display);
    letter-spacing: -0.01em;
  }
  nav {
    display: grid;
    gap: 2px;
  }
  .group {
    padding: 14px 12px 6px;
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--text-faint);
  }
  .group:first-child {
    padding-top: 0;
  }
  nav a {
    position: relative;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 12px;
    border-radius: 8px;
    color: var(--text-dim);
    text-decoration: none;
    font-size: 13.5px;
    font-weight: 500;
    transition: background 0.15s, color 0.15s;
  }
  nav a .label {
    flex: 1;
  }
  nav a:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  nav a.active {
    background: var(--bg-hover);
    color: #fff;
    font-weight: 600;
  }
  nav a.active::before {
    content: "";
    position: absolute;
    left: 0;
    top: 8px;
    bottom: 8px;
    width: 2px;
    border-radius: 1px;
    background: var(--accent);
  }
  nav a.active :global(svg) {
    color: var(--accent);
  }
  nav a:focus-visible {
    outline: none;
    box-shadow: 0 0 0 2px var(--focus);
  }
  .badge {
    padding: 1px 7px;
    border-radius: 999px;
    background: var(--accent-soft);
    color: var(--accent);
    font-size: 11px;
    font-weight: 600;
  }
  .sidebar-foot {
    margin-top: auto;
    display: grid;
    gap: 6px;
    padding: 14px 12px 4px;
    border-top: 1px solid var(--border);
    font-size: 12px;
  }
  .space {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 6px;
    margin-bottom: 4px;
    color: var(--text-faint);
  }
  .space .free {
    color: var(--text);
  }
  .space.low .free {
    color: var(--warn);
  }
  .space.low .bar div {
    background: var(--warn);
  }
  .bar {
    grid-column: 1 / -1;
    height: 3px;
    border-radius: 2px;
    background: var(--border);
  }
  .bar div {
    height: 3px;
    border-radius: 2px;
    background: var(--text-faint);
  }
  .status {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--text-dim);
  }
  .status-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--ok);
  }
  .status.error {
    color: var(--rec);
  }
  .status.error .status-dot {
    background: var(--rec);
  }
  main {
    display: flex;
    flex-direction: column;
    min-width: 0;
    overflow: hidden;
  }
  .page-head {
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    gap: 20px;
    padding: 28px 36px 20px;
  }
  .page-head.hidden {
    display: none;
  }
  .page-head h1 {
    font-size: 26px;
    font-weight: 700;
  }
  .page-head p {
    margin-top: 4px;
  }
  .reset {
    margin-left: auto;
    padding: 6px 14px;
    border-radius: 10px;
    border: 1px solid var(--border);
    background: var(--bg-input);
    color: var(--text-dim);
    font: inherit;
    font-size: 13px;
    cursor: pointer;
  }
  .reset:hover {
    color: var(--text);
    border-color: var(--border-strong);
  }
  .reset.armed {
    color: var(--warn);
    border-color: var(--warn);
  }
  .mode {
    display: flex;
    align-items: center;
    gap: 12px;
    color: var(--text-dim);
    font-size: 13px;
  }
  .segmented {
    display: inline-flex;
    padding: 3px;
    border-radius: 10px;
    border: 1px solid var(--border);
    background: var(--bg-input);
  }
  .segmented button {
    padding: 6px 14px;
    border: none;
    border-radius: 7px;
    background: transparent;
    color: var(--text-dim);
    font: 600 13px var(--font);
    cursor: pointer;
  }
  .segmented button[aria-pressed="true"] {
    background: var(--bg-hover);
    color: var(--text);
    box-shadow: 0 1px 2px rgb(0 0 0 / 0.3);
  }
  .segmented button:focus-visible {
    outline: none;
    box-shadow: 0 0 0 2px var(--focus);
  }
  .page {
    flex: 1;
    overflow-y: auto;
    padding: 0 36px 36px;
  }
  .page > :global(*) {
    max-width: 920px;
  }
</style>
