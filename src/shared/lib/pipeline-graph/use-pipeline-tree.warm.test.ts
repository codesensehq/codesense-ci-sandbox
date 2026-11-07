// SPDX-License-Identifier: MIT
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { scan } = vi.hoisted(() => ({ scan: vi.fn() }));
vi.mock("@/tauri/commands", async () => ({
  ...(await vi.importActual<typeof import("@/tauri/commands")>("@/tauri/commands")),
  scanPipelineTree: scan,
}));
import { usePipelineTree } from "./use-pipeline-tree";
import { useConfigStore } from "@/shared/stores/config-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { seedFileRefresh } from "@/shared/stores/__test-utils__/seed-file-refresh";
import { warmScopes } from "@/shared/lib/warm-scopes";
import { expectFlatAcross } from "@/shared/lib/warm-scopes/__test-utils__/expect-flat-across";
import { buildStoryRef, buildTree } from "./test-fixtures/derived-tree";

const flush = async () => {
  for (let i = 0; i < 25; i++) await Promise.resolve();
};
const settle = () =>
  act(async () => {
    vi.advanceTimersByTime(200);
    await flush();
  });

const A = "acme:A";
const B = "acme:B";
const repo = (repoPath: string, teamContentPath: string) => ({
  repoPath,
  repoName: repoPath,
  contextRepoPath: "",
  teamContentPath,
  hasRemote: false,
  remoteUrl: null,
  mainDoc: "",
});

beforeEach(() => {
  vi.useFakeTimers();
  scan.mockReset();
  seedFileRefresh();
  useConfigStore.setState({
    initialized: true,
    repos: [repo("/a", "/content/a"), repo("/b", "/content/b")],
  });
  useViewerStore.setState({ stateLoaded: true, scopeRecoveryComplete: true });
  warmScopes.activate(A);
});
afterEach(() => {
  cleanup();
  warmScopes.forget(A);
  warmScopes.forget(B);
  warmScopes.activate(null);
  vi.useRealTimers();
});

describe("usePipelineTree warm cache", () => {
  it("a remounted team shows its last tree at once, without a loading frame", async () => {
    const tree = buildTree("", { stories: [buildStoryRef("a.md")] });
    scan.mockResolvedValue(tree);
    const first = renderHook(() => usePipelineTree("/a", "", undefined, true, A));
    await settle();
    expect(first.result.current.tree).toEqual(tree);
    first.unmount();

    const second = renderHook(() => usePipelineTree("/a", "", undefined, true, A));
    expect(second.result.current.loading).toBe(false);
    expect(second.result.current.tree).toEqual(tree);
  });

  it("revalidates behind the cached tree and commits a changed one", async () => {
    scan.mockResolvedValueOnce(buildTree("", { stories: [buildStoryRef("a.md")] }));
    const first = renderHook(() => usePipelineTree("/a", "", undefined, true, A));
    await settle();
    first.unmount();
    const next = buildTree("", { stories: [buildStoryRef("a.md"), buildStoryRef("b.md")] });
    scan.mockResolvedValueOnce(next);
    const { result } = renderHook(() => usePipelineTree("/a", "", undefined, true, A));
    expect(result.current.loading).toBe(false);
    await settle();
    expect(scan).toHaveBeenCalledTimes(2);
    expect(result.current.tree).toEqual(next);
    expect(result.current.loading).toBe(false);
  });

  it("does not hand one team's tree to another", async () => {
    scan.mockResolvedValue(buildTree("", { stories: [buildStoryRef("a.md")] }));
    renderHook(() => usePipelineTree("/a", "", undefined, true, A));
    await settle();
    cleanup();
    warmScopes.activate(B);
    scan.mockReturnValue(new Promise(() => {}));
    const { result } = renderHook(() => usePipelineTree("/b", "", undefined, true, B));
    expect(result.current.tree).toBeNull();
    expect(result.current.loading).toBe(true);
  });

  it("drops a team's tree once the team is gone", async () => {
    scan.mockResolvedValue(buildTree("", { stories: [buildStoryRef("a.md")] }));
    const first = renderHook(() => usePipelineTree("/a", "", undefined, true, A));
    await settle();
    first.unmount();
    expect(warmScopes.debugCounts().caches["pipeline-tree"]).toBe(1);
    warmScopes.forget(A);
    expect(warmScopes.debugCounts().caches["pipeline-tree"]).toBe(0);
  });

  it("caches nothing without a scope", async () => {
    scan.mockResolvedValue(buildTree("", { stories: [buildStoryRef("a.md")] }));
    const first = renderHook(() => usePipelineTree("/a", ""));
    await settle();
    first.unmount();
    expect(warmScopes.debugCounts().caches["pipeline-tree"] ?? 0).toBe(0);
  });

  it("a cached tree seeds when the repo path resolves after mount, as with a Private root", async () => {
    const tree = buildTree("", { stories: [buildStoryRef("a.md")] });
    scan.mockResolvedValue(tree);
    const first = renderHook(() => usePipelineTree("/a", "", undefined, true, A));
    await settle();
    first.unmount();

    scan.mockReturnValue(new Promise(() => {}));
    let path = "";
    let loadingFrames = 0;
    const { result, rerender } = renderHook(() => {
      const value = usePipelineTree(path, "", undefined, true, A);
      if (path && value.loading && value.tree === null) loadingFrames += 1;
      return value;
    });
    path = "/a";
    rerender();
    await settle();
    expect(result.current.tree).toEqual(tree);
    expect(result.current.loading).toBe(false);
    expect(loadingFrames).toBe(0);
    expect(scan).toHaveBeenCalledTimes(2);
  });

  it("a changed input key without a cached tree still shows loading", async () => {
    scan.mockReturnValue(new Promise(() => {}));
    let path = "";
    const { result, rerender } = renderHook(() => usePipelineTree(path, "", undefined, true, A));
    path = "/a";
    rerender();
    await settle();
    expect(result.current.tree).toBeNull();
    expect(result.current.loading).toBe(true);
  });

  it("50 rounds through four teams never show a loading frame once each has been shown", async () => {
    const teams = ["acme:A", "acme:B", "acme:C", "acme:D"];
    useConfigStore.setState({
      repos: teams.map((team) => repo(`/${team}`, `/content/${team}`)),
    });
    scan.mockResolvedValue(buildTree("", { stories: [buildStoryRef("a.md")] }));
    let loadingFrames = 0;
    const visit = async (team: string) => {
      warmScopes.activate(team);
      const view = renderHook(() => {
        const result = usePipelineTree(`/${team}`, "", undefined, true, team);
        if (result.loading && result.tree === null) loadingFrames += 1;
        return result;
      });
      await settle();
      expect(view.result.current.tree).not.toBeNull();
      view.unmount();
    };
    await expectFlatAcross(
      async () => {
        for (const team of teams) await visit(team);
      },
      () => ({ loadingFrames }),
      { warmup: 1 },
    );
    for (const team of teams) warmScopes.forget(team);
  });
});
