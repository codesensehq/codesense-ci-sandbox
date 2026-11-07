// SPDX-License-Identifier: MIT
/**
 * Visibility-resilient animation-frame scheduling.
 *
 * WKWebView pauses `requestAnimationFrame` while its document is
 * `visibilityState === "hidden"`. The terminal's entire mount → first-frame path
 * (the frame-budget scheduler, the spawn/attach entry, the fit-wait poll, and the
 * reveal fit) funnels through rAF, so a workspace opened or reactivated while the
 * window is hidden (cold open before show, reload, background/minimized restore)
 * never mounts or fits: the containers exist but `.xterm` never appears until a
 * later rAF resume. This primitive removes that coupling.
 *
 * Design — visibility-checked at ARM time, not a permanent race:
 * - visible → `requestAnimationFrame` (byte-for-byte the prior behavior; no extra
 *   timer, no idle/battery cost);
 * - hidden  → `setTimeout` (which keeps firing while hidden) so forward progress
 *   never stalls.
 * Exactly ONE primitive is armed per call, so there is no double-fire / double-
 * drain to guard against, and the callback is always scheduled — never invoked
 * synchronously, so nothing blocks the main thread.
 *
 * Two boundaries the caller should know about (both acceptable for this use):
 * - **Hidden cadence is browser-throttled, not frame-fast.** A hidden document's
 *   `setTimeout` is clamped by the engine (WKWebView included) to ~1s, not the 16ms
 *   floor requested here — so while hidden the fallback advances at roughly ~1s per
 *   tick. Forward progress is guaranteed (the point: mount *happens* instead of
 *   *never*), just not at frame rate; a foregrounded user is unaffected (rAF path),
 *   and a hidden mount lands within a tick or two before the window is shown. This
 *   relies on WKWebView *throttling* (not fully *suspending*) hidden timers — the
 *   OC-2 native User Harness (Task 6) is what confirms that in the real stack.
 * - **Only ARM time is checked.** A callback armed via rAF while visible, then the
 *   window hides before the frame, stalls until re-show — identical to the prior
 *   plain-rAF behavior (not a regression). The bug this fixes is hidden-*at-arm*,
 *   which is the mount/reactivation case.
 *
 * The handle is a plain number (same shape `requestAnimationFrame` returns), so
 * this is a drop-in for existing `raf`/`cancelRaf` seams with no type changes. The
 * primitive's KIND is encoded in the handle's sign — a timer handle is returned
 * negated — so {@link cancelResilientRaf} dispatches to exactly the API that armed
 * it. (rAF ids and timeout ids are independent low-integer counters that DO overlap
 * numerically, so cancelling through both APIs on a bare id could cancel an
 * unrelated in-flight primitive of the other kind; the sign disambiguates without a
 * lookup table.) This is sound because the HTML spec requires both
 * `requestAnimationFrame` handles and `setTimeout` ids to be integers ≥ 1 — never
 * 0, never negative — so a negated timer handle is always ≤ -1 and never collides
 * with the (positive) rAF space or with 0. Negation (not bitwise `~`) is used
 * deliberately: it has no 32-bit ceiling, and the spec's ≥ 1 guarantee makes the
 * `-0` case impossible.
 *
 * Dependencies are injected so the visible/hidden branches are unit-testable
 * without a real document or a live rAF/timer loop.
 */

/** Lower bound for the hidden-path fallback (ms). The engine clamps a hidden
 *  document's timer up to ~1s regardless, so this is a floor, not a guarantee;
 *  it is intentionally decoupled from the visible-path frame budget. */
export const RESILIENT_FRAME_FALLBACK_MS = 16;

export interface ResilientRafDeps {
  /** True when rAF is paused (document hidden). */
  isHidden: () => boolean;
  /** Schedule on the next animation frame; returns a positive numeric handle. */
  raf: (cb: () => void) => number;
  /** Cancel a rAF handle. */
  cancelRaf: (handle: number) => void;
  /** Schedule via a timer that fires while hidden; returns a positive numeric handle. */
  setTimer: (cb: () => void, ms: number) => number;
  /** Cancel a timer handle. */
  clearTimer: (handle: number) => void;
}

const defaultDeps: ResilientRafDeps = {
  isHidden: () => typeof document !== "undefined" && document.visibilityState === "hidden",
  raf: (cb) => requestAnimationFrame(cb),
  cancelRaf: (handle) => cancelAnimationFrame(handle),
  setTimer: (cb, ms) => setTimeout(cb, ms) as unknown as number,
  clearTimer: (handle) => clearTimeout(handle),
};

/**
 * Schedule `cb` for the next frame, resilient to a hidden document. Returns a
 * numeric handle for {@link cancelResilientRaf} — positive for the rAF (visible)
 * branch, negated for the timer (hidden) branch. Never runs `cb` synchronously.
 */
export function resilientRaf(cb: () => void, deps: ResilientRafDeps = defaultDeps): number {
  // Negate the timer handle so its kind travels with the value: rAF/timeout id
  // spaces overlap, and the sign lets cancel target exactly the right API.
  return deps.isHidden() ? -deps.setTimer(cb, RESILIENT_FRAME_FALLBACK_MS) : deps.raf(cb);
}

/** Cancel a handle returned by {@link resilientRaf}, dispatching by its sign to
 *  the single API that armed it (negative → timer, otherwise → rAF). */
export function cancelResilientRaf(handle: number, deps: ResilientRafDeps = defaultDeps): void {
  if (handle < 0) deps.clearTimer(-handle);
  else deps.cancelRaf(handle);
}
