// SPDX-License-Identifier: MIT
import { useCallback, useMemo, useState } from "react";

/**
 * Manages the common `useState<T | null>(null)` + `open = target !== null`
 * pattern for target-based dialogs and confirmation flows.
 *
 * Returns a memoized object so callers that close over the result (e.g.,
 * `useCallback` deps) get a stable reference until `target` actually
 * changes — keeps `react-hooks/exhaustive-deps` honest without forcing
 * downstream callbacks to re-create on every parent render.
 */
export function useDialogTarget<T>() {
  const [target, setTarget] = useState<T | null>(null);
  const clear = useCallback(() => setTarget(null), []);
  return useMemo(
    () =>
      ({
        target,
        open: target !== null,
        show: setTarget,
        clear,
      }) as const,
    [target, clear],
  );
}
