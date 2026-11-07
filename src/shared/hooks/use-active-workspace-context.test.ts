// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import { act, renderHook } from "@testing-library/react";

// Mock tauri commands used by viewer-store. `updatePipelineStatus`
// are needed by the T-11 referential-stability tests, which drive the real
// `changeStoryStatus` / `closeWorkspace` store actions.
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
import { useOrgStore } from "@/shared/stores/org-store";
import { NO_TEAM_SENTINEL, buildRootWorkspaceId, buildTeamKey } from "@/shared/lib/team-key";
import {
  focusedWorkspaceOf,
  getActiveWorkspaceContext,
  useActiveWorkspaceContext,
} from "./use-active-workspace-context";

const ORG = "acme";
const TEAM = "default";
const TEAM_KEY = buildTeamKey(ORG, TEAM);

function makeRoot(slug = "acme/web"): Workspace {
  return {
    id: buildRootWorkspaceId(slug),
    kind: "root",
    cwd: `/Users/dev/repos/${slug.split("/").pop()}`,
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

function seedActiveTeam(orgSlug: string | null, teamName: string | null) {
  useOrgStore.setState({
    activeOrgSlug: orgSlug,
    activeTeamName: teamName,
  });
}

function seedWorkspaces(teamKey: string, workspaces: Workspace[], activeId?: string) {
  useViewerStore.setState({
    workspaces: { [teamKey]: workspaces },
    activeWorkspace: activeId !== undefined ? { [teamKey]: activeId } : {},
  });
}

beforeEach(() => {
  useViewerStore.setState({
    workspaces: {},
    activeWorkspace: {},
    softClosedWorkspaces: {},
    openedTeams: [],
  });
  seedActiveTeam(null, null);
});

// ---------------------------------------------------------------------------
// getActiveWorkspaceContext (imperative)
// ---------------------------------------------------------------------------

describe("getActiveWorkspaceContext", () => {
  it("returns NO_TEAM_SENTINEL + null workspace when no org is active", () => {
    seedActiveTeam(null, null);
    const ctx = getActiveWorkspaceContext();
    expect(ctx.teamKey).toBe(NO_TEAM_SENTINEL);
    expect(ctx.workspace).toBeNull();
    expect(ctx.sourceRepoSlug).toBeNull();
  });

  it("returns the active team key but null workspace when team has no workspaces", () => {
    seedActiveTeam(ORG, TEAM);
    seedWorkspaces(TEAM_KEY, []);
    const ctx = getActiveWorkspaceContext();
    expect(ctx.teamKey).toBe(TEAM_KEY);
    expect(ctx.workspace).toBeNull();
    expect(ctx.sourceRepoSlug).toBeNull();
  });

  it("returns the focused workspace + its sourceRepoSlug when one is active", () => {
    seedActiveTeam(ORG, TEAM);
    const root = makeRoot("acme/web");
    seedWorkspaces(TEAM_KEY, [root], root.id);
    const ctx = getActiveWorkspaceContext();
    expect(ctx.teamKey).toBe(TEAM_KEY);
    expect(ctx.workspace?.id).toBe(root.id);
    expect(ctx.sourceRepoSlug).toBe("acme/web");
  });

  it("returns null workspace when activeWorkspaceId points at a non-existent id", () => {
    seedActiveTeam(ORG, TEAM);
    seedWorkspaces(TEAM_KEY, [makeRoot()], "stale-id-that-doesnt-exist");
    const ctx = getActiveWorkspaceContext();
    expect(ctx.workspace).toBeNull();
    expect(ctx.sourceRepoSlug).toBeNull();
  });

  it("falls back to the first root when activeWorkspaceId is undefined", () => {
    // Resolution routes through `activeWorkspaceForTeam`, so an unset active id
    // falls back to the team's first root — the same workspace the bar
    // highlights. The real mutators always set an active id when a team has
    // workspaces (ensureRootWorkspaces/closeWorkspace), so this directly-seeded
    // undefined state is not reachable in production; the fallback just keeps
    // the context consistent with the bar if it ever occurs.
    seedActiveTeam(ORG, TEAM);
    const root = makeRoot("acme/web");
    seedWorkspaces(TEAM_KEY, [root]);
    const ctx = getActiveWorkspaceContext();
    expect(ctx.workspace?.id).toBe(root.id);
    expect(ctx.sourceRepoSlug).toBe("acme/web");
  });

  it("picks the story workspace when it is the active one", () => {
    seedActiveTeam(ORG, TEAM);
    const root = makeRoot("acme/web");
    const story = makeStory("api-feature-1", "acme/api");
    seedWorkspaces(TEAM_KEY, [root, story], story.id);
    const ctx = getActiveWorkspaceContext();
    expect(ctx.workspace?.id).toBe(story.id);
    expect(ctx.sourceRepoSlug).toBe("acme/api");
  });
});

// ---------------------------------------------------------------------------
// useActiveWorkspaceContext (React hook)
// ---------------------------------------------------------------------------

describe("useActiveWorkspaceContext", () => {
  it("returns NO_TEAM_SENTINEL when no org is active", () => {
    seedActiveTeam(null, null);
    const { result } = renderHook(() => useActiveWorkspaceContext());
    expect(result.current.teamKey).toBe(NO_TEAM_SENTINEL);
    expect(result.current.workspace).toBeNull();
    expect(result.current.sourceRepoSlug).toBeNull();
  });

  it("returns the focused workspace + sourceRepoSlug when one is active", () => {
    seedActiveTeam(ORG, TEAM);
    const root = makeRoot("acme/web");
    seedWorkspaces(TEAM_KEY, [root], root.id);
    const { result } = renderHook(() => useActiveWorkspaceContext());
    expect(result.current.teamKey).toBe(TEAM_KEY);
    expect(result.current.workspace?.id).toBe(root.id);
    expect(result.current.sourceRepoSlug).toBe("acme/web");
  });

  it("returns null workspace when team has no workspaces", () => {
    seedActiveTeam(ORG, TEAM);
    seedWorkspaces(TEAM_KEY, []);
    const { result } = renderHook(() => useActiveWorkspaceContext());
    expect(result.current.teamKey).toBe(TEAM_KEY);
    expect(result.current.workspace).toBeNull();
    expect(result.current.sourceRepoSlug).toBeNull();
  });

  it("picks the story workspace when its sourceRepoSlug differs from the root's", () => {
    seedActiveTeam(ORG, TEAM);
    const root = makeRoot("acme/web");
    const story = makeStory("api-feature-1", "acme/api");
    seedWorkspaces(TEAM_KEY, [root, story], story.id);
    const { result } = renderHook(() => useActiveWorkspaceContext());
    expect(result.current.workspace?.id).toBe(story.id);
    expect(result.current.sourceRepoSlug).toBe("acme/api");
  });
});

// ---------------------------------------------------------------------------
// T-11: referential stability (no wasted renders) + liveness (nothing dropped)
//
// All mutations below go through the REAL store actions (changeStoryStatus,
// switchWorkspace, closeWorkspace) so the immutable-update path is exercised.
// If any mutator ever regressed to in-place mutation, the active-workspace
// object reference would stop changing and the liveness assertions would fail.
// ---------------------------------------------------------------------------

describe("useActiveWorkspaceContext — referential stability (T-11)", () => {
  it("AC-1: output stays Object.is-stable when a SIBLING workspace changes", async () => {
    seedActiveTeam(ORG, TEAM);
    const root = makeRoot("acme/web");
    const active = makeStory("active-story", "acme/web");
    const sibling = makeStory("sibling-story", "acme/api");
    seedWorkspaces(TEAM_KEY, [root, active, sibling], active.id);

    const { result } = renderHook(() => useActiveWorkspaceContext());
    const before = result.current;

    await act(async () => {
      await useViewerStore.getState().changeStoryStatus(TEAM_KEY, sibling.id, "done");
    });

    // Active workspace is untouched → context identity must be preserved.
    expect(result.current.workspace?.id).toBe(active.id);
    expect(Object.is(result.current, before)).toBe(true);
  });

  it("AC-5 liveness: output identity changes when the ACTIVE workspace's field changes", async () => {
    seedActiveTeam(ORG, TEAM);
    const root = makeRoot("acme/web");
    const active = makeStory("active-story", "acme/web");
    seedWorkspaces(TEAM_KEY, [root, active], active.id);

    const { result } = renderHook(() => useActiveWorkspaceContext());
    const before = result.current;

    await act(async () => {
      await useViewerStore.getState().changeStoryStatus(TEAM_KEY, active.id, "done");
    });

    expect(Object.is(result.current, before)).toBe(false);
    expect(result.current.workspace?.id).toBe(active.id);
    expect(result.current.workspace?.storyStatus).toBe("done");
  });

  it("AC-5 liveness: output changes when the active workspace is switched", () => {
    seedActiveTeam(ORG, TEAM);
    const root = makeRoot("acme/web");
    const story = makeStory("story-1", "acme/api");
    seedWorkspaces(TEAM_KEY, [root, story], root.id);

    const { result } = renderHook(() => useActiveWorkspaceContext());
    const before = result.current;
    expect(before.workspace?.id).toBe(root.id);

    act(() => {
      useViewerStore.getState().switchWorkspace(TEAM_KEY, story.id);
    });

    expect(Object.is(result.current, before)).toBe(false);
    expect(result.current.workspace?.id).toBe(story.id);
    expect(result.current.sourceRepoSlug).toBe("acme/api");
  });

  it("AC-5 liveness: output changes when the active workspace is removed", () => {
    seedActiveTeam(ORG, TEAM);
    const root = makeRoot("acme/web");
    const story = makeStory("story-1", "acme/api");
    seedWorkspaces(TEAM_KEY, [root, story], story.id);

    const { result } = renderHook(() => useActiveWorkspaceContext());
    const before = result.current;
    expect(before.workspace?.id).toBe(story.id);

    act(() => {
      useViewerStore.getState().closeWorkspace(TEAM_KEY, story.id);
    });

    // Active falls back to the root after the story closes.
    expect(Object.is(result.current, before)).toBe(false);
    expect(result.current.workspace?.id).toBe(root.id);
  });
});

// ---------------------------------------------------------------------------
// Two open teams: the context is the active team's, with its own remembered focus
// ---------------------------------------------------------------------------

describe("the workspace context with two open teams", () => {
  const OTHER = buildTeamKey(ORG, "other");
  const root = makeRoot();
  const story = makeStory("SW-2", "acme/api");
  function seedTwoTeams() {
    seedActiveTeam(ORG, TEAM);
    useViewerStore.setState({
      workspaces: { [TEAM_KEY]: [root], [OTHER]: [story] },
      activeWorkspace: { [TEAM_KEY]: root.id, [OTHER]: story.id },
      openedTeams: [TEAM_KEY, OTHER],
    });
  }

  it("follows a team switch to that team's remembered focus, and back", () => {
    seedTwoTeams();
    const { result } = renderHook(() => useActiveWorkspaceContext());
    expect(getActiveWorkspaceContext().workspace).toBe(root);
    act(() => seedActiveTeam(ORG, "other"));
    expect(getActiveWorkspaceContext()).toEqual({
      teamKey: OTHER,
      workspace: story,
      sourceRepoSlug: "acme/api",
      repoPath: expect.stringContaining("acme/api"),
    });
    expect(result.current).toEqual(getActiveWorkspaceContext());
    act(() => seedActiveTeam(ORG, TEAM));
    expect(result.current.workspace).toBe(root);
  });

  it("is no team with no org active", () => {
    seedTwoTeams();
    seedActiveTeam(null, null);
    expect(getActiveWorkspaceContext()).toEqual({
      teamKey: NO_TEAM_SENTINEL,
      workspace: null,
      sourceRepoSlug: null,
      repoPath: null,
    });
    expect(focusedWorkspaceOf(useViewerStore.getState(), NO_TEAM_SENTINEL)).toBeNull();
  });
});
