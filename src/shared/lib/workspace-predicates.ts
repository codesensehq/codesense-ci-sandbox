// SPDX-License-Identifier: MIT
import type { Workspace } from "@/shared/stores/viewer-store";

/** An epic *integration* shell workspace: the epic-level row that has no
 *  source repo of its own (`sourceRepoSlug` unset) but is anchored to an epic
 *  dir (`epicDirPath`). Such a workspace is a VIEW over the team's pipeline,
 *  not a repo workspace — its identity `repoPath` is "" and its terminal is
 *  rooted at the epic dir rather than a source clone.
 *
 *  Canonical home for the predicate that previously lived inline (and
 *  divergently named — `isEpicShellWorkspace`, `isEpicIntegration`) across
 *  team-workspace-view, status-bar, and use-team-pipeline-repo-path.
 *
 *  NOTE: the `use-epic-repo-overlay` / `workspace-watch-bridge` variants add
 *  `epicDocPath` / `epicWorkspaceDir` / `kind` conditions on top — they are a
 *  different (stricter) test and intentionally NOT folded in here. */
export function isEpicShellWorkspace(
  workspace: Pick<Workspace, "sourceRepoSlug" | "epicDirPath"> | null | undefined,
): boolean {
  return !!workspace && !workspace.sourceRepoSlug && !!workspace.epicDirPath;
}
