// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@/test-utils";

import { PipelineStatusNav } from "./pipeline-status-nav";
import { buildStoryRef, buildTree } from "@/shared/lib/pipeline-graph/test-fixtures/derived-tree";

// Track each StoryRow render by replacing StageRail with a
// minimal counter. Each StoryRow renders the rail exactly once, so the
// counter == number of StoryRow renders. The other row components
// (FolderHeaderRow, DocRow, …) don't render StageRail, so this
// signal isolates StoryRow's memo behavior — which is the load-bearing
// row component for NB-4 memoization and NB-7 AC-1.
let storyRowRenderCount = 0;
vi.mock("@/shared/components/stage-rail", () => ({
  StageRail: () => {
    storyRowRenderCount++;
    return null;
  },
  StageRailShape: () => null,
}));

// RF-12 AC-3: drive the per-row status popover without Radix/portal machinery.
// The mock still renders `children` (the counted StageRail) and exposes a
// per-row toggle keyed by the row's status, so a test can open ONE row's popover
// and assert the keyed `statusPopoverOpenMap` flip re-renders only that row.
vi.mock("@/shared/components/status-selector-popover", () => ({
  StatusSelectorPopover: ({
    children,
    currentStatus,
    open,
    onOpenChange,
  }: {
    children: React.ReactNode;
    currentStatus: string;
    open: boolean;
    onOpenChange: (open: boolean) => void;
  }) => (
    <>
      <button data-testid={`popover-toggle-${currentStatus}`} onClick={() => onOpenChange(!open)} />
      {children}
    </>
  ),
}));

const mockToggleStatusNavExpanded = vi.fn();
vi.mock("@/shared/stores/viewer-store", () => ({
  useViewerStore: Object.assign(
    (selector: (s: Record<string, unknown>) => unknown) =>
      selector({
        navLens: {},
        statusNavExpandedPaths: {},
        revealCounter: {},
        toggleStatusNavExpanded: mockToggleStatusNavExpanded,
      }),
    {
      subscribe: () => () => {},
      getState: () => ({
        toggleStatusNavExpanded: mockToggleStatusNavExpanded,
        readViewState: () => undefined,
        captureViewState: () => {},
      }),
    },
  ),
}));

function makeStory(name: string, status: string | null = "todo") {
  return buildStoryRef(name, { absolutePath: `/repo/${name}`, status });
}

describe("PipelineStatusNav — memo regression (NB-7 AC-1)", () => {
  beforeEach(() => {
    storyRowRenderCount = 0;
  });

  it("StoryRow memo skips re-renders when parent props are reference-stable", () => {
    const stories = Array.from({ length: 8 }, (_, i) => makeStory(`S-${i + 1}-story.md`, "todo"));
    const tree = buildTree("", { absoluteDirPath: "/repo", stories });

    // Stable callbacks — simulates the post-NB-7 consumer (useCallback wraps
    // in repo-viewer-view.tsx). These references stay identical across the
    // two render() / rerender() calls below.
    const handlers = {
      onFileSelect: vi.fn(),
      onDrillPathChange: vi.fn(),
      onStatusChange: vi.fn(),
    };

    const { rerender } = render(
      <PipelineStatusNav tree={tree} repoPath="/repo" selectedPath={null} {...handlers} />,
    );

    const initialRenders = storyRowRenderCount;
    expect(initialRenders).toBe(8); // one per story on mount

    // Re-render with IDENTICAL prop references — no callback churn, no
    // tree replacement. With NB-7 in place, every StoryRow's memo wrap
    // should short-circuit.
    rerender(<PipelineStatusNav tree={tree} repoPath="/repo" selectedPath={null} {...handlers} />);

    expect(storyRowRenderCount).toBe(initialRenders);
  });

  it("StoryRow re-renders only the row whose selection changed", () => {
    const stories = Array.from({ length: 5 }, (_, i) => makeStory(`S-${i + 1}-story.md`, "todo"));
    const tree = buildTree("", { absoluteDirPath: "/repo", stories });

    const handlers = {
      onFileSelect: vi.fn(),
      onDrillPathChange: vi.fn(),
      onStatusChange: vi.fn(),
    };

    const { rerender } = render(
      <PipelineStatusNav tree={tree} repoPath="/repo" selectedPath={null} {...handlers} />,
    );

    const initialRenders = storyRowRenderCount;
    expect(initialRenders).toBe(5);

    // Move selection to row 3. With memo, only that row's isSelected
    // prop changed (false → true); the other 4 rows' props are identical.
    // Memo should let 4 rows skip and re-render only 1.
    rerender(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath="/repo/S-3-story.md"
        {...handlers}
      />,
    );

    // Exactly one additional render (the newly-selected row).
    expect(storyRowRenderCount).toBe(initialRenders + 1);
  });

  // RF-12 AC-3: after the decomposition (rows + interaction model relocated),
  // toggling one story's status popover must still re-render ONLY that row. The
  // `statusPopoverOpenMap` identity flips on toggle → `rowCallbacks` re-memos →
  // `renderRow` re-runs for every row, but each StoryRow's memo compares props
  // and only the toggled row's `isPopoverOpen` (false → true) changed.
  it("toggling a story's status popover re-renders only that row", () => {
    // Distinct statuses so each row's popover toggle is individually targetable.
    const stories = [
      makeStory("S-1-story.md", "todo"),
      makeStory("S-2-story.md", "done"),
      makeStory("S-3-story.md", "review"),
    ];
    const tree = buildTree("", { absoluteDirPath: "/repo", stories });

    const handlers = {
      onFileSelect: vi.fn(),
      onDrillPathChange: vi.fn(),
      onStatusChange: vi.fn(),
    };

    render(<PipelineStatusNav tree={tree} repoPath="/repo" selectedPath={null} {...handlers} />);

    const initialRenders = storyRowRenderCount;
    expect(initialRenders).toBe(3);

    // Open the "done" row's popover. Only that row's isPopoverOpen prop changes.
    fireEvent.click(screen.getByTestId("popover-toggle-done"));

    // Exactly one additional render (the toggled row) — sibling rows stay memoized.
    expect(storyRowRenderCount).toBe(initialRenders + 1);
  });
});
