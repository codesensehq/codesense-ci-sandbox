// SPDX-License-Identifier: MIT
import { sourceRepoRemoteKey } from "@/shared/lib/source-repo-key";
import type { Org, SourceRepoSlug } from "@/shared/types/domain";

export interface WorkspaceTrackingMatch {
  orgSlug: string;
  orgDisplayName: string;
  teamName: string;
}

/**
 * Derive which workspaces (org + team pairs) track a given source repo.
 *
 * Takes a pre-parsed `{owner, repo}` — NOT a raw URL — because
 * `parseGitRemote` is an async IPC call that cannot run inside a
 * synchronous pure function. The hook calls `parseGitRemote` first
 * and passes the result here.
 *
 * Returns all matches across all orgs. `null` input → empty array.
 */
export function deriveWorkspacesTracking(
  parsed: SourceRepoSlug | null,
  orgs: Org[],
): WorkspaceTrackingMatch[] {
  if (!parsed) return [];

  const matches: WorkspaceTrackingMatch[] = [];
  for (const org of orgs) {
    for (const team of org.teams) {
      const tracked = team.source_repos.some(
        (sr) => sourceRepoRemoteKey(sr) === sourceRepoRemoteKey(parsed),
      );
      if (tracked) {
        matches.push({
          orgSlug: org.slug,
          orgDisplayName: org.display_name,
          teamName: team.name,
        });
      }
    }
  }
  return matches;
}
