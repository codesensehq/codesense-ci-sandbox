// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { terminalFrameScheduler } from "@/shared/lib/frame-budget-scheduler";
import type { ReferenceBuffer } from "./buffer-dump";
import { loadPtylog, type PtylogFixture, type PtylogRecord } from "./__fixtures__/ptylog";
import { headlessFrom } from "./__test-utils__/headless-from";
import { FakeEventBus, FakeTerminalServer } from "./__test-utils__/fake-terminal-server";
import { chunked, paneSteps } from "./__test-utils__/pane-feed";
import {
  buildTerminalOptions,
  createTerminalManager,
  loadWebglRenderer,
  type DisplayDump,
  type TerminalManager,
  type TerminalManagerDeps,
} from "./terminal-manager";
import {
  DEFAULT_MAC_OPTION_IS_META,
  DEFAULT_SCROLLBACK,
  MAX_SCROLLBACK,
  TERMINAL_FONT_FAMILY,
  TERMINAL_SCROLL_SENSITIVITY,
  TERMINAL_SMOOTH_SCROLL_DURATION_MS,
  scrollbackLines,
} from "./terminal-constants";
import { TERMINAL_THEME } from "@/features/viewer/components/terminal-constants";
import { SCROLLBACK_OPTIONS } from "@/shared/constants/settings-options";
import type { WebglAddon } from "@xterm/addon-webgl";

const fake = vi.hoisted(() => ({
  bus: null as unknown as import("./__test-utils__/fake-terminal-server").FakeEventBus,
  server: null as unknown as import("./__test-utils__/fake-terminal-server").FakeTerminalServer,
}));

const listen = vi.hoisted(() =>
  vi.fn((name: string, handler: (event: { payload: unknown }) => void) =>
    fake.bus.listen(name, handler),
  ),
);
vi.mock("@tauri-apps/api/event", () => ({ listen }));
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

/** The reference terminals keep xterm's default history (1,000 lines); a parity
 *  check compares every row, so the display keeps the same. */
const OPTS = { fontSizePx: 13 };
const noWebgl: TerminalManagerDeps["createWebgl"] = () => {
  throw new Error("no WebGL in tests");
};

let managers: Array<{
  m: TerminalManager;
  sessions: Set<import("./terminal-display-reference").TerminalDisplayReference>;
}> = [];
function newManager(deps: TerminalManagerDeps = {}) {
  const m = createTerminalManager({ createWebgl: noWebgl, ...deps });
  const tracked = {
    m,
    sessions: new Set<import("./terminal-display-reference").TerminalDisplayReference>(),
  };
  managers.push(tracked);
  const ensure = m.ensure.bind(m);
  m.ensure = (sessionId, opts) => {
    tracked.sessions.add(sessionId);
    return ensure(sessionId, opts);
  };
  return m;
}

function slot(): HTMLElement {
  const el = document.createElement("div");
  document.body.appendChild(el);
  return el;
}

/** Run the clock until `pending` settles (a restore waits on xterm's parser,
 *  which runs on timers). */
async function until<T>(pending: Promise<T>): Promise<T> {
  let done = false;
  const settled = pending.finally(() => (done = true));
  for (let i = 0; i < 500 && !done; i++) await vi.advanceTimersByTimeAsync(10);
  return settled;
}

/** Let frames flush and xterm parse, then read the display's dump. */
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

/** A session "s1" opened fresh at the recordings' starting size. */
async function open(m: TerminalManager, sessionId = "s1") {
  fake.server.open(sessionId);
  const handle = await until(m.ensure(sessionId, OPTS));
  expect(handle?.outcome).toEqual({ kind: "attached" });
  return handle!;
}

/** Mulberry32: a seeded PRNG so a failing move sequence can be replayed. */
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

beforeEach(() => {
  vi.clearAllMocks();
  fake.bus = new FakeEventBus();
  fake.server = new FakeTerminalServer(fake.bus);
  vi.useFakeTimers(FAKE_TIMERS);
});

afterEach(() => {
  for (const { m, sessions } of managers) for (const s of sessions) m.dispose(s);
  managers = [];
  document.body.innerHTML = "";
  vi.useRealTimers();
});

describe("one display per session (I2)", () => {
  it("created_once_across_random_moves", async () => {
    const seed = Number(process.env.TERMINAL_MOVES_SEED ?? Date.now() % 2 ** 31);
    const random = prng(seed);
    const m = newManager();
    const first = await open(m);
    const element = m.terminal("s1")!.element;
    const slots = Array.from({ length: 5 }, slot);
    for (let move = 0; move < 200; move++) {
      const target = slots[Math.floor(random() * slots.length)];
      const kind = random();
      if (kind < 0.4) m.attach("s1", target, { focus: false });
      else if (kind < 0.7) m.detach("s1", target);
      else {
        // A remount: the slot unmounts, then a slot mounts and ensures again.
        m.detach("s1", target);
        await until(m.ensure("s1", OPTS));
        m.attach("s1", slots[Math.floor(random() * slots.length)], { focus: false });
      }
      const dump = await dumped(m, "s1");
      expect(
        { created: dump.created, instanceId: dump.instanceId },
        `seed ${seed}, move ${move}`,
      ).toEqual({ created: 1, instanceId: first.instanceId });
      expect(m.terminal("s1")!.element, `seed ${seed}, move ${move}`).toBe(element);
    }
  });

  it("out-of-order detach: slot A detaching after B attached leaves the display in B", async () => {
    const m = newManager();
    await open(m);
    const [a, b] = [slot(), slot()];
    m.attach("s1", a, { focus: false });
    m.attach("s1", b, { focus: false });
    m.detach("s1", a);
    expect(b.contains(m.terminal("s1")!.element!)).toBe(true);
    expect(a.childElementCount).toBe(0);
  });

  it("a borrowed display goes back to the slot it was borrowed from, shown as that slot now is", async () => {
    const m = newManager();
    await open(m);
    const [tab, popup] = [slot(), slot()];
    m.attach("s1", tab, { focus: false, visible: false });
    // The quick view borrows the display; the tab is shown meanwhile.
    m.attach("s1", popup, { focus: false, borrow: true });
    expect(m.holdsVisible("s1")).toBe(false);
    m.setVisible("s1", tab, true);
    expect(m.holdsVisible("s1")).toBe(true);
    m.detach("s1", popup);
    expect(m.holds("s1", tab)).toBe(true);
    expect(tab.contains(m.terminal("s1")!.element!)).toBe(true);
    expect((await dumped(m, "s1")).created).toBe(1);

    // A lender that left the page is skipped: the display parks.
    m.attach("s1", popup, { focus: false, borrow: true });
    tab.remove();
    m.detach("s1", popup);
    expect(m.holds("s1", tab)).toBe(false);
    expect(popup.childElementCount).toBe(0);
  });

  it("a lender that unmounts while lent out is not given the display back", async () => {
    const m = newManager();
    await open(m);
    const [tab, popup] = [slot(), slot()];
    m.attach("s1", tab, { focus: false });
    m.attach("s1", popup, { focus: false, borrow: true });
    m.detach("s1", tab);
    m.detach("s1", popup);
    expect(m.holds("s1", tab)).toBe(false);
    expect(tab.childElementCount).toBe(0);
  });

  it("a shown tile a grid cell took the display from gets it back when the cell lets go", async () => {
    const m = newManager();
    await open(m);
    const [tile, cell] = [slot(), slot()];
    m.attach("s1", tile, { focus: false });
    // The tab moves from its shown tile into a grid cell (or an expanded view): not a borrow.
    m.attach("s1", cell, { focus: false });
    expect(m.holds("s1", cell)).toBe(true);
    m.detach("s1", cell);
    expect(m.holds("s1", tile)).toBe(true);
    expect(tile.contains(m.terminal("s1")!.element!)).toBe(true);
    expect((await dumped(m, "s1")).created).toBe(1);
  });

  it("a hidden slot a shown one took the display from is not given it back: it parks", async () => {
    const m = newManager();
    await open(m);
    const [tile, cell] = [slot(), slot()];
    m.attach("s1", tile, { focus: false, visible: false });
    m.attach("s1", cell, { focus: false });
    m.detach("s1", cell);
    expect(m.holds("s1", tile)).toBe(false);
    expect(tile.childElementCount).toBe(0);
  });

  it("a lender given the display back while it is still being built keeps the focus it was owed", async () => {
    const m = newManager();
    fake.server.open("s1");
    const pending = m.ensure("s1", OPTS);
    const [tile, popup] = [slot(), slot()];
    // The tile asks for focus before the display exists; the quick view borrows it meanwhile.
    m.attach("s1", tile, { focus: true });
    m.attach("s1", popup, { focus: false, borrow: true });
    m.detach("s1", popup);
    await until(pending);
    await dumped(m, "s1");
    const term = m.terminal("s1")!;
    expect(tile.contains(term.element!)).toBe(true);
    expect(document.activeElement).toBe(term.textarea);
  });

  it("while borrowed, a hidden slot that mounts lends the display; a shown one takes it", async () => {
    const m = newManager();
    await open(m);
    const [popup, hidden, shown] = [slot(), slot(), slot()];
    m.attach("s1", popup, { focus: false, borrow: true });
    m.attach("s1", hidden, { focus: false, visible: false });
    expect(m.holds("s1", popup)).toBe(true);
    expect(m.holdsVisible("s1")).toBe(false);
    m.attach("s1", shown, { focus: false, visible: true });
    expect(m.holds("s1", shown)).toBe(true);
    expect(m.holdsVisible("s1")).toBe(true);
    // The displaced borrower's close gives nothing back.
    m.detach("s1", popup);
    expect(m.holds("s1", shown)).toBe(true);
  });

  it("a built display given back to its lender takes the focus the lender asked for", async () => {
    const m = newManager();
    await open(m);
    const [popup, tile] = [slot(), slot()];
    m.attach("s1", popup, { focus: false, borrow: true });
    // A hidden tile that asks for focus while the quick view has the display lends it.
    m.attach("s1", tile, { focus: true, visible: false });
    m.detach("s1", popup);
    const term = m.terminal("s1")!;
    expect(tile.contains(term.element!)).toBe(true);
    expect(document.activeElement).toBe(term.textarea);
  });

  it("a lender that left the page is dropped, and whether it is on screen is heard again", async () => {
    const m = newManager();
    await open(m);
    const [tab, popup, stray] = [slot(), slot(), slot()];
    m.attach("s1", tab, { focus: false });
    m.attach("s1", popup, { focus: false, borrow: true });
    expect(m.holdsVisible("s1")).toBe(true);
    const heard = vi.fn();
    const unwatch = m.watchShown("s1", heard);
    // The tab leaves the page without letting go; the next slot that does prunes it.
    tab.remove();
    m.detach("s1", stray);
    expect(heard).toHaveBeenCalledTimes(1);
    expect(m.holdsVisible("s1")).toBe(false);
    m.detach("s1", popup);
    expect(popup.childElementCount).toBe(0);
    unwatch();
  });

  it("a lender that leaves the page while lent out is heard within a frame, with no slot letting go", async () => {
    const m = newManager();
    await open(m);
    const [tab, popup] = [slot(), slot()];
    m.attach("s1", tab, { focus: false });
    m.attach("s1", popup, { focus: false, borrow: true });
    const heard = vi.fn();
    const unwatch = m.watchShown("s1", heard);
    // Its tile's subtree goes: nothing calls detach for the tab yet. The page is checked once a
    // frame, however many changes it had.
    tab.remove();
    document.body.appendChild(document.createElement("div")).remove();
    await Promise.resolve();
    expect(heard).not.toHaveBeenCalled();
    vi.advanceTimersToNextFrame();
    expect(heard).toHaveBeenCalledTimes(1);
    expect(m.holdsVisible("s1")).toBe(false);
    m.detach("s1", popup);
    unwatch();
  });

  it("given back to a lender owed focus, it leaves focus where the close put it (the row)", async () => {
    const m = newManager();
    await open(m);
    const [popup, tile] = [slot(), slot()];
    m.attach("s1", popup, { focus: false, borrow: true });
    m.attach("s1", tile, { focus: true, visible: false });
    // The quick view's Escape gives focus back to its row before the popup lets go.
    const row = document.createElement("button");
    document.body.appendChild(row);
    row.focus();
    m.detach("s1", popup);
    expect(tile.contains(m.terminal("s1")!.element!)).toBe(true);
    expect(document.activeElement).toBe(row);
    row.remove();
  });

  it("watchShown hears only its session, and only when whether it is on screen changes", async () => {
    const m = newManager();
    await open(m);
    await open(m, "s2");
    const [tile, other, stray] = [slot(), slot(), slot()];
    const heard = vi.fn();
    const unwatch = m.watchShown("s1", heard);
    m.attach("s2", other, { focus: false });
    expect(heard).not.toHaveBeenCalled();
    m.attach("s1", tile, { focus: false, visible: false });
    expect(heard).not.toHaveBeenCalled();
    m.setVisible("s1", tile, true);
    expect(heard).toHaveBeenCalledTimes(1);
    // A slot that does not hold the display letting go changes nothing.
    m.detach("s1", stray);
    m.setVisible("s1", tile, true);
    expect(heard).toHaveBeenCalledTimes(1);
    m.detach("s1", tile);
    expect(heard).toHaveBeenCalledTimes(2);
    unwatch();
    m.attach("s1", tile, { focus: false });
    expect(heard).toHaveBeenCalledTimes(2);
  });

  it("holdsVisible: a shown tile holds it on screen; a hidden one or a parked display does not", async () => {
    const m = newManager();
    await open(m);
    const tile = slot();
    const heard = vi.fn();
    const unwatch = m.watchShown("s1", heard);
    expect(m.holdsVisible("s1")).toBe(false);
    m.attach("s1", tile, { focus: false, visible: true });
    expect(m.holdsVisible("s1")).toBe(true);
    m.setVisible("s1", tile, false);
    expect(m.holdsVisible("s1")).toBe(false);
    expect(heard).toHaveBeenCalled();
    unwatch();
  });

  it("dispose rules: hide parks; a new process life rebuilds once; close disposes", async () => {
    const m = newManager();
    const first = await open(m);
    const a = slot();
    m.attach("s1", a, { focus: false });
    // Hide (the slot goes away) parks the display; the next show is the same one.
    m.detach("s1", a);
    const shown = await until(m.ensure("s1", OPTS));
    m.attach("s1", slot(), { focus: false });
    expect(shown?.instanceId).toBe(first.instanceId);
    expect((await dumped(m, "s1")).created).toBe(1);

    // Restart in place: the tab's life changes → exactly one rebuild.
    const respawned = await until(m.ensure("s1", { ...OPTS, life: 1 }));
    expect(respawned?.instanceId).not.toBe(first.instanceId);
    const dump = await dumped(m, "s1");
    expect(dump).toMatchObject({ created: 2, rebuilds: 1 });
    await until(m.ensure("s1", { ...OPTS, life: 1 }));
    expect((await dumped(m, "s1")).created).toBe(2);

    m.dispose("s1");
    expect(m.terminal("s1")).toBeNull();
    expect(fake.server.boundStreams()).toEqual([]);
  });

  it("logs one `terminal display created` line per new Terminal, with its reason", async () => {
    const { logger } = await import("@/shared/lib/logger");
    const info = vi.spyOn(logger, "info");
    const m = newManager();
    await open(m);
    for (let i = 0; i < 5; i++) {
      m.attach("s1", slot(), { focus: false });
      await until(m.ensure("s1", OPTS));
    }
    await until(m.ensure("s1", { ...OPTS, life: 1 }));
    const created = info.mock.calls
      .map(([, line]) => String(line))
      .filter((line) => line.startsWith("terminal display created"));
    expect(created).toEqual([
      "terminal display created: session_id=s1 reason=first",
      "terminal display created: session_id=s1 reason=respawn",
    ]);
  });
});

describe("a parked display matches the reference (I4)", () => {
  const references = new Map<PtylogFixture, ReferenceBuffer>();
  beforeEach(async () => {
    vi.useRealTimers();
    for (const name of ["codex-tui", "claude-ink", "append-only"] as const) {
      if (!references.has(name)) references.set(name, await headlessFrom(name));
    }
    vi.useFakeTimers(FAKE_TIMERS);
  });

  /** Stream `name` into a display: attached for the first of three chunks,
   *  parked 5 s, the rest fed while parked, then attached again. With
   *  `rebuildOnAttach` every attach disposes the display and restores it from
   *  the host snapshot. */
  async function streamParkedMidway(
    name: PtylogFixture,
    opts: { rebuildOnAttach?: boolean; showBetweenChunks?: boolean } = {},
  ): Promise<ReferenceBuffer> {
    const m = newManager();
    await open(m);
    const attach = async (target: HTMLElement) => {
      if (opts.rebuildOnAttach) {
        m.dispose("s1");
        await until(m.ensure("s1", OPTS));
      }
      m.attach("s1", target, { focus: false });
    };
    const a = slot();
    await attach(a);
    const chunks = chunked(paneSteps(loadPtylog(name)), 3);
    expect(chunks.length).toBeGreaterThanOrEqual(3);
    for (const [index, chunk] of chunks.entries()) {
      for (const step of chunk) {
        if (step.kind === "output") fake.server.produce("s1", step.data);
        else m.requestResize("s1", step.cols, step.rows);
      }
      if (index === 0) {
        m.detach("s1", a);
        await vi.advanceTimersByTimeAsync(5000);
      } else if (opts.showBetweenChunks && index < chunks.length - 1) {
        await attach(a);
        await dumped(m, "s1");
        m.detach("s1", a);
      }
    }
    await attach(a);
    return bufferOf(await dumped(m, "s1"));
  }

  it.each(["codex-tui", "claude-ink", "append-only"] as const)(
    "parked_display_matches_reference_on_cursor_addressed_fixture: %s",
    async (name) => {
      expect(await streamParkedMidway(name)).toEqual(references.get(name));
      expect(
        await streamParkedMidway(name, { showBetweenChunks: true }),
        "shown between chunks too",
      ).toEqual(references.get(name));
    },
  );

  it.each(["codex-tui", "claude-ink", "append-only"] as const)(
    "a display restored from the snapshot on every attach matches too: %s",
    async (name) => {
      expect(await streamParkedMidway(name, { rebuildOnAttach: true })).toEqual(
        references.get(name),
      );
    },
  );
});

describe("scroll position survives a park (AC-7)", () => {
  async function scrolledDisplay(scrollback: number) {
    const m = newManager();
    fake.server.open("s1", { cols: 80, rows: 24, scrollback });
    await until(m.ensure("s1", OPTS));
    const a = slot();
    m.attach("s1", a, { focus: false });
    return { m, a };
  }
  const lines = (from: number, to: number) =>
    Array.from({ length: to - from }, (_, i) => `line ${from + i}\r\n`).join("");

  it("a display scrolled up keeps its top line and thumb; one at the bottom follows output", async () => {
    const { m, a } = await scrolledDisplay(5000);
    fake.server.produce("s1", lines(0, 400));
    await dumped(m, "s1");
    const term = m.terminal("s1")!;
    // The glide animates on real time; the position is what's under test.
    term.options.smoothScrollDuration = 0;
    term.scrollLines(-150);
    await vi.advanceTimersByTimeAsync(500); // the smooth-scroll glide
    const viewportY = term.buffer.active.viewportY;
    const thumb = viewportY / term.buffer.active.baseY;
    m.detach("s1", a);
    fake.server.produce("s1", lines(400, 450));
    await vi.advanceTimersByTimeAsync(2000);
    m.attach("s1", a, { focus: false });
    await dumped(m, "s1");
    expect(term.buffer.active.viewportY).toBe(viewportY);
    expect(term.buffer.active.getLine(viewportY)?.translateToString(true)).toBe(
      `line ${viewportY}`,
    );
    // The thumb sits at the same line; the content below it grew.
    expect(term.buffer.active.viewportY / term.buffer.active.baseY).toBeLessThan(thumb);

    term.scrollToBottom();
    await vi.advanceTimersByTimeAsync(500);
    m.detach("s1", a);
    fake.server.produce("s1", lines(450, 500));
    await vi.advanceTimersByTimeAsync(2000);
    m.attach("s1", a, { focus: false });
    await dumped(m, "s1");
    expect(term.buffer.active.viewportY).toBe(term.buffer.active.baseY);
    expect(term.buffer.active.getLine(term.buffer.active.baseY + 22)?.translateToString(true)).toBe(
      "line 499",
    );
  });

  it("with a full scrollback the same top line (by text) stays on top", async () => {
    const { m, a } = await scrolledDisplay(200);
    fake.server.produce("s1", lines(0, 400));
    await dumped(m, "s1");
    const term = m.terminal("s1")!;
    term.options.smoothScrollDuration = 0;
    term.scrollLines(-100);
    await vi.advanceTimersByTimeAsync(500);
    const top = term.buffer.active.getLine(term.buffer.active.viewportY)!.translateToString(true);
    m.detach("s1", a);
    fake.server.produce("s1", lines(400, 420));
    await vi.advanceTimersByTimeAsync(2000);
    m.attach("s1", a, { focus: false });
    await dumped(m, "s1");
    const now = term.buffer.active.getLine(term.buffer.active.viewportY)!.translateToString(true);
    expect(now).toBe(top);
  });
});

describe("focus on attach (AC-8)", () => {
  it.each(["direct", "borrowed", "hidden lender"])(
    "%s restore rechecks focus permission only when the display is placed",
    async (path) => {
      const m = newManager();
      fake.server.open("s1");
      fake.server.holdSubscribes = true;
      const pending = m.ensure("s1", OPTS);
      const [tile, popup] = [slot(), slot()];
      let permitted = true;
      const policy = vi.fn(() => permitted);
      if (path === "hidden lender") {
        m.attach("s1", popup, { focus: false, borrow: true });
        m.attach("s1", tile, { focus: policy, visible: false });
      } else {
        m.attach("s1", tile, { focus: policy });
        if (path === "borrowed") m.attach("s1", popup, { focus: false, borrow: true });
      }
      if (path !== "direct") m.detach("s1", popup);
      await vi.advanceTimersByTimeAsync(20);
      expect(m.terminal("s1")).toBeNull();
      expect(policy).not.toHaveBeenCalled();
      permitted = false;
      fake.server.releaseSubscribes();
      await until(pending);
      const term = m.terminal("s1")!;
      expect(tile.contains(term.element!)).toBe(true);
      expect(policy).toHaveBeenCalledOnce();
      expect(document.activeElement).not.toBe(term.textarea);
    },
  );

  it("a queued focus request keeps its live permission until restore", async () => {
    const m = newManager();
    fake.server.open("s1");
    fake.server.holdSubscribes = true;
    const pending = m.ensure("s1", OPTS);
    m.attach("s1", slot(), { focus: false });
    let permitted = true;
    const policy = vi.fn(() => permitted);
    m.focus("s1", policy);
    await vi.advanceTimersByTimeAsync(20);
    expect(policy).not.toHaveBeenCalled();
    permitted = false;
    fake.server.releaseSubscribes();
    await until(pending);
    expect(policy).toHaveBeenCalledOnce();
    expect(document.activeElement).not.toBe(m.terminal("s1")!.textarea);
  });

  it("a still-current live permission focuses the restored display", async () => {
    const m = newManager();
    fake.server.open("s1");
    const pending = m.ensure("s1", OPTS);
    const policy = vi.fn(() => true);
    m.attach("s1", slot(), { focus: policy });
    expect(policy).not.toHaveBeenCalled();
    await until(pending);
    expect(policy).toHaveBeenCalledOnce();
    expect(document.activeElement).toBe(m.terminal("s1")!.textarea);
  });

  it("an active slot takes focus and typing reaches the PTY; an inactive one does not steal it", async () => {
    const m = newManager();
    await open(m);
    const elsewhere = document.createElement("input");
    document.body.appendChild(elsewhere);
    elsewhere.focus();
    m.attach("s1", slot(), { focus: false });
    expect(document.activeElement).toBe(elsewhere);
    m.attach("s1", slot(), { focus: true });
    const term = m.terminal("s1")!;
    expect(document.activeElement).toBe(term.textarea);
    term.input("ls\r", true);
    expect(transport.writePty).toHaveBeenCalledWith("s1", "ls\r");
  });
});

describe("apply pipeline (§3.6)", () => {
  /** xterm's write buffer, held by hand: the bytes are handed over but not
   *  parsed until the test releases them. */
  function heldWrites() {
    const held: Array<() => void> = [];
    const applyOutput: TerminalManagerDeps["applyOutput"] = (term, data, parsed) =>
      held.push(() => term.write(data, parsed));
    return { held, applyOutput };
  }

  it("applied_seq_is_parse_complete", async () => {
    const { held, applyOutput } = heldWrites();
    const m = newManager({ applyOutput });
    fake.server.open("s1");
    const opening = m.ensure("s1", OPTS);
    await vi.advanceTimersByTimeAsync(50);
    held.splice(0).forEach((f) => f());
    await until(opening);
    fake.server.produce("s1", "before\r\n");
    await vi.advanceTimersByTimeAsync(50);
    held.splice(0).forEach((f) => f());
    const bufferBefore = (await dumped(m, "s1")).lines;
    const seqBefore = m.appliedSeq("s1");
    expect(seqBefore).toBe(8);

    fake.server.produce("s1", "after\r\n");
    await vi.advanceTimersByTimeAsync(50); // the frame flushes: write called, parse held
    const term = m.terminal("s1")!;
    // Mid-write: position and buffer are both the previous ones, never a mix.
    expect(m.appliedSeq("s1")).toBe(seqBefore);
    expect(term.buffer.active.getLine(1)?.translateToString(true)).toBe(bufferBefore[1]);
    let dump: DisplayDump | null = null;
    void m.debugDump("s1").then((d) => (dump = d));
    await vi.advanceTimersByTimeAsync(50);
    expect(dump).toBeNull(); // queued behind the pending write
    held.splice(0).forEach((f) => f());
    await vi.advanceTimersByTimeAsync(50);
    expect(dump!.appliedSeq).toBe(fake.server.seqOf("s1"));
    expect(dump!.lines[1]).toBe("after");
  });

  it("a coalesced chunk still waiting for its frame is parsed before a queued dump", async () => {
    const m = newManager();
    await open(m);
    fake.server.produce("s1", "pending\r\n");
    // No frame has run: the chunk sits in the coalescer.
    const dump = await dumped(m, "s1");
    expect(dump.lines[0]).toBe("pending");
    expect(dump.appliedSeq).toBe(fake.server.seqOf("s1"));
  });

  describe("teeth", () => {
    it("advancing appliedSeq when write is called (not parsed) shows a seq ahead of its buffer", async () => {
      const held: Array<() => void> = [];
      const m = newManager({
        applyOutput: (term, data, parsed) => {
          parsed();
          held.push(() => term.write(data));
        },
      });
      fake.server.open("s1");
      const opening = m.ensure("s1", OPTS);
      await vi.advanceTimersByTimeAsync(20);
      held.splice(0).forEach((f) => f());
      await until(opening);
      fake.server.produce("s1", "after\r\n");
      await vi.advanceTimersByTimeAsync(20);
      const term = m.terminal("s1")!;
      const parsedLine = term.buffer.active.getLine(0)?.translateToString(true);
      expect(m.appliedSeq("s1")).toBe(fake.server.seqOf("s1"));
      expect(parsedLine).not.toBe("after");
    });
  });
});

describe("streams lost (reconnect)", () => {
  it("streams_lost_rebuilds_once", async () => {
    const { logger } = await import("@/shared/lib/logger");
    const info = vi.spyOn(logger, "info");
    const records = loadPtylog("append-only");
    const steps = paneSteps(records);
    const half = Math.floor(steps.length / 2);
    const m = newManager();
    await open(m);
    const a = slot();
    m.attach("s1", a, { focus: false });
    const feed = (from: number, to: number) => {
      for (const step of steps.slice(from, to)) {
        if (step.kind === "output") fake.server.produce("s1", step.data);
        else m.requestResize("s1", step.cols, step.rows);
      }
    };
    feed(0, half);
    await dumped(m, "s1");
    const [lost] = fake.server.boundStreams();
    transport.reserveTerminalStream.mockClear();
    transport.subscribeTerminal.mockClear();
    fake.server.loseStreams([lost]);
    await vi.advanceTimersByTimeAsync(50);
    expect(transport.unsubscribeTerminal).toHaveBeenCalledWith(lost);
    const [fresh] = fake.server.boundStreams();
    expect(fresh).not.toBe(lost);
    // Reserved, then listened on, then subscribed on that id.
    expect(transport.subscribeTerminal).toHaveBeenCalledWith("s1", fresh);
    feed(half, steps.length);
    const dump = await dumped(m, "s1");
    expect(dump).toMatchObject({ created: 2, rebuilds: 1 });
    expect(a.contains(m.terminal("s1")!.element!)).toBe(true);
    expect(info.mock.calls.filter(([, l]) => String(l).includes("reason=reconnect"))).toHaveLength(
      1,
    );
    expect(bufferOf(dump)).toEqual(await headlessFromReal("append-only"));
    expect(listen.mock.calls.map(([name]) => name)).not.toContain("pty-data");
  });
});

async function headlessFromReal(name: PtylogFixture | PtylogRecord[]) {
  vi.useRealTimers();
  try {
    return await headlessFrom(name);
  } finally {
    vi.useFakeTimers(FAKE_TIMERS);
  }
}

describe("WebGL lease (I7)", () => {
  interface StubAddon {
    loss: (() => void) | null;
    live: boolean;
  }
  function stubWebgl() {
    const addons: StubAddon[] = [];
    const create = () => {
      const a = {
        loss: null as (() => void) | null,
        live: true,
        activate: vi.fn(),
        dispose: vi.fn(() => {
          a.live = false;
        }),
        onContextLoss: (cb: () => void) => {
          a.loss = cb;
          return { dispose: () => {} };
        },
      };
      addons.push(a);
      return a as unknown as WebglAddon;
    };
    return { addons, create, live: () => addons.filter((a) => a.live) };
  }
  const frames = (n = 3) => vi.advanceTimersByTimeAsync(16 * n);
  // The shared frame scheduler outlives each test's fake clock: drain it so a
  // frame an earlier test left pending does not stall this one.
  beforeEach(() => terminalFrameScheduler.flush());

  it("switches the theme with 20 live terminals under 100 ms without losing a context", async () => {
    const { applyTheme } = await import("@/themes/runtime");
    const { resolveTerminalTheme } = await import("@/themes/terminal");
    const { LiveHub } = await import("@/features/dashboard/lib/live-hub");
    const hub = new LiveHub({
      read: async () => {
        throw new Error("unexpected backend theme read");
      },
      listen: async () => () => {},
      readUi: async () => ({}),
      writeUi: async (_, v) => v,
    });
    const gl = stubWebgl(),
      m = newManager({ createWebgl: gl.create });
    for (let i = 0; i < 20; i++) {
      await open(m, `perf${i}`);
      m.attach(`perf${i}`, slot(), { focus: false });
    }
    await frames();
    applyTheme("codesense", "light");
    const live = gl.live().slice();
    const start = performance.now();
    applyTheme("nord", "dark");
    hub.publishTheme("nord", "dark");
    const config = resolveTerminalTheme("match-app", "nord", "dark");
    for (let i = 0; i < 20; i++)
      m.setAppearance(`perf${i}`, { fontSizePx: 13, themeConfig: config });
    const elapsed = performance.now() - start;
    process.stdout.write(
      `Theme switch, 20 live terminals: ${elapsed.toFixed(2)} ms; ${live.length} WebGL leases retained\n`,
    );
    expect(elapsed).toBeLessThan(100);
    expect(gl.live()).toEqual(live);
    for (let i = 0; i < 20; i++) expect(m.terminal(`perf${i}`)!.options.theme).toBe(config.theme);
    hub.dispose();
    applyTheme("codesense", "light");
  });

  it("shown displays borrow at most 10 contexts; a closed one hands its lease on", async () => {
    const gl = stubWebgl();
    const m = newManager({ createWebgl: gl.create });
    for (let i = 0; i < 12; i++) {
      await open(m, `s${i}`);
      m.attach(`s${i}`, slot(), { focus: false });
    }
    await frames();
    expect(gl.live()).toHaveLength(10);
    expect(m.webglLease("s0")).toEqual({ leased: true, pinned: false, held: 10, cap: 10 });
    expect(m.webglLease("s11")?.leased).toBe(false);
    m.dispose("s0");
    await frames();
    expect(gl.live()).toHaveLength(10);
    expect(m.webglLease("s10")?.leased).toBe(true);
  });

  it("a borrowed display given back takes the lender's shown state, and its lease with it", async () => {
    const gl = stubWebgl();
    const m = newManager({ createWebgl: gl.create });
    await open(m);
    const [tab, popup] = [slot(), slot()];
    m.attach("s1", tab, { focus: false, visible: false });
    m.attach("s1", popup, { focus: false, borrow: true });
    await frames();
    expect(m.webglLease("s1")?.leased).toBe(true);
    // The tab is shown while the quick view has the display: it gets it back shown.
    m.setVisible("s1", tab, true);
    m.detach("s1", popup);
    await frames();
    expect(m.webglLease("s1")?.leased).toBe(true);

    // Hidden while lent out: it gets it back hidden, and the lease goes.
    m.attach("s1", popup, { focus: false, borrow: true });
    m.setVisible("s1", tab, false);
    m.detach("s1", popup);
    await vi.advanceTimersByTimeAsync(800);
    expect(m.holds("s1", tab)).toBe(true);
    expect(m.webglLease("s1")?.leased).toBe(false);
  });

  it("swap_is_geometry_neutral", async () => {
    const gl = stubWebgl();
    const m = newManager({ createWebgl: gl.create });
    await open(m);
    for (let i = 0; i < 80; i++) fake.server.produce("s1", `line ${i}\r\n`);
    const target = slot();
    m.attach("s1", target, { focus: true });
    await dumped(m, "s1");
    await frames();
    expect(gl.live()).toHaveLength(1);
    const term = m.terminal("s1")!;
    // Smooth scrolling animates on the wall clock; jump instead.
    term.options.smoothScrollDuration = 0;
    term.scrollLines(-5);
    term.select(0, term.buffer.active.viewportY + 1, 6);
    const before = {
      viewportY: term.buffer.active.viewportY,
      selection: term.getSelectionPosition(),
      focus: document.activeElement,
    };
    const spies = [
      vi.spyOn(term, "resize"),
      vi.spyOn(term, "reset"),
      vi.spyOn(term, "write"),
      vi.spyOn(m.fitAddon("s1")!, "fit"),
    ];
    const state = () => ({
      viewportY: term.buffer.active.viewportY,
      selection: term.getSelectionPosition(),
      focus: document.activeElement,
    });

    // WebGL → DOM (context lost), then DOM → WebGL on the next frame.
    gl.live()[0].loss!();
    expect(gl.live()).toHaveLength(0);
    expect(state()).toEqual(before);
    await frames();
    expect(gl.live()).toHaveLength(1);
    expect(state()).toEqual(before);
    // WebGL → DOM (released on hide), then DOM → WebGL on show.
    m.setVisible("s1", target, false);
    await vi.advanceTimersByTimeAsync(800);
    expect(gl.live()).toHaveLength(0);
    m.setVisible("s1", target, true);
    await frames();
    expect(gl.live()).toHaveLength(1);
    expect(state()).toEqual(before);

    for (const spy of spies) expect(spy).not.toHaveBeenCalled();
    expect(transport.resizePty).not.toHaveBeenCalled();
  });

  describe("context_loss_keeps_buffer", () => {
    /** codex-tui fed to a point where its alt screen is up. */
    async function midStream() {
      const records = loadPtylog("codex-tui");
      const steps = paneSteps(records);
      const cut = Math.floor(steps.length / 2);
      const fed = steps.slice(0, cut);
      const gl = stubWebgl();
      const m = newManager({ createWebgl: gl.create });
      await open(m);
      m.attach("s1", slot(), { focus: true });
      for (const step of fed) {
        if (step.kind === "output") fake.server.produce("s1", step.data);
        else m.requestResize("s1", step.cols, step.rows);
      }
      await dumped(m, "s1");
      await frames();
      // Switching clocks drops pending fake timers: let the host's emulator finish first.
      await until(fake.server.emulator("s1"));
      const reference = await headlessFromReal(records.slice(0, cut));
      return { m, gl, reference };
    }

    it("falls back to DOM with the same buffer and the same display", async () => {
      const { m, gl, reference } = await midStream();
      expect(reference.alt, "the cut is inside the alt screen").toBeTruthy();
      const before = await dumped(m, "s1");
      gl.live()[0].loss!();
      expect(m.webglLease("s1")?.leased).toBe(false);
      const after = await dumped(m, "s1");
      expect(bufferOf(after)).toEqual(reference);
      expect(after.created).toBe(before.created);
      expect(after.instanceId).toBe(before.instanceId);
    });

    describe("teeth", () => {
      it("a rebuild on context loss changes `created`", async () => {
        const { m, gl } = await midStream();
        const before = await dumped(m, "s1");
        gl.live()[0].loss!();
        // The fault: dispose and re-create the display instead of falling back.
        await until(m.ensure("s1", { ...OPTS, life: 1 }));
        const after = await dumped(m, "s1");
        expect(after.created).not.toBe(before.created);
      });
    });
  });
});

describe("find state lives with the session", () => {
  it("is kept across panes and cleared with the display", async () => {
    const m = newManager();
    await open(m);
    const state = {
      query: "error",
      caseSensitive: true,
      regex: false,
      wholeWord: false,
      resultIndex: 2,
      resultCount: 5,
    };
    m.setFindState("s1", state);
    expect(m.findState("s1")).toEqual(state);
    m.dispose("s1");
    expect(m.findState("s1")).toBeNull();
  });
});

describe("buildTerminalOptions", () => {
  it("carries the native-feel defaults", () => {
    const opts = buildTerminalOptions({ fontSizePx: 16, scrollback: 5_000 });
    expect(opts).toMatchObject({
      fontSize: 16,
      scrollback: 5_000,
      cursorStyle: "block",
      cursorInactiveStyle: "outline",
      cursorBlink: true,
      scrollSensitivity: TERMINAL_SCROLL_SENSITIVITY,
      smoothScrollDuration: TERMINAL_SMOOTH_SCROLL_DURATION_MS,
      drawBoldTextInBrightColors: false,
      fontWeight: "normal",
      fontWeightBold: "bold",
      macOptionIsMeta: DEFAULT_MAC_OPTION_IS_META,
      allowProposedApi: true,
      theme: TERMINAL_THEME,
    });
    expect(opts.fontFamily).toContain(TERMINAL_FONT_FAMILY);
    expect(
      buildTerminalOptions({ fontSizePx: 14, scrollback: 1, macOptionIsMeta: false }),
    ).toMatchObject({ macOptionIsMeta: false });
    const theme = { background: "#123456" };
    expect(
      buildTerminalOptions({
        fontSizePx: 14,
        scrollback: 1,
        themeConfig: { theme, chromeBg: "#000" },
      }).theme,
    ).toBe(theme);
  });

  it("the display is built with the session's history length, from its snapshot", async () => {
    const m = newManager();
    fake.server.open("s1", { scrollback: 1234 });
    await until(m.ensure("s1", OPTS));
    expect(m.terminal("s1")!.options.scrollback).toBe(1234);
  });
});

describe("scrollbackLines", () => {
  it("defaults to 5,000, maps Maximum to the ceiling, and sanitizes", () => {
    expect(DEFAULT_SCROLLBACK).toBe(5_000);
    expect(scrollbackLines(undefined)).toBe(5_000);
    expect(scrollbackLines(null)).toBe(5_000);
    expect(scrollbackLines(Number.NaN)).toBe(5_000);
    expect(scrollbackLines(-3)).toBe(5_000);
    expect(scrollbackLines(0)).toBe(MAX_SCROLLBACK);
    expect(scrollbackLines(10_000)).toBe(10_000);
    expect(scrollbackLines(1234.7)).toBe(1234);
    expect(scrollbackLines(10_000_000)).toBe(MAX_SCROLLBACK);
  });

  it("the settings presets are unchanged", () => {
    expect(SCROLLBACK_OPTIONS.map((o) => o.value)).toEqual([1000, 5000, 10000, 50000, 0]);
  });
});

describe("loadWebglRenderer", () => {
  function fakeWebgl() {
    let loss: (() => void) | null = null;
    const addon = {
      onContextLoss: vi.fn((cb: () => void) => {
        loss = cb;
        return { dispose: () => {} };
      }),
      dispose: vi.fn(),
    };
    return { addon: addon as unknown as WebglAddon, dispose: addon.dispose, loss: () => loss?.() };
  }

  it("loads after open and falls back to the DOM renderer on any failure or context loss", () => {
    const ok = fakeWebgl();
    const handle = loadWebglRenderer(() => ok.addon, vi.fn());
    expect(handle.active).toBe(true);
    ok.loss();
    expect(ok.dispose).toHaveBeenCalledOnce();
    expect(handle.active).toBe(false);
    handle.dispose();
    expect(ok.dispose).toHaveBeenCalledOnce();

    const failing = fakeWebgl();
    const noContext = loadWebglRenderer(
      () => failing.addon,
      () => {
        throw new Error("no WebGL context");
      },
    );
    expect(noContext.active).toBe(false);
    expect(failing.dispose).toHaveBeenCalledOnce();

    const throwing = loadWebglRenderer(() => {
      throw new Error("no GPU");
    }, vi.fn());
    expect(throwing.active).toBe(false);
  });

  it("is re-entrancy-safe when dispose fires the loss event", () => {
    const w = fakeWebgl();
    w.dispose.mockImplementation(() => w.loss());
    const handle = loadWebglRenderer(() => w.addon, vi.fn());
    expect(() => handle.dispose()).not.toThrow();
    expect(w.dispose).toHaveBeenCalledOnce();
  });
});

it("defers a hidden terminal's palette until it returns to a visible slot", async () => {
  const { resolveTerminalTheme } = await import("@/themes/terminal");
  const m = newManager();
  await open(m);
  const face = slot();
  m.attach("s1", face, { visible: true, focus: false });
  const before = m.terminal("s1")!.options.theme;
  m.setVisible("s1", face, false);
  const next = resolveTerminalTheme("match-app", "nord", "light");
  m.setAppearance("s1", { fontSizePx: 13, themeConfig: next });
  expect(m.terminal("s1")!.options.theme).toBe(before);
  m.setVisible("s1", face, true);
  expect(m.terminal("s1")!.options.theme).toBe(next.theme);
});
