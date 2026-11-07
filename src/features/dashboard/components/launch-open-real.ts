// SPDX-License-Identifier: MIT
/**
 * The real launch mechanism behind every "launch this agent somewhere" dialog: the shipped
 * `dispatchOpenWorkspace` over its real deps, with the per-kind worktree hydrate seams wrapped
 * so the dialog's progress moves from `hydrating` to `starting` the instant the worktree is
 * ready. No new spawn path: every reserve and bind flows through the one launch registry.
 *
 * Kept in its own module (dialogs lazy-import it) so the dialogs and their tests never pull
 * in the launch controller.
 */

import {
  dispatchOpenWorkspace,
  makeRealBranchLaunchDeps,
  makeRealDashboardLaunchDeps,
  makeRealHeadlessLaunchers,
  type HeadlessSessionLaunchers,
} from "@/features/dashboard/components/dashboard-launch-controller";
import type {
  BranchLaunchDeps,
  DashboardLaunchDeps,
  DirLaunchDeps,
} from "@/features/dashboard/components/dashboard-launch";
import type { LaunchOpen } from "@/shared/lib/launch-target";
import { localDefaultBranch } from "@/tauri/commands";
import { getActiveTeamKey } from "@/shared/lib/team-key";
import { useViewerStore } from "@/shared/stores/viewer-store";

/** Launch through `dispatchOpenWorkspace`, stamping each session with `launcher` (the
 *  launching surface the Running Agents panel shows). */
export function makeRealLaunchOpen(launcher: string): LaunchOpen {
  return (arg, { onHydrated }) => {
    const baseDeps = makeRealDashboardLaunchDeps(launcher);
    const storyDeps: DashboardLaunchDeps = {
      ...baseDeps,
      // `story` open: the worktree is created/attached here — announce hydrate-done on resolve.
      ensureWorktree: async (teamKey, storyFilePath) => {
        const cwd = await baseDeps.ensureWorktree(teamKey, storyFilePath);
        onHydrated();
        return cwd;
      },
    };

    const baseBranchDeps = makeRealBranchLaunchDeps(launcher);
    const branchDeps: BranchLaunchDeps = {
      ...baseBranchDeps,
      // `branch` open: the checkout into a managed worktree is the hydrate step.
      ensureBranchWorktree: async (res, target) => {
        const cwd = await baseBranchDeps.ensureBranchWorktree(res, target);
        onHydrated();
        return cwd;
      },
    };

    const dirDeps: DirLaunchDeps = {
      openGenericWorkspace: (path) =>
        useViewerStore.getState().openOrFocusGenericWorkspace(getActiveTeamKey(), path),
    };

    const baseHeadless = makeRealHeadlessLaunchers(launcher);
    const headlessLaunchers: HeadlessSessionLaunchers = {
      ...baseHeadless,
      // `dir` open: the dir already exists (hydrate is a no-op), so announce it before the
      // agent session spawns.
      openDirHeadless: (path, opts) => {
        onHydrated();
        return baseHeadless.openDirHeadless(path, opts);
      },
    };

    return dispatchOpenWorkspace(arg, launcher, storyDeps, branchDeps, dirDeps, headlessLaunchers);
  };
}

/** The branch a "New worktree" launch in `repo` (`owner/repo`) starts from, read from the
 *  repo's local clone in the active team; `null` when there is no such clone. */
export async function resolveNewBranchBase(launcher: string, repo: string): Promise<string | null> {
  const res = await makeRealBranchLaunchDeps(launcher).resolveBranch(repo);
  return res ? localDefaultBranch(res.repoPath) : null;
}
