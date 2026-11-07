// SPDX-License-Identifier: MIT
/**
 * SW-GS-3 — the redline data-source seam.
 *
 * A SECOND sanctioned swap-point (alongside `publish-set-source.ts`) allowed to
 * import `@/tauri/commands` — see `no-direct-invoke.test.ts`. Wraps the real
 * `get_file_redline` Tauri command over an EXPLICIT `org_root`, so the redline
 * viewer (and any git-sync consumer) never touches `invoke` directly.
 */

import { getFileRedline as invokeGetFileRedline } from "@/tauri/commands";

import type { FileRedline } from "./types";

/**
 * Strip the `org_root` prefix from an absolute changed-file path, yielding the
 * `org_root`-relative path the backend expects (the inverse of `joinOrgPath`).
 * Returns `null` when `absPath` is not under `orgRoot` (e.g. a stale/cold root),
 * so the caller can render an empty state instead of querying a bogus path.
 */
export function stripOrgRoot(absPath: string, orgRoot: string | null): string | null {
  if (!orgRoot) return null;
  const root = orgRoot.endsWith("/") ? orgRoot.slice(0, -1) : orgRoot;
  if (absPath === root) return "";
  const prefix = `${root}/`;
  return absPath.startsWith(prefix) ? absPath.slice(prefix.length) : null;
}

/**
 * Fetch the before/after redline content for a changed file. The caller owns
 * `org_root` resolution and passes the `org_root`-relative `filePath`.
 *
 * SW-GS-14: `oldPath` (also `org_root`-relative) is set only for a rename, so
 * "before" is read from the pre-rename baseline path (a moved doc's content
 * lives at the old path) instead of reading as all-added.
 */
export function getFileRedline(
  orgRoot: string,
  filePath: string,
  oldPath?: string,
  prNumber?: number,
  commitSha?: string,
  worktree?: string,
): Promise<FileRedline> {
  return worktree
    ? invokeGetFileRedline(orgRoot, filePath, oldPath, prNumber, commitSha, worktree)
    : invokeGetFileRedline(orgRoot, filePath, oldPath, prNumber, commitSha);
}
