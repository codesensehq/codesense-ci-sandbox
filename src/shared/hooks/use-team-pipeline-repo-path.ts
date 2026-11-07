// SPDX-License-Identifier: MIT
import type { Workspace } from "@/shared/stores/viewer-store";
import type { Org } from "@/shared/types/domain";
import { resolveTeamContentContext } from "@/shared/lib/team-content-context";
import { useTeamContentContext } from "./use-team-content-context";

/** Navigation belongs to the team context, independently of source workspaces. */
export function resolveTeamPipelineRepoPath(teamKey: string, orgs: Org[]): string {
  const context = resolveTeamContentContext(teamKey, orgs, true);
  return context.kind === "ready" ? context.source.repoPath : "";
}

export function useTeamPipelineRepoPath(_workspace: Workspace | null, teamKey: string): string {
  const context = useTeamContentContext(teamKey);
  return context.kind === "ready" ? context.source.repoPath : "";
}
