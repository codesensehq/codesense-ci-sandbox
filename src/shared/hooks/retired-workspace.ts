// SPDX-License-Identifier: MIT
import type { ViewerState, Workspace } from "@/shared/stores/viewer-store";

/** The `mcp:workspace-retired` payload: a workspace `close_workspace` closed. */
export interface RetiredWorkspace {
  teamKey: string;
  /** The story id, or the workspace id for a non-story workspace. */
  form: string;
  /** The viewer row's id, when the backend's viewer held one. */
  workspaceId?: string | null;
}

/** Whether `ws` is a row `close_workspace` retired: the story row for `form`, or
 *  the row with `workspaceId`. The same match the backend applied to its copy. */
export function isRetiredRow(
  ws: Pick<Workspace, "id" | "kind" | "storyId">,
  retired: RetiredWorkspace,
): boolean {
  if (ws.kind === "story" && ws.storyId === retired.form) return true;
  return !!retired.workspaceId && ws.id === retired.workspaceId;
}

/** The team's rows `close_workspace` retired: open ones, and minimized ones in the tray. */
export function retiredWorkspaceIds(
  state: Pick<ViewerState, "workspaces" | "softClosedWorkspaces">,
  retired: RetiredWorkspace,
): { open: string[]; minimized: string[] } {
  const open = (state.workspaces[retired.teamKey] ?? [])
    .filter((ws) => isRetiredRow(ws, retired))
    .map((ws) => ws.id);
  const minimized = Object.entries(state.softClosedWorkspaces[retired.teamKey] ?? {})
    .filter(([, shadow]) => isRetiredRow(shadow.workspace, retired))
    .map(([id]) => id);
  return { open, minimized };
}
