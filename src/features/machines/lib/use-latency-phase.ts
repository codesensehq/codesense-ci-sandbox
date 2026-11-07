// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";

/** Show a spinner only once a remote load has taken this long (Sally §6). */
const SPINNER_AFTER_MS = 150;
/** Then say so plainly and offer Cancel. */
const STILL_LOADING_AFTER_MS = 2000;

export type LatencyPhase = "idle" | "spinner" | "slow";

/**
 * The remote-load latency ladder: `idle` for the first 150 ms (fast loads never
 * flash a spinner), `spinner` until 2 s, then `slow` ("Still loading from
 * ‹machine›…" + Cancel). Resets the moment the load settles.
 */
export function useLatencyPhase(pending: boolean): LatencyPhase {
  const [phase, setPhase] = useState<Exclude<LatencyPhase, "idle"> | null>(null);
  useEffect(() => {
    if (!pending) return;
    const spinner = setTimeout(() => setPhase("spinner"), SPINNER_AFTER_MS);
    const slow = setTimeout(() => setPhase("slow"), STILL_LOADING_AFTER_MS);
    return () => {
      clearTimeout(spinner);
      clearTimeout(slow);
      setPhase(null);
    };
  }, [pending]);
  return pending ? (phase ?? "idle") : "idle";
}
