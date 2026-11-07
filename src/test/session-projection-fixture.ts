// SPDX-License-Identifier: MIT
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import type { AgentStatus } from "@/shared/lib/agent-status-config";
import type { AgentSessionRow } from "@/tauri/commands";

/** Supply the Rust-owned part of a legacy terminal fixture; never derive it from local signals. */
export function projectSessionStatus(
  sessionId: string,
  effectiveStatus: AgentStatus,
  fields: Partial<AgentSessionRow> = {},
) {
  const sessions = useAgentListProjectionStore.getState().sessions ?? [];
  const old = sessions.find((row) => row.sessionId === sessionId);
  const row = {
    id: sessionId,
    sessionId,
    handle: sessionId,
    kind: "dashboard-launched",
    alive: true,
    backgrounded: false,
    startedWith: null,
    startedAtMs: null,
    launcher: null,
    lastOutputMs: null,
    ...old,
    effectiveStatus,
    ...fields,
  } satisfies AgentSessionRow;
  useAgentListProjectionStore.setState({
    sessions: old ? sessions.map((before) => (before === old ? row : before)) : [...sessions, row],
  });
  return row;
}

/** Start each scenario without status or placement left by a previous test. */
export function resetSessionProjection() {
  useAgentListProjectionStore.setState(useAgentListProjectionStore.getInitialState());
}
