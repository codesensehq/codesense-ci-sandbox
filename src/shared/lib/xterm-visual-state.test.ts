// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { readGridText, readXtermVisualState } from "./xterm-visual-state";

/** Minimal xterm stand-in exposing only the getters the reader touches. */
function fakeTerm(over: {
  cols?: number;
  rows?: number;
  viewportY?: number;
  baseY?: number;
  type?: "normal" | "alternate";
  /** Full-buffer line strings keyed by absolute buffer index (as `getLine` sees). */
  bufferLines?: Record<number, string>;
  /** Absolute buffer indices whose line is a soft-wrap continuation. */
  wrappedLines?: number[];
}) {
  const cols = over.cols ?? 80;
  return {
    cols,
    rows: over.rows ?? 24,
    buffer: {
      active: {
        viewportY: over.viewportY ?? 0,
        baseY: over.baseY ?? 0,
        type: over.type ?? "normal",
        getLine(y: number) {
          const text = over.bufferLines?.[y];
          if (text === undefined) return undefined;
          return {
            isWrapped: over.wrappedLines?.includes(y) ?? false,
            translateToString(_trimRight?: boolean, startColumn = 0, endColumn = cols) {
              // Emulate xterm: null cells render as spaces; slice to [start,end),
              // padding to the requested width with spaces (as a real buffer does).
              return text.padEnd(endColumn, " ").slice(startColumn, endColumn);
            },
          };
        },
      },
    },
  } as unknown as import("@xterm/xterm").Terminal;
}

describe("readXtermVisualState", () => {
  it("reads cols, rows, and the active buffer's scroll offsets verbatim", () => {
    const state = readXtermVisualState(fakeTerm({ cols: 120, rows: 40, viewportY: 7, baseY: 12 }));
    expect(state).toEqual({ cols: 120, rows: 40, viewportY: 7, baseY: 12, altScreen: false });
  });

  it("maps the alternate buffer type to altScreen=true", () => {
    expect(readXtermVisualState(fakeTerm({ type: "alternate" })).altScreen).toBe(true);
  });

  it("maps the normal buffer type to altScreen=false", () => {
    expect(readXtermVisualState(fakeTerm({ type: "normal" })).altScreen).toBe(false);
  });
});

describe("readGridText", () => {
  it("returns the visible rows' cell text top→bottom, offset by viewportY", () => {
    const grid = readGridText(
      fakeTerm({
        cols: 5,
        rows: 3,
        viewportY: 10,
        bufferLines: { 10: "abc", 11: "de", 12: "fghij" },
      }),
    );
    expect(grid.cols).toBe(5);
    expect(grid.rows).toBe(3);
    // Not right-trimmed: padded to `cols` so the per-row cell count is faithful.
    expect(grid.lines).toEqual(["abc  ", "de   ", "fghij"]);
  });

  it("renders a row past the end of the buffer as all-blank (never crashes)", () => {
    const grid = readGridText(
      fakeTerm({ cols: 4, rows: 2, viewportY: 0, bufferLines: { 0: "hi" } }),
    );
    expect(grid.lines).toEqual(["hi  ", "    "]);
  });

  it("reports which visible rows are soft-wrap continuations", () => {
    const grid = readGridText(
      fakeTerm({
        cols: 4,
        rows: 3,
        viewportY: 5,
        bufferLines: { 5: "abcd", 6: "ef", 7: "gh" },
        wrappedLines: [6],
      }),
    );
    expect(grid.wrapped).toEqual([false, true, false]);
  });
});
