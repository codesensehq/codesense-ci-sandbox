// SPDX-License-Identifier: MIT
// @vitest-environment node
import { describe, expect, it } from "vitest";
import { COLLECT_AFTER_BYTES } from "./emulator";
import { TestWorker } from "./test-worker";

const BATCH = 64 * 1024;

/** A worker whose collections are counted. */
function countingWorker() {
  const counted = { collections: 0 };
  let fedTo = 0;
  const worker = new TestWorker({ collect: () => counted.collections++ });
  worker.create(1, { cols: 120, rows: 40 });
  const feed = (bytes: number) => {
    for (let sent = 0; sent < bytes; sent += BATCH) {
      fedTo += BATCH;
      worker.send({ type: "feed", session: 1, seq_end: fedTo, data: "x".repeat(BATCH) });
    }
  };
  return { worker, counted, feed };
}

describe("worker garbage collection", () => {
  it("collects after every few MiB of output", async () => {
    const { worker, counted, feed } = countingWorker();
    feed(3 * COLLECT_AFTER_BYTES);
    await worker.dump(1);
    // Three thresholds of output, plus the dump itself.
    expect(counted.collections).toBe(4);
    // Reading text makes no garbage worth a collection.
    await worker.text(1, "screen");
    expect(counted.collections).toBe(4);
  });

  it("collects after a dump or snapshot", async () => {
    const { worker, counted } = countingWorker();
    await worker.snapshot(1);
    expect(counted.collections).toBe(1);
    await worker.dump(1);
    expect(counted.collections).toBe(2);
  });

  // Negative control: light output alone never pays for a collection.
  it("does not collect below the threshold", async () => {
    const { worker, counted, feed } = countingWorker();
    feed(COLLECT_AFTER_BYTES - BATCH);
    await worker.text(1, "screen");
    expect(counted.collections).toBe(0);
  });
});
