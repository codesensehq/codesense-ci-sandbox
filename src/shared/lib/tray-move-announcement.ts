// SPDX-License-Identifier: MIT
/**
 * The last move made from each team's workspace tray (a drop or a keyboard step), for the tray's
 * live region. Plain module state, read through React's external-store hook and importing no app
 * module, so the drag code that records a move and the tray that announces it share it without a
 * store. The tray clears a move once it has announced it (or finds it stale), so a tray that
 * mounts again never repeats an old one. Each team keeps its own: a move in one team's tray never
 * replaces another team's move before it is announced.
 */

import { useCallback, useSyncExternalStore } from "react";

/** One move. `seq` tells two identical moves apart, so each is announced. */
export interface TrayMove {
  teamKey: string;
  workspaceId: string;
  place: { before: string } | { after: string };
  seq: number;
}

const lastMoves = new Map<string, TrayMove>();
let lastSeq = 0;
const listeners = new Set<() => void>();
const notify = () => listeners.forEach((l) => l());

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Record a move made from `teamKey`'s tray. */
export function recordTrayMove(
  teamKey: string,
  workspaceId: string,
  place: TrayMove["place"],
): void {
  lastSeq += 1;
  lastMoves.set(teamKey, { teamKey, workspaceId, place, seq: lastSeq });
  notify();
}

/** Clear `teamKey`'s move: any, or only the move `seq` (a newer one stays). */
export function clearTrayMove(teamKey: string, seq?: number): void {
  const move = lastMoves.get(teamKey);
  if (!move || (seq !== undefined && move.seq !== seq)) return;
  lastMoves.delete(teamKey);
  notify();
}

/** The last move made from `teamKey`'s tray, or null. */
export function useLastTrayMove(teamKey: string): TrayMove | null {
  const read = useCallback(() => lastMoves.get(teamKey) ?? null, [teamKey]);
  return useSyncExternalStore(subscribe, read);
}
