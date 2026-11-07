// SPDX-License-Identifier: MIT
/**
 * SLW-WD-9 (RD-3/RD-10): the pure commit-graph lane layout.
 *
 * A small "active lanes" sweep over the loaded commits (newest → oldest, the
 * order WD-8 returns: `TOPOLOGICAL | TIME`). Each commit is assigned a lane
 * (column) and each of its parents is routed to a lane, so a per-row SVG can
 * draw the graph without ever materializing one giant SVG (RD-4).
 *
 * Two invariants make it cheap and jank-free:
 * - **Pure over a `{ sha, parents }[]` window** — no dependency on the full repo
 *   and no `features/*` import (it takes a minimal input, not WD-8's
 *   `SourceCommitGraphEntry`), so it runs off-thread in a worker and is trivially
 *   unit-testable (like `dagre-layout`).
 * - **Append-stable (RD-10)** — lane assignment for row `i` depends only on rows
 *   `0..i`, so `layoutCommitGraph(page1)` is a byte-identical prefix of
 *   `layoutCommitGraph(page1 ++ page2)`. Appending an older page only extends
 *   lanes downward; it never re-columns the rows already on screen (no jump on
 *   scroll-to-load).
 */

export interface CommitLaneInput {
  sha: string;
  /** All parent shas (a merge has ≥2; a root has none). */
  parents: string[];
}

export interface CommitGraphRow {
  sha: string;
  /** The column this commit's node sits in. */
  lane: number;
  /** The lane each parent routes into, index-aligned with the input `parents`. */
  parentLanes: number[];
  /**
   * Lane occupancy as this row is entered (the sha each lane is waiting for, or
   * `null` if free) — BEFORE the commit is placed. Any lane still holding this
   * commit's sha converges INTO `lane` (a merge of multiple children); the
   * renderer derives the incoming vertical + join geometry from this snapshot.
   */
  incoming: (string | null)[];
}

export interface CommitGraphLayout {
  rows: CommitGraphRow[];
  /** Max columns active across all rows (the rail width in lane units). */
  width: number;
}

/** Leftmost free lane, or `lanes.length` to open a new one on the right. */
function firstFreeLane(lanes: (string | null)[]): number {
  const i = lanes.indexOf(null);
  return i === -1 ? lanes.length : i;
}

export function layoutCommitGraph(commits: CommitLaneInput[]): CommitGraphLayout {
  // lanes[j] = the sha lane j is waiting for (a parent a placed child expects),
  // or null when the lane is free.
  const lanes: (string | null)[] = [];
  const rows: CommitGraphRow[] = [];
  let width = 0;

  for (const commit of commits) {
    // 1. This commit's lane: the leftmost lane already expecting it (reserved by
    //    a child above), else a fresh leftmost-free lane (a tip with no placed
    //    child yet).
    let lane = lanes.indexOf(commit.sha);
    if (lane === -1) {
      lane = firstFreeLane(lanes);
      if (lane === lanes.length) lanes.push(null);
    }

    // Snapshot occupancy as the row is entered, before reassigning for parents.
    const incoming = lanes.slice();

    // Collapse any OTHER lanes that were also expecting this commit (multiple
    // children converge here) — their edges route into `lane`; free them.
    for (let j = 0; j < lanes.length; j++) {
      if (j !== lane && lanes[j] === commit.sha) lanes[j] = null;
    }

    // 2. Route parents: the first parent continues in this lane (keeps the
    //    mainline straight); each extra parent (a merge) reuses a lane already
    //    expecting it or opens the leftmost-free lane. A root ends the lane.
    const parentLanes: number[] = [];
    if (commit.parents.length === 0) {
      lanes[lane] = null;
    } else {
      lanes[lane] = commit.parents[0];
      parentLanes.push(lane);
      for (let k = 1; k < commit.parents.length; k++) {
        const parent = commit.parents[k];
        let pl = lanes.indexOf(parent);
        if (pl === -1) {
          pl = firstFreeLane(lanes);
          if (pl === lanes.length) lanes.push(null);
          lanes[pl] = parent;
        }
        parentLanes.push(pl);
      }
    }

    rows.push({ sha: commit.sha, lane, parentLanes, incoming });
    width = Math.max(width, lanes.length, lane + 1);
  }

  return { rows, width };
}
