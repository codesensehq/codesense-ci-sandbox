// SPDX-License-Identifier: MIT
import type { Terminal } from "@xterm/xterm";
import type { XtermWebglDiag } from "./xterm-webgl-diag";

/**
 * A live xterm's visual state: grid dimensions, active-buffer scroll offsets,
 * and whether the alternate screen (a full-screen TUI) is showing.
 *
 * `viewportY` is the buffer line at the top of the viewport; `baseY` is the
 * line the viewport top sits at when scrolled fully to the bottom. So
 * `viewportY >= baseY` means "pinned to the tail", and `baseY - viewportY` is
 * the scroll distance from the bottom in lines.
 */
export interface XtermVisualState {
  cols: number;
  rows: number;
  viewportY: number;
  baseY: number;
  altScreen: boolean;
}

/**
 * Read a terminal's visual state from its cached getters. Pure and O(1): no
 * layout is forced, no DOM or WebGL context is touched, and no work leaves the
 * calling frame — safe to call from any frame without risking a stall.
 */
export function readXtermVisualState(term: Terminal): XtermVisualState {
  const buffer = term.buffer.active;
  return {
    cols: term.cols,
    rows: term.rows,
    viewportY: buffer.viewportY,
    baseY: buffer.baseY,
    altScreen: buffer.type === "alternate",
  };
}

/**
 * The visible viewport read straight from xterm's OWN buffer model — the "what
 * SHOULD be on screen" truth the screen-vs-buffer oracle (SW-CL-11 D1) compares
 * the rendered pixel crop against. This is xterm's line array, NOT the WebGL
 * atlas: it is exactly what xterm intends to paint, so a divergence between it
 * and the crop is the definition of the founder's within-pane self-corruption
 * (a status line doubled on screen, a row rendered as atlas fragments) that the
 * relational pixel-only oracle is structurally blind to.
 *
 * `rows[i]` is the full cell string of the i-th VISIBLE row, top to bottom
 * (`viewportY + i`), WITHOUT right-trimming — trailing blanks are kept so the
 * per-row cell count is column-faithful and the D1 structural comparison lines
 * up crop rows with buffer rows one-for-one.
 *
 * Pure and O(visible cells): it reads only the `rows` lines currently in view
 * (a small bounded window), touches no DOM and no WebGL context, and forces no
 * layout or reflow — webdriver-safe to call in the same tick as a crop so the
 * grid and the pixels describe the same instant (D1's teeth: a mid-write row is
 * mid-write in both, only a true screen≠buffer divergence trips).
 */
export interface XtermGridText {
  cols: number;
  rows: number;
  /** Cell text of each visible row, top→bottom; length === `rows`. */
  lines: string[];
  /** Per visible row: whether it is a soft-wrap CONTINUATION of the row above
   *  (xterm's `isWrapped`), so a reader can rejoin logical lines. */
  wrapped?: boolean[];
}

export function readGridText(term: Terminal): XtermGridText {
  const buffer = term.buffer.active;
  const cols = term.cols;
  const rows = term.rows;
  const lines: string[] = [];
  const wrapped: boolean[] = [];
  for (let i = 0; i < rows; i += 1) {
    const line = buffer.getLine(buffer.viewportY + i);
    // translateToString(false) keeps trailing blanks so the cell count is
    // column-faithful; a row past the end of the buffer reads as all-blank.
    lines.push(line ? line.translateToString(false, 0, cols) : " ".repeat(cols));
    wrapped.push(line?.isWrapped ?? false);
  }
  return { cols, rows, lines, wrapped };
}

declare global {
  interface Window {
    /**
     * WebDriver-only registry of per-workspace visual-state readers, keyed by
     * workspace id. Registered by the shell terminal panel under
     * `import.meta.env.MODE === "webdriver"` and read by the terminal-webgl
     * fixture control; tree-shaken from — and proven absent in — release builds
     * (`scripts/verify-webdriver-absence.ts`). A reader returns `null` when its
     * terminal is not currently mounted.
     */
    __CODESENSE_TERMINAL_STATE__?: Record<string, () => XtermVisualState | null>;
    /**
     * SW-CL-12: WebDriver-only registry of per-workspace BUFFER-GRID readers. Each
     * returns the current viewport's line array ({@link readGridText}) — the "what
     * SHOULD be on screen" truth the screen-vs-buffer oracle (D1) compares the
     * rendered crop against, read in the SAME tick as the crop. Registered by the
     * shell terminal panel under `webdriver` mode; tree-shaken from release builds.
     * Returns `null` when the terminal is not currently mounted.
     */
    __CODESENSE_TERMINAL_GRID__?: Record<string, () => XtermGridText | null>;
    /** WebDriver-only per-pane WebGL renderer diagnostics ({@link XtermWebglDiag}). */
    __CODESENSE_TERMINAL_WEBGL_DIAG__?: Record<string, () => XtermWebglDiag | null>;
    /**
     * SW-CL-12: WebDriver-only registry of per-workspace scrollback-viewport
     * drivers. `scrollBy(lines)` scrolls the viewport (negative = up into
     * scrollback) via xterm's own `scrollLines` and returns the resulting
     * distance-from-bottom (in lines), or `null` when the terminal is not mounted.
     * Drives Repro 2 (tab-switch→scroll-up); tree-shaken from release builds.
     */
    __CODESENSE_TERMINAL_SCROLL__?: Record<string, (lines: number) => number | null>;
  }
}
