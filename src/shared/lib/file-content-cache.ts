// SPDX-License-Identifier: MIT
import { VIEWER_PERF } from "./viewer-perf-config";

interface CacheEntry {
  content: string;
  refreshKey: number;
  touchedAt: number;
}

export interface FileContentCache {
  get(filePath: string): CacheEntry | undefined;
  set(filePath: string, content: string, refreshKey: number): void;
  invalidate(filePath: string): void;
  clear(): void;
}

/**
 * LRU cache of raw file text. Keyed by absolute `filePath`. Each entry
 * stores the `refreshKey` it was read at as a freshness token — callers
 * compare their current refreshKey against the stored one to decide
 * whether to trust the cached content. `refreshKey` is NOT part of the
 * key, so a stale entry is overwritten in place on the next `set`.
 *
 * Eviction: on `set`, if size exceeds `maxEntries`, drop the entry with
 * the smallest `touchedAt`. `get` also bumps `touchedAt`, so actively-
 * read entries stay resident.
 *
 * Production code imports the default instance `fileContentCache`.
 * Tests import the factory for isolated eviction tests with small caps.
 */
export function createFileContentCache(maxEntries: number): FileContentCache {
  const map = new Map<string, CacheEntry>();
  // Monotonic counter — unaffected by clock skew, trivially comparable.
  let tick = 0;

  return {
    get(filePath) {
      const entry = map.get(filePath);
      if (entry) entry.touchedAt = ++tick;
      return entry;
    },
    set(filePath, content, refreshKey) {
      const existing = map.get(filePath);
      if (existing && existing.content === content) {
        existing.refreshKey = refreshKey;
        existing.touchedAt = ++tick;
        return;
      }
      map.set(filePath, { content, refreshKey, touchedAt: ++tick });
      if (map.size <= maxEntries) return;

      let oldestKey: string | undefined;
      let oldestTouchedAt = Infinity;
      for (const [k, v] of map) {
        if (v.touchedAt < oldestTouchedAt) {
          oldestTouchedAt = v.touchedAt;
          oldestKey = k;
        }
      }
      if (oldestKey !== undefined) map.delete(oldestKey);
    },
    invalidate(filePath) {
      map.delete(filePath);
    },
    clear() {
      map.clear();
    },
  };
}

export const fileContentCache = createFileContentCache(VIEWER_PERF.fileCacheMaxEntries);
