// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";
import type { SourceCommitGraphEntry, FileRedline } from "@/features/git-sync/lib/types";

const { mockGraph, mockRedline } = vi.hoisted(() => ({
  mockGraph: vi.fn(),
  mockRedline: vi.fn(),
}));

vi.mock("@/features/git-sync/lib/source-redline-source", () => ({
  sourceCommitGraph: mockGraph,
  sourceCommitChangeset: vi.fn(),
  sourceCommitFileRedline: mockRedline,
  // The two working-tree seam fns are imported by use-source-diff too.
  sourceWorktreeChangeset: vi.fn(),
  sourceFileRedline: vi.fn(),
}));

import { useSourceCommitGraph, useSourceCommitRedline } from "./use-source-diff";
import { __resetCommitGraphCache } from "./commit-graph-cache";

const entry = (sha: string): SourceCommitGraphEntry => ({
  sha,
  shortSha: sha.slice(0, 7),
  parents: [],
  refs: [],
  subject: sha,
  author: "t",
  committedAt: 0,
});
// A full page (60) so `hasMore` is true and `loadMore` will fetch.
const fullPage = (prefix: string): SourceCommitGraphEntry[] =>
  Array.from({ length: 60 }, (_, i) => entry(`${prefix}${i}`));

beforeEach(() => {
  __resetCommitGraphCache();
  mockGraph.mockReset();
  mockRedline.mockReset();
});

describe("useSourceCommitGraph — paging + cache (AC-6/AC-7)", () => {
  it("fetches page 1 on open (no cursor) and exposes the entries", async () => {
    mockGraph.mockResolvedValueOnce([entry("A"), entry("B")]); // partial page
    const { result } = renderHook(() => useSourceCommitGraph("ws1", "/wt", 0, true));

    await waitFor(() => expect(result.current.entries).toHaveLength(2));
    expect(mockGraph).toHaveBeenCalledWith("/wt", 60); // no cursor
    expect(result.current.hasMore).toBe(false); // partial page → exhausted
    expect(result.current.loading).toBe(false);
  });

  it("appends an older page on loadMore, resuming after the cursor (AC-7)", async () => {
    mockGraph.mockResolvedValueOnce(fullPage("p1-")); // 60 → hasMore true
    const { result } = renderHook(() => useSourceCommitGraph("ws1", "/wt", 0, true));
    await waitFor(() => expect(result.current.entries).toHaveLength(60));
    expect(result.current.hasMore).toBe(true);

    mockGraph.mockResolvedValueOnce([entry("older")]);
    await act(async () => result.current.loadMore());
    await waitFor(() => expect(result.current.entries).toHaveLength(61));
    // Second call resumes AFTER the last sha of page 1.
    expect(mockGraph).toHaveBeenLastCalledWith("/wt", 60, "p1-59");
    expect(result.current.hasMore).toBe(false);
  });

  it("serves the cache on refocus with the same (wsId, nonce) — no re-walk (AC-6)", async () => {
    mockGraph.mockResolvedValueOnce([entry("A")]);
    const first = renderHook(() => useSourceCommitGraph("ws1", "/wt", 0, true));
    await waitFor(() => expect(first.result.current.entries).toHaveLength(1));
    expect(mockGraph).toHaveBeenCalledTimes(1);
    first.unmount();

    // Re-mount (refocus) with the same key → served from cache, no new fetch.
    const second = renderHook(() => useSourceCommitGraph("ws1", "/wt", 0, true));
    expect(second.result.current.entries.map((e) => e.sha)).toEqual(["A"]);
    expect(mockGraph).toHaveBeenCalledTimes(1);
  });

  it("refetches from the tip when the nonce bumps (RD-5)", async () => {
    mockGraph.mockResolvedValueOnce([entry("A")]);
    const { result, rerender } = renderHook(
      ({ n }) => useSourceCommitGraph("ws1", "/wt", n, true),
      {
        initialProps: { n: 0 },
      },
    );
    await waitFor(() => expect(result.current.entries.map((e) => e.sha)).toEqual(["A"]));

    mockGraph.mockResolvedValueOnce([entry("A2"), entry("A")]); // new commit at tip
    rerender({ n: 1 });
    await waitFor(() => expect(result.current.entries.map((e) => e.sha)).toEqual(["A2", "A"]));
    // The refetch is a fresh page-1 (no cursor), not a cursor-resume.
    expect(mockGraph).toHaveBeenLastCalledWith("/wt", 60);
  });

  it("does not fetch when disabled", () => {
    renderHook(() => useSourceCommitGraph("ws1", "/wt", 0, false));
    expect(mockGraph).not.toHaveBeenCalled();
  });
});

describe("useSourceCommitRedline — lazy per sha + file (AC-8/RD-11)", () => {
  it("does not fetch until both a sha and a file are selected", () => {
    const { rerender } = renderHook(
      ({ sha, file }) => useSourceCommitRedline("/wt", sha, file, undefined),
      {
        initialProps: { sha: null as string | null, file: null as string | null },
      },
    );
    expect(mockRedline).not.toHaveBeenCalled();
    rerender({ sha: "abc", file: null });
    expect(mockRedline).not.toHaveBeenCalled();
  });

  it("fetches by sha + file once both are selected", async () => {
    const rl: FileRedline = { status: "modified", binary: false, before: "a", after: "b" };
    mockRedline.mockResolvedValueOnce(rl);
    const { result } = renderHook(() =>
      useSourceCommitRedline("/wt", "abc", "src/x.ts", undefined),
    );
    await waitFor(() => expect(result.current.redline).toEqual(rl));
    expect(mockRedline).toHaveBeenCalledWith("/wt", "abc", "src/x.ts", undefined);
  });
});
