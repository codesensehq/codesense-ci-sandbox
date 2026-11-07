// SPDX-License-Identifier: MIT
import { create } from "zustand";

import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";

/**
 * Each team's pipeline tree, as its team view last scanned it. The team view owns the one scan
 * per team and publishes the result here, so the shell's surfaces that show the active team (the
 * shared workspace tray's epic grouping, the epic repos overlay) read the same tree without a
 * second scan. Not persisted: a team view publishes again as soon as it mounts.
 */
interface TeamSurfaceState {
  trees: Record<string, DerivedPipelineTree | null | undefined>;
}

export const useTeamSurfaceStore = create<TeamSurfaceState>(() => ({ trees: {} }));

/** Publish a loaded tree. Loading gaps keep the previous grouping until its replacement arrives. */
export function publishTeamTree(teamKey: string, tree: DerivedPipelineTree | null): void {
  if (!tree || useTeamSurfaceStore.getState().trees[teamKey] === tree) return;
  useTeamSurfaceStore.setState((s) => ({ trees: { ...s.trees, [teamKey]: tree } }));
}

/** Forget `teamKey`'s tree (the team closed). */
export function clearTeamTree(teamKey: string): void {
  if (!(teamKey in useTeamSurfaceStore.getState().trees)) return;
  useTeamSurfaceStore.setState((s) => {
    const trees = { ...s.trees };
    delete trees[teamKey];
    return { trees };
  });
}

/** Move `oldKey`'s tree to `newKey` (the team was renamed). */
export function renameTeamTree(oldKey: string, newKey: string): void {
  const { trees } = useTeamSurfaceStore.getState();
  if (oldKey === newKey || !(oldKey in trees)) return;
  useTeamSurfaceStore.setState((s) => {
    const next = { ...s.trees, [newKey]: s.trees[oldKey] };
    delete next[oldKey];
    return { trees: next };
  });
}

/** `teamKey`'s published tree, reactive; null until its team view has published one. */
export function useTeamTree(teamKey: string | null): DerivedPipelineTree | null {
  return useTeamSurfaceStore((s) => (teamKey ? (s.trees[teamKey] ?? null) : null));
}
