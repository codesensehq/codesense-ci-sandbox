// SPDX-License-Identifier: MIT
import { useState, type RefObject } from "react";
import { getViewerState } from "@/features/viewer/lib/cli-workspace/store/viewer-access";
import { computeClampedRatio } from "@/features/viewer/views/drag-resize-utils";
import type { DragResizeOptions } from "@/features/viewer/views/use-drag-resize";
import { PaneCollapseDivider, type PaneSize } from "./pane-collapse-divider";

export const TERM_SPLIT_VAR = "--cs-term-split";

/** The smallest the drag lets either side get. */
const FLOOR_PX = 80;
/** Dragged this short (half the floor), the terminal snaps closed: the preview
 *  shows it collapsed and releasing there collapses the CLI. */
const SNAP_CLOSED_PX = FLOOR_PX / 2;
/** A viewer ratio of 1 is the collapse sentinel the drag hands to `onCommit`. */
const COLLAPSED_RATIO = 1;
/** Keyboard range for the terminal's share, in percent. */
const MIN_PCT = 10;
const MAX_PCT = 90;

interface TerminalSplitDividerProps {
  /** Ref to the flex container whose height is the drag axis and on
   *  which the `--cs-term-split` CSS variable is written during drag. */
  leftColumnRef: RefObject<HTMLDivElement | null>;
  /** The committed viewer share (`terminalSplitRatio`: the VIEWER's share, the
   *  terminal holds the rest), read by the host; drives the keyboard value. */
  viewerRatio: number;
  /** SW-FX-8: whether the terminal is collapsed (hidden). */
  collapsed: boolean;
  /** SW-FX-8: toggle the terminal collapsed/expanded. */
  onToggle: () => void;
}

/**
 * SF-7 / SW-FX-8: terminal/viewer vertical-split divider, now expressed as a
 * top-edge {@link PaneCollapseDivider} — the border doubles as a resize handle
 * (drag) and a collapse toggle (click), and stays rendered as the kept bottom
 * border when the terminal is collapsed so it can be clicked to re-open.
 *
 * Visual-follow during drag via a CSS variable on `leftColumnRef.current`;
 * single store commit on pointerup. The global `.dragging` class and the
 * window-wide resize cursor (SLW-FX-9) are both owned by {@link useDragResize}
 * / {@link PaneCollapseDivider}, so this divider only manages its own CSS var.
 *
 * Snap closed: dragging the terminal below half its floor previews it collapsed
 * (the viewer takes the whole column) and releasing there collapses the CLI
 * through `onToggle` — the same closed state as a click or the hotkey — while
 * the saved height is kept for the reopen. Keyboard: ArrowUp grows the
 * terminal, ArrowDown shrinks it, Home/End jump to 10% / 90% of the column.
 */
export function TerminalSplitDivider({
  leftColumnRef,
  viewerRatio,
  collapsed,
  onToggle,
}: TerminalSplitDividerProps) {
  // True while the drag sits in the snap zone, so the border can say a release
  // there closes the CLI.
  const [snapping, setSnapping] = useState(false);
  // The viewer and terminal split what the column has left above the workspace
  // bar (a direct child at the column's bottom); counting the bar would make
  // the ratio drift away from the pointer.
  const splitHeight = () => {
    const column = leftColumnRef.current;
    if (!column) return 0;
    const bar = column.querySelector(":scope > [data-workspace-bar]");
    return column.getBoundingClientRect().height - (bar?.getBoundingClientRect().height ?? 0);
  };
  const pct = Math.round((1 - viewerRatio) * 100);
  const size: PaneSize = {
    now: pct,
    min: MIN_PCT,
    max: MAX_PCT,
    step: 2,
    largeStep: 10,
    format: (n) => `Terminal ${n}% of the column`,
    // The drag value is the VIEWER's share; the collapse sentinel reads as 0%.
    fromDrag: (r) => Math.round((1 - r) * 100),
    onChange: (next) => {
      const height = splitHeight();
      const ratio = Math.round((1 - next / 100) * 10000) / 10000;
      // Still honour the drag's pixel floor on a short column.
      const floor = height > 0 ? FLOOR_PX / height : 0;
      // Too short for both floors: the drag locks this column at 50/50, so the
      // keyboard has nothing to move.
      if (floor >= 0.5) return;
      getViewerState().setTerminalSplitRatio(Math.min(1 - floor, Math.max(floor, ratio)));
    },
  };
  const resize: Omit<DragResizeOptions, "onTap"> = {
    getContainerSize: splitHeight,
    getStartRatio: () => getViewerState().terminalSplitRatio,
    getPos: (e) => e.clientY,
    computeRatio: ({ startRatio, startPos, currentPos, containerSize }) => {
      const startPx = (1 - startRatio) * containerSize;
      const terminalPx = startPx - (currentPos - startPos);
      // Only a terminal that started above the snap point can be dragged into it.
      if (startPx >= SNAP_CLOSED_PX && terminalPx < SNAP_CLOSED_PX) return COLLAPSED_RATIO;
      return Math.min(
        0.9,
        Math.max(
          0.1,
          computeClampedRatio(startRatio, startPos, currentPos, containerSize, FLOOR_PX),
        ),
      );
    },
    onVisualUpdate: (r) => {
      leftColumnRef.current?.style.setProperty(TERM_SPLIT_VAR, String(r));
      setSnapping(r >= COLLAPSED_RATIO);
    },
    onCommit: (r, { viaPointerUp }) => {
      // Released in the snap zone: collapse (the saved height is kept), never
      // persist the sentinel as a split. An aborted drag (pointercancel, window
      // blur) there does nothing: the terminal springs back.
      if (r < COLLAPSED_RATIO) getViewerState().setTerminalSplitRatio(r);
      else if (viaPointerUp) onToggle();
    },
    // SLW-FX-9: `.dragging` is managed by useDragResize itself (add on first
    // move, remove on every teardown path) and the resize cursor by its
    // `dragCursor` (via PaneCollapseDivider) — this cleanup only reverts the
    // consumer-specific CSS var. No global-state toggling to duplicate here.
    onCleanup: () => {
      leftColumnRef.current?.style.removeProperty(TERM_SPLIT_VAR);
      setSnapping(false);
    },
  };

  return (
    <PaneCollapseDivider
      edge="top"
      collapsed={collapsed}
      onToggle={onToggle}
      hotkeyActionId="viewer.toggle-terminal"
      paneLabel="terminal"
      resize={resize}
      size={size}
      dragHint={snapping ? "Release to close the terminal" : undefined}
      data-testid="terminal-drag-handle"
    />
  );
}
