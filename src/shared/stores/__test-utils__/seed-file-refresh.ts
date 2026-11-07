// SPDX-License-Identifier: MIT
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";

/**
 * Test helper: seed `useFileRefreshStore` slices from a single call.
 *
 * Replaces the 25+ pre-existing `useSyncStore.setState({ fileRefreshKeys, ... })`
 * seeds that grew organically across SF-3, F-26, NB-5, and PTS-5 test suites.
 *
 * Pass undefined (or omit) to skip seeding that slice — it keeps its current
 * value. Pass `{}` to explicitly reset that slice to empty.
 *
 * Examples:
 *   seedFileRefresh()                          // reset ALL slices to {} (default)
 *   seedFileRefresh({ "/r1": 1 })              // set keys; leave the others alone
 *   seedFileRefresh(undefined, {...})          // set lastChangedContextFile; leave others
 *   seedFileRefresh(undefined, undefined, {})  // set contentRefreshKeys (SR-9); leave others
 */
export function seedFileRefresh(
  fileRefreshKeys?: Record<string, number>,
  lastChangedContextFile?: Record<string, Set<string> | null>,
  contentRefreshKeys?: Record<string, number>,
): void {
  // No-args call (literally zero arguments) resets all slices to empty.
  // Iter-2 review: check `arguments.length`, not `=== undefined` on both
  // params — otherwise `seedFileRefresh(undefined)` (explicit single
  // undefined) would silently full-reset, contradicting the docstring's
  // "Pass undefined ... to skip seeding that slice" contract.
  if (arguments.length === 0) {
    useFileRefreshStore.setState({
      fileRefreshKeys: {},
      lastChangedContextFile: {},
      contentRefreshKeys: {},
    });
    return;
  }
  // Targeted update: only set the slices the caller explicitly passed,
  // preserving whichever they omitted. Zustand merges shallow.
  const update: Partial<{
    fileRefreshKeys: Record<string, number>;
    lastChangedContextFile: Record<string, Set<string> | null>;
    contentRefreshKeys: Record<string, number>;
  }> = {};
  if (fileRefreshKeys !== undefined) update.fileRefreshKeys = fileRefreshKeys;
  if (lastChangedContextFile !== undefined) update.lastChangedContextFile = lastChangedContextFile;
  if (contentRefreshKeys !== undefined) update.contentRefreshKeys = contentRefreshKeys;
  // No-op if all args were explicit undefined (caller error, but
  // defensively safe — preserves all slices).
  if (Object.keys(update).length > 0) {
    useFileRefreshStore.setState(update);
  }
}
