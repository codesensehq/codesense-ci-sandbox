// SPDX-License-Identifier: MIT
// ---------------------------------------------------------------------------
// SW-EW-33 — directional pane focus (Cmd+Opt+arrows). Given the on-screen tile
// rectangles, pick the nearest pane in a pressed direction. Pure + geometric so
// it works on ANY mosaic arrangement (nested splits, not just a grid) and maps
// to what the user actually sees — no coordinate addressing, no tree walking.
// ---------------------------------------------------------------------------

import { queryAllShown } from "@/shared/lib/warm-scopes";
import { focusWorkspaceTerminal } from "./focus-cascade";

export type FocusDirection = "left" | "right" | "up" | "down";

/** A tile's id + its on-screen bounding box (the subset of DOMRect we use). */
export interface TileRect {
  id: string;
  left: number;
  right: number;
  top: number;
  bottom: number;
}

interface Center {
  x: number;
  y: number;
}

const centerOf = (r: TileRect): Center => ({
  x: (r.left + r.right) / 2,
  y: (r.top + r.bottom) / 2,
});

/**
 * Resolve the workspace id of the tile adjacent to `currentId` in `direction`,
 * or `null` when there's no tile that way.
 *
 * Heuristic (standard spatial navigation): a candidate must (1) sit on the
 * correct side of the current tile's center and (2) OVERLAP the current tile's
 * span on the perpendicular axis — so "down" only reaches a pane actually below
 * (sharing horizontal extent), never a diagonal pane in a different column. Among
 * qualifiers, score = travel-axis gap + a penalty on cross-axis misalignment, so
 * the nearest aligned neighbor wins. Returns null when nothing qualifies (e.g.
 * "down" from a full-height pane with only side neighbors).
 */
export function resolveDirectionalFocus(
  tiles: readonly TileRect[],
  currentId: string,
  direction: FocusDirection,
): string | null {
  const current = tiles.find((t) => t.id === currentId);
  if (!current) return null;
  const c = centerOf(current);
  const EPS = 1;
  const OVERLAP_TOL = 1; // treat a shared edge as non-overlapping
  const CROSS_PENALTY = 2; // weight cross-axis misalignment so the aligned neighbor wins

  const overlapsX = (t: TileRect): boolean =>
    t.left < current.right - OVERLAP_TOL && t.right > current.left + OVERLAP_TOL;
  const overlapsY = (t: TileRect): boolean =>
    t.top < current.bottom - OVERLAP_TOL && t.bottom > current.top + OVERLAP_TOL;

  let best: { id: string; score: number } | null = null;
  for (const t of tiles) {
    if (t.id === currentId) continue;
    const tc = centerOf(t);
    let qualifies: boolean;
    let travel: number; // distance along the pressed axis
    let cross: number; // misalignment on the perpendicular axis
    switch (direction) {
      case "left":
        qualifies = tc.x < c.x - EPS && overlapsY(t);
        travel = c.x - tc.x;
        cross = Math.abs(tc.y - c.y);
        break;
      case "right":
        qualifies = tc.x > c.x + EPS && overlapsY(t);
        travel = tc.x - c.x;
        cross = Math.abs(tc.y - c.y);
        break;
      case "up":
        qualifies = tc.y < c.y - EPS && overlapsX(t);
        travel = c.y - tc.y;
        cross = Math.abs(tc.x - c.x);
        break;
      case "down":
        qualifies = tc.y > c.y + EPS && overlapsX(t);
        travel = tc.y - c.y;
        cross = Math.abs(tc.x - c.x);
        break;
    }
    if (!qualifies) continue;
    const score = travel + CROSS_PENALTY * cross;
    if (best === null || score < best.score) best = { id: t.id, score };
  }
  return best?.id ?? null;
}

const OPPOSITE: Record<FocusDirection, FocusDirection> = {
  left: "right",
  right: "left",
  up: "down",
  down: "up",
};

/**
 * Like {@link resolveDirectionalFocus}, but WRAPS at the edge: pressing Left on
 * the leftmost tile lands on the rightmost, Up on the topmost lands on the
 * bottommost (and vice-versa). When there's no neighbor in `direction`, walk the
 * chain in the OPPOSITE direction to its far edge and land there. Because each
 * step still requires perpendicular overlap, the wrap stays within the current
 * row (left/right) or column (up/down) rather than jumping diagonally. Returns
 * null only when there's nothing to wrap to (a lone tile in that row/column).
 */
export function resolveDirectionalFocusWithWrap(
  tiles: readonly TileRect[],
  currentId: string,
  direction: FocusDirection,
): string | null {
  const direct = resolveDirectionalFocus(tiles, currentId, direction);
  if (direct) return direct;

  const opposite = OPPOSITE[direction];
  const seen = new Set<string>([currentId]);
  let cursor = currentId;
  for (;;) {
    const next = resolveDirectionalFocus(tiles, cursor, opposite);
    if (!next || seen.has(next)) break;
    seen.add(next);
    cursor = next;
  }
  return cursor === currentId ? null : cursor;
}

/** Read the on-screen mosaic tile rects (each tile frame carries
 *  `data-pane-id`). Only VISIBLE tiles are returned — non-active
 *  teams' mounts are `display:none` (zero-size rect), so visibility scoping
 *  naturally excludes them without fragile repoPath matching. DOM-only — kept
 *  out of the pure resolver so that stays unit-testable. */
export function readPaneRects(): TileRect[] {
  if (typeof document === "undefined") return [];
  const out: TileRect[] = [];
  for (const el of queryAllShown("[data-pane-id]")) {
    const id = el.getAttribute("data-pane-id");
    if (!id) continue;
    // SW-EW-46: a pane frame that CONTAINS another pane frame is a container — the
    // expanded workspace tile hosting a terminal grid (its cells carry their own
    // `data-pane-id`) — not a navigable leaf. Skip it so directional nav
    // resolves between the cells, never jumping to the enclosing tile. In the plain
    // mosaic no workspace frame nests another, so this changes nothing there.
    if (el.querySelector("[data-pane-id]")) continue;
    const r = el.getBoundingClientRect();
    if (r.width <= 0 || r.height <= 0) continue; // hidden (other team / collapsed)
    out.push({ id, left: r.left, right: r.right, top: r.top, bottom: r.bottom });
  }
  return out;
}

/** SW-EW-33: focus the pane adjacent to the active workspace in `direction`.
 *  Reads the visible mosaic tiles, resolves geometrically, marks it the focused
 *  tile via `switchWorkspace`, AND moves the keyboard cursor into that pane's
 *  terminal so the user can type there immediately (a tiled mosaic keeps every
 *  pane's terminal visible, so switching the active tile alone would leave the
 *  cursor stranded in the old pane). Wraps at the edge (leftmost + Left →
 *  rightmost, etc.); a no-op only when there is no other visible tile (one-tile
 *  mosaic, or zoomed — a zoomed mosaic exposes a single tile so readPaneRects
 *  returns one and nothing qualifies). Shared by the registry action (bubble) and
 *  the capture-phase intercept (terminal-focused — arrows are xterm-consumed, so
 *  the intercept is the load-bearing path). `switchFn` is injected to keep the
 *  resolver store-decoupled + testable.
 *
 *  Focus is deliberately SYNCHRONOUS — NOT routed through the OC-5 activation
 *  coordinator / rAF generation fence. Directional focus only moves the cursor
 *  among tiles already on-screen; it never activates a hidden/minimized
 *  workspace, so there is no activation to race and no fence to need. The single
 *  getBoundingClientRect read per keypress is not on a scroll path, so key-repeat
 *  cannot batch forced reflows.
 *
 *  Returns `true` when it moved focus, `false` on the no-op (no active workspace,
 *  or no tile in `direction`) so the caller can surface the blocked-nav cue. */
export function focusAdjacentPane(
  teamKey: string,
  activeWorkspaceId: string | undefined,
  direction: FocusDirection,
  switchFn: (teamKey: string, workspaceId: string) => void,
): boolean {
  if (!activeWorkspaceId) return false;
  const target = resolveDirectionalFocusWithWrap(readPaneRects(), activeWorkspaceId, direction);
  if (!target) return false;
  switchFn(teamKey, target);
  focusWorkspaceTerminal(target);
  return true;
}
