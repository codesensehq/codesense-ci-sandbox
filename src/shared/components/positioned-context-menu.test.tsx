// SPDX-License-Identifier: MIT
import { useState } from "react";
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { OwningScopeProvider, warmScopes } from "@/shared/lib/warm-scopes";
import {
  PositionedContextMenu,
  PositionedMenuItem,
  contextMenuItemClass,
  clampMenuToViewport,
  type ContextMenuPosition,
} from "./positioned-context-menu";

describe("clampMenuToViewport (SW-FX-55)", () => {
  const VIEWPORT = { viewportWidth: 1000, viewportHeight: 800 };

  it("AC-1: right-overflow shifts the menu left so its right edge stays in-viewport", () => {
    // cursor near the right edge; menu would paint past innerWidth
    const { left, top } = clampMenuToViewport({
      x: 990,
      y: 100,
      menuWidth: 160,
      menuHeight: 120,
      ...VIEWPORT,
      padding: 8,
    });
    // pulled inward to viewportWidth - menuWidth - padding
    expect(left).toBe(1000 - 160 - 8);
    expect(top).toBe(100); // y already fits — untouched
  });

  it("AC-2: bottom-overflow shifts the menu up so its bottom edge stays in-viewport", () => {
    const { left, top } = clampMenuToViewport({
      x: 100,
      y: 790,
      menuWidth: 160,
      menuHeight: 120,
      ...VIEWPORT,
      padding: 8,
    });
    expect(left).toBe(100); // x already fits — untouched
    expect(top).toBe(800 - 120 - 8);
  });

  it("AC-3: a position that already fits is returned unchanged (no-op — cursor-anchoring preserved)", () => {
    const { left, top } = clampMenuToViewport({
      x: 200,
      y: 300,
      menuWidth: 160,
      menuHeight: 120,
      ...VIEWPORT,
      padding: 8,
    });
    expect(left).toBe(200);
    expect(top).toBe(300);
  });

  it("AC-4: a menu larger than the viewport pins to padding and never returns a negative coordinate", () => {
    const { left, top } = clampMenuToViewport({
      x: 500,
      y: 500,
      menuWidth: 2000, // wider than the viewport
      menuHeight: 1500, // taller than the viewport
      ...VIEWPORT,
      padding: 8,
    });
    expect(left).toBe(8);
    expect(top).toBe(8);
    expect(left).toBeGreaterThanOrEqual(0);
    expect(top).toBeGreaterThanOrEqual(0);
  });

  it("AC-4: a cursor past the top-left edge is floored at padding (never negative)", () => {
    const { left, top } = clampMenuToViewport({
      x: -50,
      y: -50,
      menuWidth: 160,
      menuHeight: 120,
      ...VIEWPORT,
      padding: 8,
    });
    expect(left).toBe(8);
    expect(top).toBe(8);
  });

  it("defaults padding to 8 when omitted", () => {
    const { left } = clampMenuToViewport({
      x: 990,
      y: 100,
      menuWidth: 160,
      menuHeight: 120,
      ...VIEWPORT,
    });
    expect(left).toBe(1000 - 160 - 8);
  });
});

describe("PositionedContextMenu", () => {
  it("renders nothing when position is null", () => {
    const { container } = render(
      <PositionedContextMenu position={null} onClose={vi.fn()}>
        <button>item</button>
      </PositionedContextMenu>,
    );
    expect(container.firstChild).toBeNull();
  });

  it("SW-EW-23: a click on a child that does NOT close keeps the menu open (no backdrop click-through)", () => {
    const onClose = vi.fn();
    render(
      <PositionedContextMenu position={{ x: 10, y: 10 }} onClose={onClose}>
        <button className={contextMenuItemClass}>noop item</button>
      </PositionedContextMenu>,
    );
    // Clicking an item must not bubble to the backdrop's onClose — items own
    // their own close, so a non-closing item (e.g. a confirm toggle) survives.
    fireEvent.click(screen.getByText("noop item"));
    expect(onClose).not.toHaveBeenCalled();
  });

  it("SW-EW-23: a right-click on a child does NOT bubble to the backdrop's onClose", () => {
    const onClose = vi.fn();
    render(
      <PositionedContextMenu position={{ x: 10, y: 10 }} onClose={onClose}>
        <button className={contextMenuItemClass}>noop item</button>
      </PositionedContextMenu>,
    );
    // Right-clicking an item must not dismiss the menu (mirrors the left-click
    // guard) — otherwise an inline confirm toggle is lost on a re-right-click.
    fireEvent.contextMenu(screen.getByText("noop item"));
    expect(onClose).not.toHaveBeenCalled();
  });

  it("closes on a backdrop (outside) click", () => {
    const onClose = vi.fn();
    const { container } = render(
      <PositionedContextMenu position={{ x: 10, y: 10 }} onClose={onClose}>
        <button>item</button>
      </PositionedContextMenu>,
    );
    // The backdrop is the outer fixed-inset-0 overlay.
    const backdrop = container.querySelector(".fixed.inset-0") as HTMLElement;
    fireEvent.click(backdrop);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("SW-EW-30 D4: closes on Escape when open", () => {
    const onClose = vi.fn();
    render(
      <PositionedContextMenu position={{ x: 10, y: 10 }} onClose={onClose}>
        <button>item</button>
      </PositionedContextMenu>,
    );
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("a menu left open when its team is hidden closes, so the shown team's Escape is not swallowed", () => {
    function Tile() {
      const [position, setPosition] = useState<ContextMenuPosition>({ x: 10, y: 10 });
      return (
        <OwningScopeProvider scope="menu:a">
          <PositionedContextMenu position={position} onClose={() => setPosition(null)}>
            <button>item</button>
          </PositionedContextMenu>
        </OwningScopeProvider>
      );
    }
    act(() => warmScopes.activate("menu:a"));
    render(<Tile />);
    const teamBEscape = vi.fn();
    document.addEventListener("keydown", teamBEscape);
    act(() => warmScopes.activate("menu:b"));
    expect(screen.queryByText("item")).not.toBeInTheDocument();
    fireEvent.keyDown(document.body, { key: "Escape" });
    expect(teamBEscape).toHaveBeenCalledTimes(1);
    document.removeEventListener("keydown", teamBEscape);
    for (const k of ["menu:a", "menu:b"]) warmScopes.forget(k);
  });

  it("SW-EW-30 D4: attaches no Escape listener while closed (no spurious onClose)", () => {
    const onClose = vi.fn();
    render(
      <PositionedContextMenu position={null} onClose={onClose}>
        <button>item</button>
      </PositionedContextMenu>,
    );
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onClose).not.toHaveBeenCalled();
  });

  it("SW-FX-55: applies the clamped left/top to the menu style when opened near the right/bottom edge", () => {
    // happy-dom has no layout engine, so stub the menu's measured rect and the
    // viewport bounds, then assert the layout-effect clamp reached the DOM.
    const rectSpy = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
      width: 160,
      height: 120,
      top: 0,
      left: 0,
      right: 0,
      bottom: 0,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    } as DOMRect);
    const origW = window.innerWidth;
    const origH = window.innerHeight;
    Object.defineProperty(window, "innerWidth", { value: 1000, configurable: true });
    Object.defineProperty(window, "innerHeight", { value: 800, configurable: true });
    try {
      const { container } = render(
        <PositionedContextMenu position={{ x: 995, y: 795 }} onClose={vi.fn()}>
          <button>item</button>
        </PositionedContextMenu>,
      );
      const menu = container.querySelector(".z-50") as HTMLElement;
      // pulled inward to viewport - menu - padding on BOTH axes
      expect(menu.style.left).toBe(`${1000 - 160 - 8}px`);
      expect(menu.style.top).toBe(`${800 - 120 - 8}px`);
    } finally {
      rectSpy.mockRestore();
      Object.defineProperty(window, "innerWidth", { value: origW, configurable: true });
      Object.defineProperty(window, "innerHeight", { value: origH, configurable: true });
    }
  });

  it("SW-FX-55: a degenerate 0×0 measurement keeps the raw cursor seed (no clamp against a zero-size rect)", () => {
    // Element not yet laid out → getBoundingClientRect is 0×0. Clamping against
    // a zero-size menu would wrongly pull a near-edge cursor inward (top would
    // become 792, left 992); the guard instead falls back to the raw cursor.
    const rectSpy = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
      width: 0,
      height: 0,
      top: 0,
      left: 0,
      right: 0,
      bottom: 0,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    } as DOMRect);
    const origW = window.innerWidth;
    const origH = window.innerHeight;
    Object.defineProperty(window, "innerWidth", { value: 1000, configurable: true });
    Object.defineProperty(window, "innerHeight", { value: 800, configurable: true });
    try {
      const { container } = render(
        <PositionedContextMenu position={{ x: 995, y: 795 }} onClose={vi.fn()}>
          <button>item</button>
        </PositionedContextMenu>,
      );
      const menu = container.querySelector(".z-50") as HTMLElement;
      expect(menu.style.left).toBe("995px");
      expect(menu.style.top).toBe("795px");
    } finally {
      rectSpy.mockRestore();
      Object.defineProperty(window, "innerWidth", { value: origW, configurable: true });
      Object.defineProperty(window, "innerHeight", { value: origH, configurable: true });
    }
  });

  it("SW-FX-55: relocating into a degenerate 0×0 measurement clears the prior clamp (no stale position leaks)", () => {
    // Regression-lock: open near an edge (clamps to 832/672), then relocate to a
    // spot whose measurement is 0×0. The guard must reset the clamp so the menu
    // seeds from the NEW raw cursor — not the stale prior clamp. Guards against a
    // future refactor that drops the `setClamped(null)` from the 0×0 branch.
    const rectSpy = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
      width: 160,
      height: 120,
      top: 0,
      left: 0,
      right: 0,
      bottom: 0,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    } as DOMRect);
    const origW = window.innerWidth;
    const origH = window.innerHeight;
    Object.defineProperty(window, "innerWidth", { value: 1000, configurable: true });
    Object.defineProperty(window, "innerHeight", { value: 800, configurable: true });
    try {
      const { container, rerender } = render(
        <PositionedContextMenu position={{ x: 995, y: 795 }} onClose={vi.fn()}>
          <button>item</button>
        </PositionedContextMenu>,
      );
      const menu = container.querySelector(".z-50") as HTMLElement;
      expect(menu.style.left).toBe("832px"); // clamped at A
      expect(menu.style.top).toBe("672px");

      // Relocate to B with a degenerate measurement.
      rectSpy.mockReturnValue({
        width: 0,
        height: 0,
        top: 0,
        left: 0,
        right: 0,
        bottom: 0,
        x: 0,
        y: 0,
        toJSON: () => ({}),
      } as DOMRect);
      rerender(
        <PositionedContextMenu position={{ x: 300, y: 300 }} onClose={vi.fn()}>
          <button>item</button>
        </PositionedContextMenu>,
      );
      // Falls back to the NEW raw cursor (300/300), not the stale A clamp (832/672).
      expect(menu.style.left).toBe("300px");
      expect(menu.style.top).toBe("300px");
    } finally {
      rectSpy.mockRestore();
      Object.defineProperty(window, "innerWidth", { value: origW, configurable: true });
      Object.defineProperty(window, "innerHeight", { value: origH, configurable: true });
    }
  });

  it("SW-EW-30 D4: an open menu CONSUMES Escape so layered Esc handlers underneath don't fire", () => {
    // Mirrors the real collision: SW-EW-4 zoom-restore + the management panel
    // both keep a document/bubble-phase Escape handler registered. The open
    // menu must own the keypress, or dismissing it would also un-zoom / steal
    // focus. Capture phase + stopPropagation makes the menu win.
    const onClose = vi.fn();
    const underneath = vi.fn(); // a pre-existing bubble-phase document handler
    document.addEventListener("keydown", underneath);
    try {
      render(
        <PositionedContextMenu position={{ x: 10, y: 10 }} onClose={onClose}>
          <button>item</button>
        </PositionedContextMenu>,
      );
      fireEvent.keyDown(document.body, { key: "Escape" });
      expect(onClose).toHaveBeenCalledTimes(1);
      expect(underneath).not.toHaveBeenCalled();
    } finally {
      document.removeEventListener("keydown", underneath);
    }
  });
});

describe("PositionedContextMenu — keyboard menu", () => {
  /** An invoker that opens the menu, like a tab pill. Each item closes the menu
   *  on pick, as every real consumer does. */
  function Harness({
    items = ["One", "Two", "Three"],
    disabled = [],
    onPick = vi.fn(),
    onParentKeyDown,
    pickFocuses,
  }: {
    items?: string[];
    disabled?: string[];
    onPick?: (label: string) => void;
    onParentKeyDown?: (key: string) => void;
    pickFocuses?: string;
  }) {
    const [pos, setPos] = useState<ContextMenuPosition>(null);
    const [showInvoker, setShowInvoker] = useState(true);
    return (
      <div onKeyDown={(e) => onParentKeyDown?.(e.key)}>
        {showInvoker && <button onClick={() => setPos({ x: 10, y: 10 })}>invoker</button>}
        <input aria-label="elsewhere" />
        <button onClick={() => setShowInvoker(false)}>drop invoker</button>
        <button onClick={() => setPos((p) => (p ? { x: p.x + 50, y: p.y + 50 } : p))}>
          re-anchor
        </button>
        <PositionedContextMenu position={pos} onClose={() => setPos(null)}>
          {items.map((label, i) => (
            <div key={label}>
              {i > 0 && <div role="separator" />}
              <PositionedMenuItem
                disabled={disabled.includes(label)}
                onClick={() => {
                  onPick(label);
                  if (pickFocuses) screen.getByLabelText(pickFocuses).focus();
                  setPos(null);
                }}
              >
                {label}
              </PositionedMenuItem>
            </div>
          ))}
        </PositionedContextMenu>
      </div>
    );
  }

  async function open(props: Parameters<typeof Harness>[0] = {}) {
    const user = userEvent.setup();
    const utils = render(<Harness {...props} />);
    const invoker = screen.getByRole("button", { name: "invoker" });
    invoker.focus();
    await user.keyboard("{Enter}");
    return { user, invoker, ...utils };
  }

  const item = (name: string) => screen.getByRole("menuitem", { name });

  it("is a role=menu of menuitems, outside the Tab order", async () => {
    await open();
    const menu = screen.getByRole("menu");
    expect(menu).toHaveAttribute("aria-orientation", "vertical");
    const items = screen.getAllByRole("menuitem");
    expect(items.map((el) => el.textContent)).toEqual(["One", "Two", "Three"]);
    for (const el of items) {
      expect(el).toHaveAttribute("tabindex", "-1");
      expect(el).toHaveAttribute("type", "button");
    }
  });

  it("opening focuses the first item", async () => {
    await open();
    expect(document.activeElement).toBe(item("One"));
  });

  it("opening skips a disabled first item", async () => {
    await open({ disabled: ["One"] });
    expect(document.activeElement).toBe(item("Two"));
  });

  it("an item-less menu takes focus itself, so its keys still reach it", async () => {
    await open({ items: [] });
    expect(document.activeElement).toBe(screen.getByRole("menu"));
  });

  it("↓ moves down and wraps from the last item to the first", async () => {
    const { user } = await open();
    await user.keyboard("{ArrowDown}");
    expect(document.activeElement).toBe(item("Two"));
    await user.keyboard("{ArrowDown}");
    expect(document.activeElement).toBe(item("Three"));
    await user.keyboard("{ArrowDown}");
    expect(document.activeElement).toBe(item("One"));
  });

  it("↑ moves up and wraps from the first item to the last", async () => {
    const { user } = await open();
    await user.keyboard("{ArrowUp}");
    expect(document.activeElement).toBe(item("Three"));
    await user.keyboard("{ArrowUp}");
    expect(document.activeElement).toBe(item("Two"));
  });

  it("arrows skip disabled items", async () => {
    const { user } = await open({ disabled: ["Two"] });
    await user.keyboard("{ArrowDown}");
    expect(document.activeElement).toBe(item("Three"));
    await user.keyboard("{ArrowUp}");
    expect(document.activeElement).toBe(item("One"));
  });

  it("Home and End jump to the first and last item", async () => {
    const { user } = await open();
    await user.keyboard("{End}");
    expect(document.activeElement).toBe(item("Three"));
    await user.keyboard("{Home}");
    expect(document.activeElement).toBe(item("One"));
  });

  it("Enter activates the focused item exactly once", async () => {
    const onPick = vi.fn();
    const { user } = await open({ onPick });
    await user.keyboard("{ArrowDown}{Enter}");
    expect(onPick).toHaveBeenCalledTimes(1);
    expect(onPick).toHaveBeenCalledWith("Two");
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("Space activates the focused item exactly once", async () => {
    const onPick = vi.fn();
    const { user } = await open({ onPick });
    await user.keyboard("{End}[Space]");
    expect(onPick).toHaveBeenCalledTimes(1);
    expect(onPick).toHaveBeenCalledWith("Three");
  });

  it("Escape closes and returns focus to the invoker", async () => {
    const { user, invoker } = await open();
    await user.keyboard("{ArrowDown}{Escape}");
    expect(screen.queryByRole("menu")).toBeNull();
    expect(document.activeElement).toBe(invoker);
  });

  it("Tab closes and returns focus to the invoker (focus never leaks into the page behind)", async () => {
    const { user, invoker } = await open();
    await user.keyboard("{Tab}");
    expect(screen.queryByRole("menu")).toBeNull();
    expect(document.activeElement).toBe(invoker);
  });

  it("activating an item returns focus to the invoker", async () => {
    const { user, invoker } = await open();
    await user.keyboard("{Enter}");
    expect(document.activeElement).toBe(invoker);
  });

  it("an item that moves focus on purpose keeps it (the invoker doesn't take it back)", async () => {
    const { user } = await open({ pickFocuses: "elsewhere" });
    await user.keyboard("{Enter}");
    expect(document.activeElement).toBe(screen.getByLabelText("elsewhere"));
  });

  it("an invoker that went away while open is not refocused", async () => {
    const { invoker } = await open();
    act(() => {
      fireEvent.click(screen.getByRole("button", { name: "drop invoker" }));
    });
    expect(invoker.isConnected).toBe(false);
    fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
    expect(screen.queryByRole("menu")).toBeNull();
    expect(document.activeElement).not.toBe(invoker);
  });

  it("re-anchoring an open menu keeps the focused item (no jump back to the first)", async () => {
    const { user, invoker } = await open();
    await user.keyboard("{ArrowDown}");
    fireEvent.click(screen.getByRole("button", { name: "re-anchor" }));
    expect(screen.getByRole("menu").style.left).toBe("60px");
    // Still the same item, and closing still goes back to the original invoker.
    expect(document.activeElement).toBe(item("Two"));
    await user.keyboard("{Escape}");
    expect(document.activeElement).toBe(invoker);
  });

  it("hovering an item focuses it, so the arrows continue from the pointer", async () => {
    const { user } = await open();
    fireEvent.pointerMove(item("Three"));
    expect(document.activeElement).toBe(item("Three"));
    await user.keyboard("{ArrowDown}");
    expect(document.activeElement).toBe(item("One"));
  });

  it("the keys it handles stay in the menu (nothing underneath sees them)", async () => {
    const onParentKeyDown = vi.fn();
    const { user } = await open({ onParentKeyDown });
    onParentKeyDown.mockClear();
    await user.keyboard("{ArrowDown}{ArrowUp}{Home}{End}");
    expect(onParentKeyDown).not.toHaveBeenCalled();
    // A key it doesn't handle is left alone.
    await user.keyboard("x");
    expect(onParentKeyDown).toHaveBeenCalledWith("x");
  });

  it("a label names the menu; without one it has no name", () => {
    const { rerender } = render(
      <PositionedContextMenu position={{ x: 1, y: 1 }} onClose={vi.fn()} label="Tab actions">
        <PositionedMenuItem>One</PositionedMenuItem>
      </PositionedContextMenu>,
    );
    expect(screen.getByRole("menu", { name: "Tab actions" })).toBeInTheDocument();
    rerender(
      <PositionedContextMenu position={{ x: 1, y: 1 }} onClose={vi.fn()}>
        <PositionedMenuItem>One</PositionedMenuItem>
      </PositionedContextMenu>,
    );
    expect(screen.getByRole("menu")).not.toHaveAttribute("aria-label");
  });

  it("items show a keyboard focus style, the same highlight as hover", () => {
    expect(contextMenuItemClass).toContain("focus-visible:bg-accent");
    expect(contextMenuItemClass).toContain("focus-visible:text-accent-foreground");
    expect(contextMenuItemClass).toContain("outline-none");
    render(
      <PositionedContextMenu position={{ x: 1, y: 1 }} onClose={vi.fn()}>
        <PositionedMenuItem className="text-red-500">Delete</PositionedMenuItem>
      </PositionedContextMenu>,
    );
    const el = screen.getByRole("menuitem", { name: "Delete" });
    expect(el.className).toContain("focus-visible:bg-accent");
    expect(el.className).toContain("text-red-500");
  });
});
