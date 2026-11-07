// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import { useOrgStore } from "./org-store";
import { selectWorkspaceTabs, useViewerStore, type Workspace } from "./viewer-store";
import { installTerminalLayoutHarness } from "@/test/terminal-layout-harness";

installTerminalLayoutHarness();

vi.mock("@/tauri/commands", () => ({
  LOCAL_MACHINE_ID: "local",
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
  // SW-DX-11: openOrFocusEpicWorkspace registers the epic terminal's session
  // under its epicDocPath handle (cap-free) for sendToSession addressability.
  dashboardRegisterSession: vi.fn().mockResolvedValue(undefined),
  detectTools: vi.fn().mockResolvedValue({
    terminals: [],
    editors: [],
    default_terminal: "",
    default_editor: "",
  }),
}));

const TEAM = "test-org:default";

beforeEach(() => {
  useViewerStore.setState({
    workspaces: {},
    activeWorkspace: {},
  });
  // Reset the org store so each test controls whether org_root resolves
  // (SW-EW-7 AC-3: integration cwd = `{org_root}/_epic/{id}`).
  useOrgStore.setState({
    orgs: [],
    activeOrgSlug: "test-org",
    activeTeamName: "default",
  });
});

/** Inject workspaces directly for the store-action tests below. */
function seedWorkspaces(team: string, list: Workspace[]) {
  useViewerStore.setState((st) => ({
    workspaces: { ...st.workspaces, [team]: list },
  }));
}

describe("openOrFocusEpicWorkspace (SW-EW-1c)", () => {
  it("creates + activates the workspace but DEFERS the terminal when org_root is unresolved (F1)", () => {
    // No org seeded → cwd can't resolve. SW-EW-7 review F1: the workspace is
    // still created + activated, but the terminal is NOT spawned (spawning at
    // cwd "" would chdir("")). It launches on a later open once org_root loads.
    useViewerStore.getState().openOrFocusEpicWorkspace(TEAM, "epic-workspaces", "acme/web");
    const list = useViewerStore.getState().workspaces[TEAM] ?? [];
    const ws = list.find((w) => w.id === "integration:epic-workspaces:acme/web");
    expect(ws).toMatchObject({
      kind: "epic",
      epicDirPath: "epic-workspaces",
      sourceRepoSlug: "acme/web",
      cwd: "",
      status: "creating",
    });
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(
      "integration:epic-workspaces:acme/web",
    );
    const tabs = selectWorkspaceTabs(
      useViewerStore.getState(),
      TEAM,
      "integration:epic-workspaces:acme/web",
    );
    expect(tabs.length).toBe(0);
  });

  it("is idempotent — re-opening focuses without duplicating", () => {
    const s = useViewerStore.getState();
    s.openOrFocusEpicWorkspace(TEAM, "epic-workspaces", "acme/web");
    s.openOrFocusEpicWorkspace(TEAM, "epic-workspaces", "acme/web");
    const list = useViewerStore.getState().workspaces[TEAM] ?? [];
    expect(list.filter((w) => w.kind === "epic")).toHaveLength(1);
  });

  it("ignores an empty epic name", () => {
    // (An empty/absent slug is NOT an error — it's the epic-level terminal.)
    const s = useViewerStore.getState();
    s.openOrFocusEpicWorkspace(TEAM, "", "acme/web");
    s.openOrFocusEpicWorkspace(TEAM, "", undefined, "/some/dir");
    expect((useViewerStore.getState().workspaces[TEAM] ?? []).length).toBe(0);
  });

  it("SW-EW-15: roots cwd at the caller-supplied epic dir + launches the terminal", () => {
    // SW-EW-15: the dir is AUTHORITATIVE from the backend handle (the FE no
    // longer derives `{org_root}/_epic/{leaf}`); the caller passes it as cwd.
    const epicDir = "/home/.codesense/users/1/orgs/test-org/_epic/seamless__epic-workspaces";
    useViewerStore
      .getState()
      .openOrFocusEpicWorkspace(TEAM, "epic-workspaces", "acme/web", epicDir);
    const ws = (useViewerStore.getState().workspaces[TEAM] ?? []).find((w) => w.kind === "epic");
    expect(ws?.cwd).toBe(epicDir);
    expect(ws?.epicWorkspaceDir).toBe(epicDir);
    expect(ws?.status).toBe("ready");
    // D3: integration workspaces never carry sourceRepoPath (story-cleanup field).
    expect(ws?.sourceRepoPath).toBeUndefined();
    // F1: a resolved cwd launches the rooted session.
    const tabs = selectWorkspaceTabs(
      useViewerStore.getState(),
      TEAM,
      "integration:epic-workspaces:acme/web",
    );
    expect(tabs.length).toBe(1);
  });

  it("F1 re-open: repairs cwd + launches the terminal once the dir resolves", () => {
    const s = useViewerStore.getState();
    const wsId = "integration:epic-workspaces:acme/web";
    // First open before the dir resolved → cwd "", no terminal.
    s.openOrFocusEpicWorkspace(TEAM, "epic-workspaces", "acme/web");
    expect((useViewerStore.getState().workspaces[TEAM] ?? []).find((w) => w.id === wsId)?.cwd).toBe(
      "",
    );
    expect(selectWorkspaceTabs(useViewerStore.getState(), TEAM, wsId)).toHaveLength(0);

    // The backend dir resolves; re-open WITH it repairs the stale cwd + spawns.
    const epicDir = "/home/.codesense/users/1/orgs/test-org/_epic/seamless__epic-workspaces";
    s.openOrFocusEpicWorkspace(TEAM, "epic-workspaces", "acme/web", epicDir);
    const ws = (useViewerStore.getState().workspaces[TEAM] ?? []).find((w) => w.id === wsId);
    expect(ws?.cwd).toBe(epicDir);
    expect(ws?.status).toBe("ready");
    // Repaired in place — not duplicated.
    expect(
      (useViewerStore.getState().workspaces[TEAM] ?? []).filter((w) => w.kind === "epic"),
    ).toHaveLength(1);
    expect(selectWorkspaceTabs(useViewerStore.getState(), TEAM, wsId)).toHaveLength(1);
  });

  it("SW-EW-6: a per-repo terminal (slug given) roots at the explicit worktree cwd", () => {
    const wtPath = "/home/.codesense/_epic/epic-x/owner/repo";
    useViewerStore.getState().openOrFocusEpicWorkspace(TEAM, "epic-x", "owner/repo", wtPath);
    const ws = useViewerStore.getState().workspaces[TEAM]?.[0];
    expect(ws).toMatchObject({
      id: "integration:epic-x:owner/repo",
      kind: "epic",
      epicDirPath: "epic-x",
      sourceRepoSlug: "owner/repo",
      cwd: wtPath, // the assembled worktree, not the clone root
      status: "ready",
    });
  });

  it("SW-EW-6: an epic-level terminal (no slug) opens ONE row at the epic dir", () => {
    const epicDir = "/home/.codesense/_epic/epic-x";
    useViewerStore.getState().openOrFocusEpicWorkspace(TEAM, "epic-x", undefined, epicDir);
    const ws = useViewerStore.getState().workspaces[TEAM]?.[0];
    expect(ws).toMatchObject({
      id: "integration:epic-x", // no slug in the id
      kind: "epic",
      epicDirPath: "epic-x",
      cwd: epicDir, // the PARENT dir that holds the repo worktrees
      status: "ready",
    });
    expect(ws?.sourceRepoSlug).toBeUndefined();
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe("integration:epic-x");
  });

  it("SW-EW-15: stores the epic doc path on the record at create time", () => {
    const epicDir = "/home/.codesense/_epic/epic-x";
    const docPath = "/ctx/content/epics/epic-x/epic-x.md";
    useViewerStore.getState().openOrFocusEpicWorkspace(TEAM, "epic-x", undefined, epicDir, docPath);
    const ws = useViewerStore.getState().workspaces[TEAM]?.[0];
    expect(ws?.epicDocPath).toBe(docPath);
  });

  it("SW-EW-15: two same-leaf epics (distinct doc paths) get DISTINCT workspaces", () => {
    const s = useViewerStore.getState();
    // Same leaf ("foo") but different doc paths → the doc-path-keyed id keeps
    // them separate (no "open one, focus the other" collision).
    s.openOrFocusEpicWorkspace(
      TEAM,
      "foo",
      undefined,
      "/home/.codesense/_epic/planning__foo",
      "/ctx/planning/foo/epic.md",
    );
    s.openOrFocusEpicWorkspace(
      TEAM,
      "foo",
      undefined,
      "/home/.codesense/_epic/docs__foo",
      "/ctx/docs/foo/epic.md",
    );
    const integrations = (useViewerStore.getState().workspaces[TEAM] ?? []).filter(
      (w) => w.kind === "epic",
    );
    expect(integrations).toHaveLength(2);
    expect(new Set(integrations.map((w) => w.id)).size).toBe(2);
  });
});

describe("openOrFocusEpicWorkspace — SW-FX-35 tray pair coherence", () => {
  it("AC-2: from a cold viewer layoutMode, the tray pair is satisfied (visible + non-viewer)", () => {
    useViewerStore.setState({ layoutMode: "viewer", lastTerminalMode: "shared" });
    useViewerStore.getState().openOrFocusEpicWorkspace(TEAM, "epic-workspaces", "acme/web");
    const s = useViewerStore.getState();
    expect(s.terminalVisible).toBe(true);
    expect(s.layoutMode).not.toBe("viewer");
  });

  it("AC-4: restores lastTerminalMode === 'console'", () => {
    useViewerStore.setState({
      layoutMode: "viewer",
      lastTerminalMode: "console",
    });
    useViewerStore.getState().openOrFocusEpicWorkspace(TEAM, "epic-workspaces", "acme/web");
    expect(useViewerStore.getState().layoutMode).toBe("console");
  });
});

describe("active-terminal invariant (SW-EW-2 AC-7)", () => {
  it("setPipelineScope changes the drill scope but NOT the active workspace", () => {
    const s = useViewerStore.getState();
    seedWorkspaces(TEAM, [
      { id: "story-a", kind: "story", cwd: "/wt/a", status: "ready", storyFilePath: "/r/a.md" },
    ]);
    s.switchWorkspace(TEAM, "story-a");
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe("story-a");
    // Drill the pipeline scope — the panel neither follows nor drives this.
    s.setPipelineScope(TEAM, "in-progress/beta");
    expect(useViewerStore.getState().pipelineScope[TEAM]).toContain("beta");
    // INVARIANT: drilling must not move the active terminal/workspace.
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe("story-a");
  });

  it("syncDrillPath updates drill + scope but leaves the active workspace untouched", () => {
    const s = useViewerStore.getState();
    seedWorkspaces(TEAM, [
      { id: "story-a", kind: "story", cwd: "/wt/a", status: "ready", storyFilePath: "/r/a.md" },
    ]);
    s.switchWorkspace(TEAM, "story-a");
    const before = useViewerStore.getState().activeWorkspace[TEAM];
    s.syncDrillPath(TEAM, "/ctx/content/in-progress/beta", "/ctx", "/ctx/content");
    expect(useViewerStore.getState().drillPaths[TEAM]).toBe("/ctx/content/in-progress/beta");
    // INVARIANT: the nav drill never switches the active workspace.
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(before);
  });

  it("switchWorkspace is the sole mutator of the active workspace (an explicit click)", () => {
    const s = useViewerStore.getState();
    seedWorkspaces(TEAM, [
      { id: "story-x", kind: "story", cwd: "/wt/x", status: "ready", storyFilePath: "/r/x.md" },
      { id: "story-y", kind: "story", cwd: "/wt/y", status: "ready", storyFilePath: "/r/y.md" },
    ]);
    s.switchWorkspace(TEAM, "story-x");
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe("story-x");
    s.switchWorkspace(TEAM, "story-y");
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe("story-y");
  });
});
