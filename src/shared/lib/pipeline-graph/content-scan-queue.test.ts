// SPDX-License-Identifier: MIT
import { afterEach, describe, expect, it, vi } from "vitest";
import { createContentScanQueue, CONTENT_SCAN_TIMEOUT_MS } from "./content-scan-queue";
import { buildTree } from "./test-fixtures/derived-tree";
const tree = buildTree("");
const flush = async () => {
  for (let i = 0; i < 8; i++) await Promise.resolve();
};
afterEach(() => vi.useRealTimers());
describe("bounded native content scan requests", () => {
  it("shares identical in-flight reads and retains only the latest follow-up", async () => {
    const queue = createContentScanQueue();
    let finish!: (value: typeof tree) => void;
    const first = vi.fn(
      () =>
        new Promise<typeof tree>((resolve) => {
          finish = resolve;
        }),
    );
    const a = queue.request("team-root", "initial", first);
    expect(queue.request("team-root", "initial", first)).toBe(a);
    await flush();
    const b = queue.request("team-root", "refresh-1", async () => tree);
    const replaced = expect(b).rejects.toThrow("superseded");
    const latest = vi.fn(async () => tree);
    const c = queue.request("team-root", "refresh-2", latest);
    await replaced;
    expect(first).toHaveBeenCalledTimes(1);
    expect(latest).not.toHaveBeenCalled();
    finish(tree);
    await a;
    await c;
    expect(latest).toHaveBeenCalledTimes(1);
  });
  it("times out waiting without spawning replacement workers or publishing a late answer", async () => {
    vi.useFakeTimers();
    const queue = createContentScanQueue();
    let finish!: (value: typeof tree) => void;
    const read = vi.fn(
      () =>
        new Promise<typeof tree>((resolve) => {
          finish = resolve;
        }),
    );
    const a = queue.request("team-root", "initial", read);
    const timeout = expect(a).rejects.toThrow("timed out");
    await flush();
    vi.advanceTimersByTime(CONTENT_SCAN_TIMEOUT_MS);
    await timeout;
    for (let i = 0; i < 20; i++)
      await expect(queue.request("team-root", String(i), read)).rejects.toThrow("still finishing");
    expect(read).toHaveBeenCalledTimes(1);
    finish(tree);
    await flush();
    await expect(queue.request("team-root", "retry", async () => tree)).resolves.toBe(tree);
  });
  it("allows different teams to read independently", async () => {
    const queue = createContentScanQueue();
    let finish!: (value: typeof tree) => void;
    const blocked = queue.request(
      "A",
      "first",
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    void blocked.catch(() => undefined);
    await expect(queue.request("B", "first", async () => tree)).resolves.toBe(tree);
    finish(tree);
    await blocked;
  });
});
