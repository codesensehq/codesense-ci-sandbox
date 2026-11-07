// SPDX-License-Identifier: MIT
/**
 * Reorder by dragging a pill along the tray. The pill is already a dnd-kit draggable (a drag into
 * the terminal grid splits a tile); while that drag's pointer stays in the tray's band this hook
 * works out the slot between pills, shows an insertion line there, scrolls the strip near its
 * ends, and records the reorder for the workbench's drag end (see `tray-reorder-drag`). Leaving the
 * band drops the intent, so the drag goes back to being a split.
 *
 * The slot is worked out at once on every pointer move (and on the release itself, before the
 * workbench's drag end reads it), so a release lands where the line was drawn; only the edge
 * auto-scroll runs per frame. The boxes a move reads (the strip's, the pills', the tiles' drop
 * zones) are measured once a frame and reused by the moves within it; a scroll, a resize and the
 * release measure afresh. The band never covers a tile's drop zone: where the two meet (the
 * full-screen CLI's tray sits right above the tiles' top edges), a release splits.
 */

import { useDndContext } from "@dnd-kit/core";
import { queryAllShown } from "@/shared/lib/warm-scopes";
import { useEffect, useLayoutEffect, useRef, useState } from "react";

import type { TrayWorkspaceEntry } from "../lib/group-workspaces-by-epic";
import { isTrayChipDragId, unwrapDragWorkspaceId } from "../lib/terminal-drag";
import { setTrayReorderIntent, type TrayReorderIntent } from "../lib/tray-reorder-drag";
import { resolveSlot, type SlotCard } from "../lib/workspace-order";

/** How far past the strip's box the pointer still counts as "in the tray", in px. */
export const BAND_SLACK_Y = 12;
export const BAND_SLACK_X = 24;
/** Near the strip's left or right end, the strip scrolls toward the pointer. */
const EDGE_PX = 36;
const MAX_SCROLL_STEP = 12;

/** The tiles' drop zones (each tile's four edges while a workspace is dragged). */
const TILE_ZONE_SELECTOR = "[data-drop-side]";

/** Where the pointer last was, kept from before a drag starts: a release that comes before this
 *  hook's listeners are attached is still read from the right place. One pair of listeners for
 *  the app, held while any tray is mounted. */
let lastPointer: { x: number; y: number; at: number } | null = null;
let pointerTrackers = 0;
const notePointer = (e: PointerEvent) => {
  lastPointer = { x: e.clientX, y: e.clientY, at: e.timeStamp };
};
const TRACK_OPTIONS = { capture: true, passive: true } as const;
/** Start noting the pointer; the returned function stops this caller's share of it. */
function trackPointer(): () => void {
  if (pointerTrackers++ === 0) {
    window.addEventListener("pointerdown", notePointer, TRACK_OPTIONS);
    window.addEventListener("pointermove", notePointer, TRACK_OPTIONS);
  }
  return () => {
    if (--pointerTrackers > 0) return;
    window.removeEventListener("pointerdown", notePointer, TRACK_OPTIONS);
    window.removeEventListener("pointermove", notePointer, TRACK_OPTIONS);
    lastPointer = null;
  };
}

interface Box {
  left: number;
  right: number;
  top: number;
  bottom: number;
}

const contains = (r: Box, x: number, y: number) =>
  x >= r.left && x <= r.right && y >= r.top && y <= r.bottom;

/**
 * Whether the pointer is in the tray's band: within the strip's box plus its slack, and not over
 * a tile's drop zone (that zone's split wins, so the band's slack never overlaps it).
 */
export function pointerInBand(
  x: number,
  y: number,
  strip: Box,
  tileZones: readonly Box[],
): boolean {
  const inSlack =
    y >= strip.top - BAND_SLACK_Y &&
    y <= strip.bottom + BAND_SLACK_Y &&
    x >= strip.left - BAND_SLACK_X &&
    x <= strip.right + BAND_SLACK_X;
  if (!inSlack) return false;
  return !tileZones.some((z) => contains(z, x, y));
}

/** Where the insertion line is drawn, in the strip's scroll coordinates. */
export interface TrayInsertion {
  /** The dragged pill's id. */
  workspaceId: string;
  /** Left edge of the line, inside the strip's scrolled content. */
  x: number;
  /** The line is at the pill's own place: the band is held (a release neither splits nor
   *  moves anything), and the line is drawn dimmed so the drag still reads as live. */
  atHome: boolean;
}

export function useTrayReorder(
  teamKey: string,
  chipArea: HTMLElement | null,
  entries: readonly TrayWorkspaceEntry[],
  /** The computed order the pills are drawn from (the tree's), carried with the intent so the
   *  move lands over the same order the line was drawn on. */
  drawnBase?: readonly string[],
): TrayInsertion | null {
  useEffect(() => trackPointer(), []);
  const { active, activatorEvent } = useDndContext();
  const draggedId = active ? unwrapDragWorkspaceId(active.id) : null;
  const isTrayDrag = active != null && isTrayChipDragId(active.id);
  const inThisTray =
    isTrayDrag && draggedId !== null && entries.some((e) => e.workspace.id === draggedId);
  const [insertion, setInsertion] = useState<TrayInsertion | null>(null);
  const entryIdsRef = useRef<ReadonlySet<string>>(new Set());
  const drawnBaseRef = useRef(drawnBase);
  const activatorRef = useRef(activatorEvent);
  // This tray's own intent slot, so another mounted tray never clears or overwrites it.
  const ownerRef = useRef<object>({});
  useLayoutEffect(() => {
    entryIdsRef.current = new Set(entries.map((e) => e.workspace.id));
    drawnBaseRef.current = drawnBase;
    activatorRef.current = activatorEvent;
  });

  useEffect(() => {
    if (!inThisTray || !chipArea || draggedId === null) return;
    const owner = ownerRef.current;
    const setIntent = (next: TrayReorderIntent | null) => setTrayReorderIntent(next, owner);
    // Seeded from where the drag started (or where the pointer went since), so a quick release
    // still reads a pointer.
    const start = activatorRef.current as PointerEvent | null;
    const fromStart = start && "clientX" in start ? { x: start.clientX, y: start.clientY } : null;
    const since = lastPointer && start && lastPointer.at >= start.timeStamp ? lastPointer : null;
    let pointer: { x: number; y: number } | null = since ? { x: since.x, y: since.y } : fromStart;
    let frame = 0;
    let cancelled = false;
    let inBand = false;
    // The boxes measured this frame, dropped at the next one (or by a scroll, a resize, a release).
    let measured: {
      box: DOMRect;
      zones: DOMRect[];
      cards: SlotCard[];
      rects: DOMRect[];
      scale: number;
    } | null = null;
    let expiry = 0;
    const forget = () => {
      measured = null;
      if (expiry) cancelAnimationFrame(expiry);
      expiry = 0;
    };
    const measure = () => {
      if (measured) return measured;
      const box = chipArea.getBoundingClientRect();
      const zones = queryAllShown(TILE_ZONE_SELECTOR).map((z) => z.getBoundingClientRect());
      // The slot cards come from the drawn chips themselves, so the rects and the cards always
      // share one index space. A chip the tray's list no longer holds (one leaving a beat late)
      // keeps its slot but is never an anchor.
      const chips = Array.from(chipArea.querySelectorAll<HTMLElement>("[data-tray-chip]"));
      const ids = entryIdsRef.current;
      const cards: SlotCard[] = chips.map((c) => {
        const id = c.dataset.trayChip ?? "";
        return { id, pill: ids.has(id) };
      });
      const rects = chips.map((c) => c.getBoundingClientRect());
      // The bar can be zoomed (Settings → Workspace bar size): rects are zoomed, `left` is not.
      const scale = chipArea.offsetWidth > 0 ? box.width / chipArea.offsetWidth : 1;
      measured = { box, zones, cards, rects, scale };
      expiry = requestAnimationFrame(() => {
        expiry = 0;
        measured = null;
      });
      return measured;
    };
    const update = () => {
      if (!pointer || cancelled) return;
      const { box, zones, cards, rects, scale } = measure();
      inBand = pointerInBand(pointer.x, pointer.y, box, zones);
      if (!inBand) {
        setIntent(null);
        setInsertion(null);
        return;
      }
      const raw = rects.filter((r) => r.left + r.width / 2 < pointer!.x).length;
      const { slot, place } = resolveSlot(cards, draggedId, raw);
      setIntent({ teamKey, workspaceId: draggedId, place, base: drawnBaseRef.current });
      // The line sits in the gap before the slot's pill, or just after the last one.
      const gapLeft =
        slot < rects.length
          ? rects[slot]!.left - (slot > 0 ? (rects[slot]!.left - rects[slot - 1]!.right) / 2 : 2)
          : (rects[rects.length - 1]?.right ?? box.left) + 2;
      const x = Math.max(
        1,
        Math.round((gapLeft - box.left) / (scale || 1) + chipArea.scrollLeft - 1),
      );
      const atHome = place === null;
      setInsertion((prev) =>
        prev && prev.x === x && prev.atHome === atHome
          ? prev
          : { workspaceId: draggedId, x, atHome },
      );
    };
    // Edge auto-scroll: while the pointer is in the band near an end of the strip, scroll a step
    // a frame. Past the band's slack (a pill dragged off toward a tile) the strip stays put.
    const tick = () => {
      frame = 0;
      if (!pointer || cancelled) return;
      const box = chipArea.getBoundingClientRect();
      // In the band as the last move measured it (the strip, its slack, no tile zone).
      const inRow = inBand;
      let step = 0;
      const canLeft = chipArea.scrollLeft > 0;
      const canRight = chipArea.scrollLeft + chipArea.clientWidth < chipArea.scrollWidth - 1;
      if (inRow && canLeft && pointer.x < box.left + EDGE_PX) {
        step = -Math.ceil(((box.left + EDGE_PX - pointer.x) / EDGE_PX) * MAX_SCROLL_STEP);
      } else if (inRow && canRight && pointer.x > box.right - EDGE_PX) {
        step = Math.ceil(((pointer.x - (box.right - EDGE_PX)) / EDGE_PX) * MAX_SCROLL_STEP);
      }
      if (step === 0) return;
      const before = chipArea.scrollLeft;
      chipArea.scrollLeft = before + Math.max(-MAX_SCROLL_STEP, Math.min(MAX_SCROLL_STEP, step));
      // The strip's scroll event recomputes the slot.
      if (chipArea.scrollLeft !== before) frame = requestAnimationFrame(tick);
    };
    const onMove = (e: PointerEvent) => {
      pointer = { x: e.clientX, y: e.clientY };
      cancelled = false;
      update();
      if (!frame) frame = requestAnimationFrame(tick);
    };
    // The release's own position settles the slot before the team view's drag end reads it
    // (window capture runs ahead of dnd-kit's document listener).
    const onUp = (e: PointerEvent) => {
      pointer = { x: e.clientX, y: e.clientY };
      cancelled = false;
      forget();
      update();
    };
    // A scroll mid-drag (wheel, trackpad, the auto-scroll) moves the pills under the pointer.
    const onScroll = () => {
      forget();
      update();
    };
    // Leaving the app mid-drag (another window, another space) may hold back the release. No
    // intent is left standing meanwhile: the line and the intent go, and tracking resumes when
    // the app has the pointer again (focus back, a move, or the release itself), so a drag that
    // carries on reads where it is.
    const onBlur = () => {
      if (cancelled) return;
      cancelled = true;
      if (frame) cancelAnimationFrame(frame);
      frame = 0;
      inBand = false;
      forget();
      setIntent(null);
      setInsertion(null);
    };
    // Focus back: the next move or the release places the line from where the pointer is then
    // (the last one noted is from before the app lost it).
    const resume = () => {
      cancelled = false;
    };
    const onVisibility = () => {
      if (document.visibilityState === "hidden") onBlur();
      else resume();
    };
    window.addEventListener("pointermove", onMove, { passive: true });
    window.addEventListener("pointerup", onUp, { capture: true });
    window.addEventListener("resize", forget);
    window.addEventListener("blur", onBlur);
    window.addEventListener("focus", resume);
    document.addEventListener("visibilitychange", onVisibility);
    chipArea.addEventListener("scroll", onScroll, { passive: true });
    // The pointer is already somewhere: place the line (and the intent) before the first move.
    update();
    return () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp, { capture: true });
      window.removeEventListener("resize", forget);
      window.removeEventListener("blur", onBlur);
      window.removeEventListener("focus", resume);
      document.removeEventListener("visibilitychange", onVisibility);
      chipArea.removeEventListener("scroll", onScroll);
      if (frame) cancelAnimationFrame(frame);
      forget();
      // An unmount or a team change mid-drag must not leave a reorder behind for a later drop.
      setIntent(null);
      setInsertion(null);
    };
  }, [inThisTray, chipArea, draggedId, teamKey]);

  return inThisTray ? insertion : null;
}
