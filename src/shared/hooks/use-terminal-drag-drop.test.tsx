// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { useRef } from "react";

/**
 * Tests for the drag-drop-to-terminal hook (SW-EW-26):
 *  1. buildDropText — the sanitize → shell-escape → space-join pipeline (NO
 *     bracketed-paste framing; `term.paste()` frames per the live mode).
 *  2. dropPositionInRect — the physical→CSS px hit-test geometry.
 *  3. useTerminalDragDrop wiring — the webview-global event fans out to every
 *     visible terminal; only the tile under the cursor pastes / highlights, and
 *     not where something else is drawn over it.
 *
 * 1 & 2 are pure and import the REAL helpers (no mirror copy). The webview mock
 * below only affects the wiring suite (3), which injects a paste spy per tile.
 */

// Collect every handler registered via onDragDropEvent so a dispatched event
// fans out to all of them — exactly how the real webview-global event behaves.
type DragDropHandler = (event: { payload: unknown }) => void;
const handlers: DragDropHandler[] = [];

vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: (handler: DragDropHandler) => {
      handlers.push(handler);
      return Promise.resolve(() => {});
    },
  }),
}));

import { useTerminalDragDrop, dropPositionInRect, buildDropText } from "./use-terminal-drag-drop";

// --- buildDropText -----------------------------------------------------------

describe("buildDropText", () => {
  it("returns the bare path for a single simple path (no bracketed framing)", () => {
    expect(buildDropText(["/Users/me/file.txt"])).toBe("/Users/me/file.txt");
  });

  it("shell-escapes spaces and special chars in paths", () => {
    expect(buildDropText(["/Users/me/My Files (2)/doc's.txt"])).toBe(
      "/Users/me/My\\ Files\\ \\(2\\)/doc\\'s.txt",
    );
  });

  it("joins multiple paths with spaces", () => {
    expect(buildDropText(["/a/b.txt", "/c/d.txt", "/e/f.txt"])).toBe("/a/b.txt /c/d.txt /e/f.txt");
  });

  it("strips control chars before escaping", () => {
    expect(buildDropText(["/path/evil\rname\x03.txt"])).toBe("/path/evilname.txt");
  });

  it("handles paths with dollar signs and backticks", () => {
    expect(buildDropText(["/path/$HOME/`whoami`"])).toBe("/path/\\$HOME/\\`whoami\\`");
  });

  it("produces an empty string for an empty paths array", () => {
    expect(buildDropText([])).toBe("");
  });

  it("handles path with all metacharacters", () => {
    expect(buildDropText(["a b'\"\\$`!&|;*?[]{}~#<>"])).toBe(
      "a\\ b\\'\\\"\\\\\\$\\`\\!\\&\\|\\;\\*\\?\\[\\]\\{\\}\\~\\#\\<\\>",
    );
  });
});

// --- dropPositionInRect (hit-test geometry) ----------------------------------

describe("dropPositionInRect", () => {
  // 100×100 tile at the origin. Position and rect share the webview's CSS-px
  // coordinate space (Tauri's drag-drop position is logical px) — compared raw.
  const rect = { left: 0, top: 0, right: 100, bottom: 100 };

  it("returns true for a point inside the rect", () => {
    expect(dropPositionInRect({ x: 50, y: 50 }, rect)).toBe(true);
  });

  it("returns true on the inclusive top-left edge", () => {
    expect(dropPositionInRect({ x: 0, y: 0 }, rect)).toBe(true);
  });

  it("returns false past each outer edge", () => {
    expect(dropPositionInRect({ x: -1, y: 50 }, rect)).toBe(false); // left
    expect(dropPositionInRect({ x: 50, y: -1 }, rect)).toBe(false); // top
    expect(dropPositionInRect({ x: 101, y: 50 }, rect)).toBe(false); // right
    expect(dropPositionInRect({ x: 50, y: 101 }, rect)).toBe(false); // bottom
  });

  it("is half-open on the right/bottom edge (no double-match on a shared seam)", () => {
    expect(dropPositionInRect({ x: 99, y: 99 }, rect)).toBe(true);
    expect(dropPositionInRect({ x: 100, y: 50 }, rect)).toBe(false); // right seam
    expect(dropPositionInRect({ x: 50, y: 100 }, rect)).toBe(false); // bottom seam
  });

  it("compares position directly to the rect (no dpr scaling)", () => {
    // Regression guard for the "always lands left" bug: a position deep inside a
    // right-hand tile must NOT be down-scaled into a left-hand tile. Mirrors the
    // real fullscreen repro (right tile rect ~[865,1727], drop at x≈1321).
    const rightTile = { left: 865, top: 296, right: 1727, bottom: 1056 };
    const leftTile = { left: 0, top: 296, right: 864, bottom: 1056 };
    const drop = { x: 1321, y: 666 };
    expect(dropPositionInRect(drop, rightTile)).toBe(true);
    expect(dropPositionInRect(drop, leftTile)).toBe(false);
  });

  it("resolves a seam between two adjacent tiles to exactly one", () => {
    const tileA = { left: 0, top: 0, right: 100, bottom: 100 };
    const tileB = { left: 100, top: 0, right: 200, bottom: 100 };
    const seam = { x: 100, y: 50 };
    expect(dropPositionInRect(seam, tileA)).toBe(false); // A's half-open right
    expect(dropPositionInRect(seam, tileB)).toBe(true); // B's inclusive left
  });
});

// --- useTerminalDragDrop wiring (multi-terminal hit-test) ---------------------

function dispatchDrop(payload: unknown) {
  // act() flushes the setIsDragOver re-render so result.current reflects state.
  act(() => {
    for (const h of handlers) h({ payload });
  });
}

/** jsdom lays nothing out: the boxes on screen, in paint order (a later one is drawn on top),
 *  stand in for layout, and `document.elementFromPoint` returns the topmost one under a point. */
const painted: HTMLElement[] = [];

/** An element on screen at `rect`, drawn over every box placed before it. */
function placeBox(rect: DOMRect): HTMLElement {
  const el = document.createElement("div");
  el.getBoundingClientRect = () => rect;
  painted.push(el);
  return el;
}

beforeEach(() => {
  painted.length = 0;
  document.elementFromPoint = (x: number, y: number) =>
    [...painted].reverse().find((el) => dropPositionInRect({ x, y }, el.getBoundingClientRect())) ??
    null;
});

/** Render a hook bound to a tile at the given rect (visible by default). The
 *  injected `pasteText` spy stands in for the panel's `term.paste()` seam. */
function renderTile(pasteText: (text: string) => void, rect: DOMRect, visible = true) {
  const tile = placeBox(rect);
  return renderHook(() => {
    const visibleRef = useRef(visible);
    const disposedRef = useRef(false);
    const tileRef = useRef<HTMLElement | null>(tile);
    return useTerminalDragDrop(pasteText, visibleRef, disposedRef, tileRef);
  });
}

const rect = (left: number, right: number): DOMRect =>
  ({ left, top: 0, right, bottom: 100 }) as DOMRect;

describe("useTerminalDragDrop — multi-terminal hit-test", () => {
  beforeEach(() => {
    handlers.length = 0;
  });

  it("pastes the drop into ONLY the tile under the cursor (AC-1/AC-2)", () => {
    const pasteA = vi.fn();
    const pasteB = vi.fn();
    renderTile(pasteA, rect(0, 100)); // left tile
    renderTile(pasteB, rect(100, 200)); // right tile
    expect(handlers).toHaveLength(2); // both visible terminals subscribed

    // Drop at x=50 → inside tile A only.
    dispatchDrop({ type: "drop", paths: ["/tmp/pic.png"], position: { x: 50, y: 50 } });

    // Raw path only — xterm's paste() owns bracketed-paste framing per mode.
    expect(pasteA).toHaveBeenCalledWith("/tmp/pic.png");
    expect(pasteB).not.toHaveBeenCalled();
  });

  it("does not paste when the drop misses every tile", () => {
    const pasteA = vi.fn();
    const pasteB = vi.fn();
    renderTile(pasteA, rect(0, 100));
    renderTile(pasteB, rect(100, 200));

    // Drop at x=500 → in neither rect (e.g. a gap / chrome).
    dispatchDrop({ type: "drop", paths: ["/tmp/pic.png"], position: { x: 500, y: 50 } });

    expect(pasteA).not.toHaveBeenCalled();
    expect(pasteB).not.toHaveBeenCalled();
  });

  it("only the hovered tile reports isDragOver on over (AC-3)", () => {
    const a = renderTile(vi.fn(), rect(0, 100));
    const b = renderTile(vi.fn(), rect(100, 200));

    dispatchDrop({ type: "over", position: { x: 150, y: 50 } }); // inside B

    expect(a.result.current).toBe(false);
    expect(b.result.current).toBe(true);
  });

  it("moves the highlight as the cursor crosses from one tile to another (AC-3)", () => {
    const a = renderTile(vi.fn(), rect(0, 100));
    const b = renderTile(vi.fn(), rect(100, 200));

    dispatchDrop({ type: "over", position: { x: 50, y: 50 } }); // inside A
    expect(a.result.current).toBe(true);
    expect(b.result.current).toBe(false);

    dispatchDrop({ type: "over", position: { x: 150, y: 50 } }); // crossed into B
    expect(a.result.current).toBe(false); // A's highlight cleared
    expect(b.result.current).toBe(true);
  });

  it("clears the lit tile's highlight on leave, leaving others false (AC-3)", () => {
    const a = renderTile(vi.fn(), rect(0, 100));
    const b = renderTile(vi.fn(), rect(100, 200));

    dispatchDrop({ type: "over", position: { x: 50, y: 50 } }); // light A
    expect(a.result.current).toBe(true);

    dispatchDrop({ type: "leave" }); // no position on leave
    expect(a.result.current).toBe(false); // the lit tile clears
    expect(b.result.current).toBe(false); // others stay clear
  });

  it("a tile covered by a popup neither pastes nor highlights under it", () => {
    const paste = vi.fn();
    const tile = renderTile(paste, rect(0, 200));
    placeBox({ left: 50, top: 0, right: 150, bottom: 100 } as DOMRect); // the popup, on top

    dispatchDrop({ type: "over", position: { x: 100, y: 50 } });
    expect(tile.result.current).toBe(false);
    dispatchDrop({ type: "drop", paths: ["/tmp/pic.png"], position: { x: 100, y: 50 } });
    expect(paste).not.toHaveBeenCalled();

    // Beside the popup the tile is still on top, and takes the drop.
    dispatchDrop({ type: "over", position: { x: 20, y: 50 } });
    expect(tile.result.current).toBe(true);
    dispatchDrop({ type: "drop", paths: ["/tmp/pic.png"], position: { x: 20, y: 50 } });
    expect(paste).toHaveBeenCalledWith("/tmp/pic.png");
  });

  it("a hidden tile never pastes even when the position is inside its rect", () => {
    const paste = vi.fn();
    renderTile(paste, rect(0, 100), /* visible */ false);

    dispatchDrop({ type: "drop", paths: ["/tmp/pic.png"], position: { x: 50, y: 50 } });

    expect(paste).not.toHaveBeenCalled();
  });
});

// --- SW-GS-1 AC-11: input-lock gates the drag-drop PTY side-channel ----------

/** Render a tile with an EXTERNAL input-lock ref (the same object the
 *  once-on-mount closure captures) so the test can flip `.current` and verify
 *  the gate reads it live — exactly how the panel's synced ref behaves. */
function renderLockedTile(pasteText: (text: string) => void, locked: boolean) {
  const inputLockedRef: React.RefObject<boolean> = { current: locked };
  const tile = placeBox(rect(0, 100));
  const view = renderHook(() => {
    const visibleRef = useRef(true);
    const disposedRef = useRef(false);
    const tileRef = useRef<HTMLElement | null>(tile);
    return useTerminalDragDrop(pasteText, visibleRef, disposedRef, tileRef, inputLockedRef);
  });
  return { view, inputLockedRef };
}

describe("useTerminalDragDrop — input-lock gate (AC-11)", () => {
  beforeEach(() => {
    handlers.length = 0;
  });

  it("a drop inside the tile does NOT paste while input-locked", () => {
    const paste = vi.fn();
    renderLockedTile(paste, /* locked */ true);

    dispatchDrop({ type: "drop", paths: ["/tmp/pic.png"], position: { x: 50, y: 50 } });

    expect(paste).not.toHaveBeenCalled();
  });

  it("does not highlight on over while input-locked", () => {
    const { view } = renderLockedTile(vi.fn(), /* locked */ true);

    dispatchDrop({ type: "over", position: { x: 50, y: 50 } });

    expect(view.result.current).toBe(false);
  });

  it("unlocked tile still pastes (gate is opt-in, existing behavior preserved)", () => {
    const paste = vi.fn();
    renderLockedTile(paste, /* locked */ false);

    dispatchDrop({ type: "drop", paths: ["/tmp/pic.png"], position: { x: 50, y: 50 } });

    expect(paste).toHaveBeenCalledWith("/tmp/pic.png");
  });

  it("reads the lock LIVE off the ref — flipping it after mount blocks the next drop", () => {
    const paste = vi.fn();
    const { inputLockedRef } = renderLockedTile(paste, /* locked */ false);

    // Initially unlocked → pastes.
    dispatchDrop({ type: "drop", paths: ["/tmp/a.png"], position: { x: 50, y: 50 } });
    expect(paste).toHaveBeenCalledTimes(1);

    // Flip the lock on (as the agent takes the wheel) — the once-on-mount
    // closure must honor it without re-subscribing.
    inputLockedRef.current = true;
    dispatchDrop({ type: "drop", paths: ["/tmp/b.png"], position: { x: 50, y: 50 } });
    expect(paste).toHaveBeenCalledTimes(1); // still 1 — second drop blocked
  });
});
