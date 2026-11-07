// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, render } from "@testing-library/react";

import type { AgentContextPivot as PivotState } from "@/features/agent-context/hooks/use-agent-context";
import {
  resetContextGitStoreForTests,
  useContextGitStore,
} from "@/features/git-sync/store/use-context-git-store";
import type { RepoState } from "@/shared/stores/config-store";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useWorktreeRosterStore } from "@/features/viewer/stores/worktree-roster-store";
import type { WorktreeRow } from "@/tauri/commands";

import { LeftNavHeader } from "./left-nav-header";

// The context repo's status and changed files follow file writes on their own: the real
// source hook, the real file-change counter the watcher bumps, and the command wrappers
// answered here. Bursts read once, a read in flight is followed by exactly one more, a hidden
// window reads nothing until it is back, and a collapsed nav leaves the changed files alone.

const TEAM = "acme:default";
const NAV = "/checkout";

const cmd = vi.hoisted(() => ({
  contextRepoStatus: vi.fn(),
  getPublishSet: vi.fn(),
}));

vi.mock("@/tauri/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/tauri/commands")>();
  return { ...actual, ...cmd, saveAppState: vi.fn().mockResolvedValue(undefined) };
});
const bus = vi.hoisted(() => {
  const handlers = new Map<string, Set<(e: { payload: unknown }) => void>>();
  return {
    handlers,
    emit(name: string, payload: unknown) {
      for (const h of handlers.get(name) ?? []) h({ payload });
    },
  };
});
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((name: string, handler: (e: { payload: unknown }) => void) => {
    const set = bus.handlers.get(name) ?? new Set();
    set.add(handler);
    bus.handlers.set(name, set);
    return Promise.resolve(() => set.delete(handler));
  }),
}));
vi.mock("@/shared/lib/pipeline-graph", () => ({ useTeamRepoGlyphMap: () => new Map() }));
vi.mock("@/shared/hooks/use-avatar-packs", () => ({
  useAvatarPacks: () => ({ faces: undefined }),
}));
vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn() }) }));

const STATUS = {
  branch: "main",
  headSha: "5a7a83a",
  hasUpstream: true,
  upstream: "origin/main",
  remoteProvider: "github",
  ahead: 0,
  behind: 0,
  sync: { state: "ok", lastSyncedAt: 1_000, noSender: false },
  conflicts: [],
};

const file = (name: string, added = 1) => ({
  path: `default/content/${name}`,
  status: "modified",
  added,
  removed: 0,
});
const setOf = (...files: ReturnType<typeof file>[]) => ({
  buckets: [],
  general: { count: files.length, files },
});

function pivot(): PivotState {
  return {
    agents: [],
    repos: [],
    agent: null,
    repo: null,
    select: vi.fn(),
    view: null,
    loading: false,
    error: null,
    reload: vi.fn(),
    membership: () => ({ state: "out" }),
    plan: () => null,
    edit: vi.fn(),
  };
}

const gitMoved = async (kinds: string[], repoPath = "/org") => {
  act(() => bus.emit("context-git-moved", { repoPath, kinds }));
  await tick(0);
};
const historyMoves = () => useContextGitStore.getState().byTeam[TEAM]?.historyMoves ?? 0;

const tick = (ms: number) => act(() => vi.advanceTimersByTimeAsync(ms));
const write = (path = "/org/default/content/a.md") =>
  act(() => useFileRefreshStore.getState().bumpFileRefresh(NAV, new Set([path])));

let visibility: DocumentVisibilityState = "visible";
function setVisibility(v: DocumentVisibilityState) {
  visibility = v;
  act(() => {
    document.dispatchEvent(new Event("visibilitychange"));
  });
}

async function mount() {
  render(<LeftNavHeader teamKey={TEAM} navRepoPath={NAV} pivot={pivot()} />);
  await tick(0);
  expect(cmd.contextRepoStatus).toHaveBeenCalledTimes(1);
  expect(cmd.getPublishSet).toHaveBeenCalledTimes(1);
}

beforeEach(() => {
  vi.useFakeTimers();
  visibility = "visible";
  Object.defineProperty(document, "visibilityState", {
    configurable: true,
    get: () => visibility,
  });
  resetContextGitStoreForTests();
  vi.clearAllMocks();
  bus.handlers.clear();
  useFileRefreshStore.setState({
    fileRefreshKeys: {},
    lastChangedContextFile: {},
    contentRefreshKeys: {},
  });
  useOrgStore.setState({
    orgs: [],
    activeOrgSlug: "acme",
    activeTeamName: "default",
    workspaceRepos: {
      [NAV]: {
        repoPath: NAV,
        contextRepoPath: "/org",
        teamContentPath: "/org/default/content",
      } as unknown as RepoState,
    },
  });
  useViewerStore.setState({ navLens: {}, navCollapsed: {}, openTabs: {}, activeTab: {} });
  useWorktreeRosterStore.setState({ rows: null, error: null });
  cmd.contextRepoStatus.mockResolvedValue(STATUS);
  cmd.getPublishSet.mockResolvedValue(setOf(file("a.md")));
});

afterEach(() => {
  vi.useRealTimers();
});

describe("the context repo's status and changes follow file writes", () => {
  it("a burst of writes is one status read and one changed-files read", async () => {
    await mount();
    for (let i = 0; i < 12; i++) {
      write(`/org/default/content/f${i}.md`);
      await tick(30);
    }
    expect(cmd.contextRepoStatus).toHaveBeenCalledTimes(1);
    await tick(400);
    expect(cmd.contextRepoStatus).toHaveBeenCalledTimes(2);
    expect(cmd.getPublishSet).toHaveBeenCalledTimes(2);
    await tick(10_000);
    expect(cmd.contextRepoStatus).toHaveBeenCalledTimes(2);
    expect(cmd.getPublishSet).toHaveBeenCalledTimes(2);
  });

  it("writes during a read in flight make exactly one more read", async () => {
    await mount();
    let release!: () => void;
    cmd.contextRepoStatus.mockImplementationOnce(
      () => new Promise((r) => (release = () => r(STATUS))),
    );
    write();
    await tick(400);
    expect(cmd.contextRepoStatus).toHaveBeenCalledTimes(2);
    for (let i = 0; i < 8; i++) {
      write(`/org/default/content/g${i}.md`);
      await tick(200);
    }
    expect(cmd.contextRepoStatus).toHaveBeenCalledTimes(2);
    await act(async () => release());
    await tick(400);
    expect(cmd.contextRepoStatus).toHaveBeenCalledTimes(3);
    await tick(10_000);
    expect(cmd.contextRepoStatus).toHaveBeenCalledTimes(3);
  });

  it("a hidden window reads nothing, then catches up once when shown", async () => {
    await mount();
    setVisibility("hidden");
    for (let i = 0; i < 5; i++) {
      write(`/org/default/content/h${i}.md`);
      await tick(1_000);
    }
    expect(cmd.contextRepoStatus).toHaveBeenCalledTimes(1);
    expect(cmd.getPublishSet).toHaveBeenCalledTimes(1);
    setVisibility("visible");
    await tick(0);
    expect(cmd.contextRepoStatus).toHaveBeenCalledTimes(2);
    expect(cmd.getPublishSet).toHaveBeenCalledTimes(2);
    await tick(10_000);
    expect(cmd.contextRepoStatus).toHaveBeenCalledTimes(2);
    expect(cmd.getPublishSet).toHaveBeenCalledTimes(2);
  });

  it("a collapsed nav keeps the status live and reads the changed files when it opens", async () => {
    await mount();
    act(() => useViewerStore.setState({ navCollapsed: { [TEAM]: true } }));
    write();
    await tick(400);
    expect(cmd.contextRepoStatus).toHaveBeenCalledTimes(2);
    expect(cmd.getPublishSet).toHaveBeenCalledTimes(1);
    act(() => useViewerStore.setState({ navCollapsed: { [TEAM]: false } }));
    await tick(0);
    expect(cmd.getPublishSet).toHaveBeenCalledTimes(2);
  });

  it("a re-read that finds the same answer publishes nothing new", async () => {
    await mount();
    const before = useContextGitStore.getState().byTeam[TEAM];
    write();
    await tick(400);
    expect(cmd.getPublishSet).toHaveBeenCalledTimes(2);
    const after = useContextGitStore.getState().byTeam[TEAM];
    expect(after.changes).toBe(before.changes);
    expect(after.status).toBe(before.status);
  });

  it("a newly changed file joins without replacing the rows already there", async () => {
    await mount();
    const a = useContextGitStore.getState().byTeam[TEAM].byPath.get("/org/default/content/a.md");
    expect(a).toBeDefined();
    cmd.getPublishSet.mockResolvedValue(setOf(file("z.md"), file("a.md")));
    write("/org/default/content/z.md");
    await tick(400);
    const snap = useContextGitStore.getState().byTeam[TEAM];
    expect(snap.changes.map((c) => c.relPath)).toEqual([
      "default/content/a.md",
      "default/content/z.md",
    ]);
    // The unchanged row is the same object, so its tinted row doesn't re-render.
    expect(snap.byPath.get("/org/default/content/a.md")).toBe(a);
  });

  it("an edit to a changed file updates its tint data", async () => {
    await mount();
    cmd.getPublishSet.mockResolvedValue(setOf(file("a.md", 9)));
    write();
    await tick(400);
    expect(
      useContextGitStore.getState().byTeam[TEAM].byPath.get("/org/default/content/a.md")?.added,
    ).toBe(9);
  });

  it("a commit no event reported still shows on the safety-net poll, and the changed files follow", async () => {
    await mount();
    // The agent committed and pushed: nothing in the working tree changed.
    cmd.contextRepoStatus.mockResolvedValue({ ...STATUS, headSha: "9f00d1e", ahead: 1 });
    cmd.getPublishSet.mockResolvedValue(setOf());
    await tick(60_400);
    expect(cmd.contextRepoStatus).toHaveBeenCalledTimes(2);
    expect(useContextGitStore.getState().byTeam[TEAM].status?.ahead).toBe(1);
    await tick(400);
    expect(cmd.getPublishSet).toHaveBeenCalledTimes(2);
    expect(useContextGitStore.getState().byTeam[TEAM].changes).toEqual([]);
    // The next quiet poll finds the same commits: the changed files are left alone.
    await tick(60_400);
    expect(cmd.contextRepoStatus).toHaveBeenCalledTimes(3);
    expect(cmd.getPublishSet).toHaveBeenCalledTimes(2);
  });

  it("a sync under way is re-read often enough to see it finish or stall", async () => {
    cmd.contextRepoStatus.mockResolvedValue({
      ...STATUS,
      sync: { ...STATUS.sync, state: "syncing", progress: "Rebasing 1 of 2" },
    });
    await mount();
    await tick(30_400);
    expect(cmd.contextRepoStatus).toHaveBeenCalledTimes(2);
  });

  it("a quiet repo is polled only as a slow safety net", async () => {
    await mount();
    await tick(59_000);
    expect(cmd.contextRepoStatus).toHaveBeenCalledTimes(1);
  });

  it("while an agent's run is live, writes that never stop read the changed files rarely", async () => {
    useWorktreeRosterStore.setState({
      rows: [{ path: "/wt-a", liveCliSession: true } as unknown as WorktreeRow],
    });
    await mount();
    for (let i = 0; i < 40; i++) {
      write(`/org/default/content/w${i}.md`);
      await tick(500);
    }
    // 20s of writes: one changed-files read per 10s, not one per settled burst.
    expect(cmd.getPublishSet.mock.calls.length).toBeLessThanOrEqual(3);
    // The status stays live meanwhile.
    expect(cmd.contextRepoStatus.mock.calls.length).toBeGreaterThan(5);
  });
});

describe("the context repo's status, changes and History follow git moves", () => {
  it("a commit is one status read at once, then the changed files and History", async () => {
    await mount();
    cmd.contextRepoStatus.mockResolvedValue({ ...STATUS, headSha: "9f00d1e", ahead: 1 });
    cmd.getPublishSet.mockResolvedValue(setOf());
    await gitMoved(["refs", "index"]);
    // No debounce: the backend already let the burst settle.
    expect(cmd.contextRepoStatus).toHaveBeenCalledTimes(2);
    expect(cmd.getPublishSet).toHaveBeenCalledTimes(2);
    expect(historyMoves()).toBe(1);
    expect(useContextGitStore.getState().byTeam[TEAM].status?.ahead).toBe(1);
    // One move, one read each: the status read showing the commit adds nothing more.
    await tick(10_000);
    expect(cmd.contextRepoStatus).toHaveBeenCalledTimes(2);
    expect(cmd.getPublishSet).toHaveBeenCalledTimes(2);
    expect(historyMoves()).toBe(1);
  });

  it("a fetch reads the status and History, not the changed files", async () => {
    await mount();
    await gitMoved(["fetch"]);
    expect(cmd.contextRepoStatus).toHaveBeenCalledTimes(2);
    expect(historyMoves()).toBe(1);
    await tick(1_000);
    expect(cmd.getPublishSet).toHaveBeenCalledTimes(1);
  });

  it("staging reads the status and the changed files, not History", async () => {
    await mount();
    await gitMoved(["index"]);
    expect(cmd.contextRepoStatus).toHaveBeenCalledTimes(2);
    expect(cmd.getPublishSet).toHaveBeenCalledTimes(2);
    expect(historyMoves()).toBe(0);
  });

  it("another repo's move reads nothing", async () => {
    await mount();
    await gitMoved(["head", "refs", "index"], "/other-org");
    await tick(1_000);
    expect(cmd.contextRepoStatus).toHaveBeenCalledTimes(1);
    expect(cmd.getPublishSet).toHaveBeenCalledTimes(1);
  });

  it("a move during a status read in flight is read once more after it", async () => {
    await mount();
    let release!: () => void;
    cmd.contextRepoStatus.mockImplementationOnce(
      () => new Promise((r) => (release = () => r(STATUS))),
    );
    await gitMoved(["index"]);
    expect(cmd.contextRepoStatus).toHaveBeenCalledTimes(2);
    await gitMoved(["refs"]);
    expect(cmd.contextRepoStatus).toHaveBeenCalledTimes(2);
    await act(async () => release());
    await tick(0);
    expect(cmd.contextRepoStatus).toHaveBeenCalledTimes(3);
    // The ref move reaches History on the read that started after it.
    expect(historyMoves()).toBe(1);
  });

  it("a hidden window reads nothing on a move, then catches up once when shown", async () => {
    await mount();
    setVisibility("hidden");
    await gitMoved(["head", "refs", "index"]);
    await tick(5_000);
    expect(cmd.contextRepoStatus).toHaveBeenCalledTimes(1);
    expect(cmd.getPublishSet).toHaveBeenCalledTimes(1);
    setVisibility("visible");
    await tick(0);
    expect(cmd.contextRepoStatus).toHaveBeenCalledTimes(2);
    expect(cmd.getPublishSet).toHaveBeenCalledTimes(2);
    expect(historyMoves()).toBe(1);
  });
});
