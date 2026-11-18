// Self-check for nvidia.ts. The app has no test runner, so run it directly:
//   node --experimental-strip-types src/lib/nvidia.check.ts   (from app/)
import assert from "node:assert/strict";
import { clashesFor, clashSignature, useOtherKeys } from "./nvidia.ts";

const previous = { "save-replay": "Alt+F10", screenshot: "Alt+F1" };
const defaults = { "save-replay": "Alt+C", screenshot: "Alt+S", "toggle-mic": "Alt+T" };
const fallback = { "save-replay": "Alt+X" };
const clash = (action: string, key: string) => ({ action, key, nvidia: "x" });

// Still the previous default set: all of it moves to today's defaults, clashing or not.
let keys: Record<string, string> = { ...previous, "toggle-mic": "Alt+M" };
let r = useOtherKeys(keys, [clash("save-replay", "Alt+F10")], defaults, previous, fallback, []);
assert.deepEqual(r, { moved: ["save-replay", "screenshot"], stuck: [] });
assert.deepEqual(keys, { "save-replay": "Alt+C", screenshot: "Alt+S", "toggle-mic": "Alt+M" });

// Keys set by hand: only the clashing one moves, and never onto a key in use.
keys = { ...previous, "toggle-mic": "Alt+S", "add-marker": "Alt+F7" };
keys["save-replay"] = "Alt+F9";
r = useOtherKeys(keys, [clash("save-replay", "Alt+F9"), clash("screenshot", "Alt+F1")], defaults, previous, fallback, []);
assert.deepEqual(r, { moved: ["save-replay"], stuck: ["screenshot"] });
assert.equal(keys["save-replay"], "Alt+C");
assert.equal(keys.screenshot, "Alt+F1");

// A default that is itself an NVIDIA key, or taken, gives way to the fallback key.
const rDefaults = { "toggle-recording": "Alt+R", "pause-recording": "Alt+Shift+R" };
const rFallback = { "toggle-recording": "Alt+V", "pause-recording": "Alt+Shift+V" };
keys = { "toggle-recording": "Alt+R", "pause-recording": "Alt+Shift+R" };
r = useOtherKeys(keys, [clash("toggle-recording", "Alt+R")], rDefaults, previous, rFallback, [{ key: "alt+r", nvidia: "x" }]);
assert.deepEqual(r, { moved: ["toggle-recording"], stuck: [] });
assert.deepEqual(keys, { "toggle-recording": "Alt+V", "pause-recording": "Alt+Shift+R" });
keys = { "save-replay": "Alt+F9", "open-app": "Alt+C" };
r = useOtherKeys(keys, [clash("save-replay", "Alt+F9")], defaults, previous, fallback, []);
assert.equal(keys["save-replay"], "Alt+X", "default taken by Open Backsight");
// Both taken or NVIDIA's: the key stays and is reported.
keys = { "save-replay": "Alt+F9", "open-app": "Alt+C", screenshot: "Alt+X" };
r = useOtherKeys(keys, [clash("save-replay", "Alt+F9")], defaults, previous, fallback, []);
assert.deepEqual(r, { moved: [], stuck: ["save-replay"] });
// A cleared key ("no key") never clashes.
assert.deepEqual(clashesFor({ "save-replay": "" }, [{ key: "Alt+F10", nvidia: "x" }]), []);

// Clashes follow the live hotkeys, matching keys the way the engine does (case, modifier order).
const overlay = [
  { key: "Alt+F10", nvidia: "saves NVIDIA's Instant Replay" },
  { key: "Ctrl+Alt+S", nvidia: "is an NVIDIA overlay shortcut" },
];
assert.deepEqual(clashesFor({ "save-replay": "alt+F10", screenshot: "Alt+Ctrl+S", "toggle-mic": "" }, overlay), [
  { action: "save-replay", key: "alt+F10", nvidia: "saves NVIDIA's Instant Replay" },
  { action: "screenshot", key: "Alt+Ctrl+S", nvidia: "is an NVIDIA overlay shortcut" },
]);
assert.deepEqual(clashesFor({ "save-replay": "Ctrl+Shift+F10" }, overlay), []);
assert.deepEqual(clashesFor({ "save-replay": "Alt+F10" }, []), [], "overlay off");

// A dismissal is for one set of clashes, whatever their order.
assert.equal(clashSignature([clash("b", "Alt+1"), clash("a", "Alt+2")]), clashSignature([clash("a", "Alt+2"), clash("b", "Alt+1")]));
assert.notEqual(clashSignature([clash("a", "Alt+2")]), clashSignature([clash("a", "Alt+3")]));
console.log("nvidia.ts ok");
