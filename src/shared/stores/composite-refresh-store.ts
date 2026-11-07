// SPDX-License-Identifier: MIT
import { create } from "zustand";

/**
 * SLW-NC-5 — live-refresh signal for the source-repo composite overlay.
 *
 * SW-XS-15: `useAgentContext` memoizes the picked agent's want set and only
 * re-resolves when its own inputs (agent / repo / an agent edit) change — so an
 * EXTERNAL layer-store change (install, reverse-sync, an agent doc write, a raw
 * filesystem write) leaves the nav overlay + shadow badge stale until a restart.
 * This store is the one-way bridge from the singleton wave/watcher listener
 * (`useCompositeOverlayRefresh`, mounted once in `App`) to the per-team
 * `useAgentContext`: bumping a team's nonce re-runs that team's resolve.
 *
 * Keyed by `teamKey` (not source-repo slug): only the ACTIVE team's overlay
 * resolves (NC-1 scoping) and the wave payload can't name which source repo went
 * stale (NC-5 Finding 2), so per-team is the granularity we can act on. The
 * consuming hook is already selection-scoped, so a per-team bump re-resolves
 * exactly the one selected repo (AC-3).
 */
interface CompositeRefreshState {
  /** `teamKey` → monotonic bump counter. Absent key reads as 0. */
  nonces: Record<string, number>;
  /** Invalidate the given team's cached composite overlay. */
  bumpCompositeRefresh: (teamKey: string) => void;
}

export const useCompositeRefreshStore = create<CompositeRefreshState>((set) => ({
  nonces: {},
  bumpCompositeRefresh: (teamKey) =>
    set((s) => ({ nonces: { ...s.nonces, [teamKey]: (s.nonces[teamKey] ?? 0) + 1 } })),
}));
