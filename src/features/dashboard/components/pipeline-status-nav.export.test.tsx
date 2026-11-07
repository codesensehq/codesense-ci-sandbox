// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { PipelineStatusNav } from "./pipeline-status-nav";
import type { DerivedPipelineTree, MainDoc } from "@/shared/lib/pipeline-graph/types";
import { buildTree } from "@/shared/lib/pipeline-graph/test-fixtures/derived-tree";

// -- store / team-key (mirror pipeline-status-nav.attach.test.tsx) ----------

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

const listDirectory = vi.fn<() => Promise<unknown[]>>().mockResolvedValue([]);
vi.mock("@/tauri/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/tauri/commands")>();
  return { ...actual, listDirectory: () => listDirectory() };
});

vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn() }) }));

// The export handler is the SC-13 boundary under test for click-wiring; the
// engine/dialog/file IO it performs is unit-tested in export-canvas.test.ts.
const exportCanvas = vi.fn<(path: string, format: "png" | "svg") => Promise<void>>();
vi.mock("@/features/dashboard/lib/export-canvas", () => ({
  exportCanvas: (path: string, format: "png" | "svg") => exportCanvas(path, format),
}));

// -- fixtures ---------------------------------------------------------------

const CANVAS = "/repo/design.excalidraw";

const mainDoc = (filePath: string): MainDoc => ({
  filePath,
  supportingDocs: null,
  purposeText: null,
});

function treeWithDocs(docs: { fileName: string; filePath: string }[]): DerivedPipelineTree {
  return buildTree("epic", {
    absoluteDirPath: "/repo",
    mainDoc: mainDoc("/repo/README.md"),
    stories: [],
    docs,
  });
}

function renderNav(tree: DerivedPipelineTree) {
  return render(
    <PipelineStatusNav tree={tree} repoPath="/repo" selectedPath={null} onFileSelect={vi.fn()} />,
  );
}

function rightClick(container: HTMLElement, filePath: string) {
  const row = container.querySelector(`[data-filepath="${filePath}"]`);
  expect(row).not.toBeNull();
  fireEvent.contextMenu(row!);
}

beforeEach(() => {
  vi.clearAllMocks();
  listDirectory.mockResolvedValue([]);
});

describe("PipelineStatusNav — Export as PNG/SVG (SW-SC-13)", () => {
  it("AC-1/AC-2: shows both export items on a .excalidraw node", () => {
    const { container } = renderNav(
      treeWithDocs([{ fileName: "design.excalidraw", filePath: CANVAS }]),
    );
    rightClick(container, CANVAS);
    expect(screen.getByText("Export as PNG")).toBeInTheDocument();
    expect(screen.getByText("Export as SVG")).toBeInTheDocument();
  });

  it("AC-9: hides the items on a plain doc node", () => {
    const { container } = renderNav(
      treeWithDocs([{ fileName: "notes.md", filePath: "/repo/notes.md" }]),
    );
    rightClick(container, "/repo/notes.md");
    expect(screen.queryByText("Export as PNG")).not.toBeInTheDocument();
    expect(screen.queryByText("Export as SVG")).not.toBeInTheDocument();
  });

  it("AC-9: hides the items on a *.excalidraw.png derived render", () => {
    const { container } = renderNav(
      treeWithDocs([
        { fileName: "design.excalidraw.png", filePath: "/repo/design.excalidraw.png" },
      ]),
    );
    rightClick(container, "/repo/design.excalidraw.png");
    expect(screen.queryByText("Export as PNG")).not.toBeInTheDocument();
    expect(screen.queryByText("Export as SVG")).not.toBeInTheDocument();
  });

  it("AC-1: clicking Export as PNG calls the handler with the canvas path + format", () => {
    const { container } = renderNav(
      treeWithDocs([{ fileName: "design.excalidraw", filePath: CANVAS }]),
    );
    rightClick(container, CANVAS);
    fireEvent.click(screen.getByText("Export as PNG").closest("button")!);
    expect(exportCanvas).toHaveBeenCalledWith(CANVAS, "png");
  });

  it("AC-2: clicking Export as SVG calls the handler with the svg format", () => {
    const { container } = renderNav(
      treeWithDocs([{ fileName: "design.excalidraw", filePath: CANVAS }]),
    );
    rightClick(container, CANVAS);
    fireEvent.click(screen.getByText("Export as SVG").closest("button")!);
    expect(exportCanvas).toHaveBeenCalledWith(CANVAS, "svg");
  });
});
