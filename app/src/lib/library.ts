// Pure logic for the Library page: filtering, grouping and friendly units.
// No imports on purpose, so it runs under plain Node (see library.check.ts).

export type MediaKind = "video" | "screenshot";
export interface LibraryItem {
  path: string;
  name: string;
  kind: MediaKind;
  ext: string;
  bytes: number;
  created_ms: number;
  duration_ms: number | null;
  width: number | null;
  height: number | null;
  fps: number | null;
  video_codec: string | null;
  audio_tracks: number | null;
  thumbnail: string | null;
  /** A captions file sits next to it (ADR 0016). */
  captions: boolean;
}
export interface Library {
  items: LibraryItem[];
  video_dir: string | null;
  screenshot_dir: string | null;
}

export type Tab = "all" | "screenshots" | "videos";
export const tabKind: Record<Tab, MediaKind | null> = { all: null, screenshots: "screenshot", videos: "video" };

const MB = 1024 * 1024;
const GB = 1024 * MB;
const DAY = 86_400_000;

// Each preset is a half-open range [min, max); null means unbounded.
export type Preset = { id: string; label: string; min?: number; max?: number };

export const datePresets = (now: number): Preset[] => {
  const d = new Date(now);
  d.setHours(0, 0, 0, 0);
  return [
    { id: "any", label: "Any time" },
    { id: "today", label: "Today", min: d.getTime() },
    { id: "7d", label: "Last 7 days", min: now - 7 * DAY },
    { id: "30d", label: "Last 30 days", min: now - 30 * DAY },
    { id: "custom", label: "Custom range…" },
  ];
};
export const sizePresets: Preset[] = [
  { id: "any", label: "Any size" },
  { id: "s", label: "Under 10 MB", max: 10 * MB },
  { id: "m", label: "10 – 100 MB", min: 10 * MB, max: 100 * MB },
  { id: "l", label: "100 MB – 1 GB", min: 100 * MB, max: GB },
  { id: "xl", label: "Over 1 GB", min: GB },
  { id: "custom", label: "Custom range…" },
];
export const durationPresets: Preset[] = [
  { id: "any", label: "Any length" },
  { id: "s", label: "Under 30 s", max: 30_000 },
  { id: "m", label: "30 s – 2 min", min: 30_000, max: 120_000 },
  { id: "l", label: "2 – 10 min", min: 120_000, max: 600_000 },
  { id: "xl", label: "Over 10 min", min: 600_000 },
];
// Compared against the short side of the frame, so vertical video lands in the right bucket.
export const resolutionPresets: Preset[] = [
  { id: "any", label: "Any resolution" },
  { id: "4k", label: "4K and up", min: 2160 },
  { id: "1440", label: "1440p", min: 1440, max: 2160 },
  { id: "1080", label: "1080p", min: 1080, max: 1440 },
  { id: "720", label: "720p", min: 720, max: 1080 },
  { id: "sd", label: "Below 720p", max: 720 },
];

export interface Filters {
  date: string;
  dateFrom: string; // datetime-local value, used when date === "custom"
  dateTo: string;
  size: string;
  sizeMinMb: string; // used when size === "custom"; strings so empty inputs mean "no limit"
  sizeMaxMb: string;
  format: string; // ext, or "any"
  duration: string;
  resolution: string;
}
export const noFilters = (): Filters => ({
  date: "any", dateFrom: "", dateTo: "", size: "any", sizeMinMb: "", sizeMaxMb: "",
  format: "any", duration: "any", resolution: "any",
});

/** Which filters make sense on a tab. Hidden filters are never applied. */
export const filtersFor = (tab: Tab) => ({
  format: true,
  duration: tab === "videos",
  resolution: tab === "videos",
});

const num = (s: string) => (s.trim() === "" || isNaN(+s) ? null : +s);
const time = (s: string) => (s ? new Date(s).getTime() || null : null);
const find = (list: Preset[], id: string) => list.find((p) => p.id === id);

function ranges(f: Filters, tab: Tab, now: number) {
  const fe = filtersFor(tab);
  const date = f.date === "custom"
    ? { min: time(f.dateFrom) ?? undefined, max: time(f.dateTo) !== null ? time(f.dateTo)! + 60_000 : undefined } // minute precision: include the whole minute
    : find(datePresets(now), f.date) ?? {};
  const size = f.size === "custom"
    ? { min: num(f.sizeMinMb) !== null ? num(f.sizeMinMb)! * MB : undefined, max: num(f.sizeMaxMb) !== null ? (num(f.sizeMaxMb)! + 0.0001) * MB : undefined }
    : find(sizePresets, f.size) ?? {};
  return {
    date,
    size,
    duration: fe.duration ? find(durationPresets, f.duration) ?? {} : {},
    resolution: fe.resolution ? find(resolutionPresets, f.resolution) ?? {} : {},
    format: f.format,
  };
}

const within = (v: number | null, r: { min?: number; max?: number }) =>
  (r.min === undefined && r.max === undefined) || (v !== null && (r.min === undefined || v >= r.min) && (r.max === undefined || v < r.max));

/** Short names of the filters that are narrowing the list right now. */
export function activeFilters(f: Filters, tab: Tab): string[] {
  const fe = filtersFor(tab);
  const on: string[] = [];
  if (f.date !== "any") on.push("Date");
  if (f.size !== "any") on.push("Size");
  if (f.format !== "any") on.push("Format");
  if (fe.duration && f.duration !== "any") on.push("Length");
  if (fe.resolution && f.resolution !== "any") on.push("Resolution");
  return on;
}

export function applyFilters(items: LibraryItem[], f: Filters, tab: Tab, now = Date.now()): LibraryItem[] {
  if (!activeFilters(f, tab).length) return items;
  const r = ranges(f, tab, now);
  return items.filter(
    (i) =>
      within(i.created_ms, r.date) &&
      within(i.bytes, r.size) &&
      (r.format === "any" || i.ext === r.format) &&
      within(i.duration_ms, r.duration) &&
      within(i.width !== null && i.height !== null ? Math.min(i.width, i.height) : null, r.resolution),
  );
}

/** A video taller than it is wide, such as a vertical clip (ADR 0015). */
export const isVertical = (i: LibraryItem) => i.kind === "video" && !!i.width && !!i.height && i.height > i.width;

export const totalBytes = (items: LibraryItem[]) => items.reduce((n, i) => n + i.bytes, 0);

export interface Group {
  key: string;
  label: string;
  items: LibraryItem[];
  bytes: number;
}

const dayKey = (ms: number) => {
  const d = new Date(ms);
  return `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
};

export function dayLabel(ms: number, now = Date.now()): string {
  if (dayKey(ms) === dayKey(now)) return "Today";
  if (dayKey(ms) === dayKey(now - DAY)) return "Yesterday";
  const sameYear = new Date(ms).getFullYear() === new Date(now).getFullYear();
  return new Date(ms).toLocaleDateString(undefined, {
    weekday: "long", day: "numeric", month: "long", ...(sameYear ? {} : { year: "numeric" }),
  });
}

/** Groups items that are already newest first into one group per calendar day. */
export function groupByDay(items: LibraryItem[], now = Date.now()): Group[] {
  const groups: Group[] = [];
  for (const item of items) {
    const key = dayKey(item.created_ms);
    let g = groups[groups.length - 1];
    if (g?.key !== key) groups.push((g = { key, label: dayLabel(item.created_ms, now), items: [], bytes: 0 }));
    g.items.push(item);
    g.bytes += item.bytes;
  }
  return groups;
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = bytes / 1024;
  let u = 0;
  while (v >= 1024 && u < units.length - 1) (v /= 1024), u++;
  return `${u === 0 || v >= 100 ? Math.round(v) : v.toFixed(1)} ${units[u]}`;
}

/** 125000 -> "2:05", 3725000 -> "1:02:05". */
export function formatClock(ms: number): string {
  const s = Math.round(ms / 1000);
  const [h, m, sec] = [Math.floor(s / 3600), Math.floor((s % 3600) / 60), s % 60];
  const pad = (n: number) => String(n).padStart(2, "0");
  return h ? `${h}:${pad(m)}:${pad(sec)}` : `${m}:${pad(sec)}`;
}

export const formatTime = (ms: number) => new Date(ms).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });

/** "Today, 14:32" or "9 Nov 2026, 14:32". */
export function formatWhen(ms: number, now = Date.now()): string {
  const day = dayKey(ms) === dayKey(now) ? "Today" : dayKey(ms) === dayKey(now - DAY) ? "Yesterday"
    : new Date(ms).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
  return `${day}, ${formatTime(ms)}`;
}

/** Formats that exist in these items, for the format filter. */
export const formatsIn = (items: LibraryItem[]) => [...new Set(items.map((i) => i.ext))].sort();
