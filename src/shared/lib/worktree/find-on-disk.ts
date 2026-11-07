// SPDX-License-Identifier: MIT
import { listWorktrees, type WorktreeInfo } from "@/tauri/commands";
import { resolveBranchName, branchRefMatches } from "./resolve";

/**
 * Find an existing on-disk worktree for a story by matching the resolved
 * branch name against `git worktree list` output.
 *
 * This is the single source of truth for on-disk detection — consumed by
 * `ensureWorktree()` (click-time gate) and the `hasWorktree` prefetch
 * (render-time button visibility).
 *
 * Matches by **branch name**, complementing `setupWorktree()`'s rehydrate
 * check which matches by **path**. Branch-match catches worktrees created
 * externally at non-standard paths (e.g., `git worktree add ../custom-dir feat/story-id`).
 *
 * @param repoPath       Absolute path to the main repo.
 * @param storyFilePath  Absolute path to the story `.md` file.
 * @param worktrees      Optional pre-fetched worktree list. When provided,
 *                        skips the `listWorktrees` IPC call — useful for
 *                        panel prefetch where one call serves N stories.
 */
export async function findWorktreeOnDisk(
  repoPath: string,
  storyFilePath: string,
  worktrees?: WorktreeInfo[],
): Promise<WorktreeInfo | null> {
  const [branchName, wtList] = await Promise.all([
    resolveBranchName(storyFilePath),
    worktrees ?? listWorktrees(repoPath).catch(() => []),
  ]);

  for (const wt of wtList) {
    if (branchRefMatches(wt.branch, branchName)) {
      return wt;
    }
  }

  return null;
}
