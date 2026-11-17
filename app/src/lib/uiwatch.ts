// What happens inside the window reaches Backsight's log (`app/src-tauri/src/uiwatch.rs`): page
// errors, unhandled promise rejections and failed commands; and a heartbeat every 2 s, so a page
// that stops answering is noticed.
import { invoke as tauriInvoke, isTauri, type InvokeArgs } from "@tauri-apps/api/core";

/** Sends a line to Backsight's log. Never throws. */
export function logProblem(level: "error" | "warn", message: string): void {
  if (isTauri()) tauriInvoke("ui_log", { level, message }).catch(() => {});
}

function describe(e: unknown): string {
  return e instanceof Error ? `${e.message}${e.stack ? `\n${e.stack}` : ""}` : String(e);
}

/** `invoke` that also logs a failure; the caller still gets the error. */
export async function invoke<T>(cmd: string, args?: InvokeArgs): Promise<T> {
  try {
    return await tauriInvoke<T>(cmd, args);
  } catch (e) {
    logProblem("warn", `${cmd} failed: ${describe(e)}`);
    throw e;
  }
}

/** Call once per page. */
export function watchPage(): void {
  if (!isTauri()) return;
  addEventListener("error", (e) =>
    logProblem("error", `${e.message} (${e.filename}:${e.lineno}:${e.colno})${e.error?.stack ? `\n${e.error.stack}` : ""}`));
  addEventListener("unhandledrejection", (e) => logProblem("error", `unhandled: ${describe(e.reason)}`));
  // Hidden pages' timers are slowed down, so the page says whether it is shown.
  const beat = () => void tauriInvoke("ui_heartbeat", { visible: !document.hidden }).catch(() => {});
  beat();
  setInterval(beat, 2000);
  document.addEventListener("visibilitychange", beat);
}
