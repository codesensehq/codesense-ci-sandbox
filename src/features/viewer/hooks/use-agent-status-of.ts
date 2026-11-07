// SPDX-License-Identifier: MIT
import { useMemo } from "react";
import type { AgentStatus } from "@/shared/lib/agent-status-config";
import type { AgentSessionRow } from "@/tauri/commands";

/** Status is owned by the sessions projection for every surface. */
export const projectedAgentStatus = (row: AgentSessionRow): AgentStatus =>
  row.effectiveStatus ?? "unknown";

export function useAgentStatusOf(
  _rows: readonly AgentSessionRow[],
): (row: AgentSessionRow) => AgentStatus {
  return projectedAgentStatus;
}
export function getAgentStatusOf(
  _rows: readonly AgentSessionRow[],
): (row: AgentSessionRow) => AgentStatus {
  return projectedAgentStatus;
}
export function useAgentSubagentsOf(
  _rows: readonly AgentSessionRow[],
): (row: AgentSessionRow) => number {
  return useMemo(() => (row: AgentSessionRow) => row.subagents ?? 0, []);
}
