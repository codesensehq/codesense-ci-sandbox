// SPDX-License-Identifier: MIT
import type { FileEntry } from "@/tauri/commands";

/**
 * Shallow equality for `Map<string, FileEntry[]>` results returned by
 * `useOrphanFilesByPath` and `useGenericChildrenByPath`. Two maps are equal
 * when they share the same key set AND each key's FileEntry array has the
 * same length and same entry paths in the same order. This is enough to
 * preserve memo identity across watcher ticks that re-fetch unchanged
 * directories — the lifted hooks fetch a fresh list every time and only the
 * `path` field matters for downstream rendering (name/extension/is_dir are
 * derived from path and don't change for the same path).
 */
export function fileEntryMapsEqual(
  a: Map<string, FileEntry[]>,
  b: Map<string, FileEntry[]>,
): boolean {
  if (a === b) return true;
  if (a.size !== b.size) return false;
  for (const [key, av] of a) {
    const bv = b.get(key);
    if (!bv || bv.length !== av.length) return false;
    for (let i = 0; i < av.length; i++) {
      if (av[i].path !== bv[i].path) return false;
    }
  }
  return true;
}
