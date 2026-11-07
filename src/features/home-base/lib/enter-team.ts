// SPDX-License-Identifier: MIT
// The single team-entry seam shared by the command palette and other entry
// points (DRY). Team entry is `setActiveTeam(name)`, with one variant — re-open a
// specific file tab on the way in — kept in ONE place so callers don't drift.

import { useCallback } from "react";

import { buildTeamKey, getActiveTeamKey } from "@/shared/lib/team-key";
import { switchContext, switchContextToTeamName } from "@/features/viewer/lib/context-switch";
import { useViewerStore } from "@/shared/stores/viewer-store";

export interface EnterTeamFileTarget {
  orgSlug: string;
  /** The file path to re-open + activate on entry (AC-9). */
  filePath: string;
  /** The owning repo path, or null when it can't be resolved — `setActiveTab`
   *  tolerates null (it just skips the nav reveal). */
  repoPath?: string | null;
}

/**
 * Return a stable `enterTeam(teamName, target?)` callback.
 *
 * - No `target` → plain team entry.
 * - With `target` → also mount the team (`openTeam`, idempotent) and re-activate
 *   the given file tab (`setActiveTab`) so the user lands back on the exact file
 *   they were on, not a cold team landing.
 */
export function useEnterTeam(): (teamName: string, target?: EnterTeamFileTarget) => void {
  return useCallback((teamName: string, target?: EnterTeamFileTarget) => {
    const teamKey = target ? buildTeamKey(target.orgSlug, teamName) : null;
    if (teamKey) switchContext({ teamKey }, "switcher");
    else switchContextToTeamName(teamName, "switcher");
    if (teamKey && getActiveTeamKey() !== teamKey) return;
    if (target) {
      const teamKey = buildTeamKey(target.orgSlug, teamName);
      const viewer = useViewerStore.getState();
      viewer.setActiveTab(teamKey, target.repoPath ?? null, target.filePath);
    }
  }, []);
}
