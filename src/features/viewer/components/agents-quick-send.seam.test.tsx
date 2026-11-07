// SPDX-License-Identifier: MIT
/**
 * Seam test: the quick view and the CLI's tiles share one display per session through the real
 * terminal manager and real `TerminalSlot`s. Only the terminal server is faked. The popup never
 * takes a terminal a shown tile holds (that tile would go blank), and borrows one the CLI does
 * not show, including every terminal while the CLI column is hidden.
 */
import { act, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { QuickSendPopover } from "@/features/viewer/components/agents-quick-send";
import {
  FakeEventBus,
  FakeTerminalServer,
} from "@/shared/terminal/__test-utils__/fake-terminal-server";
import { createTerminalManager, type TerminalManager } from "@/shared/terminal/terminal-manager";
import { TerminalSlot } from "@/shared/terminal/terminal-slot";

const fake = vi.hoisted(() => ({
  bus: null as unknown as import("@/shared/terminal/__test-utils__/fake-terminal-server").FakeEventBus,
  server:
    null as unknown as import("@/shared/terminal/__test-utils__/fake-terminal-server").FakeTerminalServer,
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
  document.body.innerHTML = "";
});

/** Whether a tile's reveal may take focus (the app's rule says no while focus is in a popup). */
let tileTakesFocus = true;

/** A CLI tile showing session s1, shown or hidden (the CLI column, or another tab in front). */
function Tile({ shown }: { shown: boolean }) {
  return (
    <TerminalSlot
      data-testid="tile"
      sessionId="s1"
      display={DISPLAY}
      active={shown}
      focusOnReveal={() => tileTakesFocus}
      manager={manager}
    />
  );
}

function Popup({ open }: { open: boolean }) {
  const anchor = document.getElementById("row") as HTMLElement;
  return open ? (
    <QuickSendPopover
      name="Worker"
      statusWord="Working"
      workspace={null}
      sessionId="s1"
      anchor={anchor}
      onClose={() => {}}
      onShowInCli={() => {}}
      manager={manager}
    />
  ) : null;
}

async function mount(tileShown: boolean, popupOpen = true) {
  const row = document.createElement("button");
  row.id = "row";
  document.body.appendChild(row);
  const view = render(
    <>
      <Tile shown={tileShown} />
      <Popup open={popupOpen} />
    </>,
  );
  await act(async () => {
    await vi.waitFor(() => expect(manager.terminal("s1")).not.toBeNull());
  });
  return {
    tile: () => screen.getByTestId("tile"),
    popupSlot: () =>
      document.querySelector<HTMLElement>(".ag-quick-send-terminal > div") ?? undefined,
    rerender: (shown: boolean, open = true) =>
      view.rerender(
        <>
          <Tile shown={shown} />
          <Popup open={open} />
        </>,
      ),
  };
}

describe("the quick view and the CLI's tiles share one display", () => {
  it("a session shown in a tile (focused or not) is not borrowed: the popup links to it", async () => {
    const { tile } = await mount(true);
    expect(screen.getByText("Worker's terminal is on screen in the CLI.")).toBeInTheDocument();
    expect(document.querySelector(".ag-quick-send-terminal")).toBeNull();
    expect(manager.holds("s1", tile())).toBe(true);
  });

  it("with the CLI column hidden, the popup shows the live terminal and gives it back on close", async () => {
    const { tile, popupSlot, rerender } = await mount(false);
    expect(popupSlot()).toBeDefined();
    expect(manager.holds("s1", popupSlot()!)).toBe(true);
    act(() => rerender(false, false));
    expect(manager.holds("s1", tile())).toBe(true);
  });

  it("a tile shown while the popup has its terminal takes it back, and the popup links to it", async () => {
    const { tile, rerender } = await mount(false);
    expect(document.querySelector(".ag-quick-send-terminal")).not.toBeNull();
    act(() => rerender(true));
    expect(manager.holds("s1", tile())).toBe(true);
    expect(document.querySelector(".ag-quick-send-terminal")).toBeNull();
    expect(screen.getByRole("button", { name: "Show in CLI" })).toBeInTheDocument();
  });

  const nextFrame = () =>
    act(async () => {
      await new Promise((resolve) => requestAnimationFrame(() => resolve(null)));
    });

  it("a terminal typed into here that a shown tile takes keeps focus in that tile, and says it moved", async () => {
    const { tile, rerender } = await mount(false);
    manager.terminal("s1")!.textarea!.focus();
    act(() => rerender(true));
    await nextFrame();
    expect(tile().contains(document.activeElement)).toBe(true);
    expect(screen.getByRole("button", { name: "Show in CLI" })).not.toHaveFocus();
    expect(screen.getByRole("status")).toHaveTextContent(
      "Worker's terminal moved to its tile in the CLI.",
    );
  });

  it("with focus on the popup's own controls, the way to the tile takes it on the next frame", async () => {
    const { rerender } = await mount(false);
    tileTakesFocus = false;
    try {
      screen.getByRole("button", { name: "Close" }).focus();
      act(() => rerender(true));
      const show = screen.getByRole("button", { name: "Show in CLI" });
      // Not at once: a key on its way to the terminal must not press the button.
      expect(show).not.toHaveFocus();
      await nextFrame();
      expect(show).toHaveFocus();
    } finally {
      tileTakesFocus = true;
    }
  });
});
