// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useRef, useState } from "react";

import { setAgentInboxRailWidth } from "@/features/viewer/stores/agent-inbox-rail-store";

/** Docked inbox sizing: a comfortable two-column default, never so narrow the lenses break,
 *  never so wide the terminal disappears. */
export const INBOX_RAIL_WIDTH_DEFAULT = 520;
export const INBOX_RAIL_WIDTH_MIN = 360;
export const INBOX_RAIL_WIDTH_MAX_RATIO = 0.72;
const KEYBOARD_STEP_PX = 24;

/** The rail's CSS width for a stored px value: the value, held inside the split's bounds. */
export function inboxRailCssWidth(px: number): { width: string; minWidth: string } {
  const maxPct = `${INBOX_RAIL_WIDTH_MAX_RATIO * 100}%`;
  return {
    width: `min(${px}px, ${maxPct})`,
    minWidth: `min(${INBOX_RAIL_WIDTH_MIN}px, ${maxPct})`,
  };
}

/**
 * The vertical splitter between a tab's terminal and its inbox. Drag (rAF-throttled, so the
 * terminal reflows once a frame), double-click or Enter to reset, ←/→ to nudge, Home/End for
 * the bounds. Every write is clamped to `[MIN, 72% of the split]`.
 */
export function InboxSplitter({
  tabId,
  width,
  containerRef,
}: {
  tabId: string;
  width: number;
  containerRef: React.RefObject<HTMLElement | null>;
}) {
  const drag = useRef<{
    pointerId: number;
    startX: number;
    startWidth: number;
    max: number;
  } | null>(null);
  const [dragging, setDragging] = useState(false);

  const maxWidth = useCallback(() => {
    const w = containerRef.current?.getBoundingClientRect().width ?? 0;
    return Math.max(INBOX_RAIL_WIDTH_MIN, Math.floor(w * INBOX_RAIL_WIDTH_MAX_RATIO));
  }, [containerRef]);
  const clamp = (px: number, max: number) =>
    Math.min(max, Math.max(INBOX_RAIL_WIDTH_MIN, Math.round(px)));
  const commit = (px: number) => setAgentInboxRailWidth(tabId, clamp(px, maxWidth()));

  useEffect(() => {
    if (!dragging) return;
    let frame: number | null = null;
    let pending: number | null = null;
    const next = (x: number) => {
      const d = drag.current!;
      // The rail sits on the right: dragging left grows it.
      return clamp(d.startWidth + (d.startX - x), d.max);
    };
    const onMove = (e: PointerEvent) => {
      if (e.pointerId !== drag.current?.pointerId) return;
      pending = e.clientX;
      if (frame !== null) return;
      frame = requestAnimationFrame(() => {
        frame = null;
        if (pending !== null && drag.current) setAgentInboxRailWidth(tabId, next(pending));
      });
    };
    const end = (e: PointerEvent, cancelled: boolean) => {
      const d = drag.current;
      if (!d || e.pointerId !== d.pointerId) return;
      if (frame !== null) cancelAnimationFrame(frame);
      setAgentInboxRailWidth(tabId, cancelled ? d.startWidth : next(e.clientX));
      drag.current = null;
      setDragging(false);
    };
    const onUp = (e: PointerEvent) => end(e, false);
    const onCancel = (e: PointerEvent) => end(e, true);
    // Released outside the webview, the pointerup never arrives here but capture is still
    // lost: end the drag where it last was rather than leaving it stuck. (After a normal
    // pointerup the drag is already over and this does nothing.)
    const onLost = (e: PointerEvent) => {
      const d = drag.current;
      if (!d || e.pointerId !== d.pointerId) return;
      if (frame !== null) cancelAnimationFrame(frame);
      setAgentInboxRailWidth(tabId, pending === null ? d.startWidth : next(pending));
      drag.current = null;
      setDragging(false);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointercancel", onCancel);
    window.addEventListener("lostpointercapture", onLost);
    return () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", onCancel);
      window.removeEventListener("lostpointercapture", onLost);
      if (frame !== null) cancelAnimationFrame(frame);
    };
  }, [dragging, tabId]);

  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize inbox"
      aria-valuenow={width}
      aria-valuemin={INBOX_RAIL_WIDTH_MIN}
      tabIndex={0}
      className="ibx-splitter"
      data-testid="inbox-splitter"
      data-dragging={dragging ? "true" : "false"}
      onPointerDown={(e) => {
        if (e.button !== 0) return;
        e.preventDefault();
        const max = maxWidth();
        drag.current = {
          pointerId: e.pointerId,
          startX: e.clientX,
          startWidth: clamp(width, max),
          max,
        };
        // Captured, the drag keeps its pointer even when released outside the window.
        try {
          e.currentTarget.setPointerCapture(e.pointerId);
        } catch {
          // No active pointer to capture (a synthetic event): the window listeners still work.
        }
        setDragging(true);
      }}
      onDoubleClick={() => commit(INBOX_RAIL_WIDTH_DEFAULT)}
      onKeyDown={(e) => {
        const step: Record<string, () => void> = {
          ArrowLeft: () => commit(width + KEYBOARD_STEP_PX),
          ArrowRight: () => commit(width - KEYBOARD_STEP_PX),
          Home: () => commit(INBOX_RAIL_WIDTH_MIN),
          End: () => commit(maxWidth()),
          Enter: () => commit(INBOX_RAIL_WIDTH_DEFAULT),
        };
        const act = step[e.key];
        if (act) {
          e.preventDefault();
          act();
        }
      }}
    />
  );
}
