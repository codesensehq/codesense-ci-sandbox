// SPDX-License-Identifier: MIT
import { create } from "zustand";

import { setActiveTeamForWave, type SetActiveTeamForWaveParams } from "@/tauri/commands";

/**
 * The team the backend's active-team state holds, as far as this window knows. The
 * backend resolves every active-team read (Activity's board, the weft ledger, …) from
 * that state, which only `set_active_team_for_wave` commits — so on a fresh profile
 * those reads fail until it has run. `epoch` bumps on every commit, so a surface whose
 * first read failed can re-read the moment the backend has a team.
 */
interface BackendActiveTeamState {
  /** `org:team` of the last commit, or `null` before the first. */
  teamKey: string | null;
  epoch: number;
}

export const useBackendActiveTeamStore = create<BackendActiveTeamState>(() => ({
  teamKey: null,
  epoch: 0,
}));

/** Record that the backend now holds `orgSlug:team` as its active team. */
export function markBackendActiveTeam(orgSlug: string, team: string): void {
  useBackendActiveTeamStore.setState((s) => ({
    teamKey: `${orgSlug}:${team}`,
    epoch: s.epoch + 1,
  }));
}

let commitSeq = 0;
/** The previous commit's IPC; each commit is sent only once it has settled. */
let commitChain: Promise<unknown> = Promise.resolve();

/** Commit the active team to the backend (`set_active_team_for_wave`), then announce it.
 *  Commits reach the backend one at a time, in call order, so an earlier one (e.g. a
 *  switch-away that isn't awaited) can never land after — and override — a later one.
 *  Only the latest commit announces: an earlier one answering late never names a team
 *  the backend no longer holds. */
export async function commitActiveTeamForWave(params: SetActiveTeamForWaveParams): Promise<void> {
  const mine = ++commitSeq;
  const sent = commitChain.catch(() => undefined).then(() => setActiveTeamForWave(params));
  commitChain = sent;
  await sent;
  if (mine === commitSeq) markBackendActiveTeam(params.orgSlug, params.team);
}
