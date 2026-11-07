// SPDX-License-Identifier: MIT
import { createContext, useContext, useMemo, type ReactNode } from "react";

import { useAgentStatusOf, useAgentSubagentsOf } from "@/features/viewer/hooks/use-agent-status-of";
import {
  useTeamAgentIdentities,
  type TeamAgentIdentity,
} from "@/features/viewer/hooks/use-team-agent-identities";
import type { AgentStatus } from "@/shared/lib/agent-status-config";
import { markOfAgentStatus, type StatusMarkKind } from "@/shared/lib/status-mark";
import { agentStatusWords } from "@/shared/lib/terminal-dot-words";
import type { AgentSessionRow } from "@/tauri/commands";

/** The team's running agents as the notes pane shows them: who a line can be sent
 *  to, and the live status a handed-off line's chip shows. */
export interface NoteAgents {
  teamKey: string;
  agents: readonly TeamAgentIdentity[];
  byHandle: ReadonlyMap<string, TeamAgentIdentity>;
  statusOf: (row: AgentSessionRow) => AgentStatus;
  /** How many subagents each agent runs now, for its status words. */
  subagentsOf: (row: AgentSessionRow) => number;
}

const EMPTY: NoteAgents = {
  teamKey: "",
  agents: [],
  byHandle: new Map(),
  statusOf: () => "unknown",
  subagentsOf: () => 0,
};

const NoteAgentsContext = createContext<NoteAgents>(EMPTY);

/** Reads the team's agents once for the whole pane, so every chip and the send
 *  picker share one roster read and one status read. */
export function NoteAgentsProvider({
  teamKey,
  children,
}: {
  teamKey: string;
  children: ReactNode;
}): React.JSX.Element {
  const agents = useTeamAgentIdentities(teamKey);
  const rows = useMemo(() => agents.map((a) => a.row), [agents]);
  const statusOf = useAgentStatusOf(rows);
  const subagentsOf = useAgentSubagentsOf(rows);
  const value = useMemo<NoteAgents>(
    () => ({
      teamKey,
      agents,
      byHandle: new Map(agents.map((a) => [a.row.handle, a])),
      statusOf,
      subagentsOf,
    }),
    [teamKey, agents, statusOf, subagentsOf],
  );
  return <NoteAgentsContext.Provider value={value}>{children}</NoteAgentsContext.Provider>;
}

export function useNoteAgents(): NoteAgents {
  return useContext(NoteAgentsContext);
}

/** How an agent's status reads beside its name, in the shared words and mark. An
 *  agent that is no longer running reads as ended. */
export function agentStatusLook(
  status: AgentStatus | null,
  subagents?: number,
): {
  mark: StatusMarkKind;
  word: string;
} {
  if (!status) return { mark: "ended", word: "not running" };
  return {
    mark: markOfAgentStatus(status),
    word: agentStatusWords(status, subagents).toLowerCase(),
  };
}
