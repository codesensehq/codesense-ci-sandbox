// SPDX-License-Identifier: MIT
/**
 * SF-3 Bug B: per-burst single-bump fan-out for context-file-change debounce
 * flushes (Option B design — Set-aware store contract).
 *
 * Pre-fix: when a 300ms window collapsed N>1 distinct file paths, `App.tsx`
 * emitted ONE broad bump with `changedFilePath = undefined`. The store
 * widened that to `null`, which caused every keep-alive `MarkdownViewer`
 * (up to `VIEWER_PERF.keepAliveMaxViewers = 10`) to bypass the Layer 2
 * fast-path and fire a fresh `readFile` IPC — ~30 IPCs/sec during status-
 * transition / cargo-adjacent bursts.
 *
 * First attempt (per-path loop): emitted N bumps in a tight loop, intending
 * each viewer to see its own path in `changedFilePath`. **Failed in production
 * due to React 18 / Zustand batching** — all N store sets were batched into
 * one re-render with only the last path visible. Viewers for the first N-1
 * paths took the Layer 2 fast-path against stale `changedFilePath`, leaving
 * stale content displayed even though the files had changed on disk.
 * (Confirmed empirically via a probe test — see SF-3 Review Findings.)
 *
 * Post-fix (this design): emit ONE bump per burst with the full `Set<string>`
 * of changed paths. The store records the Set as `lastChangedContextFile`;
 * the viewer's Layer 2 check becomes `if (changedPaths instanceof Set &&
 * !changedPaths.has(filePath) && cached) { ... }`. Single store update →
 * one batched React render → all viewers see the same authoritative Set →
 * Layer 2 fast-path fires correctly for non-member viewers, readFile fires
 * for member viewers. No store-update amplification.
 *
 * Empty-set fallback (AC-9): if the debounce window flushed with no path
 * ever added (a watcher emit with no `file_path` field), emit one bump
 * with `undefined` so the store records `null` (broad refresh — viewers
 * can't skip). Log a warning so future regressions surface.
 */
export function flushRefreshDebounce(
  key: string,
  paths: Set<string> | undefined,
  bump: (key: string, changedPaths?: Set<string>) => void,
  log: { warn: (tag: string, msg: string) => void },
): void {
  if (!paths || paths.size === 0) {
    // AC-9: empty-set fallback — broad bump preserves the bump-all contract
    // for watcher emits without a file_path. Warn so a regression (paths
    // missing when they should be there) surfaces.
    log.warn(
      "refresh-debounce",
      `flush for "${key}" had no paths; emitting broad bump as fallback`,
    );
    bump(key, undefined);
    return;
  }

  // Single bump with the full Set — React batches one store update into
  // one render. Layer 2 check on the viewer side becomes a Set membership
  // test against `changedPaths`.
  bump(key, paths);
}
