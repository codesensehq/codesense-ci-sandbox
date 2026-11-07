// SPDX-License-Identifier: MIT
/**
 * A terminal's state as a string a fresh xterm can read: SerializeAddon output
 * plus the few things the addon does not carry, and the size and stream
 * position it was taken at.
 *
 * Restoring is "construct at `cols × rows`, write `vt`, then keep writing the
 * stream from `seq`". For that to equal never having rebuilt, `vt` must leave
 * the fresh terminal exactly where the source stood, parser included:
 *
 * - The addon's final cursor move is relative to where its content ends, which
 *   misplaces the cursor (xterm.js #6165), and setting origin mode after the
 *   content homes it. So an absolute cursor position is appended last.
 * - The addon does not carry cursor visibility, cursor style, the scroll
 *   region, the mouse encoding (1006 / 1016) or a pending wrap. Each is
 *   appended when it differs from a fresh terminal's.
 * - A feed can end inside an escape sequence (or between the halves of a
 *   surrogate pair). The source parser holds those characters; the buffer does
 *   not show them. `SequenceTail` keeps them and they are appended last, so the
 *   fresh parser continues the sequence the way the source did.
 *
 * OSC 8 hyperlinks are written back as plain text (xterm.js #6189) and the
 * window title is not carried; both are accepted.
 */
import type { Terminal } from "@xterm/headless";
import { SerializeAddon } from "@xterm/addon-serialize";

/** What a display is rebuilt from. Mirrors the Rust `TerminalSnapshot`. */
export interface TerminalSnapshot {
  /** The emulator's size at `seq`; the display is built at exactly this size. */
  cols: number;
  rows: number;
  /** Byte offset: every later output has `seq_end > seq`. */
  seq: number;
  /** Resizes applied up to `seq`: every later resize has a greater one. */
  resize_gen: number;
  /** The session's scrollback-lines setting. */
  scrollback: number;
  vt: string;
}

const ESC = 0x1b;
const CAN = 0x18;
const SUB = 0x1a;
const BEL = 0x07;

const enum State {
  Ground,
  /** After ESC, or ESC plus intermediates. C0 controls here execute. */
  Escape,
  /** CSI parameters and intermediates. C0 controls here execute. */
  Csi,
  /** OSC, DCS, SOS, PM or APC payload, ended by ESC (or BEL for OSC). */
  String,
}

/**
 * Tracks whether the characters written so far end inside an escape sequence
 * and, if so, keeps the characters a fresh parser must see to be in the same
 * state. Mirrors xterm's `EscapeSequenceParser` transitions for 7-bit and C1
 * introducers.
 */
export class SequenceTail {
  private state = State.Ground;
  /** Characters of the unfinished sequence from earlier writes. */
  private carried = "";
  /** A high surrogate whose low half has not arrived. */
  private highSurrogate = "";

  /** Feed the same characters the terminal is written, in the same order. */
  push(data: string): void {
    let state = this.state;
    let start = state === State.Ground ? -1 : 0;
    let carried = state === State.Ground ? "" : this.carried;
    this.highSurrogate = "";
    for (let i = 0; i < data.length; i++) {
      const c = data.charCodeAt(i);
      if (c === ESC) {
        state = State.Escape;
        start = i;
        carried = "";
        continue;
      }
      if (c === CAN || c === SUB) {
        state = State.Ground;
        continue;
      }
      switch (state) {
        case State.Ground:
          if (c === 0x9b || c === 0x9d || c === 0x90 || c === 0x98 || c === 0x9e || c === 0x9f) {
            state = c === 0x9b ? State.Csi : State.String;
            start = i;
            carried = "";
          }
          break;
        case State.Escape:
          if (c === 0x5b) state = State.Csi;
          else if (c === 0x5d || c === 0x50 || c === 0x58 || c === 0x5e || c === 0x5f) {
            state = State.String;
          } else if (c >= 0x30 && c <= 0x7e) state = State.Ground;
          break;
        case State.Csi:
          if (c >= 0x40 && c <= 0x7e) state = State.Ground;
          break;
        case State.String:
          if (c === BEL || c === 0x9c) state = State.Ground;
          break;
      }
    }
    this.state = state;
    if (state === State.Ground) {
      this.carried = "";
      const last = data.charCodeAt(data.length - 1);
      if (last >= 0xd800 && last <= 0xdbff) this.highSurrogate = data[data.length - 1];
      return;
    }
    let tail = data.slice(start < 0 ? 0 : start);
    // A C0 control inside ESC or CSI executes immediately and is not part of
    // the sequence; replaying it would execute it twice.
    // eslint-disable-next-line no-control-regex
    if (state !== State.String) tail = tail.replace(/[\x00-\x17\x19\x1c-\x1f]/g, "");
    this.carried = carried + tail;
  }

  /** The characters that put a fresh parser where the source's stands. */
  get pending(): string {
    return this.state === State.Ground ? this.highSurrogate : this.carried;
  }
}

/** The attribute surface both a buffer cell and xterm's pen share. */
interface Attributes {
  isBold(): number;
  isItalic(): number;
  isDim(): number;
  isUnderline(): number;
  isBlink(): number;
  isInverse(): number;
  isInvisible(): number;
  isStrikethrough(): number;
  isOverline(): number;
  getFgColor(): number;
  getBgColor(): number;
  isFgDefault(): boolean;
  isBgDefault(): boolean;
  isFgRGB(): boolean;
  isBgRGB(): boolean;
}

interface CoreBuffer {
  x: number;
  y: number;
  ybase: number;
  scrollTop: number;
  scrollBottom: number;
  savedX: number;
  savedY: number;
  savedCurAttrData: Attributes;
}

interface CoreInternals {
  buffer: CoreBuffer;
  buffers: { normal: CoreBuffer };
  coreService: {
    isCursorHidden: boolean;
    decPrivateModes: {
      cursorStyle?: "block" | "underline" | "bar";
      cursorBlink?: boolean;
      origin: boolean;
    };
  };
  coreMouseService: { activeEncoding: string };
  _inputHandler: { _curAttrData: Attributes };
}

/** No public accessor exists for what the addon misses; `buffer-dump.ts` reads
 *  cursor visibility the same way. */
function core(term: Terminal): CoreInternals {
  return (term as unknown as { _core: CoreInternals })._core;
}

function colorParams(attrs: Attributes, fg: boolean): string[] {
  if (fg ? attrs.isFgDefault() : attrs.isBgDefault()) return [];
  const color = fg ? attrs.getFgColor() : attrs.getBgColor();
  if (fg ? attrs.isFgRGB() : attrs.isBgRGB()) {
    const rgb = [(color >> 16) & 0xff, (color >> 8) & 0xff, color & 0xff];
    return [fg ? "38" : "48", "2", ...rgb.map(String)];
  }
  if (color < 8) return [String((fg ? 30 : 40) + color)];
  if (color < 16) return [String((fg ? 90 : 100) + color - 8)];
  return [fg ? "38" : "48", "5", String(color)];
}

const RESET_PEN = "\x1b[0m";

/** An SGR that sets exactly `attrs`, from a reset. */
export function sgrFor(attrs: Attributes): string {
  const params = ["0"];
  if (attrs.isBold()) params.push("1");
  if (attrs.isDim()) params.push("2");
  if (attrs.isItalic()) params.push("3");
  if (attrs.isUnderline()) params.push("4");
  if (attrs.isBlink()) params.push("5");
  if (attrs.isInverse()) params.push("7");
  if (attrs.isInvisible()) params.push("8");
  if (attrs.isStrikethrough()) params.push("9");
  if (attrs.isOverline()) params.push("53");
  params.push(...colorParams(attrs, true), ...colorParams(attrs, false));
  return `\x1b[${params.join(";")}m`;
}

const CURSOR_STYLE_PARAM = { block: 2, underline: 4, bar: 6 } as const;

/**
 * What `serialize` leaves out, as sequences written after it: the scroll
 * region, mouse encoding, cursor style and visibility, synchronized output, the
 * saved cursor (`ESC 7`), and an absolute cursor position, pending wrap
 * included.
 */
export function stateFixup(term: Terminal): string {
  const c = core(term);
  const { cols, rows } = term;
  const buffer = c.buffer;
  const modes = c.coreService.decPrivateModes;
  let out = "";
  if (buffer.scrollTop !== 0 || buffer.scrollBottom !== rows - 1) {
    out += `\x1b[${buffer.scrollTop + 1};${buffer.scrollBottom + 1}r`;
  }
  const encoding = c.coreMouseService.activeEncoding;
  if (encoding === "SGR") out += "\x1b[?1006h";
  else if (encoding === "SGR_PIXELS") out += "\x1b[?1016h";
  if (modes.cursorStyle !== undefined) {
    const param = CURSOR_STYLE_PARAM[modes.cursorStyle] - (modes.cursorBlink ? 1 : 0);
    out += `\x1b[${param} q`;
  }
  if (c.coreService.isCursorHidden) out += "\x1b[?25l";
  if (term.modes.synchronizedOutputMode) out += "\x1b[?2026h";

  let penMoved = false;
  // Origin mode would make the saved row relative to the scroll region.
  if (modes.origin) out += "\x1b[?6l";
  const savedRow = Math.max(buffer.savedY - buffer.ybase, 0);
  const savedPen = sgrFor(buffer.savedCurAttrData);
  if (buffer.savedX !== 0 || savedRow !== 0 || savedPen !== RESET_PEN) {
    out += `\x1b[${savedRow + 1};${Math.min(buffer.savedX, cols - 1) + 1}H${savedPen}\x1b7`;
    penMoved = true;
  }
  // Setting origin mode homes the cursor; it is placed after.
  if (modes.origin) out += "\x1b[?6h";
  const top = modes.origin ? buffer.scrollTop : 0;
  const row = buffer.y - top + 1;
  if (buffer.x >= cols) {
    out += reprintLastCell(term, row);
    penMoved = true;
  } else {
    out += `\x1b[${row};${buffer.x + 1}H`;
  }
  if (penMoved) out += sgrFor(c._inputHandler._curAttrData);
  return out;
}

/**
 * A pending wrap (the cursor past the last column after printing there) cannot
 * be set by a cursor move. Print the row's last glyph again, with its own
 * attributes, which leaves the cursor exactly there.
 */
function reprintLastCell(term: Terminal, row: number): string {
  const { cols } = term;
  const buffer = term.buffer.active;
  const line = buffer.getLine(buffer.baseY + buffer.cursorY);
  let col = cols - 1;
  let cell = line?.getCell(col);
  // The last cell is the right half of a wide glyph: print the glyph.
  if (cell && cell.getWidth() === 0 && col > 0) cell = line?.getCell(--col);
  const glyph = cell?.getChars() || " ";
  const insert = term.modes.insertMode;
  return (
    `\x1b[${row};${col + 1}H` +
    (insert ? "\x1b[4l" : "") +
    (cell ? sgrFor(cell) : RESET_PEN) +
    glyph +
    (insert ? "\x1b[4h" : "")
  );
}

type Line = NonNullable<ReturnType<Terminal["buffer"]["active"]["getLine"]>>;
type Buffer = Terminal["buffer"]["active"];

/** A row as wide as the terminal. After a narrowing resize without reflow
 *  (the alternate screen) xterm keeps the cut-off cells; the addon would write
 *  them and wrap them onto the next row. */
function clampLine(line: Line, cols: number): Line {
  if (line.length <= cols) return line;
  return {
    get isWrapped() {
      return line.isWrapped;
    },
    length: cols,
    getCell: (x, cell) => (x < cols ? line.getCell(x, cell) : undefined),
    translateToString: (trim, start = 0, end = cols) =>
      line.translateToString(trim, start, Math.min(end, cols)),
  };
}

function clampBuffer(buffer: Buffer, cols: number): Buffer {
  return new Proxy(buffer, {
    get(target, prop) {
      if (prop === "getLine") {
        return (y: number) => {
          const line = target.getLine(y);
          return line && clampLine(line, cols);
        };
      }
      const value = Reflect.get(target, prop, target);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
}

/** `term` as the addon should see it: every row clamped to the width. */
function clampedView(term: Terminal): Terminal {
  const buffers = {
    get active() {
      return clampBuffer(term.buffer.active, term.cols);
    },
    get normal() {
      return clampBuffer(term.buffer.normal, term.cols);
    },
    get alternate() {
      return clampBuffer(term.buffer.alternate, term.cols);
    },
  };
  return new Proxy(term, {
    get(target, prop) {
      if (prop === "buffer") return buffers;
      return Reflect.get(target, prop, target);
    },
  });
}

const ALT_ENTER = "\x1b[?1049h\x1b[H";

/** Where the stream stood when a snapshot was taken; the host owns both. */
export interface StreamPosition {
  seq: number;
  resize_gen: number;
}

/**
 * Serializes one terminal. Every string written to the terminal must also be
 * passed to `written`, in the same order, so a snapshot can finish a sequence
 * the last write left open.
 */
export class SnapshotSource {
  private readonly addon = new SerializeAddon();
  private readonly tail = new SequenceTail();

  constructor(private readonly term: Terminal) {
    this.addon.activate(clampedView(term) as never);
  }

  written(data: string): void {
    this.tail.push(data);
  }

  /** Call only once every write so far has been parsed. */
  snapshot(position: StreamPosition): TerminalSnapshot {
    const { cols, rows } = this.term;
    const scrollback = this.term.options.scrollback ?? 0;
    const vt = this.serializeWithAltPen(scrollback) + stateFixup(this.term) + this.tail.pending;
    return { cols, rows, seq: position.seq, resize_gen: position.resize_gen, scrollback, vt };
  }

  /**
   * The addon's output with the pen fixed at the alternate screen switch. It
   * enters the alternate screen with the normal buffer's current pen still set,
   * while its alternate content assumes a default pen, so every cell up to the
   * first colour change takes the wrong colours. And `?1049h` saves the pen
   * the normal screen gets back on exit, which must be the one the source
   * saved when it entered.
   */
  private serializeWithAltPen(scrollback: number): string {
    const vt = this.addon.serialize({ scrollback, excludeModes: false, excludeAltBuffer: false });
    const at = vt.indexOf(ALT_ENTER);
    if (at < 0) return vt;
    const saved = core(this.term).buffers.normal.savedCurAttrData;
    return (
      vt.slice(0, at) + sgrFor(saved) + ALT_ENTER + "\x1b[0m" + vt.slice(at + ALT_ENTER.length)
    );
  }

  /**
   * The normal buffer alone, with no modes and a default pen: what a restarted
   * process's terminal starts from. A full screen program the old process was
   * showing is not carried.
   */
  normalBufferCarry(): string {
    const scrollback = this.term.options.scrollback ?? 0;
    return (
      this.addon.serialize({ scrollback, excludeModes: true, excludeAltBuffer: true }) + "\x1b[0m"
    );
  }
}
