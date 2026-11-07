// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { createDebouncedAction } from "./debounced-action";

// Mock logger
vi.mock("@/shared/lib/logger", () => ({
  logger: {
    error: vi.fn(),
    warn: vi.fn(),
    info: vi.fn(),
  },
}));

describe("createDebouncedAction", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("calls the function after the delay", async () => {
    const fn = vi.fn().mockResolvedValue(undefined);
    const debounced = createDebouncedAction(fn, 500, "test");

    debounced.trigger();
    expect(fn).not.toHaveBeenCalled();

    vi.advanceTimersByTime(500);
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it("resets timer on repeated triggers", async () => {
    const fn = vi.fn().mockResolvedValue(undefined);
    const debounced = createDebouncedAction(fn, 500, "test");

    debounced.trigger();
    vi.advanceTimersByTime(300);
    debounced.trigger(); // resets
    vi.advanceTimersByTime(300);
    expect(fn).not.toHaveBeenCalled(); // only 300ms since last trigger

    vi.advanceTimersByTime(200);
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it("passes arguments through to the function", async () => {
    const fn = vi.fn().mockResolvedValue(undefined);
    const debounced = createDebouncedAction(fn, 100, "test");

    debounced.trigger("a", 42);
    vi.advanceTimersByTime(100);
    expect(fn).toHaveBeenCalledWith("a", 42);
  });

  it("cancel prevents execution", () => {
    const fn = vi.fn().mockResolvedValue(undefined);
    const debounced = createDebouncedAction(fn, 500, "test");

    debounced.trigger();
    debounced.cancel();
    vi.advanceTimersByTime(500);
    expect(fn).not.toHaveBeenCalled();
  });

  describe("flush", () => {
    it("cancels pending timer and invokes fn immediately", async () => {
      const fn = vi.fn().mockResolvedValue(undefined);
      const debounced = createDebouncedAction(fn, 500, "test");

      debounced.trigger();
      expect(fn).not.toHaveBeenCalled();

      await debounced.flush();
      expect(fn).toHaveBeenCalledTimes(1);

      // The original timer should be cancelled — advancing shouldn't call again
      vi.advanceTimersByTime(500);
      expect(fn).toHaveBeenCalledTimes(1);
    });

    it("returns a resolved promise when no pending timer exists", async () => {
      const fn = vi.fn().mockResolvedValue(undefined);
      const debounced = createDebouncedAction(fn, 500, "test");

      // No trigger called — flush should be a no-op
      await debounced.flush();
      expect(fn).not.toHaveBeenCalled();
    });

    it("awaits a run that already fired (not just the pending timer)", async () => {
      // Regression: flush() must await an in-flight write so a caller that
      // flushes before re-reading shared state never races it (SW-SC-5).
      let settled = false;
      const fn = vi.fn().mockImplementation(
        () =>
          new Promise<void>((r) =>
            setTimeout(() => {
              settled = true;
              r();
            }, 50),
          ),
      );
      const debounced = createDebouncedAction(fn, 100, "test");

      debounced.trigger();
      vi.advanceTimersByTime(100); // timer fires → fn in flight, no pending timer left
      expect(fn).toHaveBeenCalledTimes(1);
      expect(settled).toBe(false);

      const flushed = debounced.flush(); // must await the in-flight run, not resolve early
      let flushDone = false;
      void flushed.then(() => {
        flushDone = true;
      });
      await vi.advanceTimersByTimeAsync(50);
      await flushed;
      expect(settled).toBe(true);
      expect(flushDone).toBe(true);
      expect(fn).toHaveBeenCalledTimes(1); // did NOT re-invoke
    });

    it("serializes an in-flight run with a later one and awaits both, in order", async () => {
      // Two writes to the same target must not reorder, and flush() must
      // await the whole chain (not just the latest run) — SW-SC-5.
      const order: string[] = [];
      const fn = vi.fn().mockImplementation(
        (tag: string) =>
          new Promise<void>((r) =>
            setTimeout(() => {
              order.push(tag);
              r();
            }, 30),
          ),
      );
      const debounced = createDebouncedAction(fn, 100, "test");

      debounced.trigger("A");
      vi.advanceTimersByTime(100); // A's timer fires → A in flight (settles in 30ms)
      debounced.trigger("B"); // queue B while A is still in flight
      const flushed = debounced.flush(); // must await BOTH A and B, A before B

      await vi.advanceTimersByTimeAsync(100);
      await flushed;
      expect(order).toEqual(["A", "B"]); // ordered, neither dropped
    });

    it("returns the promise from the wrapped fn", async () => {
      let resolved = false;
      const fn = vi.fn().mockImplementation(
        () =>
          new Promise<void>((r) =>
            setTimeout(() => {
              resolved = true;
              r();
            }, 10),
          ),
      );
      const debounced = createDebouncedAction(fn, 500, "test");

      debounced.trigger();
      const promise = debounced.flush();
      expect(fn).toHaveBeenCalledTimes(1);
      expect(resolved).toBe(false);

      await vi.advanceTimersByTimeAsync(10);
      await promise;
      expect(resolved).toBe(true);
    });
  });

  it("logs errors with label", async () => {
    const { logger } = await import("@/shared/lib/logger");
    const fn = vi.fn().mockRejectedValue(new Error("boom"));
    const debounced = createDebouncedAction(fn, 100, "myLabel");

    debounced.trigger();
    vi.advanceTimersByTime(100);
    // Let the promise rejection propagate
    await vi.runAllTimersAsync();
    expect(logger.error).toHaveBeenCalledWith("viewer", expect.stringContaining("myLabel failed"));
  });
});

it("strict flush propagates a failed write and allows an explicit retry", async () => {
  const fn = vi.fn().mockRejectedValueOnce(new Error("disk full")).mockResolvedValue(undefined);
  const action = createDebouncedAction(fn, 1000, "notes");
  action.trigger("note", "latest body");
  await expect(action.flushStrict()).rejects.toThrow("disk full");
  await expect(action.flushStrict()).resolves.toBeUndefined();
  expect(fn.mock.calls).toEqual([
    ["note", "latest body"],
    ["note", "latest body"],
  ]);
});
it("strict flush observes failures from an already running autosave", async () => {
  let fail!: (error: Error) => void;
  const action = createDebouncedAction(
    () =>
      new Promise<void>((_, reject) => {
        fail = reject;
      }),
    1000,
    "notes",
  );
  action.trigger();
  const ordinary = action.flush();
  const strict = action.flushStrict();
  fail(new Error("write denied"));
  await expect(ordinary).resolves.toBeUndefined();
  await expect(strict).rejects.toThrow("write denied");
});

it("a successful write to another note cannot hide a failed note body", async () => {
  const writes: string[] = [];
  const action = createDebouncedAction(
    async (note) => {
      writes.push(String(note));
      if (note === "A") throw new Error("A unavailable");
    },
    1000,
    "notes",
    (note) => String(note),
  );
  action.trigger("A", "body A");
  await action.flush();
  action.trigger("B", "body B");
  await action.flush();
  await expect(action.flushStrict()).rejects.toThrow("A unavailable");
  expect(writes).toEqual(["A", "B", "A"]);
});
