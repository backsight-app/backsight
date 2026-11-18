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
 * defaults, so they stay grouped. Otherwise only the clashing ones move, and only to a default key
 * nothing else uses. Changes `keys`; returns the actions moved and the clashing ones that couldn't be.
 */
export function useOtherKeys(
  keys: Record<string, string>,
  clashes: Clash[],
  defaults: Record<string, string>,
  previous: Record<string, string>,
): { moved: string[]; stuck: string[] } {
  const untouched = Object.entries(previous).every(([a, k]) => keys[a] === k);
  const moving = untouched ? Object.keys(previous) : clashes.map((c) => c.action);
  const moved: string[] = [];
  const stuck: string[] = [];
  for (const action of moving) {
    const key = defaults[action];
    const taken = Object.entries(keys).some(([a, k]) => a !== action && k === key);
    if (key && !taken) {
      keys[action] = key;
      moved.push(action);
    } else stuck.push(action);
  }
  return { moved, stuck: stuck.filter((a) => clashes.some((c) => c.action === a)) };
}
