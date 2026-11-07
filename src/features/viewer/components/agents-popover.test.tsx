// SPDX-License-Identifier: MIT
import { createPortal } from "react-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup } from "@testing-library/react";
import { act, fireEvent, render, screen } from "@/test-utils";

import { useRowPopover, type PopoverBounds } from "@/features/viewer/components/agents-popover";

const KEY = "test.popover.size";
const MIN = { width: 380, height: 160 };

function Popover({
  anchor,
  onClose,
  portal = false,
  body = true,
  bounds,
}: {
  anchor: HTMLElement;
  onClose: (restoreFocus: boolean) => void;
  portal?: boolean;
  body?: boolean;
  bounds?: () => PopoverBounds;
}) {
  const { dialogRef, bodyRef, place, shown, gripProps, gripHint, arrowProps, onMouseDown } =
    useRowPopover({
      anchor,
      onClose,
      storageKey: KEY,
      defaultSize: { width: 600, height: 400 },
      minSize: MIN,
      chromePx: 80,
      bounds,
    });
  return (
    <div
      ref={dialogRef}
      role="dialog"
      aria-label="Popup"
      onMouseDown={onMouseDown}
      style={{
        position: "fixed",
        top: place?.top ?? 0,
        left: place?.left ?? 0,
        width: shown.width,
      }}
    >
      <span data-testid="arrow" {...arrowProps} />
      <button type="button">Inside</button>
      {body && (
        <div ref={bodyRef} data-testid="body" style={{ height: shown.height }}>
          <div data-testid="inner-scroll" />
        </div>
      )}
      {/* What a menu or a select opened from the popup draws: in a portal, outside its DOM. */}
      {portal && createPortal(<button type="button">Menu item</button>, document.body)}
      <div data-testid="grip" {...gripProps}>
        <span id={gripHint.id} hidden>
          {gripHint.text}
        </span>
      </div>
    </div>
  );
}

function setup(
  portal = false,
  opts: { anchor?: HTMLElement; body?: boolean; bounds?: () => PopoverBounds } = {},
) {
  const anchor = opts.anchor ?? document.createElement("button");
  if (!anchor.isConnected) document.body.appendChild(anchor);
  const onClose = vi.fn();
  const view = render(
    <Popover
      anchor={anchor}
      onClose={onClose}
      portal={portal}
      body={opts.body ?? true}
      bounds={opts.bounds}
    />,
  );
  return { anchor, onClose, view };
}

/** A box at `top` (and `left`), `height` tall and `width` wide, as layout reports it. */
function box(top: number, height: number, left = 0, width = 100): DOMRect {
  return {
    top,
    left,
    height,
    width,
    bottom: top + height,
    right: left + width,
    x: left,
    y: top,
    toJSON: () => ({}),
  } as DOMRect;
}

/** Lay `el` out at `rect`. */
function at(el: Element, rect: DOMRect): void {
  Object.defineProperty(el, "getBoundingClientRect", { value: () => rect, configurable: true });
}

const px = (value: string) => parseFloat(value);

const saved = () => JSON.parse(localStorage.getItem(KEY) ?? "null");

describe("useRowPopover", () => {
  const size = { width: window.innerWidth, height: window.innerHeight };
  beforeEach(() => localStorage.clear());
  afterEach(() => {
    Object.assign(window, { innerWidth: size.width, innerHeight: size.height });
    cleanup();
    document.body.innerHTML = "";
  });

  it("focus Tabbing out of the popup and its row closes it, and its Escape is left alone", () => {
    const { onClose } = setup();
    screen.getByRole("button", { name: "Inside" }).focus();
    expect(onClose).not.toHaveBeenCalled();
    const other = document.createElement("input");
    document.body.appendChild(other);
    other.focus();
    // Closed where focus went: nothing pulls it back to the row.
    expect(onClose).toHaveBeenCalledExactlyOnceWith(false);
    const heard = vi.fn();
    other.addEventListener("keydown", heard);
    fireEvent.keyDown(other, { key: "Escape" });
    expect(onClose).not.toHaveBeenCalledWith(true);
    expect(heard).toHaveBeenCalled();
  });

  it("an Escape that ends an IME composition is the composition's", () => {
    const { onClose } = setup();
    screen.getByRole("button", { name: "Inside" }).focus();
    fireEvent.keyDown(document.activeElement!, { key: "Escape", isComposing: true });
    fireEvent.keyDown(document.activeElement!, { key: "Escape", keyCode: 229 });
    expect(onClose).not.toHaveBeenCalled();
  });

  it("Escape inside the popup, or on its row, closes it and returns focus", () => {
    const { anchor, onClose } = setup();
    screen.getByRole("button", { name: "Inside" }).focus();
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    expect(onClose).toHaveBeenLastCalledWith(true);
    anchor.focus();
    fireEvent.keyDown(anchor, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(2);
  });

  it("a press in a portal the popup opened (a menu, a select) is not a press outside", () => {
    const { onClose } = setup(true);
    fireEvent.mouseDown(screen.getByRole("button", { name: "Menu item" }));
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.mouseDown(document.body);
    expect(onClose).toHaveBeenCalledWith(false);
  });

  it("a press in a Radix portal outside React's tree (a menu's content) is not a press outside", () => {
    const { onClose } = setup();
    const wrapper = document.createElement("div");
    wrapper.setAttribute("data-radix-popper-content-wrapper", "");
    const item = document.createElement("div");
    wrapper.appendChild(item);
    document.body.appendChild(wrapper);
    fireEvent.mouseDown(item);
    expect(onClose).not.toHaveBeenCalled();
  });

  it("a drag of its grip does not subscribe its listeners again on every move", () => {
    setup();
    const grip = screen.getByTestId("grip");
    const add = vi.spyOn(window, "addEventListener");
    fireEvent.pointerDown(grip, { button: 0, clientX: 600, clientY: 600, pointerId: 1 });
    for (let x = 610; x <= 660; x += 10) {
      fireEvent.pointerMove(grip, { clientX: x, clientY: 610, pointerId: 1 });
    }
    expect(screen.getByRole("dialog").style.width).toBe("660px");
    expect(
      add.mock.calls.filter(
        ([type]) => (type as string) === "resize" || (type as string) === "scroll",
      ),
    ).toEqual([]);
    add.mockRestore();
  });

  it("is fitted again when the window shrinks under it", () => {
    Object.assign(window, { innerWidth: 1200, innerHeight: 900 });
    setup();
    const dialog = screen.getByRole("dialog");
    expect(px(dialog.style.width)).toBe(600);
    Object.assign(window, { innerWidth: 400, innerHeight: 900 });
    act(() => {
      window.dispatchEvent(new Event("resize"));
    });
    expect(px(dialog.style.width)).toBeLessThanOrEqual(400 - 16);
  });

  it("a keyboard resize hold shrinks inside the window before moving its left edge", () => {
    Object.assign(window, { innerWidth: 1200, innerHeight: 900 });
    const anchor = document.createElement("button");
    at(anchor, box(100, 30, 0, 262));
    setup(false, { anchor });
    const dialog = screen.getByRole("dialog");
    const grip = screen.getByTestId("grip");
    grip.focus();
    fireEvent.keyDown(grip, { key: "ArrowRight", shiftKey: true });
    fireEvent.keyUp(grip, { key: "ArrowRight" });
    const intended = saved();
    expect(px(dialog.style.left)).toBe(270);

    Object.assign(window, { innerWidth: 800, innerHeight: 500 });
    fireEvent(window, new Event("resize"));
    expect(px(dialog.style.left)).toBe(270);
    expect(px(dialog.style.width)).toBe(800 - 270 - 8);
    expect(px(dialog.style.width)).toBeGreaterThanOrEqual(MIN.width);
    expect(px(dialog.style.top)).toBeGreaterThanOrEqual(8);
    expect(
      px(dialog.style.top) + px(screen.getByTestId("body").style.height) + 80,
    ).toBeLessThanOrEqual(492);
    expect(saved()).toEqual(intended);

    Object.assign(window, { innerWidth: 1200, innerHeight: 900 });
    fireEvent(window, new Event("resize"));
    expect(px(dialog.style.left)).toBe(270);
    expect(px(dialog.style.width)).toBe(intended.width);
    expect(px(screen.getByTestId("body").style.height)).toBe(intended.height);
    expect(saved()).toEqual(intended);
  });

  it("one ArrowLeft shrinks the displayed width immediately after a held window shrink", () => {
    Object.assign(window, { innerWidth: 1200, innerHeight: 900 });
    const anchor = document.createElement("button");
    at(anchor, box(100, 30, 0, 262));
    setup(false, { anchor });
    const dialog = screen.getByRole("dialog");
    const grip = screen.getByTestId("grip");
    fireEvent.keyDown(grip, { key: "ArrowRight", shiftKey: true });
    fireEvent.keyUp(grip, { key: "ArrowRight" });
    Object.assign(window, { innerWidth: 800 });
    fireEvent(window, new Event("resize"));
    expect(px(dialog.style.width)).toBe(522);

    fireEvent.keyDown(grip, { key: "ArrowLeft" });
    fireEvent.keyUp(grip, { key: "ArrowLeft" });
    expect(px(dialog.style.width)).toBe(506);
    expect(saved().width).toBe(506);
  });

  it("restores the remembered width on window growth while its row is undrawn", () => {
    Object.assign(window, { innerWidth: 500, innerHeight: 900 });
    const remembered = { width: 700, height: 400 };
    localStorage.setItem(KEY, JSON.stringify(remembered));
    const row = document.createElement("div");
    row.className = "ag-row";
    at(row, box(100, 30));
    const anchor = document.createElement("button");
    row.appendChild(anchor);
    document.body.appendChild(row);
    const { onClose } = setup(false, { anchor });
    const dialog = screen.getByRole("dialog");
    expect(px(dialog.style.width)).toBe(484);
    const position = { top: dialog.style.top, left: dialog.style.left };

    row.remove();
    Object.assign(window, { innerWidth: 1200 });
    fireEvent(window, new Event("resize"));
    expect(px(dialog.style.width)).toBe(700);
    expect({ top: dialog.style.top, left: dialog.style.left }).toEqual(position);
    expect(saved()).toEqual(remembered);
    expect(onClose).not.toHaveBeenCalled();
  });

  it("a held bounds shrink reduces body height before moving its top", () => {
    const bounds = { top: 40, left: 30, right: 1200, bottom: 800 };
    const anchor = document.createElement("button");
    at(anchor, box(200, 30, 30, 232));
    setup(false, { anchor, bounds: () => bounds });
    const dialog = screen.getByRole("dialog");
    const body = screen.getByTestId("body");
    const grip = screen.getByTestId("grip");
    fireEvent.keyDown(grip, { key: "ArrowRight" });
    fireEvent.keyUp(grip, { key: "ArrowRight" });
    const intended = saved();
    expect(px(dialog.style.top)).toBe(200);
    expect(px(body.style.height)).toBe(400);

    bounds.bottom = 650;
    fireEvent(window, new Event("resize"));
    expect(px(dialog.style.top)).toBe(200);
    expect(px(body.style.height)).toBe(362);
    expect(saved()).toEqual(intended);

    // Once even the minimum cannot fit, move only enough to show that minimum.
    bounds.bottom = 400;
    fireEvent(window, new Event("resize"));
    expect(px(dialog.style.top)).toBe(152);
    expect(px(body.style.height)).toBe(MIN.height);
    expect(saved()).toEqual(intended);

    bounds.bottom = 800;
    fireEvent(window, new Event("resize"));
    expect(px(body.style.height)).toBe(400);
    fireEvent.blur(grip);
    expect(px(dialog.style.top)).toBe(200);
  });

  it("a held window shrink keeps its arrow within the drawn popup or hidden", () => {
    Object.assign(window, { innerWidth: 1200, innerHeight: 800 });
    const anchor = document.createElement("button");
    at(anchor, box(600, 30, 0, 262));
    setup(false, { anchor });
    const grip = screen.getByTestId("grip");
    fireEvent.keyDown(grip, { key: "ArrowRight" });
    fireEvent.keyUp(grip, { key: "ArrowRight" });

    Object.assign(window, { innerHeight: 500 });
    fireEvent(window, new Event("resize"));
    const dialog = screen.getByRole("dialog");
    const arrow = screen.getByTestId("arrow");
    const drawnHeight = px(screen.getByTestId("body").style.height) + 80;
    expect(px(dialog.style.top)).toBe(252);
    expect(drawnHeight).toBe(240);
    expect(arrow).toHaveAttribute("hidden");
    expect(px(arrow.style.top)).toBeLessThanOrEqual(drawnHeight - 14);
  });

  it("a keyboard resize hold shifts only when its minimum width cannot fit the bounds", () => {
    const bounds = { top: 40, left: 30, right: 1200, bottom: 800 };
    const anchor = document.createElement("button");
    at(anchor, box(100, 30, 30, 232));
    setup(false, { anchor, bounds: () => bounds });
    const dialog = screen.getByRole("dialog");
    const grip = screen.getByTestId("grip");
    fireEvent.keyDown(grip, { key: "ArrowRight" });
    fireEvent.keyUp(grip, { key: "ArrowRight" });
    const intended = saved();
    expect(px(dialog.style.left)).toBe(270);

    bounds.right = 600;
    bounds.bottom = 380;
    fireEvent(window, new Event("resize"));
    expect(px(dialog.style.width)).toBe(MIN.width);
    expect(px(dialog.style.left)).toBe(600 - MIN.width - 8);
    expect(px(dialog.style.left)).toBeGreaterThanOrEqual(bounds.left + 8);
    expect(px(dialog.style.top)).toBeGreaterThanOrEqual(bounds.top + 8);
    expect(
      px(dialog.style.top) + px(screen.getByTestId("body").style.height) + 80,
    ).toBeLessThanOrEqual(bounds.bottom - 8);
    expect(saved()).toEqual(intended);
  });

  it("a drag hold shrinks inside the bounds while keeping its left edge and intended size", () => {
    const bounds = { top: 40, left: 30, right: 1200, bottom: 800 };
    const anchor = document.createElement("button");
    at(anchor, box(100, 30, 30, 232));
    setup(false, { anchor, bounds: () => bounds });
    const dialog = screen.getByRole("dialog");
    const grip = screen.getByTestId("grip");
    fireEvent.pointerDown(grip, { button: 0, clientX: 870, clientY: 580, pointerId: 1 });
    fireEvent.pointerMove(grip, { clientX: 934, clientY: 580, pointerId: 1 });
    expect(px(dialog.style.left)).toBe(270);
    expect(px(dialog.style.width)).toBe(664);

    bounds.right = 800;
    bounds.bottom = 500;
    fireEvent(window, new Event("resize"));
    expect(px(dialog.style.left)).toBe(270);
    expect(px(dialog.style.width)).toBe(800 - 270 - 8);
    expect(
      px(dialog.style.top) + px(screen.getByTestId("body").style.height) + 80,
    ).toBeLessThanOrEqual(bounds.bottom - 8);
    expect(saved()).toBeNull();

    bounds.right = 1200;
    bounds.bottom = 800;
    fireEvent(window, new Event("resize"));
    expect(px(dialog.style.width)).toBe(664);
    fireEvent.pointerUp(grip, { pointerId: 1 });
    expect(saved()).toEqual({ width: 664, height: 400 });
  });

  it("closing mid-resize keeps the size reached, and lost capture ends the drag", () => {
    const { view } = setup();
    const grip = screen.getByTestId("grip");
    fireEvent.pointerDown(grip, { button: 0, clientX: 600, clientY: 600, pointerId: 1 });
    fireEvent.pointerMove(grip, { clientX: 650, clientY: 620, pointerId: 1 });
    view.unmount();
    expect(saved()).toEqual({ width: 650, height: 420 });

    localStorage.clear();
    const again = setup();
    const grip2 = screen.getByTestId("grip");
    fireEvent.pointerDown(grip2, { button: 0, clientX: 600, clientY: 600, pointerId: 2 });
    fireEvent.pointerMove(grip2, { clientX: 620, clientY: 600, pointerId: 2 });
    grip2.dispatchEvent(new Event("lostpointercapture"));
    expect(saved()).toEqual({ width: 620, height: 400 });
    // The drag is over: a later move does nothing.
    fireEvent.pointerMove(grip2, { clientX: 700, clientY: 700, pointerId: 2 });
    expect(screen.getByRole("dialog").style.width).toBe("620px");
    again.view.unmount();
  });

  it("never grows past the window, even when that is smaller than its minimum", () => {
    Object.assign(window, { innerWidth: 300, innerHeight: 200 });
    setup();
    const dialog = screen.getByRole("dialog");
    expect(parseFloat(dialog.style.width)).toBeLessThanOrEqual(300 - 16);
    expect(parseFloat(screen.getByTestId("body").style.height)).toBeLessThanOrEqual(200 - 80 - 16);
  });

  it("its grip is a focusable handle the arrow keys resize both ways, saving the size once a key is let go", () => {
    setup();
    const grip = screen.getByRole("separator", { name: "Resize popup" });
    expect(grip).toHaveAttribute("tabindex", "0");
    expect(grip).toHaveAccessibleDescription("Arrow keys change its size; Shift moves further.");
    fireEvent.keyDown(grip, { key: "ArrowRight" });
    fireEvent.keyDown(grip, { key: "ArrowDown" });
    expect(screen.getByRole("dialog").style.width).toBe("616px");
    expect(screen.getByTestId("body").style.height).toBe("416px");
    expect(saved()).toBeNull();
    fireEvent.keyUp(grip, { key: "ArrowDown" });
    expect(saved()).toEqual({ width: 616, height: 416 });
  });

  it("arrow keys held with ⌘, ⌥ or ⌃ are shortcuts, not a resize", () => {
    setup();
    const grip = screen.getByRole("separator", { name: "Resize popup" });
    for (const mod of ["metaKey", "altKey", "ctrlKey"]) {
      fireEvent.keyDown(grip, { key: "ArrowRight", [mod]: true });
      fireEvent.keyUp(grip, { key: "ArrowRight", [mod]: true });
    }
    expect(screen.getByRole("dialog").style.width).toBe("600px");
    expect(saved()).toBeNull();
  });

  it("a press on the grip that never moves saves nothing", () => {
    localStorage.setItem(KEY, JSON.stringify({ width: 900, height: 500 }));
    Object.assign(window, { innerWidth: 700, innerHeight: 900 });
    setup();
    const grip = screen.getByTestId("grip");
    fireEvent.pointerDown(grip, { button: 0, clientX: 600, clientY: 600, pointerId: 1 });
    fireEvent.pointerUp(grip, { pointerId: 1 });
    expect(saved()).toEqual({ width: 900, height: 500 });
  });

  it("a drag past the window's edge saves the size dragged to, not the size the window shows", () => {
    Object.assign(window, { innerWidth: 700, innerHeight: 900 });
    setup();
    const grip = screen.getByTestId("grip");
    const shown = px(screen.getByRole("dialog").style.width);
    fireEvent.pointerDown(grip, { button: 0, clientX: 600, clientY: 600, pointerId: 1 });
    fireEvent.pointerMove(grip, { clientX: 900, clientY: 610, pointerId: 1 });
    fireEvent.pointerUp(grip, { pointerId: 1 });
    expect(px(screen.getByRole("dialog").style.width)).toBeLessThanOrEqual(700 - 8);
    expect(saved().width).toBe(shown + 300);
  });

  it("the arrow keys grow it only as far as the window shows it below its top, as a drag does", () => {
    Object.assign(window, { innerWidth: 1200, innerHeight: 800 });
    const anchor = document.createElement("button");
    at(anchor, box(300, 30));
    setup(false, { anchor });
    const grip = screen.getByRole("separator", { name: "Resize popup" });
    for (let i = 0; i < 6; i++) fireEvent.keyDown(grip, { key: "ArrowDown", shiftKey: true });
    // 800 high, its top at 300, 80 of chrome and the 8px margin: 412 for the body.
    expect(px(screen.getByTestId("body").style.height)).toBe(412);
    expect(px(screen.getByRole("dialog").style.top)).toBe(300);
  });

  it("is placed beside its row again after a size change releases its hold", () => {
    Object.assign(window, { innerWidth: 1200, innerHeight: 800 });
    const anchor = document.createElement("button");
    at(anchor, box(600, 30));
    setup(false, { anchor });
    const dialog = screen.getByRole("dialog");
    // 400 of body and 80 of chrome fit below 312, not at the row's 600.
    expect(px(dialog.style.top)).toBe(312);
    const grip = screen.getByRole("separator", { name: "Resize popup" });
    fireEvent.keyDown(grip, { key: "ArrowUp" });
    expect(px(dialog.style.top)).toBe(312);
    fireEvent.keyUp(grip, { key: "ArrowUp" });
    fireEvent.blur(grip);
    expect(px(dialog.style.top)).toBe(328);
  });

  it("without a body, all of it is chrome: no body height is kept for it", () => {
    Object.assign(window, { innerWidth: 1200, innerHeight: 800 });
    const spy = vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockImplementation(function (
      this: HTMLElement,
    ) {
      return this.getAttribute("role") === "dialog" ? 120 : 0;
    });
    try {
      const anchor = document.createElement("button");
      at(anchor, box(700, 30));
      setup(false, { anchor, body: false });
      // 120 tall in all: it sits at 800 - 120 - 8, not 400 higher for a body it doesn't have.
      expect(px(screen.getByRole("dialog").style.top)).toBe(672);
    } finally {
      spy.mockRestore();
    }
  });

  it("its arrow points back at the row from the side facing it, and flips with it", () => {
    Object.assign(window, { innerWidth: 1000, innerHeight: 800 });
    const left = document.createElement("button");
    at(left, box(100, 30, 0, 200));
    const first = setup(false, { anchor: left });
    expect(screen.getByTestId("arrow")).toHaveAttribute("data-side", "left");
    expect(px(screen.getByRole("dialog").style.left)).toBe(208);
    first.view.unmount();

    // No room to its right: it opens to the row's left, the arrow on its right edge.
    const right = document.createElement("button");
    at(right, box(100, 30, 700, 200));
    setup(false, { anchor: right });
    expect(screen.getByTestId("arrow")).toHaveAttribute("data-side", "right");
    expect(px(screen.getByRole("dialog").style.left)).toBe(92);
  });

  it("dragged wider while it sits left of its row, it keeps its left edge and side until the drag ends", () => {
    Object.assign(window, { innerWidth: 1000, innerHeight: 800 });
    const anchor = document.createElement("button");
    at(anchor, box(100, 30, 700, 200));
    setup(false, { anchor });
    const dialog = screen.getByRole("dialog");
    expect(px(dialog.style.left)).toBe(92);
    const grip = screen.getByTestId("grip");
    fireEvent.pointerDown(grip, { button: 0, clientX: 692, clientY: 600, pointerId: 1 });
    // 750 wide no longer fits left of the row: placed afresh it would jump to the row's right.
    fireEvent.pointerMove(grip, { clientX: 842, clientY: 600, pointerId: 1 });
    expect(dialog.style.width).toBe("750px");
    expect(px(dialog.style.left)).toBe(92);
    // It now covers the row's left edge: no arrow points at the row from inside it.
    expect(screen.getByTestId("arrow")).not.toHaveAttribute("data-side");
    // As far as the window shows it from that left edge, not from where a jump would put it.
    fireEvent.pointerMove(grip, { clientX: 2000, clientY: 600, pointerId: 1 });
    expect(dialog.style.width).toBe(`${1000 - 92 - 8}px`);
    fireEvent.pointerMove(grip, { clientX: 842, clientY: 600, pointerId: 1 });
    fireEvent.pointerUp(grip, { pointerId: 1 });
    // The drag is over: it is placed beside the row again, as far right as the window lets it.
    expect(px(dialog.style.left)).toBe(1000 - 750 - 8);
  });

  it("resized wider by its arrow keys, it keeps its left edge and side until the grip loses focus", () => {
    Object.assign(window, { innerWidth: 1000, innerHeight: 800 });
    const anchor = document.createElement("button");
    at(anchor, box(100, 30, 700, 200));
    setup(false, { anchor });
    const dialog = screen.getByRole("dialog");
    expect(px(dialog.style.left)).toBe(92);
    const grip = screen.getByTestId("grip");
    grip.focus();
    // 600 → 792 wide no longer fits left of the row: placed afresh it would flip to its right.
    for (let i = 0; i < 3; i++) fireEvent.keyDown(grip, { key: "ArrowRight", shiftKey: true });
    expect(dialog.style.width).toBe("792px");
    expect(px(dialog.style.left)).toBe(92);
    fireEvent.blur(grip);
    expect(px(dialog.style.left)).toBe(1000 - 792 - 8);
  });

  it("a keyboard resize lets go of the popup's place once its keys rest", () => {
    vi.useFakeTimers();
    try {
      Object.assign(window, { innerWidth: 1000, innerHeight: 800 });
      const anchor = document.createElement("button");
      at(anchor, box(100, 30, 700, 200));
      setup(false, { anchor });
      const dialog = screen.getByRole("dialog");
      const grip = screen.getByTestId("grip");
      for (let i = 0; i < 3; i++) fireEvent.keyDown(grip, { key: "ArrowRight", shiftKey: true });
      expect(px(dialog.style.left)).toBe(92);
      act(() => vi.advanceTimersByTime(1000));
      expect(px(dialog.style.left)).toBe(92);
      act(() => vi.advanceTimersByTime(1000));
      expect(px(dialog.style.left)).toBe(1000 - 792 - 8);
    } finally {
      vi.useRealTimers();
    }
  });

  it("hides its arrow when the row's middle is above or below it", () => {
    Object.assign(window, { innerWidth: 1200, innerHeight: 800 });
    const anchor = document.createElement("button");
    // A row at the window's foot: the popup sits above it, its foot at 792, the row's middle 795.
    at(anchor, box(780, 30, 0, 200));
    setup(false, { anchor });
    expect(px(screen.getByRole("dialog").style.top)).toBe(312);
    const arrow = screen.getByTestId("arrow");
    expect(arrow).not.toHaveAttribute("data-side");
    expect(arrow).toHaveAttribute("hidden");
  });

  describe("its row scrolling out of the list", () => {
    /** A list (`.ag-scroll`, 0–200) with the popup's row and another row, laid out at `rows`. */
    function list(rows: { anchor: DOMRect; other: DOMRect | null }, tabbable = false) {
      const scroller = document.createElement("div");
      scroller.className = "ag-scroll";
      if (tabbable) scroller.tabIndex = -1;
      at(scroller, box(0, 200));
      const row = document.createElement("div");
      row.className = "ag-row";
      const anchor = document.createElement("button");
      row.appendChild(anchor);
      const otherRow = document.createElement("div");
      otherRow.className = "ag-row";
      const otherButton = document.createElement("button");
      otherRow.appendChild(otherButton);
      scroller.append(row, otherRow);
      document.body.appendChild(scroller);
      at(row, rows.anchor);
      at(otherRow, rows.other ?? box(-500, 30));
      return { scroller, anchor, otherButton };
    }

    it("closes it and moves focus from inside it to a row still in view", () => {
      const { scroller, anchor, otherButton } = list({ anchor: box(20, 30), other: box(60, 30) });
      const { onClose } = setup(false, { anchor });
      screen.getByRole("button", { name: "Inside" }).focus();
      at(anchor.closest(".ag-row")!, box(-100, 30));
      fireEvent.scroll(scroller);
      expect(onClose).toHaveBeenCalledWith(false);
      expect(otherButton).toHaveFocus();
    });

    it("with no row in view, focus goes to the list, never the page body", () => {
      const { scroller, anchor } = list({ anchor: box(20, 30), other: null }, true);
      const { onClose } = setup(false, { anchor });
      screen.getByRole("button", { name: "Inside" }).focus();
      at(anchor.closest(".ag-row")!, box(-100, 30));
      fireEvent.scroll(scroller);
      expect(onClose).toHaveBeenCalledWith(false);
      expect(scroller).toHaveFocus();
    });

    it("with no row in view and a list that takes no focus, the list is made to take it", () => {
      const { scroller, anchor } = list({ anchor: box(20, 30), other: null });
      expect(scroller).not.toHaveAttribute("tabindex");
      const { onClose } = setup(false, { anchor });
      screen.getByRole("button", { name: "Inside" }).focus();
      at(anchor.closest(".ag-row")!, box(-100, 30));
      fireEvent.scroll(scroller);
      expect(onClose).toHaveBeenCalledWith(false);
      expect(scroller).toHaveFocus();
      expect(scroller).toHaveAttribute("tabindex", "-1");
    });

    it("holds its place while its row is drawn away, and follows the row drawn again under its key", () => {
      Object.assign(window, { innerWidth: 1000, innerHeight: 800 });
      const scroller = document.createElement("div");
      scroller.className = "ag-scroll";
      at(scroller, box(0, 600));
      const drawRow = (top: number) => {
        const wrap = document.createElement("div");
        wrap.dataset.rowKey = "agent:mover";
        const row = document.createElement("div");
        row.className = "ag-row";
        const button = document.createElement("button");
        row.appendChild(button);
        wrap.appendChild(row);
        scroller.appendChild(wrap);
        at(row, box(top, 30, 0, 200));
        return { wrap, button };
      };
      document.body.appendChild(scroller);
      const first = drawRow(100);
      const { onClose } = setup(false, { anchor: first.button });
      const dialog = screen.getByRole("dialog");
      expect(px(dialog.style.top)).toBe(100);
      // A fold (or a drag) draws the row away: no jump to a zero box at the top-left corner.
      first.wrap.remove();
      fireEvent(window, new Event("resize"));
      expect(onClose).not.toHaveBeenCalled();
      expect(px(dialog.style.top)).toBe(100);
      expect(px(dialog.style.left)).toBe(208);
      // Drawn again under the same key, lower down: the popup follows it.
      drawRow(300);
      fireEvent(window, new Event("resize"));
      expect(px(dialog.style.top)).toBe(300);
      expect(onClose).not.toHaveBeenCalled();
    });

    it("closed with focus inside (its agent left the list), focus goes to a row, never the body", () => {
      const { anchor, otherButton } = list({ anchor: box(20, 30), other: box(60, 30) });
      const { view } = setup(false, { anchor });
      screen.getByRole("button", { name: "Inside" }).focus();
      anchor.closest(".ag-row")!.remove();
      view.unmount();
      expect(otherButton).toHaveFocus();
    });

    it("closed with focus inside while its row is still listed, focus goes back to the row", () => {
      const { anchor } = list({ anchor: box(20, 30), other: box(60, 30) });
      const { view } = setup(false, { anchor });
      screen.getByRole("button", { name: "Inside" }).focus();
      view.unmount();
      expect(anchor).toHaveFocus();
    });

    it("a scroll of its list that takes its row straight from view to undrawn closes it", () => {
      const scroller = document.createElement("div");
      scroller.className = "ag-scroll";
      at(scroller, box(0, 600));
      const wrap = document.createElement("div");
      wrap.dataset.rowKey = "agent:jumper";
      const row = document.createElement("div");
      row.className = "ag-row";
      const anchor = document.createElement("button");
      row.appendChild(anchor);
      wrap.appendChild(row);
      scroller.appendChild(wrap);
      document.body.appendChild(scroller);
      at(row, box(100, 30));
      const { onClose } = setup(false, { anchor });
      // A fold draws it away: the popup holds its place.
      wrap.remove();
      fireEvent(window, new Event("resize"));
      expect(onClose).not.toHaveBeenCalled();
      // A scroll of the list with the row still undrawn: it scrolled out of the drawn window.
      fireEvent.scroll(scroller);
      expect(onClose).toHaveBeenCalledWith(false);
    });

    it("a scroll inside the popup keeps it open", () => {
      const { anchor } = list({ anchor: box(20, 30), other: null });
      const { onClose } = setup(false, { anchor });
      at(anchor.closest(".ag-row")!, box(-100, 30));
      fireEvent.scroll(screen.getByTestId("inner-scroll"));
      expect(onClose).not.toHaveBeenCalled();
    });
  });

  it("a row in no list closes it once it leaves the window", () => {
    const anchor = document.createElement("button");
    at(anchor, box(100, 30));
    const { onClose } = setup(false, { anchor });
    fireEvent.scroll(window);
    expect(onClose).not.toHaveBeenCalled();
    at(anchor, box(-60, 30));
    fireEvent.scroll(window);
    expect(onClose).toHaveBeenCalledWith(false);
  });

  it("a saved size no drag could reach is not read", () => {
    for (const bad of [
      { width: -5, height: 300 },
      { width: 500, height: 1e9 },
      { width: 0, height: 300 },
      { width: 500, height: null },
    ]) {
      localStorage.setItem(KEY, JSON.stringify(bad));
      const { view } = setup();
      expect(screen.getByRole("dialog").style.width).toBe("600px");
      view.unmount();
    }
  });

  it("its grip carries its width as its value, between its minimum and the window's room", () => {
    Object.assign(window, { innerWidth: 1000, innerHeight: 800 });
    setup();
    const grip = screen.getByRole("separator", { name: "Resize popup" });
    expect(grip).toHaveAttribute("aria-valuenow", "600");
    expect(grip).toHaveAttribute("aria-valuemin", "380");
    expect(grip).toHaveAttribute("aria-valuemax", String(1000 - 16));
    expect(grip).toHaveAttribute("aria-valuetext", "600 by 400 pixels");
  });

  it("held right of its row, it hides its arrow once the row has moved under it", () => {
    Object.assign(window, { innerWidth: 1000, innerHeight: 800 });
    const anchor = document.createElement("button");
    at(anchor, box(100, 30, 0, 200));
    setup(false, { anchor });
    const arrow = screen.getByTestId("arrow");
    expect(arrow).toHaveAttribute("data-side", "left");
    const grip = screen.getByTestId("grip");
    fireEvent.pointerDown(grip, { button: 0, clientX: 808, clientY: 600, pointerId: 1 });
    // The row now reaches past the popup's held left edge: no arrow points at it from inside.
    at(anchor, box(100, 30, 300, 200));
    fireEvent(window, new Event("resize"));
    expect(px(screen.getByRole("dialog").style.left)).toBe(208);
    expect(arrow).not.toHaveAttribute("data-side");
    fireEvent.pointerUp(grip, { pointerId: 1 });
  });

  it("a saved size without a height (an old shape) is not read", () => {
    localStorage.setItem(KEY, JSON.stringify({ width: 500, terminalHeight: 300 }));
    setup();
    expect(screen.getByRole("dialog").style.width).toBe("600px");
  });
});
