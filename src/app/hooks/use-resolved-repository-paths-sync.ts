// SPDX-License-Identifier: MIT
import { useEffect, useRef } from "react";

import { reattachWorkspaceWatchesWithNewContext } from "@/app/lib/workspace-watch-bridge";
import { loadOrg } from "@/shared/api/registry";
import { syntheticRepoKey } from "@/shared/lib/build-synthetic-repo";
import { useCurrentUser } from "@/shared/lib/get-current-user";
import { logger } from "@/shared/lib/logger";
import { sourceRepoRemoteKey } from "@/shared/lib/source-repo-key";
import { useOrgStore } from "@/shared/stores/org-store";
import type { Org } from "@/shared/types/domain";
import type { RepoState } from "@/shared/stores/config-store";

/** `{team}\n{host/owner/repo}` — the key Rust stamps `resolved_repository_paths` with. */
function resolvedPathKey(team: string, slug: Parameters<typeof sourceRepoRemoteKey>[0]) {
  try {
    return `${team}\n${sourceRepoRemoteKey(slug)}`;
  } catch {
    return null;
  }
}

/**
 * SW-LN-B51: the tracked repos whose context-repo path Rust has not resolved yet. Rust
 * fills `resolved_repository_paths` only when an org loads, so a repo added to a team
 * mid-session (Add a source repo, the sample seed, attach) has none — and a worktree
 * opened on it attaches its watch with an empty context path, so no team context edit
 * ever reaches it.
 */
export function missingResolvedRepositoryPaths(org: Org): string[] {
  const out: string[] = [];
  for (const team of org.teams) {
    for (const repo of team.source_repos) {
      const key = resolvedPathKey(team.name, repo);
      if (key && !org.resolved_repository_paths?.[key]) out.push(key);
    }
  }
  return out.sort();
}

/**
 * SW-LN-B51: the synthetic workspace repos whose `contextRepoPath` differs from the path
 * Rust resolved for them, each returned with the resolved path. A repo with no resolved
 * path yet is left alone.
 */
export function syntheticReposWithStaleContextPath(
  orgs: readonly Org[],
  workspaceRepos: Record<string, RepoState>,
): RepoState[] {
  const out: RepoState[] = [];
  for (const org of orgs) {
    for (const team of org.teams) {
      for (const repo of team.source_repos) {
        const key = resolvedPathKey(team.name, repo);
        const resolved = key ? org.resolved_repository_paths?.[key] : undefined;
        if (!resolved) continue;
        let synthetic: RepoState | undefined;
        try {
          synthetic = workspaceRepos[syntheticRepoKey(org.slug, team.name, repo)];
        } catch {
          continue;
        }
        if (synthetic && synthetic.contextRepoPath !== resolved) {
          out.push({ ...synthetic, contextRepoPath: resolved });
        }
      }
    }
  }
  return out;
}

/**
 * SW-LN-B51: keep each workspace repo's context-repo path current with the tracked repos.
 * When a team gains a repo whose path Rust has not resolved, reload that org once to get
 * it; when a resolved path differs from the workspace repo's (an added repo, or a
 * repository-path migration), patch the workspace repo and re-attach any open worktree
 * watch that was attached with the old path — so team context edits reach it.
 */
export function useResolvedRepositoryPathsSync(): void {
  const orgs = useOrgStore((s) => s.orgs);
  const workspaceRepos = useOrgStore((s) => s.workspaceRepos);
  const principal = useCurrentUser()?.principal ?? null;
  const requested = useRef(new Set<string>());

  useEffect(() => {
    for (const org of orgs) {
      const missing = missingResolvedRepositoryPaths(org);
      if (missing.length === 0) continue;
      // One request per (org, missing set, principal); forgotten on failure so a later
      // change retries instead of leaving the repo's worktrees without team edits.
      const tag = `${principal ?? ""}\n${org.slug}\n${missing.join("\n")}`;
      if (requested.current.has(tag)) continue;
      requested.current.add(tag);
      loadOrg(org.slug, org.org_root, org.remote_url ?? null, principal)
        .then((loaded) => {
          const paths = loaded.org?.resolved_repository_paths;
          if (!paths || missing.some((key) => !paths[key])) {
            requested.current.delete(tag);
            logger.warn(
              "workspace-sync",
              `resolve repository paths for ${org.slug}: still missing after reload`,
            );
          }
          if (!paths) return;
          // The reload is authoritative for this org: replace, so a removed repo's
          // old path can't re-patch the repo if it is added again.
          const state = useOrgStore.getState();
          state.setOrgs(
            state.orgs.map((o) =>
              o.slug === org.slug ? { ...o, resolved_repository_paths: paths } : o,
            ),
          );
        })
        .catch((e) => {
          requested.current.delete(tag);
          logger.warn("workspace-sync", `resolve repository paths for ${org.slug} failed: ${e}`);
        });
    }
  }, [orgs, principal]);

  useEffect(() => {
    const stale = syntheticReposWithStaleContextPath(orgs, workspaceRepos);
    if (stale.length === 0) return;
    const { setWorkspaceRepo } = useOrgStore.getState();
    for (const repo of stale) setWorkspaceRepo(repo);
    reattachWorkspaceWatchesWithNewContext();
  }, [orgs, workspaceRepos]);
}
