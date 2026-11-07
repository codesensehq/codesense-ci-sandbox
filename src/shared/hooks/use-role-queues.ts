// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { agentReadRoles, type RoleQueue } from "@/tauri/commands";

/** The existing role read model: inspection only, scoped to the queue's own team. */
export function useRoleQueues(teamKey: string) {
  const [state, setState] = useState<{
    teamKey: string;
    queues: RoleQueue[];
    error: string | null;
  }>({ teamKey, queues: [], error: null });
  useEffect(() => {
    let current = true;
    const [org, team] = teamKey.split(":");
    if (!org || !team || team === "_none") return;
    const load = async () => {
      try {
        const result = await agentReadRoles({ org, team }, undefined, 1);
        if (current) setState({ teamKey, queues: result.roles, error: null });
      } catch (e) {
        if (current)
          setState((s) => ({
            teamKey,
            queues: s.teamKey === teamKey ? s.queues : [],
            error: String(e),
          }));
      }
    };
    void load();
    const stop = listen("agent-state-changed", () => void load()).catch(() => () => {});
    const timer = setInterval(() => void load(), 20_000);
    return () => {
      current = false;
      clearInterval(timer);
      void stop.then((f) => f());
    };
  }, [teamKey]);
  return state.teamKey === teamKey ? state : { teamKey, queues: [], error: null };
}
