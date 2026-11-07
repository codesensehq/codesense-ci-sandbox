// SPDX-License-Identifier: MIT
import { teamAllContextId } from "./context-model";
import { buildTeamKey, parseTeamKey } from "./team-key";
import { pathToTeamContentDirSync } from "./team-content-path";
import type { Org } from "@/shared/types/domain";

/** Content consumers need these roots, never a source clone or terminal cwd. */
export interface ContentSource {
  repoPath: string;
  repoName: string;
  contextRepoPath: string;
  teamContentPath: string;
  mainDoc: string;
}

export type TeamContentResolution =
  | { kind: "loading" }
  | { kind: "unavailable"; error: string }
  | { kind: "ready"; source: ContentSource & { teamKey: string } };

export function resolveTeamContentContext(
  teamKey: string,
  orgs: readonly Org[],
  discoveryComplete: boolean,
  mainDoc = "",
): TeamContentResolution {
  const { orgSlug, teamName } = parseTeamKey(teamKey);
  if (!orgSlug || !teamName || /[/\\]|^\.{1,2}$/.test(teamName)) {
    return { kind: "unavailable", error: "Select an available team to browse its context." };
  }
  const org = orgs.find((item) => item.slug === orgSlug);
  if (!org)
    return discoveryComplete
      ? { kind: "unavailable", error: "This context is unavailable. Refresh the workspace list." }
      : { kind: "loading" };
  const team = org.teams.find((item) => item.name === teamName && !item.archived);
  if (!team || !org.org_root) {
    return {
      kind: "unavailable",
      error: "This team's context is unavailable. Refresh the workspace list.",
    };
  }
  const orgRoot = org.org_root.replace(/\/+$/, "");
  return {
    kind: "ready",
    source: {
      repoPath: teamAllContextId(orgSlug, team.name),
      repoName: org.display_name,
      contextRepoPath: orgRoot,
      teamContentPath: pathToTeamContentDirSync(orgRoot, team.name),
      mainDoc,
      teamKey,
    },
  };
}

/** Directory-boundary matching prevents a content-archive sibling claiming an event. */
export function teamContentIdentityForPath(
  path: string | undefined,
  orgs: readonly Org[],
): string | null {
  if (!path) return null;
  for (const org of orgs) {
    for (const team of org.teams) {
      const resolved = resolveTeamContentContext(buildTeamKey(org.slug, team.name), [org], true);
      if (resolved.kind !== "ready") continue;
      const root = resolved.source.teamContentPath;
      if (path === root || path.startsWith(`${root}/`)) return resolved.source.repoPath;
    }
  }
  return null;
}

/** Reveal uses merged folder coordinates; opening still uses the real owner path. */
export function teamContentNavigationPath(
  teamKey: string,
  filePath: string,
  orgs: readonly Org[],
  sourceTeam?: string | null,
): string | null {
  const own = resolveTeamContentContext(teamKey, orgs, true);
  if (own.kind !== "ready") return null;
  if (filePath.startsWith(`${own.source.teamContentPath}/`)) return filePath;
  if (!sourceTeam) return null;
  const { orgSlug } = parseTeamKey(teamKey);
  const source = resolveTeamContentContext(buildTeamKey(orgSlug, sourceTeam), orgs, true);
  if (source.kind !== "ready") return null;
  const root = source.source.teamContentPath;
  return filePath.startsWith(`${root}/`)
    ? `${own.source.teamContentPath}${filePath.slice(root.length)}`
    : null;
}
