// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import userEvent from "@testing-library/user-event";
import { render, screen, fireEvent, act, waitFor } from "@/test-utils";
import { getEffectiveShortcut, useShortcutsStore } from "@/shared/stores/shortcuts-store";
import { formatKeystrokeDisplay } from "@/shared/lib/shortcuts/keystroke";
import { PaneCollapseDivider, type PaneEdge, type PaneSize } from "./pane-collapse-divider";

// Minimal resize stubs — enough to drive useDragResize. getPos reads clientX
// (the vertical-divider axis); the horizontal cases reuse it harmlessly.
function makeResize(overrides: Partial<Parameters<typeof PaneCollapseDivider>[0]["resize"]> = {}) {
  return {
    getContainerSize: () => 1000,
    getStartRatio: () => 0.5,
    getPos: (e: PointerEvent | React.PointerEvent) => (e as PointerEvent).clientX,
    computeRatio: ({
      startRatio,
      startPos,
      currentPos,
      containerSize,
    }: {
      startRatio: number;
      startPos: number;
      currentPos: number;
      containerSize: number;
    }) => startRatio + (currentPos - startPos) / containerSize,
    onVisualUpdate: vi.fn(),
    onCommit: vi.fn(),
    ...overrides,
  };
}

// Polyfill pointer capture for happy-dom (the hook calls it).
beforeEach(() => {
  Element.prototype.setPointerCapture = vi.fn();
  Element.prototype.releasePointerCapture = vi.fn();
  Element.prototype.hasPointerCapture = vi.fn(() => false);
});
afterEach(() => {
  vi.restoreAllMocks();
});

function makeSize(overrides: Partial<PaneSize> = {}): PaneSize {
  return {
    now: 240,
    min: 150,
    max: 500,
    step: 8,
    largeStep: 32,
    format: (now) => `${now} pixels`,
    onChange: vi.fn(),
    ...overrides,
  };
}

function renderDivider(props: {
  edge: PaneEdge;
  collapsed: boolean;
  onToggle: () => void;
  resizeOverrides?: Partial<ReturnType<typeof makeResize>>;
  size?: PaneSize;
  hotkeyActionId?: string;
  hitPadding?: { start: number; end: number };
  dragHint?: string;
}) {
  return render(
    <PaneCollapseDivider
      edge={props.edge}
      collapsed={props.collapsed}
      onToggle={props.onToggle}
      hotkeyActionId={"hotkeyActionId" in props ? props.hotkeyActionId : "viewer.toggle-terminal"}
      paneLabel="sidebar"
      resize={makeResize(props.resizeOverrides)}
      size={props.size ?? makeSize()}
      hitPadding={props.hitPadding}
      dragHint={props.dragHint}
      data-testid="pcd"
    />,
  );
}

describe("PaneCollapseDivider", () => {
  describe("expanded", () => {
    it("renders a focusable window-splitter separator and a teaching tooltip", async () => {
      const user = userEvent.setup();
      const onToggle = vi.fn();
      renderDivider({ edge: "left", collapsed: false, onToggle });
      const el = screen.getByTestId("pcd");
      // Expanded, the border is an ARIA window splitter: a focusable separator
      // carrying the pane's size (aria-expanded is not a separator attribute).
      expect(el).toHaveAttribute("role", "separator");
      expect(el).toHaveAttribute("tabindex", "0");
      expect(el).toHaveAttribute("aria-orientation", "vertical");
      expect(el).toHaveAttribute("aria-valuenow", "240");
      expect(el).toHaveAttribute("aria-valuemin", "150");
      expect(el).toHaveAttribute("aria-valuemax", "500");
      expect(el).toHaveAttribute("aria-valuetext", "240 pixels");
      expect(el).not.toHaveAttribute("aria-expanded");
      expect(el).toHaveAccessibleName("Resize sidebar");
      // SW-FX-74: the expanded resize cursor is JS-armed on a genuine pointer-enter.
      fireEvent.pointerEnter(el);
      expect(el.style.cursor).toBe("col-resize");
      // SW-FX-80: the teaching copy moved from a native `title` into the shared
      // Radix `InfoTip` — it renders (portaled) once the trigger is hovered.
      await user.hover(el);
      expect(await screen.findByRole("tooltip")).toHaveTextContent(
        /click to collapse · drag to resize/i,
      );
      fireEvent.pointerLeave(el);
      await user.unhover(el);
      // The keyboard model is described at rest, not only in the hover tip.
      expect(el).toHaveAccessibleDescription(
        /click to collapse · drag to resize.*arrow keys resize.*home and end.*enter collapses/i,
      );
      // SW-FX-74: the expanded resize cursor is JS-armed (genuine pointer
      // presence) — a leave clears it.
      fireEvent.pointerLeave(el);
      expect(el.style.cursor).toBe("");
      // Icon-free cue: no chevron/icon is rendered (the hairline thickens on
      // hover instead).
      expect(el.querySelector("svg")).toBeNull();
    });

    it("collapses on a plain click (no drag)", () => {
      const onToggle = vi.fn();
      renderDivider({ edge: "left", collapsed: false, onToggle });
      fireEvent.click(screen.getByTestId("pcd"));
      expect(onToggle).toHaveBeenCalledTimes(1);
    });

    it("does NOT collapse on a drag — it resizes, and swallows the trailing click", () => {
      const onToggle = vi.fn();
      const onCommit = vi.fn();
      renderDivider({
        edge: "left",
        collapsed: false,
        onToggle,
        resizeOverrides: { onCommit },
      });
      const el = screen.getByTestId("pcd");
      // A real drag: down → move past threshold (starts the drag) → up.
      fireEvent.pointerDown(el, { pointerId: 1, clientX: 200 });
      fireEvent.pointerMove(el, { pointerId: 1, clientX: 260 });
      fireEvent.pointerUp(el, { pointerId: 1, clientX: 260 });
      // The browser emits a trailing click after the drag — it must be swallowed.
      fireEvent.click(el);
      expect(onToggle).not.toHaveBeenCalled();
      expect(onCommit).toHaveBeenCalledTimes(1);
    });

    it("self-heals: a drag that ends WITHOUT a trailing click (pointercancel) does not swallow the next click", () => {
      const onToggle = vi.fn();
      let t = 1000;
      const nowSpy = vi.spyOn(performance, "now").mockImplementation(() => t);
      renderDivider({ edge: "left", collapsed: false, onToggle });
      const el = screen.getByTestId("pcd");
      // Drag aborts via pointercancel — no trailing click follows.
      fireEvent.pointerDown(el, { pointerId: 1, clientX: 200 });
      fireEvent.pointerMove(el, { pointerId: 1, clientX: 260 });
      fireEvent.pointerCancel(el, { pointerId: 1, clientX: 260 });
      // 300ms later a genuine click must toggle — not be swallowed by a stuck flag.
      t = 1300;
      fireEvent.click(el);
      expect(onToggle).toHaveBeenCalledTimes(1);
      nowSpy.mockRestore();
    });
  });

  describe("collapsed", () => {
    it("renders the kept border with aria-expanded=false, pointer cursor, and a grab handle", () => {
      const onToggle = vi.fn();
      renderDivider({ edge: "left", collapsed: true, onToggle });
      const el = screen.getByTestId("pcd");
      // Collapsed there is nothing to size: the border is an expand button.
      expect(el).toHaveAttribute("role", "button");
      expect(el).not.toHaveAttribute("aria-valuenow");
      expect(el).toHaveAttribute("aria-expanded", "false");
      // SW-FX-74: collapsed cursor is applied inline (click to expand), not via
      // a Tailwind class.
      expect(el.style.cursor).toBe("pointer");
      // SW-FX-80: the collapsed tip mirrors the accessible name (Expand …).
      expect(el.getAttribute("aria-label")).toMatch(/expand sidebar/i);
      // Grab handle (rounded pill) present so the collapsed edge is discoverable.
      expect(el.querySelector(".rounded-full")).not.toBeNull();
      // Still icon-free.
      expect(el.querySelector("svg")).toBeNull();
    });

    it("does not render a grab handle when expanded (resize hairline only)", () => {
      renderDivider({ edge: "left", collapsed: false, onToggle: vi.fn() });
      expect(screen.getByTestId("pcd").querySelector(".rounded-full")).toBeNull();
    });

    it("expands on a click anywhere on the border", () => {
      const onToggle = vi.fn();
      renderDivider({ edge: "right", collapsed: true, onToggle });
      fireEvent.click(screen.getByTestId("pcd"));
      expect(onToggle).toHaveBeenCalledTimes(1);
    });
  });

  describe("keyboard a11y", () => {
    it("toggles on Enter and Space", () => {
      const onToggle = vi.fn();
      renderDivider({ edge: "top", collapsed: false, onToggle });
      const el = screen.getByTestId("pcd");
      fireEvent.keyDown(el, { key: "Enter" });
      fireEvent.keyDown(el, { key: " " });
      expect(onToggle).toHaveBeenCalledTimes(2);
    });

    // The arrow that moves the border AWAY from the pane grows it, whichever side
    // of its pane the divider sits on.
    it.each([
      ["left", "ArrowRight", "ArrowLeft"],
      ["right", "ArrowLeft", "ArrowRight"],
      ["top", "ArrowUp", "ArrowDown"],
    ] as const)(
      "%s edge: %s grows the pane, %s shrinks it, Shift takes a large step",
      (edge, grow, shrink) => {
        const onChange = vi.fn();
        renderDivider({ edge, collapsed: false, onToggle: vi.fn(), size: makeSize({ onChange }) });
        const el = screen.getByTestId("pcd");
        fireEvent.keyDown(el, { key: grow });
        expect(onChange).toHaveBeenLastCalledWith(248);
        fireEvent.keyDown(el, { key: shrink });
        expect(onChange).toHaveBeenLastCalledWith(232);
        fireEvent.keyDown(el, { key: grow, shiftKey: true });
        expect(onChange).toHaveBeenLastCalledWith(272);
      },
    );

    it("ignores the cross-axis arrows", () => {
      const onChange = vi.fn();
      renderDivider({
        edge: "left",
        collapsed: false,
        onToggle: vi.fn(),
        size: makeSize({ onChange }),
      });
      fireEvent.keyDown(screen.getByTestId("pcd"), { key: "ArrowUp" });
      expect(onChange).not.toHaveBeenCalled();
    });

    it("Home and End jump to the pane's minimum and maximum", () => {
      const onChange = vi.fn();
      renderDivider({
        edge: "right",
        collapsed: false,
        onToggle: vi.fn(),
        size: makeSize({ onChange }),
      });
      const el = screen.getByTestId("pcd");
      fireEvent.keyDown(el, { key: "Home" });
      expect(onChange).toHaveBeenLastCalledWith(150);
      fireEvent.keyDown(el, { key: "End" });
      expect(onChange).toHaveBeenLastCalledWith(500);
    });

    it("clamps an arrow step to the pane's range", () => {
      const onChange = vi.fn();
      renderDivider({
        edge: "left",
        collapsed: false,
        onToggle: vi.fn(),
        size: makeSize({ now: 495, onChange }),
      });
      fireEvent.keyDown(screen.getByTestId("pcd"), { key: "ArrowRight", shiftKey: true });
      expect(onChange).toHaveBeenLastCalledWith(500);
    });

    it("never moves the wrong way when the saved size is outside the range", () => {
      const onChange = vi.fn();
      // e.g. a nav width saved on a wider window than today's maximum.
      renderDivider({
        edge: "left",
        collapsed: false,
        onToggle: vi.fn(),
        size: makeSize({ now: 640, onChange }),
      });
      const el = screen.getByTestId("pcd");
      // Announced inside the range.
      expect(el).toHaveAttribute("aria-valuenow", "500");
      // Growing past the max is a no-op, not a jump back down to it…
      fireEvent.keyDown(el, { key: "ArrowRight" });
      expect(onChange).not.toHaveBeenCalled();
      // …while shrinking steps down into range.
      fireEvent.keyDown(el, { key: "ArrowLeft" });
      expect(onChange).toHaveBeenLastCalledWith(500);
    });

    it("does not resize while collapsed", () => {
      const onChange = vi.fn();
      renderDivider({
        edge: "left",
        collapsed: true,
        onToggle: vi.fn(),
        size: makeSize({ onChange }),
      });
      const el = screen.getByTestId("pcd");
      fireEvent.keyDown(el, { key: "ArrowRight" });
      fireEvent.keyDown(el, { key: "End" });
      expect(onChange).not.toHaveBeenCalled();
    });

    it("shows a focus-visible ring on the hairline", () => {
      renderDivider({ edge: "left", collapsed: false, onToggle: vi.fn() });
      const line = screen.getByTestId("pcd").querySelector("[aria-hidden]");
      expect(line?.className).toMatch(/group-focus-visible:/);
    });
  });

  describe("drag highlight", () => {
    it("keeps the hairline highlighted for the whole drag, even off the divider", () => {
      vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
        cb(0);
        return 1;
      });
      renderDivider({ edge: "left", collapsed: false, onToggle: vi.fn() });
      const el = screen.getByTestId("pcd");
      expect(el).not.toHaveAttribute("data-dragging");
      try {
        fireEvent.pointerDown(el, { pointerId: 1, clientX: 200 });
        fireEvent.pointerMove(el, { pointerId: 1, clientX: 260 });
        expect(el).toHaveAttribute("data-dragging", "true");
        fireEvent.pointerUp(el, { pointerId: 1, clientX: 260 });
        expect(el).not.toHaveAttribute("data-dragging");
      } finally {
        vi.unstubAllGlobals();
      }
    });
  });

  describe("hotkey hint", () => {
    it("shows the live toggle binding, and follows a rebind", async () => {
      const user = userEvent.setup();
      renderDivider({ edge: "top", collapsed: true, onToggle: vi.fn() });
      const el = screen.getByTestId("pcd");
      const binding = formatKeystrokeDisplay(getEffectiveShortcut("viewer.toggle-terminal"));
      expect(binding).not.toBe("");
      expect(el).toHaveAccessibleName(`Expand sidebar (${binding})`);
      await user.hover(el);
      expect(await screen.findByRole("tooltip")).toHaveTextContent(`(${binding})`);
      const prev = useShortcutsStore.getState().shortcuts;
      try {
        act(() =>
          useShortcutsStore.setState({
            shortcuts: { ...prev, "viewer.toggle-terminal": "CmdOrCtrl+Shift+Y" },
          }),
        );
        expect(el).toHaveAccessibleName(
          `Expand sidebar (${formatKeystrokeDisplay("CmdOrCtrl+Shift+Y")})`,
        );
      } finally {
        act(() => useShortcutsStore.setState({ shortcuts: prev }));
      }
    });

    it("drops the parentheses when the action is unbound", () => {
      const prev = useShortcutsStore.getState().shortcuts;
      vi.spyOn(console, "warn").mockImplementation(() => {});
      try {
        renderDivider({
          edge: "top",
          collapsed: true,
          onToggle: vi.fn(),
          hotkeyActionId: "viewer.no-such-action",
        });
        expect(screen.getByTestId("pcd")).toHaveAccessibleName("Expand sidebar");
      } finally {
        useShortcutsStore.setState({ shortcuts: prev });
      }
    });
  });

  describe("drag affordances", () => {
    function stubRaf() {
      vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
        cb(0);
        return 1;
      });
    }

    it("hides the tooltip for the duration of a drag", async () => {
      const user = userEvent.setup();
      stubRaf();
      try {
        renderDivider({ edge: "left", collapsed: false, onToggle: vi.fn() });
        const el = screen.getByTestId("pcd");
        await user.hover(el);
        expect(await screen.findByRole("tooltip")).toBeInTheDocument();
        fireEvent.pointerDown(el, { pointerId: 1, clientX: 200 });
        fireEvent.pointerMove(el, { pointerId: 1, clientX: 260 });
        await waitFor(() => expect(screen.queryByRole("tooltip")).toBeNull());
        fireEvent.pointerUp(el, { pointerId: 1, clientX: 260 });
      } finally {
        vi.unstubAllGlobals();
      }
    });

    it("announces the live size while dragging, not only after release", () => {
      stubRaf();
      try {
        renderDivider({
          edge: "left",
          collapsed: false,
          onToggle: vi.fn(),
          // The drag works in a 0–1 ratio; the pane reports percent.
          size: makeSize({ now: 50, min: 0, max: 100, fromDrag: (r) => Math.round(r * 100) }),
        });
        const el = screen.getByTestId("pcd");
        fireEvent.pointerDown(el, { pointerId: 1, clientX: 200 });
        fireEvent.pointerMove(el, { pointerId: 1, clientX: 260 });
        expect(el).toHaveAttribute("aria-valuenow", "56");
        expect(el).toHaveAttribute("aria-valuetext", "56 pixels");
        fireEvent.pointerUp(el, { pointerId: 1, clientX: 260 });
        expect(el).toHaveAttribute("aria-valuenow", "50");
      } finally {
        vi.unstubAllGlobals();
      }
    });

    it("shows a drag hint on the border, announced from outside the separator", () => {
      const props = {
        edge: "top" as PaneEdge,
        collapsed: false,
        onToggle: vi.fn(),
        paneLabel: "sidebar",
        resize: makeResize(),
        size: makeSize(),
        "data-testid": "pcd",
      };
      const { rerender } = render(<PaneCollapseDivider {...props} />);
      // The live region exists before the hint does, so the change is announced.
      const status = screen.getByRole("status");
      expect(status).toHaveTextContent("");
      rerender(<PaneCollapseDivider {...props} dragHint="Release to close" />);
      expect(status).toHaveTextContent("Release to close");
      // Separator children are presentational: the live region must not be one.
      expect(screen.getByTestId("pcd")).not.toContainElement(status);
      // The visible pill is drawn on the border, hidden from AT (the region speaks).
      const pill = screen.getByTestId("pcd").querySelector("[data-drag-hint]");
      expect(pill).toHaveTextContent("Release to close");
      expect(pill).toHaveAttribute("aria-hidden", "true");
    });

    it("can sit flush at its start edge while collapsed", () => {
      render(
        <PaneCollapseDivider
          edge="left"
          collapsed
          onToggle={vi.fn()}
          paneLabel="agents"
          resize={makeResize()}
          size={makeSize()}
          hitPadding={{ start: 4, end: 8 }}
          flushStartWhenCollapsed
          data-testid="pcd"
        />,
      );
      const el = screen.getByTestId("pcd");
      expect(el.style.marginLeft).toBe("0px");
      expect(el.style.paddingLeft).toBe("4px");
      expect(el.style.marginRight).toBe("-8px");
    });

    it("takes an asymmetric hit zone", () => {
      renderDivider({
        edge: "left",
        collapsed: false,
        onToggle: vi.fn(),
        hitPadding: { start: 5, end: 2 },
      });
      const el = screen.getByTestId("pcd");
      expect(el.style.paddingLeft).toBe("5px");
      expect(el.style.paddingRight).toBe("2px");
      expect(el.style.marginLeft).toBe("-5px");
      expect(el.style.marginRight).toBe("-2px");
    });
  });

  describe("without a hotkey", () => {
    it("teaches drag/click without a shortcut", async () => {
      const user = userEvent.setup();
      renderDivider({
        edge: "left",
        collapsed: false,
        onToggle: vi.fn(),
        hotkeyActionId: undefined,
      });
      await user.hover(screen.getByTestId("pcd"));
      const tip = await screen.findByRole("tooltip");
      expect(tip).toHaveTextContent(/click to collapse · drag to resize/i);
      expect(tip.textContent).not.toMatch(/\(/);
    });
  });

  describe("orientation", () => {
    it("arms a row-resize cursor on genuine hover for the top edge (horizontal divider)", () => {
      renderDivider({ edge: "top", collapsed: false, onToggle: vi.fn() });
      // SW-FX-74: JS-armed inline cursor, not a CSS `:hover` class.
      const el = screen.getByTestId("pcd");
      expect(el.style.cursor).toBe("");
      fireEvent.pointerEnter(el);
      expect(el.style.cursor).toBe("row-resize");
    });

    it("expanded top edge straddles the boundary with negative margins (resize hit area)", () => {
      renderDivider({ edge: "top", collapsed: false, onToggle: vi.fn() });
      expect(screen.getByTestId("pcd").style.marginTop).toBe("-7px");
    });

    it("collapsed top edge sits flush (no negative margin) so it can't steal the viewer's bottom pixels", () => {
      renderDivider({ edge: "top", collapsed: true, onToggle: vi.fn() });
      const mt = screen.getByTestId("pcd").style.marginTop;
      expect(mt === "0px" || mt === "0" || mt === "").toBe(true);
    });
  });

  describe("armed-cursor lifecycle (SW-FX-74)", () => {
    // The arm handlers are wired in BOTH collapsed and expanded states so a
    // collapse toggle can't strand a stale `armed=true` that would show a resize
    // cursor at rest after re-expanding.
    function props(collapsed: boolean) {
      return {
        edge: "left" as PaneEdge,
        collapsed,
        onToggle: vi.fn(),
        hotkeyActionId: "viewer.toggle-terminal",
        paneLabel: "sidebar",
        resize: makeResize(),
        size: makeSize(),
        "data-testid": "pcd",
      };
    }

    it("clears armed across a collapse → leave → expand cycle (no stale resize cursor)", () => {
      const { rerender } = render(<PaneCollapseDivider {...props(false)} />);
      const el = screen.getByTestId("pcd");
      // Genuinely hover the expanded divider → armed.
      fireEvent.pointerEnter(el);
      expect(el.style.cursor).toBe("col-resize");
      // Collapse (e.g. via hotkey) while the pointer still rests on it.
      rerender(<PaneCollapseDivider {...props(true)} />);
      expect(el.style.cursor).toBe("pointer");
      // Pointer leaves the collapsed divider — leave is wired even when
      // collapsed, so armed clears.
      fireEvent.pointerLeave(el);
      // Re-expand with the pointer elsewhere → no stale resize cursor.
      rerender(<PaneCollapseDivider {...props(false)} />);
      expect(el.style.cursor).toBe("");
    });

    it("arms while collapsed so re-expanding under the pointer shows the resize cursor immediately", () => {
      const { rerender } = render(<PaneCollapseDivider {...props(true)} />);
      const el = screen.getByTestId("pcd");
      // Hover the collapsed divider (shows pointer, but pre-arms).
      fireEvent.pointerEnter(el);
      expect(el.style.cursor).toBe("pointer");
      // Expand with the pointer still on it → resize cursor without a fresh move.
      rerender(<PaneCollapseDivider {...props(false)} />);
      expect(el.style.cursor).toBe("col-resize");
    });
  });
});
