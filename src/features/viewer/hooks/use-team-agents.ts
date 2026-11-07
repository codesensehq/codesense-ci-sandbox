// SPDX-License-Identifier: MIT
import { useAgentListNodes } from "@/features/viewer/lib/agent-list-projection";
import { useMemo } from "react";

import { useAgentStatusOf } from "@/features/viewer/hooks/use-agent-status-of";
import {
  countNodes,
  flattenRows,
  needsYouHandles,
  type AgentTreeNode,
} from "@/features/viewer/lib/agent-list-view";
import type { AgentStatus } from "@/shared/lib/agent-status-config";
import type { AgentSessionRow } from "@/tauri/commands";

/**
 * Each listed agent's status ({@link useAgentStatusOf}) and which of them wait on a person.
 */
export function useNeedsYou(nodes: readonly AgentTreeNode[]): {
  needs: ReadonlySet<string>;
  statusOf: (row: AgentSessionRow) => AgentStatus;
} {
  const rows = useMemo(() => flattenRows(nodes), [nodes]);
  const statusOf = useAgentStatusOf(rows);
  return useMemo(() => ({ needs: needsYouHandles(nodes, statusOf), statusOf }), [nodes, statusOf]);
}

/**
 * How many agents a team has running and how many of them wait on a person: the same crew
 * the agents list shows, counted for the control that opens it.
 */
export function useTeamAgentCounts(teamKey: string): { agents: number; needsYou: number } {
  const nodes = useAgentListNodes(teamKey);
  const { needs } = useNeedsYou(nodes);
  return { agents: countNodes(nodes), needsYou: needs.size };
}
