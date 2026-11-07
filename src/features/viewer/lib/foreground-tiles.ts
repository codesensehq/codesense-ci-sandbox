// SPDX-License-Identifier: MIT
import type { Workspace } from "@/features/viewer/lib/cli-workspace/store/viewer-access";
import { getKindCapabilities } from "@/shared/lib/workspace-kind-capabilities";
import { applyWorkspaceOrder } from "./workspace-order";

// SW-EW-3 D-1: the foreground (tiled) set IS the open-workspace set — every
// open workspace is conceptually foreground until SW-EW-11 introduces the
// disjoint background set. Membership is therefore *derived* here, not stored;
// only the arrangement (`foregroundLayout`) persists.

/** Below this count the terminal region renders 1-up (today's single-visible
 *  behavior, zero regression — AC-10); at/above it the split tiling kicks in. */
export const SPLIT_MIN_TILES = 2;

/** The single membership predicate: does this workspace tile in the foreground
 *  region. One definition consumed by `foregroundTilesFor` (collect+sort),
 *  `hasForegroundTile` (existence), and the store's rail/flat count sites — so no
 *  call site re-inlines `getKindCapabilities(w.kind).foregroundTile`. */
export const isForegroundTile = (w: Workspace): boolean =>
  getKindCapabilities(w.kind).foregroundTile;

/** The foreground (tiled) workspaces, in a stable order. Membership is the
 *  registry `foregroundTile` capability (every current kind tiles; the old
 *  terminal-less `kind:"epic"` grouping node that was excluded here is gone in
 *  the 4-kind model). The tiles follow the team's shared workspace `order` (the
 *  order its pills and Agents-pane cards follow, `teamTileOrder`): the person's
 *  order where they moved something, else the tray's computed order — so a
 *  mosaic laid out from a preset reads in the pills' order. (A team that never
 *  moved anything follows the tray's computed order, which can differ once from
 *  the id order tiles used before the shared order existed.) A tile the order
 *  does not hold sits after its id-sorted predecessor. Every caller passes the
 *  order, so none can miss it. A custom mosaic keeps its own arrangement either
 *  way. This is the SINGLE source of tile membership — both the renderer and the
 *  split-threshold derive from it so they can't disagree. */
export function foregroundTilesFor(
  workspaces: readonly Workspace[],
  order: readonly string[],
): Workspace[] {
  const byId = workspaces.filter(isForegroundTile).sort((a, b) => a.id.localeCompare(b.id));
  return applyWorkspaceOrder(byId, (w) => w.id, order);
}

/** Whether ANY workspace tiles in the foreground region (a close that empties it
 *  opens the team-folder workspace) — answered without
 *  `foregroundTilesFor`'s sort/allocation, over the SAME membership predicate so
 *  the count and the existence check can't disagree. */
export function hasForegroundTile(workspaces: readonly Workspace[]): boolean {
  return workspaces.some(isForegroundTile);
}

/** True once enough foreground workspaces are open to tile (vs. 1-up). */
export function isSplitActive(tileCount: number): boolean {
  return tileCount >= SPLIT_MIN_TILES;
}

/** SW-EW-40: the epic-scoped foreground tiles for GROUPED tray mode — the
 *  `foregroundTilesFor` set further filtered to those whose rail key equals
 *  `selectedRailKey`. Rail keys come from the store-cached `railKeyByWorkspaceId`
 *  index (built from the SAME `trayWorkspaceOrder`+`railKeyOf` the tray uses), so
 *  the mosaic and the tray can never disagree about which epic a workspace
 *  belongs to. Composes OVER `foregroundTilesFor` (the single membership source)
 *  — it never forks or reorders it, so the team's tile order is preserved and
 *  flat's math is untouched. A workspace absent from the index (or an absent
 *  index) matches no rail → it is parked (not tiled), never a crash. */
export function epicForegroundTiles(
  workspaces: readonly Workspace[],
  railKeyById: Readonly<Record<string, string>> | undefined,
  selectedRailKey: string,
  order: readonly string[],
): Workspace[] {
  return foregroundTilesFor(workspaces, order).filter(
    (w) => railKeyById?.[w.id] === selectedRailKey,
  );
}

/** SW-EW-40: the mode-aware foreground membership — the ONE function the store
 *  resolver and the mosaic read-site share so they can't diverge. Flat mode → the
 *  full set. Grouped mode → the selected epic's slice, EXCEPT before the rail-key
 *  index has been built (`railKeyById === undefined`, i.e. a cold start restored
 *  into grouped mode before the pipeline tree loads): fall back to the full set so
 *  the restored terminals render immediately instead of flashing the empty-CLI
 *  state — the epic scope applies on the next render once `syncWorkspaceFrontmatter`
 *  lands the index. A BUILT-but-empty match (a genuinely empty selected epic) still
 *  yields `[]` — the fallback triggers only on the never-built `undefined`. */
export function scopedForegroundTiles(
  workspaces: readonly Workspace[],
  grouped: boolean,
  railKeyById: Readonly<Record<string, string>> | undefined,
  selectedRailKey: string,
  order: readonly string[],
): Workspace[] {
  if (!grouped || railKeyById === undefined) return foregroundTilesFor(workspaces, order);
  return epicForegroundTiles(workspaces, railKeyById, selectedRailKey, order);
}
