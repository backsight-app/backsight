// Self-check for nvidia.ts. The app has no test runner, so run it directly:
//   node --experimental-strip-types src/lib/nvidia.check.ts   (from app/)
import assert from "node:assert/strict";
import { clashesFor, clashSignature, useOtherKeys } from "./nvidia.ts";

const defaults = { "save-replay": "Alt+F10", screenshot: "Alt+F1", "toggle-mic": "Alt+M" };
const alternatives = { "save-replay": "Ctrl+Shift+F10", screenshot: "Ctrl+Shift+F1" };
const clash = (action: string, key: string) => ({ action, key, nvidia: "x" });

// Untouched defaults: the whole other set moves, clashing or not.
let keys: Record<string, string> = { ...defaults };
let r = useOtherKeys(keys, [clash("save-replay", "Alt+F10")], defaults, alternatives);
assert.deepEqual(r, { moved: ["save-replay", "screenshot"], stuck: [] });
assert.deepEqual(keys, { "save-replay": "Ctrl+Shift+F10", screenshot: "Ctrl+Shift+F1", "toggle-mic": "Alt+M" });

// Keys set by hand: only the clashing one moves, and never onto a key in use.
keys = { ...defaults, "toggle-mic": "Ctrl+Shift+F1" };
r = useOtherKeys(keys, [clash("save-replay", "Alt+F10"), clash("screenshot", "Alt+F1")], defaults, alternatives);
assert.deepEqual(r, { moved: ["save-replay"], stuck: ["screenshot"] });
assert.equal(keys.screenshot, "Alt+F1");
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
