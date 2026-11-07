// SPDX-License-Identifier: MIT
/**
 * The one rebuild path: every display is built from the host emulator's
 * snapshot at the size it was recorded, then the stream from there. Real
 * `@xterm/xterm` displays, the checked-in snapshot fixtures, and a fake
 * terminal server running the worker's own snapshot code; only the Tauri
 * boundary is faked.
 */
import { readdirSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ReferenceBuffer } from "./buffer-dump";
import { referenceBuffer } from "./buffer-dump";
import {
  loadPtylog,
  replayPtylog,
  type PtylogFixture,
  type PtylogRecord,
} from "./__fixtures__/ptylog";
import {
  FakeEventBus,
  FakeTerminalServer,
  RESTART_SEAM,
} from "./__test-utils__/fake-terminal-server";
import { parityTerminal, writeParsed } from "./__test-utils__/headless-from";
import {
  RECOVERY_RETRY_DELAYS_MS,
  RESTORE_FAILED,
  createTerminalManager,
  type DisplayDump,
  type TerminalManager,
} from "./terminal-manager";
import { ATTACH_TIMEOUT_MS } from "@/features/viewer/lib/terminal-attach";
import type { TerminalSnapshot, TerminalSubscription } from "@/tauri/commands";

const h = vi.hoisted(() => ({
  bus: null as unknown as import("./__test-utils__/fake-terminal-server").FakeEventBus,
  server: null as unknown as import("./__test-utils__/fake-terminal-server").FakeTerminalServer,
  /** Replaces the fake server's subscribe for one test. */
  subscribe: null as null | ((session: string, stream?: number) => Promise<TerminalSubscription>),
  constructed: [] as Array<{ cols?: number; rows?: number; scrollback?: number }>,
}));

vi.mock("@xterm/xterm", async (importOriginal) => {
  const xterm = await importOriginal<typeof import("@xterm/xterm")>();
  class CountedTerminal extends xterm.Terminal {
    constructor(options?: ConstructorParameters<typeof xterm.Terminal>[0]) {
      super(options);
      h.constructed.push({
        cols: options?.cols,
        rows: options?.rows,
        scrollback: options?.scrollback,
      });
    }
  }
  return { ...xterm, Terminal: CountedTerminal };
});
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, handler: (event: { payload: unknown }) => void) =>
    h.bus.listen(name, handler),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));
const transport = vi.hoisted(() => ({
  reserveTerminalStream: vi.fn(() => h.server.reserve()),
  subscribeTerminal: vi.fn((session: string, stream?: number) =>
    h.subscribe ? h.subscribe(session, stream) : h.server.subscribe(session, stream),
  ),
  unsubscribeTerminal: vi.fn((stream: number) => h.server.unsubscribe(stream)),
  writePty: vi.fn(async () => {}),
  resizePty: vi.fn((session: string, cols: number, rows: number) =>
    h.server.resize(session, cols, rows),
  ),
}));
vi.mock(
  "@/features/viewer/lib/cli-workspace/transport/terminal-session-source",
  async (importOriginal) => ({
    ...(await importOriginal<
      typeof import("@/features/viewer/lib/cli-workspace/transport/terminal-session-source")
    >()),
    ...transport,
  }),
);

const OPTS = { fontSizePx: 13 };
const noWebgl = () => {
  throw new Error("no WebGL in tests");
};

let managers: TerminalManager[] = [];
function newManager(deps: Parameters<typeof createTerminalManager>[0] = {}) {
  const m = createTerminalManager({ createWebgl: noWebgl, ...deps });
  managers.push(m);
  return m;
}

function bufferOf(dump: DisplayDump): ReferenceBuffer {
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

/** Wait (on real timers) until `check` holds. */
const eventually = (check: () => void) => vi.waitFor(check, { timeout: 5000, interval: 5 });

beforeEach(() => {
  vi.clearAllMocks();
  h.bus = new FakeEventBus();
  h.server = new FakeTerminalServer(h.bus);
  h.subscribe = null;
  h.constructed = [];
});

afterEach(() => {
  for (const m of managers) for (const id of ["s1", "s2", "s3", "s4", "s5"]) m.dispose(id);
  managers = [];
  document.body.innerHTML = "";
  vi.useRealTimers();
});

// ---------------------------------------------------------------------------
// Snapshot fixtures: a cut into a recording, the worker's snapshot there, and
// the records still to come.
// ---------------------------------------------------------------------------

interface SnapshotFixture {
  fixture: PtylogFixture;
  rest_from: number;
  snapshot: TerminalSnapshot;
}

const SNAPSHOT_DIR = resolve(__dirname, "../../../tests/fixtures/terminal/snapshots");
const START: Partial<Record<PtylogFixture, { cols: number; rows: number }>> = {
  "tui-fixture": { cols: 80, rows: 24 },
};

function snapshotFixtures(name: PtylogFixture): SnapshotFixture[] {
  return readdirSync(SNAPSHOT_DIR)
    .filter((file) => file.startsWith(`${name}-`))
    .map((file) => JSON.parse(readFileSync(resolve(SNAPSHOT_DIR, file), "utf8")) as SnapshotFixture)
    .sort((a, b) => a.rest_from - b.rest_from);
}

/** A recording's stream events past a cut, as the host emits them. */
function streamEvents(records: readonly PtylogRecord[], resizeGen: number): unknown[] {
  const decoder = new TextDecoder();
  let gen = resizeGen;
  let seq = 0;
  return records.map((record) => {
    if (record.kind === "output") {
      seq = record.seqEnd;
      return { data: decoder.decode(record.data, { stream: true }), seq };
    }
    gen += 1;
    return { resized: { at_seq: seq, resize_gen: gen, cols: record.cols, rows: record.rows } };
  });
}

/** The never-rebuilt reference: the whole recording fed live. */
const references = new Map<PtylogFixture, ReferenceBuffer>();
async function reference(name: PtylogFixture): Promise<ReferenceBuffer> {
  const known = references.get(name);
  if (known) return known;
  const term = parityTerminal(START[name] ?? {});
  await replayPtylog(loadPtylog(name), {
    write: (data) => writeParsed(term, data),
    resize: (cols, rows) => term.resize(cols, rows),
  });
  const dump = referenceBuffer(term);
  term.dispose();
  references.set(name, dump);
  return dump;
}

/** The reference at a cut: the recording up to it, fed live. */
async function referenceAt(cut: SnapshotFixture): Promise<ReferenceBuffer> {
  const term = parityTerminal(START[cut.fixture] ?? {});
  await replayPtylog(loadPtylog(cut.fixture).slice(0, cut.rest_from), {
    write: (data) => writeParsed(term, data),
    resize: (cols, rows) => term.resize(cols, rows),
  });
  const dump = referenceBuffer(term);
  term.dispose();
  return dump;
}

/** Restore a display from `cut` with nothing after it: the display at the
 *  cut. `raw` is the fault under test: the raw stream up to the cut written
 *  into a display at the recording's start size, which is then resized to
 *  the cut's size — how displays were rebuilt before snapshots. */
async function restoreAt(cut: SnapshotFixture, raw?: string): Promise<ReferenceBuffer> {
  const start = START[cut.fixture] ?? { cols: 100, rows: 30 };
  h.subscribe = async (_session, stream) => ({
    snapshot: raw === undefined ? cut.snapshot : { ...cut.snapshot, ...start, vt: raw },
    stream_id: stream!,
    story_id: null,
    workspace_id: null,
    epic_id: null,
  });
  h.server.open("s1", start);
  const m = newManager();
  await m.ensure("s1", OPTS);
  if (raw !== undefined) m.requestResize("s1", cut.snapshot.cols, cut.snapshot.rows);
  const dump = await m.debugDump("s1");
  m.dispose("s1");
  return bufferOf(dump);
}

/**
 * Restore a display from `cut` through `ensure`: the events up to (and
 * including) the first resize after the cut, plus a stale output and a stale
 * resize the snapshot already holds, arrive between listen and the subscribe
 * reply; the rest arrive live. Returns the display's buffer at the end.
 */
async function restoreFrom(cut: SnapshotFixture, vt?: string): Promise<ReferenceBuffer> {
  const records = loadPtylog(cut.fixture);
  const rest = streamEvents(records.slice(cut.rest_from), cut.snapshot.resize_gen);
  const firstResize = rest.findIndex((e) => (e as { resized?: unknown }).resized);
  const heldCount = Math.max(firstResize + 1, Math.min(3, rest.length));
  const snapshot = { ...cut.snapshot, vt: vt ?? cut.snapshot.vt };
  let channel = "";
  h.subscribe = async (_session, stream) => {
    channel = `pty-output-stream-${stream}`;
    h.bus.emit(channel, { data: "STALE", seq: snapshot.seq });
    if (snapshot.resize_gen > 0) {
      h.bus.emit(channel, {
        resized: { at_seq: snapshot.seq, resize_gen: snapshot.resize_gen, cols: 7, rows: 7 },
      });
    }
    for (const event of rest.slice(0, heldCount)) h.bus.emit(channel, event);
    return {
      snapshot,
      stream_id: stream!,
      story_id: null,
      workspace_id: null,
      epic_id: null,
    };
  };
  h.server.open("s1");
  const m = newManager();
  const handle = await m.ensure("s1", OPTS);
  expect(handle?.outcome).toEqual({ kind: "attached" });
  for (const event of rest.slice(heldCount)) h.bus.emit(channel, event);
  const dump = await m.debugDump("s1");
  m.dispose("s1");
  return bufferOf(dump);
}

// ---------------------------------------------------------------------------

describe("one construction site (I6)", () => {
  it("one_construction_site", () => {
    const root = resolve(__dirname, "../..");
    const excluded = /(\.test\.|__test-utils__|__fixtures__|__guard_fixtures__)/;
    const sites: string[] = [];
    const walk = (dir: string) => {
      for (const entry of readdirSync(dir, { withFileTypes: true })) {
        const path = resolve(dir, entry.name);
        if (entry.isDirectory()) walk(path);
        else if (/\.(ts|tsx)$/.test(entry.name) && !excluded.test(path)) {
          const code = readFileSync(path, "utf8")
            .replace(/\/\*[\s\S]*?\*\//g, "")
            .replace(/\/\/.*$/gm, "");
          const count = code.split("new Terminal(").length - 1;
          for (let i = 0; i < count; i++) sites.push(path.slice(root.length + 1));
        }
      }
    };
    walk(root);
    expect(sites).toEqual(["shared/terminal/terminal-manager.ts"]);
  });

  it("every construction takes its size and history from a snapshot", async () => {
    h.server.open("s1", { cols: 91, rows: 27, scrollback: 321 });
    const m = newManager();
    await m.ensure("s1", OPTS);
    expect(h.constructed).toEqual([{ cols: 91, rows: 27, scrollback: 321 }]);
  });
});

describe("restore equals never rebuilt", () => {
  it.each(["codex-tui", "claude-ink", "tui-fixture"] as const)(
    "restore_equals_never_rebuilt: %s, at each of its 20 cut points",
    async (name) => {
      const cuts = snapshotFixtures(name);
      expect(cuts).toHaveLength(20);
      const expected = await reference(name);
      for (const cut of cuts) {
        const label = `${name} cut at record ${cut.rest_from}`;
        expect(await restoreAt(cut), `${label}, at the cut`).toEqual(await referenceAt(cut));
        expect(await restoreFrom(cut), `${label}, at the end`).toEqual(expected);
      }
    },
    120_000,
  );

  describe("teeth", () => {
    /** The raw stream up to a cut: what a display wrote before snapshots. */
    function rawUpTo(name: PtylogFixture, restFrom: number): string {
      const decoder = new TextDecoder();
      return loadPtylog(name)
        .slice(0, restFrom)
        .map((r) => (r.kind === "output" ? decoder.decode(r.data, { stream: true }) : ""))
        .join("");
    }

    it.each(["codex-tui", "claude-ink"] as const)(
      "writing the raw stream instead of snapshot.vt fails on %s after an in-place redraw",
      async (name) => {
        const cuts = snapshotFixtures(name).filter((c) => c.snapshot.resize_gen > 0);
        let failed = 0;
        for (const cut of cuts) {
          const raw = await restoreAt(cut, rawUpTo(name, cut.rest_from));
          if (JSON.stringify(raw) !== JSON.stringify(await referenceAt(cut))) failed++;
        }
        expect(failed).toBeGreaterThan(0);
      },
      120_000,
    );

    it("writing the raw stream leaves the append-only control's screen intact", async () => {
      const records = loadPtylog("append-only");
      const restFrom = records.findIndex((r) => r.kind === "resize");
      const before = records.slice(0, restFrom);
      const last = [...before].reverse().find((r) => r.kind === "output");
      const cut: SnapshotFixture = {
        fixture: "append-only",
        rest_from: restFrom,
        snapshot: {
          cols: 100,
          rows: 30,
          seq: last?.kind === "output" ? last.seqEnd : 0,
          resize_gen: 0,
          scrollback: 1000,
          vt: rawUpTo("append-only", restFrom),
        },
      };
      const screen = (b: ReferenceBuffer) => (b.alt ? b.alt.lines : b.lines).slice(-b.rows);
      expect(screen(await restoreFrom(cut))).toEqual(screen(await reference("append-only")));
    });
  });
});

describe("restore at the recorded size, then the slot's", () => {
  it("interim_ensure_from_snapshot: built at snapshot cols×rows, vt written, then resized to the slot", async () => {
    h.server.open("s1");
    h.server.produce("s1", "recorded at 100 columns\r\n");
    const m = newManager();
    const opening = m.ensure("s1", OPTS);
    // The slot already measured a different size while the restore runs.
    m.requestResize("s1", 80, 24);
    await opening;
    expect(h.constructed).toEqual([{ cols: 100, rows: 30, scrollback: 1000 }]);
    expect(transport.resizePty).toHaveBeenCalledWith("s1", 80, 24);
    const dump = await m.debugDump("s1");
    const expected = parityTerminal();
    await writeParsed(expected, "recorded at 100 columns\r\n");
    expected.resize(80, 24);
    expect(bufferOf(dump)).toEqual(referenceBuffer(expected));
    expect(dump.appliedResizeGen).toBe(1);
    expected.dispose();
  });
});

describe("rebuild triggers", () => {
  it("rebuild_triggers: a reconnect rebuilds each display once; a gap byte lands once", async () => {
    const m = newManager();
    const ids = ["s1", "s2", "s3", "s4", "s5"];
    for (const id of ids) {
      h.server.open(id);
      await m.ensure(id, OPTS);
      h.server.produce(id, `before ${id}\r\n`);
    }
    await Promise.all(ids.map((id) => m.debugDump(id)));
    const created = h.constructed.length;
    h.server.loseStreams(h.server.boundStreams());
    // Output while the streams are gone: the snapshot carries it.
    for (const id of ids) h.server.produce(id, `gap ${id}\r\n`);
    await eventually(() => expect(ids.every((id) => m.terminal(id))).toBe(true));
    for (const id of ids) {
      const dump = await m.debugDump(id);
      expect(dump.rebuilds).toBe(1);
      expect(dump.lines.filter((l) => l === `gap ${id}`)).toHaveLength(1);
      expect(dump.lines.filter((l) => l === `before ${id}`)).toHaveLength(1);
    }
    expect(h.constructed.length - created).toBe(5);
    expect(RECOVERY_RETRY_DELAYS_MS.length).toBeGreaterThan(0);
  });

  it("a respawn in place rebuilds once: carried history, the seam, the new prompt", async () => {
    const m = newManager();
    h.server.open("s1");
    await m.ensure("s1", OPTS);
    h.server.produce("s1", "old run\r\n\x1b[?1049hFULLSCREEN");
    await m.debugDump("s1");
    h.server.respawn("s1", "new prompt $ ");
    await m.ensure("s1", { ...OPTS, life: 1 });
    const dump = await m.debugDump("s1");
    expect(dump.rebuilds).toBe(1);
    expect(dump.created).toBe(2);
    const text = dump.lines.join("\n");
    expect(text).toContain("old run");
    expect(text).toContain(RESTART_SEAM);
    expect(text).toContain("new prompt $");
    expect(text).not.toContain("FULLSCREEN");
    expect(dump.alt).toBeFalsy();
  });
});

describe("incompatible host", () => {
  it("incompatible_host_builds_nothing", async () => {
    h.server.open("s1");
    h.server.failSubscribes =
      "restart terminal server (ends 2 sessions): it speaks protocol 6, this app 7";
    const m = newManager();
    const handle = await m.ensure("s1", OPTS);
    expect(handle?.outcome.kind).toBe("incompatible");
    expect(handle?.instanceId).toBeNull();
    expect(m.terminal("s1")).toBeNull();
    expect(h.constructed).toEqual([]);
  });
});

describe("restore fails", () => {
  it("restore_failure_shows_retry: no reply in time, then a host error; Retry restores", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    h.server.open("s1");
    h.subscribe = () => new Promise(() => {});
    const m = newManager();
    const first = m.ensure("s1", OPTS);
    await vi.advanceTimersByTimeAsync(ATTACH_TIMEOUT_MS);
    expect((await first)?.outcome).toEqual({ kind: "failed", reason: RESTORE_FAILED });
    expect(m.terminal("s1")).toBeNull();

    h.subscribe = () => Promise.reject(new Error("emulator worker crashed"));
    expect((await m.ensure("s1", OPTS))?.outcome).toEqual({
      kind: "failed",
      reason: RESTORE_FAILED,
    });
    vi.useRealTimers();

    // Retry: the host answers now.
    h.subscribe = null;
    h.server.produce("s1", "still running\r\n");
    expect((await m.ensure("s1", OPTS))?.outcome).toEqual({ kind: "attached" });
    expect((await m.debugDump("s1")).lines[0]).toBe("still running");
    expect(transport.writePty).not.toHaveBeenCalled();
  });
});

describe("ended while closed", () => {
  it("exited_session_restores_with_ended_bar: its final output, no resize, no respawn", async () => {
    h.server.open("s1");
    h.server.produce("s1", "last words\r\n[exit 3]\r\n");
    const m = newManager();
    await m.ensure("s1", OPTS);
    h.bus.emit("pty-exit", { session_id: "s1" });
    m.requestResize("s1", 90, 20);
    const dump = await m.debugDump("s1");
    expect(dump.lines.slice(0, 2)).toEqual(["last words", "[exit 3]"]);
    expect(transport.resizePty).not.toHaveBeenCalled();
  });
});

describe("resizes apply in stream order (I5)", () => {
  it("resize_applies_on_event_only: one IPC, no local resize until the Resized applies", async () => {
    h.server.open("s1");
    const m = newManager();
    await m.ensure("s1", OPTS);
    const term = m.terminal("s1")!;
    const resize = vi.spyOn(term, "resize");
    // Hold the Resized so the display is seen before it arrives.
    transport.resizePty.mockImplementationOnce(async () => {});
    m.requestResize("s1", 80, 24);
    expect(transport.resizePty).toHaveBeenCalledTimes(1);
    expect(resize).not.toHaveBeenCalled();
    expect(m.size("s1")).toEqual({ cols: 80, rows: 24 });
    await h.server.resize("s1", 80, 24);
    const dump = await m.debugDump("s1");
    expect(resize).toHaveBeenCalledWith(80, 24);
    expect(dump.appliedResizeGen).toBe(1);
    expect({ cols: dump.cols, rows: dump.rows }).toEqual({ cols: 80, rows: 24 });
  });

  it("a Resized behind output still in the coalescer applies after it", async () => {
    h.server.open("s1");
    const m = newManager();
    await m.ensure("s1", OPTS);
    const wide = "x".repeat(95);
    h.server.produce("s1", `${wide}\r\n`);
    await h.server.resize("s1", 40, 10);
    h.server.produce("s1", "after\r\n");
    const dump = await m.debugDump("s1");
    const expected = parityTerminal();
    await writeParsed(expected, `${wide}\r\n`);
    expected.resize(40, 10);
    await writeParsed(expected, "after\r\n");
    expect(bufferOf(dump)).toEqual(referenceBuffer(expected));
    expected.dispose();
  });

  it("a dump between a write and its callback reports the previous appliedResizeGen", async () => {
    const held: Array<() => void> = [];
    let hold = false;
    const m = newManager({
      applyOutput: (term, data, parsed) => {
        if (hold) held.push(() => term.write(data, parsed));
        else term.write(data, parsed);
      },
    });
    h.server.open("s1");
    await m.ensure("s1", OPTS);
    hold = true;
    h.server.produce("s1", "pending\r\n");
    await eventually(() => expect(held).toHaveLength(1));
    // Mid-write: a dump asked now, then a Resized; both wait behind the write.
    const mid = m.debugDump("s1");
    await h.server.resize("s1", 50, 20);
    expect(m.terminal("s1")!.cols).toBe(100);
    hold = false;
    held.splice(0).forEach((f) => f());
    expect((await mid).appliedResizeGen).toBe(0);
    expect((await m.debugDump("s1")).appliedResizeGen).toBe(1);
  });
});

describe("selection and find across a Resized", () => {
  it("a selection is cleared, never left on other cells; the find state stays", async () => {
    h.server.open("s1");
    const m = newManager();
    await m.ensure("s1", OPTS);
    h.server.produce("s1", "alpha beta gamma\r\n");
    await m.debugDump("s1");
    const term = m.terminal("s1")!;
    term.select(6, 0, 4);
    expect(term.getSelection()).toBe("beta");
    const find = {
      query: "beta",
      caseSensitive: false,
      regex: false,
      wholeWord: false,
      resultIndex: 0,
      resultCount: 1,
    };
    m.setFindState("s1", find);
    m.requestResize("s1", 8, 10);
    await m.debugDump("s1");
    expect(term.cols).toBe(8);
    expect(term.hasSelection()).toBe(false);
    expect(m.findState("s1")).toEqual(find);
  });
});
