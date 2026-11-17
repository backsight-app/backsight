<script lang="ts">
  // The region picker: a frozen picture of the monitor, stretched over this borderless window that
  // covers it. Rust crops the frozen frame from the corners sent here and the view size, so what the
  // user drags over is exactly what is saved, at any DPI.
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";

  type Point = [number, number];
  let src = $state("");
  let start = $state<Point | null>(null);
  let end = $state<Point>([0, 0]);
  let done = false;
  // The window starts hidden and can lose focus before it's shown; only a blur after that cancels.
  let shown = false;

  const box = $derived(
    start && {
      left: Math.min(start[0], end[0]),
      top: Math.min(start[1], end[1]),
      width: Math.abs(end[0] - start[0]),
      height: Math.abs(end[1] - start[1]),
    },
  );

  // Rust closes this window either way; a drag without area is a cancel.
  function finish(confirm: boolean) {
    if (done) return;
    done = true;
    const pick = confirm && start ? { start, end, view: [innerWidth, innerHeight] } : null;
    invoke("region_pick", { pick });
  }

  onMount(() => {
    invoke<ArrayBuffer>("region_image").then(
      (bytes) => (src = URL.createObjectURL(new Blob([bytes], { type: "image/bmp" }))),
      () => finish(false),
    );
  });

  const point = (e: PointerEvent): Point => [e.clientX, e.clientY];
</script>

<svelte:window
  onkeydown={(e) => {
    if (e.key === "Escape") finish(false);
    else if (e.key === "Enter") finish(true);
  }}
  onblur={() => shown && finish(false)}
/>

{#if src}
  <div
    class="picker"
    role="application"
    aria-label="Drag a box around what you want. Escape or right-click cancels."
    onpointerdown={(e) => {
      if (e.button !== 0) return;
      e.currentTarget.setPointerCapture(e.pointerId);
      start = point(e);
      end = start;
    }}
    onpointermove={(e) => {
      if (start) end = point(e);
    }}
    onpointerup={(e) => {
      if (e.button !== 0 || !start) return;
      end = point(e);
      finish(true);
    }}
    oncontextmenu={(e) => {
      e.preventDefault();
      finish(false);
    }}
  >
    <img {src} alt="" draggable="false" onload={() => invoke("region_show").then(() => (shown = true))} onerror={() => finish(false)} />
    {#if box}
      <div class="box" style:left="{box.left}px" style:top="{box.top}px" style:width="{box.width}px" style:height="{box.height}px"></div>
    {:else}
      <div class="dim"></div>
      <p class="hint">Drag a box around what you want. Esc cancels.</p>
    {/if}
  </div>
{/if}

<style>
  .picker {
    position: fixed;
    inset: 0;
    overflow: hidden;
    cursor: crosshair;
    user-select: none;
  }
  img {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    display: block;
  }
  .dim {
    position: absolute;
    inset: 0;
    background: rgb(0 0 0 / 0.45);
  }
  .box {
    position: absolute;
    box-shadow: 0 0 0 100vmax rgb(0 0 0 / 0.45);
    outline: 1px solid var(--accent-2);
  }
  .hint {
    position: absolute;
    top: 24px;
    left: 50%;
    transform: translateX(-50%);
    margin: 0;
    padding: 8px 14px;
    border-radius: var(--radius-sm);
    background: var(--bg-pop);
    color: var(--text);
    box-shadow: var(--shadow);
    pointer-events: none;
  }
</style>
