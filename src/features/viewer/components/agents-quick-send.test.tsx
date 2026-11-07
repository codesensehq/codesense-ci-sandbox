// SPDX-License-Identifier: MIT
import { describe, expect, it, vi, beforeEach } from "vitest";
import { act, fireEvent, render, screen } from "@/test-utils";

import {
  QuickSendPopover,
  type QuickSendProps,
} from "@/features/viewer/components/agents-quick-send";
import type { TerminalManager } from "@/shared/terminal/terminal-manager";

vi.mock("@/shared/terminal/terminal-slot", () => ({
  TerminalSlot: ({ sessionId }: { sessionId: string }) => (
    <div data-testid="quick-send-terminal" data-session={sessionId} />
  ),
}));

function setup(over: Partial<QuickSendProps> = {}) {
  const anchor = document.createElement("button");
  document.body.appendChild(anchor);
  const props: QuickSendProps = {
    name: "Worker",
    statusWord: "Ready",
    workspace: "Auth flow",
    sessionId: "sid-worker",
    anchor,
    onClose: vi.fn(),
    onShowInCli: vi.fn(),
    ...over,
  };
  render(<QuickSendPopover {...props} />);
  return props;
}

describe("QuickSendPopover", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    localStorage.clear();
  });

  it("attaches the agent's live terminal when its tab is not on screen", () => {
    setup();
    expect(screen.getByTestId("quick-send-terminal")).toHaveAttribute("data-session", "sid-worker");
    expect(screen.queryByRole("button", { name: "Show in CLI" })).toBeNull();
  });

  it("when its terminal is on screen in the CLI, links to it instead of moving the terminal", () => {
    const onScreen = {
      holdsVisible: () => true,
      watchShown: () => () => {},
    } as unknown as TerminalManager;
    const props = setup({ manager: onScreen });
    expect(screen.queryByTestId("quick-send-terminal")).toBeNull();
    // The way to it takes focus, and no typing hint shows (there is nothing here to type into).
    const show = screen.getByRole("button", { name: "Show in CLI" });
    expect(show).toHaveFocus();
    expect(screen.queryByText(/Typing goes to/)).toBeNull();
    fireEvent.click(show);
    expect(props.onShowInCli).toHaveBeenCalled();
  });

  it("is labelled as a quick view, and says where typing goes while the terminal is here", () => {
    setup();
    expect(screen.getByRole("dialog", { name: "Quick view of Worker" })).toBeInTheDocument();
    expect(screen.getByText(/Click the terminal to type to Worker.*⌘W closes/)).toBeInTheDocument();
  });

  it("keeps Open tab and Close in the header before the terminal", () => {
    const props = setup({ nativeSubagents: 3 });
    const header = screen.getByRole("button", { name: "Open tab" }).parentElement!;
    expect(header).toContainElement(screen.getByRole("button", { name: "Close" }));
    expect(header.querySelector(".ag-face")).toBeInTheDocument();
    expect(screen.getByRole("img", { name: "3 subagents running" })).toBeInTheDocument();
    expect(
      header.compareDocumentPosition(screen.getByTestId("quick-send-terminal")) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Open tab" }));
    expect(props.onShowInCli).toHaveBeenCalledOnce();
  });

  it("changes the typing hint with terminal focus and leaves Escape to the CLI", () => {
    const props = setup();
    const terminal = screen.getByTestId("quick-send-terminal");
    terminal.tabIndex = 0;
    act(() => terminal.focus());
    expect(
      screen.getByText(/Typing to Worker · Esc goes to Worker · ⌘W closes/),
    ).toBeInTheDocument();
    fireEvent.keyDown(terminal, { key: "Escape" });
    expect(props.onClose).not.toHaveBeenCalled();
    act(() => screen.getByRole("button", { name: "Close" }).focus());
    expect(screen.getByText(/Click the terminal to type to Worker/)).toBeInTheDocument();
  });

  it("fits the measured chrome above the host's tab strip", () => {
    const rect = (top: number, height: number, left = 0, width = 900) => ({
      top,
      bottom: top + height,
      left,
      right: left + width,
      width,
      height,
      x: left,
      y: top,
      toJSON: () => ({}),
    });
    const spy = vi
      .spyOn(HTMLElement.prototype, "getBoundingClientRect")
      .mockImplementation(function (this: HTMLElement) {
        if (this.matches('[role="dialog"]'))
          return rect(
            0,
            parseFloat(
              this.querySelector<HTMLElement>(".ag-quick-send-terminal")?.style.height ?? "0",
            ) + 94,
          );
        if (this.matches(".ag-quick-send-terminal")) return rect(0, parseFloat(this.style.height));
        return rect(440, 28, 100, 120);
      });
    const heightSpy = vi
      .spyOn(HTMLElement.prototype, "offsetHeight", "get")
      .mockImplementation(function (this: HTMLElement) {
        if (this.matches('[role="dialog"]'))
          return (
            parseFloat(
              this.querySelector<HTMLElement>(".ag-quick-send-terminal")?.style.height ?? "0",
            ) + 94
          );
        return this.matches(".ag-quick-send-terminal") ? parseFloat(this.style.height) : 0;
      });
    try {
      setup({ bounds: () => rect(48, 452, 20, 900) });
      const dialog = screen.getByRole("dialog");
      expect(dialog.style.top).toBe("56px");
      expect(
        parseFloat(dialog.style.top) +
          parseFloat(screen.getByTestId("quick-send-terminal").parentElement!.style.height) +
          94,
      ).toBeLessThanOrEqual(492);
      expect(parseFloat(dialog.style.left)).toBeGreaterThanOrEqual(28);
    } finally {
      heightSpy.mockRestore();
      spy.mockRestore();
    }
  });

  it("⌘W anywhere in it closes it and asks for focus to return to the row", () => {
    const props = setup();
    const terminal = screen.getByTestId("quick-send-terminal");
    const key = new KeyboardEvent("keydown", {
      key: "w",
      code: "KeyW",
      metaKey: true,
      bubbles: true,
      cancelable: true,
    });
    terminal.dispatchEvent(key);
    expect(props.onClose).toHaveBeenCalledWith(true);
    expect(key.defaultPrevented).toBe(true);
  });

  it("⌘W is read by its letter, and by W's place only on a layout with no w", () => {
    const props = setup();
    const name = screen.getByText("Worker");
    // AZERTY's ⌘Z sits on W's place: undo, not ⌘W. Dvorak's ⌘, there is Settings.
    fireEvent.keyDown(name, { key: "z", code: "KeyW", metaKey: true });
    fireEvent.keyDown(name, { key: ",", code: "KeyW", metaKey: true });
    fireEvent.keyDown(name, { key: "W", code: "KeyW", metaKey: true, shiftKey: true });
    expect(props.onClose).not.toHaveBeenCalled();
    // Dvorak's w sits on the comma's place.
    fireEvent.keyDown(name, { key: "w", code: "KeyComma", metaKey: true });
    expect(props.onClose).toHaveBeenCalledTimes(1);
    // A Cyrillic layout has no w: W's place, with its own letter there, is ⌘W.
    fireEvent.keyDown(name, { key: "ц", code: "KeyW", metaKey: true });
    expect(props.onClose).toHaveBeenCalledTimes(2);
  });

  it("a click on its name or header keeps focus inside it, where ⌘W still closes it", () => {
    const props = setup();
    const dialog = screen.getByRole("dialog");
    expect(dialog).toHaveAttribute("tabindex", "-1");
    // What a click on the header text does: focus goes to the nearest focusable ancestor.
    dialog.focus();
    expect(dialog).toHaveFocus();
    fireEvent.keyDown(dialog, { key: "w", code: "KeyW", metaKey: true });
    expect(props.onClose).toHaveBeenCalledWith(true);
  });

  it("an agent with no live terminal says so, and its Close takes focus", () => {
    setup({ sessionId: null });
    expect(screen.queryByTestId("quick-send-terminal")).toBeNull();
    expect(screen.getByText(/no live terminal/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Close" })).toHaveFocus();
  });

  it("an agent whose machine is offline says that is why it has no live terminal", () => {
    setup({ sessionId: null, offlineMachine: "build-box" });
    expect(
      screen.getByText(
        "build-box is offline, so Worker's terminal can't be shown until it reconnects.",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByText(/no live terminal/)).toBeNull();
  });

  it("names the question it waits on as its ask", () => {
    setup({ ask: "Merge the branch?" });
    expect(screen.getByRole("note", { name: "Worker asks" })).toHaveTextContent(
      "Waiting on you · Merge the branch?",
    );
  });

  it("Escape on its own controls closes it and asks for focus to return to the row", () => {
    const props = setup();
    screen.getByRole("button", { name: "Close" }).focus();
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    expect(props.onClose).toHaveBeenCalledWith(true);
  });

  it("a click outside it closes it without moving focus", () => {
    const props = setup();
    fireEvent.mouseDown(document.body);
    expect(props.onClose).toHaveBeenCalledWith(false);
  });

  it("opens tall enough to read recent output, and its corner drags to resize it for next time", () => {
    setup();
    const terminal = () => screen.getByTestId("quick-send-terminal").parentElement as HTMLElement;
    const dialog = screen.getByRole("dialog");
    expect(terminal().style.height).toBe("520px");
    expect(dialog.style.width).toBe("680px");

    const grip = screen.getByTestId("agent-quick-send-grip");
    fireEvent.pointerDown(grip, { button: 0, clientX: 600, clientY: 600, pointerId: 1 });
    fireEvent.pointerMove(grip, { clientX: 700, clientY: 500, pointerId: 1 });
    fireEvent.pointerUp(grip, { pointerId: 1 });
    expect(dialog.style.width).toBe("780px");
    expect(terminal().style.height).toBe("420px");

    // The next popup opens at the dragged size.
    document.body.innerHTML = "";
    setup();
    expect(screen.getByRole("dialog").style.width).toBe("780px");
  });

  it("an agent that ends while its terminal is here is not announced as moved to its tile", () => {
    const anchor = document.createElement("button");
    document.body.appendChild(anchor);
    const props: QuickSendProps = {
      name: "Worker",
      statusWord: "Working",
      workspace: null,
      sessionId: "sid-worker",
      anchor,
      onClose: vi.fn(),
      onShowInCli: vi.fn(),
    };
    const view = render(<QuickSendPopover {...props} />);
    expect(screen.getByTestId("quick-send-terminal")).toBeInTheDocument();
    view.rerender(<QuickSendPopover {...props} statusWord="Ended" sessionId={null} />);
    expect(screen.getByText(/no live terminal/)).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("");
  });
});
