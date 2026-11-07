// SPDX-License-Identifier: MIT
import { isRemoteWorkspace } from "@/shared/lib/workspace-location";
import { storyIdFromPath } from "@/shared/lib/worktree/resolve";
import { readTeamKeyed, useViewerStore } from "@/shared/stores/viewer-store";
import { LOCAL_MACHINE_ID } from "@/tauri/commands";

/** Where a story's workspace lives now: `"local"`, a machine id, or `null` when it has
 *  none (open or minimized). */
export function storyWorkspacePlace(teamKey: string, storyFilePath: string): string | null {
  const workspaceId = storyIdFromPath(storyFilePath);
  const s = useViewerStore.getState();
  const ws =
    readTeamKeyed(s.workspaces, teamKey, []).find((w) => w.id === workspaceId) ??
    s.softClosedWorkspaces[teamKey]?.[workspaceId]?.workspace;
  if (!ws) return null;
  return isRemoteWorkspace(ws) ? ws.location.machineId : LOCAL_MACHINE_ID;
}

/**
 * Make sure a story's workspace lives on `machineId` (`"local"`: This Mac) before an
 * agent is launched into it. One that exists elsewhere is refused: a workspace stays
 * where it was created. One that does not exist yet is created on the machine (and this
 * resolves once it is ready there); on This Mac the launch itself creates it.
 *
 * SW-WS-1: the "stays where it was created" invariant is mutable via one
 * authorized verb — the `move_workspace` MCP tool. The refusal below still
 * compares against the current `location.machineId`; a successful move
 * updates that value and future placements succeed on the target. An
 * in-flight move surfaces a soft "being moved to X" message instead of
 * the hard refusal — see `locationConflictMessage` in viewer-store.
 */
export async function placeStoryWorkspace(
  teamKey: string,
  storyFilePath: string,
  machineId: string,
  focus = true,
): Promise<void> {
  const workspaceId = storyIdFromPath(storyFilePath);
  const at = storyWorkspacePlace(teamKey, storyFilePath);
  if (at !== null) {
    if (at === machineId) return;
    const where = at === LOCAL_MACHINE_ID ? "This Mac" : `machine ${at}`;
    throw new Error(
      `unavailable: ${workspaceId}'s workspace already lives on ${where}; a workspace stays where it was created`,
    );
  }
  if (machineId === LOCAL_MACHINE_ID) return;
  // The id may come from a dashboard: only one of the team's own machines takes a
  // workspace, so a stray id never pins the story's workspace to nowhere.
  // Loaded here, not at import: the launch controller imports this module everywhere.
  const { ensureTeamMachines, getTeamMachines } =
    await import("@/features/machines/stores/machines-store");
  await ensureTeamMachines(teamKey, machineId);
  const known = (getTeamMachines(teamKey) ?? []).some(
    (m) => m.machineId === machineId && m.kind === "ssh" && !m.binding?.hidden,
  );
  if (!known) {
    throw new Error(`unavailable: ${machineId} is not one of this team's machines`);
  }
  const cwd = await useViewerStore
    .getState()
    .openOrFocusStoryWorkspace(teamKey, storyFilePath, undefined, undefined, focus, true, {
      kind: "machine",
      machineId,
    });
  if (!cwd) {
    throw new Error(
      `unavailable: ${workspaceId}'s workspace was not created on machine ${machineId}`,
    );
  }
}
