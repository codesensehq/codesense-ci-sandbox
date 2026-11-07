// SPDX-License-Identifier: MIT
import { findNavNodeByPath } from "@/shared/lib/pipeline-graph/find-nav-node-by-path";
import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";

/**
 * SLW-MD-1 (Task 5.3): is the open path a read-only source-repo doc — a file
 * from a source repo shown in the nav, not one of the team's own?
 *
 * The metadata band opens read-only (AC-13) when the open path's `DocRef`
 * carries `installedSourceRepo` (it came from a source repo overlaid onto the
 * team workspace) — that row is a browse-only source-repo layer file. A plain
 * team-workspace doc has no `installedSourceRepo`, so it is editable.
 *
 * This predicate walks the merged nav tree for the `DocRef` whose `filePath`
 * matches `activeTabPath` and returns true only for the installed source-repo
 * winner.
 *
 * Pure — the backend `apply_metadata_edit` still enforces (defense in depth);
 * this only drives the UI disable.
 */
export function isReadOnlyLayeredDoc(
  tree: DerivedPipelineTree | null,
  activeTabPath: string,
): boolean {
  const match = findNavNodeByPath(tree, activeTabPath);
  // Only a DocRef row can be a read-only source-repo layer; a story row (or no
  // match) is never a layered shadow. The winner is read-only iff it came from
  // a source-repo layer.
  return match?.doc?.installedSourceRepo != null;
}
