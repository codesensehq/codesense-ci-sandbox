// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";

import type { OffsetChange } from "./whitespace-diff";

const { mockRun } = vi.hoisted(() => ({ mockRun: vi.fn() }));
vi.mock("./run-structural-diff", () => ({ runStructuralDiff: mockRun }));

import { useStructuralDiff, STRUCTURAL_RECOMPUTE_DEBOUNCE_MS } from "./use-structural-diff";

const change = (n: number): OffsetChange => ({ fromA: n, toA: n + 1, fromB: n, toB: n + 1 });

/** A deferred handle the test resolves/rejects by hand. */
function deferred() {
  let resolve!: (c: OffsetChange[]) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<OffsetChange[]>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { handle: { promise, cancel: vi.fn() }, resolve, reject };
}

beforeEach(() => mockRun.mockReset());

describe("useStructuralDiff", () => {
  it("is idle and never calls the worker when disabled", () => {
    const { result } = renderHook(() => useStructuralDiff("a", "b", "typescript", false));
    expect(result.current).toEqual({ changes: null, status: "idle" });
    expect(mockRun).not.toHaveBeenCalled();
  });

  it("reports unsupported (no worker call) for a flat-tree language", () => {
    const { result } = renderHook(() => useStructuralDiff("a", "b", "rust", true));
    expect(result.current).toEqual({ changes: null, status: "unsupported" });
    expect(mockRun).not.toHaveBeenCalled();
  });

  it("computes then resolves ready with the changes for a supported language", async () => {
    const d = deferred();
    mockRun.mockReturnValue(d.handle);
    const { result } = renderHook(() => useStructuralDiff("a", "b", "typescript", true));

    await waitFor(() => expect(mockRun).toHaveBeenCalledTimes(1));
    expect(result.current.status).toBe("computing");
    expect(result.current.changes).toBeNull();

    await act(async () => d.resolve([change(0)]));
    expect(result.current.status).toBe("ready");
    expect(result.current.changes).toEqual([change(0)]);
  });

  it("falls back to failed (line diff) when the worker rejects", async () => {
    const d = deferred();
    mockRun.mockReturnValue(d.handle);
    const { result } = renderHook(() => useStructuralDiff("a", "b", "typescript", true));
    await waitFor(() => expect(mockRun).toHaveBeenCalledTimes(1));

    await act(async () => {
      d.reject(new Error("no worker"));
      await Promise.resolve();
    });
    expect(result.current.status).toBe("failed");
    expect(result.current.changes).toBeNull();
  });

  it("drops a stale result: a content change resets to computing and the old result can't land", async () => {
    const first = deferred();
    const second = deferred();
    mockRun.mockReturnValueOnce(first.handle).mockReturnValueOnce(second.handle);

    const { result, rerender } = renderHook(
      ({ after }) => useStructuralDiff("a", after, "typescript", true),
      { initialProps: { after: "b" } },
    );
    await waitFor(() => expect(mockRun).toHaveBeenCalledTimes(1));

    // Content changes before the first compute lands.
    rerender({ after: "c" });
    await waitFor(() => expect(mockRun).toHaveBeenCalledTimes(2));
    expect(result.current.status).toBe("computing");

    // The STALE first result arrives — it must be ignored (gen guard).
    await act(async () => first.resolve([change(99)]));
    expect(result.current.changes).toBeNull();
    expect(result.current.status).toBe("computing");

    // The current result lands and wins.
    await act(async () => second.resolve([change(1)]));
    expect(result.current).toEqual({ changes: [change(1)], status: "ready" });
  });

  it("debounces a live-edit burst into ONE compute for the settled content (AC-4)", () => {
    vi.useFakeTimers();
    try {
      mockRun.mockReturnValue(deferred().handle);
      const { rerender } = renderHook(
        ({ after }) => useStructuralDiff("a", after, "typescript", true),
        { initialProps: { after: "b" } },
      );
      // Rapid successive edits, each landing before the previous debounce fires.
      act(() => vi.advanceTimersByTime(STRUCTURAL_RECOMPUTE_DEBOUNCE_MS - 50));
      rerender({ after: "bc" });
      act(() => vi.advanceTimersByTime(STRUCTURAL_RECOMPUTE_DEBOUNCE_MS - 50));
      rerender({ after: "bcd" });
      // Still inside the window after the LAST edit → the worker was never asked.
      act(() => vi.advanceTimersByTime(STRUCTURAL_RECOMPUTE_DEBOUNCE_MS - 1));
      expect(mockRun).not.toHaveBeenCalled();
      // Edits go idle → exactly one compute, for the final settled content.
      act(() => vi.advanceTimersByTime(1));
      expect(mockRun).toHaveBeenCalledTimes(1);
      expect(mockRun).toHaveBeenCalledWith(expect.objectContaining({ after: "bcd" }));
    } finally {
      vi.useRealTimers();
    }
  });

  it("cancels a pending compute when it unmounts before the debounce fires", () => {
    vi.useFakeTimers();
    try {
      mockRun.mockReturnValue(deferred().handle);
      const { unmount } = renderHook(() => useStructuralDiff("a", "b", "typescript", true));
      unmount();
      act(() => vi.advanceTimersByTime(STRUCTURAL_RECOMPUTE_DEBOUNCE_MS + 10));
      expect(mockRun).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });
});
