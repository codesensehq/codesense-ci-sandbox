// SPDX-License-Identifier: MIT
import { afterEach, describe, expect, it } from "vitest";
import {
  __resetCreationFanoutForTest,
  activeCreationCount,
  DEFAULT_CREATION_FANOUT_CAP,
  setCreationFanoutCap,
  withCreationSlot,
} from "./creation-fanout";

// A controllable async task: resolves only when `release()` is called, so a
// test can pin N tasks "in flight" and inspect the gate.
// Flush all pending microtasks (a macrotask boundary) so a release→wake→start
// chain — several `.then` hops deep — has fully settled before we assert.
const tick = () => new Promise<void>((r) => setTimeout(r, 0));

function deferred() {
  let release!: () => void;
  const started = { value: false };
  const promise = new Promise<void>((resolve) => {
    release = resolve;
  });
  const run = async () => {
    started.value = true;
    await promise;
  };
  return { run, release, started };
}

afterEach(() => __resetCreationFanoutForTest());

describe("creation-fanout — SLW-RI-6 AC-2/AC-6", () => {
  it("never runs more than the cap concurrently", async () => {
    setCreationFanoutCap(3);
    const tasks = Array.from({ length: 10 }, () => deferred());
    // Fire all 10 through the gate at once (the batch-open burst).
    const running = tasks.map((t) => withCreationSlot(t.run));

    // Let microtasks settle: exactly 3 should have entered.
    await tick();
    expect(activeCreationCount()).toBe(3);
    expect(tasks.filter((t) => t.started.value).length).toBe(3);

    // Release one → exactly one more enters (FIFO), never exceeding the cap.
    tasks[0].release();
    await tick();
    expect(activeCreationCount()).toBe(3);
    expect(tasks[3].started.value).toBe(true);

    // Drain the rest so the promises settle.
    tasks.forEach((t) => t.release());
    await Promise.all(running);
    expect(activeCreationCount()).toBe(0);
  });

  it("a single open takes a slot immediately (one-open path unchanged)", async () => {
    const t = deferred();
    const p = withCreationSlot(t.run);
    await Promise.resolve();
    expect(t.started.value).toBe(true);
    expect(activeCreationCount()).toBe(1);
    t.release();
    await p;
  });

  it("releases the slot even when the task throws (no leak)", async () => {
    setCreationFanoutCap(1);
    await expect(
      withCreationSlot(async () => {
        throw new Error("setup failed");
      }),
    ).rejects.toThrow("setup failed");
    expect(activeCreationCount()).toBe(0);
    // The slot is free again — the next creation runs.
    const t = deferred();
    const p = withCreationSlot(t.run);
    await Promise.resolve();
    expect(t.started.value).toBe(true);
    t.release();
    await p;
  });

  it("raising the cap wakes queued creations", async () => {
    setCreationFanoutCap(1);
    const tasks = Array.from({ length: 4 }, () => deferred());
    const running = tasks.map((t) => withCreationSlot(t.run));
    await tick();
    expect(activeCreationCount()).toBe(1);

    // Widen the ceiling → the queued creations slot in up to the new cap.
    setCreationFanoutCap(3);
    await tick();
    expect(activeCreationCount()).toBe(3);

    tasks.forEach((t) => t.release());
    await Promise.all(running);
  });

  it("defaults to the launch-batch-mirrored cap", () => {
    expect(DEFAULT_CREATION_FANOUT_CAP).toBe(8);
  });
});
