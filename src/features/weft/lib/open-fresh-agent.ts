// SPDX-License-Identifier: MIT
import { railKeyOf, trayWorkspaceOrder } from "@/features/viewer/lib/group-workspaces-by-epic";
import { useTeamSurfaceStore } from "@/features/viewer/lib/team-surface-store";
import { focusActiveWorkspaceTerminal } from "@/features/viewer/lib/focus-cascade";
import { useTerminalGhostHintStore } from "@/features/viewer/stores/terminal-ghost-hint-store";
import { activeWorkspaceIdForTeam } from "@/shared/lib/team-key";
import {
  createTerminalTab,
  genericWorkspaceId,
  normalizeWorkspaceDir,
  useViewerStore,
} from "@/shared/stores/viewer-store";

/** The ghosted placeholder a fresh agent terminal opens with (§8 rule 11) — never a blank
 *  prompt. Constant copy: "Tell the agent what to do…". */
export const OPEN_AGENT_GHOST_HINT = "Tell the agent what to do…";

/**
 * SLW-TL-7 — the Inbox's "open an agent" gesture: land the human in a FRESH agent terminal
 * where they state the brief in their own words. New work is a terminal, not a form — no
 * dispatch brief, no gate. The fresh terminal opens with a ghosted placeholder hint so the
 * empty prompt reads as an invitation (§8 rule 11). Reuses the shipped terminal open path
 * (`createTerminalTab` + `addTerminalTab`), then reveals + focuses the terminal so a
 * keyboard user lands in it. Returns the new session id.
 */
export function openFreshAgentTerminal(
  teamKey: string,
  hint: string = OPEN_AGENT_GHOST_HINT,
): string {
  const store = useViewerStore.getState();
  const tab = createTerminalTab("Agent");
  if (hint) useTerminalGhostHintStore.getState().set(tab.sessionId, hint);
  void store.addTerminalTab(teamKey, tab);
  store.expandTerminalOnOpen(teamKey);
  focusActiveWorkspaceTerminal(teamKey);
  return tab.sessionId;
}

/**
 * The empty CLI's "Start an agent": a fresh agent terminal in `teamKey`, in its focused
 * workspace when it has one open, else in a tile for the team's own folder `teamDir` (opened
 * with no shell of its own, so the agent's is its only tab). Returns the new session id, or null
 * when the team has neither a workspace nor a folder to open one in.
 */
export function startAgentInTeam(teamKey: string, teamDir: string): string | null {
  const store = useViewerStore.getState();
  const focused = activeWorkspaceIdForTeam(teamKey, store.activeWorkspace, store.workspaces);
  const minimized = store.softClosedWorkspaces[teamKey]?.[focused];
  const existing = store.workspaces[teamKey]?.find((w) => w.id === focused) ?? minimized?.workspace;
  if (minimized) store.reopenSoftClosedWorkspace(teamKey, focused);
  if (!existing) {
    if (!teamDir) return null;
    const cwd = normalizeWorkspaceDir(teamDir);
    store.openWorkspace({
      teamKey,
      workspaceId: genericWorkspaceId(cwd),
      kind: "generic",
      cwd,
      restoreSoftClosed: true,
      focus: true,
      spawn: false,
    });
  }
  // The selected epic may be empty while another workspace remains focused. A team-scoped
  // Start reuses that workspace, and reveals its rail so the new terminal is visible.
  const live = useViewerStore.getState();
  if (live.trayMode[teamKey] === "grouped") {
    const chosen = activeWorkspaceIdForTeam(teamKey, live.activeWorkspace, live.workspaces);
    const entry = trayWorkspaceOrder(
      live.workspaces[teamKey] ?? [],
      live.softClosedWorkspaces[teamKey] ?? {},
      useTeamSurfaceStore.getState().trees[teamKey] ?? null,
    ).find((entry) => entry.workspace.id === chosen);
    if (entry) live.setSelectedEpicGroup(teamKey, railKeyOf(entry));
  }
  return openFreshAgentTerminal(teamKey);
}
