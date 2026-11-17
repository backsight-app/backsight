<script lang="ts" module>
  import { convertFileSrc, invoke, isTauri } from "@tauri-apps/api/core";

  // One request per clip: hovering the same clip again plays instantly. Failures are not cached.
  const previews = new Map<string, Promise<string>>();
  export function thumbSrc(path: string) {
    return isTauri() ? convertFileSrc(path) : path;
  }
  function preview(path: string) {
    let p = previews.get(path);
    if (!p) {
      p = (isTauri() || !import.meta.env.DEV ? invoke<string>("library_preview", { path }) : import("../library-mock").then((m) => m.mockPreview())).catch((e) => {
        previews.delete(path);
        throw e;
      });
      previews.set(path, p);
    }
    return p;
  }
</script>

<script lang="ts">
  // The hover card: a small live preview window (videos only) above an at-a-glance card
  // that leads with the file's size. Mount it to start loading, unmount it to cancel.
  import { autoUpdate, computePosition, flip, offset, shift } from "@floating-ui/dom";
  import { onMount } from "svelte";
  import { codecLabel } from "../content";
  import { formatBytes, formatClock, formatWhen, type LibraryItem } from "../library";
  import Icon from "./Icon.svelte";

  let { item, anchor, id }: { item: LibraryItem; anchor: HTMLElement; id: string } = $props();

  let box: HTMLDivElement;
  let placed = $state(false);
  let status = $state<"loading" | "ready" | "failed">("loading");
  let clip = $state("");
  let playing = $state(false);

  const facts = $derived(
    [
      ["Recorded", formatWhen(item.created_ms)],
      ["Type", `${item.ext.toUpperCase()} ${item.kind}`],
      item.width && item.height ? ["Resolution", `${item.width} × ${item.height}`] : null,
      item.duration_ms !== null ? ["Length", formatClock(item.duration_ms)] : null,
      item.video_codec ? ["Codec", codecLabel(item.video_codec)] : null,
      item.fps ? ["Frame rate", `${Math.round(item.fps * 100) / 100} FPS`] : null,
      item.audio_tracks ? ["Audio", `${item.audio_tracks} ${item.audio_tracks === 1 ? "track" : "tracks"}`] : null,
    ].filter(Boolean) as [string, string][],
  );

  onMount(() => {
    let alive = true;
    const stop = autoUpdate(anchor, box, () =>
      computePosition(anchor, box, {
        placement: "top-end",
        strategy: "fixed",
        middleware: [offset(8), flip({ padding: 12 }), shift({ padding: 12, crossAxis: true })],
      }).then(({ x, y }) => {
        Object.assign(box.style, { left: `${x}px`, top: `${y}px` });
        placed = true;
      }),
    );
    if (item.kind === "video") {
      preview(item.path).then(
        (p) => alive && ((clip = thumbSrc(p)), (status = "ready")),
        () => alive && (status = "failed"),
      );
    }
    return () => {
      alive = false;
      stop();
    };
  });
</script>

<div {id} bind:this={box} class="hover" class:placed role="tooltip">
  {#if item.kind === "video"}
    <div class="window" class:loading={status === "loading"}>
      {#if item.thumbnail}<img src={thumbSrc(item.thumbnail)} alt="" />{:else}<Icon name="video" size={32} />{/if}
      {#if status === "ready"}
        <!-- svelte-ignore a11y_media_has_caption -->
        <video src={clip} class:playing autoplay muted loop playsinline onplaying={() => (playing = true)} onerror={() => (status = "failed")}></video>
      {/if}
      {#if status === "loading"}<span class="note">Getting a preview…</span>{/if}
      {#if status === "failed"}<span class="note">Preview unavailable</span>{/if}
    </div>
  {/if}
  <div class="glance">
    <div class="name" title={item.name}>{item.name}</div>
    <div class="size"><b>{formatBytes(item.bytes)}</b> <span>on disk</span></div>
    <dl>
      {#each facts as [k, v]}
        <dt>{k}</dt>
        <dd>{v}</dd>
      {/each}
    </dl>
  </div>
</div>

<style>
  .hover {
    position: fixed;
    left: 0;
    top: 0;
    z-index: 80;
    display: grid;
    gap: 8px;
    width: 300px;
    pointer-events: none;
    opacity: 0;
  }
  .hover.placed {
    opacity: 1;
    transition: opacity 0.12s ease-out;
  }
  .window,
  .glance {
    border-radius: var(--radius);
    border: 1px solid var(--border-strong);
    background: var(--bg-pop);
    box-shadow: var(--shadow);
    overflow: hidden;
  }
  .window {
    position: relative;
    display: grid;
    place-items: center;
    aspect-ratio: 16 / 9;
    background: #05070d;
    color: var(--text-faint);
  }
  .window img,
  .window video {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: contain;
  }
  .window video {
    opacity: 0;
    transition: opacity 0.2s;
  }
  .window video.playing {
    opacity: 1;
  }
  .window.loading::after {
    content: "";
    position: absolute;
    inset: 0;
    background: linear-gradient(100deg, transparent 30%, rgb(255 255 255 / 0.1) 50%, transparent 70%);
    background-size: 220% 100%;
    animation: sweep 1.3s linear infinite;
  }
  @keyframes sweep {
    from {
      background-position: 120% 0;
    }
    to {
      background-position: -120% 0;
    }
  }
  .note {
    position: absolute;
    left: 10px;
    bottom: 8px;
    z-index: 1;
    padding: 2px 8px;
    border-radius: 99px;
    background: rgb(11 15 26 / 0.7);
    color: var(--text-dim);
    font-size: 11px;
  }
  .glance {
    padding: 14px 16px 12px;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 12px;
    color: var(--text-faint);
  }
  .size {
    margin: 2px 0 10px;
  }
  .size b {
    font: 700 32px/1.15 var(--font-display);
    letter-spacing: -0.02em;
    font-variant-numeric: tabular-nums;
    background: linear-gradient(135deg, #fff 30%, #c4b5fd 70%, #67e8f9);
    background-clip: text;
    color: transparent;
  }
  .size span {
    font-size: 12px;
    color: var(--text-faint);
  }
  dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 3px 14px;
    margin: 0;
    font-size: 12.5px;
  }
  dt {
    color: var(--text-faint);
  }
  dd {
    margin: 0;
    color: var(--text-dim);
    text-align: right;
  }
  @media (prefers-reduced-motion: reduce) {
    .window.loading::after {
      animation: none;
    }
  }
</style>
