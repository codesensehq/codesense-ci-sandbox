// SPDX-License-Identifier: MIT
import type { OpenDiffParams } from "./open-diff-types";
import { openFileInTeam } from "@/shared/lib/open-file-in-team";
import { openManagementTab } from "@/shared/lib/open-management-tab";

export const DIFF_TAB_PREFIX = "virtual://source-diff";
export type DiffTeam = { org: string; team: string };
export interface DiffTabAddress {
  params: OpenDiffParams;
  team?: DiffTeam;
}

export function diffTabPath(params: OpenDiffParams, team?: DiffTeam): string {
  return `${DIFF_TAB_PREFIX}/${encodeURIComponent(JSON.stringify(team ? { ...params, team } : params))}`;
}
/** A tab address names its read params and, when opened from a team, that team. Anything else is not a diff address. */
export function diffTabAddress(path: string): DiffTabAddress | null {
  try {
    const { team, ...params } = JSON.parse(
      decodeURIComponent(path.slice(DIFF_TAB_PREFIX.length + 1)),
    );
    if (![params.repo, params.rev, params.path].every((value) => typeof value === "string"))
      return null;
    const scoped = typeof team?.org === "string" && typeof team?.team === "string";
    return scoped ? { params, team: { org: team.org, team: team.team } } : { params };
  } catch {
    return null;
  }
}
/** Open (or focus) the diff tab in the team it came from; without one, a management tab. */
export function openDiffTab(params: OpenDiffParams, team?: DiffTeam): void {
  const path = diffTabPath(params, team);
  if (team) openFileInTeam({ ...team, path }, { newTab: true, keepLayout: true });
  else openManagementTab(path);
}
