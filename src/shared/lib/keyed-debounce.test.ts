// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { createKeyedDebounce } from "./keyed-debounce";

describe("createKeyedDebounce", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("fires callback once after the window with correct key + payload", () => {
    const cb = vi.fn();
    const d = createKeyedDebounce<{ value: number }>(300, cb);

    d.schedule("repo-a", { value: 1 });
    expect(cb).not.toHaveBeenCalled();

    vi.advanceTimersByTime(300);
    expect(cb).toHaveBeenCalledTimes(1);
    expect(cb).toHaveBeenCalledWith("repo-a", { value: 1 });
  });

  it("coalesces rapid schedules for the same key into one callback with the LAST payload", () => {
    const cb = vi.fn();
    const d = createKeyedDebounce<{ value: number }>(300, cb);

    d.schedule("repo-a", { value: 1 });
    vi.advanceTimersByTime(100);
    d.schedule("repo-a", { value: 2 });
    vi.advanceTimersByTime(100);
    d.schedule("repo-a", { value: 3 });
    vi.advanceTimersByTime(100);
    d.schedule("repo-a", { value: 4 });
    vi.advanceTimersByTime(100);
    d.schedule("repo-a", { value: 5 });

    expect(cb).not.toHaveBeenCalled();

    vi.advanceTimersByTime(300);
    expect(cb).toHaveBeenCalledTimes(1);
    expect(cb).toHaveBeenCalledWith("repo-a", { value: 5 });
  });

  it("fires independent callbacks for different keys", () => {
    const cb = vi.fn();
    const d = createKeyedDebounce<{ value: number }>(300, cb);

    d.schedule("repo-a", { value: 1 });
    d.schedule("repo-b", { value: 2 });
    vi.advanceTimersByTime(300);

    expect(cb).toHaveBeenCalledTimes(2);
    expect(cb).toHaveBeenCalledWith("repo-a", { value: 1 });
    expect(cb).toHaveBeenCalledWith("repo-b", { value: 2 });
  });

  it("cancelAll prevents pending callbacks from firing", () => {
    const cb = vi.fn();
    const d = createKeyedDebounce<{ value: number }>(300, cb);

    d.schedule("repo-a", { value: 1 });
    d.schedule("repo-b", { value: 2 });
    d.cancelAll();
    vi.advanceTimersByTime(300);

    expect(cb).not.toHaveBeenCalled();
  });

  it("scheduling again after firing produces a second callback (timers don't get stuck)", () => {
    const cb = vi.fn();
    const d = createKeyedDebounce<{ value: number }>(300, cb);

    d.schedule("repo-a", { value: 1 });
    vi.advanceTimersByTime(300);
    expect(cb).toHaveBeenCalledTimes(1);

    d.schedule("repo-a", { value: 2 });
    vi.advanceTimersByTime(300);
    expect(cb).toHaveBeenCalledTimes(2);
    expect(cb).toHaveBeenLastCalledWith("repo-a", { value: 2 });
  });

  it("scheduling after cancelAll still works (clears state but doesn't break instance)", () => {
    const cb = vi.fn();
    const d = createKeyedDebounce<{ value: number }>(300, cb);

    d.schedule("repo-a", { value: 1 });
    d.cancelAll();
    d.schedule("repo-a", { value: 2 });
    vi.advanceTimersByTime(300);

    expect(cb).toHaveBeenCalledTimes(1);
    expect(cb).toHaveBeenCalledWith("repo-a", { value: 2 });
  });

  describe("cancel(key)", () => {
    it("cancels only the given key's pending callback, leaving others intact", () => {
      const cb = vi.fn();
      const d = createKeyedDebounce<{ value: number }>(300, cb);

      d.schedule("repo-a", { value: 1 });
      d.schedule("repo-b", { value: 2 });
      d.cancel("repo-a");
      vi.advanceTimersByTime(300);

      expect(cb).toHaveBeenCalledTimes(1);
      expect(cb).toHaveBeenCalledWith("repo-b", { value: 2 });
    });

    it("is a no-op for an unknown key", () => {
      const cb = vi.fn();
      const d = createKeyedDebounce<{ value: number }>(300, cb);

      d.schedule("repo-a", { value: 1 });
      expect(() => d.cancel("nope")).not.toThrow();
      vi.advanceTimersByTime(300);

      expect(cb).toHaveBeenCalledTimes(1);
      expect(cb).toHaveBeenCalledWith("repo-a", { value: 1 });
    });
  });

  describe("flush(key)", () => {
    it("fires the pending payload immediately and clears the timer (trailing mode)", () => {
      const cb = vi.fn();
      const d = createKeyedDebounce<{ value: number }>(300, cb);

      d.schedule("repo-a", { value: 1 });
      d.flush("repo-a");
      expect(cb).toHaveBeenCalledTimes(1);
      expect(cb).toHaveBeenCalledWith("repo-a", { value: 1 });

      // Timer was consumed by the flush — no second fire when the window elapses.
      vi.advanceTimersByTime(300);
      expect(cb).toHaveBeenCalledTimes(1);
    });

    it("is a no-op when nothing is pending for the key", () => {
      const cb = vi.fn();
      const d = createKeyedDebounce<{ value: number }>(300, cb);

      expect(() => d.flush("repo-a")).not.toThrow();
      expect(cb).not.toHaveBeenCalled();
    });
  });

  describe("leading edge mode", () => {
    it("fires immediately on the first schedule (leading), not after the window", () => {
      const cb = vi.fn();
      const d = createKeyedDebounce<{ value: number }>(300, cb, { leading: true });

      d.schedule("repo-a", { value: 1 });
      // Leading edge: fired synchronously, before any timer elapses.
      expect(cb).toHaveBeenCalledTimes(1);
      expect(cb).toHaveBeenCalledWith("repo-a", { value: 1 });
    });

    it("coalesces a sustained burst into a bounded, regular cadence (not once-at-end)", () => {
      const cb = vi.fn();
      const d = createKeyedDebounce<{ value: number }>(300, cb, { leading: true });

      // Drive ~28 events/s (one every ~36ms) for ~1s: a trailing debounce would
      // NEVER fire during this burst. A leading throttle fires at t0 then once
      // per window.
      for (let t = 0; t < 900; t += 36) {
        d.schedule("repo-a", { value: t });
        vi.advanceTimersByTime(36);
      }

      // Leading (t0) + one flush per 300ms window while the burst continues.
      // The exact count is bounded by ceil(elapsed / window), not by event count.
      expect(cb.mock.calls.length).toBeGreaterThan(1);
      expect(cb.mock.calls.length).toBeLessThanOrEqual(5);
    });

    it("fires a trailing flush so the FINAL state is never dropped", () => {
      const cb = vi.fn();
      const d = createKeyedDebounce<{ value: number }>(300, cb, { leading: true });

      d.schedule("repo-a", { value: 1 }); // leading fires value:1
      vi.advanceTimersByTime(50);
      d.schedule("repo-a", { value: 2 }); // coalesced into the window
      vi.advanceTimersByTime(50);
      d.schedule("repo-a", { value: 3 }); // last event of the burst

      // Window elapses with a pending trailing payload → final state lands.
      vi.advanceTimersByTime(300);
      expect(cb).toHaveBeenLastCalledWith("repo-a", { value: 3 });
    });

    it("SW-SY-4 (AC-5): a ~1,690 events/min burst stays bounded, final state lands", () => {
      // The measured launch-minute flood rate (architecture.md). Every burst-path
      // consumer funnels through this one primitive, so bounding it here bounds
      // main-thread work for all of them. A trailing debounce would fire ~0 times
      // during the burst then stampede; the leading throttle fires ≈ once/window.
      const cb = vi.fn();
      const WINDOW = 300;
      const d = createKeyedDebounce<{ n: number }>(WINDOW, cb, { leading: true });

      const EVENTS = 1690;
      const DURATION_MS = 60_000;
      const stepMs = DURATION_MS / EVENTS; // ~35.5ms between events (~28/s)
      for (let i = 0; i < EVENTS; i++) {
        d.schedule("repo", { n: i });
        vi.advanceTimersByTime(stepMs);
      }
      vi.advanceTimersByTime(WINDOW); // let the final trailing flush land

      // Bounded by ceil(duration/window) + leading + final flush — NOT by event
      // count. ~200 fires for 1,690 events: an ~8× reduction, and crucially a
      // REGULAR cadence rather than a starved-then-stampede.
      const fires = cb.mock.calls.length;
      expect(fires).toBeGreaterThan(1);
      expect(fires).toBeLessThanOrEqual(DURATION_MS / WINDOW + 2);
      expect(fires).toBeLessThan(EVENTS / 4);
      // No dropped final state: the last scheduled payload is delivered.
      expect(cb).toHaveBeenLastCalledWith("repo", { n: EVENTS - 1 });
    });

    it("closes the window after a quiet trailing flush (no stuck/looping timer)", () => {
      const cb = vi.fn();
      const d = createKeyedDebounce<{ value: number }>(300, cb, { leading: true });

      d.schedule("repo-a", { value: 1 }); // leading
      vi.advanceTimersByTime(50);
      d.schedule("repo-a", { value: 2 }); // pending trailing
      vi.advanceTimersByTime(300); // trailing flush (value:2), window would re-arm
      vi.advanceTimersByTime(300); // ...but nothing pending → closes
      const afterQuiet = cb.mock.calls.length;

      // A fresh schedule after the burst leads again (window fully closed).
      d.schedule("repo-a", { value: 3 });
      expect(cb).toHaveBeenCalledTimes(afterQuiet + 1);
      expect(cb).toHaveBeenLastCalledWith("repo-a", { value: 3 });
    });
  });
});
