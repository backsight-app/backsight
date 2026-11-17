<script lang="ts">
  // Line icons on a 24px grid, drawn for Backsight.
  const paths: Record<string, string> = {
    home: "M3.5 10.5 12 3.5l8.5 7V20a1 1 0 0 1-1 1H15v-6H9v6H4.5a1 1 0 0 1-1-1z",
    video: "M4 6.5h10.5a1 1 0 0 1 1 1v9a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1v-9a1 1 0 0 1 1-1zM15.5 10.5 21 7.5v9l-5.5-3",
    replay: "M3.5 12a8.5 8.5 0 1 0 2.6-6.1M3.5 4v4.5H8M12 10a2 2 0 1 0 0 4 2 2 0 0 0 0-4z",
    mic: "M12 3.5a3 3 0 0 1 3 3v5a3 3 0 0 1-6 0v-5a3 3 0 0 1 3-3zM5.5 11a6.5 6.5 0 0 0 13 0M12 17.5V21",
    image: "M4 4.5h16a1 1 0 0 1 1 1v13a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1v-13a1 1 0 0 1 1-1zM3 16l5-5 4 4 3-3 6 6M15.5 8.5h.01",
    keyboard: "M3.5 6.5h17a1 1 0 0 1 1 1v9a1 1 0 0 1-1 1h-17a1 1 0 0 1-1-1v-9a1 1 0 0 1 1-1zM6.5 10h.01M10 10h.01M13.5 10h.01M17 10h.01M7.5 14h9",
    sliders: "M4 7h9M17 7h3M15 5v4M4 17h3M11 17h9M9 15v4",
    library: "M4.5 4.5h6a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1h-6a1 1 0 0 1-1-1v-5a1 1 0 0 1 1-1zM13.5 4.5h6a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1h-6a1 1 0 0 1-1-1v-5a1 1 0 0 1 1-1zM4.5 13.5h6a1 1 0 0 1 1 1v4a1 1 0 0 1-1 1h-6a1 1 0 0 1-1-1v-4a1 1 0 0 1 1-1zM13.5 13.5h6a1 1 0 0 1 1 1v4a1 1 0 0 1-1 1h-6a1 1 0 0 1-1-1v-4a1 1 0 0 1 1-1z",
    check: "M5 12.5l4.5 4.5L19 7.5",
    alert: "M12 8.5v4.5M12 16.5h.01M10.3 4 2.6 17.5A2 2 0 0 0 4.3 20.5h15.4a2 2 0 0 0 1.7-3L13.7 4a2 2 0 0 0-3.4 0z",
    bulb: "M9.5 18h5M10.5 21h3M12 3a6 6 0 0 0-3.6 10.8c.7.5 1.1 1.3 1.1 2.2h5c0-.9.4-1.7 1.1-2.2A6 6 0 0 0 12 3z",
    chevron: "M7 9.5l5 5 5-5",
    help: "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18zM9.6 9.4a2.5 2.5 0 1 1 3.4 2.4c-.6.3-1 .8-1 1.5v.4M12 16.8h.01",
    folder: "M3.5 7a1 1 0 0 1 1-1h5l2 2h8a1 1 0 0 1 1 1v9a1 1 0 0 1-1 1h-15a1 1 0 0 1-1-1z",
    bolt: "M13 3 5 13.5h6L10.5 21 19 10.5h-6z",
    memory: "M6 7h12a1 1 0 0 1 1 1v8a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V8a1 1 0 0 1 1-1zM9 7V4.5M12 7V4.5M15 7V4.5M9 19.5V17M12 19.5V17M15 19.5V17M9 10.5h6v3H9z",
    info: "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18zM12 11v5M12 8h.01",
  };
  let { name, size = 20 }: { name: string; size?: number } = $props();
</script>

<svg
  class="icon"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="1.8"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
>
  <path d={paths[name]} />
</svg>

<style>
  .icon {
    flex: none;
  }
</style>
