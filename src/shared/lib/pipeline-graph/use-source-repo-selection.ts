// SPDX-License-Identifier: MIT
import { useMemo } from "react";

import type { SourceRepoSlug } from "@/tauri/commands";
import { useReadyResolverContext } from "@/shared/hooks/use-resolver-context";
import { useActiveOrg } from "@/shared/stores/use-active-org";
import { computeTeamGlyphMap, useActiveTeamKey } from "@/shared/lib/team-key";

/**
 * SLW-NC-1 — the deduped, ordered list of a team's source repos.
 *
 * The SINGLE source of truth for the consumers that must agree on the repo set AND its order:
 * the agent context pivot's "· in ‹Repo›" choices (SW-XS-15) and the provenance glyph/color
 * map (glyph collision suffixes are order-dependent, so order must match across surfaces).
 *
 * Scoped to `teamKey`: returns `[]` unless it is the active team (App.tsx mounts one
 * `TeamWorkspaceView` per open team). Deduped by slug so a duplicated `source_repos` entry
 * can't yield two colliding choices.
 */
export function useTeamSourceRepoSlugs(teamKey: string): SourceRepoSlug[] {
  const ctx = useReadyResolverContext();
  const org = useActiveOrg();
  const activeTeamKey = useActiveTeamKey();
  const isActive = teamKey === activeTeamKey;
  return useMemo(() => {
    if (!ctx || !org || !isActive) return [];
    const raw = org.teams.find((t) => t.name === ctx.teamName)?.source_repos ?? [];
    const seen = new Set<string>();
    return raw.filter((r) => {
      const slug = `${r.owner}/${r.repo}`;
      if (seen.has(slug)) return false;
      seen.add(slug);
      return true;
    });
  }, [ctx, org, isActive]);
}

/** Source-repo slug → short-code for the team's repos, derived in repo order so collision
 *  suffixes are stable: the one provenance-glyph map the nav's repo tint reads. */
export function useTeamRepoGlyphMap(teamKey: string): Map<string, string> {
  const repos = useTeamSourceRepoSlugs(teamKey);
  return useMemo(() => computeTeamGlyphMap(repos.map((r) => `${r.owner}/${r.repo}`)), [repos]);
}
