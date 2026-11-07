// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import { selectWorkspaceTabs, useViewerStore } from "./viewer-store";
import type { Workspace } from "./viewer-store";
import { buildRootWorkspaceId, buildTeamKey } from "@/shared/lib/team-key";
import { managerFoldKey, teamFoldKey, workspaceFoldKey } from "@/shared/lib/workspace-key";
import { USER_HIDDEN_TAG } from "@/shared/lib/terminal-layout-projection";
import {
  seedTerminalTabs,
  terminalHost,
  installTerminalLayoutHarness,
} from "@/test/terminal-layout-harness";

// T.2: per-source-repo root id (replaces legacy ROOT_WORKSPACE_ID = "root").
const ROOT_WORKSPACE_ID = buildRootWorkspaceId("test/repo");

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
  detectTools: vi.fn().mockResolvedValue({
    terminals: [],
    editors: [],
    default_terminal: "",
    default_editor: "",
  }),
}));

const TEAM_A = "team-a:default";
const TEAM_B = "team-b:default";

installTerminalLayoutHarness();

const alive = (...sessions: string[]) => sessions.map((id) => terminalHost().contains(id));

const root = (cwd: string): Workspace => ({
  id: ROOT_WORKSPACE_ID,
  kind: "root",
  cwd,
  status: "ready",
});

const story = (id: string): Workspace => ({
  id,
  kind: "story",
  cwd: `/wt/${id}`,
  storyFilePath: `/stories/${id}.md`,
  storyId: id,
  status: "ready",
});

function resetState() {
  useViewerStore.setState({
    workspaces: {},
    activeWorkspace: {},
    softClosedWorkspaces: {},
  });
}

describe("closeTeam (T.1 AC-11)", () => {
  beforeEach(resetState);

  it("is a no-op when teamKey is null (cold-start branch)", () => {
    useViewerStore.setState({
      workspaces: { [TEAM_A]: [root("/a")] },
    });
    useViewerStore.getState().closeTeam(null);
    expect(useViewerStore.getState().workspaces[TEAM_A]).toBeDefined();
  });

  it("ends every terminal in every workspace under the targeted teamKey", () => {
    useViewerStore.setState({
      workspaces: {
        [TEAM_A]: [root("/a"), story("W-1"), story("W-2")],
      },
    });
    seedTerminalTabs(TEAM_A, ROOT_WORKSPACE_ID, ["sess-r1", "sess-r2"]);
    seedTerminalTabs(TEAM_A, "W-1", ["sess-w1"]);
    seedTerminalTabs(TEAM_A, "W-2", ["sess-w2a", "sess-w2b"]);

    useViewerStore.getState().closeTeam(TEAM_A);

    expect(alive("sess-r1", "sess-r2", "sess-w1", "sess-w2a", "sess-w2b")).toEqual([
      false,
      false,
      false,
      false,
      false,
    ]);
  });

  it("ends its tiles' Hidden tabs too, but keeps headless launches (no tile) running", () => {
    useViewerStore.setState({ workspaces: { [TEAM_A]: [story("W-1")] } });
    seedTerminalTabs(TEAM_A, "W-1", ["sess-w1"]);
    const describe = { displayName: "W-1", kind: "story" };
    // A tab Hidden from the W-1 tile (listed under its tray chip).
    terminalHost().seed(`${TEAM_A}|W-1`, describe, {
      session: "hidden-tab",
      placement: "background",
      tags: { [USER_HIDDEN_TAG]: "tab" },
    });
    // A headless MCP / sprint launch: a workspace the viewer has no tile for.
    terminalHost().seed(`${TEAM_A}|SW-9`, describe, {
      session: "sprint-worker",
      placement: "background",
    });

    useViewerStore.getState().closeTeam(TEAM_A);

    expect(alive("sess-w1", "hidden-tab", "sprint-worker")).toEqual([false, false, true]);
  });

  it("keeps live, hidden, and minimized terminal attachments during renderer eviction", async () => {
    const openWorkspace = story("W-1");
    const minimizedWorkspace = story("W-2");
    useViewerStore.setState({
      workspaces: { [TEAM_A]: [openWorkspace] },
      activeWorkspace: { [TEAM_A]: openWorkspace.id },
      softClosedWorkspaces: {
        [TEAM_A]: { [minimizedWorkspace.id]: { workspace: minimizedWorkspace } },
      },
    });
    seedTerminalTabs(TEAM_A, openWorkspace.id, ["sess-open"]);
    terminalHost().seed(
      `${TEAM_A}|W-1`,
      { displayName: "W-1", kind: "story" },
      {
        session: "sess-hidden",
        placement: "background",
        tags: { [USER_HIDDEN_TAG]: "tab" },
      },
    );
    terminalHost().seed(
      `${TEAM_A}|W-2`,
      { displayName: "W-2", kind: "story" },
      {
        session: "sess-minimized",
        placement: "background",
        tags: { [USER_HIDDEN_TAG]: "tab" },
      },
    );
    useViewerStore.getState().setSessionAiStatus("sess-open", "running");
    useViewerStore.getState().setSessionProgress("sess-open", { phase: "dev", note: "working" });

    useViewerStore.getState().closeTeam(TEAM_A, { preserveTerminalSessions: true });

    const state = useViewerStore.getState();
    expect(alive("sess-open", "sess-hidden", "sess-minimized")).toEqual([true, true, true]);
    expect(state.workspaces[TEAM_A]).toEqual([openWorkspace]);
    expect(state.softClosedWorkspaces[TEAM_A]?.[minimizedWorkspace.id]?.workspace).toEqual(
      minimizedWorkspace,
    );
    expect(state.activeWorkspace[TEAM_A]).toBe(openWorkspace.id);
    expect(state.aiStatusBySession["sess-open"]).toBe("running");
    expect(state.progressBySession["sess-open"]).toEqual({ phase: "dev", note: "working" });

    useViewerStore
      .getState()
      .restoreHiddenTerminalTab(TEAM_A, minimizedWorkspace.id, "sess-minimized");
    await vi.waitFor(() => {
      expect(useViewerStore.getState().workspaces[TEAM_A]).toContainEqual(minimizedWorkspace);
      expect(selectWorkspaceTabs(useViewerStore.getState(), TEAM_A, minimizedWorkspace.id)).toEqual(
        expect.arrayContaining([expect.objectContaining({ sessionId: "sess-minimized" })]),
      );
    });
  });

  it("destructively closes a previously evicted team and leaves no same-key workspace state", () => {
    const workspace = story("W-1");
    useViewerStore.setState({ workspaces: { [TEAM_A]: [workspace] } });
    seedTerminalTabs(TEAM_A, workspace.id, ["sess-evicted"]);

    useViewerStore.getState().closeTeam(TEAM_A, { preserveTerminalSessions: true });
    expect(terminalHost().contains("sess-evicted")).toBe(true);
    expect(useViewerStore.getState().workspaces[TEAM_A]).toEqual([workspace]);

    // Deleting a team that was already age-evicted uses the default destructive close.
    useViewerStore.getState().closeTeam(TEAM_A);

    expect(terminalHost().contains("sess-evicted")).toBe(false);
    expect(useViewerStore.getState().workspaces[TEAM_A]).toBeUndefined();
    expect(selectWorkspaceTabs(useViewerStore.getState(), TEAM_A, workspace.id)).toHaveLength(0);
    // A later team created under the same key starts with no retained records.
    useViewerStore.getState().openTeam(TEAM_A);
    expect(useViewerStore.getState().workspaces[TEAM_A]).toBeUndefined();
    expect(selectWorkspaceTabs(useViewerStore.getState(), TEAM_A, workspace.id)).toHaveLength(0);
  });

  it("clears every team-keyed slice entry for the targeted teamKey", () => {
    useViewerStore.setState({
      workspaces: {
        [TEAM_A]: [root("/a"), story("W-1")],
        [TEAM_B]: [root("/b")],
      },
      activeWorkspace: { [TEAM_A]: "W-1", [TEAM_B]: ROOT_WORKSPACE_ID },
    });
    seedTerminalTabs(TEAM_A, ROOT_WORKSPACE_ID, ["sess1"]);
    seedTerminalTabs(TEAM_B, ROOT_WORKSPACE_ID, ["sess2"]);

    useViewerStore.getState().closeTeam(TEAM_A);

    const state = useViewerStore.getState();
    expect(state.workspaces[TEAM_A]).toBeUndefined();
    expect(state.activeWorkspace[TEAM_A]).toBeUndefined();
    expect(selectWorkspaceTabs(state, TEAM_A, ROOT_WORKSPACE_ID)).toHaveLength(0);
    expect(selectWorkspaceTabs(state, TEAM_B, ROOT_WORKSPACE_ID)).toHaveLength(1);
  });

  it("leaves other teamKeys untouched", () => {
    useViewerStore.setState({
      workspaces: {
        [TEAM_A]: [root("/a")],
        [TEAM_B]: [root("/b"), story("W-2")],
      },
      activeWorkspace: { [TEAM_A]: ROOT_WORKSPACE_ID, [TEAM_B]: "W-2" },
    });
    seedTerminalTabs(TEAM_B, ROOT_WORKSPACE_ID, ["sess-b"]);

    useViewerStore.getState().closeTeam(TEAM_A);

    const state = useViewerStore.getState();
    expect(state.workspaces[TEAM_B]).toHaveLength(2);
    expect(state.activeWorkspace[TEAM_B]).toBe("W-2");
    expect(selectWorkspaceTabs(state, TEAM_B, ROOT_WORKSPACE_ID)).toHaveLength(1);
    expect(terminalHost().contains("sess-b")).toBe(true);
  });

  // PT-10 AC-13: closeTeam ends terminals kept alive by closed-but-running shadows
  // for the closing team, then sweeps the shadow entries — no leak on team
  // delete. A shadow for a DIFFERENT team is untouched.
  it("ends minimized terminals for the closing team and drops the shadow", () => {
    useViewerStore.setState({
      workspaces: { [TEAM_A]: [root("/a")], [TEAM_B]: [root("/b")] },
      softClosedWorkspaces: {
        [TEAM_A]: { "W-1": { workspace: story("W-1") } },
        [TEAM_B]: { "W-2": { workspace: story("W-2") } },
      },
    });
    seedTerminalTabs(TEAM_A, "W-1", ["sess-a1", "sess-a2"]);
    seedTerminalTabs(TEAM_B, "W-2", ["sess-b1"]);

    useViewerStore.getState().closeTeam(TEAM_A);

    expect(alive("sess-a1", "sess-a2", "sess-b1")).toEqual([false, false, true]);
    expect(useViewerStore.getState().softClosedWorkspaces[TEAM_A]).toBeUndefined();
    expect(useViewerStore.getState().softClosedWorkspaces[TEAM_B]?.["W-2"]).toBeDefined();
  });

  // T.4 AC-3/D-8 (Phase 8 review): closeTeam must also sweep the prompt
  // identity scalar (noClonePromptRepo) and the paired retry callback when
  // their teamKey matches the closing team. Otherwise a team switch while the
  // prompt is open leaves the modal pointing at a gone team (and the retry
  // callback firing with stale state).
  it("clears noClonePromptRepo + noClonePromptRetry together when teamKey matches", () => {
    const retry = vi.fn();
    useViewerStore.setState({
      workspaces: { [TEAM_A]: [root("/a")] },
      noClonePromptRepo: { teamKey: TEAM_A, sourceRepoSlug: "acme/web" },
      noClonePromptRetry: retry,
    });

    useViewerStore.getState().closeTeam(TEAM_A);

    const after = useViewerStore.getState();
    expect(after.noClonePromptRepo).toBeNull();
    expect(after.noClonePromptRetry).toBeNull();
  });

  it("preserves noClonePromptRepo + retry when it targets a different team", () => {
    const retry = vi.fn();
    useViewerStore.setState({
      workspaces: { [TEAM_A]: [root("/a")], [TEAM_B]: [root("/b")] },
      noClonePromptRepo: { teamKey: TEAM_B, sourceRepoSlug: "acme/api" },
      noClonePromptRetry: retry,
    });

    useViewerStore.getState().closeTeam(TEAM_A);

    const after = useViewerStore.getState();
    expect(after.noClonePromptRepo).toEqual({ teamKey: TEAM_B, sourceRepoSlug: "acme/api" });
    expect(after.noClonePromptRetry).toBe(retry);
  });
});

/**
 * T.4 AC-7 — closeTeam clears every team-keyed slice, not just the 6 from
 * T.1's cohort. The 18 T.4 slices won't be bucketed by teamKey at runtime
 * until Phase 5 flips the setter signatures, but the test pre-populates
 * `[teamKey]` directly via `setState` to prove the clear works once the
 * data flows through.
 *
 * Why this matters: if `closeTeam` doesn't clear these buckets, switching
 * teams (or evicting suspended ones) leaves stale tabs / drill paths /
 * scope / on-disk-worktrees in memory forever. Catches a regression that
 * would otherwise only surface under repeated team switches.
 */
describe("closeTeam (T.4 AC-7 — full team-keyed slice clear)", () => {
  beforeEach(resetState);

  it("clears every T.4 team-keyed slice for the targeted teamKey", () => {
    // Pre-populate every T.4 team-keyed slice for both teams.
    useViewerStore.setState({
      openTabs: {
        [TEAM_A]: [{ path: "/a.md", name: "a" }],
        [TEAM_B]: [{ path: "/b.md", name: "b" }],
      },
      activeTab: {
        [TEAM_A]: "/a.md",
        [TEAM_B]: "/b.md",
      },
      drillPaths: {
        [TEAM_A]: "/drill-a",
        [TEAM_B]: "/drill-b",
      },
      // D39: the CLI chrome is one global setting, not a team-keyed slice.
      terminalVisible: true,
      layoutMode: "shared",
      lastTerminalMode: "console",
      rightPaneSplitRatio: {
        [TEAM_A]: 0.5,
        [TEAM_B]: 0.6,
      },
      terminalSplitRatio: 0.45,
      statusNavExpandedPaths: {
        [TEAM_A]: ["/expand-a"],
        [TEAM_B]: ["/expand-b"],
      },
      revealCounter: {
        [TEAM_A]: 3,
        [TEAM_B]: 5,
      },
      pipelineScope: {
        [TEAM_A]: "scope-a",
        [TEAM_B]: "scope-b",
      },
      navHistory: {
        [TEAM_A]: ["/a.md"],
        [TEAM_B]: ["/b.md"],
      },
      navIndex: {
        [TEAM_A]: 0,
        [TEAM_B]: 0,
      },
      openedViewerPaths: {
        [TEAM_A]: ["/a.md"],
        [TEAM_B]: ["/b.md"],
      },
      onDiskWorktrees: {
        [TEAM_A]: new Map([["/story-a.md", { path: "/wt-a" }]]),
        [TEAM_B]: new Map([["/story-b.md", { path: "/wt-b" }]]),
      },
    });

    useViewerStore.getState().closeTeam(TEAM_A);

    const state = useViewerStore.getState();

    // TEAM_A entries are gone from every slice.
    expect(state.openTabs[TEAM_A]).toBeUndefined();
    expect(state.activeTab[TEAM_A]).toBeUndefined();
    expect(state.drillPaths[TEAM_A]).toBeUndefined();
    expect(state.rightPaneSplitRatio[TEAM_A]).toBeUndefined();
    expect(state.statusNavExpandedPaths[TEAM_A]).toBeUndefined();
    expect(state.revealCounter[TEAM_A]).toBeUndefined();
    expect(state.pipelineScope[TEAM_A]).toBeUndefined();
    expect(state.navHistory[TEAM_A]).toBeUndefined();
    expect(state.navIndex[TEAM_A]).toBeUndefined();
    expect(state.openedViewerPaths[TEAM_A]).toBeUndefined();
    expect(state.onDiskWorktrees[TEAM_A]).toBeUndefined();

    // D39: closing a team leaves the global CLI chrome alone.
    expect(state.terminalVisible).toBe(true);
    expect(state.layoutMode).toBe("shared");
    expect(state.lastTerminalMode).toBe("console");
    expect(state.terminalSplitRatio).toBe(0.45);

    // TEAM_B is preserved untouched across every slice.
    expect(state.openTabs[TEAM_B]).toEqual([{ path: "/b.md", name: "b" }]);
    expect(state.activeTab[TEAM_B]).toBe("/b.md");
    expect(state.drillPaths[TEAM_B]).toBe("/drill-b");
    expect(state.rightPaneSplitRatio[TEAM_B]).toBe(0.6);
    expect(state.statusNavExpandedPaths[TEAM_B]).toEqual(["/expand-b"]);
    expect(state.revealCounter[TEAM_B]).toBe(5);
    expect(state.pipelineScope[TEAM_B]).toBe("scope-b");
    expect(state.navHistory[TEAM_B]).toEqual(["/b.md"]);
    expect(state.navIndex[TEAM_B]).toBe(0);
    expect(state.openedViewerPaths[TEAM_B]).toEqual(["/b.md"]);
    expect(state.onDiskWorktrees[TEAM_B]?.get("/story-b.md")?.path).toBe("/wt-b");
  });

  it("clears T.4 slices in the same set() call as the T.1 cohort (atomic)", () => {
    // If the clear weren't atomic, an observer could see workspaces gone
    // while openTabs still populated — leading to UI rendering the
    // team-less repo against stale tabs. Run a single closeTeam and assert
    // both cohorts are gone post-call (proxy for atomicity).
    useViewerStore.setState({
      workspaces: { [TEAM_A]: [root("/a")] },
      activeWorkspace: { [TEAM_A]: ROOT_WORKSPACE_ID },
      openTabs: { [TEAM_A]: [{ path: "/a.md", name: "a" }] },
      activeTab: { [TEAM_A]: "/a.md" },
    });

    useViewerStore.getState().closeTeam(TEAM_A);

    const state = useViewerStore.getState();
    expect(state.workspaces[TEAM_A]).toBeUndefined();
    expect(state.activeWorkspace[TEAM_A]).toBeUndefined();
    expect(state.openTabs[TEAM_A]).toBeUndefined();
    expect(state.activeTab[TEAM_A]).toBeUndefined();
  });

  it("is a no-op for slices that don't have an entry for the targeted teamKey", () => {
    // Sparse population: only some slices have TEAM_A.
    useViewerStore.setState({
      workspaces: { [TEAM_A]: [root("/a")] },
      openTabs: {
        // Note: TEAM_A absent; only TEAM_B populated.
        [TEAM_B]: [{ path: "/b.md", name: "b" }],
      },
    });

    useViewerStore.getState().closeTeam(TEAM_A);

    const state = useViewerStore.getState();
    // TEAM_A workspace is cleared; TEAM_B openTabs untouched.
    expect(state.workspaces[TEAM_A]).toBeUndefined();
    expect(state.openTabs[TEAM_A]).toBeUndefined();
    expect(state.openTabs[TEAM_B]).toEqual([{ path: "/b.md", name: "b" }]);
  });
});

describe("closeTeam: the agents pane's own state for the team", () => {
  it("drops the team's folds, its place among the hidden teams and a filter on it", () => {
    useViewerStore.setState({
      agentsPaneFolds: {
        [teamFoldKey(TEAM_A)]: true,
        [managerFoldKey(TEAM_A, "lead")]: true,
        [workspaceFoldKey(TEAM_A, "s-1")]: true,
        [workspaceFoldKey(TEAM_A, "")]: true,
        [teamFoldKey(TEAM_B)]: true,
        [managerFoldKey(TEAM_B, "lead")]: true,
        [workspaceFoldKey(TEAM_B, "s-1")]: true,
      },
      agentsPaneHiddenTeams: [TEAM_A, TEAM_B],
      agentsWorkspaceFilter: { teamKey: TEAM_A, workspaceId: "s-1" },
    });
    useViewerStore.getState().closeTeam(TEAM_A);
    const s = useViewerStore.getState();
    expect(s.agentsPaneFolds).toEqual({
      [teamFoldKey(TEAM_B)]: true,
      [managerFoldKey(TEAM_B, "lead")]: true,
      [workspaceFoldKey(TEAM_B, "s-1")]: true,
    });
    expect(s.agentsPaneHiddenTeams).toEqual([TEAM_B]);
    expect(s.agentsWorkspaceFilter).toBeNull();
  });

  it("leaves another team's filter alone", () => {
    useViewerStore.setState({
      agentsWorkspaceFilter: { teamKey: TEAM_B, workspaceId: "s-1" },
    });
    useViewerStore.getState().closeTeam(TEAM_A);
    expect(useViewerStore.getState().agentsWorkspaceFilter).toEqual({
      teamKey: TEAM_B,
      workspaceId: "s-1",
    });
  });

  it("drops a funnel restore point and a pending reveal that name it", () => {
    for (const workspaceId of ["s-1", ""]) {
      useViewerStore.setState({
        agentsPaneTeamFocusRestore: { teamKey: TEAM_A, hidden: [TEAM_B] },
        agentsWorkspaceReveal: { teamKey: TEAM_A, workspaceId, nonce: 3 },
      });
      useViewerStore.getState().closeTeam(TEAM_A);
      const s = useViewerStore.getState();
      expect(s.agentsPaneTeamFocusRestore).toBeNull();
      expect(s.agentsWorkspaceReveal).toBeNull();
    }
  });

  it("keeps another team's restore point and reveal, minus the closed team", () => {
    useViewerStore.setState({
      agentsPaneTeamFocusRestore: { teamKey: TEAM_B, hidden: [TEAM_A, "team-c:default"] },
      agentsWorkspaceReveal: { teamKey: TEAM_B, workspaceId: "s-1", nonce: 1 },
    });
    useViewerStore.getState().closeTeam(TEAM_A);
    const s = useViewerStore.getState();
    expect(s.agentsPaneTeamFocusRestore).toEqual({ teamKey: TEAM_B, hidden: ["team-c:default"] });
    expect(s.agentsWorkspaceReveal).toEqual({ teamKey: TEAM_B, workspaceId: "s-1", nonce: 1 });
  });
});

describe("rekeyTeam: the agents pane's own state follows the renamed team", () => {
  const OLD = buildTeamKey("acme", "old-name");
  const NEW = buildTeamKey("acme", "new-name");
  const OTHER = buildTeamKey("acme", "other");

  it("carries the team's folds, hidden place, filter and funnel restore point", () => {
    useViewerStore.setState({
      agentsPaneFolds: {
        [teamFoldKey(OLD)]: true,
        [managerFoldKey(OLD, "lead")]: true,
        [workspaceFoldKey(OLD, "s-1")]: true,
        [workspaceFoldKey(OLD, "")]: true,
        [workspaceFoldKey(OTHER, "s-1")]: true,
      },
      agentsPaneHiddenTeams: [OLD, OTHER],
      agentsWorkspaceFilter: { teamKey: OLD, workspaceId: "s-1" },
      agentsPaneTeamFocusRestore: { teamKey: OTHER, hidden: [OLD] },
      agentsWorkspaceReveal: { teamKey: OLD, workspaceId: "s-1", nonce: 2 },
    });
    useViewerStore.getState().rekeyTeam("acme", "old-name", "new-name");
    const s = useViewerStore.getState();
    expect(s.agentsPaneFolds).toEqual({
      [teamFoldKey(NEW)]: true,
      [managerFoldKey(NEW, "lead")]: true,
      [workspaceFoldKey(NEW, "s-1")]: true,
      [workspaceFoldKey(NEW, "")]: true,
      [workspaceFoldKey(OTHER, "s-1")]: true,
    });
    expect(s.agentsPaneHiddenTeams).toEqual([NEW, OTHER]);
    expect(s.agentsWorkspaceFilter).toEqual({ teamKey: NEW, workspaceId: "s-1" });
    expect(s.agentsPaneTeamFocusRestore).toEqual({ teamKey: OTHER, hidden: [NEW] });
    expect(s.agentsWorkspaceReveal).toBeNull();
  });

  it("leaves another team's state alone", () => {
    useViewerStore.setState({
      agentsPaneFolds: { [teamFoldKey(OTHER)]: true },
      agentsPaneHiddenTeams: [OTHER],
      agentsWorkspaceFilter: { teamKey: OTHER, workspaceId: "s-1" },
      agentsPaneTeamFocusRestore: null,
    });
    const before = useViewerStore.getState();
    useViewerStore.getState().rekeyTeam("acme", "old-name", "new-name");
    const s = useViewerStore.getState();
    expect(s.agentsPaneFolds).toBe(before.agentsPaneFolds);
    expect(s.agentsPaneHiddenTeams).toBe(before.agentsPaneHiddenTeams);
    expect(s.agentsWorkspaceFilter).toBe(before.agentsWorkspaceFilter);
  });
});
