// SPDX-License-Identifier: MIT
/**
 * Memory-pressure eviction: past the display or byte limit the least recently
 * seen hidden displays are released, their sessions keep running in the host,
 * and showing one again rebuilds it from the host snapshot. Real xterm
 * displays and a fake terminal server running the worker's snapshot code.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, renderHook } from "@testing-library/react";
import type { WebglAddon } from "@xterm/addon-webgl";

import { terminalFrameScheduler } from "@/shared/lib/frame-budget-scheduler";
import type { ReferenceBuffer } from "./buffer-dump";
import { loadPtylog } from "./__fixtures__/ptylog";
import { headlessFrom } from "./__test-utils__/headless-from";
import { FakeEventBus, FakeTerminalServer } from "./__test-utils__/fake-terminal-server";
import { paneSteps } from "./__test-utils__/pane-feed";
import {
  createTerminalManager,
  type DisplayDump,
  type TerminalManager,
  type TerminalManagerDeps,
} from "./terminal-manager";
import { BUFFER_BYTES_PER_CELL, EVICT_MAX_DISPLAYS } from "./terminal-constants";
import { useEvictedRestore } from "@/features/viewer/hooks/use-evicted-restore";
import { PANE_SPINNER_DELAY_MS } from "@/features/viewer/hooks/use-pane-startup";
import {
  TerminalFindBar,
  type TerminalSearch,
} from "@/features/viewer/components/terminal-find-bar";

const fake = vi.hoisted(() => ({
  bus: null as unknown as import("./__test-utils__/fake-terminal-server").FakeEventBus,
  server: null as unknown as import("./__test-utils__/fake-terminal-server").FakeTerminalServer,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, handler: (event: { payload: unknown }) => void) =>
    fake.bus.listen(name, handler),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));
const transport = vi.hoisted(() => ({
  reserveTerminalStream: vi.fn(() => fake.server.reserve()),
  subscribeTerminal: vi.fn((session: string, stream?: number) =>
    fake.server.subscribe(session, stream),
  ),
  unsubscribeTerminal: vi.fn((stream: number) => fake.server.unsubscribe(stream)),
  writePty: vi.fn(async () => {}),
  resizePty: vi.fn((session: string, cols: number, rows: number) =>
    fake.server.resize(session, cols, rows),
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

const FAKE_TIMERS: Parameters<typeof vi.useFakeTimers>[0] = {
  toFake: [
    "setTimeout",
    "clearTimeout",
    "setInterval",
    "clearInterval",
    "requestAnimationFrame",
    "cancelAnimationFrame",
  ],
};
const OPTS = { fontSizePx: 13 };
const noWebgl = () => {
  throw new Error("no WebGL in tests");
};

let managers: Array<{ m: TerminalManager; sessions: Set<string> }> = [];
function newManager(deps: TerminalManagerDeps = {}) {
  const m = createTerminalManager({ createWebgl: noWebgl, ...deps });
  const tracked = { m, sessions: new Set<string>() };
  managers.push(tracked);
  const ensure = m.ensure.bind(m);
  m.ensure = (sessionId, opts) => {
    tracked.sessions.add(sessionId as string);
    return ensure(sessionId, opts);
  };
  return m;
}

function slot(): HTMLElement {
  const el = document.createElement("div");
  document.body.appendChild(el);
  return el;
}

async function until<T>(pending: Promise<T>): Promise<T> {
  let done = false;
  const settled = pending.finally(() => (done = true));
  for (let i = 0; i < 500 && !done; i++) await vi.advanceTimersByTimeAsync(10);
  return settled;
}

async function dumped(m: TerminalManager, sessionId: string): Promise<DisplayDump> {
  let result: DisplayDump | null = null;
  const pending = m.debugDump(sessionId).then((d) => (result = d));
  for (let i = 0; i < 200 && !result; i++) await vi.advanceTimersByTimeAsync(20);
  await pending;
  return result!;
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

async function open(m: TerminalManager, sessionId: string) {
  fake.server.open(sessionId);
  const handle = await until(m.ensure(sessionId, OPTS));
  expect(handle?.outcome).toEqual({ kind: "attached" });
}

/** The budget runs after the current task. */
const settle = () => vi.advanceTimersByTimeAsync(0);

function bytesOf(m: TerminalManager, sessionId: string): number {
  const term = m.terminal(sessionId)!;
  return term.cols * term.buffer.normal.length * BUFFER_BYTES_PER_CELL;
}

function prng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function stubWebgl() {
  const addons: Array<{ live: boolean }> = [];
  const create = () => {
    const a = {
      live: true,
      activate: vi.fn(),
      dispose: vi.fn(() => {
        a.live = false;
      }),
      onContextLoss: () => ({ dispose: () => {} }),
    };
    addons.push(a);
    return a as unknown as WebglAddon;
  };
  return { addons, create };
}

beforeEach(() => {
  vi.clearAllMocks();
  fake.bus = new FakeEventBus();
  fake.server = new FakeTerminalServer(fake.bus);
  vi.useFakeTimers(FAKE_TIMERS);
  terminalFrameScheduler.flush();
});

afterEach(() => {
  for (const { m, sessions } of managers) for (const s of sessions) m.dispose(s);
  managers = [];
  document.body.innerHTML = "";
  vi.useRealTimers();
});

describe("which displays go", () => {
  it("evicts_exact_lru_hidden_set", async () => {
    const seed = 0x5eed13;
    const random = prng(seed);
    const gl = stubWebgl();
    const m = newManager({ createWebgl: gl.create });
    const ids = Array.from({ length: 70 }, (_, i) => `s${i}`);
    // Seeded attach order; every display is shown as it is built.
    const order = [...ids].sort(() => random() - 0.5);
    const slots = new Map<string, HTMLElement>();
    for (const id of order) {
      await open(m, id);
      const el = slot();
      slots.set(id, el);
      m.attach(id, el, { focus: false });
    }
    await vi.advanceTimersByTimeAsync(50);
    expect(m.memory().displays).toBe(70);

    // The oldest-attached display is focused, so it counts as recently seen.
    const focusedThenHidden = order[0];
    m.focus(focusedThenHidden);
    const visible = new Set(order.slice(30, 36));
    const hidden = order.filter((id) => !visible.has(id));
    const heldBefore = m.webglLease(order[35])!.held;
    const instanceIds = new Map(hidden.map((id) => [id, m.terminal(id)!]));
    const leasedBefore = new Set(hidden.filter((id) => m.webglLease(id)?.leased));
    (document.activeElement as HTMLElement | null)?.blur?.();
    for (const id of hidden) m.detach(id, slots.get(id)!);
    await settle();

    const lru = hidden.filter((id) => id !== focusedThenHidden).slice(0, 70 - EVICT_MAX_DISPLAYS);
    const evicted = new Set(hidden.filter((id) => m.terminal(id) === null));
    expect(evicted, `seed ${seed}`).toEqual(new Set(lru));
    expect(evicted.has(focusedThenHidden)).toBe(false);
    for (const id of visible) expect(m.terminal(id)).not.toBeNull();
    expect(m.memory().displays).toBe(EVICT_MAX_DISPLAYS);
    for (const id of evicted) {
      expect(instanceIds.get(id)!.element?.isConnected ?? false).toBe(false);
      expect(fake.server.subscribers(id)).toBe(0);
    }
    const leasedEvicted = [...evicted].filter((id) => leasedBefore.has(id)).length;
    expect(leasedEvicted).toBeGreaterThan(0);
    expect(m.webglLease(order[35])!.held).toBe(heldBefore - leasedEvicted);
  }, 60_000);

  it("byte_threshold", async () => {
    // Five 100×30 displays with no history: 36,000 B each.
    const each = 100 * 30 * BUFFER_BYTES_PER_CELL;
    const m = newManager({ maxBytes: each * 3 });
    const slots: HTMLElement[] = [];
    for (const id of ["s1", "s2", "s3", "s4", "s5"]) {
      await open(m, id);
      const el = slot();
      slots.push(el);
      m.attach(id, el, { focus: false });
    }
    expect(m.memory().estimatedBytes).toBe(each * 5);
    for (const [i, id] of ["s1", "s2", "s3", "s4"].entries()) m.detach(id, slots[i]);
    await settle();
    expect(["s1", "s2"].map((id) => m.terminal(id))).toEqual([null, null]);
    expect(["s3", "s4", "s5"].every((id) => m.terminal(id) !== null)).toBe(true);
    expect(m.memory().estimatedBytes).toBe(each * 3);
  });

  it("estimated_bytes_drop_exactly", async () => {
    const m = newManager();
    const ids = ["s1", "s2", "s3", "s4"];
    for (const [i, id] of ids.entries()) {
      await open(m, id);
      for (let n = 0; n < 40 * (i + 1); n++) fake.server.produce(id, `line ${n}\r\n`);
      await dumped(m, id);
    }
    const before = m.memory();
    const gone = ["s2", "s4"];
    const sum = gone.reduce((total, id) => total + bytesOf(m, id), 0);
    expect(sum).toBeGreaterThan(0);
    for (const id of gone) expect(m.evict(id)).toBe(true);
    expect(m.memory()).toEqual({
      displays: before.displays - gone.length,
      estimatedBytes: before.estimatedBytes - sum,
    });
  });

  it("evict_during_ensure", async () => {
    const m = newManager({ maxDisplays: 0 });
    fake.server.open("s1");
    fake.server.holdSubscribes = true;
    const pending = m.ensure("s1", OPTS);
    await vi.advanceTimersByTimeAsync(20);
    expect(m.evict("s1")).toBe(false);
    fake.server.releaseSubscribes();
    const handle = await until(pending);
    expect(handle?.outcome).toEqual({ kind: "attached" });
    const dump = await dumped(m, "s1");
    expect(dump).toMatchObject({ created: 1, rebuilds: 0 });
  });
});

describe("a reconnect mid-swap", () => {
  it("frozen_display_counts_and_is_never_evicted_mid_swap", async () => {
    const m = newManager();
    await open(m, "s1");
    await open(m, "s2");
    const a = slot();
    m.attach("s1", a, { focus: false });
    m.detach("s1", a);
    const one = bytesOf(m, "s1");
    expect(m.memory()).toEqual({ displays: 2, estimatedBytes: one + bytesOf(m, "s2") });

    fake.server.holdSubscribes = true;
    const [lost] = fake.server.boundStreams();
    fake.server.loseStreams([lost]);
    await vi.advanceTimersByTimeAsync(20);
    // Frozen and still painted: it counts, and it is not evictable.
    expect(m.terminal("s1")).toBeNull();
    expect(m.memory().displays).toBe(2);
    expect(m.evict("s1")).toBe(false);

    // The rebuilt display is parked beside the frozen one until the swap frame.
    fake.server.releaseSubscribes();
    for (let i = 0; i < 500 && !m.terminal("s1"); i++) await vi.advanceTimersByTimeAsync(1);
    expect(m.terminal("s1")).not.toBeNull();
    expect(m.memory().displays).toBe(3);
    expect(m.evict("s1")).toBe(false);

    await vi.advanceTimersByTimeAsync(50);
    expect(m.memory().displays).toBe(2);
    expect(m.evict("s1")).toBe(true);
  });

  it("a budget of one keeps the frozen display through its swap", async () => {
    const m = newManager({ maxDisplays: 1 });
    await open(m, "s1");
    fake.server.holdSubscribes = true;
    const [lost] = fake.server.boundStreams();
    fake.server.loseStreams([lost]);
    await settle();
    await vi.advanceTimersByTimeAsync(20);
    fake.server.releaseSubscribes();
    for (let i = 0; i < 500 && !m.terminal("s1"); i++) await vi.advanceTimersByTimeAsync(1);
    await settle();
    expect(m.terminal("s1")).not.toBeNull();
    await vi.advanceTimersByTimeAsync(50);
    expect((await dumped(m, "s1")).rebuilds).toBe(1);
  });
});

describe("showing an evicted display", () => {
  it("evicted_restore_matches_full_stream", async () => {
    vi.useRealTimers();
    const reference = await headlessFrom("codex-tui");
    vi.useFakeTimers(FAKE_TIMERS);
    const steps = paneSteps(loadPtylog("codex-tui"));
    const half = Math.floor(steps.length / 2);
    const feed = (from: number, to: number) => {
      for (const step of steps.slice(from, to)) {
        if (step.kind === "output") fake.server.produce("s1", step.data);
        else m.requestResize("s1", step.cols, step.rows);
      }
    };
    const m = newManager();
    await open(m, "s1");
    const a = slot();
    m.attach("s1", a, { focus: false });
    feed(0, half);
    await dumped(m, "s1");
    m.detach("s1", a);
    expect(m.evict("s1")).toBe(true);
    transport.subscribeTerminal.mockClear();
    feed(half, steps.length);
    await vi.advanceTimersByTimeAsync(50);

    m.attach("s1", a, { focus: true });
    for (let i = 0; i < 500 && !m.terminal("s1"); i++) await vi.advanceTimersByTimeAsync(10);
    const dump = await dumped(m, "s1");
    expect(transport.subscribeTerminal).toHaveBeenCalledTimes(1);
    expect(dump).toMatchObject({ created: 2, rebuilds: 1 });
    expect(a.contains(m.terminal("s1")!.element!)).toBe(true);
    const active = m.terminal("s1")!.buffer.active;
    expect(active.viewportY).toBe(active.baseY);
    expect(bufferOf(dump)).toEqual(reference);
  });

  it("exited_while_evicted", async () => {
    const m = newManager();
    await open(m, "s1");
    const exit = vi.fn();
    m.listen("s1", { exit });
    expect(m.evict("s1")).toBe(true);
    fake.server.produce("s1", "final words\r\n");
    fake.bus.emit("pty-exit", { session_id: "s1" });
    expect(exit).toHaveBeenCalledTimes(1);

    const a = slot();
    await until(m.ensure("s1", OPTS));
    m.attach("s1", a, { focus: false });
    const dump = await dumped(m, "s1");
    expect(dump.lines.join("\n")).toContain("final words");
    // An exited session takes no resize.
    transport.resizePty.mockClear();
    m.requestResize("s1", 90, 20);
    expect(transport.resizePty).not.toHaveBeenCalled();
  });
});

describe("restore look", () => {
  async function shownThenHidden(m: TerminalManager, el: HTMLElement) {
    await open(m, "s1");
    m.attach("s1", el, { focus: false, visible: false });
    for (let n = 0; n < 200; n++) fake.server.produce("s1", `row ${n}\r\n`);
    await dumped(m, "s1");
  }

  it("restore_look: a fast restore shows nothing in between", async () => {
    const m = newManager();
    const el = slot();
    await shownThenHidden(m, el);
    const { result, rerender } = renderHook(
      ({ visible }) => useEvictedRestore("s1", visible, () => {}, m),
      { initialProps: { visible: false } },
    );
    act(() => void expect(m.evict("s1")).toBe(true));
    const labels: boolean[] = [];
    act(() => {
      m.setVisible("s1", el, true);
      rerender({ visible: true });
    });
    for (let i = 0; i < 50 && result.current.generation === 0; i++) {
      labels.push(result.current.label);
      await act(() => vi.advanceTimersByTimeAsync(5));
    }
    expect(result.current.generation).toBe(1);
    expect(labels.every((l) => !l)).toBe(true);
    expect(result.current.label).toBe(false);
    const active = m.terminal("s1")!.buffer.active;
    expect(active.viewportY).toBe(active.baseY);
  });

  it("restore_look: a slow restore says Restoring session… until it lands", async () => {
    const m = newManager();
    const el = slot();
    await shownThenHidden(m, el);
    const restored = vi.fn();
    const { result, rerender } = renderHook(
      ({ visible }) => useEvictedRestore("s1", visible, restored, m),
      { initialProps: { visible: false } },
    );
    act(() => void expect(m.evict("s1")).toBe(true));
    fake.server.holdSubscribes = true;
    act(() => {
      m.setVisible("s1", el, true);
      rerender({ visible: true });
    });
    await act(() => vi.advanceTimersByTimeAsync(PANE_SPINNER_DELAY_MS - 10));
    expect(result.current.label).toBe(false);
    await act(() => vi.advanceTimersByTimeAsync(20));
    expect(result.current.label).toBe(true);
    await act(async () => {
      fake.server.releaseSubscribes();
      for (let i = 0; i < 100 && result.current.generation === 0; i++)
        await vi.advanceTimersByTimeAsync(10);
    });
    expect(result.current).toEqual({ label: false, generation: 1 });
    expect(restored).toHaveBeenCalledWith(
      expect.objectContaining({ outcome: { kind: "attached" } }),
    );
  });

  it("restore_look: a superseded rebuild clears Restoring session…", async () => {
    const m = newManager();
    const el = slot();
    await shownThenHidden(m, el);
    const restored = vi.fn();
    const { result, rerender } = renderHook(
      ({ visible }) => useEvictedRestore("s1", visible, restored, m),
      { initialProps: { visible: false } },
    );
    act(() => void expect(m.evict("s1")).toBe(true));
    fake.server.holdSubscribes = true;
    act(() => {
      m.setVisible("s1", el, true);
      rerender({ visible: true });
    });
    await act(() => vi.advanceTimersByTimeAsync(PANE_SPINNER_DELAY_MS + 10));
    expect(result.current.label).toBe(true);
    // A restart in place replaces the eviction's rebuild.
    await act(async () => {
      void m.ensure("s1", { ...OPTS, life: 1 });
      fake.server.releaseSubscribes();
      await vi.advanceTimersByTimeAsync(50);
      fake.server.releaseSubscribes();
      await vi.advanceTimersByTimeAsync(50);
    });
    expect(result.current.label).toBe(false);
    expect(restored).not.toHaveBeenCalled();
  });

  it("restore_look: a failed rebuild clears Restoring session…", async () => {
    const m = newManager();
    const el = slot();
    await shownThenHidden(m, el);
    const { result, rerender } = renderHook(
      ({ visible }) => useEvictedRestore("s1", visible, () => {}, m),
      { initialProps: { visible: false } },
    );
    act(() => void expect(m.evict("s1")).toBe(true));
    fake.server.failSubscribes = "session not found";
    act(() => {
      m.setVisible("s1", el, true);
      rerender({ visible: true });
    });
    await act(() => vi.advanceTimersByTimeAsync(PANE_SPINNER_DELAY_MS + 10));
    // The rebuild settles (here as a failure the pane reports itself).
    for (let i = 0; i < 100 && result.current.label; i++)
      await act(() => vi.advanceTimersByTimeAsync(100));
    expect(result.current.label).toBe(false);
  });

  it("restore_look: a retired session clears Restoring session…", async () => {
    let heard: import("./terminal-manager").SessionListener = {};
    const stub = {
      listen: (_id: unknown, listener: typeof heard) => {
        heard = listener;
        return () => {};
      },
    } as unknown as TerminalManager;
    const { result } = renderHook(() => useEvictedRestore("s1", true, () => {}, stub));
    act(() => heard.evicted?.());
    await act(() => vi.advanceTimersByTimeAsync(PANE_SPINNER_DELAY_MS + 10));
    expect(result.current.label).toBe(true);
    act(() => heard.retired?.());
    expect(result.current.label).toBe(false);
  });

  it("restore_look: an open find bar searches the rebuilt display again", () => {
    const addon = {
      findNext: vi.fn(() => true),
      findPrevious: vi.fn(() => true),
      clearDecorations: vi.fn(),
      onDidChangeResults: vi.fn(() => ({ dispose: () => {} })),
    };
    const props = {
      sessionId: "s1",
      getSearch: () => addon as unknown as TerminalSearch,
      onClear: () => true,
      onClose: () => {},
      initialState: {
        query: "needle",
        caseSensitive: false,
        regex: false,
        wholeWord: false,
        resultIndex: 0,
        resultCount: 1,
      },
    };
    const view = render(<TerminalFindBar {...props} displayGeneration={0} />);
    expect(addon.findNext).not.toHaveBeenCalled();
    view.rerender(<TerminalFindBar {...props} displayGeneration={1} />);
    expect(addon.findNext).toHaveBeenCalledWith(
      "needle",
      expect.objectContaining({ incremental: true }),
    );
    expect(addon.onDidChangeResults).toHaveBeenCalledTimes(2);
  });
});
