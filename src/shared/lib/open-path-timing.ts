// SPDX-License-Identifier: MIT
import { logger } from "@/shared/lib/logger";

/**
 * SW-SO-2 (① — AC-1 measurement gate): dev-flag-gated timing of the
 * worktree-open critical path, so the before/after ranking AC-1 owes can be
 * captured ON THE REAL app without shipping any always-on cost.
 *
 * Mirrors the Rust `sync_timing_enabled()` discipline: OFF unless explicitly
 * enabled, then it logs a ranked per-phase breakdown through the normal logger
 * (target `open-timing`). Enable at runtime — no rebuild — with
 * `localStorage.setItem("codesense:open-timing", "1")` then reopen a story;
 * disable by removing the key. The gate is read once at timer start, so a
 * toggle takes effect on the next open.
 *
 * The whole instrument is null / no-op when disabled — callers use
 * `timer?.mark(...)` and `startOpenTimer` returns `null`, so there is no
 * `performance.now()` call and no allocation on the hot path in production.
 */
export const OPEN_TIMING_KEY = "codesense:open-timing";

export function isOpenTimingEnabled(): boolean {
  try {
    // localStorage can be absent (tests / SSR) or throw (locked-down webview).
    return typeof localStorage !== "undefined" && localStorage.getItem(OPEN_TIMING_KEY) === "1";
  } catch {
    return false;
  }
}

export interface OpenPhaseTimer {
  /** Close the running span and open `phase` as the next one. The first call
   *  measures from timer start; each subsequent call from the previous mark. */
  mark(phase: string): void;
  /** Emit a ranked (slowest-first) per-phase breakdown + total for this open. */
  report(): void;
}

/**
 * Start a per-open phase timer, or `null` when timing is disabled (production
 * default). `now` is injected so the phase-delta + ranking math is unit-testable
 * without a real clock.
 */
export function startOpenTimer(
  label: string,
  now: () => number = () => performance.now(),
): OpenPhaseTimer | null {
  if (!isOpenTimingEnabled()) return null;
  const start = now();
  let last = start;
  const phases: Array<{ phase: string; ms: number }> = [];
  return {
    mark(phase: string) {
      const t = now();
      phases.push({ phase, ms: t - last });
      last = t;
    },
    report() {
      const total = last - start;
      const breakdown = [...phases]
        .sort((a, b) => b.ms - a.ms)
        .map((p) => `${p.phase}=${p.ms.toFixed(1)}ms`)
        .join("  ");
      logger.info("open-timing", `open(${label}) total=${total.toFixed(1)}ms  ${breakdown}`);
    },
  };
}

/**
 * Begin timing a single span (e.g. the xterm construction band ② defers) and
 * return a `done()` that logs its elapsed ms when called — or `null` when timing
 * is disabled, so the caller pays nothing. Split from a wrap-a-closure helper for
 * call sites that can't wrap their body (the terminal mount effect returns a
 * teardown from mid-body, so a begin/done pair avoids re-indenting it).
 */
export function beginOpenSpan(
  label: string,
  now: () => number = () => performance.now(),
): (() => void) | null {
  if (!isOpenTimingEnabled()) return null;
  const t0 = now();
  return () => {
    logger.info("open-timing", `${label}=${(now() - t0).toFixed(1)}ms`);
  };
}
