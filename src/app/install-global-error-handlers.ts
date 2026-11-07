// SPDX-License-Identifier: MIT
import { logger } from "@/shared/lib/logger";

// Module-level guard: StrictMode's dev double-invoke and Vite HMR can both re-run
// boot code; the window listeners must register exactly once per document.
let installed = false;

// RF-perf-5 (AC-4): a monotonic count of unhandled promise rejections since this
// JS context loaded. The blank-screen incident showed a burst of 7 rejections in
// one second as the renderer tore down; surfacing the count on the gated memory
// line makes that signal reconstructable from the app log alone. A JS-context
// reload resets it to 0 (a fresh module scope), which is itself the "renderer
// re-initialized" signal.
let unhandledRejectionTotal = 0;

/** RF-perf-5 (AC-4): unhandled-rejection count since load, for the heartbeat to
 *  forward to the gated Rust memory sampler. */
export function unhandledRejectionCount(): number {
  return unhandledRejectionTotal;
}

function formatReason(reason: unknown): string {
  if (reason instanceof Error) return reason.stack ?? `${reason.name}: ${reason.message}`;
  if (typeof reason === "string") return reason;
  // A rejection reason is often a plain object (e.g. a Tauri IPC / serde error
  // payload); `String(obj)` would collapse it to "[object Object]" and lose the
  // fingerprint. Serialize instead. Note `JSON.stringify` RETURNS `undefined`
  // (no throw) for undefined/function/symbol reasons — the `?? String(reason)`
  // keeps the string contract + a fingerprint; the catch covers cyclic/BigInt.
  try {
    return JSON.stringify(reason) ?? String(reason);
  } catch {
    return String(reason);
  }
}

/**
 * Register global JS-error observability (SF-11 Piece 2).
 *
 * Catches the class of throws a React error boundary CANNOT: errors inside
 * `setInterval` / Tauri `listen` callbacks, and unhandled promise rejections.
 * Both are forwarded to `logger.error` (→ tauri-plugin-log → the rotated log
 * file) so the next silent crash leaves an on-disk fingerprint. Non-blocking
 * (`logger.error` is fire-and-forget) and idempotent (safe to call repeatedly).
 */
export function installGlobalErrorHandlers(): void {
  if (installed) return;
  installed = true;

  window.addEventListener("error", (event: ErrorEvent) => {
    // Skip content-free events (e.g. a cross-origin opaque "Script error." with
    // no message and no error, or a stray resource-error) — they add noise to
    // the crash log without a diagnosable fingerprint.
    if (!event.message && !event.error) return;
    const where = `${event.filename || "<unknown>"}:${event.lineno}:${event.colno}`;
    const stack = event.error instanceof Error && event.error.stack ? event.error.stack : "";
    logger.error("window.onerror", `${event.message} (${where})${stack ? `\n${stack}` : ""}`);
  });

  window.addEventListener("unhandledrejection", (event: PromiseRejectionEvent) => {
    // RF-perf-5 (AC-4): count then log — the count rides the heartbeat, the line
    // keeps the per-rejection fingerprint. This observer never `preventDefault`s,
    // so counting cannot alter existing rejection surfacing.
    unhandledRejectionTotal += 1;
    logger.error("unhandledrejection", formatReason(event.reason));
  });
}
