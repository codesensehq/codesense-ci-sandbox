// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";

import type { DerivedPipelineTree, StoryRef } from "@/shared/lib/pipeline-graph/types";
import type { Workspace } from "./viewer-store";

// Only `scanPipelineTree` matters for the durable-scan branch; the rest are the
// commands the store touches at init (mirrors viewer-store.open-and-focus.test).
const scanPipelineTree = vi.fn();
vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn(),
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
  scanPipelineTree: (...args: unknown[]) => scanPipelineTree(...args),
}));

vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: vi.fn(), debug: vi.fn() },
  initLogging: vi.fn().mockResolvedValue(undefined),
}));

// Isolate the durable-scan branch's repo resolution so it needs no org/config
// fixtures — the pure storyId→filePath join (`storyFilePathFromTree`) stays real.
let navRepoPath = "/nav-repo";
const teamContentPath: string | null = "/content";
vi.mock("@/shared/hooks/use-team-pipeline-repo-path", () => ({
  resolveTeamPipelineRepoPath: () => navRepoPath,
}));
vi.mock("@/shared/lib/resolve-repo", () => ({
  resolveRepo: () => (teamContentPath ? { teamContentPath, mainDoc: "" } : undefined),
}));

import { useViewerStore } from "./viewer-store";

function ws(over: Partial<Workspace>): Workspace {
  return {
    id: "SW-AM-7-foo",
    kind: "story",
    cwd: "/wt",
    status: "ready",
    storyId: "SW-AM-7",
    storyFilePath: "/content/SW-AM-7-foo.md",
    ...over,
  } as Workspace;
}

function makeStory(fileName: string, storyId: string, filePath: string): StoryRef {
  return {
    fileName,
    filePath,
    title: null,
    dependsOn: null,
    status: null,
    storyText: null,
    documentType: null,
    supportingDocs: null,
    sourceRepo: null,
    team: null,
    storyId,
  } as StoryRef;
}

function makeTree(stories: StoryRef[]): DerivedPipelineTree {
  return {
    dirPath: "",
    dirName: "",
    absoluteDirPath: "/content",
    stories,
    subEpics: [],
    orderError: null,
    mainDoc: null,
  };
}

describe("ensureStoryWorkspaceOpen (SW-AM-7)", () => {
  const activateTrayWorkspace = vi.fn();
  const openOrFocusStoryWorkspace = vi.fn().mockResolvedValue("/wt");

  beforeEach(() => {
    vi.clearAllMocks();
    navRepoPath = "/nav-repo";
    openOrFocusStoryWorkspace.mockResolvedValue("/wt");
    useViewerStore.setState({
      activateTrayWorkspace,
      openOrFocusStoryWorkspace,
      workspaces: {},
      softClosedWorkspaces: {},
    });
  });

  it("AC-2: reveals a live tile from the in-memory record (no disk scan)", async () => {
    useViewerStore.setState({ workspaces: { "team-1": [ws({})] } });

    const wsId = await useViewerStore.getState().ensureStoryWorkspaceOpen("team-1", "SW-AM-7");

    expect(wsId).toBe("SW-AM-7-foo");
    expect(activateTrayWorkspace).toHaveBeenCalledWith("team-1", "SW-AM-7-foo");
    expect(openOrFocusStoryWorkspace).not.toHaveBeenCalled();
    expect(scanPipelineTree).not.toHaveBeenCalled();
  });

  it("AC-2: reveals (restores) a soft-closed shadow from memory (no disk scan)", async () => {
    useViewerStore.setState({
      softClosedWorkspaces: { "team-1": { "SW-AM-7-foo": { workspace: ws({}) } } } as never,
    });

    const wsId = await useViewerStore.getState().ensureStoryWorkspaceOpen("team-1", "SW-AM-7");

    expect(wsId).toBe("SW-AM-7-foo");
    expect(activateTrayWorkspace).toHaveBeenCalledWith("team-1", "SW-AM-7-foo");
    expect(scanPipelineTree).not.toHaveBeenCalled();
  });

  it("AC-1: cold-opens a fully-closed story resolved off disk", async () => {
    scanPipelineTree.mockResolvedValue(
      makeTree([makeStory("SW-AM-7-foo.md", "SW-AM-7", "/content/SW-AM-7-foo.md")]),
    );

    const wsId = await useViewerStore.getState().ensureStoryWorkspaceOpen("team-1", "SW-AM-7");

    expect(scanPipelineTree).toHaveBeenCalled();
    // focus:true, spawnFirstTerminal:false — the agent's own PTY becomes the tab,
    // no redundant fresh shell.
    expect(openOrFocusStoryWorkspace).toHaveBeenCalledWith(
      "team-1",
      "/content/SW-AM-7-foo.md",
      undefined,
      undefined,
      true,
      false,
    );
    expect(activateTrayWorkspace).not.toHaveBeenCalled();
    expect(wsId).toBe("SW-AM-7-foo");
  });

  it("AC-4: returns null when the cold-open yields no live tile (handled error / ghost)", async () => {
    scanPipelineTree.mockResolvedValue(
      makeTree([makeStory("SW-AM-7-foo.md", "SW-AM-7", "/content/SW-AM-7-foo.md")]),
    );
    openOrFocusStoryWorkspace.mockResolvedValue(""); // "" = nothing opened, no record

    const wsId = await useViewerStore.getState().ensureStoryWorkspaceOpen("team-1", "SW-AM-7");

    expect(openOrFocusStoryWorkspace).toHaveBeenCalled();
    expect(wsId).toBeNull(); // degrade to adopt-into-active rather than orphan a tab
  });

  it("AC-4: returns null when the story is absent from the scanned tree", async () => {
    scanPipelineTree.mockResolvedValue(makeTree([]));

    const wsId = await useViewerStore.getState().ensureStoryWorkspaceOpen("team-1", "SW-AM-7");

    expect(wsId).toBeNull();
    expect(openOrFocusStoryWorkspace).not.toHaveBeenCalled();
    expect(activateTrayWorkspace).not.toHaveBeenCalled();
  });

  it("AC-4: returns null (no scan) when the team has no resolvable content repo", async () => {
    navRepoPath = ""; // no nav repo → nothing to scan

    const wsId = await useViewerStore.getState().ensureStoryWorkspaceOpen("team-1", "SW-AM-7");

    expect(wsId).toBeNull();
    expect(scanPipelineTree).not.toHaveBeenCalled();
  });
});
