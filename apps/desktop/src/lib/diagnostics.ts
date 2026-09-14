// Frontend half of the diagnostic session log.
//
// Events are buffered and flushed in batches. One `invoke` per click would
// double IPC traffic and skew the very timings this log exists to measure.
//
// The log is off unless the backend says otherwise, so the first thing this
// module does is ask once. When it is off, `recordDiagnostic` becomes a cheap
// no-op and nothing crosses the IPC boundary.
//
// Nothing here throws: a diagnostic that breaks the app it observes is worse
// than no diagnostic.

// Deliberately the raw Tauri invoke, not the wrapper in ./ipc — logging the
// logger would recurse.
import { invoke } from "@tauri-apps/api/core";

export type DiagnosticSource = "ui" | "ipc";

export type DiagnosticEvent = {
  source: DiagnosticSource;
  name: string;
  durationMs?: number;
  ok?: boolean;
  error?: string;
  /** Short, non-secret context: a view name, a count, a reason code. */
  detail?: string;
};

const FLUSH_INTERVAL_MS = 500;
const FLUSH_THRESHOLD = 25;
/** Bounded so a pre-probe burst, or a wedged backend, cannot grow without end. */
const MAX_PENDING = 500;

let pending: DiagnosticEvent[] = [];
let timer: ReturnType<typeof setTimeout> | null = null;
let enabled: boolean | undefined;
let probe: Promise<void> | null = null;

function inTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/** Monotonic where available; `Date.now` is only a fallback for the preview. */
export function nowMs(): number {
  return typeof performance !== "undefined" ? performance.now() : Date.now();
}

export function describeError(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === "string") return error;
  return String(error);
}

/** Returns the state known so far: `undefined` while the probe is in flight. */
function ensureProbe(): boolean | undefined {
  if (probe !== null || enabled !== undefined) return enabled;
  if (!inTauriRuntime()) {
    enabled = false;
    pending = [];
    return enabled;
  }
  probe = invoke<string | null>("diagnostics_log_path")
    .then((path) => {
      enabled = typeof path === "string" && path.length > 0;
    })
    .catch(() => {
      enabled = false;
    })
    .then(() => {
      if (enabled === false) {
        pending = [];
        stopTimer();
      }
    });
  return enabled;
}

export function recordDiagnostic(event: DiagnosticEvent): void {
  if (ensureProbe() === false) return;

  if (pending.length >= MAX_PENDING) pending.shift();
  pending.push(event);

  if (pending.length >= FLUSH_THRESHOLD) {
    void flushDiagnostics();
    return;
  }
  scheduleFlush();
}

export async function flushDiagnostics(): Promise<void> {
  stopTimer();
  if (enabled === undefined && probe !== null) await probe;
  if (enabled !== true) {
    pending = [];
    return;
  }
  if (pending.length === 0) return;

  const batch = pending;
  pending = [];
  try {
    await invoke("log_ui_events", { events: batch });
  } catch {
    // Dropping diagnostics is always preferable to surfacing an error the user
    // cannot act on.
  }
}

function scheduleFlush(): void {
  if (timer !== null) return;
  timer = setTimeout(() => {
    timer = null;
    void flushDiagnostics();
  }, FLUSH_INTERVAL_MS);
}

function stopTimer(): void {
  if (timer === null) return;
  clearTimeout(timer);
  timer = null;
}

// A window being hidden or closed is the most likely moment to lose the tail of
// a session, which is usually the part worth reading.
if (typeof document !== "undefined") {
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") void flushDiagnostics();
  });
}
