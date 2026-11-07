// SPDX-License-Identifier: MIT
import type { DerivedPipelineTree, StoryRef } from "@/shared/lib/pipeline-graph/types";
import type { StatusIndex } from "@/shared/stores/status-taxonomy-store";
import { countByStatus } from "@/shared/lib/pipeline-graph/count-by-status";

/** Numeric-aware alphabetical (F-2 before F-10). Used to order sub-epics,
 *  stories, and directory entries throughout the pipeline nav. */
// localeCompare with options constructs a Collator for every comparison.
// Reuse its bound comparator so refreshes do not repeatedly initialize ICU.
export const NAV_NAME_COLLATOR_OPTIONS: Intl.CollatorOptions = {
  numeric: true,
  sensitivity: "base",
};
const navCollator = new Intl.Collator(undefined, NAV_NAME_COLLATOR_OPTIONS);
export const compareNavNames = navCollator.compare;

/** Order sub-epics alphabetically by dirName. */
export function orderSubEpics(subEpics: DerivedPipelineTree[]): DerivedPipelineTree[] {
  return [...subEpics].sort((a, b) => compareNavNames(a.dirName, b.dirName));
}

/** A StoryRef is a pipeline work item (vs. a doc) when it has a status. */
export function isWorkItem(story: StoryRef): boolean {
  return story.status !== null;
}

export interface NodeCounts {
  total: number;
  done: number;
}

/**
 * Single bottom-up DFS that produces per-node {total, done} counts keyed by
 * `absoluteDirPath`. Replaces the per-folder `collectAllStories + countByStatus`
 * calls inside `flattenNavTree`, turning O(depth × N) work into O(N).
 *
 * Each node's count includes its own work items plus all descendants'.
 */
export function computeCountsByAbsPath(
  tree: DerivedPipelineTree,
  byId?: StatusIndex,
): Map<string, NodeCounts> {
  const map = new Map<string, NodeCounts>();
  function walk(node: DerivedPipelineTree): NodeCounts {
    const own = countByStatus(node.stories.filter(isWorkItem), byId);
    let total = own.total;
    let done = own.done;
    for (const sub of node.subEpics) {
      const subCounts = walk(sub);
      total += subCounts.total;
      done += subCounts.done;
    }
    const counts: NodeCounts = { total, done };
    map.set(node.absoluteDirPath, counts);
    return counts;
  }
  walk(tree);
  return map;
}
