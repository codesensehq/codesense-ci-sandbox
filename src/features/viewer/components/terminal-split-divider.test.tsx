// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, act } from "@/test-utils";
import { useRef, useState } from "react";
import { TerminalSplitDivider } from "./terminal-split-divider";

const { mockSetTerminalSplitRatio, getStoreState } = vi.hoisted(() => {
  const setter = vi.fn();
  return {
    mockSetTerminalSplitRatio: setter,
    getStoreState: () => ({
      terminalSplitRatio: 0.55,
      setTerminalSplitRatio: setter,
    }),
  };
});

vi.mock("@/shared/stores/viewer-store", () => ({
  useViewerStore: Object.assign((selector: (s: unknown) => unknown) => selector(getStoreState()), {
    subscribe: () => () => {},
    getState: getStoreState,
  }),
}));

// Mount harness: a parent <div> whose height is locked to 1000px so the
// hook's getContainerSize returns a stable, predictable value. The same
// parent receives the TERM_SPLIT_VAR writes (since `leftColumnRef` is
// what the divider component points at).
function Harness() {
  const leftColumnRef = useRef<HTMLDivElement>(null);
  return (
    <div ref={leftColumnRef} style={{ height: 1000 }}>
      <TerminalSplitDivider
        leftColumnRef={leftColumnRef}
        viewerRatio={0.55}
        collapsed={false}
        onToggle={() => {}}
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
  mockSetTerminalSplitRatio.mockReset();
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
    height: 1000,
    width: 500,
    top: 0,
    left: 0,
    right: 500,
    bottom: 1000,
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

describe("TerminalSplitDivider", () => {
  it("renders with the canonical test id and a JS-armed resize cursor", () => {
    render(<Harness />);
    const divider = screen.getByTestId("terminal-drag-handle");
    // SW-FX-74: the resize cursor is armed on a genuine pointer-enter and applied
    // inline (the window-wide cursor during a drag comes from the global body
    // `dragCursor`), not a CSS `:hover` class a stranded capture can re-assert.
    expect(divider.style.cursor).toBe("");
    fireEvent.pointerEnter(divider);
    expect(divider.style.cursor).toBe("row-resize");
    fireEvent.pointerLeave(divider);
    expect(divider.style.cursor).toBe("");
  });

  it("does NOT call setTerminalSplitRatio during pointermove", () => {
    render(<Harness />);
    const divider = screen.getByTestId("terminal-drag-handle");

    fireEvent.pointerDown(divider, { pointerId: 1, clientY: 500 });
    fireEvent.pointerMove(divider, { pointerId: 1, clientY: 520 });
    fireEvent.pointerMove(divider, { pointerId: 1, clientY: 540 });
    fireEvent.pointerMove(divider, { pointerId: 1, clientY: 600 });
    flushFrame();

    expect(mockSetTerminalSplitRatio).not.toHaveBeenCalled();
  });

  it("calls setTerminalSplitRatio exactly once on pointerup with the clamped final ratio", () => {
    render(<Harness />);
    const divider = screen.getByTestId("terminal-drag-handle");

    // startRatio=0.55, drag DOWN 100px on a 1000px container, no invertDelta
    // → 0.55 + 100/1000 = 0.65. Stays under the 0.9 ratio maximum.
    fireEvent.pointerDown(divider, { pointerId: 1, clientY: 500 });
    fireEvent.pointerMove(divider, { pointerId: 1, clientY: 600 });
    fireEvent.pointerUp(divider, { pointerId: 1, clientY: 600 });

    expect(mockSetTerminalSplitRatio).toHaveBeenCalledTimes(1);
    expect(mockSetTerminalSplitRatio).toHaveBeenCalledWith(0.65);
  });

  it("zero-movement pointerup does not call setTerminalSplitRatio", () => {
    render(<Harness />);
    const divider = screen.getByTestId("terminal-drag-handle");

    fireEvent.pointerDown(divider, { pointerId: 1, clientY: 500 });
    fireEvent.pointerUp(divider, { pointerId: 1, clientY: 500 });

    expect(mockSetTerminalSplitRatio).not.toHaveBeenCalled();
  });

  it("adds and removes the global `.dragging` class around the drag", () => {
    render(<Harness />);
    const divider = screen.getByTestId("terminal-drag-handle");

    expect(document.documentElement.classList.contains("dragging")).toBe(false);

    // SW-FX-8: onDragStart (and the `.dragging` class) is deferred until
    // movement crosses the tap threshold — a bare pointer-down is a
    // potential tap, so the class must NOT appear yet.
    fireEvent.pointerDown(divider, { pointerId: 1, clientY: 500 });
    expect(document.documentElement.classList.contains("dragging")).toBe(false);

    // Crossing the threshold starts the drag → class appears.
    fireEvent.pointerMove(divider, { pointerId: 1, clientY: 540 });
    expect(document.documentElement.classList.contains("dragging")).toBe(true);

    fireEvent.pointerUp(divider, { pointerId: 1, clientY: 540 });
    expect(document.documentElement.classList.contains("dragging")).toBe(false);
  });

  it("measures the split without the workspace bar below it, so the divider tracks the pointer", () => {
    function WithBar() {
      const leftColumnRef = useRef<HTMLDivElement>(null);
      return (
        <div ref={leftColumnRef}>
          <TerminalSplitDivider
            leftColumnRef={leftColumnRef}
            viewerRatio={0.55}
            collapsed={false}
            onToggle={() => {}}
          />
          <div data-workspace-bar="" />
        </div>
      );
    }
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (
      this: HTMLElement,
    ) {
      const height = this.hasAttribute("data-workspace-bar") ? 200 : 1000;
      return {
        height,
        width: 500,
        top: 0,
        left: 0,
        right: 500,
        bottom: height,
        x: 0,
        y: 0,
      } as DOMRect;
    });
    render(<WithBar />);
    const divider = screen.getByTestId("terminal-drag-handle");
    // The viewer + terminal share 1000 − 200 = 800px: 100px down → 0.55 + 100/800.
    fireEvent.pointerDown(divider, { pointerId: 1, clientY: 500 });
    fireEvent.pointerMove(divider, { pointerId: 1, clientY: 600 });
    fireEvent.pointerUp(divider, { pointerId: 1, clientY: 600 });
    expect(mockSetTerminalSplitRatio).toHaveBeenCalledWith(0.675);
  });

  describe("keyboard + a11y (same handle as every pane divider)", () => {
    it("is a horizontal window splitter carrying the terminal's share", () => {
      render(<Harness />);
      const divider = screen.getByRole("separator", { name: "Resize terminal" });
      expect(divider).toHaveAttribute("aria-orientation", "horizontal");
      // viewer ratio 0.55 → the terminal holds 45%.
      expect(divider).toHaveAttribute("aria-valuenow", "45");
      expect(divider).toHaveAttribute("aria-valuemin", "10");
      expect(divider).toHaveAttribute("aria-valuemax", "90");
      expect(divider).toHaveAttribute("aria-valuetext", "Terminal 45% of the column");
    });

    it("ArrowUp grows the terminal, ArrowDown shrinks it, Home/End jump to the ends", () => {
      render(<Harness />);
      const divider = screen.getByRole("separator", { name: "Resize terminal" });
      fireEvent.keyDown(divider, { key: "ArrowUp" });
      expect(mockSetTerminalSplitRatio).toHaveBeenLastCalledWith(0.53);
      fireEvent.keyDown(divider, { key: "ArrowDown", shiftKey: true });
      expect(mockSetTerminalSplitRatio).toHaveBeenLastCalledWith(0.65);
      fireEvent.keyDown(divider, { key: "Home" });
      expect(mockSetTerminalSplitRatio).toHaveBeenLastCalledWith(0.9);
      fireEvent.keyDown(divider, { key: "End" });
      expect(mockSetTerminalSplitRatio).toHaveBeenLastCalledWith(0.1);
    });
  });

  describe("drag past the floor collapses the CLI", () => {
    function CollapsibleHarness({ onToggle }: { onToggle: () => void }) {
      const leftColumnRef = useRef<HTMLDivElement>(null);
      return (
        <div ref={leftColumnRef} data-testid="column">
          <TerminalSplitDivider
            leftColumnRef={leftColumnRef}
            viewerRatio={0.55}
            collapsed={false}
            onToggle={onToggle}
          />
        </div>
      );
    }

    it("previews the collapse while dragged closed and collapses on release, keeping the saved height", () => {
      const onToggle = vi.fn();
      render(<CollapsibleHarness onToggle={onToggle} />);
      const divider = screen.getByTestId("terminal-drag-handle");
      // Terminal starts at 450px of 1000; 420px down leaves 30px — under the snap point.
      fireEvent.pointerDown(divider, { pointerId: 1, clientY: 550 });
      fireEvent.pointerMove(divider, { pointerId: 1, clientY: 970 });
      flushFrame();
      // The preview IS the closed state: the viewer takes the whole column.
      expect(screen.getByTestId("column").style.getPropertyValue("--cs-term-split")).toBe("1");
      fireEvent.pointerUp(divider, { pointerId: 1, clientY: 970 });
      expect(onToggle).toHaveBeenCalledTimes(1);
      // The last height is kept, so reopening restores it.
      expect(mockSetTerminalSplitRatio).not.toHaveBeenCalled();
    });

    it("stops at the ratio limit without collapsing when not dragged past the snap point", () => {
      const onToggle = vi.fn();
      render(<CollapsibleHarness onToggle={onToggle} />);
      const divider = screen.getByTestId("terminal-drag-handle");
      // 390px down leaves 60px: below the 80px floor but above the snap point.
      fireEvent.pointerDown(divider, { pointerId: 1, clientY: 550 });
      fireEvent.pointerMove(divider, { pointerId: 1, clientY: 940 });
      fireEvent.pointerUp(divider, { pointerId: 1, clientY: 940 });
      expect(onToggle).not.toHaveBeenCalled();
      expect(mockSetTerminalSplitRatio).toHaveBeenCalledWith(0.9);
    });
  });

  describe("review hardening", () => {
    function Column({ height, onToggle = () => {} }: { height: number; onToggle?: () => void }) {
      const leftColumnRef = useRef<HTMLDivElement>(null);
      vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
        height,
        width: 500,
        top: 0,
        left: 0,
        right: 500,
        bottom: height,
        x: 0,
        y: 0,
        toJSON: () => "",
      });
      return (
        <div ref={leftColumnRef} data-testid="column">
          <TerminalSplitDivider
            leftColumnRef={leftColumnRef}
            viewerRatio={0.55}
            collapsed={false}
            onToggle={onToggle}
          />
        </div>
      );
    }

    it("an aborted drag (pointercancel) in the snap zone neither collapses nor saves", () => {
      const onToggle = vi.fn();
      render(<Column height={1000} onToggle={onToggle} />);
      const divider = screen.getByTestId("terminal-drag-handle");
      fireEvent.pointerDown(divider, { pointerId: 1, clientY: 550 });
      fireEvent.pointerMove(divider, { pointerId: 1, clientY: 970 });
      fireEvent.pointerCancel(divider, { pointerId: 1, clientY: 970 });
      expect(onToggle).not.toHaveBeenCalled();
      expect(mockSetTerminalSplitRatio).not.toHaveBeenCalled();
    });

    it("keyboard keeps the 80px floor on a short column", () => {
      render(<Column height={300} />);
      const divider = screen.getByRole("separator", { name: "Resize terminal" });
      // End asks for a 90% terminal; the viewer keeps 80 of 300px.
      fireEvent.keyDown(divider, { key: "End" });
      const [ratio] = mockSetTerminalSplitRatio.mock.lastCall!;
      expect(ratio).toBeCloseTo(80 / 300, 4);
    });

    it("keyboard leaves a column too short for both floors alone", () => {
      render(<Column height={150} />);
      const divider = screen.getByRole("separator", { name: "Resize terminal" });
      fireEvent.keyDown(divider, { key: "ArrowUp" });
      fireEvent.keyDown(divider, { key: "Home" });
      expect(mockSetTerminalSplitRatio).not.toHaveBeenCalled();
    });

    it("a terminal already under the snap point is not collapsed by a drag that grows it", () => {
      const onToggle = vi.fn();
      // 0.55 of a 60px column leaves the terminal 27px — already in the snap zone.
      render(<Column height={60} onToggle={onToggle} />);
      const divider = screen.getByTestId("terminal-drag-handle");
      fireEvent.pointerDown(divider, { pointerId: 1, clientY: 33 });
      // 5px up: still under the snap point, but growing — never a collapse.
      fireEvent.pointerMove(divider, { pointerId: 1, clientY: 28 });
      fireEvent.pointerUp(divider, { pointerId: 1, clientY: 28 });
      expect(onToggle).not.toHaveBeenCalled();
    });
  });

  describe("snap-closed release", () => {
    function Stateful({ onToggle }: { onToggle: () => void }) {
      const leftColumnRef = useRef<HTMLDivElement>(null);
      const [collapsed, setCollapsed] = useState(false);
      return (
        <div ref={leftColumnRef}>
          <TerminalSplitDivider
            leftColumnRef={leftColumnRef}
            viewerRatio={0.55}
            collapsed={collapsed}
            onToggle={() => {
              onToggle();
              setCollapsed((c) => !c);
            }}
          />
        </div>
      );
    }

    it("the browser's trailing click after a snap-closed release leaves the CLI collapsed", () => {
      const onToggle = vi.fn();
      render(<Stateful onToggle={onToggle} />);
      const divider = screen.getByTestId("terminal-drag-handle");
      fireEvent.pointerDown(divider, { pointerId: 1, clientY: 550 });
      fireEvent.pointerMove(divider, { pointerId: 1, clientY: 970 });
      fireEvent.pointerUp(divider, { pointerId: 1, clientY: 970 });
      fireEvent.click(divider);
      expect(onToggle).toHaveBeenCalledTimes(1);
      expect(divider).toHaveAttribute("aria-expanded", "false");
    });

    it("says 'Release to close' only while the drag is in the snap zone", () => {
      render(<Stateful onToggle={vi.fn()} />);
      const divider = screen.getByTestId("terminal-drag-handle");
      fireEvent.pointerDown(divider, { pointerId: 1, clientY: 550 });
      fireEvent.pointerMove(divider, { pointerId: 1, clientY: 700 });
      act(() => flushFrame());
      expect(screen.queryByText(/release to close/i)).toBeNull();
      fireEvent.pointerMove(divider, { pointerId: 1, clientY: 970 });
      act(() => flushFrame());
      expect(screen.getByRole("status")).toHaveTextContent(/release to close/i);
      // Back out of the zone: the hint goes.
      fireEvent.pointerMove(divider, { pointerId: 1, clientY: 700 });
      act(() => flushFrame());
      expect(screen.queryByText(/release to close/i)).toBeNull();
      fireEvent.pointerUp(divider, { pointerId: 1, clientY: 700 });
    });
  });
});
