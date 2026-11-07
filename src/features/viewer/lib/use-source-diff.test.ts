// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";

import type { Workspace } from "@/shared/stores/viewer-store";

// --- mocks -----------------------------------------------------------------

const subscribeChangesetMock = vi.fn();
const forceChangesetMock = vi.fn();
const tickChangesetMock = vi.fn();
vi.mock("@/features/git-sync/lib/source-redline-source", () => ({
  sourceWorktreeChangeset: vi.fn(),
  sourceFileRedline: vi.fn(),
  sourceCommitChangeset: vi.fn(),
  sourceCommitFileRedline: vi.fn(),
  sourceCommitGraph: vi.fn(),
  sourceRedline: vi.fn(),
  subscribeChangeset: (...args: unknown[]) => subscribeChangesetMock(...args),
  forceChangeset: (...args: unknown[]) => forceChangesetMock(...args),
  tickChangeset: (...args: unknown[]) => tickChangesetMock(...args),
}));

vi.mock("@/tauri/commands", () => ({
  getWorktreeConfig: vi.fn(),
  checkBranchExists: vi.fn(),
  storyDefaultStartPoint: vi.fn(),
}));

vi.mock("@/shared/lib/worktree/resolve", () => ({
  resolveWorktreeParams: vi.fn(),
}));

// The baseRef hook reads the org root off the source repo; stub it to a constant.
vi.mock("@/features/git-sync/lib/use-org-root", () => ({
  useOrgRoot: () => "/org/root",
}));

import { sourceFileRedline } from "@/features/git-sync/lib/source-redline-source";
import { checkBranchExists, getWorktreeConfig, storyDefaultStartPoint } from "@/tauri/commands";
import { resolveWorktreeParams } from "@/shared/lib/worktree/resolve";

import { useSourceChangeset, useSourceDiffBase, useSourceFileRedline } from "./use-source-diff";

const storyWs: Workspace = {
  id: "w1",
  kind: "story",
  cwd: "/wt/story",
  status: "ready",
  storyId: "SLW-WD-2",
  storyFilePath: "/repo/story.md",
  sourceRepoPath: "/repo",
};

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(getWorktreeConfig).mockResolvedValue({ default_start_point: "main" } as never);
  vi.mocked(checkBranchExists).mockResolvedValue(true);
  // Reset, not just cleared, so one test's repo default never leaks into the next.
  vi.mocked(storyDefaultStartPoint).mockReset().mockResolvedValue("refs/heads/main");
});

describe("useSourceDiffBase (AC-10 / SLW-DV-7)", () => {
  it("resolves to the dependency branch when startPoint != default and the branch exists", async () => {
    vi.mocked(resolveWorktreeParams).mockResolvedValue({
      branchName: "feat/SLW-WD-2",
      startPoint: "feat/SLW-WD-1",
    });
    const { result } = renderHook(() => useSourceDiffBase(storyWs));
    await waitFor(() => expect(result.current.baseRef).toBe("feat/SLW-WD-1"));
    expect(result.current.defaultStart).toBe("main");
    expect(checkBranchExists).toHaveBeenCalledWith("/repo", "feat/SLW-WD-1");
  });

  it("resolves to undefined for a fork-from-default story + surfaces the default start (SLW-DV-7)", async () => {
    vi.mocked(getWorktreeConfig).mockResolvedValue({ default_start_point: "develop" } as never);
    vi.mocked(resolveWorktreeParams).mockResolvedValue({
      branchName: "feat/SLW-WD-2",
      startPoint: "develop",
    });
    const { result } = renderHook(() => useSourceDiffBase(storyWs));
    // fork-from-default → baseRef undefined; the default start surfaces for the label.
    await waitFor(() => expect(result.current.defaultStart).toBe("develop"));
    expect(result.current.baseRef).toBeUndefined();
    expect(checkBranchExists).not.toHaveBeenCalled();
  });

  it("falls back to undefined when the dependency branch is missing (F-29 edge, not an error)", async () => {
    vi.mocked(resolveWorktreeParams).mockResolvedValue({
      branchName: "feat/SLW-WD-2",
      startPoint: "feat/SLW-WD-1",
    });
    vi.mocked(checkBranchExists).mockResolvedValue(false);
    const { result } = renderHook(() => useSourceDiffBase(storyWs));
    await waitFor(() => expect(checkBranchExists).toHaveBeenCalled());
    expect(result.current.baseRef).toBeUndefined();
  });

  it("falls back to undefined on a resolver/config error", async () => {
    vi.mocked(resolveWorktreeParams).mockRejectedValue(new Error("boom"));
    const { result } = renderHook(() => useSourceDiffBase(storyWs));
    await waitFor(() => expect(resolveWorktreeParams).toHaveBeenCalled());
    expect(result.current.baseRef).toBeUndefined();
  });

  it("names 'main' when neither the config nor the repo names a default, so the label is never blank (SLW-DV-7)", async () => {
    vi.mocked(getWorktreeConfig).mockResolvedValue({ default_start_point: "" } as never);
    vi.mocked(storyDefaultStartPoint).mockRejectedValue("unavailable: no default branch found");
    vi.mocked(resolveWorktreeParams).mockResolvedValue({
      branchName: "feat/SLW-DV-2",
      startPoint: "",
    });
    const { result } = renderHook(() => useSourceDiffBase(storyWs));
    await waitFor(() => expect(resolveWorktreeParams).toHaveBeenCalled());
    expect(result.current.defaultStart).toBe("main");
    // A falsy start-point must resolve to fork-from-default (undefined baseRef),
    // never a threaded empty-string ref.
    expect(result.current.baseRef).toBeUndefined();
    expect(checkBranchExists).not.toHaveBeenCalledWith("/repo", "");
  });

  // #2840: no worktree.yaml base, so the story branch was cut from the repo's default
  // branch; the label names that branch, as worktree creation resolved it.
  it.each([
    ["develop", "refs/remotes/origin/develop", "origin/develop"],
    ["master", "master", "master"],
  ])(
    "#2840: a repo whose default is %s names it as the fork parent",
    async (_name, resolved, label) => {
      vi.mocked(getWorktreeConfig).mockResolvedValue(null as never);
      vi.mocked(storyDefaultStartPoint).mockResolvedValue(resolved);
      vi.mocked(resolveWorktreeParams).mockResolvedValue({
        branchName: "zz-open-probe",
        startPoint: null,
      });
      const { result } = renderHook(() => useSourceDiffBase(storyWs));
      await waitFor(() => expect(result.current.defaultStart).toBe(label));
      expect(storyDefaultStartPoint).toHaveBeenCalledWith("/repo", null);
      expect(result.current.baseRef).toBeUndefined();
      expect(checkBranchExists).not.toHaveBeenCalled();
    },
  );

  it("stays undefined without a source repo / story file", async () => {
    const noSource: Workspace = { ...storyWs, sourceRepoPath: undefined };
    const { result } = renderHook(() => useSourceDiffBase(noSource));
    expect(result.current.baseRef).toBeUndefined();
    expect(getWorktreeConfig).not.toHaveBeenCalled();
  });
});

describe("useSourceChangeset — subscribe routing (DESIGN-remote-phase-2.md B-1)", () => {
  // Minimal subscribe-mock harness: capture the registered (onUpdate, onError)
  // callbacks and let each test fire them like the seam would.
  interface Captured {
    onUpdate: (files: readonly { path: string; status?: string }[]) => void;
    onError: (err: unknown) => void;
    unsubscribe: () => void;
    opClass: string | undefined;
  }
  function latestSubscribe(): Captured | null {
    const call = subscribeChangesetMock.mock.calls.at(-1);
    if (!call) return null;
    const [, , , onUpdate, onError, options] = call as [
      unknown,
      unknown,
      unknown,
      Captured["onUpdate"],
      Captured["onError"],
      { opClass?: string } | undefined,
    ];
    const unsubscribe = subscribeChangesetMock.mock.results.at(-1)?.value as () => void;
    return { onUpdate, onError, unsubscribe, opClass: options?.opClass };
  }

  beforeEach(() => {
    subscribeChangesetMock.mockReset();
    forceChangesetMock.mockReset();
    tickChangesetMock.mockReset();
    subscribeChangesetMock.mockReturnValue(() => {});
  });

  it("subscribes with opClass:'diff' and threads baseRef when enabled", async () => {
    const { result } = renderHook(() => useSourceChangeset("/wt/story", "feat/SLW-WD-1", true));
    await waitFor(() => expect(subscribeChangesetMock).toHaveBeenCalledTimes(1));
    const captured = latestSubscribe()!;
    expect(captured.opClass).toBe("diff");
    const call = subscribeChangesetMock.mock.calls[0];
    expect(call[0]).toEqual({ kind: "local", cwd: "/wt/story" });
    expect(call[1]).toBe("feat/SLW-WD-1");
    // Seam fires its first onUpdate — hook state advances to the resolved list.
    captured.onUpdate([{ path: "a.ts", status: "modified" }]);
    await waitFor(() =>
      expect(result.current.files).toEqual([{ path: "a.ts", status: "modified" }]),
    );
    expect(result.current.loading).toBe(false);
    expect(result.current.error).toBeNull();
  });

  it("does not subscribe while disabled (lazy)", () => {
    renderHook(() => useSourceChangeset("/wt/story", undefined, false));
    expect(subscribeChangesetMock).not.toHaveBeenCalled();
  });

  it("surfaces an error (not a throw) and reloads via forceChangeset (bypass)", async () => {
    const { result } = renderHook(() => useSourceChangeset("/wt/story", undefined, true));
    await waitFor(() => expect(subscribeChangesetMock).toHaveBeenCalled());
    const captured = latestSubscribe()!;
    captured.onError(new Error("locked"));
    await waitFor(() => expect(result.current.error).toBe("locked"));

    forceChangesetMock.mockResolvedValue([]);
    result.current.reload();
    // reload now goes through forceChangeset, bypassing the debounce window.
    expect(forceChangesetMock).toHaveBeenCalledWith({ kind: "local", cwd: "/wt/story" }, undefined);
  });

  it("retrying an errored list shows loading until the retry settles", async () => {
    const { result } = renderHook(() => useSourceChangeset("/wt/story", undefined, true));
    await waitFor(() => expect(subscribeChangesetMock).toHaveBeenCalled());
    latestSubscribe()!.onError(new Error("locked"));
    await waitFor(() => expect(result.current.loading).toBe(false));

    let reject!: (e: unknown) => void;
    forceChangesetMock.mockReturnValue(new Promise((_, r) => (reject = r)));
    act(() => result.current.reload());
    expect(result.current.loading).toBe(true);
    await act(async () => reject(new Error("still locked")));
    expect(result.current.loading).toBe(false);
    expect(result.current.error).toBe("still locked");
  });

  it("an older retry failing after a newer one succeeded leaves the newer result", async () => {
    const { result } = renderHook(() => useSourceChangeset("/wt/story", undefined, true));
    await waitFor(() => expect(subscribeChangesetMock).toHaveBeenCalled());
    latestSubscribe()!.onError(new Error("locked"));
    await waitFor(() => expect(result.current.error).toBe("locked"));

    let rejectOld!: (e: unknown) => void;
    forceChangesetMock.mockReturnValueOnce(new Promise((_, r) => (rejectOld = r)));
    act(() => result.current.reload());
    forceChangesetMock.mockResolvedValueOnce([{ path: "a.ts", status: "modified" }]);
    await act(async () => result.current.reload());
    expect(result.current.files).toEqual([{ path: "a.ts", status: "modified" }]);
    expect(result.current.error).toBeNull();
    await act(async () => rejectOld(new Error("stale failure")));
    expect(result.current.error).toBeNull();
    expect(result.current.loading).toBe(false);
  });

  it("a nonce bump ticks the seam's debounce (no direct fetch)", async () => {
    const { rerender } = renderHook(
      ({ n }) => useSourceChangeset("/wt/story", undefined, true, n),
      { initialProps: { n: 0 } },
    );
    await waitFor(() => expect(subscribeChangesetMock).toHaveBeenCalledTimes(1));
    rerender({ n: 1 });
    await waitFor(() => expect(tickChangesetMock).toHaveBeenCalled());
    const call = tickChangesetMock.mock.calls[0];
    expect(call[0]).toEqual({ kind: "local", cwd: "/wt/story" });
    expect(call[1]).toBeUndefined();
  });

  it("a refresh settling after a worktree switch never lands in the new worktree", async () => {
    const { result, rerender } = renderHook(
      ({ wt }) => useSourceChangeset(wt, undefined, true, 0),
      { initialProps: { wt: "/wt/one" } },
    );
    await waitFor(() => expect(subscribeChangesetMock).toHaveBeenCalledTimes(1));
    let resolveOld!: (files: unknown) => void;
    forceChangesetMock.mockReturnValueOnce(new Promise((r) => (resolveOld = r)));
    act(() => result.current.reload());
    rerender({ wt: "/wt/two" });
    await act(async () => resolveOld([{ path: "old.ts", status: "modified" }]));
    expect(result.current.files).toBeNull();
  });

  it("a machine's typed rejection reads as a sentence, never [object Object]", async () => {
    const { result } = renderHook(() => useSourceChangeset("/wt/story", undefined, true));
    await waitFor(() => expect(subscribeChangesetMock).toHaveBeenCalled());
    latestSubscribe()!.onError({ kind: "disconnected" });
    await waitFor(() => expect(result.current.error).toMatch(/reach the machine/));
  });

  it("hard-resets to the skeleton on a worktree switch; new subscribe fires", async () => {
    const { result, rerender } = renderHook(
      ({ wt }) => useSourceChangeset(wt, undefined, true, 0),
      { initialProps: { wt: "/wt/one" } },
    );
    await waitFor(() => expect(subscribeChangesetMock).toHaveBeenCalledTimes(1));
    latestSubscribe()!.onUpdate([{ path: "a.ts", status: "modified" }]);
    await waitFor(() => expect(result.current.files).not.toBeNull());

    rerender({ wt: "/wt/two" });
    // Prior list clears + loading flips; new subscription fires.
    expect(result.current.files).toBeNull();
    expect(result.current.loading).toBe(true);
    await waitFor(() => expect(subscribeChangesetMock).toHaveBeenCalledTimes(2));
    const call = subscribeChangesetMock.mock.calls[1];
    expect(call[0]).toEqual({ kind: "local", cwd: "/wt/two" });
  });
});

describe("useSourceFileRedline (AC-3)", () => {
  it("threads (path, oldPath, baseRef) into the redline call", async () => {
    vi.mocked(sourceFileRedline).mockResolvedValue({
      status: "modified",
      binary: false,
      before: "a",
      after: "b",
    });
    const { result } = renderHook(() =>
      useSourceFileRedline("/wt/story", "new.ts", "old.ts", "feat/SLW-WD-1"),
    );
    await waitFor(() => expect(result.current.redline).not.toBeNull());
    expect(sourceFileRedline).toHaveBeenCalledWith(
      "/wt/story",
      "new.ts",
      "old.ts",
      "feat/SLW-WD-1",
    );
  });

  it("a machine's typed rejection reads as the per-file sentence", async () => {
    vi.mocked(sourceFileRedline).mockRejectedValue({ kind: "boundsExceeded" });
    const { result } = renderHook(() =>
      useSourceFileRedline("/wt/story", "big.bin", undefined, undefined),
    );
    await waitFor(() => expect(result.current.error).toMatch(/over 4 MB/));
  });

  it("does not fetch when no file is selected", () => {
    renderHook(() => useSourceFileRedline("/wt/story", null, undefined, undefined));
    expect(sourceFileRedline).not.toHaveBeenCalled();
  });

  // SLW-DV-5 AC-6a: the open redline survives a live worktree change — the pane
  // is never dropped to a skeleton (and so is never torn down + rebuilt) mid-edit.
  it("keeps the prior redline visible across a live worktree change (stale-while-revalidate)", async () => {
    const first = { status: "modified" as const, binary: false, before: "a", after: "b" };
    vi.mocked(sourceFileRedline).mockResolvedValue(first);
    const { result, rerender } = renderHook(
      ({ n }) => useSourceFileRedline("/wt/story", "f.ts", undefined, undefined, n),
      { initialProps: { n: 0 } },
    );
    await waitFor(() => expect(result.current.redline).toEqual(first));

    const second = { status: "modified" as const, binary: false, before: "a", after: "b2" };
    vi.mocked(sourceFileRedline).mockResolvedValue(second);
    rerender({ n: 1 });

    // Prior redline stays up synchronously — no null blank on the live tick.
    expect(result.current.redline).toEqual(first);
    await waitFor(() => expect(result.current.redline).toEqual(second));
  });

  it("resets the redline to null on a file switch (hard reset)", async () => {
    vi.mocked(sourceFileRedline).mockResolvedValue({
      status: "modified",
      binary: false,
      before: "a",
      after: "b",
    });
    const { result, rerender } = renderHook(
      ({ f }) => useSourceFileRedline("/wt/story", f, undefined, undefined, 0),
      { initialProps: { f: "a.ts" } },
    );
    await waitFor(() => expect(result.current.redline).not.toBeNull());

    vi.mocked(sourceFileRedline).mockResolvedValue({
      status: "modified",
      binary: false,
      before: "c",
      after: "d",
    });
    rerender({ f: "b.ts" });
    // A different file is a hard reset — the prior redline clears to the skeleton.
    expect(result.current.redline).toBeNull();
    await waitFor(() => expect(result.current.redline?.after).toBe("d"));
  });
});
