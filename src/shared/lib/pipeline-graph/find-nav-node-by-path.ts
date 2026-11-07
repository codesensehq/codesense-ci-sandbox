// SPDX-License-Identifier: MIT
import type { DerivedPipelineTree, DocRef, StoryRef } from "./types";

/**
 * The nav node found for a given `filePath`: the matching `StoryRef` (a story
 * row) OR `DocRef` (a plain-doc row). Exactly one is set on a hit; `null` when
 * no row in the tree owns the path.
 */
export interface NavNodeMatch {
  story?: StoryRef;
  doc?: DocRef;
}

/**
 * Single shared nav-tree recursion (SLW-MD-1 DRY). Depth-first walk of the
 * merged `DerivedPipelineTree` — stories first, then docs, then `subEpics` —
 * returning the first non-nullish result from `pick`. This is the ONE walker the
 * per-path lookups (`findNavNodeByPath`,
 * `findMetadataValidity`, `isReadOnlyLayeredDoc`) share, so the traversal lives
 * in exactly one place; callers supply only the match/derive step via `pick`.
 *
 * CONTRACT: `pick` MUST return `null`/`undefined` to signal "no match, keep
 * walking". The walk stops at (and returns) the first result that is not
 * nullish — the `found != null` check treats `null` and `undefined` alike as
 * "continue". Any other value, INCLUDING a falsy-but-valid one (`0`, `""`,
 * `false`), is a MATCH and terminates the walk. If a `pick` could legitimately
 * produce such a falsy value as a real result, it must wrap it (e.g. in an
 * object) so the sentinel stays unambiguous.
 *
 * Pure — safe to call inside a `useMemo`.
 */
export function visitNavTree<T>(
  tree: DerivedPipelineTree | null,
  pick: (node: DerivedPipelineTree) => T | null,
): T | null {
  if (!tree) return null;
  const found = pick(tree);
  if (found != null) return found;
  for (const sub of tree.subEpics) {
    const inSub = visitNavTree(sub, pick);
    if (inSub != null) return inSub;
  }
  return null;
}

/**
 * Find the first `StoryRef`/`DocRef` whose `filePath` equals `filePath`.
 * Stories are matched before docs within a node. Returns `null` when the path
 * is not a known story/doc row (e.g. a generic file). Callers derive their own
 * field from the found node (`metadataValid`, `installedSourceRepo`, …).
 */
export function findNavNodeByPath(
  tree: DerivedPipelineTree | null,
  filePath: string,
): NavNodeMatch | null {
  if (!filePath) return null;
  return visitNavTree(tree, (node) => {
    for (const s of node.stories ?? []) {
      if (s.filePath === filePath) return { story: s };
    }
    for (const d of node.docs ?? []) {
      if (d.filePath === filePath) return { doc: d };
    }
    return null;
  });
}
