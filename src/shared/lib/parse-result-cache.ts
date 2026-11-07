// SPDX-License-Identifier: MIT
import type { Block } from "./markdown-worker/types";

interface CacheEntry {
  blocks: Block[];
  touchedAt: number;
}

/**
 * 32-bit FNV-1a hash. Pure JS; sub-millisecond on a 100 KB markdown
 * file. Collision probability in a ~50-entry LRU over typical markdown
 * content is effectively zero — see story 32.6 "Content hash" section
 * for analysis. Swap to xxhash-wasm at this single call site if a
 * future need demands 64-bit entropy.
 */
function fnv1a(text: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

// `\x00` (NUL) is illegal in any filesystem path on every target OS, so it
// can never appear inside `filePath` and cannot collide on `invalidate`'s
// `startsWith(\`${filePath}\x00\`)` prefix check. (A visible delimiter like
// `|` is legal in POSIX/macOS filenames, so a path literally containing
// `|` could fool prefix-based invalidation — see Group 2 review.)
const KEY_DELIMITER = "\x00";

function cacheKey(filePath: string, content: string): string {
  return `${filePath}${KEY_DELIMITER}${fnv1a(content).toString(16)}`;
}

export interface ParseResultCache {
  /**
   * Fast path on viewer mount. Pass the file's current text; any byte
   * change flips the hash → cache miss. No mtime/refreshKey
   * plumbing needed (see story 32.6 T1 decision).
   */
  get(filePath: string, content: string): Block[] | undefined;
  set(filePath: string, content: string, blocks: Block[]): void;
  invalidate(filePath: string): void;
  clear(): void;
  stats(): { size: number; hits: number; misses: number };
}

/**
 * LRU of parsed `Block[]` keyed on `filePath + fnv1a(content)`. Mirrors
 * the shape of `file-content-cache.ts` — monotonic `tick` counter,
 * evict-oldest on overflow.
 *
 * Production code imports the default instance `parseResultCache`. Tests
 * import the factory for isolated behavior coverage.
 */
export function createParseResultCache(maxEntries: number): ParseResultCache {
  const map = new Map<string, CacheEntry>();
  let tick = 0;
  let hits = 0;
  let misses = 0;

  return {
    get(filePath, content) {
      const k = cacheKey(filePath, content);
      const entry = map.get(k);
      if (entry) {
        entry.touchedAt = ++tick;
        hits++;
        return entry.blocks;
      }
      misses++;
      return undefined;
    },
    set(filePath, content, blocks) {
      // Short-circuit on a degenerate cap so we don't allocate a Map
      // entry only to immediately evict it. A cap <= 0 is nonsensical
      // input but survives here without wasting work.
      if (maxEntries <= 0) return;
      const k = cacheKey(filePath, content);
      map.set(k, { blocks, touchedAt: ++tick });
      if (map.size <= maxEntries) return;
      let oldestKey: string | undefined;
      let oldest = Infinity;
      for (const [kk, v] of map) {
        if (v.touchedAt < oldest) {
          oldest = v.touchedAt;
          oldestKey = kk;
        }
      }
      if (oldestKey !== undefined) map.delete(oldestKey);
    },
    invalidate(filePath) {
      // Remove every entry whose key starts with `${filePath}${DELIMITER}`.
      // Cheap: entry count is capped + filePath lookups are infrequent.
      const prefix = `${filePath}${KEY_DELIMITER}`;
      for (const k of map.keys()) {
        if (k.startsWith(prefix)) map.delete(k);
      }
    },
    clear() {
      map.clear();
      hits = 0;
      misses = 0;
    },
    stats() {
      return { size: map.size, hits, misses };
    },
  };
}

/**
 * Default instance — cap of 50 entries. Typical parsed `Block[]` for a
 * 100 KB markdown file is a few hundred KB of in-memory objects, so 50
 * entries is roughly ~10-20 MB of cache — comfortably under desktop-app
 * memory budgets.
 */
export const parseResultCache = createParseResultCache(50);
