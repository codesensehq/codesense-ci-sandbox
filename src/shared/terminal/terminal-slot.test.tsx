// SPDX-License-Identifier: MIT
import { StrictMode, useState } from "react";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { TerminalFindBar } from "@/features/viewer/components/terminal-find-bar";
import { FakeEventBus, FakeTerminalServer } from "./__test-utils__/fake-terminal-server";
import { createTerminalManager, type TerminalManager } from "./terminal-manager";
import { TerminalSlot, computeFitTarget } from "./terminal-slot";

const counts = vi.hoisted(() => ({ created: 0, disposed: 0 }));
vi.mock("@xterm/xterm", async (importOriginal) => {
  const real = await importOriginal<typeof import("@xterm/xterm")>();
  class CountedTerminal extends real.Terminal {
    constructor(...args: ConstructorParameters<typeof real.Terminal>) {
      super(...args);
      counts.created++;
    }
    override dispose(): void {
      counts.disposed++;
      super.dispose();
    }
  }
  return { ...real, Terminal: CountedTerminal };
});

const fake = vi.hoisted(() => ({
  bus: null as unknown as import("./__test-utils__/fake-terminal-server").FakeEventBus,
  server: null as unknown as import("./__test-utils__/fake-terminal-server").FakeTerminalServer,
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, handler: (event: { payload: unknown }) => void) =>
    fake.bus.listen(name, handler),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));
vi.mock(
  "@/features/viewer/lib/cli-workspace/transport/terminal-session-source",
  async (importOriginal) => ({
    ...(await importOriginal<
      typeof import("@/features/viewer/lib/cli-workspace/transport/terminal-session-source")
    >()),
    reserveTerminalStream: () => fake.server.reserve(),
    subscribeTerminal: (session: string, stream?: number) => fake.server.subscribe(session, stream),
    unsubscribeTerminal: (stream: number) => fake.server.unsubscribe(stream),
    writePty: vi.fn(async () => {}),
  }),
);

const DISPLAY = { fontSizePx: 13 };
let manager: TerminalManager;

beforeEach(() => {
  counts.created = 0;
  counts.disposed = 0;
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
});

/** The display is restored from its snapshot. */
async function restored() {
  await act(async () => {
    await vi.waitFor(() => expect(manager.terminal("s1")).not.toBeNull());
  });
}

/** Parse everything written so far. */
async function parsed() {
  await act(async () => {
    await manager.debugDump("s1");
  });
}

describe("a reconnect's outcome reaches the slot", () => {
  it("a rebuild that gives up after a reconnect is reported through onEnsured", async () => {
    const onEnsured = vi.fn();
    render(
      <TerminalSlot
        sessionId="s1"
        display={DISPLAY}
        active
        manager={manager}
        onEnsured={onEnsured}
      />,
    );
    await restored();
    await act(async () => {
      await vi.waitFor(() => expect(onEnsured).toHaveBeenCalledTimes(1));
    });
    fake.server.failSubscribes =
      "restart terminal server (ends 1 session): it speaks protocol 6, this app 7";
    fake.server.loseStreams(fake.server.boundStreams());
    await act(async () => {
      await vi.waitFor(() => expect(onEnsured).toHaveBeenCalledTimes(2));
    });
    expect(onEnsured.mock.calls[1][0].outcome.kind).toBe("incompatible");
  });
});

describe("a slot shown again takes its display back", () => {
  it("does not request focus while mounted as a hidden warm pane", async () => {
    const focus = vi.spyOn(manager, "focus");
    const policy = vi.fn(() => false);
    const view = render(
      <div style={{ display: "none" }} inert aria-hidden>
        <TerminalSlot
          sessionId="s1"
          display={DISPLAY}
          active={false}
          focusOnReveal={policy}
          manager={manager}
        />
      </div>,
    );
    await restored();
    expect(focus).not.toHaveBeenCalled();
    expect(manager.holdsVisible("s1")).toBe(false);
    view.rerender(
      <div>
        <TerminalSlot
          sessionId="s1"
          display={DISPLAY}
          active
          focusOnReveal={policy}
          manager={manager}
        />
      </div>,
    );
    // The manager receives the focus permission and checks it; the policy refuses.
    for (const [, mayFocus] of focus.mock.calls) {
      expect(typeof mayFocus === "function" ? mayFocus() : mayFocus).toBe(false);
    }
    expect(policy).toHaveBeenCalled();
  });
  it("when the quick view borrowed the display while this one was hidden", async () => {
    const view = render(<TerminalSlot sessionId="s1" display={DISPLAY} active manager={manager} />);
    await act(async () => {});
    view.rerender(
      <TerminalSlot sessionId="s1" display={DISPLAY} active={false} manager={manager} />,
    );
    const popup = document.createElement("div");
    document.body.appendChild(popup);
    manager.attach("s1", popup, { focus: false, borrow: true });
    const attach = vi.spyOn(manager, "attach");
    view.rerender(<TerminalSlot sessionId="s1" display={DISPLAY} active manager={manager} />);
    expect(attach).toHaveBeenCalledWith("s1", expect.any(HTMLElement), expect.anything());
    expect(attach.mock.calls[0][1]).not.toBe(popup);
    attach.mockRestore();
    popup.remove();
  });
});

describe("a borrowing slot shown again", () => {
  it("borrows again: closing it gives the display back to the slot it came from", async () => {
    const popup = render(
      <TerminalSlot sessionId="s1" display={DISPLAY} active borrow manager={manager} />,
    );
    await restored();
    popup.rerender(
      <TerminalSlot sessionId="s1" display={DISPLAY} active={false} borrow manager={manager} />,
    );
    // A tile takes the display while the popup is hidden, and is hidden in turn.
    const tile = document.createElement("div");
    document.body.appendChild(tile);
    manager.attach("s1", tile, { focus: false });
    manager.setVisible("s1", tile, false);
    popup.rerender(
      <TerminalSlot sessionId="s1" display={DISPLAY} active borrow manager={manager} />,
    );
    expect(manager.holds("s1", tile)).toBe(false);
    popup.unmount();
    expect(manager.holds("s1", tile)).toBe(true);
    tile.remove();
  });
});

describe("reveals: focus and repaint", () => {
  it.each([false, true])(
    "a pending slot restore honors revoked focus permission (focusOnMount=%s)",
    async (focusOnMount) => {
      fake.server.holdSubscribes = true;
      const subscribe = vi.spyOn(fake.server, "subscribe");
      const policy = vi.fn(() => true);
      const view = render(
        <TerminalSlot
          sessionId="s1"
          display={DISPLAY}
          active
          focusOnMount={focusOnMount}
          focusOnReveal={policy}
          manager={manager}
        />,
      );
      await vi.waitFor(() => expect(subscribe).toHaveBeenCalled());
      expect(manager.terminal("s1")).toBeNull();
      expect(policy).not.toHaveBeenCalled();
      view.rerender(
        <TerminalSlot
          sessionId="s1"
          display={DISPLAY}
          active
          focusOnMount={focusOnMount}
          focusOnReveal={() => false}
          manager={manager}
        />,
      );
      fake.server.releaseSubscribes();
      await restored();
      expect(document.activeElement).not.toBe(manager.terminal("s1")!.textarea);
    },
  );

  it.each([false, true])(
    "a slot hidden during restore leaves outside focus intact (focusOnMount=%s)",
    async (focusOnMount) => {
      fake.server.holdSubscribes = true;
      const subscribe = vi.spyOn(fake.server, "subscribe");
      const view = render(
        <>
          <input aria-label="Chosen document" />
          <TerminalSlot
            sessionId="s1"
            display={DISPLAY}
            active
            focusOnMount={focusOnMount}
            manager={manager}
          />
        </>,
      );
      await vi.waitFor(() => expect(subscribe).toHaveBeenCalled());
      const outside = view.getByRole("textbox", { name: "Chosen document" });
      outside.focus();
      view.rerender(
        <>
          <input aria-label="Chosen document" />
          <TerminalSlot
            sessionId="s1"
            display={DISPLAY}
            active={false}
            focusOnMount={focusOnMount}
            manager={manager}
          />
        </>,
      );
      fake.server.releaseSubscribes();
      await restored();
      expect(manager.terminal("s1")).not.toBeNull();
      expect(document.activeElement).toBe(outside);
    },
  );

  it("focusOnMount: a slot mounted active takes focus (the quick view over a shown terminal)", async () => {
    render(<TerminalSlot sessionId="s1" display={DISPLAY} active focusOnMount manager={manager} />);
    await restored();
    const focus = vi.spyOn(manager, "focus");
    render(<TerminalSlot sessionId="s1" display={DISPLAY} active focusOnMount manager={manager} />);
    expect(focus).toHaveBeenCalledWith("s1", expect.any(Function));
    focus.mockRestore();
  });

  it("without focusOnMount, a slot mounted active leaves focus where it is", async () => {
    const focus = vi.spyOn(manager, "focus");
    render(<TerminalSlot sessionId="s1" display={DISPLAY} active manager={manager} />);
    expect(focus).not.toHaveBeenCalled();
    focus.mockRestore();
  });

  it("repaints once shown again after being hidden, never on the first mount", async () => {
    const repaint = vi.spyOn(manager, "repaint");
    const frames: FrameRequestCallback[] = [];
    const raf = vi
      .spyOn(window, "requestAnimationFrame")
      .mockImplementation((cb) => frames.push(cb));
    const view = render(<TerminalSlot sessionId="s1" display={DISPLAY} active manager={manager} />);
    await restored();
    frames.splice(0).forEach((cb) => cb(0));
    expect(repaint).not.toHaveBeenCalled();
    view.rerender(
      <TerminalSlot sessionId="s1" display={DISPLAY} active={false} manager={manager} />,
    );
    view.rerender(<TerminalSlot sessionId="s1" display={DISPLAY} active manager={manager} />);
    frames.splice(0).forEach((cb) => cb(0));
    expect(repaint).toHaveBeenCalledTimes(1);
    raf.mockRestore();
    repaint.mockRestore();
  });
});

describe("TerminalSlot owns a slot, never a terminal (I3)", () => {
  it("render, unmount, remount in StrictMode and under a second parent: one Terminal, no dispose", async () => {
    const slot = <TerminalSlot sessionId="s1" display={DISPLAY} active manager={manager} />;
    const first = render(<StrictMode>{slot}</StrictMode>);
    await act(async () => {});
    expect(counts.created).toBe(1);
    const element = manager.terminal("s1")!.element!;
    first.unmount();
    const second = render(
      <StrictMode>
        <section>
          <div>{slot}</div>
        </section>
      </StrictMode>,
    );
    await act(async () => {});
    expect(second.container.querySelector("section")!.contains(element)).toBe(true);
    second.unmount();
    render(slot);
    await act(async () => {});
    expect(counts.created).toBe(1);
    expect(counts.disposed).toBe(0);
    expect(manager.terminal("s1")!.element).toBe(element);
  });

  it("output written while no slot is mounted is on screen at once when one mounts", async () => {
    const view = render(<TerminalSlot sessionId="s1" display={DISPLAY} active manager={manager} />);
    await restored();
    view.unmount();
    fake.server.produce("s1", "while parked\r\n");
    await parsed();
    render(<TerminalSlot sessionId="s1" display={DISPLAY} active manager={manager} />);
    // Attached in a layout effect: before the browser paints the first frame.
    const view2 = document.querySelector(".cs-terminal-display");
    expect(view2?.parentElement?.id).not.toBe("cs-terminal-parking");
    expect(manager.terminal("s1")!.buffer.active.getLine(0)?.translateToString(true)).toBe(
      "while parked",
    );
    expect(document.body.contains(manager.terminal("s1")!.element!)).toBe(true);
  });

  it("an inactive slot does not take focus; becoming active does", async () => {
    const outside = document.createElement("input");
    document.body.appendChild(outside);
    outside.focus();
    const view = render(
      <TerminalSlot sessionId="s1" display={DISPLAY} active={false} manager={manager} />,
    );
    await act(async () => {});
    expect(document.activeElement).toBe(outside);
    view.rerender(<TerminalSlot sessionId="s1" display={DISPLAY} active manager={manager} />);
    expect(document.activeElement).toBe(manager.terminal("s1")!.textarea);
  });
});

/** The panel's find wiring: the bar's state and the search addon belong to the
 *  session's display, not the pane. */
function Pane({ shown }: { shown: boolean }) {
  return shown ? (
    <div data-terminal-session-id="s1">
      <TerminalFindBar
        sessionId="s1"
        getSearch={() => manager.search("s1")}
        onClear={() => false}
        onClose={() => manager.focus("s1")}
        initialState={manager.findState("s1")}
        onStateChange={(state) => manager.setFindState("s1", state)}
      />
      <TerminalSlot sessionId="s1" display={DISPLAY} active manager={manager} />
    </div>
  ) : null;
}

function Toggle() {
  const [shown, setShown] = useState(true);
  return (
    <>
      <button type="button" onClick={() => setShown((s) => !s)}>
        toggle
      </button>
      <Pane shown={shown} />
    </>
  );
}

describe("selection and find survive the pane (AC-9)", () => {
  it("an open find bar, its match and a selection come back after hide→show; closing clears", async () => {
    render(<Toggle />);
    await act(async () => {});
    fake.server.produce("s1", "alpha error one\r\nbeta\r\ngamma error two\r\n");
    await parsed();
    // A bar the session already had open (⌘F itself is covered by the
    // terminal-tab-keys seam suite).
    manager.setFindState("s1", {
      query: "",
      caseSensitive: false,
      regex: false,
      wholeWord: false,
      resultIndex: -1,
      resultCount: 0,
    });
    fireEvent.click(screen.getByText("toggle"));
    fireEvent.click(screen.getByText("toggle"));
    await act(async () => {});
    const input = screen.getByRole("textbox", { name: "Find in terminal" });
    fireEvent.change(input, { target: { value: "error" } });
    const term = manager.terminal("s1")!;
    term.select(0, 1, 4);
    const selection = term.getSelection();
    expect(selection).toBe("beta");
    const findBefore = manager.findState("s1");
    expect(findBefore).toMatchObject({ query: "error" });

    fireEvent.click(screen.getByText("toggle")); // hide: the pane unmounts
    expect(screen.queryByTestId("terminal-find-bar")).toBeNull();
    fireEvent.click(screen.getByText("toggle")); // show: a new pane mounts
    await act(async () => {});

    expect(screen.getByRole("textbox", { name: "Find in terminal" })).toHaveValue("error");
    expect(manager.findState("s1")).toEqual(findBefore);
    expect(manager.terminal("s1")).toBe(term);
    expect(term.getSelection()).toBe(selection);

    const clear = vi.spyOn(manager.search("s1")!, "clearDecorations");
    fireEvent.click(screen.getByRole("button", { name: "Close find" }));
    expect(clear).toHaveBeenCalled();
    expect(manager.findState("s1")).toBeNull();
    fireEvent.click(screen.getByText("toggle"));
    fireEvent.click(screen.getByText("toggle"));
    expect(screen.queryByTestId("terminal-find-bar")).toBeNull();
  });
});

describe("computeFitTarget", () => {
  const box = (w: number, h: number) => ({ clientWidth: w, clientHeight: h });
  const proposing = (cols?: number, rows?: number) => ({
    proposeDimensions: () =>
      cols === undefined && rows === undefined ? undefined : { cols, rows },
  });

  it("returns the proposed size for a measurable surface", () => {
    expect(computeFitTarget(box(800, 600), proposing(120, 30))).toEqual({ cols: 120, rows: 30 });
  });

  it("refuses a hidden or sliver surface, an unmeasured cell, and non-integer sizes", () => {
    expect(computeFitTarget(box(0, 600), proposing(120, 30))).toBeNull();
    expect(computeFitTarget(box(2, 600), proposing(120, 30))).toBeNull();
    expect(computeFitTarget(box(800, 0), proposing(120, 30))).toBeNull();
    expect(computeFitTarget(box(800, 600), proposing())).toBeNull();
    expect(
      computeFitTarget(box(800, 600), {
        proposeDimensions: () => {
          throw new Error("no renderer");
        },
      }),
    ).toBeNull();
    for (const [cols, rows] of [
      [0, 30],
      [Number.NaN, 30],
      [120, -5],
      [120, Infinity],
      [120.5, 30],
    ]) {
      expect(computeFitTarget(box(800, 600), proposing(cols, rows))).toBeNull();
    }
  });
});

it("fits after a held remote attach moves the display out of hidden parking", async () => {
  const local = await manager.ensure("s1", DISPLAY);
  expect(local).toBeDefined();
  let finish!: (handle: NonNullable<typeof local>) => void;
  vi.spyOn(manager, "ensure").mockImplementation(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const term = manager.terminal("s1")!;
  const fit = manager.fitAddon("s1")!;
  let attached = false;
  vi.spyOn(manager, "terminal").mockReturnValue(term);
  vi.spyOn(manager, "fitAddon").mockReturnValue(fit);
  vi.spyOn(fit, "proposeDimensions").mockImplementation(() =>
    attached ? { cols: 120, rows: 40 } : undefined,
  );
  vi.spyOn(manager, "attach").mockImplementation(() => {
    attached = true;
  });
  vi.spyOn(manager, "holds").mockImplementation(() => attached);
  vi.spyOn(manager, "detach").mockImplementation(() => {});
  vi.spyOn(manager, "setVisible").mockImplementation(() => {});
  vi.spyOn(manager, "setAppearance").mockImplementation(() => {});
  const resize = vi.spyOn(manager, "requestResize").mockImplementation(() => {});
  const remote = {
    teamKey: "acme:core",
    workspaceKey: "acme:core|root",
    privateAuthorityEpoch: "1",
    target: {
      member: { machineId: "host", slot: "remote" as const },
      admissionRevision: "1",
      namespace: { serverId: "host", epoch: "epoch", generation: "1" },
      session: "s1",
    },
  };
  const view = render(
    <TerminalSlot sessionId={remote} display={DISPLAY} active manager={manager} />,
  );
  const el = view.container.firstElementChild!;
  Object.defineProperties(el, { clientWidth: { value: 800 }, clientHeight: { value: 600 } });
  vi.spyOn(el, "getBoundingClientRect").mockReturnValue({ width: 800, height: 600 } as DOMRect);
  expect(resize).not.toHaveBeenCalled();
  await act(async () => finish(local!));
  expect(resize).toHaveBeenCalledWith(remote, 120, 40);
  vi.restoreAllMocks();
});

describe("the slot paints its display's background (no strip below the last row)", () => {
  // A fit rounds down to whole cells, so the slot is taller (and wider) than
  // its rows (and cols) by up to one cell. That remainder must be the
  // terminal's background, not whatever is behind the slot: a remote pane's
  // tinted theme showed a near-black strip under the last row there.
  const themed = (background: string) => ({
    fontSizePx: 13,
    themeConfig: { chromeBg: "#161b22", theme: { background } },
  });

  it("fills the slot with the theme background, and follows a theme change", async () => {
    const { container, rerender } = render(
      <TerminalSlot sessionId="s1" display={themed("#262a3d")} active manager={manager} />,
    );
    await restored();
    const slot = container.firstElementChild as HTMLElement;
    expect(slot.style.backgroundColor).toBe("#262a3d");
    rerender(<TerminalSlot sessionId="s1" display={themed("#0d1117")} active manager={manager} />);
    expect(slot.style.backgroundColor).toBe("#0d1117");
  });

  it("keeps a caller's own style, which may set its own background", async () => {
    const { container } = render(
      <TerminalSlot
        sessionId="s1"
        display={themed("#262a3d")}
        active
        manager={manager}
        style={{ overflow: "hidden", backgroundColor: "#000000" }}
      />,
    );
    await restored();
    const slot = container.firstElementChild as HTMLElement;
    expect(slot.style.overflow).toBe("hidden");
    expect(slot.style.backgroundColor).toBe("#000000");
  });
});
