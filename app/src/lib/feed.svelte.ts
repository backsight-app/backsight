// What's in the Library, for Home (today's numbers, latest captures) and the side menu ("new"
// badge, free space). One scan shared by both, done again whenever a file is saved; the Library
// page keeps its own.
import { isTauri } from "@tauri-apps/api/core";
import { invoke } from "./uiwatch";
import type { Library } from "./library";

const SEEN = "backsight.librarySeen";

export const feed = $state<{ lib: Library | null; seen: number; space: [number, number] | null }>({
  lib: null,
  seen: readSeen(),
  space: null,
});

function readSeen() {
  try {
    return Number(localStorage.getItem(SEEN)) || 0;
  } catch {
    return 0;
  }
}

/** The Library page was opened: what's there now is no longer new. */
export function markSeen() {
  feed.seen = Date.now();
  try {
    localStorage.setItem(SEEN, String(feed.seen));
  } catch {}
}

let started = false;
export function startFeed() {
  if (started || !isTauri()) return;
  started = true;
  // First run: nothing counts as new yet.
  if (!feed.seen) markSeen();
  const load = () => {
    invoke<Library>("list_library").then((l) => (feed.lib = l), () => {});
    invoke<[number, number] | null>("clips_space").then((s) => (feed.space = s), () => {});
  };
  load();
  void import("@tauri-apps/api/event").then((m) => m.listen("library-changed", load));
}
