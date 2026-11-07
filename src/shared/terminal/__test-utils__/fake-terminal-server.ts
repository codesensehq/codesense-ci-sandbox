// SPDX-License-Identifier: MIT
/**
 * A fake terminal server and Tauri event bus for tests of the pane's live
 * stream: reserve / subscribe / unsubscribe with the app's stream-id rules, a
 * real headless emulator per session answering subscribe with the backend's
 * snapshot (the worker's own `SnapshotSource`), output and resizes fanned out
 * to bound streams as `pty-output-stream-{id}` events in emission order, and
 * `terminal-streams-lost` for a reconnect that drops streams. No React, no DOM.
 */
import { Terminal } from "@xterm/headless";
import { SnapshotSource } from "../../../../src-emulator/snapshot";
import type { TerminalSubscription } from "@/tauri/commands";
import { PTYLOG_START } from "./headless-from";
import { TERMINAL_PARSER_OPTIONS, activateTerminalParser } from "../terminal-parser-options";

type Handler = (event: { payload: unknown }) => void;

export class FakeEventBus {
  private readonly handlers = new Map<string, Set<Handler>>();

  listen = (name: string, handler: Handler): Promise<() => void> => {
    const set = this.handlers.get(name) ?? new Set<Handler>();
    set.add(handler);
    this.handlers.set(name, set);
    return Promise.resolve(() => {
      set.delete(handler);
    });
  };

  emit(name: string, payload: unknown): void {
    for (const handler of [...(this.handlers.get(name) ?? [])]) handler({ payload });
  }

  count(name: string): number {
    return this.handlers.get(name)?.size ?? 0;
  }
}

const utf8 = new TextEncoder();

/** The line a restart in place leaves between the old run and the new. */
export const RESTART_SEAM = "─── session restarted ───";

interface FakeSession {
  term: Terminal;
  cols: number;
  rows: number;
  source: SnapshotSource;
  seq: number;
  resizeGen: number;
  /** Every emulator step in order; a snapshot is one of them. */
  chain: Promise<void>;
}

export interface FakeSessionOptions {
  cols?: number;
  rows?: number;
  scrollback?: number;
}

export class FakeTerminalServer {
  private next = 0;
  private readonly reserved = new Set<number>();
  private readonly bound = new Map<number, string>();
  private readonly sessions = new Map<string, FakeSession>();
  private held: Array<() => void> = [];
  /** While true, a subscribe takes its snapshot and binds at once but its reply
   *  waits for `releaseSubscribes` — events produced meanwhile are emitted on
   *  the bound stream before the reply arrives. */
  holdSubscribes = false;
  /** While set, every subscribe fails with this error. */
  failSubscribes: string | null = null;

  constructor(private readonly bus: FakeEventBus) {}

  open(session: string, options: FakeSessionOptions = {}): void {
    const term = new Terminal({
      cols: options.cols ?? PTYLOG_START.cols,
      rows: options.rows ?? PTYLOG_START.rows,
      scrollback: options.scrollback ?? 1000,
      ...TERMINAL_PARSER_OPTIONS,
    });
    activateTerminalParser(term);
    this.sessions.set(session, {
      term,
      cols: term.cols,
      rows: term.rows,
      source: new SnapshotSource(term),
      seq: 0,
      resizeGen: 0,
      chain: Promise.resolve(),
    });
  }

  private session(session: string): FakeSession {
    const state = this.sessions.get(session);
    if (!state) throw new Error(`no session ${session}`);
    return state;
  }

  private emitTo(session: string, payload: unknown): void {
    for (const [stream, of] of this.bound) {
      if (of === session) this.bus.emit(`pty-output-stream-${stream}`, payload);
    }
  }

  reserve = async (): Promise<number> => {
    const id = ++this.next;
    this.reserved.add(id);
    return id;
  };

  subscribe = (session: string, stream?: number): Promise<TerminalSubscription> => {
    if (this.failSubscribes) return Promise.reject(new Error(this.failSubscribes));
    const state = this.sessions.get(session);
    if (!state) return Promise.reject(new Error(`subscribe_terminal: session not found`));
    let id: number;
    if (stream === undefined) {
      id = ++this.next;
    } else {
      if (!this.reserved.delete(stream)) {
        return Promise.reject(new Error(`terminal stream ${stream} is not reserved`));
      }
      id = stream;
    }
    this.bound.set(id, session);
    const position = { seq: state.seq, resize_gen: state.resizeGen };
    const snapshot = new Promise<TerminalSubscription>((resolve) => {
      state.chain = state.chain.then(() =>
        resolve({
          snapshot: state.source.snapshot(position),
          stream_id: id,
          story_id: null,
          workspace_id: null,
          epic_id: null,
        }),
      );
    });
    if (!this.holdSubscribes) return snapshot;
    return new Promise((resolve) => this.held.push(() => void snapshot.then(resolve)));
  };

  releaseSubscribes(): void {
    const held = this.held;
    this.held = [];
    for (const release of held) release();
  }

  unsubscribe = async (stream: number): Promise<void> => {
    this.reserved.delete(stream);
    this.bound.delete(stream);
  };

  /** The session prints `data`: every stream bound to it gets the event. */
  produce(session: string, data: string): void {
    const state = this.session(session);
    state.seq += utf8.encode(data).length;
    state.chain = state.chain.then(() => {
      state.source.written(data);
      return new Promise<void>((resolve) => state.term.write(data, resolve));
    });
    this.emitTo(session, { data, seq: state.seq });
  }

  /** The PTY is resized: a size change emits `Resized` in order with output. */
  resize = async (session: string, cols: number, rows: number): Promise<void> => {
    const state = this.sessions.get(session);
    if (!state || (state.cols === cols && state.rows === rows)) return;
    state.cols = cols;
    state.rows = rows;
    state.resizeGen += 1;
    state.chain = state.chain.then(() => state.term.resize(cols, rows));
    this.emitTo(session, {
      resized: { at_seq: state.seq, resize_gen: state.resizeGen, cols, rows },
    });
  };

  /** The PTY's size. */
  size(session: string): { cols: number; rows: number } {
    const { cols, rows } = this.session(session);
    return { cols, rows };
  }

  /** A restart in place: the old run's normal buffer is carried, then the seam
   *  line, then the new process's output. */
  respawn(session: string, prompt: string): void {
    const state = this.session(session);
    state.chain = state.chain.then(() => {
      const carry = `${state.source.normalBufferCarry()}\r\n${RESTART_SEAM}\r\n`;
      state.term.reset();
      state.source.written(carry);
      return new Promise<void>((resolve) => state.term.write(carry, resolve));
    });
    this.produce(session, prompt);
  }

  /** The emulator once every step so far has run. */
  async emulator(session: string): Promise<Terminal> {
    const state = this.session(session);
    await state.chain;
    return state.term;
  }

  /** A reconnect that could not carry `streams` across. */
  loseStreams(streams: number[]): void {
    for (const stream of streams) this.bound.delete(stream);
    this.bus.emit("terminal-streams-lost", { streams });
  }

  seqOf(session: string): number {
    return this.sessions.get(session)?.seq ?? 0;
  }

  boundStreams(): number[] {
    return [...this.bound.keys()];
  }

  /** Streams bound to `session`: its subscribers. */
  subscribers(session: string): number {
    return [...this.bound.values()].filter((of) => of === session).length;
  }
}
