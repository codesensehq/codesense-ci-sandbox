// SPDX-License-Identifier: MIT
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { firstRepoPathForTeam } from "@/shared/lib/build-synthetic-repo";
import { bumpFileRefreshForSourceRepo } from "@/shared/lib/refresh-for-source-repo";

/**
 * SW-NR-1: route a completed reverse-sync wave's writes to the team/epic
 * pipeline nav. SW-XS-4 (K11): the only such wave is now the explicit publish
 * operation (`AttachReconcile`); nothing reverse-syncs on its own.
 *
 * A reverse write lands in a team's SHARED content dir; the team nav is a
 * team-stable synthetic whose `localPath` is the root-clone attachment, never
 * the editing worktree — so the worktree-path `localPath` fan-out in
 * `bumpFileRefreshForSourceRepo` (which `use-sync-wave-rescan` already runs over
 * `affected_repos`) can't reach it. These helpers resolve the write to an owning
 * team synthetic and feed it BACK through `bumpFileRefreshForSourceRepo`, whose
 * synthetic branch already fans out to every same-`(org, team)` sibling — no new
 * fan-out logic, symmetric with the forward SW-FX-16 path.
 */

/** SW-XS-4: waves carry no per-file `changed_paths` any more (SW-SY-4 retired),
 *  so the team nav is reached through the wave's `affected_repos` worktrees: resolve each to its owning team via
 *  the open-workspace registry (`viewer-store.workspaces`, keyed by teamKey),
 *  then bump that team's nav synthetic with `counterOnly` — the nav re-scans
 *  WITHOUT writing `lastChangedContextFile`, so a concurrent watcher-fed precise
 *  Set is never clobbered. Deduped per team. */
export function bumpTeamNavForWorktrees(worktreePaths: readonly string[]): void {
  const workspaces = useViewerStore.getState().workspaces;
  const workspaceRepos = useOrgStore.getState().workspaceRepos;
  const bumped = new Set<string>();
  for (const worktree of worktreePaths) {
    // Empty string is never intent; skips a stray `""` before it could false-match a `cwd: ""` workspace.
    if (!worktree) continue;
    const teamKey = teamKeyForWorktree(workspaces, worktree);
    if (!teamKey) continue;
    // Any one team synthetic is sufficient — the synthetic branch of
    // `bumpFileRefreshForSourceRepo` fans out to the rest of the (org, team).
    const key = firstRepoPathForTeam(workspaceRepos, teamKey);
    if (!key || bumped.has(key)) continue;
    bumped.add(key);
    bumpFileRefreshForSourceRepo(key, undefined, /* counterOnly */ true);
  }
}

/** Find the teamKey whose open-workspace registry contains a workspace rooted
 *  at `worktree` (`ws.cwd === worktree`). Returns null when no open workspace
 *  matches (e.g. a reverse write from a worktree this app instance isn't
 *  showing). */
function teamKeyForWorktree(
  workspaces: Record<string, { cwd: string }[]>,
  worktree: string,
): string | null {
  for (const [teamKey, wss] of Object.entries(workspaces)) {
    if (wss.some((w) => w.cwd === worktree)) return teamKey;
  }
  return null;
}
