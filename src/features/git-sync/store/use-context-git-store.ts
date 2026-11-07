// SPDX-License-Identifier: MIT
import { useViewerStore } from "@/shared/stores/viewer-store";
import { create } from "zustand";

import type { ContextRepoStatus } from "@/tauri/commands";

import type { NavChange } from "../lib/nav-changes";

/** One team's view of its context repo: what changed, keyed for the nav rows, and the
 *  repo's branch/sync status. Written by the one source hook the active nav mounts. */
export interface ContextGitSnapshot {
  orgRoot: string | null;
  error?: string | null;
  prefix?: string | null;
  source?: import("@/shared/lib/nav-lens").NavSource;
  cloneRoot?: string | null;
  cloneStatus?: ContextRepoStatus | null;
  sourceNote?: string | null;
  changeRevision?: number;
  changes: NavChange[];
  byPath: ReadonlyMap<string, NavChange>;
  status: ContextRepoStatus | null;
  /** True until the first changed-file read lands. */
  loading: boolean;
  /** Bumped each time git moved in a way History shows (HEAD, a ref, a fetch). */
  historyMoves: number;
}

interface ContextGitState {
  byTeam: Record<string, ContextGitSnapshot>;
  put: (teamKey: string, snapshot: ContextGitSnapshot) => void;
}

export const EMPTY_CONTEXT_GIT: ContextGitSnapshot = {
  orgRoot: null,
  changes: [],
  byPath: new Map(),
  status: null,
  loading: true,
  historyMoves: 0,
};

export const useContextGitStore = create<ContextGitState>((set) => ({
  byTeam: {},
  put: (teamKey, snapshot) =>
    set((s) => ({
      byTeam: { ...s.byTeam, [teamKey]: snapshot },
    })),
}));

export function useContextGit(teamKey: string): ContextGitSnapshot {
  const source = useViewerStore((s) => s.navLens[teamKey]?.source);
  const kind = source?.kind ?? "clone";
  return useContextGitStore((s) => {
    const snapshot = s.byTeam[teamKey];
    return snapshot &&
      (snapshot.source?.kind ?? "clone") === kind &&
      (source?.kind !== "worktree" ||
        (snapshot.source?.kind === "worktree" && source.id === snapshot.source.id))
      ? snapshot
      : EMPTY_CONTEXT_GIT;
  });
}

/** The change a nav path carries, if any (a scalar read, so only that row re-renders). */
export function useNavChange(teamKey: string, absPath: string): NavChange | undefined {
  return useContextGitStore((s) => s.byTeam[teamKey]?.byPath.get(absPath));
}

export function resetContextGitStoreForTests(): void {
  useContextGitStore.setState({ byTeam: {} });
}
