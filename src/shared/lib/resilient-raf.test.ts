// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, afterEach } from "vitest";

import {
  resilientRaf,
  cancelResilientRaf,
  RESILIENT_FRAME_FALLBACK_MS,
  type ResilientRafDeps,
} from "./resilient-raf";

/** Build injected deps with spies so a test drives the "visible" vs "hidden"
 *  branch deterministically, without a real document or a live rAF/timer. `raf`
 *  and `setTimer` hand out disjoint positive id ranges so a test can tell which
 *  primitive a handle came from. */
function makeDeps(hidden: boolean): ResilientRafDeps & {
  rafCalls: Array<() => void>;
  timerCalls: Array<() => void>;
} {
  const rafCalls: Array<() => void> = [];
  const timerCalls: Array<() => void> = [];
  return {
    rafCalls,
    timerCalls,
    isHidden: () => hidden,
    raf: (cb) => {
      rafCalls.push(cb);
      return 100 + rafCalls.length; // 101, 102, …
    },
    cancelRaf: vi.fn(),
    setTimer: (cb) => {
      timerCalls.push(cb);
      return 200 + timerCalls.length; // 201, 202, …
    },
    clearTimer: vi.fn(),
  };
}

describe("resilientRaf", () => {
  it("schedules through rAF when the document is visible (zero behavior change)", () => {
    const deps = makeDeps(false);
    const cb = vi.fn();
    const handle = resilientRaf(cb, deps);

    // Exactly ONE primitive is armed — rAF — and NOT the timer (no race, no
    // double-fire). This is the "at most once per tick" guarantee.
    expect(deps.rafCalls).toHaveLength(1);
    expect(deps.timerCalls).toHaveLength(0);
    // rAF handles are returned as-is (positive) so cancel routes back to rAF.
    expect(handle).toBeGreaterThan(0);
    // Not run synchronously — it is scheduled, never invoked inline.
    expect(cb).not.toHaveBeenCalled();

    deps.rafCalls[0]();
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it("falls back to a timer when the document is hidden (rAF is paused by WKWebView)", () => {
    const deps = makeDeps(true);
    const cb = vi.fn();
    const handle = resilientRaf(cb, deps);

    // The whole point: while hidden, forward progress comes from the timer, and
    // rAF is NOT armed (it would never fire). Exactly one primitive, again.
    expect(deps.timerCalls).toHaveLength(1);
    expect(deps.rafCalls).toHaveLength(0);
    // Timer handles are negated so their kind travels with the value.
    expect(handle).toBeLessThan(0);
    expect(cb).not.toHaveBeenCalled();

    deps.timerCalls[0]();
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it("uses the shared fallback delay for the hidden-path timer", () => {
    const setTimer = vi.fn(() => 7);
    const deps: ResilientRafDeps = {
      isHidden: () => true,
      raf: () => 1,
      cancelRaf: vi.fn(),
      setTimer,
      clearTimer: vi.fn(),
    };
    resilientRaf(() => {}, deps);
    expect(setTimer).toHaveBeenCalledWith(expect.any(Function), RESILIENT_FRAME_FALLBACK_MS);
  });
});

describe("cancelResilientRaf", () => {
  it("cancels ONLY the rAF for a visible-branch (positive) handle", () => {
    const deps = makeDeps(false);
    cancelResilientRaf(42, deps);
    expect(deps.cancelRaf).toHaveBeenCalledWith(42);
    expect(deps.clearTimer).not.toHaveBeenCalled();
  });

  it("cancels ONLY the timer for a hidden-branch (negative) handle, un-negating it", () => {
    const deps = makeDeps(true);
    cancelResilientRaf(-42, deps);
    expect(deps.clearTimer).toHaveBeenCalledWith(42);
    expect(deps.cancelRaf).not.toHaveBeenCalled();
  });

  it("round-trips a real hidden handle: cancel targets the exact timer that was armed", () => {
    const deps = makeDeps(true);
    const handle = resilientRaf(() => {}, deps); // negative timer handle
    cancelResilientRaf(handle, deps);
    // The armed timer was 201 (first setTimer call) → handle -201 → clearTimer(201).
    expect(deps.clearTimer).toHaveBeenCalledWith(201);
    expect(deps.cancelRaf).not.toHaveBeenCalled();
  });
});

// The shipping code path — no injected deps — must actually read
// document.visibilityState and pick the right primitive. Every test above injects
// deps, so this is the one case that exercises `defaultDeps`.
describe("resilientRaf / cancelResilientRaf defaults (production wiring)", () => {
  const originalVis = Object.getOwnPropertyDescriptor(document, "visibilityState");
  afterEach(() => {
    if (originalVis) Object.defineProperty(document, "visibilityState", originalVis);
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("uses setTimeout (not rAF) when the real document is hidden, and cancel clears it", () => {
    Object.defineProperty(document, "visibilityState", { value: "hidden", configurable: true });
    const setSpy = vi.spyOn(globalThis, "setTimeout");
    const clearSpy = vi.spyOn(globalThis, "clearTimeout");
    const rafSpy = vi.spyOn(globalThis, "requestAnimationFrame");

    const cb = vi.fn();
    const handle = resilientRaf(cb); // no deps → defaultDeps
    expect(rafSpy).not.toHaveBeenCalled();
    expect(setSpy).toHaveBeenCalledTimes(1);
    expect(handle).toBeLessThan(0);

    cancelResilientRaf(handle);
    expect(clearSpy).toHaveBeenCalledWith(-handle);
  });

  it("uses requestAnimationFrame when the real document is visible", () => {
    Object.defineProperty(document, "visibilityState", { value: "visible", configurable: true });
    const setSpy = vi.spyOn(globalThis, "setTimeout");
    const rafSpy = vi.spyOn(globalThis, "requestAnimationFrame").mockReturnValue(55);

    const handle = resilientRaf(() => {}); // no deps → defaultDeps
    expect(rafSpy).toHaveBeenCalledTimes(1);
    expect(setSpy).not.toHaveBeenCalled();
    expect(handle).toBe(55);
  });
});
