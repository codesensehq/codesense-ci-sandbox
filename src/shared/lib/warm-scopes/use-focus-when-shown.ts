// SPDX-License-Identifier: MIT
import { useLayoutEffect, useRef } from "react";
import type { ScopeKey } from "./tiers";
import { useScopeActive } from "./use-scope-tier";

/**
 * Runs `refocus` when the scope goes back to active, for a component whose mount effect takes
 * keyboard focus. A warm team stays mounted, so that effect never runs again on return; this is
 * what the remount used to do. Not on first mount (the mount effect covers it), and not while the
 * component is disabled. `refocus` should focus with `preventScroll`.
 */
export function useFocusWhenShown(
  scope: ScopeKey | null | undefined,
  enabled: boolean,
  refocus: () => void,
): void {
  const active = useScopeActive(scope);
  const wasActive = useRef(active);
  const latest = useRef(refocus);
  useLayoutEffect(() => {
    latest.current = refocus;
  });
  useLayoutEffect(() => {
    const returned = active && !wasActive.current;
    wasActive.current = active;
    if (returned && enabled) latest.current();
  }, [active, enabled]);
}
