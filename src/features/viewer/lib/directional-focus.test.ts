// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, afterEach } from "vitest";
import {
  resolveDirectionalFocus,
  resolveDirectionalFocusWithWrap,
  readPaneRects,
  focusAdjacentPane,
  type TileRect,
} from "./directional-focus";

// A 2×2 grid: TL TR / BL BR (each 100×100).
const TL: TileRect = { id: "tl", left: 0, right: 100, top: 0, bottom: 100 };
const TR: TileRect = { id: "tr", left: 100, right: 200, top: 0, bottom: 100 };
const BL: TileRect = { id: "bl", left: 0, right: 100, top: 100, bottom: 200 };
const BR: TileRect = { id: "br", left: 100, right: 200, top: 100, bottom: 200 };
const GRID = [TL, TR, BL, BR];

describe("resolveDirectionalFocus — 2×2 grid", () => {
  it("right/left/up/down from top-left", () => {
    expect(resolveDirectionalFocus(GRID, "tl", "right")).toBe("tr");
    expect(resolveDirectionalFocus(GRID, "tl", "down")).toBe("bl");
    expect(resolveDirectionalFocus(GRID, "tl", "left")).toBeNull();
    expect(resolveDirectionalFocus(GRID, "tl", "up")).toBeNull();
  });

  it("from bottom-right", () => {
    expect(resolveDirectionalFocus(GRID, "br", "left")).toBe("bl");
    expect(resolveDirectionalFocus(GRID, "br", "up")).toBe("tr");
    expect(resolveDirectionalFocus(GRID, "br", "right")).toBeNull();
    expect(resolveDirectionalFocus(GRID, "br", "down")).toBeNull();
  });

  it("prefers the same-row neighbor over a diagonal one", () => {
    // From TL, "right" must pick TR (same row), never BR (diagonal).
    expect(resolveDirectionalFocus(GRID, "tl", "right")).toBe("tr");
    // From BL, "right" must pick BR (same row), never TR.
    expect(resolveDirectionalFocus(GRID, "bl", "right")).toBe("br");
  });
});

describe("resolveDirectionalFocus — nested / non-grid layouts", () => {
  it("left pane + right column of two: from the left pane, down stays in the left pane (none)", () => {
    // [ A (full height left) | (B top-right / C bottom-right) ]
    const A: TileRect = { id: "a", left: 0, right: 100, top: 0, bottom: 200 };
    const B: TileRect = { id: "b", left: 100, right: 200, top: 0, bottom: 100 };
    const C: TileRect = { id: "c", left: 100, right: 200, top: 100, bottom: 200 };
    const tiles = [A, B, C];
    expect(resolveDirectionalFocus(tiles, "a", "right")).toBe("b"); // nearest right is the top of the column
    expect(resolveDirectionalFocus(tiles, "a", "down")).toBeNull(); // nothing below A
    expect(resolveDirectionalFocus(tiles, "b", "down")).toBe("c"); // within the right column
    expect(resolveDirectionalFocus(tiles, "c", "left")).toBe("a");
  });

  it("single pane → every direction is null", () => {
    const solo = [{ id: "only", left: 0, right: 100, top: 0, bottom: 100 }];
    for (const dir of ["left", "right", "up", "down"] as const) {
      expect(resolveDirectionalFocus(solo, "only", dir)).toBeNull();
    }
  });

  it("unknown current id → null", () => {
    expect(resolveDirectionalFocus(GRID, "ghost", "left")).toBeNull();
  });
});

describe("resolveDirectionalFocusWithWrap — edge wrap-around", () => {
  // A single row of three 100-wide tiles: L M R.
  const L: TileRect = { id: "l", left: 0, right: 100, top: 0, bottom: 100 };
  const M: TileRect = { id: "m", left: 100, right: 200, top: 0, bottom: 100 };
  const R: TileRect = { id: "r", left: 200, right: 300, top: 0, bottom: 100 };
  const ROW = [L, M, R];

  it("wraps leftmost + Left → rightmost (and rightmost + Right → leftmost)", () => {
    expect(resolveDirectionalFocusWithWrap(ROW, "l", "left")).toBe("r");
    expect(resolveDirectionalFocusWithWrap(ROW, "r", "right")).toBe("l");
  });

  it("does not wrap when a direct neighbor exists", () => {
    expect(resolveDirectionalFocusWithWrap(ROW, "l", "right")).toBe("m");
    expect(resolveDirectionalFocusWithWrap(ROW, "m", "right")).toBe("r");
    expect(resolveDirectionalFocusWithWrap(ROW, "r", "left")).toBe("m");
  });

  it("wraps vertically across rows (topmost + Up → bottommost), staying in-column", () => {
    expect(resolveDirectionalFocusWithWrap(GRID, "tl", "up")).toBe("bl");
    expect(resolveDirectionalFocusWithWrap(GRID, "tr", "up")).toBe("br");
    expect(resolveDirectionalFocusWithWrap(GRID, "bl", "down")).toBe("tl");
  });

  it("wraps horizontally within the row only, not diagonally", () => {
    // Top row: TL + Left wraps to TR (same row), never BR/BL.
    expect(resolveDirectionalFocusWithWrap(GRID, "tl", "left")).toBe("tr");
    expect(resolveDirectionalFocusWithWrap(GRID, "br", "right")).toBe("bl");
  });

  it("single pane → no wrap (null)", () => {
    const solo = [{ id: "only", left: 0, right: 100, top: 0, bottom: 100 }];
    for (const dir of ["left", "right", "up", "down"] as const) {
      expect(resolveDirectionalFocusWithWrap(solo, "only", dir)).toBeNull();
    }
  });
});

// The DOM-filtering layer (readPaneRects / focusAdjacentPane) is where AC6's
// zoom + minimize determinism actually lives — those cases are properties of
// which tiles readPaneRects returns, not of the pure geometry. happy-dom's
// getBoundingClientRect returns all-zeros, so without per-element rect mocks
// every tile filters out and these tests would pass vacuously; the mock below
// gives each tile distinct geometry (a zero rect models a minimized/hidden tile).
describe("readPaneRects + focusAdjacentPane — DOM filtering (OC-7 AC6)", () => {
  function makeFrame(id: string): HTMLTextAreaElement {
    const frame = document.createElement("div");
    frame.setAttribute("data-pane-id", id);
    const xterm = document.createElement("div");
    xterm.className = "xterm";
    const textarea = document.createElement("textarea");
    xterm.appendChild(textarea);
    frame.appendChild(xterm);
    let host = document.querySelector("[data-xterm-host]");
    if (!host) {
      host = document.createElement("div");
      host.setAttribute("data-xterm-host", "");
      document.body.appendChild(host);
    }
    host.appendChild(frame);
    return textarea;
  }

  /** Mock getBoundingClientRect off each frame's data-pane-id; an id
   *  absent from `rects` returns a zero rect (a minimized/hidden tile). */
  function mockRects(rects: Record<string, Omit<TileRect, "id">>) {
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (
      this: HTMLElement,
    ) {
      const id = this.getAttribute("data-pane-id");
      const r = id ? rects[id] : undefined;
      const box = r ?? { left: 0, right: 0, top: 0, bottom: 0 };
      return {
        left: box.left,
        right: box.right,
        top: box.top,
        bottom: box.bottom,
        width: box.right - box.left,
        height: box.bottom - box.top,
        x: box.left,
        y: box.top,
        toJSON() {},
      } as DOMRect;
    });
  }

  afterEach(() => {
    vi.restoreAllMocks();
    document.body.innerHTML = "";
  });

  it("readPaneRects excludes zero-size (minimized/hidden) tiles", () => {
    makeFrame("visible");
    makeFrame("minimized");
    mockRects({ visible: { left: 0, right: 100, top: 0, bottom: 100 } }); // 'minimized' → zero rect
    const rects = readPaneRects();
    expect(rects.map((r) => r.id)).toEqual(["visible"]);
  });

  it("SW-EW-46: readPaneRects skips a CONTAINER pane (one nesting cell panes) and returns its cells", () => {
    // A gridded workspace tile carries `data-pane-id` AND hosts cell
    // frames with their own ids — the container must not compete as a nav leaf.
    const container = document.createElement("div");
    container.setAttribute("data-pane-id", "ws");
    const cellA = document.createElement("div");
    cellA.setAttribute("data-pane-id", "cell-a");
    const cellB = document.createElement("div");
    cellB.setAttribute("data-pane-id", "cell-b");
    container.appendChild(cellA);
    container.appendChild(cellB);
    document.body.appendChild(container);
    mockRects({
      ws: { left: 0, right: 200, top: 0, bottom: 100 },
      "cell-a": { left: 0, right: 100, top: 0, bottom: 100 },
      "cell-b": { left: 100, right: 200, top: 0, bottom: 100 },
    });
    const rects = readPaneRects();
    expect(rects.map((r) => r.id).sort()).toEqual(["cell-a", "cell-b"]);
  });

  it("focusAdjacentPane moves focus to the neighbor tile's terminal", () => {
    makeFrame("left");
    const rightTextarea = makeFrame("right");
    mockRects({
      left: { left: 0, right: 100, top: 0, bottom: 100 },
      right: { left: 100, right: 200, top: 0, bottom: 100 },
    });
    const switchFn = vi.fn();
    // OC-16: a real move reports `true`.
    expect(focusAdjacentPane("team", "left", "right", switchFn)).toBe(true);
    expect(switchFn).toHaveBeenCalledWith("team", "right");
    expect(document.activeElement).toBe(rightTextarea);
  });

  it("is a no-op when zoomed (only one tile is visible → no neighbor)", () => {
    makeFrame("zoomed");
    makeFrame("hidden-a");
    makeFrame("hidden-b");
    // Zoom hides the other tiles (display:none → zero rect); only one visible.
    mockRects({ zoomed: { left: 0, right: 200, top: 0, bottom: 200 } });
    const switchFn = vi.fn();
    // OC-16: the blocked no-op reports `false` so the caller can flash the cue.
    expect(focusAdjacentPane("team", "zoomed", "right", switchFn)).toBe(false);
    expect(switchFn).not.toHaveBeenCalled();
  });

  it("is a no-op in a one-tile mosaic", () => {
    makeFrame("only");
    mockRects({ only: { left: 0, right: 100, top: 0, bottom: 100 } });
    const switchFn = vi.fn();
    for (const dir of ["left", "right", "up", "down"] as const) {
      expect(focusAdjacentPane("team", "only", dir, switchFn)).toBe(false);
    }
    expect(switchFn).not.toHaveBeenCalled();
  });

  it("is a no-op with no active workspace id", () => {
    makeFrame("a");
    mockRects({ a: { left: 0, right: 100, top: 0, bottom: 100 } });
    const switchFn = vi.fn();
    expect(focusAdjacentPane("team", undefined, "right", switchFn)).toBe(false);
    expect(switchFn).not.toHaveBeenCalled();
  });
});
