// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

import { promiseWithTimeout } from "./promise-with-timeout";

describe("promiseWithTimeout", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("resolves with the promise value when it settles before the timeout", async () => {
    const onTimeout = vi.fn(() => "fallback");
    const p = promiseWithTimeout(Promise.resolve("real"), 1000, onTimeout);
    await expect(p).resolves.toBe("real");
    expect(onTimeout).not.toHaveBeenCalled();
  });

  it("rejects with the promise error when it rejects before the timeout", async () => {
    const onTimeout = vi.fn(() => "fallback");
    const err = new Error("boom");
    const p = promiseWithTimeout(Promise.reject(err), 1000, onTimeout);
    await expect(p).rejects.toBe(err);
    expect(onTimeout).not.toHaveBeenCalled();
  });

  it("resolves with the onTimeout fallback when the timeout elapses first", async () => {
    const onTimeout = vi.fn(() => "fallback");
    let resolveNever!: (v: string) => void;
    const never = new Promise<string>((r) => {
      resolveNever = r;
    });
    const p = promiseWithTimeout(never, 1000, onTimeout);
    vi.advanceTimersByTime(1000);
    await expect(p).resolves.toBe("fallback");
    expect(onTimeout).toHaveBeenCalledTimes(1);
    // Settling the underlying promise afterwards must not double-resolve or throw.
    resolveNever("late");
  });

  it("clears the timer once the promise settles (fallback never fires after resolve)", async () => {
    const onTimeout = vi.fn(() => "fallback");
    const p = promiseWithTimeout(Promise.resolve("real"), 1000, onTimeout);
    await expect(p).resolves.toBe("real");
    vi.advanceTimersByTime(5000);
    expect(onTimeout).not.toHaveBeenCalled();
  });

  it("rejects with the error thrown by onTimeout (skip-on-timeout pattern)", async () => {
    const never = new Promise<string>(() => {});
    const p = promiseWithTimeout(never, 1000, () => {
      throw new Error("timed out");
    });
    vi.advanceTimersByTime(1000);
    await expect(p).rejects.toThrow("timed out");
  });

  it("does not emit an unhandled rejection when the promise rejects after the timeout won", async () => {
    const onTimeout = vi.fn(() => "fallback");
    let rejectLate!: (e: unknown) => void;
    const late = new Promise<string>((_, rej) => {
      rejectLate = rej;
    });
    const p = promiseWithTimeout(late, 1000, onTimeout);
    vi.advanceTimersByTime(1000);
    await expect(p).resolves.toBe("fallback");
    // Late rejection is already handled internally (attached rejection handler) → no throw.
    expect(() => rejectLate(new Error("late boom"))).not.toThrow();
  });
});
