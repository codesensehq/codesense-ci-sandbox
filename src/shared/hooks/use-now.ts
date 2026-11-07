// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";

/**
 * A single, activity-gated clock. Returns an epoch-ms `now` that advances every
 * `intervalMs` ONLY while `active` is true, and is frozen otherwise — so a caller
 * rendering live-elapsed values (e.g. the SW-GS-25 outbox chip) forces exactly ONE
 * re-render per tick, and pays nothing when there's no in-flight work. When `active`
 * goes false the interval is torn down (no lingering background timer).
 *
 * Deliberately ONE timer at the consuming list, never one-per-row: N chips share the
 * same `now`, and an idle queue runs no timer at all. On re-activation `now` resumes
 * ticking from the NEXT interval (it is not re-synced synchronously — that would be a
 * setState in the effect body); a caller rendering `now - since` clamps the brief
 * one-tick staleness to its floor (e.g. "0s"), so no stale value is ever shown.
 */
export function useNow(active: boolean, intervalMs = 1000): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    // setState lives in the interval callback (a subscription), not the effect body —
    // so no synchronous cascading render, and the timer is the sole `now` driver.
    const id = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(id);
  }, [active, intervalMs]);
  return now;
}
