// SPDX-License-Identifier: MIT
import { create } from "zustand";

/**
 * SW-SB-11: per-team monotonic nonces that lifecycle writes (create / close /
 * promote / delete a sprint) bump to force that team's `useSprintRegistry` to
 * re-load. SB-3's hook only re-runs on `teamKey`/`orgRoot` change, so the write
 * side needs an explicit signal for the new/closed sprint to appear.
 *
 * Keyed by `teamKey` so a write in one team does NOT re-fetch every OTHER
 * mounted team's registry — only the team that changed reloads. Kept as a tiny
 * dedicated store (mirroring `file-refresh-store`) so an action running OUTSIDE
 * React — the orchestrator — can trigger a reload via `getState().bump(teamKey)`.
 */
interface SprintRegistryRefreshState {
  nonces: Record<string, number>;
  bump: (teamKey: string) => void;
}

export const useSprintRegistryRefresh = create<SprintRegistryRefreshState>((set) => ({
  nonces: {},
  bump: (teamKey) =>
    set((s) => ({ nonces: { ...s.nonces, [teamKey]: (s.nonces[teamKey] ?? 0) + 1 } })),
}));
