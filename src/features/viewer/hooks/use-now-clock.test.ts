// SPDX-License-Identifier: MIT
import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useNowClock } from "./use-now-clock";

describe("SW-AU-3 · useNowClock", () => {
  afterEach(() => vi.useRealTimers());

  it("advances on the interval", () => {
    vi.useFakeTimers();
    const { result } = renderHook(() => useNowClock(1000));
    const first = result.current;
    act(() => {
      vi.advanceTimersByTime(1000);
    });
    expect(result.current).toBeGreaterThan(first);
  });

  it("does not tick while disabled (empty/idle panel does zero per-second work)", () => {
    vi.useFakeTimers();
    const { result } = renderHook(() => useNowClock(1000, false));
    const first = result.current;
    act(() => {
      vi.advanceTimersByTime(5000);
    });
    expect(result.current).toBe(first);
  });
});
