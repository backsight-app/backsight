import { isTauri } from "@tauri-apps/api/core";
import { invoke } from "./uiwatch";

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
