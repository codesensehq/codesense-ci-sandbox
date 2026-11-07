// SPDX-License-Identifier: MIT
/**
 * The one way the app's context moves to another team: an agent row's click, the team switcher,
 * the command palette, a notification, a file of another team, and ⌘[ / ⌘] back and forward.
 * The nav, the viewer, the right pane, the pills and the CLI all show the active team, so a
 * switch changes all of them at once. Each team keeps its own focused workspace and tabs; a
 * switch lands on what the target team remembers unless the route names a workspace.
 *
 * A switch records where it left (the team and its focused workspace) so ⌘[ goes back there and
 * ⌘] forward again, without changing the agents pane. Peeks (the quick view, the inbox chat) never come here.
 */

import { focusActiveWorkspaceTerminal } from "@/features/viewer/lib/focus-cascade";
import { isTerminalColumnShown } from "@/features/viewer/lib/terminal-column";
import { NO_TEAM_SENTINEL, getActiveTeamKey, parseTeamKey } from "@/shared/lib/team-key";
import { useOrgStore } from "@/shared/stores/org-store";
import {
  useViewerStore,
  type ContextEntry,
  type ContextHistory,
} from "@/shared/stores/viewer-store";

/** The route a switch takes. Back and forward walk the history instead of adding to it. */
export type ContextSwitchVia = "agent" | "switcher" | "nav" | "back" | "forward";

/** Where a switch goes: a team, and optionally the workspace (and its tab) to focus there. */
export interface ContextTarget {
  teamKey: string;
  workspaceId?: string | null;
  tabId?: string | null;
}

/** How many places each direction of the history keeps. */
export const CONTEXT_HISTORY_CAP = 50;

import { beginContextNavigation } from "@/shared/lib/context-navigation";
export {
  beginContextNavigation,
  currentContextNavigationRevision,
} from "@/shared/lib/context-navigation";

/** The place the user is at now, or null with no active team. */
export function currentContextEntry(): ContextEntry | null {
  const teamKey = getActiveTeamKey();
  if (teamKey === NO_TEAM_SENTINEL || parseTeamKey(teamKey).teamName === null) return null;
  return { teamKey, workspaceId: useViewerStore.getState().activeWorkspace[teamKey] ?? null };
}

const sameEntry = (a: ContextEntry | undefined, b: ContextEntry): boolean =>
  !!a && a.teamKey === b.teamKey && a.workspaceId === b.workspaceId;

/** `list` with `entry` on top, unless it is on top already, kept to the cap. */
function pushEntry(list: readonly ContextEntry[], entry: ContextEntry): ContextEntry[] {
  if (sameEntry(list[list.length - 1], entry)) return [...list];
  return [...list, entry].slice(-CONTEXT_HISTORY_CAP);
}

/** Whether `teamKey` has the workspace `workspaceId`, open or minimized. */
function hasWorkspace(teamKey: string, workspaceId: string): boolean {
  const s = useViewerStore.getState();
  return (
    (s.workspaces[teamKey]?.some((w) => w.id === workspaceId) ?? false) ||
    s.softClosedWorkspaces[teamKey]?.[workspaceId] !== undefined
  );
}

/** Whether the user can enter this team. */
export function canSwitchContext(teamKey: string): boolean {
  if (getActiveTeamKey() === teamKey) return true;
  const { orgSlug, teamName } = parseTeamKey(teamKey);
  const org = useOrgStore.getState();
  const shown = org.shownTeamsByOrgSlug[orgSlug];
  return (
    teamName !== null &&
    !org.disabledOrgSlugs.includes(orgSlug) &&
    org.orgs.some(
      (o) => o.slug === orgSlug && o.teams.some((t) => t.name === teamName && !t.archived),
    ) &&
    (shown === undefined || shown.includes(teamName))
  );
}

function makeActive(teamKey: string): void {
  if (!canSwitchContext(teamKey)) return;
  if (getActiveTeamKey() === teamKey) return;
  const { orgSlug, teamName } = parseTeamKey(teamKey);
  if (teamName !== null) useOrgStore.getState().setActiveTeamIn(orgSlug, teamName);
}

/** The target team's memory, set before the team changes so its first frame is already right:
 *  the named workspace focused (a minimized one comes back through the tray's own rule) and its
 *  named tab active. A workspace the team no longer has leaves the team's own focus. */
function prepare(target: ContextTarget, keepLayout = false): void {
  const { teamKey, workspaceId, tabId } = target;
  if (!workspaceId || !hasWorkspace(teamKey, workspaceId)) return;
  const store = useViewerStore.getState();
  store.activateTrayWorkspace(teamKey, workspaceId, { keepLayout });
  if (tabId) store.setActiveTerminalTab(teamKey, tabId, workspaceId);
}

/** Record a user gesture that changes the active team or its focused workspace.
 * Background mutations of another team only update its memory and record nothing. */
export function recordContextChange(
  via: ContextSwitchVia,
  change: () => void,
  from = currentContextEntry(),
): boolean {
  beginContextNavigation();
  change();
  const destination = currentContextEntry();
  if ((!destination && !from) || (destination && sameEntry(from ?? undefined, destination)))
    return false;
  if (from && via !== "back" && via !== "forward") {
    const { back } = useViewerStore.getState().contextHistory;
    useViewerStore.getState().setContextHistory({ back: pushEntry(back, from), forward: [] });
  }
  return true;
}

/**
 * Switch the context to `target.teamKey` by `via`. The target team's memory is prepared first
 * (the named workspace and tab, if any), then the team changes, then the user is oriented. The
 * caller focuses what its route focuses (an agent's terminal, D43); a switch alone moves no focus.
 * Returns whether the active team or focused workspace changed.
 */
export function switchContext(
  target: ContextTarget,
  via: ContextSwitchVia,
  origin?: ContextEntry | null,
): boolean {
  if (!canSwitchContext(target.teamKey)) return false;
  return recordContextChange(
    via,
    () => {
      prepare(target, true);
      makeActive(target.teamKey);
    },
    origin,
  );
}

/** Switch the context to the org `orgSlug`, landing on the team it remembers (the org store's
 *  own rule). An org with no team is not a history place: keep the departed
 *  team for Back, but never add an unreachable `org:_none` to either stack. */
export function switchContextToOrg(orgSlug: string, via: ContextSwitchVia): boolean {
  return recordContextChange(via, () => {
    const org = useOrgStore.getState();
    if (!org.disabledOrgSlugs.includes(orgSlug) && org.activeOrgSlug !== orgSlug)
      org.setActiveOrg(orgSlug);
  });
}

/** Switch the context to the team `teamName` of the active org. */
export function switchContextToTeamName(teamName: string, via: ContextSwitchVia): boolean {
  const orgSlug = useOrgStore.getState().activeOrgSlug;
  return orgSlug ? switchContext({ teamKey: `${orgSlug}:${teamName}` }, via) : false;
}

/** Whether an entry can still be gone to: its team is open (a closed team's places are skipped). */
function reachable(entry: ContextEntry): boolean {
  return useViewerStore.getState().openedTeams.includes(entry.teamKey);
}

/** Walk the history one step in `direction`: the place left goes on the other direction's list.
 *  Entries that can no longer be reached are dropped on the way. */
function walk(direction: "back" | "forward"): boolean {
  const here = currentContextEntry();
  const history: ContextHistory = useViewerStore.getState().contextHistory;
  const from = [...history[direction]];
  const otherKey = direction === "back" ? "forward" : "back";
  const deferred: ContextEntry[] = [];
  while (from.length > 0) {
    const entry = from.pop()!;
    if (!reachable(entry) || (here && sameEntry(here, entry))) continue;
    if (!canSwitchContext(entry.teamKey)) {
      deferred.push(entry);
      continue;
    }
    const target = { teamKey: entry.teamKey, workspaceId: entry.workspaceId };
    if (!switchContext(target, direction)) {
      deferred.push(entry);
      continue;
    }
    const remaining = [...from, ...deferred.reverse()];
    const to = here ? pushEntry(history[otherKey], here) : [...history[otherKey]];
    useViewerStore
      .getState()
      .setContextHistory(
        direction === "back" ? { back: remaining, forward: to } : { back: to, forward: remaining },
      );
    // The terminal takes focus only when the CLI is on screen to take it.
    if (isTerminalColumnShown(useViewerStore.getState())) {
      focusActiveWorkspaceTerminal(entry.teamKey);
    }
    return true;
  }
  const remaining = deferred.reverse();
  if (remaining.length !== history[direction].length) {
    useViewerStore.getState().setContextHistory({ ...history, [direction]: remaining });
  }
  return false;
}

/** ⌘[: back to the team and workspace before the last switch. False with nowhere to go. */
export function contextBack(): boolean {
  return walk("back");
}

/** ⌘]: forward again after going back. False with nowhere to go. */
export function contextForward(): boolean {
  return walk("forward");
}
