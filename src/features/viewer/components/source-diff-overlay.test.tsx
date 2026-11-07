// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, onTestFinished } from "vitest";
import { render, screen, fireEvent, waitFor, within, act } from "@/test-utils";

import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";
import { warmScopes } from "@/shared/lib/warm-scopes";

const navigatorSpy = vi.hoisted(() => ({
  next: vi.fn(),
  prev: vi.fn(),
  currentLine: vi.fn((): number | null => null),
}));
/** The latest `onChunkPosition` the mocked diff was given, to report through. */
const chunkReporter = vi.hoisted(() => ({
  current: null as ((p: { index: number | null; count: number }) => void) | null,
}));
import { resetDetectedEditorsCache } from "@/shared/hooks/use-detected-editors";
import type { PublishFile } from "@/features/git-sync/lib/types";

// The redline tail is exercised elsewhere; stub it to a marker so we assert it
// mounts for MARKDOWN (WD-6 keeps prose for .md) and NOT for source code.
vi.mock("@/features/git-sync/components/redline-prose", () => ({
  RedlineProse: ({
    before,
    after,
    error,
  }: {
    before: string | null;
    after: string | null;
    error?: string | null;
  }) =>
    error ? (
      <div data-testid="mock-redline-error">{error}</div>
    ) : (
      <div data-testid="mock-redline-prose">
        {before}→{after}
      </div>
    ),
}));

// SLW-WD-6: stub the code diff to a marker (the CM6 merge view is exercised in
// its own suite). Keep `detectLanguageId` REAL so the markdown/source branch is
// genuinely path-driven, not mocked away.
vi.mock("@/shared/components/code-editor", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/shared/components/code-editor")>()),
  CodeDiffReview: ({
    before,
    after,
    path,
    mode,
    wordWrap,
    collapseUnchanged,
    diffMode,
    autoFollow,
    navRef,
    onChunkPosition,
  }: {
    navRef?: { current: unknown } | ((v: unknown) => void) | null;
    onChunkPosition?: (p: { index: number | null; count: number }) => void;
    before: string;
    after: string;
    path?: string;
    mode?: string;
    wordWrap?: boolean;
    collapseUnchanged?: boolean;
    diffMode?: string;
    autoFollow?: boolean;
  }) => {
    if (navRef && typeof navRef === "object") navRef.current = navigatorSpy;
    chunkReporter.current = onChunkPosition ?? null;
    return (
      <div
        data-testid="mock-code-diff-review"
        data-mode={mode}
        data-path={path}
        data-wrap={wordWrap ? "wrap" : "nowrap"}
        data-collapse={collapseUnchanged === false ? "expand" : "collapse"}
        data-diff-mode={diffMode ?? "line"}
        data-auto-follow={String(autoFollow ?? false)}
      >
        {before}→{after}
      </div>
    );
  },
}));

// Mock the data layer so the overlay test is deterministic + seam-agnostic.
const changesetMock = vi.fn();
const redlineMock = vi.fn();
const baseRefMock = vi.fn();
const commitChangesetMock = vi.fn(
  (..._a: unknown[]): { files: PublishFile[] | null; loading: boolean; error: string | null } => ({
    files: null,
    loading: false,
    error: null,
  }),
);
const commitRedlineMock = vi.fn((..._a: unknown[]) => ({ redline: null, error: null }));
vi.mock("../lib/use-source-diff", () => ({
  // SLW-DV-7: the overlay reads the resolved base + the config default start via
  // useSourceDiffBase; `baseRefMock` still drives the resolved baseRef.
  useSourceDiffBase: (...a: unknown[]) => ({
    baseRef: baseRefMock(...a),
    defaultStart: "main",
  }),
  useSourceChangeset: (...a: unknown[]) => changesetMock(...a),
  useSourceFileRedline: (...a: unknown[]) => redlineMock(...a),
  // SLW-WD-9: History-mode hooks — no-op in the WD-2/5 (Working-mode) suites.
  useSourceCommitChangeset: (...a: unknown[]) => commitChangesetMock(...a),
  useSourceCommitRedline: (...a: unknown[]) => commitRedlineMock(...a),
}));

// SLW-WD-9: the ref-counted worktree watcher (a no-op here — its own suite covers
// it) and the commit-graph rail (a marker — the component is tested separately).
vi.mock("../lib/use-worktree-watch", () => ({ useWorktreeWatch: vi.fn() }));

// SW-VE-12: the per-tab resolver that answers "is this cwd a git working tree?"
// — threaded into the overlay to drive the "No git repo at this path." body
// state. Default is a resolved local worktree so existing suites unchanged.
type TabRepoResolution = {
  folder: { kind: "folder"; cwd: string } | null;
  repo: { root: string; isRoot: boolean } | null | undefined;
};
const tabSourceRepoMock = vi.hoisted(() =>
  vi.fn<(ws: unknown, liveCwd?: string | null) => TabRepoResolution>(() => ({
    folder: { kind: "folder", cwd: "/wt/story" },
    repo: { root: "/wt/story", isRoot: true },
  })),
);
vi.mock("@/features/viewer/hooks/use-tab-source-repo", () => ({
  useTabSourceRepo: (ws: unknown, liveCwd?: string | null) => tabSourceRepoMock(ws, liveCwd),
}));

// SW-VE-12 (B4) → SW-VE-14: the overlay reads `useSessionCwd` to retarget the
// resolver + fetch keys on a terminal `cd`, and `useSessionCwdRevision` bumps
// per-session on OSC 7. Default to null + 0 so existing suites that don't
// exercise the override behave exactly as before.
const sessionCwdMock = vi.hoisted(() =>
  vi.fn<(sid: string | undefined, rev: number) => string | null>(() => null),
);
vi.mock("@/features/viewer/hooks/use-session-cwd", () => ({
  useSessionCwd: (sid: string | undefined, rev: number) => sessionCwdMock(sid, rev),
}));
const sessionCwdRevisionMock = vi.hoisted(() => vi.fn<() => number>(() => 0));
vi.mock("@/features/viewer/hooks/use-session-cwd-revision", () => ({
  useSessionCwdRevision: () => sessionCwdRevisionMock(),
}));
vi.mock("./commit-graph-rail", () => ({
  CommitGraphRail: () => <div data-testid="mock-commit-graph-rail" />,
}));

// SLW-WD-4: mock the route/un-route command bridge; keep every other real
// command export intact (the store + siblings import the module).
const routeMock = vi.fn();
const unrouteMock = vi.fn();
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  routeFileToContext: (...a: unknown[]) => routeMock(...a),
  unrouteFileFromContext: (...a: unknown[]) => unrouteMock(...a),
  // SLW-WDE-5: the external-editor button lazily detects editor labels — keep it
  // deterministic (empty → generic "Open in editor" label) and off real IPC.
  detectTools: () =>
    Promise.resolve({ terminals: [], editors: [], default_terminal: "", default_editor: "" }),
}));

import { SourceDiffOverlay } from "./source-diff-overlay";
import { setSourceDiffReturnFocus } from "./source-diff-return-focus";
import { useMachinesStore } from "@/features/machines/stores/machines-store";

/** A rail row by repo path (rows render the directory and name as separate
 *  spans, so a whole-path text match no longer applies). */
function rowIn(scope: HTMLElement, path: string): HTMLElement {
  const row = scope.querySelector<HTMLElement>(`li[data-path="${path}"] [data-source-diff-row]`);
  if (!row) throw new Error(`no rail row for ${path}`);
  return row;
}

const storyWs: Workspace = {
  id: "w1",
  kind: "story",
  cwd: "/wt/story",
  status: "ready",
  storyId: "SLW-WD-2",
  storyFilePath: "/repo/story.md",
  sourceRepoPath: "/repo",
};

const FILES: PublishFile[] = [
  { path: "src/mod.ts", status: "modified" },
  { path: "src/new.ts", status: "added" },
  { path: "src/moved.ts", status: "modified", badges: ["rename"], renamedFrom: "src/old.ts" },
  // A markdown file (distinct status so the single-match chip assertions above
  // stay unambiguous) — WD-6 keeps prose rendering for .md.
  { path: "docs/readme.md", status: "deleted" },
];

type ChangesetShape = {
  files: PublishFile[] | null;
  loading: boolean;
  error: string | null;
  reload: () => void;
};
function stubChangeset(over: Partial<ChangesetShape> = {}) {
  changesetMock.mockReturnValue({ ...defaultChangeset(), ...over });
}
function defaultChangeset(): ChangesetShape {
  return { files: FILES, loading: false, error: null, reload: vi.fn() };
}
function stubRedline(over: Partial<{ redline: unknown; error: string | null }> = {}) {
  redlineMock.mockReturnValue({
    redline: { status: "modified", binary: false, before: "old", after: "new" },
    error: null,
    ...over,
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  warmScopes.activate("team-a");
  resetDetectedEditorsCache();
  baseRefMock.mockReturnValue(undefined);
  stubChangeset();
  stubRedline();
  // SW-VE-12: reset the per-tab resolver mocks to their defaults so a
  // test's override never leaks into the next (clearAllMocks preserves
  // mockReturnValue implementations, so we re-set them explicitly).
  tabSourceRepoMock.mockReturnValue({
    folder: { kind: "folder", cwd: "/wt/story" },
    repo: { root: "/wt/story", isRoot: true },
  });
  sessionCwdMock.mockReturnValue(null);
  sessionCwdRevisionMock.mockReturnValue(0);
  useViewerStore.setState({
    sourceDiffShownBySession: {},
    sourceDiffSelectedFile: {},
    sourceDiffViewOptions: {},
    worktreeChangedPath: {},
    worktreeEditorOpenBySession: {},
    sourceDiffMode: {},
    // SLW-WD-9: reset the History-mode + selected-sha so a prior test's adaptive
    // pick / explicit toggle doesn't leak into the next (they persist otherwise).
    sourceDiffHistoryMode: {},
    sourceDiffSelectedSha: {},
    // SLW-DV-7: reset the comparison basis so a prior test's toggle doesn't leak.
    sourceDiffBaseMode: {},
    // SLW-WD-7: pin the default-layout setting so per-test mutations don't leak.
    settings: { ...useViewerStore.getState().settings, defaultSourceDiffLayout: "inline" },
  });
});

describe("SourceDiffOverlay — render gates (AC-1/AC-5/RD-6)", () => {
  it("renders nothing on a non-focused tile (AC-1 focused-only divergence)", () => {
    useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } });
    const { container } = render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused={false}
      />,
    );
    expect(container).toBeEmptyDOMElement();
    expect(changesetMock).toHaveBeenCalledWith("/wt/story", undefined, false, 0);
  });

  it("renders nothing when hidden (the chrome toggle opens it)", () => {
    const { container } = render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  // SW-VE-12 (AC-6): the overlay mounts on every folder-rooted kind, not just
  // `story`. Non-git kinds surface the "No git repo" body state (see the new
  // suite below) instead of hiding the whole overlay.
  it("renders the overlay chrome for a non-story kind whose cwd resolves to a repo", () => {
    const genericWs: Workspace = { ...storyWs, id: "gw", kind: "generic", cwd: "/wt/plain" };
    tabSourceRepoMock.mockReturnValue({
      folder: { kind: "folder", cwd: "/wt/plain" },
      repo: { root: "/wt/plain", isRoot: true },
    });
    useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } });
    render(
      <SourceDiffOverlay
        workspace={genericWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("source-diff-overlay-expanded-gw")).toBeInTheDocument();
    // The seam IS called now — a `generic` tab in a worktree fetches like a story.
    expect(changesetMock).toHaveBeenCalledWith("/wt/plain", undefined, true, 0);
  });

  it("hides while the worktree is still materializing (status !== ready)", () => {
    const creating: Workspace = { ...storyWs, status: "creating" };
    useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } });
    const { container } = render(
      <SourceDiffOverlay
        workspace={creating}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("opens the FULL overlay on reveal — no compact peek (AC-1b / SLW-DV-2 AC-2)", () => {
    useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // SLW-WD-9 (AC-1b): reveal auto-expands to the master-detail surface; with
    // changes present the adaptive lead is Working (AC-1c), so the file rail shows.
    expect(screen.getByTestId("source-diff-overlay-expanded-w1")).toBeInTheDocument();
    // SLW-DV-2 AC-2: the retired compact card never renders — no entry point lands on it.
    expect(screen.queryByTestId("source-diff-overlay-w1")).not.toBeInTheDocument();
    // AC-2: lazy fetch enabled once shown.
    expect(changesetMock).toHaveBeenCalledWith("/wt/story", undefined, true, 0);
  });
});

describe("SourceDiffOverlay — changed-file list + states (AC-2)", () => {
  beforeEach(() => useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } }));

  it("renders paths, StatusChip/RenameChip and badge labels (reuse, no bespoke map)", () => {
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // Scope to the rail — the auto-selected file's path also appears in the pane header.
    const railList = screen.getByTestId("source-diff-rail-w1");
    expect(rowIn(railList, "src/mod.ts")).toBeInTheDocument();
    expect(rowIn(railList, "src/new.ts")).toBeInTheDocument();
    expect(rowIn(railList, "src/moved.ts")).toBeInTheDocument();
    // A/M chips (aria-label = status); the R chip carries the rename (no
    // duplicate "rename" pill), and the row names the move in its tooltip.
    expect(screen.getByLabelText("modified")).toBeInTheDocument();
    expect(screen.getByLabelText("added")).toBeInTheDocument();
    expect(screen.getByLabelText("renamed")).toBeInTheDocument();
    expect(screen.queryByText("rename")).not.toBeInTheDocument();
    expect(rowIn(railList, "src/moved.ts")).toHaveAttribute("title", "src/old.ts → src/moved.ts");
  });

  it("shows a quiet empty state (not an error) in Working mode", () => {
    stubChangeset({ files: [] });
    // In Working mode an empty change-set is a quiet empty state (NOT an error).
    // (On a clean tree the overlay adaptively leads with History — AC-1c — so pin
    // Working mode here to assert the empty-list copy specifically.)
    useViewerStore.setState({ sourceDiffHistoryMode: { "sid-ws": false } });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByText("No uncommitted changes.")).toBeInTheDocument();
  });

  it("shows an inline error row with Retry (not a toast) that calls reload", () => {
    const reload = vi.fn();
    stubChangeset({ files: null, error: "corrupt repo", reload });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    const retry = screen.getByRole("button", { name: "Retry" });
    fireEvent.click(retry);
    expect(reload).toHaveBeenCalled();
  });

  it("shows a skeleton on first fetch (loading + no files)", () => {
    stubChangeset({ files: null, loading: true });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.queryByText("No uncommitted changes.")).not.toBeInTheDocument();
    // SLW-DV-2: the sole surface is the expanded reader; the rail owns the skeleton.
    expect(screen.getByTestId("source-diff-overlay-expanded-w1")).toBeInTheDocument();
  });
});

describe("SourceDiffOverlay — line stats (SLW-DV-6 AC-2)", () => {
  const STAT_FILES: PublishFile[] = [
    { path: "src/a.ts", status: "modified", added: 3, removed: 1 },
    { path: "src/b.ts", status: "added", added: 10, removed: 0 },
    // A binary file carries no counts (undefined, not 0) — the row omits the stat.
    { path: "img.png", status: "added" },
  ];
  beforeEach(() => useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } }));

  it("shows the changed-file count in the rail header", () => {
    stubChangeset({ files: STAT_FILES });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByLabelText("3 changed files")).toHaveTextContent("3");
  });

  it("shows per-file +added / −removed on each row, omitting binary rows", () => {
    stubChangeset({ files: STAT_FILES });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    const rail = screen.getByTestId("source-diff-rail-w1");
    expect(within(rail).getByLabelText("3 added, 1 removed")).toBeInTheDocument();
    expect(within(rail).getByLabelText("10 added, 0 removed")).toBeInTheDocument();
    // A binary file (no counts) renders no stat — nothing reads as "0 added, 0 removed".
    expect(within(rail).queryByLabelText("0 added, 0 removed")).not.toBeInTheDocument();
  });

  it("sums the total +/− into the header stats slot (unknown counts contribute 0)", () => {
    stubChangeset({ files: STAT_FILES });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // 3+10 added, 1+0 removed; img.png contributes nothing.
    expect(screen.getByLabelText("13 added, 1 removed")).toBeInTheDocument();
  });

  it("omits the header total when no file carries a count (all-binary change-set)", () => {
    stubChangeset({
      files: [
        { path: "a.png", status: "added" },
        { path: "b.png", status: "modified" },
      ],
    });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // No "+0 −0" total anywhere — the stats slot renders nothing.
    expect(screen.queryByLabelText("0 added, 0 removed")).not.toBeInTheDocument();
  });
});

describe("SourceDiffOverlay — per-file redline + two-size (AC-3/RD-8)", () => {
  beforeEach(() => useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } }));

  it("clicking a SOURCE file expands + renders the code diff, NOT the prose redline (AC-1)", () => {
    const { rerender } = render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.click(rowIn(screen.getByTestId("source-diff-rail-w1"), "src/mod.ts"));
    // Clicking a rail file selects it; the pane swaps in place.
    expect(useViewerStore.getState().sourceDiffSelectedFile["sid-ws"]).toBe("src/mod.ts");
    rerender(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    const diff = screen.getByTestId("mock-code-diff-review");
    expect(diff).toBeInTheDocument();
    expect(diff).toHaveAttribute("data-path", "src/mod.ts");
    // SLW-WD-7 AC-1: an unset tile now opens INLINE by default.
    expect(diff).toHaveAttribute("data-mode", "inline");
    expect(screen.queryByTestId("mock-redline-prose")).not.toBeInTheDocument();
  });

  it("word-wrap toggle flips wordWrap + persists per tile (SLW-DV-6 AC-6)", () => {
    useViewerStore.setState({ sourceDiffSelectedFile: { "sid-ws": "src/mod.ts" } });
    const { rerender } = render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // Default: no wrap.
    expect(screen.getByTestId("mock-code-diff-review")).toHaveAttribute("data-wrap", "nowrap");
    fireEvent.click(screen.getByTestId("source-diff-wrap-toggle-w1"));
    expect(useViewerStore.getState().sourceDiffViewOptions["sid-ws"]?.wordWrap).toBe(true);
    rerender(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("mock-code-diff-review")).toHaveAttribute("data-wrap", "wrap");
  });

  it("the wrap toggle is hidden for a markdown file (code-diff only)", () => {
    useViewerStore.setState({ sourceDiffSelectedFile: { "sid-ws": "docs/readme.md" } });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.queryByTestId("source-diff-wrap-toggle-w1")).not.toBeInTheDocument();
  });

  it("expand-all toggle flips collapseUnchanged + persists per tile (SLW-DV-6 AC-4)", () => {
    useViewerStore.setState({ sourceDiffSelectedFile: { "sid-ws": "src/mod.ts" } });
    const { rerender } = render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // Default: unchanged regions collapse.
    expect(screen.getByTestId("mock-code-diff-review")).toHaveAttribute(
      "data-collapse",
      "collapse",
    );
    fireEvent.click(screen.getByTestId("source-diff-expand-toggle-w1"));
    expect(useViewerStore.getState().sourceDiffViewOptions["sid-ws"]?.expandAll).toBe(true);
    rerender(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("mock-code-diff-review")).toHaveAttribute("data-collapse", "expand");
  });

  it("ignore-whitespace toggle sets diffMode + persists per tile (SLW-DV-8 AC-1 / DV-8b RD-3)", () => {
    useViewerStore.setState({ sourceDiffSelectedFile: { "sid-ws": "src/mod.ts" } });
    const { rerender } = render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // Default: the plain line diff.
    expect(screen.getByTestId("mock-code-diff-review")).toHaveAttribute("data-diff-mode", "line");
    fireEvent.click(screen.getByTestId("source-diff-ignore-ws-toggle-w1"));
    expect(useViewerStore.getState().sourceDiffMode["sid-ws"]).toBe("ignoreWhitespace");
    rerender(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("mock-code-diff-review")).toHaveAttribute(
      "data-diff-mode",
      "ignoreWhitespace",
    );
    // Re-clicking returns to the line diff (mutually-exclusive modes).
    fireEvent.click(screen.getByTestId("source-diff-ignore-ws-toggle-w1"));
    expect(useViewerStore.getState().sourceDiffMode["sid-ws"]).toBe("line");
  });

  it("structural toggle sets diffMode + persists per tile (SLW-DV-8b AC-1/RD-3)", () => {
    useViewerStore.setState({ sourceDiffSelectedFile: { "sid-ws": "src/mod.ts" } });
    const { rerender } = render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.click(screen.getByTestId("source-diff-structural-toggle-w1"));
    expect(useViewerStore.getState().sourceDiffMode["sid-ws"]).toBe("structural");
    rerender(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("mock-code-diff-review")).toHaveAttribute(
      "data-diff-mode",
      "structural",
    );
  });

  it("the structural toggle is disabled for a flat-tree language (SLW-DV-8b AC-1)", () => {
    stubChangeset({ files: [{ path: "src/main.rs", status: "modified" }] });
    useViewerStore.setState({ sourceDiffSelectedFile: { "sid-ws": "src/main.rs" } });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("source-diff-structural-toggle-w1")).toBeDisabled();
    // ...and the ignore-whitespace toggle stays enabled (it works on any text).
    expect(screen.getByTestId("source-diff-ignore-ws-toggle-w1")).not.toBeDisabled();
  });

  it("the diff-mode toggles are hidden for a markdown file (code-diff only)", () => {
    useViewerStore.setState({ sourceDiffSelectedFile: { "sid-ws": "docs/readme.md" } });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.queryByTestId("source-diff-ignore-ws-toggle-w1")).not.toBeInTheDocument();
    expect(screen.queryByTestId("source-diff-structural-toggle-w1")).not.toBeInTheDocument();
  });

  it("gates a very large file behind 'show diff anyway' (SLW-DV-6 AC-8)", () => {
    // ~1.6M combined chars — over the LARGE_DIFF_CHARS budget.
    const big = "x\n".repeat(400_000);
    stubRedline({ redline: { status: "modified", binary: false, before: big, after: `${big}y` } });
    useViewerStore.setState({ sourceDiffSelectedFile: { "sid-ws": "src/mod.ts" } });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // The diff is NOT rendered up front — the gate is shown instead.
    expect(screen.getByTestId("source-diff-large-gate")).toBeInTheDocument();
    expect(screen.queryByTestId("mock-code-diff-review")).not.toBeInTheDocument();
    // Opting in renders the diff.
    fireEvent.click(screen.getByTestId("source-diff-show-anyway"));
    expect(screen.getByTestId("mock-code-diff-review")).toBeInTheDocument();
  });

  it("renders a normal-sized file directly, no gate (SLW-DV-6 AC-8)", () => {
    useViewerStore.setState({ sourceDiffSelectedFile: { "sid-ws": "src/mod.ts" } });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.queryByTestId("source-diff-large-gate")).not.toBeInTheDocument();
    expect(screen.getByTestId("mock-code-diff-review")).toBeInTheDocument();
  });

  it("gates a very large MARKDOWN file too — the prose redline is synchronous (DW-DV6-1)", () => {
    const big = "x\n".repeat(400_000); // ~1.6M combined chars
    stubRedline({ redline: { status: "modified", binary: false, before: big, after: `${big}y` } });
    useViewerStore.setState({ sourceDiffSelectedFile: { "sid-ws": "docs/readme.md" } });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // The gate wins over the markdown/prose renderer.
    expect(screen.getByTestId("source-diff-large-gate")).toBeInTheDocument();
    expect(screen.queryByTestId("mock-redline-prose")).not.toBeInTheDocument();
    fireEvent.click(screen.getByTestId("source-diff-show-anyway"));
    expect(screen.getByTestId("mock-redline-prose")).toBeInTheDocument();
  });

  it("a MARKDOWN file keeps the prose redline, NOT the code diff (AC-4)", () => {
    useViewerStore.setState({
      sourceDiffSelectedFile: { "sid-ws": "docs/readme.md" },
    });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("mock-redline-prose")).toBeInTheDocument();
    expect(screen.queryByTestId("mock-code-diff-review")).not.toBeInTheDocument();
  });

  it("an MDX file is a code diff, not the prose redline (its imports and JSX are code)", () => {
    stubChangeset({ files: [...FILES, { path: "docs/page.mdx", status: "modified" }] });
    useViewerStore.setState({ sourceDiffSelectedFile: { "sid-ws": "docs/page.mdx" } });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("mock-code-diff-review")).toBeInTheDocument();
    expect(screen.queryByTestId("mock-redline-prose")).not.toBeInTheDocument();
  });

  it("inline ↔ split toggle flips the mode + persists; hidden for markdown (AC-6/RD-3)", () => {
    useViewerStore.setState({
      sourceDiffSelectedFile: { "sid-ws": "src/mod.ts" },
    });
    const { rerender } = render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // SLW-WD-7: inline by default; the toggle is present for a source file.
    expect(screen.getByTestId("mock-code-diff-review")).toHaveAttribute("data-mode", "inline");
    // AC-6: the FIRST toggle from an unset tile flips relative to the effective
    // default (inline → split), NOT to a hardcoded `true`.
    fireEvent.click(screen.getByTestId("source-diff-view-toggle-w1"));
    expect(useViewerStore.getState().sourceDiffViewOptions["sid-ws"]?.inline).toBe(false);
    rerender(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // The pref drives the mode + carries to the next file (persists in-store).
    expect(screen.getByTestId("mock-code-diff-review")).toHaveAttribute("data-mode", "split");

    // A markdown file shows the prose redline regardless — no dead toggle.
    useViewerStore.setState({ sourceDiffSelectedFile: { "sid-ws": "docs/readme.md" } });
    rerender(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.queryByTestId("source-diff-view-toggle-w1")).not.toBeInTheDocument();
  });

  it("the Default diff layout setting seeds an unset tile; a per-tile toggle overrides it (AC-2/RD-1)", () => {
    useViewerStore.setState({
      sourceDiffSelectedFile: { "sid-ws": "src/mod.ts" },
    });
    // Setting = side-by-side → a fresh (unset) tile opens split.
    useViewerStore.setState({
      settings: { ...useViewerStore.getState().settings, defaultSourceDiffLayout: "split" },
    });
    const { rerender } = render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("mock-code-diff-review")).toHaveAttribute("data-mode", "split");

    // An explicit per-tile choice wins over the setting (inline here, despite split default).
    useViewerStore.setState({ sourceDiffViewOptions: { "sid-ws": { inline: true } } });
    rerender(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("mock-code-diff-review")).toHaveAttribute("data-mode", "inline");

    // Flip the setting back to inline → an unset sibling tile opens inline.
    useViewerStore.setState({
      settings: { ...useViewerStore.getState().settings, defaultSourceDiffLayout: "inline" },
      sourceDiffViewOptions: {},
    });
    rerender(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("mock-code-diff-review")).toHaveAttribute("data-mode", "inline");
  });

  it("keeps the toggle hidden while the redline is still loading (no dead switch)", () => {
    // redline null = still loading → binary-ness unknown; the toggle must not
    // flash as a dead switch (it could resolve to a binary file).
    stubRedline({ redline: null, error: null });
    useViewerStore.setState({
      sourceDiffSelectedFile: { "sid-ws": "src/mod.ts" },
    });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.queryByTestId("source-diff-view-toggle-w1")).not.toBeInTheDocument();
    // the pane shows the loading skeleton, not a crash
    expect(screen.getByTestId("source-diff-code-skeleton")).toBeInTheDocument();
  });

  it("passes the rename oldPath into the redline fetch", () => {
    useViewerStore.setState({
      sourceDiffSelectedFile: { "sid-ws": "src/moved.ts" },
    });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(redlineMock).toHaveBeenCalledWith(
      "/wt/story",
      "src/moved.ts",
      "src/old.ts",
      undefined,
      0,
    );
  });

  it("a binary file renders the placeholder — neither prose nor code diff (AC-5)", () => {
    stubRedline({ redline: { status: "modified", binary: true, before: "", after: "" } });
    useViewerStore.setState({
      sourceDiffSelectedFile: { "sid-ws": "src/mod.ts" },
    });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("source-diff-binary-placeholder")).toBeInTheDocument();
    expect(screen.queryByTestId("mock-redline-prose")).not.toBeInTheDocument();
    expect(screen.queryByTestId("mock-code-diff-review")).not.toBeInTheDocument();
    // A binary file also has no dead view-mode toggle.
    expect(screen.queryByTestId("source-diff-view-toggle-w1")).not.toBeInTheDocument();
  });

  it("SLW-DV-2: the Hide control hides the whole overlay (no compact size to fall to)", () => {
    useViewerStore.setState({ sourceDiffSelectedFile: { "sid-ws": "src/mod.ts" } });
    const { container, rerender } = render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.click(screen.getByTestId("source-diff-collapse-w1"));
    expect(useViewerStore.getState().sourceDiffShownBySession["sid-ws"]).toBe(false);
    rerender(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // Hidden entirely — not a compact peek.
    expect(container).toBeEmptyDOMElement();
    expect(screen.queryByTestId("source-diff-overlay-w1")).not.toBeInTheDocument();
  });
});

describe("SourceDiffOverlay — live follow + activity (SLW-DV-5)", () => {
  beforeEach(() =>
    useViewerStore.setState({
      sourceDiffShownBySession: { "sid-ws": true },
      sourceDiffSelectedFile: { "sid-ws": "src/mod.ts" },
    }),
  );

  it("threads autoFollow (default on) into the code diff and the toggle flips it", () => {
    const { rerender } = render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // Default on (absent store value → true) reaches CodeDiffReview.
    expect(screen.getByTestId("mock-code-diff-review")).toHaveAttribute("data-auto-follow", "true");

    // The header follow toggle is present for a selected code file and flips the store.
    fireEvent.click(screen.getByTestId("source-diff-follow-toggle-w1"));
    expect(useViewerStore.getState().sourceDiffViewOptions["sid-ws"]?.autoFollow).toBe(false);

    rerender(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("mock-code-diff-review")).toHaveAttribute(
      "data-auto-follow",
      "false",
    );
  });

  it("shows the 'editing…' activity indicator off the RAW worktree nonce, and it clears when idle", () => {
    vi.useFakeTimers();
    try {
      const { rerender } = render(
        <SourceDiffOverlay
          workspace={storyWs}
          teamKey="team-a"
          activeTabSessionId="sid-ws"
          isFocused
        />,
      );
      expect(screen.queryByTestId("source-diff-activity-w1")).not.toBeInTheDocument();

      // A raw worktree-change bump lights the indicator immediately.
      act(() => {
        useViewerStore.setState({ worktreeChangedPath: { w1: { path: "src/mod.ts", nonce: 1 } } });
      });
      rerender(
        <SourceDiffOverlay
          workspace={storyWs}
          teamKey="team-a"
          activeTabSessionId="sid-ws"
          isFocused
        />,
      );
      expect(screen.getByTestId("source-diff-activity-w1")).toBeInTheDocument();

      // It decays after the idle window with no further bumps.
      act(() => {
        vi.advanceTimersByTime(1300);
      });
      rerender(
        <SourceDiffOverlay
          workspace={storyWs}
          teamKey="team-a"
          activeTabSessionId="sid-ws"
          isFocused
        />,
      );
      expect(screen.queryByTestId("source-diff-activity-w1")).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("SourceDiffOverlay — a hidden team shown again", () => {
  it("reloads the change list at once on a resume bump, not through the live-edit debounce", () => {
    const reload = vi.fn();
    stubChangeset({ reload });
    useViewerStore.setState({
      sourceDiffShownBySession: { "sid-ws": true },
      worktreeChangedPath: {},
    });
    const ui = (
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />
    );
    const { rerender } = render(ui);
    expect(reload).not.toHaveBeenCalled();
    // An ordinary file change goes through the debounce; only a resume reloads directly.
    act(() => useViewerStore.getState().bumpWorktreeChangedPath("w1", "a.ts"));
    rerender(ui);
    expect(reload).not.toHaveBeenCalled();
    act(() => useViewerStore.getState().bumpWorktreeChangedPath("w1", "", true));
    rerender(ui);
    expect(reload).toHaveBeenCalledTimes(1);
  });
});

describe("SourceDiffOverlay — a workspace switch is not an edit", () => {
  beforeEach(() =>
    useViewerStore.setState({
      sourceDiffShownBySession: { "sid-ws": true },
      worktreeChangedPath: {
        w1: { path: "a.ts", nonce: 5 },
        w2: { path: "b.ts", nonce: 2 },
      },
    }),
  );
  it("switching to a workspace with its own watcher history shows no 'editing…'", () => {
    const el = (id: string) => (
      <SourceDiffOverlay
        workspace={{ ...storyWs, id }}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />
    );
    const { rerender } = render(el("w1"));
    rerender(el("w2"));
    expect(screen.queryByTestId("source-diff-activity-w2")).not.toBeInTheDocument();
    act(() => {
      useViewerStore.setState({
        worktreeChangedPath: { w2: { path: "b.ts", nonce: 3 } },
      });
    });
    rerender(el("w2"));
    expect(screen.getByTestId("source-diff-activity-w2")).toBeInTheDocument();
  });
});

describe("SourceDiffOverlay — baseRef threading (AC-10)", () => {
  beforeEach(() => useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } }));

  it("threads the SAME non-main baseRef into BOTH changeset and redline", () => {
    baseRefMock.mockReturnValue("feat/SLW-WD-1");
    useViewerStore.setState({
      sourceDiffSelectedFile: { "sid-ws": "src/mod.ts" },
    });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(changesetMock).toHaveBeenCalledWith("/wt/story", "feat/SLW-WD-1", true, 0);
    expect(redlineMock).toHaveBeenCalledWith(
      "/wt/story",
      "src/mod.ts",
      undefined,
      "feat/SLW-WD-1",
      0,
    );
  });

  it("threads undefined baseRef for a fork-from-main story", () => {
    baseRefMock.mockReturnValue(undefined);
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(changesetMock).toHaveBeenCalledWith("/wt/story", undefined, true, 0);
  });
});

describe("SourceDiffOverlay — comparison label + base selector (SLW-DV-7 AC-1/AC-2)", () => {
  beforeEach(() => useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } }));

  it("states the fork-default comparison in the header (AC-1)", () => {
    baseRefMock.mockReturnValue(undefined);
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("source-diff-base-toggle-w1")).toHaveTextContent(
      "vs main (merge-base)",
    );
  });

  it("names the resolved dependency branch for a stacked story (AC-1)", () => {
    baseRefMock.mockReturnValue("feat/SLW-DV-1");
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("source-diff-base-toggle-w1")).toHaveTextContent(
      "vs feat/SLW-DV-1 (merge-base)",
    );
  });

  it("toggles to vs-HEAD and re-diffs against HEAD (AC-2)", () => {
    baseRefMock.mockReturnValue(undefined);
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // Default basis threads undefined (branch-base merge-base).
    expect(changesetMock).toHaveBeenCalledWith("/wt/story", undefined, true, 0);

    fireEvent.click(screen.getByTestId("source-diff-base-head-w1"));

    // Store flips to head; the label + the re-keyed change-set both follow.
    expect(useViewerStore.getState().sourceDiffBaseMode["sid-ws"]).toBe("head");
    expect(screen.getByTestId("source-diff-base-toggle-w1")).toHaveTextContent(
      "vs HEAD (working changes only)",
    );
    expect(changesetMock).toHaveBeenCalledWith("/wt/story", "HEAD", true, 0);
  });

  it("threads the vs-HEAD basis into the redline too, so list + pane never disagree (AC-2)", () => {
    baseRefMock.mockReturnValue(undefined);
    useViewerStore.setState({
      sourceDiffBaseMode: { "sid-ws": "head" },
      sourceDiffSelectedFile: { "sid-ws": "src/mod.ts" },
    });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(redlineMock).toHaveBeenCalledWith("/wt/story", "src/mod.ts", undefined, "HEAD", 0);
  });

  it("does NOT render the base control in History mode (RD-6)", () => {
    useViewerStore.setState({ sourceDiffHistoryMode: { "sid-ws": true } });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("source-diff-history-w1")).toBeInTheDocument();
    expect(screen.queryByTestId("source-diff-base-toggle-w1")).not.toBeInTheDocument();
  });

  it("does NOT leak a head basis into History mode's working node (RD-6)", () => {
    baseRefMock.mockReturnValue(undefined);
    // A head choice made in Working mode must not follow into History — the
    // pinned Working-tree node there keeps WD-9's branch-base baseline.
    useViewerStore.setState({
      sourceDiffHistoryMode: { "sid-ws": true },
      sourceDiffBaseMode: { "sid-ws": "head" },
    });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(changesetMock).toHaveBeenCalledWith("/wt/story", undefined, true, 0);
    expect(changesetMock).not.toHaveBeenCalledWith("/wt/story", "HEAD", true, 0);
  });
});

describe("SourceDiffOverlay — scope summary + History entry coherence (SLW-DV-7 AC-3/AC-4)", () => {
  beforeEach(() => useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } }));

  it("shows the file count, total +/−, and the comparison label together (AC-3)", () => {
    baseRefMock.mockReturnValue(undefined);
    stubChangeset({
      files: [
        { path: "src/a.ts", status: "modified", added: 3, removed: 1 },
        { path: "src/b.ts", status: "added", added: 5, removed: 0 },
      ],
    });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // Rail count (AC-3, DV-6).
    expect(within(screen.getByTestId("source-diff-rail-w1")).getByText("2")).toBeInTheDocument();
    // Header total +/− (AC-3, DV-6) beside the comparison label (AC-1).
    expect(screen.getByLabelText("8 added, 1 removed")).toBeInTheDocument();
    expect(screen.getByTestId("source-diff-base-toggle-w1")).toHaveTextContent(
      "vs main (merge-base)",
    );
  });

  it("keeps a clear, accessible History entry from the Working header (AC-4)", () => {
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    const toggle = screen.getByTestId("source-diff-mode-toggle-w1");
    // Working mode → the toggle offers History with an accessible label.
    expect(toggle).toHaveAccessibleName("Show branch history");
    fireEvent.click(toggle);
    expect(useViewerStore.getState().sourceDiffHistoryMode["sid-ws"]).toBe(true);
  });
});

describe("SourceDiffOverlay — Escape + zoom-defer (AC-4)", () => {
  it("Escape on the focused tile hides the overlay", () => {
    useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.keyDown(document, { key: "Escape" });
    expect(useViewerStore.getState().sourceDiffShownBySession["sid-ws"]).toBe(false);
  });

  it("while zoomed, Escape does NOT close (defers to zoom-restore)", () => {
    useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
        zoomActive
      />,
    );
    fireEvent.keyDown(document, { key: "Escape" });
    expect(useViewerStore.getState().sourceDiffShownBySession["sid-ws"]).toBe(true);
  });

  it("SW-FX-140: Escape from within the terminal (.xterm) closes the open diff panel", () => {
    useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } });
    render(
      <div>
        <div className="xterm">
          <textarea data-testid="term-input" />
        </div>
        <SourceDiffOverlay
          workspace={storyWs}
          teamKey="team-a"
          activeTabSessionId="sid-ws"
          isFocused
        />
      </div>,
    );
    fireEvent.keyDown(screen.getByTestId("term-input"), { key: "Escape" });
    expect(useViewerStore.getState().sourceDiffShownBySession["sid-ws"]).toBe(false);
  });
});

describe("SourceDiffOverlay — a warm team's overlay owns no keys", () => {
  it("Escape from another team's terminal is not taken by a hidden team's open diff", () => {
    useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } });
    warmScopes.activate("team-b");
    render(
      <div>
        <div className="xterm">
          <textarea data-testid="term-input" />
        </div>
        <SourceDiffOverlay
          workspace={storyWs}
          teamKey="team-a"
          activeTabSessionId="sid-ws"
          isFocused
        />
      </div>,
    );
    const escape = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
    screen.getByTestId("term-input").dispatchEvent(escape);
    expect(escape.defaultPrevented).toBe(false);
    expect(useViewerStore.getState().sourceDiffShownBySession["sid-ws"]).toBe(true);
    act(() => warmScopes.activate("team-a"));
    fireEvent.keyDown(screen.getByTestId("term-input"), { key: "Escape" });
    expect(useViewerStore.getState().sourceDiffShownBySession["sid-ws"]).toBe(false);
  });
});

describe("SourceDiffOverlay — review-only (AC-7)", () => {
  beforeEach(() => useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } }));

  it("renders NO send-to-context / commit / sync / publish control", () => {
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    for (const label of [/send to context/i, /commit/i, /publish/i, /sync/i, /push/i]) {
      expect(screen.queryByRole("button", { name: label })).not.toBeInTheDocument();
    }
  });
});

describe("SourceDiffOverlay — keyboard nav j/k/Enter (AC-9)", () => {
  beforeEach(() => useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } }));

  it("the list dialog is Tab-reachable (tabIndex 0, not -1)", () => {
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // The surface is a real tab stop so the user can explicitly focus it (AC-9) —
    // -1 would make the j/k/Enter handler unreachable.
    expect(screen.getByTestId("source-diff-overlay-expanded-w1")).toHaveAttribute("tabindex", "0");
  });

  it("takes focus on show — it covers the terminal, so keys must not land there unseen", () => {
    // AC-6/AC-9 originally kept focus in the terminal while the diff was a
    // compact glanceable float. The review desk now covers the whole tile, so a
    // keystroke into the terminal would be invisible: the tool takes focus on
    // open and hands it back on hide ("final polish" suite).
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("source-diff-overlay-expanded-w1")).toHaveFocus();
  });

  it("takes focus again when its warm team is shown again (a remount used to do this)", () => {
    onTestFinished(() => warmScopes.forget("team-b"));
    render(
      <>
        <button data-testid="switch-control">switch</button>
        <SourceDiffOverlay
          workspace={storyWs}
          teamKey="team-a"
          activeTabSessionId="sid-ws"
          isFocused
        />
      </>,
    );
    act(() => warmScopes.activate("team-b"));
    act(() => screen.getByTestId("switch-control").focus());
    act(() => warmScopes.activate("team-a"));
    expect(screen.getByTestId("source-diff-overlay-expanded-w1")).toHaveFocus();
  });

  it("renders a D letter chip for a deleted file (AC-2 A/M/D/R)", () => {
    stubChangeset({ files: [{ path: "src/gone.ts", status: "deleted" }] });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByLabelText("deleted")).toHaveTextContent("D");
  });

  it("j/k swap the selection in the auto-opened rail (AC-1b/AC-9)", () => {
    const { rerender } = render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    const list = screen.getByTestId("source-diff-overlay-expanded-w1");
    // AC-1b/AC-1c: reveal auto-opens Working + self-heals to the first file.
    expect(useViewerStore.getState().sourceDiffSelectedFile["sid-ws"]).toBe("src/mod.ts");
    fireEvent.keyDown(list, { key: "j" });
    expect(useViewerStore.getState().sourceDiffSelectedFile["sid-ws"]).toBe("src/new.ts");
    fireEvent.keyDown(list, { key: "k" });
    expect(useViewerStore.getState().sourceDiffSelectedFile["sid-ws"]).toBe("src/mod.ts");
    // swap-in-place still shows the code diff (WD-6).
    rerender(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("mock-code-diff-review")).toBeInTheDocument();
  });
});

describe("SourceDiffOverlay — selected file vanished on reload (ECH-4)", () => {
  beforeEach(() => useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } }));

  it("clears the pane (no stray redline fetch) when the selected file is absent from the loaded change-set", () => {
    useViewerStore.setState({
      sourceDiffSelectedFile: { "sid-ws": "src/gone.ts" }, // not in FILES
    });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // SLW-DV-2: the rail (the file list) stays up; the pane clears to a "pick a
    // file" prompt rather than rendering the vanished path.
    expect(screen.getByTestId("source-diff-overlay-expanded-w1")).toBeInTheDocument();
    expect(screen.getByText("Select a file to view its diff.")).toBeInTheDocument();
    // And the redline was NOT fetched for the vanished path (filePath === null).
    expect(redlineMock).toHaveBeenCalledWith("/wt/story", null, undefined, undefined, 0);
  });

  it("steps to the nearest file when the open one leaves the change set while the tool is up", () => {
    useViewerStore.setState({ sourceDiffSelectedFile: { "sid-ws": "src/new.ts" } });
    const { rerender } = render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // The agent commits src/new.ts: it drops out of the working change set.
    stubChangeset({ files: FILES.filter((f) => f.path !== "src/new.ts") });
    rerender(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    const next = useViewerStore.getState().sourceDiffSelectedFile["sid-ws"];
    expect(next).not.toBeNull();
    expect(next).not.toBe("src/new.ts");
    expect(FILES.some((f) => f.path === next)).toBe(true);
    expect(screen.queryByText("Select a file to view its diff.")).toBeNull();
  });

  it("steps to the next row in the order seen, keeping focus on it when the focused row went", () => {
    useViewerStore.setState({ sourceDiffSelectedFile: { "sid-ws": "src/new.ts" } });
    const el = () => (
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />
    );
    const { rerender } = render(el());
    const rail = screen.getByTestId("source-diff-rail-w1");
    const order = [...rail.querySelectorAll<HTMLElement>("li[data-path]")].map(
      (li) => li.dataset.path,
    );
    rowIn(rail, "src/new.ts").focus();
    stubChangeset({ files: FILES.filter((f) => f.path !== "src/new.ts") });
    rerender(el());
    const expected = order[order.indexOf("src/new.ts") + 1];
    expect(useViewerStore.getState().sourceDiffSelectedFile["sid-ws"]).toBe(expected);
    expect(document.activeElement).toBe(rowIn(rail, expected!));
  });

  it("never takes focus from outside the tool when the open file leaves", () => {
    useViewerStore.setState({ sourceDiffSelectedFile: { "sid-ws": "src/new.ts" } });
    const el = () => (
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />
    );
    const { rerender } = render(el());
    const terminal = document.createElement("textarea");
    document.body.appendChild(terminal);
    try {
      terminal.focus();
      stubChangeset({ files: FILES.filter((f) => f.path !== "src/new.ts") });
      rerender(el());
      expect(document.activeElement).toBe(terminal);
    } finally {
      terminal.remove();
    }
  });

  it("keeps the reader up while the change-set is still loading (no flicker)", () => {
    stubChangeset({ files: null, loading: true });
    useViewerStore.setState({
      sourceDiffSelectedFile: { "sid-ws": "src/mod.ts" },
    });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("source-diff-overlay-expanded-w1")).toBeInTheDocument();
  });

  it("on an ERRORED change-set, the rail shows the error+Retry row — the reader is not stuck", () => {
    // files === null because the fetch FAILED (not because it's loading): the rail
    // owns the inline error+Retry so it stays reachable on the sole surface.
    stubChangeset({ files: null, loading: false, error: "repo locked" });
    useViewerStore.setState({
      sourceDiffSelectedFile: { "sid-ws": "src/mod.ts" },
    });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("source-diff-overlay-expanded-w1")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Retry" })).toBeInTheDocument();
  });
});

// ────────────────────────────────────────────────────────────────────────────
// SLW-WD-4 — the per-file "→ context" route affordance.
// ────────────────────────────────────────────────────────────────────────────

describe("SourceDiffOverlay — route-to-context affordance (WD-4)", () => {
  // WD-4 needs the source-repo slug to route; the WD-2 fixture omits it.
  const routableWs: Workspace = { ...storyWs, sourceRepoSlug: "acme/repo" };
  const routeBtn = () => document.querySelector("[data-route-to-context]") as HTMLElement | null;
  const undoBtn = () => document.querySelector("[data-route-undo]") as HTMLElement | null;

  beforeEach(() => useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } }));

  it("AC-1: offers '→ context' only on the added (non-rename) row", () => {
    render(
      <SourceDiffOverlay
        workspace={routableWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // FILES has exactly one routable row: src/new.ts (added, not renamed).
    const buttons = document.querySelectorAll("[data-route-to-context]");
    expect(buttons.length).toBe(1);
    // and it sits in the new.ts row (its <li> also contains the path text).
    const li = buttons[0].closest("li");
    expect(li?.textContent).toContain("src/new.ts");
  });

  it("AC-2: routes to the team workspace in one gesture — cwd + slug + path, no layer, no picker", async () => {
    routeMock.mockResolvedValue({ outcome: "routed" });
    render(
      <SourceDiffOverlay
        workspace={routableWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.click(routeBtn()!);
    await waitFor(() =>
      expect(routeMock).toHaveBeenCalledWith("/wt/story", "acme/repo", "src/new.ts"),
    );
    // no tier/repo picker is rendered (single-target, RD-1).
    expect(screen.queryByRole("combobox")).not.toBeInTheDocument();
    expect(screen.queryByText(/this repo only/i)).not.toBeInTheDocument();
  });

  it("AC-3/RD-6: on 'routed' the row shows a routed state + Undo, and does NOT reload the change-set", async () => {
    const reload = vi.fn();
    stubChangeset({ reload });
    routeMock.mockResolvedValue({ outcome: "routed" });
    render(
      <SourceDiffOverlay
        workspace={routableWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.click(routeBtn()!);
    expect(await screen.findByText("routed")).toBeInTheDocument();
    expect(undoBtn()).toBeInTheDocument();
    expect(reload).not.toHaveBeenCalled(); // organic drop, not a forced refetch
  });

  it("AC-4: Undo un-routes and the row returns to a routable '→ context' state", async () => {
    routeMock.mockResolvedValue({ outcome: "routed" });
    unrouteMock.mockResolvedValue(undefined);
    render(
      <SourceDiffOverlay
        workspace={routableWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.click(routeBtn()!);
    fireEvent.click(await screen.findByText("Undo"));
    await waitFor(() =>
      expect(unrouteMock).toHaveBeenCalledWith("/wt/story", "acme/repo", "src/new.ts"),
    );
    await waitFor(() => expect(routeBtn()).toBeInTheDocument());
  });

  it("AC-5: a collision renders a non-destructive inline notice and leaves the file in the diff", async () => {
    routeMock.mockResolvedValue({ outcome: "collision", sourceHash: "aa", destHash: "bb" });
    render(
      <SourceDiffOverlay
        workspace={routableWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.click(routeBtn()!);
    expect(await screen.findByText(/already in context/i)).toBeInTheDocument();
    // non-destructive: the file is still listed.
    expect(rowIn(document.body, "src/new.ts")).toBeInTheDocument();
  });

  it("AC-5: an already-tracked file flips to a disabled 'commit or remove first' hint", async () => {
    routeMock.mockResolvedValue({ outcome: "alreadyTracked" });
    render(
      <SourceDiffOverlay
        workspace={routableWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.click(routeBtn()!);
    expect(await screen.findByText(/already tracked/i)).toBeInTheDocument();
  });

  it("AC-5: a refusal renders a non-destructive inline 'can’t route'", async () => {
    routeMock.mockResolvedValue({ outcome: "refused" });
    render(
      <SourceDiffOverlay
        workspace={routableWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.click(routeBtn()!);
    expect(await screen.findByText(/can’t route/i)).toBeInTheDocument();
    expect(rowIn(document.body, "src/new.ts")).toBeInTheDocument();
  });

  it("AC-6: a thrown command surfaces an inline retry, never crashing the overlay", async () => {
    routeMock.mockRejectedValue(new Error("boom"));
    render(
      <SourceDiffOverlay
        workspace={routableWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.click(routeBtn()!);
    expect(await screen.findByText(/route failed — retry/i)).toBeInTheDocument();
    expect(screen.getByTestId("source-diff-overlay-expanded-w1")).toBeInTheDocument();
  });

  it("AC-7: adds no commit / publish / sync control to the overlay", () => {
    render(
      <SourceDiffOverlay
        workspace={routableWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.queryByRole("button", { name: /commit/i })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /publish/i })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /sync/i })).not.toBeInTheDocument();
  });

  it("AC-8: the route action is a keyboard-focusable button, and keys inside it don't drive list nav", async () => {
    render(
      <SourceDiffOverlay
        workspace={routableWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    const btn = routeBtn()!;
    // A real <button> — in the tab order, operable by keyboard.
    expect(btn.tagName).toBe("BUTTON");
    // The control is wrapped in [data-route-action], so the list's j/k/Enter
    // handler leaves its keystrokes alone (no selection change, no file open).
    expect(btn.closest("[data-route-action]")).not.toBeNull();
    fireEvent.keyDown(btn, { key: "j" });
    fireEvent.keyDown(btn, { key: "Enter" });
    // the guard held: keys over the route control did NOT drive rail nav (the
    // auto-selected first file stays selected — no swap to this row's file).
    expect(useViewerStore.getState().sourceDiffSelectedFile["sid-ws"]).toBe("src/mod.ts");
  });

  it("a terminal notice (already-tracked) can be dismissed, returning the row to routable", async () => {
    routeMock.mockResolvedValue({ outcome: "alreadyTracked" });
    render(
      <SourceDiffOverlay
        workspace={routableWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.click(routeBtn()!);
    fireEvent.click(await screen.findByRole("button", { name: "Dismiss" }));
    await waitFor(() => expect(routeBtn()).toBeInTheDocument());
  });
});

// ────────────────────────────────────────────────────────────────────────────
// SLW-WD-5 — the expanded size is a master-detail layout (persistent rail + pane).
// ────────────────────────────────────────────────────────────────────────────

describe("SourceDiffOverlay — master-detail expanded layout (WD-5)", () => {
  const routableWs: Workspace = { ...storyWs, sourceRepoSlug: "acme/repo" };
  const rail = () => screen.getByTestId("source-diff-rail-w1");

  beforeEach(() => useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } }));

  function expand(sel = "src/mod.ts") {
    useViewerStore.setState({
      sourceDiffSelectedFile: { "sid-ws": sel },
      // SLW-WD-9: pin Working mode — these WD-5 master-detail tests assert the
      // Working rail/pane, so opt out of the adaptive History lead (AC-1c).
      sourceDiffHistoryMode: { "sid-ws": false },
    });
  }

  it("AC-1: expanded shows BOTH a file rail (all rows) AND the diff pane at once", () => {
    expand();
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    const expanded = screen.getByTestId("source-diff-overlay-expanded-w1");
    // rail lists the whole change-set (scoped so the selected path in the pane
    // header doesn't collide with the rail row of the same name)
    expect(rowIn(rail(), "src/mod.ts")).toBeInTheDocument();
    expect(rowIn(rail(), "src/new.ts")).toBeInTheDocument();
    expect(rowIn(rail(), "src/moved.ts")).toBeInTheDocument();
    // pane renders the selected file's diff simultaneously, inside the dialog
    // (src/mod.ts is source code → the code diff, WD-6)
    expect(expanded).toContainElement(screen.getByTestId("mock-code-diff-review"));
  });

  it("AC-2: clicking a rail file swaps the pane in place — rail stays", () => {
    expand("src/mod.ts");
    const { rerender } = render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.click(rowIn(rail(), "src/new.ts"));
    expect(useViewerStore.getState().sourceDiffSelectedFile["sid-ws"]).toBe("src/new.ts");
    rerender(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("source-diff-overlay-expanded-w1")).toBeInTheDocument();
    expect(rowIn(rail(), "src/moved.ts")).toBeInTheDocument(); // rail still lists all
    // the pane re-fetched the redline for the newly-selected file (same baseRef)
    expect(redlineMock).toHaveBeenCalledWith("/wt/story", "src/new.ts", undefined, undefined, 0);
  });

  it("AC-2: j/k in the expanded rail swaps the pane without collapsing", () => {
    expand("src/mod.ts");
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.keyDown(screen.getByTestId("source-diff-overlay-expanded-w1"), { key: "j" });
    expect(useViewerStore.getState().sourceDiffSelectedFile["sid-ws"]).toBe("src/new.ts");
  });

  it("AC-3: the selected rail row is marked active (aria-selected)", () => {
    expand("src/new.ts");
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    const active = rail().querySelector('[aria-selected="true"]');
    expect(active?.textContent).toContain("src/new.ts");
  });

  it("AC-4: an empty change-set shows the empty state in the rail, pane invites a pick", () => {
    stubChangeset({ files: [] });
    expand("src/mod.ts");
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // SLW-DV-2: the sole surface stays up; the rail shows the empty state and the
    // pane is a "pick a file" prompt rather than a dead pane.
    expect(screen.getByTestId("source-diff-overlay-expanded-w1")).toBeInTheDocument();
    expect(screen.getByText("No uncommitted changes.")).toBeInTheDocument();
    expect(screen.getByText("No changes to review.")).toBeInTheDocument();
  });

  it("AC-4: a redline error renders in the PANE while the rail stays listed", () => {
    stubRedline({ redline: null, error: "redline blew up" });
    expand("src/mod.ts");
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // src/mod.ts is source code → the code path surfaces the error in place (AC-6).
    expect(screen.getByTestId("source-diff-code-error")).toHaveTextContent("redline blew up");
    // a pane error must NOT blank the rail
    expect(rowIn(rail(), "src/new.ts")).toBeInTheDocument();
    expect(rowIn(rail(), "src/moved.ts")).toBeInTheDocument();
  });

  it("SLW-DV-2: Hide from the pane header hides the whole overlay", () => {
    expand("src/mod.ts");
    const { container, rerender } = render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.click(screen.getByTestId("source-diff-collapse-w1"));
    expect(useViewerStore.getState().sourceDiffShownBySession["sid-ws"]).toBe(false);
    rerender(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("AC-6: a binary selected file renders the placeholder in the pane; rail stays", () => {
    stubRedline({ redline: { status: "modified", binary: true, before: "", after: "" } });
    expand("src/mod.ts");
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("source-diff-binary-placeholder")).toBeInTheDocument();
    expect(screen.queryByTestId("mock-redline-prose")).not.toBeInTheDocument();
    expect(rowIn(rail(), "src/new.ts")).toBeInTheDocument();
  });

  it("AC-9: the WD-4 '→ context' affordance carries into the rail and routes", async () => {
    routeMock.mockResolvedValue({ outcome: "routed" });
    expand("src/mod.ts");
    render(
      <SourceDiffOverlay
        workspace={routableWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    const routeBtn = rail().querySelector("[data-route-to-context]") as HTMLElement | null;
    expect(routeBtn).not.toBeNull();
    fireEvent.click(routeBtn!);
    await waitFor(() =>
      expect(routeMock).toHaveBeenCalledWith("/wt/story", "acme/repo", "src/new.ts"),
    );
    expect(await within(rail()).findByText("routed")).toBeInTheDocument();
  });

  it("AC-9: a route made in the rail persists across a re-render (one map)", async () => {
    routeMock.mockResolvedValue({ outcome: "routed" });
    const { rerender } = render(
      <SourceDiffOverlay
        workspace={routableWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.click(document.querySelector("[data-route-to-context]")! as HTMLElement);
    expect(await screen.findByText("routed")).toBeInTheDocument();
    // same instance, same routeStates map → the rail row still reads routed
    expand();
    rerender(
      <SourceDiffOverlay
        workspace={routableWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(rail().textContent).toContain("routed");
  });

  it("AC-9: j over a focused route control in the rail does not swap the pane", () => {
    routeMock.mockResolvedValue({ outcome: "routed" });
    expand("src/mod.ts");
    render(
      <SourceDiffOverlay
        workspace={routableWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    const routeBtn = rail().querySelector("[data-route-to-context]") as HTMLElement;
    fireEvent.keyDown(routeBtn, { key: "j" });
    // selection unchanged — the [data-route-action] guard held in the rail too
    expect(useViewerStore.getState().sourceDiffSelectedFile["sid-ws"]).toBe("src/mod.ts");
  });

  it("AC-3: expanding with no prior selection defaults to the first file (self-heal)", () => {
    useViewerStore.setState({
      sourceDiffSelectedFile: { "sid-ws": null },
    });
    const { rerender } = render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // the effect auto-selects files[0] so the pane is never dead
    expect(useViewerStore.getState().sourceDiffSelectedFile["sid-ws"]).toBe("src/mod.ts");
    rerender(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("source-diff-overlay-expanded-w1")).toBeInTheDocument();
    // first file src/mod.ts is source code → the code diff (WD-6)
    expect(screen.getByTestId("mock-code-diff-review")).toBeInTheDocument();
  });

  it("j/k step files from inside the read-only diff pane too, but never from a text field", () => {
    expand("src/mod.ts");
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    const pane = document.querySelector("[data-source-diff-pane]") as HTMLElement;
    expect(pane).not.toBeNull();
    // Reading the diff and pressing j moves on to the next file.
    fireEvent.keyDown(pane, { key: "j" });
    expect(useViewerStore.getState().sourceDiffSelectedFile["sid-ws"]).toBe("src/new.ts");
    // Typing in the filter is text, not navigation.
    fireEvent.keyDown(screen.getByTestId("source-diff-filter-w1"), { key: "j" });
    expect(useViewerStore.getState().sourceDiffSelectedFile["sid-ws"]).toBe("src/new.ts");
  });

  it("AC-2: swapping to a rename in the rail threads its oldPath into the redline fetch", () => {
    expand("src/mod.ts");
    const { rerender } = render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.click(rowIn(rail(), "src/moved.ts"));
    rerender(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // the rename's baseline old-path is threaded (same baseRef) — not add-only
    expect(redlineMock).toHaveBeenCalledWith(
      "/wt/story",
      "src/moved.ts",
      "src/old.ts",
      undefined,
      0,
    );
  });

  it("AC-4: a first-fetch loading change-set shows the rail skeleton in the expanded layout", () => {
    stubChangeset({ files: null, loading: true });
    expand("src/mod.ts");
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // expanded surface holds during load (no flicker to compact)…
    expect(screen.getByTestId("source-diff-overlay-expanded-w1")).toBeInTheDocument();
    // …and the empty state is NOT shown while loading (skeleton, not "No source changes")
    expect(screen.queryByText("No uncommitted changes.")).not.toBeInTheDocument();
  });

  it("AC-3: the rail scrolls independently of the pane (each has its own overflow)", () => {
    expand("src/mod.ts");
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // the rail's list body owns vertical overflow…
    expect(rail().querySelector(".overflow-y-auto")).not.toBeNull();
    // …and the pane body owns its own overflow independently
    expect(document.querySelector("[data-source-diff-pane]")?.className).toContain("overflow-auto");
  });
});

describe("SourceDiffOverlay — worktree editor entry (SLW-DV-3)", () => {
  beforeEach(() => useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } }));

  it("RD-2: the header 'Open editor' control opens the worktree editor via the store", () => {
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(useViewerStore.getState().worktreeEditorOpenBySession["sid-ws"] ?? false).toBe(false);
    fireEvent.click(screen.getByTestId("source-diff-open-editor-w1"));
    expect(useViewerStore.getState().worktreeEditorOpenBySession["sid-ws"]).toBe(true);
    // The editor is a separate tile surface now (WorktreeEditorSurface in
    // workspace-terminal-panels) — it does NOT render inside this overlay.
    expect(screen.queryByTestId("source-editor-inset-w1")).not.toBeInTheDocument();
  });

  it("RD-2/RD-4: opening from the diff seeds the editor's remembered file from the selection", () => {
    useViewerStore.setState({
      sourceDiffSelectedFile: { "sid-ws": "src/mod.ts" },
      worktreeEditorFile: {},
    });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.click(screen.getByTestId("source-diff-open-editor-w1"));
    // The button writes the diff selection through to the editor's memory
    // (`worktreeEditorFile`) and opens — so the editor resumes on that file. The
    // diff's own selection is left intact.
    expect(useViewerStore.getState().worktreeEditorFile.w1).toBe("src/mod.ts");
    expect(useViewerStore.getState().worktreeEditorOpenBySession["sid-ws"]).toBe(true);
    expect(useViewerStore.getState().sourceDiffSelectedFile["sid-ws"]).toBe("src/mod.ts");
  });

  it("SLW-WDE-5 §A/§D: the header hosts the EXTERNAL-editor control beside the in-app one", () => {
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // both controls coexist, disambiguated: the in-app "Open editor" and the
    // external escape hatch (distinct testid + not labeled "Open editor").
    expect(screen.getByTestId("source-diff-open-editor-w1")).toBeInTheDocument();
    const external = screen.getByTestId("source-open-external-editor-w1");
    expect(external).toBeInTheDocument();
    expect(external).toHaveAttribute("aria-label", "Open in editor");
  });

  // SLW-DV-3: the editor takes precedence over the diff — it renders as a sibling
  // tile surface, so this overlay yields (never stacks under it).
  it("yields (renders nothing) while the worktree editor is open over a shown diff", () => {
    useViewerStore.setState({
      sourceDiffShownBySession: { "sid-ws": true },
      worktreeEditorOpenBySession: { "sid-ws": true },
    });
    const { container } = render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("does NOT hide the diff on Escape while the editor is open (the editor owns Escape)", () => {
    useViewerStore.setState({
      sourceDiffShownBySession: { "sid-ws": true },
      worktreeEditorOpenBySession: { "sid-ws": true },
    });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.keyDown(document, { key: "Escape" });
    // One Escape must not both close the editor and hide the diff beneath it.
    expect(useViewerStore.getState().sourceDiffShownBySession["sid-ws"]).toBe(true);
  });
});

// ────────────────────────────────────────────────────────────────────────────
// SLW-WD-9 — History mode: adaptive lead, the mode toggle, and commit selection.
// ────────────────────────────────────────────────────────────────────────────

describe("SourceDiffOverlay — History mode (SLW-WD-9)", () => {
  beforeEach(() => useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } }));

  it("AC-1c: a CLEAN tree leads with History (the commit graph rail)", () => {
    stubChangeset({ files: [] }); // no working changes → adaptive picks History
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("source-diff-history-w1")).toBeInTheDocument();
    expect(screen.getByTestId("mock-commit-graph-rail")).toBeInTheDocument();
    // The store recorded the adaptive pick (History = true).
    expect(useViewerStore.getState().sourceDiffHistoryMode["sid-ws"]).toBe(true);
  });

  it("AC-1c: a DIRTY tree leads with Working (the file list), not History", () => {
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    ); // default FILES = dirty
    expect(screen.getByTestId("source-diff-overlay-expanded-w1")).toBeInTheDocument();
    expect(screen.queryByTestId("mock-commit-graph-rail")).not.toBeInTheDocument();
    expect(useViewerStore.getState().sourceDiffHistoryMode["sid-ws"]).toBe(false);
  });

  it("AC-1: the mode toggle switches Working ↔ History and back", () => {
    const { rerender } = render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // starts Working (dirty tree)
    expect(screen.queryByTestId("mock-commit-graph-rail")).not.toBeInTheDocument();
    fireEvent.click(screen.getByTestId("source-diff-mode-toggle-w1"));
    expect(useViewerStore.getState().sourceDiffHistoryMode["sid-ws"]).toBe(true);
    rerender(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("mock-commit-graph-rail")).toBeInTheDocument();
    // toggle back
    fireEvent.click(screen.getByTestId("source-diff-mode-toggle-w1"));
    expect(useViewerStore.getState().sourceDiffHistoryMode["sid-ws"]).toBe(false);
  });

  it("AC-1c: an explicit toggle wins — the adaptive pick does not override it", () => {
    // Pre-set History even though the tree is dirty; the adaptive effect must NOT
    // flip it back to Working (historyMode !== undefined → no re-adapt, RD-12).
    useViewerStore.setState({ sourceDiffHistoryMode: { "sid-ws": true } });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    ); // dirty tree
    expect(screen.getByTestId("mock-commit-graph-rail")).toBeInTheDocument();
    expect(useViewerStore.getState().sourceDiffHistoryMode["sid-ws"]).toBe(true);
  });

  it("AC-2: the Working node (selectedSha null) shows the working changeset in the pane", () => {
    useViewerStore.setState({
      sourceDiffHistoryMode: { "sid-ws": true },
      sourceDiffSelectedSha: { "sid-ws": null },
    });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // The detail pane lists the working files (passed in, not the commit hook).
    expect(
      within(screen.getByTestId("source-diff-history-w1")).getByText("mod.ts"),
    ).toBeInTheDocument();
    expect(commitChangesetMock).toHaveBeenCalledWith("/wt/story", null);
  });

  it("SLW-DV-6: History header shows the total +/− and its wrap/expand toggles drive the commit pane (AC-2/AC-4/AC-6)", () => {
    stubChangeset({
      files: [
        { path: "src/a.ts", status: "modified", added: 3, removed: 1 },
        { path: "src/b.ts", status: "added", added: 10, removed: 0 },
      ],
    });
    useViewerStore.setState({
      sourceDiffHistoryMode: { "sid-ws": true },
      sourceDiffSelectedSha: { "sid-ws": null }, // Working node → the stubbed redline resolves
    });
    const { rerender } = render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // The commit header sums the change-set into the stats slot.
    expect(screen.getByLabelText("13 added, 1 removed")).toBeInTheDocument();
    // The commit pane renders a code diff, default no-wrap / collapse.
    const diff = screen.getByTestId("mock-code-diff-review");
    expect(diff).toHaveAttribute("data-wrap", "nowrap");
    expect(diff).toHaveAttribute("data-collapse", "collapse");
    // The commit-diff controls thread through CommitDetailPane and flip the pane.
    fireEvent.click(screen.getByTestId("commit-diff-wrap-toggle-w1"));
    fireEvent.click(screen.getByTestId("commit-diff-expand-toggle-w1"));
    rerender(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    const flipped = screen.getByTestId("mock-code-diff-review");
    expect(flipped).toHaveAttribute("data-wrap", "wrap");
    expect(flipped).toHaveAttribute("data-collapse", "expand");
  });

  it("hides the commit-diff controls for a mode-only change and for a redline error", () => {
    stubChangeset({ files: [{ path: "src/a.ts", status: "modified" }] });
    useViewerStore.setState({
      sourceDiffHistoryMode: { "sid-ws": true },
      sourceDiffSelectedSha: { "sid-ws": null },
    });
    stubRedline({ redline: { status: "modified", binary: false, before: "same", after: "same" } });
    const { rerender } = render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.queryByTestId("commit-diff-wrap-toggle-w1")).toBeNull();
    stubRedline({ error: "boom" });
    rerender(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.queryByTestId("commit-diff-wrap-toggle-w1")).toBeNull();
  });

  it("AC-2/AC-8: selecting a commit sha fetches + shows THAT commit's changeset", () => {
    commitChangesetMock.mockReturnValue({
      files: [{ path: "src/committed.ts", status: "added" }],
      loading: false,
      error: null,
    });
    useViewerStore.setState({
      sourceDiffHistoryMode: { "sid-ws": true },
      sourceDiffSelectedSha: { "sid-ws": "abc123" },
    });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // The commit changeset was fetched by sha (lazy, AC-8)…
    expect(commitChangesetMock).toHaveBeenCalledWith("/wt/story", "abc123");
    // …and its file is listed in the pane.
    expect(screen.getByText("committed.ts")).toBeInTheDocument();
  });

  it("AC-10b: a clean tree with commits still renders the graph (never a dead end)", () => {
    stubChangeset({ files: [] });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // The graph rail renders regardless of working-tree state — no "No source
    // changes" wall (that text is only ever the Working node's empty pane).
    expect(screen.getByTestId("mock-commit-graph-rail")).toBeInTheDocument();
  });
});

describe("SourceDiffOverlay — review tool: filter, tree, keys", () => {
  beforeEach(() => useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } }));
  const shell = () => screen.getByTestId("source-diff-overlay-expanded-w1");
  const selected = () => useViewerStore.getState().sourceDiffSelectedFile["sid-ws"];

  it("filters the rail and walks only the matching files with j/k", () => {
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.change(screen.getByTestId("source-diff-filter-w1"), { target: { value: "mo" } });
    const rail = screen.getByTestId("source-diff-rail-w1");
    expect(rail.querySelectorAll("[data-source-diff-row]")).toHaveLength(2);
    expect(selected()).toBe("src/mod.ts");
    fireEvent.keyDown(shell(), { key: "j" });
    expect(selected()).toBe("src/moved.ts");
    fireEvent.keyDown(shell(), { key: "j" });
    expect(selected()).toBe("src/moved.ts"); // clamped at the last match
  });

  it("says so when nothing matches the filter", () => {
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.change(screen.getByTestId("source-diff-filter-w1"), { target: { value: "zzz" } });
    expect(screen.getByText("No files match “zzz”.")).toBeInTheDocument();
  });

  it("/ focuses the filter", () => {
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.keyDown(shell(), { key: "/" });
    expect(document.activeElement).toBe(screen.getByTestId("source-diff-filter-w1"));
  });

  it("t groups the rail by folder and a folded folder drops out of j/k", () => {
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.keyDown(shell(), { key: "t" });
    expect(useViewerStore.getState().sourceDiffViewOptions["sid-ws"]?.fileTree).toBe(true);
    const srcFolder = screen.getByRole("button", { name: "src" });
    fireEvent.click(srcFolder);
    expect(srcFolder).toHaveAttribute("aria-expanded", "false");
    const rail = screen.getByTestId("source-diff-rail-w1");
    expect(rail.querySelectorAll("[data-source-diff-row]")).toHaveLength(1); // docs/readme.md
  });

  it("n / p step through the open file's changes", () => {
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.keyDown(shell(), { key: "n" });
    fireEvent.keyDown(shell(), { key: "p" });
    expect(navigatorSpy.next).toHaveBeenCalledTimes(1);
    expect(navigatorSpy.prev).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByTestId("source-diff-next-change-w1"));
    expect(navigatorSpy.next).toHaveBeenCalledTimes(2);
  });

  it("the change counter reads the diff's reports and clears on a file switch", () => {
    useViewerStore.setState({ sourceDiffSelectedFile: { "sid-ws": "src/mod.ts" } });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    const nav = () => screen.getByTestId("source-diff-chunk-nav-w1");
    expect(nav()).toHaveTextContent("—");
    act(() => chunkReporter.current!({ index: null, count: 3 }));
    expect(nav()).toHaveTextContent("3 changes");
    act(() => chunkReporter.current!({ index: 1, count: 3 }));
    expect(nav()).toHaveTextContent("2 of 3");
    fireEvent.keyDown(screen.getByTestId("source-diff-overlay-expanded-w1"), { key: "j" });
    expect(nav()).toHaveTextContent("—");
  });

  it("b flips the comparison basis; s and w flip split and wrap", () => {
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.keyDown(shell(), { key: "b" });
    expect(useViewerStore.getState().sourceDiffBaseMode["sid-ws"]).toBe("head");
    fireEvent.keyDown(shell(), { key: "s" });
    expect(screen.getByTestId("mock-code-diff-review")).toHaveAttribute("data-mode", "split");
    fireEvent.keyDown(shell(), { key: "w" });
    expect(screen.getByTestId("mock-code-diff-review")).toHaveAttribute("data-wrap", "wrap");
  });

  it("copies the open file's path", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    const original = Object.getOwnPropertyDescriptor(navigator, "clipboard");
    onTestFinished(() => {
      if (original) Object.defineProperty(navigator, "clipboard", original);
      else delete (navigator as { clipboard?: unknown }).clipboard;
    });
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.click(screen.getByTestId("source-diff-copy-path-w1"));
    expect(writeText).toHaveBeenCalledWith("src/mod.ts");
    expect(await screen.findByRole("button", { name: "Path copied" })).toBeInTheDocument();
  });

  it("r refreshes the change list", () => {
    const reload = vi.fn();
    stubChangeset({ reload });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.keyDown(shell(), { key: "r" });
    expect(reload).toHaveBeenCalled();
  });
});

describe("SourceDiffOverlay — a workspace on a machine", () => {
  const remoteWs: Workspace = {
    ...storyWs,
    cwd: "",
    sourceRepoSlug: "acme/repo",
    location: {
      kind: "machine",
      machineId: "m-studio",
      requestId: "r1",
      baseRef: "main",
      branch: "feat/x",
      handle: "h1",
    },
  };
  beforeEach(() => useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } }));

  it("diffs through the machine, names it, and states the fork-point basis", () => {
    render(
      <SourceDiffOverlay
        workspace={remoteWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(changesetMock).toHaveBeenCalledWith(
      { kind: "remote", machineId: "m-studio", handle: "h1" },
      undefined,
      true,
      0,
    );
    expect(redlineMock).toHaveBeenCalledWith(
      { kind: "remote", machineId: "m-studio", handle: "h1" },
      "src/mod.ts",
      undefined,
      undefined,
      0,
    );
    expect(screen.getByTestId("source-diff-machine-w1")).toHaveTextContent("m-studio");
    expect(screen.getByTestId("source-diff-base-toggle-w1")).toHaveTextContent(
      "vs main (fork point)",
    );
  });

  it("offers no local-only actions: history, route-to-context, external editor", () => {
    render(
      <SourceDiffOverlay
        workspace={remoteWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.queryByTestId("source-diff-mode-toggle-w1")).not.toBeInTheDocument();
    expect(document.querySelector("[data-route-to-context]")).toBeNull();
    expect(screen.queryByTestId("source-open-external-editor-w1")).not.toBeInTheDocument();
    // The in-app code editor reads remote files, so it stays.
    expect(screen.getByTestId("source-diff-open-editor-w1")).toBeInTheDocument();
  });

  it("never auto-flips a clean remote tree into the local-only History view", () => {
    stubChangeset({ files: [] });
    render(
      <SourceDiffOverlay
        workspace={remoteWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(useViewerStore.getState().sourceDiffHistoryMode["sid-ws"]).toBeUndefined();
    expect(screen.getByText("No uncommitted changes.")).toBeInTheDocument();
  });

  it("shows the shared 'Loading diff…' copy on a remote tab's first read", () => {
    stubChangeset({ files: null, loading: true });
    render(
      <SourceDiffOverlay
        workspace={remoteWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // SW-VE-12 (AC-4): one shared "Loading diff…" copy, local or remote —
    // the old remote-specific "Reading changes on {machine}…" is gone.
    expect(screen.getByTestId("source-diff-loading")).toHaveTextContent("Loading diff…");
  });
});

describe("SourceDiffOverlay — final polish", () => {
  beforeEach(() => {
    useViewerStore.setState({
      sourceDiffShownBySession: { "sid-ws": true },
      worktreeEditorFile: {},
      worktreeEditorRevealLine: {},
    });
    navigatorSpy.currentLine.mockReturnValue(null);
  });
  const shell = () => screen.getByTestId("source-diff-overlay-expanded-w1");
  const overlay = (ws: Workspace = storyWs) => (
    <SourceDiffOverlay workspace={ws} teamKey="team-a" activeTabSessionId="sid-ws" isFocused />
  );

  it("takes focus on open (so j/k work at once) and hands it back on hide", async () => {
    const terminal = document.createElement("textarea");
    document.body.appendChild(terminal);
    terminal.focus();
    useViewerStore.setState({ sourceDiffShownBySession: {} });
    const { rerender } = render(overlay());
    act(() => useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } }));
    rerender(overlay());
    await waitFor(() => expect(shell()).toHaveFocus());
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    await waitFor(() => expect(terminal).toHaveFocus());
    terminal.remove();
  });

  it("opened from a menu item, it hands focus back to the target the opener recorded", async () => {
    const terminal = document.createElement("textarea");
    document.body.appendChild(terminal);
    // The caption menu is still mounted (exit animation) when the diff opens.
    const menu = document.createElement("div");
    menu.setAttribute("role", "menu");
    const item = document.createElement("div");
    item.setAttribute("role", "menuitem");
    item.tabIndex = -1;
    menu.appendChild(item);
    document.body.appendChild(menu);
    item.focus();
    setSourceDiffReturnFocus("sid-ws", terminal);
    useViewerStore.setState({ sourceDiffShownBySession: {} });
    const { rerender } = render(overlay());
    act(() => useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } }));
    rerender(overlay());
    await waitFor(() => expect(shell()).toHaveFocus());
    menu.remove();
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    await waitFor(() => expect(terminal).toHaveFocus());
    terminal.remove();
  });

  it("keeps the return target across the code editor's handoff and back", async () => {
    const terminal = document.createElement("textarea");
    document.body.appendChild(terminal);
    terminal.focus();
    useViewerStore.setState({ sourceDiffShownBySession: {}, worktreeEditorOpenBySession: {} });
    const { rerender } = render(overlay());
    act(() => useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } }));
    rerender(overlay());
    await waitFor(() => expect(shell()).toHaveFocus());
    // `e`: the editor opens in the diff's place (closing it), focus leaves.
    act(() =>
      useViewerStore.getState().toggleWorktreeEditorOpen({
        sessionId: "sid-ws",
        workspaceId: "w1",
        teamKey: "team-a",
        open: true,
      }),
    );
    rerender(overlay());
    expect(screen.queryByTestId("source-diff-overlay-expanded-w1")).toBeNull();
    (document.activeElement as HTMLElement | null)?.blur();
    // The diff shortcut from the editor brings the diff back (closing the editor).
    act(() =>
      useViewerStore.getState().toggleSourceDiffShown({
        sessionId: "sid-ws",
        workspaceId: "w1",
        teamKey: "team-a",
        shown: true,
      }),
    );
    rerender(overlay());
    await waitFor(() => expect(shell()).toHaveFocus());
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    await waitFor(() => expect(terminal).toHaveFocus());
    terminal.remove();
  });

  it("? opens a keyboard map listing every key; Escape closes the map, not the tool", () => {
    render(overlay());
    fireEvent.keyDown(shell(), { key: "?" });
    const map = screen.getByRole("region", { name: "Keyboard shortcuts" });
    for (const label of [
      "Next / previous file",
      "Next / previous change",
      "Filter files",
      "Open in code editor",
    ]) {
      expect(within(map).getByText(label)).toBeInTheDocument();
    }
    fireEvent.keyDown(shell(), { key: "Escape" });
    expect(screen.queryByRole("region", { name: "Keyboard shortcuts" })).not.toBeInTheDocument();
    expect(useViewerStore.getState().sourceDiffShownBySession["sid-ws"]).toBe(true);
    // The rail footer's "?" opens it too — discoverable without knowing the key.
    fireEvent.click(screen.getByRole("button", { name: "Keyboard shortcuts" }));
    expect(screen.getByRole("region", { name: "Keyboard shortcuts" })).toBeInTheDocument();
  });

  it("SW-FX-140: with the keyboard map open, Escape from the terminal closes the map, not the panel", () => {
    render(
      <div>
        {overlay()}
        <div className="xterm">
          <textarea data-testid="term-input" />
        </div>
      </div>,
    );
    fireEvent.keyDown(shell(), { key: "?" });
    expect(screen.getByRole("region", { name: "Keyboard shortcuts" })).toBeInTheDocument();
    fireEvent.keyDown(screen.getByTestId("term-input"), { key: "Escape" });
    expect(screen.queryByRole("region", { name: "Keyboard shortcuts" })).not.toBeInTheDocument();
    expect(useViewerStore.getState().sourceDiffShownBySession["sid-ws"]).toBe(true);
  });

  it("stepping to the next file keeps the code controls up while its diff loads (no header shift)", () => {
    const { rerender } = render(overlay());
    expect(screen.getByTestId("source-diff-view-toggle-w1")).toBeInTheDocument();
    stubRedline({ redline: null, error: null });
    fireEvent.keyDown(shell(), { key: "j" });
    rerender(overlay());
    expect(screen.getByTestId("source-diff-code-skeleton")).toBeInTheDocument();
    expect(screen.getByTestId("source-diff-view-toggle-w1")).toBeInTheDocument();
    // …and a file that turns out binary drops them once known.
    stubRedline({ redline: { status: "added", binary: true, before: "", after: "" } });
    rerender(overlay());
    expect(screen.queryByTestId("source-diff-view-toggle-w1")).not.toBeInTheDocument();
  });

  it("j/k keep the selected row in view in a long list", () => {
    const spy = vi.fn();
    const original = Element.prototype.scrollIntoView;
    Element.prototype.scrollIntoView = spy;
    try {
      render(overlay());
      spy.mockClear();
      fireEvent.keyDown(shell(), { key: "j" });
      expect(spy).toHaveBeenCalledWith({ block: "nearest" });
      expect(spy.mock.contexts.at(-1)).toBe(
        rowIn(screen.getByTestId("source-diff-rail-w1"), "src/new.ts"),
      );
    } finally {
      Element.prototype.scrollIntoView = original;
    }
  });

  it("'Open in code editor' lands on the line of the change being read", () => {
    navigatorSpy.currentLine.mockReturnValue(42);
    render(overlay());
    fireEvent.keyDown(shell(), { key: "e" });
    const s = useViewerStore.getState();
    expect(s.worktreeEditorFile.w1).toBe("src/mod.ts");
    expect(s.worktreeEditorRevealLine.w1).toEqual({ path: "src/mod.ts", line: 42 });
    expect(s.worktreeEditorOpenBySession["sid-ws"]).toBe(true);
  });

  it("'Open in code editor' with no change to land on opens the file plainly", () => {
    render(overlay());
    fireEvent.click(screen.getByTestId("source-diff-open-editor-w1"));
    expect(useViewerStore.getState().worktreeEditorRevealLine.w1).toBeUndefined();
    expect(useViewerStore.getState().worktreeEditorFile.w1).toBe("src/mod.ts");
  });

  it("r re-reads the open file's diff, not only the list", () => {
    const reload = vi.fn();
    stubChangeset({ reload });
    render(overlay());
    const before = redlineMock.mock.calls.at(-1)!;
    fireEvent.keyDown(shell(), { key: "r" });
    expect(reload).toHaveBeenCalled();
    const after = redlineMock.mock.calls.at(-1)!;
    expect(after[4]).toBe((before[4] as number) + 1);
  });

  it("a mode-only change says so instead of an empty diff", () => {
    stubChangeset({ files: [{ path: "bin/run.sh", status: "modified", badges: ["mode"] }] });
    stubRedline({ redline: { status: "modified", binary: false, before: "x\n", after: "x\n" } });
    useViewerStore.setState({ sourceDiffSelectedFile: { "sid-ws": "bin/run.sh" } });
    render(overlay());
    expect(screen.getByTestId("source-diff-no-content-change")).toHaveTextContent(
      "Only the file mode changed",
    );
    expect(screen.queryByTestId("mock-code-diff-review")).not.toBeInTheDocument();
    // …and no diff controls with nothing to drive.
    expect(screen.queryByTestId("source-diff-chunk-nav-w1")).not.toBeInTheDocument();
  });

  it("a binary file reads as plain fact, not a warning", () => {
    stubRedline({ redline: { status: "added", binary: true, before: "", after: "" } });
    render(overlay());
    expect(screen.getByTestId("source-diff-binary-placeholder")).toHaveTextContent(
      "Binary file — no text diff to show",
    );
  });

  it("an errored change list leaves the pane quiet — no 'select a file' prompt", () => {
    stubChangeset({ files: null, error: "fatal: not a git repository" });
    render(overlay());
    expect(screen.queryByText(/select a file/i)).not.toBeInTheDocument();
  });

  describe("on a machine", () => {
    const remoteWs: Workspace = {
      ...storyWs,
      cwd: "",
      location: {
        kind: "machine",
        machineId: "m-studio",
        requestId: "r1",
        baseRef: "main",
        branch: "feat/x",
        handle: "h1",
      },
    };

    it("names the machine by its label once known", () => {
      useMachinesStore.setState({
        byTeam: { "team-a": [{ machineId: "m-studio", label: "Studio", kind: "ssh" } as never] },
      });
      render(overlay(remoteWs));
      expect(screen.getByTestId("source-diff-machine-w1")).toHaveTextContent("Studio");
      useMachinesStore.setState({ byTeam: {} });
    });

    it("a machine with a blank label is named by its id, never as This Mac", () => {
      useMachinesStore.setState({
        byTeam: { "team-a": [{ machineId: "m-studio", label: "", kind: "ssh" } as never] },
      });
      render(overlay(remoteWs));
      expect(screen.getByTestId("source-diff-machine-w1")).toHaveTextContent("m-studio");
      expect(screen.queryByText("This Mac")).toBeNull();
      useMachinesStore.setState({ byTeam: {} });
    });

    it("an out-of-date terminal server reads as an update to make, with the way there", () => {
      stubChangeset({
        files: null,
        error:
          "This machine's terminal server is too old to diff its workspaces. Update the terminal server from Machines, then retry.",
      });
      render(overlay(remoteWs));
      const rail = screen.getByTestId("source-diff-rail-w1");
      expect(within(rail).getByText("Update the terminal server")).toBeInTheDocument();
      expect(within(rail).queryByText(/couldn’t load/i)).not.toBeInTheDocument();
      expect(within(rail).getByRole("button", { name: "Open Machines" })).toBeInTheDocument();
      expect(within(rail).getByRole("button", { name: "Retry" })).toBeInTheDocument();
    });
  });
});

describe("SourceDiffOverlay — review round 1", () => {
  beforeEach(() =>
    useViewerStore.setState({
      sourceDiffShownBySession: { "sid-ws": true },
      worktreeEditorFile: {},
      worktreeEditorRevealLine: {},
    }),
  );
  const overlay = (ws: Workspace = storyWs) => (
    <SourceDiffOverlay workspace={ws} teamKey="team-a" activeTabSessionId="sid-ws" isFocused />
  );

  it("Enter on a focused header control is left to the control, not taken for a file", () => {
    useViewerStore.setState({ sourceDiffSelectedFile: { "sid-ws": "src/new.ts" } });
    render(overlay());
    const refresh = screen.getByTestId("source-diff-refresh-w1");
    refresh.focus();
    // fireEvent returns false when the handler called preventDefault.
    expect(fireEvent.keyDown(refresh, { key: "Enter" })).toBe(true);
    expect(useViewerStore.getState().sourceDiffSelectedFile["sid-ws"]).toBe("src/new.ts");
  });

  it("Enter in the filter moves focus into the list, so j/k keep working", () => {
    useViewerStore.setState({ sourceDiffSelectedFile: { "sid-ws": "src/mod.ts" } });
    render(overlay());
    const filter = screen.getByTestId("source-diff-filter-w1");
    filter.focus();
    fireEvent.keyDown(filter, { key: "Enter" });
    expect(document.activeElement).toBe(
      rowIn(screen.getByTestId("source-diff-rail-w1"), "src/mod.ts"),
    );
  });

  it("Escape in an empty filter hands focus back to the tool, not to the page", () => {
    render(overlay());
    const filter = screen.getByTestId("source-diff-filter-w1");
    filter.focus();
    fireEvent.keyDown(filter, { key: "Escape" });
    expect(document.activeElement).toBe(screen.getByTestId("source-diff-overlay-expanded-w1"));
  });

  it("a workspace switch clears the last worktree's filter", () => {
    const { rerender } = render(overlay());
    fireEvent.change(screen.getByTestId("source-diff-filter-w1"), { target: { value: "zzz" } });
    rerender(overlay({ ...storyWs, id: "w2" }));
    expect(screen.getByTestId("source-diff-filter-w2")).toHaveValue("");
    expect(screen.queryByText("No files match “zzz”.")).not.toBeInTheDocument();
  });

  it("e on a deleted file opens the code editor without that file", () => {
    useViewerStore.setState({ sourceDiffSelectedFile: { "sid-ws": "docs/readme.md" } });
    render(overlay());
    fireEvent.keyDown(screen.getByTestId("source-diff-overlay-expanded-w1"), { key: "e" });
    expect(useViewerStore.getState().worktreeEditorFile.w1 ?? null).toBeNull();
  });
});

describe("SourceDiffOverlay — review round 2", () => {
  beforeEach(() =>
    useViewerStore.setState({
      sourceDiffShownBySession: { "sid-ws": true },
      sourceDiffViewOptions: {},
      worktreeEditorFile: {},
      worktreeEditorRevealLine: {},
    }),
  );
  const overlay = (ws: Workspace = storyWs) => (
    <SourceDiffOverlay workspace={ws} teamKey="team-a" activeTabSessionId="sid-ws" isFocused />
  );
  const shell = () => screen.getByTestId("source-diff-overlay-expanded-w1");

  it("t flips an automatically chosen tree back to a flat list", () => {
    stubChangeset({
      files: [
        { path: "a/1.ts", status: "modified" },
        { path: "a/2.ts", status: "modified" },
        { path: "a/3.ts", status: "modified" },
        { path: "b/4.ts", status: "modified" },
        { path: "b/5.ts", status: "modified" },
        { path: "b/6.ts", status: "modified" },
      ],
    });
    render(overlay());
    // Six files over two folders: the rail picks the tree by itself.
    expect(screen.getByRole("button", { name: "a" })).toBeInTheDocument();
    fireEvent.keyDown(shell(), { key: "t" });
    expect(useViewerStore.getState().sourceDiffViewOptions["sid-ws"]?.fileTree).toBe(false);
    expect(screen.queryByRole("button", { name: "a" })).toBeNull();
  });

  it("an automatically chosen layout holds while live refreshes cross the threshold", () => {
    const six = [
      { path: "a/1.ts", status: "modified" as const },
      { path: "a/2.ts", status: "modified" as const },
      { path: "a/3.ts", status: "modified" as const },
      { path: "b/4.ts", status: "modified" as const },
      { path: "b/5.ts", status: "modified" as const },
      { path: "b/6.ts", status: "modified" as const },
    ];
    stubChangeset({ files: six });
    const { rerender } = render(overlay());
    expect(screen.getByRole("button", { name: "a" })).toBeInTheDocument();
    // The agent reverts most of it: still the tree the reader was reading.
    stubChangeset({ files: six.slice(0, 2) });
    rerender(overlay());
    expect(screen.getByRole("button", { name: "a" })).toBeInTheDocument();
  });

  it("a workspace switch chooses the new list's layout, never the old one's", () => {
    const six = ["a/1.ts", "a/2.ts", "a/3.ts", "b/4.ts", "b/5.ts", "b/6.ts"].map((path) => ({
      path,
      status: "modified" as const,
    }));
    stubChangeset({ files: six });
    const { rerender } = render(overlay());
    expect(screen.getByRole("button", { name: "a" })).toBeInTheDocument();
    // The switch render still sees the old list, then the new one lands.
    rerender(overlay({ ...storyWs, id: "w2" }));
    stubChangeset({ files: [{ path: "one.ts", status: "modified" }] });
    rerender(overlay({ ...storyWs, id: "w2" }));
    // The new worktree's own list grows past the threshold: still flat.
    stubChangeset({ files: six });
    rerender(overlay({ ...storyWs, id: "w2" }));
    expect(screen.queryByRole("button", { name: "a" })).toBeNull();
  });

  it("closing the keyboard map from its focused button keeps focus in the tool", () => {
    render(overlay());
    shell().focus();
    fireEvent.keyDown(shell(), { key: "?" });
    const close = screen.getByRole("button", { name: /close/i });
    close.focus();
    fireEvent.click(close);
    expect(screen.queryByTestId("source-diff-keymap")).toBeNull();
    expect(document.activeElement).toBe(shell());
  });

  it("never takes focus back after the user pressed outside the tool (focus on body)", async () => {
    const { rerender } = render(overlay());
    shell().focus();
    const outside = document.createElement("div");
    document.body.appendChild(outside);
    try {
      // A press on something unfocusable outside: focus falls to body, no focusin.
      fireEvent.pointerDown(outside);
      shell().blur();
      expect(document.activeElement).toBe(document.body);
      await act(() => new Promise((r) => setTimeout(r, 0)));
      stubChangeset({ files: FILES.filter((f) => f.path !== "src/new.ts") });
      rerender(overlay());
      expect(document.activeElement).toBe(document.body);
    } finally {
      outside.remove();
    }
  });

  it("an outside press that leaves focus in the tool keeps the recovery armed", async () => {
    render(overlay());
    const rail = screen.getByTestId("source-diff-rail-w1");
    const row = rowIn(rail, "src/mod.ts");
    row.focus();
    const outside = document.createElement("div");
    document.body.appendChild(outside);
    try {
      // A press outside that moves no focus (a scrollbar, a prevented mousedown).
      fireEvent.pointerDown(outside);
      await act(() => new Promise((r) => setTimeout(r, 0)));
      // A press inside the tool never disarms it either.
      fireEvent.pointerDown(row);
      await act(() => new Promise((r) => setTimeout(r, 0)));
      stubChangeset({ files: null, loading: true });
      fireEvent.keyDown(row, { key: "b" });
      expect(document.activeElement).toBe(shell());
    } finally {
      outside.remove();
    }
  });

  it("b pressed on a focused row keeps focus in the tool as the list reloads", () => {
    render(overlay());
    const rail = screen.getByTestId("source-diff-rail-w1");
    const row = rowIn(rail, "src/mod.ts");
    row.focus();
    stubChangeset({ files: null, loading: true });
    fireEvent.keyDown(row, { key: "b" });
    expect(document.activeElement).toBe(shell());
  });

  it("hiding the tool closes the keyboard map", () => {
    const { rerender } = render(overlay());
    fireEvent.keyDown(shell(), { key: "?" });
    expect(screen.getByTestId("source-diff-keymap")).toBeInTheDocument();
    act(() => useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": false } }));
    rerender(overlay());
    act(() => useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } }));
    rerender(overlay());
    expect(screen.queryByTestId("source-diff-keymap")).toBeNull();
  });

  it("a held toggle key acts once, not on every repeat", () => {
    render(overlay());
    const before = useViewerStore.getState().sourceDiffViewOptions["sid-ws"]?.wordWrap ?? false;
    fireEvent.keyDown(shell(), { key: "w" });
    fireEvent.keyDown(shell(), { key: "w", repeat: true });
    expect(useViewerStore.getState().sourceDiffViewOptions["sid-ws"]?.wordWrap).toBe(!before);
    // j still repeats: a held j walks the list.
    const start = useViewerStore.getState().sourceDiffSelectedFile["sid-ws"];
    fireEvent.keyDown(shell(), { key: "j" });
    fireEvent.keyDown(shell(), { key: "j", repeat: true });
    const order = [
      ...screen.getByTestId("source-diff-rail-w1").querySelectorAll<HTMLElement>("li[data-path]"),
    ].map((li) => li.dataset.path);
    const from = start ? order.indexOf(start) : -1;
    expect(useViewerStore.getState().sourceDiffSelectedFile["sid-ws"]).toBe(order[from + 2]);
  });

  it("Enter confirming an IME composition in the filter stays in the filter", () => {
    render(overlay());
    const filter = screen.getByTestId("source-diff-filter-w1");
    filter.focus();
    fireEvent.keyDown(filter, { key: "Enter", isComposing: true, keyCode: 229 });
    expect(document.activeElement).toBe(filter);
  });

  it("a filter shows matches inside a folded folder", () => {
    render(overlay());
    fireEvent.keyDown(shell(), { key: "t" });
    fireEvent.click(screen.getByRole("button", { name: "src" }));
    const rail = screen.getByTestId("source-diff-rail-w1");
    expect(() => rowIn(rail, "src/mod.ts")).toThrow();
    fireEvent.change(screen.getByTestId("source-diff-filter-w1"), { target: { value: "mod" } });
    expect(rowIn(rail, "src/mod.ts")).toBeTruthy();
    expect(screen.queryByText("No files match “mod”.")).not.toBeInTheDocument();
  });

  it("a folder click while filtering changes nothing once the filter clears", () => {
    render(overlay());
    fireEvent.keyDown(shell(), { key: "t" });
    fireEvent.change(screen.getByTestId("source-diff-filter-w1"), { target: { value: "s" } });
    fireEvent.click(screen.getByRole("button", { name: "src" }));
    fireEvent.change(screen.getByTestId("source-diff-filter-w1"), { target: { value: "" } });
    expect(screen.getByRole("button", { name: "src" })).toHaveAttribute("aria-expanded", "true");
  });

  it("Enter on a Tab-focused row opens that row, and arrows step from it", () => {
    useViewerStore.setState({ sourceDiffSelectedFile: { "sid-ws": "src/mod.ts" } });
    render(overlay());
    const rail = screen.getByTestId("source-diff-rail-w1");
    const row = rowIn(rail, "src/moved.ts");
    row.focus();
    fireEvent.keyDown(row, { key: "Enter" });
    expect(useViewerStore.getState().sourceDiffSelectedFile["sid-ws"]).toBe("src/moved.ts");
    useViewerStore.setState({ sourceDiffSelectedFile: { "sid-ws": "src/mod.ts" } });
    const focused = rowIn(rail, "src/new.ts");
    focused.focus();
    fireEvent.keyDown(focused, { key: "ArrowDown" });
    const order = [...rail.querySelectorAll<HTMLElement>("li[data-path]")].map(
      (li) => li.dataset.path,
    );
    expect(useViewerStore.getState().sourceDiffSelectedFile["sid-ws"]).toBe(
      order[order.indexOf("src/new.ts") + 1],
    );
  });

  it("e with nothing editable keeps the editor's remembered file", () => {
    stubChangeset({ files: [{ path: "src/gone.ts", status: "deleted" }] });
    useViewerStore.setState({
      sourceDiffSelectedFile: { "sid-ws": "src/gone.ts" },
      worktreeEditorFile: { w1: "src/remembered.ts" },
    });
    render(overlay());
    fireEvent.keyDown(shell(), { key: "e" });
    expect(useViewerStore.getState().worktreeEditorFile.w1).toBe("src/remembered.ts");
  });
});

describe("SourceDiffOverlay — review round 3", () => {
  beforeEach(() =>
    useViewerStore.setState({
      sourceDiffShownBySession: { "sid-ws": true },
      sourceDiffViewOptions: {},
    }),
  );
  const overlay = (ws: Workspace = storyWs, sid = "sid-ws") => (
    <SourceDiffOverlay workspace={ws} teamKey="team-a" activeTabSessionId={sid} isFocused />
  );
  const shell = () => screen.getByTestId("source-diff-overlay-expanded-w1");
  const selected = () => useViewerStore.getState().sourceDiffSelectedFile["sid-ws"];

  it("j/k from a focused row keep stepping, and Enter opens the row reached", () => {
    useViewerStore.setState({ sourceDiffSelectedFile: { "sid-ws": "src/mod.ts" } });
    render(overlay());
    const rail = screen.getByTestId("source-diff-rail-w1");
    const order = [...rail.querySelectorAll<HTMLElement>("li[data-path]")].map(
      (li) => li.dataset.path!,
    );
    rowIn(rail, order[0]).focus();
    fireEvent.keyDown(document.activeElement!, { key: "j" });
    fireEvent.keyDown(document.activeElement!, { key: "ArrowDown" });
    expect(selected()).toBe(order[2]);
    expect(document.activeElement).toBe(rowIn(rail, order[2]));
    fireEvent.keyDown(document.activeElement!, { key: "k" });
    expect(selected()).toBe(order[1]);
    fireEvent.keyDown(document.activeElement!, { key: "Enter" });
    expect(selected()).toBe(order[1]);
  });

  it("hiding by switching to a tab whose diff is closed leaves focus off the old terminal", async () => {
    const terminal = document.createElement("textarea");
    document.body.appendChild(terminal);
    terminal.focus();
    const { rerender } = render(overlay());
    await waitFor(() => expect(shell()).toHaveFocus());
    rerender(overlay(storyWs, "sid-other"));
    expect(screen.queryByTestId("source-diff-overlay-expanded-w1")).toBeNull();
    expect(terminal).not.toHaveFocus();
    terminal.remove();
  });

  it("n/p do nothing (and aren't swallowed) when the pane isn't a code diff", () => {
    useViewerStore.setState({ sourceDiffSelectedFile: { "sid-ws": "docs/readme.md" } });
    render(overlay());
    const n = new KeyboardEvent("keydown", { key: "n", bubbles: true, cancelable: true });
    shell().dispatchEvent(n);
    expect(n.defaultPrevented).toBe(false);
    expect(navigatorSpy.next).not.toHaveBeenCalled();
  });

  it("copying the path with no clipboard says so", async () => {
    const { toast } = await import("sonner");
    const error = vi.spyOn(toast, "error").mockImplementation(() => "");
    const original = Object.getOwnPropertyDescriptor(navigator, "clipboard");
    onTestFinished(() => {
      if (original) Object.defineProperty(navigator, "clipboard", original);
      else delete (navigator as { clipboard?: unknown }).clipboard;
    });
    Object.defineProperty(navigator, "clipboard", { value: undefined, configurable: true });
    render(overlay());
    fireEvent.keyDown(shell(), { key: "c" });
    expect(error).toHaveBeenCalledWith("Couldn't copy the path");
    error.mockRestore();
  });

  it("a remembered History choice never strands a machine's workspace", () => {
    const remoteWs: Workspace = {
      ...storyWs,
      cwd: "",
      location: {
        kind: "machine",
        machineId: "m-studio",
        requestId: "r1",
        baseRef: "main",
        branch: "feat/x",
        handle: "h1",
      },
    };
    useViewerStore.setState({ sourceDiffHistoryMode: { "sid-ws": true } });
    render(overlay(remoteWs));
    expect(screen.queryByTestId("source-diff-history-w1")).toBeNull();
    expect(shell()).toBeInTheDocument();
  });

  it("a route begun on A doesn't land after A → B → A with no new route", async () => {
    let settle!: (v: unknown) => void;
    routeMock.mockReturnValueOnce(new Promise((r) => (settle = r)));
    const wsA: Workspace = { ...storyWs, sourceRepoSlug: "acme/repo" };
    const wsB: Workspace = { ...wsA, id: "w2" };
    const routeBtn = () => document.querySelector("[data-route-to-context]") as HTMLElement | null;
    const { rerender } = render(overlay(wsA));
    fireEvent.click(routeBtn()!);
    useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } });
    rerender(overlay(wsB));
    rerender(overlay(wsA));
    await act(async () => {
      settle({ outcome: "routed" });
    });
    expect(screen.queryByText("Undo")).toBeNull();
    expect(screen.queryByText("routing…")).toBeNull();
  });

  it("Escape on a row's route control with the keyboard map open closes the map, not the tool", () => {
    const wsA: Workspace = { ...storyWs, sourceRepoSlug: "acme/repo" };
    render(overlay(wsA));
    fireEvent.keyDown(shell(), { key: "?" });
    expect(screen.getByRole("region", { name: "Keyboard shortcuts" })).toBeInTheDocument();
    const btn = document.querySelector("[data-route-to-context]") as HTMLElement;
    fireEvent.keyDown(btn, { key: "Escape" });
    expect(screen.queryByRole("region", { name: "Keyboard shortcuts" })).not.toBeInTheDocument();
    expect(useViewerStore.getState().sourceDiffShownBySession["sid-ws"]).toBe(true);
  });

  it("an older route settling late doesn't overwrite a newer one's state", async () => {
    let settleFirst!: (v: unknown) => void;
    routeMock
      .mockReturnValueOnce(new Promise((r) => (settleFirst = r)))
      .mockReturnValueOnce(new Promise(() => undefined));
    const wsA: Workspace = { ...storyWs, sourceRepoSlug: "acme/repo" };
    const wsB: Workspace = { ...wsA, id: "w2" };
    const routeBtn = () => document.querySelector("[data-route-to-context]") as HTMLElement | null;
    const { rerender } = render(overlay(wsA));
    fireEvent.click(routeBtn()!);
    // Away and back while it runs: the row starts clean and is routed again.
    useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } });
    rerender(overlay(wsB));
    rerender(overlay(wsA));
    fireEvent.click(routeBtn()!);
    expect(await screen.findByText("routing…")).toBeInTheDocument();
    await act(async () => {
      settleFirst({ outcome: "routed" });
    });
    expect(screen.queryByText("Undo")).toBeNull();
    expect(screen.getByText("routing…")).toBeInTheDocument();
  });
});

// ────────────────────────────────────────────────────────────────────────────
// SW-VE-12 — source editor + diff visible from every tab. Covers the new body
// state matrix: a non-git cwd shows "No git repo at this path." instead of
// hiding the overlay. Loading / No-changes / Load-failed states are owned by
// the rail (its own suite covers them); the overlay-level body state we added
// is the one the rail CANNOT express (there's no working tree to list).
// ────────────────────────────────────────────────────────────────────────────

describe("SourceDiffOverlay — SW-VE-12 always-on empty-state matrix", () => {
  beforeEach(() => useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } }));

  it("AC-2: cwd resolved to a non-git dir renders the 'No git repo at this path.' body", () => {
    const generic: Workspace = { ...storyWs, id: "gw", kind: "generic", cwd: "/home/user" };
    tabSourceRepoMock.mockReturnValue({
      folder: { kind: "folder", cwd: "/home/user" },
      repo: null,
    });
    render(
      <SourceDiffOverlay
        workspace={generic}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // The overlay chrome mounts (we no longer hide on non-story kinds).
    // R3-N1: no-repo branch carries a dedicated testid so "overlay is open
    // AND populated" assertions never false-green on a non-git cwd.
    expect(screen.getByTestId("source-diff-overlay-no-repo-gw")).toBeInTheDocument();
    expect(screen.queryByTestId("source-diff-overlay-expanded-gw")).not.toBeInTheDocument();
    // The new body state owns the "no working tree" message.
    expect(screen.getByTestId("source-diff-no-repo")).toHaveTextContent(/no git repo/i);
    // And when there's no repo the rail is not rendered — a 0-row rail next to
    // the message would double-signal (impeccable finding 1.1).
    expect(screen.queryByTestId("source-diff-rail-gw")).not.toBeInTheDocument();
    // No seam call either — effectiveActive stays false, so no spurious fetch.
    expect(changesetMock).toHaveBeenCalledWith("/home/user", undefined, false, 0);
  });

  it("AC-1 / AC-6: a `generic` cwd that IS a worktree gets the full rail + diff pane", () => {
    const generic: Workspace = { ...storyWs, id: "gw", kind: "generic", cwd: "/wt/plain" };
    tabSourceRepoMock.mockReturnValue({
      folder: { kind: "folder", cwd: "/wt/plain" },
      repo: { root: "/wt/plain", isRoot: true },
    });
    render(
      <SourceDiffOverlay
        workspace={generic}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // The rail and pane render identically to a `story` tile (AC-6 parity).
    expect(screen.getByTestId("source-diff-overlay-expanded-gw")).toBeInTheDocument();
    expect(screen.getByTestId("source-diff-rail-gw")).toBeInTheDocument();
    expect(screen.queryByTestId("source-diff-no-repo")).not.toBeInTheDocument();
  });

  it("AC-7: the `story` kind still gets its rail (no regression)", () => {
    tabSourceRepoMock.mockReturnValue({
      folder: { kind: "folder", cwd: "/wt/story" },
      repo: { root: "/wt/story", isRoot: true },
    });
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("source-diff-rail-w1")).toBeInTheDocument();
    expect(screen.queryByTestId("source-diff-no-repo")).not.toBeInTheDocument();
  });

  it("tri-state `repo: undefined` (resolver in flight) keeps the rail — the rail owns loading", () => {
    const generic: Workspace = { ...storyWs, id: "gw", kind: "generic", cwd: "/wt/unknown" };
    tabSourceRepoMock.mockReturnValue({
      folder: { kind: "folder", cwd: "/wt/unknown" },
      // undefined = still resolving. We don't jump straight to "No git repo"
      // — the rail's own skeleton/loading surface handles the in-flight read.
      repo: undefined,
    });
    // Changeset stays loading so the rail's `loading && files == null` skeleton
    // branch is exercised — this is the state we're asserting on.
    stubChangeset({ files: null, loading: false, error: null });
    render(
      <SourceDiffOverlay
        workspace={generic}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.queryByTestId("source-diff-no-repo")).not.toBeInTheDocument();
    // The overlay chrome still mounts (its header stays stable through state transitions).
    expect(screen.getByTestId("source-diff-overlay-expanded-gw")).toBeInTheDocument();
    // The rail renders — AND it reads as loading (via the overlay's merged
    // `loading || tabRepo === undefined` prop) so we never flash an empty
    // file list under a still-resolving probe.
    expect(screen.getByTestId("source-diff-rail-gw")).toBeInTheDocument();
    expect(screen.getByTestId("source-diff-rail-gw").textContent).not.toMatch(/no source changes/i);
  });

  // N3a: the no-repo body carries its own Close button that routes to the
  // shared `toggleShown` action.
  it("N3: no-repo body Close button dismisses the overlay", () => {
    const generic: Workspace = { ...storyWs, id: "gw", kind: "generic", cwd: "/home/user" };
    tabSourceRepoMock.mockReturnValue({
      folder: { kind: "folder", cwd: "/home/user" },
      repo: null,
    });
    render(
      <SourceDiffOverlay
        workspace={generic}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.click(screen.getByTestId("source-diff-close-gw"));
    expect(useViewerStore.getState().sourceDiffShownBySession["sid-ws"]).toBe(false);
  });

  // N3b: Escape dismisses the overlay from the no-repo body — the shared
  // `useOverlayEscape` listener runs upstream of the body branch, so the
  // no-repo state keeps the same Escape semantics as a populated diff.
  it("N3: Escape dismisses the overlay from the no-repo body state", () => {
    const generic: Workspace = { ...storyWs, id: "gw", kind: "generic", cwd: "/home/user" };
    tabSourceRepoMock.mockReturnValue({
      folder: { kind: "folder", cwd: "/home/user" },
      repo: null,
    });
    render(
      <SourceDiffOverlay
        workspace={generic}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.keyDown(document, { key: "Escape" });
    expect(useViewerStore.getState().sourceDiffShownBySession["sid-ws"]).toBe(false);
  });

  it("N4: the no-repo message carries role=status so screen readers announce transitions", () => {
    const generic: Workspace = { ...storyWs, id: "gw", kind: "generic", cwd: "/home/user" };
    tabSourceRepoMock.mockReturnValue({
      folder: { kind: "folder", cwd: "/home/user" },
      repo: null,
    });
    render(
      <SourceDiffOverlay
        workspace={generic}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("source-diff-no-repo")).toHaveAttribute("role", "status");
  });

  // R3-M4 convergence: the no-repo branch wraps `onKeyDown` to forward only
  // Escape + `e` into the shared dispatcher. Other keys (?, /, j/k, n/p,
  // t/s/w/b/c/r) have no target on this branch — no filter input, no rail
  // rows, no KeyboardMap — so the wrapper must swallow them. Verified via
  // the Escape path which DOES still fire `toggleShown`, and a sampling of
  // the swallowed keys which must NOT change store state.
  it("R3-M4: no-repo onKeyDown forwards Escape + e; swallows ?, /, j, k, n, p, t, s, w, b, c, r", () => {
    const generic: Workspace = { ...storyWs, id: "gw", kind: "generic", cwd: "/home/user" };
    tabSourceRepoMock.mockReturnValue({
      folder: { kind: "folder", cwd: "/home/user" },
      repo: null,
    });
    render(
      <SourceDiffOverlay
        workspace={generic}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    const shell = screen.getByTestId("source-diff-overlay-no-repo-gw");
    // Swallow sampling — fire each key and confirm the overlay still open.
    for (const key of ["?", "/", "j", "k", "n", "p", "t", "s", "w", "b", "c", "r"]) {
      fireEvent.keyDown(shell, { key });
    }
    // Not dismissed by any of them.
    expect(useViewerStore.getState().sourceDiffShownBySession["sid-ws"]).toBe(true);
    // KeyboardMap would mount if `?` were routed through — never does.
    expect(screen.queryByRole("region", { name: "Keyboard shortcuts" })).not.toBeInTheDocument();
  });

  // SW-VE-14: a terminal `cd` from a git worktree to a non-git dir retargets
  // both the resolver (populated → "No git repo") and the local fetch key
  // (ws.cwd → the new cwd). Drives the real seam: the revision counter is
  // the hook's parameter, so bumping it bumps `useSessionCwd`'s read, which
  // this overlay then threads through `effectiveCwd` into the resolver + the
  // changeset key. Covers AC-1 of the story.
  it("SW-VE-14: a `cd` bumps the revision counter and retargets the resolver + fetch key", () => {
    const generic: Workspace = { ...storyWs, id: "gw", kind: "generic", cwd: "/wt/plain" };
    useViewerStore.setState({ sourceDiffShownBySession: { "sid-ws": true } });
    // The live cwd override follows the revision: 0 reads as "/wt/plain",
    // any bump reads as "/home/user" (the mocked `cd` destination).
    sessionCwdMock.mockImplementation((_sid, rev) => (rev > 0 ? "/home/user" : "/wt/plain"));
    // The resolver mirrors it: the pre-cd cwd is a git worktree, the post-cd
    // cwd is not. The overlay's resolver read passes `liveCwd` through so
    // the mock keys on it exactly like the real hook.
    tabSourceRepoMock.mockImplementation((_ws, liveCwd) =>
      liveCwd === "/home/user"
        ? { folder: { kind: "folder", cwd: "/home/user" }, repo: null }
        : {
            folder: { kind: "folder", cwd: "/wt/plain" },
            repo: { root: "/wt/plain", isRoot: true },
          },
    );
    const { rerender } = render(
      <SourceDiffOverlay
        workspace={generic}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // Pre-cd: the populated chrome is up (the worktree is a repo), the
    // changeset read keys on the live cwd `/wt/plain` (not the stale ws.cwd
    // which happens to match today but needn't).
    expect(screen.getByTestId("source-diff-overlay-expanded-gw")).toBeInTheDocument();
    expect(screen.queryByTestId("source-diff-no-repo")).not.toBeInTheDocument();
    expect(changesetMock).toHaveBeenCalledWith("/wt/plain", undefined, true, 0);

    // Post-cd: bump the OSC 7 revision counter — the overlay re-reads cwd
    // (now `/home/user`), the resolver drops to null, and the body swaps
    // to the "No git repo at this path." state.
    sessionCwdRevisionMock.mockReturnValue(1);
    rerender(
      <SourceDiffOverlay
        workspace={generic}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.getByTestId("source-diff-overlay-no-repo-gw")).toBeInTheDocument();
    expect(screen.queryByTestId("source-diff-overlay-expanded-gw")).not.toBeInTheDocument();
    expect(screen.getByTestId("source-diff-no-repo")).toHaveTextContent(/no git repo/i);
    // The resolver was asked with the new cwd (seam: `useTabSourceRepo` is
    // keyed on `liveCwd`, so a cwd mutation changes the resolver's input).
    expect(tabSourceRepoMock).toHaveBeenCalledWith(expect.anything(), "/home/user");
  });
});
