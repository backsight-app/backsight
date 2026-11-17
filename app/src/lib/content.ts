// The shared content files at the repo root drive the whole settings UI.
// Rust reads the same files (crates/backsight-encode/src/catalog.rs), so the
// engine and the UI can never disagree about what an option means.
import glossaryJson from "../../../content/glossary.en.json";
import catalogJson from "../../../content/encoders.json";

export type GlossaryEntry = {
  term: string;
  short: string;
  eli5: string;
  pros?: string[];
  cons?: string[];
  pickIf?: string;
};

export type Choice = {
  value: any;
  label: string;
  hint?: string;
  glossary?: string;
  group?: string;
  disabled?: boolean;
  disabledReason?: string;
};

export type OptionDef = {
  id: string;
  label: string;
  glossary: string;
  kind: "select" | "number" | "toggle";
  default: any;
  choices?: Choice[];
  min?: number;
  max?: number;
  step?: number;
  unit?: string;
  advanced?: boolean;
  showIf?: { option: string; in: any[] };
  /** Shown under the option when this PC limits it (see `OptionList`'s `limits`). */
  limitedNote?: string;
};

export const glossary = glossaryJson as Record<string, GlossaryEntry>;
export const catalog = catalogJson as any;

export const vendorLabel: Record<string, string> = {
  nvidia: "NVIDIA",
  amd: "AMD",
  intel: "Intel",
  cpu: "Software (CPU)",
};

const byId = (list: any[], id: string) => list.find((x) => x.id === id);
export const videoEncoder = (id: string) => byId(catalog.videoEncoders, id);
export const audioEncoder = (id: string) => byId(catalog.audioEncoders, id);
export const container = (id: string) => byId(catalog.containers, id);
export const codecLabel = (id: string) => byId(catalog.videoCodecs, id)?.label ?? id;

/** Family options with the encoder's overrides applied (mirrors `Catalog::video_options`). */
export function videoOptions(encoder: any): OptionDef[] {
  return (catalog.families[encoder?.family] ?? [])
    .filter((opt: OptionDef) => !encoder.overrides?.[opt.id]?.omit)
    .map((opt: OptionDef) => ({ ...opt, ...(encoder.overrides?.[opt.id] ?? {}) }));
}

export function resolved(defs: OptionDef[], saved: Record<string, any> = {}) {
  return Object.fromEntries(defs.map((o) => [o.id, saved[o.id] ?? o.default]));
}

export function isVisible(opt: OptionDef, values: Record<string, any>, advanced: boolean) {
  if (opt.advanced && !advanced) return false;
  return !opt.showIf || opt.showIf.in.includes(values[opt.showIf.option]);
}

/** Keeps encoder, container and audio format compatible. Returns a note when it had to switch something. */
export function fixCompatibility(s: any): string | null {
  const notes: string[] = [];
  const enc = videoEncoder(s.video.encoder);
  let box = container(s.video.container);
  if (enc && box && !box.videoCodecs.includes(enc.codec)) {
    const next = catalog.containers.find((c: any) => c.videoCodecs.includes(enc.codec));
    notes.push(`${box.label} can't hold ${codecLabel(enc.codec)} video, so the file format was switched to ${next.label}.`);
    s.video.container = next.id;
    box = next;
  }
  if (box && !box.audioEncoders.includes(s.audio.encoder)) {
    const next = audioEncoder(box.audioEncoders[0]);
    notes.push(`${box.label} can't hold ${audioEncoder(s.audio.encoder)?.label} audio, so audio was switched to ${next.label}.`);
    s.audio.encoder = next.id;
  }
  return notes.length ? notes.join(" ") : null;
}

const LOSSLESS_KBPS: Record<string, number> = { flac: 900, alac: 1000, "pcm-16": 1536, "pcm-24": 2304, "pcm-32f": 3072 };

/** Rough data rate of the current settings. `video` is null for constant-quality modes. */
export function estimateKbps(s: any): { video: number | null; audio: number } {
  const enc = videoEncoder(s.video.encoder);
  const v = resolved(videoOptions(enc), s.video.encoder_options[enc?.id]);
  const aenc = audioEncoder(s.audio.encoder);
  const perTrack = LOSSLESS_KBPS[aenc?.id] ?? resolved(aenc?.options ?? [], s.audio.encoder_options[aenc?.id]).bitrate ?? 192;
  // Game-only audio replaces desktop loopback. Separate tracks add a copy of each source.
  const sources = Number(s.audio.desktop) + Number(s.audio.microphone);
  const tracks = sources === 0 ? 0 : 1 + (s.audio.separate_tracks ? sources : 0);
  return { video: v.rc === "cq" ? null : Number(v.bitrate), audio: perTrack * tracks };
}

export const megabytes = (kbps: number, seconds: number) => Math.round((kbps * seconds) / 8 / 1000);

export function formatDuration(seconds: number) {
  const m = Math.floor(seconds / 60);
  const s = seconds % 60;
  return m ? `${m}:${String(s).padStart(2, "0")} min` : `${s} s`;
}
