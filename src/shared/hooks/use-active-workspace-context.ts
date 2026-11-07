// SPDX-License-Identifier: MIT
import { useMemo } from "react";

import { useViewerStore, type ViewerState, type Workspace } from "@/shared/stores/viewer-store";
import { buildSyntheticRepoPathFromPrompt } from "@/shared/lib/build-synthetic-repo";
import {
  NO_TEAM_SENTINEL,
  activeWorkspaceForTeam,
  getActiveTeamKey,
  useActiveTeamKey,
} from "@/shared/lib/team-key";

/** Context for the workspace currently focused in the active team's
 *  workspace bar. Replaces the per-call `useViewerStore((s) => s.selectedRepoPath)`
 *  + manual workspace lookup pattern that recurred across ~17 consumers
 *  pre-T.6.
 *
 *  - `teamKey` is the active team key, or `NO_TEAM_SENTINEL` when no
 *    org is active. Always a string for safe store-read keying.
 *  - `workspace` is the active workspace (root or story) inside that
 *    team's bar, or `null` when the team has no workspaces or no
 *    workspace is focused.
 *  - `sourceRepoSlug` is the source repo identity carried by the
 *    workspace (M.3), or `null` when there is no workspace.
 *  - `repoPath` is the synthetic workspace key
 *    (`workspace:{org}:{team}:{owner}/{repo}`) derived from `teamKey +
 *    sourceRepoSlug` — the same canonical form the pre-T.6 store's
 *    `selectedRepoPath` carried, and the key consumers use to look up
 *    `workspaceRepos[repoPath]`, thread to `<ViewerTabs repoPath>`, resolve
 *    `teamContentPath` for `revealOpenedFile`, etc. **NOT** the same as
 *    `workspace.sourceRepoPath`, which is the M.2 on-disk clone path
 *    (a real filesystem path, not a synthetic key) and would miss every
 *    `workspaceRepos` lookup.
 */
export interface ActiveWorkspaceContext {
  teamKey: string;
  workspace: Workspace | null;
  sourceRepoSlug: string | null;
  repoPath: string | null;
}

function deriveRepoPath(teamKey: string, sourceRepoSlug: string | null): string | null {
  if (teamKey === NO_TEAM_SENTINEL || !sourceRepoSlug) return null;
  return buildSyntheticRepoPathFromPrompt({ teamKey, sourceRepoSlug });
}

/** The workspace focused in `teamKey`'s bar, or null (no team, no workspaces, none focused). The
 *  one read of "the team has a focused workspace" every context and shortcut gate goes through.
 *  Returned by reference, so a store selector wrapping it stays `Object.is`-stable while the
 *  focused workspace is untouched. */
export function focusedWorkspaceOf(
  s: Pick<ViewerState, "activeWorkspace" | "workspaces">,
  teamKey: string,
): Workspace | null {
  if (teamKey === NO_TEAM_SENTINEL) return null;
  return activeWorkspaceForTeam(teamKey, s.activeWorkspace, s.workspaces);
}

/** The context of `teamKey` focused on `workspace`: the one builder every variant shares. */
function workspaceContextOf(teamKey: string, workspace: Workspace | null): ActiveWorkspaceContext {
  const sourceRepoSlug = workspace?.sourceRepoSlug ?? null;
  return { teamKey, workspace, sourceRepoSlug, repoPath: deriveRepoPath(teamKey, sourceRepoSlug) };
}

/** `teamKey`'s context, re-rendering when its focused workspace changes. Subscribes to the single
 *  focused `Workspace` object rather than the team's whole list, so a change that leaves the
 *  focused workspace untouched (a frontmatter rescan of a sibling) re-renders nothing. Workspace
 *  objects are replaced immutably on any field change, so a change to the focused one still
 *  re-renders — see use-active-workspace-context.test.ts. */
export function useTeamWorkspaceContext(teamKey: string): ActiveWorkspaceContext {
  const workspace = useViewerStore((s) => focusedWorkspaceOf(s, teamKey));
  return useMemo(() => workspaceContextOf(teamKey, workspace), [teamKey, workspace]);
}

/** React hook variant — subscribes to viewer-store + org-store and
 *  re-renders the consumer when the active team or workspace changes.
 *  Use in components and hooks. */
export function useActiveWorkspaceContext(): ActiveWorkspaceContext {
  return useTeamWorkspaceContext(useActiveTeamKey());
}

/** Non-React variant — reads stores imperatively via `getState()`.
 *  Use in shortcut `run()` bodies, callbacks, action bodies, or
 *  anywhere a hook cannot run. Same contract as the hook. */
export function getActiveWorkspaceContext(): ActiveWorkspaceContext {
  const teamKey = getActiveTeamKey();
  return workspaceContextOf(teamKey, focusedWorkspaceOf(useViewerStore.getState(), teamKey));
}
