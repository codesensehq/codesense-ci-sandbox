// SPDX-License-Identifier: MIT
/**
 * A reconnect swaps displays instead of blanking the pane: the current display
 * is frozen (no stream, no input) under "Reconnecting…", the rebuilt one is
 * built parked from the snapshot, and it replaces the frozen one in one frame
 * once its snapshot is parsed. Real `@xterm/xterm` displays and a fake
 * terminal server; only the Tauri boundary is faked.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Terminal } from "@xterm/xterm";

import { FakeEventBus, FakeTerminalServer } from "./__test-utils__/fake-terminal-server";
import {
  RECONNECTING_LABEL,
  RECOVERY_RETRY_DELAYS_MS,
  RESTORE_FAILED,
  createTerminalManager,
  type DisplayHandle,
  type TerminalManager,
} from "./terminal-manager";
import { ATTACH_TIMEOUT_MS } from "@/features/viewer/lib/terminal-attach";

const h = vi.hoisted(() => ({
  bus: null as unknown as import("./__test-utils__/fake-terminal-server").FakeEventBus,
  server: null as unknown as import("./__test-utils__/fake-terminal-server").FakeTerminalServer,
}));

// No layout here: xterm's viewport scrolls by pixel height, which is 0. Scroll
// the buffer by lines, as the viewport does with a real cell height.
vi.mock("@xterm/xterm", async (importOriginal) => {
  const xterm = await importOriginal<typeof import("@xterm/xterm")>();
  class LaidOutTerminal extends xterm.Terminal {
    override scrollLines(amount: number): void {
      (this as any)._core._bufferService.scrollLines(amount);
    }
    override scrollToLine(line: number): void {
      this.scrollLines(line - this.buffer.active.viewportY);
    }
    override scrollToBottom(): void {
      this.scrollLines(this.buffer.active.baseY - this.buffer.active.viewportY);
    }
  }
  return { ...xterm, Terminal: LaidOutTerminal };
});
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, handler: (event: { payload: unknown }) => void) =>
    h.bus.listen(name, handler),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));
const transport = vi.hoisted(() => ({
  reserveTerminalStream: vi.fn(() => h.server.reserve()),
  subscribeTerminal: vi.fn((session: string, stream?: number) =>
    h.server.subscribe(session, stream),
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
const eventually = (check: () => void) => vi.waitFor(check, { timeout: 5000, interval: 5 });

let managers: TerminalManager[] = [];
let observers: MutationObserver[] = [];

beforeEach(() => {
  vi.clearAllMocks();
  h.bus = new FakeEventBus();
  h.server = new FakeTerminalServer(h.bus);
});

afterEach(() => {
  for (const o of observers) o.disconnect();
  observers = [];
  for (const m of managers) m.dispose("s1");
  managers = [];
  document.body.innerHTML = "";
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

const LABEL = '[data-testid="terminal-reconnecting"]';

function screenText(term: Terminal): string {
  const buffer = term.buffer.active;
  const rows: string[] = [];
  for (let y = 0; y < term.rows; y++) {
    rows.push(buffer.getLine(buffer.viewportY + y)?.translateToString(true) ?? "");
  }
  return rows.join("\n");
}

function topLine(term: Terminal): string {
  const buffer = term.buffer.active;
  return buffer.getLine(buffer.viewportY)?.translateToString(true) ?? "";
}

/** A session with `lines` of output, shown and focused in a slot, with a
 *  write gate on every display's apply queue. */
async function shown(lines: number) {
  const writes: Terminal[] = [];
  let hold = false;
  const held: Array<() => void> = [];
  const m = createTerminalManager({
    createWebgl: noWebgl,
    applyOutput: (term, data, parsed) => {
      writes.push(term);
      if (hold) held.push(() => term.write(data, parsed));
      else term.write(data, parsed);
    },
  });
  managers.push(m);
  h.server.open("s1");
  for (let i = 0; i < lines; i++) h.server.produce("s1", `line ${i}\r\n`);
  const slot = document.createElement("div");
  document.body.appendChild(slot);
  await m.ensure("s1", OPTS);
  m.attach("s1", slot, { focus: true });
  await m.debugDump("s1");
  const rebuilt: DisplayHandle[] = [];
  m.listen("s1", { rebuilt: (handle) => rebuilt.push(handle) });
  return {
    m,
    slot,
    writes,
    rebuilt,
    hold: () => {
      hold = true;
    },
    release: () => {
      hold = false;
      held.splice(0).forEach((write) => write());
    },
    held,
  };
}

/**
 * Watch `slot` at every DOM change and after every animation-frame callback:
 * it must always show exactly one display with content — the frozen one under
 * its label, or the registered one with its snapshot written. And the frozen
 * display is never the registered one.
 */
function watchFrames(m: TerminalManager, slot: HTMLElement, frozen: Terminal) {
  const blanks: string[] = [];
  const sample = (at: string) => {
    const hosts = slot.querySelectorAll<HTMLElement>(".cs-terminal-display");
    if (hosts.length !== 1) {
      blanks.push(`${at}: ${hosts.length} displays in the slot`);
      return;
    }
    const term = m.terminal("s1");
    if (term === frozen) blanks.push(`${at}: the frozen display is registered`);
    const host = hosts[0];
    if (host.querySelector(LABEL)) return;
    if (!term || !host.contains(term.element ?? null) || screenText(term).trim() === "") {
      blanks.push(`${at}: an unwritten display is shown`);
    }
  };
  const observer = new MutationObserver(() => sample("mutation"));
  observer.observe(slot, { childList: true, subtree: true });
  observers.push(observer);
  const raf = window.requestAnimationFrame.bind(window);
  vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) =>
    raf((t) => {
      cb(t);
      sample("frame");
    }),
  );
  return blanks;
}

/** One reconnect of `s1`, the rebuilt display's snapshot write held until the
 *  frozen display has been checked. `disposeEarly` removes the frozen display
 *  before that write's callback — what a plain dispose-then-rebuild does. */
async function reconnectSwap({ disposeEarly }: { disposeEarly: boolean }) {
  const s = await shown(40);
  const oldTerm = s.m.terminal("s1")!;
  s.m.focus("s1");
  expect(oldTerm.element!.contains(document.activeElement)).toBe(true);
  const blanks = watchFrames(s.m, s.slot, oldTerm);
  const created = (await s.m.debugDump("s1")).created;

  s.hold();
  h.server.loseStreams(h.server.boundStreams());
  const writesBefore = s.writes.length;
  // The frozen display: still in the slot, dimmed and labelled, taking no input.
  const oldHost = s.slot.querySelector<HTMLElement>(".cs-terminal-display")!;
  expect(oldHost.contains(oldTerm.element!)).toBe(true);
  expect(oldHost.querySelector(LABEL)?.textContent).toBe(RECONNECTING_LABEL);
  expect(s.m.terminal("s1")).toBeNull();
  oldTerm.input("typed while frozen");
  expect(transport.writePty).not.toHaveBeenCalled();

  // The rebuilt display's snapshot is being written, parked.
  await eventually(() => expect(s.held.length).toBeGreaterThan(0));
  const newTerm = s.m.terminal("s1")!;
  expect(newTerm).not.toBe(oldTerm);
  expect(s.slot.contains(newTerm.element!)).toBe(false);
  h.server.produce("s1", "after reconnect\r\n");
  if (disposeEarly) oldHost.remove();
  await new Promise((resolve) => setTimeout(resolve, 20));
  expect(s.slot.contains(newTerm.element!)).toBe(false);

  s.release();
  await eventually(() => expect(s.slot.contains(newTerm.element!)).toBe(true));
  const dump = await s.m.debugDump("s1");
  // No write ever reached the frozen display after the loss.
  expect(s.writes.slice(writesBefore).every((t) => t !== oldTerm)).toBe(true);
  return { ...s, blanks, oldHost, oldTerm, newTerm, dump, created };
}

describe("reconnect swap", () => {
  it("swap_in_one_frame: the frozen display stays until the rebuilt one replaces it", async () => {
    const r = await reconnectSwap({ disposeEarly: false });
    expect(r.blanks).toEqual([]);
    expect(r.oldHost.isConnected).toBe(false);
    expect(r.slot.querySelectorAll(".cs-terminal-display")).toHaveLength(1);
    expect(r.slot.querySelector(LABEL)).toBeNull();
    expect(r.newTerm.element!.contains(document.activeElement)).toBe(true);
    expect(r.dump.rebuilds).toBe(1);
    expect(r.dump.created).toBe(r.created + 1);
    expect(r.dump.lines).toContain("after reconnect");
    expect(r.rebuilt.map((handle) => handle.outcome.kind)).toEqual(["attached"]);
    // Input goes to the rebuilt display.
    r.newTerm.input("x");
    expect(transport.writePty).toHaveBeenCalledWith("s1", "x");
  });

  it("scroll_rule: bottom stays bottom; a scrolled-up reader keeps the line if restored, else bottom", async () => {
    // At bottom.
    let s = await shown(100);
    h.server.loseStreams(h.server.boundStreams());
    h.server.produce("s1", "gap output\r\n");
    await eventually(() => expect(s.slot.contains(s.m.terminal("s1")?.element ?? null)).toBe(true));
    let term = s.m.terminal("s1")!;
    await s.m.debugDump("s1");
    expect(term.buffer.active.viewportY).toBe(term.buffer.active.baseY);
    s.m.dispose("s1");
    document.body.innerHTML = "";

    // Scrolled up, the line survives in the restored scrollback.
    h.server = new FakeTerminalServer(h.bus);
    s = await shown(100);
    s.m.terminal("s1")!.scrollToLine(50);
    expect(topLine(s.m.terminal("s1")!)).toBe("line 50");
    h.server.loseStreams(h.server.boundStreams());
    await eventually(() => expect(s.slot.contains(s.m.terminal("s1")?.element ?? null)).toBe(true));
    term = s.m.terminal("s1")!;
    expect(topLine(term)).toBe("line 50");
    s.m.dispose("s1");
    document.body.innerHTML = "";

    // Scrolled up, the line scrolled out of the host's history during the gap.
    h.server = new FakeTerminalServer(h.bus);
    s = await shown(100);
    s.m.terminal("s1")!.scrollToLine(50);
    h.server.loseStreams(h.server.boundStreams());
    for (let i = 0; i < 1100; i++) h.server.produce("s1", `gap ${i}\r\n`);
    await eventually(() => expect(s.slot.contains(s.m.terminal("s1")?.element ?? null)).toBe(true));
    term = s.m.terminal("s1")!;
    expect(term.buffer.active.viewportY).toBe(term.buffer.active.baseY);
  });

  it("reconnect_gives_up: an incompatible server's state replaces the frozen display", async () => {
    const s = await shown(5);
    h.server.failSubscribes =
      "restart terminal server (ends 1 session): it speaks protocol 6, this app 7";
    h.server.loseStreams(h.server.boundStreams());
    expect(s.slot.querySelector(LABEL)).not.toBeNull();
    await eventually(() => expect(s.rebuilt).toHaveLength(1));
    expect(s.rebuilt[0].outcome.kind).toBe("incompatible");
    expect(s.slot.querySelector(".cs-terminal-display")).toBeNull();
    expect(s.slot.querySelector(LABEL)).toBeNull();
  });

  it("reconnect_gives_up: a restore that keeps failing ends in Couldn't restore, not a dimmed pane", async () => {
    const s = await shown(5);
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    h.server.failSubscribes = "emulator worker crashed";
    h.server.loseStreams(h.server.boundStreams());
    const retries = RECOVERY_RETRY_DELAYS_MS.reduce((a, b) => a + b, 0);
    await vi.advanceTimersByTimeAsync(retries + ATTACH_TIMEOUT_MS);
    vi.useRealTimers();
    await eventually(() => expect(s.rebuilt).toHaveLength(1));
    expect(s.rebuilt[0].outcome).toEqual({ kind: "failed", reason: RESTORE_FAILED });
    expect(s.slot.querySelector(".cs-terminal-display")).toBeNull();
    expect(s.slot.querySelector(LABEL)).toBeNull();
  });
});

describe("teeth", () => {
  it("disposing the frozen display before the rebuilt one's write callback is a blank frame", async () => {
    const r = await reconnectSwap({ disposeEarly: true });
    expect(r.blanks.length).toBeGreaterThan(0);
    expect(r.blanks[0]).toMatch(/0 displays in the slot/);
  });
});
