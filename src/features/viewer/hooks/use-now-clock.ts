// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";

/**
 * One shared ticking `Date.now()` for "elapsed"/"countdown" labels — a single interval that drives
 * every row's time display, so a panel never mounts a timer per row. Kept always-on (the `setState`
 * lives in the interval callback, so a row that appears after an idle-open panel still reads a live
 * value); an empty-panel tick is a harmless no-op re-render, and memoized derived state doesn't
 * recompute on it. Extracted so every surface that shows relative times shares ONE implementation.
 *
 * @param intervalMs how often to tick (default 1s — the natural granularity for elapsed seconds).
 * @param enabled gate the interval on a boolean (e.g. "the panel has rows") so an empty/idle panel
 *   does zero per-second work; keep it a boolean so a content refresh doesn't churn the timer.
 */
export function useNowClock(intervalMs = 1000, enabled = true): number {
  const [nowMs, setNowMs] = useState(() => Date.now());
  useEffect(() => {
    if (!enabled) return;
    const id = setInterval(() => setNowMs(Date.now()), intervalMs);
    return () => clearInterval(id);
  }, [intervalMs, enabled]);
  return nowMs;
}
