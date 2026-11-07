// SPDX-License-Identifier: MIT
import type { SourceRepoSlug, Team } from "@/shared/types/domain";

/** Deduped source-repo count for a team (SW-HB-2 AC-6). Two configs of the same
 *  owner/repo count once, matching how the inventory dedupes. */
export function repoCount(repos: SourceRepoSlug[]): number {
  return new Set(repos.map((r) => `${r.owner}/${r.repo}`.toLowerCase())).size;
}

/** Team name + deduped repo count — shared by the active/archived cards and the
 *  SLW-PT-1 provider card. SLW-IN-5 (AC-4): when the team inherits, a muted
 *  "inherits from {parent}" subtitle. Root teams (no parent) render nothing
 *  extra — no noise. A dangling parent (a hand-typed name with no matching team
 *  — possible until IN-3's load-time validation ships) still renders the name
 *  gracefully. */
export function TeamIdentity({ team }: { team: Team }) {
  const count = repoCount(team.source_repos);
  const parent = team.parent_team ?? null;
  return (
    <div className="flex min-w-0 flex-col gap-1">
      <span className="truncate font-medium">{team.name}</span>
      <span className="text-xs text-muted-foreground">
        {count} {count === 1 ? "repo" : "repos"}
      </span>
      {parent && (
        <span
          data-testid={`home-base-team-inherits-${team.name}`}
          className="truncate text-xs text-muted-foreground"
        >
          inherits from {parent}
        </span>
      )}
    </div>
  );
}
