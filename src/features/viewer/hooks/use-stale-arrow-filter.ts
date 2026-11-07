// SPDX-License-Identifier: MIT
import { useEffect } from "react";

import { useWorkspaceFilterFate } from "./use-pane-workspace-wait";
import { arrowFilterFate, machineFactsOf } from "@/features/viewer/lib/arrow-filter-fate";
import { useViewerStore, type ViewerState } from "@/shared/stores/viewer-store";

/** App-root owner of workspace filter cleanup, including roster-aware retirement. */
export function useStaleArrowFilterCleanup(): void {
  const { fate } = useWorkspaceFilterFate();
  const clear = useViewerStore((s) => s.setAgentsWorkspaceFilter);
  useEffect(() => {
    if (fate?.fate === "drop") clear(null);
  }, [fate?.fate, clear]);
}

/** Whether the saved arrow filter in `s` can no longer show anything, reading no agents. */
export function shouldDropArrowFilter(
  s: Pick<
    ViewerState,
    | "agentsWorkspaceFilter"
    | "agentsPaneHiddenTeams"
    | "workspaces"
    | "softClosedWorkspaces"
    | "persistedStateRestored"
    | "terminalLayout"
  >,
  machines: Parameters<typeof machineFactsOf>[1],
): boolean {
  const filter = s.agentsWorkspaceFilter;
  if (!filter) return false;
  const record =
    s.workspaces[filter.teamKey]?.find((w) => w.id === filter.workspaceId) ??
    s.softClosedWorkspaces[filter.teamKey]?.[filter.workspaceId]?.workspace;
  return (
    arrowFilterFate({
      filter,
      record,
      teamHidden: s.agentsPaneHiddenTeams.includes(filter.teamKey),
      local: s.persistedStateRestored === true && s.terminalLayout.hydrated ? "done" : "loading",
      roster: null,
      remoteLoading: false,
      machineLoading: () => false,
      ...machineFactsOf(s.terminalLayout, machines, filter.teamKey),
      holdsAgent: null,
    }).fate === "drop"
  );
}
