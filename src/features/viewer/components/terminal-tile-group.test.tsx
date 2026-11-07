// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { reconcileSizes, TerminalTileGroup } from "./terminal-tile-group";

// SW-EW-3 Task 2: the reusable tiling primitive. We test MY composition
// (N panels, reconciled sizes) by mocking the react-resizable-panels wrapper
// down to plain divs — the library's drag/ResizeObserver internals aren't
// available under happy-dom and aren't ours to test.
const resizeProbe = vi.hoisted(() => ({
  changed: null as ((layout: Record<string, number>) => void) | null,
}));
vi.mock("@/components/ui/resizable", () => ({
  ResizablePanelGroup: ({
    children,
    orientation,
    onLayoutChanged,
    defaultLayout,
    onPointerDownCapture,
  }: any) => {
    resizeProbe.changed = onLayoutChanged;
    return (
      <div
        onPointerDownCapture={onPointerDownCapture}
        data-testid="panel-group"
        data-direction={orientation}
        data-onlayout={!!onLayoutChanged}
        data-default-layout={JSON.stringify(defaultLayout)}
      >
        {children}
      </div>
    );
  },
  // Forward ...rest so the test sees `data-zoom` (the real lib spreads unknown
  // props onto the OUTER [data-panel] div). Drop `minSize` to avoid a DOM-attr
  // warning on the mock div.
  ResizablePanel: ({ children, id, className, minSize, ...rest }: any) => (
    <div data-testid={`panel-${id}`} data-min-size={minSize} className={className} {...rest}>
      {children}
    </div>
  ),
  ResizableHandle: ({ className }: any) => <div data-testid="handle" className={className} />,
}));

describe("reconcileSizes (R-3 — no stale/desynced sizes)", () => {
  it("even-splits when no persisted sizes", () => {
    expect(reconcileSizes([], 2)).toEqual([50, 50]);
    expect(reconcileSizes([], 4)).toEqual([25, 25, 25, 25]);
  });

  it("even-splits when persisted length != tile count (add/remove)", () => {
    // had 2 tiles persisted, now 3 → discard stale, even-split
    expect(reconcileSizes([60, 40], 3)).toEqual([100 / 3, 100 / 3, 100 / 3]);
    // had 3, now 2 (removed middle) → even-split, never a gap
    expect(reconcileSizes([33, 33, 34], 2)).toEqual([50, 50]);
  });

  it("normalizes persisted sizes to sum 100 when length matches", () => {
    expect(reconcileSizes([30, 30], 2)).toEqual([50, 50]);
    const r = reconcileSizes([60, 40], 2);
    expect(r[0]).toBeCloseTo(60);
    expect(r[1]).toBeCloseTo(40);
    expect(r[0] + r[1]).toBeCloseTo(100);
  });

  it("returns [] for zero tiles (no empty cell)", () => {
    expect(reconcileSizes([50, 50], 0)).toEqual([]);
  });
});

describe("TerminalTileGroup", () => {
  const tiles = [{ key: "ws-a" }, { key: "ws-b" }, { key: "ws-c" }];

  it("renders one panel per tile, the group seeded with the reconciled sizes", () => {
    render(
      <TerminalTileGroup
        tiles={tiles}
        direction="row"
        sizes={[]}
        onSizesCommit={vi.fn()}
        renderTile={(t) => <span>{t.key}</span>}
      />,
    );
    const third = 100 / 3;
    expect(
      JSON.parse(screen.getByTestId("panel-group").getAttribute("data-default-layout")!),
    ).toEqual({ "ws-a": third, "ws-b": third, "ws-c": third });
    // A percent floor is a "%" string (react-resizable-panels reads a bare number
    // as PIXELS); a pixel floor is a bare number.
    expect(screen.getByTestId("panel-ws-a")).toHaveAttribute("data-min-size", "10%");
    expect(screen.getByTestId("panel-ws-b")).toBeInTheDocument();
    expect(screen.getByTestId("panel-ws-c")).toBeInTheDocument();
    // N-1 handles between N tiles
    expect(screen.getAllByTestId("handle")).toHaveLength(2);
  });

  it("a pixel floor replaces the percent one", () => {
    render(
      <TerminalTileGroup
        tiles={tiles}
        direction="row"
        sizes={[]}
        onSizesCommit={vi.fn()}
        minSizePx={220}
        renderTile={(t) => <span>{t.key}</span>}
      />,
    );
    expect(screen.getByTestId("panel-ws-c")).toHaveAttribute("data-min-size", "220");
  });

  it("threads direction to the panel group", () => {
    render(
      <TerminalTileGroup
        tiles={tiles}
        direction="column"
        sizes={[]}
        onSizesCommit={vi.fn()}
        renderTile={(t) => <span>{t.key}</span>}
      />,
    );
    // public "column" maps to react-resizable-panels' "vertical" axis
    expect(screen.getByTestId("panel-group")).toHaveAttribute("data-direction", "vertical");
  });

  it("renders nothing-but-empty for zero tiles (no dead cell)", () => {
    render(
      <TerminalTileGroup
        tiles={[] as { key: string }[]}
        direction="row"
        sizes={[]}
        onSizesCommit={vi.fn()}
        renderTile={(t) => <span>{t.key}</span>}
      />,
    );
    expect(screen.queryAllByTestId("handle")).toHaveLength(0);
  });
});

describe("TerminalTileGroup — SW-EW-4 zoom (D-1: CSS promotion, never remount)", () => {
  const tiles = [{ key: "ws-a" }, { key: "ws-b" }, { key: "ws-c" }];

  it("does NOT remount tiles across a zoom toggle — DOM node identity persists (R-1/R-6)", () => {
    // The load-bearing no-remount guard: a real remount would tear down the
    // xterm/PTY and lose scrollback. We can't boot a real terminal headless, but
    // we CAN assert the structural invariant that makes a remount impossible —
    // React preserves the SAME DOM nodes across the zoom toggle (same component
    // tree + stable keys). If a future refactor swapped to a conditional single-
    // tile render (unmount siblings + remount the survivor), these identities
    // would change and this test fails.
    const renderTile = (t: { key: string }) => (
      <span data-testid={`content-${t.key}`}>{t.key}</span>
    );
    const props = {
      tiles,
      direction: "row" as const,
      sizes: [],
      onSizesCommit: vi.fn(),
      renderTile,
    };
    const { rerender } = render(<TerminalTileGroup {...props} zoomedKey={null} />);
    const zoomedBefore = screen.getByTestId("content-ws-b");
    const siblingBefore = screen.getByTestId("content-ws-a");

    rerender(<TerminalTileGroup {...props} zoomedKey="ws-b" />); // zoom ON
    expect(screen.getByTestId("content-ws-b")).toBe(zoomedBefore);
    expect(screen.getByTestId("content-ws-a")).toBe(siblingBefore); // sibling kept mounted

    rerender(<TerminalTileGroup {...props} zoomedKey={null} />); // zoom OFF
    expect(screen.getByTestId("content-ws-b")).toBe(zoomedBefore);
    expect(screen.getByTestId("content-ws-a")).toBe(siblingBefore);
  });

  it("promotes the zoomed panel (data-zoom=active), collapses siblings + hides handles, keeps ALL mounted", () => {
    render(
      <TerminalTileGroup
        tiles={tiles}
        direction="row"
        sizes={[]}
        onSizesCommit={vi.fn()}
        zoomedKey="ws-b"
        renderTile={(t) => <span data-testid={`content-${t.key}`}>{t.key}</span>}
      />,
    );
    // Zoom is driven by `data-zoom` on the OUTER [data-panel] div (the element
    // that owns flex sizing) — NOT className (which the lib puts on the inner
    // content div). The CSS rules promote active → flex:1 / collapsed →
    // display:none. Every panel + its content stays in the DOM (no unmount → no
    // PTY/scrollback loss, R-1).
    expect(screen.getByTestId("panel-ws-b").getAttribute("data-zoom")).toBe("active");
    expect(screen.getByTestId("panel-ws-a").getAttribute("data-zoom")).toBe("collapsed");
    expect(screen.getByTestId("panel-ws-c").getAttribute("data-zoom")).toBe("collapsed");
    expect(screen.getByTestId("content-ws-a")).toBeInTheDocument();
    expect(screen.getByTestId("content-ws-b")).toBeInTheDocument();
    expect(screen.getByTestId("content-ws-c")).toBeInTheDocument();
    // Handles hidden while zoomed (no resizing a single full-region tile).
    screen.getAllByTestId("handle").forEach((h) => expect(h.className).toContain("hidden"));
  });

  it("ignores a stale zoomedKey not in the live tile set (R-5)", () => {
    render(
      <TerminalTileGroup
        tiles={tiles}
        direction="row"
        sizes={[]}
        onSizesCommit={vi.fn()}
        zoomedKey="ghost-ws"
        renderTile={(t) => <span>{t.key}</span>}
      />,
    );
    // No promotion, nothing collapsed — falls back to the normal split.
    expect(screen.getByTestId("panel-ws-a").getAttribute("data-zoom")).toBeNull();
    expect(screen.getByTestId("panel-ws-b").getAttribute("data-zoom")).toBeNull();
  });

  it("no zoom classes when zoomedKey is null", () => {
    render(
      <TerminalTileGroup
        tiles={tiles}
        direction="row"
        sizes={[]}
        onSizesCommit={vi.fn()}
        zoomedKey={null}
        renderTile={(t) => <span>{t.key}</span>}
      />,
    );
    expect(screen.getByTestId("panel-ws-a").getAttribute("data-zoom")).toBeNull();
    screen
      .getAllByTestId("handle")
      .forEach((h) => expect(h.className ?? "").not.toContain("hidden"));
  });
});

it("cancels an in-flight resize when the persistent mosaic changes teams, even with the same panel IDs", () => {
  const commitA = vi.fn();
  const commitB = vi.fn();
  const props = {
    tiles: [{ key: "same-a" }, { key: "same-b" }],
    direction: "row" as const,
    renderTile: (tile: { key: string }) => <span data-testid={`live-${tile.key}`}>{tile.key}</span>,
  };
  const { rerender } = render(
    <TerminalTileGroup
      {...props}
      resizeIdentity="acme:alpha"
      sizes={[50, 50]}
      onSizesCommit={commitA}
    />,
  );
  const tile = screen.getByTestId("live-same-a");
  fireEvent.pointerDown(screen.getByTestId("panel-group"));
  rerender(
    <TerminalTileGroup
      {...props}
      resizeIdentity="acme:beta"
      sizes={[30, 70]}
      onSizesCommit={commitB}
    />,
  );
  act(() => resizeProbe.changed?.({ "same-a": 60, "same-b": 40 }));
  expect(commitA).not.toHaveBeenCalled();
  expect(commitB).not.toHaveBeenCalled();
  expect(screen.getByTestId("live-same-a")).toBe(tile);
  fireEvent.pointerDown(screen.getByTestId("panel-group"));
  act(() => resizeProbe.changed?.({ "same-a": 40, "same-b": 60 }));
  expect(commitB).toHaveBeenCalledWith([40, 60]);
});
