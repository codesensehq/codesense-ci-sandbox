// SPDX-License-Identifier: MIT
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";

import { useInboxSessions } from "./use-inbox-sessions";

/** Messages to a tab's session it has not read yet (0 when unknown). */
export function useTabInboxCounts(sessionId: string): { direct: number; group: number } {
  const handle = useAgentListProjectionStore(
    (s) => s.sessions?.find((r) => r.sessionId === sessionId)?.handle ?? sessionId,
  );
  const sessions = useInboxSessions();
  const row = sessions?.find((r) => r.handle === handle);
  return { direct: row?.unread ?? 0, group: row?.groupUnread ?? 0 };
}

export function useTabInboxUnread(sessionId: string): number {
  const { direct, group } = useTabInboxCounts(sessionId);
  return direct + group;
}
