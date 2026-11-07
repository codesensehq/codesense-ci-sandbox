// SPDX-License-Identifier: MIT
/**
 * One headless xterm per session, each fed through its own ordered apply
 * pipeline.
 *
 * xterm's `write()` is asynchronous (bytes are parsed later, in time slices)
 * while `resize()` and buffer reads act at once. So a session's messages are
 * applied strictly one after another: a `feed` is `write(data, cb)` and the
 * next message for that session starts only in `cb`. Every reply therefore
 * describes a position that has actually been parsed, never a buffer that is
 * half way through a write. Sessions do not wait on each other.
 */
import { Terminal } from "@xterm/headless";
import { referenceBuffer, type ReferenceBuffer } from "../src/shared/terminal/buffer-dump";
import {
  activateTerminalParser,
  TERMINAL_PARSER_OPTIONS,
} from "../src/shared/terminal/terminal-parser-options";
import { SnapshotSource } from "./snapshot";
import {
  currentRuntime,
  decodeRequest,
  helloReply,
  type CreateRequest,
  type Reply,
  type RuntimeInfo,
  type SessionRequest,
  type TextMode,
} from "./protocol";

/** Written as the first line of a terminal recreated after the worker
 *  restarted: what came before is gone, and the user should see why. */
export const HISTORY_LOST_LINE = "─── earlier output unavailable: terminal service restarted ───";

/** Written between the ended process's output and the new one's when a session
 *  is respawned in place. Same text and style as the host's respawn marker. */
export const RESPAWN_LINE = "─── session restarted ───";

const DIM = (text: string) => `\x1b[90m${text}\x1b[0m`;

/** Garbage, in bytes of output parsed, after which the worker collects. */
export const COLLECT_AFTER_BYTES = 2 * 1024 * 1024;

/** A full, synchronous garbage collection where the runtime offers one. */
export function collectGarbage(): void {
  const runtime = globalThis as { Bun?: { gc(force: boolean): void }; gc?: () => void };
  if (runtime.Bun) runtime.Bun.gc(true);
  else runtime.gc?.();
}

/** A write of the worker's own, not of the session's output: it moves no
 *  stream position. */
type SeamOp = { type: "seam"; data: string };
type Op = Exclude<SessionRequest, CreateRequest> | SeamOp;

class SessionTerminal {
  readonly queue: Op[] = [];
  /** A `write` is being parsed; everything else waits for its callback. */
  writing = false;
  /** Sees every write, so a snapshot can finish a sequence a write left open. */
  source: SnapshotSource;

  constructor(
    public term: Terminal,
    public seqEnd: number,
    public resizeGen: number,
    public historyLost: boolean,
  ) {
    this.source = new SnapshotSource(term);
  }

  /** Swap in a fresh terminal (a respawn), keeping the queue and positions. */
  replaceTerminal(term: Terminal): void {
    this.term.dispose();
    this.term = term;
    this.source = new SnapshotSource(term);
  }
}

export interface EmulatorHostOptions {
  /** Answer `stats` (test mode only: it forces a garbage collection). */
  stats?: boolean;
  /** Parser setup of every new terminal; the display's by default. */
  activate?: (term: Terminal) => void;
  runtime?: RuntimeInfo;
  /** Runs a garbage collection; [`collectGarbage`] by default. */
  collect?: () => void;
}

export class EmulatorHost {
  private readonly sessions = new Map<number, SessionTerminal>();
  private readonly activate: (term: Terminal) => void;
  private readonly runtime: RuntimeInfo;
  private readonly collect: () => void;
  /**
   * Garbage made since the last collection, in bytes of output parsed; a dump
   * or snapshot counts as the whole threshold. The runtime's allocator keeps
   * the pages a heap grew into, so garbage that piles up between its own
   * collections stays in RSS for good. Collecting after every few MiB of work
   * lets the next burst reuse those pages instead.
   */
  private garbage = 0;

  constructor(
    private readonly send: (reply: Reply) => void,
    private readonly options: EmulatorHostOptions = {},
  ) {
    this.activate = options.activate ?? activateTerminalParser;
    this.runtime = options.runtime ?? currentRuntime();
    this.collect = options.collect ?? collectGarbage;
  }

  /** Handle one frame's payload. Replies go to `send`, possibly later. */
  handleFrame(payload: Uint8Array): void {
    const decoded = decodeRequest(payload);
    switch (decoded.kind) {
      case "hello":
        this.send(helloReply(decoded.id, this.runtime));
        return;
      case "stats":
        this.send(this.stats(decoded.id));
        return;
      case "error":
        this.send(decoded.reply);
        return;
      case "session":
        this.dispatch(decoded.request);
    }
  }

  dispatch(request: SessionRequest): void {
    if (request.type === "create") {
      this.create(request);
      return;
    }
    const session = this.sessions.get(request.session);
    if (!session) {
      const id = "id" in request ? request.id : null;
      this.send({ id, type: "error", message: `no session ${request.session}` });
      return;
    }
    session.queue.push(request);
    this.pump(request.session, session);
  }

  /** The session's recorded position and buffer as they stand right now,
   *  outside the pipeline. Only for tests that check what a read would see
   *  while a write is still being parsed. */
  debugPeek(key: number): { seqEnd: number; resizeGen: number; buffer: ReferenceBuffer } | null {
    const session = this.sessions.get(key);
    if (!session) return null;
    return {
      seqEnd: session.seqEnd,
      resizeGen: session.resizeGen,
      buffer: referenceBuffer(session.term),
    };
  }

  /** The parser's active Unicode version for a session's terminal. */
  unicodeVersion(key: number): string | null {
    return this.sessions.get(key)?.term.unicode.activeVersion ?? null;
  }

  private newTerminal(cols: number, rows: number, scrollback: number): Terminal {
    const term = new Terminal({ cols, rows, scrollback, ...TERMINAL_PARSER_OPTIONS });
    this.activate(term);
    return term;
  }

  private create(request: CreateRequest): void {
    this.sessions.get(request.session)?.term.dispose();
    const term = this.newTerminal(request.cols, request.rows, request.scrollback);
    const session = new SessionTerminal(
      term,
      request.seq_base,
      request.resize_gen_base,
      request.history_lost,
    );
    this.sessions.set(request.session, session);
    if (request.history_lost) {
      session.queue.push({ type: "seam", data: `${DIM(HISTORY_LOST_LINE)}\r\n` });
      this.pump(request.session, session);
    }
  }

  private pump(key: number, session: SessionTerminal): void {
    while (!session.writing && session.queue.length > 0) {
      this.collectIfDue();
      this.apply(key, session, session.queue.shift() as Op);
    }
    this.collectIfDue();
  }

  /** Collect when heavy work made garbage. Called only between a session's
   *  applies, so the pause never lands inside that session's write. */
  private collectIfDue(): void {
    if (this.garbage < COLLECT_AFTER_BYTES) return;
    this.garbage = 0;
    this.collect();
  }

  private write(key: number, session: SessionTerminal, data: string, done: () => void): void {
    session.writing = true;
    session.source.written(data);
    session.term.write(data, () => {
      session.writing = false;
      done();
      this.pump(key, session);
    });
  }

  private apply(key: number, session: SessionTerminal, op: Op): void {
    const { term } = session;
    switch (op.type) {
      case "feed": {
        const started = performance.now();
        this.garbage += op.data.length;
        this.write(key, session, op.data, () => {
          // The position advances only once the bytes are parsed.
          session.seqEnd = op.seq_end;
          this.send({
            type: "applied",
            session: key,
            seq_end: op.seq_end,
            apply_ms: performance.now() - started,
          });
        });
        return;
      }
      case "seam":
        this.write(key, session, op.data, () => {});
        return;
      case "resize":
        term.resize(op.cols, op.rows);
        session.resizeGen = op.resize_gen;
        return;
      case "dump":
        this.garbage += COLLECT_AFTER_BYTES;
        this.send({
          ...referenceBuffer(term),
          id: op.id,
          type: "dump",
          session: key,
          seq_end: session.seqEnd,
          resize_gen: session.resizeGen,
          history_lost: session.historyLost,
        });
        return;
      case "text":
        this.send({
          id: op.id,
          type: "text",
          session: key,
          seq_end: session.seqEnd,
          resize_gen: session.resizeGen,
          alt_screen: term.buffer.active.type === "alternate",
          lines: textOf(term, op.mode, op.lines),
        });
        return;
      case "snapshot":
        this.garbage += COLLECT_AFTER_BYTES;
        this.send({
          id: op.id,
          type: "snapshot",
          session: key,
          snapshot: session.source.snapshot({
            seq: session.seqEnd,
            resize_gen: session.resizeGen,
          }),
        });
        return;
      case "respawn": {
        session.seqEnd = op.seq_base;
        session.resizeGen = op.resize_gen_base;
        // The new process gets a fresh terminal holding only the old normal
        // buffer: no modes, and no full-screen program's last frame.
        const carry = session.source.normalBufferCarry();
        session.replaceTerminal(
          this.newTerminal(term.cols, term.rows, term.options.scrollback ?? 0),
        );
        this.write(key, session, `${carry}\r\n${DIM(RESPAWN_LINE)}\r\n`, () => {});
        return;
      }
      case "dispose":
        term.dispose();
        if (this.sessions.get(key) === session) this.sessions.delete(key);
        session.queue.length = 0;
        return;
    }
  }

  private stats(id: number): Reply {
    if (!this.options.stats) {
      return { id, type: "error", message: "stats is only answered in test mode" };
    }
    collectGarbage();
    const memory = process.memoryUsage();
    return {
      id,
      type: "stats",
      rss: memory.rss,
      live: memory.heapUsed + memory.arrayBuffers,
      sessions: this.sessions.size,
    };
  }
}

/**
 * `normal`: the last `lines` lines of the normal buffer, a soft-wrapped row
 * joined to the one it continues, trailing blank lines dropped.
 * `screen`: the rows on screen in the active buffer (the alternate screen
 * while it is active).
 */
export function textOf(term: Terminal, mode: TextMode, lines: number): string[] {
  if (mode === "screen") {
    const buffer = term.buffer.active;
    const rows: string[] = [];
    for (let y = 0; y < term.rows; y++) {
      rows.push(buffer.getLine(buffer.baseY + y)?.translateToString(true) ?? "");
    }
    return rows;
  }
  if (lines <= 0) return [];
  // Walk up from the bottom and stop once `lines` logical lines are read, so a
  // short tail (polled on a timer) never serializes the whole scrollback.
  const buffer = term.buffer.normal;
  const logical: string[] = [];
  let pending = "";
  for (let y = buffer.length - 1; y >= 0 && logical.length < lines; y--) {
    const line = buffer.getLine(y);
    if (!line) continue;
    // A row continued on the next one keeps its trailing spaces: they are
    // part of the logical line.
    const continued = buffer.getLine(y + 1)?.isWrapped ?? false;
    pending = line.translateToString(!continued) + pending;
    if (line.isWrapped && y > 0) continue;
    // Trailing blank lines are dropped.
    if (logical.length > 0 || pending !== "") logical.push(pending);
    pending = "";
  }
  return logical.reverse();
}
