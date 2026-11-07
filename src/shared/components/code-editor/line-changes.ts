// SPDX-License-Identifier: MIT
/**
 * Line-level change markers for the editor's change gutter: which lines of the
 * current buffer were added or modified relative to a baseline (the file's
 * committed version), and where lines were deleted.
 *
 * Pure and dependency-free so it runs per debounced keystroke without loading
 * the merge bundle. Common prefix/suffix lines are trimmed first — a typical
 * edit touches a few lines, so the Myers pass usually runs over a handful — and
 * the edit-distance search is capped so a wholesale rewrite degrades to "the
 * changed middle is modified" instead of stalling the editor.
 */

export type LineChangeKind = "added" | "modified" | "deleted";

export interface LineChange {
  /** 1-based line in the CURRENT text. For `deleted`, the line the removed
   *  block sat just above (or the last line when it was at the very end). */
  line: number;
  kind: LineChangeKind;
}

export interface LineChangeSummary {
  added: number;
  modified: number;
  deleted: number;
}

/** Past this many edits the search stops and the trimmed middle is reported
 *  as one modified block — precise enough for a gutter, bounded in time. */
const MAX_EDIT_DISTANCE = 1000;

function splitLines(text: string): string[] {
  // Line endings are not content here: a CRLF file edited in an LF-joined
  // buffer must not mark every line modified.
  return text.split(/\r\n|\r|\n/);
}

type Op = "equal" | "insert" | "delete";

/** Myers' O((N+M)D) diff over line arrays; `null` when D exceeds `cap`. */
function myers(a: string[], b: string[], cap: number): Op[] | null {
  const n = a.length;
  const m = b.length;
  const max = Math.min(n + m, cap);
  const offset = max + 1;
  const v = new Int32Array(2 * max + 3);
  const trace: Int32Array[] = [];
  for (let d = 0; d <= max; d++) {
    // Round `d` only reads diagonals in [-d, d]: keep just that window, not the
    // whole frontier (a capped rewrite would otherwise snapshot ~8 MB).
    trace.push(v.slice(offset - d - 1, offset + d + 2));
    for (let k = -d; k <= d; k += 2) {
      let x: number;
      if (k === -d || (k !== d && v[offset + k - 1] < v[offset + k + 1])) {
        x = v[offset + k + 1];
      } else {
        x = v[offset + k - 1] + 1;
      }
      let y = x - k;
      while (x < n && y < m && a[x] === b[y]) {
        x++;
        y++;
      }
      v[offset + k] = x;
      if (x >= n && y >= m) {
        return backtrack(trace, n, m, d);
      }
    }
  }
  return null;
}

/** Extra depth searched past the cap when a search gives up, so the give-up
 *  stays provably valid while the buffer drifts by up to this many line edits. */
const GIVE_UP_MARGIN = 100;

/** The last search that gave up: its baseline, the buffer it ran on, and a
 *  lower bound on that buffer's edit distance. A wholesale rewrite keeps
 *  hitting the cap on every debounced keystroke, and the full capped search is
 *  the costliest thing here. */
let gaveUp: { base: string; lines: string[]; minEdits: number } | null = null;

/** Upper bound on the line edits turning `from` into `to`: everything between
 *  their common prefix and suffix. */
function driftBound(from: string[], to: string[]): number {
  let p = 0;
  while (p < from.length && p < to.length && from[p] === to[p]) p++;
  let ef = from.length;
  let et = to.length;
  while (ef > p && et > p && from[ef - 1] === to[et - 1]) {
    ef--;
    et--;
  }
  return ef - p + (et - p);
}

/** {@link myers} over the trimmed middles (`a`, `b`) of `base` and the current
 *  buffer `bAll`, capped at {@link MAX_EDIT_DISTANCE}, skipping a search known
 *  to exceed the cap. The size gap alone is a lower bound on the edits; and by
 *  the triangle inequality the edits from the baseline to this buffer are at
 *  least the last give-up's bound minus how far the buffer drifted since — so
 *  the skip only fires when that still clears the cap, never for a buffer that
 *  has become diffable. */
function cappedMyers(base: string, bAll: string[], a: string[], b: string[]): Op[] | null {
  if (Math.abs(a.length - b.length) > MAX_EDIT_DISTANCE) return null;
  if (
    gaveUp &&
    gaveUp.base === base &&
    gaveUp.minEdits - driftBound(gaveUp.lines, bAll) > MAX_EDIT_DISTANCE
  ) {
    return null;
  }
  const ops = myers(a, b, MAX_EDIT_DISTANCE + GIVE_UP_MARGIN);
  const edits = ops ? ops.reduce((n, op) => (op === "equal" ? n : n + 1), 0) : Infinity;
  if (edits <= MAX_EDIT_DISTANCE) {
    gaveUp = null;
    return ops;
  }
  gaveUp = {
    base,
    lines: bAll,
    minEdits: ops ? edits : MAX_EDIT_DISTANCE + GIVE_UP_MARGIN + 1,
  };
  return null;
}

/** Test seam: forget the remembered capped search. */
export function resetLineChangesCache(): void {
  gaveUp = null;
}

// `trace[d]` is the frontier as it stood before round `d`, so it names the
// diagonal each step came from.
function backtrack(trace: Int32Array[], n: number, m: number, depth: number): Op[] {
  const ops: Op[] = [];
  let x = n;
  let y = m;
  for (let d = depth; d > 0; d--) {
    // `trace[d]` holds diagonals [-d-1, d+1]; diagonal `k` sits at `k + d + 1`.
    const prev = trace[d];
    const at = (diag: number) => prev[diag + d + 1];
    const k = x - y;
    const down = k === -d || (k !== d && at(k - 1) < at(k + 1));
    const prevK = down ? k + 1 : k - 1;
    const prevX = at(prevK);
    const prevY = prevX - prevK;
    while (x > prevX && y > prevY) {
      ops.push("equal");
      x--;
      y--;
    }
    ops.push(down ? "insert" : "delete");
    if (down) y--;
    else x--;
  }
  while (x > 0 && y > 0) {
    ops.push("equal");
    x--;
    y--;
  }
  return ops.reverse();
}

/**
 * Compute gutter markers for `current` against `base`. Within one contiguous
 * change hunk, lines pair up as modified; extra current lines are added and
 * extra base lines collapse into one deletion marker.
 */
export function computeLineChanges(base: string, current: string): LineChange[] {
  if (base === current) return [];
  // An empty baseline has no lines (not one empty line to pair against): every
  // line of a brand-new file is added.
  if (base === "") {
    const lines = splitLines(current);
    // A final line break ends the last line; it does not open a phantom one.
    if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
    return lines.map((_, i) => ({ line: i + 1, kind: "added" }));
  }
  const a = splitLines(base);
  const b = splitLines(current);

  let start = 0;
  while (start < a.length && start < b.length && a[start] === b[start]) start++;
  let endA = a.length;
  let endB = b.length;
  while (endA > start && endB > start && a[endA - 1] === b[endB - 1]) {
    endA--;
    endB--;
  }
  const midA = a.slice(start, endA);
  const midB = b.slice(start, endB);
  const ops = cappedMyers(base, b, midA, midB) ?? [
    ...midA.map((): Op => "delete"),
    ...midB.map((): Op => "insert"),
  ];

  const changes: LineChange[] = [];
  const totalLines = b.length;
  let lineB = start; // 0-based index into b
  let pendingDeletes = 0;
  let pendingInserts: number[] = [];
  const flush = () => {
    const paired = Math.min(pendingDeletes, pendingInserts.length);
    pendingInserts.forEach((line, i) => {
      changes.push({ line: line + 1, kind: i < paired ? "modified" : "added" });
    });
    if (pendingDeletes > pendingInserts.length) {
      const anchor = pendingInserts.length
        ? pendingInserts[pendingInserts.length - 1] + 2
        : lineB + 1;
      changes.push({ line: Math.min(Math.max(anchor, 1), totalLines), kind: "deleted" });
    }
    pendingDeletes = 0;
    pendingInserts = [];
  };
  for (const op of ops) {
    if (op === "equal") {
      flush();
      lineB++;
    } else if (op === "insert") {
      pendingInserts.push(lineB);
      lineB++;
    } else {
      pendingDeletes++;
    }
  }
  flush();
  return changes;
}

/** Counts for a status readout ("+3 ~2 −1"). */
export function summarizeLineChanges(changes: readonly LineChange[]): LineChangeSummary {
  const summary: LineChangeSummary = { added: 0, modified: 0, deleted: 0 };
  for (const c of changes) summary[c.kind]++;
  return summary;
}
