// SPDX-License-Identifier: MIT
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";

// SW-FU-B7 seam: every management opener, in every team state, lands on a REAL tab —
// the no-team tab host when there is no team, the active team's content area when
// there is one. Never a no-op, never a dialog.
//
// The tab BODIES are stubbed (each renders a marker) so the seam under test is
// opener → tab host → virtual-tab registry → rendered surface.

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve(null)) }));

function stub(testId: string) {
  return () => <div data-testid={testId} />;
}
vi.mock("@/features/settings/components/settings-panel", () => ({
  SettingsPanel: stub("surface:settings"),
}));
vi.mock("@/features/org/components/org-picker", () => ({ OrgPicker: stub("org-picker") }));
vi.mock("@/features/shell/components/manage-teams-tab", () => ({
  ManageTeamsTab: stub("surface:manage-teams"),
}));
vi.mock("@/features/shell/components/machines-tab", () => ({
  MachinesTab: stub("surface:machines"),
}));
vi.mock("@/features/dashboard/components/manage-repos-tab", () => ({
  ManageReposTab: stub("surface:source-repos"),
}));

import { openWorkspaces, WORKSPACES_VIRTUAL_PATH } from "@/features/org/lib/open-workspaces";
import { registerWorkspacesTab } from "@/features/org/workspaces-virtual-tab";
import { openSettingsTab, SETTINGS_VIRTUAL_PATH } from "@/features/settings/lib/open-settings-tab";
import { registerSettingsTab } from "@/features/settings/settings-virtual-tab";
import {
  MANAGE_TEAMS_VIRTUAL_PATH,
  openManageTeams,
} from "@/features/shell/manage-teams-virtual-tab";
import { MACHINES_VIRTUAL_PATH, openMachines } from "@/features/shell/machines-virtual-tab";
import {
  openSourceRepos,
  SOURCE_REPOS_VIRTUAL_PATH,
} from "@/features/dashboard/source-repos-virtual-tab";
import { TeamWorkspaceEmptyState } from "@/features/dashboard/components/team-workspace-empty-state";
import { resolveMainTreeSurfaces } from "@/features/home-base/lib/home-base-routing";
import { SHORTCUT_ACTIONS } from "@/shared/lib/shortcuts/registry";
import { buildTeamKey } from "@/shared/lib/team-key";
import { useLocalProfileStore } from "@/shared/stores/local-profile-store";
import { useFirstRunStore } from "@/shared/stores/first-run-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { useUiStore } from "@/shared/stores/ui-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { Org } from "@/shared/types/domain";

import { NoTeamTabHost } from "./no-team-tab-host";

type StateName = "zero orgs" | "no org" | "org without team" | "one team" | "multiple teams";

function org(teams: string[]): Org {
  return {
    slug: "acme",
    display_name: "Acme",
    teams: teams.map((name) => ({ name })),
  } as unknown as Org;
}

const STATES: Record<StateName, () => void> = {
  "zero orgs": () => useOrgStore.setState({ orgs: [], activeOrgSlug: null, activeTeamName: null }),
  "no org": () =>
    useOrgStore.setState({ orgs: [org(["core"])], activeOrgSlug: null, activeTeamName: null }),
  "org without team": () =>
    useOrgStore.setState({ orgs: [org([])], activeOrgSlug: "acme", activeTeamName: null }),
  "one team": () =>
    useOrgStore.setState({ orgs: [org(["core"])], activeOrgSlug: "acme", activeTeamName: "core" }),
  "multiple teams": () =>
    useOrgStore.setState({
      orgs: [org(["core", "web"])],
      activeOrgSlug: "acme",
      activeTeamName: "web",
    }),
};

/** Where a team-scoped surface lands when there is no team to host it. */
const TEAM_REQUIRED: Record<"zero orgs" | "no org" | "org without team", string> = {
  "zero orgs": WORKSPACES_VIRTUAL_PATH,
  "no org": WORKSPACES_VIRTUAL_PATH,
  "org without team": MANAGE_TEAMS_VIRTUAL_PATH,
};

interface Opener {
  name: string;
  open: () => void | Promise<void>;
  path: string;
  /** Needs a team; with none it lands on the tab that gets the user one. */
  teamScoped?: boolean;
}

function shortcut(id: string): () => Promise<void> {
  return async () => {
    const action = SHORTCUT_ACTIONS.find((a) => a.id === id);
    if (!action) throw new Error(`no shortcut action ${id}`);
    action.run();
    await vi.dynamicImportSettled();
  };
}

const OPENERS: Opener[] = [
  // Menu items + keyboard shortcuts (the native menu dispatches the same actions).
  { name: "⌘, Open Settings", open: shortcut("settings.open"), path: SETTINGS_VIRTUAL_PATH },
  {
    name: "⌘⌥C Open Machines",
    open: shortcut("nav.open-machines"),
    path: MACHINES_VIRTUAL_PATH,
  },
  { name: "⌘⌥T Open Teams", open: shortcut("nav.open-teams"), path: MANAGE_TEAMS_VIRTUAL_PATH },
  {
    name: "⌘⌥W Open Workspaces",
    open: shortcut("nav.open-workspaces"),
    path: WORKSPACES_VIRTUAL_PATH,
  },
  {
    name: "⌘⌥R Open Source Repos",
    open: shortcut("nav.open-source-repos"),
    path: SOURCE_REPOS_VIRTUAL_PATH,
    teamScoped: true,
  },
  // In-app openers.
  { name: "Manage your teams…", open: () => openWorkspaces("home"), path: WORKSPACES_VIRTUAL_PATH },
  {
    name: "Create a workspace",
    open: () => openWorkspaces("create"),
    path: WORKSPACES_VIRTUAL_PATH,
  },
  { name: "Join a workspace", open: () => openWorkspaces("join"), path: WORKSPACES_VIRTUAL_PATH },
  { name: "Browse all teams", open: () => openManageTeams(), path: MANAGE_TEAMS_VIRTUAL_PATH },
  {
    name: "Create a team",
    open: () => openManageTeams("create"),
    path: MANAGE_TEAMS_VIRTUAL_PATH,
  },
  { name: "Settings", open: () => openSettingsTab("general"), path: SETTINGS_VIRTUAL_PATH },
  { name: "Machines", open: () => openMachines(), path: MACHINES_VIRTUAL_PATH },
  {
    name: "Add source repo",
    open: () => openSourceRepos("add"),
    path: SOURCE_REPOS_VIRTUAL_PATH,
    teamScoped: true,
  },
];

const SURFACE_TEST_ID: Record<string, string> = {
  [SETTINGS_VIRTUAL_PATH]: "surface:settings",
  [WORKSPACES_VIRTUAL_PATH]: "workspaces-tab",
  [MANAGE_TEAMS_VIRTUAL_PATH]: "surface:manage-teams",
  [MACHINES_VIRTUAL_PATH]: "surface:machines",
};

function resetStores() {
  useUiStore.setState({
    noTeamTabs: [],
    noTeamActiveTab: null,
    settingsInitialSection: null,
    manageTeamsIntent: null,
    workspacesScreen: null,
    sourceReposIntent: null,
  });
  useViewerStore.setState({ openedTeams: [], openTabs: {}, activeTab: {} });
}

beforeAll(() => {
  // App registers these at boot.
  registerSettingsTab();
  registerWorkspacesTab();
});

beforeEach(() => {
  resetStores();
  // A resolved user (a guest is enough) so the empty state renders its actions.
  useLocalProfileStore.setState({
    initialized: true,
    status: {
      active: { principal: "42", displayName: "Local", machineOwner: null },
      profiles: [],
      privateAuthority: null,
    },
  });
  useFirstRunStore.setState({ guest: true });
});

afterEach(() => {
  cleanup();
  resetStores();
});

describe.each(Object.keys(STATES) as StateName[])("with %s", (state) => {
  const hasTeam = state === "one team" || state === "multiple teams";

  it.each(OPENERS.map((o) => [o.name, o] as const))("%s lands on a real tab", async (_, o) => {
    STATES[state]();
    await act(async () => {
      await o.open();
    });

    if (hasTeam) {
      // The tab opens in the ACTIVE team's content area, and that pane is the
      // visible surface (not the empty state).
      const { activeOrgSlug, activeTeamName } = useOrgStore.getState();
      const teamKey = buildTeamKey(activeOrgSlug!, activeTeamName);
      const viewer = useViewerStore.getState();
      expect(viewer.activeTab[teamKey]).toBe(o.path);
      expect(viewer.openedTeams).toContain(teamKey);
      expect(
        resolveMainTreeSurfaces({
          openedTeamsCount: viewer.openedTeams.length,
          activeTeamName,
          activeTeamOpened: viewer.openedTeams.includes(teamKey),
        }).showTeamEmptyState,
      ).toBe(false);
      expect(useUiStore.getState().noTeamTabs).toEqual([]);
      return;
    }

    // No team: the no-team tab host shows the tab, and the tab renders its surface.
    const expected = o.teamScoped ? TEAM_REQUIRED[state as keyof typeof TEAM_REQUIRED] : o.path;
    expect(useUiStore.getState().noTeamActiveTab).toBe(expected);
    expect(Object.values(useViewerStore.getState().activeTab)).toEqual([]);
    render(
      <NoTeamTabHost>
        <div data-testid="empty-state" />
      </NoTeamTabHost>,
    );
    expect(screen.getByTestId("no-team-tab-host")).toBeInTheDocument();
    expect(screen.getByTestId(`no-team-tab-${expected}`)).toHaveAttribute("aria-selected", "true");
    expect(screen.getByTestId(SURFACE_TEST_ID[expected])).toBeInTheDocument();
    expect(screen.queryByTestId("empty-state")).toBeNull();
  });
});

describe("no-team tab host", () => {
  it("shows the empty state until a tab opens, and again once every tab is closed", () => {
    STATES["org without team"]();
    render(
      <NoTeamTabHost>
        <div data-testid="empty-state" />
      </NoTeamTabHost>,
    );
    expect(screen.getByTestId("empty-state")).toBeInTheDocument();

    act(() => openSettingsTab());
    act(() => openWorkspaces());
    expect(screen.getByTestId("workspaces-tab")).toBeInTheDocument();

    // Switch tabs.
    fireEvent.click(screen.getByTestId(`no-team-tab-${SETTINGS_VIRTUAL_PATH}`));
    expect(screen.getByTestId("surface:settings")).toBeInTheDocument();

    // Closing the active tab focuses its neighbour; closing the last shows the empty state.
    fireEvent.click(screen.getByRole("button", { name: "Close Settings" }));
    expect(screen.getByTestId("workspaces-tab")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Close Context repos" }));
    expect(screen.getByTestId("empty-state")).toBeInTheDocument();
  });

  it("middle-click on a tab closes it", () => {
    STATES["org without team"]();
    render(
      <NoTeamTabHost>
        <div data-testid="empty-state" />
      </NoTeamTabHost>,
    );
    act(() => openSettingsTab());
    fireEvent.pointerDown(screen.getByRole("button", { name: "Close Settings" }), { button: 1 });
    expect(screen.getByTestId("empty-state")).toBeInTheDocument();
  });

  it("once a team is active (the host unmounts), its tabs are gone — they never reappear later", () => {
    STATES["org without team"]();
    const view = render(
      <NoTeamTabHost>
        <div data-testid="empty-state" />
      </NoTeamTabHost>,
    );
    act(() => openSettingsTab());
    view.unmount();
    expect(useUiStore.getState().noTeamTabs).toEqual([]);
    expect(useUiStore.getState().noTeamActiveTab).toBeNull();
  });

  it("re-opening an open tab focuses it instead of duplicating it", () => {
    STATES["no org"]();
    openSettingsTab();
    openWorkspaces();
    openSettingsTab("account");
    expect(useUiStore.getState().noTeamTabs).toEqual([
      SETTINGS_VIRTUAL_PATH,
      WORKSPACES_VIRTUAL_PATH,
    ]);
    expect(useUiStore.getState().noTeamActiveTab).toBe(SETTINGS_VIRTUAL_PATH);
    expect(useUiStore.getState().settingsInitialSection).toBe("account");
  });
});

describe("no-team empty-state actions open tabs", () => {
  function renderEmptyState() {
    render(
      <NoTeamTabHost>
        <TeamWorkspaceEmptyState />
      </NoTeamTabHost>,
    );
  }

  it("no org: 'Open workspaces' opens the Workspaces tab", () => {
    STATES["no org"]();
    renderEmptyState();
    fireEvent.click(screen.getByRole("button", { name: "Open context repos" }));
    expect(screen.getByTestId("workspaces-tab")).toBeInTheDocument();
  });

  it("no org: 'Connect a context repo…' opens the Context repos tab on its Connect action", () => {
    STATES["no org"]();
    renderEmptyState();
    fireEvent.click(screen.getByRole("button", { name: "Connect a context repo…" }));
    expect(screen.getByTestId("workspaces-tab")).toBeInTheDocument();
    expect(screen.getByTestId("context-repo-connect")).toHaveTextContent("Connect a context repo…");
  });

  it("org without teams: 'Create a team' opens the Teams tab", () => {
    STATES["org without team"]();
    renderEmptyState();
    fireEvent.click(screen.getByRole("button", { name: "Create a team" }));
    expect(screen.getByTestId("surface:manage-teams")).toBeInTheDocument();
    expect(useUiStore.getState().manageTeamsIntent).toBe("create");
  });

  it("org with teams but none active: 'Browse all teams' opens the Teams tab", () => {
    useOrgStore.setState({ orgs: [org(["core"])], activeOrgSlug: "acme", activeTeamName: null });
    renderEmptyState();
    fireEvent.click(screen.getByRole("button", { name: "Browse all teams" }));
    expect(screen.getByTestId("surface:manage-teams")).toBeInTheDocument();
  });
});
