// SPDX-License-Identifier: MIT
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useMemo } from "react";
import { useContentSource } from "@/shared/hooks/use-content-source";
import { resolveOrgRoot, resolveRealRepoPath } from "@/shared/lib/resolve-source-repo-path";

/** The surface the left nav is showing: its source repo, org root and team name. */
export interface NavSurface {
  sourceRepoPath: string;
  orgRoot: string;
  team: string;
}

/**
 * The nav's surface, for the git-shared team reads (team nav pins, merge marks).
 * `teamKey` is `${orgSlug}:${teamName}` (`_none` for no team); parsed inline so
 * the nav render path doesn't couple to `parseTeamKey`'s module in component
 * tests.
 */
export function useNavSurface(
  navRepoPath: string,
  teamKey: string,
  /** The owning clone/team identity, whatever source the sidebar currently displays. */
  owning = false,
): NavSurface {
  const privateSource =
    useViewerStore((s) => s.navLens[teamKey]?.source?.kind === "private") && !owning;
  const sourceRepoPath = resolveRealRepoPath(navRepoPath) ?? "";
  const content = useContentSource(navRepoPath);
  const orgRoot = content?.contextRepoPath
    ? resolveOrgRoot(navRepoPath) || content.contextRepoPath
    : "";
  const colon = teamKey.indexOf(":");
  const rawTeam = colon === -1 ? "" : teamKey.slice(colon + 1);
  const team = rawTeam === "_none" ? "" : rawTeam;
  return useMemo(
    () =>
      privateSource
        ? { sourceRepoPath: "", orgRoot: "", team: "" }
        : { sourceRepoPath, orgRoot, team },
    [sourceRepoPath, orgRoot, team, privateSource],
  );
}
