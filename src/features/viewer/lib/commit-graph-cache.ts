// SPDX-License-Identifier: MIT
import type { SourceCommitGraphEntry } from "@/features/git-sync/lib/types";
import { useViewerStore } from "@/shared/stores/viewer-store";

/**
 * SLW-WD-9 (RD-6): a bounded module-level LRU for the accumulated commit-graph
 * pages, keyed by `wsId` and tagged with the invalidation `nonce`.
 *
 * Two properties, by construction:
 * - **Correct memory bound:** capacity-capped LRU — many open tiles keep memory
 *   flat no matter what (the cap is the backstop even if a lifecycle eviction is
 *   ever missed).
 * - **Right layering:** the cache lives in `features/viewer` and is evicted from
 *   the overlay's OWN lifecycle (`evictCommitGraph` on unmount) — NOT from the
 *   store teardown (that would be a `shared → features` dependency-direction
 *   violation). The heavy computed pages stay out of the Zustand store too, so a
 *   graph refetch never churns store subscribers.
 *
 * A `nonce` mismatch is a MISS (RD-5): a bumped nonce means the ref set changed,
 * so the accumulated older pages are discarded and paging restarts from the tip.
 */

export interface CachedCommitGraph {
  /** The `worktreeChangedPath[wsId].nonce` this snapshot was fetched under. */
  nonce: number;
  /** Accumulated commits across the pages fetched so far (newest → oldest). */
  entries: SourceCommitGraphEntry[];
  /** The sha to resume AFTER for the next page, or `null` when exhausted. */
  cursor: string | null;
  /** Whether another older page may exist (last page was full). */
  hasMore: boolean;
}

/** Max distinct workspaces cached at once — a small handful, LRU-evicted. */
const CACHE_CAP = 8;

// A Map preserves insertion order, so it doubles as the LRU list: the oldest
// key is `keys().next()`, and re-inserting a key moves it to most-recent.
const cache = new Map<string, CachedCommitGraph>();

/**
 * Return the cached graph for `wsId` IFF it was fetched under the same `nonce`
 * (else a miss → the caller refetches from the tip). A hit is touched to
 * most-recently-used.
 */
export function getCommitGraph(wsId: string, nonce: number): CachedCommitGraph | null {
  const hit = cache.get(wsId);
  if (!hit || hit.nonce !== nonce) return null;
  cache.delete(wsId);
  cache.set(wsId, hit); // touch → MRU
  return hit;
}

/** Write-through the accumulated pages for `wsId`, evicting the LRU beyond the cap. */
export function setCommitGraph(wsId: string, value: CachedCommitGraph): void {
  cache.delete(wsId);
  cache.set(wsId, value);
  while (cache.size > CACHE_CAP) {
    const oldest = cache.keys().next().value as string | undefined;
    if (oldest === undefined) break;
    cache.delete(oldest);
  }
}

/** Drop a workspace's cached graph — driven by the workspace-close subscription below. */
export function evictCommitGraph(wsId: string): void {
  cache.delete(wsId);
}

/** Test-only: clear the whole cache between cases. */
export function __resetCommitGraphCache(): void {
  cache.clear();
}

/**
 * SLW-WD-9 (RD-6): evict a workspace's cached graph when it is CLOSED — i.e. when
 * it leaves the workspace list — NOT when its overlay unmounts. The overlay only
 * renders for the focused tile, so it unmounts on every focus SWITCH too; an
 * unmount-based evict would therefore defeat the refocus-serves-cache contract
 * (AC-6). Driving eviction off the workspace set (which shrinks only on real
 * close) is both prompt AND correct, and this features→shared store subscription
 * keeps the dependency arrow right (the store never reaches into this feature).
 *
 * Cheap: it early-returns on the many store mutations that don't touch
 * `workspaces` (an O(1) reference check); when the reference does change (an
 * add/remove OR a plain status update that re-derives the list) it re-flattens a
 * small handful of ids and evicts only those that actually left — so a status
 * churn costs a tiny walk and evicts nothing.
 */
let lastWorkspacesRef: unknown = null;
let knownWsIds = new Set<string>();
useViewerStore.subscribe((state) => {
  if (state.workspaces === lastWorkspacesRef) return;
  lastWorkspacesRef = state.workspaces;
  const current = new Set<string>();
  for (const list of Object.values(state.workspaces)) for (const w of list) current.add(w.id);
  for (const id of knownWsIds) if (!current.has(id)) evictCommitGraph(id);
  knownWsIds = current;
});
