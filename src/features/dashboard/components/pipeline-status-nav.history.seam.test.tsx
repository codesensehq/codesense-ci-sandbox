// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@/test-utils";

import { PipelineStatusNav } from "./pipeline-status-nav";
import { buildTree } from "@/shared/lib/pipeline-graph/test-fixtures/derived-tree";
import { installTerminalLayoutHarness } from "@/test/terminal-layout-harness";
import { useViewerStore } from "@/shared/stores/viewer-store";
import {
  resetContextGitStoreForTests,
  useContextGitStore,
} from "@/features/git-sync/store/use-context-git-store";
import { navGitCommitFiles, navGitHistory } from "@/app/mock/handlers/nav-git-history";
import type { NavAgentContext } from "@/features/agent-context/lib/nav-agent-context";
import type { ContextRepoStatus, HistoryBranches } from "@/tauri/commands";

// History, driven through the nav with the real stores and the real lane layout: the
// commands answer with a context repo shaped like the design's (a merged PR, an open PR, a
// local branch, a backup, a commit not pushed), and opening a commit, a file and the branch
// filter go through the same command wrappers a real build calls.

const TEAM = "acme:default";

const cmd = vi.hoisted(() => ({
  contextRepoHistory: vi.fn(),
  getPublishSet: vi.fn(),
}));

vi.mock("@/shared/lib/send-to-terminal", () => ({ sendTextToTerminal: vi.fn() }));
vi.mock("@/shared/lib/team-key", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/shared/lib/team-key")>();
  return { ...actual, useActiveTeamKey: () => TEAM, getActiveTeamKey: () => TEAM };
});
vi.mock("@/shared/hooks/use-avatar-packs", () => ({
  useAvatarPacks: () => ({ faces: undefined }),
}));
vi.mock("@/tauri/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/tauri/commands")>();
  return {
    ...actual,
    ...cmd,
    listDirectory: () => Promise.resolve([]),
    getTeamNavPins: () => Promise.resolve([]),
  };
});
vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn() }) }));

installTerminalLayoutHarness();

const STATUS: ContextRepoStatus = {
  branch: "main",
  headSha: "5a7a83a",
  hasUpstream: true,
  upstream: "origin/main",
  remoteProvider: "github",
  ahead: 1,
  behind: 0,
  sync: { state: "ok", lastSyncedAt: Date.now() - 120_000 },
  conflicts: [],
};

function seed(status: ContextRepoStatus = STATUS) {
  useContextGitStore.getState().put(TEAM, {
    orgRoot: "/org",
    changes: [],
    byPath: new Map(),
    status,
    loading: false,
    historyMoves: 0,
  });
}

function renderNav(agentContext?: NavAgentContext) {
  return render(
    <PipelineStatusNav
      tree={buildTree("root", { absoluteDirPath: "/ctx" })}
      repoPath="/ctx"
      rootPath="/ctx"
      selectedPath={null}
      onFileSelect={() => {}}
      agentContext={agentContext}
    />,
  );
}

const commits = () => screen.getAllByTestId("history-commit");
const commitNamed = (subject: string) =>
  commits().find((el) => el.getAttribute("aria-label")?.includes(subject))!;

beforeEach(() => {
  resetContextGitStoreForTests();
  useViewerStore.setState({
    workspaces: {},
    navLens: {},
    openTabs: {},
    activeTab: {},
    tabDiff: {},
  });
  cmd.contextRepoHistory
    .mockReset()
    .mockImplementation((_root: string, branches: HistoryBranches, paths: string[] | null) =>
      Promise.resolve(navGitHistory(branches, paths, false, Date.now())),
    );
  cmd.getPublishSet
    .mockReset()
    .mockImplementation((_root: string, _ws: unknown, _pr: unknown, sha?: string) =>
      Promise.resolve(
        sha ? navGitCommitFiles(sha, false) : { buckets: [], general: { count: 0, files: [] } },
      ),
    );
  seed();
  useViewerStore.getState().setNavLens(TEAM, { view: "history" });
});

describe("the History view", () => {
  it("re-reads when git moves, keeping the commits on screen meanwhile", async () => {
    renderNav();
    await waitFor(() => expect(commits()).toHaveLength(15));
    expect(cmd.contextRepoHistory).toHaveBeenCalledTimes(1);
    let release!: () => void;
    cmd.contextRepoHistory.mockImplementationOnce(
      (_root: string, branches: HistoryBranches, paths: string[] | null) =>
        new Promise((r) => (release = () => r(navGitHistory(branches, paths, false, Date.now())))),
    );
    act(() => {
      const snap = useContextGitStore.getState().byTeam[TEAM];
      useContextGitStore.getState().put(TEAM, { ...snap, historyMoves: snap.historyMoves + 1 });
    });
    await waitFor(() => expect(cmd.contextRepoHistory).toHaveBeenCalledTimes(2));
    // No flash: the last page stays while the move is read.
    expect(commits()).toHaveLength(15);
    await act(async () => release());
    expect(commits()).toHaveLength(15);
  });

  it("is a view in the lens menu, shown as a chip in the filter field", async () => {
    renderNav();
    expect(screen.getByTestId("left-nav-history-chip")).toHaveTextContent("History");
    await waitFor(() => expect(commits().length).toBeGreaterThan(0));
    expect(cmd.contextRepoHistory).toHaveBeenCalledWith("/org", { kind: "all" }, null, 60, null);
  });

  it("draws commits on lanes with their marks, labels and who made them", async () => {
    renderNav();
    await waitFor(() => expect(commits()).toHaveLength(15));
    expect(screen.getByTestId("history-ahead-behind")).toHaveTextContent("main: 1 ahead");

    const head = commits()[0];
    expect(head).toHaveAttribute("aria-label", expect.stringContaining("not on GitHub yet"));
    expect(within(head).getByTestId("history-ref-head")).toHaveTextContent("main");
    expect(within(head).getByTestId("history-ref-ahead")).toHaveTextContent("not on GitHub");

    const open = commitNamed("Sync routine stories");
    expect(open).toHaveAttribute("data-lane", "open");
    expect(within(open).getByTestId("history-ref-pr")).toHaveTextContent("#24 open");
    expect(open).toHaveTextContent("claude for you");
    // An agent's commit wears its face on the lane.
    expect(open.querySelector(".cs-history-face")).not.toBeNull();

    const merged = commitNamed("#23 Migrate dashboards");
    expect(merged).toHaveTextContent("#23");
    // A merged PR is a diamond: the only rotated mark.
    expect(merged.querySelector("rect[transform^='rotate(45']")).not.toBeNull();
    expect(commitNamed("Back up agent workspace")).toHaveAttribute("data-lane", "backup");
    expect(commitNamed("Board and Pipeline panes")).toHaveAttribute("data-lane", "local");
  });

  it("opening a commit unfolds its files; a file opens that commit's change in a tab", async () => {
    renderNav();
    await waitFor(() => expect(commits().length).toBeGreaterThan(0));
    fireEvent.click(commitNamed("#23 Migrate dashboards"));
    // Every file shows, the deleted one struck through.
    await waitFor(() => expect(screen.getAllByTestId("history-file")).toHaveLength(4));
    expect(cmd.getPublishSet).toHaveBeenCalledWith(
      "/org",
      undefined,
      undefined,
      expect.stringMatching(/^c3/),
      undefined,
    );
    expect(screen.queryByTestId("history-all-files")).toBeNull();
    const removed = screen
      .getAllByTestId("history-file")
      .find((f) => f.textContent?.includes("weft"))!;
    expect(removed.querySelector(".line-through")).not.toBeNull();

    const board = screen
      .getAllByTestId("history-file")
      .find((f) => f.textContent?.includes("inbox-board"))!;
    expect(board.querySelector(".text-\\[var\\(--cs-git-modified\\)\\]")).not.toBeNull();
    fireEvent.click(board);
    const s = useViewerStore.getState();
    const path = "/org/default/content/26-agent-inbox/dashboards/inbox-board.html";
    expect(s.activeTab[TEAM]).toBe(path);
    expect(s.tabDiff[TEAM]?.[path]).toMatchObject({ shortSha: "5486bae", pr: 23, who: "maya" });
    expect(s.openTabs[TEAM]?.find((t) => t.path === path)?.name).toBe("inbox-board @ 5486bae");
  });

  it("walks by keyboard: arrows move, Enter or → opens, ← folds", async () => {
    renderNav();
    await waitFor(() => expect(commits().length).toBeGreaterThan(0));
    const list = screen.getByTestId("history-list");
    commits()[0].focus();
    fireEvent.keyDown(list, { key: "ArrowDown" });
    expect(document.activeElement).toBe(commits()[1]);
    fireEvent.keyDown(list, { key: "Enter" });
    expect(commits()[1]).toHaveAttribute("aria-expanded", "true");
    await waitFor(() => expect(screen.getAllByTestId("history-file").length).toBeGreaterThan(0));
    fireEvent.keyDown(list, { key: "ArrowDown" });
    expect(document.activeElement).toBe(screen.getAllByTestId("history-file")[0]);
    fireEvent.keyDown(list, { key: "ArrowLeft" });
    expect(document.activeElement).toBe(commits()[1]);
    fireEvent.keyDown(list, { key: "ArrowLeft" });
    expect(commits()[1]).toHaveAttribute("aria-expanded", "false");
  });

  it("the branch filter changes only what is drawn: Main only, one branch", async () => {
    renderNav();
    await waitFor(() => expect(commits().length).toBeGreaterThan(0));
    await act(async () => {
      useViewerStore.getState().setNavLens(TEAM, { historyBranches: { kind: "main" } });
    });
    await waitFor(() =>
      expect(cmd.contextRepoHistory).toHaveBeenLastCalledWith(
        "/org",
        { kind: "main" },
        null,
        60,
        null,
      ),
    );
    await waitFor(() =>
      expect(commits().every((c) => c.getAttribute("data-lane") === "main")).toBe(true),
    );
    await act(async () => {
      useViewerStore
        .getState()
        .setNavLens(TEAM, { historyBranches: { kind: "branch", name: "gs/sync-routines" } });
    });
    await waitFor(() => expect(commits()).toHaveLength(3));
    expect(commits()[2]).toHaveTextContent("branched from main here");
    expect(screen.getByTestId("history-branch-filter")).toHaveTextContent("gs/sync-routines");
  });

  it("the typed filter narrows to commits whose words match", async () => {
    renderNav();
    await waitFor(() => expect(commits().length).toBeGreaterThan(0));
    fireEvent.change(screen.getByTestId("left-nav-search-input"), { target: { value: "FX-7" } });
    await waitFor(() => expect(commits()).toHaveLength(1));
    expect(commits()[0]).toHaveTextContent("FX-7 done");
  });

  it("says behind in the filter line and on GitHub's copy", async () => {
    seed({ ...STATUS, behind: 2 });
    cmd.contextRepoHistory.mockImplementation(
      (_r: string, b: HistoryBranches, p: string[] | null) =>
        Promise.resolve(navGitHistory(b, p, true, Date.now())),
    );
    renderNav();
    await waitFor(() => expect(commits().length).toBeGreaterThan(0));
    expect(screen.getByTestId("history-ahead-behind")).toHaveTextContent("main: 1 ahead, 2 behind");
    expect(within(commits()[0]).getByTestId("history-ref-behind")).toHaveTextContent(
      "2 new on GitHub",
    );
  });

  it("in an agent's context lists only commits that touched what the agent gets", async () => {
    const content = "/org/default/content/";
    renderNav({
      agentName: "Barb",
      repoName: null,
      relOf: (abs) => (abs.startsWith(content) ? abs.slice(content.length) : null),
      editRelOf: () => null,
      defaulted: false,
      membership: (rel) => ({ state: rel.startsWith("28-remote-machines/") ? "in" : "out" }),
      plan: () => null,
      edit: () => {},
    });
    await waitFor(() => expect(commits()).toHaveLength(3));
    expect(commits().map((c) => c.getAttribute("aria-label")?.split(",")[0])).toEqual([
      "#22 Fixes from the remote-machine test",
      "FX-7 done",
      "Live droplet fixes FX-1 to FX-6",
    ]);
  });
});

it("keeps History on the clone when a worktree is the Changes source", async () => {
  useViewerStore
    .getState()
    .setNavLens(TEAM, { source: { kind: "worktree", id: "cw_sprint" }, view: "history" });
  useContextGitStore.getState().put(TEAM, {
    source: { kind: "worktree", id: "cw_sprint" },
    orgRoot: "/worktree",
    cloneRoot: "/org",
    cloneStatus: STATUS,
    status: {
      ...STATUS,
      branch: "sprint-1",
      worktree: {
        id: "cw_sprint",
        name: "Sprint",
        path: "/worktree",
        mergeBase: "base",
        uncommittedPaths: [],
      },
    },
    changes: [],
    byPath: new Map(),
    loading: false,
    historyMoves: 0,
  });
  renderNav();
  await waitFor(() =>
    expect(cmd.contextRepoHistory).toHaveBeenCalledWith("/org", { kind: "all" }, null, 60, null),
  );
  expect(screen.getByTestId("history-ahead-behind")).toHaveTextContent("main: 1 ahead");
  expect(cmd.contextRepoHistory.mock.calls.every(([root]) => root === "/org")).toBe(true);
});
