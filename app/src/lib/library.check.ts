// Self-check for library.ts. The app has no test runner, so run it directly:
//   node --experimental-strip-types src/lib/library.check.ts   (from app/)
import assert from "node:assert/strict";
import { activeFilters, applyFilters, formatBytes, formatClock, groupByDay, noFilters, type LibraryItem } from "./library.ts";

const now = new Date(2026, 10, 8, 15, 0).getTime();
const at = (daysAgo: number, hour = 12) => new Date(2026, 10, 8 - daysAgo, hour).getTime();
const item = (o: Partial<LibraryItem>): LibraryItem => ({
  path: "p", name: "n", kind: "video", ext: "mp4", bytes: 1, created_ms: now, duration_ms: null, width: null, height: null,
  fps: null, video_codec: null, audio_tracks: null, thumbnail: null, ...o,
});

assert.equal(formatBytes(0), "0 B");
assert.equal(formatBytes(340 * 1024), "340 KB");
assert.equal(formatBytes(1.2 * 1024 ** 3), "1.2 GB");
assert.equal(formatBytes(512 * 1024 ** 2), "512 MB");
assert.equal(formatClock(125_000), "2:05");
assert.equal(formatClock(3_725_000), "1:02:05");

const items = [
  item({ path: "a", created_ms: at(0, 14), bytes: 2 * 1024 ** 3, width: 1920, height: 1080, duration_ms: 125_000 }),
  item({ path: "b", created_ms: at(0, 9), kind: "screenshot", ext: "png", bytes: 3 * 1024 ** 2 }),
  item({ path: "c", created_ms: at(1), bytes: 50 * 1024 ** 2, width: 3840, height: 2160, duration_ms: 20_000 }),
  item({ path: "d", created_ms: at(10), ext: "mkv" }), // no metadata at all
];
const ids = (f: Partial<ReturnType<typeof noFilters>>, tab: "all" | "videos" = "all") =>
  applyFilters(items, { ...noFilters(), ...f }, tab, now).map((i) => i.path).join("");

assert.equal(ids({}), "abcd");
assert.equal(ids({ date: "today" }), "ab");
assert.equal(ids({ date: "7d" }), "abc");
assert.equal(ids({ size: "xl" }), "a");
assert.equal(ids({ size: "custom", sizeMinMb: "10", sizeMaxMb: "100" }), "c");
assert.equal(ids({ format: "mkv" }), "d");
assert.equal(ids({ resolution: "4k" }, "videos"), "c");
assert.equal(ids({ resolution: "4k" }, "all"), "abcd", "resolution is ignored where its filter is hidden");
assert.equal(ids({ duration: "s" }, "videos"), "c");
assert.equal(ids({ date: "custom", dateFrom: "2026-11-07T00:00", dateTo: "2026-11-07T23:59" }), "c");
assert.deepEqual(activeFilters({ ...noFilters(), duration: "s", size: "xl" }, "all"), ["Size"]);

const groups = groupByDay(items, now);
assert.deepEqual(groups.map((g) => [g.label, g.items.length]), [["Today", 2], ["Yesterday", 1], [groups[2].label, 1]]);
console.log("library.ts: all checks passed");
