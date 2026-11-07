// SPDX-License-Identifier: MIT
// ---------------------------------------------------------------------------
// SW-EW-17 — mosaic-tree: the pure, unit-tested layout model for 2D workspace
// tiling. A `MosaicNode` is the UNIFIED representation of the foreground
// arrangement: a single leaf is 1-up, a depth-1 split is SW-EW-3/SW-EW-4's flat
// columns/rows (rendered through the unchanged flat TerminalTileGroup), and a
// nested split is the 2D mosaic this story adds.
//
// Membership (which workspaces are foreground) stays DERIVED from
// `foregroundTilesFor`; the tree owns ARRANGEMENT only and is reconciled against
// the live set on read (`reconcileMosaic`) — the tree analog of EW-3's
// `reconcileSizes`. Every function here is pure (no store, no React) so the
// split/move/collapse logic is exhaustively testable in isolation.
// ---------------------------------------------------------------------------

import type { LayoutPreset } from "@/shared/stores/viewer-store";

export type Axis = "row" | "column";
export type SplitSide = "before" | "after";

/** A leaf names what fills its region: a workspace in the workspace mosaic, a
 *  terminal tab in a workspace's tab grid. */
export type MosaicNode =
  | { kind: "leaf"; workspaceId: string }
  | { kind: "split"; axis: Axis; children: MosaicNode[]; sizes: number[] };

/** A directional placement target: split `targetWorkspaceId`'s region along
 *  `axis`, putting the new/moved tile `before` or `after` it. */
export interface TilePlacementTarget {
  targetWorkspaceId: string;
  axis: Axis;
  side: SplitSide;
}

const leaf = (workspaceId: string): MosaicNode => ({ kind: "leaf", workspaceId });

const evenSizes = (n: number): number[] => Array.from({ length: n }, () => 100 / n);

/** Normalize a sizes array to sum 100 (defends against rounding/removal drift).
 *  An empty or all-zero array falls back to an even split. */
function normalizeSizes(sizes: number[]): number[] {
  const sum = sizes.reduce((a, b) => a + b, 0);
  if (sizes.length === 0) return [];
  if (sum <= 0) return evenSizes(sizes.length);
  return sizes.map((s) => (s / sum) * 100);
}

/** Migration seed + columns/rows preset generator: a flat list of workspaces
 *  arranged on a single axis. 0 → null, 1 → leaf, ≥2 → one depth-1 split. This
 *  is what makes columns/rows render identically to the pre-EW-17 flat group. */
export function seedTreeFromFlat(
  workspaceIds: string[],
  direction: Axis,
  sizes?: number[],
): MosaicNode | null {
  if (workspaceIds.length === 0) return null;
  if (workspaceIds.length === 1) return leaf(workspaceIds[0]);
  // Honor persisted flat sizes when they still match the live count (so a
  // columns/rows width survives reload); otherwise fall back to an even split.
  const seed =
    sizes && sizes.length === workspaceIds.length
      ? normalizeSizes(sizes)
      : evenSizes(workspaceIds.length);
  return { kind: "split", axis: direction, children: workspaceIds.map(leaf), sizes: seed };
}

/** In-order leaf workspace ids (left→right / top→bottom render order). */
export function leafWorkspaceIds(root: MosaicNode | null): string[] {
  if (!root) return [];
  if (root.kind === "leaf") return [root.workspaceId];
  return root.children.flatMap(leafWorkspaceIds);
}

/** The index trail from `root` to the leaf, or null if absent. A single-leaf
 *  root that IS the target has the empty path. */
export function findLeafPath(root: MosaicNode | null, workspaceId: string): number[] | null {
  if (!root) return null;
  if (root.kind === "leaf") return root.workspaceId === workspaceId ? [] : null;
  for (let i = 0; i < root.children.length; i++) {
    const sub = findLeafPath(root.children[i], workspaceId);
    if (sub) return [i, ...sub];
  }
  return null;
}

/** Replace the node at `path` with `next` (or drop it when `next` is null),
 *  collapsing any split left with a single child. Returns the rebuilt tree. */
function replaceAt(node: MosaicNode, path: number[], next: MosaicNode | null): MosaicNode | null {
  if (path.length === 0) return next;
  if (node.kind !== "split") return node;
  const [head, ...rest] = path;
  const child = node.children[head];
  const replaced = replaceAt(child, rest, next);

  const children: MosaicNode[] = [];
  const sizes: number[] = [];
  node.children.forEach((c, i) => {
    if (i === head) {
      if (replaced) {
        children.push(replaced);
        sizes.push(node.sizes[i] ?? 100 / node.children.length);
      }
      // replaced === null → drop this slot entirely
    } else {
      children.push(c);
      sizes.push(node.sizes[i] ?? 100 / node.children.length);
    }
  });

  if (children.length === 0) return null;
  if (children.length === 1) return children[0]; // collapse single-child split
  return { kind: "split", axis: node.axis, children, sizes: normalizeSizes(sizes) };
}

/** Split the region occupied by `targetWorkspaceId` along `axis`, inserting a
 *  new leaf `newWorkspaceId` on `side`. Same-axis as the target's parent →
 *  flatten (insert a sibling, halving the target's slot); otherwise → wrap the
 *  target in a nested split. */
export function splitAt(
  root: MosaicNode,
  targetWorkspaceId: string,
  axis: Axis,
  side: SplitSide,
  newWorkspaceId: string,
): MosaicNode {
  const path = findLeafPath(root, targetWorkspaceId);
  if (!path) return root;

  // Same-axis flatten: the target's parent split already runs on `axis` → insert
  // the new leaf as a sibling and halve the target's slot (siblings untouched).
  if (path.length >= 1) {
    const parentPath = path.slice(0, -1);
    const idx = path[path.length - 1];
    const parent = nodeAt(root, parentPath);
    if (parent && parent.kind === "split" && parent.axis === axis) {
      const children = [...parent.children];
      const sizes = [...parent.sizes];
      const slot = sizes[idx] ?? 100 / children.length;
      const insertIdx = side === "before" ? idx : idx + 1;
      children.splice(insertIdx, 0, leaf(newWorkspaceId));
      // The new tile takes half of the target's slot; the target keeps the rest.
      sizes[idx] = slot / 2;
      sizes.splice(insertIdx, 0, slot / 2);
      const nextParent: MosaicNode = {
        kind: "split",
        axis,
        children,
        sizes: normalizeSizes(sizes),
      };
      return replaceAt(root, parentPath, nextParent) ?? nextParent;
    }
  }

  // Cross-axis (or splitting a single-leaf root): wrap the target leaf in a new
  // 2-child split on `axis`.
  const target = nodeAt(root, path)!;
  const wrapped: MosaicNode = {
    kind: "split",
    axis,
    children: side === "before" ? [leaf(newWorkspaceId), target] : [target, leaf(newWorkspaceId)],
    sizes: [50, 50],
  };
  return replaceAt(root, path, wrapped) ?? wrapped;
}

/** The node at an index path (inverse of findLeafPath). */
function nodeAt(node: MosaicNode, path: number[]): MosaicNode | null {
  let cur: MosaicNode = node;
  for (const i of path) {
    if (cur.kind !== "split") return null;
    cur = cur.children[i];
    if (!cur) return null;
  }
  return cur;
}

/** Remove a leaf, collapsing single-child splits and renormalizing sibling
 *  sizes. Returns null when the last leaf is removed; the original tree when
 *  the id is absent. */
export function pruneLeaf(root: MosaicNode | null, workspaceId: string): MosaicNode | null {
  if (!root) return null;
  const path = findLeafPath(root, workspaceId);
  if (!path) return root;
  if (path.length === 0) return null; // root leaf itself
  return replaceAt(root, path, null);
}

/** SW-EW-33 — structural equality of two arrangements, IGNORING split sizes.
 *  Two trees are "the same" when they have the same nesting, axes, and leaf order
 *  — a pure size difference (e.g. a 30/70 split reset to 50/50) does NOT count as
 *  a different arrangement. Used to decide whether a drag-placement would
 *  actually move anything (so a no-op drop zone isn't highlighted). */
export function sameMosaicStructure(a: MosaicNode | null, b: MosaicNode | null): boolean {
  if (a === b) return true;
  if (a === null || b === null) return false;
  if (a.kind !== b.kind) return false;
  if (a.kind === "leaf")
    return a.workspaceId === (b as Extract<MosaicNode, { kind: "leaf" }>).workspaceId;
  const bs = b as Extract<MosaicNode, { kind: "split" }>;
  if (a.axis !== bs.axis || a.children.length !== bs.children.length) return false;
  return a.children.every((c, i) => sameMosaicStructure(c, bs.children[i]));
}

/** SW-EW-33 — replace a leaf's occupant IN PLACE: swap the workspace at the
 *  `oldWorkspaceId` leaf for `newWorkspaceId`, keeping the leaf's position/slot
 *  intact (pane count + arrangement unchanged). Absent `oldWorkspaceId` → the
 *  tree is returned unchanged (matches `pruneLeaf`/`splitAt`). This is the one
 *  arrangement primitive click-to-replace needs that no existing helper provides
 *  (split/move/prune/append all CHANGE structure). */
export function replaceLeafWorkspaceId(
  root: MosaicNode | null,
  oldWorkspaceId: string,
  newWorkspaceId: string,
): MosaicNode | null {
  if (!root) return root;
  const path = findLeafPath(root, oldWorkspaceId);
  if (!path) return root; // absent → unchanged (defensive; F is normally the focused leaf)
  return replaceAt(root, path, leaf(newWorkspaceId));
}

/** SW-EW-33 — would dropping `draggedId` at `target` actually change the
 *  arrangement? Simulates the placement (`moveLeaf` for an on-screen workspace,
 *  `splitAt` for one not yet in the tree) and compares structure (ignoring
 *  sizes). Used to suppress drop zones that would be no-ops — e.g. dropping a
 *  workspace onto the edge it already occupies relative to its neighbor — so a
 *  highlighted zone is always a real move. A not-in-tree (minimized) workspace
 *  always changes the layout (it gets added). Pure. */
export function placementChangesLayout(
  root: MosaicNode | null,
  draggedId: string,
  target: TilePlacementTarget,
): boolean {
  if (!root) return true;
  const onScreen = leafWorkspaceIds(root).includes(draggedId);
  const next = onScreen
    ? moveLeaf(root, draggedId, target)
    : splitAt(root, target.targetWorkspaceId, target.axis, target.side, draggedId);
  return !sameMosaicStructure(root, next);
}

/** On-screen rearrange: prune the leaf, then re-split at the target. A move onto
 *  itself (or onto a now-absent target) is a no-op. */
export function moveLeaf(
  root: MosaicNode,
  workspaceId: string,
  target: TilePlacementTarget,
): MosaicNode {
  if (workspaceId === target.targetWorkspaceId) return root;
  const pruned = pruneLeaf(root, workspaceId);
  if (!pruned) return root;
  if (!findLeafPath(pruned, target.targetWorkspaceId)) return root;
  return splitAt(pruned, target.targetWorkspaceId, target.axis, target.side, workspaceId);
}

/** Append a workspace opened NOT via drop (e.g. from the nav): root null → leaf;
 *  root a leaf → a row split of [existing, new]; root a split → push a child and
 *  renormalize evenly. Matches the pre-EW-17 "append a tile" behavior. */
export function appendLeaf(root: MosaicNode | null, workspaceId: string): MosaicNode {
  if (!root) return leaf(workspaceId);
  if (root.kind === "leaf") {
    return { kind: "split", axis: "row", children: [root, leaf(workspaceId)], sizes: [50, 50] };
  }
  const children = [...root.children, leaf(workspaceId)];
  return { kind: "split", axis: root.axis, children, sizes: evenSizes(children.length) };
}

/** Reconcile arrangement against the live foreground set: prune leaves whose
 *  workspace is gone (+ collapse), then append live workspaces missing from the
 *  tree in `liveWorkspaceIds` order. Runs on read. */
export function reconcileMosaic(
  root: MosaicNode | null,
  liveWorkspaceIds: string[],
): MosaicNode | null {
  const live = new Set(liveWorkspaceIds);
  let next = dedupeLeaves(root);
  // Prune anything no longer live.
  for (const id of leafWorkspaceIds(next)) {
    if (!live.has(id)) next = pruneLeaf(next, id);
  }
  // Append anything live but not yet placed, in the given (id-sorted) order.
  const present = new Set(leafWorkspaceIds(next));
  for (const id of liveWorkspaceIds) {
    if (!present.has(id)) {
      next = appendLeaf(next, id);
      present.add(id);
    }
  }
  return next;
}

/** Remove duplicate-id leaves (a corrupt/hand-edited persisted root could repeat
 *  a workspace) keeping one occurrence — two leaves for the same workspace would
 *  fight over the single terminal-frame registration and strand one empty. */
export function dedupeLeaves(root: MosaicNode | null): MosaicNode | null {
  let next = root;
  const seen = new Set<string>();
  const extras: string[] = [];
  for (const id of leafWorkspaceIds(next)) {
    if (seen.has(id)) extras.push(id);
    else seen.add(id);
  }
  for (const id of extras) next = pruneLeaf(next, id); // drops the first occurrence, leaves one
  return next;
}

/** Commit new sizes for the split node at `path` ([] = root). No-ops on a leaf,
 *  a bad path, or a length mismatch (defends against a stale commit). */
export function setNodeSizes(root: MosaicNode, path: number[], sizes: number[]): MosaicNode {
  if (path.length === 0) {
    if (root.kind !== "split" || sizes.length !== root.children.length) return root;
    return { ...root, sizes: normalizeSizes(sizes) };
  }
  if (root.kind !== "split") return root;
  const [head, ...rest] = path;
  const child = root.children[head];
  if (!child) return root;
  const children = [...root.children];
  children[head] = setNodeSizes(child, rest, sizes);
  return { ...root, children };
}

/** The balanced ⌈√N⌉ column/row split for `n` tiles — the ONE definition of "how
 *  N tiles balance into a grid", shared by the workspace mosaic and a workspace's
 *  grid cells. `n` is clamped to ≥1 so the result is always renderable. */
export function balancedColsRows(n: number): { cols: number; rows: number } {
  const count = Math.max(1, n);
  const cols = Math.ceil(Math.sqrt(count));
  return { cols, rows: Math.ceil(count / cols) };
}

/**
 * The effective `{cols, rows}` for `count` cells under `preset`, capped so no cell
 * falls below the readable floor. `maxCols`/`maxRows` are the size-derived caps
 * (pane size over the minimum cell size); pass `Infinity` for no cap (the raw
 * preset shape). The single-axis presets WRAP when capped: `columns` with more
 * cells than fit spills into extra rows rather than shrinking every cell to a sliver.
 */
export function clampedGridShape(
  preset: LayoutPreset,
  count: number,
  maxCols: number,
  maxRows: number,
): { cols: number; rows: number } {
  const n = Math.max(1, count);
  const capCols = Math.max(1, Math.floor(maxCols));
  const capRows = Math.max(1, Math.floor(maxRows));
  if (preset === "columns") {
    // Single row when the cells fit (uncapped → capCols=Infinity → 1 row); when the
    // floor forces a wrap, BALANCE the rows so it's near-square (5 cells capped to 4
    // cols → 3×2, not a lonely 4 + full-width-1). `max(1, …)` guards the Infinity/0 edge.
    const rows = Math.max(1, Math.ceil(n / capCols));
    return { cols: Math.max(1, Math.ceil(n / rows)), rows };
  }
  if (preset === "rows") {
    const cols = Math.max(1, Math.ceil(n / capRows));
    return { cols, rows: Math.max(1, Math.ceil(n / cols)) };
  }
  // `grid` preset: the balanced ⌈√n⌉ split, capped to width.
  const cols = Math.min(balancedColsRows(n).cols, capCols);
  return { cols, rows: Math.ceil(n / cols) };
}

/**
 * The even arrangement `preset` gives `ids`: the floor-clamped shape (see
 * {@link clampedGridShape}) filled row by row, in order. A single row is a row
 * split, a single column a column split, anything else a column of rows — so a
 * short last row simply has fewer children and spans the full width. 0 ids → null,
 * 1 → a leaf.
 */
export function buildPresetTree(
  preset: LayoutPreset,
  ids: string[],
  maxCols = Infinity,
  maxRows = Infinity,
): MosaicNode | null {
  const n = ids.length;
  if (n === 0) return null;
  if (n === 1) return leaf(ids[0]);
  const { cols } = clampedGridShape(preset, n, maxCols, maxRows);
  if (cols >= n) return seedTreeFromFlat(ids, "row");
  if (cols === 1) return seedTreeFromFlat(ids, "column");
  const rows: MosaicNode[] = [];
  for (let i = 0; i < n; i += cols) rows.push(seedTreeFromFlat(ids.slice(i, i + cols), "row")!);
  return { kind: "split", axis: "column", children: rows, sizes: evenSizes(rows.length) };
}

/** The `grid` preset: a balanced ⌈√N⌉ arrangement — a column of rows. Degenerate
 *  single-child levels collapse (N≤cols → one row; N=1 → a leaf). */
export function buildBalancedGrid(workspaceIds: string[]): MosaicNode | null {
  return buildPresetTree("grid", workspaceIds);
}

/** A saved arrangement and the preset it was resized under. */
export interface SavedPresetTree {
  preset: LayoutPreset;
  root: MosaicNode;
}

/**
 * The arrangement to show for `ids` under `preset`: the even preset tree, unless
 * `saved` was resized under the same preset and — once leaves no longer in `ids`
 * are dropped — still has exactly that tree's shape, in which case its sizes are
 * kept. So closing a cell keeps the others' proportions, while a new cell, a
 * reorder, a preset change or a floor-forced reshape lays out evenly.
 */
export function reconcilePresetTree(
  saved: SavedPresetTree | undefined,
  preset: LayoutPreset,
  ids: string[],
  maxCols = Infinity,
  maxRows = Infinity,
): MosaicNode | null {
  const even = buildPresetTree(preset, ids, maxCols, maxRows);
  if (!saved || saved.preset !== preset || !even) return even;
  const live = new Set(ids);
  let kept = dedupeLeaves(saved.root);
  for (const id of leafWorkspaceIds(kept)) if (!live.has(id)) kept = pruneLeaf(kept, id);
  return sameMosaicStructure(kept, even) ? kept : even;
}

/** A mosaic node read back from saved state, or null when it isn't one (a split
 *  needs ≥2 children, one finite size per child). */
export function parseMosaicNode(value: unknown): MosaicNode | null {
  if (!value || typeof value !== "object") return null;
  const v = value as Record<string, unknown>;
  if (v.kind === "leaf") return typeof v.workspaceId === "string" ? leaf(v.workspaceId) : null;
  if (v.kind !== "split" || (v.axis !== "row" && v.axis !== "column")) return null;
  if (!Array.isArray(v.children) || !Array.isArray(v.sizes)) return null;
  if (v.children.length < 2 || v.sizes.length !== v.children.length) return null;
  if (!v.sizes.every((n) => typeof n === "number" && Number.isFinite(n) && n >= 0)) return null;
  const children: MosaicNode[] = [];
  for (const c of v.children) {
    const child = parseMosaicNode(c);
    if (!child) return null;
    children.push(child);
  }
  return { kind: "split", axis: v.axis, children, sizes: normalizeSizes(v.sizes as number[]) };
}
