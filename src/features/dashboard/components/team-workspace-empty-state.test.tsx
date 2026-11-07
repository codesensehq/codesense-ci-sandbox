// SPDX-License-Identifier: MIT
import { vi, describe, it, expect, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { TeamWorkspaceEmptyState } from "./team-workspace-empty-state";
import { useOrgStore } from "@/shared/stores/org-store";
import { useUiStore } from "@/shared/stores/ui-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { syntheticRepoKey } from "@/shared/lib/build-synthetic-repo";
import type { CurrentUser, Org } from "@/shared/types/domain";

let currentUserValue: CurrentUser | null = null;
vi.mock("@/shared/lib/get-current-user", () => ({
  getCurrentUser: () => currentUserValue,
  useCurrentUser: () => currentUserValue,
  buildCurrentUser: () => currentUserValue,
}));

// SW-UX: the add-source-repo CTA opens the source-repos tab in add mode.
const { mockOpenSourceRepos } = vi.hoisted(() => ({ mockOpenSourceRepos: vi.fn() }));
vi.mock("@/features/dashboard/source-repos-virtual-tab", () => ({
  openSourceRepos: mockOpenSourceRepos,
}));

// SW-UX: the team CTAs open the team-management tab.
const { mockOpenManageTeams } = vi.hoisted(() => ({ mockOpenManageTeams: vi.fn() }));
vi.mock("@/features/shell/manage-teams-virtual-tab", () => ({
  openManageTeams: mockOpenManageTeams,
}));

const ACME: Org = {
  slug: "acme",
  display_name: "Acme",
  created_at: "2026-01-01T00:00:00Z",
  org_root: "/fake",
  remote_url: null,
  config_flags: {},
  people: [],
  teams: [
    {
      name: "core",
      source_repos: [],
    },
  ],
};

function resetStores() {
  useOrgStore.setState({
    orgs: [],
    activeOrgSlug: null,
    activeTeamName: null,
    workspaceRepos: {},
  });
  useUiStore.setState({
    orgSwitcherOpen: false,
    teamSwitcherOpen: false,
    manageTeamsIntent: null,
    settingsPanelOpen: false,
    settingsInitialSection: null,
    sourceReposIntent: null,
    workspacesScreen: null,
    noTeamTabs: [],
    noTeamActiveTab: null,
  });
  mockOpenSourceRepos.mockClear();
  mockOpenManageTeams.mockClear();
  currentUserValue = {
    github_user_id: 42,
    github_username: "sam",
    display_name: "Sam",
    avatar_url: null,
  };
}

beforeEach(() => {
  resetStores();
});

describe("TeamWorkspaceEmptyState", () => {
  it("SW-FU-B2: never renders nothing — shows a loading state while currentUser is null", () => {
    currentUserValue = null;
    render(<TeamWorkspaceEmptyState />);
    expect(screen.getByRole("status")).toHaveTextContent(/loading local profile/i);
    // SW-FU-C1: the shared wait, not a hand-rolled spinner.
    expect(document.querySelector('[data-slot="loading-state"]')).not.toBeNull();
  });

  it("SW-FU-C1: every rung is the one shared empty state with one primary action", () => {
    render(<TeamWorkspaceEmptyState />);
    const states = document.querySelectorAll('[data-slot="empty-state"]');
    expect(states).toHaveLength(1);
    expect(states[0].querySelectorAll('[data-variant="default"]')).toHaveLength(1);
  });

  describe("SW-FU-B2: the active team has no open pane (upgrade drift)", () => {
    const slug = { owner: "acme", repo: "web" };
    beforeEach(() => {
      useViewerStore.setState({ openedTeams: ["acme:other"] });
    });

    it("a clone-backed active team renders an 'Open team' action, never nothing", async () => {
      useOrgStore.setState({
        orgs: [ACME],
        activeOrgSlug: "acme",
        activeTeamName: "core",
        workspaceRepos: {
          [syntheticRepoKey("acme", "core", slug)]: {
            repoPath: syntheticRepoKey("acme", "core", slug),
            repoName: "web (Acme)",
            contextRepoPath: "/fake/core/repos/acme/web",
            teamContentPath: "/fake/core/content",
            hasRemote: false,
            remoteUrl: null,
            mainDoc: "",
            localPath: "/code/web",
          },
        },
      });
      render(<TeamWorkspaceEmptyState />);
      await userEvent.click(screen.getByRole("button", { name: /open team/i }));
      expect(useViewerStore.getState().openedTeams).toContain("acme:core");
    });

    it("'Add source repo' opens the team first, so the tab lands in a mounted pane", async () => {
      useOrgStore.setState({
        orgs: [ACME],
        activeOrgSlug: "acme",
        activeTeamName: "core",
        workspaceRepos: {},
      });
      render(<TeamWorkspaceEmptyState />);
      await userEvent.click(screen.getByRole("button", { name: /add source repo/i }));
      expect(useViewerStore.getState().openedTeams).toContain("acme:core");
      expect(mockOpenSourceRepos).toHaveBeenCalledWith("add");
    });
  });

  describe("no-team / no-org sub-state", () => {
    beforeEach(() => {
      useOrgStore.setState({ orgs: [ACME], activeOrgSlug: null, activeTeamName: null });
    });

    it("renders the workspace-picker copy (SW-OB-5: standardized noun)", () => {
      render(<TeamWorkspaceEmptyState />);
      expect(screen.getByRole("heading", { level: 2 })).toHaveTextContent(/open a context repo/i);
    });

    it("primary CTA opens the Workspaces tab (SW-FU-B7)", async () => {
      render(<TeamWorkspaceEmptyState />);
      await userEvent.click(screen.getByRole("button", { name: /open context repos/i }));
      expect(useUiStore.getState().noTeamActiveTab).toBe("virtual://workspaces");
    });

    it("SW-FU-B6: offers Connect a context repo…, straight to the join form", async () => {
      render(<TeamWorkspaceEmptyState />);
      await userEvent.click(screen.getByRole("button", { name: /connect a context repo…/i }));
      expect(useUiStore.getState().noTeamActiveTab).toBe("virtual://workspaces");
      expect(useUiStore.getState().workspacesScreen).toBe("join");
    });
  });

  describe("no-team / org-active sub-state", () => {
    beforeEach(() => {
      // The active org has a team, but no team is currently selected
      // (activeTeamName does not match it).
      const orgWithForeignTeam: Org = {
        ...ACME,
        teams: [
          {
            name: "core",
            source_repos: [],
          },
        ],
      };
      useOrgStore.setState({
        orgs: [orgWithForeignTeam],
        activeOrgSlug: "acme",
        activeTeamName: null,
      });
    });

    it("mentions the active org by display name", () => {
      render(<TeamWorkspaceEmptyState />);
      expect(screen.getByRole("heading", { level: 2 })).toHaveTextContent(/acme/i);
    });

    it("SW-OB-9 AC-6: primary CTA opens the team-management tab (re-add surface, not the switcher)", async () => {
      // The switcher can't mount without an active team, so with teams present
      // but no active team the re-add path is the team-management tab.
      render(<TeamWorkspaceEmptyState />);
      await userEvent.click(screen.getByRole("button", { name: /browse all teams/i }));
      expect(mockOpenManageTeams).toHaveBeenCalledWith();
    });

    it("no 'Create team' CTA is rendered (stub deferred to C.10a)", () => {
      render(<TeamWorkspaceEmptyState />);
      expect(screen.queryByRole("button", { name: /create team/i })).not.toBeInTheDocument();
    });
  });

  describe("active-team existence routing (SW-OB-7 open access)", () => {
    it("a foreign-but-existing active team is workable → routes to the source-repo state", () => {
      // Org has team A and team B. The current user is a member of neither
      // relevant to routing anymore — under open access, `activeTeamName="B"`
      // is valid because team B EXISTS, so the user falls through to the
      // source-repo emptiness check (they may attach a repo to B).
      const orgWithTwoTeams: Org = {
        ...ACME,
        teams: [
          { name: "A", source_repos: [] },
          { name: "B", source_repos: [] },
        ],
      };
      useOrgStore.setState({
        orgs: [orgWithTwoTeams],
        activeOrgSlug: "acme",
        activeTeamName: "B",
        workspaceRepos: {},
      });
      render(<TeamWorkspaceEmptyState />);
      // Existing active team → the add-source-repo copy for team B.
      expect(screen.getByRole("heading", { level: 2 })).toHaveTextContent(/add a source repo/i);
      expect(screen.getByRole("heading", { level: 2 })).toHaveTextContent(/\bB\b/);
    });

    it("renders no-team sub-state when activeTeamName references a team that no longer exists in the org", () => {
      // Stale activeTeamName: the team named "ghost" was removed from
      // teams.yml but activeTeamName wasn't cleared. The team-existence
      // check rejects this and routes to NoTeamInOrgState.
      const orgWithoutGhost: Org = {
        ...ACME,
        teams: [{ name: "real", source_repos: [] }],
      };
      useOrgStore.setState({
        orgs: [orgWithoutGhost],
        activeOrgSlug: "acme",
        activeTeamName: "ghost",
        workspaceRepos: {},
      });
      render(<TeamWorkspaceEmptyState />);
      expect(screen.getByRole("heading", { level: 2 })).toHaveTextContent(/pick a team in acme/i);
    });

    it("uses the 'pick a team' body copy when the org has teams but none is active", () => {
      // Open access: with teams present and no active team, the body invites
      // picking a team — no membership language.
      const orgTwoTeams: Org = {
        ...ACME,
        teams: [
          { name: "A", source_repos: [] },
          { name: "B", source_repos: [] },
        ],
      };
      useOrgStore.setState({
        orgs: [orgTwoTeams],
        activeOrgSlug: "acme",
        activeTeamName: null,
        workspaceRepos: {},
      });
      render(<TeamWorkspaceEmptyState />);
      expect(screen.getByText(/pick a team to start working/i)).toBeInTheDocument();
      // SW-OB-5: the "what a team is" explainer accompanies the pick-a-team copy.
      expect(screen.getByText(/one slice of work/i)).toBeInTheDocument();
      expect(screen.queryByText(/no teams yet/i)).not.toBeInTheDocument();
    });

    it("zero-teams state opens the Create Team dialog (no dead-end team switcher)", async () => {
      // The org genuinely has no teams. The team switcher can't mount without
      // an active team, so the CTA opens the SW-OB-1 Create Team dialog
      // directly, not a switcher flag that renders nothing.
      const orgNoTeams: Org = { ...ACME, teams: [] };
      useOrgStore.setState({
        orgs: [orgNoTeams],
        activeOrgSlug: "acme",
        activeTeamName: null,
        workspaceRepos: {},
      });
      render(<TeamWorkspaceEmptyState />);
      expect(screen.getByRole("heading", { level: 2 })).toHaveTextContent(/no teams in acme yet/i);
      expect(screen.getByText(/create your first team/i)).toBeInTheDocument();
      // SW-OB-5: the "what a team is" explainer accompanies the zero-teams copy.
      expect(screen.getByText(/one slice of work/i)).toBeInTheDocument();
      // Primary CTA opens the team-management tab in create mode.
      await userEvent.click(screen.getByRole("button", { name: /create a team/i }));
      expect(mockOpenManageTeams).toHaveBeenCalledWith("create");
    });

    it("SW-FU-B6: the pick-a-team state also offers Connect a context repo…", async () => {
      render(<TeamWorkspaceEmptyState />);
      await userEvent.click(screen.getByRole("button", { name: /connect a context repo…/i }));
      expect(useUiStore.getState().workspacesScreen).toBe("join");
    });

    it("SW-FU-B6: the zero-teams state also offers Connect a context repo…", async () => {
      useOrgStore.setState({
        orgs: [{ ...ACME, teams: [] }],
        activeOrgSlug: "acme",
        activeTeamName: null,
        workspaceRepos: {},
      });
      render(<TeamWorkspaceEmptyState />);
      await userEvent.click(screen.getByRole("button", { name: /connect a context repo…/i }));
      expect(useUiStore.getState().noTeamActiveTab).toBe("virtual://workspaces");
      expect(useUiStore.getState().workspacesScreen).toBe("join");
    });
  });

  // SW-OC-B9: collapse the competing first-run empty states into ONE focused
  // panel with a single primary action, and stop triplicating the org name.
  describe("SW-OC-B9: single first-run panel + no org-name triplication", () => {
    it("renders exactly one primary CTA (the earliest unmet precondition)", () => {
      const orgNoTeams: Org = { ...ACME, teams: [] };
      useOrgStore.setState({
        orgs: [orgNoTeams],
        activeOrgSlug: "acme",
        activeTeamName: null,
        workspaceRepos: {},
      });
      render(<TeamWorkspaceEmptyState />);
      // One panel, one heading, one primary — not three stacked empty states. The
      // only other action is the secondary "Connect a context repo…" link (SW-FU-B6).
      expect(screen.getAllByRole("heading", { level: 2 })).toHaveLength(1);
      expect(screen.getAllByRole("button").map((b) => b.textContent)).toEqual([
        "Create a team",
        "Connect a context repo…",
      ]);
    });

    it("does not repeat the org name in a shell header (identity lives in the title bar)", () => {
      // The org name appears only inside the single empty-state heading, not in a
      // second shell header — the persistent title bar's IdentityMenu is the
      // one always-present source of the org name.
      const orgNoTeams: Org = { ...ACME, teams: [] };
      useOrgStore.setState({
        orgs: [orgNoTeams],
        activeOrgSlug: "acme",
        activeTeamName: null,
        workspaceRepos: {},
      });
      render(<TeamWorkspaceEmptyState />);
      // "Acme" is only in the "No teams in Acme yet" heading; the FirstRunShell no
      // longer prints a second copy.
      expect(screen.getAllByText(/acme/i)).toHaveLength(1);
    });
  });

  describe("no-source-repos sub-state", () => {
    beforeEach(() => {
      useOrgStore.setState({
        orgs: [ACME],
        activeOrgSlug: "acme",
        activeTeamName: "core",
        workspaceRepos: {}, // empty for this team
      });
    });

    it("renders the add-source-repo copy with team name", () => {
      render(<TeamWorkspaceEmptyState />);
      expect(screen.getByRole("heading", { level: 2 })).toHaveTextContent(/add a source repo/i);
      expect(screen.getByRole("heading", { level: 2 })).toHaveTextContent(/core/i);
    });

    it("primary CTA opens the source-repos tab in add mode", async () => {
      render(<TeamWorkspaceEmptyState />);
      await userEvent.click(screen.getByRole("button", { name: /add source repo/i }));
      expect(mockOpenSourceRepos).toHaveBeenCalledWith("add");
    });

    it("offers no starter pack or sample, only the add-repo action", () => {
      render(<TeamWorkspaceEmptyState />);
      expect(screen.queryByTestId("starter-pack-import")).toBeNull();
      expect(screen.queryByText(/sample/i)).toBeNull();
      expect(screen.getAllByRole("button")).toHaveLength(1);
    });

    it("does NOT route repo management to Settings (SW-UX: repos are first-class on the team)", () => {
      render(<TeamWorkspaceEmptyState />);
      expect(screen.queryByText(/manage source repos in settings/i)).not.toBeInTheDocument();
    });
  });

  // SW-UX (app-flow redesign): the ladder is collapsed. The old NoAttachedClone
  // clone-gate and the setup-checklist rungs are removed — attaching a
  // source repo (local folder or URL) via the one dialog IS the whole action. So
  // a team that tracks a repo with no local attachment shows the SAME clean "add
  // a source repo" state, not a separate clone gate.
  describe("collapsed ladder (SW-UX): tracked-but-unattached shows 'add a source repo'", () => {
    const slug = { owner: "acme", repo: "web" };
    function seedTeamWithRepo(localPath: string | null) {
      useOrgStore.setState({
        orgs: [ACME],
        activeOrgSlug: "acme",
        activeTeamName: "core",
        workspaceRepos: {
          [syntheticRepoKey("acme", "core", slug)]: {
            repoPath: syntheticRepoKey("acme", "core", slug),
            repoName: "web (Acme)",
            contextRepoPath: "/fake/_team/core/repos/acme/web",
            teamContentPath: "/fake/_team/core/content",
            hasRemote: false,
            remoteUrl: null,
            mainDoc: "",
            localPath,
          },
        },
      });
    }

    it("a tracked repo with no local attachment shows 'add a source repo' (no clone-gate)", () => {
      seedTeamWithRepo(null);
      render(<TeamWorkspaceEmptyState />);
      expect(screen.getByRole("heading", { level: 2 })).toHaveTextContent(/add a source repo/i);
      // The removed clone-gate copy must be gone.
      expect(screen.queryByText(/attach a local clone/i)).not.toBeInTheDocument();
    });

    it("the single 'add a source repo' CTA opens the source-repos tab in add mode", async () => {
      seedTeamWithRepo(null);
      render(<TeamWorkspaceEmptyState />);
      await userEvent.click(screen.getByRole("button", { name: /add source repo/i }));
      expect(mockOpenSourceRepos).toHaveBeenCalledWith("add");
    });
  });
});
