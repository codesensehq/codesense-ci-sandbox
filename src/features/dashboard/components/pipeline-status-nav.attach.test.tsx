// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@/test-utils";
import { PipelineStatusNav } from "./pipeline-status-nav";
import type { DerivedPipelineTree, MainDoc, StoryRef } from "@/shared/lib/pipeline-graph/types";
import { buildStoryRef, buildTree } from "@/shared/lib/pipeline-graph/test-fixtures/derived-tree";

// -- store / team-key (mirror pipeline-status-nav.test.tsx) -----------------

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

// -- tauri commands + toast (the attach write path) -------------------------

const readFile = vi.fn<(p: string) => Promise<string>>();
const writeContentFile = vi.fn<(p: string, c: string) => Promise<void>>().mockResolvedValue();
const listDirectory = vi.fn<() => Promise<unknown[]>>().mockResolvedValue([]);
vi.mock("@/tauri/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/tauri/commands")>();
  return {
    ...actual,
    readFile: (p: string) => readFile(p),
    writeContentFile: (p: string, c: string) => writeContentFile(p, c),
    listDirectory: () => listDirectory(),
  };
});

vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn() }) }));

// -- fixtures ---------------------------------------------------------------

const CANVAS = "/repo/design.excalidraw";
const STORY_MD = `---\ntype: story\nstory_id: SW-SC-12\n---\n\n# Story\n`;

const mainDoc = (filePath: string): MainDoc => ({
  filePath,
  supportingDocs: null,
  purposeText: null,
});

/** An epic root (mainDoc set) carrying the given stories plus the canvas as a
 *  supporting doc, so findEnclosingEpic resolves the canvas → this epic. */
function epicWithCanvas(
  stories: StoryRef[],
  canvasName = "design.excalidraw",
): DerivedPipelineTree {
  return buildTree("epic", {
    absoluteDirPath: "/repo",
    mainDoc: mainDoc("/repo/README.md"),
    stories,
    docs: [{ fileName: canvasName, filePath: `/repo/${canvasName}` }],
  });
}

function story(fileName: string) {
  return buildStoryRef(fileName, { absolutePath: `/repo/${fileName}`, status: "todo" });
}

function renderNav(tree: DerivedPipelineTree) {
  return render(
    <PipelineStatusNav tree={tree} repoPath="/repo" selectedPath={null} onFileSelect={vi.fn()} />,
  );
}

/** Right-click the nav row for `filePath` to open its context menu. */
function rightClick(container: HTMLElement, filePath: string) {
  const row = container.querySelector(`[data-filepath="${filePath}"]`);
  expect(row).not.toBeNull();
  fireEvent.contextMenu(row!);
}

beforeEach(() => {
  vi.clearAllMocks();
  readFile.mockReset();
  writeContentFile.mockResolvedValue();
  listDirectory.mockResolvedValue([]);
});

describe("PipelineStatusNav — Attach to story… (SW-SC-12)", () => {
  it("shows the item on a .excalidraw node only (AC-1)", () => {
    const tree = buildTree("epic", {
      absoluteDirPath: "/repo",
      mainDoc: mainDoc("/repo/README.md"),
      stories: [story("SW-SC-12.md")],
      docs: [
        { fileName: "design.excalidraw", filePath: CANVAS },
        { fileName: "notes.md", filePath: "/repo/notes.md" },
      ],
    });
    const { container } = renderNav(tree);

    // Canvas node → item present.
    rightClick(container, CANVAS);
    expect(screen.getByText(/Attach to story/)).toBeInTheDocument();

    // Plain doc node → item absent (Copy path still there).
    rightClick(container, "/repo/notes.md");
    expect(screen.queryByText(/Attach to story/)).not.toBeInTheDocument();
    expect(screen.getByText("Copy path")).toBeInTheDocument();
  });

  it("does not show the item on a *.excalidraw.png derived render (AC-1)", () => {
    const tree = buildTree("epic", {
      absoluteDirPath: "/repo",
      mainDoc: mainDoc("/repo/README.md"),
      stories: [story("SW-SC-12.md")],
      docs: [{ fileName: "design.excalidraw.png", filePath: "/repo/design.excalidraw.png" }],
    });
    const { container } = renderNav(tree);

    rightClick(container, "/repo/design.excalidraw.png");
    expect(screen.queryByText(/Attach to story/)).not.toBeInTheDocument();
  });

  it("disables the item when the epic has no stories (AC-2: 0)", () => {
    const { container } = renderNav(epicWithCanvas([]));

    rightClick(container, CANVAS);
    const item = screen.getByText(/Attach to story/).closest("button")!;
    expect(item).toBeDisabled();
  });

  it("attaches immediately for a single story (AC-2: 1, AC-3)", async () => {
    readFile.mockResolvedValue(STORY_MD);
    const { container } = renderNav(epicWithCanvas([story("SW-SC-12.md")]));

    rightClick(container, CANVAS);
    fireEvent.click(screen.getByText(/Attach to story/).closest("button")!);

    await waitFor(() => expect(writeContentFile).toHaveBeenCalledTimes(1));
    const [path, text] = writeContentFile.mock.calls[0];
    expect(path).toBe("/repo/SW-SC-12.md");
    expect(text).toContain("supporting_docs:\n  - ./design.excalidraw");
  });

  it("opens the picker for 2+ stories (AC-2: 2+)", async () => {
    const { container } = renderNav(epicWithCanvas([story("A.md"), story("B.md")]));

    rightClick(container, CANVAS);
    fireEvent.click(screen.getByText(/Attach to story/).closest("button")!);

    // The reused quick-picker is now open (with story-scoped copy); no write yet.
    await waitFor(() =>
      expect(screen.getByPlaceholderText("Search stories...")).toBeInTheDocument(),
    );
    expect(writeContentFile).not.toHaveBeenCalled();
  });
});
