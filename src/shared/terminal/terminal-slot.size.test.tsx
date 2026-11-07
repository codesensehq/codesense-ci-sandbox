// SPDX-License-Identifier: MIT
import { act, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, onTestFinished, vi } from "vitest";

import { FitAddon } from "@xterm/addon-fit";
import { FakeEventBus, FakeTerminalServer } from "./__test-utils__/fake-terminal-server";
import {
  SIZE_SETTLE_BOUND_MS,
  createTerminalManager,
  type TerminalManager,
} from "./terminal-manager";
import { FONT_WAIT_MS, RESIZE_SETTLE_MS, TerminalSlot } from "./terminal-slot";

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
    reserveTerminalStream: () => fake.server.reserve(),
    subscribeTerminal: (session: string, stream?: number) => fake.server.subscribe(session, stream),
    unsubscribeTerminal: (stream: number) => fake.server.unsubscribe(stream),
    ...transport,
  }),
);

/** A ResizeObserver the test drives: `resizeTo` sets a slot's pixel size and
 *  notifies its observers, as the browser does after layout. */
const observers = new Set<{ el: Element; cb: ResizeObserverCallback; ro: ResizeObserver }>();
class TestResizeObserver {
  private readonly targets: Element[] = [];
  constructor(private readonly cb: ResizeObserverCallback) {}
  observe(el: Element) {
    this.targets.push(el);
    observers.add({ el, cb: this.cb, ro: this as unknown as ResizeObserver });
  }
  unobserve() {}
  disconnect() {
    for (const o of [...observers])
      if (o.ro === (this as unknown as ResizeObserver)) observers.delete(o);
  }
}
const pixels = new WeakMap<Element, { w: number; h: number }>();
function setPixels(el: HTMLElement, w: number, h: number) {
  pixels.set(el, { w, h });
  Object.defineProperty(el, "clientWidth", { configurable: true, get: () => pixels.get(el)!.w });
  Object.defineProperty(el, "clientHeight", { configurable: true, get: () => pixels.get(el)!.h });
}
function resizeTo(el: HTMLElement, w: number, h: number) {
  setPixels(el, w, h);
  for (const o of observers) if (o.el === el) o.cb([], o.ro);
}

/** Cell size of the fake font: 8×16 px, 1 px less per cell at font size 16. */
function cellFor(fontSizePx: number) {
  return fontSizePx === 16 ? { w: 10, h: 20 } : { w: 8, h: 16 };
}

const DISPLAY = { fontSizePx: 13 };
let manager: TerminalManager;
let fontLoad: ReturnType<typeof vi.fn>;

/** Every display's fit measures its slot's pixels with the fake font. */
function stubMeasurement() {
  vi.spyOn(FitAddon.prototype, "proposeDimensions").mockImplementation(() => {
    const term = manager.terminal("s1");
    const el = term?.element?.parentElement?.parentElement as HTMLElement | undefined;
    const px = el && pixels.get(el);
    if (!term || !px) return undefined;
    const cell = cellFor(term.options.fontSize ?? 13);
    return { cols: Math.floor(px.w / cell.w), rows: Math.floor(px.h / cell.h) };
  });
}

/** Run the clock until the display is restored from its snapshot. */
async function restored() {
  for (let i = 0; i < 200 && !manager.terminal("s1"); i++) {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10);
    });
  }
  expect(manager.terminal("s1")).not.toBeNull();
}

function slotEl(container: HTMLElement): HTMLElement {
  return container.querySelector("[data-slot]") as HTMLElement;
}

async function settle() {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(RESIZE_SETTLE_MS + 1);
  });
}

beforeEach(() => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
  vi.stubGlobal("ResizeObserver", TestResizeObserver);
  fontLoad = vi.fn(async () => []);
  Object.defineProperty(document, "fonts", {
    configurable: true,
    value: { load: fontLoad },
  });
  observers.clear();
  transport.resizePty.mockClear();
  stubMeasurement();
  fake.bus = new FakeEventBus();
  fake.server = new FakeTerminalServer(fake.bus);
  fake.server.open("s1");
  manager = createTerminalManager({
    createWebgl: () => {
      throw new Error("no WebGL in tests");
    },
  });
});

afterEach(() => {
  manager.dispose("s1");
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

/** One slot of 800×480 px (100×30 at the fake font), measured and settled. */
async function mountMeasured(extra: Partial<Parameters<typeof TerminalSlot>[0]> = {}) {
  const view = render(
    <TerminalSlot sessionId="s1" display={DISPLAY} active manager={manager} data-slot {...extra} />,
  );
  const el = slotEl(view.container);
  setPixels(el, 800, 480);
  await restored();
  resizeTo(el, 800, 480);
  await settle();
  return { view, el };
}

/** Moves, detach/attach, a renderer swap, focus, and a non-owner slot
 *  resizing: counts the resizes they cause. */
async function sizeNeutralScenario() {
  const first = await mountMeasured();
  const term = manager.terminal("s1")!;
  expect(term.cols).toBe(100);
  transport.resizePty.mockClear();
  const termResize = vi.spyOn(term, "resize");

  // Move to a second slot of equal size.
  first.view.unmount();
  const second = render(
    <TerminalSlot sessionId="s1" display={DISPLAY} active manager={manager} data-slot />,
  );
  const el2 = slotEl(second.container);
  resizeTo(el2, 800, 480);
  await settle();
  // Detach / attach in place.
  manager.detach("s1", el2);
  manager.attach("s1", el2, { focus: false });
  await settle();
  // Hide and reveal (tab switch).
  second.rerender(
    <TerminalSlot sessionId="s1" display={DISPLAY} active={false} manager={manager} data-slot />,
  );
  second.rerender(
    <TerminalSlot sessionId="s1" display={DISPLAY} active manager={manager} data-slot />,
  );
  await settle();
  // Focus change and a renderer repaint.
  manager.focus("s1");
  term.refresh(0, term.rows - 1);
  // A second, non-owner slot for the same session resizing its div: it takes
  // the display, then the first owner resizes after losing it.
  const other = document.createElement("div");
  document.body.appendChild(other);
  setPixels(other, 400, 240);
  manager.attach("s1", other, { focus: false });
  resizeTo(el2, 1600, 960);
  await settle();
  manager.attach("s1", el2, { focus: false });
  setPixels(el2, 800, 480);
  await settle();
  other.remove();
  second.unmount();
  return {
    resizePty: transport.resizePty.mock.calls.length,
    termResize: termResize.mock.calls.length,
  };
}

describe("the slot is the one size owner", () => {
  it("no_resize_on_moves", async () => {
    expect(await sizeNeutralScenario()).toEqual({ resizePty: 0, termResize: 0 });
  });

  it("burst_coalesces_to_final", async () => {
    const { el } = await mountMeasured();
    transport.resizePty.mockClear();
    for (let i = 1; i <= 10; i++) {
      resizeTo(el, 800 + i * 8, 480 + i * 16);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(4);
      });
    }
    expect(transport.resizePty).not.toHaveBeenCalled();
    await settle();
    expect(transport.resizePty.mock.calls).toEqual([["s1", 110, 40]]);
    // The display follows on the stream's `Resized`: no second PTY resize.
    await settle();
    expect(manager.terminal("s1")!.cols).toBe(110);
    expect(transport.resizePty).toHaveBeenCalledTimes(1);

    // The end of a CSS transition applies at once.
    resizeTo(el, 640, 320);
    el.dispatchEvent(new Event("transitionend"));
    expect(transport.resizePty).toHaveBeenLastCalledWith("s1", 80, 20);
    await settle();
    expect(transport.resizePty).toHaveBeenCalledTimes(2);
  });

  it("a zero-size or unmeasured slot never resizes", async () => {
    const { el } = await mountMeasured();
    transport.resizePty.mockClear();
    resizeTo(el, 0, 0);
    await settle();
    vi.mocked(manager.fitAddon("s1")!.proposeDimensions).mockReturnValue(undefined);
    resizeTo(el, 900, 500);
    await settle();
    expect(transport.resizePty).not.toHaveBeenCalled();
  });

  it("a font-size change resizes once, keeps the bottom anchor and focus", async () => {
    const { view } = await mountMeasured();
    const term = manager.terminal("s1")!;
    let written = false;
    term.write(Array.from({ length: 100 }, (_, i) => `line ${i}\r\n`).join(""), () => {
      written = true;
    });
    while (!written) {
      await act(async () => {
        await vi.advanceTimersByTimeAsync(10);
      });
    }
    expect(term.buffer.active.baseY).toBeGreaterThan(0);
    manager.focus("s1");
    transport.resizePty.mockClear();
    view.rerender(
      <TerminalSlot
        sessionId="s1"
        display={{ ...DISPLAY, fontSizePx: 16 }}
        active
        manager={manager}
        data-slot
      />,
    );
    await settle();
    expect(transport.resizePty.mock.calls).toEqual([["s1", 80, 24]]);
    expect(term.buffer.active.viewportY).toBe(term.buffer.active.baseY);
    expect(document.activeElement).toBe(term.textarea);
  });

  it("waits for the terminal face (at most FONT_WAIT_MS) before the first measurement", async () => {
    fontLoad.mockImplementation(() => new Promise(() => {}));
    const onMeasured = vi.fn();
    const view = render(
      <TerminalSlot
        sessionId="s1"
        display={DISPLAY}
        active
        manager={manager}
        onMeasured={onMeasured}
        data-slot
      />,
    );
    const el = slotEl(view.container);
    await restored();
    resizeTo(el, 960, 480);
    await settle();
    expect(transport.resizePty).not.toHaveBeenCalled();
    expect(onMeasured).not.toHaveBeenCalled();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(FONT_WAIT_MS);
    });
    expect(onMeasured).toHaveBeenCalledTimes(1);
    expect(transport.resizePty.mock.calls).toEqual([["s1", 120, 30]]);
  });

  it("a resize queued behind output still reaches the PTY after the pane unmounts", async () => {
    const { view, el } = await mountMeasured();
    transport.resizePty.mockClear();
    fake.server.produce("s1", "pending\r\n");
    resizeTo(el, 960, 480);
    view.unmount();
    await settle();
    // The slot is gone before the settle: nothing asked.
    expect(transport.resizePty).not.toHaveBeenCalled();
    manager.requestResize("s1", 120, 30);
    expect(transport.resizePty.mock.calls).toEqual([["s1", 120, 30]]);
    await settle();
    expect(manager.terminal("s1")!.cols).toBe(120);
  });

  it("a respawned PTY (spawned at another pane's size) is brought to the slot's size", async () => {
    const { view } = await mountMeasured();
    fake.bus.emit("pty-exit", { session_id: "s1" });
    // The new process spawns at another pane's size.
    await fake.server.resize("s1", 80, 24);
    transport.resizePty.mockClear();
    view.rerender(
      <TerminalSlot
        sessionId="s1"
        display={{ ...DISPLAY, life: 1 }}
        active
        manager={manager}
        data-slot
      />,
    );
    await restored();
    await settle();
    expect(transport.resizePty.mock.calls).toEqual([["s1", 100, 30]]);
    expect(manager.terminal("s1")!.cols).toBe(100);
  });

  it("an exited session's PTY takes no resize", async () => {
    await mountMeasured();
    transport.resizePty.mockClear();
    fake.bus.emit("pty-exit", { session_id: "s1" });
    manager.requestResize("s1", 90, 20);
    expect(transport.resizePty).not.toHaveBeenCalled();
  });
});

describe("a slot that changed size is refit once its display can follow", () => {
  async function hide(view: ReturnType<typeof render>, el: HTMLElement) {
    view.rerender(
      <TerminalSlot sessionId="s1" display={DISPLAY} active={false} manager={manager} data-slot />,
    );
    resizeTo(el, 0, 0);
    await settle();
  }
  function reveal(view: ReturnType<typeof render>) {
    view.rerender(
      <TerminalSlot sessionId="s1" display={DISPLAY} active manager={manager} data-slot />,
    );
  }
  async function frame() {
    await act(async () => {
      await new Promise((resolve) => requestAnimationFrame(() => resolve(null)));
      await vi.advanceTimersByTimeAsync(10);
    });
  }

  it("a reveal at a new size (tile count, layout mode, agents pane, window) resizes", async () => {
    const { view, el } = await mountMeasured();
    await hide(view, el);
    transport.resizePty.mockClear();
    reveal(view);
    resizeTo(el, 1200, 600);
    await settle();
    expect(transport.resizePty.mock.calls).toEqual([["s1", 150, 37]]);
    await settle();
    expect(manager.terminal("s1")!.cols).toBe(150);
    expect(manager.terminal("s1")!.rows).toBe(37);
  });

  it("an evicted display revived after the slot's resize settled is brought to the slot's size", async () => {
    const { view, el } = await mountMeasured();
    await hide(view, el);
    (document.activeElement as HTMLElement | null)?.blur();
    expect(manager.evict("s1")).toBe(true);
    transport.resizePty.mockClear();
    // The rebuild is slower than the resize settle: the slot measures while
    // it has no display.
    fake.server.holdSubscribes = true;
    reveal(view);
    resizeTo(el, 1200, 600);
    await settle();
    expect(manager.terminal("s1")).toBeNull();
    expect(transport.resizePty).not.toHaveBeenCalled();
    fake.server.releaseSubscribes();
    await restored();
    await settle();
    expect(transport.resizePty.mock.calls).toEqual([["s1", 150, 37]]);
    expect(manager.terminal("s1")!.cols).toBe(150);
    expect(manager.terminal("s1")!.rows).toBe(37);
  });

  it("a reconnect's rebuilt display is brought to a size the slot took meanwhile", async () => {
    const { el } = await mountMeasured();
    transport.resizePty.mockClear();
    fake.server.holdSubscribes = true;
    fake.server.loseStreams(fake.server.boundStreams());
    resizeTo(el, 1200, 600);
    await settle();
    expect(transport.resizePty).not.toHaveBeenCalled();
    fake.server.releaseSubscribes();
    for (let i = 0; i < 50 && transport.resizePty.mock.calls.length === 0; i++) await frame();
    await settle();
    expect(transport.resizePty.mock.calls).toEqual([["s1", 150, 37]]);
    expect(manager.terminal("s1")!.cols).toBe(150);
  });

  function refuseEveryResize() {
    const answer = transport.resizePty.getMockImplementation();
    transport.resizePty.mockImplementation(() =>
      Promise.reject(new Error("terminal is held by its owner")),
    );
    onTestFinished(() => {
      transport.resizePty.mockImplementation(answer!);
    });
  }

  it("a refused resize is not re-sent for the same measured size, only for a new one", async () => {
    const { el } = await mountMeasured();
    transport.resizePty.mockClear();
    // A protected remote session refuses a non-owner every time.
    refuseEveryResize();
    resizeTo(el, 1200, 600);
    await settle();
    expect(transport.resizePty.mock.calls).toEqual([["s1", 150, 37]]);
    expect(manager.terminal("s1")!.cols).toBe(100);
    expect(manager.size("s1")).toEqual({ cols: 100, rows: 30 });
    // The same pixels measured again (a pane toggled back, a window nudge,
    // a font settle): nothing is re-sent.
    for (let i = 0; i < 3; i++) {
      resizeTo(el, 1200, 600);
      await settle();
    }
    expect(transport.resizePty).toHaveBeenCalledTimes(1);
    // A new measured size is asked.
    resizeTo(el, 960, 480);
    await settle();
    expect(transport.resizePty.mock.calls).toEqual([
      ["s1", 150, 37],
      ["s1", 120, 30],
    ]);
    // And the first size again, now that the measurement changed.
    resizeTo(el, 1200, 600);
    await settle();
    expect(transport.resizePty).toHaveBeenCalledTimes(3);
    resizeTo(el, 1200, 600);
    await settle();
    expect(transport.resizePty).toHaveBeenCalledTimes(3);
    expect(manager.terminal("s1")!.cols).toBe(100);
  });

  it("a resize that failed to send is retried once on the next reveal", async () => {
    const { view, el } = await mountMeasured();
    transport.resizePty.mockClear();
    transport.resizePty.mockRejectedValueOnce(new Error("session server write backpressure"));
    resizeTo(el, 1200, 600);
    await settle();
    resizeTo(el, 1200, 600);
    await settle();
    expect(transport.resizePty).toHaveBeenCalledTimes(1);
    expect(manager.terminal("s1")!.cols).toBe(100);
    await hide(view, el);
    reveal(view);
    resizeTo(el, 1200, 600);
    await settle();
    expect(transport.resizePty.mock.calls).toEqual([
      ["s1", 150, 37],
      ["s1", 150, 37],
    ]);
    await settle();
    expect(manager.terminal("s1")!.cols).toBe(150);
  });

  it("a reveal retries a refused size once, not on every later measurement", async () => {
    const { view, el } = await mountMeasured();
    transport.resizePty.mockClear();
    refuseEveryResize();
    resizeTo(el, 1200, 600);
    await settle();
    await hide(view, el);
    reveal(view);
    resizeTo(el, 1200, 600);
    await settle();
    expect(transport.resizePty).toHaveBeenCalledTimes(2);
    for (let i = 0; i < 3; i++) {
      resizeTo(el, 1200, 600);
      await settle();
    }
    expect(transport.resizePty).toHaveBeenCalledTimes(2);
    expect(manager.terminal("s1")!.cols).toBe(100);
  });

  it("a resize that failed to send is retried once a reconnect places the display", async () => {
    const { el } = await mountMeasured();
    transport.resizePty.mockClear();
    transport.resizePty.mockRejectedValueOnce(new Error("session server write backpressure"));
    resizeTo(el, 1200, 600);
    await settle();
    expect(transport.resizePty).toHaveBeenCalledTimes(1);
    fake.server.loseStreams(fake.server.boundStreams());
    for (let i = 0; i < 50 && transport.resizePty.mock.calls.length < 2; i++) await frame();
    await settle();
    expect(transport.resizePty.mock.calls).toEqual([
      ["s1", 150, 37],
      ["s1", 150, 37],
    ]);
    expect(manager.terminal("s1")!.cols).toBe(150);
  });
});

describe("the first frame after a restore", () => {
  async function mountRestoring(answer: boolean) {
    if (!answer) transport.resizePty.mockImplementation(async () => {});
    const onEnsured = vi.fn();
    const view = render(
      <TerminalSlot
        sessionId="s1"
        display={DISPLAY}
        active
        manager={manager}
        onEnsured={onEnsured}
        data-slot
      />,
    );
    // The slot holds 120×30; the session was recorded at 100×30.
    setPixels(slotEl(view.container), 960, 480);
    await restored();
    return onEnsured;
  }

  it("is reported once the display is at the slot's size", async () => {
    const onEnsured = await mountRestoring(true);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10);
    });
    expect(transport.resizePty.mock.calls).toEqual([["s1", 120, 30]]);
    expect(onEnsured).toHaveBeenCalledTimes(1);
    expect(manager.terminal("s1")!.cols).toBe(120);
  });

  it("is reported anyway after SIZE_SETTLE_BOUND_MS when no Resized comes", async () => {
    const onEnsured = await mountRestoring(false);
    expect(onEnsured).not.toHaveBeenCalled();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(SIZE_SETTLE_BOUND_MS);
    });
    expect(onEnsured).toHaveBeenCalledTimes(1);
    expect(manager.terminal("s1")!.cols).toBe(100);
  });
});

describe("teeth", () => {
  it("a fit on attach (the deleted reveal fit) makes no_resize_on_moves fail", async () => {
    const attach = manager.attach.bind(manager);
    manager.attach = (sessionId, slot, opts) => {
      attach(sessionId, slot, opts);
      const fit = manager.fitAddon(sessionId);
      if (fit && pixels.get(slot)) {
        const dims = fit.proposeDimensions();
        if (dims?.cols && dims.rows) manager.terminal(sessionId)!.resize(dims.cols, dims.rows);
      }
    };
    const counts = await sizeNeutralScenario();
    expect(counts.termResize).toBeGreaterThan(0);
  });
});
