// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, act } from "@/test-utils";
import { useRef } from "react";
import { ScratchNoteWidthDivider } from "./scratch-note-width-divider";
import { usePaneStore } from "@/features/shell/right-pane/panes/pane-store";
import { registerPanel, usePanelRegistry } from "@/features/shell/right-pane/panel-registry";
import { Inbox } from "lucide-react";

const { mockSetScratchNoteRatio, getStoreState } = vi.hoisted(() => {
  const setter = vi.fn();
  return {
    mockSetScratchNoteRatio: setter,
    getStoreState: () => ({
      rightPaneRatio: 0.33,
      setRightPaneRatio: setter,
    }),
  };
});

vi.mock("@/shared/stores/viewer-store", () => ({
  useViewerStore: Object.assign((selector: (s: unknown) => unknown) => selector(getStoreState()), {
    subscribe: () => () => {},
    getState: getStoreState,
  }),
}));

function Harness({ collapsed = false, onToggle = () => {} } = {}) {
  const containerRef = useRef<HTMLDivElement>(null);
  return (
    <div ref={containerRef} style={{ width: 1000 }}>
      <ScratchNoteWidthDivider
        containerRef={containerRef}
        collapsed={collapsed}
        onToggle={onToggle}
      />
    </div>
  );
}

let rafQueue: Array<() => void> = [];

// Save originals so afterEach can restore the prototype to its pristine
// happy-dom state — otherwise a later test that asserts real layout
// sees these canned values across the worker.
const originalSetPointerCapture = Element.prototype.setPointerCapture;
const originalReleasePointerCapture = Element.prototype.releasePointerCapture;

beforeEach(() => {
  mockSetScratchNoteRatio.mockReset();
  usePaneStore.setState({ badges: {} });
  usePanelRegistry.setState({ panels: {} });
  rafQueue = [];
  vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
    rafQueue.push(() => cb(performance.now()));
    return rafQueue.length;
  });
  vi.stubGlobal("cancelAnimationFrame", () => {
    rafQueue.pop();
  });
  Element.prototype.setPointerCapture = vi.fn();
  Element.prototype.releasePointerCapture = vi.fn();
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
    height: 600,
    width: 1000,
    top: 0,
    left: 0,
    right: 1000,
    bottom: 600,
    x: 0,
    y: 0,
    toJSON: () => "",
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  Element.prototype.setPointerCapture = originalSetPointerCapture;
  Element.prototype.releasePointerCapture = originalReleasePointerCapture;
  document.documentElement.classList.remove("dragging");
});

function flushFrame() {
  const pending = rafQueue;
  rafQueue = [];
  for (const cb of pending) cb();
}

describe("ScratchNoteWidthDivider", () => {
  it("sums any pane's badges on the closed edge, names them, and clears at zero", () => {
    const onToggle = vi.fn();
    registerPanel({ id: "mail", title: "Inbox", icon: Inbox, render: () => null });
    registerPanel({ id: "work", title: "Reviews", icon: Inbox, render: () => null });
    const { rerender } = render(<Harness collapsed onToggle={onToggle} />);
    const edge = screen.getByTestId("scratch-note-width-divider");
    expect(edge.querySelector("[data-badge-count]")).toBeNull();
    act(() => {
      usePaneStore.getState().setBadge("mail", { count: 3, severity: "info" });
      usePaneStore.getState().setBadge("work", { count: 2, severity: "info" });
    });
    expect(edge).toHaveAttribute(
      "aria-label",
      expect.stringContaining(
        "Open right pane: Inbox 3 unread (last known; open to refresh), Reviews 2 unread (last known; open to refresh).",
      ),
    );
    act(() =>
      usePaneStore.setState({
        knownEntries: {
          mail: {
            id: "inbox",
            panelId: "mail",
            title: "Inbox",
            scope: "private",
            path: "/inbox.html",
            panesDir: "/",
            declaration: { slots: [], capabilities: [], badge: "live" },
          },
        },
      }),
    );
    expect(edge.getAttribute("aria-label")).toContain(
      "Inbox 3 unread, Reviews 2 unread (last known; open to refresh)",
    );
    expect(edge.querySelector("[data-badge-count]")).toHaveAttribute("data-badge-count", "5");
    fireEvent.keyDown(edge, { key: "Enter" });
    expect(onToggle).toHaveBeenCalledOnce();
    rerender(<Harness collapsed={false} onToggle={onToggle} />);
    expect(edge.querySelector("[data-badge-count]")).toBeNull();
    rerender(<Harness collapsed onToggle={onToggle} />);
    act(() => usePaneStore.getState().setBadge("mail", null));
    expect(edge.querySelector("[data-badge-count]")).toHaveAttribute("data-badge-count", "2");
    expect(edge).toHaveAttribute("aria-label", expect.stringContaining("Reviews 2 unread"));
    act(() => usePaneStore.getState().setBadge("work", null));
    expect(edge.querySelector("[data-badge-count]")).toBeNull();
    expect(edge.getAttribute("aria-label")).not.toContain("unread");
  });

  it("renders with a JS-armed col-resize cursor and SF-7 test id", () => {
    render(<Harness />);
    const divider = screen.getByTestId("scratch-note-width-divider");
    // SW-FX-74: the resize cursor is armed on a genuine pointer-enter and applied
    // inline, not via a CSS `:hover` class a stranded capture can re-assert.
    expect(divider.style.cursor).toBe("");
    fireEvent.pointerEnter(divider);
    expect(divider.style.cursor).toBe("col-resize");
    fireEvent.pointerLeave(divider);
    expect(divider.style.cursor).toBe("");
  });

  it("does NOT call setRightPaneRatio during pointermove", () => {
    render(<Harness />);
    const divider = screen.getByTestId("scratch-note-width-divider");

    fireEvent.pointerDown(divider, { pointerId: 1, clientX: 700 });
    fireEvent.pointerMove(divider, { pointerId: 1, clientX: 680 });
    fireEvent.pointerMove(divider, { pointerId: 1, clientX: 650 });
    fireEvent.pointerMove(divider, { pointerId: 1, clientX: 600 });
    flushFrame();

    expect(mockSetScratchNoteRatio).not.toHaveBeenCalled();
  });

  it("calls setRightPaneRatio exactly once on pointerup with the clamped final ratio", () => {
    render(<Harness />);
    const divider = screen.getByTestId("scratch-note-width-divider");

    // startRatio=0.33, container=1000px, invertDelta=true. Dragging LEFT
    // (clientX decreases by 100) increases the scratch ratio by 100/1000
    // → 0.33 + 0.10 = 0.43. Under maxRatio=0.5 and above min (0.15).
    fireEvent.pointerDown(divider, { pointerId: 1, clientX: 700 });
    fireEvent.pointerMove(divider, { pointerId: 1, clientX: 600 });
    fireEvent.pointerUp(divider, { pointerId: 1, clientX: 600 });

    expect(mockSetScratchNoteRatio).toHaveBeenCalledTimes(1);
    const [actualRatio] = mockSetScratchNoteRatio.mock.calls[0];
    expect(actualRatio).toBeCloseTo(0.43, 5);
  });

  it("clamps at maxRatio=0.5 when dragging far left", () => {
    render(<Harness />);
    const divider = screen.getByTestId("scratch-note-width-divider");

    // Drag LEFT all the way → would push the ratio well past 0.5; clamp
    // must hold at 0.5.
    fireEvent.pointerDown(divider, { pointerId: 1, clientX: 700 });
    fireEvent.pointerMove(divider, { pointerId: 1, clientX: 0 });
    fireEvent.pointerUp(divider, { pointerId: 1, clientX: 0 });

    expect(mockSetScratchNoteRatio).toHaveBeenCalledWith(0.5);
  });

  it("zero-movement pointerup does not call setRightPaneRatio", () => {
    render(<Harness />);
    const divider = screen.getByTestId("scratch-note-width-divider");

    fireEvent.pointerDown(divider, { pointerId: 1, clientX: 700 });
    fireEvent.pointerUp(divider, { pointerId: 1, clientX: 700 });

    expect(mockSetScratchNoteRatio).not.toHaveBeenCalled();
  });

  it("adds and removes the global `.dragging` class around the drag", () => {
    render(<Harness />);
    const divider = screen.getByTestId("scratch-note-width-divider");

    expect(document.documentElement.classList.contains("dragging")).toBe(false);

    // SW-FX-8: onDragStart (and the `.dragging` class) is deferred until
    // movement crosses the tap threshold — bare pointer-down is a potential
    // tap, so the class must NOT appear yet.
    fireEvent.pointerDown(divider, { pointerId: 1, clientX: 700 });
    expect(document.documentElement.classList.contains("dragging")).toBe(false);

    // Crossing the threshold starts the drag → class appears.
    fireEvent.pointerMove(divider, { pointerId: 1, clientX: 650 });
    expect(document.documentElement.classList.contains("dragging")).toBe(true);

    fireEvent.pointerUp(divider, { pointerId: 1, clientX: 650 });
    expect(document.documentElement.classList.contains("dragging")).toBe(false);
  });

  it("is a window splitter: arrows resize the notes pane, Home/End jump to its range", () => {
    render(<Harness />);
    const divider = screen.getByRole("separator", { name: "Resize right pane" });
    expect(divider).toHaveAttribute("aria-orientation", "vertical");
    expect(divider).toHaveAttribute("aria-valuenow", "33");
    expect(divider).toHaveAttribute("aria-valuemin", "15");
    expect(divider).toHaveAttribute("aria-valuemax", "50");
    // The pane sits right of its border: ArrowLeft widens it.
    fireEvent.keyDown(divider, { key: "ArrowLeft" });
    expect(mockSetScratchNoteRatio).toHaveBeenLastCalledWith(0.35);
    fireEvent.keyDown(divider, { key: "ArrowRight", shiftKey: true });
    expect(mockSetScratchNoteRatio).toHaveBeenLastCalledWith(0.23);
    fireEvent.keyDown(divider, { key: "Home" });
    expect(mockSetScratchNoteRatio).toHaveBeenLastCalledWith(0.15);
    fireEvent.keyDown(divider, { key: "End" });
    expect(mockSetScratchNoteRatio).toHaveBeenLastCalledWith(0.5);
  });
});
