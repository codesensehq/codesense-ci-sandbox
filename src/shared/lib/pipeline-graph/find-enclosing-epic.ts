// SPDX-License-Identifier: MIT
import type { DerivedPipelineTree } from "./types";

/**
 * SW-SC-12: resolve the enclosing *epic* subtree for the file at `targetPath`
 * within `tree`. An epic is a node carrying a main doc (`mainDoc !== null`); a
 * plain directory has `mainDoc === null`.
 *
 * Walks down `subEpics`, following at each level the child whose
 * `absoluteDirPath` contains `targetPath`, and returns the DEEPEST node on
 * that path that is an epic — so a canvas nested inside a sub-epic resolves to
 * that sub-epic, not the outer one.
 *
 * Fallback: when no enclosing epic is found within `tree` — e.g. the nav is
 * drilled below the epic boundary, or the file sits outside any epic — returns
 * `tree` itself, so `collectAllStories(findEnclosingEpic(...))` still yields a
 * sane candidate set rather than nothing.
 *
 * Pure and side-effect-free: a single in-memory tree walk off data the nav
 * already holds (no disk I/O — AC-6).
 */
export function findEnclosingEpic(
  tree: DerivedPipelineTree,
  targetPath: string,
): DerivedPipelineTree {
  let bestEpic: DerivedPipelineTree | null = null;
  let node: DerivedPipelineTree | undefined = tree;

  while (node && isWithin(node.absoluteDirPath, targetPath)) {
    if (node.mainDoc !== null) bestEpic = node;
    node = node.subEpics.find((sub) => isWithin(sub.absoluteDirPath, targetPath));
  }

  return bestEpic ?? tree;
}

/**
 * True when `targetPath` is `dir` itself or nested under it. Separator-guarded
 * so a sibling whose path is a string-prefix (`/a/epic-foo` vs
 * `/a/epic-foobar`) does not falsely match.
 */
function isWithin(dir: string, targetPath: string): boolean {
  return targetPath === dir || targetPath.startsWith(dir.endsWith("/") ? dir : `${dir}/`);
}
