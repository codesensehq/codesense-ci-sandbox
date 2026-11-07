// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useOrgStore } from "@/shared/stores/org-store";
import { useUiStore } from "@/shared/stores/ui-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { WORKSPACES_VIRTUAL_PATH } from "@/features/org/lib/open-workspaces";

import {
  openManageTeams,
  openTeamRequired,
  MANAGE_TEAMS_VIRTUAL_PATH,
} from "./manage-teams-virtual-tab";

// The team-management opener must ALWAYS provide a forward path: with an active team
// it opens the tab in the team; with NO active team (the first-run floor) it opens
// the same tab in the no-team tab host (SW-FU-B7) instead of silently no-opping.

function resetStores() {
  useOrgStore.setState({ orgs: [], activeOrgSlug: null, activeTeamName: null });
  useUiStore.setState({ manageTeamsIntent: null, noTeamTabs: [], noTeamActiveTab: null });
}

beforeEach(resetStores);
afterEach(() => {
  vi.restoreAllMocks();
  resetStores();
});

describe("openManageTeams — no active team (first-run floor)", () => {
  it("'create' opens the Teams tab in the no-team host (does NOT silently no-op)", () => {
    const openFile = vi.spyOn(useViewerStore.getState(), "openFile");
    openManageTeams("create");

    expect(useUiStore.getState().noTeamActiveTab).toBe(MANAGE_TEAMS_VIRTUAL_PATH);
    expect(useUiStore.getState().manageTeamsIntent).toBe("create");
    // No viewer tab is opened when there's no team content area.
    expect(openFile).not.toHaveBeenCalled();
  });

  it("'list' (default) opens the Teams tab too", () => {
    openManageTeams();
    expect(useUiStore.getState().noTeamActiveTab).toBe(MANAGE_TEAMS_VIRTUAL_PATH);
    expect(useUiStore.getState().manageTeamsIntent).toBe("list");
  });
});

describe("openManageTeams — with an active team", () => {
  beforeEach(() => {
    useOrgStore.setState({
      orgs: [{ slug: "acme", display_name: "Acme", teams: [{ name: "core" }] } as never],
      activeOrgSlug: "acme",
      activeTeamName: "core",
    });
  });

  it("opens the virtual tab in the active team's content area", () => {
    const openFile = vi.spyOn(useViewerStore.getState(), "openFile");
    openManageTeams("create");

    expect(useUiStore.getState().manageTeamsIntent).toBe("create");
    expect(useUiStore.getState().noTeamTabs).toEqual([]);
    expect(openFile).toHaveBeenCalledWith("acme:core", "", MANAGE_TEAMS_VIRTUAL_PATH, undefined);
  });
});

describe("openTeamRequired — a team-scoped surface with no team", () => {
  it("no workspace open → the Workspaces tab", () => {
    openTeamRequired();
    expect(useUiStore.getState().noTeamActiveTab).toBe(WORKSPACES_VIRTUAL_PATH);
  });

  it("a workspace with no teams → the Teams tab, focused on create", () => {
    useOrgStore.setState({
      orgs: [{ slug: "acme", display_name: "Acme", teams: [] } as never],
      activeOrgSlug: "acme",
      activeTeamName: null,
    });
    openTeamRequired();
    expect(useUiStore.getState().noTeamActiveTab).toBe(MANAGE_TEAMS_VIRTUAL_PATH);
    expect(useUiStore.getState().manageTeamsIntent).toBe("create");
  });

  it("a workspace whose teams are all archived → the Teams tab, focused on create", () => {
    useOrgStore.setState({
      orgs: [
        { slug: "acme", display_name: "Acme", teams: [{ name: "old", archived: true }] } as never,
      ],
      activeOrgSlug: "acme",
      activeTeamName: null,
    });
    openTeamRequired();
    expect(useUiStore.getState().manageTeamsIntent).toBe("create");
  });

  it("a workspace with teams but none active → the Teams tab, to pick one", () => {
    useOrgStore.setState({
      orgs: [{ slug: "acme", display_name: "Acme", teams: [{ name: "core" }] } as never],
      activeOrgSlug: "acme",
      activeTeamName: null,
    });
    openTeamRequired();
    expect(useUiStore.getState().noTeamActiveTab).toBe(MANAGE_TEAMS_VIRTUAL_PATH);
    expect(useUiStore.getState().manageTeamsIntent).toBe("list");
  });
});
