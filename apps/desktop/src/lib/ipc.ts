// Instrumented `invoke`.
//
// Import this instead of `@tauri-apps/api/core` so every call across the Tauri
// boundary is timed and its outcome recorded. Wrapping the import is what makes
// the log affordable: one edit per file covers every call site in it.
//
// The signature mirrors Tauri's exactly, so switching an import is the whole
// change — no call site is rewritten.

import {
  invoke as tauriInvoke,
  type InvokeArgs,
  type InvokeOptions
} from "@tauri-apps/api/core";
import { describeError, nowMs, recordDiagnostic } from "./diagnostics";

/** The log's own transport. Recording these would recurse. */
const UNINSTRUMENTED = new Set(["log_ui_events", "diagnostics_log_path"]);

export async function invoke<T>(
  cmd: string,
  args?: InvokeArgs,
  options?: InvokeOptions
): Promise<T> {
  if (UNINSTRUMENTED.has(cmd)) {
    return tauriInvoke<T>(cmd, args, options);
  }

  const startedAt = nowMs();
  try {
    const result = await tauriInvoke<T>(cmd, args, options);
    recordDiagnostic({
      source: "ipc",
      name: cmd,
      durationMs: Math.round(nowMs() - startedAt),
      ok: true
    });
    return result;
  } catch (error) {
    recordDiagnostic({
      source: "ipc",
      name: cmd,
      durationMs: Math.round(nowMs() - startedAt),
      ok: false,
      error: describeError(error)
    });
    throw error;
  }
}
