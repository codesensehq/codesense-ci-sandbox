// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

import { createRefreshScheduler } from "./refresh-scheduler";

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((r) => (resolve = r));
  return { promise, resolve };
}

const tick = (ms: number) => vi.advanceTimersByTimeAsync(ms);

describe("createRefreshScheduler", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("a settled change reads at once while active, and is kept for later while not", async () => {
    const read = vi.fn(() => Promise.resolve());
    const s = createRefreshScheduler(read, { active: false });
    s.changedNow();
    await tick(5_000);
    expect(read).not.toHaveBeenCalled();
    s.setActive(true);
    await tick(0);
    expect(read).toHaveBeenCalledTimes(1);
    s.changedNow();
    await tick(0);
    expect(read).toHaveBeenCalledTimes(2);
  });

  it("a settled change during a read in flight reads exactly once more, straight after", async () => {
    const first = deferred();
    const read = vi.fn().mockReturnValueOnce(first.promise).mockResolvedValue(undefined);
    const s = createRefreshScheduler(read, { active: true });
    s.changedNow();
    await tick(0);
    s.changedNow();
    s.changedNow();
    await tick(1_000);
    expect(read).toHaveBeenCalledTimes(1);
    first.resolve();
    await tick(0);
    expect(read).toHaveBeenCalledTimes(2);
    await tick(5_000);
    expect(read).toHaveBeenCalledTimes(2);
  });

  it("reads a burst of changes once, after it settles", async () => {
    const read = vi.fn(() => Promise.resolve());
    const s = createRefreshScheduler(read, { active: true });
    for (let i = 0; i < 20; i++) {
      s.markDirty();
      await tick(50);
    }
    expect(read).not.toHaveBeenCalled();
    await tick(400);
    expect(read).toHaveBeenCalledTimes(1);
    await tick(5_000);
    expect(read).toHaveBeenCalledTimes(1);
  });

  it("changes that never stop still read within the max wait", async () => {
    const read = vi.fn(() => Promise.resolve());
    const s = createRefreshScheduler(read, { active: true });
    for (let i = 0; i < 40; i++) {
      s.markDirty();
      await tick(100);
    }
    // 4s of writes every 100ms: a read at least every 2s, never one per write.
    expect(read.mock.calls.length).toBeGreaterThanOrEqual(2);
    expect(read.mock.calls.length).toBeLessThanOrEqual(3);
  });

  it("one read at a time: changes during a read make exactly one more", async () => {
    let gate = deferred();
    const read = vi.fn(() => gate.promise);
    const s = createRefreshScheduler(read, { active: true });
    s.refreshNow();
    await tick(0);
    expect(read).toHaveBeenCalledTimes(1);
    for (let i = 0; i < 10; i++) s.markDirty();
    await tick(3_000);
    expect(read).toHaveBeenCalledTimes(1);
    const first = gate;
    gate = deferred();
    first.resolve();
    await tick(400);
    expect(read).toHaveBeenCalledTimes(2);
    gate.resolve();
    await tick(5_000);
    expect(read).toHaveBeenCalledTimes(2);
  });

  it("inactive never reads; becoming active catches up once", async () => {
    const read = vi.fn(() => Promise.resolve());
    const s = createRefreshScheduler(read, { active: false });
    for (let i = 0; i < 5; i++) s.markDirty();
    await tick(10_000);
    expect(read).not.toHaveBeenCalled();
    s.setActive(true);
    await tick(0);
    expect(read).toHaveBeenCalledTimes(1);
    await tick(5_000);
    expect(read).toHaveBeenCalledTimes(1);
  });

  it("becoming active with nothing missed reads nothing", async () => {
    const read = vi.fn(() => Promise.resolve());
    const s = createRefreshScheduler(read, { active: false });
    s.setActive(true);
    await tick(5_000);
    expect(read).not.toHaveBeenCalled();
  });

  it("going inactive drops a pending read until it comes back", async () => {
    const read = vi.fn(() => Promise.resolve());
    const s = createRefreshScheduler(read, { active: true });
    s.markDirty();
    s.setActive(false);
    await tick(5_000);
    expect(read).not.toHaveBeenCalled();
    s.setActive(true);
    await tick(0);
    expect(read).toHaveBeenCalledTimes(1);
  });

  it("a failed read is not retried in a loop", async () => {
    const read = vi.fn(() => Promise.reject(new Error("git busy")));
    const s = createRefreshScheduler(read, { active: true });
    s.refreshNow();
    await tick(10_000);
    expect(read).toHaveBeenCalledTimes(1);
  });

  it("nothing reads after dispose", async () => {
    const read = vi.fn(() => Promise.resolve());
    const s = createRefreshScheduler(read, { active: true });
    s.markDirty();
    s.dispose();
    await tick(5_000);
    expect(read).not.toHaveBeenCalled();
  });

  it("a minimum interval spaces reads out under writes that never stop", async () => {
    const read = vi.fn(() => Promise.resolve());
    const s = createRefreshScheduler(read, { active: true });
    s.setMinInterval(10_000);
    for (let i = 0; i < 200; i++) {
      s.markDirty();
      await tick(100);
    }
    // 20s of writes every 100ms: one read per 10s, not one per 2s.
    expect(read.mock.calls.length).toBeGreaterThanOrEqual(1);
    expect(read.mock.calls.length).toBeLessThanOrEqual(2);
    // Lifting the floor lets a pending change read once it settles.
    s.markDirty();
    s.setMinInterval(0);
    await tick(400);
    expect(read.mock.calls.length).toBeLessThanOrEqual(3);
    const before = read.mock.calls.length;
    s.refreshNow();
    await tick(0);
    expect(read).toHaveBeenCalledTimes(before + 1);
  });
});
