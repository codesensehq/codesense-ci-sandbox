// SPDX-License-Identifier: MIT
/**
 * SW-GS-2 — the publish-set data-source seam.
 *
 * This is the SINGLE swap-point between the git-sync front-end and the backend:
 * the ONLY git-sync file allowed to import `@/tauri/commands` (enforced by
 * `no-direct-invoke.test.ts`). It calls the real `get_publish_set` Tauri command
 * over an EXPLICIT context-repo root (`org_root`).
 *
 * The caller owns org-root resolution and passes it in. A null/empty root yields
 * an empty set rather than fetching — there is deliberately NO store-scan
 * fallback here: a stale/cold-start root could target the wrong repo, so the
 * caller short-circuits to the empty state and re-fetches reactively once it can
 * resolve the right root.
 */

import { getPublishSet as invokeGetPublishSet, listScratchSpace } from "@/tauri/commands";

import { EMPTY_PUBLISH_SET, type PublishSet } from "./types";

/**
 * Resolve the current grouped publish set for the given context-repo root.
 * Returns an empty set when `orgRoot` is missing (never falls back to a scan).
 *
 * SW-GS-19 (A1): when `prNumber` is set, returns that PR's changed files
 * (head-tree vs merge-base) instead of the working-tree set.
 * SW-GS-13: when `commitSha` is set (and no `prNumber`), returns that landed
 * commit's changed files (tree vs first-parent). Either way the shape + grouping
 * are identical, so the exact same list pipeline renders it.
 */
export function getPublishSet(
  orgRoot?: string | null,
  prNumber?: number,
  commitSha?: string,
  // Scope the working-tree diff to the active team's content subtree via a git
  // pathspec (server-side). Ignored for PR/commit sources.
  pathPrefix?: string | null,
  worktree?: string,
): Promise<PublishSet> {
  if (!orgRoot) return Promise.resolve(EMPTY_PUBLISH_SET);
  return worktree
    ? invokeGetPublishSet(
        orgRoot,
        undefined,
        prNumber,
        commitSha,
        pathPrefix ?? undefined,
        worktree,
      )
    : invokeGetPublishSet(orgRoot, undefined, prNumber, commitSha, pathPrefix ?? undefined);
}

export function readPrivateSourceListing(org: string | null) {
  return listScratchSpace(org);
}
