// SPDX-License-Identifier: MIT
import { showAgent } from "./show-agent";
import { isRunningAgent } from "./hand-agents";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { openAgentInboxRail } from "../stores/agent-inbox-rail-store";
import type { AgentSessionRow } from "@/tauri/commands";

export async function openAgentInboxForRow(
  row: AgentSessionRow,
  reveal: (row: AgentSessionRow) => unknown = showAgent,
  opts?: { compose?: boolean },
): Promise<boolean> {
  if (!row.alive || !row.sessionId || !isRunningAgent(useAgentStateStore.getState(), row.sessionId))
    return false;
  await reveal(row);
  openAgentInboxRail(row.sessionId, opts);
  return true;
}
