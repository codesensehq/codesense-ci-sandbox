// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

import { safeUnlisten, createListenerGroup } from "./tauri-listeners";

describe("safeUnlisten", () => {
  it("swallows a rejecting unlisten (no unhandled rejection)", () => {
    const rejecting = () => Promise.reject(new Error("unlisten boom"));
    // The rejection is handled internally → must not throw synchronously.
    expect(() => safeUnlisten(rejecting as unknown as () => void)).not.toThrow();
  });

  it("swallows a synchronous throw from the unlisten", () => {
    const throwing = () => {
      throw new Error("sync boom");
    };
    expect(() => safeUnlisten(throwing)).not.toThrow();
  });

  it("invokes the unlisten function exactly once", () => {
    const fn = vi.fn();
    safeUnlisten(fn);
    expect(fn).toHaveBeenCalledTimes(1);
  });
});

describe("createListenerGroup", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("tears down tracked listeners on dispose", async () => {
    const unlistenA = vi.fn();
    const unlistenB = vi.fn();
    const group = createListenerGroup();
    group.track(Promise.resolve(unlistenA));
    group.track(Promise.resolve(unlistenB));

    await vi.advanceTimersByTimeAsync(0); // let the track promises resolve
    group.dispose();

    expect(unlistenA).toHaveBeenCalledTimes(1);
    expect(unlistenB).toHaveBeenCalledTimes(1);
  });

  it("unlistens immediately for a registration that resolves AFTER dispose", async () => {
    const unlisten = vi.fn();
    const group = createListenerGroup();
    group.track(Promise.resolve(unlisten));

    group.dispose(); // dispose before the track promise resolves
    await vi.advanceTimersByTimeAsync(0);

    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it("does not throw when a tracked registration rejects", async () => {
    const group = createListenerGroup();
    group.track(Promise.reject(new Error("listen failed")));
    await vi.advanceTimersByTimeAsync(0);
    expect(() => group.dispose()).not.toThrow();
  });
});
