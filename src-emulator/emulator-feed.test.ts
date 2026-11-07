// SPDX-License-Identifier: MIT
// @vitest-environment node
import { Terminal } from "@xterm/headless";
import { beforeAll, describe, expect, it } from "vitest";
import { referenceBuffer, type ReferenceBuffer } from "../src/shared/terminal/buffer-dump";
import { loadPtylog, type PtylogFixture } from "../src/shared/terminal/__fixtures__/ptylog";
import {
  headlessFrom,
  parityTerminal,
  writeParsed,
} from "../src/shared/terminal/__test-utils__/headless-from";
import { HISTORY_LOST_LINE } from "./emulator";
import {
  bufferOf,
  chunked,
  play,
  seededRandom,
  segmentsOf,
  TestWorker,
  type Segment,
  type Step,
} from "./test-worker";

const PARITY_FIXTURES: PtylogFixture[] = ["codex-tui", "claude-ink", "tui-fixture"];
const CHUNKINGS = 200;

/** Feed `steps` through a fresh worker session and dump it. */
async function workerDump(steps: readonly Step[]) {
  const worker = new TestWorker();
  worker.create(1);
  play(worker, 1, steps);
  const reply = await worker.dump(1);
  expect(worker.errors).toEqual([]);
  return reply;
}

describe("worker_matches_direct_headless", () => {
  const references = new Map<PtylogFixture, ReferenceBuffer>();
  const segments = new Map<PtylogFixture, Segment[]>();

  beforeAll(async () => {
    for (const fixture of PARITY_FIXTURES) {
      references.set(fixture, await headlessFrom(fixture));
      segments.set(fixture, segmentsOf(loadPtylog(fixture)));
    }
  });

  it.each(PARITY_FIXTURES)(
    "%s: every random chunking dumps exactly what a direct headless terminal does",
    async (fixture) => {
      const records = loadPtylog(fixture);
      const lastSeq = records.reduce((seq, r) => (r.kind === "output" ? r.seqEnd : seq), 0);
      const resizes = records.filter((r) => r.kind === "resize").length;
      for (let seed = 1; seed <= CHUNKINGS; seed++) {
        const steps = chunked(segments.get(fixture)!, seededRandom(seed));
        const reply = await workerDump(steps);
        expect(bufferOf(reply), `${fixture} seed ${seed}`).toEqual(references.get(fixture));
        expect(reply.seq_end, `${fixture} seed ${seed}`).toBe(lastSeq);
        expect(reply.resize_gen, `${fixture} seed ${seed}`).toBe(resizes);
      }
    },
    120_000,
  );

  // Negative control: a worker that drops resizes must be caught.
  it("fails on tui-fixture when the worker skips resizes", async () => {
    const steps = chunked(segments.get("tui-fixture")!, seededRandom(1)).filter(
      (step) => step.kind === "feed",
    );
    const reply = await workerDump(steps);
    expect(bufferOf(reply)).not.toEqual(references.get("tui-fixture"));
  });
});

describe("text_modes", () => {
  async function fed(data: string, cols = 100) {
    const worker = new TestWorker();
    worker.create(1, { cols });
    worker.send({ type: "feed", session: 1, seq_end: data.length, data });
    return worker;
  }

  it("reads the normal buffer and the alternate screen separately", async () => {
    const worker = await fed("shell line\r\n\x1b[?1049h\x1b[Halt row one\r\nalt row two");
    const normal = await worker.text(1, "normal");
    expect(normal.lines).toEqual(["shell line"]);
    expect(normal.alt_screen).toBe(true);
    const screen = await worker.text(1, "screen");
    expect(screen.alt_screen).toBe(true);
    expect(screen.lines.slice(0, 3)).toEqual(["alt row one", "alt row two", ""]);
    expect(screen.lines).toHaveLength(30);
  });

  it("joins soft-wrapped rows in normal mode and keeps them apart on screen", async () => {
    const long = "x".repeat(150);
    const worker = await fed(`${long}\r\nnext`);
    expect((await worker.text(1, "normal")).lines).toEqual([long, "next"]);
    expect((await worker.text(1, "screen")).lines.slice(0, 3)).toEqual([
      "x".repeat(100),
      "x".repeat(50),
      "next",
    ]);
    expect((await worker.text(1, "normal", 1)).lines).toEqual(["next"]);
  });

  it("keeps wide characters and combining marks", async () => {
    const worker = await fed("漢字テスト|\r\né café");
    expect((await worker.text(1, "normal")).lines).toEqual(["漢字テスト|", "é café"]);
  });

  it("an inline codex-tui cut: the screen is the viewport of a headless fed the same bytes", async () => {
    const stream = Buffer.concat(
      loadPtylog("codex-tui").flatMap((r) => (r.kind === "output" ? [r.data] : [])),
    ).toString("utf8");
    const inline = stream.slice(0, stream.indexOf("\x1b[?1049h"));
    expect(inline.length).toBeGreaterThan(0);
    const worker = await fed(inline);
    const screen = await worker.text(1, "screen");
    const reference = parityTerminal();
    await writeParsed(reference, inline);
    const buffer = reference.buffer.active;
    const expected = Array.from(
      { length: reference.rows },
      (_, y) => buffer.getLine(buffer.baseY + y)?.translateToString(true) ?? "",
    );
    expect(screen.alt_screen).toBe(false);
    expect(screen.lines).toEqual(expected);
  });
});

describe("history_lost create", () => {
  it("starts with the restart line and reports the positions it was given", async () => {
    const worker = new TestWorker();
    worker.create(7, { seq_base: 4096, resize_gen_base: 2, history_lost: true });
    worker.send({ type: "feed", session: 7, seq_end: 4100, data: "more" });
    const reply = await worker.dump(7);
    expect(reply.lines[0]).toBe(HISTORY_LOST_LINE);
    expect(reply.lines[1]).toBe("more");
    expect(reply).toMatchObject({ history_lost: true, seq_end: 4100, resize_gen: 2 });
  });
});

describe("apply pipeline", () => {
  const CHUNK = Array.from(
    { length: 16_384 },
    (_, i) => `\x1b[3${i % 8}mline ${i} ${"-".repeat(40 + (i % 13))}\x1b[0m\r\n`,
  ).join("");

  type Applied = { buffer: ReferenceBuffer; seqEnd: number; resizeGen: number };

  /** A 1 MiB feed, then a resize and a dump sent at once, before it is parsed. */
  async function feedResizeDump(
    apply: (chunk: string, seqEnd: number) => Promise<Applied>,
  ): Promise<void> {
    const seqEnd = Buffer.byteLength(CHUNK);
    expect(seqEnd).toBeGreaterThanOrEqual(1024 * 1024);
    const reference = parityTerminal();
    await writeParsed(reference, CHUNK);
    reference.resize(60, 20);
    const got = await apply(CHUNK, seqEnd);
    expect(got.buffer).toEqual(referenceBuffer(reference));
    expect(got.seqEnd).toBe(seqEnd);
    expect(got.resizeGen).toBe(1);
  }

  it("dump_after_unparsed_feed: resize and dump wait for the feed's parse", async () => {
    await feedResizeDump(async (chunk, seqEnd) => {
      const worker = new TestWorker();
      worker.create(1);
      worker.send({ type: "feed", session: 1, seq_end: seqEnd, data: chunk });
      worker.send({ type: "resize", session: 1, cols: 60, rows: 20, resize_gen: 1 });
      const reply = await worker.dump(1);
      return { buffer: bufferOf(reply), seqEnd: reply.seq_end, resizeGen: reply.resize_gen };
    });
  });

  // Negative control: resizing and dumping without waiting for the write
  // callback, as a plain `write` / `resize` / read sequence does, is caught.
  it("fails when resize and dump do not wait for the write callback", async () => {
    await expect(
      feedResizeDump(async (chunk) => {
        const term = new Terminal({ cols: 100, rows: 30, allowProposedApi: true });
        let seqEnd = 0;
        term.write(chunk, () => (seqEnd = Buffer.byteLength(chunk)));
        term.resize(60, 20);
        return { buffer: referenceBuffer(term), seqEnd, resizeGen: 1 };
      }),
    ).rejects.toThrow();
  });

  it("dump_between_enqueue_and_callback: a read while a feed parses sees the previous position", async () => {
    const worker = new TestWorker();
    worker.create(1);
    worker.send({ type: "feed", session: 1, seq_end: 6, data: "first\n" });
    const before = await worker.dump(1);
    worker.send({ type: "feed", session: 1, seq_end: 6 + CHUNK.length, data: CHUNK });
    const peek = worker.host.debugPeek(1)!;
    expect(peek.seqEnd).toBe(6);
    expect(peek.buffer).toEqual(bufferOf(before));
    const pending = worker.dump(1);
    const appliedAtSend = worker.applied.length;
    const after = await pending;
    // The dump was answered only after the big feed's callback.
    expect(worker.applied.length).toBe(appliedAtSend + 1);
    expect(after.seq_end).toBe(6 + CHUNK.length);
    expect(after.lines).toContain(`line 16383 ${"-".repeat(40 + (16_383 % 13))}`);
  });

  it("does not hold one session's messages behind another's parse", async () => {
    const worker = new TestWorker();
    worker.create(1);
    worker.create(2);
    // xterm time-slices between writes, not inside one, so the busy session
    // gets batch-sized feeds as the host sends them: several MiB of them.
    const busy = CHUNK.repeat(4);
    const batch = 64 * 1024;
    let feeds = 0;
    for (let at = 0; at < busy.length; at += batch) {
      const data = busy.slice(at, at + batch);
      worker.send({ type: "feed", session: 1, seq_end: at + data.length, data });
      feeds++;
    }
    worker.send({ type: "feed", session: 2, seq_end: 2, data: "hi" });
    const quick = await worker.dump(2);
    expect(quick.seq_end).toBe(2);
    const busyApplied = worker.applied.filter((a) => a.session === 1).length;
    expect(busyApplied).toBeLessThan(feeds);
  });
});
