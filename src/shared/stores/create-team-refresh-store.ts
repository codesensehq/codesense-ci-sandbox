// SPDX-License-Identifier: MIT
import { create } from "zustand";

/**
 * SW-GT-3 · Factory for a per-team monotonic refresh-nonce store. The repo has
 * several byte-identical variants of this (`sprint-registry-refresh-store`,
 * `composite-refresh-store`, `file-refresh-store`) — a write-side action bumps a
 * team's nonce so that team's async loader hook (which only re-runs on
 * `teamKey`/`orgRoot` change) re-loads in place.
 *
 * Keyed by `teamKey` so a write in one team never re-fetches every OTHER mounted
 * team's data. A plain object nonce (absent key reads as 0) keeps `bump` callable
 * from OUTSIDE React (e.g. an orchestrator) via `getState().bump(teamKey)`.
 *
 * Scope-guard: this factory backs the NEW `status-taxonomy-refresh-store` only.
 * The existing hand-rolled variants carry their own stories' tests and are left
 * untouched here; future single-purpose refresh stores should adopt this factory.
 */
export interface TeamRefreshStore {
  /** `teamKey` → monotonic bump counter. Absent key reads as 0. */
  nonces: Record<string, number>;
  /** Invalidate the given team's cached load. */
  bump: (teamKey: string) => void;
}

export function createTeamRefreshStore() {
  return create<TeamRefreshStore>((set) => ({
    nonces: {},
    bump: (teamKey) =>
      set((s) => ({ nonces: { ...s.nonces, [teamKey]: (s.nonces[teamKey] ?? 0) + 1 } })),
  }));
}
