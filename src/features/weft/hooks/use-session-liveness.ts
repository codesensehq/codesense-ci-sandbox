// SPDX-License-Identifier: MIT
import { useAgentStateStore, selectSessionExit } from "@/shared/stores/agent-state-store";
import { useMemo } from "react";

import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { isTerminalManagedState } from "@/shared/lib/managed-session";
import {
  selectManagedState,
  useManagedSessionStore,
} from "@/shared/stores/use-managed-session-store";

/**
 * SW-AV-8 — the liveness picture for one opaque `liveSessionId`, joined from the two
 * app-root sources of truth: the SW-AV-1 union roster (`dashboard_list_sessions` real
 * liveness) and the managed-session store (the run's task state). A Weft Activity row /
 * fleet pill consults this to decide whether an action targets a still-running agent or a
 * DEAD / exited / crashed one — so a Go / Ratify / Steer / Answer into a gone session
 * routes to recovery instead of silently no-op'ing (never a dead-end click).
 */
export interface SessionLiveness {
  /** Whether we have a settled liveness read at all — the roster snapshot has loaded
   *  (`sessions != null`, incl. the empty `[]`) OR the session carries a managed state.
   *  A pre-load flicker (neither settled) reports `loaded: false` so a row never falsely
   *  reads "no longer running" before the truth lands. */
  loaded: boolean;
  /** The agent is still running — its roster row reports a live child OR its managed task
   *  state is non-terminal (a PTY-less headless run that is `running`/`awaiting-answer`
   *  reads alive even though it has no live process the roster can see). A crashed run
   *  (`failed`/`needs-attention`) is a TERMINAL managed state → not alive → dead-eligible. */
  alive: boolean;
  /** The run id for the agent's last output / run detail (`openAgentResult`), recovered
   *  from the roster row's provenance stamp when present; `null` when unknown. */
  runId: string | null;
  /** The run's story id (threads the run panel's Evidence key), or `null`. */
  storyId: string | null;
}

/**
 * SW-AV-8 — resolve {@link SessionLiveness} for a `liveSessionId` (or `null`). Reads the
 * two shared stores by selector (the app-root listener owns the fetch — this never
 * triggers one), so every consumer observes the SAME snapshot and can't drift.
 */
export function useSessionLiveness(sessionId: string | null): SessionLiveness {
  const exit = useAgentStateStore(selectSessionExit(sessionId ?? ""));
  const sessions = useAgentListProjectionStore((s) => s.sessions);
  const managedState = useManagedSessionStore(selectManagedState(sessionId ?? ""));
  return useMemo(() => {
    // A managed state (even a terminal one) is itself a settled liveness read.
    const loaded = sessions != null || managedState != null;
    if (!sessionId) return { loaded, alive: false, runId: null, storyId: null };
    const rosterRow = sessions?.find((r) => r.sessionId === sessionId);
    const managedAlive = managedState != null && !isTerminalManagedState(managedState);
    const alive = exit === undefined && (!!rosterRow?.alive || managedAlive);
    return {
      loaded,
      alive,
      runId: rosterRow?.provenance?.runId ?? null,
      storyId: rosterRow?.provenance?.storyId ?? null,
    };
  }, [sessions, managedState, sessionId, exit]);
}
