// SPDX-License-Identifier: MIT
import { fileChangeAffects } from "@/shared/lib/file-change-affects";
import { useEffect, useMemo, useRef, useState } from "react";
import { getStoryDetails, type StoryDetails } from "@/tauri/commands";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";

/**
 * R-14: On-demand fetch for body-extracted story details (H1 title +
 * `## Story` section). The scanner stopped producing these on `StoryRef`
 * — consumers that need rich title or storyText preview call this hook.
 *
 * Architecture:
 * - **Module-level cache.** Shared across all hook instances so the same
 *   file isn't re-fetched per-mount. Bounded by LRU eviction (default 250
 *   entries — enough to cover codesense's expanded post-R-14 scope plus
 *   headroom for other teams).
 * - **Watcher-driven invalidation.** Subscribes to
 *   `file-refresh-store.lastChangedContextFile[repoPath]`. When a file change
 *   notifier matches the hook's `filePath`, the cache entry is dropped
 *   and a refetch fires.
 * - **No abort on unmount.** Pending fetches complete and populate the
 *   shared cache, so the next mount sees results instantly.
 *
 * @param filePath Absolute path to the story file. Pass `undefined` to
 *   skip fetching (e.g. before a file is selected).
 * @param headings SW-EW-32: the configured ordered summary-section headings
 *   (first match wins). Part of the cache key, so changing the setting
 *   re-resolves the summary without a restart or file touch.
 * @returns `{ data, loading }` — `data` is `undefined` while loading and
 *   on error; consumers should treat absence as "not available" and fall
 *   back to filename / no-subtitle UX.
 */
// SW-EW-32: the cache is keyed by `filePath` + the configured summary
// headings, so changing the headings setting misses (→ refetch) instead of
// returning a summary keyed off the old list. NUL separates the components —
// it can't appear in a file path and won't appear in a heading.
const cache = new Map<string, StoryDetails>();
const inFlight = new Map<string, Promise<StoryDetails>>();
const touchedAt = new Map<string, number>();
let tick = 0;
const MAX_ENTRIES = 250;
const KEY_SEP = "\u0000";

function cacheKeyFor(filePath: string, headings: readonly string[]): string {
  return filePath + KEY_SEP + headings.join(KEY_SEP);
}

function pathFromCacheKey(key: string): string {
  const i = key.indexOf(KEY_SEP);
  return i === -1 ? key : key.slice(0, i);
}

function readCached(key: string): StoryDetails | undefined {
  const entry = cache.get(key);
  if (entry) touchedAt.set(key, ++tick);
  return entry;
}

function storeAndEvict(key: string, value: StoryDetails): void {
  cache.set(key, value);
  touchedAt.set(key, ++tick);
  if (cache.size <= MAX_ENTRIES) return;
  // Evict the entry with the smallest touchedAt.
  let oldestKey: string | undefined;
  let oldestTick = Infinity;
  for (const [k, t] of touchedAt) {
    if (t < oldestTick) {
      oldestTick = t;
      oldestKey = k;
    }
  }
  if (oldestKey !== undefined) {
    cache.delete(oldestKey);
    touchedAt.delete(oldestKey);
  }
}

function invalidate(key: string): void {
  cache.delete(key);
  touchedAt.delete(key);
  inFlight.delete(key);
}

/**
 * SW-EW-32: a watcher change carries a *file path*, not a cache key. Drop every
 * heading-variant entry for that path so the next fetch reflects disk.
 */
function invalidateByPath(filePath: string): void {
  for (const key of [...cache.keys(), ...inFlight.keys()]) {
    if (pathFromCacheKey(key) === filePath) invalidate(key);
  }
}

/**
 * Strip HTML comments (CodeSense section markers like `<!-- cs:evidence-section -->`
 * included) from an extracted summary and tidy the spaces they leave. Summaries
 * are shown as plain text on cards, where a raw marker is noise. `null` when nothing is left.
 */
export function cleanStorySummary(text: string | null | undefined): string | null {
  if (text == null) return null;
  if (!text.includes("<!--")) return text;
  const cleaned = text
    .replace(/[ \t]*<!--[\s\S]*?(?:-->|$)[ \t]*/g, " ")
    .replace(/[ \t]{2,}/g, " ")
    .trim();
  return cleaned || null;
}

async function fetchStoryDetails(
  key: string,
  filePath: string,
  headings: readonly string[],
): Promise<StoryDetails> {
  const existing = inFlight.get(key);
  if (existing) return existing;
  const promise = getStoryDetails(filePath, headings)
    .then((raw) => {
      const value: StoryDetails = {
        ...raw,
        title: cleanStorySummary(raw.title),
        storyText: cleanStorySummary(raw.storyText),
      };
      storeAndEvict(key, value);
      inFlight.delete(key);
      return value;
    })
    .catch((err) => {
      inFlight.delete(key);
      throw err;
    });
  inFlight.set(key, promise);
  return promise;
}

/** Test-only — clear the module-level cache between tests. */
export function _resetStoryDetailsCacheForTests(): void {
  cache.clear();
  touchedAt.clear();
  inFlight.clear();
  tick = 0;
}

/**
 * Test-only — seed the module-level cache directly without going through
 * the React/IPC lifecycle. Used by the LRU eviction test to populate
 * thousands of entries quickly. Production code MUST NOT call this.
 * SW-EW-32: keys on the same `(filePath, headings)` composite the hook uses.
 */
export function _seedStoryDetailsCacheForTests(
  filePath: string,
  headings: readonly string[],
  value: StoryDetails,
): void {
  storeAndEvict(cacheKeyFor(filePath, headings), value);
}

/**
 * Test-only — read the current cache size. The LRU test asserts the cache
 * stays bounded after a populate-past-the-limit pass.
 */
export function _storyDetailsCacheSizeForTests(): number {
  return cache.size;
}

export interface UseStoryDetailsResult {
  data: StoryDetails | undefined;
  loading: boolean;
}

export function useStoryDetails(
  filePath: string | undefined,
  headings: readonly string[],
): UseStoryDetailsResult {
  // The cache key folds in the headings list, so the effect keys off this
  // string (not the array reference) — a new-but-equal headings array won't
  // re-run the effect, and a real settings change will.
  const cacheKey = filePath ? cacheKeyFor(filePath, headings) : undefined;

  const [data, setData] = useState<StoryDetails | undefined>(() =>
    cacheKey ? readCached(cacheKey) : undefined,
  );
  const [loading, setLoading] = useState<boolean>(false);
  const mountedRef = useRef(true);
  // Tracks the filePath the most recent effect ran with, so we can
  // distinguish "filePath transition" (clear stale data from prior story)
  // from "watcher-triggered refetch for the same path" (preserve last-
  // known-good data while the refresh is in flight).
  const lastFilePathRef = useRef<string | undefined>(undefined);

  // Subscribe to lastChangedContextFile globally. Set-membership match
  // catches multi-file watcher events from any repo (paths are unique
  // across the tree, so cross-repo over-fire is impossible here). SF-3
  // changed the value shape from `string | null` to `Set<string> | null`:
  // a single bump per debounce burst carries the full Set of changed
  // paths, so a multi-file burst correctly invalidates EACH affected
  // hook (no longer leaves them stale as the pre-SF-3 NB-5 trade-off
  // did). A `null` value remains the broad-refresh sentinel and is
  // intentionally NOT treated as an invalidate signal here — the
  // alternative (invalidate on any null) would cascade across every
  // hook in every repo whenever any team's watcher emits without a
  // file_path. Now genuinely rare; the only known emit source is the
  // empty-set debounce fallback (App.tsx → flushRefreshDebounce).
  const lastChangedFiles = useFileRefreshStore((s) => s.lastChangedContextFile);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  // Single combined effect: invalidate-on-watcher + fetch-on-mount/path.
  // The invalidation path drops the cache entry then re-runs the fetch
  // logic in the same effect so the consumer sees a refresh.
  // Set-state-in-effect throughout this body is intentional — `data` is
  // populated from an async file-system fetch (not derived) and the
  // synchronous resets/clears below are required to avoid stale-data
  // flashes between consumers' renders.
  useEffect(() => {
    const prevFilePath = lastFilePathRef.current;
    lastFilePathRef.current = filePath;

    if (!filePath || !cacheKey) {
      setData(undefined);
      setLoading(false);
      return;
    }
    // Watcher Set-membership invalidation. See module-level comment above
    // about why we don't fall through on `null`. SW-EW-32: a watcher event
    // carries the file path, so invalidate every heading-variant for it.
    for (const changed of Object.values(lastChangedFiles)) {
      if (changed instanceof Set && fileChangeAffects(changed, filePath)) {
        invalidateByPath(filePath);
        break;
      }
    }
    const cached = readCached(cacheKey);
    if (cached) {
      setData(cached);
      setLoading(false);
      return;
    }
    // Cache miss — fetch.
    // P7: preserve `data` on same-path refetch (watcher invalidate cycle)
    // so the consumer doesn't see a flash to empty preview. On a real
    // filePath transition, clear `data` so the prior story's preview
    // doesn't leak into the new card. SW-EW-32: a headings-only change keeps
    // the same filePath, so the prior summary stays visible (stale-while-
    // revalidate) until the new one resolves — no blank flash.
    if (prevFilePath !== filePath) {
      setData(undefined);
    }
    setLoading(true);
    // P6: cleanup-based cancellation. The effect's cleanup fires before
    // the next effect run (or on unmount). A stale fetch promise that
    // resolves after cancellation is dropped here, not in the consumer.
    let cancelled = false;
    fetchStoryDetails(cacheKey, filePath, headings)
      .then((value) => {
        if (cancelled || !mountedRef.current) return;
        setData(value);
        setLoading(false);
      })
      .catch(() => {
        if (cancelled || !mountedRef.current) return;
        // P7: preserve last-known-good `data` on transient errors. A
        // failed refetch shouldn't blank a previously-visible preview;
        // the consumer treats `loading=false` + existing `data` as stable.
        setLoading(false);
      });
    return () => {
      cancelled = true;
    };
    // `headings` is covered by `cacheKey` (which embeds `headingsKey`); listing
    // it explicitly would re-run on a new-but-equal array reference.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [filePath, cacheKey, lastChangedFiles]);

  return useMemo(() => ({ data, loading }), [data, loading]);
}
