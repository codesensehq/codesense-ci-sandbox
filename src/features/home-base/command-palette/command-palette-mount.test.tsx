// SPDX-License-Identifier: MIT
import { resetContentFileIndexForTests } from "@/shared/lib/pipeline-graph/content-scan-queue";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { useMachinesStore } from "@/features/machines/stores/machines-store";
import { useLocalProfileStore, type LocalProfile } from "@/shared/stores/local-profile-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { useUiStore } from "@/shared/stores/ui-store";
import { setDynamicShortcutActions, type ShortcutAction } from "@/shared/lib/shortcuts/registry";
import { EMPTY_CONTEXT_HISTORY, useViewerStore } from "@/shared/stores/viewer-store";
import type { Org, Team } from "@/shared/types/domain";
import type { MachineSummary } from "@/tauri/commands";
import { listAttachmentsByRemote, listContextFiles, type PaneEntry } from "@/tauri/commands";
import { usePaneStore } from "@/features/shell/right-pane/panes/pane-store";

// Partial-mock the command layer: spy the local epic-file walk and the REMOTE
// roster fetch so we can assert the on-disk-only guarantee (AC-6). importActual
// keeps every other command (used by the stores) real.
vi.mock("@/tauri/commands", async () => {
  const actual = await vi.importActual<typeof import("@/tauri/commands")>("@/tauri/commands");
  return { ...actual, listContextFiles: vi.fn(), listAttachmentsByRemote: vi.fn() };
});

// A live index-changed listener, so listings are cached per generation (SV-PBF-267).
vi.mock("@tauri-apps/api/event", async () => {
  const actual =
    await vi.importActual<typeof import("@tauri-apps/api/event")>("@tauri-apps/api/event");
  return { ...actual, listen: vi.fn(async () => () => {}) };
});
const mockToastError = vi.fn();
vi.mock("@/shared/lib/with-error-toast", async () => {
  const actual = await vi.importActual<typeof import("@/shared/lib/with-error-toast")>(
    "@/shared/lib/with-error-toast",
  );
  return { ...actual, toastError: (...a: unknown[]) => mockToastError(...a) };
});

// Spy the settings opener so the shortcuts-launcher wiring (which SectionKey it
// deep-links to) is asserted at the real mount boundary.
vi.mock("@/features/settings/lib/open-settings", () => ({ openAppSettings: vi.fn() }));
vi.mock("@/features/shell/manage-teams-virtual-tab", () => ({ openManageTeams: vi.fn() }));
vi.mock("@/features/shell/machines-virtual-tab", () => ({ openMachines: vi.fn() }));

import { openAppSettings } from "@/features/settings/lib/open-settings";
import { openManageTeams } from "@/features/shell/manage-teams-virtual-tab";

import { CommandPaletteMount } from "./command-palette-mount";

const mockListContextFiles = vi.mocked(listContextFiles);
const mockListAttachmentsByRemote = vi.mocked(listAttachmentsByRemote);
const mockOpenAppSettings = vi.mocked(openAppSettings);

function team(name: string): Team {
  return { name, source_repos: [{ owner: "acme", repo: "web" }] };
}

const ORG: Org = {
  slug: "acme",
  display_name: "Acme",
  created_at: "2026-01-01T00:00:00Z",
  remote_url: null,
  config_flags: {} as Org["config_flags"],
  people: [],
  // "shown" is curated-in; "hidden" is curated OUT of the hub — both must appear
  // in the palette (G1/AC-2).
  teams: [team("shown"), team("hidden")],
  org_root: "/home/.codesense/users/42/orgs/acme",
};

function seed() {
  useOrgStore.setState({
    orgs: [ORG],
    activeOrgSlug: "acme",
    activeTeamName: null,
    // Curate the palette DOWN to just "shown" — the palette must ignore this.
    shownTeamsByOrgSlug: { acme: ["shown"] },
    workspaceRepos: {},
    hasUserIntent: false,
  });
  const octo: LocalProfile = { principal: "1", displayName: "Octo Cat", machineOwner: null };
  const hubot: LocalProfile = { principal: "2", displayName: "Hubot", machineOwner: null };
  useLocalProfileStore.setState({
    status: { active: octo, profiles: [octo, hubot], privateAuthority: null },
  });
  useViewerStore.setState({
    openedViewerPaths: {
      "acme:shown": ["/home/.codesense/users/42/orgs/acme/shown/content/e/epic.md"],
    },
  });
  useUiStore.setState({ commandPaletteOpen: true });
}

function machine(machineId: string, kind: MachineSummary["kind"]): MachineSummary {
  return {
    machineId,
    kind,
    label: machineId,
    scope: kind === "local" ? null : { kind: "personal" },
    declaration: null,
    binding: null,
    canPublish: true,
    status: { state: "connected", workspaces: 0 },
    attention: 0,
    capabilities: { agentCallbacks: true },
  };
}

beforeEach(() => {
  resetContentFileIndexForTests();
  useMachinesStore.setState({ byTeam: {} });
  useUiStore.setState({ machinesPanel: null });
  mockListContextFiles.mockReset().mockResolvedValue([]);
  mockListAttachmentsByRemote.mockReset().mockResolvedValue(new Map());
  mockToastError.mockReset();
  mockOpenAppSettings.mockReset();
  seed();
});

describe("CommandPaletteMount", () => {
  it("indexes ALL teams incl. curated-out ones (G1/AC-2 curation transparency)", () => {
    render(<CommandPaletteMount />);
    // The curated-out "hidden" team is reachable from the palette.
    expect(screen.getByText("hidden")).toBeInTheDocument();
    expect(screen.getByText("shown")).toBeInTheDocument();
  });

  it("surfaces a recent epic-file affordance from the session MRU (R2)", () => {
    render(<CommandPaletteMount />);
    // The dialog strips the viewer extension from the label (".md" → ""), so the
    // row reads "epic"; the sublabel matches the ordinary file's path.
    expect(screen.getByText("epic")).toBeInTheDocument();
    expect(screen.getByText("shown · e/epic.md")).toBeInTheDocument();
  });

  it("Enter opens the stronger object match over a weak recent file, also on the next search", () => {
    useViewerStore.setState({
      openedViewerPaths: {
        "acme:shown": ["/home/.codesense/users/42/orgs/acme/shown/content/hidden-notes.md"],
      },
    });
    render(<CommandPaletteMount />);
    const search = () => {
      const input = screen.getByPlaceholderText(
        "Jump to a team, repo, or epic — or run an action…",
      );
      fireEvent.change(input, { target: { value: "hidden" } });
      fireEvent.keyDown(input, { key: "Enter" });
      expect(useOrgStore.getState().activeTeamName).toBe("hidden");
    };
    search();
    act(() => {
      useOrgStore.setState({ activeTeamName: null });
      useUiStore.setState({ commandPaletteOpen: true });
    });
    search();
  });

  it("finds and opens the same path query after the file becomes recent, before indexing finishes", async () => {
    const fullPath = `${ORG.org_root}/shown/content/guides/target.md`;
    useViewerStore.setState({ openedViewerPaths: {}, activeTab: {} });
    mockListContextFiles.mockImplementation(async (root) =>
      root.endsWith("/shown/content")
        ? [{ name: "target.md", relativePath: "guides/target.md", fullPath }]
        : [],
    );
    render(<CommandPaletteMount />);
    await screen.findByText("target");
    const searchAndOpen = () => {
      const input = screen.getByRole("combobox");
      fireEvent.change(input, { target: { value: "guides/" } });
      expect(screen.getByRole("option", { selected: true })).toHaveTextContent("target");
      fireEvent.keyDown(input, { key: "Enter" });
      expect(useViewerStore.getState().activeTab["acme:shown"]).toBe(fullPath);
    };
    searchAndOpen();
    // The mounted page records a displayed path through this store seam.
    act(() => useViewerStore.getState().touchViewerPath("acme:shown", fullPath));
    // The next disk walk is pending: the MRU row must have the same search text.
    mockListContextFiles.mockImplementation(() => new Promise(() => {}));
    act(() => {
      useViewerStore.setState({ activeTab: {} });
      useUiStore.setState({ commandPaletteOpen: true });
    });
    searchAndOpen();
  });

  it("lists a switch action for each INACTIVE local profile (AC-5)", () => {
    render(<CommandPaletteMount />);
    expect(screen.getByText("Switch to Hubot")).toBeInTheDocument();
    // The active profile is not offered as a switch target.
    expect(screen.queryByText("Switch to Octo Cat")).not.toBeInTheDocument();
  });

  it("builds the epic-file index from LOCAL disk only — no remote roster fetch on open (AC-6)", async () => {
    render(<CommandPaletteMount />);
    await waitFor(() => expect(mockListContextFiles).toHaveBeenCalled());
    // One local walk per team (FULL set), rooted at each team's content dir.
    expect(mockListContextFiles).toHaveBeenCalledWith(
      "/home/.codesense/users/42/orgs/acme/shown/content",
    );
    expect(mockListContextFiles).toHaveBeenCalledWith(
      "/home/.codesense/users/42/orgs/acme/hidden/content",
    );
    // The on-disk-only invariant: opening the palette triggers NO remote call.
    expect(mockListAttachmentsByRemote).not.toHaveBeenCalled();
  });

  // SV-PBF-267 PR2-AC5: one listing per distinct, non-archived root; reopening at the same
  // index generation asks the backend for nothing.
  it("lists each non-archived root once and serves a reopen from the cache", async () => {
    useOrgStore.setState({
      orgs: [
        { ...ORG, teams: [team("shown"), team("hidden"), { ...team("old"), archived: true }] },
      ],
    });
    render(<CommandPaletteMount />);
    await waitFor(() => expect(mockListContextFiles).toHaveBeenCalledTimes(2));
    expect(mockListContextFiles).not.toHaveBeenCalledWith(
      "/home/.codesense/users/42/orgs/acme/old/content",
    );
    act(() => useUiStore.setState({ commandPaletteOpen: false }));
    act(() => useUiStore.setState({ commandPaletteOpen: true }));
    await waitFor(() => expect(screen.getByText("hidden")).toBeInTheDocument());
    expect(mockListContextFiles).toHaveBeenCalledTimes(2);
  });

  it("says why when every root is refused as not indexed, instead of showing an empty palette", async () => {
    mockListContextFiles.mockRejectedValue(new Error("not indexed: /home/x"));
    render(<CommandPaletteMount />);
    await waitFor(() => expect(mockToastError).toHaveBeenCalledTimes(1));
    expect(String(mockToastError.mock.calls[0][1])).toMatch(/isn't part of your synced context/);
  });

  it("does not index while closed (no disk walk fires until the palette opens)", () => {
    useUiStore.setState({ commandPaletteOpen: false });
    render(<CommandPaletteMount />);
    expect(mockListContextFiles).not.toHaveBeenCalled();
  });

  it("selecting a team records the departing workspace in context history", () => {
    useOrgStore.setState({ activeTeamName: "shown" });
    useViewerStore.setState({
      contextHistory: EMPTY_CONTEXT_HISTORY,
      activeWorkspace: { "acme:shown": "previous-workspace" },
    });
    render(<CommandPaletteMount />);
    fireEvent.click(screen.getByText("hidden"));
    expect(useOrgStore.getState().activeTeamName).toBe("hidden");
    expect(useViewerStore.getState().contextHistory.back).toEqual([
      { teamKey: "acme:shown", workspaceId: "previous-workspace" },
    ]);
  });

  it("selecting Create team opens the team-management tab in create mode", () => {
    render(<CommandPaletteMount />);
    fireEvent.click(screen.getByText("Create team…"));
    expect(vi.mocked(openManageTeams)).toHaveBeenCalledWith("create");
  });

  it("selecting the shortcuts launcher deep-links to Settings → Shortcuts (OC-17)", () => {
    render(<CommandPaletteMount />);
    fireEvent.click(screen.getByText("View keyboard shortcuts…"));
    expect(mockOpenAppSettings).toHaveBeenCalledWith("shortcuts");
  });

  it("clears the prior org's epic-file rows when the org switches while open (no stale cross-org rows)", async () => {
    mockListContextFiles.mockResolvedValue([
      { name: "alpha-epic.md", relativePath: "e/alpha-epic.md", fullPath: "/a/e/alpha-epic.md" },
    ]);
    const { rerender } = render(<CommandPaletteMount />);
    await waitFor(() => expect(screen.getAllByText("alpha-epic").length).toBeGreaterThan(0));

    // Switch to a different org while the palette stays open; the new walk is
    // slow (never resolves here) so only the CLEAR is observable.
    mockListContextFiles.mockReset().mockReturnValue(new Promise(() => {}));
    const OTHER: Org = { ...ORG, slug: "other", teams: [team("z")], org_root: "/other" };
    useOrgStore.setState({ orgs: [OTHER], activeOrgSlug: "other", shownTeamsByOrgSlug: {} });
    rerender(<CommandPaletteMount />);

    // The stale org-A epic rows must be gone immediately (not shown until the new
    // walk resolves).
    await waitFor(() => expect(screen.queryAllByText("alpha-epic")).toHaveLength(0));
  });

  it("does not commit a phantom tab when opening an epic-file for a team removed since open", () => {
    render(<CommandPaletteMount />);
    const openFileSpy = vi.spyOn(useViewerStore.getState(), "openFile");
    // The recent "epic" row belongs to team "shown" (seeded openedViewerPaths).
    // Remove "shown" from the org, then pick that recent row: `enterTeam` (shared
    // seam) no-ops setActiveTeam for the gone team, and the openEpicFile guard
    // must then SKIP openFile so no tab is committed under a dead team key.
    // (Leaving the hub is the shared seam's consistent behavior across all
    // hub-entry points and is not asserted here.)
    useOrgStore.setState({ orgs: [{ ...ORG, teams: [team("hidden")] }] });
    fireEvent.click(screen.getByText("epic"));
    expect(useOrgStore.getState().activeTeamName).not.toBe("shown");
    expect(openFileSpy).not.toHaveBeenCalled();
    openFileSpy.mockRestore();
  });

  it("offers right-pane panes but not a built-in the right pane does not declare", () => {
    useOrgStore.setState({ activeTeamName: "shown" });
    const pane = (id: string, slots: string[]): PaneEntry =>
      ({
        id,
        title: id === "agent-list" ? "Agents" : "Board",
        scope: "team",
        team: "shown",
        path: `/p/${id}.html`,
        panesDir: "/p",
        panelId: `pane:acme:shown/${id}`,
        declaration: { slots, capabilities: [] },
      }) as PaneEntry;
    usePaneStore.getState().setListing(
      "acme:shown",
      {
        contentRoot: "/ctx",
        chain: ["shown"],
        panes: [pane("board", ["right-pane"]), pane("agent-list", ["dock"])],
      },
      {},
    );
    render(<CommandPaletteMount />);
    expect(screen.getByText("Open pane: Board")).toBeInTheDocument();
    expect(screen.queryByText("Open pane: Agents")).not.toBeInTheDocument();
  });

  describe("machines", () => {
    it("Add machine… opens the add panel; there is no machine switch", () => {
      useOrgStore.setState({ activeTeamName: "shown" });
      useMachinesStore.setState({ byTeam: { "acme:shown": [machine("local", "local")] } });
      render(<CommandPaletteMount />);
      expect(screen.queryByText("Switch machine…")).not.toBeInTheDocument();
      fireEvent.click(screen.getByText("Add machine…"));
      expect(useUiStore.getState().machinesPanel).toEqual({ mode: "add" });
    });

    it("without the machine registry (not wired in this build): no machine rows", () => {
      render(<CommandPaletteMount />);
      expect(screen.queryByText("Switch machine…")).not.toBeInTheDocument();
      expect(screen.queryByText("Add machine…")).not.toBeInTheDocument();
    });
  });
});

describe("dashboard commands", () => {
  const command = (id: string, label: string, active: boolean): ShortcutAction => ({
    id: `dashboard:team:acme:shown/inbox/${id}`,
    label,
    defaultKeystroke: "",
    scope: "global",
    command: {
      dashboard: "dashboard:team:acme:shown/inbox",
      dashboardTitle: "Inbox",
      dashboardScope: "team",
      humanOnly: false,
      active,
    },
    run: vi.fn(),
  });

  it("lists only active commands and runs the picked one", () => {
    const archive = command("archive", "Archive thread", true);
    setDynamicShortcutActions([archive, command("reply", "Reply to thread", false)]);
    try {
      render(<CommandPaletteMount />);
      fireEvent.change(screen.getByRole("combobox"), { target: { value: "thread" } });
      expect(screen.getByText("Archive thread")).toBeInTheDocument();
      expect(screen.queryByText("Reply to thread")).toBeNull();
      fireEvent.click(screen.getByText("Archive thread"));
      expect(archive.run).toHaveBeenCalledTimes(1);
    } finally {
      setDynamicShortcutActions([]);
    }
  });
});
