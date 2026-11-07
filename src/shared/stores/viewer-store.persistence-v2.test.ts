// SPDX-License-Identifier: MIT
vi.mock("@/features/dashboard/lib/prefs-client", () => ({
  readPrefsState: async () => JSON.parse((await commands.loadAppState()) || "{}"),
  usePrefsStore: { getState: () => ({ principal: null, globalReady: false, ownedReady: false }) },
}));
import { describe, it, expect, beforeEach, vi } from "vitest";
import {
  expandedWorkspaceId,
  migrateT1Slices,
  promptIdentityFromRepoPath,
  selectActiveTerminalTab,
  selectWorkspaceTabs,
  shouldRenderTabGrid,
  teamTileOrder,
  useViewerStore,
  resetViewerPrefsPersistence,
  applyOwnedViewerPrefs,
  workspacePanedTabIds,
  viewerMirrorFields,
  type Workspace,
} from "./viewer-store";
import { useOrgStore } from "./org-store";
import * as commands from "@/tauri/commands";
import { buildRootWorkspaceId, NO_TEAM_SENTINEL } from "@/shared/lib/team-key";
import { EMPTY_TERMINAL_LAYOUT } from "@/shared/lib/terminal-layout-projection";
import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";
import type { RepoState } from "./config-store";
import {
  seedTerminalTabs,
  installTerminalLayoutHarness,
  refetchTestLayout,
} from "@/test/terminal-layout-harness";
import { sessionPinId } from "@/shared/lib/agent-pins";

describe("host-aware viewer identity", () => {
  const workspace = (sourceRepoSlug: string): Workspace => ({
    id: sourceRepoSlug,
    kind: "story",
    cwd: `/worktrees/${sourceRepoSlug}`,
    status: "ready",
    sourceRepoSlug,
  });

  const migrate = (sourceRepoSlug: string, workspaceRepos: Record<string, { repoPath: string }>) =>
    migrateT1Slices({
      workspaces: { "/legacy/repo": [workspace(sourceRepoSlug)] },
      activeWorkspace: {},
      workspaceRepos,
    });

  it("round-trips a non-GitHub host through the prompt identity producer", () => {
    expect(promptIdentityFromRepoPath("workspace:acme:platform:gitlab.com/Acme/Api")).toEqual({
      teamKey: "acme:platform",
      sourceRepoSlug: "gitlab.com/acme/api",
    });
    expect(promptIdentityFromRepoPath("workspace:acme:platform:Acme/Api")).toEqual({
      teamKey: "acme:platform",
      sourceRepoSlug: "acme/api",
    });
  });

  it("routes exact host identities without hostless twin overwrite", () => {
    const repos = {
      github: { repoPath: "workspace:acme:github:acme/api" },
      gitlab: { repoPath: "workspace:acme:gitlab:gitlab.com/acme/api" },
    };

    expect(migrate("acme/api", repos).workspaces["acme:github"]).toHaveLength(1);
    expect(migrate("gitlab.com/acme/api", repos).workspaces["acme:gitlab"]).toHaveLength(1);
  });

  it("sends an ambiguous hostless non-GitHub fallback to the sentinel", () => {
    const result = migrate("acme/api", {
      gitlab: { repoPath: "workspace:acme:gitlab:gitlab.com/acme/api" },
      selfHosted: { repoPath: "workspace:acme:self-hosted:git.example.com/acme/api" },
    });

    expect(result.workspaces["_no_team:_none"]).toHaveLength(1);
    expect(result.workspaces["acme:gitlab"]).toBeUndefined();
    expect(result.workspaces["acme:self-hosted"]).toBeUndefined();
  });
});

// The persist → reload round-trip. Terminals are the host's, so the saved
// viewer carries none of them; minimized workspaces round-trip as records.

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

const loadAppStateMock = commands.loadAppState as ReturnType<typeof vi.fn>;
const saveAppStateMock = commands.saveAppState as ReturnType<typeof vi.fn>;

const TEAM_KEY = "acme:default";
const SLUG_A = "owner/a";
const ROOT_A = buildRootWorkspaceId(SLUG_A);

installTerminalLayoutHarness();

function seedOrg(slugs: string[]) {
  const workspaceRepos: Record<string, RepoState> = {};
  const source_repos = slugs.map((s) => {
    const [owner, repo] = s.split("/");
    return { owner, repo };
  });
  for (const s of slugs) {
    const key = `workspace:acme:default:${s}`;
    workspaceRepos[key] = {
      repoPath: key,
      localPath: `/path/${s}`,
    } as unknown as RepoState;
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
        teams: [
          {
            name: "default",
            source_repos,
          },
        ],
        org_root: "/orgs/acme",
      },
    ],
    workspaceRepos,
    activeOrgSlug: "acme",
    activeTeamName: "default",
  });
}

function resetViewer() {
  resetViewerPrefsPersistence();
  useViewerStore.setState({
    // Persisting is a no-op until the saved state has loaded (SW-FU-B2).
    stateLoaded: true,
    workspaces: {},
    activeWorkspace: {},
    softClosedWorkspaces: {},
    openedTeams: [],
  });
  loadAppStateMock.mockReset();
  saveAppStateMock.mockReset().mockResolvedValue(undefined);
}

it("persists only edited setting fields, never cached settings or unrelated app-state keys", async () => {
  resetViewer();
  loadAppStateMock.mockResolvedValue(
    JSON.stringify({
      viewer: { settings: { editorLineWrap: "off", newerOutsideField: "keep" } },
      unrelated: { newer: true },
    }),
  );
  useViewerStore.getState().updateSettings({ editorLineWrap: "on" });
  await useViewerStore.getState().flushPersist();
  const [payload, patch] = saveAppStateMock.mock.calls.at(-1)!;
  const saved = JSON.parse(payload);
  expect(saved.viewer.settings).toEqual({ editorLineWrap: "on" });
  expect(saved).not.toHaveProperty("unrelated");
  expect(patch).toBe(true);
});

/** Apply the captured field patches as the native writer does. */
function committedViewerState(initial: Record<string, unknown> = {}) {
  const merge = (before: Record<string, unknown>, patch: Record<string, unknown>) => {
    for (const [key, value] of Object.entries(patch)) {
      if (value === null) delete before[key];
      else if (typeof value === "object" && !Array.isArray(value)) {
        const nested = (before[key] ??= {}) as Record<string, unknown>;
        merge(nested, value as Record<string, unknown>);
      } else before[key] = value;
    }
    return before;
  };
  return saveAppStateMock.mock.calls.reduce(
    (state, [raw]) => merge(state, JSON.parse(raw)),
    initial,
  ) as { viewer: Record<string, unknown> };
}

/** What `persistState` commits, after applying patches. */
async function persisted(): Promise<{ viewer: Record<string, unknown> }> {
  let capturedPayload: string | null = null;
  saveAppStateMock.mockImplementation(async (payload: string) => {
    capturedPayload = payload;
  });
  useViewerStore.getState().persistState();
  // Allow the debounced persist to fire.
  await new Promise((resolve) => setTimeout(resolve, 400));
  expect(capturedPayload).not.toBeNull();
  return committedViewerState();
}

describe("viewer persistence round-trip", () => {
  beforeEach(resetViewer);

  const story = (id: string, fields: Partial<Workspace> = {}): Workspace => ({
    id,
    kind: "story",
    cwd: `/wt/${id}`,
    storyFilePath: `/stories/${id}.md`,
    storyId: id,
    status: "ready",
    ...fields,
  });

  it("AC-1: the persisted viewer carries NO terminal fields — the host owns the terminals", async () => {
    seedOrg([SLUG_A]);
    useViewerStore.setState({
      workspaces: { [TEAM_KEY]: [story("S-1"), story("S-2")] },
      activeWorkspace: { [TEAM_KEY]: "S-1" },
    });
    seedTerminalTabs(TEAM_KEY, "S-1", ["sess-a", "sess-b"], {
      active: "sess-b",
      paned: ["sess-a"],
    });
    seedTerminalTabs(TEAM_KEY, "S-2", ["sess-c"]);

    const parsed = await persisted();
    // SS-FU-12 … D39: the persist site writes the current schemaVersion.
    expect(parsed.viewer.schemaVersion).toBe(15);
    for (const legacy of [
      "terminalTabs",
      "activeTerminalTab",
      "shellTabCounter",
      "terminalTabsByWorkspace",
      "activeTerminalTabByWorkspace",
      "shellTabCounterByWorkspace",
      "panedTabsByWorkspace",
      "tabGridPreset",
      "tabGridCollapsed",
      "terminalLayout",
    ]) {
      expect(parsed.viewer).not.toHaveProperty(legacy);
    }
    // No terminal slice under any other name either: no session id is written.
    const blob = JSON.stringify(parsed.viewer);
    for (const session of ["sess-a", "sess-b", "sess-c"]) expect(blob).not.toContain(session);
  });

  it("AC-1: softClosedWorkspaces round-trips the workspace without global chrome", async () => {
    seedOrg([SLUG_A]);
    useViewerStore.setState({
      workspaces: { [TEAM_KEY]: [] },
      softClosedWorkspaces: {
        [TEAM_KEY]: {
          "S-1": {
            workspace: story("S-1", { statusError: "stale", backgroundSetup: true } as never),
          },
          "S-2": { workspace: story("S-2") },
          // Transient (not ready) records are never persisted.
          "S-3": { workspace: story("S-3", { status: "creating" }) },
        },
      },
    });
    seedTerminalTabs(TEAM_KEY, "S-1", ["sess-min"]);

    const parsed = await persisted();
    const saved = parsed.viewer.softClosedWorkspaces as Record<string, Record<string, unknown>>;
    expect(saved[TEAM_KEY]).toEqual({
      "S-1": { workspace: story("S-1") },
      "S-2": { workspace: story("S-2") },
    });
    expect(JSON.stringify(saved)).not.toContain("sess-min");

    resetViewer();
    seedOrg([SLUG_A]);
    loadAppStateMock.mockResolvedValue(JSON.stringify(parsed));
    await useViewerStore.getState().loadPersistedState();

    // A workspace minimized at quit comes back minimized — the same record.
    expect(useViewerStore.getState().softClosedWorkspaces[TEAM_KEY]).toEqual({
      "S-1": { workspace: story("S-1") },
      "S-2": { workspace: story("S-2") },
    });
  });

  it("round-trips remote grid arrangement and divider sizes before reconnect", async () => {
    seedOrg([SLUG_A]);
    const key = `${TEAM_KEY}|S-1`;
    const grids = {
      [key]: {
        paned: [{ session: "remote-a", member: "box/remote" }],
        preset: "rows" as const,
        collapsed: true,
      },
    };
    const sizes = {
      [key]: {
        preset: "rows" as const,
        root: {
          kind: "split" as const,
          axis: "column" as const,
          children: [
            { kind: "leaf" as const, workspaceId: "remote-a" },
            { kind: "leaf" as const, workspaceId: "remote-b" },
          ],
          sizes: [30, 70],
        },
      },
    };
    useViewerStore.setState({
      workspaces: { [TEAM_KEY]: [story("S-1")] },
      terminalLayout: { ...EMPTY_TERMINAL_LAYOUT, grids },
      tabGridLayout: sizes,
    });
    const parsed = await persisted();
    expect(parsed.viewer.terminalViewerGrid).toEqual(grids);
    expect(parsed.viewer.terminalTabGridSizes).toEqual(sizes);
    resetViewer();
    useViewerStore.setState({ terminalLayout: EMPTY_TERMINAL_LAYOUT, tabGridLayout: {} });
    loadAppStateMock.mockResolvedValue(JSON.stringify(parsed));
    await useViewerStore.getState().loadPersistedState();
    expect(useViewerStore.getState().terminalLayout.grids).toEqual(grids);
    expect(useViewerStore.getState().tabGridLayout).toEqual(sizes);
  });

  it("round-trips this viewer's remote tab focus before its Machine reconnects", async () => {
    seedOrg([SLUG_A]);
    useViewerStore.setState({
      terminalLayout: EMPTY_TERMINAL_LAYOUT,
      workspaces: { [TEAM_KEY]: [story("S-1")] },
      activeWorkspace: { [TEAM_KEY]: "S-1" },
    });
    const key = `${TEAM_KEY}|S-1`;
    useViewerStore.getState().applyLayout({ kind: "focused", key, session: "remote-second" });
    const parsed = await persisted();
    expect(parsed.viewer.terminalViewerFocus).toEqual({ [key]: "remote-second" });

    resetViewer();
    useViewerStore.setState({ terminalLayout: EMPTY_TERMINAL_LAYOUT });
    loadAppStateMock.mockResolvedValue(JSON.stringify(parsed));
    await useViewerStore.getState().loadPersistedState();
    expect(useViewerStore.getState().terminalLayout.focus).toEqual({ [key]: "remote-second" });
    // Choosing a local tab clears the override in the next saved state.
    useViewerStore.getState().applyLayout({ kind: "focused", key, session: null });
    expect((await persisted()).viewer.terminalViewerFocus).toEqual({});
  });

  it("choosing the displayed local tab clears and autosaves a pending remote focus", async () => {
    seedOrg([SLUG_A]);
    useViewerStore.setState({
      workspaces: { [TEAM_KEY]: [story("S-1")] },
      activeWorkspace: { [TEAM_KEY]: "S-1" },
    });
    const key = seedTerminalTabs(TEAM_KEY, "S-1", ["local-first"]);
    await useViewerStore.getState().flushPersist();
    useViewerStore.getState().applyLayout({ kind: "focused", key, session: "remote-later" });
    expect(selectActiveTerminalTab(useViewerStore.getState(), TEAM_KEY, "S-1")).toBe("local-first");
    const before = committedViewerState();
    saveAppStateMock.mockClear();
    useViewerStore.getState().setActiveTerminalTab(TEAM_KEY, "local-first", "S-1");
    await new Promise((resolve) => setTimeout(resolve, 450));
    expect(useViewerStore.getState().terminalLayout.focus).toEqual({});
    expect(saveAppStateMock).toHaveBeenCalled();
    const saved = committedViewerState(before);
    expect(saved.viewer.terminalViewerFocus).toEqual({});
  });

  it.each([null, [], "bad", { empty: "", number: 7, nil: null, valid: "remote" }])(
    "validates saved remote focus %j",
    async (terminalViewerFocus) => {
      seedOrg([SLUG_A]);
      useViewerStore.setState({ terminalLayout: EMPTY_TERMINAL_LAYOUT });
      loadAppStateMock.mockResolvedValue(
        JSON.stringify({ viewer: { schemaVersion: 15, terminalViewerFocus } }),
      );
      await useViewerStore.getState().loadPersistedState();
      expect(useViewerStore.getState().terminalLayout.focus).toEqual(
        terminalViewerFocus &&
          !Array.isArray(terminalViewerFocus) &&
          typeof terminalViewerFocus === "object"
          ? { valid: "remote" }
          : {},
      );
    },
  );

  it("the MCP mirror gets the open workspaces at launch (what open_terminal_tab resolves)", async () => {
    seedOrg([SLUG_A]);
    loadAppStateMock.mockResolvedValue(
      JSON.stringify({
        viewer: {
          schemaVersion: 15,
          workspaces: { [TEAM_KEY]: [story("S-1")] },
          activeWorkspace: { [TEAM_KEY]: "S-1" },
          cliChrome: { layoutMode: "console", splitRatio: 0.3, expanded: true },
        },
      }),
    );
    const push = commands.pushViewerState as ReturnType<typeof vi.fn>;
    push.mockClear();
    await useViewerStore.getState().loadPersistedState();
    expect(push).toHaveBeenCalledWith(
      expect.objectContaining({
        cliChrome: expect.objectContaining({
          layoutMode: "console",
          splitRatio: 0.3,
          expanded: true,
        }),
      }),
    );
    const mirrored = push.mock.calls[0][0] as {
      workspaces: Record<string, Array<Record<string, unknown>>>;
    };
    // `find_open_workspace` (handler.rs) reads id, kind, storyId and cwd.
    expect(mirrored.workspaces[TEAM_KEY][0]).toMatchObject({
      id: "S-1",
      kind: "story",
      cwd: story("S-1").cwd,
    });
  });

  it("the Agents dock's fold and width survive a reload, in the one cliChrome", async () => {
    seedOrg([SLUG_A]);
    useViewerStore.setState({
      workspaces: { [TEAM_KEY]: [] },
      activeWorkspace: {},
      workspacePanelCollapsed: true,
      workspacePanelWidth: 320,
    });

    let capturedPayload: string | null = null;
    saveAppStateMock.mockImplementation(async (payload: string) => {
      capturedPayload = payload;
    });
    useViewerStore.getState().persistState();
    await new Promise((resolve) => setTimeout(resolve, 400));

    const parsed = JSON.parse(capturedPayload!);
    expect(parsed.viewer.cliChrome).toMatchObject({
      agentsDockCollapsed: true,
      agentsDockWidth: 320,
    });
    expect(parsed.viewer).not.toHaveProperty("workspacePanelCollapsed");
    expect(parsed.viewer).not.toHaveProperty("workspacePanelWidth");

    resetViewer();
    seedOrg([SLUG_A]);
    loadAppStateMock.mockResolvedValue(capturedPayload!);
    await useViewerStore.getState().loadPersistedState();

    const state = useViewerStore.getState();
    expect(state.workspacePanelCollapsed).toBe(true);
    expect(state.workspacePanelWidth).toBe(320);
  });

  it("the agents pane's folds are one map for every team, saved by ui:dock, not the viewer", async () => {
    seedOrg([SLUG_A]);
    useViewerStore.setState({
      workspaces: { [TEAM_KEY]: [] },
      activeWorkspace: {},
      agentsPaneFolds: {},
    });
    const { setAgentsPaneFold } = useViewerStore.getState();
    setAgentsPaneFold(`#team-fold:${TEAM_KEY}`, true);
    setAgentsPaneFold("#team-fold:acme:ops", true);
    setAgentsPaneFold(`#ws-fold:${TEAM_KEY}|ws-a`, true);
    setAgentsPaneFold(`#ws-fold:${TEAM_KEY}|ws-a`, false);
    expect(useViewerStore.getState().agentsPaneFolds).toEqual({
      [`#team-fold:${TEAM_KEY}`]: true,
      "#team-fold:acme:ops": true,
    });

    let capturedPayload: string | null = null;
    saveAppStateMock.mockImplementation(async (payload: string) => {
      capturedPayload = payload;
    });
    useViewerStore.getState().persistState();
    await new Promise((resolve) => setTimeout(resolve, 400));
    const saved = JSON.parse(capturedPayload!);
    // ui:dock owns the folds; the viewer neither saves nor reads them.
    expect(saved.viewer.agentsPaneFolds).toBeUndefined();
    saved.viewer.agentsPaneFolds = { [`#team-fold:${TEAM_KEY}`]: true };
    resetViewer();
    useViewerStore.setState({ agentsPaneFolds: {} });
    seedOrg([SLUG_A]);
    loadAppStateMock.mockResolvedValue(JSON.stringify(saved));
    await useViewerStore.getState().loadPersistedState();
    expect(useViewerStore.getState().agentsPaneFolds).toEqual({});
  });

  it("the shared workspace order and its base survive a reload, cleaned of junk, repeats and gone workspaces", async () => {
    seedOrg([SLUG_A]);
    useViewerStore.setState({
      workspaces: { [TEAM_KEY]: [story("S-1"), story("S-2"), story("S-3")] },
      softClosedWorkspaces: {
        [TEAM_KEY]: { "M-1": { workspace: story("M-1") } },
      },
      activeWorkspace: { [TEAM_KEY]: "S-1" },
      // As the team view's tree sync leaves it.
      trayBaseOrderByTeam: { [TEAM_KEY]: ["S-1", "S-2", "S-3", "M-1"] },
      workspaceOrderByTeam: {},
    });
    expect(useViewerStore.getState().reorderWorkspace(TEAM_KEY, "S-3", { before: "S-1" })).toBe(
      true,
    );
    const saved = await persisted();
    expect(saved.viewer.workspaceOrderByTeam).toEqual({ [TEAM_KEY]: ["S-3", "S-1", "S-2", "M-1"] });
    expect(saved.viewer.trayBaseOrderByTeam).toEqual({ [TEAM_KEY]: ["S-1", "S-2", "S-3", "M-1"] });

    // Junk, a repeat and a workspace that does not come back are dropped on load.
    const orders = saved.viewer.workspaceOrderByTeam as Record<string, unknown>;
    orders[TEAM_KEY] = ["S-3", 7, "S-gone", "S-1", "S-3", "S-2", "M-1"];
    orders["acme:ops"] = "not a list";
    resetViewer();
    useViewerStore.setState({ workspaceOrderByTeam: {}, trayBaseOrderByTeam: {} });
    seedOrg([SLUG_A]);
    loadAppStateMock.mockResolvedValue(JSON.stringify(saved));
    await useViewerStore.getState().loadPersistedState();
    expect(useViewerStore.getState().workspaceOrderByTeam).toEqual({
      [TEAM_KEY]: ["S-3", "S-1", "S-2", "M-1"],
    });
    // The base comes back too, so the order holds before the tree syncs again.
    expect(useViewerStore.getState().trayBaseOrderByTeam).toEqual({
      [TEAM_KEY]: ["S-1", "S-2", "S-3", "M-1"],
    });
  });

  it("a workspace closed for good leaves the order; a minimized one keeps its place", () => {
    useViewerStore.setState({
      workspaces: { [TEAM_KEY]: [story("S-1"), story("S-2"), story("S-3")] },
      softClosedWorkspaces: {
        [TEAM_KEY]: { "M-1": { workspace: story("M-1") } },
      },
      activeWorkspace: { [TEAM_KEY]: "S-1" },
      workspaceOrderByTeam: { [TEAM_KEY]: ["S-3", "S-1", "S-2", "M-1"] },
    });
    seedTerminalTabs(TEAM_KEY, "S-1", ["sess-s1"]);
    seedTerminalTabs(TEAM_KEY, "M-1", ["sess-m1"]);
    const order = () => useViewerStore.getState().workspaceOrderByTeam[TEAM_KEY];
    // S-1 still runs a terminal: closing it minimizes it, in place.
    useViewerStore.getState().closeWorkspace(TEAM_KEY, "S-1");
    expect(order()).toEqual(["S-3", "S-1", "S-2", "M-1"]);
    // S-2 has nothing running: it is gone.
    useViewerStore.getState().closeWorkspace(TEAM_KEY, "S-2");
    expect(order()).toEqual(["S-3", "S-1", "M-1"]);
    // Ending a minimized workspace's terminals ends it for good.
    useViewerStore.getState().killTerminalsForSoftClosedWorkspace(TEAM_KEY, "M-1");
    expect(order()).toEqual(["S-3", "S-1"]);
  });

  it("closing a team drops its cached tile order", () => {
    const workspaces = [story("S-1"), story("S-2")];
    useViewerStore.setState({
      workspaces: { [TEAM_KEY]: workspaces },
      softClosedWorkspaces: {},
      workspaceOrderByTeam: {},
      trayBaseOrderByTeam: {},
      openedTeams: [TEAM_KEY],
    });
    const before = teamTileOrder(useViewerStore.getState(), TEAM_KEY);
    useViewerStore.getState().closeTeam(TEAM_KEY);
    // The same slices again: a kept cache entry would hand back the old array.
    useViewerStore.setState({ workspaces: { [TEAM_KEY]: workspaces } });
    const s = useViewerStore.getState();
    const after = teamTileOrder(
      { ...s, softClosedWorkspaces: {}, workspaceOrderByTeam: {}, trayBaseOrderByTeam: {} },
      TEAM_KEY,
    );
    expect(after).toEqual(before);
    expect(after).not.toBe(before);
  });

  it("pin edits keep team and order; legacy saved pins remain migration input", async () => {
    seedOrg([SLUG_A]);
    useViewerStore.setState({
      workspaces: { [TEAM_KEY]: [] },
      activeWorkspace: {},
      agentPins: {},
    });
    const { pinAgent, unpinAgents } = useViewerStore.getState();
    pinAgent(TEAM_KEY, {
      kind: "session",
      teamKey: TEAM_KEY,
      handle: "manager",
      sessionId: null,
      name: "Manager",
    });
    pinAgent(TEAM_KEY, {
      kind: "session",
      teamKey: TEAM_KEY,
      handle: "scratch",
      sessionId: "sid-s",
      name: "Scratch",
    });
    pinAgent(TEAM_KEY, {
      kind: "session",
      teamKey: TEAM_KEY,
      handle: "release",
      sessionId: null,
      name: "Release",
    });
    // Re-pinning updates in place: the pin keeps its position.
    pinAgent(TEAM_KEY, {
      kind: "session",
      teamKey: TEAM_KEY,
      handle: "manager",
      sessionId: "sid-m",
      name: "Manager",
    });
    unpinAgents(TEAM_KEY, [sessionPinId(TEAM_KEY, "release")]);
    const expected = [
      {
        kind: "session",
        teamKey: TEAM_KEY,
        handle: "manager",
        sessionId: "sid-m",
        name: "Manager",
      },
      {
        kind: "session",
        teamKey: TEAM_KEY,
        handle: "scratch",
        sessionId: "sid-s",
        name: "Scratch",
      },
    ];
    expect(useViewerStore.getState().agentPins[TEAM_KEY]).toEqual(expected);

    let capturedPayload: string | null = null;
    saveAppStateMock.mockImplementation(async (payload: string) => {
      capturedPayload = payload;
    });
    useViewerStore.getState().persistState();
    await new Promise((resolve) => setTimeout(resolve, 400));
    const saved = JSON.parse(capturedPayload!);
    expect(saved.viewer.agentPins).toBeUndefined();
    // Legacy viewer input remains readable for the one-time ui:dock migration.
    saved.viewer.agentPins = { [TEAM_KEY]: [...expected] };

    // A malformed saved pin is dropped on load; the rest come back.
    saved.viewer.agentPins[TEAM_KEY].push(
      { kind: "agent", agentKey: "acme/backend/lead", name: "Lead" },
      { kind: "session" },
      // A pin saved before pins named their team.
      { kind: "session", handle: "old", sessionId: null, name: "Old" },
      "junk",
    );
    resetViewer();
    useViewerStore.setState({ agentPins: {} });
    seedOrg([SLUG_A]);
    loadAppStateMock.mockResolvedValue(JSON.stringify(saved));
    await useViewerStore.getState().loadPersistedState();
    expect(useViewerStore.getState().agentPins[TEAM_KEY]).toEqual(expected);
  });

  it("reads the legacy Manager arrangement but never saves folds", async () => {
    seedOrg([SLUG_A]);
    useViewerStore.setState({
      workspaces: { [TEAM_KEY]: [story("S-1")] },
      activeWorkspace: { [TEAM_KEY]: "S-1" },
    });
    const state = useViewerStore.getState();
    state.setAgentsPaneArrange("manager");
    state.setAgentsPaneFold(`#manager-fold:${TEAM_KEY}|lead`, true);
    state.setAgentsPaneFold(`#ws-fold:${TEAM_KEY}|S-1`, true);
    const folds = useViewerStore.getState().agentsPaneFolds;
    const saved = await persisted();
    expect(saved.viewer.agentsPaneArrange).toBeUndefined();
    expect(saved.viewer.agentsPaneFolds).toBeUndefined();
    // ui:dock owns new writes; old viewer preferences remain migration input.
    saved.viewer.agentsPaneArrange = "manager";
    resetViewer();
    useViewerStore.setState({ agentsPaneArrange: "teams", agentsPaneFolds: {} });
    seedOrg([SLUG_A]);
    loadAppStateMock.mockResolvedValue(JSON.stringify(saved));
    await useViewerStore.getState().loadPersistedState();
    expect(useViewerStore.getState().agentsPaneArrange).toBe("manager");
    // Folds live in ui:dock, not the viewer.
    expect(folds).toMatchObject({
      [`#manager-fold:${TEAM_KEY}|lead`]: true,
      [`#ws-fold:${TEAM_KEY}|S-1`]: true,
    });
    expect(useViewerStore.getState().agentsPaneFolds).toEqual({});
  });

  it("saved group-mode, density and pane-view values are ignored on load, and unrelated settings survive", async () => {
    seedOrg([SLUG_A]);
    useViewerStore.setState({ workspaces: { [TEAM_KEY]: [] }, activeWorkspace: {} });
    useViewerStore.getState().setWorkspacePanelWidth(320);

    let capturedPayload: string | null = null;
    saveAppStateMock.mockImplementation(async (payload: string) => {
      capturedPayload = payload;
    });
    useViewerStore.getState().persistState();
    await new Promise((resolve) => setTimeout(resolve, 400));

    const reload = async (payload: string) => {
      resetViewer();
      seedOrg([SLUG_A]);
      loadAppStateMock.mockResolvedValue(payload);
      await useViewerStore.getState().loadPersistedState();
    };
    // Stale values from the retired switches: the manager and flat modes, a compact density,
    // and the Run history view. None of them is read back.
    const parsed = JSON.parse(capturedPayload!);
    parsed.viewer.agentListGroupMode = { [TEAM_KEY]: "manager" };
    parsed.viewer.agentListDensity = { [TEAM_KEY]: "compact" };
    parsed.viewer.agentsPaneView = { [TEAM_KEY]: "runs" };
    await reload(JSON.stringify(parsed));

    const state = useViewerStore.getState() as unknown as Record<string, unknown>;
    expect(state.agentListGroupMode).toBeUndefined();
    expect(state.agentListDensity).toBeUndefined();
    expect(state.agentsPaneView).toBeUndefined();
    // The width, which the pane still reads, comes back untouched.
    expect(useViewerStore.getState().workspacePanelWidth).toBe(320);
  });

  it("the pane's hidden teams and workspace arrow filter are saved; the explicit reveal is not", async () => {
    seedOrg([SLUG_A]);
    useViewerStore.setState({ workspaces: { [TEAM_KEY]: [] }, activeWorkspace: {} });
    useViewerStore.getState().setAgentsPaneTeamHidden("other:team", true);
    useViewerStore.getState().setAgentsWorkspaceFilter({ teamKey: TEAM_KEY, workspaceId: "w" });

    let capturedPayload: string | null = null;
    saveAppStateMock.mockImplementation(async (payload: string) => {
      capturedPayload = payload;
    });
    useViewerStore.getState().persistState();
    await new Promise((resolve) => setTimeout(resolve, 400));

    const saved = JSON.parse(capturedPayload!).viewer as Record<string, unknown>;
    expect(saved.agentsPaneHiddenTeams).toEqual(["other:team"]);
    expect(saved.agentsWorkspaceFilter).toEqual({ teamKey: TEAM_KEY, workspaceId: "w" });
    expect(saved.agentsWorkspaceReveal).toBeUndefined();
  });

  it.each([
    ["an open workspace", "S-1"],
    ["a minimized workspace", "S-2"],
    ["the team's agents with no workspace", ""],
    // Settled once the layouts that could bring it are read: the agents pane's rule (and the
    // app's own cleanup) drops it then if nothing can.
    ["a workspace not restored", "S-9"],
  ])("the arrow filter on %s comes back while its team exists", async (_what, workspaceId) => {
    seedOrg([SLUG_A]);
    const filter = { teamKey: TEAM_KEY, workspaceId };
    loadAppStateMock.mockResolvedValue(
      JSON.stringify({
        viewer: {
          schemaVersion: 10,
          workspaces: { [TEAM_KEY]: [story("S-1")] },
          activeWorkspace: { [TEAM_KEY]: "S-1" },
          softClosedWorkspaces: { [TEAM_KEY]: { "S-2": { workspace: story("S-2") } } },
          agentsWorkspaceFilter: filter,
        },
      }),
    );
    await useViewerStore.getState().loadPersistedState();
    expect(useViewerStore.getState().agentsWorkspaceFilter).toEqual(filter);
  });

  it("the arrow filter comes back knowing the machine its workspace runs on", async () => {
    seedOrg([SLUG_A]);
    const remote = story("S-3", {
      location: { kind: "machine", machineId: "box", requestId: "r", handle: "h", cwd: "/h" },
    });
    const load = (filter: object) => {
      loadAppStateMock.mockResolvedValue(
        JSON.stringify({
          viewer: {
            schemaVersion: 10,
            workspaces: { [TEAM_KEY]: [story("S-1"), remote] },
            activeWorkspace: { [TEAM_KEY]: "S-1" },
            agentsWorkspaceFilter: filter,
          },
        }),
      );
      return useViewerStore.getState().loadPersistedState();
    };
    // From its record, saved or not.
    await load({ teamKey: TEAM_KEY, workspaceId: "S-3" });
    expect(useViewerStore.getState().agentsWorkspaceFilter).toEqual({
      teamKey: TEAM_KEY,
      workspaceId: "S-3",
      machineId: "box",
    });
    // With no record here, the machine it was saved with.
    await load({ teamKey: TEAM_KEY, workspaceId: "S-9", machineId: "box" });
    expect(useViewerStore.getState().agentsWorkspaceFilter).toEqual({
      teamKey: TEAM_KEY,
      workspaceId: "S-9",
      machineId: "box",
    });
    // A local workspace's record wins over a stale machine.
    await load({ teamKey: TEAM_KEY, workspaceId: "S-1", machineId: "box" });
    expect(useViewerStore.getState().agentsWorkspaceFilter).toEqual({
      teamKey: TEAM_KEY,
      workspaceId: "S-1",
    });
  });

  it("setting the arrow filter on a remote workspace stamps its machine", () => {
    useViewerStore.setState({
      workspaces: {
        [TEAM_KEY]: [
          story("S-1"),
          story("S-3", { location: { kind: "machine", machineId: "box", requestId: "r" } }),
        ],
      },
      softClosedWorkspaces: {},
    });
    const set = useViewerStore.getState().setAgentsWorkspaceFilter;
    set({ teamKey: TEAM_KEY, workspaceId: "S-3" });
    expect(useViewerStore.getState().agentsWorkspaceFilter).toEqual({
      teamKey: TEAM_KEY,
      workspaceId: "S-3",
      machineId: "box",
    });
    set({ teamKey: TEAM_KEY, workspaceId: "S-1" });
    expect(useViewerStore.getState().agentsWorkspaceFilter).toEqual({
      teamKey: TEAM_KEY,
      workspaceId: "S-1",
    });
    set(null);
    expect(useViewerStore.getState().agentsWorkspaceFilter).toBeNull();
  });

  it.each([
    ["no-workspace", ""],
    ["workspace", "S-1"],
  ])("the %s filter of a team that no longer exists does not come back", async (_what, id) => {
    seedOrg([SLUG_A]);
    loadAppStateMock.mockResolvedValue(
      JSON.stringify({
        viewer: {
          schemaVersion: 10,
          workspaces: { [TEAM_KEY]: [story("S-1")] },
          activeWorkspace: { [TEAM_KEY]: "S-1" },
          agentsWorkspaceFilter: { teamKey: "gone-org:gone-team", workspaceId: id },
        },
      }),
    );
    await useViewerStore.getState().loadPersistedState();
    expect(useViewerStore.getState().agentsWorkspaceFilter).toBeNull();
  });

  it("saved hidden teams come back once each, and only keys that name a team", async () => {
    seedOrg([SLUG_A]);
    loadAppStateMock.mockResolvedValue(
      JSON.stringify({
        viewer: {
          schemaVersion: 10,
          agentsPaneHiddenTeams: ["other:team", "other:team", 5, "", "no-colon", NO_TEAM_SENTINEL],
        },
      }),
    );
    await useViewerStore.getState().loadPersistedState();
    expect(useViewerStore.getState().agentsPaneHiddenTeams).toEqual(["other:team"]);
  });

  it("a team scope keeps each hidden team once, in its restore point too", () => {
    useViewerStore.getState().setAgentsPaneTeamScope(["a:x", "b:y", "a:x", ""], {
      teamKey: "c:z",
      hidden: ["a:x", "a:x"],
    });
    const s = useViewerStore.getState();
    expect(s.agentsPaneHiddenTeams).toEqual(["a:x", "b:y"]);
    expect(s.agentsPaneTeamFocusRestore).toEqual({ teamKey: "c:z", hidden: ["a:x"] });
  });

  it("chosen looks stay per team, return to Auto, prune, and legacy values remain readable", async () => {
    seedOrg([SLUG_A]);
    useViewerStore.setState({
      workspaces: { [TEAM_KEY]: [] },
      activeWorkspace: {},
      agentIdentity: {},
    });
    const { setAgentIdentity, pruneAgentIdentity } = useViewerStore.getState();
    setAgentIdentity(TEAM_KEY, "dev", { role: "qa", face: "owl" });
    setAgentIdentity(TEAM_KEY, "dev", { face: null });
    setAgentIdentity(TEAM_KEY, "gone", { role: "manager" });
    setAgentIdentity(TEAM_KEY, "auto", { role: "worker" });
    setAgentIdentity(TEAM_KEY, "auto", { role: null });
    expect(useViewerStore.getState().agentIdentity).toEqual({
      [TEAM_KEY]: { dev: { role: "qa" }, gone: { role: "manager" } },
    });
    pruneAgentIdentity(TEAM_KEY, new Set(["dev"]));
    expect(useViewerStore.getState().agentIdentity).toEqual({
      [TEAM_KEY]: { dev: { role: "qa" } },
    });

    let capturedPayload: string | null = null;
    saveAppStateMock.mockImplementation(async (payload: string) => {
      capturedPayload = payload;
    });
    useViewerStore.getState().persistState();
    await new Promise((resolve) => setTimeout(resolve, 400));
    const saved = JSON.parse(capturedPayload!);
    expect(saved.viewer.agentIdentity).toBeUndefined();
    saved.viewer.agentIdentity = { [TEAM_KEY]: { dev: { role: "qa" } } };
    capturedPayload = JSON.stringify(saved);

    resetViewer();
    useViewerStore.setState({ agentIdentity: {} });
    seedOrg([SLUG_A]);
    loadAppStateMock.mockResolvedValue(capturedPayload!);
    await useViewerStore.getState().loadPersistedState();
    expect(useViewerStore.getState().agentIdentity[TEAM_KEY]).toEqual({ dev: { role: "qa" } });
  });

  it("SW-EW-33 AC-8: the tray coachmark seen-flag survives a reload", async () => {
    seedOrg([SLUG_A]);
    useViewerStore.setState({
      workspaces: { [TEAM_KEY]: [] },
      activeWorkspace: {},
      workspaceTraySeenCoachmark: true,
    });

    let capturedPayload: string | null = null;
    saveAppStateMock.mockImplementation(async (payload: string) => {
      capturedPayload = payload;
    });
    useViewerStore.getState().persistState();
    await new Promise((resolve) => setTimeout(resolve, 400));

    expect(JSON.parse(capturedPayload!).viewer.workspaceTraySeenCoachmark).toBe(true);

    resetViewer();
    seedOrg([SLUG_A]);
    loadAppStateMock.mockResolvedValue(capturedPayload!);
    await useViewerStore.getState().loadPersistedState();

    expect(useViewerStore.getState().workspaceTraySeenCoachmark).toBe(true);
  });

  it("OC-17: the CLI shortcuts-hint seen-flag survives a reload", async () => {
    seedOrg([SLUG_A]);
    useViewerStore.setState({
      workspaces: { [TEAM_KEY]: [] },
      activeWorkspace: {},
      cliShortcutsHintSeen: true,
    });

    let capturedPayload: string | null = null;
    saveAppStateMock.mockImplementation(async (payload: string) => {
      capturedPayload = payload;
    });
    useViewerStore.getState().persistState();
    await new Promise((resolve) => setTimeout(resolve, 400));

    expect(JSON.parse(capturedPayload!).viewer.cliShortcutsHintSeen).toBe(true);

    resetViewer();
    seedOrg([SLUG_A]);
    loadAppStateMock.mockResolvedValue(capturedPayload!);
    await useViewerStore.getState().loadPersistedState();

    // Guards the persist-payload + load-coercion arms: without either, the flag
    // resets to `false` and the one-time hint reappears on every restart.
    expect(useViewerStore.getState().cliShortcutsHintSeen).toBe(true);
  });

  it("SW-AGR-19: dismissed out-of-date terminal servers survive a reload", async () => {
    seedOrg([SLUG_A]);
    useViewerStore.setState({
      workspaces: { [TEAM_KEY]: [] },
      activeWorkspace: {},
      dismissedTerminalServers: [],
    });
    let capturedPayload: string | null = null;
    saveAppStateMock.mockImplementation(async (payload: string) => {
      capturedPayload = payload;
    });
    useViewerStore.getState().dismissTerminalServers(["box@aaaaaaaaaaaa", "farm@0.1.0"]);
    // Dismissing what is already dismissed changes nothing.
    useViewerStore.getState().dismissTerminalServers(["box@aaaaaaaaaaaa"]);
    await new Promise((resolve) => setTimeout(resolve, 400));
    expect(JSON.parse(capturedPayload!).viewer.dismissedTerminalServers).toEqual([
      "box@aaaaaaaaaaaa",
      "farm@0.1.0",
    ]);

    resetViewer();
    seedOrg([SLUG_A]);
    loadAppStateMock.mockResolvedValue(capturedPayload!);
    await useViewerStore.getState().loadPersistedState();
    expect(useViewerStore.getState().dismissedTerminalServers).toEqual([
      "box@aaaaaaaaaaaa",
      "farm@0.1.0",
    ]);

    // A garbled saved value loads as nothing dismissed.
    const garbled = JSON.parse(capturedPayload!);
    garbled.viewer.dismissedTerminalServers = [7, "ok@x", null];
    resetViewer();
    seedOrg([SLUG_A]);
    loadAppStateMock.mockResolvedValue(JSON.stringify(garbled));
    await useViewerStore.getState().loadPersistedState();
    expect(useViewerStore.getState().dismissedTerminalServers).toEqual(["ok@x"]);
  });

  it("migrates legacy scratchNoteOpen/scratchNoteRatio → rightPaneOpen/rightPaneRatio on load", async () => {
    seedOrg([SLUG_A]);
    useViewerStore.setState({
      workspaces: { [TEAM_KEY]: [] },
      activeWorkspace: {},
      rightPaneOpen: true,
      rightPaneRatio: 0.5,
    });

    let capturedPayload: string | null = null;
    saveAppStateMock.mockImplementation(async (payload: string) => {
      capturedPayload = payload;
    });
    useViewerStore.getState().persistState();
    await new Promise((resolve) => setTimeout(resolve, 400));

    // Rewrite the captured blob into the pre-rename (legacy) on-disk shape so
    // the load path exercises the scratchNote* → rightPane* back-compat reads.
    const blob = JSON.parse(capturedPayload!);
    delete blob.viewer.rightPaneOpen;
    delete blob.viewer.rightPaneRatio;
    blob.viewer.scratchNoteOpen = true;
    blob.viewer.scratchNoteRatio = 0.5;

    resetViewer();
    seedOrg([SLUG_A]);
    loadAppStateMock.mockResolvedValue(JSON.stringify(blob));
    await useViewerStore.getState().loadPersistedState();

    expect(useViewerStore.getState().rightPaneOpen).toBe(true);
    expect(useViewerStore.getState().rightPaneRatio).toBe(0.5);
  });

  it("the Agents dock's fold and width setters set the one value for every team", () => {
    useViewerStore.setState({
      workspacePanelCollapsed: false,
      workspacePanelWidth: null,
    });
    useViewerStore.getState().setWorkspacePanelCollapsed(true);
    useViewerStore.getState().setWorkspacePanelWidth(280);
    const state = useViewerStore.getState();
    expect(state.workspacePanelCollapsed).toBe(true);
    expect(state.workspacePanelWidth).toBe(280);
  });

  it("SW-WL-3 T6/AC-10: rehydrate paints workspaces + rebuilds the open-set (optimistic, non-blocking)", async () => {
    seedOrg([SLUG_A]);
    const STORY_ID = "epic-x-story-1";
    useViewerStore.setState({
      workspaces: {
        [TEAM_KEY]: [
          { id: ROOT_A, kind: "root", cwd: "/path/a", status: "ready", sourceRepoSlug: SLUG_A },
          {
            id: STORY_ID,
            kind: "story",
            cwd: "/wt/story-1",
            status: "ready",
            sourceRepoSlug: SLUG_A,
            storyFilePath: "/ctx/story-1.md",
          },
        ],
      },
      activeWorkspace: { [TEAM_KEY]: STORY_ID },
      workspaceOpenSet: {},
    });

    let capturedPayload: string | null = null;
    saveAppStateMock.mockImplementation(async (payload: string) => {
      capturedPayload = payload;
    });
    useViewerStore.getState().persistState();
    await new Promise((resolve) => setTimeout(resolve, 400));

    // Reload. The restore set() paints synchronously; the open-set is rebuilt
    // and attach/reconcile are fire-and-forget (hooks default to no-op here),
    // so loadPersistedState resolves without awaiting any attach.
    resetViewer();
    seedOrg([SLUG_A]);
    useViewerStore.setState({ workspaceOpenSet: {} });
    loadAppStateMock.mockResolvedValue(capturedPayload!);
    await useViewerStore.getState().loadPersistedState();

    const state = useViewerStore.getState();
    // SW-EW-2: the legacy root is swept on load; only the story workspace
    // remains, painted immediately from persisted state.
    expect((state.workspaces[TEAM_KEY] ?? []).map((w) => w.id)).toEqual([STORY_ID]);
    // Open-set rebuilt (T1.3): the story surface is open; the swept root is gone.
    const open = state.workspaceOpenSet[TEAM_KEY] ?? {};
    expect(open[STORY_ID]).toBe(1);
    expect(open[ROOT_A]).toBeUndefined();
  });

  it("SW-EW-15: sweeps a stale pre-EW-15 epic integration row (no epicDocPath) on restore", async () => {
    seedOrg([SLUG_A]);
    const STALE_ID = "integration:epic-x"; // legacy leaf-based id, no epicDocPath
    const FRESH_ID = "integration:/ctx/epics/epic-y/epic.md"; // EW-15 doc-path id
    useViewerStore.setState({
      workspaces: {
        [TEAM_KEY]: [
          { id: STALE_ID, kind: "epic", cwd: "/x", status: "ready", epicDirPath: "epic-x" },
          {
            id: FRESH_ID,
            kind: "epic",
            cwd: "/y",
            status: "ready",
            epicDirPath: "epic-y",
            epicDocPath: "/ctx/epics/epic-y/epic.md",
          },
        ],
      },
      activeWorkspace: { [TEAM_KEY]: STALE_ID },
      workspaceOpenSet: {},
    });

    let capturedPayload: string | null = null;
    saveAppStateMock.mockImplementation(async (payload: string) => {
      capturedPayload = payload;
    });
    useViewerStore.getState().persistState();
    await new Promise((resolve) => setTimeout(resolve, 400));

    resetViewer();
    seedOrg([SLUG_A]);
    useViewerStore.setState({ workspaceOpenSet: {} });
    loadAppStateMock.mockResolvedValue(capturedPayload!);
    await useViewerStore.getState().loadPersistedState();

    // The stale doc-path-less epic row is swept (it can't resolve repos or
    // re-key); the fresh doc-path-keyed row survives, with epicDocPath intact.
    const list = useViewerStore.getState().workspaces[TEAM_KEY] ?? [];
    expect(list.map((w) => w.id)).toEqual([FRESH_ID]);
    expect(list[0]?.epicDocPath).toBe("/ctx/epics/epic-y/epic.md");
  });
});

describe("SLW-CO-5 — transient non-ready story workspaces are not persisted", () => {
  beforeEach(resetViewer);

  it("persists only ready story workspaces, dropping 'creating' + 'error' (bad-cwd restore guard)", async () => {
    seedOrg([SLUG_A]);
    useViewerStore.setState({
      workspaces: {
        [TEAM_KEY]: [
          {
            id: "ready-1",
            kind: "story",
            cwd: "/wt/ready-1",
            storyFilePath: "/s/ready-1.md",
            storyId: "ready-1",
            status: "ready",
          },
          {
            id: "creating-1",
            kind: "story",
            cwd: "/wt/creating-1",
            storyFilePath: "/s/creating-1.md",
            storyId: "creating-1",
            status: "creating",
            setupState: { kind: "step1", phase: "creatingWorktree" },
          },
          {
            // A failed setup leaves a durable error record whose cwd points at a
            // worktree that was never created — must ALSO be dropped (load coerces
            // status→ready, so it'd otherwise restore as a broken "ready" tile).
            id: "error-1",
            kind: "story",
            cwd: "/wt/error-1",
            storyFilePath: "/s/error-1.md",
            storyId: "error-1",
            status: "error",
            statusError: "git worktree add failed",
          },
        ],
      },
      activeWorkspace: {},
    });

    let capturedPayload: string | null = null;
    saveAppStateMock.mockImplementation(async (payload: string) => {
      capturedPayload = payload;
    });
    useViewerStore.getState().persistState();
    await new Promise((resolve) => setTimeout(resolve, 400));

    const persisted = JSON.parse(capturedPayload!).viewer.workspaces[TEAM_KEY] ?? [];
    const ids = persisted.map((w: { id: string }) => w.id);
    expect(ids).toEqual(["ready-1"]);
  });
});

describe("SW-FU-B2: an upgrade load never drops the team just opened", () => {
  beforeEach(resetViewer);

  it("merges the saved openedTeams with a team opened before the load resolved", async () => {
    seedOrg([SLUG_A]);
    useViewerStore.setState({ stateLoaded: false, openedTeams: [] });
    // The workspace sync opens the active team while the viewer load is in flight.
    useViewerStore.getState().openTeam(TEAM_KEY);
    // A v0.0.11 (Home Base era) blob: its openedTeams names another team.
    loadAppStateMock.mockResolvedValue(
      JSON.stringify({ viewer: { schemaVersion: 10, openedTeams: ["acme:other"] } }),
    );

    await useViewerStore.getState().loadPersistedState();

    expect(useViewerStore.getState().openedTeams).toEqual(["acme:other", TEAM_KEY]);
    // The open held back during the load is written once the load lands.
    await new Promise((resolve) => setTimeout(resolve, 400));
    const written = committedViewerState();
    expect(written.viewer.openedTeams).toEqual(["acme:other", TEAM_KEY]);
  });

  it("persistState before the load is a no-op, so defaults never overwrite the saved state", async () => {
    useViewerStore.setState({ stateLoaded: false });
    loadAppStateMock.mockResolvedValue("{}");
    useViewerStore.getState().persistState();
    await new Promise((resolve) => setTimeout(resolve, 400));
    expect(saveAppStateMock).not.toHaveBeenCalled();
  });
});

describe("a reopen shows the CLI as it was at quit", () => {
  const OPS = "acme:ops";
  const story = (id: string): Workspace => ({
    id,
    kind: "story",
    cwd: `/wt/${id}`,
    storyFilePath: `/stories/${id}.md`,
    storyId: id,
    status: "ready",
  });
  const tree: DerivedPipelineTree = {
    dirPath: "",
    dirName: "",
    absoluteDirPath: "",
    stories: [],
    subEpics: [],
    orderError: null,
    mainDoc: null,
  };

  /** Two teams of one org, `ops` active: the CLI shows it. */
  function seedTwoTeams() {
    const workspaceRepos: Record<string, RepoState> = {};
    for (const team of ["default", "ops"]) {
      const key = `workspace:acme:${team}:${SLUG_A}`;
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
          teams: [
            { name: "default", source_repos: [{ owner: "owner", repo: "a" }] },
            { name: "ops", source_repos: [{ owner: "owner", repo: "a" }] },
          ],
          org_root: "/orgs/acme",
        },
      ],
      workspaceRepos,
      activeOrgSlug: "acme",
      activeTeamName: "ops",
    });
  }

  /** The app quits: the viewer's runtime state is gone, the terminal host (its own process) keeps
   *  its tabs. */
  function quit() {
    useViewerStore.setState({
      workspaces: {},
      activeWorkspace: {},
      softClosedWorkspaces: {},
      openedTeams: [],
      foregroundLayout: {},
      foregroundLayoutByRail: {},
      railKeyByWorkspaceId: {},
      trayMode: {},
      layoutMode: null,
      lastTerminalMode: "shared",
      terminalVisible: false,
      cliExpanded: false,
      terminalLayout: EMPTY_TERMINAL_LAYOUT,
      tabGridLayout: {},
    });
  }

  beforeEach(() => {
    resetViewer();
    seedTwoTeams();
  });

  it("restores expanded on the focused workspace, its active tab and grid, with the host reattaching after load", async () => {
    useViewerStore.setState({
      workspaces: {
        [TEAM_KEY]: [story("D-1")],
        [OPS]: [story("O-1"), story("O-2"), story("O-3")],
      },
      activeWorkspace: { [TEAM_KEY]: "D-1", [OPS]: "O-2" },
      openedTeams: [TEAM_KEY, OPS],
      layoutMode: "console",
      lastTerminalMode: "console",
      terminalVisible: true,
    });
    // The CLI shows the active ops team, its O-2 expanded on a two-tab grid with b focused.
    useViewerStore.getState().switchWorkspace(OPS, "O-2");
    useViewerStore.getState().setForegroundPreset(OPS, "rows");
    useViewerStore.getState().expandWorkspace(OPS, "O-2");
    seedTerminalTabs(OPS, "O-2", ["sess-a", "sess-b", "sess-c"], {
      active: "sess-b",
      paned: ["sess-a", "sess-b"],
    });
    seedTerminalTabs(OPS, "O-1", ["sess-d"]);
    const before = useViewerStore.getState();
    expect(shouldRenderTabGrid(before, OPS, "O-2")).toBe(true);

    const saved = await persisted();
    quit();
    loadAppStateMock.mockResolvedValue(JSON.stringify(saved));
    await useViewerStore.getState().loadPersistedState();
    // The host reattaches once the saved state is in, then the team's tree syncs.
    refetchTestLayout();
    useViewerStore.getState().syncWorkspaceFrontmatter(OPS, tree);

    const s = useViewerStore.getState();
    expect(s.activeWorkspace[OPS]).toBe("O-2");
    expect(s.foregroundLayout[OPS]).toMatchObject({ preset: "rows" });
    expect(s.cliExpanded).toBe(true);
    expect(saved.viewer.cliChrome).toMatchObject({ expanded: true, layoutMode: "console" });
    expect(expandedWorkspaceId(s, OPS)).toBe("O-2");
    expect(s.layoutMode).toBe("console");
    expect(s.terminalVisible).toBe(true);
    const tabs = selectWorkspaceTabs(s, OPS, "O-2");
    const sessionOf = (tabId: string | undefined) => tabs.find((t) => t.id === tabId)?.sessionId;
    expect(sessionOf(selectActiveTerminalTab(s, OPS, "O-2") ?? undefined)).toBe("sess-b");
    expect(shouldRenderTabGrid(s, OPS, "O-2")).toBe(true);
    expect(workspacePanedTabIds(s, OPS, "O-2").map(sessionOf)).toEqual(["sess-a", "sess-b"]);
  });

  it("a viewer key the app no longer knows is ignored and gone from the next save", async () => {
    loadAppStateMock.mockResolvedValue(
      JSON.stringify({
        viewer: {
          schemaVersion: 14,
          workspaces: { [TEAM_KEY]: [story("D-1")] },
          activeWorkspace: { [TEAM_KEY]: "D-1" },
          openedTeams: [TEAM_KEY],
          retiredKey: OPS,
        },
      }),
    );
    await useViewerStore.getState().loadPersistedState();
    expect(useViewerStore.getState().activeWorkspace[TEAM_KEY]).toBe("D-1");
    const saved = await persisted();
    expect(saved.viewer).not.toHaveProperty("retiredKey");
  });
});

it("persists each team's nav source and restores older lenses as clone", async () => {
  resetViewer();
  seedOrg([]);
  useViewerStore.setState({ navLens: {} });
  useViewerStore.getState().setNavLens(TEAM_KEY, { source: { kind: "private" }, view: "changes" });
  const saved = await persisted();
  expect(saved.viewer.navLens).toMatchObject({
    [TEAM_KEY]: { source: { kind: "private" }, view: "changes" },
  });
  useViewerStore.setState({ navLens: {} });
  loadAppStateMock.mockResolvedValue(JSON.stringify(saved));
  await useViewerStore.getState().loadPersistedState();
  expect(useViewerStore.getState().navLens[TEAM_KEY].source).toEqual({ kind: "private" });
  loadAppStateMock.mockResolvedValue(
    JSON.stringify({
      ...saved,
      viewer: { ...saved.viewer, navLens: { [TEAM_KEY]: { view: "history" } } },
    }),
  );
  await useViewerStore.getState().loadPersistedState();
  expect(useViewerStore.getState().navLens[TEAM_KEY]).toMatchObject({
    source: { kind: "clone" },
    view: "history",
  });
});

it("keeps the History view but clears repository-specific branch filters on source changes", () => {
  useViewerStore.setState({
    navLens: {},
    pipelineScope: { [TEAM_KEY]: "docs" },
    drillPaths: { [TEAM_KEY]: "/org/default/content/docs" },
  });
  useViewerStore.getState().setNavLens(TEAM_KEY, {
    view: "history",
    historyBranches: { kind: "branch", name: "gs/sync-docs" },
  });
  useViewerStore.getState().setNavLens(TEAM_KEY, { source: { kind: "private" } });
  expect(useViewerStore.getState().navLens[TEAM_KEY]).toMatchObject({
    view: "history",
    source: { kind: "private" },
    historyBranches: { kind: "all" },
  });
  expect(useViewerStore.getState().pipelineScope[TEAM_KEY]).toBe("");
  expect(useViewerStore.getState().drillPaths[TEAM_KEY]).toBe("");
});

// Uses the same save/load owner as sidebar folds and identity choices.
describe("manager identity persistence", () => {
  it("reads legacy explicit, top-level and nested managers and names", async () => {
    resetViewer();
    seedOrg([SLUG_A]);
    useViewerStore.setState({ agentIdentity: {}, agentsPaneFolds: {} });
    const state = useViewerStore.getState();
    state.setAgentIdentity(TEAM_KEY, "worker", { manager: "lead" });
    state.setAgentIdentity(TEAM_KEY, "lead", { manager: "root" });
    state.setAgentIdentity(TEAM_KEY, "unmanaged", { manager: "@none" });
    state.setAgentIdentity(TEAM_KEY, "sid-worker", { name: "My worker" });
    state.setAgentsPaneFold(`#ws-fold:${TEAM_KEY}|S-1`, true);
    const choices = useViewerStore.getState().agentIdentity;
    const folds = useViewerStore.getState().agentsPaneFolds;
    const saved = await persisted();
    expect(saved.viewer.agentIdentity).toBeUndefined();
    expect(saved.viewer.agentsPaneFolds).toBeUndefined();
    saved.viewer.agentIdentity = choices;
    resetViewer();
    useViewerStore.setState({ agentIdentity: {}, agentsPaneFolds: {} });
    seedOrg([SLUG_A]);
    loadAppStateMock.mockResolvedValue(JSON.stringify(saved));
    await useViewerStore.getState().loadPersistedState();
    expect(useViewerStore.getState().agentIdentity[TEAM_KEY]).toEqual({
      worker: { manager: "lead" },
      lead: { manager: "root" },
      unmanaged: { manager: "@none" },
      "sid-worker": { name: "My worker" },
    });
    expect(folds[`#ws-fold:${TEAM_KEY}|S-1`]).toBe(true);
    expect(useViewerStore.getState().agentIdentity[TEAM_KEY]?.["fresh-session"]).toBeUndefined();
  });
});

it("persists clearing an optional setting as a field deletion", async () => {
  useViewerStore.setState({
    stateLoaded: true,
    settings: { ...useViewerStore.getState().settings, terminalShell: "/bin/zsh" },
  });
  useViewerStore.getState().updateSettings({ terminalShell: undefined });
  await useViewerStore.getState().flushPersist();
  const patch = JSON.parse(saveAppStateMock.mock.calls.at(-1)![0]);
  expect(patch.viewer.settings).toHaveProperty("terminalShell", null);
});
it("keeps the latest committed baseline and mirror when ingress arrives during a save", async () => {
  applyOwnedViewerPrefs({}, true);
  useViewerStore.setState({ stateLoaded: true, activeWorkspace: {} });
  applyOwnedViewerPrefs({ activeWorkspace: { A: "a0", B: "b0" } });
  useViewerStore.setState({ activeWorkspace: { A: "a1", B: "b0" } });
  saveAppStateMock.mockImplementationOnce(async () => {
    applyOwnedViewerPrefs({ activeWorkspace: { A: "a1", B: "b1" } });
  });
  await useViewerStore.getState().flushPersist();
  expect(vi.mocked(commands.pushViewerState).mock.calls.at(-1)![0]).toMatchObject({
    activeWorkspace: { A: "a1", B: "b1" },
  });
  applyOwnedViewerPrefs({ activeWorkspace: { A: "a1", B: "b2" } });
  expect(useViewerStore.getState().activeWorkspace.B).toBe("b2");
  applyOwnedViewerPrefs({}, true);
});

it("the launch's mirror push carries every viewer field, the saved one-time tip included", async () => {
  resetViewer();
  const pushMock = commands.pushViewerState as ReturnType<typeof vi.fn>;
  pushMock.mockClear();
  loadAppStateMock.mockResolvedValue(
    JSON.stringify({ viewer: { workspaceTraySeenCoachmark: true } }),
  );
  await useViewerStore.getState().loadPersistedState();
  const pushed = pushMock.mock.lastCall?.[0] as Record<string, unknown>;
  expect(pushed.workspaceTraySeenCoachmark).toBe(true);
  for (const key of Object.keys(viewerMirrorFields(useViewerStore.getState()))) {
    expect(pushed).toHaveProperty(key);
  }
});
