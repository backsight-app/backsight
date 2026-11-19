// Self-check for overlays.ts. The app has no test runner, so run it directly:
//   node --experimental-strip-types src/lib/overlays.check.ts   (from app/)
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { clashesFor, clashSignature, same, takenClashes, useOtherKeys, type App } from "./overlays.ts";

const previous = { "save-replay": "Alt+F10", screenshot: "Alt+F1" };
const defaults = { "save-replay": "Alt+C", screenshot: "Alt+S", "toggle-mic": "Alt+T" };
const fallback = { "save-replay": "Alt+X" };
const app = (keys: string[], o: Partial<App> = {}): App => ({
  id: "x", name: "X", running: true, saved: false, passes: "both", scope: "always",
  keys: keys.map((key) => ({ key, does: "does x" })), ...o,
});
const clash = (action: string, key: string, app_id = "x") => ({ action, key, app: "X", app_id, does: "x", passes: "both" });

// Still the previous default set: all of it moves to today's defaults, clashing or not.
let keys: Record<string, string> = { ...previous, "toggle-mic": "Alt+M" };
let r = useOtherKeys(keys, [clash("save-replay", "Alt+F10")], defaults, previous, fallback, {}, []);
assert.deepEqual(r, { moved: ["save-replay", "screenshot"], stuck: [] });
assert.deepEqual(keys, { "save-replay": "Alt+C", screenshot: "Alt+S", "toggle-mic": "Alt+M" });

// Keys set by hand: only the clashing one moves, and never onto a key in use.
keys = { ...previous, "toggle-mic": "Alt+S", "add-marker": "Alt+F7" };
keys["save-replay"] = "Alt+F9";
r = useOtherKeys(keys, [clash("save-replay", "Alt+F9"), clash("screenshot", "Alt+F1")], defaults, previous, fallback, {}, []);
assert.deepEqual(r, { moved: ["save-replay"], stuck: ["screenshot"] });
assert.equal(keys["save-replay"], "Alt+C");
assert.equal(keys.screenshot, "Alt+F1");

// A default that is itself an app's key, or taken, gives way to the fallback key.
const rDefaults = { "toggle-recording": "Alt+R", "pause-recording": "Alt+Shift+R" };
const rFallback = { "toggle-recording": "Alt+V", "pause-recording": "Alt+Shift+V" };
keys = { "toggle-recording": "Alt+R", "pause-recording": "Alt+Shift+R" };
r = useOtherKeys(keys, [clash("toggle-recording", "Alt+R")], rDefaults, previous, rFallback, {}, [app(["alt+r"])]);
// Pause follows record to its fallback, so the pair stays together.
assert.deepEqual(r, { moved: ["toggle-recording", "pause-recording"], stuck: [] });
assert.deepEqual(keys, { "toggle-recording": "Alt+V", "pause-recording": "Alt+Shift+V" });
keys = { "save-replay": "Alt+F9", "open-app": "Alt+C" };
r = useOtherKeys(keys, [clash("save-replay", "Alt+F9")], defaults, previous, fallback, {}, []);
assert.equal(keys["save-replay"], "Alt+X", "default taken by Open Backsight");
// Default and fallback taken or an app's: the first free spare, skipping app and Backsight keys.
const spares = { "save-replay": ["Alt+N", "Alt+J", "Alt+Y"], "toggle-recording": ["Alt+Y", "Alt+J"] };
keys = { "toggle-recording": "Alt+R", "pause-recording": "Alt+Shift+R", screenshot: "Alt+Y" };
r = useOtherKeys(keys, [clash("toggle-recording", "Alt+R")], rDefaults, previous, rFallback, spares, [app(["Alt+R", "Alt+V"])]);
assert.equal(keys["toggle-recording"], "Alt+J", "Alt+V is NVIDIA's push-to-talk, Alt+Y is the screenshot key");
assert.equal(keys["pause-recording"], "Alt+Shift+J", "pause follows record, with Shift");
// A partner whose Shift key isn't free stays where it is.
keys = { "toggle-recording": "Alt+R", "pause-recording": "Alt+Shift+R", "add-marker": "Alt+Shift+V" };
r = useOtherKeys(keys, [clash("toggle-recording", "Alt+R")], rDefaults, previous, rFallback, {}, [app(["Alt+R"])]);
assert.deepEqual(r, { moved: ["toggle-recording"], stuck: [] });
assert.equal(keys["pause-recording"], "Alt+Shift+R");
// Nothing free: the key stays and is reported (once, even with two apps on it).
keys = { "save-replay": "Alt+F9", "open-app": "Alt+C", screenshot: "Alt+X" };
r = useOtherKeys(keys, [clash("save-replay", "Alt+F9"), clash("save-replay", "Alt+F9", "y")], defaults, previous, fallback, spares, [app(["Alt+N", "Alt+J", "Alt+Y"])]);
assert.deepEqual(r, { moved: [], stuck: ["save-replay"] });
// A key another program holds counts as a clash, follows the live key, and is never picked again.
assert.deepEqual(takenClashes({ screenshot: "Alt+S", "open-app": "Alt+O" }, [{ action: "screenshot", key: "alt+s" }, { action: "open-app", key: "Alt+P" }]), [
  { action: "screenshot", key: "Alt+S", app: "", app_id: "taken", does: "", passes: "taken" },
]);
keys = { "save-replay": "Alt+F9", screenshot: "Alt+C" };
r = useOtherKeys(keys, [clash("save-replay", "Alt+F9"), { ...clash("screenshot", "Alt+C"), passes: "taken" }], { "save-replay": "Alt+C", screenshot: "Alt+H" }, previous, {}, {}, []);
assert.deepEqual(r, { moved: ["screenshot"], stuck: ["save-replay"] });
assert.deepEqual(keys, { "save-replay": "Alt+F9", screenshot: "Alt+H" }, "Alt+C is held by a program, so save stays put");
// A cleared key ("no key") never clashes, and an app's unbound action never matches.
assert.deepEqual(clashesFor({ "save-replay": "" }, [app(["Alt+F10", ""])]), []);

// Clashes follow the live hotkeys, matching keys the way the engine does (case, modifier order),
// one per app that uses the key.
const apps = [
  { ...app(["Alt+F10"], { id: "nvidia", name: "NVIDIA app", passes: "hidden" }), keys: [{ key: "Alt+F10", does: "saves NVIDIA's Instant Replay" }] },
  { ...app([], { id: "discord", name: "Discord" }), keys: [{ key: "alt+f10", does: "saves a Discord clip" }, { key: "Ctrl+Alt+S", does: "mutes Discord" }] },
];
assert.deepEqual(clashesFor({ "save-replay": "alt+F10", screenshot: "Alt+Ctrl+S", "toggle-mic": "" }, apps), [
  { action: "save-replay", key: "alt+F10", app: "NVIDIA app", app_id: "nvidia", does: "saves NVIDIA's Instant Replay", passes: "hidden" },
  { action: "save-replay", key: "alt+F10", app: "Discord", app_id: "discord", does: "saves a Discord clip", passes: "both" },
  { action: "screenshot", key: "Alt+Ctrl+S", app: "Discord", app_id: "discord", does: "mutes Discord", passes: "both" },
]);
assert.deepEqual(clashesFor({ "save-replay": "Ctrl+Shift+F10" }, apps), []);
assert.deepEqual(clashesFor({ "save-replay": "Alt+F10" }, []), [], "no apps");

// A dismissal is for one set of clashes, whatever their order.
assert.equal(clashSignature([clash("b", "Alt+1"), clash("a", "Alt+2")]), clashSignature([clash("a", "Alt+2"), clash("b", "Alt+1")]));
assert.notEqual(clashSignature([clash("a", "Alt+2")]), clashSignature([clash("a", "Alt+3")]));
assert.notEqual(clashSignature([clash("a", "Alt+2")]), clashSignature([clash("a", "Alt+2", "y")]), "another app");

// Same key whatever the spelling, as the engine's parser reads it.
assert.equal(same("Control+Alt+S"), same("alt+ctrl+s"));
assert.equal(same("Meta+K"), same("Win+K"));
assert.equal(same("Super+Alt+K"), same("Alt+Win+K"));
assert.equal(same("Alt++C"), same("Alt+C"), "empty parts dropped");
assert.notEqual(same("Alt+C"), same("Alt+Shift+C"));

// With the real data files.
const content = (f: string) => JSON.parse(readFileSync(new URL(`../../../content/${f}`, import.meta.url), "utf8"));
const houseDefaults: Record<string, string> = content("default-settings.json").hotkeys;
const houseFallback: Record<string, string> = content("fallback-hotkeys.json");
const housePrevious: Record<string, string> = content("previous-default-hotkeys.json");
const spare: Record<string, string[]> = content("spare-hotkeys.json");

// The old F-key set untouched, and Open Backsight clashing with Intel's overlay: the set moves to
// today's defaults and Open Backsight, outside that set, moves too (its default is Intel's key).
keys = { ...houseDefaults, ...housePrevious, "open-app": "Alt+Shift+O" };
const intel = app(["Alt+Shift+O"], { id: "intel", name: "Intel Graphics Software" });
r = useOtherKeys(keys, clashesFor(keys, [intel]), houseDefaults, housePrevious, houseFallback, spare, [intel]);
assert.ok(r.moved.includes("open-app") && r.moved.includes("save-replay"), JSON.stringify(r));
assert.deepEqual(r.stuck, []);
assert.equal(keys["open-app"], spare["open-app"][0]);
assert.equal(keys["save-replay"], "Alt+C");

// NVIDIA's real keys clash with record and pause: pause follows record with Shift, either order.
for (const order of [["toggle-recording", "pause-recording"], ["pause-recording", "toggle-recording"]]) {
  keys = { ...houseDefaults };
  const nvidia = app(["Alt+R", "Alt+Shift+R", "Alt+V"], { id: "nvidia", passes: "hidden" });
  const both = clashesFor(keys, [nvidia]).sort((a, b) => order.indexOf(a.action) - order.indexOf(b.action));
  assert.deepEqual(both.map((c) => c.action), order);
  r = useOtherKeys(keys, both, houseDefaults, housePrevious, houseFallback, spare, [nvidia]);
  assert.deepEqual(r.stuck, []);
  assert.equal(keys["toggle-recording"], "Alt+N", "Alt+R and Alt+V are NVIDIA's: the fallback Alt+N is free");
  assert.equal(keys["pause-recording"], "Alt+Shift+N", `order ${order}`);
}

// Spare keys: every action has some, in the house style, none a default, fallback or movement key.
const reserved = [...Object.values(houseDefaults), ...Object.values(houseFallback)]
  .filter(Boolean)
  .map(same);
for (const action of Object.keys(houseDefaults)) {
  const list = spare[action];
  assert.ok(list && list.length >= 4 && list.length <= 6, `${action} has 4-6 spare keys`);
  for (const key of list) {
    assert.match(key, /^Alt\+(Shift\+)?[A-Z]$/, key);
    assert.doesNotMatch(key, /\+[WASDQEFZ]$/, `${key}: a movement or overlay letter`);
    assert.ok(!reserved.includes(same(key)), `${key} is already a Backsight key`);
  }
}
console.log("overlays.ts ok");
