// Builds canonical hotkey strings ("Ctrl+Alt+F10") from browser events.
// Key names must match NAMED_KEYS in crates/backsight-config/src/hotkey.rs.

const RENAMED: Record<string, string> = { ArrowUp: "Up", ArrowDown: "Down", ArrowLeft: "Left", ArrowRight: "Right" };
const SAME_NAME = new Set([
  "Space", "Tab", "Enter", "Backspace", "Insert", "Delete", "Home", "End", "PageUp", "PageDown",
  "PrintScreen", "Pause", "ScrollLock", "NumLock", "NumpadAdd", "NumpadSubtract", "NumpadMultiply",
  "NumpadDivide", "NumpadDecimal", "NumpadEnter", "Minus", "Equal", "BracketLeft", "BracketRight",
  "Backslash", "Semicolon", "Quote", "Backquote", "Comma", "Period", "Slash",
]);
const MOUSE: Record<number, string> = { 1: "Mouse3", 3: "Mouse4", 4: "Mouse5" };

function keyName(code: string): string | null {
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit[0-9]$/.test(code)) return code.slice(5);
  if (/^(F([1-9]|1[0-9]|2[0-4])|Numpad[0-9])$/.test(code)) return code;
  return RENAMED[code] ?? (SAME_NAME.has(code) ? code : null);
}

const modifiers = (e: KeyboardEvent | MouseEvent) =>
  [e.ctrlKey && "Ctrl", e.altKey && "Alt", e.shiftKey && "Shift", e.metaKey && "Win"].filter(Boolean) as string[];

/** Null while only modifiers are held. */
export function comboFromKeyboard(e: KeyboardEvent): string | null {
  const key = keyName(e.code);
  return key ? [...modifiers(e), key].join("+") : null;
}

/** Middle and side mouse buttons can be hotkeys; left and right clicks can't. */
export function comboFromMouse(e: MouseEvent): string | null {
  const key = MOUSE[e.button];
  return key ? [...modifiers(e), key].join("+") : null;
}

export const ACTIONS = [
  { id: "save-replay", group: "Instant Replay", label: "Save clip", desc: "Saves the last few minutes as a clip." },
  { id: "toggle-replay", group: "Instant Replay", label: "Turn Instant Replay on or off", desc: "Stops or starts the always-on rewind." },
  { id: "toggle-recording", group: "Recording", label: "Start or stop recording", desc: "Records until you press it again." },
  { id: "pause-recording", group: "Recording", label: "Pause or resume recording", desc: "Skips the boring bits without making a new file." },
  { id: "add-marker", group: "Recording", label: "Add a marker", desc: "Drops a bookmark so you can find this moment later." },
  { id: "screenshot", group: "Screenshots", label: "Screenshot: whole screen", desc: "Captures the monitor your game is on." },
  { id: "screenshot-window", group: "Screenshots", label: "Screenshot: active window", desc: "Captures only the focused window." },
  { id: "screenshot-region", group: "Screenshots", label: "Screenshot: pick an area", desc: "Drag a box around what you want." },
  { id: "toggle-mic", group: "Microphone", label: "Mute or unmute mic", desc: "Flips your microphone on or off in recordings." },
  { id: "push-to-talk", group: "Microphone", label: "Push to talk", desc: "Your mic is only recorded while this is held." },
  { id: "open-app", group: "App", label: "Open Backsight", desc: "Brings this window back from the tray." },
] as const;
