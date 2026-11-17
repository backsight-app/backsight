<script lang="ts">
  import mark from "../../../assets/brand/backsight-mark.svg";
  import Icon from "../lib/components/Icon.svelte";
  import Kbd from "../lib/components/Kbd.svelte";
  import NvidiaClashes from "../lib/components/NvidiaClashes.svelte";
  import { clashesFor, clashSignature, type OverlayKey } from "../lib/nvidia";
  import { catalog, container, estimateKbps, formatDuration, megabytes, vendorLabel, videoEncoder } from "../lib/content";
  import { ACTIONS } from "../lib/hotkeys";
  import { isTauri } from "@tauri-apps/api/core";
  import { invoke } from "../lib/uiwatch";
  import { onMount } from "svelte";
  import { app, type ReplayStatus } from "../lib/settings.svelte";

  const s = $derived(app.settings);
  let live = $state<ReplayStatus | null>(null);
  // The encoder can't keep up (engine's overload watch); `lighter` says what the button changed.
  let overload = $state<string | null>(null);
  let lighter = $state("");
  let lightening = $state(false);
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
    };
    poll();
    const id = setInterval(poll, 1000);
    return () => {
      clearInterval(id);
      clearTimeout(clearLighter);
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
  // NVIDIA's overlay shares some hotkeys. Its keys are read each time Home opens (off the UI
  // thread in Rust); the clash follows the hotkeys as they change, Setup's swap included.
  let overlay = $state<OverlayKey[]>([]);
  onMount(() => {
    if (isTauri()) invoke<OverlayKey[]>("nvidia_overlay_keys").then((k) => (overlay = k), () => {});
  });
  const clashes = $derived(clashesFor(s.hotkeys as Record<string, string>, overlay));
  // Dismissing is remembered for this set of clashing keys; a different set shows the notice again.
  const DISMISSED = "backsight.nvidiaClashDismissed";
  let dismissed = $state(readDismissed());
  function readDismissed() {
    try {
      return localStorage.getItem(DISMISSED) ?? "";
    } catch {
      return "";
    }
  }
  function dismissClashes() {
    dismissed = clashSignature(clashes);
    try {
      localStorage.setItem(DISMISSED, dismissed);
    } catch {}
  }
  const dismissCrash = () => {
    crash = null;
    invoke("crash_dismiss").catch(() => {});
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

  const pill = $derived(live ? { on: "On", off: "Off", failed: "Problem" }[live.state] : s.replay.enabled ? "On" : "Off");
  const mb = (bytes: number) => (bytes / 1048576).toFixed(0);

  const rate = $derived(estimateKbps(s));
  const replayMb = $derived(rate.video === null ? null : megabytes(rate.video + rate.audio, s.replay.length_seconds));
  const shot = $derived(catalog.screenshotFormats.find((f: any) => f.id === s.screenshot.format));

  const quick = ["save-replay", "toggle-recording", "screenshot"].map((action) => ({
    action,
    label: ACTIONS.find((a) => a.id === action)?.label ?? action,
  }));
  const enc = $derived(videoEncoder(s.video.encoder));
  const madeBy = $derived(
    !enc ? "" : !enc.hardware ? "Made by your processor" : enc.vendor === "nvidia" ? "Made by your NVIDIA card" : `Made by your ${vendorLabel[enc.vendor]} graphics`,
  );
  const quality = $derived(
    [
      catalog.resolutions.find((r: any) => r.value === s.video.resolution)?.label ?? s.video.resolution,
      catalog.frameRates.find((f: any) => f.value === s.video.fps)?.label ?? `${s.video.fps} FPS`,
    ].join(" · "),
  );
</script>

<div class="stack">
  {#if crash}
    <div class="notice" role="status">
      <Icon name="info" size={18} />
      <span>
        Backsight closed unexpectedly last time. To help fix it, save a problem report and send it to whoever gave you
        Backsight.
      </span>
      <button type="button" class="btn" onclick={saveReport} disabled={reporting}>Save a problem report</button>
      <button type="button" class="btn" onclick={dismissCrash}>Dismiss</button>
    </div>
  {/if}
  {#if clashes.length && clashSignature(clashes) !== dismissed}
    <div class="notice" role="status">
      <Icon name="info" size={18} />
      <div class="clashes"><NvidiaClashes {clashes} /></div>
      <button type="button" class="btn" onclick={dismissClashes}>Dismiss</button>
    </div>
  {/if}
  {#if builds.next}
    <div class="notice info update" role="status">
      <Icon name="bolt" size={18} />
      <span>
        A newer build is ready: {builds.next}.
        {live?.recording ? "Stop recording first: updating closes Backsight." : "Backsight closes, updates and opens again. Unsaved replay is cleared."}
        {#if updateError || builds.error}<br />{updateError || builds.error}{/if}
      </span>
      <button type="button" class="btn" onclick={update} disabled={!!live?.recording}>Update</button>
    </div>
  {/if}
  <section class="hero">
    <img src={mark} alt="" width="88" height="88" />
    <div class="hero-text">
      {#if live?.recording}
        <div class="rec" class:problem={live.recording_message} role="status">
          <span class="dot"></span>
          {#if live.recording_message}
            Recording stopped: {live.recording_message} Press <Kbd combo={s.hotkeys["toggle-recording"]} /> to finish.
          {:else}
            {live.recording_paused ? "Paused" : "Recording"} · {formatDuration(Math.floor(live.recording_seconds))}
          {/if}
        </div>
      {/if}
      <h2>Ready when you are.</h2>
      <p>
        {#if s.replay.enabled}
          Backsight always keeps the last {formatDuration(s.replay.length_seconds)}. Press
          <Kbd combo={s.hotkeys["save-replay"]} /> to save it as a clip.
        {/if}
        Your clips are in <a href="#library">Library</a>. You can close this window: Backsight keeps running near the
        clock (click ^ if you don't see its icon).
      </p>
      <div class="quick">
        {#each quick as q}
          <div class="quick-item"><span>{q.label}</span><Kbd combo={s.hotkeys[q.action]} /></div>
        {/each}
      </div>
    </div>
  </section>

  <div class="tiles">
    <a class="tile" href="#replay">
      <div class="tile-head"><Icon name="replay" /><span>Instant Replay</span>
        <span class="pill" class:on={live ? live.state === "on" : s.replay.enabled}>{pill}</span></div>
      <div class="tile-value">{formatDuration(s.replay.length_seconds)}</div>
      <div class="tile-sub">
        {#if live?.state === "failed"}
          {live.message}
        {:else if live?.state === "on"}
          <!-- Dropped frames are the surplus of a faster screen (144 Hz → 60 FPS) and normal; only late ones hurt. -->
          Ready to save the last {formatDuration(Math.round(live.buffer_seconds))} · {mb(live.buffer_bytes)} MB of memory{live.late_frames ? ` · ${live.late_frames} late frames` : ""}
        {:else}
          {replayMb === null ? "RAM use depends on the scene" : `≈ ${replayMb} MB of RAM while on`}
        {/if}
      </div>
    </a>
    <a class="tile" href="#video">
      <div class="tile-head"><Icon name="video" /><span>Video</span></div>
      <div class="tile-value">{quality}</div>
      <div class="tile-sub">{[madeBy, container(s.video.container)?.label].filter(Boolean).join(" · ")}</div>
    </a>
    <a class="tile" href="#screenshots">
      <div class="tile-head"><Icon name="image" /><span>Screenshots</span></div>
      <div class="tile-value">{shot?.label}</div>
      <div class="tile-sub">{s.screenshot.copy_to_clipboard ? "Also copied to clipboard" : "Saved to disk"}</div>
    </a>
  </div>

  {#if overload || lighter}
    <div class="notice update" role="status">
      <Icon name="info" size={18} />
      <span>
        {#if overload}<strong>Clips may stutter.</strong> {overload}{/if}
        {#if lighter}<br />{lighter}{/if}
      </span>
      {#if overload}<button type="button" class="btn" onclick={lighten} disabled={lightening}>Use a lighter setting</button>{/if}
    </div>
  {/if}
  {#if live?.hotkey_problem}
    <div class="notice" role="status">
      <Icon name="info" size={18} />
      <span><strong>Hotkeys aren't working.</strong> {live.hotkey_problem}</span>
    </div>
  {/if}
  {#if live?.audio_problem}
    <div class="notice" role="status">
      <Icon name="info" size={18} />
      <span><strong>Audio problem.</strong> {live.audio_problem}</span>
    </div>
  {/if}
  {#if live?.capture_notice}
    <div class="notice info" role="status">
      <Icon name="info" size={18} />
      <span>{live.capture_notice}</span>
    </div>
  {/if}
  {#if live?.mic_muted}
    <div class="notice info" role="status">
      <Icon name="info" size={18} />
      <span>Your microphone is muted. Press <Kbd combo={s.hotkeys["toggle-mic"]} /> to unmute.</span>
    </div>
  {/if}
  {#if live?.game_audio_from && !live?.audio_problem}
    <div class="notice info" role="status">
      <Icon name="info" size={18} />
      <span>Game sound comes from {live.game_audio_from}. To pick another game, focus it and turn Instant Replay off and on.</span>
    </div>
  {/if}
  {#if live?.pending_apply}
    <div class="notice info" role="status">
      <Icon name="info" size={18} />
      <span>Saved capture settings take effect when you stop recording.</span>
    </div>
  {/if}
  {#if !isTauri()}
    <div class="notice info">
      <Icon name="info" size={18} />
      <span><strong>Preview.</strong> This page is running outside the app, so nothing is captured or saved.</span>
    </div>
  {/if}
  {#if builds.version}
    <p class="build">Backsight {builds.version}, {builds.current ? `build ${builds.current}` : "development build"}</p>
  {/if}
  {#if isTauri()}
    <p class="report">
      Something not working?
      <button type="button" class="btn" onclick={saveReport} disabled={reporting}>Save a problem report</button>
      {#if reportSaved}Saved on your Desktop as "{reportSaved}". Send it to whoever gave you Backsight.{/if}
      {#if reportError}{reportError}{/if}
    </p>
  {/if}
</div>

<style>
  .update {
    align-items: center;
  }
  .update span {
    flex: 1;
  }
  .report {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px;
    margin: 0;
  }
  .build {
    margin: 0;
    font-size: 12px;
    color: var(--text-faint);
  }
  .hero {
    position: relative;
    display: flex;
    gap: 26px;
    align-items: center;
    padding: 30px 32px;
    border-radius: 18px;
    border: 1px solid var(--border-strong);
    background:
      radial-gradient(600px 220px at 0% 0%, rgb(139 92 246 / 0.28), transparent 70%),
      radial-gradient(520px 240px at 100% 100%, rgb(34 211 238 / 0.18), transparent 70%),
      var(--bg-card);
    overflow: hidden;
  }
  .rec {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 8px;
    padding: 4px 10px;
    border-radius: 999px;
    background: rgb(239 68 68 / 0.15);
    color: #fca5a5;
    font-size: 13px;
    font-weight: 600;
  }
  .rec.problem {
    background: rgb(245 158 11 / 0.15);
    color: #fcd34d;
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
    .rec .dot {
      animation: none;
    }
  }
  .hero img {
    flex: none;
    filter: drop-shadow(0 10px 24px rgb(99 102 241 / 0.45));
  }
  .hero-text {
    display: grid;
    gap: 10px;
  }
  .hero h2 {
    font-size: 24px;
    font-weight: 700;
  }
  .hero p {
    max-width: 620px;
    color: var(--text-dim);
  }
  .hero p :global(svg) {
    vertical-align: -2px;
    color: var(--accent-2);
  }
  .quick {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    margin-top: 6px;
  }
  .quick-item {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 7px 10px 7px 12px;
    border-radius: 10px;
    border: 1px solid var(--border);
    background: rgb(11 15 26 / 0.55);
    font-size: 13px;
    color: var(--text-dim);
  }
  .tiles {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 14px;
  }
  .tile {
    display: grid;
    gap: 6px;
    padding: 18px 20px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--bg-card);
    color: inherit;
    text-decoration: none;
    transition: border-color 0.15s, transform 0.15s;
  }
  .tile:hover {
    border-color: var(--border-strong);
    transform: translateY(-1px);
  }
  .tile-head {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--text-dim);
    font-weight: 600;
    font-size: 13px;
  }
  .tile-head :global(svg) {
    color: var(--accent-2);
  }
  .pill {
    margin-left: auto;
    padding: 1px 9px;
    border-radius: 99px;
    font-size: 11px;
    background: var(--bg-hover);
    color: var(--text-faint);
  }
  .pill.on {
    background: rgb(52 211 153 / 0.14);
    color: var(--ok);
  }
  .tile-value {
    font: 650 19px/1.3 var(--font-display);
  }
  .tile-sub {
    font-size: 12.5px;
    color: var(--text-faint);
  }
  .clashes {
    display: grid;
    flex: 1;
    gap: 8px;
  }
</style>
