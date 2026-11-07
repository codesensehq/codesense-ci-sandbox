// SPDX-License-Identifier: MIT
import { getActiveTeamKey, teamKeyHasTeam } from "@/shared/lib/team-key";
import { useUiStore } from "@/shared/stores/ui-store";
import { useViewerStore } from "@/shared/stores/viewer-store";

/**
 * SW-FU-B7 — the ONE way a management surface (`virtual://…`) opens as a tab.
 *
 * With a real team active, the tab opens in that team's content area (the team is
 * also marked opened, so its pane is guaranteed to render the tab). With NO team —
 * no org, or an org with no team yet — it opens in the minimal no-team tab host
 * instead. Either way a tab opens: an opener is never a silent no-op, and there is
 * no dialog fallback (CONSISTENCY-SPEC Rule 1). `keepLayout` marks an open the user
 * did not ask for, which leaves full-screen CLI on (see `openFile`).
 */
export function openManagementTab(path: string, opts?: { keepLayout?: boolean }): void {
  const teamKey = getActiveTeamKey();
  if (!teamKeyHasTeam(teamKey)) {
    useUiStore.getState().openNoTeamTab(path);
    return;
  }
  const viewer = useViewerStore.getState();
  viewer.openTeam(teamKey);
  viewer.openFile(teamKey, "", path, opts);
}

/** Whether a real team is active, i.e. team-scoped surfaces can open. */
export function hasActiveTeam(): boolean {
  return teamKeyHasTeam(getActiveTeamKey());
}
