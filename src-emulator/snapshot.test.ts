// SPDX-License-Identifier: MIT
// @vitest-environment node
import { writeFileSync } from "node:fs";
import { beforeAll, describe, expect, it } from "vitest";
import { Terminal } from "@xterm/headless";
import { SerializeAddon } from "@xterm/addon-serialize";
import { referenceBuffer } from "../src/shared/terminal/buffer-dump";
import { SequenceTail, SnapshotSource, stateFixup, type TerminalSnapshot } from "./snapshot";
import {
  loadStream,
  seededCuts,
  StreamCursor,
  writeParsed,
  type Cut,
  type Stream,
} from "./snapshot-stream";
import { RESPAWN_LINE } from "./emulator";
import { bufferOf, REFERENCE_SCROLLBACK, seededRandom, TestWorker } from "./test-worker";

const OPTIONS = { allowProposedApi: true, scrollback: 1000 } as const;
const FIXTURES = ["codex-tui", "claude-ink", "tui-fixture"] as const;
type Fixture = (typeof FIXTURES)[number];
/** The size each recording starts at (tests/fixtures/terminal/README.md). */
const START: Record<Fixture, { cols: number; rows: number }> = {
  "codex-tui": { cols: 100, rows: 30 },
  "claude-ink": { cols: 100, rows: 30 },
  "tui-fixture": { cols: 80, rows: 24 },
};
const CUTS = 50;
const SEED = 38;

function sinkFor(term: Terminal, source?: SnapshotSource) {
  return {
    write: async (text: string) => {
      source?.written(text);
      await writeParsed(term, text);
    },
    resize: (cols: number, rows: number) => term.resize(cols, rows),
  };
}

async function restore(
  snapshot: TerminalSnapshot,
  size: { cols: number; rows: number } = snapshot,
): Promise<Terminal> {
  const term = new Terminal({
    ...OPTIONS,
    cols: size.cols,
    rows: size.rows,
    scrollback: snapshot.scrollback,
  });
  await writeParsed(term, snapshot.vt);
  return term;
}

/** The first differing field, for a readable failure list. */
function firstDiff(got: unknown, want: unknown, path = ""): string | null {
  if (JSON.stringify(got) === JSON.stringify(want)) return null;
  if (typeof got !== "object" || typeof want !== "object" || got === null || want === null) {
    return `${path}: got ${JSON.stringify(got)} want ${JSON.stringify(want)}`;
  }
  for (const key of new Set([...Object.keys(got), ...Object.keys(want)])) {
    const diff = firstDiff(
      (got as Record<string, unknown>)[key],
      (want as Record<string, unknown>)[key],
      `${path}.${key}`,
    );
    if (diff) return diff;
  }
  return null;
}

interface CutResult {
  cut: Cut;
  roundtrip: string | null;
  continued: string | null;
}

/** Walk a fixture once, snapshotting at every cut: each snapshot is restored
 *  and compared (AC-1), then fed the rest of the stream and compared with the
 *  terminal that saw it all live (AC-2). */
async function walkCuts(name: Fixture): Promise<CutResult[]> {
  const stream = loadStream(name);
  const liveTerm = new Terminal({ ...OPTIONS, ...START[name] });
  await new StreamCursor(stream, sinkFor(liveTerm)).toEnd();
  const live = referenceBuffer(liveTerm);
  liveTerm.dispose();

  const term = new Terminal({ ...OPTIONS, ...START[name] });
  const source = new SnapshotSource(term);
  const walk = new StreamCursor(stream, sinkFor(term, source));
  const results: CutResult[] = [];
  for (const cut of seededCuts(stream, CUTS, SEED)) {
    await walk.advanceTo(cut.offset);
    const snapshot = source.snapshot({ seq: walk.seq, resize_gen: walk.resizeGen });
    const restored = await restore(snapshot);
    const roundtrip = firstDiff(referenceBuffer(restored), referenceBuffer(term));
    const rest = new StreamCursor(stream, sinkFor(restored));
    await rest.skipTo(cut.offset);
    await rest.toEnd();
    const continued = firstDiff(referenceBuffer(restored), live);
    restored.dispose();
    results.push({ cut, roundtrip, continued });
  }
  term.dispose();
  return results;
}

function failures(results: CutResult[], field: "roundtrip" | "continued"): string[] {
  return results
    .filter((r) => r[field] !== null)
    .map((r) => `seed=${SEED} cut=${r.cut.offset} (${r.cut.label}) ${r[field]}`);
}

describe.each(FIXTURES)("snapshot of %s", (name) => {
  let results: CutResult[] = [];
  beforeAll(async () => {
    results = await walkCuts(name);
  }, 120_000);

  it("covers the hard cut kinds", () => {
    const labels = new Set(results.map((r) => r.cut.label));
    expect(labels).toContain("mid-csi");
    expect(labels).toContain("after-resize");
    if (name !== "claude-ink") expect(labels).toContain("alt-screen");
    if (name === "tui-fixture") expect(labels).toContain("scroll-region");
  });

  it("roundtrip_prop: a fresh terminal written the snapshot equals the source", () => {
    expect(failures(results, "roundtrip")).toEqual([]);
  });

  it("snapshot_then_rest_equals_live: restored, then fed the rest, equals live", () => {
    expect(failures(results, "continued")).toEqual([]);
  });
});

/** Snapshot `data` written to a `cols × rows` terminal, restore it, and write
 *  `after` to both. */
async function roundtrip(data: string, after = "", size = { cols: 10, rows: 4 }) {
  const term = new Terminal({ ...OPTIONS, ...size });
  const source = new SnapshotSource(term);
  source.written(data);
  await writeParsed(term, data);
  const snapshot = source.snapshot({ seq: 0, resize_gen: 0 });
  const restored = await restore(snapshot);
  const snapshotDump = [referenceBuffer(restored), referenceBuffer(term)];
  await writeParsed(term, after);
  await writeParsed(restored, after);
  const afterDump = [referenceBuffer(restored), referenceBuffer(term)];
  term.dispose();
  restored.dispose();
  return { snapshot, snapshotDump, afterDump };
}

describe("cursor_roundtrip_with_fixup", () => {
  it.each([
    ["full row, cursor on its last cell (#6165)", "\x1b[?1049hABCDEFGHIJ\x1b[1;10H"],
    ["full row on the normal screen", "ABCDEFGHIJ\x1b[1;10H"],
    ["pending wrap after a full row", "ABCDEFGHIJ"],
    ["pending wrap on the last row", "\x1b[4;1HABCDEFGHIJ"],
    ["pending wrap after a wide glyph", "ABCDEFGH中"],
    ["pending wrap with insert mode on", "ABCDEFGHIJ\x1b[4h"],
    ["below the last content", "abc\r\n\r\n"],
    ["on an empty row", "abc\x1b[4;7H"],
    ["after scrolling", "1\r\n2\r\n3\r\n4\r\n5\r\n6\x1b[2;4H"],
    ["origin mode inside a scroll region", "\x1b[2;3r\x1b[?6h\x1b[2;3H"],
    ["hidden", "abc\x1b[?25l"],
  ])("%s", async (_label, data) => {
    const { snapshotDump, afterDump } = await roundtrip(data, "XY\r\nZ");
    expect(snapshotDump[0]).toEqual(snapshotDump[1]);
    expect(afterDump[0]).toEqual(afterDump[1]);
  });
});

// Each case goes red against the unpatched addon
// (patches/@xterm__addon-serialize@0.14.0.patch).
describe("patched_addon_bugs", () => {
  it.each([
    ["dim then bold: SGR 22 must not clear the new bold", "\x1b[2mA\x1b[22;1mB"],
    ["bold+dim then dim: SGR 22 must not clear the kept dim", "\x1b[1;2mA\x1b[22;2mB"],
    [
      "an all-blank row before a wrapped row: no zero-parameter CUF/CUB",
      "ABCDEFGHIJK\x1b[1;1H\x1b[2K\x1b[3;1H",
    ],
    [
      "the first row separator that scrolls clears the scrolled-in row's colour",
      "1\r\n2\r\n3\r\n\x1b[41mXXXXXXXXXX\x1b[0m\r\n5",
    ],
  ])("%s", async (_label, data) => {
    const { snapshotDump, afterDump } = await roundtrip(data, "XY\r\nZ");
    expect(snapshotDump[0]).toEqual(snapshotDump[1]);
    expect(afterDump[0]).toEqual(afterDump[1]);
  });
});

describe("known issues of the raw addon (no fix-up)", () => {
  async function rawRestore(data: string, cols = 10, rows = 2) {
    const term = new Terminal({ ...OPTIONS, cols, rows });
    const addon = new SerializeAddon();
    term.loadAddon(addon as never);
    await writeParsed(term, data);
    const vt = addon.serialize();
    const restored = new Terminal({ ...OPTIONS, cols, rows });
    await writeParsed(restored, vt);
    return { term, restored, vt };
  }

  // https://github.com/xtermjs/xterm.js/issues/6165 — goes red once upstream
  // restores the cursor exactly; then drop the absolute cursor move from
  // `stateFixup` and flip this test.
  it("known_issue_6165_raw_addon: a full row with the cursor on its last cell restores one column left", async () => {
    const { term, restored } = await rawRestore("\x1b[?1049hABCDEFGHIJ\x1b[1;10H");
    expect(term.buffer.active.cursorX).toBe(9);
    expect(restored.buffer.active.cursorX).toBe(8);
  });

  // https://github.com/xtermjs/xterm.js/issues/6189 — accepted: after a
  // restore an OSC 8 link is plain text. Goes red once upstream writes links
  // back; then consider carrying them.
  it("known_issue_6189_raw_addon: OSC 8 link text is restored, its target is not", async () => {
    const link = "see \x1b]8;;https://example.org/pr\x1b\\PR\x1b]8;;\x1b\\ end";
    const { term, restored, vt } = await rawRestore(link, 20, 2);
    const urlId = (t: Terminal, x: number) =>
      (t.buffer.active.getLine(0)?.getCell(x) as unknown as { extended: { urlId: number } })
        .extended.urlId;
    expect(urlId(term, 4)).not.toBe(0);
    expect(vt).not.toContain("\x1b]8;");
    expect(restored.buffer.active.getLine(0)?.translateToString(true)).toBe("see PR end");
    expect(urlId(restored, 4)).toBe(0);
  });

  it("title_not_restored: the OSC 0/2 window title is not carried", async () => {
    const term = new Terminal({ ...OPTIONS, cols: 10, rows: 2 });
    const source = new SnapshotSource(term);
    const data = "\x1b]0;My title\x07\x1b]2;Other\x07ok";
    source.written(data);
    await writeParsed(term, data);
    const { vt } = source.snapshot({ seq: 0, resize_gen: 0 });
    const restored = new Terminal({ ...OPTIONS, cols: 10, rows: 2 });
    const titles: string[] = [];
    restored.onTitleChange((title) => titles.push(title));
    await writeParsed(restored, vt);
    expect(titles).toEqual([]);
  });

  // Upstream: after a narrowing resize without reflow (the alternate screen)
  // xterm keeps the cut-off cells and the addon writes them, so they wrap onto
  // the next row. `SnapshotSource` hands the addon rows clamped to the width.
  it("known_issue_narrowed_rows_raw_addon: cut-off cells wrap onto the next row", async () => {
    const term = new Terminal({ ...OPTIONS, cols: 20, rows: 3 });
    const addon = new SerializeAddon();
    term.loadAddon(addon as never);
    await writeParsed(term, "\x1b[?1049h" + "x".repeat(20) + "\r\nsecond");
    term.resize(10, 3);
    const restored = new Terminal({ ...OPTIONS, cols: 10, rows: 3 });
    await writeParsed(restored, addon.serialize());
    expect(referenceBuffer(restored).alt?.lines[1]).toBe("xxxxxxxxxx");
    expect(referenceBuffer(restored)).not.toEqual(referenceBuffer(term));

    const clamped = new SnapshotSource(term);
    const fixed = await restore(clamped.snapshot({ seq: 0, resize_gen: 0 }));
    expect(referenceBuffer(fixed)).toEqual(referenceBuffer(term));
  });
});

/** Probe replies (DECRQM / DECRQSS) a terminal gives for `probe`. */
async function probe(term: Terminal, query: string): Promise<string> {
  let reply = "";
  const sub = term.onData((data) => (reply += data));
  await writeParsed(term, query);
  sub.dispose();
  return reply;
}

describe("modes_survive_serialize", () => {
  type Read = (term: Terminal) => Promise<string>;
  const ask =
    (query: string): Read =>
    (term) =>
      probe(term, query);
  const decrqm = (mode: number) => ask(`\x1b[?${mode}$p`);
  // DECRQSS answers the configured cursor style, not the one DECSCUSR set.
  const cursorStyle: Read = async (term) => {
    const modes = (
      term as unknown as {
        _core: {
          coreService: { decPrivateModes: { cursorStyle?: string; cursorBlink?: boolean } };
        };
      }
    )._core.coreService.decPrivateModes;
    return `${modes.cursorStyle}/${modes.cursorBlink}`;
  };
  const table: Array<[string, string, Read]> = [
    ["DECCKM application cursor keys", "\x1b[?1h", decrqm(1)],
    ["application keypad (DECKPAM)", "\x1b=", decrqm(66)],
    ["bracketed paste 2004", "\x1b[?2004h", decrqm(2004)],
    ["mouse X10 9", "\x1b[?9h", decrqm(9)],
    ["mouse 1000", "\x1b[?1000h", decrqm(1000)],
    ["mouse 1002", "\x1b[?1002h", decrqm(1002)],
    ["mouse 1003", "\x1b[?1003h", decrqm(1003)],
    ["SGR mouse encoding 1006", "\x1b[?1000h\x1b[?1006h", decrqm(1006)],
    ["focus reporting 1004", "\x1b[?1004h", decrqm(1004)],
    ["cursor hidden 25", "\x1b[?25l", decrqm(25)],
    ["cursor style (DECSCUSR)", "\x1b[5 q", cursorStyle],
    ["scroll region (DECSTBM)", "\x1b[2;3r", ask("\x1bP$qr\x1b\\")],
    ["origin mode 6", "\x1b[2;3r\x1b[?6h", decrqm(6)],
    ["autowrap off 7", "\x1b[?7l", decrqm(7)],
    ["insert mode 4", "\x1b[4h", ask("\x1b[4$p")],
    ["reverse wraparound 45", "\x1b[?45h", decrqm(45)],
    ["synchronized output 2026", "\x1b[?2026h", decrqm(2026)],
  ];

  it.each(table)("%s", async (_label, set, read) => {
    const fresh = new Terminal({ ...OPTIONS, cols: 10, rows: 4 });
    const term = new Terminal({ ...OPTIONS, cols: 10, rows: 4 });
    const source = new SnapshotSource(term);
    source.written(set);
    await writeParsed(term, set);
    const restored = await restore(source.snapshot({ seq: 0, resize_gen: 0 }));
    const want = await read(term);
    // The probe must see the mode, or the comparison proves nothing.
    expect(want).not.toBe(await read(fresh));
    expect(await read(restored)).toBe(want);
  });

  it("saved cursor (DECSC) position and pen", async () => {
    const set = "\x1b[3;5H\x1b[31;1m\x1b7\x1b[H\x1b[0mtop";
    const { afterDump } = await roundtrip(set, "\x1b8X");
    expect(afterDump[0]).toEqual(afterDump[1]);
    expect(afterDump[1].cursorY).toBe(2);
  });

  it("the normal screen's pen and cursor saved on entering the alternate screen", async () => {
    const set = "\x1b[32mgreen\x1b[?1049h\x1b[0malt";
    const { snapshotDump, afterDump } = await roundtrip(set, "\x1b[?1049lX");
    expect(snapshotDump[0]).toEqual(snapshotDump[1]);
    expect(afterDump[0]).toEqual(afterDump[1]);
  });
});

describe("SequenceTail", () => {
  /** xterm's own parser state: 0 is ground. */
  const parserState = (term: Terminal) =>
    (term as unknown as { _core: { _inputHandler: { _parser: { currentState: number } } } })._core
      ._inputHandler._parser.currentState;

  it.each(FIXTURES)(
    "%s: has a tail exactly when xterm's parser is inside a sequence",
    async (name) => {
      const stream = loadStream(name);
      const random = seededRandom(SEED);
      const term = new Terminal({ ...OPTIONS, ...START[name] });
      const tail = new SequenceTail();
      const cursor = new StreamCursor(stream, {
        write: async (text) => {
          tail.push(text);
          await writeParsed(term, text);
        },
        resize: (cols, rows) => term.resize(cols, rows),
      });
      let offset = 0;
      let inside = 0;
      while (offset < stream.length) {
        offset += 1 + Math.floor(random() * 64);
        await cursor.advanceTo(offset);
        const state = parserState(term);
        if (state !== 0) inside++;
        const lastIsHighSurrogate = /[\ud800-\udbff]$/.test(tail.pending);
        if (!lastIsHighSurrogate) expect(tail.pending !== "", `offset ${offset}`).toBe(state !== 0);
      }
      expect(inside).toBeGreaterThan(0);
      term.dispose();
    },
    60_000,
  );

  it("drops C0 controls a CSI executes on the way", () => {
    const tail = new SequenceTail();
    tail.push("ab\x1b[3");
    tail.push("\r;4");
    expect(tail.pending).toBe("\x1b[3;4");
    tail.push("H");
    expect(tail.pending).toBe("");
  });

  it("keeps an unfinished OSC across writes, ended by BEL or ESC", () => {
    const tail = new SequenceTail();
    tail.push("\x1b]0;ti");
    tail.push("tle");
    expect(tail.pending).toBe("\x1b]0;title");
    tail.push("\x07");
    expect(tail.pending).toBe("");
    tail.push("\x1b]8;;http://x\x1b");
    expect(tail.pending).toBe("\x1b");
  });

  it("keeps a lone high surrogate", () => {
    const tail = new SequenceTail();
    tail.push("x\ud83d");
    expect(tail.pending).toBe("\ud83d");
    tail.push("\ude00");
    expect(tail.pending).toBe("");
  });
});

describe("mutation controls", () => {
  async function sourceAt(name: Fixture, label: string) {
    const stream = loadStream(name);
    const cut = seededCuts(stream, CUTS, SEED).find((c) => c.label === label);
    if (!cut) throw new Error(`no ${label} cut in ${name}`);
    const term = new Terminal({ ...OPTIONS, ...START[name] });
    const source = new SnapshotSource(term);
    const walk = new StreamCursor(stream, sinkFor(term, source));
    await walk.advanceTo(cut.offset);
    return { stream, cut, term, source, walk };
  }

  async function continued(stream: Stream, cut: Cut, restored: Terminal) {
    const rest = new StreamCursor(stream, sinkFor(restored));
    await rest.skipTo(cut.offset);
    await rest.toEnd();
    return referenceBuffer(restored);
  }

  it("excludeAltBuffer: true fails the round trip on an alternate-screen cut", async () => {
    const { term } = await sourceAt("codex-tui", "alt-screen");
    expect(term.buffer.active.type).toBe("alternate");
    const addon = new SerializeAddon();
    term.loadAddon(addon as never);
    const vt =
      addon.serialize({ scrollback: OPTIONS.scrollback, excludeAltBuffer: true }) +
      stateFixup(term);
    const restored = new Terminal({ ...OPTIONS, cols: term.cols, rows: term.rows });
    await writeParsed(restored, vt);
    expect(referenceBuffer(restored)).not.toEqual(referenceBuffer(term));
  });

  // Codex redraws its whole frame after every resize, which hides both
  // mutations by the end of its recording; Claude Code's inline UI keeps its
  // history in scrollback, where they stay visible.
  async function liveEnd(name: Fixture, stream: Stream) {
    const live = new Terminal({ ...OPTIONS, ...START[name] });
    await new StreamCursor(stream, sinkFor(live)).toEnd();
    return referenceBuffer(live);
  }

  it("restoring at the slot's size, then resizing, fails continuation after a resize", async () => {
    const { stream, cut, source, walk } = await sourceAt("claude-ink", "after-resize");
    const snapshot = source.snapshot({ seq: walk.seq, resize_gen: walk.resizeGen });
    const live = await liveEnd("claude-ink", stream);
    const wrong = await restore(snapshot, { cols: snapshot.cols + 7, rows: snapshot.rows + 3 });
    wrong.resize(snapshot.cols, snapshot.rows);
    expect(await continued(stream, cut, await restore(snapshot))).toEqual(live);
    expect(await continued(stream, cut, wrong)).not.toEqual(live);
  });

  it("dropping the unfinished sequence fails continuation on a mid-CSI cut", async () => {
    const { stream, cut, source, walk } = await sourceAt("claude-ink", "mid-csi");
    const snapshot = source.snapshot({ seq: walk.seq, resize_gen: walk.resizeGen });
    const live = await liveEnd("claude-ink", stream);
    const tail = new SequenceTail();
    const prefix = new StreamCursor(stream, { write: (text) => tail.push(text), resize: () => {} });
    await prefix.advanceTo(cut.offset);
    expect(tail.pending).not.toBe("");
    expect(snapshot.vt.endsWith(tail.pending)).toBe(true);
    const withoutTail = { ...snapshot, vt: snapshot.vt.slice(0, -tail.pending.length) };
    expect(await continued(stream, cut, await restore(snapshot))).toEqual(live);
    expect(await continued(stream, cut, await restore(withoutTail))).not.toEqual(live);
  });
});

describe("snapshot size", () => {
  it("a 5 000-line mostly plain scrollback serializes to at most 1 MiB", async () => {
    const term = new Terminal({ allowProposedApi: true, cols: 100, rows: 30, scrollback: 5000 });
    const source = new SnapshotSource(term);
    // `append-only.ptylog`'s numbered lines, widened to near-full rows of
    // prose with a coloured line now and then: the heaviest ordinary history.
    const words = "the quick brown fox jumps over a lazy dog while octopuses change colour ";
    let text = "";
    for (let n = 1; n <= 5030; n++) {
      const line = `${n}. ${words.repeat(2)}`.slice(0, 96);
      text += n % 10 === 0 ? `\x1b[32m${line}\x1b[0m\r\n` : `${line}\r\n`;
    }
    source.written(text);
    await writeParsed(term, text);
    expect(term.buffer.normal.length - term.rows).toBe(5000);
    const { vt } = source.snapshot({ seq: 0, resize_gen: 0 });
    const bytes = new TextEncoder().encode(vt).length;
    if (process.env.SNAPSHOT_SIZE_OUT) writeFileSync(process.env.SNAPSHOT_SIZE_OUT, String(bytes));
    expect(bytes).toBeLessThanOrEqual(1024 * 1024);
  });
});

describe("worker snapshot", () => {
  it("describes exactly the parsed stream: requested between two feeds, waits for the first", async () => {
    const worker = new TestWorker();
    worker.create(1);
    const first = "first batch ".repeat(90_000); // ~1 MiB: parsed over several slices
    worker.send({ type: "feed", session: 1, seq_end: first.length, data: first });
    const reply = worker.snapshot(1);
    worker.send({ type: "feed", session: 1, seq_end: first.length + 6, data: "SECOND" });
    const { snapshot } = await reply;
    expect(snapshot.seq).toBe(first.length);
    expect(snapshot.vt).toContain("first batch");
    expect(snapshot.vt).not.toContain("SECOND");
    expect(worker.errors).toEqual([]);
  });

  it("reports the recorded size and positions", async () => {
    const worker = new TestWorker();
    worker.create(4, { seq_base: 100, resize_gen_base: 2 });
    worker.send({ type: "resize", session: 4, cols: 60, rows: 12, resize_gen: 3 });
    worker.send({ type: "feed", session: 4, seq_end: 105, data: "hello" });
    const { snapshot } = await worker.snapshot(4);
    expect(snapshot).toMatchObject({ cols: 60, rows: 12, seq: 105, resize_gen: 3 });
    expect(snapshot.scrollback).toBe(REFERENCE_SCROLLBACK);
  });

  it("mutation: serializing before the write callback misses the bytes being parsed", async () => {
    const term = new Terminal({ ...OPTIONS, cols: 80, rows: 24 });
    const source = new SnapshotSource(term);
    const data = "unparsed ".repeat(120_000);
    source.written(data);
    const parsed = writeParsed(term, data);
    const early = source.snapshot({ seq: 0, resize_gen: 0 });
    await parsed;
    const late = source.snapshot({ seq: 0, resize_gen: 0 });
    expect(early.vt).not.toBe(late.vt);
  });
});

describe("respawn_carries_normal_buffer", () => {
  it("codex-tui mid alternate screen: the new terminal holds the normal buffer and the seam only", async () => {
    const stream = loadStream("codex-tui");
    const cut = seededCuts(stream, CUTS, SEED).find((c) => c.label === "alt-screen");
    if (!cut) throw new Error("no alternate-screen cut");
    const worker = new TestWorker();
    worker.create(1);
    let resizeGen = 0;
    let seq = 0;
    await new StreamCursor(stream, {
      write: (text) => {
        seq += new TextEncoder().encode(text).length;
        worker.send({ type: "feed", session: 1, seq_end: seq, data: text });
      },
      resize: (cols, rows) => {
        resizeGen += 1;
        worker.send({ type: "resize", session: 1, cols, rows, resize_gen: resizeGen });
      },
    }).advanceTo(cut.offset);
    const before = await worker.dump(1);
    expect(before.activeBuffer).toBe("alternate");

    // What the normal buffer alone serializes to, from an independent terminal.
    const reference = new Terminal({ ...OPTIONS, ...START["codex-tui"] });
    await new StreamCursor(stream, sinkFor(reference)).advanceTo(cut.offset);
    const carry = new SnapshotSource(reference).normalBufferCarry();
    const expected = new Terminal({ ...OPTIONS, cols: reference.cols, rows: reference.rows });
    await writeParsed(expected, `${carry}\r\n\x1b[90m${RESPAWN_LINE}\x1b[0m\r\n`);

    worker.send({ type: "respawn", session: 1, seq_base: 9_000_000, resize_gen_base: 42 });
    const after = await worker.dump(1);
    expect(bufferOf(after)).toEqual(referenceBuffer(expected));
    expect(after.activeBuffer).toBe("normal");
    expect(after).toMatchObject({ seq_end: 9_000_000, resize_gen: 42 });
    // Nothing of the full-screen frame survives above the seam.
    const seamRow = after.lines.indexOf(RESPAWN_LINE);
    expect(seamRow).toBeGreaterThan(0);
    const altRows = new Set(before.alt!.lines.filter((line) => line.trim() !== ""));
    expect(after.lines.slice(0, seamRow).filter((line) => altRows.has(line))).toEqual([]);

    worker.send({ type: "feed", session: 1, seq_end: 9_000_003, data: "new" });
    const next = await worker.snapshot(1);
    expect(next.snapshot).toMatchObject({ seq: 9_000_003, resize_gen: 42 });
    expect(worker.errors).toEqual([]);
  });

  it("keeps the old output, one marker line, then the new process's output", async () => {
    const worker = new TestWorker();
    worker.create(1);
    worker.send({ type: "feed", session: 1, seq_end: 12, data: "\x1b[?1hold output\r\n" });
    worker.send({ type: "respawn", session: 1, seq_base: 200, resize_gen_base: 3 });
    worker.send({ type: "feed", session: 1, seq_end: 210, data: "new output" });
    const reply = await worker.dump(1);
    expect(reply.lines.filter((line) => line !== "")).toEqual([
      "old output",
      RESPAWN_LINE,
      "new output",
    ]);
    expect(reply).toMatchObject({ seq_end: 210, resize_gen: 3 });
    const { snapshot } = await worker.snapshot(1);
    // The ended process's modes are not carried.
    expect(snapshot.vt).not.toContain("\x1b[?1h");
  });
});
