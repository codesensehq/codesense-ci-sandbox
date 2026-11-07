// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useRef, useState } from "react";

import { listen } from "@tauri-apps/api/event";

import type { AgentUpdate, InboxTeam } from "@/tauri/commands";
import { listAgentUpdates } from "@/tauri/commands";
import { logger } from "@/shared/lib/logger";

/**
 * SW-BSI-UX — reads the Inbox's Updates lane: agent → UI progress notifications
 * persisted in `wv_agent_updates`. Refetches when the live `mcp:agent-message`
 * event fires (so a landing emit shows up without a reload). Fire-and-forget; a
 * failed load keeps the previous rows + records an error.
 */
export interface AgentUpdatesHandle {
  updates: AgentUpdate[];
  loading: boolean;
  error: string | null;
  reload: () => Promise<void>;
}

const AGENT_MESSAGE_EVENT = "mcp:agent-message";
const DEFAULT_LIMIT = 50;

export function useAgentUpdates(
  team: InboxTeam | null,
  limit: number = DEFAULT_LIMIT,
): AgentUpdatesHandle {
  const [updates, setUpdates] = useState<AgentUpdate[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const gen = useRef(0);

  const reload = useCallback(async () => {
    if (!team) {
      setUpdates([]);
      setError(null);
      return;
    }
    const my = ++gen.current;
    setLoading(true);
    setError(null);
    try {
      const rows = await listAgentUpdates(team, limit);
      if (my !== gen.current) return;
      setUpdates(rows);
      setLoading(false);
    } catch (err) {
      if (my !== gen.current) return;
      const message = err instanceof Error ? err.message : String(err);
      logger.info("inbox", `Agent updates read failed: ${message}`);
      setError(message);
      setLoading(false);
    }
  }, [team, limit]);

  useEffect(() => {
    void reload();
    const unlisten = listen(AGENT_MESSAGE_EVENT, () => {
      void reload();
    });
    return () => {
      void unlisten.then((u) => u()).catch(() => {});
    };
  }, [reload]);

  return { updates, loading, error, reload };
}
