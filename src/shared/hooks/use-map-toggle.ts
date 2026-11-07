// SPDX-License-Identifier: MIT
import { useCallback, useState, type Dispatch, type SetStateAction } from "react";

/** Map-backed open/close toggle. Stores only open (`true`) keys; close deletes.
 *
 *  `setOpen` short-circuits — returns the SAME Map when the value is unchanged
 *  — so unaffected memoized rows skip re-renders. That reference-equality
 *  bail-out is load-bearing for render perf, not cosmetic: the inline versions
 *  this centralizes (teams-card, pipeline-status-nav) hand-rolled it for
 *  exactly that reason.
 *
 *  Returns the raw `setMap` too (mirrors React's `useState` return) so a caller
 *  can run a referentially-stable prune (see `pruneMapByKeys`) from a GC effect
 *  without the hook needing to own the prune policy. */
export function useMapToggle<K>(): [
  Map<K, boolean>,
  (key: K, open: boolean) => void,
  Dispatch<SetStateAction<Map<K, boolean>>>,
] {
  const [map, setMap] = useState<Map<K, boolean>>(() => new Map());
  const setOpen = useCallback((key: K, open: boolean) => {
    setMap((prev) => {
      if ((prev.get(key) ?? false) === open) return prev;
      const next = new Map(prev);
      if (open) next.set(key, true);
      else next.delete(key);
      return next;
    });
  }, []);
  return [map, setOpen, setMap];
}
