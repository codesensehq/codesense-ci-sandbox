// SPDX-License-Identifier: MIT
import { useMemo } from "react";

import { useInboxProjection } from "@/features/inbox/hooks/use-inbox-projection";
import { useInboxSessions } from "@/features/inbox/hooks/use-inbox-sessions";
import { useTeamAgentIdentities } from "@/features/viewer/hooks/use-team-agent-identities";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { useActiveTeamKey } from "@/shared/lib/team-key";
import type { SessionListRow } from "@/tauri/commands";

import {
  instancesByAgent,
  mergeInstanceSessions,
  needsMailBySender,
  startedAtByHandle,
  type RunningInstance,
} from "./agent-running";

const NONE: readonly never[] = [];
const NO_INSTANCES: readonly RunningInstance[] = [];

/**
 * Every saved agent's live sessions, by agent key: the shared session roster plus the sessions in
 * the active team the roster has not caught up with yet (read from the team's running agents),
 * with the shared inbox's unanswered needs-you mail folded into each one's waiting state and
 * start times read from every team's projected session rows.
 */
export function useAgentInstanceIndex(): ReadonlyMap<string, readonly RunningInstance[]> {
  const sessions = useInboxSessions();
  const counts = useInboxProjection();
  const teamKey = useActiveTeamKey();
  const identities = useTeamAgentIdentities(teamKey);
  const projected = useAgentListProjectionStore((s) => s.sessions);
  return useMemo(() => {
    const known: SessionListRow[] = [];
    for (const i of identities) {
      if (!i.agentKey) continue;
      known.push({
        handle: i.row.handle,
        displayName: i.label,
        state: i.row.alive ? "running" : "exited",
        launchedBy: i.row.launchedBy ?? null,
        unread: null,
        oldestUnreadAt: null,
        agent: i.agentKey,
        ...(i.row.story ? { story: i.row.story } : {}),
      });
    }
    return instancesByAgent({
      sessions: mergeInstanceSessions(sessions ?? NONE, known),
      startedAt: {
        ...startedAtByHandle(identities.map((i) => i.row)),
        ...startedAtByHandle(projected ?? NONE),
      },
      needsBySender: needsMailBySender(counts),
    });
  }, [sessions, counts, identities, projected]);
}

/** The live sessions of saved agent `agentKey`. */
export function useAgentInstances(agentKey: string | null): readonly RunningInstance[] {
  const index = useAgentInstanceIndex();
  return (agentKey ? index.get(agentKey) : undefined) ?? NO_INSTANCES;
}
