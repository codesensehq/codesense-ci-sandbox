// SPDX-License-Identifier: MIT
import { useEffect, useMemo } from "react";
import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";

import { userFacingErrorMessage } from "@/shared/lib/error-message";
import { logger } from "@/shared/lib/logger";
import { parseTeamKey } from "@/shared/lib/team-key";
import { createListenerGroup, type ListenerGroup } from "@/shared/lib/tauri-listeners";
import { SESSION_STATE_CHANGED_EVENT } from "@/features/viewer/lib/session-events";
import {
  AGENTS_CHANGED_EVENT,
  CONTEXT_INDEX_CHANGED_EVENT,
  type ContextIndexChangedPayload,
} from "@/tauri/events";
import { agentsList, type AgentRow } from "@/tauri/commands";

/** The id part of an agent key (`org/team/id` or `_personal/<uid>/personal/id`). */
function idOfKey(key: string): string {
  return key.slice(key.lastIndexOf("/") + 1);
}

/**
 * The agent a reference names, resolved the way a launch resolves it: a key as is (archived
 * or not), or a bare id in the given team, then in your personal agents, then among the
 * starters (`claude`, `codex`), skipping archived agents. With `personal: false` a bare id resolves only in the team (a
 * trigger falls back to its owner's personal agents, which you can't see, before the
 * starters). `null` when nothing matches (the caller shows the reference itself).
 */
export function findAgent(
  reference: string,
  rows: readonly AgentRow[],
  teamKey?: string | null,
  { personal = true }: { personal?: boolean } = {},
): AgentRow | null {
  const ref = reference.trim();
  if (!ref) return null;
  if (ref.includes("/")) return rows.find((r) => r.key === ref) ?? null;
  // A bare id never names an archived agent, as a launch skips them: the next place's agent.
  const named = rows.filter((r) => idOfKey(r.key) === ref && !r.archived);
  const { orgSlug, teamName } = parseTeamKey(teamKey ?? "");
  const inTeam = named.find((r) => r.scope === "team" && r.org === orgSlug && r.team === teamName);
  // Without your personal agents in view, the owner's personal agent may be the one a
  // launch finds before the starter, so nothing is guessed past the team.
  if (inTeam || !personal) return inTeam ?? null;
  return (
    named.find((r) => r.scope === "personal") ?? named.find((r) => r.scope === "builtin") ?? null
  );
}

/** The agents list as {@link useAgentRows} reads it. */
export interface AgentRowsState {
  /** `null` until the first read lands. */
  rows: AgentRow[] | null;
  /** The last read's failure, cleared by the next good read. */
  error: string | null;
}

/**
 * Every agent you can see, archived ones included so an older session or trigger still
 * shows a name: ONE list shared by every reader (the views below and the app-wide tab-title
 * follower). It is read again whenever a reader opens, whenever agents change here
 * (`agents:changed`), whenever synced content changes an agent definition (a teammate's
 * edit arriving by git, seen by the context index) and when sessions start or stop (the
 * running counts). A reply that a newer read has overtaken is dropped, and a failed read
 * keeps the last list. When the last reader stops, the list is forgotten.
 */
export const useAllAgentRowsStore = create<AgentRowsState>(() => ({ rows: null, error: null }));

let watchers = 0;
let listeners: ListenerGroup | null = null;
let latest = 0;
let refreshTimer: ReturnType<typeof setTimeout> | null = null;

/** How long a burst of changes is gathered into one read. */
const REFRESH_DEBOUNCE_MS = 150;

/** A pty exiting: an agent that ends on its own moves the running counts. */
const PTY_EXIT_EVENT = "pty-exit";

function loadAllAgentRows(): void {
  const request = ++latest;
  agentsList(true)
    .then((rows) => {
      if (request === latest) useAllAgentRowsStore.setState({ rows, error: null });
    })
    .catch((err: unknown) => {
      if (request !== latest) return;
      logger.warn("agents", `failed to list agents: ${String(err)}`);
      useAllAgentRowsStore.setState({ error: userFacingErrorMessage(err) });
    });
}

/** Read the list again, once per burst of changes. */
function scheduleRead(): void {
  if (refreshTimer !== null) return;
  refreshTimer = setTimeout(() => {
    refreshTimer = null;
    if (watchers > 0) loadAllAgentRows();
  }, REFRESH_DEBOUNCE_MS);
}

/** Whether a context index change touched an agent definition (`…/agents/<id>/…`). */
export function touchesAgents(change: ContextIndexChangedPayload): boolean {
  return change.truncated || change.paths.some((p) => /(^|\/)agents\//.test(p));
}

/** Keep {@link useAllAgentRowsStore} live while the returned stop is not called. Every new
 *  watcher reads the list afresh, so a view that opens never shows what it last saw. */
export function watchAllAgentRows(): () => void {
  watchers += 1;
  if (watchers === 1) {
    listeners = createListenerGroup();
    listeners.track(listen(AGENTS_CHANGED_EVENT, scheduleRead));
    listeners.track(
      listen<ContextIndexChangedPayload>(CONTEXT_INDEX_CHANGED_EVENT, (e) => {
        if (touchesAgents(e.payload)) scheduleRead();
      }),
    );
    listeners.track(listen(SESSION_STATE_CHANGED_EVENT, scheduleRead));
    listeners.track(listen(PTY_EXIT_EVENT, scheduleRead));
  }
  loadAllAgentRows();
  return () => {
    watchers -= 1;
    if (watchers > 0) return;
    listeners?.dispose();
    listeners = null;
    if (refreshTimer !== null) clearTimeout(refreshTimer);
    refreshTimer = null;
    latest += 1;
    useAllAgentRowsStore.setState({ rows: null, error: null });
  };
}

/** Every agent you can see, archived ones included, kept live while mounted. */
export function useAgentRows(): AgentRowsState {
  useEffect(() => watchAllAgentRows(), []);
  const rows = useAllAgentRowsStore((s) => s.rows);
  const error = useAllAgentRowsStore((s) => s.error);
  return useMemo(() => ({ rows, error }), [rows, error]);
}
