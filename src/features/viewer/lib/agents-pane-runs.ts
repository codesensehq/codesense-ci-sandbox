// SPDX-License-Identifier: MIT
/**
 * The agents pane's Run history, opened from outside the pane (an ended agent's toast): the pane
 * and the terminal column come back, and the list shows its runs once it is drawn, whether or not
 * it is mounted when asked.
 */

import { create } from "zustand";

import { getActiveTeamKey, teamKeyHasTeam } from "@/shared/lib/team-key";
import { useViewerStore } from "@/shared/stores/viewer-store";

/** Whether the pane has been asked to show its Run history and has not yet done so. */
export const usePaneRunsRequest = create<{ pending: boolean }>(() => ({ pending: false }));

/** Show the active team's agents pane on its Run history: the terminal column and the pane open,
 *  and the list shows the runs as soon as it draws. */
export function openPaneRunHistory(): void {
  const teamKey = getActiveTeamKey();
  if (teamKeyHasTeam(teamKey)) {
    const store = useViewerStore.getState();
    store.setWorkspacePanelCollapsed(false);
    store.revealTerminalInPlace();
  }
  usePaneRunsRequest.setState({ pending: true });
}

/** The list has shown its Run history: the request is spent. */
export function takePaneRunsRequest(): void {
  usePaneRunsRequest.setState({ pending: false });
}
