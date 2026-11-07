// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { createPtyInputBuffer } from "./pty-input-buffer";

/**
 * A deterministic stand-in for the default `queueMicrotask` flush scheduler, so
 * SW-FX-51 input-batching tests can assert coalescing without racing the
 * microtask queue. `run()` fires the most-recently scheduled flush.
 */
function manualScheduler() {
  let scheduled: (() => void) | null = null;
  return {
    schedule: (flush: () => void) => {
      scheduled = flush;
    },
    run: () => {
      const f = scheduled;
      scheduled = null;
      f?.();
    },
    get pending() {
      return scheduled !== null;
    },
  };
}

/**
 * SW-EW-39 (AC-1/AC-6b): keystrokes typed during the spawn/attach window must be
 * buffered in order and flushed once the PTY exists — never silently dropped.
 *
 * SW-FX-51 (AC-4): ready-egress keystrokes coalesce into fewer IPC invokes via a
 * flush window; a single idle keystroke still flushes with no perceptible delay.
 */
describe("createPtyInputBuffer", () => {
  it("holds input while not ready, then flushes it IN ORDER on markReady", () => {
    const send = vi.fn();
    const buf = createPtyInputBuffer(send);

    buf.write("l");
    buf.write("s");
    buf.write("\r");
    // Nothing reaches the PTY until the session exists.
    expect(send).not.toHaveBeenCalled();

    buf.markReady();

    // Pre-ready hold flushes individually, in exact order (SW-EW-39 unchanged).
    expect(send.mock.calls.map((c) => c[0])).toEqual(["l", "s", "\r"]);
  });

  it("markReady is idempotent — a second call neither re-flushes nor throws", () => {
    const send = vi.fn();
    const buf = createPtyInputBuffer(send);
    buf.write("x");
    buf.markReady();
    buf.markReady();

    expect(send).toHaveBeenCalledTimes(1);
    expect(send).toHaveBeenCalledWith("x");
  });

  it("interleaves correctly: pre-ready held (individual), post-ready coalesced", () => {
    const send = vi.fn();
    const sched = manualScheduler();
    const buf = createPtyInputBuffer(send, { scheduleFlush: sched.schedule });

    buf.write("1"); // held
    buf.write("2"); // held
    buf.markReady(); // flush 1,2 individually, synchronously
    expect(send.mock.calls.map((c) => c[0])).toEqual(["1", "2"]);

    buf.write("3"); // ready → coalesced batch, not sent yet
    expect(send).toHaveBeenCalledTimes(2);
    sched.run(); // flush window elapses
    expect(send.mock.calls.map((c) => c[0])).toEqual(["1", "2", "3"]);
  });

  it("dispose drops held input WITHOUT sending (viewport torn down before ready)", () => {
    const send = vi.fn();
    const buf = createPtyInputBuffer(send);

    buf.write("typed-then-closed");
    buf.dispose();
    // A late markReady must not resurrect the dropped input.
    buf.markReady();

    expect(send).not.toHaveBeenCalled();
  });

  it("dispose also rejects future writes (no send to a gone viewport)", () => {
    const send = vi.fn();
    const buf = createPtyInputBuffer(send);
    buf.markReady();
    buf.dispose();

    buf.write("after-dispose");

    expect(send).not.toHaveBeenCalled();
  });

  it("markReady(false) DISCARDS held input but still goes ready (agent-launch spawn)", () => {
    const send = vi.fn();
    const sched = manualScheduler();
    const buf = createPtyInputBuffer(send, { scheduleFlush: sched.schedule });

    buf.write("pre-launch-noise"); // would race the injected launch line
    buf.markReady(false); // discard, don't flush
    buf.write("post-launch"); // but the buffer IS ready now — coalesced
    sched.run();

    expect(send.mock.calls.map((c) => c[0])).toEqual(["post-launch"]);
  });

  // ── SW-FX-51 input batching ──────────────────────────────────────────────

  it("coalesces a same-window ready burst into ONE send (fewer IPC invokes)", () => {
    const send = vi.fn();
    const sched = manualScheduler();
    const buf = createPtyInputBuffer(send, { scheduleFlush: sched.schedule });
    buf.markReady();

    buf.write("g");
    buf.write("i");
    buf.write("t");
    // Coalesced — nothing sent until the flush window elapses.
    expect(send).not.toHaveBeenCalled();

    sched.run();
    expect(send).toHaveBeenCalledTimes(1);
    expect(send).toHaveBeenCalledWith("git");
  });

  it("schedules exactly one flush per burst (no per-keystroke scheduling)", () => {
    const send = vi.fn();
    const schedule = vi.fn();
    const buf = createPtyInputBuffer(send, { scheduleFlush: schedule });
    buf.markReady();

    buf.write("a");
    buf.write("b");
    buf.write("c");

    expect(schedule).toHaveBeenCalledTimes(1);
  });

  it("a single idle keystroke flushes on the very next window (no perceptible delay)", () => {
    const send = vi.fn();
    const sched = manualScheduler();
    const buf = createPtyInputBuffer(send, { scheduleFlush: sched.schedule });
    buf.markReady();

    buf.write("k");
    expect(sched.pending).toBe(true); // flush already scheduled immediately
    sched.run();
    expect(send).toHaveBeenCalledExactlyOnceWith("k");
  });

  it("dispose makes a pending scheduled flush a no-op (no send to a gone viewport)", () => {
    const send = vi.fn();
    const sched = manualScheduler();
    const buf = createPtyInputBuffer(send, { scheduleFlush: sched.schedule });
    buf.markReady();

    buf.write("z"); // batched
    buf.dispose();
    sched.run(); // stale flush fires after teardown

    expect(send).not.toHaveBeenCalled();
  });

  it("defaults to a microtask flush window when no scheduler is injected", async () => {
    const send = vi.fn();
    const buf = createPtyInputBuffer(send);
    buf.markReady();

    buf.write("m");
    buf.write("n");
    expect(send).not.toHaveBeenCalled(); // not synchronous

    await Promise.resolve(); // let the microtask run
    expect(send).toHaveBeenCalledExactlyOnceWith("mn");
  });
});
