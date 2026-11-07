// SPDX-License-Identifier: MIT
import type { DerivedPipelineTree, StoryRef } from "./types";
import { isActiveStatus, isBlockedStatus, isDoneStatus, normalizeStatus } from "./pipeline-utils";
import type { StatusIndex } from "@/shared/stores/status-taxonomy-store";
import { firstStatusId, firstStatusIdOfRole } from "@/shared/stores/status-taxonomy-store";
import { collectAllStories } from "./collect-all-stories";
import { findEnclosingEpic } from "./find-enclosing-epic";

/**
 * Count the total number of stories and how many are "done".
 * Used by both the pipeline story feed and the pipeline status nav
 * for progress calculations.
 */
export function countByStatus(
  stories: StoryRef[],
  byId?: StatusIndex,
): { total: number; done: number } {
  let done = 0;
  for (const s of stories) {
    if (isDoneStatus(normalizeStatus(s.status, byId), byId)) done++;
  }
  return { total: stories.length, done };
}

/**
 * SW-EW-25: roll a set of child stories up to a single epic-level status for
 * the epic-workspace chrome chip. Each child is normalized first
 * (`normalizeStatus` — off-enum frontmatter like `ready-for-dev` collapses to
 * `todo`), then the first matching rule wins:
 *
 *   1. `blocked`     — ANY child is blocked (highest signal; beats `done`).
 *   2. `done`        — ALL children are done.
 *   3. `in-progress` — ANY child is started (in-progress/review/qa), OR some
 *                      (but not all) children are done.
 *   4. `todo`        — otherwise.
 *
 * Empty epic (no child stories) → `planning`: an epic with nothing in it is
 * being planned, not vacuously `done`/`todo`.
 *
 * Status-only — NOT dependency-aware (a dep-blocked `todo` child reads `todo`,
 * not `blocked`; readiness is a separate layer, `resolve-nav-readiness.ts`).
 * Pure and side-effect-free.
 */
export function rollupEpicStatus(stories: StoryRef[]): string {
  // The default (first-by-order) state represents both an empty epic and the
  // all-not-started case — for the default taxonomy that is `planning`, matching
  // the prior behavior; for a custom set it is whatever the team ordered first.
  const fallback = firstStatusId() ?? "";
  if (stories.length === 0) return fallback;
  const statuses = stories.map((s) => normalizeStatus(s.status));
  // Role-keyed rollup — the representative state id for each role outcome, so the
  // epic chip reads correctly under an arbitrary state set (no literal ids).
  if (statuses.some((s) => isBlockedStatus(s))) return firstStatusIdOfRole("blocked") ?? fallback;
  if (statuses.every((s) => isDoneStatus(s))) return firstStatusIdOfRole("done") ?? fallback;
  if (statuses.some((s) => isActiveStatus(s) || isDoneStatus(s))) {
    return firstStatusIdOfRole("active") ?? firstStatusIdOfRole("review") ?? fallback;
  }
  return fallback;
}

/**
 * SW-EW-25: resolve the epic enclosing `epicDocPath` within `tree` and roll its
 * child stories (incl. nested sub-epics) up to a single status — the one-call
 * seam the epic-workspace chrome uses.
 *
 * Returns `null` when the epic can't be located: `findEnclosingEpic` falls back
 * to the deepest enclosing epic (or the root) when the exact doc path isn't
 * found (epic renamed/deleted, or outside the scanned scope) — and that fallback
 * can itself carry a `mainDoc` (a root README, or an ancestor epic in a nested
 * layout), so a mere `mainDoc !== null` check isn't enough. An epic-workspace's
 * `epicDocPath` is precisely the epic node's OWN `mainDoc.filePath` (the nav
 * opens epic terminals with `node.mainDoc.filePath`), so we require an EXACT
 * match: a fallback onto an ancestor/root whose `mainDoc` differs is rejected →
 * the caller OMITS the chip rather than rolling up the wrong scope into a
 * plausible-but-wrong status. Pure and side-effect-free.
 */
export function rollupEpicStatusForDoc(
  tree: DerivedPipelineTree,
  epicDocPath: string,
): string | null {
  const epic = findEnclosingEpic(tree, epicDocPath);
  if (epic.mainDoc?.filePath !== epicDocPath) return null;
  return rollupEpicStatus(collectAllStories(epic));
}
