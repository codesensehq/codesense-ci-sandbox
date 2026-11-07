// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { createSerialWriteQueue } from "@/shared/stores/serial-write-queue";

describe("serial write queue", () => {
  it("persists rapid choices in dispatch order", async () => {
    const enqueue = createSerialWriteQueue();
    const events: string[] = [];
    let releaseFirst!: () => void;
    const firstGate = new Promise<void>((resolve) => {
      releaseFirst = resolve;
    });

    const first = enqueue(async () => {
      events.push("first-start");
      await firstGate;
      events.push("first-finish");
    });
    const second = enqueue(async () => {
      events.push("second");
    });

    await Promise.resolve();
    expect(events).toEqual(["first-start"]);
    releaseFirst();
    await Promise.all([first, second]);
    expect(events).toEqual(["first-start", "first-finish", "second"]);
  });
});
