// SPDX-License-Identifier: MIT
import { isConsoleLayout } from "@/features/viewer/lib/terminal-column";
import { teamKeyHasTeam, useActiveTeamKey } from "@/shared/lib/team-key";
import { useViewerStore } from "@/shared/stores/viewer-store";

/** Saved full-screen chrome only applies while an active team is open in the workbench. */
export function useIsConsole(): boolean {
  const teamKey = useActiveTeamKey();
  return useViewerStore((s) =>
    isConsoleLayout(s, teamKeyHasTeam(teamKey) && s.openedTeams.includes(teamKey)),
  );
}
