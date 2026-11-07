// SPDX-License-Identifier: MIT
import { useOrgStore } from "@/shared/stores/org-store";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import { isSyntheticRepoPath, parseSyntheticRepoKey } from "@/shared/lib/build-synthetic-repo";
import { parseContextId, teamAllContextId } from "./context-model";
import { logger } from "@/shared/lib/logger";

/** Content refresh has one team owner; source subscribers retain their scoped keys. */
export function bumpFileRefreshForSourceRepo(
  repoIdentity: string,
  changedPaths?: Set<string>,
  counterOnly?: boolean,
): void {
  if (!repoIdentity) return;
  const context = parseContextId(repoIdentity);
  const synthetic = isSyntheticRepoPath(repoIdentity) ? parseSyntheticRepoKey(repoIdentity) : null;
  if (
    (isSyntheticRepoPath(repoIdentity) && !synthetic) ||
    (repoIdentity.startsWith("team:") && context?.kind !== "team-all")
  ) {
    logger.warn(
      "refresh-for-source-repo",
      `malformed synthetic key, skipping bump: ${repoIdentity}`,
    );
    return;
  }
  const workspaceRepos = useOrgStore.getState().workspaceRepos;
  const owner =
    context?.kind === "team-all" ? { orgSlug: context.orgSlug, team: context.teamName } : synthetic;
  const keys = new Set([repoIdentity]);
  if (owner?.orgSlug && owner.team) keys.add(teamAllContextId(owner.orgSlug, owner.team));
  for (const repo of Object.values(workspaceRepos)) {
    const other = parseSyntheticRepoKey(repo.repoPath);
    if (owner && other?.orgSlug === owner.orgSlug && other?.team === owner.team) {
      keys.add(repo.repoPath);
    } else if (!owner && repo.localPath === repoIdentity) {
      keys.add(repo.repoPath);
      if (other) keys.add(teamAllContextId(other.orgSlug, other.team));
    }
  }
  for (const key of keys)
    useFileRefreshStore.getState().bumpFileRefresh(key, changedPaths, counterOnly);
}
