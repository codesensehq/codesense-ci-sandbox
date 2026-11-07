// SPDX-License-Identifier: MIT
/**
 * Display equals emulator: the recorded Codex, Claude Code and scripted-TUI
 * streams driven through the backend emulator worker's own code and, in
 * parallel, through the manager's real restore path — a display rebuilt from
 * the worker's snapshot cut mid-stream at the recorded size, resized by the
 * slot, then fed the rest of the stream live. At the end the display's buffer
 * must equal the worker's exactly, through the one `referenceBuffer` dump both
 * use. Only the Tauri boundary is faked: it forwards to the worker.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ReferenceBuffer } from "./buffer-dump";
import { loadPtylog, type PtylogFixture } from "./__fixtures__/ptylog";
import { FakeEventBus } from "./__test-utils__/fake-terminal-server";
import { paneSteps, type PaneStep } from "./__test-utils__/pane-feed";
import { createTerminalManager, type DisplayDump } from "./terminal-manager";
import type { TerminalSubscription } from "@/tauri/commands";
import {
  REFERENCE_SCROLLBACK,
  TestWorker,
  bufferOf as workerBuffer,
} from "../../../src-emulator/test-worker";

const SESSION = "s1";
const KEY = 1;

const h = vi.hoisted(() => ({
  bus: null as unknown as import("./__test-utils__/fake-terminal-server").FakeEventBus,
  backend: null as unknown as {
    subscribe(stream: number): Promise<TerminalSubscription>;
    resize(cols: number, rows: number): void;
  },
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, handler: (event: { payload: unknown }) => void) =>
    h.bus.listen(name, handler),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));
vi.mock(
  "@/features/viewer/lib/cli-workspace/transport/terminal-session-source",
  async (importOriginal) => {
    let next = 0;
    return {
      ...(await importOriginal<
        typeof import("@/features/viewer/lib/cli-workspace/transport/terminal-session-source")
      >()),
      reserveTerminalStream: vi.fn(async () => ++next),
      subscribeTerminal: vi.fn((_session: string, stream: number) => h.backend.subscribe(stream)),
      unsubscribeTerminal: vi.fn(async () => {}),
      writePty: vi.fn(async () => {}),
      resizePty: vi.fn(async (_session: string, cols: number, rows: number) =>
        h.backend.resize(cols, rows),
      ),
    };
  },
);

const START: Record<PtylogFixture, { cols: number; rows: number }> = {
  "codex-tui": { cols: 100, rows: 30 },
  "claude-ink": { cols: 100, rows: 30 },
  "append-only": { cols: 100, rows: 30 },
  "tui-fixture": { cols: 80, rows: 24 },
};
/** The size the slot measures once the display is shown. */
const SLOT = { cols: 90, rows: 26 };

/**
 * One session on the host: the worker's emulator, and the stream events the
 * host emits for it. Output and resizes go to both in the same order.
 */
class Host {
  readonly worker = new TestWorker();
  private seq = 0;
  private resizeGen = 0;
  private cols: number;
  private rows: number;
  private channel: string | null = null;

  constructor(start: { cols: number; rows: number }) {
    this.cols = start.cols;
    this.rows = start.rows;
    this.worker.create(KEY, { ...start, scrollback: REFERENCE_SCROLLBACK });
  }

  output(data: string, seqEnd: number): void {
    this.seq = seqEnd;
    this.worker.send({ type: "feed", session: KEY, seq_end: seqEnd, data });
    this.emit({ data, seq: seqEnd });
  }

  resize(cols: number, rows: number): void {
    if (cols === this.cols && rows === this.rows) return;
    this.cols = cols;
    this.rows = rows;
    this.resizeGen += 1;
    this.worker.send({ type: "resize", session: KEY, cols, rows, resize_gen: this.resizeGen });
    this.emit({ resized: { at_seq: this.seq, resize_gen: this.resizeGen, cols, rows } });
  }

  /** Bind `stream` and answer with the worker's snapshot at this point. */
  async subscribe(
    stream: number,
    fault?: (snapshot: TerminalSubscription["snapshot"]) => TerminalSubscription["snapshot"],
  ) {
    this.channel = `pty-output-stream-${stream}`;
    const { snapshot } = await this.worker.snapshot(KEY);
    return {
      snapshot: fault ? fault(snapshot) : snapshot,
      stream_id: stream,
      story_id: null,
      workspace_id: null,
      epic_id: null,
    };
  }

  private emit(payload: unknown): void {
    if (this.channel) h.bus.emit(this.channel, payload);
  }
}

function stepsOf(name: PtylogFixture): Array<PaneStep & { seqEnd: number }> {
  const records = loadPtylog(name);
  return paneSteps(records).map((step, i) => {
    const record = records[i];
    return { ...step, seqEnd: record.kind === "output" ? record.seqEnd : 0 };
  });
}

/** Mid-stream, between the recording's second and third resize: the host is
 *  at a recorded size other than the one the session started at. */
function cutOf(steps: ReadonlyArray<PaneStep>): number {
  const resizes = steps.flatMap((s, i) => (s.kind === "resize" ? [i] : []));
  return Math.floor((resizes[1] + resizes[2]) / 2);
}

function displayBuffer(dump: DisplayDump): ReferenceBuffer {
  const {
    instanceId: _i,
    created: _c,
    rebuilds: _r,
    restoredFromSnapshot: _s,
    appliedSeq: _a,
    appliedResizeGen: _g,
    ...buffer
  } = dump;
  return buffer;
}

/** Both sides dumped at the same stream position. */
interface Compare {
  display: DisplayDump;
  emulator: Awaited<ReturnType<TestWorker["dump"]>>;
}

/**
 * Run `name` through the host up to its cut, open a display there and resize
 * it to the slot, then stream the rest. Both sides are dumped once restored and
 * resized, and again at the end. `fault` replaces the snapshot the display is
 * restored from.
 */
async function run(
  name: PtylogFixture,
  fault?: (snapshot: TerminalSubscription["snapshot"]) => TerminalSubscription["snapshot"],
): Promise<{ restored: Compare; live: Compare }> {
  const steps = stepsOf(name);
  const cut = cutOf(steps);
  const host = new Host(START[name]);
  const play = (step: PaneStep & { seqEnd: number }) =>
    step.kind === "output"
      ? host.output(step.data, step.seqEnd)
      : host.resize(step.cols, step.rows);
  steps.slice(0, cut).forEach(play);
  h.backend = {
    subscribe: (stream) => host.subscribe(stream, fault),
    resize: (cols, rows) => host.resize(cols, rows),
  };
  const manager = createTerminalManager({
    createWebgl: () => {
      throw new Error("no WebGL in tests");
    },
  });
  try {
    expect((await manager.ensure(SESSION, { fontSizePx: 13 }))?.outcome).toEqual({
      kind: "attached",
    });
    const compare = async (): Promise<Compare> => ({
      emulator: await host.worker.dump(KEY),
      display: await manager.debugDump(SESSION),
    });
    manager.requestResize(SESSION, SLOT.cols, SLOT.rows);
    const restored = await compare();
    steps.slice(cut).forEach(play);
    return { restored, live: await compare() };
  } finally {
    manager.dispose(SESSION);
  }
}

beforeEach(() => {
  h.bus = new FakeEventBus();
});

afterEach(() => {
  document.body.innerHTML = "";
});

describe("display equals emulator", () => {
  it.each(["codex-tui", "claude-ink", "tui-fixture"] as const)(
    "%s: restored mid-stream, resized, streamed live: the display's buffer is the worker's",
    async (name) => {
      const { restored, live } = await run(name);
      for (const [label, { display, emulator }] of Object.entries({ restored, live })) {
        expect(display.appliedSeq, label).toBe(emulator.seq_end);
        expect(display.appliedResizeGen, label).toBe(emulator.resize_gen);
        expect(displayBuffer(display), label).toEqual(workerBuffer(emulator));
      }
      expect(restored.display.cols).toBe(SLOT.cols);
    },
    60_000,
  );

  it("teeth: a display rebuilt by raw replay instead of the snapshot differs from the worker", async () => {
    const name = "codex-tui";
    const steps = stepsOf(name);
    const raw = steps
      .slice(0, cutOf(steps))
      .map((s) => (s.kind === "output" ? s.data : ""))
      .join("");
    // The pre-snapshot rebuild: the session's raw output written into a display
    // at the size the session started at.
    const { restored } = await run(name, (snapshot) => ({
      ...snapshot,
      ...START[name],
      vt: raw,
    }));
    expect(restored.display.appliedSeq).toBe(restored.emulator.seq_end);
    expect(displayBuffer(restored.display)).not.toEqual(workerBuffer(restored.emulator));
  }, 60_000);
});
