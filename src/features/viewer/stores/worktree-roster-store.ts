// SPDX-License-Identifier: MIT
import { create } from "zustand";

import { type WorktreeRow, listManagedWorktrees } from "@/tauri/commands";

export interface WorktreeRosterStore {
  /** `null` = not yet loaded (distinct from `[]` = "no worktrees"). */
  rows: WorktreeRow[] | null;
  /** The last fetch error, or `null`. */
  error: string | null;
  /** Re-enumerate the worktree roster (`list_managed_worktrees`, off the UI thread).
   *  Gen-guarded store update; returns the fetched roster, or null on error. */
  reload: () => Promise<WorktreeRow[] | null>;
}

/**
 * SW-WU-9: the ONE app-root worktree roster, mirroring SW-AM-9's agent roster. The
 * events are the app-root listener's job ({@link useWorktreeRosterListener}); this store
 * just holds the fetched array + the gen-guarded refetch, which the context repo's changed-
 * files read uses through {@link useLiveCliSession}.
 *
 * The `worktree-changed` event is a payloadless nudge, so `reload` re-snapshots (event → refetch, not event → patch);
 * the listing is small and the fetch is off-thread, so a delta model would be premature.
 */

// A process-wide generation guard: the store is a singleton, so concurrent reloads
// resolve last-writer-wins — a superseded fetch's `.then` is dropped. Module scope, not store
// state, so bumping it never triggers a re-render.
let generation = 0;

export const useWorktreeRosterStore = create<WorktreeRosterStore>((set) => ({
  rows: null,
  error: null,
  reload: () => {
    const gen = ++generation;
    return listManagedWorktrees()
      .then(({ rows }) => {
        if (gen !== generation) return rows;
        // The `reaped` count (gone-worktree records dropped by the listing) is
        // intentionally ignored here; this store only holds the list.
        set({ rows, error: null });
        return rows;
      })
      .catch((e) => {
        if (gen === generation) set({ error: e instanceof Error ? e.message : String(e) });
        return null;
      });
  },
}));

/**
 * Reactive selector: whether a CLI session that could be writing to `path` is
 * currently live — the signal the context repo's changed-files read uses to space its
 * full diffs out while an agent may be writing. Two cases:
 *
 *  - `path` matches a roster worktree row → that row's own `liveCliSession` (a
 *    worktree sync view suspends on ITS run).
 *  - `path` is not a known worktree (the org-root / context-repo content path,
 *    the root-workspace sync view) → true if ANY roster row is live. A worktree
 *    agent's writes reverse-sync into the context repo and bump the org-root
 *    refreshKey, so the context view slows down whenever any agent CLI run is
 *    live — this is the exact view the storm hammers.
 *
 * `false` when the roster is empty / not yet loaded.
 */
export function useLiveCliSession(path: string): boolean {
  return useWorktreeRosterStore((s) => {
    const rows = s.rows;
    if (!rows || rows.length === 0) return false;
    const own = rows.find((r) => r.path === path);
    if (own) return own.liveCliSession;
    // Not a known worktree (org-root/content path) → any live session counts.
    return rows.some((r) => r.liveCliSession);
  });
}
