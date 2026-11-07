// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, afterEach } from "vitest";
import { createWorkerLayoutRunner } from "./worker-layout-runner";

// A controllable fake Worker so the worker-path branches (singleton, message
// resolve, error-reset, timeout, postMessage-throw fallback, cancel) can be
// exercised — happy-dom has no real Worker.
class MockWorker {
  listeners: Record<string, ((ev: unknown) => void)[]> = {};
  posted: unknown[] = [];
  terminated = false;
  throwOnPost = false;
  addEventListener(type: string, fn: (ev: unknown) => void) {
    (this.listeners[type] ??= []).push(fn);
  }
  postMessage(msg: unknown) {
    if (this.throwOnPost) throw new Error("DataCloneError");
    this.posted.push(msg);
  }
  terminate() {
    this.terminated = true;
  }
  emit(type: string, ev: unknown) {
    (this.listeners[type] ?? []).forEach((fn) => fn(ev));
  }
}

// The domain used by these tests: input is a plain number, result is that
// number doubled (sync fallback), wire messages carry a requestId + payload.
type Msg = { requestId: number; value: number };
type Res =
  | { requestId: number; ok: true; result: number }
  | { requestId: number; ok: false; error: string };

function makeRunner(opts?: { workers?: MockWorker[]; timeoutMs?: number }) {
  const workers = opts?.workers ?? [];
  const runner = createWorkerLayoutRunner<number, number, Msg, Res>({
    createWorker: () => {
      const w = new MockWorker();
      workers.push(w);
      return w as unknown as Worker;
    },
    toMessage: (requestId, value) => ({ requestId, value }),
    fromMessage: (data) =>
      data.ok
        ? { requestId: data.requestId, ok: true, result: data.result }
        : { requestId: data.requestId, ok: false, error: data.error },
    runSync: (value) => value * 2,
    timeoutMs: opts?.timeoutMs,
    labels: {
      timeout: "test layout timed out",
      workerError: "test layout worker error",
      cloneError: "test layout: response could not be cloned",
    },
  });
  return { runner, workers };
}

describe("createWorkerLayoutRunner — synchronous fallback (no Worker)", () => {
  it("resolves via runSync when Worker is undefined", async () => {
    // No globalThis.Worker in happy-dom → sync path.
    const { runner } = makeRunner();
    await expect(runner.run(21).promise).resolves.toBe(42);
  });

  it("exposes a no-op cancel on the sync path", () => {
    const { runner } = makeRunner();
    const handle = runner.run(1);
    expect(() => handle.cancel()).not.toThrow();
  });
});

describe("createWorkerLayoutRunner — worker path", () => {
  afterEach(() => {
    delete (globalThis as unknown as { Worker?: unknown }).Worker;
    vi.useRealTimers();
  });

  function enableWorker() {
    (globalThis as unknown as { Worker: unknown }).Worker = MockWorker;
  }

  it("posts to the worker and resolves with the parsed result", async () => {
    enableWorker();
    const { runner, workers } = makeRunner();
    const handle = runner.run(5);

    expect(workers).toHaveLength(1);
    const w = workers[0];
    expect(w.posted).toHaveLength(1);
    const requestId = (w.posted[0] as Msg).requestId;

    w.emit("message", { data: { requestId, ok: true, result: 99 } });
    await expect(handle.promise).resolves.toBe(99);
  });

  it("reuses the singleton worker across calls", () => {
    enableWorker();
    const { runner, workers } = makeRunner();
    runner.run(1);
    runner.run(2);
    expect(workers).toHaveLength(1);
    expect(workers[0].posted).toHaveLength(2);
  });

  it("rejects all pending and terminates the worker on error, then re-spawns", async () => {
    enableWorker();
    const { runner, workers } = makeRunner();
    const a = runner.run(1);
    const b = runner.run(2);
    const w = workers[0];

    w.emit("error", { message: "boom" });

    await expect(a.promise).rejects.toThrow("boom");
    await expect(b.promise).rejects.toThrow("boom");
    expect(w.terminated).toBe(true);

    runner.run(3);
    expect(workers).toHaveLength(2);
  });

  it("uses the workerError label when the error event has no message", async () => {
    enableWorker();
    const { runner, workers } = makeRunner();
    const a = runner.run(1);
    workers[0].emit("error", { message: "" });
    await expect(a.promise).rejects.toThrow("test layout worker error");
  });

  it("rejects on timeout when the worker never responds", async () => {
    enableWorker();
    vi.useFakeTimers();
    const { runner } = makeRunner({ timeoutMs: 30_000 });
    const handle = runner.run(1);
    const assertion = expect(handle.promise).rejects.toThrow(/timed out/);
    await vi.advanceTimersByTimeAsync(30_000);
    await assertion;
  });

  it("falls back to sync when postMessage throws (non-cloneable payload)", async () => {
    enableWorker();
    const { runner, workers } = makeRunner();
    runner.run(1); // constructs the worker
    workers[0].throwOnPost = true;
    await expect(runner.run(10).promise).resolves.toBe(20); // sync: 10*2
  });

  it("cancel drops a pending request so a late response is ignored", async () => {
    enableWorker();
    const { runner, workers } = makeRunner();
    const handle = runner.run(1);
    const requestId = (workers[0].posted[0] as Msg).requestId;

    handle.cancel();
    workers[0].emit("message", { data: { requestId, ok: true, result: 7 } });
    const settled = await Promise.race([
      handle.promise.then(() => "resolved").catch(() => "rejected"),
      Promise.resolve("pending"),
    ]);
    expect(settled).toBe("pending");
  });
});
