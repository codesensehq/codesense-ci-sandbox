// SPDX-License-Identifier: MIT
import { applyFieldPatch } from "@/shared/lib/field-patch";
import "@/test/prefs-state-fixture";
import { describe, it, expect, beforeEach, vi } from "vitest";

// D39: the CLI pane's chrome (visibility, split width, layout mode, last terminal mode, expanded
// and the Agents dock) is one setting for every team, saved as one `cliChrome` value.

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  listNotes: vi.fn().mockResolvedValue([]),
  readNote: vi
    .fn()
    .mockResolvedValue({ id: "n1", title: "Scratch", pinned: false, lastOpenedAt: 0, content: "" }),
  createNote: vi.fn().mockResolvedValue({
    id: "n2",
    title: "Untitled",
    pinned: false,
    lastOpenedAt: 0,
    content: "",
  }),
  writeNote: vi.fn().mockResolvedValue(undefined),
  updateNoteMeta: vi
    .fn()
    .mockResolvedValue({ id: "n1", title: "Scratch", pinned: false, lastOpenedAt: 0 }),
  deleteNote: vi.fn().mockResolvedValue([]),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
  detectTools: vi.fn().mockResolvedValue({
    terminals: [],
    editors: [],
    default_terminal: "",
    default_editor: "",
  }),
}));

vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: vi.fn(), debug: vi.fn() },
  initLogging: vi.fn().mockResolvedValue(undefined),
}));

import { useViewerStore, type Workspace } from "./viewer-store";
import { useOrgStore } from "./org-store";
import * as commands from "@/tauri/commands";
import type { RepoState } from "./config-store";
import { installTerminalLayoutHarness, seedTerminalTabs } from "@/test/terminal-layout-harness";

installTerminalLayoutHarness();

const loadAppStateMock = commands.loadAppState as ReturnType<typeof vi.fn>;
const saveAppStateMock = commands.saveAppState as ReturnType<typeof vi.fn>;

const TEAM_A = "acme:alpha";
const TEAM_B = "acme:beta";
const TEAM_C = "acme:gamma";

const story = (id: string): Workspace => ({
  id,
  kind: "story",
  cwd: `/wt/${id}`,
  storyFilePath: `/stories/${id}.md`,
  storyId: id,
  status: "ready",
});

/** Three teams of one org, `activeTeam` selected. */
function seedOrg(activeTeam = "alpha") {
  const teams = ["alpha", "beta", "gamma"];
  const workspaceRepos: Record<string, RepoState> = {};
  for (const team of teams) {
    const key = `workspace:acme:${team}:owner/${team}`;
    workspaceRepos[key] = { repoPath: key, localPath: `/path/${team}` } as unknown as RepoState;
  }
  useOrgStore.setState({
    orgs: [
      {
        slug: "acme",
        display_name: "Acme",
        created_at: "2026-01-01T00:00:00Z",
        remote_url: null,
        config_flags: {},
        people: [],
        teams: teams.map((name) => ({ name, source_repos: [{ owner: "owner", repo: name }] })),
        org_root: "/orgs/acme",
      },
    ],
    workspaceRepos,
    activeOrgSlug: "acme",
    activeTeamName: activeTeam,
  });
}

function resetViewer() {
  useViewerStore.setState({
    stateLoaded: true,
    workspaces: {},
    activeWorkspace: {},
    softClosedWorkspaces: {},
    openedTeams: [],
    terminalVisible: false,
    layoutMode: null,
    lastTerminalMode: "shared",
    terminalSplitRatio: 0.55,
    cliExpanded: false,
    workspacePanelCollapsed: false,
    workspacePanelWidth: null,
  });
  loadAppStateMock.mockReset();
  saveAppStateMock.mockReset().mockResolvedValue(undefined);
}

/** What `persistState` writes. */
async function persistedPayload(): Promise<string> {
  let captured: string | null = null;
  saveAppStateMock.mockImplementation(async (payload: string) => {
    captured = payload;
  });
  useViewerStore.getState().persistState();
  await new Promise((resolve) => setTimeout(resolve, 400));
  expect(captured).not.toBeNull();
  return captured!;
}

/** Load a saved app state (its `viewer` and `org` sections) as a restart would. */
async function loadSaved(saved: object) {
  loadAppStateMock.mockResolvedValue(JSON.stringify(saved));
  await useViewerStore.getState().loadPersistedState();
}

function chrome() {
  const s = useViewerStore.getState();
  return {
    terminalVisible: s.terminalVisible,
    layoutMode: s.layoutMode,
    lastTerminalMode: s.lastTerminalMode,
    terminalSplitRatio: s.terminalSplitRatio,
  };
}

describe("D39: the CLI chrome is one setting for every team", () => {
  beforeEach(() => {
    resetViewer();
    seedOrg();
  });

  it("survives a restart whole, and is saved as one cliChrome rather than one per team", async () => {
    useViewerStore.setState({
      openedTeams: [TEAM_A, TEAM_B],
      workspaces: { [TEAM_A]: [story("A-1")], [TEAM_B]: [story("B-1")] },
      activeWorkspace: { [TEAM_A]: "A-1", [TEAM_B]: "B-1" },
    });
    const store = useViewerStore.getState();
    store.setLayoutMode("console");
    store.setTerminalSplitRatio(0.3);
    store.setCliExpanded(true);
    store.setWorkspacePanelCollapsed(true);
    store.setWorkspacePanelWidth(250);

    const payload = await persistedPayload();
    const viewer = JSON.parse(payload).viewer;
    expect(vi.mocked(commands.pushViewerState)).toHaveBeenCalledWith(
      expect.objectContaining({ cliChrome: viewer.cliChrome }),
    );
    expect(viewer.cliChrome).toEqual({
      visible: true,
      layoutMode: "console",
      lastTerminalMode: "console",
      splitRatio: 0.3,
      expanded: true,
      agentsDockCollapsed: true,
      agentsDockWidth: 250,
    });
    for (const key of ["terminalVisible", "layoutMode", "lastTerminalMode", "terminalSplitRatio"]) {
      expect(viewer).not.toHaveProperty(key);
    }

    resetViewer();
    seedOrg("beta");
    await loadSaved(JSON.parse(payload));
    expect(chrome()).toEqual({
      terminalVisible: true,
      layoutMode: "console",
      lastTerminalMode: "console",
      terminalSplitRatio: 0.3,
    });
    expect(useViewerStore.getState()).toMatchObject({
      cliExpanded: true,
      workspacePanelCollapsed: true,
      workspacePanelWidth: 250,
    });
  });

  it("a collapsed CLI comes back collapsed, with its last terminal mode kept for the reopen", async () => {
    const store = useViewerStore.getState();
    store.setLayoutMode("console");
    store.setLayoutMode("viewer");
    const payload = await persistedPayload();

    resetViewer();
    await loadSaved(JSON.parse(payload));
    expect(chrome()).toMatchObject({
      terminalVisible: false,
      layoutMode: "viewer",
      lastTerminalMode: "console",
    });
    useViewerStore.getState().expandTerminalOnOpen();
    expect(chrome()).toMatchObject({ terminalVisible: true, layoutMode: "console" });
  });

  it("seeds once from the team that was open when the app last closed", async () => {
    useViewerStore.setState({ stateLoaded: false });
    await loadSaved({
      org: { activeOrgSlug: "acme", activeTeamName: "beta" },
      viewer: {
        schemaVersion: 14,
        openedTeams: [TEAM_A, TEAM_B],
        terminalVisible: { [TEAM_A]: false, [TEAM_B]: true },
        layoutMode: { [TEAM_A]: "viewer", [TEAM_B]: "console" },
        lastTerminalMode: { [TEAM_A]: "shared", [TEAM_B]: "console" },
        terminalSplitRatio: { [TEAM_A]: 0.7, [TEAM_B]: 0.25 },
      },
    });
    expect(chrome()).toEqual({
      terminalVisible: true,
      layoutMode: "console",
      lastTerminalMode: "console",
      terminalSplitRatio: 0.25,
    });
    expect(commands.pushViewerState).toHaveBeenCalledWith(
      expect.objectContaining({
        cliChrome: expect.objectContaining({ layoutMode: "console", splitRatio: 0.25 }),
      }),
    );
    expect(saveAppStateMock).toHaveBeenCalled();

    const seeded = JSON.parse(saveAppStateMock.mock.calls[0][0]).viewer;
    expect(seeded.cliChrome).toMatchObject({ visible: true, layoutMode: "console" });
    expect(seeded.schemaVersion).toBe(15);
    // A later unchanged save leaves the committed seed alone.
    const viewer = JSON.parse(await persistedPayload()).viewer;
    expect(viewer).not.toHaveProperty("cliChrome");
    expect(viewer).not.toHaveProperty("terminalVisible");
    expect(viewer).not.toHaveProperty("schemaVersion");
  });

  it("falls back to the first saved team when the last open team saved no chrome", async () => {
    await loadSaved({
      org: { activeOrgSlug: "acme", activeTeamName: "gamma" },
      viewer: {
        schemaVersion: 14,
        openedTeams: [TEAM_B, TEAM_A, TEAM_C],
        terminalVisible: { [TEAM_A]: false, [TEAM_B]: true },
        layoutMode: { [TEAM_A]: "viewer", [TEAM_B]: "shared" },
        lastTerminalMode: { [TEAM_A]: "console", [TEAM_B]: "shared" },
        terminalSplitRatio: { [TEAM_A]: 0.7, [TEAM_B]: 0.4 },
      },
    });
    expect(chrome()).toEqual({
      terminalVisible: true,
      layoutMode: "shared",
      lastTerminalMode: "shared",
      terminalSplitRatio: 0.4,
    });
  });

  it("takes every field from the one seed team, defaulting what that team never saved", async () => {
    await loadSaved({
      org: { activeOrgSlug: "acme", activeTeamName: "alpha" },
      viewer: {
        schemaVersion: 14,
        openedTeams: [TEAM_A, TEAM_B],
        terminalVisible: { [TEAM_A]: true, [TEAM_B]: false },
        layoutMode: { [TEAM_B]: "console" },
        terminalSplitRatio: { [TEAM_B]: 0.2 },
      },
    });
    expect(chrome()).toEqual({
      terminalVisible: true,
      layoutMode: null,
      lastTerminalMode: "shared",
      terminalSplitRatio: 0.55,
    });
  });

  it("ignores saved chrome values of the wrong type without touching unrelated settings", async () => {
    await loadSaved({
      viewer: {
        schemaVersion: 15,
        cliChrome: {
          visible: "yes",
          layoutMode: "fullscreen",
          lastTerminalMode: "viewer",
          splitRatio: "bad",
        },
        rightPaneOpen: true,
      },
    });
    expect(chrome()).toEqual({
      terminalVisible: false,
      layoutMode: null,
      lastTerminalMode: "shared",
      terminalSplitRatio: 0.55,
    });
    expect(useViewerStore.getState().rightPaneOpen).toBe(true);
  });

  it("loads a full-screen or collapsed layout with the visibility it implies", async () => {
    await loadSaved({
      viewer: {
        schemaVersion: 15,
        cliChrome: { layoutMode: "console", visible: false, lastTerminalMode: "shared" },
      },
    });
    // An explicit saved reopen mode survives independently of current full screen.
    expect(chrome()).toMatchObject({
      layoutMode: "console",
      terminalVisible: true,
      lastTerminalMode: "shared",
    });

    resetViewer();
    await loadSaved({
      viewer: { schemaVersion: 15, cliChrome: { layoutMode: "viewer", visible: true } },
    });
    expect(chrome()).toMatchObject({ layoutMode: "viewer", terminalVisible: false });
  });

  it("preserves an explicit reopen mode across workspace opens and a restart", async () => {
    useViewerStore.setState({
      openedTeams: [TEAM_A],
      workspaces: { [TEAM_A]: [story("A-1")] },
      activeWorkspace: { [TEAM_A]: "A-1" },
      layoutMode: "console",
      terminalVisible: true,
      lastTerminalMode: "shared",
    });
    useViewerStore.getState().openWorkspace({
      teamKey: TEAM_A,
      workspaceId: "A-2",
      kind: "generic",
      cwd: "/a2",
      spawn: false,
    });
    expect(chrome()).toMatchObject({ layoutMode: "console", lastTerminalMode: "shared" });
    const saved = await persistedPayload();
    resetViewer();
    await loadSaved(JSON.parse(saved));
    expect(chrome()).toMatchObject({ layoutMode: "console", lastTerminalMode: "shared" });
    useViewerStore.getState().setLayoutMode("viewer");
    useViewerStore.getState().expandTerminalOnOpen(TEAM_A);
    expect(chrome()).toMatchObject({ layoutMode: "shared", terminalVisible: true });
  });

  it("does not automatically overwrite a save from a newer schema", async () => {
    await loadSaved({
      viewer: { schemaVersion: 16, cliChrome: { layoutMode: "shared" }, futureSetting: "keep" },
    });
    expect(saveAppStateMock).not.toHaveBeenCalled();
  });

  it("seeds from the last open team of a save that still has a legacy owner--repo org slug", async () => {
    await loadSaved({
      org: { activeOrgSlug: "acme--ctx", activeTeamName: "beta" },
      viewer: {
        schemaVersion: 14,
        openedTeams: ["acme/ctx:alpha", "acme/ctx:beta"],
        layoutMode: { "acme/ctx:alpha": "viewer", "acme/ctx:beta": "console" },
      },
    });
    expect(chrome()).toMatchObject({ layoutMode: "console", terminalVisible: true });
  });

  it("opening a file for a team that is not on screen leaves full-screen CLI alone", () => {
    useViewerStore.setState({
      openedTeams: [TEAM_A, TEAM_B],
      workspaces: { [TEAM_A]: [story("A-1")], [TEAM_B]: [story("B-1")] },
      activeWorkspace: { [TEAM_A]: "A-1", [TEAM_B]: "B-1" },
    });
    useViewerStore.getState().setLayoutMode("console");

    useViewerStore.getState().openFile(TEAM_B, "workspace:acme:beta:owner/beta", "/b/doc.md");
    expect(chrome()).toMatchObject({ layoutMode: "console" });

    // An open the user did not ask for (a redirect, a launch landing) keeps it too.
    useViewerStore
      .getState()
      .openFile(TEAM_A, "workspace:acme:alpha:owner/alpha", "/a/landing.md", { keepLayout: true });
    expect(chrome()).toMatchObject({ layoutMode: "console" });

    // An open in the team on screen still steps out, so the file is not behind the terminals.
    useViewerStore.getState().openFile(TEAM_A, "workspace:acme:alpha:owner/alpha", "/a/doc.md");
    expect(chrome()).toMatchObject({ layoutMode: "shared", terminalVisible: true });
  });

  it("a team view's first settle leaves a saved chrome exactly as it loaded", async () => {
    // Shared and hidden, with console as the reopen mode (a launch revealed it in place).
    await loadSaved({
      viewer: {
        schemaVersion: 15,
        openedTeams: [TEAM_A, TEAM_B],
        cliChrome: {
          layoutMode: "shared",
          visible: false,
          lastTerminalMode: "console",
          splitRatio: 0.3,
        },
      },
    });
    const loaded = chrome();
    useViewerStore.getState().settleInitialLayout(TEAM_A);
    useViewerStore.getState().settleInitialLayout(TEAM_B);
    expect(chrome()).toEqual(loaded);
  });

  it("does not drift when the selected team changes: every team reads the same chrome", () => {
    useViewerStore.setState({
      openedTeams: [TEAM_A, TEAM_B],
      workspaces: { [TEAM_A]: [story("A-1")], [TEAM_B]: [] },
      activeWorkspace: { [TEAM_A]: "A-1" },
    });
    const store = useViewerStore.getState();
    store.settleInitialLayout(TEAM_A);
    store.setLayoutMode("console");
    store.setTerminalSplitRatio(0.35);
    const before = chrome();

    // Team B is selected and its view settles for the first time: B has no workspace, but the
    // chrome is already set, so it stays as it is.
    seedOrg("beta");
    useViewerStore.getState().settleInitialLayout(TEAM_B);
    expect(chrome()).toEqual(before);

    // Collapsing the CLI in B collapses it for A too.
    useViewerStore.getState().setLayoutMode("viewer");
    seedOrg("alpha");
    expect(chrome()).toMatchObject({ terminalVisible: false, layoutMode: "viewer" });
  });

  it("closing or renaming a team keeps the chrome", () => {
    useViewerStore.setState({
      openedTeams: [TEAM_A, TEAM_B],
      workspaces: { [TEAM_A]: [story("A-1")], [TEAM_B]: [story("B-1")] },
      activeWorkspace: { [TEAM_A]: "A-1", [TEAM_B]: "B-1" },
    });
    useViewerStore.getState().setLayoutMode("console");
    useViewerStore.getState().setTerminalSplitRatio(0.4);
    const before = chrome();

    useViewerStore.getState().closeTeam(TEAM_A);
    expect(chrome()).toEqual(before);
    useViewerStore.getState().rekeyTeam("acme", "beta", "delta");
    expect(chrome()).toEqual(before);
  });

  it("closing any team's last workspace keeps full-screen CLI, the team on screen too", () => {
    useViewerStore.setState({
      openedTeams: [TEAM_A, TEAM_B],
      workspaces: { [TEAM_A]: [story("A-1")], [TEAM_B]: [story("B-1")] },
      activeWorkspace: { [TEAM_A]: "A-1", [TEAM_B]: "B-1" },
    });
    useViewerStore.getState().setLayoutMode("console");
    const before = chrome();

    // Emptying the team on screen, or another, leaves full screen on.
    useViewerStore.getState().closeWorkspace(TEAM_A, "A-1");
    expect(chrome()).toEqual(before);
    useViewerStore.getState().closeWorkspace(TEAM_B, "B-1");
    expect(chrome()).toEqual(before);

    // A workspace opened later keeps it too; the user's toggle leaves it.
    useViewerStore.setState({ lastTerminalMode: "shared" });
    useViewerStore.getState().expandTerminalOnOpen();
    expect(chrome()).toMatchObject({ layoutMode: "console", terminalVisible: true });
    useViewerStore.getState().toggleConsoleShared();
    expect(chrome()).toMatchObject({ layoutMode: "shared", terminalVisible: true });
  });

  it("a minimized workspace reopens in the current global mode", () => {
    useViewerStore.setState({
      openedTeams: [TEAM_A],
      workspaces: { [TEAM_A]: [story("A-1"), story("A-2")] },
      activeWorkspace: { [TEAM_A]: "A-1" },
    });
    seedTerminalTabs(TEAM_A, "A-2", ["sess-a2"]);
    useViewerStore.getState().setLayoutMode("console");
    // A close keeps a workspace with running terminals minimized.
    useViewerStore.getState().closeWorkspace(TEAM_A, "A-2");
    expect(useViewerStore.getState().softClosedWorkspaces[TEAM_A]?.["A-2"]).not.toHaveProperty(
      "layoutMode",
    );

    useViewerStore.getState().setLayoutMode("shared");
    useViewerStore.getState().reopenSoftClosedWorkspace(TEAM_A, "A-2");
    expect(chrome()).toMatchObject({ layoutMode: "shared", lastTerminalMode: "shared" });
  });
  it("background opens, reveals, and expansion keep global chrome unchanged", () => {
    useViewerStore.setState({
      workspaces: { [TEAM_B]: [story("B-1")] },
      layoutMode: "viewer",
      terminalVisible: false,
      cliExpanded: false,
    });
    const before = chrome();
    useViewerStore.getState().expandTerminalOnOpen(TEAM_B);
    useViewerStore.getState().revealTerminalInPlace(TEAM_B);
    useViewerStore.getState().expandWorkspace(TEAM_B, "B-1");
    useViewerStore.getState().openWorkspace({
      teamKey: TEAM_B,
      workspaceId: "B-2",
      kind: "generic",
      cwd: "/b2",
      spawn: false,
    });
    expect(chrome()).toEqual(before);
    expect(useViewerStore.getState().cliExpanded).toBe(false);
  });
});

it("durably seeds a partially upgraded v15 save with only a grouped rail zoom", async () => {
  resetViewer();
  seedOrg();
  useViewerStore.setState({ stateLoaded: false });
  await loadSaved({
    org: { activeOrgSlug: "acme", activeTeamName: "alpha" },
    viewer: {
      schemaVersion: 15,
      openedTeams: [TEAM_A],
      trayMode: { [TEAM_A]: "grouped" },
      foregroundLayoutByRail: { [TEAM_A]: { "": { zoomedTileId: "w" } } },
    },
  });
  expect(useViewerStore.getState().cliExpanded).toBe(true);
  expect(saveAppStateMock).toHaveBeenCalled();
  const saved = JSON.parse(saveAppStateMock.mock.calls[0][0]);
  expect(saved.viewer.cliChrome.expanded).toBe(true);
  const committed = applyFieldPatch(JSON.parse(await commands.loadAppState()), saved) as {
    viewer: unknown;
  };
  expect(JSON.stringify(committed.viewer)).not.toContain("zoomedTileId");
});

it("selects the saved active team after resolving multiple pre-T4 repo buckets", async () => {
  resetViewer();
  seedOrg("beta");
  useViewerStore.setState({ stateLoaded: false });
  const alpha = "workspace:acme:alpha:owner/alpha";
  const beta = "workspace:acme:beta:owner/beta";
  await loadSaved({
    org: { activeOrgSlug: "acme", activeTeamName: "beta" },
    viewer: {
      schemaVersion: 1,
      openedTeams: [TEAM_A, TEAM_B],
      layoutMode: { [alpha]: "viewer", [beta]: "console" },
      terminalVisible: { [alpha]: false, [beta]: true },
      terminalSplitRatio: { [alpha]: 0.7, [beta]: 0.25 },
    },
  });
  expect(chrome()).toMatchObject({
    layoutMode: "console",
    terminalVisible: true,
    terminalSplitRatio: 0.25,
  });
  expect(saveAppStateMock).toHaveBeenCalled();
  const saved = JSON.parse(saveAppStateMock.mock.calls[0][0]);
  expect(saved.viewer.cliChrome).toMatchObject({ layoutMode: "console", splitRatio: 0.25 });
});

it("background placement and missing expansion targets leave the shown team's zoom alone", () => {
  resetViewer();
  seedOrg();
  useViewerStore.setState({
    workspaces: { [TEAM_A]: [story("a"), story("b")], [TEAM_B]: [story("c"), story("d")] },
    activeWorkspace: { [TEAM_A]: "a", [TEAM_B]: "c" },
    cliExpanded: true,
  });
  useViewerStore
    .getState()
    .placeWorkspaceTile(TEAM_B, "d", { targetWorkspaceId: "c", axis: "column", side: "before" });
  expect(useViewerStore.getState().cliExpanded).toBe(true);
  useViewerStore.getState().setCliExpanded(false);
  useViewerStore.getState().expandWorkspace(TEAM_A, "missing");
  expect(useViewerStore.getState().cliExpanded).toBe(false);
});

it("visibility setters keep the render mode paired and split ratios round-trip within the divider range", async () => {
  resetViewer();
  seedOrg();
  const store = useViewerStore.getState();
  store.setLayoutMode("console");
  store.setTerminalVisible(false);
  expect(chrome()).toMatchObject({ layoutMode: "viewer", terminalVisible: false });
  store.setTerminalVisible(true);
  expect(chrome()).toMatchObject({ layoutMode: "console", terminalVisible: true });
  store.setTerminalSplitRatio(0.95);
  expect(chrome().terminalSplitRatio).toBe(0.9);
  const saved = JSON.parse(await persistedPayload());
  resetViewer();
  await loadSaved(saved);
  expect(chrome().terminalSplitRatio).toBe(0.9);
});
