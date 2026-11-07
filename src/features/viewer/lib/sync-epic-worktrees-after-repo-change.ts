// SPDX-License-Identifier: MIT
import { assembleEpicWorktrees, reconcileEpicWorktrees } from "@/tauri/commands";
import { withErrorToast } from "@/shared/lib/with-error-toast";
import { logger } from "@/shared/lib/logger";

import { collectInUsePathsFromStore } from "./collect-in-use-paths";
import type { EpicCommandContext } from "./epic-workspace-opener";

/**
 * SW-EW-45: after an epic's `repos:` change is persisted, sync its integration
 * worktrees to the new set — fire-and-forget. An **add** (`opts.assemble`)
 * assembles the newly assigned repo; a **remove** reconciles: the de-assigned
 * repo's worktree is torn down (branch-preserving, dirty/in-use-skipping via the
 * live cwd set). The caller's frontmatter write is the source of truth and has
 * already succeeded, so the UI never awaits this and a worktree failure only toasts.
 *
 * `ctx` is resolved by the caller; a `null` ctx (org/team/user not fully resolved —
 * a narrow race) is logged, not thrown: the change is persisted and re-syncs on the
 * next add/open once context resolves.
 *
 * Extracted from `team-workspace-view`'s `handleSetEpicRepos` so the add-vs-remove
 * dispatch (and the in-use guard the remove path forwards) is unit-testable without
 * rendering the view.
 */
export function syncEpicWorktreesAfterRepoChange(
  ctx: EpicCommandContext | null,
  epicDocPath: string,
  opts: { assemble: boolean },
): void {
  if (!ctx) {
    logger.warn(
      "epic-repos",
      `skipped worktree ${opts.assemble ? "assemble" : "reconcile"} for "${epicDocPath}" — epic command context unresolved`,
    );
    return;
  }
  if (opts.assemble) {
    withErrorToast(
      assembleEpicWorktrees({ ...ctx, epicFilePath: epicDocPath }),
      "Couldn't assemble worktrees: ",
    );
  } else {
    // Reconcile tears down worktrees for de-assigned repos; the live in-use set
    // (open + minimized cwds) is the guard that preserves an in-use worktree.
    withErrorToast(
      reconcileEpicWorktrees({ ...ctx, epicFilePath: epicDocPath }, collectInUsePathsFromStore()),
      "Couldn't tear down removed worktrees: ",
    );
  }
}
