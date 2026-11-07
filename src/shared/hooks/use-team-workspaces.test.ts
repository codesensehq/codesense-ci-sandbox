// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import { act, renderHook } from "@testing-library/react";

// Mock tauri commands used by viewer-store. `updatePipelineStatus`
// backs the real store actions the liveness tests drive.
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
  updatePipelineStatus: vi.fn().mockResolvedValue(undefined),
}));

import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";
import { buildRootWorkspaceId, buildTeamKey } from "@/shared/lib/team-key";
import { useTeamWorkspaces, EMPTY_WORKSPACES } from "./use-team-workspaces";

const TEAM_KEY = buildTeamKey("acme", "default");
const OTHER_TEAM = buildTeamKey("acme", "other");

function makeRoot(slug = "acme/web"): Workspace {
  return {
    id: buildRootWorkspaceId(slug),
    kind: "root",
    cwd: `/repos/${slug.split("/").pop()}`,
    status: "ready",
    sourceRepoSlug: slug,
  };
}

function makeStory(id: string, slug = "acme/web"): Workspace {
  return {
    id,
    kind: "story",
    cwd: `/wt/${id}`,
    storyFilePath: `/ctx/stories/${id}.md`,
    storyId: id,
    status: "ready",
    sourceRepoSlug: slug,
  };
}

beforeEach(() => {
  useViewerStore.setState({ workspaces: {}, activeWorkspace: {} });
});

describe("useTeamWorkspaces (T-11)", () => {
  it("returns the stable EMPTY_WORKSPACES sentinel for a team with no workspaces", () => {
    const { result, rerender } = renderHook(() => useTeamWorkspaces(TEAM_KEY));
    expect(result.current).toBe(EMPTY_WORKSPACES);
    rerender();
    expect(result.current).toBe(EMPTY_WORKSPACES);
  });

  it("structural stability: returns the prior reference when the array is re-keyed with identical elements", () => {
    const a = makeStory("a");
    const b = makeStory("b");
    useViewerStore.setState({ workspaces: { [TEAM_KEY]: [a, b] } });

    const { result } = renderHook(() => useTeamWorkspaces(TEAM_KEY));
    const before = result.current;

    // New array reference, same element references (the shape a no-op rebuild
    // would produce). useShallow must collapse this to the prior reference.
    act(() => {
      useViewerStore.setState((s) => ({
        workspaces: { ...s.workspaces, [TEAM_KEY]: [a, b] },
      }));
    });

    expect(result.current).toBe(before);
  });

  it("cross-team stability: unaffected when ANOTHER team's workspaces change", async () => {
    const mine = makeStory("mine");
    const theirs = makeStory("theirs");
    useViewerStore.setState({
      workspaces: { [TEAM_KEY]: [mine], [OTHER_TEAM]: [theirs] },
      activeWorkspace: { [TEAM_KEY]: mine.id, [OTHER_TEAM]: theirs.id },
    });

    const { result } = renderHook(() => useTeamWorkspaces(TEAM_KEY));
    const before = result.current;

    await act(async () => {
      await useViewerStore.getState().changeStoryStatus(OTHER_TEAM, theirs.id, "done");
    });

    expect(result.current).toBe(before);
  });

  it("liveness: returns a new array reflecting an element field change", async () => {
    const root = makeRoot("acme/web");
    const story = makeStory("story-1", "acme/web");
    useViewerStore.setState({
      workspaces: { [TEAM_KEY]: [root, story] },
      activeWorkspace: { [TEAM_KEY]: story.id },
    });

    const { result } = renderHook(() => useTeamWorkspaces(TEAM_KEY));
    const before = result.current;

    await act(async () => {
      await useViewerStore.getState().changeStoryStatus(TEAM_KEY, story.id, "done");
    });

    expect(result.current).not.toBe(before);
    expect(result.current.find((w) => w.id === story.id)?.storyStatus).toBe("done");
  });

  it("liveness: reflects a removed workspace", () => {
    const root = makeRoot("acme/web");
    const story = makeStory("story-1", "acme/web");
    useViewerStore.setState({
      workspaces: { [TEAM_KEY]: [root, story] },
      activeWorkspace: { [TEAM_KEY]: story.id },
    });

    const { result } = renderHook(() => useTeamWorkspaces(TEAM_KEY));
    const before = result.current;
    expect(before).toHaveLength(2);

    act(() => {
      useViewerStore.getState().closeWorkspace(TEAM_KEY, story.id);
    });

    expect(result.current).not.toBe(before);
    expect(result.current).toHaveLength(1);
    expect(result.current[0].id).toBe(root.id);
  });
});
