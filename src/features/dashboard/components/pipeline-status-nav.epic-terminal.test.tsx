// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { PipelineStatusNav } from "./pipeline-status-nav";
import type { MainDoc } from "@/shared/lib/pipeline-graph/types";
import { buildTree } from "@/shared/lib/pipeline-graph/test-fixtures/derived-tree";

const mockToggleStatusNavExpanded = vi.fn();
const mockStatusNavExpandedPaths: Record<string, string[]> = {};

vi.mock("@/shared/lib/team-key", () => ({
  useActiveTeamKey: () => "/repo",
  getActiveTeamKey: () => "/repo",
  buildTeamKey: (orgSlug: string, teamName: string | null) => `${orgSlug}:${teamName ?? "_none"}`,
  NO_TEAM_SENTINEL: "_no_team:_none",
}));

vi.mock("@/shared/stores/viewer-store", () => ({
  useViewerStore: Object.assign(
    (selector: (s: Record<string, unknown>) => unknown) =>
      selector({
        navLens: {},
        statusNavExpandedPaths: mockStatusNavExpandedPaths,
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

vi.mock("@/tauri/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/tauri/commands")>();
  return { ...actual, listDirectory: () => Promise.resolve([]) };
});

vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn() }) }));

const mainDoc = (filePath: string): MainDoc => ({
  filePath,
  supportingDocs: null,
  purposeText: null,
});

beforeEach(() => vi.clearAllMocks());

describe("PipelineStatusNav — Open epic workspace (SW-EW-6)", () => {
  const DOC = "/r/in-progress/epic-workspaces/README.md";

  function renderWithEpicFolder(onOpenEpicTerminal?: (n: string, f: string) => void) {
    // Root holds an epic sub-folder "epic-workspaces" (mainDoc → isEpic).
    const tree = buildTree("root", {
      subEpics: [
        buildTree("epic-workspaces", {
          dirPath: "in-progress/epic-workspaces",
          mainDoc: mainDoc(DOC),
        }),
      ],
    });
    return render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={vi.fn()}
        onOpenEpicTerminal={onOpenEpicTerminal}
      />,
    );
  }

  it("shows the item on an epic-folder right-click and fires with (dirName, epic doc path)", () => {
    const onOpenEpicTerminal = vi.fn();
    renderWithEpicFolder(onOpenEpicTerminal);
    const folderBtn = screen.getByText("epic-workspaces").closest("button")!;
    fireEvent.contextMenu(folderBtn);
    fireEvent.click(screen.getByText("Open epic workspace"));
    expect(onOpenEpicTerminal).toHaveBeenCalledWith("epic-workspaces", DOC);
  });

  it("does not render the item when no handler is wired", () => {
    renderWithEpicFolder(undefined);
    const folderBtn = screen.getByText("epic-workspaces").closest("button")!;
    fireEvent.contextMenu(folderBtn);
    expect(screen.queryByText("Open epic workspace")).toBeNull();
  });

  it("does NOT show the item on a non-epic folder (no mainDoc)", () => {
    const onOpenEpicTerminal = vi.fn();
    const tree = buildTree("root", {
      subEpics: [buildTree("just-a-dir", { dirPath: "in-progress/just-a-dir" })],
    });
    render(
      <PipelineStatusNav
        tree={tree}
        repoPath="/repo"
        selectedPath={null}
        onFileSelect={vi.fn()}
        onOpenEpicTerminal={onOpenEpicTerminal}
      />,
    );
    const folderBtn = screen.getByText("just-a-dir").closest("button")!;
    fireEvent.contextMenu(folderBtn);
    expect(screen.queryByText("Open epic workspace")).toBeNull();
  });
});
