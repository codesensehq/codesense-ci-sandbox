// SPDX-License-Identifier: MIT
/**
 * A recorded stream as the worker receives it (decoded text and resizes, each
 * resize at the text offset it was applied at), and seeded cut points into it.
 * Shared by the snapshot tests and the snapshot fixture writer.
 */
import { Terminal } from "@xterm/headless";
import { loadPtylog, type PtylogFixture } from "../src/shared/terminal/__fixtures__/ptylog";
import type { TerminalSnapshot } from "./snapshot";
import { seededRandom } from "./test-worker";

export type StreamEvent =
  | { kind: "output"; text: string; seqEnd: number }
  | { kind: "resize"; cols: number; rows: number };

export interface Stream {
  events: StreamEvent[];
  /** Total text length. */
  length: number;
}

/** Decode a fixture the way the host delivers it: one UTF-8 decoder carried
 *  across batches. */
export function loadStream(name: PtylogFixture): Stream {
  const decoder = new TextDecoder();
  const events: StreamEvent[] = [];
  let length = 0;
  for (const record of loadPtylog(name)) {
    if (record.kind === "output") {
      const text = decoder.decode(record.data, { stream: true });
      events.push({ kind: "output", text, seqEnd: record.seqEnd });
      length += text.length;
    } else {
      events.push({ kind: "resize", cols: record.cols, rows: record.rows });
    }
  }
  return { events, length };
}

/** One checked-in file under `tests/fixtures/terminal/snapshots/`. */
export interface SnapshotFixture {
  fixture: PtylogFixture;
  /** `loadPtylog(fixture).slice(rest_from)` is what follows the snapshot. */
  rest_from: number;
  snapshot: TerminalSnapshot;
}

/** Where a fed stream stands: `offset` text characters written, and every
 *  resize at or before that offset applied. */
export interface Cut {
  offset: number;
  /** Why this cut was picked, printed on failure. */
  label: string;
}

/** What a stream feeds: a worker session, a headless terminal, a display. */
export interface StreamSink {
  write(text: string): Promise<void> | void;
  resize(cols: number, rows: number): Promise<void> | void;
}

/**
 * Feeds a stream to a sink in order and can stop at any offset: every text
 * character before it, and every resize at or before it.
 */
export class StreamCursor {
  private index = 0;
  /** Characters of `events[index]` already written. */
  private within = 0;
  offset = 0;
  /** Resizes applied so far. */
  resizeGen = 0;
  /** The `seq_end` of the last complete output written. */
  seq = 0;

  constructor(
    private readonly stream: Stream,
    private readonly sink: StreamSink,
  ) {}

  /** Feed everything up to `target`. */
  advanceTo(target: number): Promise<void> {
    return this.step(target, true);
  }

  /** Move to `target` without feeding: what a restored terminal already holds. */
  skipTo(target: number): Promise<void> {
    return this.step(target, false);
  }

  private async step(target: number, apply: boolean): Promise<void> {
    const { events } = this.stream;
    while (this.index < events.length) {
      const event = events[this.index];
      if (event.kind === "resize") {
        if (this.offset > target) break;
        if (apply) await this.sink.resize(event.cols, event.rows);
        this.resizeGen++;
        this.index++;
        continue;
      }
      if (this.offset >= target && event.text.length > 0) break;
      const take = Math.min(event.text.length - this.within, target - this.offset);
      if (apply && take > 0)
        await this.sink.write(event.text.slice(this.within, this.within + take));
      this.within += take;
      this.offset += take;
      if (this.within < event.text.length) break;
      this.seq = event.seqEnd;
      this.index++;
      this.within = 0;
    }
  }

  /** Events fully applied; the stream's records from here on are still to come. */
  get applied(): number {
    return this.index;
  }

  async toEnd(): Promise<void> {
    await this.advanceTo(Number.POSITIVE_INFINITY);
  }
}

function textOf(stream: Stream): string {
  let text = "";
  for (const event of stream.events) if (event.kind === "output") text += event.text;
  return text;
}

function resizeOffsets(stream: Stream): number[] {
  const offsets: number[] = [];
  let offset = 0;
  for (const event of stream.events) {
    if (event.kind === "output") offset += event.text.length;
    else offsets.push(offset);
  }
  return offsets;
}

/** Offsets strictly inside a CSI sequence, after `ESC [` and before the final. */
function midCsiOffsets(text: string): number[] {
  const out: number[] = [];
  // eslint-disable-next-line no-control-regex
  for (const m of text.matchAll(/\x1b\[[\x30-\x3f]+[\x20-\x2f]*[\x40-\x7e]/g)) {
    for (let k = 1; k < m[0].length; k++) out.push(m.index + k);
  }
  return out;
}

/** Offsets where the alternate screen is active. */
function altScreenOffsets(text: string): number[] {
  const out: number[] = [];
  let enter = -1;
  // eslint-disable-next-line no-control-regex
  for (const m of text.matchAll(/\x1b\[\?1049([hl])/g)) {
    if (m[1] === "h" && enter < 0) enter = m.index + m[0].length;
    else if (m[1] === "l" && enter >= 0) {
      for (let o = enter; o <= m.index; o += 7) out.push(o);
      enter = -1;
    }
  }
  if (enter >= 0) for (let o = enter; o <= text.length; o += 7) out.push(o);
  return out;
}

/** Offsets inside a partial scroll region (`CSI t ; b r` with t > 1 or b < rows). */
function scrollRegionOffsets(text: string): number[] {
  const out: number[] = [];
  let start = -1;
  // eslint-disable-next-line no-control-regex
  for (const m of text.matchAll(/\x1b\[(\d*)(?:;(\d*))?r/g)) {
    const partial = m[1] !== "" && m[1] !== "1" ? true : m[2] !== undefined && m[2] !== "";
    if (start >= 0) {
      for (let o = start; o <= m.index; o += 3) out.push(o);
      start = -1;
    }
    if (partial) start = m.index + m[0].length;
  }
  return out;
}

/**
 * `count` seeded cut offsets over `stream`, a share of them in each kind of
 * place a snapshot is hard to take: inside a CSI sequence, on the alternate
 * screen, inside a scroll region, right after a resize. The rest are uniform.
 */
export function seededCuts(stream: Stream, count: number, seed: number): Cut[] {
  const random = seededRandom(seed);
  const text = textOf(stream);
  const pick = (from: number[]) => from[Math.floor(random() * from.length)];
  const kinds: Array<[string, number[]]> = [
    ["mid-csi", midCsiOffsets(text)],
    ["alt-screen", altScreenOffsets(text)],
    ["scroll-region", scrollRegionOffsets(text)],
    ["after-resize", resizeOffsets(stream)],
  ];
  const cuts: Cut[] = [];
  const share = Math.floor(count / (kinds.length + 1));
  for (const [label, offsets] of kinds) {
    if (offsets.length === 0) continue;
    for (let i = 0; i < share; i++) cuts.push({ offset: pick(offsets), label });
  }
  while (cuts.length < count)
    cuts.push({ offset: Math.floor(random() * text.length), label: "uniform" });
  return cuts
    .map((cut) => ({ ...cut, offset: avoidSurrogateSplit(text, cut.offset) }))
    .sort((a, b) => a.offset - b.offset);
}

/** A cut between the halves of a surrogate pair moves one character later. The
 *  host never delivers half a character. */
function avoidSurrogateSplit(text: string, offset: number): number {
  const before = text.charCodeAt(offset - 1);
  return before >= 0xd800 && before <= 0xdbff ? offset + 1 : offset;
}

/** Write and resolve once xterm has parsed it. */
export function writeParsed(term: Terminal, data: string): Promise<void> {
  return new Promise((resolve) => term.write(data, resolve));
}
