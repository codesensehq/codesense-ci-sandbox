// SPDX-License-Identifier: MIT
/**
 * SLW-FX-21: build the candidate list for the chip's "Relate a file…" picker
 * (capability 1) by walking the in-memory pipeline tree. Pure — no I/O, no scan
 * on open — so the picker never blocks the UI. The caller memoizes and passes
 * the open doc + already-related paths in `excludePaths`.
 */

import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";
import { basename } from "@/shared/lib/basename";

export interface RelateCandidate {
  filePath: string;
  fileName: string;
}

/**
 * Collect every openable content file (stories, docs, epic main docs) under
 * `tree` as a relate candidate, skipping any path in `excludePaths` (the open
 * doc itself + docs already related) and de-duping. Sorted by file name for a
 * stable picker order.
 */
export function collectRelateCandidates(
  tree: DerivedPipelineTree | null | undefined,
  excludePaths: Set<string>,
): RelateCandidate[] {
  if (!tree) return [];
  const out: RelateCandidate[] = [];
  const seen = new Set<string>();
  const push = (filePath: string | undefined | null) => {
    if (!filePath || excludePaths.has(filePath) || seen.has(filePath)) return;
    seen.add(filePath);
    out.push({ filePath, fileName: basename(filePath) });
  };
  const walk = (node: DerivedPipelineTree) => {
    for (const s of node.stories) push(s.filePath);
    for (const d of node.docs ?? []) push(d.filePath);
    if (node.mainDoc) push(node.mainDoc.filePath);
    for (const sub of node.subEpics) walk(sub);
  };
  walk(tree);
  out.sort((a, b) => a.fileName.localeCompare(b.fileName));
  return out;
}
