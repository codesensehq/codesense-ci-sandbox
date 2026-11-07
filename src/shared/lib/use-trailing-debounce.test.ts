// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { useTrailingDebounce } from "./use-trailing-debounce";

describe("useTrailingDebounce", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("returns the initial value synchronously", () => {
    const { result } = renderHook(({ v }) => useTrailingDebounce(v, 150), {
      initialProps: { v: "a" },
    });
    expect(result.current).toBe("a");
  });

  it("holds the previous value during the delay window, then catches up", () => {
    const { result, rerender } = renderHook(({ v }) => useTrailingDebounce(v, 150), {
      initialProps: { v: "a" },
    });

    rerender({ v: "b" });
    expect(result.current).toBe("a");

    act(() => {
      vi.advanceTimersByTime(149);
    });
    expect(result.current).toBe("a");

    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(result.current).toBe("b");
  });

  it("coalesces rapid changes — only the settled value propagates", () => {
    const { result, rerender } = renderHook(({ v }) => useTrailingDebounce(v, 150), {
      initialProps: { v: "a" },
    });

    rerender({ v: "b" });
    act(() => {
      vi.advanceTimersByTime(50);
    });
    rerender({ v: "c" });
    act(() => {
      vi.advanceTimersByTime(50);
    });
    rerender({ v: "d" });

    expect(result.current).toBe("a");

    act(() => {
      vi.advanceTimersByTime(149);
    });
    expect(result.current).toBe("a");

    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(result.current).toBe("d");
  });

  it("does not schedule a settle when value is unchanged across rerenders", () => {
    const setTimeoutSpy = vi.spyOn(window, "setTimeout");
    const { rerender } = renderHook(({ v }) => useTrailingDebounce(v, 150), {
      initialProps: { v: "a" },
    });
    setTimeoutSpy.mockClear();

    rerender({ v: "a" });
    rerender({ v: "a" });

    expect(setTimeoutSpy).not.toHaveBeenCalled();
    setTimeoutSpy.mockRestore();
  });

  // SLW-DV-5: maxWait force-flushes so a sustained sub-delay burst can't starve
  // the output (the "live diff freezes while the agent streams a file" case).
  it("force-flushes mid-burst once maxWaitMs elapses (sustained sub-delay changes)", () => {
    let now = 0;
    const nowSpy = vi.spyOn(performance, "now").mockImplementation(() => now);
    const { result, rerender } = renderHook(({ v }) => useTrailingDebounce(v, 150, 400), {
      initialProps: { v: 0 },
    });

    // Bump every 100ms (< the 150ms delay) so a trailing-only debounce would
    // never settle. maxWait=400 must force a flush before the burst ends.
    for (let i = 1; i <= 4; i++) {
      now = i * 100;
      rerender({ v: i });
      act(() => {
        vi.advanceTimersByTime(100);
      });
    }

    // Without maxWait this would still read 0 (never settled); with it, the
    // oldest-pending clamp fired and propagated a mid-burst value.
    expect(result.current).toBe(4);
    nowSpy.mockRestore();
  });

  it("without maxWaitMs a sustained sub-delay burst never settles (baseline)", () => {
    const { result, rerender } = renderHook(({ v }) => useTrailingDebounce(v, 150), {
      initialProps: { v: 0 },
    });
    for (let i = 1; i <= 4; i++) {
      rerender({ v: i });
      act(() => {
        vi.advanceTimersByTime(100);
      });
    }
    expect(result.current).toBe(0); // frozen — the exact gap maxWait closes
  });
});
