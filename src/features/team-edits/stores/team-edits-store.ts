// SPDX-License-Identifier: MIT
import { create } from "zustand";

import { getErrorMessage } from "@/shared/lib/error-message";
import { teamFileEditsStatus, type TeamFileEditsStatus } from "@/tauri/commands";

/** What to count now besides the workspaces on This Mac (always counted). */
export type CountTarget =
  | { kind: "local"; codeRepoPath: string }
  | { kind: "remote"; machineId: string; workspaceHandle: string };

interface TeamEdits {
  status: TeamFileEditsStatus | null;
  /** The last read failed (the last good status stays). */
  error: string | null;
  /** A read is in flight. */
  loading: boolean;
  /** Machine workspaces being counted now (`machineId:handle`). */
  counting: string[];
}

interface TeamEditsStore {
  byTeam: Record<string, TeamEdits>;
  /** Read the team's unsaved team edits; with `count`, count that workspace now. */
  refresh: (teamKey: string, count?: CountTarget) => Promise<void>;
  /** Read, then count each connected machine workspace whose last count is at least
   *  `minAgeMs` old (default a minute). */
  refreshAll: (teamKey: string, minAgeMs?: number) => Promise<void>;
}

/** A machine workspace is counted again at most this often by a broad refresh. */
export const MACHINE_RECOUNT_MIN_MS = 60_000;
const machineCountedAt = new Map<string, number>();

const EMPTY: TeamEdits = { status: null, error: null, loading: false, counting: [] };

/** `machineId:handle` of a machine workspace. */
export const remoteKey = (machineId: string, handle: string) => `${machineId}:${handle}`;

/** Requests are numbered per team so a slow read never overwrites a newer one. */
const latest = new Map<string, number>();

/**
 * SW-XS-27: the active team's unsaved team edits ("unsaved changes" for team files
 * edited inside workspaces). Reads only — the backend counts This Mac's
 * workspaces on every read and a machine workspace only when asked. Nothing here
 * polls: `useTeamEditsRefresh` reads on open, on focus, after an operation, after
 * an agent's turn, and on a click.
 */
export const useTeamEditsStore = create<TeamEditsStore>((set, get) => ({
  byTeam: {},
  refreshAll: async (teamKey, minAgeMs = MACHINE_RECOUNT_MIN_MS) => {
    await get().refresh(teamKey);
    const status = get().byTeam[teamKey]?.status;
    const now = Date.now();
    await Promise.all(
      (status?.machineWorkspaces ?? [])
        .filter((ws) => ws.machineOnline)
        .filter((ws) => {
          const key = remoteKey(ws.target.machineId, ws.target.workspaceHandle);
          const last = Math.max(ws.checkedAtMs ?? 0, machineCountedAt.get(key) ?? 0);
          if (now - last < minAgeMs) return false;
          machineCountedAt.set(key, now);
          return true;
        })
        .map((ws) => get().refresh(teamKey, ws.target)),
    );
  },
  refresh: async (teamKey, count) => {
    const ticket = (latest.get(teamKey) ?? 0) + 1;
    latest.set(teamKey, ticket);
    const countingKey =
      count?.kind === "remote" ? remoteKey(count.machineId, count.workspaceHandle) : null;
    set((s) => {
      const prev = s.byTeam[teamKey] ?? EMPTY;
      return {
        byTeam: {
          ...s.byTeam,
          [teamKey]: {
            ...prev,
            loading: true,
            counting: countingKey ? [...new Set([...prev.counting, countingKey])] : prev.counting,
          },
        },
      };
    });
    let next: Partial<TeamEdits>;
    try {
      next = { status: await teamFileEditsStatus(count), error: null };
    } catch (e) {
      next = { error: getErrorMessage(e) };
    }
    const stale = latest.get(teamKey) !== ticket;
    set((s) => {
      const prev = s.byTeam[teamKey] ?? EMPTY;
      const counting = countingKey ? prev.counting.filter((k) => k !== countingKey) : prev.counting;
      // A newer read answered first: this one's whole status is older, so it is
      // dropped (a machine count it did is picked up by the read below).
      if (stale) return { byTeam: { ...s.byTeam, [teamKey]: { ...prev, counting } } };
      return {
        byTeam: { ...s.byTeam, [teamKey]: { ...prev, ...next, loading: false, counting } },
      };
    });
    // The machine count landed in the backend's last counts; read them back.
    if (stale && countingKey && !next.error) await useTeamEditsStore.getState().refresh(teamKey);
  },
}));

export function selectTeamEdits(s: TeamEditsStore, teamKey: string): TeamEdits {
  return s.byTeam[teamKey] ?? EMPTY;
}
