// Instrumented toasts.
//
// Import `toast` from here instead of `sonner`. Toasts are where errors become
// visible to the user, so they are the single best record of what actually went
// wrong during a session — and there is no shared error helper in this codebase
// to hook instead.
//
// Only the members the app actually uses are re-exported. Reaching for a new
// one is a type error rather than a silent instrumentation gap.

import { toast as sonnerToast } from "sonner";
import { recordDiagnostic } from "./diagnostics";

type ToastFn = typeof sonnerToast.error;
type ToastMessage = Parameters<ToastFn>[0];
type ToastData = Parameters<ToastFn>[1];

/**
 * Titles are literals in this codebase; descriptions are usually a stringified
 * backend error. Both are scrubbed again in Rust before they reach the file, so
 * this only needs to pass through what it can read as text.
 */
function describeToast(message: ToastMessage, data: ToastData): string {
  const title = typeof message === "string" ? message : "";
  const description =
    data !== undefined && typeof data.description === "string" ? data.description : "";
  if (title && description) return `${title}: ${description}`;
  return title || description;
}

function instrumented(kind: "error" | "warning" | "info" | "success", fn: ToastFn): ToastFn {
  return (message, data) => {
    const text = describeToast(message, data);
    recordDiagnostic({
      source: "ui",
      name: `toast.${kind}`,
      ok: kind !== "error",
      error: kind === "error" ? text : undefined,
      detail: kind === "error" ? undefined : text
    });
    return fn(message, data);
  };
}

export const toast = {
  error: instrumented("error", sonnerToast.error),
  warning: instrumented("warning", sonnerToast.warning),
  info: instrumented("info", sonnerToast.info),
  success: instrumented("success", sonnerToast.success),
  // Pass-throughs: not user-facing failures, nothing to learn from recording them.
  message: sonnerToast.message,
  custom: sonnerToast.custom,
  dismiss: sonnerToast.dismiss
};
