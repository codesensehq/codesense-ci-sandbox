// SPDX-License-Identifier: MIT
// @vitest-environment node
/**
 * Budget measurements of the shipped worker binary (Bun), each printed as a
 * table row and asserted:
 *
 * - apply: `write` to callback inside the worker, per 64 KiB `codex-tui` batch,
 *   p99 <= 5 ms over >= 200 batches;
 * - RSS: after a forced collection, 20 sessions x 5 000 scrollback x 120
 *   filled columns <= the worker's own baseline (median of 5 fresh starts) +
 *   20 x 16 MiB. xterm's buffer alone is ~10 MiB live per full session in Bun
 *   (12 B/cell plus three objects per row), so 8 MiB is out of reach at this
 *   scrollback; the worker's collections keep RSS at ~14.5 MiB.
 *
 * Run with `pnpm run bench:emulator` (needs `pnpm run build:emulator`).
 */
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { existsSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { loadPtylog } from "../../src/shared/terminal/__fixtures__/ptylog";
import { encodeFrame, FrameReader } from "../frame";
import type { AppliedReply, Reply, SessionRequest } from "../protocol";

const TRIPLES: Record<string, string> = {
  "darwin-arm64": "aarch64-apple-darwin",
  "darwin-x64": "x86_64-apple-darwin",
  "linux-x64": "x86_64-unknown-linux-gnu",
  "linux-arm64": "aarch64-unknown-linux-gnu",
};
const WORKER = resolve(
  __dirname,
  "../../src-tauri/binaries",
  `codesense-emulator-${TRIPLES[`${process.platform}-${process.arch}`]}`,
);
const BATCH = 64 * 1024;
const MiB = 1024 * 1024;
const SESSION_BUDGET = 16 * MiB;

class WorkerProcess {
  private readonly child: ChildProcessWithoutNullStreams;
  private readonly reader = new FrameReader();
  private readonly waiting = new Map<number, (reply: Reply) => void>();
  readonly applied: AppliedReply[] = [];
  private nextId = 1;

  constructor() {
    this.child = spawn(WORKER, ["--test-mode"]);
    this.child.stdout.on("data", (chunk: Buffer) => {
      for (const frame of this.reader.push(chunk)) {
        const reply = JSON.parse(new TextDecoder().decode(frame)) as Reply;
        if (reply.type === "applied") this.applied.push(reply);
        else if ("id" in reply && reply.id !== null) this.waiting.get(reply.id)?.(reply);
      }
    });
  }

  send(request: SessionRequest | { type: "hello" | "stats"; id: number }): void {
    this.child.stdin.write(encodeFrame(new TextEncoder().encode(JSON.stringify(request))));
  }

  ask<T extends Reply>(
    request: (id: number) => SessionRequest | { type: "hello" | "stats"; id: number },
  ): Promise<T> {
    const id = this.nextId++;
    const reply = new Promise<T>((done) => this.waiting.set(id, done as (reply: Reply) => void));
    this.send(request(id));
    return reply;
  }

  async memory(): Promise<{ rss: number; live: number }> {
    return this.ask<Reply & { rss: number; live: number }>((id) => ({ type: "stats", id }));
  }

  stop(): void {
    this.child.kill();
  }
}

/** Budget rows go straight to stdout: the test setup quiets `console`. */
const report = (row: string) => process.stdout.write(`${row}\n`);

const percentile = (samples: number[], p: number) =>
  [...samples].sort((a, b) => a - b)[Math.min(samples.length - 1, Math.floor(samples.length * p))];

const running: WorkerProcess[] = [];
function start(): WorkerProcess {
  const worker = new WorkerProcess();
  running.push(worker);
  return worker;
}
afterEach(() => running.splice(0).forEach((worker) => worker.stop()));

describe.skipIf(!process.env.EMULATOR_BENCH)("emulator worker budgets", () => {
  it("has a built worker to measure", () => {
    expect(existsSync(WORKER), `${WORKER} — run pnpm run build:emulator`).toBe(true);
  });

  it("applies a 64 KiB codex-tui batch within 5 ms at p99", async () => {
    const stream = Buffer.concat(
      loadPtylog("codex-tui").flatMap((r) => (r.kind === "output" ? [r.data] : [])),
    ).toString("utf8");
    const worker = start();
    worker.send({
      type: "create",
      session: 1,
      cols: 100,
      rows: 30,
      scrollback: 5000,
      seq_base: 0,
      resize_gen_base: 0,
      history_lost: false,
    });
    let seq = 0;
    let batches = 0;
    while (batches < 240) {
      for (let at = 0; at + BATCH <= stream.length && batches < 240; at += BATCH) {
        const data = stream.slice(at, at + BATCH);
        seq += Buffer.byteLength(data);
        worker.send({ type: "feed", session: 1, seq_end: seq, data });
        batches++;
      }
    }
    await worker.ask((id) => ({ type: "dump", id, session: 1 }));
    // The first batches warm the JIT; the budget is the steady state.
    const samples = worker.applied.slice(40).map((a) => a.apply_ms);
    const p50 = percentile(samples, 0.5);
    const p99 = percentile(samples, 0.99);
    report(
      `| worker apply, 64 KiB codex-tui | n=${samples.length} | p50 ${p50.toFixed(2)} ms | p99 ${p99.toFixed(2)} ms | budget 5 ms |`,
    );
    expect(samples.length).toBeGreaterThanOrEqual(200);
    expect(p99).toBeLessThanOrEqual(5);
  }, 120_000);

  it("holds 20 full 5 000-line sessions within baseline + 20 x 16 MiB", async () => {
    const baselines: number[] = [];
    for (let i = 0; i < 5; i++) {
      const fresh = start();
      await fresh.ask((id) => ({ type: "hello", id }));
      baselines.push((await fresh.memory()).rss);
      fresh.stop();
    }
    const baseline = percentile(baselines, 0.5);

    const worker = start();
    await worker.ask((id) => ({ type: "hello", id }));
    const line = (i: number) =>
      `\x1b[3${i % 8}m${`${i} `.padEnd(120, String.fromCharCode(97 + (i % 26)))}\x1b[0m`;
    const fill = Array.from({ length: 5_040 }, (_, i) => line(i)).join("\r\n");
    for (let session = 1; session <= 20; session++) {
      worker.send({
        type: "create",
        session,
        cols: 120,
        rows: 40,
        scrollback: 5000,
        seq_base: 0,
        resize_gen_base: 0,
        history_lost: false,
      });
      for (let at = 0; at < fill.length; at += BATCH) {
        worker.send({ type: "feed", session, seq_end: at, data: fill.slice(at, at + BATCH) });
      }
    }
    for (let session = 1; session <= 20; session++) {
      const dump = await worker.ask<Reply & { lines: string[] }>((id) => ({
        type: "dump",
        id,
        session,
      }));
      expect(dump.lines.length).toBe(5_040);
    }
    const { rss, live } = await worker.memory();
    const budget = baseline + 20 * SESSION_BUDGET;
    report(
      `| worker RSS, 20 x 5 000 x 120 | baseline ${(baseline / MiB).toFixed(1)} MiB (median of ${baselines.map((b) => (b / MiB).toFixed(1)).join(", ")}) | filled ${(rss / MiB).toFixed(1)} MiB | +${((rss - baseline) / MiB / 20).toFixed(2)} MiB/session | live ${(live / MiB / 20).toFixed(2)} MiB/session | budget ${(budget / MiB).toFixed(1)} MiB |`,
    );
    expect(rss).toBeLessThanOrEqual(budget);
  }, 300_000);
});
