// SPDX-License-Identifier: MIT
import { isMac } from "@/shared/lib/shortcuts/keystroke";
import { useCallback, useState, type ReactNode } from "react";
import { create } from "zustand";

import {
  sessionsRunningProcesses,
  type RunningJobs,
} from "@/features/viewer/lib/cli-workspace/transport/terminal-session-source";
import { activeWorkspaceIdForTeam } from "@/shared/lib/team-key";
import { toast } from "sonner";
import { restartRequest } from "@/shared/lib/agent-session-launcher";
import { runRestart } from "./terminal-restart";
import { workspaceDisplayLabel } from "@/shared/lib/worktree";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { recordClosedTab } from "@/features/viewer/lib/closed-tab-history";
import { heldRemotely } from "@/shared/lib/terminal-layout-projection";
import {
  getViewerState,
  selectActiveTerminalTab,
  selectHiddenTerminalTabs,
  selectWorkspaceTabs,
  type TerminalTab,
} from "@/features/viewer/lib/cli-workspace/store/viewer-access";

/**
 * The one place a terminal closes. There are exactly two verbs, on tabs and on
 * tiles alike:
 *
 * - **Hide** keeps it running and leaves a way back, always in the tray: a hidden
 *   tile becomes a dimmed chip, a hidden tab is listed under its tile's chip —
 *   one click restores either onto the same live session.
 * - **End** kills it, and first asks in-app when anything is still running,
 *   naming what is running.
 *
 * The `×` (and middle-click, ⌘W) is the default close ({@link defaultCloseVerb},
 * the one place it is decided): Hide when a command is running in the
 * foreground, End otherwise — asking first when background jobs would die with
 * it. So nothing running ever disappears without either a confirmation or a
 * visible place to bring it back from.
 *
 * A tab and a tile are their terminal host's: Hide places a tab in its
 * workspace's Background entries (its Hidden tabs) or moves the tile into the
 * tray (minimized, its tabs kept in the host); End closes them through the host
 * with `kill`.
 *
 * What runs comes from `sessions_running_processes`: each shell's foreground job
 * and its background jobs (prompt helpers excluded). When that probe fails, every
 * session is treated as running in the foreground — the default close then
 * Hides, and End asks. A remote Machine's processes can't be read from here, so
 * a remote tab reads as running in the foreground too: its default close Hides
 * it (its agent row stays, marked hidden), and End stays explicit and asks first.
 */

export type CloseVerb = "Hide" | "End";

/** Shown when the probe can't say what a shell is running. */
const UNKNOWN_PROCESS = "an unknown process";

const IDLE: RunningJobs = { foreground: null, background: [] };

/** sessionId → what its shell is running. */
type RunningBySession = Map<string, RunningJobs>;

/** Stands for whatever a remote tab runs: never probed, so read as running in the
 *  foreground (its `×` Hides) and always asked about before End. */
const ON_ITS_MACHINE = "whatever is running on its machine";

/** The tabs among `sessionIds` whose session runs on a remote Machine. */
function remoteSessions(sessionIds: string[]): Set<string> {
  const layout = store().terminalLayout;
  return new Set(sessionIds.filter((id) => heldRemotely(layout, id)));
}

async function probeRunning(sessionIds: string[]): Promise<RunningBySession> {
  if (sessionIds.length === 0) return new Map();
  const remote = remoteSessions(sessionIds);
  const remoteJobs = (id: string): [string, RunningJobs] => [
    id,
    { foreground: ON_ITS_MACHINE, background: [] },
  ];
  const local = sessionIds.filter((id) => !remote.has(id));
  if (local.length === 0) return new Map([...remote].map(remoteJobs));
  try {
    const jobs = await sessionsRunningProcesses(local);
    return new Map([
      ...local.map((id, i): [string, RunningJobs] => [id, jobs[i] ?? IDLE]),
      ...[...remote].map(remoteJobs),
    ]);
  } catch {
    return new Map([
      ...local.map((id): [string, RunningJobs] => [
        id,
        { foreground: UNKNOWN_PROCESS, background: [] },
      ]),
      ...[...remote].map(remoteJobs),
    ]);
  }
}

/** A command is running in the foreground. */
function inForeground(running: RunningBySession, sessionId: string): boolean {
  return running.get(sessionId)?.foreground != null;
}

/** The default close (`×`, middle-click, ⌘W) of a tab or a tile — the ONE place
 *  it is decided: Hide when any of its terminals runs a command in the
 *  foreground, End otherwise (End asks first when background jobs would die). */
export function defaultCloseVerb(running: RunningBySession, sessionIds: string[]): CloseVerb {
  return sessionIds.some((id) => inForeground(running, id)) ? "Hide" : "End";
}

/** Anything at all would die with it — End asks first. */
function hasJobs(running: RunningBySession, sessionId: string): boolean {
  const jobs = running.get(sessionId) ?? IDLE;
  return jobs.foreground != null || jobs.background.length > 0;
}

// ---------------------------------------------------------------------------
// The End confirmation — an in-app dialog (`EndConfirmDialog`) driven by this
// store. `confirmEnd` opens it and resolves true only when the user picks End.
// ---------------------------------------------------------------------------

export interface EndConfirmRequest {
  id?: string;
  requester?: string;
  details?: ReactNode;
  onAlways?: (always: boolean) => void;
  canConfirm?: () => boolean;
  title: string;
  /** One line per running terminal: `name — processes`. */
  running: string[];
  confirmLabel: string;
  /** A line under the list — e.g. that the tile's hidden tabs keep running. */
  note?: string;
}

interface EndConfirmState {
  request: EndConfirmRequest | null;
  resolve: ((confirmed: boolean) => void) | null;
}

export const useEndConfirmStore = create<EndConfirmState>(() => ({
  request: null,
  resolve: null,
}));

/** Close the open confirmation with the user's answer. */
export function settleEndConfirm(confirmed: boolean): void {
  const { resolve } = useEndConfirmStore.getState();
  useEndConfirmStore.setState({ request: null, resolve: null });
  resolve?.(confirmed);
}

export function confirmEnd(request: EndConfirmRequest): Promise<boolean> {
  // A second request supersedes an open one; the first reads as cancelled.
  settleEndConfirm(false);
  return new Promise((resolve) => useEndConfirmStore.setState({ request, resolve }));
}

function describeRunning(tabs: TerminalTab[], running: RunningBySession): string[] {
  return tabs
    .filter((t) => hasJobs(running, t.sessionId))
    .map((t) => {
      const { foreground, background } = running.get(t.sessionId) ?? IDLE;
      const jobs = [
        ...(foreground ? [foreground] : []),
        ...background.map((b) => `${b} (background)`),
      ];
      return `${t.name} — ${jobs.join(", ")}`;
    });
}

function endRequest(subject: string, busy: string[], total: number): EndConfirmRequest {
  const n = busy.length;
  const title =
    total === 1
      ? `End ${subject}? It is still running.`
      : `End ${subject}? ${n} of ${total} terminals ${n === 1 ? "is" : "are"} still running.`;
  return { title, running: busy, confirmLabel: total === 1 ? "End" : `End ${total} terminals` };
}

// ---------------------------------------------------------------------------
// Tabs
// ---------------------------------------------------------------------------

const store = getViewerState;

/** A tile's shown tabs — open or in the tray. */
function tabsOf(teamKey: string, workspaceId: string): TerminalTab[] {
  return selectWorkspaceTabs(store(), teamKey, workspaceId);
}

/** The tabs Hidden from a tile — still running, and closed with it. */
function hiddenTabsOf(teamKey: string, workspaceId: string): TerminalTab[] {
  return selectHiddenTerminalTabs(store(), teamKey, workspaceId);
}

/** An agent ran in the tab: it carries a role, or an agent reported there. */
function wasAgentTab(tab: TerminalTab): boolean {
  return (
    tab.role != null ||
    store().aiStatusBySession[tab.sessionId] != null ||
    useAgentStateStore.getState().bySession[tab.sessionId] != null
  );
}

/** End each tab through its host (`kill`), remembering it for Reopen Closed Tab. */
function endNow(teamKey: string, workspaceId: string, tabs: TerminalTab[]): void {
  const ending = new Set(tabs.map((t) => t.id));
  const wasLastTab = tabsOf(teamKey, workspaceId).every((t) => ending.has(t.id));
  for (const tab of tabs) {
    // Read before the close prunes the tab's status.
    const wasAgent = wasAgentTab(tab);
    void store().removeTerminalTab(teamKey, tab.id, workspaceId, { kill: true });
    recordClosedTab({ kind: "ended", teamKey, workspaceId, tab, wasAgent, wasLastTab });
  }
}

/**
 * Hide a tab: off its tile and into the tray (listed under the tile's chip),
 * still running. Hiding a tile's only tab hides the whole tile instead, so a
 * tile is never left empty and no replacement shell is spawned.
 */
export function hideTab(teamKey: string, workspaceId: string, tabId: string): void {
  const tab = tabsOf(teamKey, workspaceId).find((t) => t.id === tabId);
  if (!tab) return;
  if (!store().hideTerminalTab(teamKey, tabId, workspaceId)) hideWorkspace(teamKey, workspaceId);
  recordClosedTab({ kind: "hidden", teamKey, workspaceId, tab });
}

/**
 * The tab `×`, middle-click and ⌘W: Hide when a command runs in the foreground;
 * otherwise End — asking first when background jobs would die with it.
 */
export async function closeTab(
  teamKey: string,
  workspaceId: string,
  tabId: string,
): Promise<boolean> {
  const tab = tabsOf(teamKey, workspaceId).find((t) => t.id === tabId);
  if (!tab) return false;
  const running = await probeRunning([tab.sessionId]);
  if (defaultCloseVerb(running, [tab.sessionId]) === "End") {
    return endTabs(teamKey, workspaceId, { tabId }, running);
  }
  hideTab(teamKey, workspaceId, tabId);
  return true;
}

/** ⌘W on a focused terminal: the default close for the active workspace's active tab. */
export async function closeActiveTerminalTab(teamKey: string): Promise<void> {
  const s = store();
  const workspaceId = activeWorkspaceIdForTeam(teamKey, s.activeWorkspace, s.workspaces);
  const tabId = selectActiveTerminalTab(s, teamKey, workspaceId);
  if (tabId) await closeTab(teamKey, workspaceId, tabId);
}

export type EndTabsScope = { tabId: string } | { except: string } | "all";

/** End one tab, every other tab, or all tabs — asking first if any is running.
 *  Resolves false when nothing was ended (no such tab, or the ask was cancelled). */
export async function endTabs(
  teamKey: string,
  workspaceId: string,
  scope: EndTabsScope,
  probed?: RunningBySession,
): Promise<boolean> {
  const all = tabsOf(teamKey, workspaceId);
  const targets =
    scope === "all"
      ? all
      : "tabId" in scope
        ? all.filter((t) => t.id === scope.tabId)
        : all.filter((t) => t.id !== scope.except);
  if (targets.length === 0) return false;
  const running = probed ?? (await probeRunning(targets.map((t) => t.sessionId)));
  const busy = describeRunning(targets, running);
  if (busy.length > 0) {
    const subject =
      targets.length === 1
        ? `“${targets[0].name}”`
        : scope === "all"
          ? "all open terminals"
          : "the other open terminals";
    const request = endRequest(subject, busy, targets.length);
    // End Others / End All act on the tile's OPEN tabs; its hidden tabs are
    // listed under the tray chip and keep running (End them from there).
    const hidden = hiddenTabsOf(teamKey, workspaceId).length;
    if (!(typeof scope === "object" && "tabId" in scope) && hidden > 0) {
      request.note = `${hidden} hidden tab${hidden === 1 ? " keeps" : "s keep"} running in the tray.`;
    }
    if (!(await confirmEnd(request))) return false;
  }
  // Only the tabs still open after the confirmation.
  const stillOpen = new Set(tabsOf(teamKey, workspaceId).map((t) => t.id));
  endNow(
    teamKey,
    workspaceId,
    targets.filter((t) => stillOpen.has(t.id)),
  );
  return true;
}

/** End one Hidden tab from its tray chip — asking first if anything runs in it. */
export async function endHiddenTab(
  teamKey: string,
  workspaceId: string,
  tabId: string,
): Promise<void> {
  const tab = hiddenTabsOf(teamKey, workspaceId).find((t) => t.id === tabId);
  if (!tab) return;
  const running = await probeRunning([tab.sessionId]);
  const busy = describeRunning([tab], running);
  if (busy.length > 0 && !(await confirmEnd(endRequest(`“${tab.name}”`, busy, 1)))) return;
  if (!hiddenTabsOf(teamKey, workspaceId).some((t) => t.id === tabId)) return;
  const wasAgent = wasAgentTab(tab);
  void store().removeTerminalTab(teamKey, tabId, workspaceId, { kill: true });
  recordClosedTab({ kind: "ended", teamKey, workspaceId, tab, wasAgent });
}

/**
 * End a tab whose process already ended (the ended bar's Close): nothing runs
 * in it, so nothing to ask about. Its host removes the exited entry.
 */
export function endEndedTab(teamKey: string, workspaceId: string, tabId: string): void {
  const tab = tabsOf(teamKey, workspaceId).find((t) => t.id === tabId);
  if (tab) endNow(teamKey, workspaceId, [tab]);
}

/**
 * Dismiss a Hidden tab whose process ended by itself: it leaves the tray, with
 * the same teardown as closing an open tab (its host removes the entry).
 */
export function dismissHiddenTab(teamKey: string, workspaceId: string, tabId: string): void {
  const tab = hiddenTabsOf(teamKey, workspaceId).find((t) => t.id === tabId);
  if (!tab) return;
  const wasAgent = wasAgentTab(tab);
  void store().removeTerminalTab(teamKey, tabId, workspaceId, { kill: true });
  recordClosedTab({ kind: "ended", teamKey, workspaceId, tab, wasAgent });
}

/**
 * Start a new process in `tab`'s ended entry (with the shell rules of the
 * launch that started it, if CodeSense launched it) — the ended bar's Restart,
 * one at a time per tab whichever surface asks. A refusal says so in a toast
 * and is kept as the tab's failed restart, which its pane's bar shows; a second
 * ask while one runs says that one is still going. `then` runs once it started.
 */
function restartEnded(
  teamKey: string,
  workspaceId: string,
  tab: TerminalTab,
  then?: () => void,
): void {
  const spec = useAgentStateStore.getState().launchBySession[tab.sessionId]?.spec;
  const { shell } = restartRequest(spec);
  const started = runRestart(
    tab.sessionId,
    "Restart",
    async () => {
      await store().respawnTerminalTab(teamKey, tab.sessionId, workspaceId, shell ?? {});
      then?.();
      return { ok: true };
    },
    (reason) => toast.error(`Couldn't restart “${tab.name}”`, { description: reason }),
  );
  // A menu closes on the click: say why nothing new happened.
  if (!started) toast.info(`Restart already in progress for “${tab.name}”`);
}

/** Restart an open tab whose process ended (its menu's Restart). */
export function restartTab(teamKey: string, workspaceId: string, tabId: string): void {
  const tab = tabsOf(teamKey, workspaceId).find((t) => t.id === tabId);
  if (tab) restartEnded(teamKey, workspaceId, tab);
}

/** Restart a Hidden tab whose process ended, then put it back on its tile. */
export function restartHiddenTab(teamKey: string, workspaceId: string, tabId: string): void {
  const tab = hiddenTabsOf(teamKey, workspaceId).find((t) => t.id === tabId);
  if (!tab) return;
  restartEnded(teamKey, workspaceId, tab, () =>
    store().restoreHiddenTerminalTab(teamKey, workspaceId, tabId),
  );
}

// ---------------------------------------------------------------------------
// Tiles (workspaces)
// ---------------------------------------------------------------------------

/**
 * Hide a tile: into the tray, every terminal still running (click the chip to
 * restore it).
 */
export function hideWorkspace(teamKey: string, workspaceId: string): void {
  store().requestCloseWorkspace(teamKey, workspaceId);
}

/** End a tile, live or hidden in the tray — asking first if anything is running.
 *  Resolves false when the user cancels that confirmation. */
export async function endWorkspace(
  teamKey: string,
  workspaceId: string,
  probed?: RunningBySession,
): Promise<boolean> {
  const s = store();
  const live = s.workspaces[teamKey]?.find((w) => w.id === workspaceId);
  const shadow = s.softClosedWorkspaces[teamKey]?.[workspaceId];
  if (!live && !shadow) return true;
  const tabs = [...tabsOf(teamKey, workspaceId), ...hiddenTabsOf(teamKey, workspaceId)];
  const running = probed ?? (await probeRunning(tabs.map((t) => t.sessionId)));
  const busy = describeRunning(tabs, running);
  if (busy.length > 0) {
    const ws = live ?? shadow?.workspace;
    const name = ws ? workspaceDisplayLabel(ws) : workspaceId;
    if (!(await confirmEnd(endRequest(`“${name}”`, busy, tabs.length)))) return false;
  }
  // The user confirmed End (a remote tab always asks): its remote tabs end too.
  if (live) store().closeWorkspace(teamKey, workspaceId, { killTerminals: true, endRemote: true });
  else store().killTerminalsForSoftClosedWorkspace(teamKey, workspaceId, { endRemote: true });
  return true;
}

/**
 * The tile / tray-chip `×`: Hide when any command runs in the foreground;
 * otherwise End — asking first when background jobs would die with it.
 */
export async function closeWorkspace(teamKey: string, workspaceId: string): Promise<void> {
  const ids = [...tabsOf(teamKey, workspaceId), ...hiddenTabsOf(teamKey, workspaceId)].map(
    (t) => t.sessionId,
  );
  const running = await probeRunning(ids);
  if (defaultCloseVerb(running, ids) === "Hide") hideWorkspace(teamKey, workspaceId);
  else await endWorkspace(teamKey, workspaceId, running);
}

// ---------------------------------------------------------------------------
// The `×` tooltip — names the verb the click will take
// ---------------------------------------------------------------------------

export function closeTooltip(verb: CloseVerb): string {
  return verb === "Hide" ? "Hide · keeps running" : "End";
}

/** The key that closes a focused tab with its close verb, as the platform
 *  labels it: ⌫ on macOS (where Backspace does it), Del elsewhere. */
export function tabCloseKey(): string {
  return isMac() ? "⌫" : "Del";
}

/** A tab `×`'s tooltip: the verb, and the key that does it from the tab. */
export function tabCloseTooltip(verb: CloseVerb): string {
  return `${closeTooltip(verb)} (${tabCloseKey()})`;
}

/**
 * Probes lazily on hover of a `×` (no polling) and reports which verb a click
 * would take for the keyed target. Unprobed reads as Hide — the click probes
 * again anyway, and Hide never loses anything.
 */
export function useCloseVerb(): {
  verbFor: (key: string) => CloseVerb;
  probe: (key: string, sessionIds: string[]) => void;
  /** Probe a whole tile (its tabs and its hidden tabs), keyed by its id. */
  probeWorkspace: (workspaceId: string) => void;
} {
  const [verbByKey, setVerbByKey] = useState<Record<string, CloseVerb>>({});

  const verbFor = useCallback((key: string) => verbByKey[key] ?? "Hide", [verbByKey]);

  const probe = useCallback((key: string, sessionIds: string[]) => {
    void probeRunning(sessionIds).then((running) => {
      const verb = defaultCloseVerb(running, sessionIds);
      setVerbByKey((m) => (m[key] === verb ? m : { ...m, [key]: verb }));
    });
  }, []);

  const probeWorkspace = useCallback(
    (workspaceId: string) => {
      const ws = store().workspaces;
      const teamKey = Object.keys(ws).find((k) => ws[k]?.some((w) => w.id === workspaceId));
      if (teamKey === undefined) return;
      probe(
        workspaceId,
        [...tabsOf(teamKey, workspaceId), ...hiddenTabsOf(teamKey, workspaceId)].map(
          (t) => t.sessionId,
        ),
      );
    },
    [probe],
  );

  return { verbFor, probe, probeWorkspace };
}
