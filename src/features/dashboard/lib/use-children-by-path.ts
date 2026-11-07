// SPDX-License-Identifier: MIT
import { useEffect, useRef, useState } from "react";

import { listDirectory, type FileEntry } from "@/tauri/commands";
import { pruneMapByKeys } from "@/shared/lib/prune-map-by-keys";
import { fileEntryMapsEqual } from "./file-entry-map-equal";

export interface ChildrenByPathResult {
  /** Children per currently-expanded absolute path. An errored path is ABSENT
   *  (consumers treat `undefined` as `[]`); read `erroredPaths` to distinguish. */
  childrenByPath: Map<string, FileEntry[]>;
  /** Absolute paths whose most recent `list_directory` rejected. */
  erroredPaths: Set<string>;
}

interface UseChildrenByPathArgs {
  /** Absolute paths of directories currently expanded. */
  expandedPaths: Set<string>;
  /** Bump to force a re-fetch of every expanded path. */
  refreshKey?: number;
  /**
   * Pure per-directory transform applied to a successful `list_directory`
   * result (filter/sort). Held in a ref so its identity is never a fetch
   * trigger — pass a module-level or memoized function.
   */
  transform: (entries: FileEntry[], absPath: string) => FileEntry[];
  /**
   * What to store in `childrenByPath` for a path whose fetch ERRORED. Default:
   * leave it absent (and record it in `erroredPaths`) — the worktree tree reads
   * `erroredPaths` to show an inline error row. Pass a stable `[]` to keep the
   * legacy "errored dir reads as empty" contract (the pipeline nav's behavior).
   */
  errorValue?: FileEntry[];
  /**
   * The directory lister. Default: the local `list_directory`. A remote machine's
   * tree passes its own (SW-RM-UI-1) — the same lazy/prune/stale machinery either
   * way. Contract-stable like `transform` (module-level or memoized).
   */
  list?: (dirPath: string) => Promise<FileEntry[]>;
}

/**
 * Shared lazy per-directory children fetch behind both `useGenericChildrenByPath`
 * (dashboard nav, pipeline filter) and `useWorktreeChildrenByPath` (worktree
 * browser, identity). Owns the delta-fetch (only newly-expanded / stale-key
 * paths hit IPC), the synchronous collapse prune, the in-flight stale guard, and
 * reference-stable Map identity. Consumers differ only in `transform` and in
 * whether they read `erroredPaths`.
 *
 * An errored fetch marks the path resolved-at-key (so it does NOT auto-retry
 * until `refreshKey` bumps) and records it in `erroredPaths`; it is left absent
 * from `childrenByPath` (equivalent to `[]` for `?? []` consumers).
 */
export function useChildrenByPath({
  expandedPaths,
  refreshKey,
  transform,
  errorValue,
  list = listDirectory,
}: UseChildrenByPathArgs): ChildrenByPathResult {
  const [childrenByPath, setChildrenByPath] = useState<Map<string, FileEntry[]>>(() => new Map());
  const [erroredPaths, setErroredPaths] = useState<Set<string>>(() => new Set());
  const resolvedAtKeyRef = useRef<Map<string, number>>(new Map());

  // `transform` / `errorValue` are contract-stable (both wrappers pass
  // module-level values), so they sit directly in the deps — a fresh identity
  // would (correctly) re-fetch, but never does in practice.
  useEffect(() => {
    let stale = false;
    const currentKey = refreshKey ?? 0;
    const resolvedAtKey = resolvedAtKeyRef.current;

    const toFetch: string[] = [];
    for (const absPath of expandedPaths) {
      if (resolvedAtKey.get(absPath) !== currentKey) toFetch.push(absPath);
    }

    // Prune tracker entries for paths no longer expanded before setState so the
    // updater stays pure (no ref mutation inside the reducer).
    for (const path of Array.from(resolvedAtKey.keys())) {
      if (!expandedPaths.has(path)) resolvedAtKey.delete(path);
    }

    // Synchronously prune children for collapsed paths so a collapse reflects
    // immediately; reference-equality bail-out makes it a no-op when nothing
    // was pruned. Set-state-in-effect is intentional — the map is partially
    // derived (expandedPaths) and partially side-effect-fed (async listDirectory
    // below), so it can't be a useMemo (mirrors the pre-refactor hook).
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setChildrenByPath((prev) => pruneMapByKeys(prev, expandedPaths));

    if (toFetch.length === 0) {
      return () => {
        stale = true;
      };
    }

    // `null` distinguishes an errored fetch from an empty directory.
    const fetches: Array<Promise<[string, FileEntry[] | null]>> = toFetch.map((absPath) =>
      list(absPath)
        .then((entries) => [absPath, entries] as [string, FileEntry[]])
        .catch(() => [absPath, null] as [string, FileEntry[] | null]),
    );

    Promise.all(fetches).then((results) => {
      if (stale) return;
      setChildrenByPath((prev) => {
        const next = new Map(prev);
        for (const [path, files] of results) {
          if (!expandedPaths.has(path)) continue;
          // Mark attempted (success OR error) — no auto-retry until refreshKey bumps.
          resolvedAtKey.set(path, currentKey);
          if (files != null) next.set(path, transform(files, path));
          else if (errorValue !== undefined) next.set(path, errorValue);
        }
        return fileEntryMapsEqual(prev, next) ? prev : next;
      });
      setErroredPaths((prev) => {
        const next = new Set(prev);
        let mutated = false;
        for (const [path, files] of results) {
          if (!expandedPaths.has(path)) continue;
          if (files == null && !next.has(path)) {
            next.add(path);
            mutated = true;
          } else if (files != null && next.has(path)) {
            next.delete(path);
            mutated = true;
          }
        }
        return mutated ? next : prev;
      });
    });

    return () => {
      stale = true;
    };
  }, [expandedPaths, refreshKey, transform, errorValue, list]);

  return { childrenByPath, erroredPaths };
}
