// SPDX-License-Identifier: MIT
import type { ContextRepoStatus, HistoryCommit, HistoryPage, LaneKind } from "@/tauri/commands";

/**
 * The History view's lane graph, laid out from a newest-first page of commits.
 *
 * An "active lanes" sweep: each commit takes the lane a child above reserved for it (or the
 * leftmost free one), its first parent continues in that lane and every other parent (a
 * merge) takes a lane of its own. Each lane remembers the kind it is drawn as (the main line,
 * a merged branch, an open PR, a local branch, a backup), so a row can draw itself from its
 * own entry alone. Append-stable: laying out more commits never re-columns the rows above.
 */

export interface LaneEdge {
  /** The lane the edge leaves (top half) or reaches (bottom half). */
  lane: number;
  kind: LaneKind;
}

export interface HistoryRow {
  commit: HistoryCommit;
  /** The lane this commit's mark sits in. */
  lane: number;
  /** Lanes entering the row from above, each with the sha it waits for and its kind. */
  before: (LaneSlot | null)[];
  /** Lanes leaving the row below. */
  after: (LaneSlot | null)[];
  /** Lanes above that end in this commit (its children). */
  into: number[];
  /** Edges from this commit down to its parents. */
  outs: LaneEdge[];
}

export interface LaneSlot {
  sha: string;
  kind: LaneKind;
}

function freeLane(lanes: (LaneSlot | null)[]): number {
  const i = lanes.indexOf(null);
  return i === -1 ? lanes.length : i;
}

export function layoutHistory(commits: readonly HistoryCommit[]): HistoryRow[] {
  const kindOf = new Map(commits.map((c) => [c.sha, c.lane]));
  let lanes: (LaneSlot | null)[] = [];
  const rows: HistoryRow[] = [];
  for (const c of commits) {
    let lane = lanes.findIndex((l) => l?.sha === c.sha);
    if (lane === -1) {
      lane = freeLane(lanes);
      if (lane === lanes.length) lanes.push(null);
      lanes[lane] = { sha: c.sha, kind: c.lane };
    }
    const before = lanes.slice();
    const into = before.flatMap((l, j) => (l?.sha === c.sha ? [j] : []));
    const after = before.slice();
    for (const j of into) after[j] = null;
    const outs: LaneEdge[] = [];
    if (c.parents[0]) {
      after[lane] = { sha: c.parents[0], kind: c.lane };
      outs.push({ lane, kind: c.lane });
    }
    for (const p of c.parents.slice(1)) {
      let k = after.findIndex((l) => l?.sha === p);
      const kind = kindOf.get(p) ?? "merged";
      if (k === -1) {
        k = freeLane(after);
        if (k === after.length) after.push(null);
        after[k] = { sha: p, kind };
      }
      outs.push({ lane: k, kind });
    }
    while (after.length && after[after.length - 1] === null) after.pop();
    rows.push({ commit: c, lane, before, after, into, outs });
    lanes = after;
  }
  return rows;
}

/** The widest a page's graph gets, in lanes. */
export function graphWidth(rows: readonly HistoryRow[]): number {
  let w = 1;
  for (const r of rows) w = Math.max(w, r.before.length, r.after.length, r.lane + 1);
  return w;
}

/** How each lane kind is stroked. Greys and ink only: colour on a row means an agent. */
export const LANE_STROKE: Record<LaneKind, { color: string; width: number; dash: string }> = {
  main: { color: "var(--cs-lane-main)", width: 2, dash: "" },
  merged: { color: "var(--cs-lane-1)", width: 1.5, dash: "" },
  open: { color: "var(--cs-lane-1)", width: 1.5, dash: "3 2.5" },
  local: { color: "var(--cs-lane-2)", width: 1.5, dash: "" },
  backup: { color: "var(--cs-lane-3)", width: 1.25, dash: "1 2.5" },
};

/** "4m", "3h", "Mon", "Sep 29": short enough for the meta line. */
export function commitWhen(committedAtSec: number, nowMs: number): string {
  const ms = committedAtSec * 1000;
  const s = Math.max(0, Math.round((nowMs - ms) / 1000));
  if (s < 60) return "now";
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.round(m / 60);
  if (h < 24) return `${h}h`;
  const d = new Date(ms);
  if (nowMs - ms < 6 * 24 * 3600 * 1000) return d.toLocaleDateString("en-US", { weekday: "short" });
  return d.toLocaleDateString("en-US", { month: "short", day: "numeric" });
}

/** A label beside a commit: the branch checked out (filled), GitHub's copy, an open PR,
 *  a branch, or how far this Mac and GitHub differ. */
export interface RefChip {
  text: string;
  look: "head" | "ahead" | "remote" | "pr" | "branch" | "behind";
}

/**
 * The labels a commit carries. The checked-out branch is filled and, when GitHub lacks
 * commits, says "not on GitHub"; `origin/<branch>` says how many new commits it holds when
 * this Mac is behind; an open PR's head reads "#24 open"; a local branch GitHub has never
 * seen is dashed.
 */
export function refChips(
  c: HistoryCommit,
  status: ContextRepoStatus | null,
  page: Pick<HistoryPage, "openPrs">,
): RefChip[] {
  const chips: RefChip[] = [];
  const isHead = c.refs.some((r) => r.kind === "head");
  const branch = status?.branch ?? null;
  const upstream = status?.upstream ?? (branch ? `origin/${branch}` : null);
  if (isHead) {
    chips.push({ text: branch ?? `detached ${c.shortSha}`, look: "head" });
    if (status && status.ahead > 0 && c.localOnly)
      chips.push({ text: "not on GitHub", look: "ahead" });
  }
  for (const pr of page.openPrs) {
    if (pr.sha === c.sha) chips.push({ text: `#${pr.number} open`, look: "pr" });
  }
  for (const r of c.refs) {
    if (r.kind === "head" || (isHead && r.kind === "local" && r.name === branch)) continue;
    if (r.kind === "remote") {
      if (r.name.endsWith("/HEAD")) continue;
      if (r.name === upstream) {
        chips.push({ text: r.name, look: "remote" });
        if (status && status.behind > 0)
          chips.push({ text: `${status.behind} new on GitHub`, look: "behind" });
        continue;
      }
      // A remote branch whose local copy is listed reads once, by its branch name.
      const short = r.name.replace(/^origin\//, "");
      if (c.refs.some((o) => o.kind === "local" && o.name === short)) continue;
      chips.push({ text: short, look: "branch" });
      continue;
    }
    if (r.kind === "local") {
      chips.push({ text: r.name, look: c.lane === "local" ? "ahead" : "branch" });
      continue;
    }
    chips.push({ text: r.name, look: "branch" });
  }
  if (c.forkPoint) chips.push({ text: "branched from main here", look: "remote" });
  return chips;
}

/** "main: 1 ahead, 2 behind" / "main: up to date": the checked-out branch against GitHub. */
export function aheadBehindLine(status: ContextRepoStatus | null): string {
  if (!status) return "";
  if (status.branch === null) return `detached at ${status.headSha}`;
  if (!status.hasUpstream) return `${status.branch}: not on GitHub`;
  const parts: string[] = [];
  if (status.ahead > 0) parts.push(`${status.ahead} ahead`);
  if (status.behind > 0) parts.push(`${status.behind} behind`);
  return `${status.branch}: ${parts.length ? parts.join(", ") : "up to date"}`;
}

/** Who made a commit, in words: "claude for you", "maya", "you". */
export function commitWho(c: HistoryCommit): { who: string; forWhom: string | null } {
  const person = c.byYou ? "you" : c.author;
  if (c.agent) return { who: c.agent.name, forWhom: /\[bot\]$/.test(c.author) ? null : person };
  return { who: person, forWhom: null };
}
