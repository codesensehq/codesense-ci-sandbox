// SPDX-License-Identifier: MIT
/**
 * SS.8: Match a watcher-emitted `file_path` against the two locations
 * worktree.yaml can live during the SS.12 transition window:
 *
 * - Legacy: `<source>/.codesense/worktree.yaml`
 * - New:    `{orgRoot}/_codesense/worktree/{owner}/{repo}.yaml`
 *
 * Used by the settings panel's `context-file-change` subscription so a
 * refresh fires regardless of which path the active yaml lives at.
 *
 * **Strict `.yaml` suffix required.** Editor backup files (`.swp`, `.lock`,
 * `.tmp`) and sibling files under `_codesense/worktree/` (e.g. a future
 * `README.md`) do NOT trigger a refresh. The watcher already fires on
 * the final atomic rename; intermediate temp-file writes don't need
 * their own refresh.
 *
 * Single source of truth — duplicate this logic anywhere else and the
 * SS.12 cleanup will miss a site.
 */
export function isWorktreeYamlChangeEvent(changedPath: string): boolean {
  if (!changedPath || !changedPath.endsWith(".yaml")) return false;
  return (
    changedPath.endsWith("/.codesense/worktree.yaml") ||
    changedPath.includes("/_codesense/worktree/")
  );
}
