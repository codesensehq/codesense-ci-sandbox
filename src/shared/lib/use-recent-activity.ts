// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";

/**
 * True for `idleMs` after each change of `signal`, then false once the signal
 * has been quiet that long. A monotonically-bumped counter (e.g. a live-change
 * nonce) drives an "active now / idle" affordance without the caller wiring its
 * own timers.
 *
 * Each new `signal` restarts the idle timer, so a sustained burst stays "active"
 * throughout and only decays after the last change. The initial `signal` value
 * does not count as activity — a freshly-mounted consumer reads `false` until the
 * first real bump.
 */
export function useRecentActivity(signal: number, idleMs: number): boolean {
  const [active, setActive] = useState(false);
  // Adjust-during-render (React's sanctioned idiom): a changed signal flips to
  // active synchronously, so the effect below owns only the async decay timer —
  // never a synchronous setState in an effect body.
  const [seen, setSeen] = useState(signal);
  if (seen !== signal) {
    setSeen(signal);
    if (!active) setActive(true);
  }

  useEffect(() => {
    if (!active) return;
    const handle = window.setTimeout(() => setActive(false), idleMs);
    return () => window.clearTimeout(handle);
    // `signal` is a dep so a fresh bump restarts the idle window mid-burst.
  }, [active, signal, idleMs]);

  return active;
}
