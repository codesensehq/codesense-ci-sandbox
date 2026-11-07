// SPDX-License-Identifier: MIT
/**
 * SR-11 — dev-only User Timing buffer reclaim.
 *
 * The vite **dev** bundle loads React's development build, whose Component
 * Performance Track emits a `performance.measure()` per render/commit (gated only
 * on `supportsUserTiming`, which is TRUE on WKWebView — not on an active profiler).
 * WebKit never evicts User Timing entries, so the buffer ratchets unbounded — a
 * live `heap` of the incident renderer showed 6.4M `WebCore::PerformanceMeasure`
 * ≈ 6.7 GB, which is what beachballs the app and lags typing while dogfooding.
 *
 * This module periodically clears that buffer so a dev session stops ratcheting.
 *
 * Mounted at **module scope from `main.tsx`, outside the React tree**, behind an
 * `import.meta.env.DEV` dynamic import — the same posture as
 * {@link file://./renderer-heartbeat.ts}. Two reasons, both load-bearing:
 * 1. A timer on the event loop cannot be starved by a wedged/beachballing subtree
 *    — which is exactly the state this fix targets. A React `useEffect` interval
 *    would be the first thing to starve (and would double-mount under StrictMode).
 * 2. The DEV-gated dynamic import means Vite never bundles this module into the
 *    production build, so shipped users carry zero code, zero timer, zero cost.
 *
 * Clearing is safe: no FIRST-PARTY app code reads User Timing entries back (no
 * `getEntriesByType`/`PerformanceObserver` consumers exist in `src/`), and the only
 * first-party producer (`sr0-wave-trace`, flag-off by default) already tolerates
 * evicted marks. The mass producer is React's dev-build instrumentation itself —
 * dev-only and best-effort (its measures carry numeric start/end timestamps, not
 * mark-name lookups, so an evicted mark cannot break an in-flight measure). So this
 * is a dumb, unconditional sweep — no observer-active guard.
 */

/**
 * Sweep cadence. 30 s bounds the buffer to a few thousand entries between sweeps
 * (negligible) while adding no meaningful cost.
 *
 * Deliberately independent of `renderer-heartbeat`'s `RENDERER_HEARTBEAT_INTERVAL_MS`
 * (also 30 s): that constant is contractually locked to the Rust
 * `RENDERER_ACK_INTERVAL_MS` watchdog cadence (guarded by a drift test). Coupling
 * the buffer sweep to it would let a future watchdog retune silently change this.
 */
export const DEV_USER_TIMING_SWEEP_INTERVAL_MS = 30_000;

let started = false;
let timer: ReturnType<typeof setInterval> | null = null;

/**
 * Start the dev-only User Timing reclaim sweep. Idempotent — a second call
 * (StrictMode / HMR) is a no-op, so only one interval is ever live.
 *
 * Defensive: if `performance.clearMeasures` is unavailable (a non-DOM import or
 * the happy-dom test env), arming is skipped entirely — never a throw. The env
 * (DEV) gate lives at the `main.tsx` call site so this function stays directly
 * unit-testable regardless of `import.meta.env`.
 *
 * No immediate sweep: the buffer is empty at boot, so the first clear is one full
 * interval later — nothing to reclaim before then.
 */
export function startDevUserTimingReclaim(
  intervalMs: number = DEV_USER_TIMING_SWEEP_INTERVAL_MS,
): void {
  if (started) return;
  if (typeof performance === "undefined" || typeof performance.clearMeasures !== "function") {
    return;
  }
  started = true;

  timer = setInterval(() => {
    try {
      performance.clearMeasures();
      // `clearMarks` shipped alongside `clearMeasures` in User Timing L2, but
      // optional-chain it so a partial engine that has one without the other still
      // reclaims the measures (the actual leak) rather than throwing mid-sweep.
      performance.clearMarks?.();
    } catch {
      // A background sweep must never surface as an uncaught timer error — a
      // hostile engine that throws mid-clear just skips this beat.
    }
  }, intervalMs);
}

/** Test-only: stop the interval and reset the idempotency guard. */
export function __resetDevUserTimingReclaimForTest(): void {
  if (timer !== null) {
    clearInterval(timer);
    timer = null;
  }
  started = false;
}
