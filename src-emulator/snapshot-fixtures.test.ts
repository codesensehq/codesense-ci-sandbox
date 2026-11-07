// SPDX-License-Identifier: MIT
// @vitest-environment node
/**
 * Snapshot fixtures for the display's restore tests: for each recording, 20
 * seeded cuts at batch boundaries, each the worker's `TerminalSnapshot` there
 * and the index of the first recording line still to come. Regenerate with
 * `pnpm run write-snapshot-fixtures`; this test fails while they are stale.
 */
import { mkdirSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { Terminal } from "@xterm/headless";
import { loadPtylog, type PtylogFixture } from "../src/shared/terminal/__fixtures__/ptylog";
import { SnapshotSource } from "./snapshot";
import { referenceBuffer } from "../src/shared/terminal/buffer-dump";
import { replayPtylog } from "../src/shared/terminal/__fixtures__/ptylog";
import { loadStream, StreamCursor, writeParsed, type SnapshotFixture } from "./snapshot-stream";
import { seededRandom } from "./test-worker";

const DIR = resolve(__dirname, "../tests/fixtures/terminal/snapshots");
const FIXTURES: Array<[PtylogFixture, { cols: number; rows: number }]> = [
  ["codex-tui", { cols: 100, rows: 30 }],
  ["claude-ink", { cols: 100, rows: 30 }],
  ["tui-fixture", { cols: 80, rows: 24 }],
];
const PER_FIXTURE = 20;
const SEED = 38;
const SCROLLBACK = 5000;
const MAX_TOTAL_BYTES = 5 * 1024 * 1024;

/** Output lines whose bytes start a new character: a cut there leaves no half
 *  character in the host's decoder. */
function cleanBoundaries(name: PtylogFixture): number[] {
  const records = loadPtylog(name);
  const out: number[] = [];
  records.forEach((record, i) => {
    const next = records.slice(i + 1).find((r) => r.kind === "output");
    const lead = next?.kind === "output" ? next.data[0] : undefined;
    if (record.kind === "output" && lead !== undefined && (lead & 0xc0) !== 0x80) out.push(i + 1);
  });
  return out;
}

async function generate(name: PtylogFixture, start: { cols: number; rows: number }) {
  const boundaries = cleanBoundaries(name);
  const random = seededRandom(SEED);
  const picked = new Set<number>();
  while (picked.size < Math.min(PER_FIXTURE, boundaries.length)) {
    picked.add(boundaries[Math.floor(random() * boundaries.length)]);
  }
  const stream = loadStream(name);
  // Text offset at the end of each record.
  const offsets: number[] = [];
  let offset = 0;
  for (const event of stream.events) {
    if (event.kind === "output") offset += event.text.length;
    offsets.push(offset);
  }
  const term = new Terminal({ allowProposedApi: true, scrollback: SCROLLBACK, ...start });
  const source = new SnapshotSource(term);
  const walk = new StreamCursor(stream, {
    write: async (text) => {
      source.written(text);
      await writeParsed(term, text);
    },
    resize: (cols, rows) => term.resize(cols, rows),
  });
  const out: SnapshotFixture[] = [];
  for (const boundary of [...picked].sort((a, b) => a - b)) {
    await walk.advanceTo(offsets[boundary - 1]);
    const snapshot = source.snapshot({ seq: walk.seq, resize_gen: walk.resizeGen });
    out.push({ fixture: name, rest_from: walk.applied, snapshot });
  }
  term.dispose();
  return out;
}

const fileName = (f: SnapshotFixture) => `${f.fixture}-${f.rest_from}.json`;

describe("snapshot fixtures", () => {
  it("are current (regenerate: pnpm run write-snapshot-fixtures)", async () => {
    const all: SnapshotFixture[] = [];
    for (const [name, start] of FIXTURES) all.push(...(await generate(name, start)));
    const files = new Map(all.map((f) => [fileName(f), `${JSON.stringify(f, null, 1)}\n`]));
    if (process.env.UPDATE_SNAPSHOT_FIXTURES === "1") {
      rmSync(DIR, { recursive: true, force: true });
      mkdirSync(DIR, { recursive: true });
      for (const [file, text] of files) writeFileSync(resolve(DIR, file), text);
    }
    const onDisk = readdirSync(DIR).sort();
    expect(onDisk).toEqual([...files.keys()].sort());
    for (const [file, text] of files) {
      expect(readFileSync(resolve(DIR, file), "utf8"), `${file} is stale`).toBe(text);
    }
    const total = onDisk.reduce((sum, file) => sum + statSync(resolve(DIR, file)).size, 0);
    expect(total).toBeLessThanOrEqual(MAX_TOTAL_BYTES);
  }, 120_000);

  it.each(FIXTURES)(
    "%s: the earliest cut, restored and fed the rest of the recording, equals live",
    async (name, start) => {
      const first = readdirSync(DIR)
        .filter((file) => file.startsWith(`${name}-`))
        .map((file) => JSON.parse(readFileSync(resolve(DIR, file), "utf8")) as SnapshotFixture)
        .sort((a, b) => a.rest_from - b.rest_from)[0];
      const records = loadPtylog(name);
      const feed = (term: Terminal) => ({
        write: (data: Uint8Array) => new Promise<void>((done) => term.write(data, () => done())),
        resize: (cols: number, rows: number) => term.resize(cols, rows),
      });
      const live = new Terminal({ allowProposedApi: true, scrollback: SCROLLBACK, ...start });
      await replayPtylog(records, feed(live));
      const { snapshot } = first;
      const restored = new Terminal({
        allowProposedApi: true,
        scrollback: snapshot.scrollback,
        cols: snapshot.cols,
        rows: snapshot.rows,
      });
      await writeParsed(restored, snapshot.vt);
      await replayPtylog(records.slice(first.rest_from), feed(restored));
      expect(referenceBuffer(restored)).toEqual(referenceBuffer(live));
    },
    60_000,
  );
});
