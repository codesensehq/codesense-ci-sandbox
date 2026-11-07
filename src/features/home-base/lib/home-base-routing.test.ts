// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { resolveMainTreeSurfaces, teamPaneVisible } from "./home-base-routing";

describe("resolveMainTreeSurfaces", () => {
  it("team entry with a valid active team → team view (no empty-state)", () => {
    expect(
      resolveMainTreeSurfaces({
        openedTeamsCount: 1,
        activeTeamName: "default",
        activeTeamOpened: true,
      }),
    ).toEqual({ showTeamEmptyState: false });
  });

  it("SW-UX: a valid team with NO source-repo clone still renders the team view (not the empty state)", () => {
    // The clone gate was removed from routing — a context-repo-only team is
    // usable; "add a source repo" is an in-app action, never a full-screen wall.
    expect(
      resolveMainTreeSurfaces({
        openedTeamsCount: 1,
        activeTeamName: "default",
        activeTeamOpened: true,
      }),
    ).toEqual({ showTeamEmptyState: false });
  });

  it("no active team → empty-state (the only case that shows it)", () => {
    expect(
      resolveMainTreeSurfaces({
        openedTeamsCount: 0,
        activeTeamName: null,
        activeTeamOpened: false,
      }),
    ).toEqual({ showTeamEmptyState: true });
  });

  it("SW-FU-B2: an active team missing from openedTeams → empty-state, never a dead blank", () => {
    // The upgrade case: a persisted `openedTeams` names another team, so the
    // active team has no mounted pane. Every pane is `display:none`; only the
    // floor can show anything.
    expect(
      resolveMainTreeSurfaces({
        openedTeamsCount: 1,
        activeTeamName: "general",
        activeTeamOpened: false,
      }),
    ).toEqual({ showTeamEmptyState: true });
  });
});

describe("teamPaneVisible", () => {
  const base = {
    settingsPanelOpen: false,
    isActiveTeam: true,
  };

  it("visible for the active team (clone no longer required)", () => {
    expect(teamPaneVisible(base)).toBe(true);
  });

  it("hidden for a non-active team or during settings", () => {
    expect(teamPaneVisible({ ...base, isActiveTeam: false })).toBe(false);
    expect(teamPaneVisible({ ...base, settingsPanelOpen: true })).toBe(false);
  });
});
