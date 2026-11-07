// SPDX-License-Identifier: MIT
import { useConfigStore, type RepoState } from "@/shared/stores/config-store";
import { useOrgStore } from "@/shared/stores/org-store";

/** React hook equivalent of resolveRepo — uses Zustand selectors for
 *  reactive re-renders when the underlying repo data changes.
 *
 *  Three-tier lookup: org-store.workspaceRepos (active synthetic workspace
 *  repos) → config-store (legacy repos) → org-store.suspendedWorkspaceRepos
 *  (F-19 repos hidden during org/team switch). Workspace-first matches
 *  `settings-panel.tsx`'s fallback chain (R-10 AC6) and `resolveRepo()`. */
export function useResolvedRepo(repoPath: string | null): RepoState | undefined {
  const fromConfig = useConfigStore((s) =>
    repoPath ? s.repos.find((r) => r.repoPath === repoPath) : undefined,
  );
  const fromWorkspace = useOrgStore((s) => (repoPath ? s.workspaceRepos[repoPath] : undefined));
  const fromSuspended = useOrgStore((s) => {
    if (!repoPath) return undefined;
    for (const repos of Object.values(s.suspendedWorkspaceRepos)) {
      const match = repos[repoPath];
      if (match) return match;
    }
    return undefined;
  });
  return fromWorkspace ?? fromConfig ?? fromSuspended;
}
