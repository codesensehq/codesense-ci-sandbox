// SPDX-License-Identifier: MIT
/**
 * One workspace order per team, shared by the bottom tray's pills and the Agents pane's cards.
 *
 * The order starts as the tray's computed order (`trayWorkspaceOrder`: base workspaces, then each
 * epic's group in pipeline order, then the ungrouped ones). A person's drag, in either place, is
 * recorded as one stored list of workspace ids for the team, and any workspace can move anywhere
 * in it: the tray draws whatever order results (a rule wherever the epic changes, and in grouped
 * mode each epic's pills in the order they come in).
 *
 * A workspace the stored list does not know (newly opened) takes the place the computed order
 * gives it, right after its computed predecessor. Opening, minimising or restoring never changes
 * the order: only a drag does. Pure — no store reads.
 */

import type { SoftClosedWorkspace, Workspace } from "@/shared/stores/viewer-store";
import { getKindCapabilities } from "@/shared/lib/workspace-kind-capabilities";
import { CUSTOM_RAIL_PREFIX } from "./workspace-groups";
import { trayWorkspaceOrder } from "./group-workspaces-by-epic";

/** Where a moved workspace lands: just before or just after another workspace of its team. */
export type OrderPlace = { before: string } | { after: string };

/**
 * Apply a stored order to a computed one. The workspaces the stored list knows follow it; every
 * other workspace (newly opened) sits right after the one that precedes it in the computed order
 * (or first, when it is first there). With `groupOf` (each item's epic group, as the tray's rail
 * keys name them) a newcomer lands beside its own group's members wherever the stored order put
 * them: after its nearest computed predecessor in its group, else before its nearest computed
 * successor in it; only a group with no other member falls back to the plain predecessor.
 */
export function applyWorkspaceOrder<T>(
  items: readonly T[],
  idOf: (item: T) => string,
  stored: readonly string[] | undefined,
  groupOf?: (item: T) => string | undefined,
): T[] {
  if (!stored || stored.length === 0) return [...items];
  const rank = new Map(stored.map((id, i) => [id, i]));
  const placed = items
    .filter((m) => rank.has(idOf(m)))
    .sort((a, b) => rank.get(idOf(a))! - rank.get(idOf(b))!);
  const isPlaced = new Set(placed);
  items.forEach((m, i) => {
    if (rank.has(idOf(m))) return;
    placed.splice(newcomerAt(items, i, placed, isPlaced, groupOf), 0, m);
    isPlaced.add(m);
  });
  return placed;
}

/** Where the newcomer `items[i]` goes in `placed` (see `applyWorkspaceOrder`). */
function newcomerAt<T>(
  items: readonly T[],
  i: number,
  placed: readonly T[],
  isPlaced: ReadonlySet<T>,
  groupOf: ((item: T) => string | undefined) | undefined,
): number {
  const group = groupOf?.(items[i]!);
  if (group !== undefined) {
    for (let j = i - 1; j >= 0; j--) {
      const m = items[j]!;
      if (isPlaced.has(m) && groupOf!(m) === group) return placed.indexOf(m) + 1;
    }
    for (let j = i + 1; j < items.length; j++) {
      const m = items[j]!;
      if (isPlaced.has(m) && groupOf!(m) === group) return placed.indexOf(m);
    }
  }
  // Its computed predecessor is already placed (known, or placed earlier in this loop).
  return i === 0 ? 0 : placed.indexOf(items[i - 1]!) + 1;
}

/**
 * A team's order, as the tray draws it and the store reads it: the computed order, with the base
 * last saved for it applied (workspaces opened since take their computed place, beside their own
 * epic's), then the person's stored moves. The one builder for the tray's pills and
 * `teamWorkspaceOrder`, so the two can never draw different orders from the same state.
 */
export function buildTeamOrder<T>(
  computed: readonly T[],
  idOf: (item: T) => string,
  base: readonly string[] | undefined,
  stored: readonly string[] | undefined,
  groupOf?: (item: T) => string | undefined,
): T[] {
  const ordered = applyWorkspaceOrder(
    applyWorkspaceOrder(computed, idOf, base, groupOf),
    idOf,
    stored,
    groupOf,
  );
  if (!groupOf) return ordered;
  const customKeys = [
    ...new Set(
      computed.map(groupOf).filter((key): key is string => !!key?.startsWith(CUSTOM_RAIL_PREFIX)),
    ),
  ];
  if (!customKeys.length) return ordered;
  const front = [" roots", ...customKeys];
  return [
    ...front.flatMap((key) => ordered.filter((item) => groupOf(item) === key)),
    ...ordered.filter((item) => !front.includes(groupOf(item) ?? "")),
  ];
}

/** The ids of a team's order after moving `id` to `place`, or null when the move is not allowed
 *  (unknown id or anchor) or changes nothing. */
export function moveInOrder(
  order: readonly string[],
  id: string,
  place: OrderPlace,
): string[] | null {
  const anchor = "before" in place ? place.before : place.after;
  if (anchor === id || !order.includes(id) || !order.includes(anchor)) return null;
  const ids = order.filter((x) => x !== id);
  ids.splice(ids.indexOf(anchor) + ("before" in place ? 0 : 1), 0, id);
  return ids.every((x, i) => x === order[i]) ? null : ids;
}

/** The slices of the viewer store a team's order is read from. */
export interface TeamOrderSource {
  /** The tray's computed order, cached when the team's pipeline tree last synced (and saved, so
   *  it holds across a restart until the tree syncs again). */
  base: readonly string[] | undefined;
  workspaces: readonly Workspace[] | undefined;
  softClosed: Readonly<Record<string, SoftClosedWorkspace>> | undefined;
  /** The person's stored order. */
  stored: readonly string[] | undefined;
  /** Each workspace's epic group (the rail keys the tree sync caches), so a newly opened
   *  workspace lands beside its own epic's members, as the tray draws it. */
  groups?: Readonly<Record<string, string>>;
}

/**
 * A team's workspace ids as the tray shows them, from store state alone. The computed order comes
 * from the base the tray's tree sync writes (saved across restarts); a team with no base yet (its
 * tree never synced) falls back to the tree-less computed order, which is the real one for a team
 * of roots and folders. Workspaces opened since the base was written take their computed place.
 */
export function teamWorkspaceOrder(src: TeamOrderSource): string[] {
  // A record of a kind this build does not know (a stale save) has no tray place.
  const knownKind = (w: Workspace | undefined) => !!w && getKindCapabilities(w.kind) !== undefined;
  const softClosed = Object.fromEntries(
    Object.entries(src.softClosed ?? {}).filter(([, v]) => knownKind(v?.workspace)),
  );
  let computed = trayWorkspaceOrder((src.workspaces ?? []).filter(knownKind), softClosed, null).map(
    (e) => e.workspace.id,
  );
  if (src.groups && src.base) computed = applyWorkspaceOrder(computed, (id) => id, src.base);
  const groupOf = src.groups ? (id: string) => src.groups![id] : undefined;
  return buildTeamOrder(computed, (id) => id, src.base, src.stored, groupOf);
}

/**
 * The pane's cards in the shared order. `ids` are a team's cards in launch order. Cards with a
 * pill (their id is in `order`) take the pill order among the slots they occupy; a card with no
 * pill (an epic's shell, another team's workspace) keeps its own slot.
 */
export function arrangeByOrder(ids: readonly string[], order: readonly string[]): string[] {
  const rank = new Map(order.map((id, i) => [id, i]));
  const pills = ids.filter((id) => rank.has(id)).sort((a, b) => rank.get(a)! - rank.get(b)!);
  let next = 0;
  return ids.map((id) => (rank.has(id) ? pills[next++]! : id));
}

/** One card in a team's list, for the drop math: its id, and whether it has a pill (a card
 *  without one cannot move, and a drop next to it anchors on a pill card instead). */
export interface SlotCard {
  id: string;
  pill: boolean;
}

/** Whether `slot` is the dragged card's own place (no pill card between them). */
function isHomeSlot(cards: readonly SlotCard[], from: number, slot: number): boolean {
  const lo = slot <= from ? slot : from + 1;
  const hi = slot <= from ? from : slot;
  return !cards.slice(lo, hi).some((c) => c.pill);
}

/** Where the dragged card would land at slot `slot` (before `cards[slot]`; `cards.length` = after
 *  the last), or null when that slot leaves it where it is. */
export function placeForSlot(
  cards: readonly SlotCard[],
  draggedId: string,
  slot: number,
): OrderPlace | null {
  const from = cards.findIndex((c) => c.id === draggedId);
  if (!cards[from]?.pill || isHomeSlot(cards, from, slot)) return null;
  for (let i = slot - 1; i >= 0; i--) {
    const c = cards[i]!;
    if (c.id !== draggedId && c.pill) return { after: c.id };
  }
  for (let i = slot; i < cards.length; i++) {
    const c = cards[i]!;
    if (c.id !== draggedId && c.pill) return { before: c.id };
  }
  return null;
}

/** The slot a drag shows for a raw pointer slot (clamped to the team's cards) and where the card
 *  would land there; `place` is null at the card's own place. */
export function resolveSlot(
  cards: readonly SlotCard[],
  draggedId: string,
  rawSlot: number,
): { slot: number; place: OrderPlace | null } {
  const slot = Math.max(0, Math.min(cards.length, rawSlot));
  return { slot, place: placeForSlot(cards, draggedId, slot) };
}

/** A keyboard move: one pill card up or down, landing just past it. Null when there is no pill
 *  card that way (the card is at that end of its team). */
export function stepSlot(
  cards: readonly SlotCard[],
  draggedId: string,
  direction: "up" | "down",
): { slot: number; place: OrderPlace } | null {
  const from = cards.findIndex((c) => c.id === draggedId);
  if (!cards[from]?.pill) return null;
  const step = direction === "up" ? -1 : 1;
  for (let i = from + step; i >= 0 && i < cards.length; i += step) {
    const c = cards[i]!;
    if (!c.pill) continue;
    return direction === "up"
      ? { slot: i, place: { before: c.id } }
      : { slot: i + 1, place: { after: c.id } };
  }
  return null;
}
