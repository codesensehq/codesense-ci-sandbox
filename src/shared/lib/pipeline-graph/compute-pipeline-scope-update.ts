// SPDX-License-Identifier: MIT
import { normalizeScopePath } from "./normalize";

/**
 * Sync-helper between file-tree navigation and pipeline-viewer scope (30.7 AC-35).
 *
 * Given the absolute path the user just navigated to, decide whether the
 * pipeline scope should update, and if so, what the new scope value is.
 *
 * PTS-2: the scope base is `teamContentPath` (the per-team flat content dir,
 * resolved by Rust) — used to live at `${contextRepoPath}/${viewerRoot}` but
 * that composition no longer reaches context content.
 *
 * Returns:
 * - A non-null string (possibly "") — the new `pipelineScope[repoPath]`
 *   value, relative to `teamContentPath`. Sub-cases:
 *     - target is `teamContentPath` itself → ""
 *     - target is inside `teamContentPath` → relative path under it
 *     - target is inside the context repo but ABOVE the content dir
 *       (e.g. the file tree's "back to root" button) → "" (reset to root)
 * - `null` — "pin, don't update". Only returned when the target is outside
 *   the context repo entirely (a different repo, a symlink out, etc.).
 *
 * R-14: The `pipelineRoot` parameter was removed. The install/projection
 * layer still consumes the stored `pipelineRoot` value but at a different
 * boundary.
 *
 * Pure function: no store access, no IO. Used by every file-tree nav surface
 * (drill-in, drill-out, re-entry) to keep sync rules in one place.
 */
export function computePipelineScopeUpdate(
  targetAbsolutePath: string,
  contextRepoPath: string,
  teamContentPath: string,
): string | null {
  // Guard `teamContentPath` alongside the others: pre-hydration race (PTS-3's
  // async `resolveTeamContentDir` hasn't returned yet) leaves it as "". With
  // `contentRootAbs = ""` and `insidePrefix = "/"`, any absolute target falsely
  // matches the inside-content-root branch and returns the path-minus-slash as
  // a scope. Treat the unhydrated case as "no decision possible — pin null".
  if (!targetAbsolutePath || !contextRepoPath || !teamContentPath) return null;

  const target = stripTrailingSlash(targetAbsolutePath);
  const repo = stripTrailingSlash(contextRepoPath);
  const contentRootAbs = stripTrailingSlash(teamContentPath);

  // Inside content root — standard drill-in case.
  if (target === contentRootAbs) return "";
  const insidePrefix = `${contentRootAbs}/`;
  if (target.startsWith(insidePrefix)) {
    return normalizeScopePath(target.slice(insidePrefix.length));
  }

  // Above content root but still inside the context repo — reset to root.
  if (target === repo || target.startsWith(`${repo}/`)) {
    return "";
  }

  // Truly outside the repo — pin.
  return null;
}

function stripTrailingSlash(p: string): string {
  return p.endsWith("/") && p.length > 1 ? p.slice(0, -1) : p;
}
