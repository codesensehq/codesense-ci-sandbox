// SPDX-License-Identifier: MIT
import { fileChangeAffects } from "@/shared/lib/file-change-affects";
import { useState } from "react";

/**
 * Gate a per-repo `refreshKey` down to a per-file "effective" key.
 *
 * A single `refreshKey` is bumped on any watcher tick across the whole repo,
 * so a consumer keyed on it (e.g. an iframe `src`) would react to unrelated
 * changes. This hook advances the returned key ONLY when `filePath` is in
 * `changedPaths` — or when `changedPaths` is null/absent, in which case it
 * falls back to advancing so a coarse invalidation can't strand stale
 * content. Otherwise it holds the prior value, keeping downstream output
 * stable so unrelated/hidden views never reload.
 *
 * Uses React's "adjust state during render" pattern (storing info from prior
 * renders) rather than an effect, so the gated key is correct on the same
 * commit it's consumed — no cascading re-render.
 *
 * Self-contained: a `filePath` change resets the gate to the current key so
 * a new file always loads fresh, without depending on the caller remounting
 * (e.g. a `key={filePath}`) to clear prior state.
 *
 * Introduced for SW-FX-5 (HTML viewer scroll preservation); see
 * `markdown-viewer.tsx`'s inline `changedPaths` skip for the sibling gate.
 */
export function useEffectiveRefreshKey(
  refreshKey: number | undefined,
  changedPaths: Set<string> | null | undefined,
  filePath: string | null,
): number | undefined {
  const [effectiveRefreshKey, setEffectiveRefreshKey] = useState(refreshKey);
  const [seenRefreshKey, setSeenRefreshKey] = useState(refreshKey);
  const [seenFilePath, setSeenFilePath] = useState(filePath);

  if (filePath !== seenFilePath) {
    // New file → load at the current key regardless of `changedPaths`.
    setSeenFilePath(filePath);
    setSeenRefreshKey(refreshKey);
    setEffectiveRefreshKey(refreshKey);
  } else if (refreshKey != null && refreshKey !== seenRefreshKey) {
    setSeenRefreshKey(refreshKey);
    const fileChanged = fileChangeAffects(changedPaths, filePath);
    if (fileChanged) {
      setEffectiveRefreshKey(refreshKey);
    }
  }

  return effectiveRefreshKey;
}
