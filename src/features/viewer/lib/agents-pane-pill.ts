// SPDX-License-Identifier: MIT
/** Explicit workspace navigation may reveal its group only in an already open pane. */
export interface PillPaneStore {
  revealAgentsWorkspace: (teamKey: string, workspaceId: string) => void;
}

export function applyPillSelection(
  store: PillPaneStore,
  teamKey: string,
  workspaceId: string,
): void {
  store.revealAgentsWorkspace(teamKey, workspaceId);
}
