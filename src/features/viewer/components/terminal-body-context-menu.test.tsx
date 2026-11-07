// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor, renderHook, act } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import {
  TerminalBodyContextMenu,
  useTerminalBodyContextMenu,
  shouldOpenTerminalMenu,
  contextMenuAnchor,
  pasteFromClipboard,
  type TerminalMenuTarget,
  type TerminalMenuTerminal,
} from "./terminal-body-context-menu";
import type { MouseEvent as ReactMouseEvent } from "react";

function makeTarget(over: Partial<TerminalMenuTarget> = {}): TerminalMenuTarget {
  return { hasSelection: () => false, getSelection: () => "", paste: vi.fn(), ...over };
}

function makeTerminal(over: Partial<TerminalMenuTerminal> = {}): TerminalMenuTerminal {
  return {
    modes: { mouseTrackingMode: "none" },
    hasSelection: () => false,
    getSelection: () => "",
    paste: vi.fn(),
    focus: vi.fn(),
    ...over,
  };
}

function fakeEvent(
  over: Partial<{ shiftKey: boolean; clientX: number; clientY: number; detail: number }> = {},
  rect: { left: number; top: number } = { left: 0, top: 0 },
) {
  const preventDefault = vi.fn();
  const e = {
    shiftKey: false,
    clientX: 10,
    clientY: 20,
    detail: 1, // mouse right-click by default
    currentTarget: { getBoundingClientRect: () => rect },
    preventDefault,
    ...over,
  };
  return e as unknown as ReactMouseEvent<HTMLElement> & { preventDefault: typeof preventDefault };
}

describe("shouldOpenTerminalMenu", () => {
  it("opens on a plain right-click when mouse tracking is off", () => {
    expect(shouldOpenTerminalMenu(false, false)).toBe(true);
  });

  it("does NOT open on a plain right-click while mouse tracking is on (leaves the report to the app)", () => {
    expect(shouldOpenTerminalMenu(false, true)).toBe(false);
  });

  it("opens on Shift+right-click even while mouse tracking is on", () => {
    expect(shouldOpenTerminalMenu(true, true)).toBe(true);
  });

  it("opens on Shift+right-click when mouse tracking is off", () => {
    expect(shouldOpenTerminalMenu(true, false)).toBe(true);
  });
});

describe("contextMenuAnchor", () => {
  const currentTarget = { getBoundingClientRect: () => ({ left: 100, top: 200 }) };

  it("uses the cursor position for a mouse right-click", () => {
    expect(contextMenuAnchor({ detail: 1, clientX: 40, clientY: 55, currentTarget })).toEqual({
      x: 40,
      y: 55,
    });
  });

  it("anchors inside the terminal body for a keyboard-triggered menu (no pointer position)", () => {
    expect(contextMenuAnchor({ detail: 0, clientX: 0, clientY: 0, currentTarget })).toEqual({
      x: 108,
      y: 208,
    });
  });
});

describe("pasteFromClipboard", () => {
  const readText = vi.fn<() => Promise<string>>();

  beforeEach(() => {
    readText.mockReset().mockResolvedValue("");
    Object.defineProperty(navigator, "clipboard", {
      value: { readText },
      configurable: true,
      writable: true,
    });
  });

  it("writes clipboard text to the terminal seam (raw — xterm frames bracketed paste)", async () => {
    readText.mockResolvedValue("clipboard contents");
    const paste = vi.fn();
    await pasteFromClipboard(makeTarget({ paste }));
    expect(paste).toHaveBeenCalledWith("clipboard contents");
  });

  it("does not paste an empty clipboard", async () => {
    readText.mockResolvedValue("");
    const paste = vi.fn();
    await pasteFromClipboard(makeTarget({ paste }));
    expect(paste).not.toHaveBeenCalled();
  });

  it("tolerates a missing Clipboard API without throwing", async () => {
    Object.defineProperty(navigator, "clipboard", { value: undefined, configurable: true });
    const paste = vi.fn();
    await expect(pasteFromClipboard(makeTarget({ paste }))).resolves.toBeUndefined();
    expect(paste).not.toHaveBeenCalled();
  });

  it("swallows a rejected clipboard read (denied / unfocused) without throwing", async () => {
    readText.mockRejectedValue(new Error("not focused"));
    const paste = vi.fn();
    await expect(pasteFromClipboard(makeTarget({ paste }))).resolves.toBeUndefined();
    expect(paste).not.toHaveBeenCalled();
  });
});

describe("useTerminalBodyContextMenu", () => {
  it("opens at the cursor on a plain right-click when mouse tracking is off", () => {
    const term = makeTerminal({ modes: { mouseTrackingMode: "none" } });
    const { result } = renderHook(() => useTerminalBodyContextMenu({ current: term }));
    const e = fakeEvent({ clientX: 33, clientY: 44 });
    act(() => result.current.onContextMenu(e));
    expect(e.preventDefault).toHaveBeenCalled();
    expect(result.current.position).toEqual({ x: 33, y: 44 });
  });

  it("anchors a keyboard-triggered menu inside the terminal body, not the viewport corner", () => {
    const term = makeTerminal({ modes: { mouseTrackingMode: "none" } });
    const { result } = renderHook(() => useTerminalBodyContextMenu({ current: term }));
    // Menu key / Shift+F10: detail 0 and coords (0,0).
    const e = fakeEvent({ detail: 0, clientX: 0, clientY: 0 }, { left: 100, top: 200 });
    act(() => result.current.onContextMenu(e));
    expect(result.current.position).toEqual({ x: 108, y: 208 });
  });

  it("suppresses the native menu but does NOT open on a plain right-click while mouse tracking is on", () => {
    const term = makeTerminal({ modes: { mouseTrackingMode: "any" } });
    const { result } = renderHook(() => useTerminalBodyContextMenu({ current: term }));
    const e = fakeEvent();
    act(() => result.current.onContextMenu(e));
    expect(e.preventDefault).toHaveBeenCalled();
    expect(result.current.position).toBeNull();
  });

  it("opens on Shift+right-click even while mouse tracking is on", () => {
    const term = makeTerminal({ modes: { mouseTrackingMode: "vt200" } });
    const { result } = renderHook(() => useTerminalBodyContextMenu({ current: term }));
    act(() => result.current.onContextMenu(fakeEvent({ shiftKey: true })));
    expect(result.current.position).not.toBeNull();
  });

  it("suppresses the native menu but does not open when the terminal is not ready", () => {
    const { result } = renderHook(() =>
      useTerminalBodyContextMenu<TerminalMenuTerminal>({ current: null }),
    );
    const e = fakeEvent();
    act(() => result.current.onContextMenu(e));
    // preventDefault fires even before the terminal exists — the terminal body
    // must never flash the native browser menu during the cold-spawn window.
    expect(e.preventDefault).toHaveBeenCalled();
    expect(result.current.position).toBeNull();
  });

  it("returns focus to the terminal and clears position on close", () => {
    const focus = vi.fn();
    const term = makeTerminal({ focus });
    const { result } = renderHook(() => useTerminalBodyContextMenu({ current: term }));
    act(() => result.current.onContextMenu(fakeEvent()));
    expect(result.current.position).not.toBeNull();
    act(() => result.current.onClose());
    expect(result.current.position).toBeNull();
    expect(focus).toHaveBeenCalled();
  });

  it("exposes a target that delegates copy/paste to the live terminal", () => {
    const paste = vi.fn();
    const term = makeTerminal({ hasSelection: () => true, getSelection: () => "sel", paste });
    const { result } = renderHook(() => useTerminalBodyContextMenu({ current: term }));
    expect(result.current.target.hasSelection()).toBe(true);
    expect(result.current.target.getSelection()).toBe("sel");
    result.current.target.paste("x");
    expect(paste).toHaveBeenCalledWith("x");
  });
});

describe("TerminalBodyContextMenu", () => {
  const writeText = vi.fn<(t: string) => Promise<void>>();
  const readText = vi.fn<() => Promise<string>>();

  beforeEach(() => {
    writeText.mockReset().mockResolvedValue(undefined);
    readText.mockReset().mockResolvedValue("");
    // Override clipboard only, leaving `navigator` intact so userEvent works.
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText, readText },
      configurable: true,
      writable: true,
    });
  });

  it("renders nothing when closed (position is null)", () => {
    const { container } = render(
      <TerminalBodyContextMenu position={null} onClose={vi.fn()} target={makeTarget()} />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("disables Copy when there is no selection", () => {
    render(
      <TerminalBodyContextMenu
        position={{ x: 1, y: 1 }}
        onClose={vi.fn()}
        target={makeTarget({ hasSelection: () => false })}
      />,
    );
    expect(screen.getByRole("menuitem", { name: "Copy" })).toBeDisabled();
  });

  it("copies the current selection and closes when Copy is clicked", async () => {
    const onClose = vi.fn();
    render(
      <TerminalBodyContextMenu
        position={{ x: 1, y: 1 }}
        onClose={onClose}
        target={makeTarget({ hasSelection: () => true, getSelection: () => "selected text" })}
      />,
    );
    await userEvent.click(screen.getByRole("menuitem", { name: "Copy" }));
    expect(writeText).toHaveBeenCalledWith("selected text");
    expect(onClose).toHaveBeenCalled();
  });

  it("does not clobber the clipboard when the selection is empty at click time", async () => {
    const onClose = vi.fn();
    // hasSelection() true at render (button enabled) but getSelection() empty at
    // click — the selection was dropped while the menu was open.
    render(
      <TerminalBodyContextMenu
        position={{ x: 1, y: 1 }}
        onClose={onClose}
        target={makeTarget({ hasSelection: () => true, getSelection: () => "" })}
      />,
    );
    await userEvent.click(screen.getByRole("menuitem", { name: "Copy" }));
    expect(writeText).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalled();
  });

  it("pastes clipboard text through the terminal seam and closes when Paste is clicked", async () => {
    readText.mockResolvedValue("clipboard contents");
    const paste = vi.fn();
    const onClose = vi.fn();
    render(
      <TerminalBodyContextMenu
        position={{ x: 1, y: 1 }}
        onClose={onClose}
        target={makeTarget({ paste })}
      />,
    );
    await userEvent.click(screen.getByRole("menuitem", { name: "Paste" }));
    await waitFor(() => expect(paste).toHaveBeenCalledWith("clipboard contents"));
    expect(onClose).toHaveBeenCalled();
  });
});
