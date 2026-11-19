// Other apps' hotkeys (Discord, the NVIDIA app, Steam, ...) and Backsight keys they share. Home,
// Setup and Hotkeys use this; the engine's `overlays` module finds the apps (`loadScan` in setup.ts).

/** One of an app's hotkeys: "Alt+C", "saves a Discord clip" (`overlays::AppKey`). */
export type AppKey = { key: string; does: string };
/** An app on this PC with hotkeys (`overlays::App`). `saved`: keys read from its own settings. */
export type App = {
  id: string;
  name: string;
  running: boolean;
  saved: boolean;
  /** "both": both apps act on the press. "hidden": the app may keep the key from Backsight. */
  passes: string;
  /** "always", or "in-game" when its keys only work while a game runs. */
  scope: string;
  keys: AppKey[];
};
/** A Backsight key some other program has reserved (`overlays::Taken`). */
export type Taken = { action: string; key: string };
/** `failed`: the check didn't run (set by `loadScan`), so empty lists mean nothing. */
export type Scan = { apps: App[]; taken: Taken[]; failed?: boolean };
/** A Backsight hotkey that an app also uses (`overlays::Clash`). */
export type Clash = { action: string; key: string; app: string; app_id: string; does: string; passes: string };

// "alt+F10", "Alt+F10" and "Control+Alt+F10" match as in the engine's hotkey parser (hotkey.rs).
const MODS: Record<string, string> = { ctrl: "ctrl", control: "ctrl", alt: "alt", shift: "shift", win: "win", meta: "win", super: "win" };
export const same = (combo: string) => {
  const parts = combo.toLowerCase().split("+").map((p) => p.trim()).filter(Boolean);
  const mods = [...new Set(parts.filter((p) => p in MODS).map((p) => MODS[p]))].sort();
  return mods.join("+") + "|" + parts.filter((p) => !(p in MODS)).join("+");
};

/** Backsight hotkeys that an app also uses, in `hotkeys` order (one per app that uses it). */
export function clashesFor(hotkeys: Record<string, string>, apps: App[]): Clash[] {
  return Object.entries(hotkeys).flatMap(([action, key]) =>
    !key
      ? []
      : apps.flatMap((a) =>
          a.keys
            .filter((k) => k.key && same(k.key) === same(key))
            .map((k) => ({ action, key, app: a.name, app_id: a.id, does: k.does, passes: a.passes })),
        ),
  );
}

/**
 * Backsight hotkeys some other program has reserved, as clashes with `passes: "taken"` (Backsight
 * won't see the key at all), while each still has the key it was probed with.
 */
export const takenClashes = (hotkeys: Record<string, string>, taken: Taken[]): Clash[] =>
  taken
    .filter((t) => hotkeys[t.action] && same(hotkeys[t.action]) === same(t.key))
    .map((t) => ({ action: t.action, key: hotkeys[t.action], app: "", app_id: "taken", does: "", passes: "taken" }));

/** Names a set of clashes, to remember that its notice was dismissed. */
export const clashSignature = (clashes: Clash[]) =>
  [...new Set(clashes.map((c) => `${c.action}=${c.key}@${c.app_id}`))].sort().join(",");

/**
 * Keys still exactly the previous default set all move to today's defaults, so they stay grouped,
 * along with every clashing key; otherwise only the clashing ones move. A key moves to the first free one of its default, its
 * `fallback`, then its `spare` keys; free means no app here, no program holding a key (a "taken"
 * clash) and no other Backsight action uses it.
 * Changes `keys`; returns the actions moved and the clashing ones that couldn't be.
 */
export function useOtherKeys(
  keys: Record<string, string>,
  clashes: Clash[],
  defaults: Record<string, string>,
  previous: Record<string, string>,
  fallback: Record<string, string>,
  spares: Record<string, string[]>,
  apps: App[],
): { moved: string[]; stuck: string[] } {
  const untouched = Object.entries(previous).every(([a, k]) => keys[a] === k);
  const clashing = clashes.map((c) => c.action);
  const moving = [...new Set([...(untouched ? Object.keys(previous) : []), ...clashing])];
  const appKeys = [
    ...apps.flatMap((a) => a.keys.filter((k) => k.key).map((k) => same(k.key))),
    ...clashes.filter((c) => c.passes === "taken").map((c) => same(c.key)),
  ];
  const moved: string[] = [];
  const stuck: string[] = [];
  // A key that moves off its default takes its partner along to the same key with Shift (Alt+V
  // records, Alt+Shift+V pauses), when the partner still has its default or is moving anyway.
  const partners: Record<string, string> = { "toggle-recording": "pause-recording", "save-replay": "toggle-replay" };
  const isPartner = (a: string) => Object.values(partners).includes(a);
  const queue = [...moving].sort((a, b) => +isPartner(a) - +isPartner(b)); // leaders first
  const follow: Record<string, string> = {};
  const done = new Set<string>();
  for (const action of queue) {
    done.add(action);
    const free = (key?: string) =>
      !!key &&
      !appKeys.includes(same(key)) &&
      !Object.entries(keys).some(([a, k]) => a !== action && k && same(k) === same(key));
    const own = moving.includes(action) ? [defaults[action], fallback[action], ...(spares[action] ?? [])] : [];
    const key = [follow[action], ...own].find(free);
    if (key) {
      keys[action] = key;
      moved.push(action);
      const partner = partners[action];
      const waits = !!partner && partner in keys && (keys[partner] === defaults[partner] || moving.includes(partner));
      if (key !== defaults[action] && waits && !done.has(partner)) {
        follow[partner] = key.replace(/^Alt\+/, "Alt+Shift+");
        if (!queue.includes(partner)) queue.push(partner);
      }
    } else stuck.push(action);
  }
  return { moved, stuck: stuck.filter((a) => clashes.some((c) => c.action === a)) };
}
