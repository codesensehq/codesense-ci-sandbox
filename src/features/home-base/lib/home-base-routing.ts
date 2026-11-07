// SPDX-License-Identifier: MIT
// The ONE routing decision that picks the App main-tree surface: a team surface
// vs the team-scoped empty-state ladder. Extracted as a pure function so the
// rules unit-test without mounting App, and so the choice lives in ONE place
// (not scattered across the JSX).

export interface MainTreeSurfaces {
  /** Render the team-scoped empty-state ladder (no valid active team). */
  showTeamEmptyState: boolean;
}

export interface MainTreeRoutingInput {
  openedTeamsCount: number;
  activeTeamName: string | null;
  /** Whether the active team has a mounted pane, i.e. its key is in
   *  `openedTeams`. App renders a pane only for opened teams, so an active team
   *  missing from that list (a persisted list from an older build, or a switch
   *  that bailed) would otherwise leave every pane `display:none` and the
   *  content area blank. Treated like `activeTeamName === null`. */
  activeTeamOpened: boolean;
}

/**
 * Decide the active content surface once the App gate ladder has reached the
 * main tree (all earlier gates passed AND `orgCount > 0`).
 *
 * SW-UX (app-flow redesign): the full-screen empty state is shown ONLY when there
 * is no valid active team at all (no opened team / null active team). A team WITH
 * a context repo but NO attached source-repo clone is fully usable — it renders
 * the real workspace, and "add a source repo" is an in-app action, never a
 * full-screen blocker. So the old `!activeTeamHasClone` condition (which covered
 * the whole app) is removed from routing.
 *
 * Never a dead blank (SW-FX-96, restored by SW-FU-B2): an active team with no
 * mounted pane gets the empty-state floor, whose action opens the team.
 */
export function resolveMainTreeSurfaces(input: MainTreeRoutingInput): MainTreeSurfaces {
  const showTeamEmptyState =
    input.openedTeamsCount === 0 || input.activeTeamName === null || !input.activeTeamOpened;
  return { showTeamEmptyState };
}

/** Whether a given team pane should be the VISIBLE surface (vs `display:none`).
 *  Panes always stay mounted; this only toggles `display`.
 *
 *  SW-UX (app-flow redesign): a team is VISIBLE as soon as it's the active team —
 *  a missing source-repo clone no longer hides it (the context repo makes the
 *  workspace usable; adding a source repo is an in-app action). */
export function teamPaneVisible(input: {
  settingsPanelOpen: boolean;
  isActiveTeam: boolean;
}): boolean {
  return !input.settingsPanelOpen && input.isActiveTeam;
}
