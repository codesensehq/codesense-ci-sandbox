// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { useDragResize, type DragResizeOptions } from "./use-drag-resize";

// Drives the hook through a real <div> bound to the returned event handlers.
// Each test fires synthetic pointer events on this element so the React event
// system + setPointerCapture path get exercised end-to-end.
function setupHarness(overrides: Partial<DragResizeOptions> = {}) {
  const visualUpdate = vi.fn<(ratio: number) => void>();
  const commit = vi.fn<(ratio: number, release: { viaPointerUp: boolean }) => void>();
  const dragStart = vi.fn<() => void>();
  const onCleanup = vi.fn<() => void>();
  const onTap = vi.fn<() => void>();
  const computeRatio = vi.fn(
    ({
      startRatio,
      startPos,
      currentPos,
      containerSize,
    }: {
      startRatio: number;
      startPos: number;
      currentPos: number;
      containerSize: number;
    }) => {
      if (containerSize <= 0) return startRatio;
      return startRatio + (currentPos - startPos) / containerSize;
    },
  );

  const opts: DragResizeOptions = {
    computeRatio,
    getContainerSize: () => 1000,
    getStartRatio: () => 0.5,
    getPos: (e) => (e as PointerEvent).clientY,
    onDragStart: dragStart,
    onVisualUpdate: visualUpdate,
    onCleanup,
    onCommit: commit,
    onTap,
    ...overrides,
  };

  // Render a wrapper that lets the test swap `opts` at will (simulates
  // mid-drag re-renders from parent state changes — e.g. teamKey swap).
  // `renders` counts hook renders so tests can assert the drag hot-path stays
  // render-free (SW-FX-74: armed only toggles on enter/leave/drag-start).
  const optsHolder = { current: opts };
  let renders = 0;
  const { result, unmount, rerender } = renderHook(() => {
    renders += 1;
    return useDragResize(optsHolder.current);
  });

  function rerenderWithOpts(newOverrides: Partial<DragResizeOptions>) {
    optsHolder.current = { ...optsHolder.current, ...newOverrides };
    rerender();
  }

  // Mount a divider element and bind the hook's React event handlers via
  // direct DOM listeners (since we're outside a React tree). The hook
  // returns SyntheticEvent-shaped callbacks; we translate by passing a
  // minimal compatible object — only the fields the hook actually reads.
  const divider = document.createElement("div");
  document.body.appendChild(divider);

  function dispatch(
    type:
      | "pointerdown"
      | "pointermove"
      | "pointerup"
      | "pointercancel"
      | "pointerenter"
      | "pointerleave",
    clientY: number,
    pointerId = 1,
    button = 0,
  ) {
    // Build a real PointerEvent-shaped object and hand it to the hook's
    // React event handler. The hook only reads {pointerId, clientX,
    // clientY, currentTarget, button}, so a structural fake is sufficient.
    const synthetic = {
      pointerId,
      button,
      clientX: 0,
      clientY,
      currentTarget: divider as unknown as HTMLElement,
      // Real PointerEvent surface — used only if the hook reaches for it.
      nativeEvent: new Event(type),
    } as unknown as React.PointerEvent<HTMLElement>;

    // SW-FX-74: several handlers now flip the armed hover-cursor React state
    // (pointerenter/leave, and disarm-at-drag-start inside pointermove), so wrap
    // every dispatch in act() to flush the re-render before assertions read
    // result.current.cursor. act() only flushes React work — the rAF visual
    // queue is stubbed and still drains via the manual flushFrame() helper, so
    // the existing coalescing/commit tests are unaffected.
    act(() => {
      if (type === "pointerdown") result.current.onPointerDown(synthetic);
      if (type === "pointermove") result.current.onPointerMove(synthetic);
      if (type === "pointerup") result.current.onPointerUp(synthetic);
      if (type === "pointercancel") result.current.onPointerCancel(synthetic);
      if (type === "pointerenter") result.current.onPointerEnter(synthetic);
      if (type === "pointerleave") result.current.onPointerLeave(synthetic);
    });
  }

  return {
    visualUpdate,
    commit,
    dragStart,
    onCleanup,
    onTap,
    computeRatio,
    dispatch,
    rerenderWithOpts,
    divider,
    unmount,
    // SW-FX-74: read the hook's latest return (e.g. the armed `cursor`).
    result: () => result.current,
    // SW-FX-74: hook render count (assert the drag hot-path is render-free).
    renderCount: () => renders,
    cleanup: () => {
      divider.remove();
    },
  };
}

// Replace requestAnimationFrame/cancelAnimationFrame with a controllable
// queue so tests can decide exactly when frames fire.
let rafQueue: Array<() => void> = [];
let rafHandle = 0;

// Save originals so afterEach can restore the prototype to its pristine
// happy-dom state — otherwise a later test in the same worker that
// asserts real pointer-capture behavior gets these stubbed versions.
const originalSetPointerCapture = Element.prototype.setPointerCapture;
const originalReleasePointerCapture = Element.prototype.releasePointerCapture;
const originalHasPointerCapture = Element.prototype.hasPointerCapture;

beforeEach(() => {
  rafQueue = [];
  rafHandle = 0;

  vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
    rafHandle += 1;
    const id = rafHandle;
    rafQueue.push(() => cb(performance.now()));
    return id;
  });

  vi.stubGlobal("cancelAnimationFrame", (_id: number) => {
    // Tests use a simple "flush-all-pending" model — they don't rely on
    // partial cancellation by id. The hook calls cancel() before scheduling
    // a fresh frame, so we drop the most-recently-queued callback.
    rafQueue.pop();
  });

  // Polyfill setPointerCapture / releasePointerCapture for happy-dom.
  Element.prototype.setPointerCapture = vi.fn();
  Element.prototype.releasePointerCapture = vi.fn();
  Element.prototype.hasPointerCapture = vi.fn(() => false);
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  Element.prototype.setPointerCapture = originalSetPointerCapture;
  Element.prototype.releasePointerCapture = originalReleasePointerCapture;
  Element.prototype.hasPointerCapture = originalHasPointerCapture;
  // Guard against a test leaving the hook's selection-guard class behind.
  document.documentElement.classList.remove("dragging");
  // SLW-FX-9: likewise reset the global drag cursor between tests.
  document.body.style.cursor = "";
});

function flushFrame() {
  const pending = rafQueue;
  rafQueue = [];
  for (const cb of pending) cb();
}

describe("useDragResize", () => {
  describe("AC-2: rAF coalescing", () => {
    it("coalesces N pointermoves into one onVisualUpdate per frame", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);

      // Three moves before a single rAF tick.
      h.dispatch("pointermove", 510);
      h.dispatch("pointermove", 520);
      h.dispatch("pointermove", 540);

      expect(h.visualUpdate).not.toHaveBeenCalled();

      flushFrame();

      // Exactly one visual update, with the LATEST ratio.
      expect(h.visualUpdate).toHaveBeenCalledTimes(1);
      // startRatio=0.5, startPos=500, currentPos=540, containerSize=1000
      // → 0.5 + 40/1000 = 0.54
      expect(h.visualUpdate).toHaveBeenLastCalledWith(0.54);

      h.cleanup();
    });

    it("schedules a fresh frame after each flush", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);

      h.dispatch("pointermove", 510);
      flushFrame();
      expect(h.visualUpdate).toHaveBeenCalledTimes(1);

      h.dispatch("pointermove", 520);
      flushFrame();
      expect(h.visualUpdate).toHaveBeenCalledTimes(2);
      expect(h.visualUpdate).toHaveBeenLastCalledWith(0.52);

      h.cleanup();
    });
  });

  describe("AC-3: single commit on pointerup", () => {
    it("calls onCommit exactly once with the final ratio", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);

      h.dispatch("pointermove", 510);
      h.dispatch("pointermove", 540);
      h.dispatch("pointermove", 560);
      flushFrame();

      expect(h.commit).not.toHaveBeenCalled();

      h.dispatch("pointerup", 560);

      expect(h.commit).toHaveBeenCalledTimes(1);
      // 0.5 + 60/1000 = 0.56
      expect(h.commit).toHaveBeenCalledWith(0.56, { viaPointerUp: true });

      h.cleanup();
    });

    it("computes the final ratio from the LAST pointer position, even if no frame has flushed", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);

      h.dispatch("pointermove", 530);
      // No rAF flush — pointerup must still commit the right value.
      h.dispatch("pointerup", 530);

      expect(h.commit).toHaveBeenCalledTimes(1);
      expect(h.commit).toHaveBeenCalledWith(0.53, { viaPointerUp: true });

      h.cleanup();
    });
  });

  describe("AC-4: abort paths", () => {
    it("commits once on pointercancel", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 520);
      h.dispatch("pointercancel", 520);

      expect(h.commit).toHaveBeenCalledTimes(1);
      // The consumer learns the release was an abort (e.g. to skip a snap action).
      expect(h.commit).toHaveBeenCalledWith(0.52, { viaPointerUp: false });

      h.cleanup();
    });

    it("commits once on window.blur mid-drag", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 520);

      window.dispatchEvent(new Event("blur"));

      expect(h.commit).toHaveBeenCalledTimes(1);
      expect(h.commit).toHaveBeenCalledWith(0.52, { viaPointerUp: false });

      h.cleanup();
    });

    it("treats unmount mid-drag as cancellation — fires onCleanup but NOT onCommit", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 520);

      act(() => {
        h.unmount();
      });

      // Unmount tears the consumer down; there is no React state to
      // commit *to*. Hook cleans up listeners and pending rAF and
      // fires onCleanup so visual side effects (CSS vars, global
      // classes) can revert even though no commit lands.
      expect(h.commit).not.toHaveBeenCalled();
      expect(h.onCleanup).toHaveBeenCalledTimes(1);

      h.cleanup();
    });

    it("does NOT double-commit if pointerup arrives after a blur abort", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 520);

      window.dispatchEvent(new Event("blur"));
      expect(h.commit).toHaveBeenCalledTimes(1);

      // A late pointerup must be a no-op.
      h.dispatch("pointerup", 520);
      expect(h.commit).toHaveBeenCalledTimes(1);

      h.cleanup();
    });
  });

  describe("off-divider release (document safety net)", () => {
    it("commits + clears the drag when the pointer is lifted off the divider, with no wedge", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 540); // start the drag
      // Released OFF the element: the element's own pointerup never fires.
      // The document-level safety listener must still commit + tear down.
      document.dispatchEvent(new PointerEvent("pointerup", { pointerId: 1, clientY: 540 }));
      expect(h.commit).toHaveBeenCalledTimes(1);

      // dragRef was cleared (not stranded) → the re-entry guard no longer
      // wedges; a fresh drag starts and commits normally.
      h.dispatch("pointerdown", 600);
      h.dispatch("pointermove", 640);
      h.dispatch("pointerup", 640);
      expect(h.commit).toHaveBeenCalledTimes(2);

      h.cleanup();
    });
  });

  describe("document-level pointermove safety net (edge grab before threshold)", () => {
    it("starts + tracks the drag from a document pointermove when the pointer left the thin divider pre-threshold", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);
      // No element pointermove — the pointer left the thin divider before
      // crossing the threshold. A document-level move must still start the drag.
      act(() => {
        document.dispatchEvent(new PointerEvent("pointermove", { pointerId: 1, clientY: 560 }));
      });
      expect(h.dragStart).toHaveBeenCalledTimes(1);
      flushFrame();
      expect(h.visualUpdate).toHaveBeenCalled();
      h.dispatch("pointerup", 560);
      expect(h.commit).toHaveBeenCalledTimes(1);
      h.cleanup();
    });

    it("stops tracking document pointermoves after the gesture ends (listener removed)", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);
      h.dispatch("pointerup", 500); // tap — no movement, drag never started
      h.dragStart.mockClear();
      act(() => {
        document.dispatchEvent(new PointerEvent("pointermove", { pointerId: 1, clientY: 560 }));
      });
      expect(h.dragStart).not.toHaveBeenCalled();
      h.cleanup();
    });
  });

  describe("selection guard (.dragging) covers the pre-threshold window", () => {
    it("applies .dragging on the first SUB-threshold move, and clears it on the tap release", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);
      // No movement yet → still a potential tap → no guard (no flash on click).
      expect(document.documentElement.classList.contains("dragging")).toBe(false);
      // A 2px drift (< the 4px threshold) must already apply the guard so the
      // browser can't anchor a native selection during the pre-threshold drift.
      h.dispatch("pointermove", 502);
      expect(document.documentElement.classList.contains("dragging")).toBe(true);
      h.dispatch("pointerup", 502); // never crossed threshold → tap
      expect(document.documentElement.classList.contains("dragging")).toBe(false);
      h.cleanup();
    });

    it("never applies .dragging on a pure tap (pointerdown→pointerup, no move)", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);
      h.dispatch("pointerup", 500);
      expect(document.documentElement.classList.contains("dragging")).toBe(false);
      expect(h.onTap).toHaveBeenCalledTimes(1);
      h.cleanup();
    });

    it("clears .dragging on an abort (pointercancel) after movement", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 540);
      expect(document.documentElement.classList.contains("dragging")).toBe(true);
      h.dispatch("pointercancel", 540);
      expect(document.documentElement.classList.contains("dragging")).toBe(false);
      h.cleanup();
    });
  });

  describe("AC-4: zero-movement no-op", () => {
    it("does NOT call onCommit when pointerup arrives with no movement", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);
      h.dispatch("pointerup", 500);

      expect(h.commit).not.toHaveBeenCalled();
      expect(h.visualUpdate).not.toHaveBeenCalled();

      h.cleanup();
    });

    it("does NOT call onCommit when pointerup arrives at the exact start coord after moves cancel out", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 530);
      h.dispatch("pointermove", 500); // back to start
      flushFrame();
      h.dispatch("pointerup", 500);

      // Visual update fired with the latest (post-rollback) ratio == start
      // → that's the same as startRatio, so commit is skipped.
      expect(h.commit).not.toHaveBeenCalled();

      h.cleanup();
    });
  });

  describe("AC-1: zero-container abort", () => {
    it("aborts cleanly when getContainerSize() returns 0", () => {
      const h = setupHarness({ getContainerSize: () => 0 });
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 520);
      flushFrame();
      h.dispatch("pointerup", 520);

      expect(h.visualUpdate).not.toHaveBeenCalled();
      expect(h.commit).not.toHaveBeenCalled();

      h.cleanup();
    });
  });

  describe("listener hygiene", () => {
    it("removes the blur listener after pointerup", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 520);
      h.dispatch("pointerup", 520);

      expect(h.commit).toHaveBeenCalledTimes(1);

      // A later blur must not fire another commit.
      window.dispatchEvent(new Event("blur"));
      expect(h.commit).toHaveBeenCalledTimes(1);

      h.cleanup();
    });

    it("does not fire commit or visual updates on stale moves after release", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 520);
      flushFrame();
      h.dispatch("pointerup", 520);

      // One commit on pointerup, one visual update from the flushed frame.
      expect(h.commit).toHaveBeenCalledTimes(1);
      expect(h.visualUpdate).toHaveBeenCalledTimes(1);

      // Stale moves after release must not touch either callback.
      h.dispatch("pointermove", 540);
      flushFrame();

      expect(h.commit).toHaveBeenCalledTimes(1);
      expect(h.visualUpdate).toHaveBeenCalledTimes(1);

      h.cleanup();
    });
  });

  // The hook's symmetric paired callbacks. onDragStart fires once per
  // *successful* pointer-down (so bailouts don't poison global state);
  // onCleanup fires once on every release path including zero-movement.
  describe("onDragStart / onCleanup symmetry", () => {
    it("fires onDragStart exactly once, deferred until movement crosses the tap threshold", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);
      // Deferred: a bare pointer-down is a potential tap, not yet a drag.
      expect(h.dragStart).not.toHaveBeenCalled();
      // Sub-threshold jitter still does not start the drag.
      h.dispatch("pointermove", 502);
      expect(h.dragStart).not.toHaveBeenCalled();
      // Crossing the threshold starts the drag exactly once.
      h.dispatch("pointermove", 520);
      expect(h.dragStart).toHaveBeenCalledTimes(1);
      h.dispatch("pointermove", 540);
      expect(h.dragStart).toHaveBeenCalledTimes(1);
      h.dispatch("pointerup", 540);
      expect(h.dragStart).toHaveBeenCalledTimes(1);
      h.cleanup();
    });

    it("does NOT fire onDragStart on pointerdown re-entry", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 520); // cross threshold → drag starts
      expect(h.dragStart).toHaveBeenCalledTimes(1);
      // Second pointerdown without releasing → hook should reject.
      h.dispatch("pointerdown", 500, 2);
      expect(h.dragStart).toHaveBeenCalledTimes(1);
      h.cleanup();
    });

    it("does NOT fire onDragStart when getContainerSize() returns 0", () => {
      const h = setupHarness({ getContainerSize: () => 0 });
      h.dispatch("pointerdown", 500);
      expect(h.dragStart).not.toHaveBeenCalled();
      expect(h.onCleanup).not.toHaveBeenCalled();
      h.cleanup();
    });

    it("does NOT fire onDragStart for non-primary buttons (right-click guard)", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500, 1, 2); // button=2 → right-click
      expect(h.dragStart).not.toHaveBeenCalled();
      expect(h.onCleanup).not.toHaveBeenCalled();
      h.cleanup();
    });

    it("fires onCleanup on pointerup with movement (commit path)", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 520);
      h.dispatch("pointerup", 520);
      expect(h.commit).toHaveBeenCalledTimes(1);
      expect(h.onCleanup).toHaveBeenCalledTimes(1);
      h.cleanup();
    });

    it("treats a zero-movement pointerup as a TAP — fires neither onDragStart nor onCleanup", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);
      h.dispatch("pointerup", 500);
      expect(h.commit).not.toHaveBeenCalled();
      // New contract: a zero-movement release never started a drag, so the
      // onDragStart/onCleanup pair both stay silent (symmetry preserved) and
      // the gesture is reported as a tap instead.
      expect(h.dragStart).not.toHaveBeenCalled();
      expect(h.onCleanup).not.toHaveBeenCalled();
      expect(h.onTap).toHaveBeenCalledTimes(1);
      h.cleanup();
    });

    it("fires onCleanup on pointercancel", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 520);
      h.dispatch("pointercancel", 520);
      expect(h.onCleanup).toHaveBeenCalledTimes(1);
      h.cleanup();
    });

    it("fires onCleanup on window.blur", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 520);
      window.dispatchEvent(new Event("blur"));
      expect(h.onCleanup).toHaveBeenCalledTimes(1);
      h.cleanup();
    });

    it("invokes onCommit BEFORE onCleanup (release ordering)", () => {
      const order: string[] = [];
      const h = setupHarness({
        onCommit: () => order.push("commit"),
        onCleanup: () => order.push("cleanup"),
      });
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 520);
      h.dispatch("pointerup", 520);
      expect(order).toEqual(["commit", "cleanup"]);
      h.cleanup();
    });

    it("fires onCleanup even if onCommit throws (try/finally guarantee)", () => {
      const onCleanup = vi.fn();
      const h = setupHarness({
        onCommit: () => {
          throw new Error("boom");
        },
        onCleanup,
      });
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 520);
      // The hook re-throws the consumer's onCommit error, but onCleanup
      // must still run so visual side effects (CSS var, global cursor
      // class) don't get stranded — otherwise the entire app's cursor
      // + text selection freezes until reload.
      expect(() => h.dispatch("pointerup", 520)).toThrow("boom");
      expect(onCleanup).toHaveBeenCalledTimes(1);
      h.cleanup();
    });

    it("invokes onDragStart AFTER setPointerCapture and BEFORE the first onVisualUpdate", () => {
      const order: string[] = [];
      // setPointerCapture is mocked at module level — spy on it to
      // observe ordering. The hook calls capture before snapshot,
      // snapshot before onDragStart.
      const captureSpy = vi.spyOn(Element.prototype, "setPointerCapture");
      captureSpy.mockImplementation(() => {
        order.push("capture");
      });
      const h = setupHarness({
        onDragStart: () => order.push("dragStart"),
        onVisualUpdate: () => order.push("visualUpdate"),
      });
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 520);
      flushFrame();

      expect(order[0]).toBe("capture");
      expect(order[1]).toBe("dragStart");
      expect(order[2]).toBe("visualUpdate");
      captureSpy.mockRestore();
      h.cleanup();
    });
  });

  // Opts snapshot at pointer-down — mid-drag re-renders cannot move
  // the commit target. Critical for the cross-team / teamKey-swap path.
  describe("opts snapshot at pointer-down", () => {
    it("commits using the onCommit captured at pointer-down, NOT the latest one", () => {
      const initial = vi.fn();
      const replacement = vi.fn();
      const h = setupHarness({ onCommit: initial });

      h.dispatch("pointerdown", 500);
      // Parent re-renders with new closures mid-drag (e.g. team switch).
      h.rerenderWithOpts({ onCommit: replacement });
      h.dispatch("pointermove", 520);
      h.dispatch("pointerup", 520);

      expect(initial).toHaveBeenCalledTimes(1);
      expect(initial).toHaveBeenCalledWith(0.52, { viaPointerUp: true });
      expect(replacement).not.toHaveBeenCalled();

      h.cleanup();
    });

    it("calls visual updates and cleanup against the pointer-down snapshot, not the latest opts", () => {
      const initialVisual = vi.fn();
      const initialCleanup = vi.fn();
      const replacementVisual = vi.fn();
      const replacementCleanup = vi.fn();
      const h = setupHarness({
        onVisualUpdate: initialVisual,
        onCleanup: initialCleanup,
      });

      h.dispatch("pointerdown", 500);
      h.rerenderWithOpts({
        onVisualUpdate: replacementVisual,
        onCleanup: replacementCleanup,
      });
      h.dispatch("pointermove", 520);
      flushFrame();
      h.dispatch("pointerup", 520);

      expect(initialVisual).toHaveBeenCalledTimes(1);
      expect(initialCleanup).toHaveBeenCalledTimes(1);
      expect(replacementVisual).not.toHaveBeenCalled();
      expect(replacementCleanup).not.toHaveBeenCalled();

      h.cleanup();
    });

    it("next drag picks up the latest opts (snapshot is per-drag)", () => {
      const initial = vi.fn();
      const replacement = vi.fn();
      const h = setupHarness({ onCommit: initial });

      // First drag uses `initial`.
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 520);
      h.dispatch("pointerup", 520);
      expect(initial).toHaveBeenCalledTimes(1);

      // Re-render BETWEEN drags. Next drag should snapshot the new opts.
      h.rerenderWithOpts({ onCommit: replacement });
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 530);
      h.dispatch("pointerup", 530);

      expect(initial).toHaveBeenCalledTimes(1);
      expect(replacement).toHaveBeenCalledTimes(1);
      expect(replacement).toHaveBeenCalledWith(0.53, { viaPointerUp: true });

      h.cleanup();
    });
  });

  // SW-FX-8 T1: a pointer-down → pointer-up with sub-threshold movement is a
  // TAP (click), not a drag. Taps fire `onTap` and nothing else — no
  // onDragStart, no onVisualUpdate, no onCommit, no onCleanup. This is what
  // lets a divider be BOTH a resize handle (drag) and a collapse toggle (click).
  describe("tap detection (click vs drag)", () => {
    it("fires onTap on a zero-movement pointerup and skips the drag lifecycle", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);
      h.dispatch("pointerup", 500);

      expect(h.onTap).toHaveBeenCalledTimes(1);
      expect(h.dragStart).not.toHaveBeenCalled();
      expect(h.visualUpdate).not.toHaveBeenCalled();
      expect(h.commit).not.toHaveBeenCalled();
      expect(h.onCleanup).not.toHaveBeenCalled();

      h.cleanup();
    });

    it("treats sub-threshold jitter (< 4px) as a tap", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 503); // 3px < default 4px threshold
      h.dispatch("pointerup", 503);

      expect(h.onTap).toHaveBeenCalledTimes(1);
      expect(h.dragStart).not.toHaveBeenCalled();
      expect(h.commit).not.toHaveBeenCalled();

      h.cleanup();
    });

    it("does NOT fire onTap once movement crosses the threshold (it's a drag)", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 520); // 20px ≥ threshold → drag
      h.dispatch("pointerup", 520);

      expect(h.onTap).not.toHaveBeenCalled();
      expect(h.dragStart).toHaveBeenCalledTimes(1);
      expect(h.commit).toHaveBeenCalledTimes(1);
      expect(h.onCleanup).toHaveBeenCalledTimes(1);

      h.cleanup();
    });

    it("does NOT fire onTap when a started drag returns to its origin (still a drag-release)", () => {
      const h = setupHarness();
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 530); // crosses threshold → drag started
      h.dispatch("pointermove", 500); // back to origin
      h.dispatch("pointerup", 500);

      expect(h.onTap).not.toHaveBeenCalled();
      expect(h.dragStart).toHaveBeenCalledTimes(1);
      expect(h.commit).not.toHaveBeenCalled(); // origin == start → no commit
      expect(h.onCleanup).toHaveBeenCalledTimes(1); // but cleanup runs (drag started)

      h.cleanup();
    });

    it("does NOT fire onTap on pointercancel, window.blur, or unmount", () => {
      const cancel = setupHarness();
      cancel.dispatch("pointerdown", 500);
      cancel.dispatch("pointercancel", 500);
      expect(cancel.onTap).not.toHaveBeenCalled();
      cancel.cleanup();

      const blur = setupHarness();
      blur.dispatch("pointerdown", 500);
      window.dispatchEvent(new Event("blur"));
      expect(blur.onTap).not.toHaveBeenCalled();
      blur.cleanup();

      const unmounted = setupHarness();
      unmounted.dispatch("pointerdown", 500);
      act(() => {
        unmounted.unmount();
      });
      expect(unmounted.onTap).not.toHaveBeenCalled();
      unmounted.cleanup();
    });

    it("respects a custom tapThresholdPx", () => {
      const h = setupHarness({ tapThresholdPx: 50 });
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 540); // 40px < 50 → still a tap
      h.dispatch("pointerup", 540);
      expect(h.onTap).toHaveBeenCalledTimes(1);
      expect(h.dragStart).not.toHaveBeenCalled();
      h.cleanup();

      const h2 = setupHarness({ tapThresholdPx: 50 });
      h2.dispatch("pointerdown", 500);
      h2.dispatch("pointermove", 560); // 60px ≥ 50 → drag
      h2.dispatch("pointerup", 560);
      expect(h2.onTap).not.toHaveBeenCalled();
      expect(h2.dragStart).toHaveBeenCalledTimes(1);
      h2.cleanup();
    });
  });

  // SLW-FX-9: the global body cursor is the deterministic, capture-independent
  // carrier of the resize cursor during a drag. Set on the same threshold-cross
  // that starts the drag; cleared in releaseInternals on EVERY teardown path, so
  // a stranded WebKit pointer-capture (releasePointerCapture throwing/failing)
  // can never leave the `↕`/`↔` cursor stuck after the drag ends.
  describe("SLW-FX-9: dragCursor global body cursor lifecycle", () => {
    it("sets document.body.style.cursor on threshold-cross, not on bare pointer-down", () => {
      const h = setupHarness({ dragCursor: "row-resize" });
      expect(document.body.style.cursor).toBe("");
      h.dispatch("pointerdown", 500);
      // A bare pointer-down is a potential tap — no cursor yet (no click flash).
      expect(document.body.style.cursor).toBe("");
      // Sub-threshold jitter still withholds it.
      h.dispatch("pointermove", 502);
      expect(document.body.style.cursor).toBe("");
      // Crossing the 4px threshold starts the drag → cursor applied window-wide.
      h.dispatch("pointermove", 520);
      expect(document.body.style.cursor).toBe("row-resize");
      h.cleanup();
    });

    it("clears the cursor on pointerup", () => {
      const h = setupHarness({ dragCursor: "row-resize" });
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 520);
      expect(document.body.style.cursor).toBe("row-resize");
      h.dispatch("pointerup", 520);
      expect(document.body.style.cursor).toBe("");
      h.cleanup();
    });

    it("clears the cursor on pointercancel", () => {
      const h = setupHarness({ dragCursor: "col-resize" });
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 520);
      expect(document.body.style.cursor).toBe("col-resize");
      h.dispatch("pointercancel", 520);
      expect(document.body.style.cursor).toBe("");
      h.cleanup();
    });

    it("clears the cursor on window.blur", () => {
      const h = setupHarness({ dragCursor: "row-resize" });
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 520);
      expect(document.body.style.cursor).toBe("row-resize");
      window.dispatchEvent(new Event("blur"));
      expect(document.body.style.cursor).toBe("");
      h.cleanup();
    });

    it("clears the cursor on unmount mid-drag", () => {
      const h = setupHarness({ dragCursor: "row-resize" });
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 520);
      expect(document.body.style.cursor).toBe("row-resize");
      act(() => {
        h.unmount();
      });
      expect(document.body.style.cursor).toBe("");
      h.cleanup();
    });

    it("clears the cursor even when releasePointerCapture throws (the WebKit stranded-capture bug)", () => {
      Element.prototype.releasePointerCapture = vi.fn(() => {
        throw new Error("WebKit failed to release capture");
      });
      const h = setupHarness({ dragCursor: "row-resize" });
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 520);
      expect(document.body.style.cursor).toBe("row-resize");
      // Even though capture release throws (and is swallowed in releaseInternals),
      // the cursor reset MUST still happen — this is the actual SLW-FX-9 defect.
      h.dispatch("pointerup", 520);
      expect(document.body.style.cursor).toBe("");
      h.cleanup();
    });

    it("never sets the cursor on a pure tap (no threshold cross)", () => {
      const h = setupHarness({ dragCursor: "row-resize" });
      h.dispatch("pointerdown", 500);
      h.dispatch("pointerup", 500);
      expect(document.body.style.cursor).toBe("");
      expect(h.onTap).toHaveBeenCalledTimes(1);
      h.cleanup();
    });

    it("a pure tap does NOT clobber a body cursor owned by unrelated code (set/clear symmetry)", () => {
      // The clear is gated on the SAME `drag.started` + `dragCursor` conditions
      // as the set, so a bare click on a divider whose drag never started must
      // leave any pre-existing body cursor untouched.
      document.body.style.cursor = "wait";
      const h = setupHarness({ dragCursor: "row-resize" });
      h.dispatch("pointerdown", 500);
      h.dispatch("pointerup", 500); // tap: threshold never crossed
      expect(document.body.style.cursor).toBe("wait");
      expect(h.onTap).toHaveBeenCalledTimes(1);
      h.cleanup();
    });
  });

  // SW-FX-74: the at-rest resize cursor is JS-armed (genuine pointerenter),
  // NOT the bare CSS `:hover` a stranded WebKit pointer-capture can re-assert.
  // The hook exposes `cursor` (the armed value or undefined); the divider
  // applies it inline. Genuine pointerenter is the sole re-arm signal, so a
  // stranded release (no boundary events) leaves the cursor disarmed.
  describe("SW-FX-74: armed hover-cursor (decoupled from :hover)", () => {
    it("arms on pointerenter and disarms on pointerleave (the at-rest affordance)", () => {
      const h = setupHarness({ dragCursor: "col-resize" });
      // Disarmed at rest — no cursor until the pointer genuinely enters.
      expect(h.result().cursor).toBeUndefined();
      h.dispatch("pointerenter", 500);
      expect(h.result().cursor).toBe("col-resize");
      h.dispatch("pointerleave", 500);
      expect(h.result().cursor).toBeUndefined();
      h.cleanup();
    });

    it("stays undefined when the consumer supplied no dragCursor (the render is the sole gate)", () => {
      const h = setupHarness({ dragCursor: undefined });
      h.dispatch("pointerenter", 500);
      expect(h.result().cursor).toBeUndefined();
      h.cleanup();
    });

    it("does NOT re-arm on a pointerenter DURING a drag (capture retarget can't strand it)", () => {
      // Under pointer capture a pointer that leaves and re-enters the divider can
      // fire pointerenter mid-drag; without the drag-in-flight guard that would
      // defeat the drag-start disarm and, on a stranded release, strand the
      // cursor again (the exact bug). Force the stranded path too.
      Element.prototype.releasePointerCapture = vi.fn(() => {
        throw new Error("WebKit failed to release capture");
      });
      const h = setupHarness({ dragCursor: "row-resize" });
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 520); // drag starts → disarmed
      h.dispatch("pointerenter", 520); // stray mid-drag enter (capture retarget)
      expect(h.result().cursor).toBeUndefined(); // guard held: still disarmed
      h.dispatch("pointerup", 520); // stranded release
      expect(h.result().cursor).toBeUndefined();
      expect(document.body.style.cursor).toBe("");
      h.cleanup();
    });

    it("keeps the drag hot-path render-free (armed never toggles during pointermove)", () => {
      const h = setupHarness({ dragCursor: "row-resize" });
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 520); // drag starts (may render once: disarm)
      const baseline = h.renderCount();
      // Subsequent in-drag moves must not re-render the hook.
      h.dispatch("pointermove", 540);
      h.dispatch("pointermove", 560);
      h.dispatch("pointermove", 580);
      expect(h.renderCount()).toBe(baseline);
      h.cleanup();
    });

    it("disarms on drag-start so the <body> cursor is the sole carrier during a drag", () => {
      const h = setupHarness({ dragCursor: "row-resize" });
      h.dispatch("pointerenter", 500);
      expect(h.result().cursor).toBe("row-resize");
      h.dispatch("pointerdown", 500);
      // Cross the 4px threshold → the drag starts and the element cursor disarms,
      // leaving the window-wide <body> cursor as the only carrier (AC-4).
      h.dispatch("pointermove", 520);
      expect(h.result().cursor).toBeUndefined();
      expect(document.body.style.cursor).toBe("row-resize");
      h.cleanup();
    });

    it("stays disarmed after a stranded release (releasePointerCapture throws, no fresh enter)", () => {
      Element.prototype.releasePointerCapture = vi.fn(() => {
        throw new Error("WebKit failed to release capture");
      });
      const h = setupHarness({ dragCursor: "row-resize" });
      h.dispatch("pointerenter", 500);
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 520); // drag starts → disarmed
      h.dispatch("pointerup", 520); // stranded release: no boundary events fire
      // The element cursor must NOT re-assert — only a fresh genuine pointerenter
      // (which a stranded capture cannot synthesize) may re-arm it (AC-1, AC-3).
      expect(h.result().cursor).toBeUndefined();
      expect(document.body.style.cursor).toBe("");
      h.cleanup();
    });

    it("re-arms on a genuine pointerenter after the drag ends (AC-2 preserved)", () => {
      const h = setupHarness({ dragCursor: "row-resize" });
      h.dispatch("pointerenter", 500);
      h.dispatch("pointerdown", 500);
      h.dispatch("pointermove", 520); // drag starts → disarmed
      h.dispatch("pointerup", 520); // normal release
      expect(h.result().cursor).toBeUndefined();
      // A genuine re-enter (pointer truly back over the divider) re-arms it.
      h.dispatch("pointerenter", 520);
      expect(h.result().cursor).toBe("row-resize");
      h.cleanup();
    });
  });
});
