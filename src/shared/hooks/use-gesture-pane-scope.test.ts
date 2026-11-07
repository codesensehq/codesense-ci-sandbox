// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, afterEach } from "vitest";
import { renderHook, cleanup } from "@testing-library/react";
import {
  EXCALIDRAW_CANVAS_ATTR,
  isGestureOutsideCanvas,
  useGesturePaneScope,
} from "./use-gesture-pane-scope";

const GESTURE_TYPES = ["gesturestart", "gesturechange", "gestureend"] as const;

function el(parent: HTMLElement, attrs: Record<string, string> = {}): HTMLElement {
  const node = document.createElement("div");
  for (const [k, v] of Object.entries(attrs)) node.setAttribute(k, v);
  parent.appendChild(node);
  return node;
}

// Dispatches a cancelable, bubbling gesture event and returns false when a
// listener called preventDefault (mirrors `dispatchEvent`'s contract).
function gesture(type: string, target: HTMLElement): boolean {
  return target.dispatchEvent(new Event(type, { bubbles: true, cancelable: true }));
}

describe("isGestureOutsideCanvas", () => {
  afterEach(() => {
    document.body.innerHTML = "";
  });

  it("returns false when the target is inside a canvas", () => {
    const canvas = el(document.body, { [EXCALIDRAW_CANVAS_ATTR]: "" });
    expect(isGestureOutsideCanvas(el(canvas))).toBe(false);
  });

  it("returns true when the target is outside every canvas", () => {
    expect(isGestureOutsideCanvas(el(document.body))).toBe(true);
  });

  it("returns true for null / non-Element targets", () => {
    expect(isGestureOutsideCanvas(null)).toBe(true);
    expect(isGestureOutsideCanvas(window as unknown as EventTarget)).toBe(true);
  });
});

describe("useGesturePaneScope", () => {
  // Deterministically unmount every hook so the module-level refCount returns
  // to zero between tests (the guard's lifecycle is shared state).
  afterEach(() => {
    cleanup();
    document.body.innerHTML = "";
  });

  // Stands in for Excalidraw's bubble-phase document listeners so the tests
  // assert real propagation across all three gesture event types.
  function withExcalidrawProbe(run: (probe: ReturnType<typeof vi.fn>) => void) {
    const probe = vi.fn();
    for (const t of GESTURE_TYPES) document.addEventListener(t, probe);
    try {
      run(probe);
    } finally {
      for (const t of GESTURE_TYPES) document.removeEventListener(t, probe);
    }
  }

  it("suppresses a pinch that begins outside a canvas (Excalidraw never sees it; page-zoom prevented)", () => {
    renderHook(() => useGesturePaneScope());
    withExcalidrawProbe((probe) => {
      const outside = el(document.body);
      const startNotPrevented = gesture("gesturestart", outside);
      gesture("gesturechange", outside);
      gesture("gestureend", outside);
      expect(probe).not.toHaveBeenCalled();
      expect(startNotPrevented).toBe(false); // preventDefault was called
    });
  });

  it("lets a pinch inside a canvas reach Excalidraw and leaves its default intact", () => {
    renderHook(() => useGesturePaneScope());
    withExcalidrawProbe((probe) => {
      const inside = el(el(document.body, { [EXCALIDRAW_CANVAS_ATTR]: "" }));
      const startNotPrevented = gesture("gesturestart", inside);
      gesture("gesturechange", inside);
      expect(probe).toHaveBeenCalledTimes(2);
      expect(startNotPrevented).toBe(true); // default left for Excalidraw
    });
  });

  it("latches the start decision for the whole pinch even if a later event retargets across panes", () => {
    renderHook(() => useGesturePaneScope());
    withExcalidrawProbe((probe) => {
      const inside = el(el(document.body, { [EXCALIDRAW_CANVAS_ATTR]: "" }));
      const outside = el(document.body);
      // Begins over the canvas → allowed; a mid-pinch change/end that retargets
      // outside must STILL pass through (a gesture is handled atomically).
      gesture("gesturestart", inside);
      gesture("gesturechange", outside);
      gesture("gestureend", outside);
      expect(probe).toHaveBeenCalledTimes(3);
    });
  });

  it("latches suppression for a pinch that begins outside even if it retargets onto the canvas", () => {
    renderHook(() => useGesturePaneScope());
    withExcalidrawProbe((probe) => {
      const inside = el(el(document.body, { [EXCALIDRAW_CANVAS_ATTR]: "" }));
      const outside = el(document.body);
      gesture("gesturestart", outside);
      gesture("gesturechange", inside);
      gesture("gestureend", inside);
      expect(probe).not.toHaveBeenCalled();
    });
  });

  it("resets the latch on gestureend so an orphan change (no preceding start) fails safe to allow", () => {
    renderHook(() => useGesturePaneScope());
    withExcalidrawProbe((probe) => {
      const outside = el(document.body);
      const inside = el(el(document.body, { [EXCALIDRAW_CANVAS_ATTR]: "" }));
      // A complete suppressed pinch outside the canvas leaves the latch reset.
      gesture("gesturestart", outside);
      gesture("gestureend", outside);
      // A later change with no start (dropped/mid-mount) must NOT inherit the
      // prior suppression — it reaches Excalidraw rather than being stuck-killed.
      gesture("gesturechange", inside);
      expect(probe).toHaveBeenCalledTimes(1);
    });
  });

  it("re-decides on each new gesturestart rather than first-write-wins", () => {
    renderHook(() => useGesturePaneScope());
    withExcalidrawProbe((probe) => {
      const outside = el(document.body);
      const inside = el(el(document.body, { [EXCALIDRAW_CANVAS_ATTR]: "" }));
      // First pinch outside the canvas is suppressed end-to-end.
      gesture("gesturestart", outside);
      gesture("gestureend", outside);
      // Second pinch over the canvas must be allowed (start re-decides).
      gesture("gesturestart", inside);
      gesture("gestureend", inside);
      expect(probe).toHaveBeenCalledTimes(2);
    });
  });

  it("keeps the guard while any caller is mounted and removes it only when the last unmounts", () => {
    const a = renderHook(() => useGesturePaneScope());
    const b = renderHook(() => useGesturePaneScope());
    a.unmount();
    // One caller still mounted → guard active → an outside pinch is suppressed.
    withExcalidrawProbe((probe) => {
      gesture("gesturestart", el(document.body));
      expect(probe).not.toHaveBeenCalled();
    });
    b.unmount();
    // Last caller gone → guard removed → the event reaches the document again.
    withExcalidrawProbe((probe) => {
      gesture("gesturestart", el(document.body));
      expect(probe).toHaveBeenCalledTimes(1);
    });
  });
});
