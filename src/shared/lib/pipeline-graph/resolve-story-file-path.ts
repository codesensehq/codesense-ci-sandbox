// SPDX-License-Identifier: MIT
import { buildGroupIndex, buildGroupStoryIdIndex } from "./build-group-index";
import type { DerivedPipelineTree } from "./types";

/**
 * SW-AM-7: resolve a bare `story_id` (e.g. "SW-AM-7") to its absolute story file
 * path from a scanned pipeline tree — the DURABLE join that survives a workspace
 * tile being fully closed (a disk scan owns the tree, not any open-workspace
 * record). Reuses the ONE shared `buildGroupStoryIdIndex` resolver, so it can
 * never disagree with pipeline-edge / readiness resolution about which file an id
 * maps to; an ambiguous (duplicate) id is poisoned to `null` there and resolves
 * to `null` here too. Pure — the caller supplies the tree (from `scanPipelineTree`
 * or a fixture), so this is unit-tested without a scan.
 */
export function storyFilePathFromTree(tree: DerivedPipelineTree, storyId: string): string | null {
  const groupIndex = buildGroupIndex(tree);
  const loc = buildGroupStoryIdIndex(groupIndex).get(storyId);
  if (!loc) return null;
  return groupIndex.get(loc.scopeRel)?.files.get(loc.fileName)?.filePath ?? null;
}
