// SPDX-License-Identifier: MIT
/**
 * Drag a workspace card in the Agents pane to move it, with its agents, among its team's cards.
 *
 * Pointer events on the card's head row, with the same 6px threshold the right pane's drags use,
 * so a click (fold, the name, the stage) never starts a drag. While dragging the card's agents fold
 * out of the list, one floating preview follows the pointer and one line marks the gap it would
 * land in. The gap is worked out from the flat row list and the virtualizer's row positions every
 * move and every auto-scroll frame, never from drawn rows, so it holds when rows scroll out of the
 * drawn window. Escape, a pointer cancel, or a release outside the team puts the card back, as
 * does a mouse that moves with no button down (its release was never delivered).
 *
 * A press starts no drag when the card has nowhere to go: its team shows fewer than two movable
 * cards (a folded team, one card under the workspace arrow). Drags while a filter hides some of a
 * team's cards (a search, a Show filter, the agent chip), in the By status arrangement, or of a
 * card with no pill, are the caller's to refuse (`refusal`): with cards hidden, a drop between two
 * shown cards could mean any place among the hidden ones, so such a drag must be refused, not
 * guessed. A refused press stays a click; dragged past the threshold, it says why (`onRefused`)
 * instead of doing nothing.
 */

import { useCallback, useEffect, useLayoutEffect, useRef, useState, type RefObject } from "react";

import type { PaneRow } from "../lib/agents-pane-rows";
import {
  CARD_MOVE_REFUSED,
  cardDropTarget,
  teamSlotCards,
  type RowBox,
} from "../lib/agents-pane-drag";
import type { OrderPlace } from "../lib/workspace-order";

/** Movement before a press becomes a drag: the right pane's dnd-kit threshold. */
export const CARD_DRAG_THRESHOLD_PX = 6;
/** Near the list's top or bottom edge the list scrolls toward the pointer. */
const EDGE_PX = 44;
const MAX_SCROLL_STEP = 16;

export interface CardDrag {
  teamKey: string;
  workspaceId: string;
  /** The preview's top, in list px. */
  previewTop: number;
  /** The drop line, in list px; null when no drop would happen here. */
  lineY: number | null;
  /** Whether the pointer has left the team (the drop is refused). */
  outside: boolean;
}

interface Press {
  pointerId: number;
  teamKey: string;
  workspaceId: string;
  x: number;
  y: number;
  /** Pointer to the head row's top, so the preview keeps the grab point. */
  grabY: number;
  /** Why this press cannot move the card, said once it is dragged; null for a movable card. */
  refused: string | null;
}

export interface UseCardDragArgs {
  scrollRef: RefObject<HTMLDivElement | null>;
  /** The pane's rows as last rendered, and each row's place (from the virtualizer). */
  rowsRef: RefObject<readonly PaneRow[]>;
  boxesRef: RefObject<() => readonly (RowBox | undefined)[]>;
  orderOf: (teamKey: string) => readonly string[];
  /** A release that moves the card; `previewTop` is where the preview was let go (list px). */
  onDrop: (teamKey: string, workspaceId: string, place: OrderPlace, previewTop: number) => void;
  /** The list is about to change shape without a move (the drag starts and folds the card's
   *  agents, or it is cancelled and they come back), so drawn rows can glide to their new places. */
  onReflow?: (teamKey: string, workspaceId: string) => void;
  /** Why a press on this card cannot move it (a search or filter, By status, no pill), or null.
   *  Asked at the press, before the hook's own "nowhere to move" check. */
  refusal?: (teamKey: string, workspaceId: string) => string | null;
  /** A refused press was dragged: the words to say in the pane's live region. */
  onRefused?: (words: string) => void;
}

export function useWorkspaceCardDrag({
  scrollRef,
  rowsRef,
  boxesRef,
  orderOf,
  onDrop,
  onReflow,
  refusal,
  onRefused,
}: UseCardDragArgs) {
  const [drag, setDrag] = useState<CardDrag | null>(null);
  const press = useRef<Press | null>(null);
  const active = useRef(false);
  const pointer = useRef<{ x: number; y: number } | null>(null);
  const target = useRef<{ place: OrderPlace | null }>({ place: null });
  const frame = useRef(0);
  const cleanup = useRef<(() => void) | null>(null);
  const orderOfRef = useRef(orderOf);
  const onDropRef = useRef(onDrop);
  const onReflowRef = useRef(onReflow);
  const refusalRef = useRef(refusal);
  const onRefusedRef = useRef(onRefused);
  const lastPreviewTop = useRef(0);
  useLayoutEffect(() => {
    orderOfRef.current = orderOf;
    onDropRef.current = onDrop;
    onReflowRef.current = onReflow;
    refusalRef.current = refusal;
    onRefusedRef.current = onRefused;
  });

  /** Recompute the slot and the preview from the pointer and the current row positions. */
  const measure = useCallback(() => {
    const p = press.current;
    const at = pointer.current;
    const scroller = scrollRef.current;
    if (!p || !at || !scroller || !active.current) return;
    const box = scroller.getBoundingClientRect();
    const y = at.y - box.top + scroller.scrollTop;
    const t = cardDropTarget(
      rowsRef.current,
      boxesRef.current(),
      p.teamKey,
      p.workspaceId,
      y,
      orderOfRef.current(p.teamKey),
      { x: at.x, left: box.left, right: box.right },
    );
    target.current = { place: t.place };
    lastPreviewTop.current = y - p.grabY;
    document.documentElement.setAttribute("data-ag-card-drag", t.slot === null ? "refused" : "");
    const next: CardDrag = {
      teamKey: p.teamKey,
      workspaceId: p.workspaceId,
      previewTop: y - p.grabY,
      lineY: t.lineY,
      outside: t.slot === null,
    };
    setDrag((prev) =>
      prev &&
      prev.previewTop === next.previewTop &&
      prev.lineY === next.lineY &&
      prev.outside === next.outside
        ? prev
        : next,
    );
  }, [scrollRef, rowsRef, boxesRef]);

  const end = useCallback((commit: boolean) => {
    const p = press.current;
    const wasActive = active.current;
    const place = target.current.place;
    press.current = null;
    active.current = false;
    pointer.current = null;
    target.current = { place: null };
    if (frame.current) cancelAnimationFrame(frame.current);
    frame.current = 0;
    cleanup.current?.();
    cleanup.current = null;
    document.documentElement.removeAttribute("data-ag-card-drag");
    const moves = wasActive && commit && p && place;
    if (wasActive && p && !moves) onReflowRef.current?.(p.teamKey, p.workspaceId);
    setDrag(null);
    if (moves) onDropRef.current(p.teamKey, p.workspaceId, place, lastPreviewTop.current);
  }, []);

  const begin = useCallback(
    (e: React.PointerEvent<HTMLElement>, teamKey: string, workspaceId: string) => {
      // The primary button only, unmodified: a modified press keeps its own meaning.
      if (e.button !== 0 || e.metaKey || e.ctrlKey || e.altKey || e.shiftKey || press.current) {
        return;
      }
      // Nowhere to move (over the same cards the drop math reads): the press stays a plain
      // click, and a drag of it says why.
      const movable = teamSlotCards(rowsRef.current, teamKey, orderOfRef.current(teamKey), {
        measured: boxesRef.current(),
      }).filter((c) => c.pill);
      const refused =
        refusalRef.current?.(teamKey, workspaceId) ??
        (movable.length < 2 || !movable.some((c) => c.id === workspaceId)
          ? CARD_MOVE_REFUSED.nowhere
          : null);
      if (refused && !onRefusedRef.current) return;
      const head = e.currentTarget.getBoundingClientRect();
      press.current = {
        pointerId: e.pointerId,
        teamKey,
        workspaceId,
        x: e.clientX,
        y: e.clientY,
        grabY: e.clientY - head.top,
        refused,
      };
      // A refused press dragged past the threshold has said why: its release is not a click.
      let said = false;
      // Auto-scroll: while the pointer is near the list's top or bottom edge, scroll toward it and
      // recompute the slot every frame.
      const tick = () => {
        frame.current = 0;
        const scroller = scrollRef.current;
        const at = pointer.current;
        if (!scroller || !at || !active.current) return;
        const box = scroller.getBoundingClientRect();
        let step = 0;
        const canUp = scroller.scrollTop > 0;
        const canDown = scroller.scrollTop + scroller.clientHeight < scroller.scrollHeight - 1;
        if (canUp && at.y < box.top + EDGE_PX)
          step = -((box.top + EDGE_PX - at.y) / EDGE_PX) * MAX_SCROLL_STEP;
        else if (canDown && at.y > box.bottom - EDGE_PX)
          step = ((at.y - (box.bottom - EDGE_PX)) / EDGE_PX) * MAX_SCROLL_STEP;
        step = Math.max(-MAX_SCROLL_STEP, Math.min(MAX_SCROLL_STEP, Math.round(step)));
        const before = scroller.scrollTop;
        if (step !== 0) scroller.scrollTop = before + step;
        measure();
        if (step !== 0 && scroller.scrollTop !== before)
          frame.current = requestAnimationFrame(tick);
      };
      const onMove = (ev: PointerEvent) => {
        const p = press.current;
        if (!p || ev.pointerId !== p.pointerId) return;
        // A mouse moving with no button down was released where we never heard it (outside the
        // window, under a native menu): put the card back.
        if (ev.pointerType === "mouse" && ev.buttons === 0) {
          end(false);
          return;
        }
        pointer.current = { x: ev.clientX, y: ev.clientY };
        if (said) return;
        if (!active.current) {
          if (Math.hypot(ev.clientX - p.x, ev.clientY - p.y) < CARD_DRAG_THRESHOLD_PX) return;
          if (p.refused) {
            said = true;
            onRefusedRef.current?.(p.refused);
            return;
          }
          active.current = true;
          onReflowRef.current?.(p.teamKey, p.workspaceId);
          document.documentElement.setAttribute("data-ag-card-drag", "");
          window.getSelection()?.removeAllRanges();
        }
        ev.preventDefault();
        measure();
        if (!frame.current) frame.current = requestAnimationFrame(tick);
      };
      const onUp = (ev: PointerEvent) => {
        if (ev.pointerId !== press.current?.pointerId) return;
        if (active.current || said) {
          // The release ends a drag, not a click: the click that follows is swallowed once.
          const swallow = (c: MouseEvent) => {
            c.stopPropagation();
            c.preventDefault();
          };
          window.addEventListener("click", swallow, { capture: true, once: true });
          window.setTimeout(() => window.removeEventListener("click", swallow, true), 0);
        }
        end(true);
      };
      const onCancel = () => end(false);
      const onKey = (ev: KeyboardEvent) => {
        if (ev.key !== "Escape" || !press.current) return;
        // Below the threshold there is no drag to cancel: the press ends, and Escape keeps its
        // own meaning (close a popup, leave a field).
        if (active.current) {
          ev.preventDefault();
          ev.stopPropagation();
        }
        end(false);
      };
      // Leaving the app mid-drag (another window, another space) may never deliver the release:
      // put the card back rather than leave it in the hand.
      const onHidden = () => {
        if (document.visibilityState === "hidden") end(false);
      };
      window.addEventListener("pointermove", onMove);
      window.addEventListener("pointerup", onUp);
      window.addEventListener("pointercancel", onCancel);
      window.addEventListener("keydown", onKey, { capture: true });
      window.addEventListener("blur", onCancel);
      document.addEventListener("visibilitychange", onHidden);
      cleanup.current = () => {
        window.removeEventListener("pointermove", onMove);
        window.removeEventListener("pointerup", onUp);
        window.removeEventListener("pointercancel", onCancel);
        window.removeEventListener("keydown", onKey, { capture: true });
        window.removeEventListener("blur", onCancel);
        document.removeEventListener("visibilitychange", onHidden);
      };
    },
    [end, measure, scrollRef, rowsRef, boxesRef],
  );

  // A scroll by wheel or keys mid-drag moves the rows under the pointer: recompute.
  useEffect(() => {
    const scroller = scrollRef.current;
    if (!drag || !scroller) return;
    const onScroll = () => measure();
    scroller.addEventListener("scroll", onScroll, { passive: true });
    return () => scroller.removeEventListener("scroll", onScroll);
  }, [drag !== null, scrollRef, measure]); // eslint-disable-line react-hooks/exhaustive-deps

  // Unmounting mid-drag (the pane closes) drops the drag.
  useEffect(() => () => end(false), [end]);

  return { drag, begin, cancel: () => end(false) };
}
