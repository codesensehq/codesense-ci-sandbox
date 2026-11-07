// SPDX-License-Identifier: MIT
/**
 * Drives an in-process `EmulatorHost` through the same JSON payloads the host
 * sends over the pipe, for the worker's tests and budget bench.
 */
import { EmulatorHost, type EmulatorHostOptions } from "./emulator";
import type {
  AppliedReply,
  DumpReply,
  Reply,
  SessionRequest,
  SnapshotReply,
  TextMode,
  TextReply,
} from "./protocol";
import type { ReferenceBuffer } from "../src/shared/terminal/buffer-dump";
import type { PtylogRecord } from "../src/shared/terminal/__fixtures__/ptylog";
import { PTYLOG_START } from "../src/shared/terminal/__test-utils__/headless-from";

/** xterm's default scrollback, which the reference terminals use. */
export const REFERENCE_SCROLLBACK = 1000;

type Pending = { resolve: (reply: Reply) => void };

export class TestWorker {
  readonly host: EmulatorHost;
  readonly applied: AppliedReply[] = [];
  readonly errors: Reply[] = [];
  private readonly pending = new Map<number, Pending>();
  private nextId = 1;

  constructor(options: EmulatorHostOptions = {}) {
    this.host = new EmulatorHost((reply) => this.receive(reply), options);
  }

  send(request: SessionRequest): void {
    this.host.handleFrame(new TextEncoder().encode(JSON.stringify(request)));
  }

  create(session: number, fields: Partial<Omit<SessionRequest & { type: "create" }, "type">> = {}) {
    this.send({
      type: "create",
      session,
      cols: PTYLOG_START.cols,
      rows: PTYLOG_START.rows,
      scrollback: REFERENCE_SCROLLBACK,
      seq_base: 0,
      resize_gen_base: 0,
      history_lost: false,
      ...fields,
    });
  }

  dump(session: number): Promise<DumpReply> {
    const id = this.nextId++;
    const reply = this.await(id);
    this.send({ type: "dump", id, session });
    return reply as Promise<DumpReply>;
  }

  text(session: number, mode: TextMode, lines = 10_000): Promise<TextReply> {
    const id = this.nextId++;
    const reply = this.await(id);
    this.send({ type: "text", id, session, lines, mode });
    return reply as Promise<TextReply>;
  }

  snapshot(session: number): Promise<SnapshotReply> {
    const id = this.nextId++;
    const reply = this.await(id);
    this.send({ type: "snapshot", id, session });
    return reply as Promise<SnapshotReply>;
  }

  private await(id: number): Promise<Reply> {
    return new Promise((resolve) => this.pending.set(id, { resolve }));
  }

  private receive(reply: Reply): void {
    if (reply.type === "applied") {
      this.applied.push(reply);
      return;
    }
    if (reply.type === "error") this.errors.push(reply);
    const id = "id" in reply ? reply.id : null;
    if (id === null) return;
    this.pending.get(id)?.resolve(reply);
    this.pending.delete(id);
  }
}

/** A dump reply's buffer, without the reply's own fields. */
export function bufferOf(reply: DumpReply): ReferenceBuffer {
  const {
    id: _id,
    type: _type,
    session: _session,
    seq_end: _seqEnd,
    resize_gen: _resizeGen,
    history_lost: _historyLost,
    ...buffer
  } = reply;
  return buffer;
}

/** Deterministic PRNG (mulberry32), so a failing chunking can be replayed. */
export function seededRandom(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** One step of a recording as the host would send it. */
export type Step =
  | { kind: "feed"; seqEnd: number; data: string }
  | { kind: "resize"; cols: number; rows: number };

/** A recording as runs of characters between resizes. A character a
 *  recording splits across two writes is joined, as the host's batcher does
 *  before it delivers anything. */
export type Segment =
  | { kind: "text"; chars: string[] }
  | { kind: "resize"; cols: number; rows: number };

export function segmentsOf(records: readonly PtylogRecord[]): Segment[] {
  const segments: Segment[] = [];
  const decoder = new TextDecoder();
  for (const record of records) {
    if (record.kind === "resize") {
      segments.push({ kind: "resize", cols: record.cols, rows: record.rows });
      continue;
    }
    const last = segments[segments.length - 1];
    const chars = Array.from(decoder.decode(record.data, { stream: true }));
    if (last?.kind === "text") for (const char of chars) last.chars.push(char);
    else segments.push({ kind: "text", chars });
  }
  return segments;
}

/**
 * A recording's output re-cut into random chunks (never inside a character:
 * the host only ever delivers whole UTF-8 characters), with every resize kept
 * at its byte offset.
 */
export function chunked(segments: readonly Segment[], random: () => number): Step[] {
  const encoder = new TextEncoder();
  const steps: Step[] = [];
  let seq = 0;
  for (const segment of segments) {
    if (segment.kind === "resize") {
      steps.push(segment);
      continue;
    }
    let at = 0;
    while (at < segment.chars.length) {
      const size = 1 + Math.floor(random() * Math.min(8192, segment.chars.length - at));
      const piece = segment.chars.slice(at, at + size).join("");
      at += size;
      seq += encoder.encode(piece).byteLength;
      steps.push({ kind: "feed", seqEnd: seq, data: piece });
    }
  }
  return steps;
}

/** Send `steps` for `session`, numbering resizes from 1. */
export function play(worker: TestWorker, session: number, steps: readonly Step[]): void {
  let resizeGen = 0;
  for (const step of steps) {
    if (step.kind === "feed") {
      worker.send({ type: "feed", session, seq_end: step.seqEnd, data: step.data });
    } else {
      resizeGen += 1;
      worker.send({
        type: "resize",
        session,
        cols: step.cols,
        rows: step.rows,
        resize_gen: resizeGen,
      });
    }
  }
}
