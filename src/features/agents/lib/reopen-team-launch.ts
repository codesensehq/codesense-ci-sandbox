// SPDX-License-Identifier: MIT
import { agentKeyOfRevision } from "@/features/viewer/lib/agent-rows";
import type { Workspace } from "@/shared/stores/viewer-store";
import type { AgentSessionRow } from "@/tauri/commands";
import type { LaunchSheetRequest } from "./launch-sheet";

/** A team's relaunch is a destination choice, preserving the session's launch fields. */
export function teamSessionLaunchRequest(
  teamKey: string,
  workspace: Workspace,
  row: AgentSessionRow | undefined,
  tabName: string,
): LaunchSheetRequest | null {
  if (!row || !agentKeyOfRevision(row.agentRevision)) return null;
  if (
    workspace.kind !== "context" &&
    !(workspace.kind === "generic" && workspace.cwd.includes("/_worker/"))
  )
    return null;
  return {
    teamKey,
    where: "team-workspace",
    ...(workspace.kind === "context"
      ? { contextWorktreeId: workspace.contextWorktreeId ?? workspace.id }
      : {}),
    agentKey: agentKeyOfRevision(row?.agentRevision) ?? undefined,
    name: row?.displayName ?? tabName,
    role: row.role ?? undefined,
    queueRole: row.queueRole ?? null,
    managerHandle: row?.launchedBy ?? undefined,
    task: row?.startedWith ?? undefined,
    face: row?.avatar ?? undefined,
  };
}
