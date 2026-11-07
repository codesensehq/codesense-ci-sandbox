// SPDX-License-Identifier: MIT
import { useId, useRef, useState, type CSSProperties, type KeyboardEvent } from "react";
import { InfoTip } from "@/components/ui/tooltip";
import { useDragResize, type DragResizeOptions } from "@/features/viewer/views/use-drag-resize";
import { getEffectiveShortcut, useShortcutsStore } from "@/shared/stores/shortcuts-store";
import { formatKeystrokeDisplay } from "@/shared/lib/shortcuts/keystroke";

export type PaneEdge = "left" | "right" | "top";

/**
 * The owning pane's size, as the keyboard and assistive tech see it (the ARIA
 * window-splitter value). Units are the consumer's: pixels for the nav and the
 * Agents list, percent for the ratio-sized notes pane and terminal.
 */
export interface PaneSize {
  /** The pane's current size. */
  now: number;
  /** The smallest and largest size a keyboard resize may choose. */
  min: number;
  max: number;
  /** Arrow-key step, and the Shift+arrow step. */
  step: number;
  largeStep: number;
  /** Spoken form of a size (aria-valuetext), e.g. `(n) => \`${n} pixels\``. */
  format?: (now: number) => string;
  /** Map the drag's value (whatever `resize.computeRatio` returns) to this
   *  size's units, so the announced value follows the pointer mid-drag.
   *  Omitted ⇒ `now` is already live (the consumer re-renders during the drag). */
  fromDrag?: (dragValue: number) => number;
  /** Apply a keyboard-chosen size, already clamped to [min, max]. */
  onChange: (size: number) => void;
}

/** Per edge, the arrow that moves the border AWAY from its pane (grows it) and
 *  the one that moves it toward the pane (shrinks it). */
const GROW_SHRINK_KEYS: Record<PaneEdge, readonly [grow: string, shrink: string]> = {
  left: ["ArrowRight", "ArrowLeft"],
  right: ["ArrowLeft", "ArrowRight"],
  top: ["ArrowUp", "ArrowDown"],
};

/** Invisible hit padding (px) on each side of the 1px visible hairline, so the
 *  thin divider is forgiving to hover and click. */
const HIT_PADDING_PX = 7;

/** A resize-drag emits a trailing `click`; swallow any click within this window
 *  of a drag *end* so a resize never also collapses. */
const CLICK_AFTER_DRAG_MS = 250;

/** Collapsed-state colors — foreground-tinted (not the near-invisible
 *  `--border`) so the kept edge + grab handle read against the window chrome. */
const GRIP_COLOR = "color-mix(in srgb, var(--foreground) 55%, transparent)";
const COLLAPSED_EDGE_COLOR = "color-mix(in srgb, var(--foreground) 22%, transparent)";

interface PaneCollapseDividerProps {
  /** Which side of its pane the divider sits on / which way the pane
   *  collapses. `left`/`right` → vertical divider (full height); `top` →
   *  horizontal divider (full width, the bottom-terminal case). */
  edge: PaneEdge;
  /** Whether the owning pane is currently collapsed (size 0). */
  collapsed: boolean;
  /** Toggle the pane's collapsed state (wired to the border click). */
  onToggle: () => void;
  /** Shortcut action id whose live binding is shown in the tooltip/label.
   *  Omitted for a pane with no toggle shortcut (the Agents list). */
  hotkeyActionId?: string;
  /** Human label for the pane (e.g. "sidebar", "terminal") for tooltip + aria. */
  paneLabel: string;
  /** Resize behaviour while expanded. */
  resize: Omit<DragResizeOptions, "onTap">;
  /** The pane's size for the keyboard and the window-splitter semantics. */
  size: PaneSize;
  /** Border/line color CSS value. Defaults to `var(--border)`; the nav passes
   *  its themed `var(--cs-sidebar-border, …)` to keep its exact appearance. */
  borderColor?: string;
  /** Invisible hit padding (px) before (`start`: left/top) and after (`end`:
   *  right/bottom) the 1px line. Defaults to `HIT_PADDING_PX` both sides; a pane
   *  whose neighbours carry dense controls right at the edge narrows it. */
  hitPadding?: { start: number; end: number };
  /** A short status shown on the border while it applies (e.g. "Release to
   *  close" once a drag is in a snap zone). */
  dragHint?: string;
  /** While collapsed, drop the start-side negative margin so the hit zone sits
   *  flush inside its container — for a grip that is its row's first child at
   *  an `overflow: hidden` edge, where the margin would clip it. */
  flushStartWhenCollapsed?: boolean;
  /** Counts and pane names supplied by the owning surface. */
  collapsedBadge?: { count: number; label: string };
  "data-testid"?: string;
}

/**
 * SW-FX-8: the one reusable "the border is the control" affordance, shared
 * by every resizable pane: the left nav, the right pane, the terminal split, and
 * the Agents list.
 *
 * - **Expanded:** drag the divider to resize (via {@link useDragResize}); a
 *   plain click collapses the pane. Click vs drag is disambiguated by a flag
 *   set when a real drag starts (so a resize-drag never also collapses). The
 *   collapse uses a native `onClick` — robust across webviews — rather than a
 *   synthesized pointer tap. The hairline thickens on hover as an icon-free
 *   "interactive" cue; a native `title` tooltip explains drag-vs-click.
 * - **Collapsed:** the pane is 0-size but its border stays rendered and is the
 *   click-to-expand target — shown as a foreground-tinted accent edge plus a
 *   centered rounded "grab handle" so it's discoverable against the window
 *   chrome (the bare `--border` hairline was nearly invisible). Both brighten +
 *   grow on hover; cursor is a pointer (click to open, not a resize drag).
 *
 * A wide invisible hit zone (`HIT_PADDING_PX` either side of the 1px line by
 * default — `hitPadding` narrows either side — with matching negative margins
 * so layout footprint stays ~1px) makes the thin border forgiving.
 * Collapse/expand is INSTANT (no size transition). The tooltip is held shut
 * while a drag is in flight; a consumer's `dragHint` shows on the border instead.
 *
 * Keyboard + a11y: expanded, the border is an ARIA window splitter — a focusable
 * `separator` with `aria-orientation` and the pane's size as
 * `aria-valuenow/min/max` (live during a drag via `size.fromDrag`) and a
 * description of the keys. The arrow that moves the border away from the pane
 * grows it (Shift for a large step), Home/End jump to its smallest/largest size,
 * and Enter/Space collapse it. Collapsed, there is nothing to size, so it is a
 * `button` with `aria-expanded=false` that Enter/Space expand. Focus shows the
 * same thickened hairline as hover, in the ring colour; during a drag the
 * hairline stays thickened even after the pointer leaves the hit zone.
 */
export function PaneCollapseDivider({
  edge,
  collapsed,
  onToggle,
  hotkeyActionId,
  paneLabel,
  resize,
  size,
  borderColor = "var(--border)",
  hitPadding,
  dragHint,
  flushStartWhenCollapsed = false,
  collapsedBadge,
  "data-testid": testId,
}: PaneCollapseDividerProps) {
  const padStart = hitPadding?.start ?? HIT_PADDING_PX;
  const padEnd = hitPadding?.end ?? HIT_PADDING_PX;
  const descriptionId = useId();
  // Timestamp of the last drag's end. Using a timestamp (not a sticky flag)
  // self-heals: a drag that ends WITHOUT a trailing click (pointercancel /
  // window blur / pointerup released off the divider — common when dragging
  // the nav wide) can't leave a latched flag that swallows the next genuine
  // click. `onCleanup` fires on every started-drag teardown path.
  const dragEndedAtRef = useRef(0);
  // True between a drag's start and its teardown: keeps the hairline highlighted
  // when the pointer runs ahead of the hit zone (capture keeps the drag alive).
  const [dragging, setDragging] = useState(false);
  // The pane's size while a drag is in flight (`size.fromDrag`), so the
  // separator announces where the pointer is, not where the drag began.
  const [liveNow, setLiveNow] = useState<number | null>(null);
  // The tooltip stays controlled (never flipping controlled ↔ uncontrolled):
  // Radix's hover/focus intent, gated shut for the duration of a drag.
  const [tipOpen, setTipOpen] = useState(false);

  const isVertical = edge === "left" || edge === "right";

  // SLW-FX-9 / SW-FX-74: one resize-cursor value drives BOTH the global `<body>`
  // cursor for the drag duration (deterministic, capture-independent) AND the
  // armed at-rest hover cursor (`cursor` from the hook). Centralized here so
  // every PaneCollapseDivider consumer (nav, terminal, scratch-note) gets the
  // stuck-cursor fix without duplicating the logic in each `resize` object.
  const dragCursor = isVertical ? "col-resize" : "row-resize";

  // Hook is called unconditionally (rules of hooks). The DRAG handlers are wired
  // only when expanded — a collapsed pane has nothing to size — but the
  // enter/leave arm handlers are wired in BOTH states so `armed` always tracks
  // genuine pointer presence: a collapse toggle (or hotkey) while the pointer
  // rests on the divider can't strand a stale `armed=true`, and re-expanding
  // while genuinely hovered shows the resize cursor immediately. `cursor` is the
  // armed at-rest resize cursor (SW-FX-74): the `dragCursor` value while the
  // pointer genuinely rests on the divider, else undefined — a stranded WebKit
  // capture can't re-assert it because only a fresh, genuine pointer-enter
  // re-arms it.
  const {
    cursor: armedCursor,
    onPointerEnter,
    onPointerLeave,
    ...dragHandlers
  } = useDragResize({
    ...resize,
    dragCursor,
    onDragStart: () => {
      setDragging(true);
      resize.onDragStart?.();
    },
    onVisualUpdate: (value) => {
      resize.onVisualUpdate(value);
      if (size.fromDrag) setLiveNow(size.fromDrag(value));
    },
    onCleanup: () => {
      dragEndedAtRef.current = performance.now();
      setDragging(false);
      setLiveNow(null);
      resize.onCleanup?.();
    },
  });
  const hint = useShortcutHint(hotkeyActionId);

  const ariaLabel = collapsed
    ? collapsedBadge
      ? `Open ${paneLabel}: ${collapsedBadge.label}${hint}`
      : `Expand ${paneLabel}${hint}`
    : `Resize ${paneLabel}`;
  const tooltip = collapsed ? ariaLabel : `Click to collapse · drag to resize${hint}`;
  const keyboardHelp = `${tooltip}. Arrow keys resize, Shift for larger steps; Home and End go to the limits; Enter collapses.`;
  const now = Math.min(size.max, Math.max(size.min, liveNow ?? size.now));

  const handleToggle = () => {
    // Swallow the trailing click emitted right after a resize-drag; a genuine
    // click (no recent drag) toggles.
    if (performance.now() - dragEndedAtRef.current < CLICK_AFTER_DRAG_MS) return;
    onToggle();
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      onToggle();
      return;
    }
    if (collapsed) return;
    const [grow, shrink] = GROW_SHRINK_KEYS[edge];
    const step = e.shiftKey ? size.largeStep : size.step;
    let next: number;
    if (e.key === grow) next = size.now + step;
    else if (e.key === shrink) next = size.now - step;
    else if (e.key === "Home") next = size.min;
    else if (e.key === "End") next = size.max;
    else return;
    e.preventDefault();
    next = Math.min(size.max, Math.max(size.min, next));
    // A saved size outside today's range (a nav width from a wider window) must
    // never make the grow key shrink the pane, or the shrink key grow it.
    if (e.key === grow && next <= size.now) return;
    if (e.key === shrink && next >= size.now) return;
    if (next !== size.now) size.onChange(next);
  };

  // Wide invisible hit area around the 1px line; negative margins keep the
  // layout footprint ~1px. Collapsed top-edge sits flush (margin 0) so it
  // doesn't overlap the viewer content above.
  const hitStyle: CSSProperties = isVertical
    ? {
        boxSizing: "content-box",
        width: "1px",
        paddingLeft: padStart,
        paddingRight: padEnd,
        marginLeft: collapsed && flushStartWhenCollapsed ? 0 : -padStart,
        marginRight: -padEnd,
        zIndex: 20,
      }
    : {
        boxSizing: "content-box",
        height: "1px",
        paddingTop: padStart,
        paddingBottom: padEnd,
        marginTop: collapsed ? 0 : -padStart,
        marginBottom: collapsed ? 0 : -padEnd,
        zIndex: 20,
      };

  // SW-FX-74: the expanded resize cursor is JS-armed (genuine pointer presence),
  // applied inline — NOT a CSS `:hover` class a stranded WebKit pointer capture
  // can re-assert. Collapsed shows a plain pointer (click to expand); expanded
  // shows the armed value (undefined → default arrow) until a fresh enter arms
  // it, and disarms for the drag (the `<body>` cursor carries it then).
  const cursor = collapsed ? "pointer" : armedCursor;

  // The visible line sits on the 1px content box, which an asymmetric hit zone
  // moves off-centre: place every drawn part on it rather than at 50%.
  const onLine: CSSProperties = isVertical ? { left: padStart + 0.5 } : { top: padStart + 0.5 };

  return (
    // The tip is held shut for the drag: it would otherwise hang mid-viewport
    // where the drag began.
    <>
      <InfoTip label={tooltip} open={tipOpen && !dragging} onOpenChange={setTipOpen}>
        <div
          tabIndex={0}
          {...(collapsed
            ? { role: "button", "aria-expanded": false }
            : {
                role: "separator",
                "aria-orientation": isVertical ? "vertical" : "horizontal",
                "aria-valuenow": now,
                "aria-valuemin": size.min,
                "aria-valuemax": size.max,
                "aria-valuetext": size.format?.(now),
                "aria-describedby": descriptionId,
              })}
          aria-label={ariaLabel}
          data-dragging={dragging || undefined}
          onKeyDown={onKeyDown}
          onClick={handleToggle}
          data-testid={testId}
          // Arm handlers always wired (see destructure); drag handlers only when
          // expanded — a collapsed pane has nothing to size.
          onPointerEnter={onPointerEnter}
          onPointerLeave={onPointerLeave}
          {...(collapsed ? {} : dragHandlers)}
          // `select-none`: the divider is the press anchor; its hit zone overlaps
          // the neighbouring panes, so without this a drag started here anchors a
          // native text selection and highlights the adjacent content instead of
          // resizing. The global `.dragging` guard only lands once the drag starts.
          className={`group relative flex-shrink-0 touch-none select-none outline-none ${isVertical ? "self-stretch" : "w-full"}`}
          style={{ position: "relative", cursor, ...hitStyle }}
        >
          {collapsed ? (
            <>
              {/* Accent edge: a touch thicker + foreground-tinted so the collapsed
              border stands out from the window chrome (the bare --border line
              was nearly invisible). Brightens on hover. */}
              <div
                aria-hidden="true"
                className={`pointer-events-none absolute opacity-80 group-hover:opacity-100 group-focus-visible:opacity-100 ${
                  isVertical
                    ? "inset-y-0 w-[3px] -translate-x-1/2 group-hover:w-1.5 group-focus-visible:w-1.5"
                    : "inset-x-0 h-[3px] -translate-y-1/2 group-hover:h-1.5 group-focus-visible:h-1.5"
                }`}
                style={{ ...onLine, backgroundColor: COLLAPSED_EDGE_COLOR }}
              />
              {collapsedBadge && (
                <span
                  aria-hidden="true"
                  data-badge-count={collapsedBadge.count}
                  className="pointer-events-none absolute top-1/2 -mt-7 size-1.5 -translate-x-1/2 rounded-full bg-primary"
                  style={onLine}
                />
              )}
              {/* Grab handle: a short rounded "pull" centered on the edge — a
              recognizable, icon-free "grab to open" cue that's visible at rest
              and grows + brightens on hover. */}
              <div
                aria-hidden="true"
                className={`pointer-events-none absolute -translate-x-1/2 -translate-y-1/2 rounded-full opacity-70 group-hover:opacity-100 group-focus-visible:opacity-100 group-focus-visible:!bg-[var(--ring)] ${
                  isVertical
                    ? "top-1/2 h-8 w-[5px] group-hover:h-12 group-focus-visible:h-12"
                    : "left-1/2 h-[5px] w-8 group-hover:w-12 group-focus-visible:w-12"
                }`}
                style={{ ...onLine, backgroundColor: GRIP_COLOR }}
              />
            </>
          ) : (
            /* Expanded: a 1px resize hairline that thickens on hover — and
           stays thick for the whole drag — as an icon-free "interactive" cue.
           Keyboard focus thickens it too, in the ring colour. pointer-events-none
           so clicks fall through to the hit-area root above. */
            <div
              aria-hidden="true"
              className={`pointer-events-none absolute group-focus-visible:!bg-[var(--ring)] ${
                isVertical
                  ? "inset-y-0 w-px -translate-x-1/2 group-hover:w-[3px] group-focus-visible:w-[3px] group-data-[dragging]:w-[3px]"
                  : "inset-x-0 h-px -translate-y-1/2 group-hover:h-[3px] group-focus-visible:h-[3px] group-data-[dragging]:h-[3px]"
              }`}
              style={{ ...onLine, backgroundColor: borderColor }}
            />
          )}
          {!collapsed && (
            <span id={descriptionId} className="sr-only">
              {keyboardHelp}
            </span>
          )}
          {dragHint && (
            <span
              aria-hidden="true"
              data-drag-hint=""
              className={`pointer-events-none absolute z-10 whitespace-nowrap rounded-md border border-border bg-popover px-2 py-0.5 text-[11px] font-medium text-popover-foreground shadow-sm ${
                isVertical
                  ? "top-1/2 left-full ml-1.5 -translate-y-1/2"
                  : "left-1/2 bottom-full mb-1.5 -translate-x-1/2"
              }`}
            >
              {dragHint}
            </span>
          )}
        </div>
      </InfoTip>
      {/* The hint's live region lives OUTSIDE the separator (whose children are
      presentational to AT) and always exists, so a new hint is announced. */}
      <span role="status" className="sr-only">
        {dragHint ?? ""}
      </span>
    </>
  );
}

/** " (⌘B)" for the pane's live toggle binding, or "" for a pane without one.
 *  Subscribes to the binding so a rebind re-renders the tooltip. */
function useShortcutHint(actionId: string | undefined): string {
  // Subscribing to the override keeps the hint live across rebinds; the
  // effective value (override or default) is then read for the known id.
  useShortcutsStore((s) => (actionId ? s.shortcuts[actionId] : undefined));
  const binding = actionId ? getEffectiveShortcut(actionId) : "";
  return binding ? ` (${formatKeystrokeDisplay(binding)})` : "";
}
