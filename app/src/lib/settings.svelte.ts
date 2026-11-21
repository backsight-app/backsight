import { isTauri } from "@tauri-apps/api/core";
import { invoke } from "./uiwatch";
import { fixCompatibility } from "./content";
import defaults from "../../../content/default-settings.json";

export type Status = "preview" | "loading" | "saving" | "saved" | "error";

export const app = $state({
  settings: structuredClone(defaults) as any,
  advanced: false,
  status: (isTauri() ? "loading" : "preview") as Status,
  error: "",
  // Never save until the file was read: a broken file must not be overwritten with defaults.
  loaded: false,
  /** Setup was opened again from General (a first run opens it through `settings.setup.done`). */
  setupOpen: false,
  /** What opened setup again (General's Run setup), focused again when it closes. */
  setupOpener: null as HTMLElement | null,
});

/** Puts one settings section (one page) back to its defaults; the others stay as they are. */
export function resetSection(section: keyof typeof defaults) {
  app.settings[section] = structuredClone(defaults[section]);
  if (section === "video") void useWorkingEncoder();
}

/** The default encoder is NVIDIA's; on a PC without it, ask the engine's check and switch (as first-run setup does). */
async function useWorkingEncoder() {
  if (!isTauri()) return;
  const asked = app.settings.video.encoder;
  try {
    const c = await invoke<{ pick: { encoder: string; automatic: boolean } | null }>("setup_check", {
      settings: $state.snapshot(app.settings),
    });
    if (c.pick?.automatic && app.settings.video.encoder === asked) {
      app.settings.video.encoder = c.pick.encoder;
      fixCompatibility(app.settings);
    }
  } catch {
    // The check is a convenience: keep the defaults if it can't run.
  }
}

export async function loadSettings() {
  if (!isTauri()) return;
  // The engine may change a setting by itself (a lighter one when clips stutter): its copy wins,
  // so a save from here can't put the old one back.
  const { listen } = await import("@tauri-apps/api/event");
  listen<any>("settings-changed", (e) => {
    // The engine changes only video. An edit still waiting for its save is newer and wins (its save
    // restarts the session anyway); otherwise the auto-save writes the new video with any other edit.
    if (timer === undefined) app.settings.video = e.payload.video;
  });
  try {
    app.settings = await invoke("get_settings");
    app.loaded = true;
    app.status = "saved";
  } catch (e) {
    app.status = "error";
    app.error = String(e);
  }
}

/** Live Instant Replay state from the engine (see `get_status` in main.rs). */
export type ReplayStatus = {
  state: "on" | "off" | "failed";
  message: string;
  buffer_seconds: number;
  buffer_bytes: number;
  dropped_frames: number;
  late_frames: number;
  recording: boolean;
  recording_seconds: number;
  recording_paused: boolean;
  recording_message: string;
  pending_apply: boolean;
  audio_problem: string | null;
  mic_muted: boolean;
  game_audio_from: string | null;
  capture_notice: string | null;
  hotkey_problem: string | null;
  /** The game being watched (its folder name), null for the desktop. */
  game: string | null;
  /** A newer version is ready to install (installed copies check by themselves). */
  update: string | null;
};

let timer: ReturnType<typeof setTimeout> | undefined;
// Saves run one after another: Rust's lock doesn't keep arrival order, so an older save could
// otherwise land last and win.
let saving: Promise<void> = Promise.resolve();

function save() {
  timer = undefined;
  const settings = $state.snapshot(app.settings);
  saving = saving.then(async () => {
    try {
      await invoke("save_settings", { settings });
      app.status = "saved";
    } catch (e) {
      app.status = "error";
      app.error = String(e);
    }
  });
  return saving;
}

export function scheduleSave() {
  if (!app.loaded) return;
  clearTimeout(timer);
  app.status = "saving";
  timer = setTimeout(save, 400);
}

/** Saves a change still waiting for its timer, and every save before it (the window is closing). */
export function flushSave(): Promise<void> {
  if (timer === undefined) return saving;
  clearTimeout(timer);
  return save();
}
