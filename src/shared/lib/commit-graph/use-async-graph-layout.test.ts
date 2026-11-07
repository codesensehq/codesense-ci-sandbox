// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";
import type { CommitLaneInput, CommitGraphLayout } from "./layout";

const { perf, mockRunGraphLayout } = vi.hoisted(() => ({
  perf: { layoutInWorker: false } as { layoutInWorker: boolean },
  mockRunGraphLayout: vi.fn(),
}));

vi.mock("@/shared/lib/viewer-perf-config", () => ({ VIEWER_PERF: perf }));
vi.mock("./run-graph-layout", () => ({ runGraphLayout: mockRunGraphLayout }));

import { useAsyncGraphLayout } from "./use-async-graph-layout";

const commits = (): CommitLaneInput[] => [
  { sha: "A", parents: ["B"] },
  { sha: "B", parents: [] },
];
const laidOut = (w: number): CommitGraphLayout => ({
  rows: [
    { sha: "A", lane: 0, parentLanes: [0], incoming: [] },
    { sha: "B", lane: 0, parentLanes: [], incoming: ["B"] },
  ],
  width: w,
});

beforeEach(() => {
  perf.layoutInWorker = false;
  mockRunGraphLayout.mockReset();
});

describe("useAsyncGraphLayout — synchronous path (worker off)", () => {
  it("lays out immediately, never pending, never calls the runner", () => {
    const { result } = renderHook(() => useAsyncGraphLayout(commits()));
    expect(result.current.layoutPending).toBe(false);
    expect(result.current.layout.rows).toHaveLength(2);
    expect(mockRunGraphLayout).not.toHaveBeenCalled();
  });

  it("returns empty (not pending) for no commits", () => {
    const { result } = renderHook(() => useAsyncGraphLayout([]));
    expect(result.current.layoutPending).toBe(false);
    expect(result.current.layout).toEqual({ rows: [], width: 0 });
  });
});

describe("useAsyncGraphLayout — worker path (worker on)", () => {
  beforeEach(() => {
    perf.layoutInWorker = true;
  });

  it("is pending until the worker resolves, then returns the layout", async () => {
    let resolve!: (l: CommitGraphLayout) => void;
    mockRunGraphLayout.mockReturnValue({
      promise: new Promise<CommitGraphLayout>((r) => (resolve = r)),
      cancel: vi.fn(),
    });

    const c = commits(); // stable reference — a new array each render would loop the effect
    const { result } = renderHook(() => useAsyncGraphLayout(c));
    await waitFor(() => expect(mockRunGraphLayout).toHaveBeenCalledTimes(1));
    expect(result.current.layoutPending).toBe(true);
    expect(result.current.layout.rows).toEqual([]);

    await act(async () => resolve(laidOut(1)));
    expect(result.current.layoutPending).toBe(false);
    expect(result.current.layout).toEqual(laidOut(1));
  });

  it("drops a stale layout — the newer input's result wins even if it resolves first", async () => {
    const handles: Array<{ resolve: (l: CommitGraphLayout) => void }> = [];
    mockRunGraphLayout.mockImplementation(() => {
      let resolve!: (l: CommitGraphLayout) => void;
      const promise = new Promise<CommitGraphLayout>((r) => (resolve = r));
      handles.push({ resolve });
      return { promise, cancel: vi.fn() };
    });

    const first = commits();
    const second = commits();
    const { result, rerender } = renderHook(({ c }) => useAsyncGraphLayout(c), {
      initialProps: { c: first },
    });
    await waitFor(() => expect(mockRunGraphLayout).toHaveBeenCalledTimes(1));
    rerender({ c: second });
    await waitFor(() => expect(mockRunGraphLayout).toHaveBeenCalledTimes(2));

    await act(async () => {
      handles[1].resolve(laidOut(2)); // current
      handles[0].resolve(laidOut(9)); // stale
    });
    expect(result.current.layout).toEqual(laidOut(2));
  });

  it("surfaces a retryable failure (not a stuck skeleton) when the worker rejects", async () => {
    mockRunGraphLayout.mockReturnValue({
      promise: Promise.reject(new Error("worker died")),
      cancel: vi.fn(),
    });
    const c = commits(); // stable reference — see note above
    const { result } = renderHook(() => useAsyncGraphLayout(c));
    await waitFor(() => expect(result.current.layoutFailed).toBe(true));
    expect(result.current.layoutPending).toBe(false);
    expect(result.current.layout.rows).toEqual([]);
  });

  it("keeps the previous graph mounted while a re-layout is in flight, then swaps", async () => {
    const handles: Array<{ resolve: (l: CommitGraphLayout) => void }> = [];
    mockRunGraphLayout.mockImplementation(() => {
      let resolve!: (l: CommitGraphLayout) => void;
      const promise = new Promise<CommitGraphLayout>((r) => (resolve = r));
      handles.push({ resolve });
      return { promise, cancel: vi.fn() };
    });

    const first = commits();
    const second = commits();
    const { result, rerender } = renderHook(({ c }) => useAsyncGraphLayout(c), {
      initialProps: { c: first },
    });
    await waitFor(() => expect(mockRunGraphLayout).toHaveBeenCalledTimes(1));
    await act(async () => handles[0].resolve(laidOut(1)));
    expect(result.current.layout).toEqual(laidOut(1));

    rerender({ c: second });
    await waitFor(() => expect(mockRunGraphLayout).toHaveBeenCalledTimes(2));
    // Keep-previous: prior graph stays, flagged refreshing (non-gating).
    expect(result.current.layoutPending).toBe(false);
    expect(result.current.layout).toEqual(laidOut(1));
    expect(result.current.layoutRefreshing).toBe(true);

    await act(async () => handles[1].resolve(laidOut(2)));
    expect(result.current.layout).toEqual(laidOut(2));
    expect(result.current.layoutRefreshing).toBe(false);
  });
});
