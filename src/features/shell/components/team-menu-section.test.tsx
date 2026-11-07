// SPDX-License-Identifier: MIT
import { vi, describe, it, expect, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { TeamMenuSection } from "./team-menu-section";
import { EMPTY_CONTEXT_HISTORY, useViewerStore } from "@/shared/stores/viewer-store";
import { useOrgStore } from "@/shared/stores/org-store";
import type { Org, Team } from "@/shared/types/domain";
import { useScratchTeamUseStore } from "@/shared/stores/scratch-team-use-store";

vi.mock("@/shared/stores/shortcuts-store", () => ({
  useEffectiveShortcut: () => null,
}));

// SW-UX: create/browse teams open the team-management tab now.
const { mockOpenManageTeams } = vi.hoisted(() => ({ mockOpenManageTeams: vi.fn() }));
vi.mock("@/features/shell/manage-teams-virtual-tab", () => ({
  openManageTeams: mockOpenManageTeams,
}));

const { mockOpenWorkspaces } = vi.hoisted(() => ({ mockOpenWorkspaces: vi.fn() }));
vi.mock("@/features/org/lib/open-workspaces", () => ({
  openWorkspaces: mockOpenWorkspaces,
}));

const { mockGetScratchTeamUse, mockSetScratchTeamUse } = vi.hoisted(() => ({
  mockGetScratchTeamUse: vi.fn(),
  mockSetScratchTeamUse: vi.fn(),
}));
vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn(() => Promise.resolve()),
  loadAppState: vi.fn(() => Promise.resolve("{}")),
  getRepoPermissions: vi.fn(),
  parseGitRemote: vi.fn(),
  // SLW-PT-20: the team's real "Use my scratch items" switch.
  getScratchTeamUse: mockGetScratchTeamUse,
  setScratchTeamUse: mockSetScratchTeamUse,
}));

const mkTeam = (name: string): Team => ({
  name,
  source_repos: [],
});

function makeOrg(teams: Team[] = [mkTeam("platform")]): Org {
  return {
    slug: "acme",
    display_name: "Acme",
    created_at: "2026-01-01T00:00:00Z",
    org_root: "/fake/org",
    remote_url: null,
    config_flags: {},
    teams,
    people: [],
  };
}

const onDone = vi.fn();

function renderSection(org: Org, activeTeamName: string) {
  return render(<TeamMenuSection org={org} activeTeamName={activeTeamName} onDone={onDone} />);
}

/** Seed the real org-store so the switcher's `setShownTeams`/`setActiveTeam`
 *  actions have an org to reconcile against. */
function seedOrgStore(org: Org, activeTeamName: string | null) {
  useOrgStore.setState({
    orgs: [org],
    activeOrgSlug: org.slug,
    activeTeamName,
    lastTeamByOrgSlug: {},
    shownTeamsByOrgSlug: {},
    stateLoaded: true,
    hasUserIntent: false,
  });
}

describe("TeamMenuSection create-team entry point (SW-OB-1)", () => {
  beforeEach(() => {
    mockOpenManageTeams.mockClear();
    onDone.mockClear();
    seedOrgStore(makeOrg(), "platform");
  });

  it("opens the team-management tab in create mode when the `+` is clicked", async () => {
    renderSection(makeOrg(), "platform");

    await userEvent.click(screen.getByTestId("team-switcher-create"));

    expect(mockOpenManageTeams).toHaveBeenCalledWith("create");
    // The menu closes when the affordance is chosen.
    expect(onDone).toHaveBeenCalled();
  });

  it("SW-OC-16: connecting a context repo lives in the context repo switcher, not here", () => {
    renderSection(makeOrg(), "platform");
    expect(screen.queryByTestId("team-switcher-join")).toBeNull();
  });
});

describe("TeamMenuSection local visibility (SW-OB-9)", () => {
  beforeEach(() => {
    mockOpenManageTeams.mockClear();
    onDone.mockClear();
  });

  it("opens the team-management tab from the Browse all teams row", async () => {
    seedOrgStore(makeOrg(), "platform");
    renderSection(makeOrg(), "platform");

    await userEvent.click(screen.getByTestId("team-switcher-browse-all"));

    expect(mockOpenManageTeams).toHaveBeenCalledWith();
    expect(onDone).toHaveBeenCalled();
  });

  it("AC-4: hiding a team writes the reduced shown set and REACTIVELY drops the row", async () => {
    const org = makeOrg([mkTeam("a"), mkTeam("b")]);
    seedOrgStore(org, "a");
    renderSection(org, "a");

    // Both rows visible under the absent (show-all) default.
    expect(screen.getByTestId("team-switcher-row-a")).toBeInTheDocument();
    expect(screen.getByTestId("team-switcher-row-b")).toBeInTheDocument();

    // Hide "b" (non-active) via the inline ×.
    await userEvent.click(screen.getByTestId("team-switcher-hide-b"));

    // Store recorded the local hide (absent → materialized explicit set).
    expect(useOrgStore.getState().shownTeamsByOrgSlug.acme).toEqual(["a"]);
    // Reactivity lock: the row is GONE without any external re-render — proves
    // the switcher subscribes to the shown-map, not a stale getState() read.
    expect(screen.queryByTestId("team-switcher-row-b")).not.toBeInTheDocument();
    expect(screen.getByTestId("team-switcher-row-a")).toBeInTheDocument();
    // Active team unaffected (we hid a non-active team).
    expect(useOrgStore.getState().activeTeamName).toBe("a");
  });

  it("the hide action is its own menu item (keyboard-reachable), not a key chord on the row", async () => {
    const org = makeOrg([mkTeam("a"), mkTeam("b")]);
    seedOrgStore(org, "a");
    renderSection(org, "a");
    const hide = screen.getByTestId("team-switcher-hide-b");
    expect(hide).toHaveAttribute("role", "menuitem");
    screen.getByTestId("team-switcher-row-b").focus();
    await userEvent.keyboard("{Delete}");
    expect(useOrgStore.getState().shownTeamsByOrgSlug.acme).toBeUndefined();
    hide.focus();
    await userEvent.keyboard("{Enter}");
    expect(useOrgStore.getState().shownTeamsByOrgSlug.acme).toEqual(["a"]);
  });

  it("rows are menu radios: the active team is checked in place, and clicking another switches", async () => {
    const org = makeOrg([mkTeam("a"), mkTeam("b")]);
    seedOrgStore(org, "a");
    renderSection(org, "a");
    expect(screen.getByTestId("team-switcher-row-a")).toHaveAttribute("role", "menuitemradio");
    expect(screen.getByTestId("team-switcher-row-a")).toHaveAttribute("aria-checked", "true");
    expect(screen.getByTestId("team-switcher-row-b")).toHaveAttribute("aria-checked", "false");
    useViewerStore.setState({
      contextHistory: EMPTY_CONTEXT_HISTORY,
      activeWorkspace: { "acme:a": "a-focused" },
    });
    await userEvent.click(screen.getByTestId("team-switcher-row-b"));
    expect(useViewerStore.getState().contextHistory.back).toEqual([
      { teamKey: "acme:a", workspaceId: "a-focused" },
    ]);
    expect(useOrgStore.getState().activeTeamName).toBe("b");
    expect(onDone).toHaveBeenCalled();
  });

  it("AC-6: hiding the ACTIVE team switches active to the first remaining shown team", async () => {
    const org = makeOrg([mkTeam("a"), mkTeam("b")]);
    seedOrgStore(org, "a");
    renderSection(org, "a");

    await userEvent.click(screen.getByTestId("team-switcher-hide-a"));

    expect(useOrgStore.getState().shownTeamsByOrgSlug.acme).toEqual(["b"]);
    expect(useOrgStore.getState().activeTeamName).toBe("b");
  });
});

// SLW-PT-20 AC-2: "Use my private items in ‹Team›" — the ACTIVE team's switch, read from and
// written to the backend (the scratch repo), never browser storage.
describe("TeamMenuSection — Use my private items in ‹Team›", () => {
  beforeEach(() => {
    seedOrgStore(makeOrg([mkTeam("platform"), mkTeam("data")]), "platform");
    useScratchTeamUseStore.setState({ byTeam: {}, saving: {} });
    mockGetScratchTeamUse.mockReset();
    mockSetScratchTeamUse.mockReset();
    mockSetScratchTeamUse.mockResolvedValue(undefined);
  });

  it("reads the active team's switch and turns it off through the backend", async () => {
    mockGetScratchTeamUse.mockResolvedValue(true);
    renderSection(makeOrg([mkTeam("platform"), mkTeam("data")]), "platform");
    const row = screen.getByTestId("team-switcher-scratch-items");
    expect(row).toHaveTextContent("Use my private items in platform");
    expect(row.getAttribute("title")).toMatch(/^Covers platform's work/);
    const toggle = screen.getByRole("menuitemcheckbox");
    await vi.waitFor(() => expect(mockGetScratchTeamUse).toHaveBeenCalledWith("acme", "platform"));
    await vi.waitFor(() => expect(toggle).toHaveAttribute("aria-checked", "true"));

    await userEvent.click(toggle);
    expect(mockSetScratchTeamUse).toHaveBeenCalledWith("acme", "platform", false);
    await vi.waitFor(() => expect(toggle).toHaveAttribute("aria-checked", "false"));
    expect(globalThis.localStorage?.getItem("codesense.personal-hide")).toBeNull();
  });

  it("shows OFF when the backend has it off, and a failed write snaps back", async () => {
    mockGetScratchTeamUse.mockResolvedValue(false);
    mockSetScratchTeamUse.mockRejectedValue(new Error("disk full"));
    renderSection(makeOrg([mkTeam("platform"), mkTeam("data")]), "platform");
    const toggle = screen.getByRole("menuitemcheckbox");
    await vi.waitFor(() => expect(toggle).toHaveAttribute("aria-checked", "false"));
    await userEvent.click(toggle);
    await vi.waitFor(() => expect(toggle).toHaveAttribute("aria-checked", "false"));
    expect(mockSetScratchTeamUse).toHaveBeenCalledWith("acme", "platform", true);
  });
});
