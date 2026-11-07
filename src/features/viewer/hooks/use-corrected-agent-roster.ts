// SPDX-License-Identifier: MIT
import { useMemo } from "react";

import { type SessionsUpdater } from "@/features/viewer/lib/agent-list-projection";
import { useAgentSessions } from "@/features/viewer/hooks/use-agent-sessions";
import { findTabLocationBySessionId, type TerminalTabIndex } from "@/shared/stores/viewer-store";
import { useTerminalTabIndex } from "@/features/viewer/lib/cli-workspace/store/terminal-selectors";
import { type AgentSessionRow } from "@/tauri/commands";

// SW-AM-9: the "attended" rule, defined ONCE — a backgrounded agent that already
// has a live FOREGROUND tab is being watched, so it isn't unattended. Shared by the
// pure presentation memo (present it as foreground) and the slot-release effect
// (foreground it server-side) so the rule can't drift between the two.
export function isAttendedBackgrounded(r: AgentSessionRow, tabs: TerminalTabIndex): boolean {
  return Boolean(r.backgrounded && r.sessionId && findTabLocationBySessionId(tabs, r.sessionId));
}

export interface CorrectedAgentRoster {
  /** The roster with attended-but-backgrounded agents corrected to foreground. */
  sessions: AgentSessionRow[] | null;
  error: string | null;
  reload: () => Promise<void>;
  setSessions: (next: SessionsUpdater) => void;
}

/**
 * The corrected agent roster the agents list rows read: the attended-agent correction is
 * applied here once, so every row sees the same roster.
 *
 * PURE by construction: presenting an attended agent as foreground is a memo here;
 * the SIDE EFFECT that releases its slot server-side is the app-root
 * `useAttendedAgentRelease` (so it fires once, not once per consumer of this hook).
 */
export function useCorrectedAgentRoster(): CorrectedAgentRoster {
  const { sessions: rawSessions, error, reload, setSessions } = useAgentSessions();
  const terminalTabIndex = useTerminalTabIndex();

  const sessions = useMemo(() => {
    if (!rawSessions) return rawSessions; // preserve null (not-loaded) / [] (empty)
    return rawSessions.map((r) =>
      isAttendedBackgrounded(r, terminalTabIndex) ? { ...r, backgrounded: false } : r,
    );
  }, [rawSessions, terminalTabIndex]);

  return {
    sessions,
    error,
    reload,
    setSessions,
  };
}
