// SPDX-License-Identifier: MIT
/**
 * SW-AM-8 — shared wiring for the session-registry lifecycle event that makes the
 * Agents roster live (no manual Refresh on background/foreground/create/kill).
 *
 * The trusted side emits {@link SESSION_STATE_CHANGED_EVENT} from the
 * dashboard-launch command layer on every registry transition; the app-root
 * live hub consumes these transitions; other readers refresh their own domains.
 * Declaring the name ONCE — instead of repeating the `"pty-exit"`
 * bare-string-in-6-files pattern — keeps the Rust emit and the FE subscriber from
 * typo-drifting.
 */

/** Tauri event announcing a session-registry lifecycle transition (create /
 *  background / foreground / kill). Payload is empty — a receiver re-snapshots
 *  via `dashboardListSessions()`, exactly as the `pty-exit` path does. Must match
 *  the Rust `SESSION_STATE_CHANGED` const in `dashboard_launch.rs`. */
export const SESSION_STATE_CHANGED_EVENT = "session-state-changed";

/** A pending trailing-edge coalescer: repeated {@link Coalescer.schedule} calls
 *  within the window collapse to a single trailing `fn()`. {@link Coalescer.cancel}
 *  clears a pending fire (call on teardown). */
export interface Coalescer {
  schedule: () => void;
  cancel: () => void;
}

/**
 * Minimal trailing-edge coalescer, extracted so the two listeners share one
 * debounce implementation rather than each hand-rolling the same
 * `clearTimeout`/`setTimeout` dance. Every `schedule()` resets the timer; the
 * wrapped `fn` fires once, `ms` after the last call.
 */
export function createTrailingCoalescer(fn: () => void, ms: number): Coalescer {
  let timer: ReturnType<typeof setTimeout> | null = null;
  return {
    schedule() {
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => {
        timer = null;
        fn();
      }, ms);
    },
    cancel() {
      if (timer) {
        clearTimeout(timer);
        timer = null;
      }
    },
  };
}
