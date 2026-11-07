// SPDX-License-Identifier: MIT
import { useLayoutEffect, useRef, type ReactNode } from "react";
import { useGroupRef, type Layout } from "react-resizable-panels";
import { ResizableHandle, ResizablePanel, ResizablePanelGroup } from "@/components/ui/resizable";

// ---------------------------------------------------------------------------
// SW-EW-3 — TerminalTileGroup: the reusable split-terminal tiling primitive.
//
// Built on `react-resizable-panels` (via @/components/ui/resizable) so the
// hard parts come for free: conditionally-rendered panels renormalize the
// group (AC-2 — reflow, no empty cells), dragging a separator resizes (AC-4),
// `onLayoutChanged` commits sizes on release (R-2). A "tile" is a generic slot
// (`renderTile`) so a future terminal-LEVEL split nests the same group without a
// rewrite (R-4 / re-anchor note). Zoom/expand is NOT here — that's an overlay
// above the group, specced as the proper toggle in SW-EW-4.
// ---------------------------------------------------------------------------

export interface TileLike {
  /** Stable identity — the workspace id in SW-EW-3 (workspace-level split).
   *  Also the react-resizable-panels Panel `id` (Layout is keyed by it). */
  key: string;
}

export interface TileRenderContext {
  index: number;
}

interface TerminalTileGroupProps<T extends TileLike> {
  /** Ordered foreground tiles. One `ResizablePanel` is rendered per tile. */
  tiles: T[];
  direction: "row" | "column";
  /** Persisted percent sizes (parallel to `tiles`); reconciled to the live
   *  tile count (R-3). */
  sizes: number[];
  /** Commit new sizes (store setter persists, debounced) — AC-9. */
  onSizesCommit: (sizes: number[]) => void;
  renderTile: (tile: T, ctx: TileRenderContext) => ReactNode;
  /** Minimum tile size as a percent of the region. Default 10. */
  minSizePct?: number;
  /** Minimum tile size in pixels along the group's axis; replaces `minSizePct`
   *  when set (a readable floor that holds whatever the region's size). */
  minSizePx?: number;
  /** SW-EW-4 AC-1: key of the zoomed (full-region) tile, or null. Zoom is a
   *  pure CSS promotion (locked rule 4 — D-1): every panel stays MOUNTED with
   *  its stable key (no remount, no PTY/scrollback loss); the zoomed panel is
   *  forced to fill (`data-zoom="active"` → `flex:1`) and the siblings are
   *  collapsed (`data-zoom="collapsed"` → `display:none`) plus handles hidden.
   *  Driven via `data-zoom` so the rule lands on the OUTER `[data-panel]` flex-
   *  item (SW-EW-21). The library keeps its size bookkeeping, so restore is
   *  instant + lossless. A `zoomedKey` not in `tiles` is ignored (R-5). */
  zoomedKey?: string | null;
  /** The group is out of play — it sits inside a collapsed sibling of a zoomed
   *  tile (rendered `display:none`) — so none of its dividers may resize. */
  disabled?: boolean;
  /** Gesture ownership for a persistent group whose layout changes teams. */
  resizeIdentity?: string;
}

/**
 * Reconcile persisted sizes to the live tile count — the layout the group is
 * seeded with and held to (react-resizable-panels renormalizes the live layout
 * itself on add/remove, which is what actually guarantees AC-2's no-empty-cells
 * reflow). This keeps a restored layout sane and never feeds a stale/desynced
 * array (R-3):
 *   - 0 tiles → [] (no dead cell)
 *   - length mismatch (a tile was added/removed since persist) → even split
 *   - length match → normalize to sum 100 (defends against rounding drift)
 */
export function reconcileSizes(sizes: number[], count: number): number[] {
  if (count <= 0) return [];
  const even = Array.from({ length: count }, () => 100 / count);
  if (sizes.length !== count) return even;
  const sum = sizes.reduce((a, b) => a + b, 0);
  if (sum <= 0) return even;
  return sizes.map((s) => (s / sum) * 100);
}

/** How far (in percent) a live layout may drift from the given sizes and still
 *  count as the same layout — rounding in the library never echoes back as a change. */
const LAYOUT_TOLERANCE_PCT = 0.1;

function sameLayout(a: number[], b: number[]): boolean {
  return a.length === b.length && a.every((v, i) => Math.abs(v - b[i]) <= LAYOUT_TOLERANCE_PCT);
}

export function TerminalTileGroup<T extends TileLike>({
  tiles,
  direction,
  sizes,
  onSizesCommit,
  renderTile,
  minSizePct = 10,
  minSizePx,
  zoomedKey,
  disabled = false,
  resizeIdentity,
}: TerminalTileGroupProps<T>) {
  const groupRef = useGroupRef();
  const reconciled = reconcileSizes(sizes, tiles.length);
  const layoutFor = (values: number[]): Layout =>
    Object.fromEntries(tiles.map((t, i) => [t.key, values[i]]));
  const inTileOrder = (layout: Layout): number[] =>
    tiles.map((t) => layout[t.key]).filter((v): v is number => v != null);

  // The sizes are the one truth: when they change under a mounted group (a preset
  // that evens, a restored layout, a resize elsewhere), apply them to it. The
  // group's own drags arrive back here as the same sizes, so this never loops.
  // While the group still holds another set of panels (one just added or
  // removed), it lays itself out from `defaultLayout` as it takes the new set.
  const sizesKey = reconciled.join(",");
  const tilesKey = tiles.map((t) => t.key).join(",");
  // The layout last re-asserted over one the group arrived at by itself, so a
  // layout its constraints clamp is asserted once, never in a loop.
  const assertedRef = useRef<number[] | null>(null);
  useLayoutEffect(() => {
    assertedRef.current = null;
    const group = groupRef.current;
    if (!group || tiles.length === 0) return;
    const live = group.getLayout();
    if (Object.keys(live).length !== tiles.length) return;
    const current = inTileOrder(live);
    if (current.length !== tiles.length) return;
    if (!sameLayout(current, reconciled)) group.setLayout(layoutFor(reconciled));
    // Keyed on the values, not the arrays' identity, which change every render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sizesKey, tilesKey, resizeIdentity]);

  // R-5: only treat zoom as active when the key is a LIVE tile (a stale id —
  // e.g. the zoomed workspace was closed — falls back to the normal split).
  const zoomActive = zoomedKey != null && tiles.some((t) => t.key === zoomedKey);

  // Only a layout the user made — a divider dragged or moved by keyboard — is
  // committed. The group also lays itself out on its own: on mount, and when a
  // set of panels it has held before comes back it replays the layout it last
  // had for that set. Those are not the user's, so the given sizes are put back.
  const userResizingRef = useRef(false);
  const resizeOwnerRef = useRef(resizeIdentity);
  if (resizeOwnerRef.current !== resizeIdentity) {
    resizeOwnerRef.current = resizeIdentity;
    userResizingRef.current = false;
  }
  const userResizeEndRef = useRef<(() => void) | null>(null);
  useLayoutEffect(() => {
    userResizingRef.current = false;
    userResizeEndRef.current?.();
    return () => {
      userResizingRef.current = false;
      userResizeEndRef.current?.();
    };
  }, [resizeIdentity]);
  const beginUserResize = () => {
    userResizingRef.current = true;
    if (userResizeEndRef.current) return;
    // The group reports the drag's layout on release; the flag drops just after.
    const end = () => {
      window.removeEventListener("pointerup", end);
      window.removeEventListener("pointercancel", end);
      userResizeEndRef.current = null;
      setTimeout(() => {
        userResizingRef.current = false;
      }, 0);
    };
    window.addEventListener("pointerup", end);
    window.addEventListener("pointercancel", end);
    userResizeEndRef.current = end;
  };
  const onSeparatorKey = (target: EventTarget) => {
    if (!(target instanceof Element) || !target.hasAttribute("data-separator")) return;
    userResizingRef.current = true;
    setTimeout(() => {
      if (!userResizeEndRef.current) userResizingRef.current = false;
    }, 0);
  };

  const handleLayoutChanged = (layout: Layout) => {
    // Layout is keyed by panel id; serialize back to tile order so the
    // persisted array stays parallel to `tiles`. A layout that is already the
    // given sizes is not a change; one that doesn't fill the region (an
    // unmeasured group) is not a layout.
    const ordered = inTileOrder(layout);
    if (ordered.length !== tiles.length || Object.keys(layout).length !== tiles.length) return;
    if (Math.abs(ordered.reduce((a, b) => a + b, 0) - 100) > 1) return;
    if (sameLayout(ordered, reconciled)) return;
    if (userResizingRef.current) {
      onSizesCommit(ordered);
      return;
    }
    if (assertedRef.current && sameLayout(ordered, assertedRef.current)) return;
    const group = groupRef.current;
    if (group) assertedRef.current = inTileOrder(group.setLayout(layoutFor(reconciled)));
  };

  // Double-clicking a divider evens the two tiles either side of it.
  const evenPairAt = (index: number) => {
    const group = groupRef.current;
    if (!group) return;
    const current = inTileOrder(group.getLayout());
    if (current.length !== tiles.length) return;
    const half = (current[index] + current[index + 1]) / 2;
    const next = [...current];
    next[index] = half;
    next[index + 1] = half;
    userResizingRef.current = true;
    const applied = inTileOrder(group.setLayout(layoutFor(next)));
    userResizingRef.current = userResizeEndRef.current !== null;
    if (applied.length === tiles.length && !sameLayout(applied, reconciled)) onSizesCommit(applied);
  };

  if (tiles.length === 0) return null;

  return (
    <ResizablePanelGroup
      groupRef={groupRef}
      orientation={direction === "column" ? "vertical" : "horizontal"}
      // While a tile is zoomed (or the whole group is hidden) nothing here may
      // resize. The library finds its resize targets from panel GEOMETRY, and a
      // collapsed (`display:none`) panel measures as a 0×0 box at the window
      // origin, so it sorts first and the gap between it and the zoomed tile
      // reads as a separator spanning from the window edge to the tile — a
      // stray resize cursor (and a live drag) across whatever sits there, e.g.
      // the title bar above a full-screen CLI. Disabling the group drops it
      // from that hit-testing entirely.
      disabled={disabled || zoomActive}
      defaultLayout={layoutFor(reconciled)}
      onLayoutChanged={handleLayoutChanged}
      onPointerDownCapture={beginUserResize}
      onKeyDownCapture={(e) => onSeparatorKey(e.target)}
      className="h-full w-full"
    >
      {tiles.map((tile, i) => (
        <PanelWithHandle
          key={tile.key}
          tileKey={tile.key}
          isLast={i === tiles.length - 1}
          minSize={minSizePx ?? `${minSizePct}%`}
          zoomActive={zoomActive}
          isZoomed={tile.key === zoomedKey}
          onEvenPair={() => evenPairAt(i)}
        >
          {renderTile(tile, { index: i })}
        </PanelWithHandle>
      ))}
    </ResizablePanelGroup>
  );
}

interface PanelWithHandleProps {
  tileKey: string;
  isLast: boolean;
  /** A pixel count, or a `"N%"` share of the region. */
  minSize: number | string;
  /** SW-EW-4: some tile in the group is zoomed. */
  zoomActive: boolean;
  /** SW-EW-4: THIS panel is the zoomed one. */
  isZoomed: boolean;
  /** Even this panel and the next (the separator after it was double-clicked). */
  onEvenPair: () => void;
  children: ReactNode;
}

/** One resizable panel plus the separator that follows it (omitted on the last
 *  tile). `id` lets react-resizable-panels key its Layout across conditional
 *  add/remove. */
function PanelWithHandle({
  tileKey,
  isLast,
  minSize,
  zoomActive,
  isZoomed,
  onEvenPair,
  children,
}: PanelWithHandleProps) {
  // react-resizable-panels v4.8 treats a BARE NUMBER as pixels and a "%"-string
  // as a percentage. The starting sizes ride on the group's `defaultLayout`, not
  // on each panel's `defaultSize`: a panel with a default size makes the library's
  // own divider double-click snap back to it, and the double-click here evens the
  // pair instead.
  //
  // SW-EW-4/SW-EW-21 zoom (D-1): the panel STAYS MOUNTED either way (pure CSS,
  // no remount → PTY/scrollback intact). Zoom is driven via `data-zoom` rather
  // than `className`, because react-resizable-panels@4.8 puts `className` on the
  // INNER content div but spreads unknown props (`...rest`, incl. `data-*`) onto
  // the OUTER `[data-panel]` flex-item — and the OUTER element is the one that
  // owns panel width/flex. The `[data-panel][data-zoom=…]` rules in index.css
  // then collapse siblings (`display:none`) and grow the zoomed tile (`flex:1`).
  // `!important` beats the library's inline flex without touching its size model,
  // so un-zoom restores instantly + losslessly. `min-w-0 min-h-0` stays on the
  // zoomed INNER div (terminal fit); the non-zoom split path keeps className
  // undefined exactly as before.
  const isActiveZoom = zoomActive && isZoomed;
  const dataZoom = zoomActive ? (isActiveZoom ? "active" : "collapsed") : undefined;
  return (
    <>
      <ResizablePanel
        id={tileKey}
        minSize={minSize}
        data-zoom={dataZoom}
        className={isActiveZoom ? "min-w-0 min-h-0" : undefined}
      >
        {children}
      </ResizablePanel>
      {/* withHandle = a visible grip; the hover tint makes the otherwise 1px
          separator findable and signals it's draggable. Hidden while zoomed
          (no resizing a single full-region tile). */}
      {!isLast && (
        <ResizableHandle
          withHandle
          onDoubleClick={onEvenPair}
          className={`bg-[var(--cs-sidebar-border,#30363d)] transition-colors hover:bg-[var(--cs-accent,#388bfd)] ${
            zoomActive ? "hidden" : ""
          }`}
        />
      )}
    </>
  );
}
