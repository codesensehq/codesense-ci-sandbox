// SPDX-License-Identifier: MIT
import { HAND_STARTED_KIND } from "./hand-agents";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { dashboardKillByHandle, type AgentSessionRow } from "@/tauri/commands";

export function endAgentSession(
  row: Pick<AgentSessionRow, "handle"> & Partial<Pick<AgentSessionRow, "kind" | "sessionId">>,
) {
  return row.kind === HAND_STARTED_KIND && row.sessionId
    ? useViewerStore.getState().removeTerminalTabBySession(row.sessionId, { kill: true })
    : dashboardKillByHandle(row.handle);
}
