// SPDX-License-Identifier: MIT
import { toast } from "sonner";

import { openAgentResult } from "@/features/viewer/components/agent-result-panel";
import {
  formatSessionHandle,
  isHeadlessSession,
} from "@/features/viewer/lib/agent-row-presentation";
import {
  switchContext,
  canSwitchContext,
  currentContextEntry,
  beginContextNavigation,
  currentContextNavigationRevision,
} from "@/features/viewer/lib/context-switch";
import {
  activateAndFocus,
  focusActiveWorkspaceTerminal,
} from "@/features/viewer/lib/focus-cascade";
import { storyIdFromHandle } from "@/features/viewer/lib/session-story";
import { tabLayoutDir, workspaceInDir } from "@/features/viewer/lib/workspace-in-dir";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import {
  frozenTabMachineName,
  isRemoteMachineId,
  machineLabelOf,
  offlineMachineName,
} from "@/features/machines/lib/machine-offline";
import { logger } from "@/shared/lib/logger";
import { getActiveTeamKey, teamKeyHasTeam } from "@/shared/lib/team-key";
import {
  findTabLocationBySessionId,
  genericWorkspaceId,
  normalizeWorkspaceDir,
  selectHiddenTerminalTabIndex,
  selectMinimizedTerminalTabIndex,
  selectTerminalTabIndex,
  useViewerStore,
  type TerminalTab,
  type Workspace,
} from "@/shared/stores/viewer-store";
import {
  type AgentSessionRow,
  dashboardForegroundSession,
  dashboardListSessions,
} from "@/tauri/commands";

/**
 * One Show for every agent surface: a row click, "Go to terminal", the inbox's open-sender and
 * the MCP reveal all land here. Show always ends in something visible (a tab, a restored tab, a
 * session adopted into a tab, the run output) or a stated reason. "Isn't running" is only ever
 * said of a session that has ended.
 */

/** What Show did, as the MCP reveal reports it. A tab shown as it last was carries the notice
 *  that said so. */
export type ShowOutcome =
  | { shown: "tab" | "restored" | "adopted" | "runOutput"; notice?: string }
  | { notShown: string };

/** The machine whose tab can only be shown as it last was: it reads offline, or its layout
 *  could not be read live (frozen) while it reads connected. */
export interface HeldMachine {
  machine: string;
  offline: boolean;
}

/** Where Show takes an agent. Every plan that shows something names the team holding it, so a
 *  plan in another team switches to that team first. */
export type ShowPlan =
  /** A tab on a tile, open or minimized in the tray: bring the tile up and focus the tab. With
   *  `held`, its machine's terminals can't be read, and the tab shows how it last was. */
  | {
      kind: "tab";
      teamKey: string;
      workspaceId: string;
      tabId: string;
      minimized: boolean;
      held?: HeldMachine;
    }
  /** A tab the person Hid: put it back on its tile, same session. */
  | { kind: "restore"; teamKey: string; workspaceId: string; tabId: string }
  /** A live terminal session with no tab on a tile: adopt it into one. `tabDir` is the
   *  directory of the host workspace its tab is in, when it has one; `dir` is that, else the
   *  directory its launch recorded. `confirm`: nothing read yet says the session runs on This
   *  Mac (a roster row with no launch record, after a restart its machine's layout may still
   *  bring), so This Mac's own host is asked first, and a session it doesn't hold is never
   *  adopted. */
  | {
      kind: "adopt";
      teamKey: string;
      sessionId: string;
      storyId: string | null;
      tabDir: string | null;
      dir: string | null;
      confirm: boolean;
      /** Its run and story, should the fresh read find it ended: its saved output is shown
       *  then. */
      ended: { runId: string | null; storyId: string | null };
    }
  /** A headless run: its live run output. */
  | {
      kind: "run-output";
      runId: string;
      sessionId: string | null;
      storyId: string | null;
      teamKey?: string;
    }
  /** The machine its tab runs on is offline, and the tab is on no tile: nothing to open. */
  | { kind: "offline"; machine: string; teamKey?: string }
  /** Its machine reads connected, but its layout could not be read live, and its tab is on no
   *  tile: the tab is held as it last was, with nothing to attach. */
  | { kind: "frozen"; machine: string }
  /** It runs on a remote machine in a workspace with no tile on this Mac: a remote session is
   *  never adopted into a tile here, so there is no tab to show. */
  | { kind: "remote-untiled"; machine: string }
  /** It runs on a remote machine and no tab of it has reached This Mac yet (its machine's
   *  layout is still loading, or can't be read): there is nothing to show, and it is never
   *  adopted here. */
  | { kind: "remote-tabless"; machine: string }
  /** It ended: its saved run output when it has a run, else nothing to open. */
  | {
      kind: "ended";
      teamKey?: string;
      runId: string | null;
      sessionId?: string | null;
      storyId?: string | null;
    }
  /** Still starting: it has no session to show yet. */
  | { kind: "starting" };

/** What the resolver reads about a session, gathered from the stores by {@link readShowInputs}. */
export interface ShowInputs {
  /** The team Show runs from; a session nowhere else adopts into it. */
  teamKey: string;
  /** The tab on an open tile (or in a workspace with no tile), if any. */
  open?: TabAt & { tile: "live" | "minimized" | "none"; dir: string | null };
  /** The tab on a tile minimized into the tray, if any. */
  minimized?: TabAt;
  /** The Hidden tab, if any. */
  hidden?: TabAt;
  /** The latest roster's word on whether the session still runs; undefined when the roster
   *  does not list it. */
  rosterAlive?: boolean;
  /** The offline machine's name when the session runs on a machine that is offline. */
  offlineMachine?: string | null;
  /** The machine's name when its tab is held frozen (its layout not read live) while the
   *  machine itself does not read offline. */
  frozenMachine?: string | null;
  /** The machine's name when the session runs on a remote machine: its tab's, else the one its
   *  launch recorded. */
  remoteMachine?: string | null;
  /** Known to run on This Mac: its tab here has no remote origin, or its launch record says so.
   *  False when nothing read says where it runs (a roster row with no launch record). */
  onThisMac?: boolean;
  /** The team the session's story belongs to, if it has one. */
  storyTeamKey?: string | null;
}

interface TabAt {
  teamKey: string;
  workspaceId: string;
  tab: TerminalTab;
}

/**
 * Where Show takes `row`, in the order of the Show contract: ended, headless, a tab on a tile
 * (any team), a Hidden tab, a tab on a tile minimized into the tray, still starting, then a live
 * session with no tab: a remote one's machine is named (in a workspace with no tile here), a
 * local one is adopted. A tab whose machine is offline or frozen follows one rule: on a tile it
 * shows as it last was, with a notice; anywhere else its machine is named instead. Pure: the
 * caller reads the stores ({@link readShowInputs}).
 */
export function resolveAgentShow(
  row: Pick<
    AgentSessionRow,
    | "handle"
    | "sessionId"
    | "alive"
    | "kind"
    | "provenance"
    | "outputRunId"
    | "story"
    | "workspace"
    | "launchTeamKey"
  >,
  inputs: ShowInputs,
): ShowPlan {
  const owningTeam =
    inputs.open?.teamKey ??
    inputs.hidden?.teamKey ??
    inputs.minimized?.teamKey ??
    inputs.storyTeamKey ??
    row.launchTeamKey ??
    inputs.teamKey;
  const tabExited =
    (inputs.open?.tab.exit ?? inputs.minimized?.tab.exit ?? inputs.hidden?.tab.exit) != null;
  if (!row.alive || inputs.rosterAlive === false || tabExited) {
    return {
      kind: "ended",
      teamKey: owningTeam,
      runId: row.provenance?.runId || row.outputRunId || null,
      sessionId: row.sessionId,
      storyId: row.provenance?.storyId || null,
    };
  }
  if (isHeadlessSession(row)) {
    const runId = row.provenance?.runId;
    if (runId) {
      return {
        kind: "run-output",
        teamKey: owningTeam,
        runId,
        sessionId: row.sessionId,
        storyId: row.provenance?.storyId || null,
      };
    }
    return { kind: "starting" };
  }
  const held: HeldMachine | null = inputs.offlineMachine
    ? { machine: inputs.offlineMachine, offline: true }
    : inputs.frozenMachine
      ? { machine: inputs.frozenMachine, offline: false }
      : null;
  const heldOn = held ? { held } : {};
  const open = inputs.open;
  if (open && open.tile !== "none") {
    return {
      kind: "tab",
      teamKey: open.teamKey,
      workspaceId: open.workspaceId,
      tabId: open.tab.id,
      minimized: open.tile === "minimized",
      ...heldOn,
    };
  }
  // A Hidden tab is on no tile: one whose machine can't be read has nothing to put back.
  if (inputs.hidden && !held) {
    return {
      kind: "restore",
      teamKey: inputs.hidden.teamKey,
      workspaceId: inputs.hidden.workspaceId,
      tabId: inputs.hidden.tab.id,
    };
  }
  if (inputs.minimized) {
    return {
      kind: "tab",
      teamKey: inputs.minimized.teamKey,
      workspaceId: inputs.minimized.workspaceId,
      tabId: inputs.minimized.tab.id,
      minimized: true,
      ...heldOn,
    };
  }
  if (held) {
    if (held.offline)
      return {
        kind: "offline",
        machine: held.machine,
        teamKey: owningTeam,
      };
    return { kind: "frozen", machine: held.machine };
  }
  if (!row.sessionId) return { kind: "starting" };
  // A remote tab's session lives on its machine's host: This Mac's host cannot place it.
  if (inputs.remoteMachine) {
    return { kind: open ? "remote-untiled" : "remote-tabless", machine: inputs.remoteMachine };
  }
  return {
    kind: "adopt",
    // A tab out of sight in a launch workspace stays in that team; a tabless session goes to
    // its story's team, else the team Show runs from.
    teamKey: open?.teamKey ?? inputs.storyTeamKey ?? inputs.teamKey,
    sessionId: row.sessionId,
    storyId: row.story ?? storyIdFromHandle(row.handle) ?? null,
    tabDir: open?.dir ?? null,
    dir: open?.dir ?? row.workspace ?? null,
    confirm: !inputs.onThisMac,
    ended: { runId: row.provenance?.runId || null, storyId: row.provenance?.storyId || null },
  };
}

/** Read what {@link resolveAgentShow} needs about `row` from the stores. */
export function readShowInputs(
  row: Pick<AgentSessionRow, "handle" | "sessionId" | "machineId">,
  teamKey: string,
  storyTeamKey?: string | null,
): ShowInputs {
  const st = useViewerStore.getState();
  const sid = row.sessionId;
  const roster = useAgentListProjectionStore.getState().sessions;
  const listed = roster?.find((r) => (sid ? r.sessionId === sid : r.handle === row.handle));
  const inputs: ShowInputs = {
    teamKey,
    rosterAlive: listed ? listed.alive : undefined,
    storyTeamKey: storyTeamKey ?? null,
  };
  if (!sid) return inputs;
  const open = findTabLocationBySessionId(selectTerminalTabIndex(st), sid);
  if (open) {
    const live = st.workspaces[open.teamKey]?.some((w) => w.id === open.workspaceId) ?? false;
    const minimized = st.softClosedWorkspaces[open.teamKey]?.[open.workspaceId] !== undefined;
    const tile = live ? "live" : minimized ? "minimized" : "none";
    // Only a tab in a workspace with no tile is adopted, into the workspace in its directory.
    const dir = tile === "none" ? tabLayoutDir(st.terminalLayout.byKey, open) : null;
    inputs.open = { ...open, tile, dir };
  }
  inputs.minimized = findTabLocationBySessionId(selectMinimizedTerminalTabIndex(st), sid);
  inputs.hidden = findTabLocationBySessionId(selectHiddenTerminalTabIndex(st), sid);
  const tab = (inputs.open ?? inputs.minimized ?? inputs.hidden)?.tab;
  const origin = tab?.origin;
  // A tab says where it runs: its machine, or This Mac when it has no remote origin. With no
  // tab yet (its machine's layout still loading, or unreadable), the roster's launch record
  // does: a machine, null for This Mac, or nothing known. Frozen is a tab's state, so only a
  // tab can read frozen.
  const machineId = tab
    ? isRemoteMachineId(origin?.machineId)
      ? origin.machineId
      : null
    : launchMachineOf(row, listed);
  inputs.offlineMachine = offlineMachineName(machineId);
  inputs.frozenMachine = inputs.offlineMachine ? null : frozenTabMachineName(origin);
  inputs.remoteMachine = isRemoteMachineId(machineId) ? machineLabelOf(machineId) : null;
  inputs.onThisMac = machineId === null;
  return inputs;
}

/** Where a session's launch record says it runs: a remote machine's id, null for This Mac, or
 *  undefined when no row read carries a launch record. The caller's row is read first; an
 *  explicit null there means This Mac, not "ask the roster". */
function launchMachineOf(
  row: Pick<AgentSessionRow, "machineId">,
  listed: Pick<AgentSessionRow, "machineId"> | undefined,
): string | null | undefined {
  if (row.machineId !== undefined) return isRemoteMachineId(row.machineId) ? row.machineId : null;
  if (listed?.machineId !== undefined) {
    return isRemoteMachineId(listed.machineId) ? listed.machineId : null;
  }
  return undefined;
}

/** Where This Mac's own hosts say `sessionId` runs, from a fresh read of the live sessions:
 *  ended when the read lists it ended, here when a host here holds it and it still runs, on a
 *  machine when the read names one, null when nothing lists it (or the read failed, so nothing
 *  says where it is). */
async function hostOfSession(
  sessionId: string,
): Promise<{ on: "here" } | { on: "ended" } | { on: "machine"; machineId: string } | null> {
  const rows = await dashboardListSessions().catch((e: unknown) => {
    logger.warn("agents", `reading the live sessions failed: ${String(e)}`);
    return null;
  });
  const found = rows?.find((r) => r.sessionId === sessionId);
  if (!found) return null;
  if (!found.alive) return { on: "ended" };
  if (isRemoteMachineId(found.machineId)) return { on: "machine", machineId: found.machineId };
  return { on: "here" };
}

/** Bring up `teamKey`'s workspace `ws`, open or minimized. A minimized one comes back through
 *  the tray's own rule ({@link ViewerState.activateTrayWorkspace}), which reopens it in place of
 *  the focused tile, as a tray chip click does; when that rule can't bring it up (it is not the
 *  focused tile, yet the rule did nothing), it is opened as a tile. Null when the workspace
 *  did not end up focused: it is not shown, so the caller falls through to its next choice. */
function bringUpWorkspace(teamKey: string, ws: Workspace): string | null {
  const store = useViewerStore.getState();
  if (store.activateTrayWorkspace(teamKey, ws.id)) return ws.id;
  if (useViewerStore.getState().activeWorkspace[teamKey] === ws.id) return ws.id;
  store.openWorkspace({
    teamKey,
    workspaceId: ws.id,
    kind: ws.kind,
    cwd: ws.cwd ?? "",
    restoreSoftClosed: true,
    focus: true,
    spawn: false,
  });
  return useViewerStore.getState().activeWorkspace[teamKey] === ws.id ? ws.id : null;
}

/** Bring up `teamKey`'s workspace in `dir` (the one the agents pane lists the session under),
 *  else open its generic tile for `dir` with no shell of its own. */
function openTileFor(teamKey: string, dir: string): string | null {
  const cwd = normalizeWorkspaceDir(dir);
  const store = useViewerStore.getState();
  const existing = workspaceInDir(store, teamKey, cwd);
  if (existing) return bringUpWorkspace(teamKey, existing);
  const context = Object.entries(store.terminalLayout.byKey).find(
    ([key, layout]) =>
      key.startsWith(`${teamKey}|`) &&
      layout.descriptor.kind === "context" &&
      layout.descriptor.worktreePath === cwd,
  );
  const workspaceId = context ? context[0].slice(`${teamKey}|`.length) : genericWorkspaceId(cwd);
  store.openWorkspace({
    teamKey,
    workspaceId,
    kind: context ? "context" : "generic",
    cwd,
    restoreSoftClosed: true,
    focus: true,
    spawn: false,
  });
  if (context) {
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [teamKey]: (s.workspaces[teamKey] ?? []).map((w) =>
          w.id === workspaceId
            ? {
                ...w,
                contextWorktreeId: workspaceId,
                contextWorktreeName: context[1].descriptor.displayName,
                contextWorktreeBranch: context[1].descriptor.branch ?? undefined,
              }
            : w,
        ),
      },
    }));
  }
  return workspaceId;
}

/** Release a now-attended session's background slot. Fire-and-forget. */
function releaseBackgroundSlot(handle: string, after?: () => void): void {
  dashboardForegroundSession(handle)
    .catch((e) => logger.debug("dashboard", `foreground release failed: ${String(e)}`))
    .finally(() => after?.());
}

function focusTab(teamKey: string, preserveFocus = false): void {
  const store = useViewerStore.getState();
  store.expandTerminalOnOpen(teamKey);
  if (preserveFocus) activateAndFocus({ teamKey, focus: "preserve-focus" });
  else focusActiveWorkspaceTerminal(teamKey);
}

export interface ShowOptions {
  /** Select and reveal the tab while leaving keyboard focus on the calling surface. */
  preserveFocus?: boolean;
  /** The team Show runs from (the surface's team). Defaults to the active team. */
  teamKey?: string | null;
  /** The team the agent's story belongs to, when the caller knows it. */
  storyTeamKey?: string | null;
  /** The name to use in a toast; defaults to the row's label. */
  name?: string;
  /** Run after a foreground slot release (the agents list re-reads the roster). */
  onAfterForeground?: () => void;
  /** Say nothing in a toast: the caller reports the outcome itself. */
  quiet?: boolean;
  /** Never change the active team (a launch revealing its own new tab). */
  stayInTeam?: boolean;
}

let openRunHistory: (() => void) | null = null;

/** How to open the agents' Run history, registered by the surface that shows it (null once it is
 *  gone): an ended agent with no saved output of its own still offers the way to every run. */
export function setRunHistoryOpener(open: (() => void) | null): void {
  openRunHistory = open;
}

/** What a tab on a tile shown as it last was says about its machine. */
function heldNotice(held: HeldMachine, name: string): string {
  return held.offline
    ? `${held.machine} is offline, so ${name}'s tab shows how it last was; the agent may still be running there.`
    : `CodeSense can't read ${held.machine}'s terminals right now, so ${name}'s tab shows how it last was; the agent may still be running there.`;
}

/** Why a remote agent with no tab on This Mac yet can't be shown. */
function remoteTablessReason(name: string, machine: string): string {
  return `${name} runs on ${machine}, and its tab hasn't reached this Mac yet, so there is nothing to show.`;
}

/** Carry out a resolved plan. */
export async function runShowPlan(
  plan: ShowPlan,
  row: Pick<AgentSessionRow, "handle">,
  opts: ShowOptions & { name: string },
): Promise<ShowOutcome> {
  const say = (message: string, data?: Parameters<typeof toast>[1]) => {
    if (!opts.quiet) toast(message, data);
  };
  // Capture before restore/adopt changes the active workspace, including work that awaits a host.
  const origin = currentContextEntry();
  const revision = opts.stayInTeam ? currentContextNavigationRevision() : beginContextNavigation();
  const isCurrent = () => currentContextNavigationRevision() === revision;
  const superseded = () => ({ notShown: "A newer navigation superseded this Show." });
  const targetTeam = "teamKey" in plan ? plan.teamKey : undefined;
  const refused = () => {
    const reason = `Can't show ${opts.name}: its team is unavailable.`;
    say(reason);
    return { notShown: reason };
  };
  if (targetTeam && !opts.stayInTeam && !canSwitchContext(targetTeam)) return refused();
  const switchToTeam = (teamKey: string, workspaceId?: string, tabId?: string): boolean => {
    if (!opts.stayInTeam) {
      switchContext({ teamKey, workspaceId, tabId }, "agent", origin);
      return getActiveTeamKey() === teamKey;
    }
    if (workspaceId) {
      useViewerStore.getState().activateTrayWorkspace(teamKey, workspaceId);
      if (tabId) useViewerStore.getState().setActiveTerminalTab(teamKey, tabId, workspaceId);
    }
    return true;
  };
  const mayFocus = (teamKey: string) => getActiveTeamKey() === teamKey;
  switch (plan.kind) {
    case "tab": {
      if (!switchToTeam(plan.teamKey, plan.workspaceId, plan.tabId)) return refused();
      if (mayFocus(plan.teamKey)) focusTab(plan.teamKey, opts.preserveFocus);
      const shown = plan.minimized ? "restored" : "tab";
      if (!plan.held) return { shown };
      const notice = heldNotice(plan.held, opts.name);
      say(notice);
      return { shown, notice };
    }
    case "restore": {
      useViewerStore
        .getState()
        .restoreHiddenTerminalTab(plan.teamKey, plan.workspaceId, plan.tabId);
      if (!switchToTeam(plan.teamKey, plan.workspaceId, plan.tabId)) return refused();
      if (mayFocus(plan.teamKey)) focusTab(plan.teamKey, opts.preserveFocus);
      if (mayFocus(plan.teamKey)) releaseBackgroundSlot(row.handle, opts.onAfterForeground);
      return { shown: "restored" };
    }
    case "adopt": {
      // A host can publish the new tab before its placement acknowledgement.
      // Mint a quiet intent now so even that first attach can be revoked by a
      // newer file/layout navigation; only the completed Show requests focus.
      activateAndFocus({ teamKey: plan.teamKey, presence: "background", focus: "preserve-focus" });
      if (plan.confirm) {
        const host = await hostOfSession(plan.sessionId);
        if (!isCurrent()) return superseded();
        // Found ended: shown as any ended agent is (its saved output, else Run history).
        if (host?.on === "ended") {
          return runShowPlan(
            { kind: "ended", ...plan.ended, teamKey: plan.teamKey, sessionId: plan.sessionId },
            row,
            opts,
          );
        }
        if (host?.on !== "here") {
          const reason =
            host === null
              ? `Can't find ${opts.name} on this Mac. If it runs on another machine, that machine may still be reconnecting.`
              : remoteTablessReason(opts.name, machineLabelOf(host.machineId));
          say(reason);
          return { notShown: reason };
        }
      }
      if (!opts.stayInTeam && !canSwitchContext(plan.teamKey)) return refused();
      const store = useViewerStore.getState();
      // The agents pane's rule: the workspace in its tab's directory, else its story's, else the
      // tile for the directory it runs in, else the active one.
      const inTabDir = plan.tabDir ? workspaceInDir(store, plan.teamKey, plan.tabDir) : undefined;
      let target: string | null = inTabDir ? bringUpWorkspace(plan.teamKey, inTabDir) : null;
      if (!target && plan.storyId) {
        target = await store.ensureStoryWorkspaceOpen(plan.teamKey, plan.storyId, { isCurrent });
        if (!isCurrent()) return superseded();
      }
      if (!target && plan.dir) target = openTileFor(plan.teamKey, plan.dir);
      // Opening its story's tile may already have brought its tab back: focus that one.
      const st = useViewerStore.getState();
      const back = findTabLocationBySessionId(selectTerminalTabIndex(st), plan.sessionId);
      if (
        back &&
        back.teamKey === plan.teamKey &&
        (st.workspaces[back.teamKey]?.some((w) => w.id === back.workspaceId) ?? false)
      ) {
        if (!switchToTeam(plan.teamKey, back.workspaceId, back.tab.id)) return refused();
        if (mayFocus(back.teamKey)) focusTab(back.teamKey, opts.preserveFocus);
      } else {
        await st.adoptTerminalSession(plan.teamKey, plan.sessionId, target ?? undefined, {
          isCurrent,
        });
        if (!isCurrent()) return superseded();
        if (!switchToTeam(plan.teamKey, target ?? undefined)) return refused();
        if (mayFocus(plan.teamKey)) focusTab(plan.teamKey, opts.preserveFocus);
      }
      if (mayFocus(plan.teamKey)) releaseBackgroundSlot(row.handle, opts.onAfterForeground);
      return { shown: "adopted" };
    }
    case "run-output":
      if (plan.teamKey && !switchToTeam(plan.teamKey)) return refused();
      if (opts.stayInTeam && plan.teamKey && !mayFocus(plan.teamKey))
        return { notShown: "Run output waits in its team." };
      openAgentResult({
        run_id: plan.runId,
        sessionId: plan.sessionId,
        story_id: plan.storyId,
        preserveFocus: opts.preserveFocus,
      });
      return { shown: "runOutput" };
    case "offline": {
      if (plan.teamKey && !switchToTeam(plan.teamKey)) return refused();
      const reason = `${plan.machine} is offline, so ${opts.name} can't be opened; the agent may still be running there.`;
      say(reason);
      return { notShown: reason };
    }
    case "frozen": {
      const reason = `CodeSense can't read ${plan.machine}'s terminals right now, so ${opts.name} can't be opened; the agent may still be running there.`;
      say(reason);
      return { notShown: reason };
    }
    case "remote-tabless": {
      const reason = remoteTablessReason(opts.name, plan.machine);
      say(reason);
      return { notShown: reason };
    }
    case "remote-untiled": {
      const reason = `${opts.name} runs on ${plan.machine}, in a workspace that isn't open on this Mac, so there is no tab to show.`;
      say(reason);
      return { notShown: reason };
    }
    case "starting": {
      const reason = `${opts.name} is still starting.`;
      say(reason);
      return { notShown: reason };
    }
    case "ended": {
      if (plan.runId) {
        if (plan.teamKey && !switchToTeam(plan.teamKey)) return refused();
        if (opts.stayInTeam && plan.teamKey && !mayFocus(plan.teamKey))
          return { notShown: "Run output waits in its team." };
        // Its run saved output: open that, as a headless run's Show does.
        openAgentResult({
          run_id: plan.runId,
          sessionId: plan.sessionId ?? null,
          story_id: plan.storyId ?? null,
          preserveFocus: opts.preserveFocus,
        });
        return { shown: "runOutput" };
      }
      // With no run id there is no saved output of its own to open; Run history lists the rest.
      const reason = `${opts.name} has ended. There's no saved output for this run.`;
      const history = openRunHistory;
      say(reason, history ? { action: { label: "Run history", onClick: history } } : undefined);
      return { notShown: reason };
    }
    default:
      plan satisfies never;
      return { notShown: "unknown" };
  }
}

/** Show an agent: resolve where it is and take the person there, or say why not. */
export async function showAgent(
  row: AgentSessionRow,
  opts: ShowOptions = {},
): Promise<ShowOutcome> {
  const teamKey = opts.teamKey || getActiveTeamKey();
  const name = opts.name ?? (row.displayName?.trim() || formatSessionHandle(row.handle));
  // With no team on screen there is nowhere to show it.
  if (!opts.teamKey && !teamKeyHasTeam(teamKey)) {
    const reason = row.alive
      ? "Open a team to bring this agent to the foreground."
      : `${name} has ended.`;
    if (!opts.quiet) toast(reason);
    return { notShown: reason };
  }
  try {
    const plan = resolveAgentShow(row, readShowInputs(row, teamKey, opts.storyTeamKey));
    return await runShowPlan(plan, row, { ...opts, teamKey, name });
  } catch (e) {
    const reason = `Couldn't show ${name}: ${String(e)}`;
    logger.warn("agents", reason);
    if (!opts.quiet) toast.error(reason);
    return { notShown: reason };
  }
}
