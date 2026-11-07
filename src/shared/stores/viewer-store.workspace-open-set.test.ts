// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { useViewerStore, type Workspace } from "./viewer-store";
import { useOrgStore } from "./org-store";
import { buildRootWorkspaceId } from "@/shared/lib/team-key";
import {
  registerWorkspaceWatchHooks,
  resetWorkspaceWatchHooks,
} from "@/shared/lib/workspace-watch-hooks";
import type { RepoState } from "./config-store";

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
}));

const TEAM_A = "test-org:default";
const TEAM_B = "test-org:other";
const SLUG = "test-org/test-repo";
const SYNTHETIC_KEY = `workspace:test-org:default:${SLUG}`;
const ROOT_ID = buildRootWorkspaceId(SLUG);

const onOpened = vi.fn();
const onClosed = vi.fn();

function seedRepoState(localPath: string, repoPath: string = SYNTHETIC_KEY): RepoState {
  return { repoPath, localPath } as unknown as RepoState;
}

beforeEach(() => {
  onOpened.mockReset();
  onClosed.mockReset();
  registerWorkspaceWatchHooks({ onWorkspaceOpened: onOpened, onWorkspaceClosed: onClosed });
  useOrgStore.setState({
    activeOrgSlug: "test-org",
    activeTeamName: "default",
    workspaceRepos: { [SYNTHETIC_KEY]: seedRepoState("/repo/test-repo") },
  });
  useViewerStore.setState({
    workspaces: {},
    activeWorkspace: {},
    workspaceOpenSet: {},
  });
});

afterEach(() => {
  resetWorkspaceWatchHooks();
});

describe("WorkspaceOpenSet refcount (SW-WL-3 AC-1)", () => {
  it("fires attach only on the 0→1 transition; idempotent thereafter", () => {
    const s = useViewerStore.getState();
    s.openWorkspaceWatch(TEAM_A, "ws-1");
    expect(onOpened).toHaveBeenCalledTimes(1);
    expect(onOpened).toHaveBeenCalledWith(TEAM_A, "ws-1");
    expect(useViewerStore.getState().workspaceOpenSet[TEAM_A]?.["ws-1"]).toBe(1);

    // Re-open is a no-op — no second attach (membership semantics, no leak).
    s.openWorkspaceWatch(TEAM_A, "ws-1");
    s.openWorkspaceWatch(TEAM_A, "ws-1");
    expect(onOpened).toHaveBeenCalledTimes(1);
  });

  it("fires detach only on the 1→0 transition; close-when-absent is a no-op", () => {
    const s = useViewerStore.getState();
    // Closing an unopened workspace does nothing.
    s.closeWorkspaceWatch(TEAM_A, "ws-1");
    expect(onClosed).not.toHaveBeenCalled();

    s.openWorkspaceWatch(TEAM_A, "ws-1");
    s.closeWorkspaceWatch(TEAM_A, "ws-1");
    expect(onClosed).toHaveBeenCalledTimes(1);
    expect(onClosed).toHaveBeenCalledWith(TEAM_A, "ws-1");
    // Inner key removed; empty team bucket pruned.
    expect(useViewerStore.getState().workspaceOpenSet[TEAM_A]).toBeUndefined();

    // Second close is a no-op.
    s.closeWorkspaceWatch(TEAM_A, "ws-1");
    expect(onClosed).toHaveBeenCalledTimes(1);
  });

  it("keeps refcounts isolated per team", () => {
    const s = useViewerStore.getState();
    s.openWorkspaceWatch(TEAM_A, "ws-1");
    s.openWorkspaceWatch(TEAM_B, "ws-1");
    expect(useViewerStore.getState().workspaceOpenSet[TEAM_A]?.["ws-1"]).toBe(1);
    expect(useViewerStore.getState().workspaceOpenSet[TEAM_B]?.["ws-1"]).toBe(1);
    s.closeWorkspaceWatch(TEAM_A, "ws-1");
    expect(useViewerStore.getState().workspaceOpenSet[TEAM_A]).toBeUndefined();
    expect(useViewerStore.getState().workspaceOpenSet[TEAM_B]?.["ws-1"]).toBe(1);
  });

  it("multi-workspace coexistence: closing one releases only its own watch (AC-11)", () => {
    const s = useViewerStore.getState();
    for (const id of ["a", "b", "c", "d", "e"]) s.openWorkspaceWatch(TEAM_A, id);
    expect(onOpened).toHaveBeenCalledTimes(5);
    s.closeWorkspaceWatch(TEAM_A, "c");
    expect(onClosed).toHaveBeenCalledTimes(1);
    expect(onClosed).toHaveBeenCalledWith(TEAM_A, "c");
    const open = useViewerStore.getState().workspaceOpenSet[TEAM_A] ?? {};
    expect(Object.keys(open).sort()).toEqual(["a", "b", "d", "e"]);
  });
});

describe("WorkspaceOpenSet call-site wiring (SW-WL-3 AC-1 / F-5)", () => {
  it("switchWorkspace opens a surface's watch only on activation (F-5/AC-12)", () => {
    // SW-EW-1c: root minting is retired (`ensureRootWorkspaces` is a no-op), so
    // the "mint attaches only the active root" half of this test is gone. The
    // surviving invariant — switchWorkspace attaches a surface's watch on
    // activation, and re-activation does NOT re-attach — is preserved here by
    // seeding two resolvable workspaces directly. With nothing active yet, no
    // watch is attached until the first switch.
    const ROOT_B = buildRootWorkspaceId("test-org/second-repo");
    useViewerStore.setState({
      workspaces: {
        [TEAM_A]: [
          {
            id: ROOT_ID,
            kind: "root",
            cwd: "/repo/test-repo",
            status: "ready",
            sourceRepoSlug: SLUG,
          },
          {
            id: ROOT_B,
            kind: "root",
            cwd: "/repo/second-repo",
            status: "ready",
            sourceRepoSlug: "test-org/second-repo",
          },
        ],
      },
      activeWorkspace: {},
      workspaceOpenSet: {},
    });
    // Nothing activated yet → no attach.
    expect(onOpened).not.toHaveBeenCalled();

    // Activating the first surface attaches exactly its watch.
    useViewerStore.getState().switchWorkspace(TEAM_A, ROOT_ID);
    expect(onOpened).toHaveBeenCalledTimes(1);
    expect(onOpened).toHaveBeenCalledWith(TEAM_A, ROOT_ID);
    expect(Object.keys(useViewerStore.getState().workspaceOpenSet[TEAM_A] ?? {})).toEqual([
      ROOT_ID,
    ]);

    // Switching to the second surface activates (attaches) it on demand.
    useViewerStore.getState().switchWorkspace(TEAM_A, ROOT_B);
    expect(onOpened).toHaveBeenCalledWith(TEAM_A, ROOT_B);
    expect(onOpened).toHaveBeenCalledTimes(2);

    // Re-activating an already-open surface must NOT re-attach (no leak).
    useViewerStore.getState().switchWorkspace(TEAM_A, ROOT_B);
    expect(onOpened).toHaveBeenCalledTimes(2);
  });

  it("closeTeam detaches every open watch for the team", () => {
    const s = useViewerStore.getState();
    s.openWorkspaceWatch(TEAM_A, "a");
    s.openWorkspaceWatch(TEAM_A, "b");
    onClosed.mockReset();
    s.closeTeam(TEAM_A);
    const detached = onClosed.mock.calls.map((c) => c[1]).sort();
    expect(detached).toEqual(["a", "b"]);
    expect(useViewerStore.getState().workspaceOpenSet[TEAM_A]).toBeUndefined();
  });
});

describe("WorkspaceOpenSet scale + stress (SW-WL-3 T7)", () => {
  it("AC-12 at scale: 90 present surfaces, but only activated ones attach", () => {
    // SW-EW-1c: roots are no longer minted by `ensureRootWorkspaces`, so seed
    // the 90 surfaces directly. AC-12's invariant is unchanged: presence in the
    // bar does NOT attach a watch — only the activated (switched-to) surfaces do.
    const N = 90;
    const rootIds: string[] = [];
    const wsList: Workspace[] = [];
    for (let i = 0; i < N; i++) {
      const slug = `test-org/repo-${i}`;
      const id = buildRootWorkspaceId(slug);
      rootIds.push(id);
      wsList.push({ id, kind: "root", cwd: `/repo/${i}`, status: "ready", sourceRepoSlug: slug });
    }
    useViewerStore.setState({
      workspaces: { [TEAM_A]: wsList },
      activeWorkspace: {},
      workspaceOpenSet: {},
    });
    // 90 surfaces present but none activated → nothing attached.
    expect(Object.keys(useViewerStore.getState().workspaceOpenSet[TEAM_A] ?? {}).length).toBe(0);

    // Activate the first + 5 more distinct surfaces.
    useViewerStore.getState().switchWorkspace(TEAM_A, rootIds[0]);
    for (const id of rootIds.slice(20, 25)) useViewerStore.getState().switchWorkspace(TEAM_A, id);
    const open = useViewerStore.getState().workspaceOpenSet[TEAM_A] ?? {};
    // 6 switched = 6 attached watches for 90 present surfaces.
    expect(Object.keys(open).length).toBe(6);
  });

  it("rapid open/close is balanced — membership semantics leave no refcount leak", () => {
    const s = useViewerStore.getState();
    for (let round = 0; round < 50; round++) {
      s.openWorkspaceWatch(TEAM_A, "ws-x");
      s.closeWorkspaceWatch(TEAM_A, "ws-x");
    }
    expect(useViewerStore.getState().workspaceOpenSet[TEAM_A]).toBeUndefined();
    expect(onOpened).toHaveBeenCalledTimes(50);
    expect(onClosed).toHaveBeenCalledTimes(50);
  });

  it("interleaved opens across many surfaces detach independently", () => {
    const s = useViewerStore.getState();
    const ids = Array.from({ length: 12 }, (_, i) => `ws-${i}`);
    for (const id of ids) s.openWorkspaceWatch(TEAM_A, id);
    expect(Object.keys(useViewerStore.getState().workspaceOpenSet[TEAM_A] ?? {}).length).toBe(12);
    // Close the even-indexed ones.
    for (let i = 0; i < ids.length; i += 2) s.closeWorkspaceWatch(TEAM_A, ids[i]);
    const remaining = Object.keys(useViewerStore.getState().workspaceOpenSet[TEAM_A] ?? {}).sort();
    expect(remaining).toEqual(["ws-1", "ws-11", "ws-3", "ws-5", "ws-7", "ws-9"]);
  });
});

describe("WorkspaceOpenSet surface-bound invariant (SW-WL-3 review)", () => {
  // SW-EW-1c: EC-2 (the mint-driven "unresolvable active root defers open until
  // a later ensureRootWorkspaces rerun flips creating→ready") was retired — root
  // minting/flipping is gone. The deferred-open-until-cwd-resolves invariant it
  // protected is fully covered by EC-2b below via the live switchWorkspace path.

  it("EC-2b: switchWorkspace defers opening a not-yet-cloned root until its cwd resolves", () => {
    // A root present in the bar but not yet cloned (cwd "").
    useViewerStore.setState({
      workspaces: {
        [TEAM_A]: [
          { id: ROOT_ID, kind: "root", cwd: "", status: "creating", sourceRepoSlug: SLUG },
        ],
      },
      activeWorkspace: {},
      workspaceOpenSet: {},
    });
    useViewerStore.getState().switchWorkspace(TEAM_A, ROOT_ID);
    // Activated but NOT watched — nothing on disk yet, membership must not latch.
    expect(useViewerStore.getState().activeWorkspace[TEAM_A]).toBe(ROOT_ID);
    expect(useViewerStore.getState().workspaceOpenSet[TEAM_A]).toBeUndefined();
    expect(onOpened).not.toHaveBeenCalled();

    // Clone lands → cwd populated → re-activation now attaches for the first time.
    useViewerStore.setState({
      workspaces: {
        [TEAM_A]: [
          {
            id: ROOT_ID,
            kind: "root",
            cwd: "/repo/test-repo",
            status: "ready",
            sourceRepoSlug: SLUG,
          },
        ],
      },
    });
    useViewerStore.getState().switchWorkspace(TEAM_A, ROOT_ID);
    expect(useViewerStore.getState().workspaceOpenSet[TEAM_A]?.[ROOT_ID]).toBe(1);
    expect(onOpened).toHaveBeenCalledWith(TEAM_A, ROOT_ID);
  });

  it("EC-1: closing the active surface attaches the root it falls back to", () => {
    // A root minted-but-never-activated (NOT in the open set) plus an active
    // story. Closing the story reassigns active → the root; its watcher must
    // attach so the surface the viewer now renders is watched.
    const storyId = "story-1";
    const root: Workspace = {
      id: ROOT_ID,
      kind: "root",
      cwd: "/repo/test-repo",
      status: "ready",
      sourceRepoSlug: SLUG,
    };
    const story: Workspace = {
      id: storyId,
      kind: "story",
      cwd: "/wt/story-1",
      status: "ready",
      sourceRepoSlug: SLUG,
    };
    useViewerStore.setState({
      workspaces: { [TEAM_A]: [root, story] },
      activeWorkspace: { [TEAM_A]: storyId },
      workspaceOpenSet: { [TEAM_A]: { [storyId]: 1 } }, // story open, root NOT
    });
    onOpened.mockReset();

    useViewerStore.getState().closeWorkspace(TEAM_A, storyId);

    expect(useViewerStore.getState().activeWorkspace[TEAM_A]).toBe(ROOT_ID);
    expect(Object.keys(useViewerStore.getState().workspaceOpenSet[TEAM_A] ?? {})).toEqual([
      ROOT_ID,
    ]);
    expect(onOpened).toHaveBeenCalledWith(TEAM_A, ROOT_ID);
  });
});
