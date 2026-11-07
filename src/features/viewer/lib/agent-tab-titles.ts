// SPDX-License-Identifier: MIT
import { useEffect } from "react";

import { useAllAgentRowsStore, watchAllAgentRows } from "@/features/agents/lib/agent-names";
import { agentKeyOfRevision } from "@/features/viewer/lib/agent-rows";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import {
  selectMinimizedTerminalTabIndex,
  selectRunningTerminalTabIndex,
  useViewerStore,
  type TerminalTabIndex,
} from "@/shared/stores/viewer-store";
import type { AgentRow, AgentSessionRow } from "@/tauri/commands";

/** One open tab to retitle, and its new title. */
export interface TabRetitle {
  teamKey: string;
  workspaceId: string;
  tabId: string;
  name: string;
}

/** The title a session's tab gets when its launch names none: its agent's display name. */
function agentTitle(
  row: Pick<AgentSessionRow, "agentRevision">,
  rows: readonly AgentRow[],
): string | null {
  const key = agentKeyOfRevision(row.agentRevision?.trim());
  if (!key) return null;
  return rows.find((r) => r.key === key)?.name.trim() || null;
}

/**
 * The open agent tabs, of every team, local and remote, to retitle when agents change from
 * `before` to `after`: those whose agent's title changed and that still carry the old one. A
 * tab someone titled themselves keeps its title.
 */
export function agentTabRetitles(
  tabIndex: TerminalTabIndex,
  sessions: readonly Pick<AgentSessionRow, "sessionId" | "agentRevision">[],
  before: readonly AgentRow[],
  after: readonly AgentRow[],
): TabRetitle[] {
  const bySession = new Map(sessions.flatMap((r) => (r.sessionId ? [[r.sessionId, r]] : [])));
  const out: TabRetitle[] = [];
  for (const [teamKey, workspaces] of Object.entries(tabIndex)) {
    for (const [workspaceId, tabs] of Object.entries(workspaces)) {
      for (const tab of tabs) {
        const row = bySession.get(tab.sessionId);
        if (!row) continue;
        const from = agentTitle(row, before);
        const to = agentTitle(row, after);
        if (from && to && from !== to && tab.name === from) {
          out.push({ teamKey, workspaceId, tabId: tab.id, name: to });
        }
      }
    }
  }
  return out;
}

/** What {@link followAgentTabTitles} reads and does. */
export interface AgentTabTitleDeps {
  /** Keep the shared agents list live; returns the stop. */
  watch: () => () => void;
  /** Call `onRows` with each new agents list and the one before it; returns the stop. */
  onRows: (onRows: (rows: AgentRow[], before: AgentRow[] | null) => void) => () => void;
  /** Every running tab: the open workspaces' (Hidden ones included) and the minimized ones'. */
  tabIndexes: () => TerminalTabIndex[];
  sessions: () => readonly AgentSessionRow[];
  rename: (retitle: TabRetitle) => void;
}

const realDeps: AgentTabTitleDeps = {
  watch: watchAllAgentRows,
  onRows: (onRows) =>
    useAllAgentRowsStore.subscribe((state, prev) => {
      if (state.rows && state.rows !== prev.rows) onRows(state.rows, prev.rows);
    }),
  tabIndexes: () => {
    const state = useViewerStore.getState();
    return [selectRunningTerminalTabIndex(state), selectMinimizedTerminalTabIndex(state)];
  },
  sessions: () => useAgentListProjectionStore.getState().sessions ?? [],
  rename: (t) =>
    useViewerStore.getState().renameTerminalTab(t.teamKey, t.tabId, t.name, t.workspaceId, true),
};

/**
 * Keep agent tabs titled with their agents' names for the whole app: on every new agents list
 * (the one list every agents view shares), retitle the running tabs — any team's, open or
 * minimized, local or remote — whose agent was renamed, from the
 * list before it. Returns the stop.
 */
export function followAgentTabTitles(deps: AgentTabTitleDeps = realDeps): () => void {
  const stopRows = deps.onRows((rows, before) => {
    if (!before) return;
    const seen = new Set<string>();
    for (const index of deps.tabIndexes()) {
      for (const t of agentTabRetitles(index, deps.sessions(), before, rows)) {
        if (seen.has(t.tabId)) continue;
        seen.add(t.tabId);
        deps.rename(t);
      }
    }
  });
  const stopWatch = deps.watch();
  return () => {
    stopWatch();
    stopRows();
  };
}

/** Mounted once at the app root: {@link followAgentTabTitles} while the app runs. */
export function useAgentTabTitles(): void {
  useEffect(() => followAgentTabTitles(), []);
}
