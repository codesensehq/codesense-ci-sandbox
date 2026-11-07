// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import type { ReactNode } from "react";

import { useViewerStore } from "@/shared/stores/viewer-store";
import type { SourceCommitGraphEntry } from "@/features/git-sync/lib/types";
import type { CommitGraphLayout } from "@/shared/lib/commit-graph/layout";

const graphMock = vi.fn();
vi.mock("../lib/use-source-diff", () => ({
  useSourceCommitGraph: (...a: unknown[]) => graphMock(...a),
}));

// Sync layout so the rail renders deterministically (the async hook + worker are
// covered in their own suites).
const layoutMock = vi.fn();
vi.mock("@/shared/lib/commit-graph/use-async-graph-layout", () => ({
  useAsyncGraphLayout: (...a: unknown[]) => layoutMock(...a),
}));

// Render EVERY item (bypass @tanstack/react-virtual — happy-dom has no layout, so
// a real virtualizer would mount zero rows). We only test the item construction +
// selection wiring here; virtualization is a VirtualList concern.
vi.mock("@/shared/components/virtual-list", () => ({
  VirtualList: ({
    items,
    renderItem,
  }: {
    items: unknown[];
    renderItem: (item: unknown, vr: unknown) => ReactNode;
  }) => (
    <div>
      {items.map((it, i) => (
        <div key={i}>{renderItem(it, { index: i })}</div>
      ))}
    </div>
  ),
}));

import { CommitGraphRail } from "./commit-graph-rail";

const entry = (sha: string, refs: SourceCommitGraphEntry["refs"] = []): SourceCommitGraphEntry => ({
  sha,
  shortSha: sha.slice(0, 7),
  parents: [],
  refs,
  subject: `subject ${sha}`,
  author: "t",
  committedAt: 0,
});

function stubGraph(over: Partial<ReturnType<typeof defaultGraph>> = {}) {
  graphMock.mockReturnValue({ ...defaultGraph(), ...over });
}
function defaultGraph() {
  return {
    entries: [] as SourceCommitGraphEntry[],
    loading: false,
    loadingMore: false,
    error: null as string | null,
    hasMore: false,
    loadMore: vi.fn(),
  };
}
function layout(entries: SourceCommitGraphEntry[]): CommitGraphLayout {
  return {
    rows: entries.map((e) => ({ sha: e.sha, lane: 0, parentLanes: [], incoming: [e.sha] })),
    width: 1,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  layoutMock.mockReturnValue({
    layout: { rows: [], width: 0 },
    layoutPending: false,
    layoutFailed: false,
    layoutRefreshing: false,
    retryLayout: vi.fn(),
  });
  useViewerStore.setState({ sourceDiffSelectedSha: {} });
});

const ws = { id: "w1", cwd: "/wt" };

describe("CommitGraphRail", () => {
  it("always renders a pinned Working-tree node", () => {
    stubGraph();
    render(<CommitGraphRail ws={ws} worktreeNonce={0} activeTabSessionId="sid-a" />);
    expect(screen.getByTestId("commit-row-working")).toBeInTheDocument();
  });

  it("AC-10: an empty/unborn repo shows ONLY the Working node (no commit rows)", () => {
    stubGraph({ entries: [] });
    render(<CommitGraphRail ws={ws} worktreeNonce={0} activeTabSessionId="sid-a" />);
    expect(screen.getByTestId("commit-row-working")).toBeInTheDocument();
    expect(screen.queryByTestId(/^commit-row-(?!working)/)).not.toBeInTheDocument();
  });

  it("renders a row per commit with its subject", () => {
    const entries = [entry("aaaaaaa1"), entry("bbbbbbb2")];
    stubGraph({ entries });
    layoutMock.mockReturnValue({
      layout: layout(entries),
      layoutPending: false,
      layoutFailed: false,
      layoutRefreshing: false,
      retryLayout: vi.fn(),
    });
    render(<CommitGraphRail ws={ws} worktreeNonce={0} activeTabSessionId="sid-a" />);
    expect(screen.getByTestId("commit-row-aaaaaaa1")).toBeInTheDocument();
    expect(screen.getByText("subject bbbbbbb2")).toBeInTheDocument();
  });

  it("marks the HEAD commit (from the synthetic head ref)", () => {
    const entries = [entry("headsha", [{ name: "HEAD", kind: "head" }])];
    stubGraph({ entries });
    layoutMock.mockReturnValue({
      layout: layout(entries),
      layoutPending: false,
      layoutFailed: false,
      layoutRefreshing: false,
      retryLayout: vi.fn(),
    });
    render(<CommitGraphRail ws={ws} worktreeNonce={0} activeTabSessionId="sid-a" />);
    // The HEAD ref pill renders.
    expect(screen.getByText("HEAD")).toBeInTheDocument();
  });

  it("selecting a commit sets the selected sha; the Working node clears it", () => {
    const entries = [entry("commit1")];
    stubGraph({ entries });
    layoutMock.mockReturnValue({
      layout: layout(entries),
      layoutPending: false,
      layoutFailed: false,
      layoutRefreshing: false,
      retryLayout: vi.fn(),
    });
    render(<CommitGraphRail ws={ws} worktreeNonce={0} activeTabSessionId="sid-a" />);
    fireEvent.click(screen.getByTestId("commit-row-commit1"));
    // SW-TP CAP-3: sourceDiffSelectedSha is keyed by sessionId, not workspaceId.
    expect(useViewerStore.getState().sourceDiffSelectedSha["sid-a"]).toBe("commit1");
    fireEvent.click(screen.getByTestId("commit-row-working"));
    expect(useViewerStore.getState().sourceDiffSelectedSha["sid-a"]).toBeNull();
  });

  it("shows a skeleton while the first page loads", () => {
    stubGraph({ loading: true });
    render(<CommitGraphRail ws={ws} worktreeNonce={0} activeTabSessionId="sid-a" />);
    expect(screen.getByTestId("commit-graph-skeleton")).toBeInTheDocument();
  });

  it("surfaces a COLD (first-page) error", () => {
    stubGraph({ error: "walk failed" }); // no entries → cold error
    render(<CommitGraphRail ws={ws} worktreeNonce={0} activeTabSessionId="sid-a" />);
    expect(screen.getByTestId("commit-graph-error")).toBeInTheDocument();
  });

  it("a load-older (page-N) failure keeps the loaded graph — never blanks (AC-10, review r1)", () => {
    const entries = [entry("c1")];
    // error IS set, but entries are already loaded (a page-2 failure).
    stubGraph({ entries, hasMore: true, error: "older page failed" });
    layoutMock.mockReturnValue({
      layout: layout(entries),
      layoutPending: false,
      layoutFailed: false,
      layoutRefreshing: false,
      retryLayout: vi.fn(),
    });
    render(<CommitGraphRail ws={ws} worktreeNonce={0} activeTabSessionId="sid-a" />);
    // The full-rail error view must NOT show — the graph stays.
    expect(screen.queryByTestId("commit-graph-error")).not.toBeInTheDocument();
    expect(screen.getByTestId("commit-row-c1")).toBeInTheDocument();
  });

  it("the load-more sentinel fires loadMore when more pages exist (AC-7)", () => {
    const loadMore = vi.fn();
    stubGraph({ entries: [entry("c1")], hasMore: true, loadMore });
    layoutMock.mockReturnValue({
      layout: layout([entry("c1")]),
      layoutPending: false,
      layoutFailed: false,
      layoutRefreshing: false,
      retryLayout: vi.fn(),
    });
    render(<CommitGraphRail ws={ws} worktreeNonce={0} activeTabSessionId="sid-a" />);
    expect(loadMore).toHaveBeenCalled();
  });
});
