// SPDX-License-Identifier: MIT
import { resolveTeamContentContext } from "@/shared/lib/team-content-context";
import { getActiveTeamKey, teamKeyHasTeam } from "@/shared/lib/team-key";
import { useOrgStore } from "@/shared/stores/org-store";
import {
  normalizeWorkspaceDir,
  cliChromeOf,
  expandedWorkspaceId,
  selectActiveTerminalTab,
  selectTabGridCollapsed,
  selectWorkspaceTabs,
  useViewerStore,
  workspacePanedTabIds,
  workspaceTabGridPreset,
} from "@/shared/stores/viewer-store";
import type { CliChrome } from "@/shared/stores/cli-chrome";

/**
 * WebDriver-only control door for the SW-RT-CAP close → reopen journey.
 *
 * The journey's workspaces must be ones the FE really shows, opened the way a
 * user opens them. Its root is the team's own root CLI workspace — the
 * ever-present team-folder workspace at the team's content path, opened through
 * `openOrFocusTeamFolderWorkspace`, the action the product runs whenever a team
 * would otherwise be left without a CLI. The others are generic workspaces
 * ("Open workspace here"), the kind that needs no story, worktree or agent: the
 * UI handler asks the native folder picker for a directory, then calls
 * `openOrFocusGenericWorkspace`. WebDriver cannot drive a native picker, so
 * `openGenericWorkspaces` makes that same store call with the directory the
 * picker would have returned — nothing else differs.
 *
 * `readTerminalLayout` is the FE side of the comparison: per workspace, the
 * tabs (session + name, in order), the active tab and the split grid, exactly
 * as the viewer store projects them from the server layout (its selectors).
 *
 * Behind App.tsx's `MODE === "webdriver"` guard, so it is tree-shaken from — and
 * proven absent (`scripts/verify-webdriver-absence.ts`) in — the shipped build.
 */

/** One FE workspace's terminals, keyed by PTY session id. */
export interface CloseReopenFeWorkspace {
  tabs: Array<{ session: string; name: string }>;
  /** The active tab's session, or null when the workspace has none. */
  active: string | null;
  grid: { paned: string[]; preset: string; collapsed: boolean };
}

export interface CloseReopenFeLayout {
  teamKey: string;
  activeWorkspace: string | null;
  layoutMode: string | null;
  expandedWorkspace: string | null;
  cliChrome: CliChrome;
  /** Per requested workspace id; null when the FE has no such workspace open. */
  workspaces: Record<string, CloseReopenFeWorkspace | null>;
}

export interface CloseReopenE2eControl {
  /** The active team's key, as the viewer store keys its slices. */
  activeTeamKey: () => string;
  /** Open (or focus) the active team's root CLI workspace through the product's
   *  own action. `teamKey` is null when no real team is active; `id` is null
   *  when the team has no content path to root it at (the product then has no
   *  root workspace to open). */
  openRootWorkspace: () => { teamKey: string | null; id: string | null; cwd: string | null };
  /** Open (or focus) one generic workspace per directory through the product's
   *  own action, in the active team; returns, per directory, the workspace id
   *  the store now holds for it (a directory the store did not open is absent).
   *  `teamKey` is null, and nothing is opened, when no real team is active. */
  openGenericWorkspaces: (dirs: readonly string[]) => {
    teamKey: string | null;
    ids: Record<string, string>;
  };
  /** Make `workspaceId` the active workspace of `teamKey`; true once it is. */
  focusWorkspace: (teamKey: string, workspaceId: string) => boolean;
  /** The FE's terminal layout for the given workspaces of `teamKey`. */
  readTerminalLayout: (teamKey: string, workspaceIds: readonly string[]) => CloseReopenFeLayout;
}

declare global {
  interface Window {
    __CODESENSE_CLOSE_REOPEN_E2E__?: CloseReopenE2eControl;
  }
}

function readTerminalLayout(teamKey: string, workspaceIds: readonly string[]): CloseReopenFeLayout {
  const state = useViewerStore.getState();
  const open = new Set((state.workspaces[teamKey] ?? []).map((workspace) => workspace.id));
  const workspaces: Record<string, CloseReopenFeWorkspace | null> = {};
  for (const id of workspaceIds) {
    if (!open.has(id)) {
      workspaces[id] = null;
      continue;
    }
    const tabs = selectWorkspaceTabs(state, teamKey, id);
    // Active and paned are held by tab id; report sessions, the shared key. A
    // dangling id stays visible as such rather than passing for a session.
    const sessionOf = (tabId: string) =>
      tabs.find((tab) => tab.id === tabId)?.sessionId ?? `missing:${tabId}`;
    const activeTab = selectActiveTerminalTab(state, teamKey, id);
    workspaces[id] = {
      tabs: tabs.map((tab) => ({ session: tab.sessionId, name: tab.name })),
      active: activeTab ? sessionOf(activeTab) : null,
      grid: {
        paned: workspacePanedTabIds(state, teamKey, id).map(sessionOf),
        preset: workspaceTabGridPreset(state, teamKey, id),
        collapsed: selectTabGridCollapsed(state, teamKey, id),
      },
    };
  }
  return {
    teamKey,
    activeWorkspace: state.activeWorkspace[teamKey] ?? null,
    layoutMode: state.layoutMode,
    expandedWorkspace: expandedWorkspaceId(state, teamKey),
    cliChrome: cliChromeOf(state),
    workspaces,
  };
}

export function registerCloseReopenE2eControl(): void {
  window.__CODESENSE_CLOSE_REOPEN_E2E__ = {
    activeTeamKey: () => getActiveTeamKey(),
    openRootWorkspace: () => {
      const teamKey = getActiveTeamKey();
      if (!teamKeyHasTeam(teamKey)) return { teamKey: null, id: null, cwd: null };
      const org = useOrgStore.getState();
      const resolved = resolveTeamContentContext(teamKey, org.orgs, org.discoveryComplete);
      const content = resolved.kind === "ready" ? resolved.source.teamContentPath : null;
      if (!content) return { teamKey, id: null, cwd: null };
      useViewerStore.getState().openOrFocusTeamFolderWorkspace(teamKey);
      const cwd = normalizeWorkspaceDir(content);
      const root = (useViewerStore.getState().workspaces[teamKey] ?? []).find(
        (workspace) => workspace.kind === "generic" && workspace.cwd === cwd,
      );
      return { teamKey, id: root?.id ?? null, cwd };
    },
    openGenericWorkspaces: (dirs) => {
      const teamKey = getActiveTeamKey();
      // Opening into the no-team bucket would bind the run to a team nobody sees.
      if (!teamKeyHasTeam(teamKey)) return { teamKey: null, ids: {} };
      const store = useViewerStore.getState();
      for (const dir of dirs) store.openOrFocusGenericWorkspace(teamKey, dir);
      const opened = (useViewerStore.getState().workspaces[teamKey] ?? []).filter(
        (workspace) => workspace.kind === "generic",
      );
      const ids: Record<string, string> = {};
      for (const dir of dirs) {
        const match = opened.find((workspace) => workspace.cwd === normalizeWorkspaceDir(dir));
        if (match) ids[dir] = match.id;
      }
      return { teamKey, ids };
    },
    focusWorkspace: (teamKey, workspaceId) => {
      useViewerStore.getState().switchWorkspace(teamKey, workspaceId);
      return useViewerStore.getState().activeWorkspace[teamKey] === workspaceId;
    },
    readTerminalLayout,
  };
}
