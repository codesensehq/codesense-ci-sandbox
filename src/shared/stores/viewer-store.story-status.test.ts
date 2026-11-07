// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import { useViewerStore } from "./viewer-store";
import type { Workspace, SoftClosedWorkspace } from "./viewer-store";
import { buildRootWorkspaceId } from "@/shared/lib/team-key";

// T.2: per-source-repo root id (replaces legacy ROOT_WORKSPACE_ID = "root").
const ROOT_WORKSPACE_ID = buildRootWorkspaceId("test/repo");
import type { DerivedPipelineTree, PipelineStatus } from "@/shared/lib/pipeline-graph/types";
import { useOrgStore } from "./org-store";

const mockUpdatePipelineStatus = vi.fn().mockResolvedValue(undefined);
const mockBumpFileRefresh = vi.fn();

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
  updatePipelineStatus: (...args: unknown[]) => mockUpdatePipelineStatus(...args),
  detectTools: vi.fn().mockResolvedValue({
    terminals: [],
    editors: [],
    default_terminal: "",
    default_editor: "",
  }),
}));

vi.mock("./file-refresh-store", () => ({
  useFileRefreshStore: {
    getState: () => ({
      bumpFileRefresh: mockBumpFileRefresh,
    }),
  },
}));

const repo = "/test/repo";
const contextRepo = "/test/context-repo";

function resetState() {
  useViewerStore.setState({
    workspaces: {},
    softClosedWorkspaces: {},
    activeWorkspace: {},
  });
}

// SW-FX-30: a minimized (soft-closed) workspace's frozen shadow entry.
const softClosed = (ws: Workspace): SoftClosedWorkspace => ({ workspace: ws });

const storyWorkspace = (
  id: string,
  storyFilePath: string,
  storyStatus?: PipelineStatus,
): Workspace => ({
  id,
  kind: "story",
  cwd: `/worktrees/${id}`,
  storyFilePath,
  storyId: id,
  status: "ready",
  ...(storyStatus ? { storyStatus } : {}),
});

const rootWorkspace: Workspace = {
  id: ROOT_WORKSPACE_ID,
  kind: "root",
  cwd: repo,
  status: "ready",
};

const makeTree = (
  stories: Array<{
    filePath: string;
    status: string | null;
    storyId?: string;
    sourceRepo?: string | null;
  }>,
): DerivedPipelineTree => ({
  dirPath: "",
  dirName: "root",
  absoluteDirPath: contextRepo,
  stories: stories.map((s) => ({
    fileName: s.filePath.split("/").pop()!,
    filePath: s.filePath,
    title: null,
    dependsOn: null,
    status: s.status,
    storyText: null,
    documentType: null,
    supportingDocs: null,
    storyId: s.storyId,
    sourceRepo: s.sourceRepo ?? null,
    team: null,
  })),
  subEpics: [],
  orderError: null,
  mainDoc: null,
});

describe("syncWorkspaceFrontmatter", () => {
  beforeEach(() => {
    resetState();
    vi.clearAllMocks();
  });

  it("a workspace dropped by another path than a close leaves the stored order", () => {
    const w5 = storyWorkspace("W-5", `${contextRepo}/workspaces/W-5-foo.md`);
    useViewerStore.setState({
      workspaces: { [repo]: [rootWorkspace, w5] },
      // "gone" left the team without a close (a sync, a move) after the person ordered it.
      workspaceOrderByTeam: { [repo]: ["gone", w5.id, rootWorkspace.id] },
    });
    const tree = makeTree([{ filePath: `${contextRepo}/workspaces/W-5-foo.md`, status: "todo" }]);
    useViewerStore.getState().syncWorkspaceFrontmatter(repo, tree);
    expect(useViewerStore.getState().workspaceOrderByTeam[repo]).toEqual([w5.id, rootWorkspace.id]);
    expect(useViewerStore.getState().trayBaseOrderByTeam[repo]).not.toContain("gone");
  });

  it("updates storyStatus for matching workspaces", () => {
    useViewerStore.setState({
      workspaces: {
        [repo]: [rootWorkspace, storyWorkspace("W-5", `${contextRepo}/workspaces/W-5-foo.md`)],
      },
    });

    const tree = makeTree([
      { filePath: `${contextRepo}/workspaces/W-5-foo.md`, status: "in-progress" },
    ]);
    useViewerStore.getState().syncWorkspaceFrontmatter(repo, tree);

    const ws = useViewerStore.getState().workspaces[repo]!;
    expect(ws[1].storyStatus).toBe("in-progress");
  });

  // SLW-IN-9 AC-3-FE: move-resilient reconnect. A workspace whose story moved
  // (same durable story_id, new path) has its path hint repointed to the
  // current location on the next tree refresh — self-healing reconnect, tray
  // grouping, and doc-open without a manual reopen.
  it("repoints storyFilePath when a story moves (same story_id, new path)", () => {
    const oldPath = `${contextRepo}/old/SLW-IN-9.md`;
    const newPath = `${contextRepo}/new/SLW-IN-9.md`;
    useViewerStore.setState({
      workspaces: {
        [repo]: [rootWorkspace, storyWorkspace("SLW-IN-9", oldPath, "todo")],
      },
    });
    // The SAME story_id now lives at a DIFFERENT path (the move).
    const tree = makeTree([{ filePath: newPath, status: "in-progress", storyId: "SLW-IN-9" }]);
    useViewerStore.getState().syncWorkspaceFrontmatter(repo, tree);

    const ws = useViewerStore.getState().workspaces[repo]!;
    expect(ws[1].storyFilePath).toBe(newPath);
    expect(ws[1].storyStatus).toBe("in-progress"); // status still reconciles
  });

  // SLW-IN-9 AC-3-FE guard: the path-stem fallback matches a DIFFERENT physical
  // copy (context-repo source vs `_bmad-output/` projection, same basename). It
  // must sync display fields but NEVER rewrite the workspace's path to the other
  // copy — only the unambiguous story_id match is a genuine move.
  it("does NOT repoint storyFilePath on a path-stem (projected copy) match", () => {
    const wsPath = `${contextRepo}/a/S.md`;
    const otherCopy = `${contextRepo}/b/S.md`;
    // storyId === path stem "S" ⇒ no real frontmatter-id signal ⇒ stem fallback.
    useViewerStore.setState({
      workspaces: {
        [repo]: [rootWorkspace, storyWorkspace("S", wsPath)],
      },
    });
    // A different physical copy (same basename, no story_id) — a stem match only.
    const tree = makeTree([{ filePath: otherCopy, status: "done" }]);
    useViewerStore.getState().syncWorkspaceFrontmatter(repo, tree);

    const ws = useViewerStore.getState().workspaces[repo]!;
    expect(ws[1].storyFilePath).toBe(wsPath); // path NOT rewritten to the other copy
    expect(ws[1].storyStatus).toBe("done"); // but status still syncs
  });

  // SLW-IN-9 AC-4: an epic workspace's doc path (`epicDocPath`, the README) is a
  // stale snapshot that 404s after the epic's main doc moves. It is repointed
  // live from the tree by epic identity (`epicDirPath` → `mainDoc.filePath`) so
  // double-clicking the epic title opens the current doc.
  it("repoints epicDocPath when the epic's main doc moves", () => {
    const oldReadme = `${contextRepo}/old/epic-x/README.md`;
    const newReadme = `${contextRepo}/new/epic-x/README.md`;
    const epicWorkspace: Workspace = {
      id: "epic:epic-x",
      kind: "epic",
      cwd: "/worktrees/epic-x",
      epicDirPath: "epic-x",
      epicDocPath: oldReadme,
      status: "ready",
    };
    useViewerStore.setState({ workspaces: { [repo]: [rootWorkspace, epicWorkspace] } });

    // Tree: a sub-epic named "epic-x" whose main doc now lives at newReadme.
    const tree: DerivedPipelineTree = {
      ...makeTree([]),
      subEpics: [
        {
          ...makeTree([]),
          dirName: "epic-x",
          dirPath: "epic-x",
          mainDoc: { filePath: newReadme, supportingDocs: null, purposeText: null },
        },
      ],
    };
    useViewerStore.getState().syncWorkspaceFrontmatter(repo, tree);

    const ws = useViewerStore.getState().workspaces[repo]!;
    expect(ws[1].epicDocPath).toBe(newReadme);
  });

  it("ignores root workspace", () => {
    useViewerStore.setState({
      workspaces: { [repo]: [rootWorkspace] },
    });

    const tree = makeTree([{ filePath: `${contextRepo}/some/story.md`, status: "done" }]);
    useViewerStore.getState().syncWorkspaceFrontmatter(repo, tree);

    const ws = useViewerStore.getState().workspaces[repo]!;
    expect(ws[0].storyStatus).toBeUndefined();
  });

  it("handles workspaces with no storyFilePath", () => {
    const noPath: Workspace = { id: "orphan", kind: "story", cwd: "/x", status: "ready" };
    useViewerStore.setState({
      workspaces: { [repo]: [rootWorkspace, noPath] },
    });

    const tree = makeTree([{ filePath: `${contextRepo}/story.md`, status: "todo" }]);
    useViewerStore.getState().syncWorkspaceFrontmatter(repo, tree);

    const ws = useViewerStore.getState().workspaces[repo]!;
    expect(ws[1].storyStatus).toBeUndefined();
  });

  it("normalizes raw status strings", () => {
    useViewerStore.setState({
      workspaces: {
        [repo]: [rootWorkspace, storyWorkspace("F-1", `${contextRepo}/stories/F-1.md`)],
      },
    });

    // SW-GT-3: "invalid-status" is off-taxonomy → normalizes to the taxonomy
    // default (first state by order = planning), not the old hardcoded "todo".
    const tree = makeTree([
      { filePath: `${contextRepo}/stories/F-1.md`, status: "invalid-status" },
    ]);
    useViewerStore.getState().syncWorkspaceFrontmatter(repo, tree);

    const ws = useViewerStore.getState().workspaces[repo]!;
    expect(ws[1].storyStatus).toBe("planning");
  });

  it("syncs storyId from frontmatter when present", () => {
    useViewerStore.setState({
      workspaces: {
        [repo]: [
          rootWorkspace,
          storyWorkspace(
            "W-5-source-repo-management-picker",
            `${contextRepo}/workspaces/W-5-source-repo-management-picker.md`,
          ),
        ],
      },
    });

    const tree = makeTree([
      {
        filePath: `${contextRepo}/workspaces/W-5-source-repo-management-picker.md`,
        status: "review",
        storyId: "W-5",
      },
    ]);
    useViewerStore.getState().syncWorkspaceFrontmatter(repo, tree);

    const ws = useViewerStore.getState().workspaces[repo]!;
    expect(ws[1].storyId).toBe("W-5");
  });

  it("R-18: falls back to storyIdFromPath when ref.storyId is absent", () => {
    // Workspace seeded with a stale `storyId` (e.g. survived from an older
    // path-derived snapshot or a prior frontmatter value that's since been
    // removed). Sync sees `ref.storyId === undefined` and should reset to
    // the path-derived form rather than preserving the stale value.
    useViewerStore.setState({
      workspaces: {
        [repo]: [
          rootWorkspace,
          {
            id: "my-story",
            kind: "story",
            cwd: "/worktrees/my-story",
            storyFilePath: `${contextRepo}/stories/my-story.md`,
            storyId: "STALE-VALUE",
            status: "ready",
          },
        ],
      },
    });

    const tree = makeTree([{ filePath: `${contextRepo}/stories/my-story.md`, status: "todo" }]);
    useViewerStore.getState().syncWorkspaceFrontmatter(repo, tree);

    const ws = useViewerStore.getState().workspaces[repo]!;
    expect(ws[1].storyId).toBe("my-story");
  });

  it("R-18: reconciles sourceRepoSlug from ref.sourceRepo", () => {
    useViewerStore.setState({
      workspaces: {
        [repo]: [
          rootWorkspace,
          {
            id: "X-1",
            kind: "story",
            cwd: "/worktrees/X-1",
            storyFilePath: `${contextRepo}/stories/X-1.md`,
            storyId: "X-1",
            sourceRepoSlug: "alderco/old",
            status: "ready",
          },
        ],
      },
    });

    const tree = makeTree([
      {
        filePath: `${contextRepo}/stories/X-1.md`,
        status: "todo",
        sourceRepo: "alderco/new",
      },
    ]);
    useViewerStore.getState().syncWorkspaceFrontmatter(repo, tree);

    const ws = useViewerStore.getState().workspaces[repo]!;
    expect(ws[1].sourceRepoSlug).toBe("alderco/new");
  });

  it("R-18: clears sourceRepoSlug when ref.sourceRepo is absent", () => {
    useViewerStore.setState({
      workspaces: {
        [repo]: [
          rootWorkspace,
          {
            id: "X-1",
            kind: "story",
            cwd: "/worktrees/X-1",
            storyFilePath: `${contextRepo}/stories/X-1.md`,
            storyId: "X-1",
            sourceRepoSlug: "alderco/old",
            status: "ready",
          },
        ],
      },
    });

    const tree = makeTree([{ filePath: `${contextRepo}/stories/X-1.md`, status: "todo" }]);
    useViewerStore.getState().syncWorkspaceFrontmatter(repo, tree);

    const ws = useViewerStore.getState().workspaces[repo]!;
    expect(ws[1].sourceRepoSlug).toBeUndefined();
  });

  it("R-18: combined sync of storyId + sourceRepoSlug in one pass (AC #20)", () => {
    useViewerStore.setState({
      workspaces: {
        [repo]: [
          rootWorkspace,
          {
            id: "R-16-hooks-as-real-dir",
            kind: "story",
            cwd: "/worktrees/R-16",
            storyFilePath: `${contextRepo}/stories/R-16-hooks-as-real-dir.md`,
            storyId: "R-16-hooks-as-real-dir",
            sourceRepoSlug: "alderco/codesense",
            status: "ready",
          },
        ],
      },
    });

    const tree = makeTree([
      {
        filePath: `${contextRepo}/stories/R-16-hooks-as-real-dir.md`,
        status: "in-progress",
        storyId: "R-16",
        sourceRepo: "alderco/codesense",
      },
    ]);
    useViewerStore.getState().syncWorkspaceFrontmatter(repo, tree);

    const ws = useViewerStore.getState().workspaces[repo]!;
    expect(ws[1].id).toBe("R-16-hooks-as-real-dir"); // identity unchanged
    expect(ws[1].storyId).toBe("R-16"); // display reconciled to frontmatter
    expect(ws[1].sourceRepoSlug).toBe("alderco/codesense"); // unchanged
    expect(ws[1].storyStatus).toBe("in-progress");
  });

  // R-18 Phase B (AC #27, #29 part 3): the use-case that motivates the
  // sync-on-add wiring at `repo-viewer-view.tsx`. A workspace added AFTER
  // the tree has resolved (so the tree-change useEffect doesn't re-fire)
  // must still get its frontmatter fields reconciled. This test asserts
  // the SEMANTIC invariant — sync correctly populates a freshly-added
  // workspace's `storyStatus`, `storyId`, `sourceRepoSlug` when invoked.
  // The dep-array wiring at the useEffect is verified separately by
  // Task 13's manual verification step.
  it("R-18 Phase B: freshly-added workspace gets fully synced on first call (AC #27)", () => {
    // Workspace has only what `addStoryWorkspace` would write at creation:
    // `id` (path-derived), `storyId` (path-derived fallback per Phase A),
    // `storyFilePath`. Crucially, `storyStatus` is undefined and
    // `sourceRepoSlug` is undefined — these are the fields that R-13
    // observed staying stale on persisted workspaces.
    useViewerStore.setState({
      workspaces: {
        [repo]: [
          rootWorkspace,
          {
            id: "R-12-capture-switch-serialization",
            kind: "story",
            cwd: "/worktrees/R-12",
            storyFilePath: `${contextRepo}/stories/R-12-capture-switch-serialization.md`,
            storyId: "R-12-capture-switch-serialization", // path-derived fallback
            status: "ready",
            // storyStatus, sourceRepoSlug — both undefined (fresh add).
          },
        ],
      },
    });

    const tree = makeTree([
      {
        filePath: `${contextRepo}/stories/R-12-capture-switch-serialization.md`,
        status: "planning",
        storyId: "R-12",
        sourceRepo: "alderco/codesense",
      },
    ]);
    useViewerStore.getState().syncWorkspaceFrontmatter(repo, tree);

    const ws = useViewerStore.getState().workspaces[repo]!;
    // All three frontmatter-derived fields populated in one pass.
    expect(ws[1].storyStatus).toBe("planning");
    expect(ws[1].storyId).toBe("R-12");
    expect(ws[1].sourceRepoSlug).toBe("alderco/codesense");
    // Identity invariant: `id` did not change.
    expect(ws[1].id).toBe("R-12-capture-switch-serialization");
  });

  // ── SW-FX-44: robust to persisted-path vs scan-path drift ──────────────
  // The reconcile matched on absolute-path identity only, so a workspace
  // whose persisted `storyFilePath` diverged from the scan's `filePath`
  // (source-vs-projected copy, or a moved/renamed file) was silently
  // skipped and kept a stale `storyStatus`. Two secondary match keys close
  // the gap without cross-wiring workspaces on duplicate ids.

  it("SW-FX-44: source-vs-projected path drift resolves via filename stem", () => {
    // Persisted path is the PROJECTED worktree copy; the scan emits the
    // SOURCE context-repo copy. Different parent dir, identical stem
    // (`SW-FX-44-foo`). The absolute-path match misses; the path-stem
    // fallback must catch it.
    useViewerStore.setState({
      workspaces: {
        [repo]: [
          rootWorkspace,
          storyWorkspace(
            "SW-FX-44-foo",
            `/worktrees/proj/_bmad-output/SW-FX-44-foo.md`,
            "in-progress",
          ),
        ],
      },
    });

    const tree = makeTree([
      {
        filePath: `${contextRepo}/_team/default/content/_bmad-output/SW-FX-44-foo.md`,
        status: "done",
        storyId: "SW-FX-44",
      },
    ]);
    useViewerStore.getState().syncWorkspaceFrontmatter(repo, tree);

    const ws = useViewerStore.getState().workspaces[repo]!;
    expect(ws[1].storyStatus).toBe("done");
    expect(ws[1].storyId).toBe("SW-FX-44");
  });

  it("SW-FX-44: moved/renamed story file resolves via frontmatter story_id", () => {
    // The file was renamed on disk (stem changed old→new). The workspace's
    // persisted path + stem are both stale, but `ws.storyId` already holds
    // the frontmatter id (from a prior reconcile). The frontmatter-id
    // fallback must catch it even though both path and stem miss.
    useViewerStore.setState({
      workspaces: {
        [repo]: [
          rootWorkspace,
          {
            id: "SW-FX-44-old-name",
            kind: "story",
            cwd: "/worktrees/SW-FX-44",
            storyFilePath: `${contextRepo}/stories/SW-FX-44-old-name.md`,
            storyId: "SW-FX-44", // frontmatter id, stable across the rename
            storyStatus: "in-progress",
            status: "ready",
          },
        ],
      },
    });

    const tree = makeTree([
      {
        filePath: `${contextRepo}/stories/SW-FX-44-new-name.md`,
        status: "done",
        storyId: "SW-FX-44",
      },
    ]);
    useViewerStore.getState().syncWorkspaceFrontmatter(repo, tree);

    const ws = useViewerStore.getState().workspaces[repo]!;
    expect(ws[1].storyStatus).toBe("done");
  });

  it("SW-FX-44: exact absolute-path match takes precedence over id fallbacks", () => {
    // A workspace whose path matches ref A exactly must adopt ref A's
    // status, even when ref B shares its stem with a different status.
    useViewerStore.setState({
      workspaces: {
        [repo]: [rootWorkspace, storyWorkspace("dup-stem", `${contextRepo}/a/dup-stem.md`, "todo")],
      },
    });

    const tree = makeTree([
      { filePath: `${contextRepo}/a/dup-stem.md`, status: "done", storyId: "A-1" },
      { filePath: `${contextRepo}/b/dup-stem.md`, status: "planning", storyId: "B-2" },
    ]);
    useViewerStore.getState().syncWorkspaceFrontmatter(repo, tree);

    const ws = useViewerStore.getState().workspaces[repo]!;
    expect(ws[1].storyStatus).toBe("done"); // ref A, not ref B
  });

  it("SW-FX-44: ambiguous frontmatter id (dup across refs) is dropped, not mis-assigned", () => {
    // Two refs share `story_id: DUP` at different paths. A workspace keyed
    // on that id — with a path that matches neither — must NOT be wired to
    // either ref. It stays stale (safe no-op), never a wrong glyph.
    useViewerStore.setState({
      workspaces: {
        [repo]: [
          rootWorkspace,
          {
            id: "dup-id-ws",
            kind: "story",
            cwd: "/worktrees/dup",
            storyFilePath: `${contextRepo}/stories/dup-id-ws.md`,
            storyId: "DUP",
            storyStatus: "in-progress",
            status: "ready",
          },
        ],
      },
    });

    const tree = makeTree([
      { filePath: `${contextRepo}/x/first.md`, status: "done", storyId: "DUP" },
      { filePath: `${contextRepo}/y/second.md`, status: "planning", storyId: "DUP" },
    ]);
    useViewerStore.getState().syncWorkspaceFrontmatter(repo, tree);

    const ws = useViewerStore.getState().workspaces[repo]!;
    // Unchanged — neither "done" nor "planning" was applied.
    expect(ws[1].storyStatus).toBe("in-progress");
  });

  it("SW-FX-44: ambiguous filename stem (dup across refs) is dropped, not mis-assigned", () => {
    // Two refs share the stem `README` at different paths. A workspace with
    // that stem but a non-matching path must not adopt either ref's status.
    useViewerStore.setState({
      workspaces: {
        [repo]: [
          rootWorkspace,
          {
            id: "readme-ws",
            kind: "story",
            cwd: "/worktrees/readme",
            storyFilePath: `${contextRepo}/c/README.md`,
            storyId: "readme-ws",
            storyStatus: "in-progress",
            status: "ready",
          },
        ],
      },
    });

    const tree = makeTree([
      { filePath: `${contextRepo}/a/README.md`, status: "done" },
      { filePath: `${contextRepo}/b/README.md`, status: "planning" },
    ]);
    useViewerStore.getState().syncWorkspaceFrontmatter(repo, tree);

    const ws = useViewerStore.getState().workspaces[repo]!;
    expect(ws[1].storyStatus).toBe("in-progress"); // unchanged
  });

  it("SW-FX-44: lone same-stem impostor with a conflicting story_id is rejected", () => {
    // Code-review round 1 (Edge Case Hunter / Blind Hunter): the dup guard
    // only catches collisions present TOGETHER in the scan. Here the
    // workspace's own file has left the scan and a single UNRELATED story
    // shares its filename stem but carries a DIFFERENT frontmatter id. The
    // workspace holds a real frontmatter id (`SW-FX-44`, distinct from its
    // stem `SW-FX-44-foo`), so the stem match must be rejected on id
    // conflict — safe staleness, never a wrong glyph / mis-routed slug.
    useViewerStore.setState({
      workspaces: {
        [repo]: [
          rootWorkspace,
          {
            id: "SW-FX-44-foo",
            kind: "story",
            cwd: "/worktrees/SW-FX-44",
            storyFilePath: `${contextRepo}/gone/SW-FX-44-foo.md`,
            storyId: "SW-FX-44", // real frontmatter id, != stem "SW-FX-44-foo"
            sourceRepoSlug: "alderco/codesense",
            storyStatus: "in-progress",
            status: "ready",
          },
        ],
      },
    });

    const tree = makeTree([
      {
        // Different story, same stem, different frontmatter id.
        filePath: `${contextRepo}/other/SW-FX-44-foo.md`,
        status: "done",
        storyId: "OTHER-99",
        sourceRepo: "alderco/other",
      },
    ]);
    useViewerStore.getState().syncWorkspaceFrontmatter(repo, tree);

    const ws = useViewerStore.getState().workspaces[repo]!;
    expect(ws[1].storyStatus).toBe("in-progress"); // not "done"
    expect(ws[1].sourceRepoSlug).toBe("alderco/codesense"); // not mis-routed
  });

  it("SW-FX-44: lone same-stem impostor with NO story_id is rejected (positive corroboration)", () => {
    // Code-review round 2 (Blind Hunter / Edge Case Hunter): an absent
    // candidate `story_id` is NOT corroboration. A workspace holding a real
    // frontmatter id must reject a same-stem candidate that carries no
    // `story_id` at all — otherwise the id-less impostor cross-wires status
    // and `sourceRepoSlug`. (Rejecting only on *active* id conflict left
    // this hole open.)
    useViewerStore.setState({
      workspaces: {
        [repo]: [
          rootWorkspace,
          {
            id: "SW-FX-44-foo",
            kind: "story",
            cwd: "/worktrees/SW-FX-44",
            storyFilePath: `${contextRepo}/gone/SW-FX-44-foo.md`,
            storyId: "SW-FX-44", // real frontmatter id, != stem "SW-FX-44-foo"
            sourceRepoSlug: "alderco/codesense",
            storyStatus: "in-progress",
            status: "ready",
          },
        ],
      },
    });

    const tree = makeTree([
      {
        // Different story, same stem, NO story_id (absent frontmatter id).
        filePath: `${contextRepo}/other/SW-FX-44-foo.md`,
        status: "done",
        sourceRepo: "alderco/other",
      },
    ]);
    useViewerStore.getState().syncWorkspaceFrontmatter(repo, tree);

    const ws = useViewerStore.getState().workspaces[repo]!;
    expect(ws[1].storyStatus).toBe("in-progress"); // not "done"
    expect(ws[1].sourceRepoSlug).toBe("alderco/codesense"); // not mis-routed
    expect(ws[1].storyId).toBe("SW-FX-44"); // real id not downgraded to stem
  });

  // ── SW-FX-30: reconcile the soft-closed (minimized) shadow too ─────────
  // The minimized tray renders the frozen `softClosedWorkspaces` shadow
  // `Workspace`. Prior to SW-FX-30 the reconcile mapped the live array only,
  // so a minimized card's status pill froze at minimize time. The shadow
  // pass mirrors PT-4's shadow-aware `aiStatus` treatment.

  it("SW-FX-30 (AC-1): updates a minimized workspace's storyStatus/storyId/sourceRepoSlug", () => {
    useViewerStore.setState({
      workspaces: { [repo]: [rootWorkspace, storyWorkspace("LIVE-1", `${contextRepo}/LIVE-1.md`)] },
      softClosedWorkspaces: {
        [repo]: {
          "MIN-1": softClosed(storyWorkspace("MIN-1", `${contextRepo}/MIN-1.md`, "in-progress")),
        },
      },
    });

    const tree = makeTree([
      { filePath: `${contextRepo}/LIVE-1.md`, status: "review", storyId: "LIVE-1" },
      {
        filePath: `${contextRepo}/MIN-1.md`,
        status: "done",
        storyId: "MIN-1-fm",
        sourceRepo: "alderco/codesense",
      },
    ]);
    useViewerStore.getState().syncWorkspaceFrontmatter(repo, tree);

    // Shadow record reconciled (AC-1).
    const shadow = useViewerStore.getState().softClosedWorkspaces[repo]!["MIN-1"].workspace;
    expect(shadow.storyStatus).toBe("done");
    expect(shadow.storyId).toBe("MIN-1-fm");
    expect(shadow.sourceRepoSlug).toBe("alderco/codesense");
    // Live still reconciled in the same pass (AC-4).
    expect(useViewerStore.getState().workspaces[repo]![1].storyStatus).toBe("review");
  });

  it("SW-FX-30 (AC-3): reconciles the shadow even when the live array is empty (all minimized)", () => {
    useViewerStore.setState({
      workspaces: { [repo]: [] },
      softClosedWorkspaces: {
        [repo]: {
          "MIN-1": softClosed(storyWorkspace("MIN-1", `${contextRepo}/MIN-1.md`, "todo")),
        },
      },
    });

    const tree = makeTree([
      { filePath: `${contextRepo}/MIN-1.md`, status: "done", storyId: "MIN-1" },
    ]);
    useViewerStore.getState().syncWorkspaceFrontmatter(repo, tree);

    const shadow = useViewerStore.getState().softClosedWorkspaces[repo]!["MIN-1"].workspace;
    expect(shadow.storyStatus).toBe("done");
  });

  it("SW-FX-30 (AC-3): reconciles the shadow when the live array key is absent entirely", () => {
    // No `workspaces[repo]` at all (undefined, not just empty) — the relaxed
    // guard must still run the shadow pass.
    useViewerStore.setState({
      workspaces: {},
      softClosedWorkspaces: {
        [repo]: {
          "MIN-1": softClosed(storyWorkspace("MIN-1", `${contextRepo}/MIN-1.md`, "todo")),
        },
      },
    });

    const tree = makeTree([
      { filePath: `${contextRepo}/MIN-1.md`, status: "review", storyId: "MIN-1" },
    ]);
    useViewerStore.getState().syncWorkspaceFrontmatter(repo, tree);

    const shadow = useViewerStore.getState().softClosedWorkspaces[repo]!["MIN-1"].workspace;
    expect(shadow.storyStatus).toBe("review");
  });

  it("SW-FX-30 guard: a non-story (epic) shadow entry is left untouched (frontmatterReconcile=false)", () => {
    const epicWs: Workspace = {
      id: "EPIC-1",
      kind: "epic",
      cwd: "/worktrees/EPIC-1",
      storyFilePath: `${contextRepo}/EPIC-1.md`,
      status: "ready",
    };
    useViewerStore.setState({
      workspaces: { [repo]: [] },
      softClosedWorkspaces: { [repo]: { "EPIC-1": softClosed(epicWs) } },
    });

    const tree = makeTree([
      { filePath: `${contextRepo}/EPIC-1.md`, status: "done", storyId: "EPIC-1" },
    ]);
    useViewerStore.getState().syncWorkspaceFrontmatter(repo, tree);

    // The gate skips it — no storyStatus written onto a non-story shadow record.
    const shadow = useViewerStore.getState().softClosedWorkspaces[repo]!["EPIC-1"].workspace;
    expect(shadow.storyStatus).toBeUndefined();
  });

  it("SW-FX-30: a no-op tick leaves the shadow object reference unchanged (no churn)", () => {
    const min = softClosed(storyWorkspace("MIN-1", `${contextRepo}/MIN-1.md`, "done"));
    useViewerStore.setState({
      workspaces: { [repo]: [] },
      softClosedWorkspaces: { [repo]: { "MIN-1": min } },
    });

    // Tree already agrees with the shadow → nothing should change.
    const tree = makeTree([
      { filePath: `${contextRepo}/MIN-1.md`, status: "done", storyId: "MIN-1" },
    ]);
    const before = useViewerStore.getState().softClosedWorkspaces[repo];
    useViewerStore.getState().syncWorkspaceFrontmatter(repo, tree);
    const after = useViewerStore.getState().softClosedWorkspaces[repo];
    expect(after).toBe(before); // same reference — no set() fired
  });
});

describe("changeStoryStatus", () => {
  beforeEach(() => {
    resetState();
    vi.clearAllMocks();
  });

  it("optimistically updates storyStatus and calls updatePipelineStatus + bumpFileRefresh", async () => {
    // T.1: changeStoryStatus is team-keyed. The bumpFileRefresh call
    // resolves the workspace's `sourceRepoSlug` against `useOrgStore.workspaceRepos`
    // to find the per-source-repo refresh signal. Set up a workspaceRepos
    // entry so the resolution succeeds.
    useOrgStore.setState({
      workspaceRepos: {
        "workspace:test-org:default:acme/api": {
          repoPath: "workspace:test-org:default:acme/api",
          repoName: "api",
          contextRepoPath: "/ctx",
          teamContentPath: "",
          hasRemote: false,
          remoteUrl: null,
          mainDoc: "",
        },
      },
    });
    const story = {
      ...storyWorkspace("W-5", `${contextRepo}/W-5.md`, "todo"),
      sourceRepoSlug: "acme/api",
    };
    useViewerStore.setState({
      workspaces: {
        [repo]: [rootWorkspace, story],
      },
    });

    await useViewerStore.getState().changeStoryStatus(repo, "W-5", "in-progress");

    // Check optimistic update
    const ws = useViewerStore.getState().workspaces[repo]!;
    expect(ws[1].storyStatus).toBe("in-progress");

    // Check Tauri command called
    expect(mockUpdatePipelineStatus).toHaveBeenCalledWith(`${contextRepo}/W-5.md`, "in-progress");

    // Check file refresh triggered against the source repo's synthetic path.
    expect(mockBumpFileRefresh).toHaveBeenCalledWith("workspace:test-org:default:acme/api");
  });

  it("refreshes only the host-qualified repository for a cross-host twin", async () => {
    const teamKey = "test-org:default";
    useOrgStore.setState({
      workspaceRepos: {
        "workspace:test-org:default:acme/api": {
          repoPath: "workspace:test-org:default:acme/api",
          repoName: "api",
          contextRepoPath: "/ctx/github",
          teamContentPath: "",
          hasRemote: false,
          remoteUrl: null,
          mainDoc: "",
        },
        "workspace:test-org:default:gitlab.com/acme/api": {
          repoPath: "workspace:test-org:default:gitlab.com/acme/api",
          repoName: "api",
          contextRepoPath: "/ctx/gitlab",
          teamContentPath: "",
          hasRemote: false,
          remoteUrl: null,
          mainDoc: "",
        },
      },
    });
    useViewerStore.setState({
      workspaces: {
        [teamKey]: [
          rootWorkspace,
          {
            ...storyWorkspace("W-GL", `${contextRepo}/W-GL.md`, "todo"),
            sourceRepoSlug: "gitlab.com/acme/api",
          },
        ],
      },
    });

    await useViewerStore.getState().changeStoryStatus(teamKey, "W-GL", "in-progress");

    expect(mockBumpFileRefresh).toHaveBeenCalledTimes(1);
    expect(mockBumpFileRefresh).toHaveBeenCalledWith(
      "workspace:test-org:default:gitlab.com/acme/api",
    );
  });

  it("reverts optimistic update on error and toasts", async () => {
    mockUpdatePipelineStatus.mockRejectedValueOnce(new Error("write failed"));

    useViewerStore.setState({
      workspaces: {
        [repo]: [rootWorkspace, storyWorkspace("W-5", `${contextRepo}/W-5.md`, "todo")],
      },
    });

    await useViewerStore.getState().changeStoryStatus(repo, "W-5", "in-progress");

    // Should revert to original status
    const ws = useViewerStore.getState().workspaces[repo]!;
    expect(ws[1].storyStatus).toBe("todo");
  });

  it("no-ops when workspace not found", async () => {
    useViewerStore.setState({
      workspaces: { [repo]: [rootWorkspace] },
    });

    await useViewerStore.getState().changeStoryStatus(repo, "nonexistent", "done");
    expect(mockUpdatePipelineStatus).not.toHaveBeenCalled();
  });

  // ── SW-FX-30: the popover write path must reach the shadow too ──────────

  it("SW-FX-30 (AC-2): resolves a minimized workspace via the shadow and writes to disk", async () => {
    useViewerStore.setState({
      workspaces: { [repo]: [] },
      softClosedWorkspaces: {
        [repo]: {
          "MIN-1": softClosed(storyWorkspace("MIN-1", `${contextRepo}/MIN-1.md`, "todo")),
        },
      },
    });

    await useViewerStore.getState().changeStoryStatus(repo, "MIN-1", "in-progress");

    // Optimistic update lands on the SHADOW record (not the live array).
    const shadow = useViewerStore.getState().softClosedWorkspaces[repo]!["MIN-1"].workspace;
    expect(shadow.storyStatus).toBe("in-progress");
    // Disk write still fired against the shadow workspace's storyFilePath.
    expect(mockUpdatePipelineStatus).toHaveBeenCalledWith(`${contextRepo}/MIN-1.md`, "in-progress");
  });

  it("SW-FX-30 (AC-2): reverts the SHADOW record on a failed disk write", async () => {
    mockUpdatePipelineStatus.mockRejectedValueOnce(new Error("write failed"));
    useViewerStore.setState({
      workspaces: { [repo]: [] },
      softClosedWorkspaces: {
        [repo]: {
          "MIN-1": softClosed(storyWorkspace("MIN-1", `${contextRepo}/MIN-1.md`, "todo")),
        },
      },
    });

    await useViewerStore.getState().changeStoryStatus(repo, "MIN-1", "in-progress");

    const shadow = useViewerStore.getState().softClosedWorkspaces[repo]!["MIN-1"].workspace;
    expect(shadow.storyStatus).toBe("todo"); // reverted
  });

  it("SW-FX-30 guard: a live-workspace change does NOT mutate the shadow slice", async () => {
    const untouched = softClosed(storyWorkspace("MIN-1", `${contextRepo}/MIN-1.md`, "todo"));
    useViewerStore.setState({
      workspaces: {
        [repo]: [rootWorkspace, storyWorkspace("W-5", `${contextRepo}/W-5.md`, "todo")],
      },
      softClosedWorkspaces: { [repo]: { "MIN-1": untouched } },
    });

    await useViewerStore.getState().changeStoryStatus(repo, "W-5", "done");

    // Live slice updated; shadow slice reference and value untouched.
    expect(useViewerStore.getState().workspaces[repo]![1].storyStatus).toBe("done");
    expect(useViewerStore.getState().softClosedWorkspaces[repo]!["MIN-1"]).toBe(untouched);
  });

  it("SW-FX-30 (review-HIGH): revert re-resolves the slice when the workspace is restored mid-write", async () => {
    // The record starts minimized (shadow). The disk write fails, but a
    // concurrent restore (shadow → live) lands BEFORE the rejection propagates.
    // The revert must follow the record to the live slice, not strand the
    // failed optimistic value on the now-empty shadow.
    useViewerStore.setState({
      workspaces: { [repo]: [] },
      softClosedWorkspaces: {
        [repo]: {
          "MIN-1": softClosed(storyWorkspace("MIN-1", `${contextRepo}/MIN-1.md`, "todo")),
        },
      },
    });

    mockUpdatePipelineStatus.mockImplementationOnce(async () => {
      // Simulate a reopen racing the in-flight write: move the record (carrying
      // its optimistic status) from the shadow into the live array.
      const cur = useViewerStore.getState().softClosedWorkspaces[repo]!["MIN-1"].workspace;
      useViewerStore.setState({
        workspaces: { [repo]: [cur] },
        softClosedWorkspaces: { [repo]: {} },
      });
      throw new Error("write failed");
    });

    await useViewerStore.getState().changeStoryStatus(repo, "MIN-1", "in-progress");

    // Revert landed on the LIVE slice (where the record now lives).
    const live = useViewerStore.getState().workspaces[repo]!.find((w) => w.id === "MIN-1")!;
    expect(live.storyStatus).toBe("todo");
  });
});

describe("syncWorkspaceFrontmatter — slice referential stability", () => {
  beforeEach(() => {
    resetState();
    vi.clearAllMocks();
  });

  it("SW-FX-30 (review-LOW): a shadow-only reconcile leaves the live workspaces reference unchanged", () => {
    // Live already agrees with the tree; only the minimized story changes. The
    // conditional-spread must not rewrite (or re-reference) the live slice.
    useViewerStore.setState({
      workspaces: { [repo]: [storyWorkspace("LIVE-1", `${contextRepo}/LIVE-1.md`, "done")] },
      softClosedWorkspaces: {
        [repo]: {
          "MIN-1": softClosed(storyWorkspace("MIN-1", `${contextRepo}/MIN-1.md`, "todo")),
        },
      },
    });
    const liveBefore = useViewerStore.getState().workspaces[repo];

    const tree = makeTree([
      { filePath: `${contextRepo}/LIVE-1.md`, status: "done", storyId: "LIVE-1" },
      { filePath: `${contextRepo}/MIN-1.md`, status: "review", storyId: "MIN-1" },
    ]);
    useViewerStore.getState().syncWorkspaceFrontmatter(repo, tree);

    // Shadow reconciled…
    expect(
      useViewerStore.getState().softClosedWorkspaces[repo]!["MIN-1"].workspace.storyStatus,
    ).toBe("review");
    // …live array reference untouched (no needless churn / re-render).
    expect(useViewerStore.getState().workspaces[repo]).toBe(liveBefore);
  });
});
