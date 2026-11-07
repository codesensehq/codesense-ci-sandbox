// SPDX-License-Identifier: MIT
import { useOwnsScope, useOwningScope } from "@/shared/lib/warm-scopes";

import { useAgentsStore, type AgentRequest } from "../stores/agents-store";

/**
 * The pending one-shot request of `kind` for agent `agentKey`, for the component that acts on it,
 * or null. A request is addressed to one team's Agents tab (the team on screen when it was made)
 * and one agent, and only that team's tab acts on it, and only while it is on screen: a retained
 * hidden team never sees it. The component that acts calls the returned `consume`.
 */
export function useAgentRequest<K extends AgentRequest["kind"]>(
  kind: K,
  agentKey: string | null,
): { request: Extract<AgentRequest, { kind: K }>; consume: () => void } | null {
  const request = useAgentsStore((s) => s.request);
  const scope = useOwningScope();
  const owns = useOwnsScope();
  if (!request || request.kind !== kind || request.agentKey !== agentKey || !owns) return null;
  if (request.scope !== null && scope !== null && request.scope !== scope) return null;
  return {
    request: request as Extract<AgentRequest, { kind: K }>,
    consume: () => useAgentsStore.getState().consumeRequest(request),
  };
}
