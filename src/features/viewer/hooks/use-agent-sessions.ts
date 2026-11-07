// SPDX-License-Identifier: MIT
import {
  type SessionsUpdater,
  useAgentListProjectionStore,
} from "@/features/viewer/lib/agent-list-projection";
import { type AgentSessionRow } from "@/features/viewer/lib/cli-workspace/transport/terminal-launch-source";

export interface UseAgentSessions {
  /** `null` = not yet loaded (distinct from an empty array = "no live sessions"). */
  sessions: AgentSessionRow[] | null;
  /** The last fetch error, or `null`. */
  error: string | null;
  /** Re-enumerate the roster. Gen-guarded; resolves once the fetch settles. */
  reload: () => Promise<void>;
  /** Direct setter for optimistic mutations (e.g. prune a killed row). */
  setSessions: (next: SessionsUpdater) => void;
}

/**
 * SW-AM-9: read the app-root agent roster — the single source of truth held in
 * {@link useAgentListProjectionStore}. This is now a thin selector: the fetch, the
 * gen-guard, and the `pty-exit`/`session-state-changed` subscription all live once
 * in {@link useAgentRosterListener} (mounted at the app root), so every consumer —
 * the Agents panel, `board-run-controls`, `automations-panel`, `pipeline-viewer` —
 * observes the SAME array reference and can never drift.
 *
 * Selecting the four members individually (each a stable primitive/reference)
 * rather than returning a fresh object from one selector avoids Zustand's
 * fresh-object-identity re-render loop; the assembled return object is consumed by
 * destructuring, never fed back into a selector.
 */
export function useAgentSessions(): UseAgentSessions {
  const sessions = useAgentListProjectionStore((s) => s.sessions);
  const error = useAgentListProjectionStore((s) => s.error);
  const reload = useAgentListProjectionStore((s) => s.reload);
  const setSessions = useAgentListProjectionStore((s) => s.setSessions);
  return { sessions, error, reload, setSessions };
}
