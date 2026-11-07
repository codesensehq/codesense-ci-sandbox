// SPDX-License-Identifier: MIT
import { useMemo } from "react";
import { useOrgStore } from "@/shared/stores/org-store";
import { parseTeamKey, useActiveTeamKey } from "@/shared/lib/team-key";

/** The active team's attached repos as `owner/repo` slugs. */
export function useTeamRepoSlugs(): string[] {
  const { orgSlug, teamName } = parseTeamKey(useActiveTeamKey());
  const team = useOrgStore((s) =>
    s.orgs.find((o) => o.slug === orgSlug)?.teams.find((t) => t.name === teamName),
  );
  return useMemo(() => (team?.source_repos ?? []).map((r) => `${r.owner}/${r.repo}`), [team]);
}
