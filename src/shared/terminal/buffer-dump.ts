// SPDX-License-Identifier: MIT
/**
 * The one terminal buffer dump every parity check compares: a display
 * (`@xterm/xterm`), the backend emulator and the test reference
 * (`@xterm/headless`) all dump through `referenceBuffer`, so "display ==
 * emulator" is a deepEqual on this shape and nothing else.
 *
 * Imports nothing outside `@xterm/*` (no DOM, no React) so the emulator worker
 * can bundle it as is.
 */
import type { IBuffer, IBufferCell } from "@xterm/headless";

/** One run of cells `[start, end)` on a row sharing the same attributes. */
export interface AttrRun {
  start: number;
  end: number;
  /** Colour mode bits OR'd with the colour (`getFgColorMode() | getFgColor()`);
   *  the default colour is -1. */
  fg: number;
  bg: number;
  /** {@link ATTR_FLAGS} bits. */
  flags: number;
}

export interface BufferPlane {
  /** `translateToString(true, 0, cols)` of every row. */
  lines: string[];
  isWrapped: boolean[];
  attrs: AttrRun[][];
}

export interface ReferenceBuffer extends BufferPlane {
  cursorX: number;
  cursorY: number;
  cursorVisible: boolean;
  activeBuffer: "normal" | "alternate";
  /** The alternate screen, only while it is the active buffer. */
  alt: BufferPlane | null;
  cols: number;
  rows: number;
}

export const ATTR_FLAGS = {
  bold: 1,
  italic: 2,
  dim: 4,
  underline: 8,
  blink: 16,
  inverse: 32,
  invisible: 64,
  strikethrough: 128,
  overline: 256,
} as const;

/** What `referenceBuffer` reads: the surface `@xterm/xterm` and `@xterm/headless`
 *  terminals share. */
export interface DumpableTerminal {
  readonly cols: number;
  readonly rows: number;
  readonly buffer: {
    readonly active: { readonly type: "normal" | "alternate" } & Pick<
      IBuffer,
      "cursorX" | "cursorY"
    >;
    readonly normal: IBuffer;
    readonly alternate: IBuffer;
  };
}

function flagsOf(cell: IBufferCell): number {
  let flags = 0;
  if (cell.isBold()) flags |= ATTR_FLAGS.bold;
  if (cell.isItalic()) flags |= ATTR_FLAGS.italic;
  if (cell.isDim()) flags |= ATTR_FLAGS.dim;
  if (cell.isUnderline()) flags |= ATTR_FLAGS.underline;
  if (cell.isBlink()) flags |= ATTR_FLAGS.blink;
  if (cell.isInverse()) flags |= ATTR_FLAGS.inverse;
  if (cell.isInvisible()) flags |= ATTR_FLAGS.invisible;
  if (cell.isStrikethrough()) flags |= ATTR_FLAGS.strikethrough;
  if (cell.isOverline()) flags |= ATTR_FLAGS.overline;
  return flags;
}

function rowAttrs(buffer: IBuffer, y: number, cols: number, cell: IBufferCell): AttrRun[] {
  const line = buffer.getLine(y);
  if (!line) return [];
  // Cells past the last one that is not a default blank carry no runs. Read
  // twice rather than kept, so a dump of a full scrollback makes no garbage
  // per cell.
  let end = 0;
  for (let x = 0; x < cols; x++) {
    if (!line.getCell(x, cell)) break;
    const chars = cell.getChars();
    // A wide glyph's trailing cell has width 0 and no chars; it is not blank.
    const blankDefault =
      (chars === "" || chars === " ") && cell.getWidth() !== 0 && cell.isAttributeDefault();
    if (!blankDefault) end = x + 1;
  }
  const runs: AttrRun[] = [];
  for (let x = 0; x < end; x++) {
    line.getCell(x, cell);
    const fg = cell.getFgColorMode() | cell.getFgColor();
    const bg = cell.getBgColorMode() | cell.getBgColor();
    const flags = flagsOf(cell);
    const last = runs[runs.length - 1];
    if (last && last.fg === fg && last.bg === bg && last.flags === flags) {
      last.end = x + 1;
    } else {
      runs.push({ start: x, end: x + 1, fg, bg, flags });
    }
  }
  return runs;
}

function plane(buffer: IBuffer, cols: number): BufferPlane {
  const cell = buffer.getNullCell();
  const lines: string[] = [];
  const isWrapped: boolean[] = [];
  const attrs: AttrRun[][] = [];
  for (let y = 0; y < buffer.length; y++) {
    const line = buffer.getLine(y);
    // OSC 8 link ids live outside the cell text and attributes read here, so
    // they never reach the dump (xterm.js #6189). Only the terminal's width
    // counts: a narrowing resize without reflow (the alternate screen) keeps the
    // cut-off cells, which nothing shows.
    lines.push(line ? line.translateToString(true, 0, cols) : "");
    isWrapped.push(line ? line.isWrapped : false);
    attrs.push(rowAttrs(buffer, y, cols, cell));
  }
  return { lines, isWrapped, attrs };
}

/** Cursor visibility (DECTCEM) has no public accessor on either package; both
 *  keep it on the same core service. */
function cursorVisible(term: DumpableTerminal): boolean {
  const core = (term as unknown as { _core?: { coreService?: { isCursorHidden?: boolean } } })
    ._core;
  return !(core?.coreService?.isCursorHidden ?? false);
}

/** Dump `term`'s buffers in the one shape every parity check compares. */
export function referenceBuffer(term: DumpableTerminal): ReferenceBuffer {
  const activeBuffer = term.buffer.active.type;
  return {
    ...plane(term.buffer.normal, term.cols),
    cursorX: term.buffer.active.cursorX,
    cursorY: term.buffer.active.cursorY,
    cursorVisible: cursorVisible(term),
    activeBuffer,
    alt: activeBuffer === "alternate" ? plane(term.buffer.alternate, term.cols) : null,
    cols: term.cols,
    rows: term.rows,
  };
}
