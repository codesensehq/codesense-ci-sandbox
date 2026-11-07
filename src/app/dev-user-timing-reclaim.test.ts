// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

import {
  startDevUserTimingReclaim,
  __resetDevUserTimingReclaimForTest,
  DEV_USER_TIMING_SWEEP_INTERVAL_MS,
} from "./dev-user-timing-reclaim";

// happy-dom exposes `performance` but not the User Timing buffer-clear methods,
// so the suite installs controllable spies on the exact functions the sweep calls.
type ClearSpies = { clearMeasures: ReturnType<typeof vi.fn>; clearMarks: ReturnType<typeof vi.fn> };
type PerfClear = { clearMeasures?: unknown; clearMarks?: unknown };

function installClearSpies(): ClearSpies {
  const clearMeasures = vi.fn();
  const clearMarks = vi.fn();
  // Assign as own properties — happy-dom's performance has no clearMeasures/clearMarks,
  // so this is an add, not an override of a native impl.
  (performance as unknown as ClearSpies).clearMeasures = clearMeasures;
  (performance as unknown as ClearSpies).clearMarks = clearMarks;
  return { clearMeasures, clearMarks };
}

function removeClearMethods(): void {
  delete (performance as unknown as Partial<ClearSpies>).clearMeasures;
  delete (performance as unknown as Partial<ClearSpies>).clearMarks;
}

// Save/restore the real descriptors so the suite never leaves the shared
// `performance` global mutated for other test files in the same worker — even if
// a future happy-dom starts providing these methods natively.
let savedClearMeasures: unknown;
let savedClearMarks: unknown;

function restoreClearMethods(): void {
  if (savedClearMeasures === undefined) delete (performance as PerfClear).clearMeasures;
  else (performance as PerfClear).clearMeasures = savedClearMeasures;
  if (savedClearMarks === undefined) delete (performance as PerfClear).clearMarks;
  else (performance as PerfClear).clearMarks = savedClearMarks;
}

describe("dev user timing reclaim", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    __resetDevUserTimingReclaimForTest();
    savedClearMeasures = (performance as PerfClear).clearMeasures;
    savedClearMarks = (performance as PerfClear).clearMarks;
    removeClearMethods();
  });

  afterEach(() => {
    __resetDevUserTimingReclaimForTest();
    restoreClearMethods();
    vi.useRealTimers();
  });

  it("clears measures AND marks on each interval tick (not immediately)", () => {
    const { clearMeasures, clearMarks } = installClearSpies();
    startDevUserTimingReclaim(1000);

    // No immediate sweep — the buffer starts empty at boot; first clear is at +interval.
    expect(clearMeasures).toHaveBeenCalledTimes(0);
    expect(clearMarks).toHaveBeenCalledTimes(0);

    vi.advanceTimersByTime(1000);
    expect(clearMeasures).toHaveBeenCalledTimes(1);
    expect(clearMarks).toHaveBeenCalledTimes(1);

    vi.advanceTimersByTime(3000);
    expect(clearMeasures).toHaveBeenCalledTimes(4);
    expect(clearMarks).toHaveBeenCalledTimes(4);
  });

  it("is idempotent — a second start does not add a second interval", () => {
    const { clearMeasures } = installClearSpies();
    startDevUserTimingReclaim(1000);
    startDevUserTimingReclaim(1000);

    vi.advanceTimersByTime(1000);
    // One tick → one sweep (not two), proving a single interval is live.
    expect(clearMeasures).toHaveBeenCalledTimes(1);
  });

  it("is a clean no-op when performance.clearMeasures is unavailable, without latching started", () => {
    // No spies installed → clearMeasures absent (the happy-dom / non-DOM case).
    expect(() => startDevUserTimingReclaim(1000)).not.toThrow();
    // Arming was skipped, so installing the methods afterward and ticking does nothing.
    const { clearMeasures } = installClearSpies();
    vi.advanceTimersByTime(5000);
    expect(clearMeasures).toHaveBeenCalledTimes(0);
    // The skipped arm must NOT have latched `started` (the guard returns BEFORE it):
    // a later start, now that the API is present, must still arm the interval.
    startDevUserTimingReclaim(1000);
    vi.advanceTimersByTime(1000);
    expect(clearMeasures).toHaveBeenCalledTimes(1);
  });

  it("reclaims measures without throwing when clearMarks is absent (partial engine)", () => {
    // An engine that ships clearMeasures but not clearMarks must still reclaim the
    // measures (the actual leak) via the optional-chained clearMarks call.
    const clearMeasures = vi.fn();
    (performance as unknown as ClearSpies).clearMeasures = clearMeasures;
    // clearMarks intentionally left absent.
    startDevUserTimingReclaim(1000);
    expect(() => vi.advanceTimersByTime(2000)).not.toThrow();
    expect(clearMeasures).toHaveBeenCalledTimes(2);
  });

  it("stops sweeping after reset", () => {
    const { clearMeasures } = installClearSpies();
    startDevUserTimingReclaim(1000);
    vi.advanceTimersByTime(1000);
    expect(clearMeasures).toHaveBeenCalledTimes(1);

    __resetDevUserTimingReclaimForTest();
    vi.advanceTimersByTime(5000);
    // No further sweeps after the interval is cleared.
    expect(clearMeasures).toHaveBeenCalledTimes(1);
  });

  it("stays infallible when a clear method throws", () => {
    const clearMeasures = vi.fn(() => {
      throw new Error("clear exploded");
    });
    const clearMarks = vi.fn();
    (performance as unknown as ClearSpies).clearMeasures = clearMeasures;
    (performance as unknown as ClearSpies).clearMarks = clearMarks;

    startDevUserTimingReclaim(1000);
    // A throwing sweep must never surface as an uncaught timer error.
    expect(() => vi.advanceTimersByTime(2000)).not.toThrow();
    expect(clearMeasures).toHaveBeenCalledTimes(2);
  });

  it("defaults to the 30s sweep interval constant", () => {
    expect(DEV_USER_TIMING_SWEEP_INTERVAL_MS).toBe(30_000);
    const { clearMeasures } = installClearSpies();
    startDevUserTimingReclaim();
    vi.advanceTimersByTime(DEV_USER_TIMING_SWEEP_INTERVAL_MS);
    expect(clearMeasures).toHaveBeenCalledTimes(1);
  });
});
