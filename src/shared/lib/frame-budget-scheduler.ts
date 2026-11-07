// SPDX-License-Identifier: MIT
import { resilientRaf } from "./resilient-raf";

/**
 * SLW-FX-27 (Part A) — a shared, frame-budgeted task scheduler.
 *
 * Problem: opening several worktree/epic workspaces fast mounts N terminal tiles,
 * each doing heavy synchronous work in the same render/effect flush — most
 * expensively a WebGL GPU-context allocation, plus a refit cascade (each new tile
 * re-lays-out the mosaic → every sibling terminal's ResizeObserver fires a
 * `fit()`). N of those in one frame freezes the single JS/webview thread.
 *
 * Fix: funnel that deferrable work through ONE process-shared scheduler that
 * admits at most `maxPerFrame` tasks per animation frame, so the cost spreads
 * across a few frames and the main thread yields between them. Only work that is
 * safe to defer a frame is routed here — the terminal's WebGL attach (optional;
 * xterm renders on its DOM fallback until it lands) and the ResizeObserver refit
 * (idempotent + dimension-guarded). Init/spawn are NOT deferred.
 *
 * Pure + dependency-injected (`raf`, and an optional `now` clock) so it's
 * unit-testable without a live rAF loop; `flush()` drains synchronously for
 * tests and teardown.
 */
export interface FrameBudgetScheduler {
  /** Enqueue a task. It runs on a future frame, at most `maxPerFrame` tasks per
   *  frame (and, when a `frameBudgetMs` deadline is set, at most one over-budget
   *  task per frame), in FIFO order. */
  schedule(task: () => void): void;
  /** Run every queued task synchronously right now (tests / teardown). */
  flush(): void;
  /** Number of tasks still queued (tests). */
  readonly size: number;
}

export interface FrameBudgetOptions {
  /** Injected monotonic clock in ms. Defaults to `performance.now()`. Only read
   *  when `frameBudgetMs` is finite. */
  now?: () => number;
  /** RF-perf-14c Prong 2 — wall-clock budget per frame (ms). After at least one
   *  task has run, the drain stops once this many ms have elapsed since the
   *  frame started, deferring the rest to the next frame — so one heavy task
   *  (e.g. a ~50ms xterm construction) takes its OWN frame instead of pairing
   *  with another and blocking ~100ms in a single frame during a burst of opens.
   *  (This is the ~50ms construction hitch, NOT the multi-second "Preparing…"
   *  freeze — that is the awaited `setup:` subprocess, addressed by RF-perf-14.)
   *  `maxPerFrame` remains a hard count backstop so a run of zero-cost tasks
   *  stays bounded even when the deadline never trips. Defaults to `Infinity`
   *  (count-only; the pre-Prong-2 behavior). */
  frameBudgetMs?: number;
}

export function createFrameBudgetScheduler(
  maxPerFrame: number,
  raf: (cb: () => void) => void = (cb) => {
    requestAnimationFrame(cb);
  },
  opts: FrameBudgetOptions = {},
): FrameBudgetScheduler {
  const now = opts.now ?? (() => performance.now());
  const frameBudgetMs = opts.frameBudgetMs ?? Infinity;
  const deadlineEnabled = frameBudgetMs !== Infinity;
  const queue: Array<() => void> = [];
  let draining = false;

  const drain = () => {
    const frameStart = deadlineEnabled ? now() : 0;
    // Snapshot BOTH bounds up front so work scheduled BY a task in this frame is
    // deferred to the next frame: the count cap (a hard backstop against a run of
    // zero-cost tasks) AND the number of tasks already queued at frame start (so
    // the defer-during-drain invariant holds regardless of which bound governs).
    let budget = Math.max(1, maxPerFrame);
    let processable = queue.length;
    let ranOne = false;
    while (budget > 0 && processable > 0 && queue.length > 0) {
      // Wall-clock deadline: once at least one task has run and the frame's time
      // budget is spent, defer the rest so a single heavy build never pairs with
      // another. Always run ≥1 task per frame (the `ranOne` guard) so a lone
      // over-budget task can't starve; measured from frameStart (relative).
      if (deadlineEnabled && ranOne && now() - frameStart >= frameBudgetMs) break;
      const task = queue.shift()!;
      budget--;
      processable--;
      ranOne = true;
      // A throwing task must not abort the whole drain (one bad tile can't wedge
      // every other tile's deferred work).
      try {
        task();
      } catch {
        // Swallow — the caller owns its own error handling; the scheduler only
        // guarantees forward progress.
      }
    }
    if (queue.length > 0) {
      raf(drain);
    } else {
      draining = false;
    }
  };

  return {
    schedule(task: () => void) {
      queue.push(task);
      if (!draining) {
        draining = true;
        raf(drain);
      }
    },
    flush() {
      // Drain everything synchronously, including tasks scheduled during flush.
      while (queue.length > 0) {
        const task = queue.shift()!;
        try {
          task();
        } catch {
          // See drain().
        }
      }
      draining = false;
    },
    get size() {
      return queue.length;
    },
  };
}

/** RF-perf-14c Prong 2 — per-frame wall-clock budget for terminal mount work:
 *  ~half a 60fps frame (16.7ms), leaving headroom for the React commit + paint
 *  that follow. A ~50ms xterm construction blows this on its own, so it takes
 *  its own frame instead of pairing with another (which blocked ~100ms/frame
 *  during a burst of opens). Tunable if live capture warrants. */
export const TERMINAL_FRAME_BUDGET_MS = 8;

/**
 * The process-shared scheduler every terminal instance funnels its deferrable
 * mount work through, so N tiles opening near-simultaneously share ONE per-frame
 * budget (rather than each racing to alloc a GPU context / refit in the same
 * frame).
 *
 * RF-perf-14c: `maxPerFrame` is now a hard COUNT BACKSTOP (8) that lets a burst
 * of *cheap* deferred tasks (WebGL arm, ResizeObserver refit) pack into a frame,
 * while the {@link TERMINAL_FRAME_BUDGET_MS} deadline guarantees at most one
 * *heavy* build per frame. Together they convert a burst of opens from N/2
 * frames each blocked ~100ms (the old `maxPerFrame = 2` × two 50ms builds) into
 * progressive, tile-by-tile appearance — one construction per frame, main thread
 * yielding between them. A single open still feels instant (its one build runs
 * on the very next frame). `undefined` raf ⇒ the real `requestAnimationFrame`.
 *
 * NB: this smooths the ~50ms tile-mount construction cost; the multi-second
 * "Preparing…" freeze is a separate root cause (the awaited `setup:` subprocess,
 * RF-perf-14). This is deliberately a modest, low-risk responsiveness win.
 */
export const terminalFrameScheduler = createFrameBudgetScheduler(
  8,
  // OC-2: drain through a visibility-resilient frame so mount/refit/atlas work
  // still makes forward progress while WKWebView pauses rAF (document hidden).
  // Falls back to a timer only while hidden; identical to plain rAF when visible.
  resilientRaf,
  { frameBudgetMs: TERMINAL_FRAME_BUDGET_MS },
);
