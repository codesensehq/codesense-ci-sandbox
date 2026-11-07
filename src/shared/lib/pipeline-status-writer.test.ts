// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";

const mockUpdatePipelineStatus = vi.fn();

vi.mock("@/tauri/commands", () => ({
  updatePipelineStatus: (...args: unknown[]) => mockUpdatePipelineStatus(...args),
}));

import { writePipelineStatus } from "./pipeline-status-writer";

/** A promise with externally-controllable settlement, for ordering assertions. */
function deferred<T = void>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe("writePipelineStatus", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("serializes concurrent writes to the SAME file in call order (last intent wins)", async () => {
    const started: string[] = [];
    const completed: string[] = [];
    const gate1 = deferred();

    mockUpdatePipelineStatus.mockImplementation(async (_path: string, status: string) => {
      started.push(status);
      if (status === "in-progress") await gate1.promise;
      completed.push(status);
    });

    // Two rapid writes to the same file.
    const p1 = writePipelineStatus("/s.md", "in-progress");
    const p2 = writePipelineStatus("/s.md", "done");

    // The second write must NOT start until the first completes — it's queued.
    await Promise.resolve();
    expect(started).toEqual(["in-progress"]);

    gate1.resolve();
    await Promise.all([p1, p2]);

    // Ran strictly in call order → disk saw in-progress then done (done last).
    expect(started).toEqual(["in-progress", "done"]);
    expect(completed).toEqual(["in-progress", "done"]);
  });

  it("runs writes to DIFFERENT files in parallel (no cross-file blocking)", async () => {
    const started: string[] = [];
    const gateA = deferred();

    mockUpdatePipelineStatus.mockImplementation(async (path: string) => {
      started.push(path);
      if (path === "/a.md") await gateA.promise;
    });

    const pA = writePipelineStatus("/a.md", "todo");
    const pB = writePipelineStatus("/b.md", "done");

    // B does not wait on A — different file, different chain.
    await Promise.resolve();
    expect(started).toContain("/b.md");

    gateA.resolve();
    await Promise.all([pA, pB]);
  });

  it("a failed write does not block the next queued write for the same file", async () => {
    mockUpdatePipelineStatus
      .mockRejectedValueOnce(new Error("write 1 failed"))
      .mockResolvedValueOnce(undefined);

    const p1 = writePipelineStatus("/s.md", "in-progress");
    const p2 = writePipelineStatus("/s.md", "done");

    // First rejects (caller sees it); second still runs and succeeds.
    await expect(p1).rejects.toThrow("write 1 failed");
    await expect(p2).resolves.toBeUndefined();
    expect(mockUpdatePipelineStatus).toHaveBeenCalledTimes(2);
    expect(mockUpdatePipelineStatus).toHaveBeenLastCalledWith("/s.md", "done");
  });

  it("propagates a single write's outcome to its own caller", async () => {
    mockUpdatePipelineStatus.mockResolvedValue(undefined);
    await expect(writePipelineStatus("/x.md", "review")).resolves.toBeUndefined();
    expect(mockUpdatePipelineStatus).toHaveBeenCalledWith("/x.md", "review");
  });
});
