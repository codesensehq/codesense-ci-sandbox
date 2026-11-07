// SPDX-License-Identifier: MIT
/**
 * The drop math for dragging a workspace card in the Agents pane. It reads only the pane's flat
 * row list and the virtualizer's row positions (which it keeps for every row, drawn or not), so a
 * card or a drop target scrolled out of the drawn rows still lands in the right place. Pure.
 */

import { NO_WORKSPACE_ID } from "@/features/agents/lib/agent-list-layout";
import type { PaneRow } from "./agents-pane-rows";
import { resolveSlot, type OrderPlace, type SlotCard } from "./workspace-order";

/** A row's place in the list, in the list's own coordinates (px from its top). */
export interface RowBox {
  start: number;
  end: number;
}

/** One card of the dragged card's team, for the drop line. */
export interface PaneCardBlock {
  id: string;
  /** The card's row index. */
  index: number;
  /** Where the card's row starts (its air above included). */
  start: number;
  /** Where the card's last agent row (or the card, folded) ends. */
  end: number;
  /** Where the row after the block starts (the next card, the No workspace bucket, the next
   *  team), when there is one and it is measured. */
  next?: number;
}

type WsRow = Extract<PaneRow, { kind: "ws" }>;

/** The row indexes of a team's workspace cards, in list order. The No workspace bucket is not a
 *  card (it never moves and is never a drop anchor) wherever it sits; both the drop line and the
 *  keyboard step read the team's cards through this one rule. */
function teamCardRowIndexes(rows: readonly PaneRow[], teamKey: string): number[] {
  const out: number[] = [];
  const teamAt = rows.findIndex((r) => r.kind === "team" && r.teamKey === teamKey);
  if (teamAt < 0) return out;
  for (let i = teamAt + 1; i < rows.length; i++) {
    const row = rows[i]!;
    if (row.kind === "team") break;
    if (row.kind === "ws" && row.group.workspaceId !== NO_WORKSPACE_ID) out.push(i);
  }
  return out;
}

/** A team's workspace cards (the No workspace bucket left out) with their blocks. A card's block
 *  runs to the next card or team header, whatever rows sit under it (its agents, or any other
 *  row the pane draws inside a card). */
export function teamCardBlocks(
  rows: readonly PaneRow[],
  boxes: readonly (RowBox | undefined)[],
  teamKey: string,
): PaneCardBlock[] {
  const out: PaneCardBlock[] = [];
  for (const i of teamCardRowIndexes(rows, teamKey)) {
    const row = rows[i] as WsRow;
    const head = boxes[i];
    if (!head) continue;
    let last = i;
    while (
      last + 1 < rows.length &&
      rows[last + 1]!.kind !== "ws" &&
      rows[last + 1]!.kind !== "team"
    )
      last += 1;
    const next = boxes[last + 1]?.start;
    out.push({
      id: row.group.workspaceId,
      index: i,
      start: head.start,
      end: boxes[last]?.end ?? head.end,
      ...(next !== undefined ? { next } : {}),
    });
  }
  return out;
}

/** The team's list range: from its header's start to the end of its last row. `before` and
 *  `after` are where the rows around it end and start (another team, the Pinned group), when there
 *  are any: a drop's slack never reaches into them. */
export function teamRange(
  rows: readonly PaneRow[],
  boxes: readonly (RowBox | undefined)[],
  teamKey: string,
): (RowBox & { before?: number; after?: number }) | null {
  const teamAt = rows.findIndex((r) => r.kind === "team" && r.teamKey === teamKey);
  if (teamAt < 0 || !boxes[teamAt]) return null;
  let last = teamAt;
  while (last + 1 < rows.length && rows[last + 1]!.kind !== "team") last += 1;
  return {
    start: boxes[teamAt]!.start,
    end: boxes[last]?.end ?? boxes[teamAt]!.end,
    before: boxes[teamAt - 1]?.end,
    after: boxes[last + 1]?.start,
  };
}

/** How far above the team's header a drag still counts as in the team, in list px. */
export const CARD_DROP_SLACK_ABOVE_PX = 8;
/** How far below the team's last row a drag still counts as in the team (a drop after its last
 *  card), in list px. */
export const CARD_DROP_SLACK_BELOW_PX = 24;
/** How far left or right of the pane a drag still counts as over it, in px. Farther out (over
 *  the tiles, another pane) a release does nothing. */
export const CARD_DROP_SLACK_X_PX = 48;

/** The words for a card press dragged that cannot move it, said in the pane's live region. */
export const CARD_MOVE_REFUSED = {
  filtered: "Cards can't move while a search or filter is on. Clear them to move it.",
  byStatus: "Cards move in the By team arrangement. Switch to it to move this one.",
  noPill: "This card has no pill in the tray, so it has no place in the order to move.",
  nowhere: "This card has no other card in its team to move past.",
} as const;

/** The air the drop line keeps above a card's head, inside the card's row (half its top air). */
export const DROP_LINE_INSET_PX = 4;

export interface CardDropTarget {
  /** Null when the pointer has left the dragged card's team: a release there does nothing. */
  slot: number | null;
  /** Where the card would move; null at its own place or outside the team. */
  place: OrderPlace | null;
  /** Where the line is drawn, in list px; null when no line shows. */
  lineY: number | null;
}

/**
 * A team's workspace cards (the No workspace bucket left out) as slot cards, each marked by
 * whether it has a pill: the one list the drop line, a press and the keyboard step read. With
 * `measured` (the rows' boxes), only the cards whose head has a box: the cards the drop math can
 * place a line by, so a press asks the same list whether its card can move at all.
 */
export function teamSlotCards(
  rows: readonly PaneRow[],
  teamKey: string,
  order: readonly string[],
  { measured }: { measured?: readonly (RowBox | undefined)[] } = {},
): SlotCard[] {
  const pills = new Set(order);
  const ids = measured
    ? teamCardBlocks(rows, measured, teamKey).map((b) => b.id)
    : teamCardRowIndexes(rows, teamKey).map((i) => (rows[i] as WsRow).group.workspaceId);
  return ids.map((id) => ({ id, pill: pills.has(id) }));
}

/**
 * Where a card dragged to list-y `y` would drop. Within its team the pointer picks the gap whose
 * cards' midpoints it is between, anywhere among the team's cards. Outside the team (past its
 * slack, which stops at the rows around it) or far to either side of the pane (`pane`, in screen
 * px): no drop.
 */
export function cardDropTarget(
  rows: readonly PaneRow[],
  boxes: readonly (RowBox | undefined)[],
  teamKey: string,
  draggedId: string,
  y: number,
  order: readonly string[],
  pane?: { x: number; left: number; right: number },
): CardDropTarget {
  const range = teamRange(rows, boxes, teamKey);
  const blocks = teamCardBlocks(rows, boxes, teamKey);
  const top = range
    ? Math.max(range.start - CARD_DROP_SLACK_ABOVE_PX, range.before ?? -Infinity)
    : 0;
  const bottom = range
    ? Math.min(range.end + CARD_DROP_SLACK_BELOW_PX, range.after ?? Infinity)
    : 0;
  const farAside =
    pane !== undefined &&
    (pane.x < pane.left - CARD_DROP_SLACK_X_PX || pane.x > pane.right + CARD_DROP_SLACK_X_PX);
  if (!range || blocks.length === 0 || y < top || y > bottom || farAside) {
    return { slot: null, place: null, lineY: null };
  }
  const pills = new Set(order);
  const cards: SlotCard[] = blocks.map((b) => ({ id: b.id, pill: pills.has(b.id) }));
  const raw = blocks.filter((b) => (b.start + b.end) / 2 < y).length;
  const { slot, place } = resolveSlot(cards, draggedId, raw);
  return { slot, place, lineY: place ? lineYForSlot(blocks, slot) : null };
}

/** Where the drop line for `slot` is drawn: in the top air of the card it lands before, or after
 *  the last card. After the last card it never reaches into the row that follows (the No
 *  workspace bucket, the next team): it stops at that row's top edge. */
function lineYForSlot(blocks: readonly PaneCardBlock[], slot: number): number {
  if (slot < blocks.length) return blocks[slot]!.start + DROP_LINE_INSET_PX;
  const last = blocks[blocks.length - 1]!;
  return Math.min(last.end + DROP_LINE_INSET_PX, last.next ?? Infinity);
}
