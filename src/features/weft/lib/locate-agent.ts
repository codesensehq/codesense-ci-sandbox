// SPDX-License-Identifier: MIT
import { toast } from "sonner";

import { showAgent, type ShowOutcome, type ShowOptions } from "@/features/viewer/lib/show-agent";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import type { AgentSessionRow } from "@/tauri/commands";

/**
 * Resolve an exact session id first, then a session handle, then a bare definition id
 * (the last path segment before `@revision`). Handle and definition lookups prefer
 * the newest live session; a session id keeps the exact instance across teams.
 */
function findSessionForAgentId(agentId: string): AgentSessionRow | null {
  const rows = useAgentListProjectionStore.getState().sessions ?? [];
  const bySession = rows.find((r) => r.sessionId === agentId);
  if (bySession) return bySession;
  const byHandle = rows.filter((r) => r.handle === agentId);
  if (byHandle.length) {
    const live = byHandle
      .filter((r) => r.alive)
      .sort((a, b) => (b.startedAtMs ?? 0) - (a.startedAtMs ?? 0));
    return live[0] ?? byHandle[0] ?? null;
  }
  const live = rows.filter((r) => r.alive);
  const match = (rev: string | null | undefined): boolean => {
    if (!rev) return false;
    const key = rev.replace(/@[^@]*$/, "");
    const last = key.lastIndexOf("/");
    const bare = last === -1 ? key : key.slice(last + 1);
    return bare === agentId;
  };
  const live_match = live
    .filter((r) => match(r.agentRevision))
    .sort((a, b) => (b.startedAtMs ?? 0) - (a.startedAtMs ?? 0));
  if (live_match[0]) return live_match[0];
  const any = rows
    .filter((r) => match(r.agentRevision))
    .sort((a, b) => (b.startedAtMs ?? 0) - (a.startedAtMs ?? 0));
  return any[0] ?? null;
}

/**
 * Show an agent by its session id, definition id or handle: the same Show the Agents panel's row click runs
 * ({@link showAgent}). Shared by the Inbox's open-sender and the pane-SDK `gotoAgent` verb, so
 * both land in the exact same place: its tab (switching team if it is in another), restored or
 * adopted into a tab, its run output, or a stated reason. `located` is true when something was
 * shown.
 */
export async function locateAgentByAgentId(
  agentId: string,
  options?: Pick<ShowOptions, "preserveFocus">,
): Promise<{ located: boolean; outcome: ShowOutcome }> {
  const row = findSessionForAgentId(agentId);
  if (!row) {
    const reason = `No agent "${agentId}" is running.`;
    toast(reason);
    return { located: false, outcome: { notShown: reason } };
  }
  const outcome = await showAgent(row, options);
  return { located: "shown" in outcome, outcome };
}

// re-export for tests that want to assert the row-picking rule
export const __test_findSessionForAgentId = findSessionForAgentId;
