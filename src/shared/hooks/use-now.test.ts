// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook, act } from "@testing-library/react";

import { useNow } from "./use-now";

// NOTE: `vi.advanceTimersByTime(ms)` advances the MOCKED `Date.now()` by the same
// amount (vitest fakes Date alongside timers). So we drive the clock purely through
// `advanceTimersByTime` and never combine it additively with `setSystemTime`.
describe("@P1 useNow — one activity-gated clock (SW-GS-25 AC-3)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(0);
  });
  afterEach(() => vi.useRealTimers());

  it("advances every interval while active", () => {
    const { result } = renderHook(() => useNow(true, 1000));
    expect(result.current).toBe(0);
    act(() => vi.advanceTimersByTime(1000)); // clock 0→1000, interval fires
    expect(result.current).toBe(1000);
    act(() => vi.advanceTimersByTime(1000)); // clock 1000→2000
    expect(result.current).toBe(2000);
  });

  it("does NOT tick while inactive (no background timer)", () => {
    vi.setSystemTime(5000);
    const { result } = renderHook(() => useNow(false, 1000));
    const started = result.current;
    act(() => vi.advanceTimersByTime(4000)); // clock advances, but inactive → no setNow
    expect(result.current).toBe(started); // frozen
  });

  it("tears the interval down when active flips false, and re-syncs on true", () => {
    const { result, rerender } = renderHook(({ a }: { a: boolean }) => useNow(a, 1000), {
      initialProps: { a: true },
    });
    act(() => vi.advanceTimersByTime(2000)); // →2000
    expect(result.current).toBe(2000);

    rerender({ a: false });
    act(() => vi.advanceTimersByTime(8000)); // →10000, but no ticks while inactive
    expect(result.current).toBe(2000);

    // Re-activating resumes ticking; `now` advances on the next interval (a caller
    // rendering `now - since` floors the one-tick staleness to "0s", never negative).
    rerender({ a: true });
    expect(result.current).toBe(2000); // not yet ticked
    act(() => vi.advanceTimersByTime(1000)); // →11000
    expect(result.current).toBe(11_000);
  });
});
