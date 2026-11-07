// SPDX-License-Identifier: MIT
import { useAgentListProjectionStore } from "./agent-list-projection";
import type { WorkspaceSnapshot } from "./session-story";
import type { TerminalTabIndex, Workspace } from "@/shared/stores/viewer-store";
import type { TerminalLayoutProjection } from "@/shared/lib/terminal-layout-projection";
import type { AgentSessionRow } from "@/tauri/commands";

export type PaneWorkspaceSources = WorkspaceSnapshot & {
  running: TerminalTabIndex;
  minimized: TerminalTabIndex;
  hidden: TerminalTabIndex;
  layoutByKey: TerminalLayoutProjection["byKey"];
};
export function makePaneWorkspaceSources(
  sources: PaneWorkspaceSources,
  _preferTeamKey?: string,
): PaneWorkspaceSources {
  return sources;
}

/** Follow the Rust attribution reference; no tab or directory fallback runs in the client. */
export function paneWorkspaceLocationOf(
  row: Pick<AgentSessionRow, "handle" | "sessionId">,
  sources: Pick<PaneWorkspaceSources, "workspaces" | "softClosedWorkspaces">,
): { workspace: Workspace; teamKey: string } | undefined {
  const state = useAgentListProjectionStore.getState();
  const owner =
    state.sessions?.find((owner) => !!row.sessionId && owner.sessionId === row.sessionId) ??
    state.sessions?.find((owner) => owner.handle === row.handle);
  const ref = state.references.find(
    (ref) => ref.sessionRef === (owner?.id ?? owner?.sessionId ?? row.sessionId ?? row.handle),
  );
  if (!ref?.workspaceRef) return;
  const workspace =
    sources.workspaces[ref.teamKey]?.find((ws) => ws.id === ref.workspaceRef) ??
    sources.softClosedWorkspaces[ref.teamKey]?.[ref.workspaceRef]?.workspace;
  return workspace ? { workspace, teamKey: ref.teamKey } : undefined;
}
export const paneWorkspaceOf = (
  row: Pick<AgentSessionRow, "handle" | "sessionId">,
  sources: Pick<PaneWorkspaceSources, "workspaces" | "softClosedWorkspaces">,
): Workspace | undefined => paneWorkspaceLocationOf(row, sources)?.workspace;
