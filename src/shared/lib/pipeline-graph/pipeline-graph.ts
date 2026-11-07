// SPDX-License-Identifier: MIT
import type { DerivedPipelineTree, StoryRef } from "./types";
import { buildFullGroupIndex, buildGroupStoryIdIndex } from "./build-group-index";
import type { StatusIndex } from "@/shared/stores/status-taxonomy-store";
import { scopeRelative } from "./pipeline-utils";
import { resolveReadiness, type ReadinessResult } from "./resolve-nav-readiness";

export { buildFullGroupIndex, type FullGroupIndex } from "./build-group-index";

/**
 * The one source of cross-epic truth for a team-content tree.
 *
 * Built once from the full tree and shared by every view (nav, board, future
 * connected-state features). Display scope and graph scope are ORTHOGONAL: a
 * drilled view renders from its scoped tree but queries the graph — which is
 * always full — for any cross-epic fact. The full index is captured in the
 * closure and never exposed, so no caller can resolve readiness against a
 * scoped subset (SW-FX-1).
 *
 * Extension point: future global-state queries (`dependentsOf`,
 * `crossScopeRefsFrom`, …) are added here as methods over the same full index.
 */
export interface PipelineGraph {
  /** Scope root the index is keyed against (the full tree's `dirPath`). */
  readonly rootScopeDirPath: string;
  /** Readiness + blocking deps for `story`, whose owner group's directory path
   *  (scanner-relative, e.g. `node.dirPath`) is `ownerDirPath`. The graph maps
   *  it into its own key space, so callers never compute the scope-relative key
   *  themselves — the one place a wrong scope key could reintroduce the
   *  drill-dependence bug. Resolves against the full graph regardless of how far
   *  the calling view has drilled in. */
  readiness(story: StoryRef, ownerDirPath: string): ReadinessResult;
}

/** `byId` resolves statuses against a retained team's own taxonomy instead of the active one. */
export function buildPipelineGraph(
  fullTree: DerivedPipelineTree,
  byId?: StatusIndex,
): PipelineGraph {
  const index = buildFullGroupIndex(fullTree);
  // SW-FX-70: build the story_id index ONCE beside the full index and close over
  // it, so per-story `readiness` calls do an O(1) lookup instead of rebuilding it
  // (no O(n²) over the tree). Same full index → keys align with `resolveReadiness`.
  const storyIdIndex = buildGroupStoryIdIndex(index);
  const rootScopeDirPath = fullTree.dirPath;
  return {
    rootScopeDirPath,
    readiness: (story, ownerDirPath) =>
      resolveReadiness(
        story,
        scopeRelative(ownerDirPath, rootScopeDirPath),
        index,
        storyIdIndex,
        byId,
      ),
  };
}
