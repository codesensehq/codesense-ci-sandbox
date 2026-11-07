// SPDX-License-Identifier: MIT
import type { CommitGraphRow } from "./layout";

/**
 * SLW-WD-9 (RD-4): the per-row SVG draw instructions for one commit row —
 * derived PURELY from that row (no neighbour lookup), which is what lets the rail
 * draw lanes per visible row instead of one giant SVG. Coordinates are in
 * abstract units: `x` is a lane index, `y` is 0 (row top) · 0.5 (the node, mid) ·
 * 1 (row bottom). The renderer scales x by the lane width and y by the row height.
 */

export interface RowSegment {
  x1: number;
  y1: number;
  x2: number;
  y2: number;
}

export interface CommitRowDraw {
  /** The lane the commit's node dot sits in. */
  nodeLane: number;
  /** Line segments (lanes in, node→parent edges, pass-throughs). */
  segments: RowSegment[];
}

export function commitRowDraw(row: CommitGraphRow): CommitRowDraw {
  const { sha, lane, parentLanes, incoming } = row;
  const segments: RowSegment[] = [];

  // Top half (top → mid): every lane entering this row from above. A lane that
  // was expecting THIS commit converges into the node; any other continues
  // straight (a pass-through of an unrelated branch).
  incoming.forEach((expected, j) => {
    if (expected == null) return;
    if (expected === sha) segments.push({ x1: j, y1: 0, x2: lane, y2: 0.5 });
    else segments.push({ x1: j, y1: 0, x2: j, y2: 0.5 });
  });

  // Bottom half (mid → bottom): the node's parent edges fan out to their lanes,
  // and the pass-through lanes continue straight down.
  parentLanes.forEach((p) => segments.push({ x1: lane, y1: 0.5, x2: p, y2: 1 }));
  incoming.forEach((expected, j) => {
    if (expected == null || expected === sha) return;
    segments.push({ x1: j, y1: 0.5, x2: j, y2: 1 });
  });

  return { nodeLane: lane, segments };
}
