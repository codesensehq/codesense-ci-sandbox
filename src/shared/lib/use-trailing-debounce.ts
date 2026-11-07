// SPDX-License-Identifier: MIT
import { useEffect, useRef, useState } from "react";

/**
 * Trailing-edge debounce hook. The returned value lags `value` by
 * `delayMs`: when `value` changes it is held until no further change
 * arrives within `delayMs`, then the returned value catches up.
 *
 * Rapid changes coalesce — only the settled value ever propagates.
 * The debounced `setState` fires ONLY on settle, not per input change,
 * so the component does not incur a per-change re-render.
 *
 * `maxWaitMs` (optional) bounds how long a *sustained* burst can starve the
 * output: if changes keep arriving closer together than `delayMs`, a pure
 * trailing debounce would never settle, so the value is force-flushed once the
 * oldest pending change is `maxWaitMs` old and the cycle restarts. Omit it to
 * keep the plain trailing-only behavior (the default for all prior callers).
 */
export function useTrailingDebounce<T>(value: T, delayMs: number, maxWaitMs?: number): T {
  const [debounced, setDebounced] = useState(value);
  // When the current pending divergence began — for the max-wait clamp only.
  const pendingSinceRef = useRef<number | null>(null);

  useEffect(() => {
    if (value === debounced) {
      pendingSinceRef.current = null;
      return;
    }
    if (pendingSinceRef.current === null) pendingSinceRef.current = performance.now();
    let wait = delayMs;
    if (maxWaitMs != null) {
      const elapsed = performance.now() - pendingSinceRef.current;
      wait = Math.max(0, Math.min(delayMs, maxWaitMs - elapsed));
    }
    const handle = window.setTimeout(() => {
      pendingSinceRef.current = null;
      setDebounced(value);
    }, wait);
    return () => window.clearTimeout(handle);
  }, [value, debounced, delayMs, maxWaitMs]);

  return debounced;
}
