// Moving Backsight's hotkeys off the keys NVIDIA's overlay uses (Setup and Home share this).

/** A Backsight hotkey that NVIDIA's overlay also uses (`nvidia::Clash` in the engine). */
export type Clash = { action: string; key: string; nvidia: string };

/** One of NVIDIA's overlay shortcuts (`nvidia::OverlayKey` in the engine). */
export type OverlayKey = { key: string; nvidia: string };

// "alt+F10" and "Alt+F10" are the same key, as in the engine's hotkey parser.
const same = (combo: string) => {
  const parts = combo.toLowerCase().split("+").map((p) => p.trim());
  const key = parts.pop();
  return [...new Set(parts)].sort().join("+") + "|" + key;
};

/** Backsight hotkeys that NVIDIA's overlay also uses, in `hotkeys` order. */
export function clashesFor(hotkeys: Record<string, string>, overlay: OverlayKey[]): Clash[] {
  return Object.entries(hotkeys).flatMap(([action, key]) => {
    const hit = key ? overlay.find((o) => same(o.key) === same(key)) : undefined;
    return hit ? [{ action, key, nvidia: hit.nvidia }] : [];
  });
}

/** Names a set of clashes, to remember that its notice was dismissed. */
export const clashSignature = (clashes: Clash[]) =>
  clashes
    .map((c) => `${c.action}=${c.key}`)
    .sort()
    .join(",");

/**
 * Keys still exactly the previous default set (which NVIDIA's overlay shared) all move to today's
 * defaults, so they stay grouped. Otherwise only the clashing ones move. A key moves to its default,
 * or to its `fallback` when the default is itself an NVIDIA key (Alt+R) or something else uses it.
 * Changes `keys`; returns the actions moved and the clashing ones that couldn't be.
 */
export function useOtherKeys(
  keys: Record<string, string>,
  clashes: Clash[],
  defaults: Record<string, string>,
  previous: Record<string, string>,
  fallback: Record<string, string>,
  overlay: OverlayKey[],
): { moved: string[]; stuck: string[] } {
  const untouched = Object.entries(previous).every(([a, k]) => keys[a] === k);
  const moving = untouched ? Object.keys(previous) : clashes.map((c) => c.action);
  const moved: string[] = [];
  const stuck: string[] = [];
  // A key that moves to its fallback takes its partner along (Alt+V records, Alt+Shift+V pauses),
  // when the partner still has its default.
  const partners: Record<string, string> = { "toggle-recording": "pause-recording", "save-replay": "toggle-replay" };
  const queue = [...moving];
  for (const action of queue) {
    const free = (key?: string) =>
      !!key &&
      !overlay.some((o) => same(o.key) === same(key)) &&
      !Object.entries(keys).some(([a, k]) => a !== action && k && same(k) === same(key));
    const following = !moving.includes(action);
    const key = (following ? [fallback[action]] : [defaults[action], fallback[action]]).find(free);
    if (key) {
      keys[action] = key;
      moved.push(action);
      const partner = partners[action];
      if (key === fallback[action] && partner && keys[partner] === defaults[partner] && !queue.includes(partner)) queue.push(partner);
    } else stuck.push(action);
  }
  return { moved, stuck: stuck.filter((a) => clashes.some((c) => c.action === a)) };
}
