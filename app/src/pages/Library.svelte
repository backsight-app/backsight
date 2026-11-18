<script lang="ts">
  import { isTauri } from "@tauri-apps/api/core";
  import { invoke } from "../lib/uiwatch";
  import { onMount, tick } from "svelte";
  import Icon from "../lib/components/Icon.svelte";
  import Kbd from "../lib/components/Kbd.svelte";
  import LibraryCard, { thumbSrc } from "../lib/components/LibraryCard.svelte";
  import ActionMenu from "../lib/components/ActionMenu.svelte";
  import Select from "../lib/components/Select.svelte";
  import { codecLabel } from "../lib/content";
  import {
    activeFilters, applyFilters, datePresets, durationPresets, filtersFor, formatBytes, formatClock, formatsIn,
    formatTime, groupByDay, isVertical, noFilters, resolutionPresets, sizePresets, tabKind, totalBytes,
    type Library, type LibraryItem, type Preset, type Tab,
  } from "../lib/library";
  import { app } from "../lib/settings.svelte";

  const tabs: { id: Tab; label: string; one: string; many: string }[] = [
    { id: "all", label: "All", one: "item", many: "items" },
    { id: "screenshots", label: "Screenshots", one: "screenshot", many: "screenshots" },
    { id: "videos", label: "Videos", one: "video", many: "videos" },
  ];

  // $state.raw: thousands of items must not become deep reactive proxies.
  let lib = $state.raw<Library | null>(null);
  let error = $state("");
  let loadedAt = $state(Date.now());
  let tab = $state<Tab>("all");
  let filters = $state(noFilters());

  let seq = 0;
  async function load() {
    const mine = ++seq;
    try {
      const l = isTauri() || !import.meta.env.DEV ? await invoke<Library>("list_library") : await import("../lib/library-mock").then((m) => m.mockLibrary());
      if (mine !== seq) return; // a newer refresh is already on its way
      lib = { ...l, items: [...l.items].sort((a, b) => b.created_ms - a.created_ms) };
      loadedAt = Date.now();
      error = "";
      dismiss(); // rows may have moved or gone
      closeMenu(false);
    } catch (e) {
      if (mine === seq) error = String(e);
    }
  }

  onMount(() => {
    let gone = false;
    let unlisten: (() => void) | undefined;
    load();
    if (isTauri()) {
      import("@tauri-apps/api/event")
        .then((m) => m.listen("library-changed", load))
        .then((u) => (gone ? u() : (unlisten = u)));
    }
    return () => {
      gone = true;
      unlisten?.();
      clearTimeout(timer);
    };
  });

  const tabItems = $derived((lib?.items ?? []).filter((i) => !tabKind[tab] || i.kind === tabKind[tab]));
  const shown = $derived(applyFilters(tabItems, filters, tab, loadedAt));
  const groups = $derived(groupByDay(shown, loadedAt));
  const byPath = $derived(new Map(shown.map((i) => [i.path, i])));
  const active = $derived(activeFilters(filters, tab));
  const formats = $derived(formatsIn(tabItems));
  const fe = $derived(filtersFor(tab));
  const info = $derived(tabs.find((t) => t.id === tab)!);
  const noun = (n: number) => `${n.toLocaleString()} ${n === 1 ? info.one : info.many}`;
  const count = (t: Tab) => (lib?.items ?? []).filter((i) => !tabKind[t] || i.kind === tabKind[t]).length;

  // A format that vanished (refresh, tab change) must not silently hide everything.
  $effect(() => {
    if (filters.format !== "any" && !formats.includes(filters.format)) filters.format = "any";
  });

  function setTab(t: Tab) {
    tab = t;
    Object.assign(filters, { duration: "any", resolution: "any" });
  }
  const choices = (list: Preset[]) => list.map((p) => ({ value: p.id, label: p.label }));
  const formatChoices = $derived([{ value: "any", label: "Any format" }, ...formats.map((e) => ({ value: e, label: e.toUpperCase() }))]);
  const dateChoices = $derived(choices(datePresets(loadedAt)));

  // Hover and focus card. A short delay keeps a sweep of the mouse from firing a request per row.
  let hovered = $state.raw<{ item: LibraryItem; el: HTMLElement } | null>(null);
  let pending: HTMLElement | null = null;
  let timer: ReturnType<typeof setTimeout>;
  let listEl = $state<HTMLElement>();
  let menu = $state.raw<{ item: LibraryItem | null; opener: HTMLElement; point?: DOMRect } | null>(null);
  let viewer = $state.raw<{ item: LibraryItem; src: string; opener: HTMLElement } | null>(null);
  let dialog = $state<HTMLDialogElement>();
  let mediaError = $state(false);
  let feedback = $state("");

  function closeMenu(restore: boolean) {
    const opener = menu?.opener;
    if (restore) opener?.focus({ preventScroll: true });
    menu = null;
  }

  function showMenu(item: LibraryItem | null, opener: HTMLElement, point?: DOMRect) {
    dismiss();
    menu = { item, opener, point };
  }

  function context(e: MouseEvent) {
    const target = e.target as HTMLElement;
    if (target.closest("input, textarea, select, [contenteditable], [role=menu], button:not(.more), a")) return;
    const row = target.closest<HTMLElement>(".row");
    const item = viewer?.item ?? (row ? byPath.get(row.dataset.path!) : null);
    if (!lib || (row && !item)) return;
    e.preventDefault();
    const opener = viewer ? dialog?.querySelector<HTMLButtonElement>(".more") :
      row ?? document.querySelector<HTMLButtonElement>(".library .folder-menu")!;
    if (opener) showMenu(item ?? null, opener, new DOMRect(e.clientX, e.clientY, 0, 0));
  }

  function actionKeys(e: KeyboardEvent) {
    if (e.key !== "ContextMenu" && !(e.shiftKey && e.key === "F10")) return;
    const target = e.target as HTMLElement;
    const row = target.closest<HTMLElement>(".row");
    const item = viewer?.item ?? (row ? byPath.get(row.dataset.path!) : null);
    if (!item && !target.matches(".folder-menu")) return;
    e.preventDefault();
    showMenu(item ?? null, target);
  }

  async function nativeAction(action: string, path?: string) {
    if (!isTauri()) throw new Error("Open the Backsight desktop app to use this command.");
    return invoke<string | null>("library_action", { action, path });
  }

  function run(task: () => Promise<unknown>, success = "") {
    feedback = "";
    void (async () => {
      try {
        await task();
        feedback = success;
      } catch (e) {
        feedback = e instanceof Error ? e.message : String(e);
      }
    })();
  }

  async function view(item: LibraryItem, opener: HTMLElement) {
    const path = await nativeAction("view", item.path);
    mediaError = false;
    if (!path) throw new Error("Backsight couldn't find this file.");
    viewer = { item, src: thumbSrc(path), opener };
    await tick();
    dialog?.showModal();
  }

  async function imageBlob(item: LibraryItem) {
    const path = await nativeAction("view", item.path);
    if (!path) throw new Error("Backsight couldn't find this file.");
    const img = new Image();
    img.crossOrigin = "anonymous";
    img.src = thumbSrc(path);
    try {
      await img.decode();
    } catch {
      throw new Error("Backsight couldn't read this image. Try opening it in your viewer.");
    }
    const canvas = document.createElement("canvas");
    canvas.width = img.naturalWidth;
    canvas.height = img.naturalHeight;
    canvas.getContext("2d")!.drawImage(img, 0, 0);
    return new Promise<Blob>((resolve, reject) => canvas.toBlob((blob) => {
      if (blob) resolve(blob);
      else reject(new Error("Backsight couldn't copy this image."));
    }));
  }

  function copyImage(item: LibraryItem) {
    if (!navigator.clipboard?.write || typeof ClipboardItem === "undefined")
      throw new Error("Image copying is unavailable here.");
    return navigator.clipboard.write([new ClipboardItem({ "image/png": imageBlob(item) })]);
  }

  function menuActions(item: LibraryItem | null, opener: HTMLElement) {
    if (!item) return [
      { label: "Refresh", run: load },
      ...(lib?.video_dir ? [{ label: "Open clips folder", run: () => run(() => nativeAction("clips")) }] : []),
      ...(lib?.screenshot_dir ? [{ label: "Open screenshots folder", run: () => run(() => nativeAction("screenshots")) }] : []),
      ...(active.length ? [{ label: "Clear filters", run: () => (filters = noFilters()) }] : []),
    ];
    const video = item.kind === "video";
    const readable = video || item.thumbnail !== null;
    return [
      ...(!viewer && readable ? [{ label: video ? "Play" : "View", run: () => run(() => view(item, opener)) }] : []),
      { label: video ? "Open in default player" : "Open in default viewer", run: () => run(() => nativeAction("open", item.path)) },
      ...(!video && readable ? [{ label: "Copy image", run: () => run(() => copyImage(item), "Image copied.") }] : []),
      { label: "Show in folder", run: () => run(() => nativeAction("reveal", item.path)) },
      { label: "Copy file path", run: () => run(() => nativeAction("copy_path", item.path), "File path copied.") },
    ];
  }

  $effect(() => {
    if (menu?.item && !viewer && !byPath.has(menu.item.path)) closeMenu(false);
  });

  function dismiss() {
    clearTimeout(timer);
    pending = null;
    hovered = null;
  }
  function enter(e: Event) {
    if (menu || viewer) return;
    const row = (e.target as Element).closest<HTMLElement>(".row");
    if (!row) return dismiss();
    if (row === pending) return;
    const item = byPath.get(row.dataset.path!);
    const delay = hovered ? 150 : 350;
    dismiss();
    if (!item) return;
    pending = row;
    timer = setTimeout(() => (hovered = { item, el: row }), delay);
  }
  // Clicking a row (or Enter on it) plays or shows it, like the menu's first item.
  function openItem(item: LibraryItem, opener: HTMLElement) {
    dismiss();
    const readable = item.kind === "video" || item.thumbnail !== null;
    run(() => (readable ? view(item, opener) : nativeAction("open", item.path)));
  }
  function rowClick(e: MouseEvent) {
    const target = e.target as Element;
    const row = target.closest<HTMLElement>(".row");
    const item = row && !target.closest("button") ? byPath.get(row.dataset.path!) : undefined;
    if (row && item) openItem(item, row);
  }
  function keys(e: KeyboardEvent) {
    if (e.key === "Enter" && (e.target as Element).matches(".row")) {
      const row = e.target as HTMLElement;
      const item = byPath.get(row.dataset.path!);
      if (item) {
        e.preventDefault();
        openItem(item, row);
      }
      return;
    }
    const step = e.key === "ArrowDown" ? 1 : e.key === "ArrowUp" ? -1 : 0;
    if (!step || !(e.target as Element).matches(".row")) return;
    const rows = [...listEl!.querySelectorAll<HTMLElement>(".row")];
    const next = rows[rows.indexOf(e.target as HTMLElement) + step];
    if (next) {
      e.preventDefault();
      next.focus();
    }
  }

  const meta = (i: LibraryItem) =>
    [
      i.kind === "video" ? "Video" : "Screenshot",
      i.width && i.height ? `${i.width}×${i.height}` : null,
      i.fps ? `${i.fps} FPS` : null,
      i.video_codec ? codecLabel(i.video_codec) : null,
    ]
      .filter(Boolean)
      .join(" · ");
</script>

<svelte:window onkeydown={(e) => e.key === "Escape" && dismiss()} />

{#snippet actionsMenu()}
  {#if menu}
    {#key menu}
      <ActionMenu label={menu.item?.name ?? "Library"} anchor={menu.point ? { getBoundingClientRect: () => menu?.point ?? new DOMRect() } : menu.opener} actions={menuActions(menu.item, menu.opener)} close={closeMenu} />
    {/key}
  {/if}
{/snippet}

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="library" oncontextmenu={context} onkeydown={actionKeys}>
  <div class="top">
    {#if lib}
      <div class="top-actions">
        {#if lib.video_dir}<button type="button" class="btn" onclick={() => run(() => nativeAction("clips"))}>Open clips folder</button>{/if}
        {#if lib.screenshot_dir}<button type="button" class="btn" onclick={() => run(() => nativeAction("screenshots"))}>Open screenshots folder</button>{/if}
        <button type="button" class="btn more folder-menu" aria-haspopup="menu"
          aria-expanded={!!menu && !menu.item} onclick={(e) => showMenu(null, e.currentTarget)}>More</button>
      </div>
    {/if}
    <div class="tabs" role="group" aria-label="Show">
      {#each tabs as t}
        <button type="button" aria-pressed={tab === t.id} onclick={() => setTab(t.id)}>
          {t.label}<span class="n">{count(t.id).toLocaleString()}</span>
        </button>
      {/each}
    </div>
    {#if lib}
      <div class="total" aria-live="polite">
        <div><strong>{formatBytes(totalBytes(tabItems))}</strong><span class="faint">in {noun(tabItems.length)}</span></div>
        {#if active.length}
          <div class="filtered">
            {formatBytes(totalBytes(shown))} in {shown.length.toLocaleString()} matching · filtered by {active.join(", ")}
          </div>
        {/if}
      </div>
    {/if}
  </div>

  {#if lib && tabItems.length}
    <div class="filters" role="group" aria-label="Filters">
      <div class="f" class:on={filters.date !== "any"}>
        <span>Date</span>
        <div class="ctl"><Select name="date" label="Date" value={filters.date} choices={dateChoices} onchange={(v) => (filters.date = v)} />
        {#if filters.date === "custom"}
          <input type="datetime-local" aria-label="From" bind:value={filters.dateFrom} />
          <span class="to">to</span>
          <input type="datetime-local" aria-label="To" bind:value={filters.dateTo} />
        {/if}
        </div>
      </div>
      <div class="f" class:on={filters.size !== "any"}>
        <span>Size</span>
        <div class="ctl"><Select name="size" label="Size" value={filters.size} choices={choices(sizePresets)} onchange={(v) => (filters.size = v)} />
        {#if filters.size === "custom"}
          <label class="number"><input type="number" min="0" placeholder="Min" aria-label="Smallest size in MB" bind:value={filters.sizeMinMb} /><span class="unit">MB</span></label>
          <span class="to">to</span>
          <label class="number"><input type="number" min="0" placeholder="Max" aria-label="Largest size in MB" bind:value={filters.sizeMaxMb} /><span class="unit">MB</span></label>
        {/if}
        </div>
      </div>
      <div class="f" class:on={filters.format !== "any"}>
        <span>Format</span>
        <div class="ctl"><Select name="format" label="Format" value={filters.format} choices={formatChoices} onchange={(v) => (filters.format = v)} /></div>
      </div>
      {#if fe.duration}
        <div class="f" class:on={filters.duration !== "any"}>
          <span>Length</span>
          <div class="ctl"><Select name="duration" label="Length" value={filters.duration} choices={choices(durationPresets)} onchange={(v) => (filters.duration = v)} /></div>
        </div>
      {/if}
      {#if fe.resolution}
        <div class="f" class:on={filters.resolution !== "any"}>
          <span>Resolution</span>
          <div class="ctl"><Select name="resolution" label="Resolution" value={filters.resolution} choices={choices(resolutionPresets)} onchange={(v) => (filters.resolution = v)} /></div>
        </div>
      {/if}
      {#if active.length}
        <button type="button" class="clear" onclick={() => (filters = noFilters())}>Clear filters</button>
      {/if}
    </div>
  {/if}

  {#if feedback && !viewer}<div class="notice" role="status">{feedback}</div>{/if}
  {#if error && lib}
    <div class="notice"><Icon name="alert" size={18} /><span>Couldn't refresh the list, so this may be out of date. {error}</span></div>
  {/if}

  {#if !lib}
    {#if error}
      <div class="empty">
        <Icon name="alert" size={30} />
        <h2>Couldn't read your library</h2>
        <p>{error} Make sure Backsight can open your save folders, then try again.</p>
        <button type="button" class="btn" onclick={load}>Try again</button>
      </div>
    {:else}
      <div class="empty"><p>Loading your library…</p></div>
    {/if}
  {:else if !lib.video_dir && !lib.screenshot_dir}
    <div class="empty">
      <Icon name="folder" size={30} />
      <h2>No save folders yet</h2>
      <p>Choose where Backsight saves clips and screenshots, and they'll show up here.</p>
      <a class="btn" href="#general">Choose folders</a>
    </div>
  {:else if !lib.items.length}
    <div class="empty">
      <Icon name="video" size={30} />
      <h2>Nothing saved yet</h2>
      <p>
        Press <Kbd combo={app.settings.hotkeys["save-replay"]} /> to save a clip or
        <Kbd combo={app.settings.hotkeys["screenshot"]} /> for a screenshot, and it will land here.
      </p>
    </div>
  {:else if !tabItems.length}
    <div class="empty">
      <Icon name={tab === "videos" ? "video" : "image"} size={30} />
      <h2>No {info.many} yet</h2>
      <p>
        {#if tab === "videos" && !lib.video_dir}Choose a video folder in <a href="#general">General</a>.
        {:else if tab === "screenshots" && !lib.screenshot_dir}Choose a screenshot folder in <a href="#general">General</a>.
        {:else}Anything you save will show up here.{/if}
      </p>
    </div>
  {:else if !shown.length}
    <div class="empty">
      <Icon name="info" size={30} />
      <h2>Nothing matches these filters</h2>
      <p>Try a wider range, or start over.</p>
      <button type="button" class="btn" onclick={() => (filters = noFilters())}>Clear filters</button>
    </div>
  {:else}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="list"
      bind:this={listEl}
      onscroll={() => { dismiss(); closeMenu(false); }}
      onpointerover={enter}
      onpointerleave={dismiss}
      onfocusin={enter}
      onfocusout={dismiss}
      onkeydown={keys}
      onclick={rowClick}
    >
      <!-- shortcut: rows are plain DOM with content-visibility:auto, fine for thousands; use a windowed list past ~50k items -->
      {#each groups as g, gi (g.key)}
        <section>
          <h3 class="day">
            <span>{g.label}</span>
            <span class="faint">{g.items.length.toLocaleString()} {g.items.length === 1 ? "item" : "items"} · {formatBytes(g.bytes)}</span>
          </h3>
          <ul>
            {#each g.items as it, n (it.path)}
              <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
              <li
                class="row"
                data-path={it.path}
                tabindex={gi === 0 && n === 0 ? 0 : -1}
                aria-describedby={hovered?.item === it ? "lib-card" : undefined}
              >
                <div class="thumb">
                  <Icon name={it.kind === "video" ? "video" : "image"} size={20} />
                  {#if it.thumbnail}
                    <img src={thumbSrc(it.thumbnail)} alt="" loading="lazy" decoding="async" onerror={(e) => ((e.currentTarget as HTMLImageElement).style.display = "none")} />
                  {/if}
                  {#if isVertical(it)}<span class="tall">Vertical</span>{/if}
                  <!-- "CC": "Captions" doesn't fit beside the length on the small picture. -->
                  {#if it.captions}<span class="cc" title="Has captions">CC</span>{/if}
                  {#if it.duration_ms !== null}<span class="dur">{formatClock(it.duration_ms)}</span>{/if}
                </div>
                <div class="main">
                  <span class="name">{it.name}</span>
                  <span class="meta">{meta(it)}</span>
                </div>
                <span class="ext">{it.ext}</span>
                <span class="when">{formatTime(it.created_ms)}</span>
                <span class="bytes">{formatBytes(it.bytes)}</span>
                <button type="button" class="more item-menu" tabindex="-1" aria-label={`More actions for ${it.name}`}
                  aria-haspopup="menu" aria-expanded={menu?.item?.path === it.path}
                  onclick={(e) => showMenu(it, e.currentTarget)}>⋯</button>
              </li>
            {/each}
          </ul>
        </section>
      {/each}
    </div>
  {/if}
</div>

{#if viewer}
  <dialog bind:this={dialog} aria-label={viewer.item.name} oncontextmenu={context} onkeydown={actionKeys}
    onclose={() => { const opener = viewer?.opener; closeMenu(false); viewer = null; opener?.focus(); }}>
    <header>
      <strong title={viewer.item.name}>{viewer.item.name}</strong>
      <button type="button" class="btn more" aria-haspopup="menu" aria-expanded={!!menu}
        onclick={(e) => viewer && showMenu(viewer.item, e.currentTarget)}>More actions</button>
      <button type="button" class="btn" onclick={() => dialog?.close()}>Close</button>
    </header>
    {#if mediaError}
      <p>Backsight can't show this file here.</p>
      <button type="button" class="btn" onclick={() => run(() => nativeAction("open", viewer!.item.path))}>Open in default app</button>
    {:else if viewer.item.kind === "video"}
      <!-- svelte-ignore a11y_media_has_caption -->
      <video src={viewer.src} controls autoplay playsinline onerror={() => (mediaError = true)}></video>
    {:else}
      <img src={viewer.src} alt={viewer.item.name} onerror={() => (mediaError = true)} />
    {/if}
    {#if feedback}<p role="status">{feedback}</p>{/if}
    {@render actionsMenu()}
  </dialog>
{:else}
  {@render actionsMenu()}
{/if}

{#if hovered}
  {#key hovered.item.path}
    <LibraryCard id="lib-card" item={hovered.item} anchor={hovered.el} />
  {/key}
{/if}

<style>
  .library {
    display: flex;
    flex-direction: column;
    gap: 14px;
    height: 100%;
  }
  .faint {
    color: var(--text-faint);
  }

  .top {
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    gap: 20px;
  }
  .tabs {
    display: inline-flex;
    padding: 3px;
    border-radius: 10px;
    border: 1px solid var(--border);
    background: var(--bg-input);
  }
  .tabs button {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 7px 14px;
    border: none;
    border-radius: 7px;
    background: transparent;
    color: var(--text-dim);
    font: 600 13px var(--font);
    cursor: pointer;
  }
  .tabs button[aria-pressed="true"] {
    background: var(--bg-hover);
    color: var(--text);
    box-shadow: 0 1px 2px rgb(0 0 0 / 0.3);
  }
  .tabs button:focus-visible {
    outline: none;
    box-shadow: 0 0 0 2px var(--focus);
  }
  .n {
    font-weight: 500;
    font-size: 12px;
    color: var(--text-faint);
    font-variant-numeric: tabular-nums;
  }
  .total {
    min-height: 46px;
    text-align: right;
    font-size: 13px;
    line-height: 1.3;
  }
  .total strong {
    margin-right: 6px;
    font: 700 22px var(--font-display);
    letter-spacing: -0.01em;
    font-variant-numeric: tabular-nums;
  }
  .filtered {
    color: var(--accent-2);
    font-size: 12.5px;
  }

  .filters {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: 10px 14px;
  }
  .f {
    display: grid;
    gap: 6px;
  }
  .ctl {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .f > span:first-child {
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--text-faint);
  }
  .f.on > span:first-child {
    color: var(--accent-2);
  }
  .f :global(.select) {
    min-width: 140px;
    height: 34px;
  }
  .f.on :global(.select) {
    border-color: var(--accent);
    background: var(--accent-soft);
  }
  .f :global(.number) {
    height: 34px;
  }
  .f :global(.number input) {
    width: 70px;
  }
  .f input[type="datetime-local"] {
    height: 34px;
    padding: 0 8px;
    border-radius: var(--radius-sm);
    border: 1px solid var(--border-strong);
    background: var(--bg-input);
    color: var(--text);
    font: inherit;
  }
  .f input[type="datetime-local"]:focus-visible {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--focus);
  }
  .to {
    color: var(--text-faint);
    font-size: 12px;
  }
  .clear {
    height: 34px;
    padding: 0 14px;
    border-radius: var(--radius-sm);
    border: 1px solid var(--border-strong);
    background: transparent;
    color: var(--text);
    font: 600 13px/32px var(--font);
    text-decoration: none;
    cursor: pointer;
  }
  .clear:hover {
    background: var(--bg-hover);
  }
  .clear:focus-visible {
    outline: none;
    box-shadow: 0 0 0 3px var(--focus);
  }

  .list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--bg-card);
  }
  ul {
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .day {
    position: sticky;
    top: 0;
    z-index: 2;
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    padding: 10px 18px 8px;
    border-bottom: 1px solid var(--border);
    background: var(--bg-elev);
    font: 650 13px var(--font-display);
  }
  .day .faint {
    font: 400 12px var(--font);
  }
  .row {
    display: grid;
    grid-template-columns: 84px minmax(0, 1fr) 48px 76px 78px 30px;
    align-items: center;
    gap: 14px;
    height: 60px;
    padding: 0 18px;
    cursor: pointer;
    content-visibility: auto;
    contain-intrinsic-size: auto 60px;
    border-bottom: 1px solid rgb(33 43 69 / 0.5);
    outline: none;
  }
  .row:hover,
  .row:focus-visible {
    background: var(--bg-hover);
  }
  .row:focus-visible {
    box-shadow: inset 0 0 0 2px var(--focus);
  }
  .thumb {
    position: relative;
    display: grid;
    place-items: center;
    width: 84px;
    height: 47px;
    border-radius: 6px;
    background: var(--bg-input);
    color: var(--text-faint);
    overflow: hidden;
  }
  .thumb img {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .tall {
    position: absolute;
    left: 4px;
    top: 3px;
    padding: 0 5px;
    border-radius: 4px;
    background: rgb(11 15 26 / 0.8);
    font-size: 11px;
  }
  .cc {
    position: absolute;
    left: 4px;
    bottom: 3px;
    padding: 0 5px;
    border-radius: 4px;
    background: rgb(11 15 26 / 0.8);
    font-size: 11px;
  }
  .dur {
    position: absolute;
    right: 4px;
    bottom: 3px;
    padding: 0 5px;
    border-radius: 4px;
    background: rgb(11 15 26 / 0.8);
    font-size: 11px;
    font-variant-numeric: tabular-nums;
  }
  .main {
    display: grid;
    min-width: 0;
  }
  .name,
  .meta {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .meta {
    font-size: 12px;
    color: var(--text-faint);
  }
  .ext {
    justify-self: start;
    padding: 1px 7px;
    border-radius: 6px;
    background: var(--bg-input);
    color: var(--text-dim);
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
  }
  .when,
  .bytes {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .when {
    white-space: nowrap;
    color: var(--text-faint);
    font-size: 13px;
  }
  .bytes {
    color: var(--text-dim);
    font-weight: 600;
  }
  .top-actions { display: flex; gap: 8px; margin-inline-start: auto; order: 1; }
  .item-menu {
    height: 30px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-elev);
    color: var(--text);
    cursor: pointer;
    font-size: 19px;
    line-height: 1;
  }
  .item-menu:hover { background: var(--bg-hover); }
  dialog {
    width: min(900px, calc(100vw - 40px));
    max-height: calc(100vh - 40px);
    padding: 14px;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    background: var(--bg-elev);
    color: var(--text);
  }
  dialog::backdrop { background: rgb(0 0 0 / 0.68); }
  dialog header { display: flex; align-items: center; gap: 8px; margin-bottom: 12px; }
  dialog strong { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  dialog video, dialog img { display: block; width: 100%; max-height: 72vh; object-fit: contain; }

  .empty {
    display: grid;
    justify-items: center;
    align-content: center;
    gap: 8px;
    flex: 1;
    min-height: 220px;
    padding: 30px;
    border-radius: var(--radius);
    border: 1px dashed var(--border-strong);
    color: var(--text-dim);
    text-align: center;
  }
  .empty :global(svg) {
    color: var(--accent-2);
  }
  .empty h2 {
    font-size: 17px;
    color: var(--text);
  }
  .empty p {
    max-width: 440px;
  }
  .empty a:not(.btn) {
    color: var(--accent-2);
  }
  .empty .btn {
    margin-top: 6px;
  }
</style>
