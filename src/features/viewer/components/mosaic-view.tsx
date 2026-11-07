// SPDX-License-Identifier: MIT
import type { ReactNode } from "react";

import type { Axis, MosaicNode } from "../lib/mosaic-tree";
import { TerminalTileGroup, type TileLike } from "./terminal-tile-group";

// ---------------------------------------------------------------------------
// MosaicView — renders a mosaic tree as nested resizable groups. The one
// renderer for every mosaic on screen: the workspace mosaic (a leaf is a
// workspace's tile) and a workspace's tab grid (a leaf is one of its terminals), so
// both have the same dividers, the same drag feel and the same double-click to
// even a pair.
// ---------------------------------------------------------------------------

const ROOT_PATH: number[] = [];

export interface MosaicViewProps {
  node: MosaicNode | null;
  /** What fills a leaf's region, by its id. */
  renderLeaf: (id: string) => ReactNode;
  /** A divider was released: the split at `path` ([] = root) now has `sizes`. */
  onSizesCommit: (path: number[], sizes: number[]) => void;
  /** The index trail to a zoomed leaf, which then fills the whole region. */
  zoomPath?: number[] | null;
  /** A pixel floor per split axis (a row's cells keep a width, a column's a
   *  height). Without it a tile keeps a percent floor. */
  minSizePx?: Record<Axis, number>;
  /** Cancels a resize gesture when a persistent mosaic changes owners. */
  resizeIdentity?: string;
}

interface MosaicNodeViewProps extends MosaicViewProps {
  path: number[];
  /** This node sits inside a collapsed sibling of the zoomed leaf (hidden), so
   *  its splits are out of play. */
  hidden?: boolean;
}

export function MosaicView(props: MosaicViewProps) {
  return <MosaicNodeView {...props} path={ROOT_PATH} />;
}

/** Stable identity for a mosaic node: a leaf is its id; a split is its index
 *  path (frame-layer only — split frames hold no terminal state, so a split
 *  remount on rearrange is harmless). */
function nodeKey(node: MosaicNode, path: number[]): string {
  return node.kind === "leaf" ? node.workspaceId : `split:${path.join("-")}`;
}

interface FrameTile extends TileLike {
  node: MosaicNode;
  path: number[];
}

/** One node of the tree. A real component (not a render function) so each split
 *  level has its own fiber identity. */
function MosaicNodeView({
  node,
  path,
  renderLeaf,
  onSizesCommit,
  zoomPath,
  minSizePx,
  hidden = false,
  resizeIdentity,
}: MosaicNodeViewProps) {
  if (!node) return null;
  if (node.kind === "leaf") return <>{renderLeaf(node.workspaceId)}</>;

  const tileItems: FrameTile[] = node.children.map((child, i) => ({
    key: nodeKey(child, [...path, i]),
    node: child,
    path: [...path, i],
  }));

  // Two-level zoom: at each split on the path to the zoomed leaf, zoom the
  // on-path child so the zoomed leaf fills the whole region up the tree.
  const zoomedChildKey =
    zoomPath && pathStartsWith(zoomPath, path) && zoomPath.length > path.length
      ? nodeKey(node.children[zoomPath[path.length]], zoomPath.slice(0, path.length + 1))
      : null;
  const zoomedHere =
    zoomedChildKey != null && tileItems.some((tile) => tile.key === zoomedChildKey);

  return (
    <TerminalTileGroup
      tiles={tileItems}
      resizeIdentity={resizeIdentity}
      direction={node.axis}
      sizes={node.sizes}
      onSizesCommit={(sizes) => onSizesCommit(path, sizes)}
      zoomedKey={zoomedChildKey}
      disabled={hidden}
      minSizePx={minSizePx?.[node.axis]}
      renderTile={(tile) => (
        <MosaicNodeView
          node={tile.node}
          path={tile.path}
          renderLeaf={renderLeaf}
          onSizesCommit={onSizesCommit}
          zoomPath={zoomPath}
          minSizePx={minSizePx}
          resizeIdentity={resizeIdentity}
          hidden={hidden || (zoomedHere && tile.key !== zoomedChildKey)}
        />
      )}
    />
  );
}

/** True when `path` is a prefix of `full`. */
function pathStartsWith(full: number[], path: number[]): boolean {
  return path.every((v, i) => full[i] === v);
}
