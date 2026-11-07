// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi, afterEach } from "vitest";
import { renderHook, act, cleanup } from "@testing-library/react";

async function flushPromises() {
  // Run several microtask turns so chained then()/catch() callbacks settle
  // under fake timers.
  for (let i = 0; i < 20; i++) {
    await Promise.resolve();
  }
}

// Mock the scanner BEFORE importing the hook so the hook captures the mock.
const { scanMock } = vi.hoisted(() => ({ scanMock: vi.fn() }));
vi.mock("@/tauri/commands", async () => {
  const actual = await vi.importActual<typeof import("@/tauri/commands")>("@/tauri/commands");
  return { ...actual, scanPipelineTree: scanMock };
});

import { usePipelineTree } from "./use-pipeline-tree";
import { useConfigStore, type RepoState } from "@/shared/stores/config-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { seedFileRefresh } from "@/shared/stores/__test-utils__/seed-file-refresh";
import { buildTree, buildStoryRef } from "./test-fixtures/derived-tree";

function setHydrated(flag: boolean) {
  useConfigStore.setState({ initialized: flag });
  useViewerStore.setState({ stateLoaded: flag, scopeRecoveryComplete: flag });
}

// PTS-2: teamContentPath must be non-empty for the hook's `canScan` gate
// to fire. Use a stable absolute path per repo so assertions can name it.
const TEAM_CONTENT = "/org/_team/default/content";

function seedRepo() {
  useConfigStore.setState({
    repos: [
      {
        repoPath: "/r1",
        repoName: "r1",
        contextRepoPath: "/ctx/r1",
        teamContentPath: TEAM_CONTENT,
        hasRemote: false,
        remoteUrl: null,
        mainDoc: "",
      },
    ],
  });
}

beforeEach(() => {
  vi.useFakeTimers();
  scanMock.mockReset();
  setHydrated(false);
  useConfigStore.setState({ repos: [] });
  seedFileRefresh();
});

const pendingReads: Array<(tree: ReturnType<typeof buildTree>) => void> = [];
afterEach(async () => {
  cleanup();
  pendingReads.splice(0).forEach((resolve) => resolve(buildTree("")));
  await flushPromises();
  vi.useRealTimers();
});

describe("usePipelineTree", () => {
  it("returns loading with no scanner call while not hydrated", () => {
    seedRepo();
    const { result } = renderHook(() => usePipelineTree("/r1", ""));
    expect(result.current.loading).toBe(true);
    expect(result.current.tree).toBeNull();
    expect(result.current.error).toBeNull();
    expect(scanMock).not.toHaveBeenCalled();
  });

  it("SW-LN-B21: empty team → first content: scans once the repo resolves, even before Rust hydrates contextRepoPath", async () => {
    setHydrated(true);
    const tree = buildTree("", { stories: [buildStoryRef("a.md")] });
    scanMock.mockResolvedValue(tree);
    // A team with no repos: nothing resolves for the nav key yet.
    const { result } = renderHook(() => usePipelineTree("/r1", ""));
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flushPromises();
    });
    expect(result.current.tree).toBeNull();
    expect(scanMock).not.toHaveBeenCalled();

    // The first repo is attached at runtime: its RepoState carries the team content
    // root, but `contextRepoPath` stays "" until the next org discovery (a reload).
    await act(async () => {
      useConfigStore.setState({
        repos: [
          {
            repoPath: "/r1",
            repoName: "r1",
            contextRepoPath: "",
            teamContentPath: TEAM_CONTENT,
            hasRemote: false,
            remoteUrl: null,
            mainDoc: "",
          },
        ],
      });
    });
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flushPromises();
    });
    expect(scanMock).toHaveBeenCalledWith(
      TEAM_CONTENT,
      "",
      "",
      "",
      expect.anything(),
      undefined,
      undefined,
    );
    expect(result.current.tree).toEqual(tree);
  });

  it("short-circuits when teamContentPath is empty", () => {
    setHydrated(true);
    useConfigStore.setState({
      repos: [
        {
          repoPath: "/r1",
          repoName: "r1",
          contextRepoPath: "",
          teamContentPath: "",
          hasRemote: false,
          remoteUrl: null,
          mainDoc: "",
        },
      ],
    });
    const { result } = renderHook(() => usePipelineTree("/r1", ""));
    expect(result.current.loading).toBe(false);
    expect(result.current.tree).toBeNull();
    expect(scanMock).not.toHaveBeenCalled();
  });

  it("debounces and calls scanner after 150ms", async () => {
    seedRepo();
    setHydrated(true);
    const tree = buildTree("", { stories: [buildStoryRef("a.md")] });
    scanMock.mockResolvedValue(tree);

    const { result } = renderHook(() => usePipelineTree("/r1", ""));
    expect(scanMock).not.toHaveBeenCalled();
    expect(result.current.loading).toBe(true);

    await act(async () => {
      vi.advanceTimersByTime(150);
      await flushPromises();
    });

    // PTS-2: scanner takes (teamContentPath, pipelineRoot="", scopePath, mainDoc).
    // viewerRoot arg retired — Rust resolves the content dir up-front.
    // SW-EW-32: + summaryHeadings (default ["Purpose", "Story"]).
    expect(scanMock).toHaveBeenCalledWith(
      TEAM_CONTENT,
      "",
      "",
      "",
      ["Purpose", "Story"],
      // SLW-MD-1: configuredSourceRepos + validTeams — undefined for a non-synthetic
      // repoPath (`/r1`) since there's no org/team to resolve.
      undefined,
      undefined,
    );
    expect(result.current.tree).toBe(tree);
    expect(result.current.loading).toBe(false);
    expect(result.current.error).toBeNull();
  });

  it("surfaces backend errors", async () => {
    seedRepo();
    setHydrated(true);
    scanMock.mockRejectedValue(new Error("boom"));

    const { result } = renderHook(() => usePipelineTree("/r1", ""));
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flushPromises();
    });

    expect(result.current.error).toBe("boom");
    expect(result.current.loading).toBe(false);
    expect(result.current.tree).toBeNull();
  });

  it("refetch bumps manual trigger and fires a fresh scan", async () => {
    seedRepo();
    setHydrated(true);
    scanMock.mockResolvedValue(buildTree(""));

    const { result } = renderHook(() => usePipelineTree("/r1", ""));
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flushPromises();
    });
    expect(scanMock).toHaveBeenCalledTimes(1);

    act(() => {
      result.current.refetch();
    });
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flushPromises();
    });
    expect(scanMock).toHaveBeenCalledTimes(2);
  });

  it("re-runs when file refresh key bumps", async () => {
    seedRepo();
    setHydrated(true);
    scanMock.mockResolvedValue(buildTree(""));

    renderHook(() => usePipelineTree("/r1", ""));
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flushPromises();
    });
    expect(scanMock).toHaveBeenCalledTimes(1);

    act(() => {
      seedFileRefresh({ "/r1": 1 });
    });
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flushPromises();
    });
    expect(scanMock).toHaveBeenCalledTimes(2);
  });

  it("discards stale results via generation counter", async () => {
    seedRepo();
    setHydrated(true);

    let resolveFirst: ((tree: unknown) => void) | null = null;
    scanMock.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveFirst = resolve;
        }),
    );
    const secondTree = buildTree("", { stories: [buildStoryRef("b.md")] });
    scanMock.mockImplementationOnce(() => Promise.resolve(secondTree));

    const { result, rerender } = renderHook(
      ({ scope }: { scope: string }) => usePipelineTree("/r1", scope),
      { initialProps: { scope: "" } },
    );
    await act(async () => {
      vi.advanceTimersByTime(150);
    });
    // First scan is in-flight. Change scope — this bumps generation.
    rerender({ scope: "sub" });
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flushPromises();
    });
    // Now resolve the first (stale) scan. It must be discarded.
    const staleTree = buildTree("", { stories: [buildStoryRef("stale.md")] });
    await act(async () => {
      resolveFirst?.(staleTree);
      await Promise.resolve();
    });

    expect(result.current.tree).toBe(secondTree);
  });

  it("preserves tree reference when a file-refresh rescan is structurally identical", async () => {
    seedRepo();
    setHydrated(true);
    const firstTree = buildTree("", {
      stories: [buildStoryRef("a.md", { title: "Alpha" })],
    });
    // Second tree is structurally identical but has a different title —
    // a pure content edit. The hook must preserve the first tree's
    // reference so the downstream Dagre re-layout is skipped.
    const secondTree = buildTree("", {
      stories: [buildStoryRef("a.md", { title: "Alpha (edited)" })],
    });
    scanMock.mockResolvedValueOnce(firstTree).mockResolvedValueOnce(secondTree);

    const { result } = renderHook(() => usePipelineTree("/r1", ""));
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flushPromises();
    });
    const committedTree = result.current.tree;
    expect(committedTree).toBe(firstTree);

    // Bump the refresh key as if the file watcher fired on a story save.
    act(() => {
      seedFileRefresh({ "/r1": 1 });
    });
    // Background scan runs but should NOT flip to loading — cached tree
    // stays visible while revalidation happens.
    expect(result.current.loading).toBe(false);
    expect(result.current.tree).toBe(committedTree);

    await act(async () => {
      vi.advanceTimersByTime(150);
      await flushPromises();
    });

    // Scan resolved with a structurally identical tree — the committed
    // tree reference must not have changed.
    expect(scanMock).toHaveBeenCalledTimes(2);
    expect(result.current.loading).toBe(false);
    expect(result.current.tree).toBe(committedTree);
    expect(result.current.tree).not.toBe(secondTree);
  });

  it("commits a new tree when a file-refresh rescan is structurally different", async () => {
    seedRepo();
    setHydrated(true);
    const firstTree = buildTree("", {
      stories: [buildStoryRef("a.md")],
    });
    // Second tree adds a new story — structural change, must commit.
    const secondTree = buildTree("", {
      stories: [buildStoryRef("a.md"), buildStoryRef("b.md")],
    });
    scanMock.mockResolvedValueOnce(firstTree).mockResolvedValueOnce(secondTree);

    const { result } = renderHook(() => usePipelineTree("/r1", ""));
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flushPromises();
    });
    expect(result.current.tree).toBe(firstTree);

    act(() => {
      seedFileRefresh({ "/r1": 1 });
    });
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flushPromises();
    });

    expect(result.current.tree).toBe(secondTree);
  });

  it("keeps the cached tree visible while a background refresh-tick scan runs", async () => {
    seedRepo();
    setHydrated(true);
    const firstTree = buildTree("", { stories: [buildStoryRef("a.md")] });
    scanMock.mockResolvedValueOnce(firstTree);

    const { result } = renderHook(() => usePipelineTree("/r1", ""));
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flushPromises();
    });
    expect(result.current.loading).toBe(false);
    expect(result.current.tree).toBe(firstTree);

    // Queue a never-resolving second scan so we can observe the
    // in-flight state cleanly.
    scanMock.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          pendingReads.push(resolve);
        }),
    );
    act(() => {
      seedFileRefresh({ "/r1": 1 });
    });
    // No skeleton flip — the cached tree stays and loading stays false.
    expect(result.current.loading).toBe(false);
    expect(result.current.tree).toBe(firstTree);

    // The debounce timer still fires; the scanner is called but the
    // promise never resolves. The cached tree remains visible.
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flushPromises();
    });
    expect(scanMock).toHaveBeenCalledTimes(2);
    expect(result.current.loading).toBe(false);
    expect(result.current.tree).toBe(firstTree);
  });

  it("drops the cached tree and shows loading when the scope changes", async () => {
    seedRepo();
    setHydrated(true);
    const firstTree = buildTree("", { stories: [buildStoryRef("a.md")] });
    scanMock.mockResolvedValueOnce(firstTree);

    const { result, rerender } = renderHook(
      ({ scope }: { scope: string }) => usePipelineTree("/r1", scope),
      { initialProps: { scope: "" } },
    );
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flushPromises();
    });
    expect(result.current.tree).toBe(firstTree);

    // Queue a never-resolving scan so we can observe the transition.
    scanMock.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          pendingReads.push(resolve);
        }),
    );
    rerender({ scope: "epic-a" });
    // Scope change invalidates the cache — tree nulled, loading flipped.
    expect(result.current.tree).toBeNull();
    expect(result.current.loading).toBe(true);
  });

  it("cleans up timer on unmount without firing the scan", async () => {
    seedRepo();
    setHydrated(true);
    scanMock.mockResolvedValue(buildTree(""));

    const { unmount } = renderHook(() => usePipelineTree("/r1", ""));
    unmount();
    await act(async () => {
      vi.advanceTimersByTime(500);
      await Promise.resolve();
    });
    expect(scanMock).not.toHaveBeenCalled();
  });

  it("SW-FX-37: stays cache-warm when repoPath does not change across rerender (cross-source-repo workspace switch regression)", async () => {
    // AC-5 load-bearing: post-SW-FX-37, `useTeamPipelineRepoPath` returns the
    // team-first synthetic regardless of which workspace is focused, so
    // `usePipelineTree`'s `repoPath` arg is identical across cross-source-repo
    // workspace switches. The scan-key (`use-pipeline-tree.ts:112`) is then
    // identical too → fingerprint cache stays warm → scanner does NOT refire.
    seedRepo();
    setHydrated(true);
    const tree = buildTree("", { stories: [buildStoryRef("a.md")] });
    scanMock.mockResolvedValue(tree);

    const { rerender } = renderHook(({ repo }: { repo: string }) => usePipelineTree(repo, ""), {
      initialProps: { repo: "/r1" },
    });
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flushPromises();
    });
    expect(scanMock).toHaveBeenCalledTimes(1);

    // Simulate the post-fix upstream behavior: a workspace switch produces
    // the SAME team-stable `navRepoPath`. The hook must not refire the
    // scanner, drop the cached tree, or flip to loading.
    rerender({ repo: "/r1" });
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flushPromises();
    });
    expect(scanMock).toHaveBeenCalledTimes(1);
  });
});

// SLW-FX-14: the `repoOverride` param lets an epic's cold-open reconstructed
// RepoState drive the scan while `useResolvedRepo` still misses (workspaceRepos
// empty before org-discovery). Without it, teamContentPath would be "" and the
// scan would short-circuit — the exact blank-tree bug this story fixes.
describe("usePipelineTree repoOverride (SLW-FX-14)", () => {
  const COLD_KEY = "workspace:acme:default:acme/web";
  const OVERRIDE: RepoState = {
    repoPath: COLD_KEY,
    repoName: "web",
    contextRepoPath: "/org/_team/default/repos/acme/web",
    teamContentPath: TEAM_CONTENT,
    hasRemote: false,
    remoteUrl: null,
    mainDoc: "",
  };

  it("drives the scan from the override when useResolvedRepo misses (cold open)", async () => {
    // No config-store repo and no workspaceRepos entry → useResolvedRepo(COLD_KEY)
    // is undefined, so without the override the hook would short-circuit.
    setHydrated(true);
    const tree = buildTree("", { stories: [buildStoryRef("a.md")] });
    scanMock.mockResolvedValue(tree);

    const { result } = renderHook(() => usePipelineTree(COLD_KEY, "", OVERRIDE));
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flushPromises();
    });

    expect(scanMock).toHaveBeenCalledWith(
      TEAM_CONTENT,
      "",
      "",
      "",
      ["Purpose", "Story"],
      // COLD_KEY is a synthetic key but its org/team aren't in the store, so
      // configuredSourceRepos / validTeams resolve undefined.
      undefined,
      undefined,
    );
    expect(result.current.tree).toBe(tree);
  });

  it("resolved repo takes precedence over the override (handoff once store populates)", async () => {
    seedRepo(); // "/r1" → TEAM_CONTENT via config-store
    setHydrated(true);
    scanMock.mockResolvedValue(buildTree("", { stories: [] }));

    // A DIFFERENT override must be ignored while the real repo resolves.
    const otherOverride: RepoState = { ...OVERRIDE, teamContentPath: "/other/content" };
    renderHook(() => usePipelineTree("/r1", "", otherOverride));
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flushPromises();
    });

    expect(scanMock).toHaveBeenCalledWith(
      TEAM_CONTENT, // resolved wins, not "/other/content"
      "",
      "",
      "",
      ["Purpose", "Story"],
      undefined,
      undefined,
    );
  });
});
