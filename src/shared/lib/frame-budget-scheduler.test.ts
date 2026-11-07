// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, afterEach } from "vitest";

import { createFrameBudgetScheduler, terminalFrameScheduler } from "./frame-budget-scheduler";
import { RESILIENT_FRAME_FALLBACK_MS } from "./resilient-raf";

// A controllable rAF: captures scheduled callbacks so the test drives frames.
function makeRaf() {
  const frames: Array<() => void> = [];
  const raf = (cb: () => void) => {
    frames.push(cb);
  };
  /** Run exactly one pending frame (the oldest). */
  const tick = () => {
    const cb = frames.shift();
    cb?.();
  };
  return { raf, tick, pending: () => frames.length };
}

describe("createFrameBudgetScheduler (SLW-FX-27 Part A)", () => {
  it("admits at most maxPerFrame tasks per frame, in FIFO order", () => {
    const { raf, tick } = makeRaf();
    const s = createFrameBudgetScheduler(2, raf);
    const ran: number[] = [];
    for (let i = 0; i < 5; i++) s.schedule(() => ran.push(i));

    // Nothing runs until a frame ticks.
    expect(ran).toEqual([]);
    tick(); // frame 1 → 2 tasks
    expect(ran).toEqual([0, 1]);
    tick(); // frame 2 → 2 tasks
    expect(ran).toEqual([0, 1, 2, 3]);
    tick(); // frame 3 → last task
    expect(ran).toEqual([0, 1, 2, 3, 4]);
  });

  it("stops scheduling frames once the queue is empty (no idle rAF churn)", () => {
    const { raf, tick, pending } = makeRaf();
    const s = createFrameBudgetScheduler(2, raf);
    s.schedule(() => {});
    s.schedule(() => {});
    expect(pending()).toBe(1); // one drain frame queued
    tick(); // drains both; queue now empty
    expect(pending()).toBe(0); // did NOT queue another frame
    expect(s.size).toBe(0);
  });

  it("a throwing task does not abort the drain — later tasks still run", () => {
    const { raf, tick } = makeRaf();
    const s = createFrameBudgetScheduler(3, raf);
    const ran: string[] = [];
    s.schedule(() => {
      throw new Error("boom");
    });
    s.schedule(() => ran.push("b"));
    s.schedule(() => ran.push("c"));
    tick();
    expect(ran).toEqual(["b", "c"]);
  });

  it("tasks scheduled during a frame defer to the next frame (strict per-frame bound)", () => {
    const { raf, tick } = makeRaf();
    const s = createFrameBudgetScheduler(2, raf);
    const ran: string[] = [];
    s.schedule(() => {
      ran.push("a");
      // A task that enqueues more work mid-frame must not blow the budget.
      s.schedule(() => ran.push("nested"));
    });
    s.schedule(() => ran.push("b"));
    tick(); // frame 1: a + b (budget 2); "nested" deferred
    expect(ran).toEqual(["a", "b"]);
    tick(); // frame 2: nested
    expect(ran).toEqual(["a", "b", "nested"]);
  });

  it("flush() drains everything synchronously (teardown/tests)", () => {
    const raf = vi.fn(); // never fires
    const s = createFrameBudgetScheduler(1, raf);
    const ran: number[] = [];
    for (let i = 0; i < 4; i++) s.schedule(() => ran.push(i));
    s.flush();
    expect(ran).toEqual([0, 1, 2, 3]);
    expect(s.size).toBe(0);
  });
});

// A controllable monotonic clock: tasks that simulate heavy work call
// `advance(ms)` so the drain's wall-clock deadline sees elapsed time.
function makeClock() {
  let t = 0;
  return {
    now: () => t,
    advance: (ms: number) => {
      t += ms;
    },
  };
}

describe("createFrameBudgetScheduler — wall-clock deadline (RF-perf-14c Prong 2)", () => {
  it("a single over-budget task takes its own frame (no heavy pairing)", () => {
    const { raf, tick } = makeRaf();
    const clock = makeClock();
    // Count backstop 8 (would pair both under the legacy count-only budget), but
    // the 8ms deadline forces one heavy build per frame.
    const s = createFrameBudgetScheduler(8, raf, { now: clock.now, frameBudgetMs: 8 });
    const ran: string[] = [];
    s.schedule(() => {
      ran.push("h1");
      clock.advance(50); // heavy: blows the frame budget
    });
    s.schedule(() => {
      ran.push("h2");
      clock.advance(50);
    });
    tick(); // frame 1: h1 runs, elapsed 50 ≥ 8 → break; h2 deferred
    expect(ran).toEqual(["h1"]);
    tick(); // frame 2: h2 alone
    expect(ran).toEqual(["h1", "h2"]);
  });

  it("packs many cheap tasks into one frame (up to the count backstop)", () => {
    const { raf, tick } = makeRaf();
    const clock = makeClock();
    const s = createFrameBudgetScheduler(8, raf, { now: clock.now, frameBudgetMs: 8 });
    const ran: string[] = [];
    for (const n of ["a", "b", "c", "d"]) s.schedule(() => ran.push(n)); // all ~0ms
    tick(); // all four fit — cheap, under 8ms and under the count of 8
    expect(ran).toEqual(["a", "b", "c", "d"]);
  });

  it("breaks mid-frame once a task blows the deadline, deferring the rest", () => {
    const { raf, tick } = makeRaf();
    const clock = makeClock();
    const s = createFrameBudgetScheduler(8, raf, { now: clock.now, frameBudgetMs: 8 });
    const ran: string[] = [];
    s.schedule(() => ran.push("t1")); // cheap
    s.schedule(() => {
      ran.push("t2");
      clock.advance(50); // now over budget
    });
    s.schedule(() => ran.push("t3")); // must defer
    tick();
    expect(ran).toEqual(["t1", "t2"]);
    tick();
    expect(ran).toEqual(["t1", "t2", "t3"]);
  });

  it("always runs at least one task per frame (deadline measured from frame start, no starvation)", () => {
    const { raf, tick } = makeRaf();
    const clock = makeClock();
    const s = createFrameBudgetScheduler(8, raf, { now: clock.now, frameBudgetMs: 8 });
    const ran: string[] = [];
    clock.advance(1000); // absolute time is already large; deadline is RELATIVE to frame start
    s.schedule(() => ran.push("solo"));
    tick();
    expect(ran).toEqual(["solo"]);
  });

  it("count backstop still bounds a run of zero-cost tasks when the deadline never fires", () => {
    const { raf, tick } = makeRaf();
    const clock = makeClock();
    const s = createFrameBudgetScheduler(2, raf, { now: clock.now, frameBudgetMs: 8 });
    const ran: number[] = [];
    for (let i = 0; i < 5; i++) s.schedule(() => ran.push(i)); // never advance the clock
    tick();
    expect(ran).toEqual([0, 1]); // deadline never trips → count cap of 2 governs
    tick();
    expect(ran).toEqual([0, 1, 2, 3]);
    tick();
    expect(ran).toEqual([0, 1, 2, 3, 4]);
  });

  it("preserves the defer-during-drain invariant under a deadline", () => {
    const { raf, tick } = makeRaf();
    const clock = makeClock();
    const s = createFrameBudgetScheduler(8, raf, { now: clock.now, frameBudgetMs: 8 });
    const ran: string[] = [];
    s.schedule(() => {
      ran.push("a");
      s.schedule(() => ran.push("nested")); // enqueued mid-frame
    });
    s.schedule(() => ran.push("b"));
    tick(); // a + b this frame; nested deferred (snapshotted queue length)
    expect(ran).toEqual(["a", "b"]);
    tick();
    expect(ran).toEqual(["a", "b", "nested"]);
  });

  it("flush() ignores the deadline and drains everything synchronously", () => {
    const clock = makeClock();
    const s = createFrameBudgetScheduler(1, () => {}, { now: clock.now, frameBudgetMs: 8 });
    const ran: number[] = [];
    for (let i = 0; i < 4; i++)
      s.schedule(() => {
        ran.push(i);
        clock.advance(50); // each blows the budget; flush must not stop
      });
    s.flush();
    expect(ran).toEqual([0, 1, 2, 3]);
    expect(s.size).toBe(0);
  });
});

// OC-2: the shared production scheduler must make forward progress while the
// document is hidden — WKWebView pauses rAF, so a plain-rAF drain would strand
// every deferred terminal mount/refit until the window is shown. It is wired to
// the visibility-resilient frame, which falls back to a timer while hidden.
describe("terminalFrameScheduler visibility resilience (OC-2)", () => {
  const original = Object.getOwnPropertyDescriptor(document, "visibilityState");
  afterEach(() => {
    vi.useRealTimers();
    if (original) Object.defineProperty(document, "visibilityState", original);
  });

  it("drains a scheduled task while the document is hidden, via the timer fallback", () => {
    Object.defineProperty(document, "visibilityState", { value: "hidden", configurable: true });
    vi.useFakeTimers();
    const ran = vi.fn();
    terminalFrameScheduler.schedule(ran);
    // Scheduled, never run synchronously (the non-blocking guarantee).
    expect(ran).not.toHaveBeenCalled();
    // Hidden → rAF is paused; only the timer fallback advances it. One tick.
    vi.advanceTimersByTime(RESILIENT_FRAME_FALLBACK_MS + 1);
    expect(ran).toHaveBeenCalledTimes(1);
  });

  it("still honors the per-frame budget under the hidden fallback (no 2×maxPerFrame per tick)", () => {
    Object.defineProperty(document, "visibilityState", { value: "hidden", configurable: true });
    vi.useFakeTimers();
    // The shared scheduler admits at most 8 tasks per frame. Schedule more than
    // that so a single hidden tick can't drain them all — the fallback must NOT
    // let a tick blow the budget (a double-drain would run 9+ here).
    const order: number[] = [];
    for (let i = 0; i < 10; i++) terminalFrameScheduler.schedule(() => order.push(i));
    expect(order).toHaveLength(0); // nothing ran synchronously

    vi.advanceTimersByTime(RESILIENT_FRAME_FALLBACK_MS + 1); // one hidden tick
    expect(order).toHaveLength(8); // budget cap held — exactly maxPerFrame, not 10/16
    expect(order).toEqual([0, 1, 2, 3, 4, 5, 6, 7]); // FIFO preserved

    vi.advanceTimersByTime(RESILIENT_FRAME_FALLBACK_MS + 1); // next tick drains the rest
    expect(order).toEqual([0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
  });
});
