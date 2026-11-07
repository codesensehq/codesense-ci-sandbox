// SPDX-License-Identifier: MIT
import type { AttentionBoardDto, AttentionNeed, AttentionRowDto } from "@/tauri/commands";

/** The three plain Inbox lanes (north-star §8.13 — one surface, three lanes). */
export type InboxLaneId = "needs-you" | "running" | "done";

/** One normalized Inbox item — the unit ONE row renderer consumes, keyed by `need`. */
export interface InboxItem {
  /** Stable, collision-free key (lane-scoped id). */
  key: string;
  /** The underlying board row (summary + backlink + provenance + live handle). */
  row: AttentionRowDto;
  /** The DECLARED need, or `null` for a legacy weft row / a running-agent row (the
   *  row renderer then falls back to a lane-plain label). */
  need: AttentionNeed | null;
  lane: InboxLaneId;
}

/** SLW-TL-4: the plain, human-readable label for each declared need. Neutral inbox
 *  vocabulary — no YOU-OWE / endorsement / tier words. */
export const NEED_LABEL: Record<AttentionNeed, string> = {
  "awaiting-answer": "Awaiting answer",
  "needs-approval": "Needs approval",
  "needs-attention": "Needs attention",
  "awaiting-validation": "Awaiting validation",
  done: "Done",
};

/** The plain per-lane fallback label a row shows when it declares no need (a legacy
 *  weft row, or a running-agent row). */
export const LANE_FALLBACK_LABEL: Record<InboxLaneId, string> = {
  "needs-you": "Needs you",
  running: "Working",
  done: "Done",
};

/** The count of items that NEED the human — what the nav badge and the passive
 *  "N need you" count paint (with a NEUTRAL color, never alarm-red — §8.10). */
export function countNeedsYou(board: AttentionBoardDto): number {
  return board.youOwe.length + board.owedToYou.length;
}

/** True when the whole board holds nothing — the calm first-open state. Must not
 *  presume a fleet (§8.9); the caller paints "Nothing needs you right now." */
export function isBoardEmpty(board: AttentionBoardDto): boolean {
  return (
    board.youOwe.length === 0 &&
    board.owedToYou.length === 0 &&
    board.onAgent.length === 0 &&
    board.parked.length === 0 &&
    board.doneUnacked.length === 0
  );
}
