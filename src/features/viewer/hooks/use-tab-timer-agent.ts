// SPDX-License-Identifier: MIT
import { useMemo } from "react";

import { useAgentsStore } from "@/features/agents/stores/agents-store";
import { agentKeyOfRevision } from "@/features/viewer/lib/agent-rows";
import { agentRowLabel, isHeadlessSession } from "@/features/viewer/lib/agent-row-presentation";
import { isAgentSession, type ListedAgentRow } from "@/features/viewer/lib/hand-agents";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { AGENT_RUNTIMES } from "@/shared/lib/agent-runtimes";
import type { AgentRuntime } from "@/tauri/commands";

/** The agent a tab runs, as its timers need it (SW-AGR-6). */
export interface TabTimerAgent {
  /** What to call it ("Remind Barb"). */
  name: string;
  /** The agent it was launched as (`<org>/<team>/<id>`, `builtin/claude`), or null. */
  agentKey: string | null;
  /** A saved agent: not a built-in starter and not a hand-started CLI. */
  saved: boolean;
  runtime: AgentRuntime;
}

const STARTER_PREFIX = "builtin/";

/** The runtime a starter key or an agent CLI names; Claude unless it names another CLI. */
function runtimeOf(agentKey: string | null, cli: string | null | undefined): AgentRuntime {
  const named = agentKey?.startsWith(STARTER_PREFIX) ? agentKey.slice(STARTER_PREFIX.length) : cli;
  return AGENT_RUNTIMES.find((runtime) => named?.startsWith(runtime)) ?? "claude";
}

/**
 * Every interactive agent session, saved or unnamed, local or remote, by
 * session id — the tabs that get timers, "Remind this agent…" and "Keep
 * this…". Plain shells and headless runs are not in it.
 */
export function useTabTimerAgents(): ReadonlyMap<string, TabTimerAgent> {
  const sessions = useAgentListProjectionStore((s) => s.sessions);
  const agents = useAgentsStore((s) => s.rows);
  return useMemo(() => {
    const out = new Map<string, TabTimerAgent>();
    for (const r of sessions ?? []) {
      const row = r as ListedAgentRow;
      if (!row.sessionId || !isAgentSession(row) || isHeadlessSession(row)) continue;
      const agentKey = agentKeyOfRevision(row.agentRevision);
      const saved = agentKey !== null && !agentKey.startsWith(STARTER_PREFIX);
      const definition = saved ? agents?.find((a) => a.key === agentKey) : undefined;
      out.set(row.sessionId, {
        name: definition?.name ?? agentRowLabel(row),
        agentKey,
        saved,
        runtime: definition?.runtime ?? runtimeOf(agentKey, row.detected?.cli),
      });
    }
    return out;
  }, [sessions, agents]);
}

/** The interactive agent `sessionId` runs, or null (a plain shell, a headless run). */
export function useTabTimerAgent(sessionId: string | undefined): TabTimerAgent | null {
  const agents = useTabTimerAgents();
  return sessionId ? (agents.get(sessionId) ?? null) : null;
}
