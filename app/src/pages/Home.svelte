<script lang="ts">
  import Icon from "../lib/components/Icon.svelte";
  import Kbd from "../lib/components/Kbd.svelte";
  import OverlayClashes from "../lib/components/OverlayClashes.svelte";
  import { thumbSrc } from "../lib/components/LibraryCard.svelte";
  import { clashesFor, clashSignature, takenClashes, type Scan } from "../lib/overlays";
  import { catalog, formatDuration } from "../lib/content";
  import { feed } from "../lib/feed.svelte";
  import { formatClock, formatTime, isVertical, todayStats, type LibraryItem } from "../lib/library";
  import { isTauri } from "@tauri-apps/api/core";
  import { installUpdate, loadScan } from "../lib/setup";
  import { invoke } from "../lib/uiwatch";
  import { onMount, tick, untrack } from "svelte";
  import { app, type ReplayStatus } from "../lib/settings.svelte";

  const s = $derived(app.settings);
  let live = $state<ReplayStatus | null>(null);
  // The encoder can't keep up (engine's overload watch); `lighter` says what the button changed.
  let overload = $state<string | null>(null);
  let lighter = $state("");
  let lightening = $state(false);
  // A setting Backsight lightened by itself, shown until dismissed (its pop-up may have been missed).
  let autoChange = $state<{ at: number; text: string } | null>(null);
  const changedOn = (at: number) => {
    const d = new Date(at);
    const year = d.getFullYear() === new Date().getFullYear() ? undefined : "numeric";
    return d.toLocaleDateString(undefined, { month: "short", day: "numeric", year });
  };
  let dismissedAt = 0; // a poll answered before the file is gone doesn't bring it back
  function dismissAutoChange() {
    dismissedAt = autoChange?.at ?? 0;
    autoChange = null;
    invoke("auto_change_dismiss", { at: dismissedAt }).catch(() => {});
  }
  let clearLighter: ReturnType<typeof setTimeout> | undefined;
  // One step per click. The engine then clears the warning and stays quiet until Backsight restarts
  // capture with the change, so a second click can't step down again before the first one counts.
  async function lighten() {
    lightening = true;
    try {
      const r = await invoke<{ video: any; change: string } | null>("lighter_video", { video: $state.snapshot(s.video) });
      if (r) {
        s.video = r.video;
        overload = null;
        lighter = live?.recording ? `${r.change} It takes effect when you stop recording.` : r.change;
      } else {
        lighter = "There's no lighter setting left. Close other busy programs while you play.";
      }
    } catch (e) {
      lighter = `Backsight couldn't change the setting. Try again, or pick one in Video. (${e})`;
    } finally {
      lightening = false;
    }
    clearTimeout(clearLighter);
    clearLighter = setTimeout(() => (lighter = ""), 15000);
  }
  onMount(() => {
    if (!isTauri()) return;
    const poll = () => {
      invoke<ReplayStatus>("get_status").then((v) => (live = v), () => {});
      invoke<string | null>("overload_status").then((v) => (overload = v), () => {});
      invoke<{ at: number; text: string } | null>("auto_change").then((v) => (autoChange = v && v.at > dismissedAt ? v : null), () => {});
    };
    poll();
    const id = setInterval(poll, 1000);
    return () => {
      clearInterval(id);
      clearTimeout(clearLighter);
      clearTimeout(hideSaved);
    };
  });
  // A newer local test build waiting to be installed (scripts/build-desktop.ps1).
  let builds = $state<{ version: string; current: string | null; next: string | null; error: string | null }>({
    version: "",
    current: null,
    next: null,
    error: null,
  });
  let updateError = $state("");
  onMount(() => {
    if (isTauri()) invoke<typeof builds>("local_builds").then((b) => (builds = b), () => {});
  });
  // A crash report saved by the last run (never sent anywhere).
  let crash = $state<string | null>(null);
  onMount(() => {
    if (isTauri()) invoke<string | null>("crash_notice").then((c) => (crash = c), () => {});
  });
  // Other apps share some hotkeys. Their keys are read each time Home opens (off the UI thread in
  // Rust), once the saved hotkeys are in (the probe checks those); the clash follows the hotkeys as
  // they change, Setup's swap included. After "Give Backsight other keys" the card stays up to say how it went.
  let scan = $state<Scan>({ apps: [], taken: [] });
  let swapped = $state(false);
  // A failed check keeps the apps found before.
  function onswap(r: Scan) {
    if (!r.failed) scan = r;
    swapped = true;
  }
  const rescan = () => loadScan($state.snapshot(s)).then((r) => (scan = r));
  let scanned = false;
  $effect(() => {
    if (!scanned && (app.loaded || !isTauri())) {
      scanned = true;
      untrack(rescan);
    }
  });
  const clashes = $derived([
    ...clashesFor(s.hotkeys as Record<string, string>, scan.apps),
    ...takenClashes(s.hotkeys as Record<string, string>, scan.taken),
  ]);
  // Dismissing is remembered for this set of clashing keys; a different set shows the notice again.
  const DISMISSED = "backsight.overlayClashDismissed";
  let dismissed = $state(readDismissed());
  function readDismissed() {
    try {
      return localStorage.getItem(DISMISSED) ?? "";
    } catch {
      return "";
    }
  }
  function dismissClashes() {
    swapped = false;
    dismissed = clashSignature(clashes);
    try {
      localStorage.setItem(DISMISSED, dismissed);
    } catch {}
  }
  const dismissCrash = () => {
    if (crash) invoke("crash_dismiss", { name: crash }).catch(() => {});
    crash = null;
  };
  // "Report it": first the scrubbed report the user would share, then "Open GitHub" copies it and
  // opens the bug report form to paste it into. Nothing is sent by Backsight itself.
  let shared = $state<string | null>(null);
  let crashError = $state("");
  let sent = $state(false);
  let sentOk = $state<HTMLButtonElement>();
  const showReport = () => {
    crashError = "";
    invoke<string | null>("crash_report_text", { name: crash }).then(
      (t) => (t === null ? (crash = null) : (shared = t)),
      (e) => (crashError = String(e)),
    );
  };
  const sendReport = () => {
    crashError = "";
    invoke("crash_report_send", { name: crash }).then(
      () => {
        crash = shared = null;
        sent = true;
        // The card's button is gone: keyboard focus goes to the next card's OK.
        tick().then(() => sentOk?.focus());
      },
      (e) => (crashError = String(e)),
    );
  };
  // A problem report on the Desktop (one text file, personal details removed).
  let reporting = $state(false);
  let reportSaved = $state("");
  let reportError = $state("");
  const saveReport = () => {
    reporting = true;
    reportSaved = reportError = "";
    invoke<string>("save_problem_report")
      .then((p) => (reportSaved = p.split("\\").pop() ?? p), (e) => (reportError = String(e)))
      .finally(() => (reporting = false));
  };
  const update = () => invoke("install_local_build").catch((e) => (updateError = String(e)));

  // Opening the window looks for a newer release too (ADR 0014); quiet when offline.
  onMount(() => {
    if (isTauri()) invoke("check_update").catch(() => {});
  });
  let installing = $state(false);
  let percent = $state<number | null>(null);
  let installError = $state("");
  async function restartToUpdate() {
    installing = true;
    installError = "";
    await installUpdate((p) => (percent = p)).catch((e) => {
      installError = String(e);
      installing = false;
    });
  }

  // Buttons do what their keys do; a failure shows in place of the tip under Save clip.
  let pressError = $state("");
  const press = (command: string, args: Record<string, string>) => {
    pressError = "";
    invoke(command, args).catch((e) => (pressError = String(e)));
  };
  const saveClip = () => press("replay_menu_action", { action: "save" });
  const record = () => press("press_action", { action: "toggle-recording" });
  const screenshot = () => press("press_action", { action: "screenshot" });

  const replayState = $derived(live ? live.state : s.replay.enabled ? "on" : "off");
  const length = $derived(s.replay.length_seconds);
  // What the replay holds now: it fills up over the first minutes after it starts.
  const held = $derived(live?.state === "on" ? Math.min(length, Math.floor(live.buffer_seconds)) : 0);
  const quality = $derived(
    [
      catalog.resolutions.find((r: any) => r.value === s.video.resolution)?.label ?? s.video.resolution,
      catalog.frameRates.find((f: any) => f.value === s.video.fps)?.label ?? `${s.video.fps} FPS`,
      s.video.hdr ? "HDR" : "",
      s.audio.desktop ? "game sound" : "",
    ].filter(Boolean).join(" · "),
  );
  const savedTo = $derived.by(() => {
    const parts = feed.lib?.video_dir?.split(/[\\/]/).filter(Boolean).slice(-2) ?? [];
    if (s.general.folder_per_game && live?.game) parts.push(live.game);
    return parts.join(" › ");
  });
  const ticks = [1, 0.75, 0.5, 0.25];

  const today = $derived(feed.lib ? todayStats(feed.lib.items) : null);
  const latest = $derived(feed.lib?.items.slice(0, 5) ?? []);
  const kindLabel = (i: LibraryItem) => (i.kind === "screenshot" ? "Screenshot" : isVertical(i) ? "Vertical" : "Video");
  const whenLabel = (ms: number) => {
    const day = new Date(ms).toDateString();
    if (day === new Date().toDateString()) return formatTime(ms);
    if (day === new Date(Date.now() - 86400000).toDateString()) return "yesterday";
    return new Date(ms).toLocaleDateString(undefined, { day: "numeric", month: "short" });
  };

  // "Clip saved": a quiet card for a few seconds when a new video lands while Home is open.
  let lastSeen: string | null | undefined;
  let saved = $state<LibraryItem | null>(null);
  let hideSaved: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    const newest = feed.lib?.items[0];
    if (!feed.lib) return;
    // A vertical copy follows its clip: one card per save.
    if (lastSeen !== undefined && newest && newest.path !== lastSeen && newest.kind === "video" && !isVertical(newest)) {
      saved = newest;
      clearTimeout(hideSaved);
      hideSaved = setTimeout(() => (saved = null), 8000);
    }
    lastSeen = newest?.path ?? null;
  });
  // The card follows the clip as the Library learns more: its thumbnail arrives, or the clip
  // becomes its vertical copy (the wide file is then gone, and its thumbnail with it).
  const savedCard = $derived.by(() => {
    if (!saved) return null;
    const items = feed.lib?.items ?? [];
    const vertical = saved.path.replace(/\.[^.\\/]+$/, "") + " (vertical).";
    return items.find((i) => i.path === saved!.path) ?? items.find((i) => i.path.startsWith(vertical)) ?? { ...saved, thumbnail: null };
  });

  const micOn = $derived(s.audio.microphone);
  const shape = $derived(s.replay.clip_shape);
</script>

<div class="home">
  <div class="topbar">
    <span class="game-tile" aria-hidden="true">{(live?.game ?? "").slice(0, 1)}</span>
    <div class="watching">
      <span class="faint">{live?.game ? "Watching" : "Watching for a game"}</span>
      <strong>{live?.game ?? "Your desktop"}</strong>
    </div>
    <div class="stats">
      {#if today}
        <span>Today <strong>{today.clips} {today.clips === 1 ? "video" : "videos"}</strong></span>
        <span class="sep"></span>
        <span>Saved <strong>{today.minutes} min</strong></span>
        <span class="sep"></span>
      {/if}
      <a class="replay-state {replayState}" href="#replay">
        <span class="dot"></span>{{ on: "Replay on", off: "Replay off", failed: "Replay problem" }[replayState]}
      </a>
    </div>
  </div>

  {#if live?.update}
    <div class="notice ready" role="status">
      <Icon name="bolt" size={16} />
      <span class="grow">
        <strong>Update ready:</strong> Backsight {live.update}.
        {#if installing}Downloading{percent !== null ? ` ${percent}%` : ""}… Backsight closes and opens again by itself.
        {:else if live.recording}Stop recording first to update.
        {:else}Takes a few seconds; the replay starts over.{/if}
        {#if installError}<br />{installError}{/if}
      </span>
      <button type="button" class="btn primary" onclick={restartToUpdate} disabled={installing || live.recording}>Restart to update</button>
    </div>
  {/if}

  <section class="hero">
    {#if live?.recording}
      <div class="rec" class:problem={live.recording_message} role="status">
        <span class="dot"></span>
        {#if live.recording_message}
          Recording stopped: {live.recording_message}
          {#if s.hotkeys["toggle-recording"]}Press <Kbd combo={s.hotkeys["toggle-recording"]} /> to finish.{:else}Recording has no key: give it one in <a href="#hotkeys">Hotkeys</a> to finish.{/if}
        {:else}
          {live.recording_paused ? "Paused" : "Recording"} · {formatDuration(Math.floor(live.recording_seconds))}
        {/if}
      </div>
    {/if}
    <div class="hero-row">
      <div class="hero-text">
        <span class="eyebrow">Instant Replay</span>
        {#if replayState === "on" && held === 0}
          <h1>Instant Replay is starting.</h1>
        {:else if replayState === "on"}
          <h1>The last <span class="accent">{formatClock(held * 1000)}</span> is ready to save.</h1>
        {:else if replayState === "failed"}
          <h1>Instant Replay has a problem.</h1>
        {:else}
          <h1>Instant Replay is off.</h1>
        {/if}
        <p class="dim">
          {#if replayState === "failed"}
            {live?.message}
          {:else if replayState === "off"}
            Turn it on in <a href="#replay">Instant Replay</a> to always keep the last {formatClock(length * 1000)}.
          {:else}
            Works in any game, even full screen · {quality}
          {/if}
        </p>
      </div>
      <div class="save-col">
        <button type="button" class="save" onclick={saveClip} disabled={replayState !== "on" || held === 0}>
          Save clip
          {#if s.hotkeys["save-replay"]}<Kbd combo={s.hotkeys["save-replay"]} />{/if}
        </button>
        {#if pressError}
          <span class="tip error">{pressError}</span>
        {:else if !s.hotkeys["save-replay"]}
          <span class="tip">No key yet: give it one in <a href="#hotkeys">Hotkeys</a>.</span>
        {:else if savedTo}
          <span class="tip">Saved to {savedTo}</span>
        {/if}
      </div>
    </div>
    <div class="timeline" aria-hidden="true">
      <div class="track">
        <div class="fill" style:width={`${(held / length) * 100}%`}></div>
        {#each [0.25, 0.5, 0.75] as t}<span class="tick" style:left={`${t * 100}%`}></span>{/each}
        {#if replayState === "on"}<span class="head"></span>{/if}
      </div>
      <div class="marks">
        {#each ticks as t}<span>−{formatClock(length * t * 1000)}</span>{/each}
        <span class="now">now</span>
      </div>
    </div>
  </section>

  <div class="tiles">
    <button type="button" class="tile" onclick={record}>
      <span class="tile-title"><span class="rec-dot" class:live={live?.recording}></span>{live?.recording ? "Stop recording" : "Record"}</span>
      <span class="tile-key"><Kbd combo={s.hotkeys["toggle-recording"]} /><span>start or stop</span></span>
    </button>
    <button type="button" class="tile" onclick={screenshot}>
      <span class="tile-title"><Icon name="camera" size={15} />Screenshot</span>
      <span class="tile-key"><Kbd combo={s.hotkeys["screenshot"]} /><span>{s.screenshot.copy_to_clipboard ? "saved and copied" : "saved"}</span></span>
    </button>
    <a class="tile" href="#replay">
      <span class="faint small">After each clip</span>
      <span class="row"><span>Vertical copy</span><span class:on={shape !== "wide"}>{shape === "vertical" ? "Only" : shape === "both" ? "On" : "Off"}</span></span>
      <span class="row"><span>Captions</span><span class:on={s.audio.captions}>{s.audio.captions ? "On" : "Off"}</span></span>
    </a>
    <a class="tile" href="#audio">
      <span class="faint small">Microphone</span>
      <span class="tile-title">{!micOn ? "Off" : live?.mic_muted ? "Muted" : "On"}</span>
      <span class="tile-key">{#if micOn}<Kbd combo={s.hotkeys["toggle-mic"]} /><span>to mute</span>{:else}<span>Turn on in Audio</span>{/if}</span>
    </a>
  </div>

  {#if crash}
    <!-- Only the message and errors are announced; the report preview is read on demand. -->
    <div class="notice crash">
      <Icon name="alert" size={16} />
      <div class="grow crash-body">
        <div class="crash-body" role="status">
          <span>
            Backsight {crash.startsWith("stuck-") ? "got stuck" : "closed unexpectedly"} last time. You can send a report
            to help fix it. Nothing is sent unless you choose to.
          </span>
          {#if crashError}<span class="small">{crashError}</span>{/if}
        </div>
        {#if shared !== null}
          <span class="faint small">This is what will be shared on GitHub. Your name and file names were taken out.</span>
          <pre class="shared">{shared}</pre>
          <span class="faint small">Open GitHub copies it and opens a new issue in your browser; you can still change it before posting.</span>
        {/if}
      </div>
      <!-- One button whose step changes, so keyboard focus stays on it. -->
      <button type="button" class="btn" onclick={shared === null ? showReport : sendReport}>{shared === null ? "Report it" : "Open GitHub"}</button>
      <button type="button" class="btn" onclick={dismissCrash}>Dismiss</button>
    </div>
  {:else if sent}
    <div class="notice" role="status">
      <Icon name="alert" size={16} />
      <span class="grow">The report is copied. On the GitHub page, paste it into the Log box (Ctrl+V), say what you were doing, and press Create.</span>
      <button type="button" class="btn" bind:this={sentOk} onclick={() => (sent = false)}>OK</button>
    </div>
  {/if}
  {#if swapped || (clashes.length && clashSignature(clashes) !== dismissed)}
    <div class="notice" role="status">
      <Icon name="alert" size={16} />
      <div class="clashes"><OverlayClashes {clashes} {onswap} /></div>
      <button type="button" class="btn" onclick={dismissClashes}>Dismiss</button>
    </div>
  {/if}
  {#if builds.next}
    <div class="notice info" role="status">
      <Icon name="bolt" size={16} />
      <span class="grow">
        A newer build is ready: {builds.next}.
        {live?.recording ? "Stop recording first: updating closes Backsight." : "Backsight closes, updates and opens again. Unsaved replay is cleared."}
        {#if updateError || builds.error}<br />{updateError || builds.error}{/if}
      </span>
      <button type="button" class="btn" onclick={update} disabled={!!live?.recording}>Update</button>
    </div>
  {/if}
  {#if overload || lighter}
    <div class="notice" role="status">
      <Icon name="alert" size={16} />
      <span class="grow">
        {#if overload}<strong>Clips may stutter.</strong> {overload}{/if}
        {#if lighter}<br />{lighter}{/if}
      </span>
      {#if overload}<button type="button" class="btn" onclick={lighten} disabled={lightening}>Use a lighter setting</button>{/if}
    </div>
  {/if}
  {#if autoChange}
    <div class="notice" role="status">
      <Icon name="alert" size={16} />
      <span class="grow">
        <strong>Changed on {changedOn(autoChange.at)}.</strong> {autoChange.text} You can undo this on the <a href="#video">Video</a> page.
      </span>
      <button type="button" class="btn" onclick={dismissAutoChange}>Got it</button>
    </div>
  {/if}
  {#if live?.hotkey_problem}
    <div class="notice" role="status">
      <Icon name="alert" size={16} />
      <span><strong>Hotkeys aren't working.</strong> {live.hotkey_problem}</span>
    </div>
  {/if}
  {#if live?.audio_problem}
    <div class="notice" role="status">
      <Icon name="alert" size={16} />
      <span><strong>Audio problem.</strong> {live.audio_problem}</span>
    </div>
  {/if}
  {#if live?.capture_notice}
    <div class="notice info" role="status">
      <Icon name="info" size={16} />
      <span>{live.capture_notice}</span>
    </div>
  {/if}
  {#if live?.mic_muted}
    <div class="notice info" role="status">
      <Icon name="info" size={16} />
      <span
        >Your microphone is muted. {#if s.hotkeys["toggle-mic"]}Press <Kbd combo={s.hotkeys["toggle-mic"]} /> to unmute.{:else}Muting
          has no key: give it one in <a href="#hotkeys">Hotkeys</a> to unmute.{/if}</span
      >
    </div>
  {/if}
  {#if live?.game_audio_from && !live?.audio_problem}
    <div class="notice info" role="status">
      <Icon name="info" size={16} />
      <span>Game sound comes from {live.game_audio_from}. To pick another game, focus it and turn Instant Replay off and on.</span>
    </div>
  {/if}
  {#if live?.pending_apply}
    <div class="notice info" role="status">
      <Icon name="info" size={16} />
      <span>Saved capture settings take effect when you stop recording.</span>
    </div>
  {/if}
  {#if !isTauri()}
    <div class="notice info">
      <Icon name="info" size={16} />
      <span><strong>Preview.</strong> This page is running outside the app, so nothing is captured or saved.</span>
    </div>
  {/if}

  {#if latest.length}
    <section class="latest">
      <div class="latest-head"><h2>Latest captures</h2><a href="#library">Open Library →</a></div>
      <div class="captures">
        {#each latest as item (item.path)}
          <a class="capture" href="#library" title={item.name}>
            <span class="thumb">
              <Icon name={item.kind === "video" ? "video" : "image"} size={18} />
              {#if item.thumbnail}
                <img src={thumbSrc(item.thumbnail)} alt="" loading="lazy" onerror={(e) => ((e.currentTarget as HTMLImageElement).style.display = "none")} />
              {/if}
              {#if item.captions}<span class="badge left">CC</span>{/if}
              {#if item.duration_ms}<span class="badge right">{formatClock(item.duration_ms)}</span>{/if}
            </span>
            <span class="faint small">{kindLabel(item)} · {whenLabel(item.created_ms)}</span>
          </a>
        {/each}
      </div>
    </section>
  {/if}

  <footer>
    <span>You can close this window: Backsight keeps running near the clock (click ^ if you don't see its icon).</span>
    {#if builds.version}
      <span>Backsight {builds.version}, {builds.current ? `build ${builds.current}` : "development build"}</span>
    {/if}
    {#if isTauri()}
      <span class="report">
        Something not working?
        <button type="button" class="link" onclick={saveReport} disabled={reporting}>Save a problem report</button>
        {#if reportSaved}Saved on your Desktop as "{reportSaved}". Attach it to a new issue at github.com/backsight-app/backsight/issues.{/if}
        {#if reportError}{reportError}{/if}
      </span>
    {/if}
  </footer>
</div>

{#if savedCard}
  <div class="saved-wrap" role="status"><a class="saved" href="#library">
    <span class="saved-thumb">{#if savedCard.thumbnail}<img src={thumbSrc(savedCard.thumbnail)} alt="" />{:else}<Icon name="video" size={18} />{/if}</span>
    <span class="saved-text">
      <strong>Saved</strong>
      <span class="faint small">
        {savedCard.duration_ms ? formatClock(savedCard.duration_ms) : ""}{shape !== "wide" ? " · vertical copy next" : ""}{s.audio.captions ? " · captions next" : ""}
      </span>
    </span>
  </a></div>
{/if}

<style>
  .home {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 16px;
    padding-top: 24px;
  }
  /* The one soft glow, behind the hero. */
  .home::before {
    content: "";
    position: absolute;
    left: -40px;
    top: -40px;
    width: 820px;
    height: 380px;
    background: radial-gradient(ellipse at 25% 0%, var(--accent-glow), transparent 70%);
    pointer-events: none;
  }
  .home > :global(*) {
    position: relative;
  }
  .faint {
    color: var(--text-faint);
  }
  .dim {
    color: var(--text-dim);
  }
  .small {
    font-size: 11.5px;
  }
  .grow {
    flex: 1;
  }

  .topbar {
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: 40px;
  }
  .game-tile {
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    flex: none;
    border-radius: 8px;
    background: linear-gradient(135deg, #263552, #3a2f4a);
    box-shadow: inset 0 0 0 1px rgb(255 255 255 / 0.08);
    font: 700 14px var(--font-display);
    color: var(--text-dim);
  }
  .watching {
    display: grid;
    flex: 1;
    min-width: 0;
    line-height: 1.25;
  }
  .watching span {
    font-size: 11.5px;
  }
  .watching strong {
    overflow: hidden;
    font-size: 15px;
    font-weight: 600;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .stats {
    display: flex;
    align-items: center;
    height: 36px;
    border: 1px solid var(--border);
    border-radius: 10px;
    font-size: 12px;
    color: var(--text-faint);
  }
  .stats > span:not(.sep) {
    padding: 0 14px;
  }
  .stats strong {
    margin-left: 4px;
    color: var(--text);
    font-weight: 600;
  }
  .sep {
    width: 1px;
    height: 16px;
    background: var(--border);
  }
  .replay-state {
    display: flex;
    align-items: center;
    gap: 7px;
    height: 100%;
    padding: 0 14px;
    border-radius: 0 9px 9px 0;
    color: #c9cbd6;
    text-decoration: none;
  }
  .replay-state:hover {
    background: var(--bg-hover);
  }
  .replay-state .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--text-faint);
  }
  .replay-state.on .dot {
    background: var(--ok);
    animation: pulse 2.4s ease-out infinite;
  }
  .replay-state.failed .dot {
    background: var(--rec);
  }
  @keyframes pulse {
    0% {
      box-shadow: 0 0 0 0 rgb(52 211 153 / 0.45);
    }
    100% {
      box-shadow: 0 0 0 7px rgb(52 211 153 / 0);
    }
  }

  .hero {
    display: flex;
    flex-direction: column;
    gap: 24px;
    padding: 30px 32px 24px;
    border-radius: 18px;
    border: 1px solid #23252f;
    background: linear-gradient(180deg, #15161d, #101117);
    box-shadow: inset 0 1px 0 rgb(255 255 255 / 0.05), 0 24px 60px rgb(0 0 0 / 0.35);
  }
  /* The 1px accent line along the top edge. */
  .hero::before {
    content: "";
    position: absolute;
    left: 32px;
    right: 32px;
    top: -1px;
    height: 1px;
    background: linear-gradient(90deg, transparent, var(--accent), transparent);
  }
  .hero-row {
    display: flex;
    align-items: flex-end;
    gap: 32px;
  }
  .hero-text {
    display: grid;
    flex: 1;
    gap: 10px;
  }
  .eyebrow {
    font-size: 11.5px;
    font-weight: 600;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--accent);
  }
  .hero h1 {
    font-size: 36px;
    line-height: 1.08;
    font-weight: 700;
    letter-spacing: -0.03em;
  }
  .accent {
    color: var(--accent);
  }
  .hero p {
    font-size: 13.5px;
  }
  .hero a,
  .tip a {
    color: var(--accent);
  }
  .save-col {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 8px;
    flex: none;
    max-width: 280px;
  }
  .save {
    display: flex;
    align-items: center;
    gap: 14px;
    height: 52px;
    padding: 0 10px 0 22px;
    border-radius: 12px;
    border: none;
    background: var(--accent-grad);
    color: #fff;
    font: 600 15.5px var(--font);
    box-shadow: inset 0 1px 0 rgb(255 255 255 / 0.25), 0 0 0 1px rgb(255 255 255 / 0.06), 0 10px 32px var(--accent-glow);
    cursor: pointer;
    transition: filter 0.15s;
  }
  .save:hover:not(:disabled) {
    filter: brightness(1.08);
  }
  .save:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .save:focus-visible {
    outline: none;
    box-shadow: 0 0 0 3px var(--focus);
  }
  .save :global(kbd) {
    border-color: rgb(255 255 255 / 0.18);
    background: rgb(0 0 0 / 0.28);
    color: #fff;
  }
  .save :global(.plus) {
    display: none;
  }
  .tip {
    font-size: 11.5px;
    color: var(--text-faint);
    text-align: right;
  }
  .tip.error {
    color: var(--rec);
  }

  .timeline {
    display: grid;
    gap: 8px;
  }
  .track {
    position: relative;
    height: 4px;
    border-radius: 2px;
    background: var(--border);
  }
  .fill {
    position: absolute;
    top: 0;
    right: 0;
    bottom: 0;
    border-radius: 2px;
    background: linear-gradient(90deg, rgb(255 255 255 / 0.04), var(--accent));
    transition: width 1s linear;
  }
  .tick {
    position: absolute;
    top: 6px;
    width: 1px;
    height: 4px;
    background: #33353f;
  }
  .head {
    position: absolute;
    right: -1px;
    top: -5px;
    width: 2px;
    height: 14px;
    border-radius: 1px;
    background: #fff;
    box-shadow: 0 0 8px rgb(255 255 255 / 0.5);
  }
  .marks {
    display: grid;
    grid-template-columns: repeat(5, minmax(0, 1fr));
    font-size: 11px;
    color: var(--text-faint);
  }
  .marks span:nth-child(3) {
    text-align: center;
  }
  .marks span:nth-child(n + 4) {
    text-align: right;
  }
  .marks .now {
    color: #c9cbd6;
  }

  .rec {
    display: inline-flex;
    align-items: center;
    align-self: flex-start;
    gap: 8px;
    margin-bottom: -8px;
    padding: 4px 10px;
    border-radius: 999px;
    background: rgb(240 80 110 / 0.14);
    color: #f7a1b3;
    font-size: 13px;
    font-weight: 600;
  }
  .rec.problem {
    background: rgb(212 178 91 / 0.12);
    color: #e8cf8a;
    font-weight: 500;
  }
  .rec .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: currentColor;
    animation: blink 1.2s ease-in-out infinite;
  }
  @keyframes blink {
    50% {
      opacity: 0.3;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .rec .dot,
    .replay-state.on .dot {
      animation: none;
    }
  }

  .tiles {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: 12px;
  }
  .tile {
    display: flex;
    flex-direction: column;
    align-items: stretch;
    justify-content: space-between;
    gap: 6px;
    min-height: 92px;
    padding: 14px 16px;
    border-radius: 14px;
    border: 1px solid var(--border);
    background: var(--bg-card);
    color: var(--text);
    font: inherit;
    text-align: left;
    text-decoration: none;
    cursor: pointer;
    transition: border-color 0.15s, background 0.15s;
  }
  .tile:hover {
    border-color: var(--border-strong);
    background: #14151c;
  }
  .tile:focus-visible {
    outline: none;
    box-shadow: 0 0 0 3px var(--focus);
  }
  .tile-title {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 14px;
    font-weight: 600;
  }
  .tile-title :global(svg) {
    color: var(--text-dim);
  }
  .tile-key {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 11.5px;
    color: var(--text-faint);
  }
  .tile-key :global(kbd) {
    min-width: 0;
    padding: 1px 7px;
    border-radius: 5px;
    background: transparent;
    color: #b4b6c2;
    font-size: 11.5px;
    font-weight: 500;
  }
  .tile-key :global(.plus) {
    display: none;
  }
  .rec-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--rec);
    box-shadow: 0 0 0 3px rgb(240 80 110 / 0.15);
  }
  .rec-dot.live {
    animation: blink 1.2s ease-in-out infinite;
  }
  .row {
    display: flex;
    justify-content: space-between;
    font-size: 13px;
  }
  .row span:last-child {
    color: var(--text-faint);
  }
  .row span.on {
    color: var(--ok);
  }

  .notice {
    align-items: center;
  }
  .notice.ready {
    border-color: rgb(163 149 255 / 0.35);
    background: var(--accent-soft);
    color: var(--text);
  }
  .notice.ready :global(svg) {
    color: var(--accent);
  }
  .notice.crash {
    align-items: flex-start;
  }
  .crash-body {
    display: grid;
    gap: 8px;
    min-width: 0;
  }
  .shared {
    max-height: 220px;
    overflow: auto;
    margin: 0;
    padding: 8px 10px;
    border-radius: var(--radius-sm);
    background: rgb(0 0 0 / 0.3);
    font-size: 12px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .clashes {
    display: grid;
    flex: 1;
    gap: 8px;
  }

  .latest {
    display: grid;
    gap: 10px;
  }
  .latest-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
  }
  .latest-head h2 {
    font: 600 13.5px var(--font);
    letter-spacing: 0;
  }
  .latest-head a {
    font-size: 12.5px;
    color: var(--text-dim);
    text-decoration: none;
  }
  .latest-head a:hover {
    color: var(--text);
  }
  .captures {
    display: grid;
    grid-template-columns: repeat(5, minmax(0, 1fr));
    gap: 12px;
  }
  .capture {
    display: grid;
    gap: 6px;
    min-width: 0;
    color: inherit;
    text-decoration: none;
  }
  .capture .small {
    overflow: hidden;
    color: var(--text-dim);
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .thumb {
    position: relative;
    display: block;
    aspect-ratio: 16 / 9;
    overflow: hidden;
    border-radius: 10px;
    background: linear-gradient(135deg, #1c1d26, #15161d);
    box-shadow: inset 0 0 0 1px rgb(255 255 255 / 0.05);
  }
  .thumb :global(svg) {
    position: absolute;
    inset: 0;
    margin: auto;
    color: var(--text-faint);
  }
  .thumb img {
    position: relative;
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .capture:hover .thumb {
    box-shadow: inset 0 0 0 1px var(--border-strong);
  }
  .badge {
    position: absolute;
    padding: 1px 5px;
    border-radius: 4px;
    background: rgb(0 0 0 / 0.6);
    font-size: 10.5px;
    font-weight: 600;
    color: #d9dae2;
  }
  .badge.left {
    left: 6px;
    top: 6px;
  }
  .badge.right {
    right: 6px;
    bottom: 6px;
  }

  footer {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    padding-top: 4px;
    font-size: 12px;
    color: var(--text-faint);
  }
  .link {
    padding: 0;
    border: none;
    background: none;
    color: var(--text-dim);
    font: inherit;
    text-decoration: underline;
    cursor: pointer;
  }
  .link:hover {
    color: var(--text);
  }

  .saved {
    position: fixed;
    right: 24px;
    bottom: 24px;
    z-index: 5;
    display: flex;
    align-items: center;
    gap: 12px;
    width: 304px;
    padding: 10px;
    border-radius: 12px;
    border: 1px solid #262833;
    background: var(--bg-pop);
    box-shadow: 0 16px 48px rgb(0 0 0 / 0.55);
    color: inherit;
    text-decoration: none;
    animation: rise 0.25s ease-out;
  }
  .saved-thumb {
    width: 64px;
    height: 40px;
    flex: none;
    display: grid;
    place-items: center;
    overflow: hidden;
    border-radius: 7px;
    background: linear-gradient(135deg, #2a2440, #16303a);
  }
  .saved-thumb img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .saved-text {
    display: grid;
    gap: 2px;
    font-size: 13px;
  }
  @keyframes rise {
    from {
      opacity: 0;
      transform: translateY(8px);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .saved {
      animation: none;
    }
  }
</style>
