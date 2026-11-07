// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { createFanoutGate } from "./fanout-gate";

// Flush pending microtasks (a macrotask boundary) so a release→wake→start chain
// has fully settled before we assert.
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

describe("createFanoutGate (RF-perf-14 shared FIFO gate)", () => {
  it("never runs more than the cap concurrently, admits FIFO on release", async () => {
    const gate = createFanoutGate(3);
    const tasks = Array.from({ length: 10 }, () => deferred());
    const running = tasks.map((t) => gate.withSlot(t.run));

    await tick();
    expect(gate.activeCount()).toBe(3);
    expect(tasks.filter((t) => t.started.value).length).toBe(3);

    tasks[0].release();
    await tick();
    expect(gate.activeCount()).toBe(3);
    expect(tasks[3].started.value).toBe(true); // oldest waiter enters next

    tasks.forEach((t) => t.release());
    await Promise.all(running);
    expect(gate.activeCount()).toBe(0);
  });

  it("a single task takes a slot immediately (one-task path unchanged)", async () => {
    const gate = createFanoutGate(3);
    const t = deferred();
    const p = gate.withSlot(t.run);
    await Promise.resolve();
    expect(t.started.value).toBe(true);
    expect(gate.activeCount()).toBe(1);
    t.release();
    await p;
  });

  it("releases the slot even when the task throws (no leak)", async () => {
    const gate = createFanoutGate(1);
    await expect(
      gate.withSlot(async () => {
        throw new Error("boom");
      }),
    ).rejects.toThrow("boom");
    expect(gate.activeCount()).toBe(0);
    const t = deferred();
    const p = gate.withSlot(t.run);
    await Promise.resolve();
    expect(t.started.value).toBe(true);
    t.release();
    await p;
  });

  it("raising the cap wakes exactly the new headroom of oldest waiters", async () => {
    const gate = createFanoutGate(1);
    const tasks = Array.from({ length: 4 }, () => deferred());
    const running = tasks.map((t) => gate.withSlot(t.run));
    await tick();
    expect(gate.activeCount()).toBe(1);

    gate.setCap(3);
    await tick();
    expect(gate.activeCount()).toBe(3); // no over-admit past the new cap

    tasks.forEach((t) => t.release());
    await Promise.all(running);
  });

  it("clamps the cap to a floor of 1 and floors fractional caps", async () => {
    const gate = createFanoutGate(2.9); // floored to 2
    const tasks = Array.from({ length: 3 }, () => deferred());
    const running = tasks.map((t) => gate.withSlot(t.run));
    await tick();
    expect(gate.activeCount()).toBe(2);

    gate.setCap(0); // clamped to 1 (never below 1) — but 2 already active
    // Two are in flight; dropping the cap doesn't preempt them, it just stops
    // NEW admits below the active count. Release one → still saturated at cap 1.
    tasks[0].release();
    await tick();
    expect(gate.activeCount()).toBe(1);

    tasks.forEach((t) => t.release());
    await Promise.all(running);
  });

  it("gate instances have independent state", async () => {
    const a = createFanoutGate(1);
    const b = createFanoutGate(1);
    const ta = deferred();
    const tb = deferred();
    const pa = a.withSlot(ta.run);
    const pb = b.withSlot(tb.run);
    await tick();
    expect(a.activeCount()).toBe(1);
    expect(b.activeCount()).toBe(1); // b is not affected by a's saturation
    ta.release();
    tb.release();
    await Promise.all([pa, pb]);
  });

  it("resetForTest restores the base cap and empties the queue", async () => {
    const gate = createFanoutGate(2);
    gate.setCap(5);
    const t = deferred();
    const p = gate.withSlot(t.run);
    await tick();
    t.release();
    await p;
    gate.resetForTest();
    expect(gate.activeCount()).toBe(0);
    // base cap restored to 2: fire 3, only 2 admit.
    const tasks = Array.from({ length: 3 }, () => deferred());
    const running = tasks.map((x) => gate.withSlot(x.run));
    await tick();
    expect(gate.activeCount()).toBe(2);
    tasks.forEach((x) => x.release());
    await Promise.all(running);
  });
});
