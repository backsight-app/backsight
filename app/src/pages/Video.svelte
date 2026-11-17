<script lang="ts">
  import Icon from "../lib/components/Icon.svelte";
  import OptionList from "../lib/components/OptionList.svelte";
  import Row from "../lib/components/Row.svelte";
  import Select from "../lib/components/Select.svelte";
  import Toggle from "../lib/components/Toggle.svelte";
  import { isTauri } from "@tauri-apps/api/core";
  import { invoke } from "../lib/uiwatch";
  import { catalog, codecLabel, container, fixCompatibility, vendorLabel, videoEncoder, videoOptions, type Choice } from "../lib/content";
  import { app } from "../lib/settings.svelte";
  import { fitHdr, hdrFits, hdrPossible, listed } from "../lib/hdr";
  import { bframeLimits, brokenEncoders } from "../lib/setup";
  import { onMount } from "svelte";

  const v = $derived(app.settings.video);
  const encoder = $derived(videoEncoder(v.encoder));
  const box = $derived(container(v.container));
  const hdrCompatible = $derived(hdrFits(v, catalog));
  const hdrEncoders = catalog.videoEncoders.filter((e: any) => e.hdr).map((e: any) => e.label).join(" or ");
  // What turning HDR video on switched, until HDR is turned off again.
  let hdrSwitched = $state("");
  const split = catalog.limits.splitMinutes;
  let note = $state("");

  // Encoders that failed the engine's check on this PC, with what they need.
  let broken = $state<Record<string, string>>({});
  // Encoders that work here but take fewer B-frames than usual.
  let bframeCaps = $state<Record<string, number>>({});
  onMount(() => {
    brokenEncoders().then((b) => (broken = b));
    bframeLimits().then((l) => (bframeCaps = l));
  });
  const encoderChoices = $derived(
    catalog.videoEncoders.map((e: any) => ({
      value: e.id,
      label: e.label,
      glossary: e.glossary,
      group: vendorLabel[e.vendor],
      hint: e.hardware ? undefined : "Uses your CPU",
      disabled: e.id in broken && e.id !== v.encoder,
      disabledReason: broken[e.id],
    })),
  );
  const containerChoices = $derived(
    catalog.containers.map((c: any) => ({
      value: c.id,
      label: `${c.label} (.${c.ext})`,
      glossary: c.glossary,
      hint: c.crashSafe ? "Crash-safe" : undefined,
      disabled: !c.videoCodecs.includes(encoder?.codec),
      disabledReason: `Can't hold ${codecLabel(encoder?.codec)} video`,
    })),
  );
  const resolutions: Choice[] = catalog.resolutions;
  const frameRates: Choice[] = catalog.frameRates;
  const targets: Choice[] = catalog.captureTargets;

  type Monitor = { id: string; label: string; primary: boolean };
  let monitors = $state<Monitor[]>([]);
  if (isTauri()) invoke<Monitor[]>("list_monitors").then((m) => (monitors = m), () => (monitors = []));
  const monitorChoices = $derived([
    { value: "", label: "Main monitor" },
    // The saved monitor stays listed if it has become the main one since.
    ...monitors.filter((m) => !m.primary || m.id === v.capture_monitor).map((m) => ({ value: m.id, label: m.label })),
    ...(v.capture_monitor && !monitors.some((m) => m.id === v.capture_monitor)
      ? [{ value: v.capture_monitor, label: "Chosen monitor (not connected)" }]
      : []),
  ]);

  function change(apply: () => void) {
    apply();
    note = fixCompatibility(app.settings) ?? "";
  }

  // Turning HDR video on switches whatever HDR needs on this PC, in one go; the engine records
  // standard video (and Home says so) for anything changed later.
  async function setHdr(on: boolean) {
    hdrSwitched = "";
    if (on) {
      // Until the encoder check answers, every encoder looks like it works.
      hdrSwitched = "Checking which encoders work on this PC…";
      broken = await brokenEncoders();
      hdrSwitched = "";
    }
    const switched = on ? fitHdr(v, catalog, broken) : [];
    if (!switched) return; // no HDR encoder works here: it stays off and the note below says why
    v.hdr = on;
    note = fixCompatibility(app.settings) ?? "";
    if (switched.length) hdrSwitched = `HDR video needs ${listed(switched)}, so Backsight switched to ${switched.length > 1 ? "them" : "it"}.`;
  }
  const hdrNote = $derived(
    !hdrPossible(catalog, broken)
      ? `This PC can't record HDR video: ${hdrEncoders} doesn't work here.${v.hdr ? " Videos are standard." : ""}`
      : v.hdr && !hdrCompatible
        ? "These settings can't record HDR, so videos are standard. Turn HDR video off and on again to fix them."
        : hdrSwitched,
  );
</script>

<div class="stack">
  <section class="card">
    <header><h2>Compression</h2></header>
    <Row term="encoder">
      <Select name="encoder" label="Encoder" value={v.encoder} choices={encoderChoices} onchange={(id) => change(() => (v.encoder = id))} />
    </Row>
    <Row term="container">
      <Select name="container" label="File format" value={v.container} choices={containerChoices} onchange={(id) => change(() => (v.container = id))} />
    </Row>
    {#if note}
      <div class="notice"><Icon name="info" size={18} /><span>{note}</span></div>
    {/if}
    {#if broken[v.encoder]}
      <div class="notice"><Icon name="info" size={18} /><span>{encoder?.label} doesn't work on this PC. {broken[v.encoder]} Pick another encoder.</span></div>
    {/if}
    {#if box?.id === "mkv"}
      <Row term="remux">
        <Toggle label="Convert to MP4 when done" checked={v.remux_to_mp4} onchange={(x) => (v.remux_to_mp4 = x)} />
      </Row>
    {/if}
  </section>

  {#if encoder}
    <section class="card">
      <header>
        <h2>{encoder.label} settings</h2>
        {#if !app.advanced}<p>Switch to Advanced to see every option.</p>{/if}
      </header>
      <OptionList
        options={videoOptions(encoder).filter((option) => !(v.hdr && hdrCompatible && option.id === "profile"))}
        store={v.encoder_options}
        key={encoder.id}
        limits={encoder.id in bframeCaps ? { bframes: bframeCaps[encoder.id] } : {}}
      />
    </section>
  {/if}

  <section class="card">
    <header><h2>Picture</h2></header>
    <Row term="video-hdr" note={hdrNote}>
      <Toggle label="HDR video" checked={v.hdr} onchange={setHdr} />
    </Row>
    <Row term="capture-target">
      <Select name="capture-target" label="What to record" value={v.capture_target} choices={targets} onchange={(x) => (v.capture_target = x)} />
    </Row>
    {#if v.capture_target !== "window" && monitorChoices.length > 1}
      <Row term="capture-monitor">
        <Select name="capture-monitor" label="Monitor" value={v.capture_monitor} choices={monitorChoices} onchange={(x) => (v.capture_monitor = x)} />
      </Row>
    {/if}
    <Row term="resolution">
      <Select name="resolution" label="Output resolution" value={v.resolution} choices={resolutions} onchange={(x) => (v.resolution = x)} />
    </Row>
    <Row term="frame-rate">
      <Select name="fps" label="Frame rate" value={v.fps} choices={frameRates} onchange={(x) => (v.fps = x)} />
    </Row>
    <Row term="capture-cursor">
      <Toggle label="Show cursor" checked={v.capture_cursor} onchange={(x) => (v.capture_cursor = x)} />
    </Row>
    {#if app.advanced}
      <Row term="file-splitting">
        <label class="number">
          <input
            type="number"
            min={split.min}
            max={split.max}
            value={v.split_minutes}
            aria-label="Split recordings every"
            onchange={(e) => {
              v.split_minutes = Math.max(split.min, Math.min(split.max, Math.round(Number(e.currentTarget.value) || 0)));
              e.currentTarget.value = String(v.split_minutes); // a clamp to the same value doesn't redraw
            }}
          />
          <span class="unit">min</span>
        </label>
      </Row>
    {/if}
  </section>
</div>
