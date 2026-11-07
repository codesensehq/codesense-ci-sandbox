// SPDX-License-Identifier: MIT
import { useConfigStore, type RepoState } from "@/shared/stores/config-store";
import { useOrgStore } from "@/shared/stores/org-store";

/** Resolve a RepoState by repoPath — checks org-store.workspaceRepos (active
 *  synthetic workspace repos) first, then config-store (legacy repos), then
 *  F-19 suspendedWorkspaceRepos (repos hidden during org/team switch).
 *
 *  Workspace-first matches the fallback chain in `settings-panel.tsx`
 *  established by R-10 AC6. In normal use the store key spaces are disjoint
 *  (legacy repos use real paths; synthetic repos use `workspace:org:team:slug`
 *  keys), so priority is moot — the alignment removes a latent footgun for
 *  fixtures, migrations, or future features that might overlap. */
export function resolveRepo(repoPath: string): RepoState | undefined {
  const orgState = useOrgStore.getState();
  const activeMatch = orgState.workspaceRepos[repoPath];
  if (activeMatch) return activeMatch;
  const configMatch = useConfigStore.getState().repos.find((r) => r.repoPath === repoPath);
  if (configMatch) return configMatch;
  // F-19: check suspended workspaces so resolveRepo still works for
  // repos hidden during org/team switch (F-16 guard, etc.).
  for (const suspended of Object.values(orgState.suspendedWorkspaceRepos)) {
    const match = suspended[repoPath];
    if (match) return match;
  }
  return undefined;
}

/** Resolve a RepoState by on-disk **source path**.
 *
 *  Use this when you have a real filesystem path (e.g. the `sourceRepoPath`
 *  argument to operation-store entrypoints) and need to find the owning
 *  `RepoState` regardless of which store it lives in.
 *
 *  - Workspace repos: `repo.localPath` holds the source path (or null when
 *    no clone is attached — F-6).
 *  - Legacy repos: `repo.repoPath` is itself the source path.
 *
 *  Workspace-first matches the fallback chain in `settings-panel.tsx` and
 *  `resolveRepo` (R-10 AC6 / R-11 AC3).
 *
 *  Returns `undefined` for the empty string and for unknown paths. Workspace
 *  entries with a null/empty `localPath` are skipped. */
export function findRepoBySourcePath(sourcePath: string): RepoState | undefined {
  if (!sourcePath) return undefined;
  const orgState = useOrgStore.getState();
  for (const repo of Object.values(orgState.workspaceRepos)) {
    if (repo.localPath && repo.localPath === sourcePath) return repo;
  }
  const configMatch = useConfigStore.getState().repos.find((r) => r.repoPath === sourcePath);
  if (configMatch) return configMatch;
  for (const suspended of Object.values(orgState.suspendedWorkspaceRepos)) {
    for (const repo of Object.values(suspended)) {
      if (repo.localPath && repo.localPath === sourcePath) return repo;
    }
  }
  return undefined;
}
