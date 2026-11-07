// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { useRecentActivity } from "./use-recent-activity";

describe("useRecentActivity", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("is false on mount (the initial signal is not activity)", () => {
    const { result } = renderHook(({ n }) => useRecentActivity(n, 1000), {
      initialProps: { n: 3 },
    });
    expect(result.current).toBe(false);
  });

  it("goes active on a bump and decays after the idle window", () => {
    const { result, rerender } = renderHook(({ n }) => useRecentActivity(n, 1000), {
      initialProps: { n: 0 },
    });

    rerender({ n: 1 });
    expect(result.current).toBe(true);

    act(() => {
      vi.advanceTimersByTime(999);
    });
    expect(result.current).toBe(true);

    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(result.current).toBe(false);
  });

  it("a sustained burst stays active — each bump restarts the idle timer", () => {
    const { result, rerender } = renderHook(({ n }) => useRecentActivity(n, 1000), {
      initialProps: { n: 0 },
    });

    rerender({ n: 1 });
    act(() => {
      vi.advanceTimersByTime(800);
    });
    rerender({ n: 2 });
    act(() => {
      vi.advanceTimersByTime(800);
    });
    // 1600ms since the first bump, but only 800ms since the last → still active.
    expect(result.current).toBe(true);

    act(() => {
      vi.advanceTimersByTime(1000);
    });
    expect(result.current).toBe(false);
  });

  it("does not go active on an unrelated re-render (unchanged signal)", () => {
    const { result, rerender } = renderHook(({ n }) => useRecentActivity(n, 1000), {
      initialProps: { n: 5 },
    });
    rerender({ n: 5 });
    expect(result.current).toBe(false);
  });
});
