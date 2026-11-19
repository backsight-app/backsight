import { isTauri } from "@tauri-apps/api/core";
import type { Scan } from "./overlays";
import { invoke } from "./uiwatch";

/** Other apps' hotkeys on this PC, for the `settings` hotkeys; empty with `failed` when the check failed. Outside the app (Vite dev) a sample; `?mock=none` for none. */
export async function loadScan(settings: unknown): Promise<Scan> {
  if (isTauri()) return invoke<Scan>("overlay_scan", { settings }).catch(() => ({ apps: [], taken: [], failed: true }));
  if (import.meta.env.DEV) return (await import("./overlays-mock")).mockScan();
  return { apps: [], taken: [] };
}

/** One encoder's result from the engine's encoder check (`backsight_encode::probe`). */
export type ProbeResult = {
  encoder_id: string;
  problem: { reason: string; detail: string } | null;
  /** Set when the encoder works only with fewer B-frames than usual (0 for HEVC on GTX 10-series). */
  max_bframes?: number;
};

let checked: Promise<ProbeResult[]> | undefined;

/** Which encoders work on this PC; the engine runs the check once per start. */
export function encoderCheck(): Promise<ProbeResult[]> {
  checked ??= isTauri() ? invoke<ProbeResult[]>("encoder_check").catch(() => []) : Promise.resolve([]);
  return checked;
}

/** Encoder id → most B-frames it can use on this PC, for encoders that can't use their usual amount. */
export async function bframeLimits(): Promise<Record<string, number>> {
  const results = await encoderCheck();
  return Object.fromEntries(results.flatMap((r) => (r.max_bframes === undefined ? [] : [[r.encoder_id, r.max_bframes]])));
}

/** Encoder id → why it doesn't work here, for encoders that failed the check. */
export async function brokenEncoders(): Promise<Record<string, string>> {
  const results = await encoderCheck();
  return Object.fromEntries(results.flatMap((r) => (r.problem ? [[r.encoder_id, r.problem.reason]] : [])));
}

/** Downloads and starts the waiting update (ADR 0014); `progress` gets whole percents. Throws why it failed. */
export async function installUpdate(progress: (percent: number) => void): Promise<void> {
  const { listen } = await import("@tauri-apps/api/event");
  const unlisten = await listen<{ done: number; total: number | null }>("update-progress", (e) => {
    if (e.payload.total) progress(Math.floor((100 * e.payload.done) / e.payload.total));
  });
  try {
    await invoke("install_update");
  } finally {
    unlisten();
  }
}
