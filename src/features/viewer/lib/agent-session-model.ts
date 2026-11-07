// SPDX-License-Identifier: MIT
import type { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { agentModelLabel } from "./agent-list-view";

type ModelSources = Pick<
  ReturnType<typeof useAgentStateStore.getState>,
  "launchBySession" | "bySession"
>;

/** The list and its host mirror read the same launch arguments and detected CLI. */
export function agentSessionModel(
  sources: ModelSources,
  sessionId?: string | null,
  detectedCli?: string | null,
) {
  const launch = sessionId ? sources.launchBySession[sessionId] : undefined;
  return agentModelLabel({
    cli: launch?.agent ?? (sessionId ? sources.bySession[sessionId]?.agent : null) ?? detectedCli,
    args: launch?.spec?.args,
  });
}
