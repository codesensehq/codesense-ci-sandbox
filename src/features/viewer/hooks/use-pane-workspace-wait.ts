// SPDX-License-Identifier: MIT
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useMachinesStore } from "@/features/machines/stores/machines-store";
import {
  machineInTeams,
  machineOfflineWord,
  UNKNOWN_MACHINE_LABEL,
} from "@/features/machines/lib/machine-offline";
import { arrowFilterFate, machineFactsOf } from "../lib/arrow-filter-fate";
import { useAgentListProjectionStore } from "../lib/agent-list-projection";
import { useRemoteLayoutsLoading, useWaitOver } from "./use-agent-layout-loading";
import { NO_WORKSPACE_ID } from "@/shared/lib/workspace-key";
import { REMOTE_LAYOUT_WAIT_MS } from "@/shared/lib/terminal-layout-projection";
import { workspaceDisplayLabel } from "@/shared/lib/worktree/resolve";

/** The roster-aware filter fate shared by native display and app-root cleanup. */
export function useWorkspaceFilterFate() {
  const filter = useViewerStore((s) => s.agentsWorkspaceFilter);
  const workspaces = useViewerStore((s) => s.workspaces);
  const closed = useViewerStore((s) => s.softClosedWorkspaces);
  const hidden = useViewerStore((s) => s.agentsPaneHiddenTeams);
  const layout = useViewerStore((s) => s.terminalLayout);
  const restored = useViewerStore((s) => s.persistedStateRestored);
  const projection = useAgentListProjectionStore();
  const machines = useMachinesStore();
  const waitOver = useWaitOver(REMOTE_LAYOUT_WAIT_MS);
  const remote = useRemoteLayoutsLoading();

  const record = filter
    ? (workspaces[filter.teamKey]?.find((w) => w.id === filter.workspaceId) ??
      closed[filter.teamKey]?.[filter.workspaceId]?.workspace)
    : undefined;
  const fate = filter
    ? arrowFilterFate({
        filter,
        record,
        teamHidden: hidden.includes(filter.teamKey),
        local: restored && layout.hydrated ? "done" : waitOver ? "stalled" : "loading",
        roster:
          projection.sessions !== null && projection.error === null && !projection.partial
            ? "done"
            : projection.sessions === null && projection.error === null && !waitOver
              ? "loading"
              : "stalled",
        remoteLoading: remote.loading,
        machineLoading: remote.loadingFor,
        ...machineFactsOf(layout, machines, filter.teamKey),
        holdsAgent: projection.references.some(
          (r) =>
            r.teamKey === filter.teamKey &&
            (r.workspaceRef ?? NO_WORKSPACE_ID) === filter.workspaceId,
        ),
      })
    : null;
  return { filter, record, fate, machines };
}

/** Present the shared filter fate for the HTML pane; cleanup belongs to the app root. */
export function usePaneWorkspaceWait() {
  const { filter, record, fate, machines } = useWorkspaceFilterFate();
  if (!filter || !fate) return { workspaceTeam: null, workspaceWait: null, workspaceLabel: null };
  let workspaceWait = null;
  if (fate.fate === "loading") {
    const bucket = filter.workspaceId === NO_WORKSPACE_ID;
    workspaceWait = {
      title: bucket ? "Loading agents" : "Loading this workspace",
      sentence: `${bucket ? "They show" : "Its agents show"} once they have loaded.`,
      busy: true,
    };
  } else if (fate.fate === "waiting") {
    const machine = machineInTeams(machines.byTeam, fate.machineId, filter.teamKey);
    const name = machine?.label ?? UNKNOWN_MACHINE_LABEL;
    const offline = !!machine?.status && machineOfflineWord(machine) !== null;
    workspaceWait = {
      title: `Waiting for ${name}`,
      sentence: offline
        ? `This workspace runs on ${name}, which is offline. Its agents show once it reconnects.`
        : `This workspace runs on ${name}. Its agents show once its terminals can be read.`,
      busy: false,
    };
  }
  return {
    workspaceTeam: filter.teamKey,
    workspaceWait,
    workspaceLabel:
      filter.workspaceId === NO_WORKSPACE_ID
        ? "No workspace"
        : record
          ? (record.kind === "story" && record.storyId) ||
            record.storyTitle ||
            workspaceDisplayLabel(record)
          : "Workspace",
  };
}
