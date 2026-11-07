// SPDX-License-Identifier: MIT
import { beginContextNavigation } from "@/shared/lib/context-navigation";
/**
 * A tray pill dragged along the tray reorders; dragged up (or down, in the full-screen CLI where
 * the tray sits on top) into the terminal grid it splits a tile. Both run through the workbench's
 * one `DndContext`. While the pointer stays in the tray's band the tray records a reorder intent
 * here; the workbench's drag end takes it first, so a release in the band reorders and never
 * splits. Module state, because the tray and the drag end share no React parent besides the
 * context; kept per owner, so an intent never clobbers another's. Cleared on every drag end and
 * cancel.
 */

import { useSyncExternalStore } from "react";

import { workspaceInTeam } from "@/shared/lib/team-key";
import { recordTrayMove } from "@/shared/lib/tray-move-announcement";
import { useViewerStore } from "@/shared/stores/viewer-store";
import {
  dragEndToReopen,
  isTrayChipDragId,
  resolveTilePlacement,
  unwrapDragWorkspaceId,
} from "./terminal-drag";
import type { OrderPlace } from "./workspace-order";

export interface TrayReorderIntent {
  teamKey: string;
  workspaceId: string;
  /** Null while the pointer is in the band at the pill's own place: a release there does nothing. */
  place: OrderPlace | null;
  /** The computed order the tray drew the line over (tree-derived when the tray has the tree), so
   *  the move lands where the line showed. Absent: the store's saved base. */
  base?: readonly string[];
}

/** Who recorded an intent: each mounted tray its own, so one tray's drag leaving its band (or
 *  unmounting) never clears another's. */
export type TrayReorderOwner = object;
const SHARED_OWNER: TrayReorderOwner = {};

const intents = new Map<TrayReorderOwner, TrayReorderIntent>();
const listeners = new Set<() => void>();
const notify = () => listeners.forEach((l) => l());

export function setTrayReorderIntent(
  next: TrayReorderIntent | null,
  owner: TrayReorderOwner = SHARED_OWNER,
): void {
  const wasInBand = intents.size > 0;
  if (next) intents.set(owner, next);
  else intents.delete(owner);
  if (wasInBand !== intents.size > 0) notify();
}

/** The intent for this drag, if the pointer was last in a tray's band; clears every tray's (the
 *  drag is over). With `match`, only an intent of that team and dragged workspace is returned. */
export function takeTrayReorderIntent(match?: {
  teamKey: string;
  workspaceId: string;
}): TrayReorderIntent | null {
  const all = Array.from(intents.values());
  const out =
    all.find(
      (i) => !match || (i.teamKey === match.teamKey && i.workspaceId === match.workspaceId),
    ) ?? null;
  intents.clear();
  if (all.length > 0) notify();
  return out;
}

/** Whether a pill drag's pointer is in the tray's band right now (a reorder, not a split): the
 *  team view hides its split ghost then, so the insertion line reads on its own. */
export function useTrayReorderInBand(): boolean {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => intents.size > 0,
  );
}

/** A tray drop that could not move its pill (the bar changed under the drag: its anchor went, or
 *  the order already had it there), per team, for the tray's live region. `seq` tells two apart. */
export interface TrayMoveRefusal {
  workspaceId: string;
  seq: number;
}
const refusals = new Map<string, TrayMoveRefusal>();
let refusalSeq = 0;
const refusalListeners = new Set<() => void>();

function recordTrayMoveRefused(teamKey: string, workspaceId: string): void {
  refusalSeq += 1;
  refusals.set(teamKey, { workspaceId, seq: refusalSeq });
  refusalListeners.forEach((l) => l());
}

/** Clear `teamKey`'s refusal: any, or only `seq` (a newer one stays). */
export function clearTrayMoveRefused(teamKey: string, seq?: number): void {
  const r = refusals.get(teamKey);
  if (!r || (seq !== undefined && r.seq !== seq)) return;
  refusals.delete(teamKey);
  refusalListeners.forEach((l) => l());
}

/** The last refused tray drop in `teamKey`, or null. */
export function useTrayMoveRefused(teamKey: string): TrayMoveRefusal | null {
  return useSyncExternalStore(
    (l) => {
      refusalListeners.add(l);
      return () => refusalListeners.delete(l);
    },
    () => refusals.get(teamKey) ?? null,
  );
}

/** Move a workspace through the team's shared order from the tray, and announce it when it
 *  moved. `base` is the computed order the tray drew. Returns whether the order changed. */
export function moveWorkspaceFromTray(
  teamKey: string,
  workspaceId: string,
  place: OrderPlace,
  base?: readonly string[],
): boolean {
  if (!useViewerStore.getState().reorderWorkspace(teamKey, workspaceId, place, base)) return false;
  recordTrayMove(teamKey, workspaceId, place);
  return true;
}

/**
 * The workbench's drag end, first: when the pointer was last in the tray's band, this drop is a
 * reorder (applied here, through the one shared order) and the caller must not split a tile.
 * With `match`, only an intent of that team for that dragged workspace counts: one left by
 * another drag, or another team's tray, is dropped and the release is not taken as a reorder.
 * A move that can't be made is announced (`useTrayMoveRefused`). Returns whether it consumed
 * the drop.
 */
export function consumeTrayReorderDrop(match?: { teamKey: string; workspaceId: string }): boolean {
  const drop = takeTrayReorderIntent(match);
  if (!drop) return false;
  // A release in the band is a reorder even when the move can't be made (it never splits a
  // tile the pointer is not on), so a refused one is said rather than eaten silently.
  if (drop.place && !moveWorkspaceFromTray(drop.teamKey, drop.workspaceId, drop.place, drop.base)) {
    recordTrayMoveRefused(drop.teamKey, drop.workspaceId);
  }
  return true;
}

/**
 * The workbench's whole drag end. A release in the tray's band reorders and never splits; a
 * release on a tile's edge splits there, and one on a tile's center or the grid restores or
 * focuses. The tiles are `teamKey`'s; a workspace that is not in that team is not dropped.
 */
export function dropWorkspace(
  teamKey: string,
  activeId: string | number | null | undefined,
  overId: string | number | null | undefined,
): void {
  // Only a tray pill's own drag reorders (a tile dragged across the band never does).
  if (isTrayChipDragId(activeId)) {
    const dragged = { teamKey, workspaceId: unwrapDragWorkspaceId(activeId) };
    if (consumeTrayReorderDrop(dragged)) return;
  } else takeTrayReorderIntent();
  const store = useViewerStore.getState();
  const minimized = (id: string) => store.softClosedWorkspaces[teamKey]?.[id] != null;
  const inTeam = (id: string) => workspaceInTeam(store, teamKey, id);
  const placement = resolveTilePlacement(activeId, overId);
  if (placement) {
    if (!inTeam(placement.workspaceId)) return;
    beginContextNavigation();
    if (minimized(placement.workspaceId)) {
      store.reopenSoftClosedWorkspace(teamKey, placement.workspaceId, placement.target);
    } else {
      store.placeWorkspaceTile(teamKey, placement.workspaceId, placement.target);
    }
    return;
  }
  const workspaceId = dragEndToReopen(activeId, overId);
  if (workspaceId && inTeam(workspaceId)) store.reopenSoftClosedWorkspace(teamKey, workspaceId);
}
