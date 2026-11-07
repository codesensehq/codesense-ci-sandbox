// SPDX-License-Identifier: MIT
import type { DerivedPipelineTree, StoryRef } from "./types";

/**
 * Single-scan result paired with its source repo slug.
 * The slug is `"owner/repo"` (e.g. `"alderco/codesense"`).
 */
export interface RepoScanResult {
  repoSlug: string;
  tree: DerivedPipelineTree;
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/** Recursively prefix every `dirPath` in the tree with the repo slug. */
function prefixDirPaths(node: DerivedPipelineTree, prefix: string): DerivedPipelineTree {
  const prefixedPath = node.dirPath === "" ? prefix : `${prefix}/${node.dirPath}`;
  return {
    ...node,
    dirPath: prefixedPath,
    stories: node.stories.map((s) => backfillSourceRepo(s, prefix)),
    subEpics: node.subEpics.map((sub) => prefixDirPaths(sub, prefix)),
  };
}

/** Ensure every StoryRef carries a `sourceRepo` — backfill from the known repo slug. */
function backfillSourceRepo(story: StoryRef, repoSlug: string): StoryRef {
  if (story.sourceRepo) return story;
  return { ...story, sourceRepo: repoSlug };
}

/**
 * Defense-in-depth team filter (B-1 AC10). The symlink layer is the primary
 * filter — the pipeline scanner only sees stories symlinked for the active
 * team. This catches stale symlinks, direct file edits, or watcher races.
 * Stories without a `team` field pass through for backward compat.
 */
export function filterByTeam(tree: DerivedPipelineTree, activeTeam: string): DerivedPipelineTree {
  return {
    ...tree,
    stories: tree.stories.filter((s) => s.team === null || s.team === activeTeam),
    subEpics: tree.subEpics.map((sub) => filterByTeam(sub, activeTeam)),
  };
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/**
 * Merge N per-repo scan results into a single `DerivedPipelineTree` with
 * a synthetic root. Each repo becomes a top-level sub-epic group.
 *
 * NOTE: currently **unwired** in production. SLW-NC-1 Phase 1/2 used this to
 * splice installed repos as `owner/repo` groups; Phase 3 replaced that with a
 * flat path-merge (`mergeTreeSourceWins` in `composite-to-repo-scan.ts`), so
 * this is exercised only by tests/fixtures now. Kept as a general, correct
 * N-source merge util (a future multi-repo grouped view could reuse it).
 *
 * Invariants:
 * - Every `dirPath` in each repo's tree is prefixed with the repo slug
 *   so `findScopedSubtree` can walk the merged tree correctly.
 * - Every `StoryRef` is guaranteed to have `sourceRepo` set (backfilled
 *   from the scan's known repo slug when absent from frontmatter).
 */
export function mergePipelineTrees(scans: RepoScanResult[]): DerivedPipelineTree {
  const subEpics = scans.map(({ repoSlug, tree }) => {
    // The scan result's root may have dirPath="" (scope root). We wrap
    // it so the repo slug becomes the dirPath of the repo-level group.
    const repoName = repoSlug.split("/").pop() ?? repoSlug;
    const prefixed = prefixDirPaths(tree, repoSlug);
    return {
      ...prefixed,
      dirName: repoName,
      // The repo-level group's dirPath is just the slug.
      dirPath: repoSlug,
    };
  });

  return {
    dirPath: "",
    dirName: "",
    absoluteDirPath: "",
    stories: [],
    subEpics,
    orderError: null,
    mainDoc: null,
  };
}
