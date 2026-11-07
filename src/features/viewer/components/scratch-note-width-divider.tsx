// SPDX-License-Identifier: MIT
import type { RefObject } from "react";
import { useDashboardProfileStore } from "@/features/dashboard/lib/dashboard-profile";
import { resolveSlot } from "@/features/shell/slots/reveal-slot";
import { INBOX_PANEL_ID } from "@/features/shell/right-pane/right-pane-layout";
import { usePaneStore } from "@/features/shell/right-pane/panes/pane-store";
import { usePanelRegistry } from "@/features/shell/right-pane/panel-registry";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { computeClampedRatio } from "@/features/viewer/views/drag-resize-utils";
import type { DragResizeOptions } from "@/features/viewer/views/use-drag-resize";
import { PaneCollapseDivider, type PaneSize } from "./pane-collapse-divider";

export const RIGHT_PANE_RATIO_VAR = "--cs-right-pane-ratio";

/** The notes pane's share of the row, in percent: the drag's 15% floor and
 *  50% ceiling, and the keyboard steps. */
const MIN_PCT = 15;
const MAX_PCT = 50;

interface ScratchNoteWidthDividerProps {
  /** Ref to the flex-row container whose width is the drag axis and on
   *  which the `--cs-right-pane-ratio` CSS variable is written during drag. */
  containerRef: RefObject<HTMLDivElement | null>;
  /** SW-FX-8: whether the right pane is collapsed (closed). */
  collapsed: boolean;
  /** SW-FX-8: toggle the right pane open/closed. */
  onToggle: () => void;
}

/**
 * SF-7 / SW-FX-8: scratch-note (right-pane) horizontal-width divider, now a
 * right-edge {@link PaneCollapseDivider} — drag to resize, click to collapse,
 * and the border stays rendered as the kept left border of the right pane when
 * closed so it can be clicked to re-open.
 *
 * Visual-follow during drag via a CSS variable on `containerRef.current`;
 * single store commit on pointerup. `invertDelta=true` because the scratch
 * panel grows toward the LEFT as the cursor moves left (it's the right-side
 * panel). `minPx = containerWidth * 0.15` and `maxRatio = 0.5` mirror the
 * prior inline clamp. Keyboard: ArrowLeft widens the pane (it sits right of its
 * border), Home/End jump to 15% / 50%.
 */
export function ScratchNoteWidthDivider({
  containerRef,
  collapsed,
  onToggle,
}: ScratchNoteWidthDividerProps) {
  const ratio = useViewerStore((s) => s.rightPaneRatio);
  const badges = usePaneStore((s) => s.badges);
  const panels = usePanelRegistry((s) => s.panels);
  const entries = usePaneStore((s) => s.knownEntries);
  useDashboardProfileStore((s) => s.profile);
  const inbox = usePaneStore(() => resolveSlot("right-pane", { surface: "inbox" }).dashboard);
  const counted = Object.entries(badges).filter(([, badge]) => badge.count > 0);
  const count = counted.reduce((sum, [, badge]) => sum + badge.count, 0);
  const badge = count
    ? {
        count,
        label:
          counted
            .map(([id, badge]) => {
              // Architecture: declaration-aware copy describes the pane's badge delivery.
              const entry = id === INBOX_PANEL_ID ? inbox : entries[id];
              const stale =
                entry?.declaration?.badge === "live" ? "" : " (last known; open to refresh)";
              return `${panels[id]?.title ?? id} ${badge.count} unread${stale}`;
            })
            .join(", ") + ".",
      }
    : undefined;
  const pct = Math.round(ratio * 100);
  const size: PaneSize = {
    now: pct,
    min: MIN_PCT,
    max: MAX_PCT,
    step: 2,
    largeStep: 10,
    format: (n) => `Notes ${n}% of the width`,
    fromDrag: (r) => Math.round(r * 100),
    onChange: (next) => useViewerStore.getState().setRightPaneRatio(next / 100),
  };
  const resize: Omit<DragResizeOptions, "onTap"> = {
    getContainerSize: () => containerRef.current?.getBoundingClientRect().width ?? 0,
    getStartRatio: () => useViewerStore.getState().rightPaneRatio,
    getPos: (e) => e.clientX,
    computeRatio: ({ startRatio, startPos, currentPos, containerSize }) =>
      computeClampedRatio(
        startRatio,
        startPos,
        currentPos,
        containerSize,
        containerSize * (MIN_PCT / 100),
        true,
        MAX_PCT / 100,
      ),
    onVisualUpdate: (r) => {
      containerRef.current?.style.setProperty(RIGHT_PANE_RATIO_VAR, String(r));
    },
    onCommit: (r) => useViewerStore.getState().setRightPaneRatio(r),
    // SLW-FX-9: `.dragging` + the resize cursor are owned by useDragResize /
    // PaneCollapseDivider; this cleanup only reverts the consumer CSS var.
    onCleanup: () => {
      containerRef.current?.style.removeProperty(RIGHT_PANE_RATIO_VAR);
    },
  };

  return (
    <PaneCollapseDivider
      edge="right"
      collapsed={collapsed}
      onToggle={onToggle}
      hotkeyActionId="viewer.toggle-right-pane"
      paneLabel="right pane"
      collapsedBadge={badge}
      resize={resize}
      size={size}
      data-testid="scratch-note-width-divider"
    />
  );
}
