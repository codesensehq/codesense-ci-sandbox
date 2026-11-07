// SPDX-License-Identifier: MIT
import { seedAgentListFixture } from "../lib/agent-list-test-fixture";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@/test-utils";

import type { RunOutcomeRead } from "@/shared/lib/run-outcome";
import type { AgentSessionRow, RoleConfig } from "@/tauri/commands";
import type { AgentStatus } from "@/shared/lib/agent-status-config";
import type { AgentListReference } from "../lib/agent-list-projection";
import { useRoleConfigsStore } from "@/shared/hooks/use-role-configs";
import { BUILTIN_FACE_SOURCE } from "@/shared/lib/avatar-packs";
import { agentRowKey, sessionPinId } from "@/shared/lib/agent-pins";
import { clearInboxDrafts } from "@/features/inbox/components/inbox-view-context";
import { REMOTE_LAYOUT_WAIT_MS } from "@/shared/lib/terminal-layout-projection";
import { projectSessionStatus } from "@/test/session-projection-fixture";
import { REGISTERED_AGENT_CLIS } from "@/shared/lib/agent-runtimes";
import { agentCliLabel } from "@/shared/lib/agent-clis";
import { NEW_WORKSPACE_TERMINAL_EVENT } from "../lib/new-workspace-terminal";

let listResult: AgentSessionRow[] = [];
let lateRoster: Promise<AgentSessionRow[]> | null = null;
let runOutcomes: RunOutcomeRead[] = [];
let rosterStatus: Record<string, string> = {};
let rosterLookup: ((sid: string) => Promise<{ status: string } | null>) | null = null;
const removeTerminalTabBySession = vi.fn(() => Promise.resolve());
const dashboardKillByHandle = vi.fn((_handle: string) => Promise.resolve());
const wvWorkerForceClose = vi.fn((_handle: string) => Promise.resolve());
const avatarPackShareWithTeam = vi.fn((_pack: string) => Promise.resolve());
const avatarDefaultPackSet = vi.fn((_pack: string | null) => Promise.resolve());
// Each team's packs: one pack named for the team, so a team's faces are told apart.
const avatarPacksListForTeam = vi.fn((teamKey: string) =>
  Promise.resolve({ packs: [], warnings: [], default_pack: null, agent_avatars: {}, teamKey }),
);
vi.mock("@/tauri/commands", () => ({
  LOCAL_MACHINE_ID: "local",
  listBuiltinPanes: async () => ({ contentRoot: "/ctx", chain: [], panes: [] }),
  dashboardListSessions: () => lateRoster ?? Promise.resolve(listResult),
  agentListRefresh: async () => {
    publishProjectionOwners({
      sessions: lateRoster ? await lateRoster : listResult,
      error: null,
      partial: false,
    });
  },
  wvListRoster: () => Promise.resolve(listResult.map((r) => ({ name: r.handle, ...r }))),
  dashboardKillByHandle: (handle: string) => dashboardKillByHandle(handle),
  dashboardForegroundSession: () => Promise.resolve(),
  sessionCwd: (sid: string) => Promise.resolve(`/work/${sid}`),
  listRunOutcomes: () => Promise.resolve(runOutcomes),
  wvRosterForSession: (sid: string) =>
    rosterLookup
      ? rosterLookup(sid)
      : Promise.resolve(rosterStatus[sid] ? { status: rosterStatus[sid] } : null),
  wvWorkerForceClose: (handle: string) => wvWorkerForceClose(handle),
  avatarPackShareWithTeam: (pack: string) => avatarPackShareWithTeam(pack),
  avatarDefaultPackSet: (pack: string | null) => avatarDefaultPackSet(pack),
  avatarPacksListForTeam: (teamKey: string) => avatarPacksListForTeam(teamKey),
  agentReadRoles: (team: unknown) => agentReadRoles(team),
  rolesList: (teamKey: string) => Promise.resolve(roleConfigs[teamKey] ?? []),
  releaseRole: (...args: unknown[]) => releaseRole(...args),
  agentsList: () => Promise.resolve([]),
  listSessionInbox: (handle: string) => listSessionInbox(handle),
  sendInboxAsHuman: (...args: unknown[]) => sendInboxAsHuman(...args),
  agentReadSessions: () => Promise.resolve([]),
  markInboxMessageRead: (...args: unknown[]) => markInboxMessageRead(...args),
}));
// The inbox chat's reads and sends: the same commands the tab's inbox rail uses.
let sessionInboxMessages: unknown[] = [];
const listSessionInbox = vi.fn((handle: string) =>
  Promise.resolve({ handle, roles: [], messages: sessionInboxMessages }),
);
const sendInboxAsHuman = vi.fn(
  (..._args: unknown[]): Promise<{ status: string; messageId?: number; message?: string }> =>
    Promise.resolve({ status: "sent", messageId: 99 }),
);
const markInboxMessageRead = vi.fn((..._args: unknown[]) => Promise.resolve());
let rolesResult: { roles: unknown[] } = { roles: [] };
let roleConfigs: Record<string, RoleConfig[]> = {};
const agentReadRoles = vi.fn((_team: unknown) => Promise.resolve(rolesResult));
const releaseRole = vi.fn((..._args: unknown[]) => Promise.resolve(0));
// Tauri events: a test can emit one to the listeners the pane registered.
const { eventListeners } = vi.hoisted(() => ({
  eventListeners: new Map<string, Set<(e: unknown) => void>>(),
}));
const openSettingsTab = vi.fn();
vi.mock("@/features/settings/lib/open-settings-tab", () => ({
  SETTINGS_VIRTUAL_PATH: "virtual://settings",
  openSettingsTab: (section?: string, row?: string) => openSettingsTab(section, row),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, fn: (e: unknown) => void) => {
    const set = eventListeners.get(name) ?? new Set();
    set.add(fn);
    eventListeners.set(name, set);
    return Promise.resolve(() => set.delete(fn));
  },
}));
const emitTauriEvent = (name: string) =>
  eventListeners.get(name)?.forEach((fn) => fn({ event: name, payload: null }));
// Each agent row reads its session's progress once per render: counting those reads counts the
// row's renders, by session.
const { rowRenders } = vi.hoisted(() => ({ rowRenders: new Map<string, number>() }));
vi.mock(
  "@/features/viewer/lib/cli-workspace/store/terminal-tab-selectors",
  async (importOriginal) => {
    const actual =
      await importOriginal<
        typeof import("@/features/viewer/lib/cli-workspace/store/terminal-tab-selectors")
      >();
    return {
      ...actual,
      useSessionProgress: (sessionId: string) => {
        rowRenders.set(sessionId, (rowRenders.get(sessionId) ?? 0) + 1);
        return actual.useSessionProgress(sessionId);
      },
    };
  },
);
vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn() }) }));
const launchOpen = vi.fn((_arg: unknown, _hooks: unknown) =>
  Promise.resolve({ sessionId: "sid-c" }),
);
const makeRealLaunchOpen = vi.fn((_launcher: string) => launchOpen);
vi.mock("@/features/dashboard/components/launch-open-real", () => ({
  makeRealLaunchOpen: (launcher: string) => makeRealLaunchOpen(launcher),
}));
const openAgentRunning = vi.fn((_key: string) => Promise.resolve());
vi.mock("@/features/agents/agents-virtual-tab", () => ({
  openAgentEditor: vi.fn(),
  openAgentRunning: (key: string) => openAgentRunning(key),
}));
// This component suite uses a minimal viewer store and legacy team labels. The
// real context-switch contract is exercised in context-switch.seam.test.ts;
// model its accepted navigation here so row behavior stays independently testable.
let shownContextTeam: string | null = null;
vi.mock("@/shared/lib/team-key", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/shared/lib/team-key")>();
  return { ...actual, getActiveTeamKey: () => shownContextTeam ?? actual.getActiveTeamKey() };
});
vi.mock("@/features/viewer/lib/context-switch", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/features/viewer/lib/context-switch")>()),
  canSwitchContext: () => true,
  switchContext: (target: { teamKey: string; workspaceId?: string; tabId?: string }) => {
    const changed = shownContextTeam !== target.teamKey;
    if (target.workspaceId) activateTrayWorkspace(target.teamKey, target.workspaceId);
    if (target.workspaceId && target.tabId) {
      setActiveTerminalTab(target.teamKey, target.tabId, target.workspaceId);
    }
    shownContextTeam = target.teamKey;
    const colon = target.teamKey.indexOf(":");
    if (changed && colon >= 0) {
      useOrgStore.getState().setActiveTeam(target.teamKey.slice(colon + 1));
    }
    return changed;
  },
}));
vi.mock("@/features/viewer/lib/focus-cascade", () => ({
  activateAndFocus: vi.fn(),
  focusActiveWorkspaceTerminal: vi.fn(),
  // The real rule over this file's mock store: reopen in place only when closed.
  revealCollapsedTerminal: () => revealCollapsed(),
}));
const openAgentResult = vi.fn();
vi.mock("@/features/viewer/components/agent-result-panel", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/features/viewer/components/agent-result-panel")>()),
  openAgentResult: (target: unknown) => openAgentResult(target),
}));
const hideTab = vi.fn();
vi.mock("@/features/viewer/lib/terminal-close", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/features/viewer/lib/terminal-close")>()),
  hideTab: (...args: unknown[]) => hideTab(...args),
}));

type Tab = { id: string; sessionId: string; name?: string };
type MockWs = {
  kind?: string;
  sourceRepoSlug?: string;
  location?: { kind: "machine"; machineId: string };
  status?: string;
  id: string;
  storyId?: string;
  storyTitle?: string;
  storyStatus?: string;
  storyFilePath?: string;
  cwd?: string;
};
let terminalTabIndex: Record<string, Record<string, Tab[]>> = {};
// The host layout's workspaces by key: only their directory is read here.
let layoutByKey: Record<string, { descriptor: { worktreePath?: string | null } }> = {};
// Whether the hosts' tab layout and the saved workspaces are back: a pin is read as ended only
// once both are.
let layoutHydrated = true;
/** The layout's admitted members and the layouts held of them, by member key. */
let layoutAdmission: Record<string, unknown> = {};
let layoutMembers: Record<string, unknown> = {};
/** When this viewer first saw each admission row, by member key. */
let layoutAdmittedAt: Record<string, number> = {};
let persistedStateRestored = true;
let workspaces: Record<string, MockWs[]> = {};
let dockCollapsed = false;
let paneFolds: Record<string, boolean> = {};
// The teams the user has open (the Teams checklist lists them, agents or not).
let openedTeamsMock: string[] = [];
// The teams' focused workspaces and their workspaces' active tabs.
let activeWorkspaceMock: Record<string, string> = {};
let activeTabMock: Record<string, string> = {};
let agentsListFilter: { agentKey: string; name: string } | null = null;
let terminalVisible = false;
let agentIdentity: Record<
  string,
  Record<string, { role?: string; face?: string; manager?: string }>
> = {};
let hiddenTabIndex: Record<string, Record<string, Tab[]>> = {};
type Pin = {
  kind: "session";
  teamKey: string;
  handle: string;
  sessionId: string | null;
  name: string;
};
let agentPins: Record<string, Pin[]> = {};
const pinIdOf = (p: Pin) => sessionPinId(p.teamKey, p.handle);
const pinAgent = vi.fn((teamKey: string, pin: Pin) => {
  const cur = agentPins[teamKey] ?? [];
  const at = cur.findIndex((p) => pinIdOf(p) === pinIdOf(pin));
  agentPins = {
    ...agentPins,
    [teamKey]: at >= 0 ? cur.map((p, i) => (i === at ? pin : p)) : [...cur, pin],
  };
});
const unpinAgents = vi.fn((teamKey: string, ids: readonly string[]) => {
  agentPins = {
    ...agentPins,
    [teamKey]: (agentPins[teamKey] ?? []).filter((p) => !ids.includes(pinIdOf(p))),
  };
});
let panelWidth: number | null = null;
// Keep the real virtual list; this seam can put an overscan header outside its visible range
// while spying on the exact row the orientation request asks it to scroll to.
const headerScroll = vi.hoisted(() => ({ outsideWindow: false, rows: [] as number[] }));
vi.mock("@tanstack/react-virtual", async (importOriginal) => {
  const real = await importOriginal<typeof import("@tanstack/react-virtual")>();
  return {
    ...real,
    useVirtualizer: (...args: Parameters<typeof real.useVirtualizer>) => {
      const list = real.useVirtualizer(...args);
      if (!headerScroll.outsideWindow) return list;
      return new Proxy(list, {
        get(target, property, receiver) {
          if (property === "range" && headerScroll.outsideWindow)
            return { startIndex: -1, endIndex: -1 };
          if (property === "scrollToIndex")
            return (index: number, options: { align?: "start" | "auto" | "center" | "end" }) => {
              headerScroll.rows.push(index);
              target.scrollToIndex(index, options);
            };
          return Reflect.get(target, property, receiver);
        },
      });
    },
  };
});

// The pane's team scope, arrow filter and explicit navigation live in the store.
let hiddenPaneTeams: string[] = [];
const setAgentsPaneTeamHidden = vi.fn((teamKey: string, hidden: boolean) => {
  hiddenPaneTeams = hidden
    ? [...hiddenPaneTeams.filter((t) => t !== teamKey), teamKey]
    : hiddenPaneTeams.filter((t) => t !== teamKey);
});
let teamFocusRestore: { teamKey: string; hidden: readonly string[] } | null = null;
const setAgentsPaneTeamScope = vi.fn(
  (hidden: string[], restore: { teamKey: string; hidden: readonly string[] } | null) => {
    hiddenPaneTeams = hidden;
    teamFocusRestore = restore;
  },
);
let paneWorkspaceFilter: { teamKey: string; workspaceId: string; machineId?: string } | null = null;
const setAgentsWorkspaceFilter = vi.fn(
  (f: { teamKey: string; workspaceId: string; machineId?: string } | null) => {
    paneWorkspaceFilter = f;
  },
);
let paneViewport = {
  offset: 0,
  measurements: [] as import("@tanstack/react-virtual").VirtualItem[],
};
let paneWorkspaceReveal: { teamKey: string; workspaceId: string; nonce: number } | null = null;
const clearAgentsWorkspaceReveal = vi.fn(() => {
  paneWorkspaceReveal = null;
});
const revealAgentsWorkspace = vi.fn((teamKey: string, workspaceId: string) => {
  if (dockCollapsed || !terminalVisible) return;
  paneWorkspaceReveal = { teamKey, workspaceId, nonce: (paneWorkspaceReveal?.nonce ?? 0) + 1 };
});
const restoreHiddenTerminalTab = vi.fn();
const setWorkspacePanelWidth = vi.fn();
const expandTerminalOnOpen = vi.fn(() => {
  terminalVisible = true;
});
const revealTerminalInPlace = vi.fn(() => {
  terminalVisible = true;
});
const revealCollapsed = (): boolean => {
  if (terminalVisible) return false;
  revealTerminalInPlace();
  return true;
};
const setAgentIdentity = vi.fn(
  (
    teamKey: string,
    handle: string,
    patch: { role?: string | null; face?: string | null; manager?: string | null },
  ) => {
    const cur = { ...agentIdentity[teamKey]?.[handle] };
    for (const [k, v] of Object.entries(patch)) {
      if (v === null) delete cur[k as "role" | "face" | "manager"];
      else cur[k as "role" | "face" | "manager"] = v;
    }
    agentIdentity = { ...agentIdentity, [teamKey]: { ...agentIdentity[teamKey], [handle]: cur } };
    notifyPaneStore();
  },
);
const pruneAgentIdentity = vi.fn();
const clearAgentsListFilter = vi.fn(() => {
  agentsListFilter = null;
});
const setAgentsPaneFold = vi.fn((key: string, folded: boolean) => {
  paneFolds = { ...paneFolds, [key]: folded };
  notifyPaneStore();
});
const setActiveTerminalTab = vi.fn();
const activateTrayWorkspace = vi.fn();
const adoptTerminalSession = vi.fn();
const setWorkspacePanelCollapsed = vi.fn((collapsed: boolean) => {
  dockCollapsed = collapsed;
});

// SW-AA-1 (Phase 1): the row's `useSessionProgress` reads `progressBySession`
// off the mocked viewer-store state. Mutable so a test can seed one session's
// live progress and assert the activity line.
let progressBySessionMock: Record<
  string,
  { phase: string; note?: string; step?: number; total?: number; settled?: boolean }
> = {};

function findLoc(map: typeof terminalTabIndex, sid: string) {
  for (const [teamKey, byWs] of Object.entries(map))
    for (const [workspaceId, tabs] of Object.entries(byWs)) {
      const tab = tabs.find((t) => t.sessionId === sid);
      if (tab) return { teamKey, workspaceId, tab };
    }
  return undefined;
}

// The team's shared workspace order (the tray's pills and the pane's cards): the real order rules
// over this file's mock workspaces.
let workspaceOrderState: Record<string, string[]> = {};
// The tray's epic groups, by team and workspace id.
let trayModesMock: Record<string, "flat" | "grouped"> = {};
let railKeysMock: Record<string, Record<string, string>> = {};
let customGroupsMock: Record<string, import("../lib/workspace-groups").WorkspaceGroup[]> = {};
const reorderWorkspace = vi.fn(
  (teamKey: string, id: string, place: { before: string } | { after: string }) => {
    const next = orderLib.moveInOrder(
      orderLib.teamWorkspaceOrder({
        base: undefined,
        workspaces: workspaces[teamKey] as never,
        softClosed: {},
        stored: workspaceOrderState[teamKey],
      }),
      id,
      place,
    );
    if (!next) return false;
    workspaceOrderState = { ...workspaceOrderState, [teamKey]: next };
    notifyPaneStore();
    return true;
  },
);

// The pane's search and arrangement are store state: a setter re-renders every subscribed hook.
let paneSearch: { query: string; status: string } = { query: "", status: "all" };
let paneArrange: "teams" | "status" | "manager" = "teams";
const paneStoreListeners = new Set<() => void>();
const notifyPaneStore = () => {
  const sessions = useAgentListProjectionStore.getState().sessions;
  if (sessions) publishProjectionOwners({ sessions });
  paneStoreListeners.forEach((listener) => listener());
};
const storeState = () => ({
  agentsPaneSearch: paneSearch,
  setAgentsPaneSearch: (patch: Partial<{ query: string; status: string }>) => {
    paneSearch = { ...paneSearch, ...patch };
    notifyPaneStore();
  },
  agentsPaneArrange: paneArrange,
  setAgentsPaneArrange: (arrange: "teams" | "status" | "manager") => {
    paneArrange = arrange;
    notifyPaneStore();
  },
  terminalTabIndex,
  terminalLayout: {
    byKey: layoutByKey,
    hydrated: layoutHydrated,
    admission: layoutAdmission,
    members: layoutMembers,
    admittedAt: layoutAdmittedAt,
  },
  persistedStateRestored,
  workspaces,
  softClosedWorkspaces: EMPTY_CLOSED,
  workspaceOrderByTeam: workspaceOrderState,
  trayBaseOrderByTeam: {},
  railKeyByWorkspaceId: railKeysMock,
  customGroupsByTeam: customGroupsMock,
  trayMode: trayModesMock,
  reorderWorkspace,
  activeWorkspace: activeWorkspaceMock,
  workspacePanelCollapsed: dockCollapsed,
  setWorkspacePanelCollapsed,
  agentsPaneFolds: paneFolds,
  setAgentsPaneFold,
  openedTeams: openedTeamsMock,
  agentsPaneHiddenTeams: hiddenPaneTeams,
  setAgentsPaneTeamHidden,
  agentsPaneTeamFocusRestore: teamFocusRestore,
  setAgentsPaneTeamScope,
  agentsWorkspaceFilter: paneWorkspaceFilter,
  setAgentsWorkspaceFilter,
  agentsPaneViewport: paneViewport,
  rememberAgentsPaneViewport: (
    offset: number,
    measurements: import("@tanstack/react-virtual").VirtualItem[],
  ) => {
    paneViewport = { offset, measurements };
  },
  agentsWorkspaceReveal: paneWorkspaceReveal,
  revealAgentsWorkspace,
  clearAgentsWorkspaceReveal,
  workspacePanelWidth: panelWidth,
  setWorkspacePanelWidth,
  agentPins,
  pinAgent,
  unpinAgents,
  restoreHiddenTerminalTab,
  agentsListFilter,
  clearAgentsListFilter,
  setActiveTerminalTab,
  activateTrayWorkspace,
  adoptTerminalSession,
  expandTerminalOnOpen,
  revealTerminalInPlace,
  terminalVisible,
  layoutMode: null,
  agentIdentity,
  setAgentIdentity,
  removeTerminalTabBySession,
  pruneAgentIdentity,
  aiStatusBySession: {},
  // SW-AA-1 (Phase 1): the row subscribes to `progressBySession[sessionId]`
  // for its live-activity line; populated by tests that need it.
  progressBySession: progressBySessionMock,
  // feat-idle-agent-status: the agents hook subscribes to this slice to tell a
  // ticking agent from a quiet one. Default empty — a test seeds a stall by
  // populating a session's `atMs` directly.
  progressTickBySession: {} as Record<string, number>,
  ensureStoryWorkspaceOpen: vi.fn(() => Promise.resolve(null)),
  openStoryDocForTeam: vi.fn(),
  changeStoryStatus: vi.fn(),
  settings: { backgroundAgentLimit: "auto" },
});
vi.mock("@/shared/stores/viewer-store", async () => {
  const React = await vi.importActual<typeof import("react")>("react");
  // Re-renders the calling component when a store setter runs, as the real store does.
  const useStoreState = (sel: (s: unknown) => unknown) => {
    const [, rerender] = React.useReducer((n: number) => n + 1, 0);
    React.useEffect(() => {
      paneStoreListeners.add(rerender);
      return () => {
        paneStoreListeners.delete(rerender);
      };
    }, []);
    return sel(storeState());
  };
  const actual = await vi.importActual<typeof import("@/shared/stores/viewer-store")>(
    "@/shared/stores/viewer-store",
  );
  return { ...storeMock(useStoreState), normalizeWorkspaceDir: actual.normalizeWorkspaceDir };
});
const EMPTY_CLOSED = {};
function storeMock(useStoreState: (sel: (s: unknown) => unknown) => unknown) {
  return {
    useViewerStore: Object.assign(useStoreState, {
      getState: () => storeState(),
      subscribe: () => () => {},
    }),
    selectTerminalTabIndex: (st: { terminalTabIndex: unknown }) => st.terminalTabIndex,
    selectRunningTerminalTabIndex: (st: { terminalTabIndex: unknown }) => st.terminalTabIndex,
    selectMinimizedTerminalTabIndex: () => ({}),
    selectHiddenTerminalTabIndex: () => hiddenTabIndex,
    selectHiddenTabLocation: (_st: unknown, sid: string) => findLoc(hiddenTabIndex, sid),
    selectActiveTerminalTab: (_st: unknown, teamKey: string, workspaceId: string) =>
      activeTabMock[`${teamKey}|${workspaceId}`],
    selectOpenTabLocation: (st: { terminalTabIndex: typeof terminalTabIndex }, sid: string) =>
      findLoc(st.terminalTabIndex, sid),
    findTabLocationBySessionId: findLoc,
    selectWorkspaceKey: (_st: unknown, teamKey: string, id: string) => `${teamKey}|${id}`,
  };
}

const { useEndedAgentPins } = await import("../hooks/use-ended-agent-pins");
const { useStaleArrowFilterCleanup } = await import("../hooks/use-stale-arrow-filter");
const { DockSlotHost: NativeDockSlotHost } = await import("@/features/shell/slots/dock-slot-host");
const { warmScopes } = await import("@/shared/lib/warm-scopes");
const { useViewerStore: useViewerStoreMock } = await import("@/shared/stores/viewer-store");
const orderLib = await import("@/features/viewer/lib/workspace-order");
const { useLaunchSheetStore } = await import("@/features/agents/lib/launch-sheet");
const { focusActiveWorkspaceTerminal } = await import("@/features/viewer/lib/focus-cascade");
const { AgentsDockToggle } = await import("./agents-dock-toggle");
const { useAgentStateStore } = await import("@/shared/stores/agent-state-store");
const { useAgentListProjectionStore } = await import("@/features/viewer/lib/agent-list-projection");
const { useAgentInboxRailStore } = await import("@/features/viewer/stores/agent-inbox-rail-store");
const { useMachinesStore } = await import("@/features/machines/stores/machines-store");
const { useOrgStore } = await import("@/shared/stores/org-store");
const { useRoutinesStore } = await import("@/features/routines/stores/routines-store");
const { toast } = await import("sonner");
const { useManagedSessionStore, resetManagedSessionStoreForTests } =
  await import("@/shared/stores/use-managed-session-store");

function row(over: Partial<AgentSessionRow> & { handle: string }): AgentSessionRow {
  return {
    sessionId: `sid-${over.handle}`,
    kind: "dashboard-launched",
    alive: true,
    backgrounded: false,
    startedWith: null,
    startedAtMs: null,
    launcher: null,
    lastOutputMs: null,
    ...over,
  };
}

/** Hooks have reported on these sessions, mid-turn: Rust projects them working. */
function hooked(...sessionIds: string[]) {
  for (const id of sessionIds) projectSessionStatus(id, "thinking");
  useAgentStateStore.setState((s) => ({
    bySession: {
      ...s.bySession,
      ...Object.fromEntries(
        sessionIds.map((id) => [
          id,
          {
            t1: { signal: { kind: "claude-hook", event: "UserPromptSubmit" }, seq: 1 },
            t2: null,
            state: "thinking",
            agent: null,
          },
        ]),
      ),
    },
  }));
}

/** These sessions wait on an answer: their managed state, and the status Rust projects. */
function awaitingAnswer(...sessionIds: string[]) {
  useManagedSessionStore.setState((s) => ({
    states: {
      ...s.states,
      ...Object.fromEntries(sessionIds.map((id) => [id, "awaiting-answer" as const])),
    },
  }));
  for (const id of sessionIds) projectSessionStatus(id, "awaiting-answer");
}

// The roster is loaded at the app root; the list only reads it.
const CONTENT_DIR = "/orgs/acme/ctx/team-1/content";
const CTL = `${CONTENT_DIR}#controller`;

/** A row's own button (not its expand/collapse toggle), by the agent's name. */
const agentRow = (re: RegExp) => (name: string) =>
  re.test(name) && !/^(Expand|Collapse|Select) /.test(name);
const agentRowButton = (re: RegExp) => screen.getByRole("button", { name: agentRow(re) });
/** A row's own button, by the agent's handle (its name also reads its details). */
const rowOf = (handle: string) =>
  document.querySelector<HTMLElement>(`[data-agent-handle="${handle}"] .ag-main`)!;
/** A drawn pane row by its row key, which holds quotes a selector would have to escape. */
const rowByKey = (key: string) =>
  Array.from(document.querySelectorAll<HTMLElement>("[data-row-key]")).find(
    (el) => el.dataset.rowKey === key,
  ) ?? null;

/** The pane's filter menu, opened from the find field's filter button if it is closed. */
function filterMenu(): HTMLElement {
  if (!screen.queryByRole("dialog", { name: "Filter and arrange agents" })) {
    fireEvent.click(screen.getByRole("button", { name: "Filter and arrange" }));
  }
  return screen.getByRole("dialog", { name: "Filter and arrange agents" });
}
/** A Show option in the filter menu by its label; `count` pins the number it shows. */
function showOption(label: string, count?: number): HTMLElement {
  const hit = Array.from(filterMenu().querySelectorAll<HTMLElement>("button.ag-fm-item")).find(
    (b) =>
      Array.from(b.children).some(
        (c) => c.textContent === label && !c.classList.contains("ag-fm-n"),
      ) &&
      (count === undefined || b.querySelector(".ag-fm-n")?.textContent === String(count)),
  );
  if (!hit) throw new Error(`no Show option "${label}"${count === undefined ? "" : ` ${count}`}`);
  return hit;
}
/** The number a Show option shows. */
function showCount(label: string): number {
  return Number(showOption(label).querySelector(".ag-fm-n")?.textContent);
}

function teamKeyOfStoryFixture(row: AgentSessionRow): string | null {
  return (
    Object.entries(workspaces).find(([, items]) =>
      items.some((ws) => !!ws.storyId && ws.storyId === (row.story ?? row.handle.split("#")[0])),
    )?.[0] ?? null
  );
}
let fixtureTeamKey = "team-1";
let projectedOverrides: Record<
  string,
  Partial<import("../lib/agent-list-projection").AgentListReference>
> = {};
let projectedTeamOrder: string[] | null = null;
let projectedHomeTeam: string | null = null;
// The status Rust projected for a session (fed with `projectSessionStatus`); a roster
// republish carries no status of its own, so it keeps what the sessions projection said.
const RUST_STATUS_FIELDS = ["effectiveStatus", "subagents", "activityAtMs"] as const;
function keepProjectedStatus(owners: AgentSessionRow[]): AgentSessionRow[] {
  const fed = useAgentListProjectionStore.getState().sessions ?? [];
  return owners.map((row) => {
    const before = row.sessionId && fed.find((f) => f.sessionId === row.sessionId);
    if (!before || before === row) return row;
    const kept = RUST_STATUS_FIELDS.filter(
      (key) => before[key] !== undefined && row[key] === undefined,
    );
    return kept.length
      ? { ...row, ...Object.fromEntries(kept.map((key) => [key, before[key]])) }
      : row;
  });
}
// Rust's Status order (`replace_status` in live_hub/agent_list/views.rs) over the statuses this
// file fed: needs you, then working, then ready, each by handle (this file feeds no unread
// counts, Rust's middle tiebreak), at its position.
const RUST_STATUS_GROUP: Record<AgentStatus, 0 | 1 | 2> = {
  "awaiting-answer": 0,
  paused: 0,
  blocked: 0,
  dropped: 0,
  failed: 0,
  crashed: 0,
  errored: 0,
  running: 1,
  thinking: 1,
  "tool-running": 1,
  idle: 2,
  stalled: 2,
  unknown: 2,
  completed: 2,
  done: 2,
};
function rustStatusOrder(
  references: readonly AgentListReference[],
  sessions: readonly AgentSessionRow[] | null,
): AgentListReference[] {
  const session = (ref: AgentListReference) =>
    sessions?.find((row) => (row.id ?? row.sessionId ?? row.handle) === ref.sessionRef);
  const group = (ref: AgentListReference) => {
    const status = session(ref)?.effectiveStatus;
    return status ? RUST_STATUS_GROUP[status] : 3;
  };
  const handle = (ref: AgentListReference) => session(ref)?.handle ?? "";
  return [...references]
    .sort(
      (a, b) =>
        group(a) - group(b) ||
        handle(a).toLowerCase().localeCompare(handle(b).toLowerCase()) ||
        handle(a).localeCompare(handle(b)),
    )
    .map((ref, order) => ({ ...ref, order, statusGroup: group(ref) }));
}
useAgentListProjectionStore.subscribe((state, previous) => {
  if (state.sessions === previous.sessions && state.references === previous.references) return;
  useAgentListProjectionStore.setState({
    statusReferences: rustStatusOrder(state.references, state.sessions),
  });
});
function publishProjectionOwners(
  next: Partial<ReturnType<typeof useAgentListProjectionStore.getState>>,
) {
  if (next.sessions) next = { ...next, sessions: keepProjectedStatus(next.sessions) };
  useAgentListProjectionStore.setState(next);
  const owners = next.sessions;
  if (!owners) return;
  const teamKey = projectedHomeTeam ?? fixtureTeamKey;
  const listed = owners.filter(
    (row) =>
      !["live-pty", "root", "notes", "#"].includes(row.kind) ||
      owners.some(
        (other) =>
          !["live-pty", "root", "notes", "#"].includes(other.kind) &&
          (other.launchedBy === row.handle || other.launchedBy === row.sessionId),
      ),
  );
  const placements = new Map(
    listed.map((row) => {
      const loc = row.sessionId
        ? (findLoc(terminalTabIndex, row.sessionId) ?? findLoc(hiddenTabIndex, row.sessionId))
        : null;
      const rowTeamKey = loc?.teamKey ?? teamKeyOfStoryFixture(row) ?? teamKey;
      const workspaceId =
        loc?.workspaceId ??
        workspaces[rowTeamKey]?.find(
          (ws) => !!ws.storyId && ws.storyId === (row.story ?? row.handle.split("#")[0]),
        )?.id ??
        null;
      const order = orderLib.teamWorkspaceOrder({
        base: undefined,
        workspaces: workspaces[rowTeamKey] as never,
        softClosed: {},
        stored: workspaceOrderState[rowTeamKey],
        groups: railKeysMock[rowTeamKey],
      });
      const chosen = agentIdentity[rowTeamKey]?.[row.handle]?.manager;
      const namedParent =
        chosen === "@none"
          ? null
          : (chosen ?? (row.provenance?.routineKey ? null : row.launchedBy));
      const parentHandle =
        owners.find((parent) => parent.handle === namedParent || parent.sessionId === namedParent)
          ?.handle ?? null;
      return [
        row.id ?? row.sessionId ?? row.handle,
        {
          teamKey: rowTeamKey,
          workspaceId,
          storyId: workspaceId,
          workspaceRank:
            workspaceId && order.includes(workspaceId) ? order.indexOf(workspaceId) : order.length,
          parentHandle,
          parentSource: chosen === undefined ? ("launcher" as const) : ("chosen" as const),
          crewParentHandle: row.provenance?.routineKey ? null : (row.launchedBy ?? null),
        },
      ];
    }),
  );
  seedAgentListFixture(owners, placements, useAgentListProjectionStore.getState().partial);
  useAgentListProjectionStore.setState((state) => ({
    references: state.references.map((ref) => ({ ...ref, ...projectedOverrides[ref.sessionRef] })),
    ...(projectedTeamOrder ? { teamOrder: projectedTeamOrder } : {}),
    ...(projectedHomeTeam ? { homeTeam: projectedHomeTeam } : {}),
  }));
}
// These owners live at the app root in production and remain mounted during dock rerenders.
function DockSlotHost(props: React.ComponentProps<typeof NativeDockSlotHost>) {
  useEndedAgentPins();
  useStaleArrowFilterCleanup();
  // The dock belongs to the team on screen, which production activates before mounting it.
  if (warmScopes.tierOf(props.teamKey) !== "active") warmScopes.activate(props.teamKey);
  return <NativeDockSlotHost {...props} />;
}
function renderList(teamKey = "team-1") {
  fixtureTeamKey = teamKey;
  const state = useAgentStateStore.getState();
  for (const r of listResult) {
    if (
      r.sessionId &&
      r.alive &&
      !["live-pty", "root", "notes", "#"].includes(r.kind) &&
      !state.bySession[r.sessionId]
    )
      state.setProcessAgent(r.sessionId, "claude");
  }
  publishProjectionOwners({ sessions: listResult, error: null });
  return render(<DockSlotHost teamKey={teamKey} visible />);
}

// The pane's list is virtualized: jsdom has no layout, so the scroll area gets a height here and
// every row gets a fixed one, so the rows a test expects are drawn. A test may shrink the area to
// leave rows out of the drawn window.
let scrollAreaPx = 4000;
beforeAll(() => {
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (
    this: HTMLElement,
  ) {
    const height = this.classList.contains("ag-scroll") ? scrollAreaPx : 40;
    return {
      x: 0,
      y: 0,
      top: 0,
      left: 0,
      bottom: height,
      right: 300,
      width: 300,
      height,
      toJSON: () => ({}),
    } as DOMRect;
  });
  Object.defineProperty(HTMLElement.prototype, "offsetHeight", {
    configurable: true,
    get(this: HTMLElement) {
      return this.classList.contains("ag-scroll") ? scrollAreaPx : 40;
    },
  });
});

beforeEach(() => {
  shownContextTeam = null;
  fixtureTeamKey = "team-1";
  projectedOverrides = {};
  projectedTeamOrder = null;
  projectedHomeTeam = null;
  vi.clearAllMocks();
  useAgentInboxRailStore.setState({ bySession: {} });
  useMachinesStore.setState({ byTeam: {} });
  terminalTabIndex = {};
  layoutByKey = {};
  layoutHydrated = true;
  layoutAdmission = {};
  layoutMembers = {};
  layoutAdmittedAt = {};
  persistedStateRestored = true;
  workspaces = {};
  dockCollapsed = false;
  paneFolds = {};
  openedTeamsMock = [];
  activeWorkspaceMock = {};
  activeTabMock = {};
  agentsListFilter = null;
  terminalVisible = true;
  agentIdentity = {};
  hiddenTabIndex = {};
  agentPins = {};
  panelWidth = null;
  rolesResult = { roles: [] };
  roleConfigs = {};
  useRoleConfigsStore.setState({ byTeam: {}, errors: {} });
  hiddenPaneTeams = [];
  teamFocusRestore = null;
  paneSearch = { query: "", status: "all" };
  paneArrange = "teams";
  paneWorkspaceFilter = null;
  paneWorkspaceReveal = null;
  paneViewport = { offset: 0, measurements: [] };
  progressBySessionMock = {};
  workspaceOrderState = {};
  railKeysMock = {};
  customGroupsMock = {};
  trayModesMock = {};
  scrollAreaPx = 4000;
  useAgentStateStore.setState({
    bySession: {},
    activityBySession: {},
    exitBySession: {},
    launchBySession: {},
    processAgentBySession: {},
  });
  rosterStatus = {};
  rosterLookup = null;
  listResult = [];
  lateRoster = null;
  runOutcomes = [];
  publishProjectionOwners({
    sessions: null,
    error: null,
    references: [],
    partial: false,
    homeTeam: null,
    teamOrder: [],
  });
  resetManagedSessionStoreForTests();
});

describe("AgentsList", () => {
  it("renders nothing while the terminal column is hidden", () => {
    listResult = [row({ handle: "one" })];
    publishProjectionOwners({ sessions: listResult, error: null });
    const { container } = render(<DockSlotHost teamKey="team-1" visible={false} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("lists this team's running agents; under its card a row's line two leaves out the workspace", async () => {
    workspaces = {
      "team-1": [
        {
          id: "ws-a",
          kind: "story",
          status: "ready",
          storyId: "SW-A-1",
          storyTitle: "Auth flow",
          storyStatus: "in-progress",
          storyFilePath: "/a.md",
        },
        { id: "ws-b" },
      ],
    };
    listResult = [row({ handle: "SW-A-1" }), row({ handle: "loose" })];
    terminalTabIndex = { "team-1": { "ws-b": [{ id: "tab-loose", sessionId: "sid-loose" }] } };
    renderList();
    await waitFor(() =>
      expect(document.querySelector('[data-agent-handle="SW-A-1"]')).not.toBeNull(),
    );
    const auth = rowOf("SW-A-1");
    // The card names the workspace and the agent runs here, so line two says neither; the
    // tooltip keeps both.
    expect(within(auth).getByTestId("agent-detail")).toHaveTextContent(/^Worker$/);
    expect(auth).toHaveAttribute("title", expect.stringContaining("Worker · SW-A-1 · This Mac"));
    expect(within(agentRowButton(/loose/)).getByTestId("agent-detail")).toHaveTextContent(
      /^Worker$/,
    );
    // No workspace sections: one tree.
    expect(screen.queryByRole("region", { name: "SW-A-1" })).toBeNull();
  });

  it("SW-AA-1: shows the agent's latest `signal_progress` note on the row", async () => {
    listResult = [row({ handle: "SW-A-1" }), row({ handle: "quiet" })];
    // Only the first agent reported progress — the other row stays silent.
    progressBySessionMock = {
      "sid-SW-A-1": { phase: "dev", note: "writing src/auth/login.tsx" },
    };
    renderList();
    await waitFor(() =>
      expect(document.querySelector('[data-agent-handle="SW-A-1"]')).not.toBeNull(),
    );
    const auth = rowOf("SW-A-1");
    const activity = within(auth).getByTestId("agent-activity");
    expect(activity).toHaveTextContent("writing src/auth/login.tsx");
    // A long note wraps to two lines rather than cutting off at one, and the hover holds it whole.
    expect(activity).toHaveClass("line-clamp-2");
    expect(activity).not.toHaveClass("truncate");
    expect(activity).toHaveAttribute("title", "writing src/auth/login.tsx");
    // A silent row renders no activity line (status dot carries the state).
    expect(within(agentRowButton(/quiet/)).queryByTestId("agent-activity")).toBeNull();
    expect(screen.queryByRole("region", { name: "Other" })).toBeNull();
  });

  it("clicking an agent reveals its terminal; the context menu no longer offers an Open panel action", async () => {
    terminalTabIndex = { "team-1": { "ws-a": [{ id: "tab-a", sessionId: "sid-SW-A-1" }] } };
    workspaces = { "team-1": [{ id: "ws-a" }] };
    listResult = [row({ handle: "SW-A-1" }), row({ handle: "SW-A-2" })];
    renderList();
    const first = await screen.findByRole("button", { name: /SW-A-1/ });
    fireEvent.click(first);
    expect(activateTrayWorkspace).toHaveBeenCalledWith("team-1", "ws-a");
    expect(setActiveTerminalTab).toHaveBeenCalledWith("team-1", "tab-a", "ws-a");
    // The right-click menu no longer carries the retired side-panel action.
    fireEvent.contextMenu(first);
    expect(screen.queryByRole("menuitem", { name: "Open panel" })).toBeNull();
    expect(await screen.findByRole("menuitem", { name: "Go to terminal" })).toBeInTheDocument();
  });

  it("clicking an agent reveals its existing tab without adopting a new one", async () => {
    terminalTabIndex = { "team-1": { "ws-a": [{ id: "tab-a", sessionId: "sid-SW-A-1" }] } };
    workspaces = { "team-1": [{ id: "ws-a" }] };
    listResult = [row({ handle: "SW-A-1" })];
    renderList();
    const button = await screen.findByRole("button", { name: /SW-A-1/ });
    fireEvent.click(button);
    expect(activateTrayWorkspace).toHaveBeenCalledWith("team-1", "ws-a");
    expect(setActiveTerminalTab).toHaveBeenCalledWith("team-1", "tab-a", "ws-a");
    expect(adoptTerminalSession).not.toHaveBeenCalled();
  });

  describe("every running agent is listed, and Show always lands", () => {
    it("shows an open team group's rows like this team's, and Show switches to it", async () => {
      const setActiveTeam = vi.fn();
      useOrgStore.setState({ activeOrgSlug: "acme", setActiveTeam, setActiveOrg: vi.fn() });
      terminalTabIndex = {
        "acme:ops": { "ws-x": [{ id: "tab-x", sessionId: "sid-elsewhere" }] },
      };
      workspaces = { "acme:ops": [{ id: "ws-x" }] };
      listResult = [row({ handle: "elsewhere" })];
      renderList();
      const other = await screen.findByRole("button", { name: agentRow(/elsewhere/) });
      // The group names the team, so the row's second line does not repeat it.
      expect(within(other).getByTestId("agent-detail")).toHaveTextContent(/^Worker/);
      fireEvent.click(other);
      expect(setActiveTeam).toHaveBeenCalledWith("ops");
      expect(setActiveTerminalTab).toHaveBeenCalledWith("acme:ops", "tab-x", "ws-x");
    });

    it("the quick view and the inbox chat on another team's agent peek: no team switches (G1)", async () => {
      const setActiveTeam = vi.fn();
      const setActiveOrg = vi.fn();
      useOrgStore.setState({ activeOrgSlug: "acme", setActiveTeam, setActiveOrg });
      terminalTabIndex = {
        "acme:ops": { "ws-x": [{ id: "tab-x", sessionId: "sid-elsewhere" }] },
      };
      workspaces = { "acme:ops": [{ id: "ws-x" }] };
      listResult = [row({ handle: "elsewhere" })];
      renderList();
      const other = await screen.findByRole("button", { name: agentRow(/elsewhere/) });
      fireEvent.keyDown(other, { key: "q" });
      expect(screen.getByTestId("agent-quick-send")).toBeInTheDocument();
      fireEvent.keyDown(document, { key: "Escape" });
      const [chat] = await screen.findAllByTestId("agent-row-message");
      fireEvent.click(chat);
      expect(screen.getByRole("dialog", { name: /^Messages with / })).toBeVisible();
      expect(setActiveTeam).not.toHaveBeenCalled();
      expect(setActiveOrg).not.toHaveBeenCalled();
      expect(activateTrayWorkspace).not.toHaveBeenCalled();
    });

    it("lists a routine-started agent at the top level with its routine on the second line", async () => {
      useRoutinesStore.setState({
        rows: [{ key: "acme/team-1/nightly", def: { name: "Nightly triage" } }] as never,
      });
      terminalTabIndex = {
        "team-1": {
          "ws-a": [
            { id: "tab-lead", sessionId: "sid-lead" },
            { id: "tab-n", sessionId: "sid-nightly" },
          ],
        },
      };
      workspaces = { "team-1": [{ id: "ws-a" }] };
      listResult = [
        row({ handle: "lead" }),
        row({
          handle: "nightly",
          launchedBy: "lead",
          provenance: { runId: "r-1", storyId: "", routineKey: "acme/team-1/nightly" },
        }),
      ];
      renderList();
      const nightly = await screen.findByRole("button", { name: agentRow(/nightly/) });
      // Listed at the top level, it names where it works, then who launched it; its pawn says
      // its role, so the line does not.
      expect(within(nightly).getByTestId("agent-detail")).toHaveTextContent(
        /^Routine · Nightly triage · lead/,
      );
      expect(screen.queryByRole("group", { name: "Launched by lead" })).toBeNull();
      useRoutinesStore.setState({ rows: null });
    });

    it("a headless agent says so and opens its run output, never 'isn't running'", async () => {
      listResult = [
        row({
          handle: "docs",
          kind: "headless",
          sessionId: "headless-1",
          provenance: { runId: "run-h1", storyId: "SW-2" },
        }),
      ];
      renderList();
      const docs = await screen.findByRole("button", { name: agentRow(/docs/) });
      expect(within(docs).getByTestId("agent-detail")).toHaveTextContent("Headless · run output");
      expect(within(docs).getByTestId("agent-headless-mark")).toBeInTheDocument();
      fireEvent.click(docs);
      await waitFor(() =>
        expect(openAgentResult).toHaveBeenCalledWith({
          run_id: "run-h1",
          sessionId: "headless-1",
          story_id: "SW-2",
        }),
      );
      expect(toast).not.toHaveBeenCalledWith("This agent isn't running.");
      fireEvent.contextMenu(docs);
      expect(await screen.findByRole("menuitem", { name: "Open run output" })).toBeInTheDocument();
    });

    it("a live agent with no tab reads Background, and Show adopts it into a tab", async () => {
      listResult = [row({ handle: "bg-agent" })];
      renderList();
      const bg = await screen.findByRole("button", { name: agentRow(/bg-agent/) });
      expect(within(bg).getByTestId("agent-detail")).toHaveTextContent(/^Background/);
      fireEvent.contextMenu(bg);
      expect(await screen.findByRole("menuitem", { name: "Show" })).toBeInTheDocument();
      expect(screen.queryByRole("menuitem", { name: "Bring to foreground" })).toBeNull();
      expect(screen.queryByRole("menuitem", { name: "Hide tab" })).toBeNull();
      fireEvent.click(bg);
      await waitFor(() =>
        expect(adoptTerminalSession).toHaveBeenCalledWith("team-1", "sid-bg-agent", undefined, {
          isCurrent: expect.any(Function),
        }),
      );
    });

    it.each([
      [0, "done", "ended"],
      [139, "crashed", "crashed"],
    ] as const)(
      "retains a known exit %s on an offline row and header",
      async (code, status, mark) => {
        useMachinesStore.setState({
          byTeam: {
            "team-1": [
              {
                machineId: "box",
                label: "Studio Mac",
                kind: "ssh",
                status: { state: "disconnected" },
              } as never,
            ],
          },
        });
        workspaces = { "team-1": [{ id: "ws-a" }] };
        terminalTabIndex = {
          "team-1": {
            "ws-a": [
              {
                id: "remote-ended",
                sessionId: "remote-ended",
                origin: { machineId: "box", member: "remote:box", handle: "h-1" },
              } as Tab,
            ],
          },
        };
        listResult = [
          row({ handle: "finished", sessionId: "remote-ended", effectiveStatus: status }),
        ];
        useAgentStateStore.getState().recordSignal("remote-ended", {
          tier: 1,
          signal: { kind: "claude-hook", event: "UserPromptSubmit" },
        });
        useAgentStateStore
          .getState()
          .recordProcess("remote-ended", { kind: "exited", code }, Infinity);
        renderList();
        const button = await screen.findByRole("button", { name: agentRow(/finished/) });
        expect(button.querySelector(".ag-avatar-dot")).toHaveAttribute("data-mark", mark);
        expect(button.closest(".ag-row")).toHaveAttribute("data-dead", "true");
        expect(button.getAttribute("title")).not.toContain("keeps running there");
        expect(
          screen.getByTestId("agents-workspace-group").querySelector(".ag-ws-count [data-mark]"),
        ).toHaveAttribute("data-mark", mark);
      },
    );

    it("an agent whose machine is offline is muted with its reason, can't be stopped, and Show says why", async () => {
      useMachinesStore.setState({
        byTeam: {
          "team-1": [
            {
              machineId: "box",
              label: "Studio Mac",
              kind: "ssh",
              status: { state: "disconnected" },
            } as never,
          ],
        },
      });
      workspaces = { "team-1": [{ id: "ws-a" }] };
      terminalTabIndex = {
        "team-1": {
          "ws-a": [
            {
              id: "remote-1",
              sessionId: "remote-1",
              origin: { machineId: "box", member: "remote:box", handle: "h-1" },
            } as Tab,
          ],
        },
      };
      listResult = [row({ handle: "far", sessionId: "remote-1" })];
      renderList();
      const far = await screen.findByRole("button", { name: agentRow(/far/) });
      // Line two keeps the role and workspace; the amber Offline tag says the machine is down.
      expect(within(far).getByText("Offline")).toBeInTheDocument();
      expect(within(far).getByTestId("agent-detail")).not.toHaveTextContent("reconnecting");
      expect(far.closest(".ag-row")).toHaveAttribute("data-offline", "true");
      expect(far.closest(".ag-row")).not.toHaveAttribute("data-dead");
      // The Offline tag on line one is the one offline indicator (no second marker repeats it).
      expect(within(far).queryByTestId("agent-offline-mark")).toBeNull();
      // the row's avatar dot flips to the shared `offline` mark —
      // the pane-ring wrapper carries `data-mark="offline"` and the inner
      // StatusMark draws the diagonal strike through the ring.
      const dot = far.querySelector<HTMLElement>(".ag-avatar-dot");
      expect(dot).not.toBeNull();
      expect(dot!.getAttribute("data-mark")).toBe("offline");
      expect(dot).toHaveAccessibleName("Machine offline");
      expect(dot!.querySelector("svg line")).not.toBeNull();
      fireEvent.click(far);
      await waitFor(() =>
        expect(toast).toHaveBeenCalledWith(
          "Studio Mac is offline, so far's tab shows how it last was; the agent may still be running there.",
          undefined,
        ),
      );
      // Its tab still comes up on its tile, as it last was, with the notice.
      expect(setActiveTerminalTab).toHaveBeenCalledWith("team-1", "remote-1", "ws-a");
      fireEvent.contextMenu(far);
      expect(await screen.findByRole("menuitem", { name: "Stop…" })).toHaveAttribute(
        "data-disabled",
      );
      // No quick view for an offline agent: the menu has no "Quick view" item, and the row has
      // no quick-view (terminal) button. The chat icon still opens its inbox.
      expect(screen.queryByRole("menuitem", { name: "Quick view" })).toBeNull();
      const farRow = far.closest(".ag-row") as HTMLElement;
      expect(within(farRow).queryByTestId("agent-row-cli")).toBeNull();
      expect(within(farRow).getByTestId("agent-row-message")).toBeInTheDocument();
      // Two hover actions are drawn, so line one makes room for two.
      expect(farRow).toHaveAttribute("data-actions", "2");
      expect(farRow.style.getPropertyValue("--ag-row-actions-n")).toBe("2");
      // The hover says what works offline: Enter or Space shows it, the inbox takes mail, and only
      // the quick view needs the machine; Q and Option-click are not offered.
      const hover = far.getAttribute("title")!;
      expect(hover).toContain("only the quick view needs the machine online");
      expect(hover).toContain("Click, Enter or Space to show it · M for its inbox");
      expect(hover).not.toMatch(/\bQ\b|Option-click/);
      // Q and Option-click do nothing on it.
      fireEvent.keyDown(far, { key: "q" });
      fireEvent.click(far, { altKey: true });
      expect(screen.queryByRole("dialog", { name: /^Quick view of / })).toBeNull();
    });

    it("Hide tab hides a shown agent's tab; the row stays", async () => {
      workspaces = { "team-1": [{ id: "ws-a" }] };
      terminalTabIndex = { "team-1": { "ws-a": [{ id: "tab-a", sessionId: "sid-shown" }] } };
      listResult = [row({ handle: "shown" })];
      renderList();
      const shown = await screen.findByRole("button", { name: agentRow(/shown/) });
      fireEvent.contextMenu(shown);
      expect(await screen.findByRole("menuitem", { name: "Go to terminal" })).toBeInTheDocument();
      fireEvent.click(screen.getByRole("menuitem", { name: "Hide tab" }));
      expect(hideTab).toHaveBeenCalledWith("team-1", "ws-a", "tab-a");
    });
  });

  describe("collapsed", () => {
    const dock = () => (
      <>
        <AgentsDockToggle teamKey="team-1" />
        <DockSlotHost teamKey="team-1" visible />
      </>
    );
    const renderDock = () => {
      publishProjectionOwners({ sessions: listResult, error: null });
      return render(dock());
    };
    const toggle = () => screen.getByRole("button", { name: /^Agents, / });

    it("hides the list entirely, leaving the tray's Agents control with the counts", async () => {
      listResult = [row({ handle: "one" }), row({ handle: "asker" })];
      awaitingAnswer("sid-asker");
      const { rerender } = renderDock();
      await screen.findByRole("button", { name: agentRow(/one/) });
      // The list is on screen, so the bar carries no second way to it.
      expect(screen.queryByRole("button", { name: /^Agents, / })).toBeNull();

      fireEvent.click(screen.getByRole("button", { name: "Hide agents" }));
      expect(setWorkspacePanelCollapsed).toHaveBeenCalledWith(true);
      rerender(dock());
      expect(screen.queryByRole("region", { name: "Agents" })).toBeNull();
      expect(screen.queryByRole("button", { name: agentRow(/one/) })).toBeNull();
      expect(toggle()).toHaveAttribute("aria-expanded", "false");
      expect(toggle()).toHaveAccessibleName("Agents, 2 running, 1 needs you");
      expect(toggle()).toHaveAttribute("title", "Show agents · 2 running, 1 needs you");
      expect(screen.getByTestId("agents-toggle-needs")).toHaveTextContent("1");
      // Folding from the list's header lands focus on the control that brings it back.
      expect(toggle()).toHaveFocus();
    });

    it("opens from the tray control, moving focus into the list, and steps aside", async () => {
      listResult = [row({ handle: "one" })];
      dockCollapsed = true;
      const { rerender } = renderDock();
      expect(screen.queryByRole("region", { name: "Agents" })).toBeNull();
      expect(screen.queryByTestId("agents-toggle-needs")).toBeNull();

      fireEvent.click(toggle());
      expect(setWorkspacePanelCollapsed).toHaveBeenLastCalledWith(false);
      expect(revealTerminalInPlace).not.toHaveBeenCalled();
      rerender(dock());
      const list = screen.getByRole("region", { name: "Agents" });
      expect(screen.queryByRole("button", { name: /^Agents, / })).toBeNull();
      await waitFor(() => expect(list).toContainElement(document.activeElement as HTMLElement));
    });

    it("stays on the bar while the CLI is closed, even with the list unfolded, and reopens both", () => {
      listResult = [row({ handle: "asker" }), row({ handle: "busy" })];
      awaitingAnswer("sid-asker");
      terminalVisible = false;
      dockCollapsed = false;
      publishProjectionOwners({ sessions: listResult, error: null });
      // The list is gone with the terminal column; the control is the way back.
      render(<AgentsDockToggle teamKey="team-1" />);
      expect(toggle()).toHaveAttribute("aria-expanded", "false");
      expect(toggle()).toHaveAccessibleName("Agents, 2 running, 1 needs you");
      expect(screen.getByTestId("agents-toggle-needs")).toHaveTextContent("1");
      fireEvent.click(toggle());
      expect(revealTerminalInPlace).toHaveBeenCalledWith();
      expect(setWorkspacePanelCollapsed).toHaveBeenLastCalledWith(false);
    });

    it("hands focus into the list when the CLI comes back while the control has focus", async () => {
      listResult = [row({ handle: "one" })];
      terminalVisible = false;
      dockCollapsed = false;
      publishProjectionOwners({ sessions: listResult, error: null });
      const both = (visible: boolean) => (
        <>
          <AgentsDockToggle teamKey="team-1" />
          <DockSlotHost teamKey="team-1" visible={visible} />
        </>
      );
      const { rerender } = render(both(false));
      toggle().focus();
      // ⌘\ (or a pill) reopens the CLI: the control steps aside.
      terminalVisible = true;
      rerender(both(true));
      const list = await screen.findByRole("region", { name: "Agents" });
      expect(screen.queryByRole("button", { name: /^Agents, / })).toBeNull();
      await waitFor(() => expect(list).toContainElement(document.activeElement as HTMLElement));
    });

    it("leaves a grip on the border that reopens the list, like the other panes", () => {
      listResult = [row({ handle: "one" })];
      dockCollapsed = true;
      renderList();
      expect(screen.queryByRole("region", { name: "Agents" })).toBeNull();
      const grip = screen.getByTestId("agents-width-divider");
      expect(grip).toHaveAttribute("role", "button");
      expect(grip).toHaveAttribute("aria-expanded", "false");
      expect(grip).toHaveAccessibleName("Expand agents");
      // It is the row's first child at the window edge (overflow hidden): no
      // start offset to clip it, and a full grab area on the terminal side.
      expect(grip.style.marginLeft).toBe("0px");
      expect(grip.style.paddingLeft).toBe("4px");
      expect(grip.style.paddingRight).toBe("8px");
      fireEvent.click(grip);
      expect(setWorkspacePanelCollapsed).toHaveBeenLastCalledWith(false);
    });

    it("leaves no grip while the CLI column itself is closed", () => {
      dockCollapsed = true;
      publishProjectionOwners({ sessions: [], error: null });
      render(<DockSlotHost teamKey="team-1" visible={false} />);
      expect(screen.queryByTestId("agents-width-divider")).toBeNull();
    });

    it("keeps the border's click zone narrow on the terminal side", async () => {
      listResult = [row({ handle: "one" })];
      renderList();
      const edge = await screen.findByRole("separator", { name: "Resize agents" });
      // Narrower than the shared 7px either side, and narrowest over the terminal.
      expect(edge.style.paddingLeft).toBe("5px");
      expect(edge.style.paddingRight).toBe("2px");
      expect(edge.style.marginLeft).toBe("-5px");
      expect(edge.style.marginRight).toBe("-2px");
    });

    it("brings the terminal column back when opening the list from a hidden one", () => {
      terminalVisible = false;
      dockCollapsed = true;
      renderDock();
      fireEvent.click(toggle());
      expect(revealTerminalInPlace).toHaveBeenCalledWith();
      expect(setWorkspacePanelCollapsed).toHaveBeenLastCalledWith(false);
    });
  });

  it("stops an agent only after confirming", async () => {
    listResult = [row({ handle: "worker" })];
    renderList();
    fireEvent.contextMenu(await screen.findByRole("button", { name: /worker/ }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Stop…" }));
    expect(dashboardKillByHandle).not.toHaveBeenCalled();
    fireEvent.click(await screen.findByRole("button", { name: "Stop" }));
    await waitFor(() => expect(dashboardKillByHandle).toHaveBeenCalledWith("worker"));
  });

  it("asks before stopping a Weft worker that has not declared done, and marks it on confirm", async () => {
    listResult = [row({ handle: "dispatched" })];
    rosterStatus = { "sid-dispatched": "dispatch" };
    renderList();
    fireEvent.contextMenu(await screen.findByRole("button", { name: /dispatched/ }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Stop…" }));
    expect(await screen.findByText("Worker has not declared done")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Stop anyway" }));
    await waitFor(() => expect(dashboardKillByHandle).toHaveBeenCalledWith("dispatched"));
    expect(wvWorkerForceClose).toHaveBeenCalledWith("dispatched");
  });

  it("opens the Stop confirm only once the dispatch lookup settles", async () => {
    let settle: (v: { status: string } | null) => void = () => {};
    rosterLookup = () =>
      new Promise((resolve) => {
        settle = resolve;
      });
    listResult = [row({ handle: "slow" })];
    renderList();
    fireEvent.contextMenu(await screen.findByRole("button", { name: /slow/ }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Stop…" }));
    // Nothing to confirm yet: no plain "Stop" a click could land on before the lookup.
    expect(screen.queryByRole("button", { name: "Stop" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Stop anyway" })).toBeNull();
    settle({ status: "dispatch" });
    expect(await screen.findByText("Worker has not declared done")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Stop" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Stop anyway" }));
    await waitFor(() => expect(dashboardKillByHandle).toHaveBeenCalledWith("slow"));
    expect(wvWorkerForceClose).toHaveBeenCalledWith("slow");
  });

  it("stops an agent with no Weft dispatch row without force-closing it", async () => {
    listResult = [row({ handle: "plain" })];
    renderList();
    fireEvent.contextMenu(await screen.findByRole("button", { name: /plain/ }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Stop…" }));
    fireEvent.click(await screen.findByRole("button", { name: "Stop" }));
    await waitFor(() => expect(dashboardKillByHandle).toHaveBeenCalledWith("plain"));
    expect(wvWorkerForceClose).not.toHaveBeenCalled();
  });

  describe("agent filter", () => {
    const sessions = () => [
      row({ handle: "triage-1", agentRevision: "acme/backend/triage@3" }),
      row({ handle: "triage-2", agentRevision: "acme/backend/triage@2" }),
      row({ handle: "scribe-1", agentRevision: "acme/backend/scribe@1" }),
    ];

    it("shows only the filtered agent's sessions, any revision, behind a visible chip", async () => {
      listResult = sessions();
      agentsListFilter = { agentKey: "acme/backend/triage", name: "Triage" };
      renderList();
      expect(await screen.findByRole("button", { name: /triage-1/ })).toBeInTheDocument();
      expect(screen.getByRole("button", { name: /triage-2/ })).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: /scribe-1/ })).toBeNull();
      expect(screen.getByText("Agent: Triage")).toBeInTheDocument();
    });

    it("clearing the chip shows every agent again", async () => {
      listResult = sessions();
      agentsListFilter = { agentKey: "acme/backend/triage", name: "Triage" };
      const { rerender } = renderList();
      fireEvent.click(await screen.findByRole("button", { name: "Clear agent filter" }));
      expect(clearAgentsListFilter).toHaveBeenCalled();
      rerender(<DockSlotHost teamKey="team-1" visible />);
      expect(screen.queryByText("Agent: Triage")).toBeNull();
      expect(screen.getByRole("button", { name: /scribe-1/ })).toBeInTheDocument();
    });

    it("says when the filtered agent has nothing running", async () => {
      listResult = [row({ handle: "scribe-1", agentRevision: "acme/backend/scribe@1" })];
      agentsListFilter = { agentKey: "acme/backend/triage", name: "Triage" };
      renderList();
      expect(await screen.findByText("No Triage sessions running")).toBeInTheDocument();
    });

    it("keeps a filtered child visible when its launcher is filtered out", async () => {
      listResult = [
        row({ handle: "lead", agentRevision: "acme/backend/scribe@1" }),
        row({ handle: "child", launchedBy: "lead", agentRevision: "acme/backend/triage@3" }),
      ];
      agentsListFilter = { agentKey: "acme/backend/triage", name: "Triage" };
      renderList();
      expect(await screen.findByRole("button", { name: /child/ })).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: /^lead/ })).toBeNull();
    });
  });

  describe("an agent named controller", () => {
    it("is listed in the tree like any other agent: no Controller section, no Start card", async () => {
      listResult = [
        row({ handle: CTL, displayName: "Controller", agentRevision: "acme/team-1/controller@1" }),
        row({ handle: "manager", launchedBy: CTL }),
      ];
      renderList();
      const ctl = await screen.findByRole("button", { name: agentRow(/Controller/) });
      // A root of the agents tree, not a pinned section above it.
      expect(ctl.closest("[data-row-index]")).toHaveAttribute("data-row-kind", "agent");
      expect(screen.queryByRole("region", { name: "Controller" })).toBeNull();
      expect(screen.queryByRole("button", { name: /Start Controller/ })).toBeNull();
      // Its role comes from the launch tree like any agent's: it launched one, so a manager.
      expect(ctl.querySelector(".ag-piece")).toHaveAttribute(
        "title",
        "Hierarchy: Manager · launched 1",
      );
      expect(ctl.querySelector(".ag-face")).toHaveAttribute("data-face");
      // Role and Show under are offered, as for every agent.
      fireEvent.contextMenu(ctl);
      expect(await screen.findByRole("menuitem", { name: "Role" })).toBeInTheDocument();
      expect(screen.getByRole("menuitem", { name: "Face" })).toBeInTheDocument();
    });

    it("shows nothing for a team with no agents running but the empty state", async () => {
      listResult = [];
      renderList();
      act(() => publishProjectionOwners({ sessions: [] }));
      expect(await screen.findByText("No agents running")).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: /Start Controller/ })).toBeNull();
    });
  });

  describe("search and status filter", () => {
    const team = () => [
      row({ handle: "controller", startedAtMs: 1 }),
      row({ handle: "manager", launchedBy: "controller", startedAtMs: 2 }),
      row({
        handle: "worker-a",
        launchedBy: "manager",
        startedAtMs: 3,
        startedWith: "Verify on staging",
      }),
      row({ handle: "worker-b", launchedBy: "manager", startedAtMs: 4, status: "done" }),
      row({ handle: "solo", startedAtMs: 0 }),
    ];
    const rowName = (re: RegExp) => (name: string) =>
      re.test(name) && !/^(Expand|Collapse|Select) /.test(name);
    const rowButton = (re: RegExp) => screen.getByRole("button", { name: rowName(re) });
    const search = () => screen.getByRole("searchbox", { name: "Find agents" });

    it("finds agents by workspace and machine too, and offers Clear filters on no match", async () => {
      workspaces = {
        "team-1": [{ id: "ws-a", kind: "story", status: "ready", storyTitle: "Auth flow" }],
      };
      terminalTabIndex = { "team-1": { "ws-a": [{ id: "tab-s", sessionId: "sid-solo" }] } };
      listResult = team();
      renderList();
      await screen.findByRole("button", { name: rowName(/solo/) });
      fireEvent.change(search(), { target: { value: "auth" } });
      expect(rowButton(/solo/)).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: rowName(/manager/) })).toBeNull();
      fireEvent.change(search(), { target: { value: "this mac" } });
      expect(rowButton(/worker-b/)).toBeInTheDocument();
      fireEvent.change(search(), { target: { value: "nowhere-at-all" } });
      expect(screen.getByText("Nothing matches")).toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: "Clear search, filters and teams" }));
      expect(search()).toHaveValue("");
      expect(rowButton(/solo/)).toBeInTheDocument();
    });

    it("counts All, Working and Needs you, and narrows to each", async () => {
      listResult = team();
      hooked("sid-controller", "sid-manager", "sid-worker-a");
      awaitingAnswer("sid-solo");
      renderList();
      await screen.findByRole("button", { name: rowName(/solo/) });
      expect(showOption("All", 5)).toHaveAttribute("aria-checked", "true");
      expect(showOption("Working", 3)).toBeInTheDocument();
      const needs = showOption("Needs you", 1);
      // The exited worker-b is Idle: the three buckets add up to All.
      expect(showOption("Ready", 1)).toBeInTheDocument();

      fireEvent.click(showOption("Working", 3));
      expect(rowButton(/worker-a/)).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: rowName(/worker-b/) })).toBeNull();
      expect(screen.queryByRole("button", { name: rowName(/solo/) })).toBeNull();

      fireEvent.click(needs);
      expect(rowButton(/solo/)).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: rowName(/worker-a/) })).toBeNull();
    });
  });

  describe("Needs you", () => {
    const team = () => [
      row({ handle: "controller", startedAtMs: 1 }),
      row({ handle: "manager", launchedBy: "controller", startedAtMs: 2 }),
      row({ handle: "worker-a", launchedBy: "manager", startedAtMs: 3 }),
      row({ handle: "worker-b", launchedBy: "manager", startedAtMs: 4 }),
      row({ handle: "solo", startedAtMs: 0 }),
    ];
    const ask = (sessionId: string) => act(() => awaitingAnswer(sessionId));
    // A row's own button, not its expand/collapse toggle.
    const rowName = (re: RegExp) => (name: string) =>
      re.test(name) && !/^(Expand|Collapse|Select) /.test(name);
    const rowButton = (re: RegExp) => screen.getByRole("button", { name: rowName(re) });
    const needsFilter = () => showOption("Needs you");

    it("badges a row waiting on an answer and one waiting on approval, and counts both", async () => {
      listResult = [
        row({ handle: "asker" }),
        row({ handle: "gated", status: "paused", effectiveStatus: "paused" }),
        row({ handle: "busy" }),
      ];
      awaitingAnswer("sid-asker");
      renderList();
      await screen.findByRole("button", { name: /asker/ });
      expect(within(rowButton(/asker/)).getByTestId("needs-you-badge")).toHaveAccessibleName(
        "Needs answer",
      );
      expect(within(rowButton(/gated/)).getByTestId("needs-you-badge")).toHaveAccessibleName(
        "Needs approval",
      );
      expect(rowButton(/gated/)).toHaveTextContent("Waiting for your approval");
      expect(within(rowButton(/busy/)).queryByTestId("needs-you-badge")).toBeNull();
      expect(showCount("Needs you")).toBe(2);
    });

    it("shows the question a row waits on, under its details", async () => {
      listResult = [row({ handle: "asker" })];
      useManagedSessionStore.setState({
        states: { "sid-asker": "awaiting-answer" },
        pendingAsks: { q1: { sessionId: "sid-asker", question: "Bump the TTL first?" } },
      } as never);
      projectSessionStatus("sid-asker", "awaiting-answer");
      renderList();
      const asker = await screen.findByRole("button", { name: rowName(/asker/) });
      expect(asker.querySelector(".ag-ask")).toHaveTextContent("Bump the TTL first?");
    });

    it("updates the badge and counts when a session starts waiting, without a roster reload", async () => {
      listResult = team();
      renderList();
      await screen.findByRole("button", { name: /worker-a/ });
      expect(screen.queryByTestId("needs-you-badge")).toBeNull();
      expect(showCount("Needs you")).toBe(0);

      ask("sid-worker-a");
      expect(within(rowButton(/worker-a/)).getByTestId("needs-you-badge")).toBeInTheDocument();
      expect(showCount("Needs you")).toBe(1);

      ask("sid-solo");
      expect(showCount("Needs you")).toBe(2);
    });

    it("filters to waiting agents, and back to all", async () => {
      listResult = team();
      awaitingAnswer("sid-worker-a");
      renderList();
      await screen.findByRole("button", { name: /solo/ });
      fireEvent.click(needsFilter());
      expect(showOption("Needs you")).toHaveAttribute("aria-checked", "true");
      expect(rowButton(/worker-a/)).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: rowName(/worker-b/) })).toBeNull();
      expect(screen.queryByRole("button", { name: rowName(/solo/) })).toBeNull();

      fireEvent.click(showOption("All"));
      expect(rowButton(/solo/)).toBeInTheDocument();
      expect(rowButton(/worker-b/)).toBeInTheDocument();
    });

    it("keeps the Show filter when the team changes: the list covers every team", async () => {
      listResult = [row({ handle: "asker" }), row({ handle: "busy" })];
      awaitingAnswer("sid-asker");
      const { rerender } = renderList();
      await screen.findByRole("button", { name: rowName(/busy/) });
      fireEvent.click(needsFilter());
      expect(screen.queryByRole("button", { name: rowName(/busy/) })).toBeNull();

      rerender(<DockSlotHost teamKey="team-2" visible />);
      expect(showOption("Needs you")).toHaveAttribute("aria-checked", "true");
      expect(screen.queryByRole("button", { name: rowName(/busy/) })).toBeNull();

      rerender(<DockSlotHost teamKey="team-1" visible />);
      expect(showOption("Needs you")).toHaveAttribute("aria-checked", "true");
    });

    it("composes with the agent filter chip", async () => {
      listResult = [
        row({ handle: "triage-1", agentRevision: "acme/backend/triage@3" }),
        row({ handle: "triage-2", agentRevision: "acme/backend/triage@3" }),
        row({ handle: "scribe-1", agentRevision: "acme/backend/scribe@1" }),
      ];
      agentsListFilter = { agentKey: "acme/backend/triage", name: "Triage" };
      awaitingAnswer("sid-triage-2", "sid-scribe-1");
      renderList();
      await screen.findByRole("button", { name: /triage-1/ });
      // The menu counts what the chip leaves, as the rows do: scribe's wait is not counted.
      expect(showCount("Needs you")).toBe(1);
      expect(screen.getByTestId("agents-count")).toHaveTextContent(/^2\/3$/);
      fireEvent.click(needsFilter());
      expect(rowButton(/triage-2/)).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: /triage-1/ })).toBeNull();
      expect(screen.queryByRole("button", { name: /scribe-1/ })).toBeNull();
    });

    it("says so when nothing needs you under the filter", async () => {
      listResult = [row({ handle: "busy" })];
      renderList();
      await screen.findByRole("button", { name: rowName(/busy/) });
      fireEvent.click(needsFilter());
      // Only the Show filter narrows the list: the empty state names the empty bucket.
      expect(screen.getByText("No agents need you")).toBeInTheDocument();
      expect(screen.queryByText("Nothing matches")).toBeNull();
      fireEvent.click(screen.getByRole("button", { name: "Show all agents" }));
      expect(paneSearch.status).toBe("all");
    });

    it("a click on a waiting row reveals its terminal tab; Enter opens the quick-send popup instead", async () => {
      terminalTabIndex = { "team-1": { "ws-a": [{ id: "tab-a", sessionId: "sid-asker" }] } };
      workspaces = { "team-1": [{ id: "ws-a" }] };
      listResult = [row({ handle: "asker" })];
      awaitingAnswer("sid-asker");
      renderList();
      const asker = await screen.findByRole("button", { name: /asker/ });
      fireEvent.keyDown(asker, { key: "Enter" });
      expect(screen.getByRole("dialog", { name: "Quick view of asker" })).toBeInTheDocument();
      expect(activateTrayWorkspace).not.toHaveBeenCalled();
      fireEvent.click(asker);
      expect(activateTrayWorkspace).toHaveBeenCalledWith("team-1", "ws-a");
      expect(setActiveTerminalTab).toHaveBeenCalledWith("team-1", "tab-a", "ws-a");
    });

    it("Q on a focused row opens the quick-send popup for that agent", async () => {
      listResult = [row({ handle: "pinger" })];
      renderList();
      const pinger = await screen.findByRole("button", { name: /pinger/ });
      pinger.focus();
      fireEvent.keyDown(pinger, { key: "q" });
      expect(screen.getByRole("dialog", { name: "Quick view of pinger" })).toBeInTheDocument();
      fireEvent.keyDown(document, { key: "Escape" });
      expect(screen.queryByRole("dialog", { name: "Quick view of pinger" })).toBeNull();
      expect(pinger).toHaveFocus();
    });

    it("shows the waiting count in amber on the tray control while the list is folded", () => {
      listResult = [row({ handle: "asker" }), row({ handle: "busy" })];
      dockCollapsed = true;
      awaitingAnswer("sid-asker");
      publishProjectionOwners({ sessions: listResult, error: null });
      render(<AgentsDockToggle teamKey="team-1" />);
      expect(screen.getByTestId("agents-toggle-needs")).toHaveClass("ag-count-needs");
      expect(screen.getByTestId("agents-toggle-needs")).toHaveTextContent("1");
      expect(
        screen.getByRole("button", { name: "Agents, 2 running, 1 needs you" }),
      ).toBeInTheDocument();
    });
  });

  it("gives each agent a face with its role piece badged on it, the launcher a manager's", async () => {
    listResult = [row({ handle: "lead" }), row({ handle: "dev", launchedBy: "lead" })];
    renderList();
    await waitFor(() => expect(rowOf("lead")).toBeInTheDocument());
    const lead = rowOf("lead");
    expect(lead.querySelector(".ag-avatar .ag-face img")).not.toBeNull();
    expect(lead.querySelector(".ag-avatar .ag-piece")).toHaveAttribute(
      "title",
      "Hierarchy: Manager · launched 1",
    );
    expect(rowOf("dev").querySelector(".ag-piece")).toHaveAttribute("title", "Hierarchy: Worker");
  });

  it("shows the role and avatar a launch assigned, through the roster's live union", async () => {
    listResult = [
      row({ handle: "SW-1", launchedBy: null }),
      row({
        handle: "SW-1#reviewer",
        launchedBy: "SW-1",
        role: "reviewer",
        avatar: "builtin-funny:skeptical-goat",
      }),
      row({ handle: "SW-1#worker-1", launchedBy: "SW-1" }),
    ];
    renderList();
    // The durable ledger never carries launch identity; reload unions it from the live rows.
    await act(() => useAgentListProjectionStore.getState().reload());
    const reviewer = await screen.findByRole("button", { name: agentRow(/Reviewer|reviewer/) });
    expect(reviewer.querySelector(".ag-piece")).toHaveAttribute("title", "Hierarchy: Reviewer");
    expect(reviewer.querySelector(".ag-face")).toHaveAttribute(
      "data-face",
      "builtin-funny:skeptical-goat",
    );
    // Without them, the derived role and the fallback face apply.
    const worker = agentRowButton(/worker/i);
    expect(worker.querySelector(".ag-piece")).toHaveAttribute("title", "Hierarchy: Worker");
    expect(worker.querySelector(".ag-face img")).not.toBeNull();
    expect(
      agentRowButton(/^(?!.*(reviewer|worker)).*SW-1/i).querySelector(".ag-piece"),
    ).toHaveAttribute("title", "Hierarchy: Manager · launched 2");
  });

  it("marks due the remote session a routine's wake is queued on, never a local one (SW-AGR-5)", async () => {
    useMachinesStore.setState({
      byTeam: { "team-1": [{ machineId: "box", label: "Build box", kind: "ssh" } as never] },
    });
    // The wake waits on the box's session (a remote tab, its origin naming the machine).
    useRoutinesStore.setState({
      rows: [
        {
          key: "acme/team-1/morning-triage",
          def: { name: "Morning triage" },
          queued: {
            runId: "run-1",
            machineId: "box",
            sessionId: "remote-sess-1",
            marker: "Morning triage",
            deadlineMs: null,
            unconfirmed: false,
          },
        },
      ] as never,
    });
    workspaces = { "team-1": [{ id: "ws-a" }] };
    terminalTabIndex = {
      "team-1": {
        "ws-a": [
          {
            id: "remote-sess-1",
            sessionId: "remote-sess-1",
            name: "Worker 1",
            origin: { machineId: "box", member: "remote:box", handle: "remote-tab-1" },
          } as Tab,
        ],
      },
    };
    listResult = [
      row({ handle: "remote-tab-1", sessionId: "remote-sess-1", displayName: "Worker 1" }),
    ];
    renderList();
    const remote = await screen.findByRole("button", { name: agentRow(/Worker 1/) });
    const due = within(remote).getByTestId("agent-due-mark");
    expect(due).toHaveAccessibleName("Wake queued: Morning triage");
    // The same wake queued on This Mac's session of that id doesn't mark the remote one.
    act(() =>
      useRoutinesStore.setState((st) => ({
        rows: (st.rows ?? []).map((r) => ({ ...r, queued: { ...r.queued!, machineId: "local" } })),
      })),
    );
    expect(within(remote).queryByTestId("agent-due-mark")).toBeNull();
    useRoutinesStore.setState({ rows: null });
  });

  it("lists an agent launched on a machine under its launcher, with its face, role and machine", async () => {
    // The row the roster gets for an agent launched on a machine: no local session, so
    // its session is the one its tab runs under in the machine's layout (that tab is
    // projected into this team's story workspace, its origin naming the machine).
    useMachinesStore.setState({
      byTeam: {
        "team-1": [{ machineId: "box", label: "Build box", kind: "ssh" } as never],
      },
    });
    workspaces = {
      "team-1": [
        { id: "sw-1", kind: "story", status: "ready", storyId: "SW-1", storyTitle: "Login" },
      ],
    };
    terminalTabIndex = {
      "team-1": {
        "sw-1": [
          { id: "tab-lead", sessionId: "sid-lead" },
          {
            id: "remote-sess-1",
            sessionId: "remote-sess-1",
            name: "Worker 1",
            origin: { machineId: "box", member: "remote:box", handle: "remote-tab-1" },
          } as Tab,
        ],
      },
    };
    listResult = [
      row({ handle: "lead" }),
      row({
        handle: "remote-tab-1",
        sessionId: "remote-sess-1",
        kind: "registered",
        status: "running",
        displayName: "Worker 1",
        launchedBy: "lead",
        role: "reviewer",
        avatar: "builtin-funny:skeptical-goat",
        story: "SW-1",
      }),
      // An agent the remote one launched names it by its session id.
      row({ handle: "SW-1#helper", launchedBy: "remote-sess-1" }),
    ];
    renderList();
    const remote = await screen.findByRole("button", { name: agentRow(/Worker 1/) });
    expect(remote.querySelector(".ag-piece")).toHaveAttribute("title", "Hierarchy: Reviewer");
    expect(remote.querySelector(".ag-face")).toHaveAttribute(
      "data-face",
      "builtin-funny:skeptical-goat",
    );
    expect(within(remote).getByTestId("agent-detail")).toHaveTextContent("Reviewer · Build box");
    expect(remote).not.toHaveTextContent(/Errored|Background/);
    const helper = within(rowOf("SW-1#helper").closest<HTMLElement>("[data-row-index]")!).getByRole(
      "button",
      {
        name: agentRow(/helper/i),
      },
    );
    // The helper runs here: its line two names no machine, its tooltip says This Mac.
    expect(within(helper).getByTestId("agent-detail")).not.toHaveTextContent(/This Mac|Build box/);
    expect(helper).toHaveAttribute("title", expect.stringContaining("This Mac"));
  });

  it("names the machine of a remote agent whose tab has not arrived yet, from its roster row", async () => {
    // The machine's layout is still loading: no tab carries the agent's origin, only the
    // roster's launch record names its machine.
    useMachinesStore.setState({
      byTeam: {
        "team-1": [
          {
            machineId: "box",
            label: "Build box",
            kind: "ssh",
            status: { state: "connected" },
          } as never,
        ],
      },
    });
    workspaces = { "team-1": [{ id: "ws-a" }] };
    terminalTabIndex = { "team-1": { "ws-a": [{ id: "tab-here", sessionId: "sid-here" }] } };
    listResult = [
      row({ handle: "here", sessionId: "sid-here" }),
      row({
        handle: "remote-tab-1",
        sessionId: "remote-sess-1",
        kind: "registered",
        status: "running",
        displayName: "Worker 1",
        role: "reviewer",
        machineId: "box",
      }),
    ];
    renderList();
    const remote = await screen.findByRole("button", { name: agentRow(/Worker 1/) });
    expect(within(remote).getByTestId("agent-row-machine")).not.toHaveAttribute("data-offline");
    expect(within(remote).getByTestId("agent-detail")).toHaveTextContent(/Build box$/);
    expect(remote).toHaveAttribute("title", expect.not.stringContaining("This Mac"));
    // A local row beside it still reads This Mac.
    const here = await screen.findByRole("button", { name: agentRow(/here/) });
    expect(within(here).queryByTestId("agent-row-machine")).toBeNull();
    expect(here).toHaveAttribute("title", expect.stringContaining("This Mac"));
  });

  it.each(["unreachable", "disconnected"])(
    "a remote agent whose tab has not arrived, on a machine that is %s, reads offline and unknown and needs no one",
    async (state) => {
      useMachinesStore.setState({
        byTeam: {
          // Another team, listed first, has the same machine as connected: the row reads its
          // own launch team's list, not the first that names the machine.
          "acme:ops": [
            {
              machineId: "box",
              label: "Build box",
              kind: "ssh",
              status: { state: "connected" },
            } as never,
          ],
          "team-1": [
            { machineId: "box", label: "Build box", kind: "ssh", status: { state } } as never,
          ],
        },
      });
      workspaces = { "team-1": [{ id: "ws-a" }] };
      listResult = [
        row({
          handle: "remote-tab-1",
          sessionId: "remote-sess-1",
          kind: "registered",
          status: "running",
          displayName: "Worker 1",
          machineId: "box",
          launchTeamKey: "team-1",
        }),
      ];
      // What it last said was a permission prompt.
      act(() =>
        useAgentStateStore.getState().recordSignal("remote-sess-1", {
          tier: 1,
          signal: { kind: "claude-hook", event: "PermissionRequest" },
        }),
      );
      renderList();
      const remote = await screen.findByRole("button", { name: agentRow(/Worker 1/) });
      expect(
        within(remote.closest(".ag-row") as HTMLElement).getByTestId("agent-row-machine"),
      ).toHaveAttribute("data-offline", "true");
      expect(remote.closest(".ag-row")).toHaveAttribute("data-offline", "true");
      expect(within(remote).getByText("Offline")).toBeInTheDocument();
      // Its status is unknown: no status word, no needs-you badge, nothing counted as needing you.
      expect(within(remote).queryByTestId("needs-you-badge")).toBeNull();
      expect(within(remote).queryByText(/Needs you/)).toBeNull();
      expect(showCount("Needs you")).toBe(0);
    },
  );

  describe("status from the signals the tab dots read", () => {
    type Event = "UserPromptSubmit" | "PreToolUse" | "PermissionRequest" | "Stop" | "StopFailure";
    const hookOn = (sid: string, event: Event) =>
      act(() =>
        useAgentStateStore
          .getState()
          .recordSignal(sid, { tier: 1, signal: { kind: "claude-hook", event } }),
      );
    const rowName = (re: RegExp) => (name: string) =>
      re.test(name) && !/^(Expand|Collapse|Select) /.test(name);
    const rowButton = (re: RegExp) => screen.getByRole("button", { name: rowName(re) });
    const dotOf = (re: RegExp) => rowButton(re).querySelector(".ag-row-dot")!;
    const filterCount = (name: string) =>
      showCount(name === "All" ? "All" : name === "Idle" ? "Ready" : name);

    it("counts a permission prompt, an error and a crash as Needs you, and paints them so", async () => {
      listResult = [
        row({ handle: "gate" }),
        row({ handle: "oops" }),
        row({ handle: "dead" }),
        row({ handle: "busy" }),
      ];
      await hookOn("sid-gate", "PermissionRequest");
      await hookOn("sid-oops", "StopFailure");
      await hookOn("sid-dead", "UserPromptSubmit");
      act(() => {
        useAgentStateStore
          .getState()
          .recordPtyExit("sid-dead", { code: 139, signal: null, requested: false });
      });
      await hookOn("sid-busy", "PreToolUse");
      projectSessionStatus("sid-gate", "paused");
      projectSessionStatus("sid-oops", "failed");
      projectSessionStatus("sid-dead", "crashed");
      projectSessionStatus("sid-busy", "tool-running");
      renderList();
      await screen.findByRole("button", { name: rowName(/gate/) });
      expect(within(rowButton(/gate/)).getByTestId("needs-you-badge")).toHaveAccessibleName(
        "Needs approval",
      );
      expect(within(rowButton(/oops/)).getByTestId("needs-you-badge")).toHaveAccessibleName(
        "Error",
      );
      expect(within(rowButton(/dead/)).getByTestId("needs-you-badge")).toHaveAccessibleName(
        "Crashed",
      );
      expect(dotOf(/oops/)).toHaveAttribute("data-mark", "error");
      // a PreToolUse record reads as `tool-running` ("Running a tool"),
      // the finer sibling of the generic "Working" label.
      expect(dotOf(/busy/)).toHaveAccessibleName("Running a tool");
      expect(filterCount("Needs you")).toBe(3);
      expect(filterCount("Working")).toBe(1);
      expect(filterCount("Working") + filterCount("Needs you") + filterCount("Idle")).toBe(
        filterCount("All"),
      );
    });

    it("a hand-started Claude and a launched one that finished a turn look and count the same: Idle", async () => {
      workspaces = { "team-1": [{ id: "ws-a", kind: "story", status: "ready" }] };
      terminalTabIndex = {
        "team-1": { "ws-a": [{ id: "tab-hand", sessionId: "pty-hand", name: "by hand" }] },
      };
      listResult = [
        row({
          handle: "pty-hand",
          sessionId: "pty-hand",
          kind: "hand-started",
          rowNames: { primary: "by hand", secondary: null, full: "by hand" },
          detected: { cli: "claude", dot: null },
        }),
        row({ handle: "launched" }),
      ];
      await hookOn("pty-hand", "UserPromptSubmit");
      await hookOn("pty-hand", "Stop");
      await hookOn("sid-launched", "UserPromptSubmit");
      await hookOn("sid-launched", "Stop");
      useAgentStateStore.getState().setProcessAgent("pty-hand", "claude");
      useAgentStateStore.getState().setProcessAgent("sid-launched", "claude");
      projectSessionStatus("pty-hand", "idle");
      projectSessionStatus("sid-launched", "idle");
      renderList();
      await screen.findByRole("button", { name: rowName(/by hand/) });
      for (const re of [/by hand/, /launched/]) {
        expect(dotOf(re)).toHaveAccessibleName("Ready");
        expect(dotOf(re)).toHaveAttribute("data-mark", "ready");
      }
      expect(filterCount("Idle")).toBe(2);
      expect(filterCount("Working")).toBe(0);
    });

    it("a hand-started agent's pending question counts as Needs you", async () => {
      workspaces = { "team-1": [{ id: "ws-a", kind: "story", status: "ready" }] };
      terminalTabIndex = {
        "team-1": { "ws-a": [{ id: "tab-hand", sessionId: "pty-hand", name: "by hand" }] },
      };
      listResult = [
        row({
          handle: "pty-hand",
          sessionId: "pty-hand",
          kind: "hand-started",
          rowNames: { primary: "by hand", secondary: null, full: "by hand" },
          detected: { cli: "claude", dot: null },
        }),
      ];
      await hookOn("pty-hand", "UserPromptSubmit");
      act(() =>
        useManagedSessionStore.setState({
          pendingAsks: { q1: { correlationId: "q1", sessionId: "pty-hand", question: "Ship?" } },
        }),
      );
      projectSessionStatus("pty-hand", "awaiting-answer");
      renderList();
      await screen.findByRole("button", { name: rowName(/by hand/) });
      expect(within(rowButton(/by hand/)).getByTestId("needs-you-badge")).toHaveAccessibleName(
        "Needs answer",
      );
      expect(filterCount("Needs you")).toBe(1);
    });

    it.each(["cozy"] as const)(
      "shows last activity age instead of stale Doing copy when stalled ",
      async () => {
        const now = Date.now();
        listResult = [row({ handle: "silent" })];
        progressBySessionMock = { "sid-silent": { phase: "dev", note: "writing the old file" } };
        await hookOn("sid-silent", "UserPromptSubmit");
        const rec = useAgentStateStore.getState().bySession["sid-silent"];
        useAgentStateStore.setState({
          bySession: { "sid-silent": { ...rec, activityAt: now - 20 * 60_000 } },
          outputAtBySession: { "sid-silent": now - 12 * 60_000 },
        });
        projectSessionStatus("sid-silent", "stalled", { activityAtMs: now - 12 * 60_000 });
        renderList();
        await screen.findByRole("button", { name: rowName(/silent/) });
        expect(dotOf(/silent/)).toHaveAttribute("data-mark", "stalled");
        expect(within(rowButton(/silent/)).getByTestId("agent-activity")).toHaveTextContent(
          "Last activity 12m ago",
        );
        expect(rowButton(/silent/)).not.toHaveTextContent("writing the old file");
        expect(rowButton(/silent/).getAttribute("title")).toContain("Last activity 12m ago");
        expect(rowButton(/silent/).getAttribute("title")).not.toContain("Doing:");
        useAgentStateStore.setState({ outputAtBySession: {} });
      },
    );

    it("an agent nothing reports on reads Activity unknown, dashed, and counts as Idle", async () => {
      listResult = [row({ handle: "quiet" })];
      renderList();
      await screen.findByRole("button", { name: rowName(/quiet/) });
      expect(dotOf(/quiet/)).toHaveAccessibleName("Activity unknown");
      expect(dotOf(/quiet/)).toHaveAttribute("data-mark", "unknown");
      expect(rowButton(/quiet/).getAttribute("title")).toMatch(/hooks are not connected/);
      expect(filterCount("Idle")).toBe(1);
      expect(filterCount("Working")).toBe(0);
    });

    it("signal_progress blocked renders 'Blocked: {note}', NOT 'Waiting for your approval'", async () => {
      // The regression: `signal_progress {phase:"blocked"}` reused the
      // permission dot and the row read "Waiting for your approval" — the
      // permission copy, pinned on a session the operator never asked to
      // approve. carries blocked as its own needs-you kind.
      listResult = [row({ handle: "worker-b" })];
      await act(async () =>
        useAgentStateStore.getState().recordSignal("sid-worker-b", {
          tier: 1,
          signal: { kind: "signal-progress", phase: "blocked" },
        }),
      );
      progressBySessionMock = {
        ...progressBySessionMock,
        "sid-worker-b": {
          phase: "blocked",
          note: "disk full — invoking bmad-build fails",
        },
      };
      projectSessionStatus("sid-worker-b", "blocked");
      renderList();
      const r = await screen.findByRole("button", { name: rowName(/worker-b/) });
      expect(r).toHaveTextContent("Blocked: disk full — invoking bmad-build fails");
      expect(r).not.toHaveTextContent("Waiting for your approval");
      const dot = within(r).getByTestId("needs-you-badge");
      expect(dot).toHaveAttribute("data-mark", "blocked");
      expect(dot).toHaveAccessibleName("Blocked");
      expect(filterCount("Needs you")).toBe(1);
    });

    it.each(["cozy"] as const)(
      "qualifies recovering row and header without replacing their marks ",
      async () => {
        listResult = [row({ handle: "recovering" })];
        useAgentStateStore.getState().recordActivity(
          {
            session: "sid-recovering",
            identity: { hostBootId: "boot", sessionIncarnation: "pty", invocationId: "run" },
            revision: 1,
            turnGeneration: 1,
            provider: "claude",
            state: "working",
            observed: true,
            limited: false,
            exit: null,
            asks: [],
            toolOpen: true,
            activityAgeMs: 0,
          },
          Infinity,
        );
        projectSessionStatus("sid-recovering", "tool-running");
        renderList();
        const button = await screen.findByRole("button", { name: rowName(/recovering/) });
        for (const freshness of ["recovering", "unavailable"] as const) {
          act(() =>
            useAgentStateStore.getState().activityUnavailable(["sid-recovering"], freshness),
          );
          const qualifier =
            freshness === "recovering" ? "Restoring agent activity" : "Agent activity unavailable";
          expect(dotOf(/recovering/)).toHaveAttribute("data-mark", "tool");
          expect(dotOf(/recovering/)).toHaveAccessibleName(new RegExp(qualifier));
          expect(button).toHaveAccessibleName(/last observation may be stale/);
          expect(button.getAttribute("title")).toContain(qualifier);
          const header = screen.getByTestId("agents-workspace-group");
          const count = header.querySelector(".ag-ws-count")!;
          expect(count).toHaveAccessibleName(new RegExp(qualifier));
          expect(count.getAttribute("title")).toContain(qualifier);
          expect(count.querySelector("[data-mark]")).toHaveAttribute("data-mark", "tool");
        }
      },
    );

    it("a row whose own turn ended reads Working · N subagents while they run, then Ready", async () => {
      listResult = [row({ handle: "parent" })];
      const snapshot = (revision: number, subagentsOpen?: number) => ({
        session: "sid-parent",
        identity: { hostBootId: "boot", sessionIncarnation: "pty", invocationId: "run" },
        revision,
        turnGeneration: 1,
        provider: "claude" as const,
        state: "done" as const,
        observed: true,
        limited: false,
        exit: null,
        asks: [],
        toolOpen: false,
        activityAgeMs: 0,
        subagentsOpen,
      });
      useAgentStateStore.getState().recordActivity(snapshot(1, 4), Infinity);
      projectSessionStatus("sid-parent", "thinking", { subagents: 4 });
      renderList();
      const button = await screen.findByRole("button", { name: rowName(/parent/) });
      expect(dotOf(/parent/)).toHaveAttribute("data-mark", "thinking");
      expect(dotOf(/parent/)).toHaveAccessibleName("Working · 4 subagents");
      expect(button.getAttribute("title")).toContain("parent · Working · 4 subagents");
      act(() => {
        useAgentStateStore.getState().recordActivity(snapshot(2), Infinity);
        projectSessionStatus("sid-parent", "idle", { subagents: 0 });
      });
      expect(dotOf(/parent/)).toHaveAttribute("data-mark", "ready");
      expect(button.getAttribute("title")).not.toContain("subagent");
    });

    it("an idle row whose CLI release is unqualified reads Limited, not Ready, and says why", async () => {
      const note =
        "This CLI version (2.0.1) isn't supported yet (supported: OpenCode 1.18 or a newer 1.x release). Status, inbox wake and usage may not work.";
      listResult = [
        row({
          handle: "parent",
          releaseQualified: false,
          releaseNote: note,
          detected: { cli: "opencode", dot: null },
        }),
      ];
      projectSessionStatus("sid-parent", "idle");
      renderList();
      const button = await screen.findByRole("button", { name: rowName(/parent/) });
      expect(dotOf(/parent/)).toHaveAccessibleName(expect.stringContaining("Limited"));
      expect(dotOf(/parent/)).not.toHaveAccessibleName(expect.stringContaining("Ready"));
      expect(button.getAttribute("title")).toContain("parent · Limited");
      // The row says it on screen too, so keyboard users are not left with a hover-only reason.
      expect(
        within(button.closest(".ag-row") as HTMLElement).getByTestId("agent-limited-note"),
      ).toHaveTextContent("Release not supported");
      expect(button.getAttribute("aria-description") ?? button.getAttribute("title")).toContain(
        note,
      );
      expect(button.getAttribute("title")).toContain(note);
      expect(button.getAttribute("title")).not.toContain("Ready");
    });

    it("line one says Working and a mark counts the subagents; the prompt gives way first", async () => {
      listResult = [row({ handle: "parent", startedWith: "Own the navigation story end to end" })];
      useAgentStateStore.getState().recordActivity(
        {
          session: "sid-parent",
          identity: { hostBootId: "boot", sessionIncarnation: "pty", invocationId: "run" },
          revision: 1,
          turnGeneration: 1,
          provider: "claude",
          state: "working",
          observed: true,
          limited: false,
          exit: null,
          asks: [],
          toolOpen: true,
          activityAgeMs: 0,
          subagentsOpen: 3,
        },
        Infinity,
      );
      projectSessionStatus("sid-parent", "tool-running", { subagents: 3 });
      renderList();
      const button = await screen.findByRole("button", { name: rowName(/parent/) });
      const mark = within(button).getByRole("img", { name: "3 subagents running" });
      expect(mark).toHaveAttribute("title", "3 subagents running");
      expect(mark).toHaveTextContent("3");
      const title = button.querySelector(".ag-title")!;
      expect(title).toHaveClass("ag-title-split");
      // Line one: the name, "Working", the mark, then the launch prompt (which truncates first).
      expect(title.querySelector(".ag-title-lead")).toHaveTextContent(/^parent · Working$/);
      expect(title.querySelector(".ag-title-rest")).toHaveTextContent(
        "· Own the navigation story end to end",
      );
      expect(title).not.toHaveTextContent("subagents");
      // The tooltip keeps the full words.
      expect(button.getAttribute("title")).toContain("parent · Working · 3 subagents");
      // The quick view's header says it as the row does.
      fireEvent.keyDown(button, { key: "q" });
      const quick = await screen.findByTestId("agent-quick-send");
      const head = quick.querySelector(".ag-quick-send-meta")!;
      expect(head).toHaveTextContent(/^Working/);
      expect(within(quick).getByRole("img", { name: "3 subagents running" })).toBeInTheDocument();
    });

    it("a subsequent hook suppresses the obsolete blocked progress note", async () => {
      listResult = [row({ handle: "resumed" })];
      act(() =>
        useAgentStateStore.getState().recordSignal("sid-resumed", {
          tier: 1,
          signal: { kind: "signal-progress", phase: "blocked" },
        }),
      );
      progressBySessionMock = { "sid-resumed": { phase: "blocked", note: "disk full" } };
      projectSessionStatus("sid-resumed", "blocked");
      renderList();
      const r = await screen.findByRole("button", { name: rowName(/resumed/) });
      expect(r).toHaveTextContent("Blocked: disk full");
      await hookOn("sid-resumed", "PreToolUse");
      act(() => projectSessionStatus("sid-resumed", "tool-running"));
      expect(r).not.toHaveTextContent("disk full");
      expect(r.getAttribute("title")).not.toContain("disk full");
    });

    it("a real PermissionRequest still renders 'Waiting for your approval' (NOT blocked)", async () => {
      listResult = [row({ handle: "worker-c" })];
      await hookOn("sid-worker-c", "PermissionRequest");
      projectSessionStatus("sid-worker-c", "paused");
      renderList();
      const r = await screen.findByRole("button", { name: rowName(/worker-c/) });
      expect(r).toHaveTextContent("Waiting for your approval");
      expect(r).not.toHaveTextContent(/^Blocked/);
      const dot = within(r).getByTestId("needs-you-badge");
      expect(dot).toHaveAttribute("data-mark", "permission");
      expect(dot).toHaveAccessibleName("Needs approval");
    });

    it("a subsequent progress tick clears the blocked state", async () => {
      // "a subsequent hook event or progress tick clears it."
      listResult = [row({ handle: "worker-d" })];
      await act(async () =>
        useAgentStateStore.getState().recordSignal("sid-worker-d", {
          tier: 1,
          signal: { kind: "signal-progress", phase: "blocked" },
        }),
      );
      progressBySessionMock = {
        ...progressBySessionMock,
        "sid-worker-d": { phase: "blocked", note: "disk full" },
      };
      projectSessionStatus("sid-worker-d", "blocked");
      renderList();
      const r = await screen.findByRole("button", { name: rowName(/worker-d/) });
      expect(r).toHaveTextContent("Blocked: disk full");
      // A progress tick on another phase clears the agent-state record off blocked.
      await act(async () =>
        useAgentStateStore.getState().recordSignal("sid-worker-d", {
          tier: 1,
          signal: { kind: "signal-progress", phase: "dev" },
        }),
      );
      act(() => projectSessionStatus("sid-worker-d", "thinking"));
      progressBySessionMock = {
        ...progressBySessionMock,
        "sid-worker-d": { phase: "dev", note: "running tests" },
      };
      // The row no longer carries the Blocked line; it is back at work.
      expect(within(rowButton(/worker-d/)).queryByTestId("needs-you-badge")).toBeNull();
      expect(rowButton(/worker-d/)).not.toHaveTextContent(/^Blocked/);
    });

    it.each(["cozy"] as const)("dead rows are distinct from Ready ", async () => {
      // the shipped bug collapsed exited/crashed sessions onto the same
      // slate as ready/idle. The new vocabulary paints `crashed` as a filled
      // square (loud) distinct from the `ready` hollow ring, so a reader with
      // no colour perception still tells them apart.
      listResult = [
        row({ handle: "ready-one" }),
        row({ handle: "crashed-one" }),
        row({ handle: "ended-one", status: "done" }),
      ];
      await hookOn("sid-ready-one", "Stop");
      await hookOn("sid-ended-one", "Stop");
      act(() =>
        useAgentStateStore
          .getState()
          .recordPtyExit("sid-ended-one", { code: 0, signal: null, requested: false }),
      );
      await hookOn("sid-crashed-one", "UserPromptSubmit");
      act(() =>
        useAgentStateStore
          .getState()
          .recordPtyExit("sid-crashed-one", { code: 139, signal: null, requested: false }),
      );
      projectSessionStatus("sid-ready-one", "idle");
      projectSessionStatus("sid-ended-one", "done");
      projectSessionStatus("sid-crashed-one", "crashed");
      renderList();
      await screen.findByRole("button", { name: rowName(/ready-one/) });
      expect(dotOf(/ready-one/)!.getAttribute("data-mark")).toBe("ready");
      expect(dotOf(/crashed-one/)!.getAttribute("data-mark")).toBe("crashed");
      expect(dotOf(/ended-one/)!.getAttribute("data-mark")).toBe("ended");
      expect(rowButton(/ready-one/).closest(".ag-row")).not.toHaveAttribute("data-dead");
      for (const name of [/crashed-one/, /ended-one/]) {
        expect(rowButton(name).closest(".ag-row")).toHaveAttribute("data-dead", "true");
      }
      // The two marks differ by shape, not merely by colour.
      expect(dotOf(/ready-one/)!.querySelector("svg circle[fill='none']")).not.toBeNull();
      expect(dotOf(/crashed-one/)!.querySelector("svg rect[fill='currentColor']")).not.toBeNull();
    });
  });

  describe("agents started by hand in a terminal tab", () => {
    const hook = (sid: string, event: "PermissionRequest" | "PreToolUse") =>
      act(() =>
        useAgentStateStore
          .getState()
          .recordSignal(sid, { tier: 1, signal: { kind: "claude-hook", event } }),
      );
    const handWorld = () => {
      workspaces = {
        "team-1": [{ id: "ws-a", kind: "story", status: "ready", storyTitle: "Auth" }],
      };
      terminalTabIndex = {
        "team-1": {
          "ws-a": [
            { id: "tab-hand", sessionId: "pty-hand", name: "remote codesensemcp" },
            { id: "tab-zsh", sessionId: "pty-zsh", name: "zsh" },
          ],
        },
      };
      // Rust promotes the detected CLI; the other tab remains a bare shell.
      listResult = [
        row({
          handle: "pty-hand",
          sessionId: "pty-hand",
          kind: "hand-started",
          rowNames: {
            primary: "remote codesensemcp",
            secondary: null,
            full: "remote codesensemcp",
          },
          detected: { cli: "claude", dot: null, processOnly: true },
        }),
        row({ handle: "pty-zsh", sessionId: "pty-zsh", kind: "live-pty" }),
      ];
    };

    it("lists a detected agent as a full agent row, counted; a plain shell is not listed", async () => {
      handWorld();
      await hook("pty-hand", "PermissionRequest");
      projectSessionStatus("pty-hand", "paused");
      renderList();
      const agent = await screen.findByRole("button", { name: agentRow(/remote codesensemcp/) });
      expect(agent.querySelector(".ag-face img")).not.toBeNull();
      expect(agent.querySelector(".ag-piece")).toHaveAttribute("title", "Hierarchy: Worker");
      expect(agent).toHaveTextContent("Waiting for your approval");
      // The pawn says Worker; line two says what runs it.
      expect(within(agent).getByTestId("agent-detail")).toHaveTextContent(/^Claude/);
      expect(within(agent).getByTestId("needs-you-badge")).toBeInTheDocument();
      expect(showCount("Needs you")).toBe(1);
      expect(screen.queryByRole("button", { name: "zsh" })).toBeNull();
    });

    it("says which CLI runs and what it is doing, even before the roster has a row", async () => {
      handWorld();
      // No launch-registry row is needed: the session owner projects the detected process.
      await hook("pty-hand", "PreToolUse");
      projectSessionStatus("pty-hand", "tool-running");
      renderList();
      const agent = await screen.findByRole("button", { name: agentRow(/remote codesensemcp/) });
      // a PreToolUse record reads as `tool-running` ("Running a tool"),
      // the finer sibling of the generic "Working" shipped before — the row's
      // task line shows the finer label. Line two names the CLI, so line one does not repeat it.
      expect(agent.querySelector(".ag-task")).toHaveTextContent("Running a tool");
      expect(agent.querySelector(".ag-task")).not.toHaveTextContent("Claude");
      expect(within(agent).getByTestId("agent-detail")).toHaveTextContent("Claude");
      expect(showCount("Needs you")).toBe(0);
    });

    it("leaves the list when its agent ends: the tab is a plain shell again", async () => {
      handWorld();
      await hook("pty-hand", "PreToolUse");
      renderList();
      await screen.findByRole("button", { name: agentRow(/remote codesensemcp/) });
      await act(() =>
        useAgentStateStore.getState().recordSignal("pty-hand", {
          tier: 1,
          signal: { kind: "claude-hook", event: "SessionEnd" },
        }),
      );
      act(() => publishProjectionOwners({ sessions: [] }));
      expect(await screen.findByText("No agents running")).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: /remote codesensemcp/ })).toBeNull();
    });

    it("lists a reattached Claude and a Codex from their running processes, with no hook at all", async () => {
      handWorld();
      terminalTabIndex["team-1"]["ws-a"].push({
        id: "tab-codex",
        sessionId: "pty-codex",
        name: "codex review",
      });
      listResult = [
        ...listResult,
        row({
          handle: "pty-codex",
          sessionId: "pty-codex",
          kind: "hand-started",
          rowNames: { primary: "codex review", secondary: null, full: "codex review" },
          detected: { cli: "codex", dot: null, processOnly: true },
        }),
      ];
      // The tab-process probe found them; no hook or signal ever arrived this run.
      act(() => {
        useAgentStateStore.getState().setProcessAgent("pty-hand", "claude");
        useAgentStateStore.getState().setProcessAgent("pty-codex", "codex");
      });
      renderList();
      const claude = await screen.findByRole("button", { name: agentRow(/remote codesensemcp/) });
      expect(claude.querySelector(".ag-face img")).not.toBeNull();
      // Nothing reports what they are doing: line one is the name alone, line two names the CLI.
      expect(claude.querySelector(".ag-task")).toBeNull();
      expect(within(claude).getByTestId("agent-detail")).toHaveTextContent("Claude");
      const codex = screen.getByRole("button", { name: agentRow(/codex review/) });
      expect(codex.querySelector(".ag-face img")).not.toBeNull();
      expect(codex.querySelector(".ag-task")).toBeNull();
      expect(within(codex).getByTestId("agent-detail")).toHaveTextContent("Codex");
      // The zsh tab runs no agent.
      expect(screen.queryByRole("button", { name: agentRow(/zsh/) })).toBeNull();

      // Codex exits: its tab is a plain shell again.
      act(() => {
        useAgentStateStore.getState().setProcessAgent("pty-codex", null);
        publishProjectionOwners({
          sessions: listResult.filter((row) => row.handle !== "pty-codex"),
        });
      });
      await waitFor(() =>
        expect(screen.queryByRole("button", { name: agentRow(/codex review/) })).toBeNull(),
      );
    });

    it("a hook still adds needs-you on top of an agent found by its process", async () => {
      handWorld();
      act(() => useAgentStateStore.getState().setProcessAgent("pty-hand", "claude"));
      await hook("pty-hand", "PermissionRequest");
      projectSessionStatus("pty-hand", "paused");
      renderList();
      const agent = await screen.findByRole("button", { name: agentRow(/remote codesensemcp/) });
      expect(agent).toHaveTextContent("Waiting for your approval");
      expect(within(agent).getByTestId("needs-you-badge")).toBeInTheDocument();
    });

    it("wears a manager's piece once it launches agents", async () => {
      handWorld();
      listResult = [...listResult, row({ handle: "child", launchedBy: "pty-hand" })];
      await hook("pty-hand", "PreToolUse");
      renderList();
      await screen.findAllByRole("button", { name: agentRow(/remote codesensemcp/) });
      expect(rowOf("pty-hand").querySelector(".ag-piece")).toHaveAttribute(
        "title",
        "Hierarchy: Manager · launched 1",
      );
    });
  });

  describe("chosen role and face", () => {
    it("shows the role and face a person chose over the launch's and the derived ones", async () => {
      listResult = [
        row({ handle: "lead" }),
        row({
          handle: "dev",
          launchedBy: "lead",
          role: "worker",
          avatar: "builtin-funny:fine-dog",
        }),
      ];
      agentIdentity = {
        "team-1": { dev: { role: "qa", face: "builtin-crest:red-bolt" }, lead: { role: "worker" } },
      };
      renderList();
      await waitFor(() => expect(rowOf("dev")).toBeInTheDocument());
      const dev = rowOf("dev");
      expect(dev.querySelector(".ag-piece")).toHaveAttribute("title", "Hierarchy: QA");
      expect(dev.querySelector(".ag-face")).toHaveAttribute("data-face", "builtin-crest:red-bolt");
      expect(rowOf("lead").querySelector(".ag-piece")).toHaveAttribute(
        "title",
        "Hierarchy: Worker",
      );
    });

    it("lists the row's own team's packs in its Face menu, and offers no share for another team", async () => {
      const { useAvatarPacksStore } = await import("@/shared/hooks/use-avatar-packs");
      type AvatarPackView = import("@/tauri/commands").AvatarPackView;
      const teamAvatars = (packs: AvatarPackView[]) => ({
        packs,
        defaultPack: null,
        agentAvatars: {},
        warnings: [],
      });
      // The row's team is the list's team ("team-1"), which is not the active team here.
      useAvatarPacksStore.setState({
        byTeam: {
          "team-1": teamAvatars([
            {
              id: "yard",
              name: "Yard Crew",
              scope: "team",
              faces: [{ id: "ricky", ref: "yard:ricky", label: "Ricky", src: "data:," }],
            },
          ]),
        },
      });
      listResult = [row({ handle: "dev" })];
      renderList();
      fireEvent.contextMenu(await screen.findByRole("button", { name: agentRow(/dev/) }));
      fireEvent.click(await screen.findByRole("menuitem", { name: "Face" }));
      fireEvent.click(await screen.findByRole("menuitem", { name: /Yard Crew/ }));
      expect(screen.queryByRole("menuitem", { name: "Share with team" })).toBeNull();
      expect(screen.queryByRole("menuitem", { name: "Team default pack" })).toBeNull();
      useAvatarPacksStore.setState({ byTeam: {} });
    });

    it("shares one of your own packs with the active team, and marks a shared copy", async () => {
      const { useAvatarPacksStore } = await import("@/shared/hooks/use-avatar-packs");
      const prev = useOrgStore.getState();
      useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "ops" });
      const pack = (over: object) => ({
        scope: "personal",
        faces: [{ id: "f", ref: "x:f", label: "F", src: "data:," }],
        warnings: [],
        ...over,
      });
      useAvatarPacksStore.setState({
        byTeam: {
          "acme:ops": {
            packs: [
              pack({ id: "mine", name: "My Pack" }),
              pack({ id: "copied", name: "Copied Pack", personal_copy: true }),
            ] as never,
            defaultPack: null,
            agentAvatars: {},
            warnings: [],
          },
        },
      });
      listResult = [row({ handle: "dev" })];
      renderList("acme:ops");
      fireEvent.contextMenu(await screen.findByRole("button", { name: agentRow(/dev/) }));
      fireEvent.click(await screen.findByRole("menuitem", { name: "Face" }));
      fireEvent.click(await screen.findByRole("menuitem", { name: /My Pack/ }));
      fireEvent.click(await screen.findByRole("menuitem", { name: "Share with team" }));
      expect(avatarPackShareWithTeam).toHaveBeenCalledWith("mine");
      fireEvent.contextMenu(agentRowButton(/dev/));
      fireEvent.click(await screen.findByRole("menuitem", { name: "Face" }));
      fireEvent.click(await screen.findByRole("menuitem", { name: /Copied Pack/ }));
      expect(await screen.findByRole("menuitem", { name: "Shared with team ✓" })).toHaveAttribute(
        "data-disabled",
      );
      useAvatarPacksStore.setState({ byTeam: {} });
      useOrgStore.setState({
        activeOrgSlug: prev.activeOrgSlug,
        activeTeamName: prev.activeTeamName,
      });
    });

    it("sets the team default pack for the active team", async () => {
      const { useAvatarPacksStore } = await import("@/shared/hooks/use-avatar-packs");
      const prev = useOrgStore.getState();
      useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "ops" });
      useAvatarPacksStore.setState({
        byTeam: {
          "acme:ops": {
            packs: [
              {
                id: "yard",
                name: "Yard Crew",
                scope: "team",
                faces: [{ id: "ricky", ref: "yard:ricky", label: "Ricky", src: "data:," }],
              },
            ] as never,
            defaultPack: null,
            agentAvatars: {},
            warnings: [],
          },
        },
      });
      listResult = [row({ handle: "dev" })];
      renderList("acme:ops");
      fireEvent.contextMenu(await screen.findByRole("button", { name: agentRow(/dev/) }));
      fireEvent.click(await screen.findByRole("menuitem", { name: "Face" }));
      fireEvent.click(await screen.findByRole("menuitem", { name: "Team default pack" }));
      fireEvent.click(await screen.findByRole("menuitemradio", { name: "Yard Crew" }));
      expect(avatarDefaultPackSet).toHaveBeenCalledWith("yard");
      await waitFor(() =>
        expect(useAvatarPacksStore.getState().byTeam["acme:ops"]?.defaultPack).toBe("yard"),
      );
      useAvatarPacksStore.setState({ byTeam: {} });
      useOrgStore.setState({
        activeOrgSlug: prev.activeOrgSlug,
        activeTeamName: prev.activeTeamName,
      });
    });

    it("assigns a role and a face from the row's context menu", async () => {
      listResult = [row({ handle: "dev" })];
      renderList();
      fireEvent.contextMenu(await screen.findByRole("button", { name: agentRow(/dev/) }));
      fireEvent.click(await screen.findByRole("menuitem", { name: "Role" }));
      fireEvent.click(await screen.findByRole("menuitemradio", { name: "Reviewer" }));
      expect(setAgentIdentity).toHaveBeenCalledWith("team-1", "dev", { role: "reviewer" });

      fireEvent.contextMenu(agentRowButton(/dev/));
      fireEvent.click(await screen.findByRole("menuitem", { name: "Face" }));
      fireEvent.click(await screen.findByRole("menuitem", { name: "Funny originals" }));
      fireEvent.click(await screen.findByRole("menuitemradio", { name: "Raccoon in a hard hat" }));
      expect(setAgentIdentity).toHaveBeenCalledWith("team-1", "dev", {
        face: "builtin-funny:hardhat-raccoon",
      });

      fireEvent.contextMenu(agentRowButton(/dev/));
      fireEvent.click(await screen.findByRole("menuitem", { name: "Face" }));
      fireEvent.click(await screen.findByRole("menuitemradio", { name: "Auto" }));
      expect(setAgentIdentity).toHaveBeenLastCalledWith("team-1", "dev", { face: null });
    });

    it("numbers agents wearing the same face by start time; a unique face has no number", async () => {
      listResult = [
        row({ handle: "second", startedAtMs: 20, avatar: "builtin-funny:tie-cat" }),
        row({ handle: "first", startedAtMs: 10, avatar: "builtin-funny:tie-cat" }),
        row({ handle: "own", startedAtMs: 5, avatar: "builtin-funny:fine-dog" }),
      ];
      renderList();
      const tagOf = (re: RegExp) =>
        agentRowButton(re).querySelector("[data-testid=agent-face-tag]")?.textContent ?? null;
      await screen.findByRole("button", { name: agentRow(/second/) });
      expect(tagOf(/first/)).toBe("1");
      expect(tagOf(/second/)).toBe("2");
      expect(tagOf(/\bown\b/)).toBeNull();
    });

    it("drops looks chosen for sessions that are gone once the roster loads, in every team", async () => {
      terminalTabIndex = {
        "team-1": { "ws-a": [{ id: "t", sessionId: "pty-shell" }] },
        "acme:ops": { "ws-x": [{ id: "t-x", sessionId: "pty-ops" }] },
      };
      agentIdentity = {
        "team-1": { dev: { face: "🦊" }, gone: { face: "🐙" } },
        "acme:ops": { "ended-ops": { role: "reviewer" } },
      };
      listResult = [row({ handle: "dev" })];
      renderList();
      await screen.findByRole("button", { name: agentRow(/dev/) });
      expect(pruneAgentIdentity).toHaveBeenCalledTimes(2);
      const calls = new Map(
        (pruneAgentIdentity.mock.calls as unknown as [string, Set<string>][]).map(
          ([team, keep]) => [team, [...keep].sort()],
        ),
      );
      expect(calls.get("team-1")).toEqual(["dev", "pty-shell"]);
      // Another team's choices are pruned by that team's own tabs.
      expect(calls.get("acme:ops")).toEqual(["dev", "pty-ops"]);
    });
  });

  describe("launch and message", () => {
    const manager = () =>
      row({ handle: CTL, displayName: "Manager", agentRevision: "acme/team-1/manager@1" });

    beforeEach(() => useLaunchSheetStore.setState({ request: null }));

    it("+ in the header opens the launch sheet for this team", async () => {
      listResult = [manager()];
      renderList();
      fireEvent.click(await screen.findByRole("button", { name: "Launch agent" }));
      expect(useLaunchSheetStore.getState().request).toEqual({ teamKey: "team-1" });
    });

    it("right-click a manager to launch under it", async () => {
      listResult = [
        manager(),
        row({ handle: "SW-1#lead", launchedBy: CTL, story: "SW-1", agentRevision: "t/x/lead@1" }),
      ];
      hooked(...listResult.flatMap((r) => (r.sessionId ? [r.sessionId] : [])));
      renderList();
      fireEvent.contextMenu(
        await screen.findByRole("button", { name: agentRow(/^Thinking Manager/) }),
      );
      fireEvent.click(await screen.findByRole("menuitem", { name: "Launch agent under Manager…" }));
      expect(useLaunchSheetStore.getState().request).toEqual({
        teamKey: "team-1",
        managerHandle: CTL,
        storyId: undefined,
      });

      fireEvent.contextMenu(agentRowButton(/SW-1 · Lead/));
      fireEvent.click(await screen.findByRole("menuitem", { name: /^Launch agent under / }));
      expect(useLaunchSheetStore.getState().request).toMatchObject({
        managerHandle: "SW-1#lead",
        storyId: "SW-1",
      });
    });

    it("the row's terminal icon opens the quick view for that agent; the chat icon opens its inbox chat and leaves the CLI where it is", async () => {
      terminalTabIndex = {
        "team-1": { "ws-a": [{ id: "tab-a", sessionId: "sid-SW-1#triage" }] },
      };
      workspaces = { "team-1": [{ id: "ws-a" }] };
      listResult = [
        row({ handle: "SW-1#triage", agentRevision: "acme/backend/triage@3" }),
        row({ handle: "bare" }),
      ];
      renderList();
      const [triage] = await screen.findAllByTestId("agent-row-cli");
      fireEvent.click(triage);
      expect(
        screen.getByRole("dialog", { name: "Quick view of SW-1 · Triage" }),
      ).toBeInTheDocument();
      // The quick view leaves the CLI on the user's current tab; the inbox waits on the agent's.
      expect(activateTrayWorkspace).not.toHaveBeenCalled();
      expect(useAgentInboxRailStore.getState().bySession["sid-SW-1#triage"]).toBeUndefined();
      fireEvent.keyDown(document, { key: "Escape" });
      useAgentInboxRailStore.setState({ bySession: {} });

      const [chat] = await screen.findAllByTestId("agent-row-message");
      fireEvent.click(chat);
      expect(screen.getByRole("dialog", { name: "Messages with SW-1 · Triage" })).toBeVisible();
      // One popup at a time, and the CLI stays put: no tab is shown, no rail opened.
      expect(screen.queryByTestId("agent-quick-send")).toBeNull();
      expect(activateTrayWorkspace).not.toHaveBeenCalled();
      expect(useAgentInboxRailStore.getState().bySession["sid-SW-1#triage"]).toBeUndefined();
      expect(listSessionInbox).toHaveBeenCalledWith("SW-1#triage");
    });

    describe("the inbox chat", () => {
      const TEAM = { org: "acme", team: "backend" };
      const msg = (over: Record<string, unknown>) => ({
        team: TEAM,
        fromName: null,
        toName: null,
        toRole: null,
        contentType: "markdown",
        inReplyTo: null,
        via: null,
        replyTo: null,
        summary: null,
        unread: false,
        delivery: null,
        kind: "update",
        ...over,
      });
      beforeEach(() => {
        terminalTabIndex = {
          "team-1": { "ws-a": [{ id: "tab-a", sessionId: "sid-SW-1#triage" }] },
        };
        workspaces = { "team-1": [{ id: "ws-a" }] };
        listResult = [row({ handle: "SW-1#triage", agentRevision: "acme/backend/triage@3" })];
        sessionInboxMessages = [
          msg({ id: 1, from: "human", to: "SW-1#triage", text: "Where are we?", createdAtMs: 1 }),
          msg({
            id: 2,
            from: "SW-1#triage",
            to: "human",
            text: "Found it, fixing now.",
            createdAtMs: 2,
            unread: true,
          }),
          // Mail with another agent is not part of its conversation with you.
          msg({ id: 3, from: "SW-1#triage", to: "other", text: "Agent to agent", createdAtMs: 3 }),
        ];
        useAgentInboxRailStore.setState({ bySession: {} });
        clearInboxDrafts();
      });
      const openChat = async () => {
        renderList();
        const [chat] = await screen.findAllByTestId("agent-row-message");
        fireEvent.click(chat);
        return screen.getByRole("dialog", { name: "Messages with SW-1 · Triage" });
      };

      it("shows the conversation with you, marks the new mail and reads it, and focuses the composer", async () => {
        const dialog = await openChat();
        expect(await within(dialog).findByText("Found it, fixing now.")).toBeVisible();
        expect(within(dialog).getByText("Where are we?")).toBeVisible();
        expect(within(dialog).queryByText("Agent to agent")).toBeNull();
        expect(within(dialog).getByTestId("ibx-new")).toBeInTheDocument();
        const box = within(dialog).getByRole("textbox", { name: "Message" });
        await waitFor(() => expect(box).toHaveFocus());
        await waitFor(() => expect(markInboxMessageRead).toHaveBeenCalledWith(TEAM, 2));
        expect(markInboxMessageRead).not.toHaveBeenCalledWith(TEAM, 1);
      });

      it("Enter sends through the inbox's own send, to that agent; Shift+Enter does not", async () => {
        const dialog = await openChat();
        const box = await within(dialog).findByRole("textbox", { name: "Message" });
        fireEvent.change(box, { target: { value: "Ship it" } });
        fireEvent.keyDown(box, { key: "Enter", shiftKey: true });
        expect(sendInboxAsHuman).not.toHaveBeenCalled();
        fireEvent.keyDown(box, { key: "Enter" });
        await waitFor(() =>
          expect(sendInboxAsHuman).toHaveBeenCalledWith(
            TEAM,
            "@SW-1#triage",
            "Ship it",
            "markdown",
            null,
            null,
            null,
            null,
            null,
          ),
        );
        await waitFor(() => expect(box).toHaveValue(""));
        expect(activateTrayWorkspace).not.toHaveBeenCalled();
        // The message shows landing in the chat: no "Sent to" toast on top of it.
        expect(toast).not.toHaveBeenCalledWith(expect.stringMatching(/^Sent to/));
      });

      it("a send that fails keeps the message and says so", async () => {
        sendInboxAsHuman.mockResolvedValueOnce({ status: "invalid", message: "nope" });
        const dialog = await openChat();
        const box = await within(dialog).findByRole("textbox", { name: "Message" });
        fireEvent.change(box, { target: { value: "Try this" } });
        fireEvent.keyDown(box, { key: "Enter" });
        expect(await within(dialog).findByTestId("ibx-send-failed")).toBeVisible();
        expect(box).toHaveValue("Try this");
      });

      it("an agent with no messages says so", async () => {
        sessionInboxMessages = [];
        const dialog = await openChat();
        expect(await within(dialog).findByText("No messages yet")).toBeVisible();
      });

      it("Open in CLI shows its tab and opens its inbox rail", async () => {
        const dialog = await openChat();
        fireEvent.click(within(dialog).getByTestId("agent-inbox-chat-open-cli"));
        expect(screen.queryByTestId("agent-inbox-chat")).toBeNull();
        expect(activateTrayWorkspace).toHaveBeenCalledWith("team-1", "ws-a");
        await waitFor(() =>
          expect(useAgentInboxRailStore.getState().bySession["sid-SW-1#triage"]).toBeDefined(),
        );
      });

      it("Escape closes it and returns focus to the row; with a draft it first leaves the box", async () => {
        const dialog = await openChat();
        const box = await within(dialog).findByRole("textbox", { name: "Message" });
        await waitFor(() => expect(box).toHaveFocus());
        fireEvent.change(box, { target: { value: "half a thought" } });
        fireEvent.keyDown(box, { key: "Escape" });
        expect(screen.getByTestId("agent-inbox-chat")).toBeInTheDocument();
        expect(box).not.toHaveFocus();
        fireEvent.keyDown(document.activeElement ?? document, { key: "Escape" });
        expect(screen.queryByTestId("agent-inbox-chat")).toBeNull();
        expect(screen.getAllByTestId("agent-row-message")[0]).toHaveFocus();
      });

      it("a click outside closes it; M on the row opens it", async () => {
        await openChat();
        fireEvent.mouseDown(document.body);
        expect(screen.queryByTestId("agent-inbox-chat")).toBeNull();
        fireEvent.keyDown(agentRowButton(/SW-1 · Triage/), { key: "m" });
        expect(screen.getByTestId("agent-inbox-chat")).toBeInTheDocument();
        expect(screen.queryByTestId("agent-quick-send")).toBeNull();
        expect(activateTrayWorkspace).not.toHaveBeenCalled();
      });
    });

    it("a plain row click reveals the terminal and leaves the per-tab rail closed", async () => {
      terminalTabIndex = {
        "team-1": { "ws-a": [{ id: "tab-a", sessionId: "sid-SW-1#triage" }] },
      };
      workspaces = { "team-1": [{ id: "ws-a" }] };
      listResult = [row({ handle: "SW-1#triage", agentRevision: "acme/backend/triage@3" })];
      renderList();
      const row1 = agentRowButton(/SW-1 · Triage/);
      fireEvent.click(row1);
      expect(useAgentInboxRailStore.getState().bySession["sid-SW-1#triage"]).toBeUndefined();
      expect(activateTrayWorkspace).toHaveBeenCalledWith("team-1", "ws-a");
    });
  });
  describe("pins, the Hidden filter and held roles", () => {
    beforeEach(() => useLaunchSheetStore.setState({ request: null }));

    const pinnedGroup = () => screen.getByTestId("agents-pinned");
    const list = () => <DockSlotHost teamKey="team-1" visible />;

    it("pins only the clicked session, even when another session runs the same Agent", async () => {
      listResult = [
        row({ handle: "mgr-1", agentRevision: "acme/backend/manager@2", story: "SW-9" }),
        row({ handle: "mgr-2", agentRevision: "acme/backend/manager@2", story: "SW-9" }),
      ];
      const { rerender } = renderList();
      await screen.findByRole("button", { name: agentRow(/mgr-1/) });
      expect(screen.queryByTestId("agents-pinned")).toBeNull();

      fireEvent.click(
        within(
          Array.from(document.querySelectorAll<HTMLElement>("[data-row-index]")).find((el) =>
            /mgr-1/.test(el.textContent ?? ""),
          )!,
        ).getAllByTestId("agent-row-pin")[0],
      );
      // Only that session is pinned, not every session of its saved Agent.
      expect(pinAgent).toHaveBeenCalledWith(
        "team-1",
        expect.objectContaining({ kind: "session", handle: "mgr-1" }),
      );
      rerender(list());
      expect(within(pinnedGroup()).getByRole("button", { name: agentRow(/mgr-1/) })).toBeVisible();
      expect(within(pinnedGroup()).queryByRole("button", { name: agentRow(/mgr-2/) })).toBeNull();
    });

    it("a pin holds its session until it ends, even for an agent running a saved Agent", async () => {
      listResult = [row({ handle: "mgr-1", agentRevision: "acme/backend/manager@2" })];
      const { rerender } = renderList();
      await screen.findByRole("button", { name: agentRow(/mgr-1/) });
      const pin = screen.getByTestId("agent-row-pin");
      expect(pin.closest("[title]")).toHaveAttribute(
        "title",
        "Pin mgr-1: it stays on top until it ends",
      );
      fireEvent.click(pin);
      rerender(list());
      // The row's own pin button unpins it: the pin and the row share one id.
      fireEvent.click(within(pinnedGroup()).getByRole("button", { name: "Unpin" }));
      expect(unpinAgents).toHaveBeenCalledWith("team-1", [sessionPinId("team-1", "mgr-1")]);
    });

    it("pinning a session with no saved Agent pins its handle, and the pin goes when it ends", async () => {
      listResult = [row({ handle: "scratch" })];
      terminalTabIndex = { "team-1": { "ws-a": [{ id: "tab-s", sessionId: "sid-scratch" }] } };
      workspaces = { "team-1": [{ id: "ws-a" }] };
      const { rerender } = renderList();
      fireEvent.contextMenu(await screen.findByRole("button", { name: agentRow(/scratch/) }));
      fireEvent.click(await screen.findByRole("menuitem", { name: "Pin" }));
      expect(pinAgent).toHaveBeenCalledWith("team-1", {
        kind: "session",
        teamKey: "team-1",
        handle: "scratch",
        sessionId: "sid-scratch",
        name: "scratch",
      });
      rerender(list());
      expect(
        within(pinnedGroup()).getByRole("button", { name: agentRow(/scratch/) }),
      ).toBeVisible();

      // It ends: its row and its tab go, and so does the pin. No Start row for a session.
      listResult = [];
      terminalTabIndex = {};
      act(() => publishProjectionOwners({ sessions: listResult }));
      rerender(list());
      await waitFor(() =>
        expect(unpinAgents).toHaveBeenCalledWith("team-1", [sessionPinId("team-1", "scratch")]),
      );
      rerender(list());
      expect(screen.queryByTestId("agents-pinned")).toBeNull();
    });

    it("Unpin from a pinned row's menu", async () => {
      listResult = [row({ handle: "mgr-1", agentRevision: "acme/backend/manager@2" })];
      agentPins = {
        "team-1": [
          { kind: "session", teamKey: "team-1", handle: "mgr-1", sessionId: null, name: "Manager" },
        ],
      };
      renderList();
      await screen.findByTestId("agents-pinned");
      fireEvent.contextMenu(within(pinnedGroup()).getByRole("button", { name: agentRow(/mgr-1/) }));
      fireEvent.click(await screen.findByRole("menuitem", { name: "Unpin" }));
      expect(unpinAgents).toHaveBeenCalledWith("team-1", [sessionPinId("team-1", "mgr-1")]);
    });

    it("shows the role a session holds and releases it from the row menu", async () => {
      roleConfigs["acme:backend"] = [
        { id: "triage", name: "Triage", icon: "builtin-chess:knight", defaultHandler: null },
      ];
      listResult = [row({ handle: "triager" }), row({ handle: "idle-one" })];
      rolesResult = {
        roles: [
          {
            role: "triage",
            roleName: "Triage",
            registrations: [
              {
                instanceId: "sid-triager",
                instanceName: null,
                generation: 4,
                registeredAtMs: 1,
                lastRenewedAtMs: 1,
                expiresAtMs: 9e15,
                live: true,
              },
              {
                instanceId: "sid-idle-one",
                instanceName: null,
                generation: 1,
                registeredAtMs: 1,
                lastRenewedAtMs: 1,
                expiresAtMs: 1,
                live: false,
              },
            ],
            counts: {},
            deliveries: [],
            deliveriesTruncated: false,
          },
        ],
      };
      fixtureTeamKey = "acme:backend";
      publishProjectionOwners({ sessions: listResult, error: null });
      render(<DockSlotHost teamKey="acme:backend" visible />);
      const holds = await screen.findByTestId("agent-holds");
      const chip = await within(holds).findByRole("button", {
        name: "Queue role: Triage (role:triage)",
      });
      expect(chip).toHaveTextContent(/^Triage$/);
      expect(chip.querySelector("img")).toHaveAttribute(
        "src",
        BUILTIN_FACE_SOURCE.resolve("builtin-chess:knight"),
      );
      expect(agentReadRoles).toHaveBeenCalledWith({ org: "acme", team: "backend" });
      // A lapsed registration holds nothing.
      expect(screen.getAllByTestId("agent-holds")).toHaveLength(1);

      fireEvent.contextMenu(screen.getByRole("button", { name: agentRow(/triager/) }));
      fireEvent.click(await screen.findByRole("menuitem", { name: "Release role Triage" }));
      await waitFor(() =>
        expect(releaseRole).toHaveBeenCalledWith(
          { org: "acme", team: "backend" },
          "triage",
          "sid-triager",
          4,
        ),
      );
      fireEvent.contextMenu(screen.getByRole("button", { name: agentRow(/idle-one/) }));
      expect(screen.queryByRole("menuitem", { name: /Release role/ })).toBeNull();
    });

    it("a narrow pane keeps one trailing marker and moves hover actions into the menu", async () => {
      listResult = [row({ handle: "tucked", agentRevision: "acme/backend/tucked@1" })];
      hiddenTabIndex = { "team-1": { "ws-a": [{ id: "tab-t", sessionId: "sid-tucked" }] } };
      agentPins = {
        "team-1": [
          { kind: "session", teamKey: "team-1", handle: "tucked", sessionId: null, name: "T" },
        ],
      };
      const { rerender } = renderList();
      await screen.findByTestId("agents-pinned");
      // The agent's place in the tree carries the pin marker (in the Pinned group the group's
      // label says it).
      const pinnedRow = () =>
        screen
          .getAllByRole("button", { name: agentRow(/tucked/) })
          .find((b) => !pinnedGroup().contains(b))!
          .closest(".ag-row")!;
      expect(
        within(pinnedGroup())
          .getByRole("button", { name: agentRow(/tucked/) })
          .closest(".ag-row"),
      ).not.toContainElement(screen.queryAllByTestId("agent-pin-mark")[0] ?? null);
      // Wide: hidden and pinned both show, with the hover actions.
      expect(
        within(pinnedRow() as HTMLElement).getByRole("img", { name: "Tab hidden" }),
      ).toBeVisible();
      expect(within(pinnedRow() as HTMLElement).getByTestId("agent-pin-mark")).toBeVisible();
      expect(within(pinnedRow() as HTMLElement).getByTestId("agent-row-pin")).toBeInTheDocument();

      panelWidth = 236;
      rerender(list());
      expect(screen.getByRole("region", { name: "Agents" })).toHaveAttribute("data-narrow", "true");
      // Narrow: the most urgent marker only (hidden beats pin), and no hover actions.
      expect(
        within(pinnedRow() as HTMLElement).getByRole("img", { name: "Tab hidden" }),
      ).toBeVisible();
      expect(within(pinnedRow() as HTMLElement).queryByTestId("agent-pin-mark")).toBeNull();
      expect(screen.queryByTestId("agent-row-pin")).toBeNull();
      expect(screen.queryByTestId("agent-row-message")).toBeNull();
      // The row menu still has them.
      fireEvent.contextMenu(
        within(pinnedGroup()).getByRole("button", { name: agentRow(/tucked/) }),
      );
      expect(await screen.findByRole("menuitem", { name: "Unpin" })).toBeInTheDocument();
      expect(screen.getByRole("menuitem", { name: "Open inbox" })).toBeInTheDocument();
      expect(screen.getByRole("menuitem", { name: "Message in inbox" })).toBeInTheDocument();
    });

    it("hover actions get their own room on line one, so the title truncates before them", async () => {
      listResult = [row({ handle: "long", agentRevision: "acme/backend/long@1" })];
      const { rerender } = renderList();
      await screen.findByRole("button", { name: agentRow(/long/) });
      const rowEl = () =>
        screen.getByRole("button", { name: agentRow(/long/) }).closest(".ag-row") as HTMLElement;
      // Wide: the row says how many actions it shows, and line one reserves that much room.
      const actions = within(rowEl()).getAllByRole("button", {
        name: /^(Quick view|Messages|Pin|Unpin)$/,
      });
      expect(rowEl()).toHaveAttribute("data-actions", String(actions.length));
      expect(rowEl().style.getPropertyValue("--ag-row-actions-n")).toBe(String(actions.length));
      const title = rowEl().querySelector(".ag-title") as HTMLElement;
      // The line truncates its parts: the name first, then a prompt; the status word keeps its room.
      expect(title).toHaveClass("ag-title-line", "min-w-0", "flex-1");
      expect(title.parentElement).toHaveClass("ag-line1");
      // The stylesheet pads line one by that count while the actions show (hover or focus).
      const css = readFileSync(resolve(__dirname, "../../../styles/agents.css"), "utf-8");
      expect(css).toMatch(
        /\.ag-row\[data-actions\]:hover \.ag-line1,\s*\.ag-row\[data-actions\]:focus-within \.ag-line1 \{\s*padding-inline-end: calc\(var\(--ag-row-actions-n, 3\) \* 26px\);/,
      );

      // Narrow: no hover actions, so nothing is reserved.
      panelWidth = 236;
      rerender(list());
      expect(rowEl()).not.toHaveAttribute("data-actions");
      expect(rowEl().style.getPropertyValue("--ag-row-actions-n")).toBe("");
    });
  });

  // SW-WC-4: the dual grouping toggle (by-manager / by-workspace / flat).

  describe("row and status fidelity", () => {
    // Twin of SW-WC-4's twoAgentsOneWs helper — kept local so this block owns its own
    // fixture setup (one lead + one worker in one story workspace, mimicking the
    // simplest "something is in the pane" scenario).
    const twoAgentsOneWs = () => {
      workspaces = {
        "team-1": [{ id: "ws-a", kind: "story", status: "ready", storyTitle: "Auth" }],
      };
      terminalTabIndex = {
        "team-1": {
          "ws-a": [
            { id: "tab-lead", sessionId: "sid-lead", name: "lead" },
            { id: "tab-worker", sessionId: "sid-worker", name: "worker" },
          ],
        },
      };
      listResult = [
        row({ handle: "lead", sessionId: "sid-lead" }),
        row({ handle: "worker", sessionId: "sid-worker", launchedBy: "lead" }),
      ];
    };
    const hooked = (...sids: string[]) =>
      act(() => {
        for (const sid of sids) {
          useAgentStateStore
            .getState()
            .recordSignal(sid, { tier: 1, signal: { kind: "claude-hook", event: "PreToolUse" } });
          projectSessionStatus(sid, "tool-running");
        }
      });

    it("status mark now clips onto the avatar — ag-avatar-wrap holds mark + face", async () => {
      twoAgentsOneWs();
      hooked("sid-lead", "sid-worker");
      renderList();
      // Locate the lead row by its stable `data-agent-handle`, not by prose that
      // overlaps with the worker's "via lead" caption.
      const pane = await screen.findByRole("region", { name: "Agents" });
      const leadRow = pane.querySelector('[data-agent-handle="lead"]') as HTMLElement | null;
      expect(leadRow).not.toBeNull();
      const wrap = leadRow!.querySelector(".ag-avatar-wrap") as HTMLElement | null;
      expect(wrap).not.toBeNull();
      expect(wrap!.querySelector(".ag-avatar-dot")).not.toBeNull();
      // Working tone → `.ag-avatar-dot-working` (dot container's group chrome).
      expect(wrap!.querySelector(".ag-avatar-dot")!.className).toContain("ag-avatar-dot-working");
      // the dot renders the SHARED StatusMark — the same mark
      // component every other surface draws, carrying the fine-grained
      // thinking-vs-tool distinction by shape.
      const mark = wrap!.querySelector(".cs-mark") as HTMLElement | null;
      expect(mark).not.toBeNull();
      expect(mark!.getAttribute("data-mark")).toBe("tool");
    });

    it("needs-you now reads as a 2px amber left bar, not a full-row wash", () => {
      // Verifying the shipped stylesheet — a regression that re-adds `background-color`
      // on `.ag-row[data-needs="true"]` is caught here.
      const css = readFileSync(resolve(__dirname, "../../../styles/agents.css"), "utf-8");
      // The `::after` left bar is defined.
      expect(css).toMatch(
        /\.ag-row\[data-needs="true"\]::after\s*\{[^}]*background:\s*var\(--cs-status-needs\)/,
      );
      // The old full-row wash on the row itself is gone (no `background-color` directly
      // on the `[data-needs]` selector). We allow it inside `::after` and `:hover`.
      const needsRule = css.match(/\.ag-row\[data-needs="true"\]\s*\{[^}]*\}/m);
      expect(needsRule).toBeNull();
    });

    it("the warm-amber workspace count badge turns on when a member needs you", async () => {
      twoAgentsOneWs();
      // Mark the worker as paused → it's in NEEDS_YOU_STATUSES.
      act(() =>
        useAgentStateStore.getState().recordSignal("sid-worker", {
          tier: 1,
          signal: { kind: "claude-hook", event: "PermissionRequest" },
        }),
      );
      projectSessionStatus("sid-worker", "paused");
      renderList();
      await screen.findByTestId("agents-workspace-group");
      const count = within(screen.getByTestId("agents-workspace-group")).getByText(/^\d+$/);
      expect(count).toHaveAttribute("data-has-needs", "true");
      expect(count.querySelector("[data-mark]")).toHaveAttribute("data-mark", "permission");
      expect(count).toHaveAccessibleName(/1 needs approval/);
      expect(count).toHaveAccessibleName(/1 unknown/);
      expect(count).toHaveTextContent(/^1$/);
    });

    it("the workspace count stays plain when no member needs you (negative case)", async () => {
      // Guard against a regression that always emits `data-has-needs` regardless of
      // membership — the CSS selector is attribute-presence only, so a stuck-on
      // attribute would always warm the badge. This case keeps that honest.
      twoAgentsOneWs();
      // Both agents working (PreToolUse hook → `running`) — no needs-you member in
      // the group, so no amber badge.
      hooked("sid-lead", "sid-worker");
      renderList();
      await screen.findByTestId("agents-workspace-group");
      const count = within(screen.getByTestId("agents-workspace-group")).getByText(/^\d+$/);
      expect(count).not.toHaveAttribute("data-has-needs");
    });

    it("the row renders the SHARED StatusMark on the avatar (one glyph vocabulary across surfaces)", async () => {
      // reverses SW-WC-5's "collapse to three tones" compromise: the row
      // now renders the SAME shared `<StatusMark>` component the pills and the
      // tab chips draw. The dot is a container, the mark inside carries the
      // state (shape + motion + colour) — one source of truth, no private tone
      // table in this file.
      twoAgentsOneWs();
      act(() =>
        useAgentStateStore.getState().recordSignal("sid-lead", {
          tier: 1,
          signal: { kind: "claude-hook", event: "PermissionRequest" },
        }),
      );
      projectSessionStatus("sid-lead", "paused");
      renderList();
      const pane = await screen.findByRole("region", { name: "Agents" });
      const leadRow = pane.querySelector('[data-agent-handle="lead"]') as HTMLElement | null;
      expect(leadRow).not.toBeNull();
      // The row's dot wraps a shared StatusMark, with the fine-grained
      // `permission` kind (not a tone-collapsed amber dot).
      const dot = within(leadRow!).getByTestId("needs-you-badge");
      expect(dot.className).toContain("ag-avatar-dot");
      const mark = dot.querySelector(".cs-mark") as HTMLElement | null;
      expect(mark).not.toBeNull();
      expect(mark!.getAttribute("data-mark")).toBe("permission");
    });

    it("the dot container no longer carries a pulse of its own — motion lives inside the StatusMark", () => {
      const css = readFileSync(resolve(__dirname, "../../../styles/agents.css"), "utf-8");
      // the dot container is now a transparent pane-ring wrapper;
      // motion (the halo pulse, the think breathing, the tool arc) belongs to
      // the `.cs-mark-*` classes under `status-mark.css` so every surface
      // honours `prefers-reduced-motion` from one place.
      expect(css).not.toMatch(/\.ag-avatar-dot-\w+\s*\{[^}]*animation:/);
      expect(css).not.toMatch(/@keyframes ag-dot-pulse/);
    });

    it("a Limited row's notice is its own line, outside the clipped launcher-pill line", async () => {
      twoAgentsOneWs();
      listResult = [
        row({ handle: "lead", sessionId: "sid-lead" }),
        row({
          handle: "worker",
          sessionId: "sid-worker",
          launchedBy: "lead",
          releaseQualified: false,
          releaseNote: "This CLI version (2.0.1) isn't supported yet.",
        }),
      ];
      projectSessionStatus("sid-lead", "idle");
      projectSessionStatus("sid-worker", "idle");
      renderList();
      await screen.findByRole("button", { name: agentRow(/worker/) });
      fireEvent.click(within(filterMenu()).getByRole("radio", { name: "By status" }));
      const pill = await screen.findByTestId("agent-manager-pill");
      const note = screen.getByTestId("agent-limited-note");
      // `.ag-via-line` has a fixed height and clips; the note must not compete for it.
      expect(pill.closest(".ag-via-line")?.contains(note)).toBe(false);
      expect(note.closest(".ag-row")).toBe(pill.closest(".ag-row"));
    });

    it("a row drawn under its launcher has no via chip; listed elsewhere, it has one with the mock's glyph", async () => {
      twoAgentsOneWs();
      hooked("sid-lead", "sid-worker");
      renderList();
      await screen.findByRole("button", { name: agentRow(/worker/) });
      // The indent already says the worker runs under lead.
      expect(screen.queryByTestId("agent-manager-pill")).toBeNull();
      // By status, the worker leaves its launcher's side, so it names it.
      fireEvent.click(within(filterMenu()).getByRole("radio", { name: "By status" }));
      const pill = await screen.findByTestId("agent-manager-pill");
      expect(pill).toHaveTextContent("lead");
      const svg = pill.querySelector("svg.ag-via-glyph");
      expect(svg).not.toBeNull();
      // Two <path> elements: the routing curve and the arrowhead, matching the mock.
      expect(svg!.querySelectorAll("path")).toHaveLength(2);
    });
  });

  describe("pane styling", () => {
    const css = () => readFileSync(resolve(__dirname, "../../../styles/agents.css"), "utf-8");
    it("keeps the find field's chips on one line, the input keeps the rest", () => {
      expect(css()).toMatch(/\.ag-chips \{[^}]*flex-wrap: nowrap;[^}]*\}/);
      expect(css()).toMatch(/\.ag-chips \{[^}]*min-width: 0;[^}]*\}/);
    });
  });

  describe("pane tree rows and resizing", () => {
    it("keeps an agent whose tab is Hidden in the tree, marked, and restores its tab on click", async () => {
      workspaces = { "team-1": [{ id: "ws-login" }] };
      const shown = { id: "tab-lead", sessionId: "sid-lead", name: "lead" };
      const hidden = { id: "tab-bg", sessionId: "sid-bg", name: "bg" };
      // A Hidden tab is off its tile: not in the open index, only in the Hidden one.
      terminalTabIndex = { "team-1": { "ws-login": [shown] } };
      hiddenTabIndex = { "team-1": { "ws-login": [hidden] } };
      listResult = [row({ handle: "lead" }), row({ handle: "bg" })];
      renderList();

      const bg = await screen.findByRole("button", { name: agentRow(/bg/) });
      expect(within(bg).getByRole("img", { name: "Tab hidden" })).toBeInTheDocument();
      expect(bg).toHaveAttribute("title", expect.stringContaining("Tab hidden, still running"));
      expect(within(agentRowButton(/lead/)).queryByRole("img", { name: "Tab hidden" })).toBeNull();

      fireEvent.click(bg);
      await waitFor(() =>
        expect(restoreHiddenTerminalTab).toHaveBeenCalledWith("team-1", "ws-login", "tab-bg"),
      );
      // A row click focuses the terminal, so the user can type straight away.
      expect(focusActiveWorkspaceTerminal).toHaveBeenCalledWith("team-1");
      // Restored in place: never a second tab on the same session.
      expect(adoptTerminalSession).not.toHaveBeenCalled();
    });

    it("resizes from its edge with the arrow keys, remembered per team", async () => {
      listResult = [row({ handle: "dev" })];
      renderList();
      const edge = await screen.findByRole("separator", { name: "Resize agents" });
      expect(edge).toHaveAttribute("aria-valuenow", "268");
      expect(edge).toHaveAttribute("aria-valuemin", "236");
      expect(edge).toHaveAttribute("aria-valuemax", "400");
      expect(edge).toHaveAttribute("aria-orientation", "vertical");
      fireEvent.keyDown(edge, { key: "ArrowRight" });
      expect(setWorkspacePanelWidth).toHaveBeenCalledWith(284);
      fireEvent.keyDown(edge, { key: "ArrowLeft", shiftKey: true });
      expect(setWorkspacePanelWidth).toHaveBeenLastCalledWith(236);
      fireEvent.keyDown(edge, { key: "End" });
      expect(setWorkspacePanelWidth).toHaveBeenLastCalledWith(400);
      fireEvent.keyDown(edge, { key: "Home" });
      expect(setWorkspacePanelWidth).toHaveBeenLastCalledWith(236);
    });

    it("uses the same border control as the other panes: drag resizes, a click hides the list", async () => {
      const { setPointerCapture, releasePointerCapture } = Element.prototype;
      Element.prototype.setPointerCapture = vi.fn();
      Element.prototype.releasePointerCapture = vi.fn();
      vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
        cb(0);
        return 1;
      });
      try {
        listResult = [row({ handle: "dev" })];
        renderList();
        const edge = await screen.findByRole("separator", { name: "Resize agents" });
        expect(edge).toHaveAttribute("data-testid", "agents-width-divider");
        // The shared divider arms the resize cursor on a genuine hover.
        fireEvent.pointerEnter(edge);
        expect(edge.style.cursor).toBe("col-resize");
        // Drag 60px right: the list follows live, and the width is saved once.
        fireEvent.pointerDown(edge, { pointerId: 1, clientX: 268 });
        fireEvent.pointerMove(edge, { pointerId: 1, clientX: 328 });
        expect(screen.getByRole("region", { name: "Agents" }).style.width).toBe("328px");
        fireEvent.pointerUp(edge, { pointerId: 1, clientX: 328 });
        expect(setWorkspacePanelWidth).toHaveBeenCalledTimes(1);
        expect(setWorkspacePanelWidth).toHaveBeenCalledWith(328);
        // A plain click on the border hides the list, as the header's Hide button does.
        // (Past the window in which a drag's trailing click is swallowed.)
        await new Promise((r) => setTimeout(r, 300));
        fireEvent.click(edge);
        expect(setWorkspacePanelCollapsed).toHaveBeenCalledWith(true);
      } finally {
        vi.unstubAllGlobals();
        Element.prototype.setPointerCapture = setPointerCapture;
        Element.prototype.releasePointerCapture = releasePointerCapture;
      }
    });
  });

  describe("pane core: teams, workspaces, selection", () => {
    /** Two teams: team-1 (a story workspace with a lead and a worker) and acme:ops (one agent). */
    function twoTeams() {
      workspaces = {
        "team-1": [{ id: "ws-a", kind: "story", status: "ready", storyTitle: "Auth flow" }],
        "acme:ops": [{ id: "ws-x" }],
      };
      terminalTabIndex = {
        "team-1": {
          "ws-a": [
            { id: "tab-lead", sessionId: "sid-lead", name: "lead" },
            { id: "tab-w", sessionId: "sid-worker", name: "worker" },
          ],
        },
        "acme:ops": { "ws-x": [{ id: "tab-x", sessionId: "sid-elsewhere" }] },
      };
      listResult = [
        row({ handle: "lead", sessionId: "sid-lead" }),
        row({ handle: "worker", sessionId: "sid-worker", launchedBy: "lead" }),
        row({ handle: "elsewhere", sessionId: "sid-elsewhere" }),
      ];
    }
    const rerenderPane = (rerender: (ui: React.ReactElement) => void) =>
      rerender(<DockSlotHost teamKey="team-1" visible />);
    const workspaceGroup = (id: string) =>
      document.querySelector<HTMLElement>(`[data-group-key="${id}"]`)!;

    it("every visible team's agents show team › workspace › agent, with Workspace as the default and no retired density controls", async () => {
      twoTeams();
      renderList();
      await screen.findByRole("button", { name: agentRow(/elsewhere/) });
      expect(screen.getAllByTestId("agents-team-group")).toHaveLength(2);
      expect(rowOf("worker").closest("[data-row-index]")).toHaveAttribute(
        "data-ws-key",
        "team-1|ws-a",
      );
      expect(screen.queryByRole("radiogroup")).toBeNull();
      expect(screen.queryByRole("button", { name: /^(Flat|Cozy|Compact)$/ })).toBeNull();
      expect(screen.getByTestId("agents-count")).toHaveTextContent(/^3$/);
    });

    it("the header shows visible/total while a filter is active", async () => {
      twoTeams();
      renderList();
      await screen.findByRole("button", { name: agentRow(/elsewhere/) });
      fireEvent.change(screen.getByLabelText("Find agents"), { target: { value: "elsewhere" } });
      expect(screen.getByTestId("agents-count")).toHaveTextContent(/^1\/3$/);
    });

    it("the Teams checklist hides a team, and no 'All teams' control exists", async () => {
      twoTeams();
      const { rerender } = renderList();
      await screen.findByRole("button", { name: agentRow(/elsewhere/) });
      const menu = filterMenu();
      expect(within(menu).getByText("acme")).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: /all teams/i })).toBeNull();
      fireEvent.click(within(menu).getByRole("checkbox", { name: /ops/ }));
      expect(setAgentsPaneTeamHidden).toHaveBeenCalledWith("acme:ops", true);
      rerenderPane(rerender);
      expect(screen.queryByRole("button", { name: agentRow(/elsewhere/) })).toBeNull();
      expect(screen.getByRole("button", { name: agentRow(/worker/) })).toBeInTheDocument();
    });

    it("switching the active team leaves the team order as it was", async () => {
      twoTeams();
      const order = () =>
        [...document.querySelectorAll<HTMLElement>("[data-team-key]")].map(
          (el) => el.dataset.teamKey,
        );
      const { rerender } = renderList();
      await screen.findByRole("button", { name: agentRow(/elsewhere/) });
      const before = order();
      rerender(<DockSlotHost teamKey="acme:ops" visible />);
      await screen.findByRole("button", { name: agentRow(/elsewhere/) });
      expect(order()).toEqual(before);
    });

    it("unpinning a pinned row removes only its own team's pin", async () => {
      twoTeams();
      agentPins = {
        "team-1": [
          {
            kind: "session",
            teamKey: "team-1",
            handle: "worker",
            sessionId: "sid-worker",
            name: "worker",
          },
        ],
        "acme:ops": [
          {
            kind: "session",
            teamKey: "acme:ops",
            handle: "elsewhere",
            sessionId: "sid-elsewhere",
            name: "elsewhere",
          },
        ],
      };
      renderList();
      const pinned = await screen.findByTestId("agents-pinned");
      fireEvent.contextMenu(within(pinned).getByRole("button", { name: agentRow(/worker/) }));
      fireEvent.click(await screen.findByRole("menuitem", { name: "Unpin" }));
      expect(unpinAgents.mock.calls).toEqual([["team-1", [sessionPinId("team-1", "worker")]]]);
      agentPins = {};
    });

    it("a filtered-out team's arrow filter is dropped when that team is hidden", async () => {
      twoTeams();
      paneWorkspaceFilter = { teamKey: "team-1", workspaceId: "ws-a" };
      hiddenPaneTeams = ["team-1"];
      renderList("acme:ops");
      await waitFor(() => expect(setAgentsWorkspaceFilter).toHaveBeenCalledWith(null));
      paneWorkspaceFilter = null;
      hiddenPaneTeams = [];
    });

    it("the Teams checklist will not hide the last team in view", async () => {
      twoTeams();
      hiddenPaneTeams = ["acme:ops"];
      renderList();
      await screen.findByRole("button", { name: agentRow(/worker/) });
      const menu = filterMenu();
      const shown = within(menu)
        .getAllByRole("checkbox")
        .find((c) => (c as HTMLInputElement).checked);
      expect(shown).toBeDefined();
      fireEvent.click(shown!);
      expect(setAgentsPaneTeamHidden).not.toHaveBeenCalledWith("team-1", true);
      hiddenPaneTeams = [];
    });

    it("a team header reads org › team, folds on click, and shows its need-you count folded", async () => {
      twoTeams();
      awaitingAnswer("sid-elsewhere");
      const { rerender } = renderList();
      await screen.findByRole("button", { name: agentRow(/elsewhere/) });
      const head = screen.getByRole("button", { name: /^acme.*ops$/ });
      expect(head).toHaveAttribute("aria-expanded", "true");
      fireEvent.click(head);
      expect(setAgentsPaneFold).toHaveBeenCalledWith("#team-fold:acme:ops", true);
      rerenderPane(rerender);
      expect(screen.queryByRole("button", { name: agentRow(/elsewhere/) })).toBeNull();
      expect(screen.getByLabelText("1 need you")).toBeInTheDocument();
    });

    describe("a team header's funnel", () => {
      const teamFunnel = (teamKey: string) =>
        document.querySelector<HTMLButtonElement>(
          `[data-team-key="${teamKey}"] [data-testid="agents-team-funnel"]`,
        )!;
      const teamChip = () => document.querySelector<HTMLElement>('.ag-chip[data-chip="team"]');

      it("shows only its team by hiding every other one, and lights while on", async () => {
        twoTeams();
        const { rerender } = renderList();
        await screen.findByRole("button", { name: agentRow(/elsewhere/) });
        expect(teamFunnel("acme:ops")).toHaveAttribute("aria-pressed", "false");
        fireEvent.click(teamFunnel("team-1"));
        expect(setAgentsPaneTeamScope).toHaveBeenLastCalledWith(["acme:ops"], {
          teamKey: "team-1",
          hidden: [],
        });
        expect(setAgentsPaneFold).not.toHaveBeenCalled();
        rerenderPane(rerender);
        expect(screen.queryByRole("button", { name: agentRow(/elsewhere/) })).toBeNull();
        expect(screen.getByRole("button", { name: agentRow(/worker/) })).toBeInTheDocument();
        expect(teamFunnel("team-1")).toHaveAttribute("aria-pressed", "true");
        expect(teamFunnel("team-1")).toHaveAttribute("title", "Show the other teams again");
        expect(teamChip()).toHaveTextContent(/^Team: /);
      });

      it("a second click puts back exactly what was hidden before", async () => {
        twoTeams();
        // A team hidden before the funnel that the pane no longer lists.
        hiddenPaneTeams = ["old:closed"];
        const { rerender } = renderList();
        await screen.findByRole("button", { name: agentRow(/worker/) });
        fireEvent.click(teamFunnel("acme:ops"));
        expect(hiddenPaneTeams).toEqual(["old:closed", "team-1"]);
        rerenderPane(rerender);
        expect(screen.queryByRole("button", { name: agentRow(/worker/) })).toBeNull();
        fireEvent.click(teamFunnel("acme:ops"));
        expect(setAgentsPaneTeamScope).toHaveBeenLastCalledWith(["old:closed"], null);
        rerenderPane(rerender);
        expect(screen.getByRole("button", { name: agentRow(/worker/) })).toBeInTheDocument();
        expect(teamFunnel("acme:ops")).toHaveAttribute("aria-pressed", "false");
      });

      it("its chip names the team, and clearing the chip shows the other teams again", async () => {
        twoTeams();
        const { rerender } = renderList();
        await screen.findByRole("button", { name: agentRow(/elsewhere/) });
        fireEvent.click(teamFunnel("acme:ops"));
        rerenderPane(rerender);
        expect(teamChip()).toHaveTextContent("Team: ops");
        // The chip replaces the "hidden" chip: the pane names what shows, once.
        expect(document.querySelectorAll(".ag-chip")).toHaveLength(1);
        fireEvent.click(screen.getByRole("button", { name: "Show the other teams" }));
        expect(setAgentsPaneTeamScope).toHaveBeenLastCalledWith([], null);
        rerenderPane(rerender);
        expect(teamChip()).toBeNull();
        expect(screen.getByRole("button", { name: agentRow(/worker/) })).toBeInTheDocument();
      });

      it("agrees with the filter menu's Teams checklist, both ways", async () => {
        twoTeams();
        const { rerender } = renderList();
        await screen.findByRole("button", { name: agentRow(/elsewhere/) });
        fireEvent.click(teamFunnel("acme:ops"));
        rerenderPane(rerender);
        const menu = filterMenu();
        expect(within(menu).getByRole("checkbox", { name: /ops/ })).toBeChecked();
        const others = within(menu)
          .getAllByRole("checkbox")
          .filter((c) => !/ops/.test(c.closest("label")?.textContent ?? ""));
        expect(others.length).toBeGreaterThan(0);
        for (const c of others) expect(c).not.toBeChecked();
        // Showing a team again from the checklist turns the funnel off.
        fireEvent.click(others[0]!);
        rerenderPane(rerender);
        expect(teamFunnel("acme:ops")).toHaveAttribute("aria-pressed", "false");
        expect(teamChip()).toBeNull();
        // Hiding the other team from the checklist turns this team's funnel on.
        hiddenPaneTeams = ["team-1"];
        rerenderPane(rerender);
        expect(teamFunnel("acme:ops")).toHaveAttribute("aria-pressed", "true");
        expect(teamChip()).toHaveTextContent("Team: ops");
      });

      it("is a real button that takes keyboard focus, shown on focus as on hover", async () => {
        twoTeams();
        renderList();
        await screen.findByRole("button", { name: agentRow(/elsewhere/) });
        const funnel = screen.getByRole("button", { name: "Filter to ops" });
        expect(funnel.tagName).toBe("BUTTON");
        expect(funnel).not.toHaveAttribute("tabindex", "-1");
        funnel.focus();
        expect(funnel).toHaveFocus();
        const css = readFileSync(resolve(__dirname, "../../../styles/agents.css"), "utf-8");
        expect(css).toMatch(/\.ag-team:focus-within \.ag-team-funnel/);
        expect(css).toMatch(/\.ag-team-funnel:focus-visible \{\s*outline: 2px solid var\(--ring\)/);
      });

      it("is not offered when only one team can show", async () => {
        renderList();
        await screen.findAllByRole("button", { name: agentRow(/./) });
        expect(document.querySelector('[data-testid="agents-team-funnel"]')).toBeNull();
      });
    });

    it("a workspace card body click folds that workspace; fold state is kept per workspace", async () => {
      twoTeams();
      renderList();
      await screen.findByRole("button", { name: agentRow(/worker/) });
      fireEvent.click(workspaceGroup("team-1|ws-a").querySelector(".ag-ws-head-row")!);
      expect(setAgentsPaneFold).toHaveBeenCalledWith("#ws-fold:team-1|ws-a", true);
    });

    it("the workspace arrow filters the list to that workspace with a chip; the arrow or the chip's × clears it", async () => {
      twoTeams();
      const { rerender } = renderList();
      await screen.findByRole("button", { name: agentRow(/elsewhere/) });
      fireEvent.click(screen.getByRole("button", { name: "Filter to Auth flow" }));
      expect(setAgentsWorkspaceFilter).toHaveBeenLastCalledWith({
        teamKey: "team-1",
        workspaceId: "ws-a",
      });
      rerenderPane(rerender);
      expect(screen.queryByRole("button", { name: agentRow(/elsewhere/) })).toBeNull();
      expect(screen.getByRole("button", { name: agentRow(/worker/) })).toBeInTheDocument();
      expect(document.querySelector('[data-chip="ws"]')).toHaveTextContent("Auth flow");

      fireEvent.click(screen.getByRole("button", { name: "Clear the Auth flow filter" }));
      expect(setAgentsWorkspaceFilter).toHaveBeenLastCalledWith(null);
      rerenderPane(rerender);
      expect(screen.getByRole("button", { name: agentRow(/elsewhere/) })).toBeInTheDocument();
    });

    it("a row click selects the agent, shows its tab, focuses the CLI, and shows no toast", async () => {
      twoTeams();
      renderList();
      await screen.findByRole("button", { name: agentRow(/worker/) });
      const row = screen.getByRole("button", { name: agentRow(/worker/) });
      row.focus();
      fireEvent.click(row);
      expect(setActiveTerminalTab).toHaveBeenCalledWith("team-1", "tab-w", "ws-a");
      // A row click focuses the terminal, so the user can type straight away.
      expect(focusActiveWorkspaceTerminal).toHaveBeenCalledWith("team-1");
      expect(toast).not.toHaveBeenCalled();
    });

    it("a via chip reveals its manager row and says nothing when the manager is listed", async () => {
      twoTeams();
      renderList();
      await screen.findByRole("button", { name: agentRow(/worker/) });
      // Under its launcher the row has no chip; by status it does.
      fireEvent.click(within(filterMenu()).getByRole("radio", { name: "By status" }));
      // The chip sits inside the row's own button, so it is no control of its own: a click on it
      // selects the launcher, and keyboard users get "Go to lead" in the row's menu.
      const pill = await screen.findByTestId("agent-manager-pill");
      expect(pill).not.toHaveAttribute("role");
      expect(pill).not.toHaveAttribute("tabindex");
      fireEvent.click(pill);
      expect(rowOf("lead")).toBeInTheDocument();
      expect(toast).not.toHaveBeenCalled();
    });

    it("a launched row's menu offers Go to its launcher", async () => {
      twoTeams();
      renderList();
      const worker = await screen.findByRole("button", { name: agentRow(/worker/) });
      fireEvent.contextMenu(worker);
      fireEvent.click(await screen.findByRole("menuitem", { name: "Go to lead" }));
      expect(rowOf("lead")).toBeInTheDocument();
      expect(toast).not.toHaveBeenCalled();
    });

    it("Arrow Down moves focus to the next visible row", async () => {
      twoTeams();
      renderList();
      await waitFor(() => expect(rowOf("lead")).toBeInTheDocument());
      const lead = rowOf("lead");
      lead.focus();
      fireEvent.keyDown(lead, { key: "ArrowDown" });
      expect(document.activeElement).toBe(screen.getByRole("button", { name: agentRow(/worker/) }));
    });

    it("the status arrangement shows sections per status with no team headers", async () => {
      twoTeams();
      renderList();
      await screen.findByRole("button", { name: agentRow(/elsewhere/) });
      fireEvent.click(within(filterMenu()).getByRole("radio", { name: "By status" }));
      expect(document.querySelector("[data-status]")).toBeInTheDocument();
      expect(screen.queryAllByTestId("agents-team-group")).toHaveLength(0);
    });

    it("the status arrangement puts an agent under the heading for its own status", async () => {
      twoTeams();
      await act(async () =>
        useAgentStateStore.getState().recordSignal("sid-lead", {
          tier: 1,
          signal: { kind: "claude-hook", event: "PermissionRequest" },
        }),
      );
      await act(async () =>
        useAgentStateStore.getState().recordSignal("sid-worker", {
          tier: 1,
          signal: { kind: "claude-hook", event: "PreToolUse" },
        }),
      );
      projectSessionStatus("sid-lead", "paused");
      projectSessionStatus("sid-worker", "tool-running");
      renderList();
      await screen.findByRole("button", { name: agentRow(/elsewhere/) });
      fireEvent.click(within(filterMenu()).getByRole("radio", { name: "By status" }));
      // Each agent's row carries the section it sits under.
      const sectionOf = (handle: string) =>
        rowOf(handle).closest<HTMLElement>("[data-row-index]")!.dataset.statusSection;
      expect(sectionOf("lead")).toBe("needs");
      expect(sectionOf("worker")).toBe("working");
      expect(rowOf("lead").querySelector(".ag-piece")).toHaveAttribute(
        "title",
        "Hierarchy: Manager · launched 1",
      );
    });

    it("the Teams checklist is grouped by organisation, and Show lists four options", async () => {
      twoTeams();
      renderList();
      await screen.findByRole("button", { name: agentRow(/elsewhere/) });
      const menu = filterMenu();
      expect(menu.querySelectorAll(".ag-fm-orgname")).toHaveLength(2);
      expect(
        Array.from(menu.querySelectorAll("button.ag-fm-item")).map((b) => b.textContent),
      ).toHaveLength(4);
    });

    it("a pinned agent follows team scope and the arrow filter", async () => {
      twoTeams();
      agentPins = {
        "team-1": [
          {
            kind: "session",
            teamKey: "acme:ops",
            handle: "elsewhere",
            sessionId: "sid-elsewhere",
            name: "elsewhere",
          },
          {
            kind: "session",
            teamKey: "team-1",
            handle: "lead",
            sessionId: "sid-lead",
            name: "lead",
          },
        ],
      };
      const { rerender } = renderList();
      await screen.findByTestId("agents-pinned");
      const pinnedRow = (handle: string) =>
        screen.getByTestId("agents-pinned").querySelector(`[data-agent-handle="${handle}"]`);
      expect(pinnedRow("elsewhere")).not.toBeNull();
      expect(pinnedRow("lead")).not.toBeNull();

      hiddenPaneTeams = ["acme:ops"];
      rerenderPane(rerender);
      expect(pinnedRow("elsewhere")).toBeNull();
      expect(pinnedRow("lead")).not.toBeNull();

      paneWorkspaceFilter = { teamKey: "acme:ops", workspaceId: "ws-x" };
      hiddenPaneTeams = [];
      rerenderPane(rerender);
      expect(pinnedRow("lead")).toBeNull();
      expect(pinnedRow("elsewhere")).not.toBeNull();
      hiddenPaneTeams = [];
      paneWorkspaceFilter = null;
      agentPins = {};
    });

    it("the arrow filter survives a team change: the list still covers every team", async () => {
      twoTeams();
      paneWorkspaceFilter = { teamKey: "team-1", workspaceId: "ws-a" };
      const { rerender } = renderList();
      await screen.findByRole("button", { name: agentRow(/worker/) });
      rerender(<DockSlotHost teamKey="acme:ops" visible />);
      expect(setAgentsWorkspaceFilter).not.toHaveBeenCalledWith(null);
      paneWorkspaceFilter = null;
    });

    it("clearing search and filters un-hides every hidden team", async () => {
      twoTeams();
      hiddenPaneTeams = ["acme:ops"];
      renderList();
      await screen.findByRole("button", { name: agentRow(/worker/) });
      fireEvent.change(screen.getByLabelText("Find agents"), { target: { value: "zzz-nothing" } });
      fireEvent.click(screen.getByRole("button", { name: "Clear search, filters and teams" }));
      expect(setAgentsPaneTeamScope).toHaveBeenCalledWith([], null);
      hiddenPaneTeams = [];
    });

    it("a live roster update keeps the list's scroll position and the focused row", async () => {
      twoTeams();
      renderList();
      await screen.findByRole("button", { name: agentRow(/worker/) });
      const scroll = document.querySelector<HTMLElement>(".ag-scroll")!;
      scroll.scrollTop = 120;
      const focused = rowOf("worker");
      focused.focus();
      act(() => {
        publishProjectionOwners({
          sessions: [...listResult, row({ handle: "newcomer", sessionId: "sid-newcomer" })],
        });
      });
      await screen.findByRole("button", { name: agentRow(/newcomer/) });
      expect(document.querySelector<HTMLElement>(".ag-scroll")).toBe(scroll);
      expect(scroll.scrollTop).toBe(120);
      expect(document.activeElement).toBe(focused);
    });

    it("no match shows the empty state, and its button clears search and filters", async () => {
      twoTeams();
      renderList();
      await screen.findByRole("button", { name: agentRow(/elsewhere/) });
      fireEvent.change(screen.getByLabelText("Find agents"), { target: { value: "zzz-nothing" } });
      expect(screen.getByText("Nothing matches")).toBeInTheDocument();
      expect(
        screen.getByText("No agent in these teams matches the current search and filters."),
      ).toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: "Clear search, filters and teams" }));
      expect(screen.getByLabelText("Find agents")).toHaveValue("");
      expect(screen.getByRole("button", { name: agentRow(/elsewhere/) })).toBeInTheDocument();
    });

    it("with every team hidden, says so, points to the Teams checklist and offers to show them all", async () => {
      twoTeams();
      hiddenPaneTeams = ["team-1", "acme:ops"];
      renderList();
      expect(await screen.findByTestId("agents-all-hidden")).toHaveTextContent(
        "Show a team from the Teams checklist.",
      );
      fireEvent.click(screen.getByRole("button", { name: "Show all teams" }));
      expect(setAgentsPaneTeamScope).toHaveBeenLastCalledWith([], null);
      hiddenPaneTeams = [];
    });

    it("the arrow filter clears when its workspace is no longer in the team", async () => {
      twoTeams();
      paneWorkspaceFilter = { teamKey: "team-1", workspaceId: "ws-a" };
      const { rerender } = renderList();
      await screen.findByRole("button", { name: agentRow(/worker/) });
      workspaces = { "team-1": [] };
      rerender(<DockSlotHost teamKey="team-1" visible />);
      expect(setAgentsWorkspaceFilter).toHaveBeenCalledWith(null);
      paneWorkspaceFilter = null;
    });

    describe("a saved arrow filter on a remote workspace that arrives with its machine's layout", () => {
      // The machine is admitted, but its layout (which brings the workspace and its tab) is not
      // read yet.
      function pendingRemote() {
        twoTeams();
        listResult = [...listResult, row({ handle: "far", sessionId: "sid-far" })];
        // The filter knows the machine its workspace runs on: only that machine's layout can
        // bring the workspace.
        paneWorkspaceFilter = { teamKey: "team-1", workspaceId: "ws-r", machineId: "box" };
        layoutAdmission = {
          "box/remote": {
            member: { machineId: "box", slot: "remote" },
            revision: "1",
            state: "live",
            namespace: {},
            historical: null,
          },
        };
        layoutAdmittedAt = { "box/remote": Date.now() };
      }

      it("survives while the layout loads, says it is loading, then applies once the workspace arrives", async () => {
        pendingRemote();
        const { rerender } = renderList();
        expect(await screen.findByTestId("agents-workspace-pending")).toHaveTextContent(
          "Loading this workspace",
        );
        expect(screen.queryByTestId("agents-no-match")).toBeNull();
        await act(async () => {});
        expect(setAgentsWorkspaceFilter).not.toHaveBeenCalledWith(null);
        // The layout is read: the workspace and its agent's tab arrive.
        layoutMembers = { "box/remote": { workspaces: {} } };
        workspaces = { ...workspaces, "team-1": [...workspaces["team-1"], { id: "ws-r" }] };
        terminalTabIndex = {
          ...terminalTabIndex,
          "team-1": {
            ...terminalTabIndex["team-1"],
            "ws-r": [{ id: "tab-far", sessionId: "sid-far" }],
          },
        };
        act(() => publishProjectionOwners({ sessions: listResult }));
        rerenderPane(rerender);
        expect(await screen.findByRole("button", { name: agentRow(/far/) })).toBeInTheDocument();
        expect(screen.queryByRole("button", { name: agentRow(/worker/) })).toBeNull();
        expect(screen.queryByTestId("agents-workspace-pending")).toBeNull();
        await act(async () => {});
        expect(setAgentsWorkspaceFilter).not.toHaveBeenCalledWith(null);
        paneWorkspaceFilter = null;
      });

      it("is dropped once the layouts have loaded and the workspace is not there", async () => {
        pendingRemote();
        const { rerender } = renderList();
        await screen.findByTestId("agents-workspace-pending");
        expect(setAgentsWorkspaceFilter).not.toHaveBeenCalledWith(null);
        layoutMembers = { "box/remote": { workspaces: {} } };
        rerenderPane(rerender);
        await waitFor(() => expect(setAgentsWorkspaceFilter).toHaveBeenCalledWith(null));
        paneWorkspaceFilter = null;
      });

      it("says the workspace is loading while its role reads busy", async () => {
        pendingRemote();
        renderList();
        const note = await screen.findByTestId("agents-workspace-pending");
        expect(note).toHaveAttribute("role", "status");
        expect(note).toHaveAttribute("aria-busy", "true");
        paneWorkspaceFilter = null;
      });
    });

    describe("a saved arrow filter on a workspace whose machine is offline at launch", () => {
      function offlineRemote(state = "unreachable") {
        twoTeams();
        useMachinesStore.setState({
          byTeam: {
            "team-1": [
              { machineId: "box", label: "Build box", kind: "ssh", status: { state } } as never,
            ],
          },
        });
        // The workspace's record is saved on this Mac; the machine sends no layout, so its
        // agent has no tab yet.
        workspaces = {
          ...workspaces,
          "team-1": [
            ...workspaces["team-1"],
            { id: "ws-r", location: { kind: "machine", machineId: "box" } },
          ],
        };
        listResult = [...listResult, row({ handle: "far", sessionId: "sid-far" })];
        paneWorkspaceFilter = { teamKey: "team-1", workspaceId: "ws-r", machineId: "box" };
      }

      it.each(["unreachable", "disconnected"])(
        "is kept while the machine is %s, saying it waits for it, then applies once it connects",
        async (state) => {
          offlineRemote(state);
          const { rerender } = renderList();
          const note = await screen.findByTestId("agents-workspace-waiting");
          expect(note).toHaveTextContent("Waiting for Build box");
          expect(note).toHaveTextContent(
            "This workspace runs on Build box, which is offline. Its agents show once it reconnects.",
          );
          expect(note).toHaveAttribute("role", "status");
          expect(screen.queryByTestId("agents-no-match")).toBeNull();
          // Well past every load's wait, it is still kept.
          await act(async () => {});
          expect(setAgentsWorkspaceFilter).not.toHaveBeenCalledWith(null);
          // The machine connects: its layout brings the agent's tab into the workspace.
          layoutMembers = { "box/remote": { workspaces: {} } };
          terminalTabIndex = {
            ...terminalTabIndex,
            "team-1": {
              ...terminalTabIndex["team-1"],
              "ws-r": [{ id: "tab-far", sessionId: "sid-far" }],
            },
          };
          act(() => publishProjectionOwners({ sessions: listResult }));
          rerenderPane(rerender);
          expect(await screen.findByRole("button", { name: agentRow(/far/) })).toBeInTheDocument();
          expect(screen.queryByTestId("agents-workspace-waiting")).toBeNull();
          await act(async () => {});
          expect(setAgentsWorkspaceFilter).not.toHaveBeenCalledWith(null);
          paneWorkspaceFilter = null;
        },
      );

      it("says it waits for the terminals of a machine that reads connected, and offers to clear the workspace filter", async () => {
        offlineRemote("connected");
        renderList();
        const note = await screen.findByTestId("agents-workspace-waiting");
        expect(note).toHaveTextContent(
          "This workspace runs on Build box. Its agents show once its terminals can be read.",
        );
        fireEvent.click(within(note).getByRole("button", { name: "Clear the workspace filter" }));
        expect(setAgentsWorkspaceFilter).toHaveBeenCalledWith(null);
        paneWorkspaceFilter = null;
      });

      it("still waits for its machine while another machine's layout loads", async () => {
        offlineRemote();
        layoutAdmission = {
          "other/remote": {
            member: { machineId: "other", slot: "remote" },
            revision: "1",
            state: "live",
            namespace: {},
            historical: null,
          },
        };
        layoutAdmittedAt = { "other/remote": Date.now() };
        renderList();
        expect(await screen.findByTestId("agents-workspace-waiting")).toBeInTheDocument();
        expect(screen.queryByTestId("agents-workspace-pending")).toBeNull();
        paneWorkspaceFilter = null;
      });

      it("leaves the empty list's own note to a search that hides every agent", async () => {
        offlineRemote();
        paneSearch = { query: "nothing-matches", status: "all" };
        renderList();
        expect(await screen.findByTestId("agents-no-match")).toBeInTheDocument();
        expect(screen.queryByTestId("agents-workspace-waiting")).toBeNull();
        paneSearch = { query: "", status: "all" };
        paneWorkspaceFilter = null;
      });

      it("says the workspace is loading while its own machine is connecting, then waits for it once that runs past its wait", async () => {
        vi.useFakeTimers({ shouldAdvanceTime: true });
        try {
          offlineRemote("connecting");
          renderList();
          expect(await screen.findByTestId("agents-workspace-pending")).toHaveTextContent(
            "Loading this workspace",
          );
          await act(async () => {
            vi.advanceTimersByTime(REMOTE_LAYOUT_WAIT_MS + 1);
          });
          expect(await screen.findByTestId("agents-workspace-waiting")).toHaveTextContent(
            "Its agents show once its terminals can be read.",
          );
          expect(setAgentsWorkspaceFilter).not.toHaveBeenCalledWith(null);
        } finally {
          vi.useRealTimers();
          paneWorkspaceFilter = null;
        }
      });

      it("does not call a machine whose status no list has read offline", async () => {
        offlineRemote();
        useMachinesStore.setState({
          byTeam: { "team-1": [{ machineId: "box", label: "Build box", kind: "ssh" } as never] },
        });
        renderList();
        expect(await screen.findByTestId("agents-workspace-waiting")).toHaveTextContent(
          "This workspace runs on Build box. Its agents show once its terminals can be read.",
        );
        paneWorkspaceFilter = null;
      });

      it("leaves the empty list's own note to the Show filter while the workspace loads", async () => {
        offlineRemote("connecting");
        paneSearch = { query: "", status: "needs" };
        renderList();
        expect(await screen.findByTestId("agents-no-match")).toBeInTheDocument();
        expect(screen.queryByTestId("agents-workspace-pending")).toBeNull();
        paneSearch = { query: "", status: "all" };
        paneWorkspaceFilter = null;
      });

      it("names a machine no list knows plainly", async () => {
        offlineRemote();
        useMachinesStore.setState({ byTeam: {} });
        renderList();
        expect(await screen.findByTestId("agents-workspace-waiting")).toHaveTextContent(
          "Waiting for a remote machine",
        );
        expect(setAgentsWorkspaceFilter).not.toHaveBeenCalledWith(null);
        paneWorkspaceFilter = null;
      });

      it("is dropped once its machine is gone from the team's machines", async () => {
        offlineRemote();
        useMachinesStore.setState({ byTeam: { "team-1": [] } });
        workspaces = {
          ...workspaces,
          "team-1": workspaces["team-1"].filter((w) => w.id !== "ws-r"),
        };
        renderList();
        await waitFor(() => expect(setAgentsWorkspaceFilter).toHaveBeenCalledWith(null));
        paneWorkspaceFilter = null;
      });
    });

    describe("a saved arrow filter that can't be settled yet", () => {
      it("on a deleted workspace of This Mac is dropped once This Mac's state is known, though a machine's layout still loads", async () => {
        twoTeams();
        paneWorkspaceFilter = { teamKey: "team-1", workspaceId: "ws-gone" };
        layoutAdmission = {
          "box/remote": {
            member: { machineId: "box", slot: "remote" },
            revision: "1",
            state: "live",
            namespace: {},
            historical: null,
          },
        };
        layoutAdmittedAt = { "box/remote": Date.now() };
        persistedStateRestored = false;
        const { rerender } = renderList();
        await act(async () => {});
        expect(setAgentsWorkspaceFilter).not.toHaveBeenCalledWith(null);
        persistedStateRestored = true;
        rerenderPane(rerender);
        await waitFor(() => expect(setAgentsWorkspaceFilter).toHaveBeenCalledWith(null));
        paneWorkspaceFilter = null;
      });

      it("says its workspace is loading on a cold launch with no agents yet, not that there are none", async () => {
        twoTeams();
        listResult = [];
        terminalTabIndex = {};
        paneWorkspaceFilter = { teamKey: "team-1", workspaceId: "ws-a" };
        publishProjectionOwners({
          sessions: null,
          error: null,
          references: [],
          partial: false,
          homeTeam: null,
          teamOrder: [],
        });
        render(<DockSlotHost teamKey="team-1" visible />);
        const note = await screen.findByTestId("agents-workspace-pending");
        expect(note).toHaveTextContent("Loading this workspace");
        expect(note).toHaveAttribute("aria-busy", "true");
        expect(screen.queryByText("No agents running")).toBeNull();
        expect(setAgentsWorkspaceFilter).not.toHaveBeenCalledWith(null);
        paneWorkspaceFilter = null;
      });

      it("stops saying it is loading once the roster's read fails, and keeps the filter", async () => {
        twoTeams();
        listResult = [];
        terminalTabIndex = {};
        paneWorkspaceFilter = { teamKey: "team-1", workspaceId: "ws-a" };
        publishProjectionOwners({
          sessions: null,
          error: null,
          references: [],
          partial: false,
          homeTeam: null,
          teamOrder: [],
        });
        render(<DockSlotHost teamKey="team-1" visible />);
        await screen.findByTestId("agents-workspace-pending");
        act(() => publishProjectionOwners({ error: "the roster can't be read" }));
        await waitFor(() => expect(screen.queryByTestId("agents-workspace-pending")).toBeNull());
        expect(setAgentsWorkspaceFilter).not.toHaveBeenCalledWith(null);
        paneWorkspaceFilter = null;
      });

      it("stops saying it is loading once a first load that never settles has waited its bound", async () => {
        vi.useFakeTimers({ shouldAdvanceTime: true });
        try {
          twoTeams();
          listResult = [];
          terminalTabIndex = {};
          paneWorkspaceFilter = { teamKey: "team-1", workspaceId: "ws-a" };
          publishProjectionOwners({
            sessions: null,
            error: null,
            references: [],
            partial: false,
            homeTeam: null,
            teamOrder: [],
          });
          render(<DockSlotHost teamKey="team-1" visible />);
          await screen.findByTestId("agents-workspace-pending");
          await act(async () => {
            vi.advanceTimersByTime(REMOTE_LAYOUT_WAIT_MS + 1);
          });
          expect(screen.queryByTestId("agents-workspace-pending")).toBeNull();
          expect(setAgentsWorkspaceFilter).not.toHaveBeenCalledWith(null);
        } finally {
          vi.useRealTimers();
          paneWorkspaceFilter = null;
        }
      });
    });

    it("a story workspace card shows its stage, and choosing from it does not fold the card", async () => {
      twoTeams();
      workspaces = {
        "team-1": [
          {
            id: "ws-a",
            kind: "story",
            status: "ready",
            storyTitle: "Auth flow",
            storyStatus: "in-progress",
          },
        ],
        "acme:ops": [{ id: "ws-x" }],
      };
      renderList();
      const stage = await screen.findByRole("button", { name: /Change status for Auth flow/ });
      fireEvent.click(stage);
      expect(workspaceGroup("team-1|ws-a")).not.toHaveAttribute("data-collapsed");
    });

    it("with a hundred-plus agents, only the rows near the viewport are drawn", async () => {
      twoTeams();
      listResult = Array.from({ length: 120 }, (_, i) =>
        row({ handle: `bulk-${i}`, sessionId: `sid-bulk-${i}` }),
      );
      renderList();
      await screen.findByRole("button", { name: agentRow(/bulk-0\b/) });
      const drawn = document.querySelectorAll("[data-row-index]").length;
      expect(drawn).toBeGreaterThan(0);
      // The test viewport is 4000px tall, so about 90 rows fit; the rest are not drawn.
      expect(drawn).toBeLessThan(120);
      expect(screen.getByTestId("agents-count")).toHaveTextContent(/^120$/);
    });

    it("arrow keys walk the entries of the Pinned group before they leave it", async () => {
      twoTeams();
      agentPins = {
        "team-1": [
          {
            kind: "session",
            teamKey: "team-1",
            handle: "lead",
            sessionId: "sid-lead",
            name: "lead",
          },
          {
            kind: "session",
            teamKey: "team-1",
            handle: "worker",
            sessionId: "sid-worker",
            name: "worker",
          },
        ],
      };
      renderList();
      await screen.findByTestId("agents-pinned");
      const [first, second] = Array.from(
        document.querySelectorAll<HTMLElement>("[data-testid='agents-pinned'] .ag-row .ag-main"),
      );
      first!.focus();
      fireEvent.keyDown(first!, { key: "ArrowDown" });
      expect(document.activeElement).toBe(second);
      agentPins = {};
    });

    it("ArrowUp from the first agent below the Pinned group lands on the group's last entry", async () => {
      twoTeams();
      projectedTeamOrder = ["acme:ops", "team-1"];
      agentPins = {
        "team-1": [
          {
            kind: "session",
            teamKey: "team-1",
            handle: "worker",
            sessionId: "sid-worker",
            name: "worker",
          },
        ],
      };
      renderList();
      await screen.findByTestId("agents-pinned");
      const teamAgent = document.querySelector<HTMLElement>(
        "[data-row-kind='agent'] [data-agent-handle='elsewhere'] .ag-main",
      )!;
      teamAgent.focus();
      fireEvent.keyDown(teamAgent, { key: "ArrowUp" });
      const pinnedStops = document.querySelectorAll<HTMLElement>(
        "[data-testid='agents-pinned'] .ag-row .ag-main",
      );
      expect(document.activeElement).toBe(pinnedStops[pinnedStops.length - 1]);
      agentPins = {};
    });

    it("a row says its status word, then what the agent was started with; an unknown status says none", async () => {
      listResult = [
        row({ handle: "quiet" }),
        row({ handle: "briefed", startedWith: "Refactor auth" }),
      ];
      renderList();
      const quiet = await screen.findByRole("button", { name: agentRow(/quiet/) });
      // Its hollow dot and tooltip say nothing reports on it; line one keeps the name.
      expect(quiet.closest(".ag-row")!.querySelector(".ag-task")).toBeNull();
      expect(quiet.getAttribute("title")).toMatch(/hooks are not connected/);
      const briefed = screen.getByRole("button", { name: agentRow(/briefed/) });
      const briefedTask = briefed.closest(".ag-row")!.querySelector(".ag-task");
      expect(briefedTask).toHaveTextContent("Refactor auth");
      expect(briefedTask).not.toHaveTextContent("Activity unknown");
    });

    it("a via chip is struck through when its manager's machine is offline", async () => {
      useMachinesStore.setState({
        byTeam: {
          "team-1": [
            {
              machineId: "box",
              label: "Studio Mac",
              kind: "ssh",
              status: { state: "disconnected" },
            } as never,
          ],
        },
      });
      workspaces = { "team-1": [{ id: "ws-a" }] };
      terminalTabIndex = {
        "team-1": {
          "ws-a": [
            {
              id: "remote-1",
              sessionId: "remote-1",
              origin: { machineId: "box", member: "remote:box", handle: "h-1" },
            } as Tab,
          ],
        },
      };
      listResult = [
        row({ handle: "far", sessionId: "remote-1" }),
        row({ handle: "near", sessionId: "sid-near", launchedBy: "far" }),
      ];
      renderList();
      const near = await screen.findByRole("button", { name: agentRow(/near/) });
      const pill = near.closest(".ag-row")!.querySelector(".ag-via-pill");
      expect(pill).toHaveAttribute("data-manager-offline", "true");
    });

    it("a waiting row keeps its status word on line one when its wait line does not say it", async () => {
      listResult = [row({ handle: "gate" })];
      await act(async () =>
        useAgentStateStore.getState().recordSignal("sid-gate", {
          tier: 1,
          signal: { kind: "claude-hook", event: "PermissionRequest" },
        }),
      );
      projectSessionStatus("sid-gate", "paused");
      renderList();
      const gate = await screen.findByRole("button", { name: agentRow(/gate/) });
      expect(gate.closest(".ag-row")!.querySelector(".ag-task")).toHaveTextContent(
        "· Needs approval",
      );
      expect(gate.querySelector(".ag-ask")).toHaveTextContent("Waiting for your approval");
    });

    it("a row whose wait line shows only the question keeps Needs answer on line one", async () => {
      listResult = [row({ handle: "asker" })];
      useManagedSessionStore.setState({
        states: { "sid-asker": "awaiting-answer" },
        pendingAsks: { q1: { sessionId: "sid-asker", question: "Ship on Friday?" } },
      } as never);
      projectSessionStatus("sid-asker", "awaiting-answer");
      renderList();
      const asker = await screen.findByRole("button", { name: agentRow(/asker/) });
      expect(asker.querySelector(".ag-ask")).toHaveTextContent("Ship on Friday?");
      expect(asker.querySelector(".ag-task")).toHaveTextContent("· Needs answer");
    });

    it("a blocked row whose wait line says Blocked does not say it again on line one", async () => {
      listResult = [row({ handle: "stuck" })];
      await act(async () =>
        useAgentStateStore.getState().recordSignal("sid-stuck", {
          tier: 1,
          signal: { kind: "signal-progress", phase: "blocked" },
        }),
      );
      progressBySessionMock = { "sid-stuck": { phase: "blocked", note: "waiting on CI" } };
      projectSessionStatus("sid-stuck", "blocked");
      renderList();
      const stuck = await screen.findByRole("button", { name: agentRow(/stuck/) });
      expect(stuck.querySelector(".ag-ask")).toHaveTextContent("Blocked: waiting on CI");
      expect(stuck.querySelector(".ag-task")).toBeNull();
    });

    it("an offline row keeps the prompt it was started with, and drops its status word", async () => {
      useMachinesStore.setState({
        byTeam: {
          "team-1": [
            {
              machineId: "box",
              label: "Studio Mac",
              kind: "ssh",
              status: { state: "disconnected" },
            } as never,
          ],
        },
      });
      workspaces = { "team-1": [{ id: "ws-a" }] };
      terminalTabIndex = {
        "team-1": {
          "ws-a": [
            {
              id: "remote-2",
              sessionId: "remote-2",
              origin: { machineId: "box", member: "remote:box", handle: "h-2" },
            } as Tab,
          ],
        },
      };
      listResult = [
        row({ handle: "briefed", sessionId: "remote-2", startedWith: "Refactor auth" }),
      ];
      renderList();
      const briefed = await screen.findByRole("button", { name: agentRow(/briefed/) });
      const task = briefed.closest(".ag-row")!.querySelector(".ag-task");
      expect(task).toHaveTextContent("Refactor auth");
      expect(task).not.toHaveTextContent("Activity unknown");
      expect(task).not.toHaveTextContent("Working");
    });

    it("a via chip whose manager a search hides says so, with a clear action", async () => {
      twoTeams();
      renderList();
      await screen.findByRole("button", { name: agentRow(/worker/) });
      fireEvent.change(screen.getByLabelText("Find agents"), { target: { value: "worker" } });
      const worker = await screen.findByRole("button", { name: agentRow(/worker/) });
      fireEvent.click(worker.closest(".ag-row")!.querySelector(".ag-via-pill")!);
      expect(toast).toHaveBeenCalledWith(
        "lead is hidden by the current search or filter.",
        expect.objectContaining({
          action: expect.objectContaining({ label: "Clear search, filters and teams" }),
        }),
      );
    });

    it("a remote agent's row shows its machine's mark, as the disconnected glyph while it is offline", async () => {
      useMachinesStore.setState({
        byTeam: {
          "team-1": [
            {
              machineId: "box",
              label: "Studio Mac",
              kind: "ssh",
              status: { state: "disconnected" },
            } as never,
          ],
        },
      });
      workspaces = { "team-1": [{ id: "ws-a" }] };
      terminalTabIndex = {
        "team-1": {
          "ws-a": [
            {
              id: "remote-3",
              sessionId: "remote-3",
              origin: { machineId: "box", member: "remote:box", handle: "h-3" },
            } as Tab,
          ],
        },
      };
      listResult = [row({ handle: "remote", sessionId: "remote-3" })];
      renderList();
      const remote = await screen.findByRole("button", { name: agentRow(/remote/) });
      const mark = within(remote.closest(".ag-row") as HTMLElement).getByTestId(
        "agent-row-machine",
      );
      expect(mark).toHaveAttribute("data-offline", "true");
    });

    it("searching for offline finds an agent whose machine cannot be reached", async () => {
      useMachinesStore.setState({
        byTeam: {
          "team-1": [
            {
              machineId: "box",
              label: "Studio Mac",
              kind: "ssh",
              status: { state: "disconnected" },
            } as never,
          ],
        },
      });
      workspaces = { "team-1": [{ id: "ws-a" }] };
      terminalTabIndex = {
        "team-1": {
          "ws-a": [
            {
              id: "remote-4",
              sessionId: "remote-4",
              origin: { machineId: "box", member: "remote:box", handle: "h-4" },
            } as Tab,
          ],
        },
      };
      listResult = [row({ handle: "stuck", sessionId: "remote-4" }), row({ handle: "fine" })];
      renderList();
      await screen.findByRole("button", { name: agentRow(/stuck/) });
      fireEvent.change(screen.getByLabelText("Find agents"), { target: { value: "offline" } });
      expect(screen.getByRole("button", { name: agentRow(/stuck/) })).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: agentRow(/fine/) })).toBeNull();
    });
  });
});

describe("moving a workspace card (the shared order with the tray)", () => {
  /** team-1: three folder workspaces, one agent each, launched c, a, b; acme:ops: one more. */
  function threeCards() {
    workspaces = {
      "team-1": ["ws-a", "ws-b", "ws-c"].map((id) => ({
        id,
        kind: "generic",
        status: "ready",
        cwd: `/w/${id}`,
      })),
      "acme:ops": [{ id: "ws-x", kind: "generic", status: "ready", cwd: "/w/ws-x" }],
    };
    terminalTabIndex = {
      "team-1": Object.fromEntries(
        ["a", "b", "c"].map((k) => [`ws-${k}`, [{ id: `tab-${k}`, sessionId: `sid-${k}` }]]),
      ),
      "acme:ops": { "ws-x": [{ id: "tab-x", sessionId: "sid-x" }] },
    };
    listResult = [
      row({ handle: "agent-c", sessionId: "sid-c" }),
      row({ handle: "agent-a", sessionId: "sid-a" }),
      row({ handle: "agent-b", sessionId: "sid-b" }),
      row({ handle: "agent-x", sessionId: "sid-x" }),
    ];
  }
  const cards = (team = "team-1") =>
    [...document.querySelectorAll<HTMLElement>("[data-testid='agents-workspace-group']")]
      .filter((el) => el.dataset.groupKey?.startsWith(`${team}|`))
      .map((el) => el.dataset.workspaceId);
  const head = (id: string, team = "team-1") =>
    document.querySelector<HTMLElement>(`[data-group-key="${team}|${id}"] .ag-ws-head-row`)!;
  /** Every row is 40px here, so a row's list y is its index × 40 (the list is not scrolled). */
  const rowY = (id: string, team = "team-1") =>
    Number(head(id, team).closest<HTMLElement>("[data-row-index]")!.dataset.rowIndex) * 40;
  const P = { pointerId: 1, button: 0, isPrimary: true, clientX: 30 };
  const press = (id: string, team = "team-1") =>
    fireEvent.pointerDown(head(id, team), { ...P, clientY: rowY(id, team) + 20 });
  const moveTo = (clientY: number) => fireEvent.pointerMove(window, { ...P, clientY });
  const release = (clientY: number) => fireEvent.pointerUp(window, { ...P, clientY });
  const agentShown = (handle: string) => document.querySelector(`[data-agent-handle="${handle}"]`);

  it("the cards follow the tray's order, not launch order", async () => {
    threeCards();
    renderList();
    await screen.findByRole("button", { name: agentRow(/agent-x/) });
    expect(cards()).toEqual(["ws-a", "ws-b", "ws-c"]);
  });

  it("follows a move made in the tray (the same stored order)", async () => {
    threeCards();
    workspaceOrderState = { "team-1": ["ws-b", "ws-c", "ws-a"] };
    renderList();
    await screen.findByRole("button", { name: agentRow(/agent-x/) });
    expect(cards()).toEqual(["ws-b", "ws-c", "ws-a"]);
  });

  it("a drag past the threshold moves the card and its agents, through the shared reorder", async () => {
    threeCards();
    renderList();
    await screen.findByRole("button", { name: agentRow(/agent-x/) });
    const start = rowY("ws-c") + 20;
    press("ws-c");
    moveTo(start - 10);
    // While it moves its agents fold out of the list, one preview and one line show.
    expect(agentShown("agent-c")).toBeNull();
    moveTo(rowY("ws-a") + 4);
    expect(screen.getByTestId("agents-drag-preview")).toBeInTheDocument();
    expect(screen.getByTestId("agents-drop-line")).toBeInTheDocument();
    release(rowY("ws-a") + 4);
    expect(reorderWorkspace).toHaveBeenCalledWith("team-1", "ws-c", { before: "ws-a" });
    expect(cards()).toEqual(["ws-c", "ws-a", "ws-b"]);
    expect(agentShown("agent-c")).not.toBeNull();
    expect(screen.queryByTestId("agents-drag-preview")).toBeNull();
    // The click that ends the release does not fold the card.
    fireEvent.click(head("ws-c"));
    expect(setAgentsPaneFold).toHaveBeenCalledTimes(0);
    expect(screen.getByTestId("agents-move-status")).toHaveTextContent(/Moved .* above /);
  });

  it("a click (movement under the threshold) still folds the card and moves nothing", async () => {
    threeCards();
    renderList();
    await screen.findByRole("button", { name: agentRow(/agent-x/) });
    const y = rowY("ws-b") + 20;
    press("ws-b");
    moveTo(y + 3);
    release(y + 3);
    fireEvent.click(head("ws-b"));
    expect(setAgentsPaneFold).toHaveBeenCalledWith("#ws-fold:team-1|ws-b", true);
    expect(reorderWorkspace).not.toHaveBeenCalled();
    expect(screen.queryByTestId("agents-drag-preview")).toBeNull();
  });

  it("no card moves under a search, and By status has no cards to move", async () => {
    threeCards();
    paneSearch = { query: "agent", status: "all" };
    const { rerender } = renderList();
    await screen.findByRole("button", { name: agentRow(/agent-x/) });
    expect(document.querySelector("[data-reorderable]")).toBeNull();
    press("ws-c");
    moveTo(rowY("ws-a"));
    release(rowY("ws-a"));
    expect(screen.queryByTestId("agents-drag-preview")).toBeNull();
    expect(reorderWorkspace).not.toHaveBeenCalled();
    paneSearch = { query: "", status: "all" };
    paneArrange = "status";
    rerender(<DockSlotHost teamKey="team-1" visible />);
    expect(document.querySelector("[data-reorderable]")).toBeNull();
    expect(screen.queryAllByTestId("agents-workspace-group")).toHaveLength(0);
  });

  it("under a Show filter no card moves, by drag or by Alt+Up/Down", async () => {
    threeCards();
    // ws-b's agent does not need you, so its card is hidden and a drop's place would be ambiguous.
    awaitingAnswer("sid-a", "sid-c");
    paneSearch = { query: "", status: "needs" };
    renderList();
    await screen.findByRole("button", { name: agentRow(/agent-c/) });
    expect(cards()).toEqual(["ws-a", "ws-c"]);
    expect(document.querySelector("[data-reorderable]")).toBeNull();
    press("ws-c");
    moveTo(rowY("ws-c"));
    moveTo(rowY("ws-a") + 4);
    expect(screen.queryByTestId("agents-drag-preview")).toBeNull();
    release(rowY("ws-a") + 4);
    const name = head("ws-c").querySelector<HTMLElement>(".ag-ws-name")!;
    name.focus();
    fireEvent.keyDown(name, { key: "ArrowUp", altKey: true });
    fireEvent.keyDown(name, { key: "ArrowDown", altKey: true });
    expect(reorderWorkspace).not.toHaveBeenCalled();
    expect(cards()).toEqual(["ws-a", "ws-c"]);
    // The keys are not swallowed silently: the live region says why nothing moved.
    expect(screen.getByTestId("agents-move-status")).toHaveTextContent(
      "Cards can't move while a search or filter is on",
    );
  });

  it("a card cannot be dropped in another team", async () => {
    threeCards();
    renderList();
    await screen.findByRole("button", { name: agentRow(/agent-x/) });
    const target = rowY("ws-x", "acme:ops") + 20;
    press("ws-a");
    moveTo(rowY("ws-a") + 40);
    moveTo(target);
    expect(screen.getByTestId("agents-drag-preview")).toHaveAttribute("data-refused");
    expect(screen.queryByTestId("agents-drop-line")).toBeNull();
    release(target);
    expect(reorderWorkspace).not.toHaveBeenCalled();
    expect(cards()).toEqual(["ws-a", "ws-b", "ws-c"]);
  });

  it("Escape puts the card back", async () => {
    threeCards();
    renderList();
    await screen.findByRole("button", { name: agentRow(/agent-x/) });
    press("ws-c");
    moveTo(rowY("ws-c"));
    moveTo(rowY("ws-a") + 4);
    expect(agentShown("agent-c")).toBeNull();
    fireEvent.keyDown(window, { key: "Escape" });
    expect(screen.queryByTestId("agents-drag-preview")).toBeNull();
    expect(agentShown("agent-c")).not.toBeNull();
    release(rowY("ws-a") + 4);
    expect(reorderWorkspace).not.toHaveBeenCalled();
    expect(cards()).toEqual(["ws-a", "ws-b", "ws-c"]);
  });

  it("leaving the window mid-drag puts the card back", async () => {
    threeCards();
    renderList();
    await screen.findByRole("button", { name: agentRow(/agent-x/) });
    press("ws-c");
    moveTo(rowY("ws-c"));
    moveTo(rowY("ws-a") + 4);
    fireEvent.blur(window);
    expect(screen.queryByTestId("agents-drag-preview")).toBeNull();
    release(rowY("ws-a") + 4);
    expect(reorderWorkspace).not.toHaveBeenCalled();
  });

  it("Alt+Up and Alt+Down on a card's name move it one place", async () => {
    threeCards();
    renderList();
    await screen.findByRole("button", { name: agentRow(/agent-x/) });
    const name = head("ws-c").querySelector<HTMLElement>(".ag-ws-name")!;
    name.focus();
    fireEvent.keyDown(name, { key: "ArrowUp", altKey: true });
    expect(reorderWorkspace).toHaveBeenLastCalledWith("team-1", "ws-c", { before: "ws-b" });
    expect(cards()).toEqual(["ws-a", "ws-c", "ws-b"]);
    expect(document.activeElement).toBe(name);
    fireEvent.keyDown(name, { key: "ArrowDown", altKey: true });
    expect(reorderWorkspace).toHaveBeenLastCalledWith("team-1", "ws-c", { after: "ws-b" });
    expect(cards()).toEqual(["ws-a", "ws-b", "ws-c"]);
    // At the end of its team: nothing to move past, and the live region says so.
    reorderWorkspace.mockClear();
    fireEvent.keyDown(name, { key: "ArrowDown", altKey: true });
    expect(reorderWorkspace).not.toHaveBeenCalled();
    expect(screen.getByTestId("agents-move-status")).toHaveTextContent(
      "This card has no other card in its team to move past.",
    );
  });

  it("a newly opened workspace's card lands beside its epic, as the tray places it", async () => {
    threeCards();
    // ws-c is new to the stored order; the tray groups it with ws-a's epic.
    workspaceOrderState = { "team-1": ["ws-b", "ws-a"] };
    railKeysMock = { "team-1": { "ws-a": "epic-1", "ws-b": "epic-2", "ws-c": "epic-1" } };
    renderList();
    await screen.findByRole("button", { name: agentRow(/agent-x/) });
    expect(cards()).toEqual(["ws-b", "ws-a", "ws-c"]);
  });

  it("a drag alone under a Show filter says why the card can't move", async () => {
    threeCards();
    awaitingAnswer("sid-a", "sid-c");
    paneSearch = { query: "", status: "needs" };
    renderList();
    await screen.findByRole("button", { name: agentRow(/agent-c/) });
    press("ws-c");
    moveTo(rowY("ws-c"));
    moveTo(rowY("ws-a") + 4);
    release(rowY("ws-a") + 4);
    expect(reorderWorkspace).not.toHaveBeenCalled();
    expect(screen.getByTestId("agents-move-status")).toHaveTextContent(
      "Cards can't move while a search or filter is on",
    );
  });

  it("a card with no pill wears no drag hints, and a drag or Alt+arrow says it has no place to move", async () => {
    threeCards();
    // A record of a kind the tray does not know has no pill, so no place in the order.
    workspaces["team-1"]![1] = { id: "ws-b", kind: "not-a-kind", status: "ready", cwd: "/w/ws-b" };
    renderList();
    await screen.findByRole("button", { name: agentRow(/agent-x/) });
    const card = head("ws-b").closest<HTMLElement>("[data-testid='agents-workspace-group']")!;
    expect(card).not.toHaveAttribute("data-reorderable");
    expect(head("ws-b").querySelector(".ag-ws-grip")).toBeNull();
    const noPill = "This card has no pill in the tray, so it has no place in the order to move.";
    press("ws-b");
    moveTo(rowY("ws-b"));
    moveTo(rowY("ws-a") + 4);
    release(rowY("ws-a") + 4);
    expect(reorderWorkspace).not.toHaveBeenCalled();
    expect(screen.getByTestId("agents-move-status")).toHaveTextContent(noPill);
    const name = head("ws-b").querySelector<HTMLElement>(".ag-ws-name")!;
    name.focus();
    fireEvent.keyDown(name, { key: "ArrowUp", altKey: true });
    expect(reorderWorkspace).not.toHaveBeenCalled();
    expect(screen.getByTestId("agents-move-status")).toHaveTextContent(noPill);
  });

  it("names the modifier as the keyboard does: Option on macOS, Alt elsewhere", async () => {
    const platform = Object.getOwnPropertyDescriptor(navigator, "platform");
    const setPlatform = (value: string) =>
      Object.defineProperty(navigator, "platform", { configurable: true, value });
    try {
      setPlatform("MacIntel");
      threeCards();
      const { unmount } = renderList();
      await screen.findByRole("button", { name: agentRow(/agent-x/) });
      expect(head("ws-a").querySelector(".ag-ws-name")).toHaveAttribute(
        "title",
        expect.stringContaining("Option+↑ / Option+↓"),
      );
      expect(rowOf("agent-a").title).toContain("Option-click for the quick view too");
      unmount();
      setPlatform("Win32");
      renderList();
      await screen.findByRole("button", { name: agentRow(/agent-x/) });
      expect(head("ws-a").querySelector(".ag-ws-name")).toHaveAttribute(
        "title",
        expect.stringContaining("Alt+↑ / Alt+↓"),
      );
      expect(rowOf("agent-a").title).toContain("Alt-click for the quick view too");
    } finally {
      if (platform) Object.defineProperty(navigator, "platform", platform);
    }
  });

  it("a card frozen by a search says to clear it to fold", async () => {
    threeCards();
    paneSearch = { query: "agent", status: "all" };
    renderList();
    await screen.findByRole("button", { name: agentRow(/agent-x/) });
    expect(head("ws-a").querySelector(".ag-ws-name")).toHaveAttribute(
      "title",
      "Clear the search or the workspace chip to fold this card",
    );
  });

  /** threeCards with a base workspace on top: alone in its tray group (the tray's roots). */
  function rootAndThreeCards() {
    threeCards();
    workspaces["team-1"] = [
      { id: "ws-root", kind: "root", status: "ready", cwd: "/w/root" },
      ...workspaces["team-1"]!,
    ];
    terminalTabIndex["team-1"]!["ws-root"] = [{ id: "tab-root", sessionId: "sid-root" } as Tab];
    listResult = [row({ handle: "agent-root", sessionId: "sid-root" }), ...listResult];
  }

  it("the top workspace, alone in its tray group, can be dragged below the others", async () => {
    rootAndThreeCards();
    renderList();
    await screen.findByRole("button", { name: agentRow(/agent-x/) });
    expect(cards()).toEqual(["ws-root", "ws-a", "ws-b", "ws-c"]);
    press("ws-root");
    moveTo(rowY("ws-root") + 30);
    // Folded while it moves, so read the target after the fold: just inside ws-c's top.
    moveTo(rowY("ws-c") + 4);
    expect(screen.getByTestId("agents-drop-line")).toBeInTheDocument();
    release(rowY("ws-c") + 4);
    expect(reorderWorkspace).toHaveBeenCalledWith("team-1", "ws-root", { after: "ws-b" });
    expect(cards()).toEqual(["ws-a", "ws-b", "ws-root", "ws-c"]);
  });

  it("Alt+Down moves the top workspace past its group's edge, Alt+Up brings it back", async () => {
    rootAndThreeCards();
    renderList();
    await screen.findByRole("button", { name: agentRow(/agent-x/) });
    const name = head("ws-root").querySelector<HTMLElement>(".ag-ws-name")!;
    name.focus();
    fireEvent.keyDown(name, { key: "ArrowDown", altKey: true });
    expect(reorderWorkspace).toHaveBeenLastCalledWith("team-1", "ws-root", { after: "ws-a" });
    expect(cards()).toEqual(["ws-a", "ws-root", "ws-b", "ws-c"]);
    const story = head("ws-a").querySelector<HTMLElement>(".ag-ws-name")!;
    story.focus();
    fireEvent.keyDown(story, { key: "ArrowDown", altKey: true });
    expect(reorderWorkspace).toHaveBeenLastCalledWith("team-1", "ws-a", { after: "ws-root" });
    expect(cards()).toEqual(["ws-root", "ws-a", "ws-b", "ws-c"]);
  });

  it("drops in the right place when the target card is scrolled out of the drawn rows", async () => {
    const ids = Array.from({ length: 31 }, (_, i) => `ws-${String(i).padStart(3, "0")}`);
    workspaces = {
      "team-1": ids.map((id) => ({ id, kind: "generic", status: "ready", cwd: `/w/${id}` })),
    };
    terminalTabIndex = {
      "team-1": Object.fromEntries(
        ids.map((id) => [id, [{ id: `tab-${id}`, sessionId: `s-${id}` }]]),
      ),
    };
    listResult = ids.map((id) => row({ handle: `a-${id}`, sessionId: `s-${id}` }));
    // No auto-scroll frames here: the slot is read from the pointer alone.
    vi.stubGlobal("requestAnimationFrame", () => 1);
    vi.stubGlobal("cancelAnimationFrame", () => {});
    try {
      renderList();
      await screen.findByRole("button", { name: agentRow(/a-ws-000/) });
      // Every row above was drawn (and measured) once; scroll them out of the drawn window.
      const scroller = document.querySelector<HTMLElement>(".ag-scroll")!;
      act(() => {
        scroller.scrollTop = 2000;
        scroller.dispatchEvent(new Event("scroll"));
      });
      await waitFor(() =>
        expect(document.querySelector('[data-group-key="team-1|ws-005"]')).toBeNull(),
      );
      const from = rowY("ws-030");
      fireEvent.pointerDown(head("ws-030"), { ...P, clientY: from + 20 - 2000 });
      moveTo(from - 2000);
      // ws-005 sits at row 11 (list y 440), not drawn: just inside its top, above its middle.
      moveTo(445 - 2000);
      release(445 - 2000);
      // Between ws-004 and ws-005, neither of them drawn.
      expect(reorderWorkspace).toHaveBeenCalledWith("team-1", "ws-030", { after: "ws-004" });
      expect(workspaceOrderState["team-1"]!.slice(3, 7)).toEqual([
        "ws-003",
        "ws-004",
        "ws-030",
        "ws-005",
      ]);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("a real drag that ends back on its own card moves nothing and does not fold it", async () => {
    threeCards();
    renderList();
    await screen.findByRole("button", { name: agentRow(/agent-x/) });
    const y = rowY("ws-b") + 20;
    press("ws-b");
    // Past the threshold, then back over its own card.
    moveTo(y - 30);
    expect(screen.getByTestId("agents-drag-preview")).toBeInTheDocument();
    moveTo(y);
    release(y);
    expect(reorderWorkspace).not.toHaveBeenCalled();
    expect(screen.queryByTestId("agents-drag-preview")).toBeNull();
    // The release's click lands on the card it started on: swallowed, so the card stays open.
    fireEvent.click(head("ws-b"));
    expect(setAgentsPaneFold).not.toHaveBeenCalled();
    expect(agentShown("agent-b")).not.toBeNull();
    // The next click is the person's own: it folds.
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    fireEvent.click(head("ws-b"));
    expect(setAgentsPaneFold).toHaveBeenCalledWith("#ws-fold:team-1|ws-b", true);
  });
});

describe("agents whose tabs the host kept under a workspace no record keys (a fresh launch)", () => {
  /** Each agent's card, in list order: the workspace id of the card above it ("" for none). */
  function cardOfEachAgent(): Record<string, string> {
    const out: Record<string, string> = {};
    let card = "";
    for (const el of document.querySelectorAll<HTMLElement>(
      "[data-group-key][data-workspace-id], [data-agent-handle]",
    )) {
      if (el.dataset.agentHandle) out[el.dataset.agentHandle] = card;
      else card = el.dataset.workspaceId ?? "";
    }
    return out;
  }

  it("lists each one under the team's workspace in its directory, from the first paint", async () => {
    // As restored at launch: the team's workspace records, and the host's layout, where three
    // agents run in workspaces of their own (keyed by directory or launch handle) that no record
    // keys, beside one in its story's own workspace.
    workspaces = {
      "team-1": [
        {
          id: "SV-3-nav",
          kind: "story",
          status: "ready",
          storyId: "SV-3",
          storyTitle: "SV-3 — Navigation",
          cwd: "/wt/feat-SV-3",
        },
        { id: "SV-5-site", kind: "story", status: "ready", storyId: "SV-5", cwd: "/wt/feat-SV-5" },
        { id: "_worker/team-1/auto", kind: "generic", status: "ready", cwd: "/workers/auto/" },
      ],
    };
    terminalTabIndex = {
      "team-1": {
        "SV-5-site": [{ id: "sid-sv5", sessionId: "sid-sv5" }],
        "generic:/wt/feat-SV-3": [{ id: "sid-badger", sessionId: "sid-badger" }],
        "generic:/workers/auto": [{ id: "sid-auto", sessionId: "sid-auto" }],
        "_worker/team-1/elsewhere": [{ id: "sid-gone", sessionId: "sid-gone" }],
      },
    };
    layoutByKey = {
      "team-1|SV-5": { descriptor: { worktreePath: "/wt/feat-SV-5" } },
      "team-1|generic:/wt/feat-SV-3": { descriptor: { worktreePath: "/wt/feat-SV-3" } },
      "team-1|generic:/workers/auto": { descriptor: { worktreePath: "/workers/auto" } },
      // A directory no workspace of the team is in.
      "team-1|_worker/team-1/elsewhere": { descriptor: { worktreePath: "/workers/elsewhere" } },
    };
    listResult = [
      row({ handle: "SV-5", sessionId: "sid-sv5" }),
      row({ handle: "badger", sessionId: "sid-badger" }),
      row({ handle: "auto", sessionId: "sid-auto" }),
      row({ handle: "gone", sessionId: "sid-gone" }),
    ];
    projectedOverrides = {
      "sid-badger": { workspaceRef: "SV-3-nav" },
      "sid-auto": { workspaceRef: "_worker/team-1/auto" },
    };
    renderList();
    await screen.findByRole("button", { name: agentRow(/gone/) });
    expect(cardOfEachAgent()).toEqual({
      "SV-5": "SV-5-site",
      badger: "SV-3-nav",
      auto: "_worker/team-1/auto",
      gone: "",
    });
  });
});

describe("an agent on a remote machine in a workspace with no tile here", () => {
  // Its machine's host keeps its tab under a key no record on this Mac mints, in a directory on
  // the machine. A stray local tile for that same path (opened by an earlier Show) is not where
  // it runs.
  const BOX = {
    machineId: "box",
    label: "Studio Box",
    kind: "ssh",
    status: { state: "connected" },
  } as never;
  function seed(origin: {
    machineId: string;
    member: string;
    handle: string;
    offlineSince?: number;
  }) {
    useMachinesStore.setState({ byTeam: { "team-1": [BOX] } });
    workspaces = {
      "team-1": [
        { id: "ws-a", kind: "generic", status: "ready", cwd: "/a" },
        { id: "generic:/home/box/w9", kind: "generic", status: "ready", cwd: "/home/box/w9" },
      ],
    };
    terminalTabIndex = {
      "team-1": {
        "ws-a": [{ id: "local-1", sessionId: "local-1" }],
        "box-w9": [{ id: "remote-shell-9", sessionId: "remote-shell-9", origin } as Tab],
      },
    };
    layoutByKey = { "team-1|box-w9": { descriptor: { worktreePath: "/home/box/w9" } } };
    listResult = [
      row({ handle: "local", sessionId: "local-1" }),
      row({ handle: "usage-claude", sessionId: "remote-shell-9" }),
    ];
  }
  /** The workspace id of the card above each agent ("" for none). */
  function cardOfEachAgent(): Record<string, string> {
    const out: Record<string, string> = {};
    let card = "";
    for (const el of document.querySelectorAll<HTMLElement>(
      "[data-group-key][data-workspace-id], [data-agent-handle]",
    )) {
      if (el.dataset.agentHandle) out[el.dataset.agentHandle] = card;
      else card = el.dataset.workspaceId ?? "";
    }
    return out;
  }

  it("is listed under no workspace, and a click says why there is no tab to show", async () => {
    seed({ machineId: "box", member: "remote:box", handle: "h-9" });
    renderList();
    const far = await screen.findByRole("button", { name: agentRow(/usage-claude/) });
    expect(cardOfEachAgent()["usage-claude"]).toBe("");
    expect(far.closest(".ag-row")).not.toHaveAttribute("data-offline");
    fireEvent.click(far);
    await waitFor(() =>
      expect(toast).toHaveBeenCalledWith(
        "usage-claude runs on Studio Box, in a workspace that isn't open on this Mac, so there is no tab to show.",
        undefined,
      ),
    );
    expect(adoptTerminalSession).not.toHaveBeenCalled();
  });

  it("reads offline while its tab is held as it last was, though its machine reads connected", async () => {
    seed({ machineId: "box", member: "remote:box", handle: "h-9", offlineSince: 1 });
    renderList();
    const far = await screen.findByRole("button", { name: agentRow(/usage-claude/) });
    expect(within(far).getByText("Offline")).toBeInTheDocument();
    expect(far.closest(".ag-row")).toHaveAttribute("data-offline", "true");
    fireEvent.click(far);
    await waitFor(() =>
      expect(toast).toHaveBeenCalledWith(
        "CodeSense can't read Studio Box's terminals right now, so usage-claude can't be opened; the agent may still be running there.",
        undefined,
      ),
    );
    expect(adoptTerminalSession).not.toHaveBeenCalled();
  });
});

describe("the pane across teams", () => {
  function twoTeams() {
    workspaces = {
      "team-1": [{ id: "ws-a", kind: "story", status: "ready", storyTitle: "Auth flow" }],
      "acme:ops": [{ id: "ws-x" }],
    };
    terminalTabIndex = {
      "team-1": {
        "ws-a": [
          { id: "tab-lead", sessionId: "sid-lead", name: "lead" },
          { id: "tab-w", sessionId: "sid-worker", name: "worker" },
        ],
      },
      "acme:ops": { "ws-x": [{ id: "tab-x", sessionId: "sid-elsewhere" }] },
    };
    listResult = [
      row({ handle: "lead", sessionId: "sid-lead" }),
      row({ handle: "worker", sessionId: "sid-worker", launchedBy: "lead" }),
      row({ handle: "elsewhere", sessionId: "sid-elsewhere" }),
    ];
  }
  const rerenderPane = (rerender: (ui: React.ReactElement) => void, teamKey = "team-1") =>
    rerender(<DockSlotHost teamKey={teamKey} visible />);
  const workspaceGroup = (key: string) =>
    document.querySelector<HTMLElement>(`[data-group-key="${key}"]`);
  const agentShown = (handle: string) =>
    document.querySelector<HTMLElement>(`[data-row-kind="agent"] [data-agent-handle="${handle}"]`);

  it("marks the agent the CLI shows as current only in its own team, and closes its popup when its tab comes up", async () => {
    twoTeams();
    openedTeamsMock = ["acme:core", "acme:ops"];
    activeWorkspaceMock = { "acme:ops": "ws-x" };
    activeTabMock = { "acme:ops|ws-x": "sid-elsewhere" };
    // With core active, the CLI shows core: ops's focused tab is not on screen.
    const { rerender } = renderList("acme:core");
    expect(await screen.findByRole("button", { name: agentRow(/elsewhere/) })).not.toHaveAttribute(
      "aria-current",
    );

    activeTabMock = {};
    rerenderPane(rerender, "acme:ops");
    const far = await screen.findByRole("button", { name: agentRow(/elsewhere/) });
    expect(far).not.toHaveAttribute("aria-current");
    fireEvent.keyDown(far, { key: "q" });
    expect(screen.getByTestId("agent-quick-send")).toBeInTheDocument();

    activeTabMock = { "acme:ops|ws-x": "sid-elsewhere" };
    rerenderPane(rerender, "acme:ops");
    expect(rowOf("elsewhere")).toHaveAttribute("aria-current", "true");
    expect(rowOf("lead")).not.toHaveAttribute("aria-current");
    expect(screen.queryByTestId("agent-quick-send")).toBeNull();
  });

  it("a role chosen on another team's row is kept in that team, and that team's choice is drawn", async () => {
    twoTeams();
    agentIdentity = { "acme:ops": { elsewhere: { role: "qa" } } };
    renderList();
    const far = await screen.findByRole("button", { name: agentRow(/elsewhere/) });
    expect(far.querySelector(".ag-piece")).toHaveAttribute("title", "Hierarchy: QA");
    fireEvent.contextMenu(far);
    fireEvent.click(await screen.findByRole("menuitem", { name: "Role" }));
    fireEvent.click(await screen.findByRole("menuitemradio", { name: "Reviewer" }));
    expect(setAgentIdentity).toHaveBeenCalledWith("acme:ops", "elsewhere", { role: "reviewer" });
    expect(setAgentIdentity).not.toHaveBeenCalledWith("team-1", "elsewhere", expect.anything());
  });

  it("under a Show filter a folded workspace stays folded, and its fold still works", async () => {
    twoTeams();
    awaitingAnswer("sid-worker");
    paneFolds = { "#ws-fold:team-1|ws-a": true };
    paneSearch = { query: "", status: "needs" };
    const { rerender } = renderList();
    await waitFor(() => expect(workspaceGroup("team-1|ws-a")).not.toBeNull());
    expect(agentShown("worker")).toBeNull();
    const name = workspaceGroup("team-1|ws-a")!.querySelector(".ag-ws-name")!;
    expect(name).toHaveAttribute("aria-expanded", "false");
    expect(name).not.toHaveAttribute("aria-disabled");
    fireEvent.click(workspaceGroup("team-1|ws-a")!.querySelector(".ag-ws-head-row")!);
    expect(setAgentsPaneFold).toHaveBeenCalledWith("#ws-fold:team-1|ws-a", false);
    rerenderPane(rerender);
    expect(agentShown("worker")).not.toBeNull();
  });

  it("a fold is the pane's own: it survives a team switch, and a pill unfolds it where the list reads", async () => {
    twoTeams();
    const { rerender } = renderList();
    await screen.findByRole("button", { name: agentRow(/elsewhere/) });
    fireEvent.click(screen.getByRole("button", { name: /^acme.*ops$/ }));
    expect(setAgentsPaneFold).toHaveBeenCalledWith("#team-fold:acme:ops", true);
    rerenderPane(rerender, "acme:ops");
    expect(agentShown("elsewhere")).toBeNull();

    paneFolds = { "#ws-fold:acme:ops|ws-x": true };
    rerenderPane(rerender, "team-1");
    expect(agentShown("elsewhere")).toBeNull();
    const { applyPillSelection } = await import("@/features/viewer/lib/agents-pane-pill");
    act(() => applyPillSelection(useViewerStoreMock.getState() as never, "acme:ops", "ws-x"));
    rerenderPane(rerender, "team-1");
    expect(agentShown("elsewhere")).not.toBeNull();
  });

  it("the workspace chip names its workspace by its record, and never says 'No workspace' before the record loads", async () => {
    twoTeams();
    paneWorkspaceFilter = { teamKey: "acme:ops", workspaceId: "ws-x" };
    workspaces = { "team-1": workspaces["team-1"]! };
    // On launch: neither the roster nor the team's workspace records have loaded yet.
    persistedStateRestored = false;
    const { rerender } = render(<DockSlotHost teamKey="team-1" visible />);
    const chip = () => document.querySelector<HTMLElement>('.ag-chip[data-chip="ws"]');
    expect(chip()).toHaveTextContent(/^Workspace$/);

    paneWorkspaceFilter = { teamKey: "team-1", workspaceId: "ws-a" };
    persistedStateRestored = true;
    twoTeams();
    rerenderPane(rerender);
    expect(chip()).toHaveTextContent(/^Auth flow$/);
    paneWorkspaceFilter = null;
  });

  it("with a team hidden, the header reads shown/all agents while the menu's All counts the teams in view", async () => {
    twoTeams();
    hiddenPaneTeams = ["acme:ops"];
    renderList();
    await screen.findByRole("button", { name: agentRow(/worker/) });
    // The hidden team's agent is left out of view, so the count says so.
    expect(screen.getByTestId("agents-count")).toHaveTextContent(/^2\/3$/);
    expect(showCount("All")).toBe(2);
    act(() => storeState().setAgentsPaneSearch({ query: "worker" }));
    await waitFor(() => expect(screen.getByTestId("agents-count")).toHaveTextContent(/^1\/3$/));
    expect(showCount("All")).toBe(2);
    // Every team shown again, and nothing filtering: a plain total.
    act(() => storeState().setAgentsPaneSearch({ query: "" }));
    hiddenPaneTeams = [];
    act(() => notifyPaneStore());
    await waitFor(() => expect(screen.getByTestId("agents-count")).toHaveTextContent(/^3$/));
  });

  it("reads each listed team's avatar packs", async () => {
    const { useAvatarPacksStore } = await import("@/shared/hooks/use-avatar-packs");
    useAvatarPacksStore.setState({ byTeam: {} });
    twoTeams();
    renderList();
    await screen.findByRole("button", { name: agentRow(/elsewhere/) });
    await waitFor(() => expect(avatarPacksListForTeam).toHaveBeenCalledWith("acme:ops"));
    expect(avatarPacksListForTeam).toHaveBeenCalledWith("team-1");
  });

  it("the last team shown cannot be unchecked: its box is disabled and says why", async () => {
    twoTeams();
    hiddenPaneTeams = ["acme:ops"];
    renderList();
    await screen.findByRole("button", { name: agentRow(/worker/) });
    const trigger = screen.getByRole("button", { name: "Filter and arrange" });
    fireEvent.click(trigger);
    const menu = screen.getByRole("dialog", { name: "Filter and arrange agents" });
    expect(trigger).toHaveAttribute("aria-controls", menu.id);
    // Opening the menu moves focus into it.
    expect(menu.contains(document.activeElement)).toBe(true);
    const boxes = within(menu).getAllByRole("checkbox") as HTMLInputElement[];
    const shown = boxes.find((b) => b.checked)!;
    expect(shown).toBeDisabled();
    expect(shown.closest("label")).toHaveAttribute(
      "title",
      "The pane always shows at least one team",
    );
    expect(boxes.find((b) => !b.checked)).not.toBeDisabled();
    // Escape closes it and puts focus back on its button.
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "Filter and arrange agents" })).toBeNull();
    expect(trigger).toHaveFocus();
  });

  it("the filter menu closes when focus tabs out of it", async () => {
    twoTeams();
    renderList();
    await screen.findByRole("button", { name: agentRow(/worker/) });
    fireEvent.click(screen.getByRole("button", { name: "Filter and arrange" }));
    const menu = screen.getByRole("dialog", { name: "Filter and arrange agents" });
    // Moving within the menu keeps it open.
    const [first, second] = within(menu).getAllByRole("checkbox");
    fireEvent.blur(first!, { relatedTarget: second });
    expect(screen.getByRole("dialog", { name: "Filter and arrange agents" })).toBe(menu);
    fireEvent.blur(second!, { relatedTarget: rowOf("lead") });
    expect(screen.queryByRole("dialog", { name: "Filter and arrange agents" })).toBeNull();
  });

  describe("a workspace card", () => {
    afterEach(() => useOrgStore.setState({ workspaceRepos: {} }));

    it("marks its repo only in a team with two or more source repos", async () => {
      const repo = (team: string, slug: string) => ({
        repoPath: `workspace:acme:${team}:${slug}`,
        repoName: slug,
        contextRepoPath: "",
        teamContentPath: "",
      });
      useOrgStore.setState({
        workspaceRepos: {
          a: repo("ops", "acme/api"),
          b: repo("ops", "acme/web"),
          c: repo("site", "acme/site"),
        } as never,
      });
      workspaces = {
        "acme:ops": [{ id: "ws-o", kind: "story", storyId: "OPS-1", sourceRepoSlug: "acme/api" }],
        "acme:site": [
          { id: "ws-s", kind: "story", storyId: "SITE-1", sourceRepoSlug: "acme/site" },
        ],
      };
      terminalTabIndex = {
        "acme:ops": { "ws-o": [{ id: "t-o", sessionId: "sid-o" }] },
        "acme:site": { "ws-s": [{ id: "t-s", sessionId: "sid-s" }] },
      };
      listResult = [
        row({ handle: "o", sessionId: "sid-o" }),
        row({ handle: "s", sessionId: "sid-s" }),
      ];
      renderList("acme:ops");
      await waitFor(() => expect(rowOf("s")).not.toBeNull());
      expect(screen.getByTestId("agents-workspace-ws-o-repo")).toBeInTheDocument();
      expect(screen.queryByTestId("agents-workspace-ws-s-repo")).toBeNull();
    });

    it("on a remote machine shows that machine's mark", async () => {
      useMachinesStore.setState({
        byTeam: {
          "team-1": [
            { machineId: "box", label: "Studio Box", kind: "ssh", status: { state: "connected" } },
          ],
        } as never,
      });
      workspaces = {
        "team-1": [
          { id: "ws-r", status: "ready", location: { kind: "machine", machineId: "box" } },
        ],
      };
      terminalTabIndex = { "team-1": { "ws-r": [{ id: "t-r", sessionId: "sid-r" }] } };
      listResult = [row({ handle: "r", sessionId: "sid-r" })];
      renderList();
      await waitFor(() => expect(rowOf("r")).not.toBeNull());
      const mark = screen.getByTestId("agents-workspace-ws-r-machine");
      expect(mark).not.toHaveAttribute("data-offline");
    });

    it("stacks its crew: the lead wearing its role, then the others", async () => {
      twoTeams();
      renderList();
      await screen.findByRole("button", { name: agentRow(/worker/) });
      const crew = screen.getByTestId("agents-workspace-ws-a-crew");
      expect(crew.querySelector(".ag-crew-lead .ag-piece")).toHaveAttribute(
        "title",
        "Hierarchy: Manager · launched 1",
      );
      expect(crew.querySelectorAll(".ag-crew-face")).toHaveLength(1);
      // The other team's card has its own crew: its one agent leads it.
      const other = screen.getByTestId("agents-workspace-ws-x-crew");
      expect(other.querySelectorAll(".ag-crew-lead")).toHaveLength(1);
      expect(other.querySelectorAll(".ag-crew-face")).toHaveLength(0);
    });
  });

  it("a remote agent whose team's machines have not loaded is not offline yet", async () => {
    workspaces = { "team-1": [{ id: "ws-a" }] };
    terminalTabIndex = {
      "team-1": {
        "ws-a": [
          {
            id: "r1",
            sessionId: "remote-1",
            origin: { machineId: "ghost", member: "remote:ghost", handle: "h-1" },
          } as Tab,
        ],
      },
    };
    listResult = [row({ handle: "far", sessionId: "remote-1" })];
    renderList();
    const far = await screen.findByRole("button", { name: agentRow(/far/) });
    expect(within(far).getByTestId("agent-row-machine")).not.toHaveAttribute("data-offline");
    // Its team's machines load and none is this one: a machine no list knows reads neither an
    // offline mark nor the Offline tag, by the tag's own rule.
    act(() => useMachinesStore.setState({ byTeam: { "team-1": [] } }));
    await act(async () => {});
    expect(within(rowOf("far")).getByTestId("agent-row-machine")).not.toHaveAttribute(
      "data-offline",
    );
    expect(within(rowOf("far")).queryByText("Offline")).toBeNull();
  });
});

describe("the Agents pane, round two", () => {
  const css = () => readFileSync(resolve(__dirname, "../../../styles/agents.css"), "utf-8");
  function twoTeams() {
    workspaces = {
      "team-1": [{ id: "ws-a", kind: "story", status: "ready", storyTitle: "Auth flow" }],
      "acme:ops": [{ id: "ws-x" }],
    };
    terminalTabIndex = {
      "team-1": {
        "ws-a": [
          { id: "tab-lead", sessionId: "sid-lead", name: "lead" },
          { id: "tab-w", sessionId: "sid-worker", name: "worker" },
        ],
      },
      "acme:ops": { "ws-x": [{ id: "tab-x", sessionId: "sid-elsewhere" }] },
    };
    listResult = [
      row({ handle: "lead", sessionId: "sid-lead" }),
      row({ handle: "worker", sessionId: "sid-worker", launchedBy: "lead" }),
      row({ handle: "elsewhere", sessionId: "sid-elsewhere" }),
    ];
  }

  it("every class a row draws its text with has a base rule in the stylesheet", async () => {
    listResult = [row({ handle: "lead" }), row({ handle: "worker", launchedBy: "lead" })];
    hooked("sid-lead", "sid-worker");
    progressBySessionMock = { "sid-worker": { phase: "working", note: "reading the spec" } };
    renderList();
    await screen.findByRole("button", { name: agentRow(/worker/) });
    const sheet = css();
    // The classes that carry a row's text: each must be styled on its own, not only under another
    // selector or state.
    const TEXT = [
      "ag-title",
      "ag-name",
      "ag-task",
      "ag-sub",
      "ag-activity",
      "ag-ask",
      "ag-via-line",
      "ag-via-lead",
      "ag-via-head",
      "ag-via-rest",
      "ag-via-pill",
      "ag-offline-tag",
      "ag-holds",
    ];
    const drawn = new Set<string>();
    for (const el of document.querySelectorAll<HTMLElement>(".ag-row [class]")) {
      for (const c of el.classList) if (TEXT.includes(c)) drawn.add(c);
    }
    // Line one's name and task, line two, and the activity line are all drawn here.
    for (const c of ["ag-title", "ag-name", "ag-task", "ag-sub", "ag-activity"]) {
      expect(drawn).toContain(c);
    }
    for (const c of TEXT) {
      // A base rule: the class on its own, not only under another selector or state.
      expect(sheet, `.${c} has no base rule`).toMatch(
        new RegExp(`(^|[},]\\s*)\\.${c}\\s*[{,]`, "m"),
      );
    }
    // The name is bold, line one and line two keep their sizes, and the via line is sized from
    // line two's own line height.
    expect(sheet).toMatch(/\.ag-name \{\s*font-weight: 600;/);
    expect(sheet).toMatch(
      /\.ag-title \{\s*font-size: 12\.5px;\s*line-height: var\(--ag-line1-lh, 17px\);/,
    );
    expect(sheet).toMatch(
      /\.ag-sub \{\s*font-size: 11px;\s*line-height: var\(--ag-line2-lh, 15px\);/,
    );
    expect(sheet).toMatch(/\.ag-via-line \{[^}]*height: var\(--ag-line2-lh, 15px\);/);
    expect(sheet).toMatch(
      /\.ag-row-actions \{[^}]*top: calc\(6px \+ var\(--ag-line1-lh, 17px\) \/ 2 - 10px\);/,
    );
  });

  it("reduced motion stops the row's, the tab face's and the via chip's transitions", () => {
    expect(css()).toMatch(
      /@media \(prefers-reduced-motion: reduce\) \{[^@]*\.ag-row,\s*\.ag-tab-face > \.ag-face,\s*\.ag-via-pill \{\s*transition: none;/,
    );
  });

  it("each drawn list item says where it sits in the whole list", async () => {
    twoTeams();
    renderList();
    await screen.findByRole("button", { name: agentRow(/elsewhere/) });
    const items = screen.getAllByRole("listitem").filter((li) => li.hasAttribute("data-row-index"));
    expect(items.length).toBeGreaterThan(0);
    for (const li of items) {
      expect(li).toHaveAttribute("aria-setsize", String(items.length));
      expect(li).toHaveAttribute("aria-posinset", String(Number(li.dataset.rowIndex) + 1));
    }
  });

  it("team headers and workspace cards are headings by level, and each agent row says its team, card and launcher", async () => {
    twoTeams();
    renderList();
    await screen.findByRole("button", { name: agentRow(/elsewhere/) });
    // The team headings hold only their fold buttons, the cards' only their names.
    const teams = screen.getAllByRole("heading", { level: 3 });
    const cards = screen.getAllByRole("heading", { level: 4 });
    expect(teams).toHaveLength(2);
    expect(cards).toHaveLength(2);
    for (const h of [...teams, ...cards]) {
      expect(within(h).getAllByRole("button")).toHaveLength(1);
      expect(h.closest("[data-row-kind]")?.getAttribute("data-row-kind")).toBe(
        h.tagName === "H3" ? "team" : "ws",
      );
    }
    // Each agent row names the card it sits under, as that card's heading reads.
    const cardName = (handle: string) => {
      const wsKey = rowOf(handle).closest("[data-row-kind]")!.getAttribute("data-ws-key")!;
      const card = Array.from(document.querySelectorAll<HTMLElement>("[data-group-key]")).find(
        (g) => g.dataset.groupKey === wsKey,
      )!;
      return within(card).getByRole("heading", { level: 4 }).textContent?.trim() ?? "";
    };
    expect(cardName("lead")).toBe(cardName("worker"));
    expect(rowOf("lead")).toHaveAttribute(
      "aria-description",
      expect.stringMatching(new RegExp(`^Team team-1 · Workspace ${cardName("lead")}\\n`)),
    );
    expect(rowOf("worker")).toHaveAttribute(
      "aria-description",
      expect.stringMatching(
        new RegExp(`^Team team-1 · Workspace ${cardName("worker")} · Under lead\\n`),
      ),
    );
    expect(rowOf("elsewhere")).toHaveAttribute(
      "aria-description",
      expect.stringMatching(new RegExp(`^Team ops · Workspace ${cardName("elsewhere")}\\n`)),
    );
    // The description goes on to the tooltip's words, which a set description would otherwise
    // replace.
    expect(rowOf("lead")).toHaveAccessibleDescription(
      expect.stringContaining(rowOf("lead").getAttribute("title")!.split("\n")[0]!),
    );
  });

  it("with the list virtualized, a drawn row's place and heading level match the rows it was planned from", async () => {
    scrollAreaPx = 200;
    const handles = Array.from({ length: 30 }, (_, i) => `a-${String(i).padStart(2, "0")}`);
    workspaces = { "team-1": [{ id: "ws-a" }] };
    terminalTabIndex = {
      "team-1": { "ws-a": handles.map((h) => ({ id: `t-${h}`, sessionId: `sid-${h}` })) },
    };
    listResult = handles.map((h, i) =>
      row({ handle: h, sessionId: `sid-${h}`, startedAtMs: 1_000 + i }),
    );
    renderList();
    await waitFor(() =>
      expect(document.querySelectorAll("[data-row-kind='agent']").length).toBeGreaterThan(0),
    );
    const items = [...document.querySelectorAll<HTMLElement>('[role="listitem"]')];
    // Team, card and every agent; fewer drawn.
    expect(Number(items[0]!.getAttribute("aria-setsize"))).toBe(handles.length + 2);
    expect(items.length).toBeLessThan(handles.length + 2);
    for (const li of items) {
      const kind = li.dataset.rowKind;
      expect(li).toHaveAttribute("aria-posinset", String(Number(li.dataset.rowIndex) + 1));
      if (kind === "team") expect(within(li).getByRole("heading", { level: 3 })).toBeTruthy();
      if (kind === "ws") expect(within(li).getByRole("heading", { level: 4 })).toBeTruthy();
      if (kind === "agent") {
        expect(within(li).queryByRole("heading")).toBeNull();
        expect(li.querySelector(".ag-main")).toHaveAttribute(
          "aria-description",
          expect.stringMatching(/^Team team-1 · Workspace ws-a\n/),
        );
      }
    }
  });

  it("a saved session loses its running inbox actions at provider exit while keeping its identity", async () => {
    listResult = [
      row({ handle: "saved", launchedBy: "launcher", agentRevision: "acme/default/saved@1" }),
    ];
    const snapshot = {
      session: "sid-saved",
      identity: { hostBootId: "host", sessionIncarnation: "shell", invocationId: "agent" },
      revision: 1,
      turnGeneration: 0,
      provider: "codex" as const,
      state: "idle" as const,
      observed: true,
      limited: false,
      exit: null,
      asks: [],
    };
    useAgentStateStore.getState().recordActivity(snapshot, Infinity);
    renderList();
    const button = await screen.findByRole("button", { name: agentRow(/saved/) });
    expect(
      within(button.closest(".ag-row") as HTMLElement).getByTestId("agent-row-message"),
    ).toBeInTheDocument();
    act(() => {
      useAgentStateStore
        .getState()
        .recordActivity(
          { ...snapshot, revision: 2, exit: { code: 0, signal: null, requested: false } },
          Infinity,
        );
    });
    expect(
      within(button.closest(".ag-row") as HTMLElement).queryByTestId("agent-row-message"),
    ).toBeNull();
    fireEvent.keyDown(button, { key: "m" });
    expect(screen.queryByRole("dialog", { name: /Messages with saved/ })).toBeNull();
    fireEvent.contextMenu(button);
    expect(screen.queryByRole("menuitem", { name: "Open inbox" })).toBeNull();
    expect(screen.queryByRole("menuitem", { name: "Message in inbox" })).toBeNull();
    expect(useAgentListProjectionStore.getState().sessions![0]).toMatchObject({
      handle: "saved",
      launchedBy: "launcher",
      agentRevision: "acme/default/saved@1",
    });
  });

  it("a utility launcher keeps its children but has no inbox menu or M shortcut", async () => {
    listResult = [
      row({ handle: "utility", kind: "live-pty" }),
      row({ handle: "worker", launchedBy: "utility" }),
    ];
    renderList();
    const shell = await screen.findByRole("button", { name: agentRow(/utility/) });
    expect(
      within(shell.closest(".ag-row") as HTMLElement).queryByTestId("agent-row-message"),
    ).toBeNull();
    fireEvent.keyDown(shell, { key: "m" });
    expect(screen.queryByRole("dialog", { name: /Messages with utility/ })).toBeNull();
    fireEvent.contextMenu(shell);
    expect(screen.queryByRole("menuitem", { name: "Open inbox" })).toBeNull();
    expect(screen.queryByRole("menuitem", { name: "Message in inbox" })).toBeNull();
  });

  it("an agent started by hand has an inbox: its chat icon, M and the menu's inbox items all work", async () => {
    terminalTabIndex = { "team-1": { "ws-a": [{ id: "tab-sh", sessionId: "shell-7" }] } };
    workspaces = { "team-1": [{ id: "ws-a" }] };
    listResult = [row({ handle: "shell-7", sessionId: "shell-7", kind: "hand-started" })];
    renderList();
    await waitFor(() => expect(rowOf("shell-7")).toBeTruthy());
    const sh = rowOf("shell-7");
    expect(
      within(sh.closest(".ag-row") as HTMLElement).getByTestId("agent-row-message"),
    ).toBeEnabled();
    fireEvent.contextMenu(sh);
    const message = await screen.findByRole("menuitem", { name: "Message in inbox" });
    const open = screen.getByRole("menuitem", { name: "Open inbox" });
    expect(message).not.toHaveAttribute("data-disabled");
    expect(open).not.toHaveAttribute("data-disabled");
    expect(message).not.toHaveAttribute("title");
    fireEvent.click(open);
    await waitFor(() =>
      expect(useAgentInboxRailStore.getState().bySession["shell-7"]).toBeDefined(),
    );
  });

  it("⌘W inside the inbox chat closes the chat, back to its row, and marks it as a row popup", async () => {
    listResult = [row({ handle: "pinger" })];
    renderList();
    const pinger = await screen.findByRole("button", { name: agentRow(/pinger/) });
    pinger.focus();
    fireEvent.keyDown(pinger, { key: "m" });
    const chat = await screen.findByRole("dialog", { name: "Messages with pinger" });
    expect(chat).toHaveAttribute("data-row-popup");
    const { isRowPopupFocused } = await import("@/shared/lib/focus-utils");
    const inside = chat.querySelector<HTMLElement>("textarea, button")!;
    inside.focus();
    expect(isRowPopupFocused()).toBe(true);
    fireEvent.keyDown(inside, { key: "w", code: "KeyW", metaKey: true });
    expect(screen.queryByRole("dialog", { name: "Messages with pinger" })).toBeNull();
    expect(pinger).toHaveFocus();
  });

  it("the context menu's quick view is called Quick view, with Q", async () => {
    listResult = [row({ handle: "pinger" })];
    renderList();
    fireEvent.contextMenu(await screen.findByRole("button", { name: agentRow(/pinger/) }));
    const item = await screen.findByRole("menuitem", { name: "Quick view" });
    expect(item).toHaveAttribute("aria-keyshortcuts", "Q");
    fireEvent.click(item);
    expect(screen.getByRole("dialog", { name: "Quick view of pinger" })).toBeInTheDocument();
  });

  it("the machine mark reads offline by the Offline tag's rule: a frozen tab on a connected machine", async () => {
    useMachinesStore.setState({
      byTeam: {
        "team-1": [
          {
            machineId: "box",
            label: "Studio Box",
            kind: "ssh",
            status: { state: "connected" },
          } as never,
        ],
      },
    });
    workspaces = { "team-1": [{ id: "ws-a" }] };
    terminalTabIndex = {
      "team-1": {
        "ws-a": [
          {
            id: "r1",
            sessionId: "remote-1",
            origin: { machineId: "box", member: "remote:box", handle: "h-1", offlineSince: 1 },
          } as Tab,
          {
            id: "r2",
            sessionId: "remote-2",
            origin: { machineId: "box", member: "remote:box", handle: "h-2" },
          } as Tab,
        ],
      },
    };
    listResult = [
      row({ handle: "frozen", sessionId: "remote-1" }),
      row({ handle: "live", sessionId: "remote-2" }),
    ];
    renderList();
    const frozen = await screen.findByRole("button", { name: agentRow(/frozen/) });
    expect(within(frozen).getByText("Offline")).toBeInTheDocument();
    expect(within(frozen).getByTestId("agent-row-machine")).toHaveAttribute("data-offline", "true");
    const live = rowOf("live");
    expect(within(live).queryByText("Offline")).toBeNull();
    expect(within(live).getByTestId("agent-row-machine")).not.toHaveAttribute("data-offline");
  });

  it("a launched agent indents at most four levels", async () => {
    const chain = ["l0", "l1", "l2", "l3", "l4", "l5", "l6"];
    workspaces = { "team-1": [{ id: "ws-a" }] };
    terminalTabIndex = {
      "team-1": { "ws-a": chain.map((h) => ({ id: `t-${h}`, sessionId: `sid-${h}` })) },
    };
    listResult = chain.map((h, i) => row({ handle: h, launchedBy: i > 0 ? chain[i - 1] : null }));
    renderList();
    await screen.findByRole("button", { name: agentRow(/l6/) });
    const pad = (h: string) =>
      parseFloat(
        document.querySelector<HTMLElement>(`[data-agent-handle="${h}"]`)!.style.paddingLeft,
      );
    expect(pad("l1")).toBeGreaterThan(pad("l0"));
    expect(pad("l4")).toBeGreaterThan(pad("l3"));
    expect(pad("l5")).toBe(pad("l4"));
    expect(pad("l6")).toBe(pad("l4"));
  });

  it("by status, one handle in two teams is two rows, and each opens its own team's quick view", async () => {
    workspaces = {
      "team-1": [{ id: "ws-a", kind: "generic", status: "ready", cwd: "/alpha-dir" }],
      "acme:ops": [{ id: "ws-x", kind: "generic", status: "ready", cwd: "/xray-dir" }],
    };
    terminalTabIndex = {
      "team-1": { "ws-a": [{ id: "t1", sessionId: "sid-twin-1" }] },
      "acme:ops": { "ws-x": [{ id: "t2", sessionId: "sid-twin-2" }] },
    };
    listResult = [
      row({ handle: "twin", sessionId: "sid-twin-1" }),
      row({ handle: "twin", sessionId: "sid-twin-2" }),
    ];
    paneArrange = "status";
    renderList();
    await waitFor(() =>
      expect(document.querySelectorAll('[data-row-kind="status-agent"]')).toHaveLength(2),
    );
    const keys = [...document.querySelectorAll<HTMLElement>('[data-row-kind="status-agent"]')].map(
      (el) => el.dataset.rowKey,
    );
    expect(keys.sort()).toEqual(
      [
        `status-agent:${agentRowKey("acme:ops", "twin")}`,
        `status-agent:${agentRowKey("team-1", "twin")}`,
      ].sort(),
    );
    // The acme row's quick view stays open: the popup finds its row by team and handle.
    const acme = rowByKey(
      `status-agent:${agentRowKey("acme:ops", "twin")}`,
    )!.querySelector<HTMLElement>(".ag-main")!;
    const { terminalManager } = await import("@/shared/terminal/terminal-manager");
    const attach = vi.spyOn(terminalManager, "attach");
    try {
      fireEvent.keyDown(acme, { key: "q" });
      await act(async () => {});
      const dialog = screen.getByRole("dialog", { name: "Quick view of twin" });
      // It borrows the acme session's terminal and names the acme workspace.
      expect(attach.mock.calls.map((c) => c[0])).toContain("sid-twin-2");
      expect(attach.mock.calls.map((c) => c[0])).not.toContain("sid-twin-1");
      expect(dialog.querySelector(".ag-quick-send-meta")).toHaveTextContent("xray-dir");
      expect(dialog.querySelector(".ag-quick-send-meta")).not.toHaveTextContent("alpha-dir");
    } finally {
      attach.mockRestore();
    }
  });

  it("the search finds an agent by its activity note, and a home-team agent by its team", async () => {
    twoTeams();
    progressBySessionMock = { "sid-worker": { phase: "working", note: "rewriting the parser" } };
    renderList();
    await screen.findByRole("button", { name: agentRow(/elsewhere/) });
    act(() => storeState().setAgentsPaneSearch({ query: "parser" }));
    await waitFor(() => expect(rowOf("lead")).toBeNull());
    expect(rowOf("worker")).not.toBeNull();
    expect(rowOf("elsewhere")).toBeNull();
    act(() => storeState().setAgentsPaneSearch({ query: "team-1" }));
    await waitFor(() => expect(rowOf("lead")).not.toBeNull());
    expect(rowOf("worker")).not.toBeNull();
    expect(rowOf("elsewhere")).toBeNull();
  });

  it("another team's row hides its tab in its team, and Launch under opens the sheet there", async () => {
    twoTeams();
    useLaunchSheetStore.setState({ request: null });
    renderList();
    const far = await screen.findByRole("button", { name: agentRow(/elsewhere/) });
    fireEvent.contextMenu(far);
    expect(await screen.findByRole("menuitem", { name: "Go to terminal" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("menuitem", { name: "Hide tab" }));
    expect(hideTab).toHaveBeenCalledWith("acme:ops", "ws-x", "tab-x");
    fireEvent.contextMenu(rowOf("elsewhere"));
    fireEvent.click(await screen.findByRole("menuitem", { name: /^Launch agent under / }));
    expect(useLaunchSheetStore.getState().request).toMatchObject({
      teamKey: "acme:ops",
      managerHandle: "elsewhere",
    });
  });

  it("the Manager menu offers the agents of the row's own team", async () => {
    twoTeams();
    listResult = [...listResult, row({ handle: "ops-peer", sessionId: "sid-peer" })];
    terminalTabIndex = {
      ...terminalTabIndex,
      "acme:ops": {
        "ws-x": [
          { id: "tab-x", sessionId: "sid-elsewhere" },
          { id: "tab-p", sessionId: "sid-peer" },
        ],
      },
    };
    renderList();
    fireEvent.contextMenu(await screen.findByRole("button", { name: agentRow(/elsewhere/) }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Manager" }));
    await screen.findByRole("menuitemradio", { name: /ops-peer/ });
    expect(screen.queryByRole("menuitemradio", { name: /^lead\b/ })).toBeNull();
  });

  it("showing a row whose tab is hidden in a hidden team shows that team again", async () => {
    workspaces = {
      "team-1": [
        {
          id: "ws-s",
          kind: "story",
          status: "ready",
          storyId: "SW-1",
          storyTitle: "Story",
          storyStatus: "in-progress",
          storyFilePath: "/ctx/stories/SW-1.md",
        },
      ],
      "acme:ops": [{ id: "ws-x" }],
    };
    hiddenTabIndex = { "acme:ops": { "ws-x": [{ id: "tab-h", sessionId: "sid-h" }] } };
    hiddenPaneTeams = ["acme:ops"];
    listResult = [row({ handle: "SW-1#lead", sessionId: "sid-h" })];
    projectedOverrides = { "sid-h": { teamKey: "team-1", workspaceRef: "ws-s", storyRef: "ws-s" } };
    renderList();
    await waitFor(() => expect(rowOf("SW-1#lead")).not.toBeNull());
    fireEvent.click(rowOf("SW-1#lead"));
    expect(setAgentsPaneTeamHidden).toHaveBeenCalledWith("acme:ops", false);
  });

  it("a team with no agents yet keeps the last team with agents uncheckable no more", async () => {
    twoTeams();
    openedTeamsMock = ["team-1", "acme:ops", "acme:empty"];
    hiddenPaneTeams = ["acme:ops"];
    renderList();
    await screen.findByRole("button", { name: agentRow(/worker/) });
    const menu = filterMenu();
    const boxOf = (name: string) =>
      within(menu)
        .getAllByRole("checkbox")
        .find((b) => b.closest("label")?.textContent?.includes(name)) as HTMLInputElement;
    // acme:empty is still shown, so team-1 is not the last team shown.
    expect(boxOf("team-1")).toBeChecked();
    expect(boxOf("team-1")).not.toBeDisabled();
    fireEvent.click(boxOf("team-1"));
    expect(setAgentsPaneTeamHidden).toHaveBeenCalledWith("team-1", true);
  });

  it("an arrow on the no-workspace card names it on its chip", async () => {
    listResult = [row({ handle: "loose", sessionId: null })];
    const { NO_WORKSPACE_ID } = await import("@/features/agents/lib/agent-list-layout");
    paneWorkspaceFilter = { teamKey: "team-1", workspaceId: NO_WORKSPACE_ID };
    renderList();
    await screen.findByRole("button", { name: agentRow(/loose/) });
    expect(document.querySelector('[data-chip="ws"]')).toHaveTextContent("No workspace");
    expect(setAgentsWorkspaceFilter).not.toHaveBeenCalledWith(null);
  });

  it("a story card whose status is blank draws no stage rail; one with a status does", async () => {
    workspaces = {
      "team-1": [
        { id: "ws-blank", kind: "story", storyId: "SW-8", storyStatus: "  " },
        { id: "ws-set", kind: "story", storyId: "SW-9", storyStatus: "in-progress" },
      ],
    };
    terminalTabIndex = {
      "team-1": {
        "ws-blank": [{ id: "t-b", sessionId: "sid-b" }],
        "ws-set": [{ id: "t-s", sessionId: "sid-s" }],
      },
    };
    listResult = [
      row({ handle: "b", sessionId: "sid-b" }),
      row({ handle: "s", sessionId: "sid-s" }),
    ];
    renderList();
    await waitFor(() => expect(rowOf("s")).not.toBeNull());
    const card = (id: string) =>
      document.querySelector<HTMLElement>(`[data-workspace-id="${id}"]`)!;
    expect(card("ws-blank").querySelector(".ag-ws-stage")).toBeNull();
    expect(card("ws-set").querySelector(".ag-ws-stage")).not.toBeNull();
  });

  it("a crew member's activity qualifier reaches its card's crew", async () => {
    twoTeams();
    useAgentStateStore.getState().recordActivity(
      {
        session: "sid-worker",
        identity: { hostBootId: "boot", sessionIncarnation: "pty", invocationId: "run" },
        revision: 1,
        turnGeneration: 1,
        provider: "claude",
        state: "working",
        observed: true,
        limited: false,
        exit: null,
        asks: [],
        toolOpen: true,
        activityAgeMs: 0,
      },
      Infinity,
    );
    renderList();
    await screen.findByRole("button", { name: agentRow(/worker/) });
    const crew = () => screen.getByTestId("agents-workspace-ws-a-crew");
    expect(crew().getAttribute("title")).not.toContain("Restoring agent activity");
    act(() => useAgentStateStore.getState().activityUnavailable(["sid-worker"], "recovering"));
    await waitFor(() => expect(crew().getAttribute("title")).toContain("Restoring agent activity"));
  });

  it("one agent's status change does not re-render another agent's row", async () => {
    workspaces = { "team-1": [{ id: "ws-a" }] };
    terminalTabIndex = {
      "team-1": {
        "ws-a": [
          { id: "t-a", sessionId: "sid-a" },
          { id: "t-b", sessionId: "sid-b" },
        ],
      },
    };
    listResult = [
      row({ handle: "a", sessionId: "sid-a" }),
      row({ handle: "b", sessionId: "sid-b" }),
    ];
    renderList();
    await waitFor(() => expect(rowOf("b")).not.toBeNull());
    await act(async () => {});
    const before = { a: rowRenders.get("sid-a") ?? 0, b: rowRenders.get("sid-b") ?? 0 };
    act(() => hooked("sid-b"));
    await waitFor(() => expect(rowOf("b").closest(".ag-row")).toHaveTextContent("Thinking"));
    expect(rowRenders.get("sid-b")).toBeGreaterThan(before.b);
    expect(rowRenders.get("sid-a")).toBe(before.a);
  });

  describe("virtualized focus and motion", () => {
    function manyAgents(n: number) {
      const handles = Array.from({ length: n }, (_, i) => `a-${String(i).padStart(2, "0")}`);
      workspaces = { "team-1": [{ id: "ws-a" }] };
      terminalTabIndex = {
        "team-1": { "ws-a": handles.map((h) => ({ id: `t-${h}`, sessionId: `sid-${h}` })) },
      };
      listResult = handles.map((h, i) =>
        row({ handle: h, sessionId: `sid-${h}`, startedAtMs: 1_000 + i }),
      );
      return handles;
    }

    it("an arrow to an undrawn row lands on that agent once it draws, though the roster changed meanwhile", async () => {
      scrollAreaPx = 200;
      manyAgents(30);
      renderList();
      await waitFor(() =>
        expect(document.querySelectorAll("[data-row-kind='agent']").length).toBeGreaterThan(0),
      );
      const drawn = [...document.querySelectorAll<HTMLElement>("[data-row-kind='agent']")];
      const lastDrawn = drawn.at(-1)!;
      const lastHandle =
        lastDrawn.querySelector<HTMLElement>("[data-agent-handle]")!.dataset.agentHandle!;
      const lastIndex = Number(lastDrawn.dataset.rowIndex);
      const next = document.querySelector<HTMLElement>(`[data-row-index="${lastIndex + 1}"]`);
      expect(next).toBeNull();
      const main = rowOf(lastHandle);
      main.focus();
      fireEvent.keyDown(main, { key: "ArrowDown" });
      // The agent below is not drawn: focus stays until it is.
      expect(document.activeElement).toBe(main);
      // The agents are drawn in a fixed order: the next one follows the drawn ones' step.
      const nums = drawn.map((el) =>
        Number(el.querySelector<HTMLElement>("[data-agent-handle]")!.dataset.agentHandle!.slice(2)),
      );
      const step = nums[1]! - nums[0]!;
      const wantHandle = `a-${String(nums.at(-1)! + step).padStart(2, "0")}`;
      // Before it draws, a new agent joins at the top, so every index below moves by one.
      act(() =>
        publishProjectionOwners({
          sessions: [row({ handle: "a-new", sessionId: "sid-new", startedAtMs: 1 }), ...listResult],
        }),
      );
      // The list scrolls far enough for it to draw.
      const scroller = document.querySelector<HTMLElement>(".ag-scroll")!;
      act(() => {
        scroller.scrollTop = 400;
        scroller.dispatchEvent(new Event("scroll"));
      });
      await waitFor(() => expect(document.activeElement).toBe(rowOf(wantHandle)));
    });

    it("a moved card's rows glide once; a later filter animates and scrolls nothing", async () => {
      const animate = vi.fn(() => ({}) as Animation);
      const scrollTo = vi.fn();
      const ownAnimate = Object.getOwnPropertyDescriptor(Element.prototype, "animate");
      const ownScrollTo = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollTo");
      Object.defineProperty(Element.prototype, "animate", {
        configurable: true,
        writable: true,
        value: animate,
      });
      Object.defineProperty(HTMLElement.prototype, "scrollTo", {
        configurable: true,
        writable: true,
        value: scrollTo,
      });
      try {
        workspaces = {
          "team-1": ["ws-a", "ws-b", "ws-c"].map((id) => ({
            id,
            kind: "generic",
            status: "ready",
            cwd: `/w/${id}`,
          })),
        };
        terminalTabIndex = {
          "team-1": Object.fromEntries(
            ["a", "b", "c"].map((k) => [`ws-${k}`, [{ id: `tab-${k}`, sessionId: `sid-${k}` }]]),
          ),
        };
        listResult = ["a", "b", "c"].map((k) =>
          row({ handle: `agent-${k}`, sessionId: `sid-${k}` }),
        );
        renderList();
        await waitFor(() => expect(rowOf("agent-c")).not.toBeNull());
        await act(async () => {});
        scrollTo.mockClear();
        const name = document.querySelector<HTMLElement>(
          '[data-group-key="team-1|ws-c"] .ag-ws-name',
        )!;
        name.focus();
        fireEvent.keyDown(name, { key: "ArrowUp", altKey: true });
        await act(async () => {});
        expect(animate).toHaveBeenCalled();
        // Every row is on screen, so a keyboard move scrolls nothing.
        expect(scrollTo).not.toHaveBeenCalled();
        const afterMove = animate.mock.calls.length;
        act(() => storeState().setAgentsPaneSearch({ query: "agent-a" }));
        await waitFor(() => expect(rowOf("agent-b")).toBeNull());
        expect(animate.mock.calls.length).toBe(afterMove);
        expect(scrollTo).not.toHaveBeenCalled();
      } finally {
        delete (Element.prototype as { animate?: unknown }).animate;
        delete (HTMLElement.prototype as { scrollTo?: unknown }).scrollTo;
        if (ownAnimate) Object.defineProperty(Element.prototype, "animate", ownAnimate);
        if (ownScrollTo) Object.defineProperty(HTMLElement.prototype, "scrollTo", ownScrollTo);
      }
    });

    it("the drag preview of a card whose agent's machine is offline wears the offline mark", async () => {
      useMachinesStore.setState({
        byTeam: {
          "team-1": [
            {
              machineId: "box",
              label: "Box",
              kind: "ssh",
              status: { state: "disconnected" },
            } as never,
          ],
        },
      });
      workspaces = {
        "team-1": ["ws-a", "ws-b"].map((id) => ({
          id,
          kind: "generic",
          status: "ready",
          cwd: `/w/${id}`,
        })),
      };
      terminalTabIndex = {
        "team-1": {
          "ws-a": [{ id: "tab-a", sessionId: "sid-a" }],
          "ws-b": [
            {
              id: "tab-b",
              sessionId: "sid-b",
              origin: { machineId: "box", member: "remote:box", handle: "h-b" },
            } as Tab,
          ],
        },
      };
      listResult = [
        row({ handle: "agent-a", sessionId: "sid-a" }),
        row({ handle: "agent-b", sessionId: "sid-b" }),
      ];
      renderList();
      await waitFor(() => expect(rowOf("agent-b")).not.toBeNull());
      const head = document.querySelector<HTMLElement>(
        '[data-group-key="team-1|ws-b"] .ag-ws-head-row',
      )!;
      const y = Number(head.closest<HTMLElement>("[data-row-index]")!.dataset.rowIndex) * 40;
      const P = { pointerId: 1, button: 0, isPrimary: true, clientX: 30 };
      fireEvent.pointerDown(head, { ...P, clientY: y + 20 });
      fireEvent.pointerMove(window, { ...P, clientY: y + 2 });
      fireEvent.pointerMove(window, { ...P, clientY: 4 });
      const preview = screen.getByTestId("agents-drag-preview");
      expect(preview.querySelector("[data-mark]")).toHaveAttribute("data-mark", "offline");
      fireEvent.keyDown(window, { key: "Escape" });
      fireEvent.pointerUp(window, { ...P, clientY: 4 });
    });
  });
});

describe("the Agents pane, round three", () => {
  /** One handle running in two teams: team-1's in alpha-dir, acme's in xray-dir. */
  function twins() {
    workspaces = {
      "team-1": [{ id: "ws-a", kind: "generic", status: "ready", cwd: "/alpha-dir" }],
      "acme:ops": [{ id: "ws-x", kind: "generic", status: "ready", cwd: "/xray-dir" }],
    };
    terminalTabIndex = {
      "team-1": { "ws-a": [{ id: "t1", sessionId: "sid-twin-1" }] },
      "acme:ops": { "ws-x": [{ id: "t2", sessionId: "sid-twin-2" }] },
    };
    listResult = [
      row({ handle: "twin", sessionId: "sid-twin-1" }),
      row({ handle: "twin", sessionId: "sid-twin-2" }),
    ];
  }
  const twinRow = (teamKey: string) => rowByKey(`agent:${agentRowKey(teamKey, "twin")}`);
  const count = () => screen.getByTestId("agents-count").textContent;

  it("a search match in one team keeps only that team's row of a twin handle, and the counts agree", async () => {
    twins();
    renderList();
    await waitFor(() => expect(twinRow("acme:ops")).not.toBeNull());
    expect(twinRow("team-1")).not.toBeNull();
    act(() => storeState().setAgentsPaneSearch({ query: "xray" }));
    await waitFor(() => expect(twinRow("team-1")).toBeNull());
    expect(twinRow("acme:ops")).not.toBeNull();
    expect(count()).toBe("1/2");
  });

  it("a Show filter match in one team keeps only that team's row of a twin handle, and the menu counts it", async () => {
    twins();
    awaitingAnswer("sid-twin-2");
    renderList();
    await waitFor(() => expect(twinRow("acme:ops")).not.toBeNull());
    expect(showCount("Needs you")).toBe(1);
    act(() => storeState().setAgentsPaneSearch({ status: "needs" }));
    await waitFor(() => expect(twinRow("team-1")).toBeNull());
    expect(twinRow("acme:ops")).not.toBeNull();
    expect(count()).toBe("1/2");
    expect(showCount("Needs you")).toBe(1);
  });

  it('team "a:b" with handle "c" and team "a" with handle "b:c" are two rows, each pinned and focused on its own', async () => {
    workspaces = {
      "a:b": [{ id: "ws-1", kind: "generic", status: "ready", cwd: "/one-dir" }],
      a: [{ id: "ws-2", kind: "generic", status: "ready", cwd: "/two-dir" }],
    };
    terminalTabIndex = {
      "a:b": { "ws-1": [{ id: "t1", sessionId: "sid-c" }] },
      a: { "ws-2": [{ id: "t2", sessionId: "sid-bc" }] },
    };
    listResult = [
      row({ handle: "c", sessionId: "sid-c" }),
      row({ handle: "b:c", sessionId: "sid-bc" }),
    ];
    renderList("a");
    const left = () => rowByKey(`agent:${agentRowKey("a:b", "c")}`);
    const right = () => rowByKey(`agent:${agentRowKey("a", "b:c")}`);
    await waitFor(() => expect(left()).not.toBeNull());
    expect(right()).not.toBeNull();
    expect(left()).not.toBe(right());

    fireEvent.click(within(right()!).getByTestId("agent-row-pin"));
    expect(pinAgent).toHaveBeenLastCalledWith(
      "a",
      expect.objectContaining({ teamKey: "a", handle: "b:c", sessionId: "sid-bc" }),
    );
    fireEvent.click(within(left()!).getByTestId("agent-row-pin"));
    expect(pinAgent).toHaveBeenLastCalledWith(
      "a:b",
      expect.objectContaining({ teamKey: "a:b", handle: "c", sessionId: "sid-c" }),
    );

    // The arrow keys walk from one row to the other: each is its own stop.
    const first = [left()!, right()!].sort(
      (x, y) => Number(x.dataset.rowIndex) - Number(y.dataset.rowIndex),
    );
    const from = first[0]!.querySelector<HTMLElement>(".ag-main")!;
    from.focus();
    fireEvent.keyDown(from, { key: "ArrowDown" });
    await waitFor(() => expect(first[1]!.contains(document.activeElement)).toBe(true));
  });

  it("pinning one team's twin pins only that team's session", async () => {
    twins();
    const { rerender } = renderList();
    await waitFor(() => expect(twinRow("acme:ops")).not.toBeNull());
    fireEvent.click(within(twinRow("acme:ops")!).getByTestId("agent-row-pin"));
    expect(pinAgent).toHaveBeenCalledWith(
      "acme:ops",
      expect.objectContaining({ teamKey: "acme:ops", handle: "twin", sessionId: "sid-twin-2" }),
    );
    rerender(<DockSlotHost teamKey="team-1" visible />);
    const pinned = await screen.findByTestId("agents-pinned");
    expect(pinned.querySelectorAll(".ag-row")).toHaveLength(1);
    expect(twinRow("acme:ops")!.querySelector(".ag-row")).toHaveAttribute("data-pinned", "dim");
    expect(twinRow("team-1")!.querySelector(".ag-row")).not.toHaveAttribute("data-pinned");
  });

  it("a card on an unreachable machine reads offline, by its own team's machine list", async () => {
    useMachinesStore.setState({
      byTeam: {
        // Another team lists the same machine id as connected: the card's own team decides.
        "acme:ops": [
          { machineId: "box", label: "Studio", kind: "ssh", status: { state: "connected" } },
        ],
        "team-1": [
          {
            machineId: "box",
            label: "Studio",
            kind: "ssh",
            status: { state: "unreachable", lastReachedAtMs: Date.UTC(2026, 0, 1, 14, 5) },
          },
        ],
      } as never,
    });
    workspaces = {
      "team-1": [{ id: "ws-r", status: "ready", location: { kind: "machine", machineId: "box" } }],
    };
    terminalTabIndex = { "team-1": { "ws-r": [{ id: "t-r", sessionId: "sid-r" }] } };
    listResult = [row({ handle: "r", sessionId: "sid-r" })];
    renderList();
    await waitFor(() => expect(rowOf("r")).not.toBeNull());
    const mark = screen.getByTestId("agents-workspace-ws-r-machine");
    expect(mark).toHaveAttribute("data-offline", "true");
    expect(mark.closest("[title]")!.getAttribute("title")).toMatch(/offline since/);
  });

  it("a card on a connected machine does not read offline, though another team lists it unreachable", async () => {
    useMachinesStore.setState({
      byTeam: {
        "acme:ops": [
          {
            machineId: "box",
            label: "Studio",
            kind: "ssh",
            status: { state: "unreachable", lastReachedAtMs: Date.UTC(2026, 0, 1, 14, 5) },
          },
        ],
        "team-1": [
          { machineId: "box", label: "Studio", kind: "ssh", status: { state: "connected" } },
        ],
      } as never,
    });
    workspaces = {
      "team-1": [{ id: "ws-r", status: "ready", location: { kind: "machine", machineId: "box" } }],
    };
    terminalTabIndex = { "team-1": { "ws-r": [{ id: "t-r", sessionId: "sid-r" }] } };
    listResult = [row({ handle: "r", sessionId: "sid-r" })];
    renderList();
    await waitFor(() => expect(rowOf("r")).not.toBeNull());
    const mark = screen.getByTestId("agents-workspace-ws-r-machine");
    expect(mark).not.toHaveAttribute("data-offline", "true");
    expect(mark.closest("[title]")?.getAttribute("title") ?? "").not.toMatch(/offline/);
  });

  it("with the list virtualized, every drawn row says its place among all the rows", async () => {
    scrollAreaPx = 200;
    const handles = Array.from({ length: 30 }, (_, i) => `a-${String(i).padStart(2, "0")}`);
    workspaces = { "team-1": [{ id: "ws-a" }] };
    terminalTabIndex = {
      "team-1": { "ws-a": handles.map((h) => ({ id: `t-${h}`, sessionId: `sid-${h}` })) },
    };
    listResult = handles.map((h, i) =>
      row({ handle: h, sessionId: `sid-${h}`, startedAtMs: 1_000 + i }),
    );
    renderList();
    await waitFor(() =>
      expect(document.querySelectorAll("[data-row-kind='agent']").length).toBeGreaterThan(0),
    );
    const items = [...document.querySelectorAll<HTMLElement>('[role="listitem"]')];
    const sizes = new Set(items.map((el) => el.getAttribute("aria-setsize")));
    expect(sizes.size).toBe(1);
    const setSize = Number([...sizes][0]);
    // Every agent, its card and its team: more rows than are drawn.
    expect(setSize).toBeGreaterThanOrEqual(handles.length + 1);
    expect(setSize).toBeGreaterThan(items.length);
  });

  it("an agent chip whose agent runs only in a hidden team says so, and offers to show it or clear the chip", async () => {
    workspaces = { "team-1": [{ id: "ws-a" }], "acme:ops": [{ id: "ws-x" }] };
    terminalTabIndex = {
      "team-1": { "ws-a": [{ id: "t-o", sessionId: "sid-other" }] },
      "acme:ops": { "ws-x": [{ id: "t-t", sessionId: "sid-triage" }] },
    };
    listResult = [
      row({ handle: "other", sessionId: "sid-other" }),
      row({ handle: "triage", sessionId: "sid-triage", agentRevision: "acme/backend/triage@3" }),
    ];
    hiddenPaneTeams = ["acme:ops"];
    agentsListFilter = { agentKey: "acme/backend/triage", name: "Triage" };
    renderList();
    const note = await screen.findByTestId("agents-chip-hidden");
    expect(note).toHaveTextContent("Triage runs in ops, a hidden team");
    expect(screen.queryByText("No Triage sessions running")).toBeNull();
    fireEvent.click(within(note).getByRole("button", { name: "Show ops" }));
    expect(setAgentsPaneTeamHidden).toHaveBeenCalledWith("acme:ops", false);
    fireEvent.click(within(note).getByRole("button", { name: "Clear agent filter" }));
    expect(clearAgentsListFilter).toHaveBeenCalled();
  });

  it("the Manager menu reads another team's tree with that team's manager choices, and its roles match the rows", async () => {
    workspaces = { "team-1": [{ id: "ws-a" }], "acme:ops": [{ id: "ws-x" }] };
    terminalTabIndex = {
      "team-1": { "ws-a": [{ id: "t-l", sessionId: "sid-lead" }] },
      "acme:ops": {
        "ws-x": [
          { id: "t-e", sessionId: "sid-elsewhere" },
          { id: "t-p", sessionId: "sid-peer" },
        ],
      },
    };
    listResult = [
      row({ handle: "lead", sessionId: "sid-lead" }),
      row({ handle: "elsewhere", sessionId: "sid-elsewhere" }),
      row({ handle: "ops-peer", sessionId: "sid-peer" }),
    ];
    // acme's own choice shows ops-peer under elsewhere; team-1 chose nothing.
    agentIdentity = { "acme:ops": { "ops-peer": { manager: "elsewhere" } } };
    renderList();
    const elsewhere = await screen.findByRole("button", { name: agentRow(/elsewhere/) });
    expect(elsewhere.querySelector(".ag-piece")).toHaveAttribute(
      "title",
      "Hierarchy: Manager · launched 1",
    );
    // elsewhere cannot go under ops-peer: that would make a loop.
    fireEvent.contextMenu(elsewhere);
    fireEvent.click(await screen.findByRole("menuitem", { name: "Manager" }));
    await screen.findByRole("menuitemradio", { name: "Top level" });
    expect(screen.queryByRole("menuitemradio", { name: /ops-peer/ })).toBeNull();
    fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
    fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
    // ops-peer's menu offers elsewhere wearing the role its row wears.
    await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
    fireEvent.contextMenu(rowOf("ops-peer"));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Manager" }));
    const option = await screen.findByRole("menuitemradio", { name: /elsewhere/ });
    expect(option.querySelector(".ag-piece")).toHaveAttribute(
      "title",
      "Hierarchy: Manager · launched 1",
    );
  });

  it("an ask that only mentions the status word mid-sentence leaves the word on line one", async () => {
    workspaces = { "team-1": [{ id: "ws-a" }] };
    terminalTabIndex = { "team-1": { "ws-a": [{ id: "t-a", sessionId: "sid-asker" }] } };
    listResult = [row({ handle: "asker", sessionId: "sid-asker" })];
    useManagedSessionStore.setState({
      states: { "sid-asker": "awaiting-answer" },
      pendingAsks: { q1: { sessionId: "sid-asker", question: "Which ticket needs answer first?" } },
    } as never);
    projectSessionStatus("sid-asker", "awaiting-answer");
    renderList();
    await waitFor(() => expect(rowOf("asker")).not.toBeNull());
    const row1 = rowOf("asker").closest(".ag-row")!;
    expect(row1.querySelector(".ag-task")).toHaveTextContent("Needs answer");
    expect(row1.querySelector(".ag-ask")).toHaveTextContent("Which ticket needs answer first?");
  });

  it("a role re-read that says the same leaves the holder's row as drawn", async () => {
    roleConfigs["acme:backend"] = [
      { id: "triage", name: "Triage", icon: "builtin-chess:knight", defaultHandler: null },
    ];
    workspaces = { "acme:backend": [{ id: "ws-a" }] };
    terminalTabIndex = {
      "acme:backend": {
        "ws-a": [
          { id: "t-h", sessionId: "sid-holder" },
          { id: "t-o", sessionId: "sid-other" },
        ],
      },
    };
    listResult = [
      row({ handle: "holder", sessionId: "sid-holder" }),
      row({ handle: "other", sessionId: "sid-other" }),
    ];
    rolesResult = {
      roles: [
        {
          role: "triage",
          roleName: "Triage",
          registrations: [
            {
              instanceId: "sid-holder",
              instanceName: null,
              generation: 4,
              registeredAtMs: 1,
              lastRenewedAtMs: 1,
              expiresAtMs: 9e15,
              live: true,
            },
          ],
          counts: {},
          deliveries: [],
          deliveriesTruncated: false,
        },
      ],
    };
    renderList("acme:backend");
    const holds = await screen.findByTestId("agent-holds");
    await within(holds).findByRole("button", { name: "Queue role: Triage (role:triage)" });
    await act(async () => {});
    const before = rowRenders.get("sid-holder") ?? 0;
    const reads = agentReadRoles.mock.calls.length;
    const { AGENT_STATE_CHANGED_EVENT } = await import("@/tauri/events");
    await act(async () => emitTauriEvent(AGENT_STATE_CHANGED_EVENT));
    await waitFor(() => expect(agentReadRoles.mock.calls.length).toBeGreaterThan(reads));
    await act(async () => {});
    const chip = within(screen.getByTestId("agent-holds")).getByRole("button", {
      name: "Queue role: Triage (role:triage)",
    });
    expect(chip).toHaveTextContent(/^Triage$/);
    expect(chip.querySelector("img")).toHaveAttribute(
      "src",
      BUILTIN_FACE_SOURCE.resolve("builtin-chess:knight"),
    );
    expect(rowRenders.get("sid-holder")).toBe(before);
  });

  it("the quick view reads its agent's live row: a session that moved to an offline machine has no live terminal", async () => {
    workspaces = { "team-1": [{ id: "ws-a" }] };
    terminalTabIndex = { "team-1": { "ws-a": [{ id: "t-1", sessionId: "sid-1" }] } };
    listResult = [row({ handle: "mover", sessionId: "sid-1" })];
    const { rerender } = renderList();
    await waitFor(() => expect(rowOf("mover")).not.toBeNull());
    fireEvent.keyDown(rowOf("mover"), { key: "q" });
    await act(async () => {});
    expect(screen.getByRole("dialog", { name: "Quick view of mover" })).toBeInTheDocument();
    // Its session is now one on a machine that cannot be reached.
    useMachinesStore.setState({
      byTeam: {
        "team-1": [
          { machineId: "box", label: "Studio", kind: "ssh", status: { state: "disconnected" } },
        ],
      } as never,
    });
    terminalTabIndex = {
      "team-1": {
        "ws-a": [
          {
            id: "remote-9",
            sessionId: "remote-9",
            origin: { machineId: "box", member: "remote:box", handle: "h-9" },
          } as Tab,
        ],
      },
    };
    listResult = [row({ handle: "mover", sessionId: "remote-9" })];
    act(() => publishProjectionOwners({ sessions: listResult }));
    rerender(<DockSlotHost teamKey="team-1" visible />);
    await waitFor(() =>
      expect(screen.getByRole("dialog", { name: "Quick view of mover" })).toHaveTextContent(
        "Studio is offline, so mover's terminal can't be shown until it reconnects.",
      ),
    );
  });

  it("with the only team with agents hidden but an empty team still shown, it says its agents run in a hidden team and offers to show it", async () => {
    workspaces = { "team-1": [{ id: "ws-a" }] };
    terminalTabIndex = { "team-1": { "ws-a": [{ id: "t-a", sessionId: "sid-a" }] } };
    listResult = [row({ handle: "a", sessionId: "sid-a" })];
    openedTeamsMock = ["team-1", "acme:empty"];
    hiddenPaneTeams = ["team-1"];
    const { rerender } = renderList();
    const note = await screen.findByTestId("agents-hidden-teams");
    expect(note).toHaveTextContent("Agents run in hidden teams");
    expect(screen.queryByText("No agents running")).toBeNull();
    expect(screen.queryByTestId("agents-all-hidden")).toBeNull();
    fireEvent.click(within(note).getByRole("button", { name: "Show team-1" }));
    expect(setAgentsPaneTeamHidden).toHaveBeenCalledWith("team-1", false);
    // Every team the checklist lists unchecked: then it says so.
    hiddenPaneTeams = ["team-1", "acme:empty"];
    rerender(<DockSlotHost teamKey="team-1" visible />);
    expect(await screen.findByTestId("agents-all-hidden")).toBeInTheDocument();
  });

  it("names hidden teams that share a name across orgs by org and team", async () => {
    workspaces = { "acme:ops": [{ id: "ws-a" }], "zeta:ops": [{ id: "ws-z" }] };
    terminalTabIndex = {
      "acme:ops": { "ws-a": [{ id: "t-a", sessionId: "sid-a" }] },
      "zeta:ops": { "ws-z": [{ id: "t-z", sessionId: "sid-z" }] },
    };
    listResult = [
      row({ handle: "a", sessionId: "sid-a" }),
      row({ handle: "z", sessionId: "sid-z" }),
    ];
    openedTeamsMock = ["team-1", "acme:ops", "zeta:ops"];
    hiddenPaneTeams = ["acme:ops", "zeta:ops"];
    renderList();
    const note = await screen.findByTestId("agents-hidden-teams");
    expect(within(note).getByRole("button", { name: "Show acme › ops" })).toBeInTheDocument();
    expect(within(note).getByRole("button", { name: "Show zeta › ops" })).toBeInTheDocument();
  });

  it("with every team hidden, an agent chip whose agent runs in one says so and offers to clear the chip", async () => {
    workspaces = { "acme:ops": [{ id: "ws-x" }] };
    terminalTabIndex = { "acme:ops": { "ws-x": [{ id: "t-t", sessionId: "sid-triage" }] } };
    listResult = [
      row({ handle: "triage", sessionId: "sid-triage", agentRevision: "acme/backend/triage@3" }),
    ];
    openedTeamsMock = ["acme:ops"];
    hiddenPaneTeams = ["acme:ops"];
    agentsListFilter = { agentKey: "acme/backend/triage", name: "Triage" };
    renderList();
    const note = await screen.findByTestId("agents-chip-hidden");
    expect(note).toHaveTextContent("Triage runs in ops, a hidden team");
    expect(screen.queryByTestId("agents-all-hidden")).toBeNull();
    fireEvent.click(within(note).getByRole("button", { name: "Clear agent filter" }));
    expect(clearAgentsListFilter).toHaveBeenCalled();
  });

  describe("a pin is never read as ended on a partial picture", () => {
    const pinOf = (teamKey: string, handle: string, sessionId: string): Pin => ({
      kind: "session",
      teamKey,
      handle,
      sessionId,
      name: handle,
    });

    it("waits for the roster and the tabs: trees that start empty unpin nothing", async () => {
      agentPins = { "team-1": [pinOf("team-1", "w", "sid-w")] };
      layoutHydrated = false;
      persistedStateRestored = false;
      // The roster has not loaded: no trees at all.
      const { rerender } = render(<DockSlotHost teamKey="team-1" visible />);
      await act(async () => {});
      // The roster loads before the tabs: still nothing to read the pin against.
      act(() => publishProjectionOwners({ sessions: [], error: null }));
      rerender(<DockSlotHost teamKey="team-1" visible />);
      await act(async () => {});
      layoutHydrated = true;
      rerender(<DockSlotHost teamKey="team-1" visible />);
      await act(async () => {});
      expect(unpinAgents).not.toHaveBeenCalled();
      // Everything is back and the session runs nowhere: now it has ended.
      persistedStateRestored = true;
      rerender(<DockSlotHost teamKey="team-1" visible />);
      await waitFor(() =>
        expect(unpinAgents).toHaveBeenCalledWith("team-1", [sessionPinId("team-1", "w")]),
      );
    });

    it("keeps a pin whose handle a live roster row runs in another team", async () => {
      // The pin is acme's, of an older session id; the roster runs "x" live, placed in team-1.
      listResult = [row({ handle: "x", sessionId: "sid-new" })];
      agentPins = { "acme:ops": [pinOf("acme:ops", "x", "sid-old")] };
      renderList();
      await waitFor(() => expect(rowOf("x")).not.toBeNull());
      await act(async () => {});
      expect(unpinAgents).not.toHaveBeenCalled();
      agentPins = {};
    });

    it("waits for a remote Machine's layout before reading its pinned session as ended", async () => {
      // The Machine is admitted, but its layout (and so the pinned session's tab) is not read yet.
      listResult = [];
      agentPins = { "team-1": [pinOf("team-1", "far", "sid-far")] };
      layoutAdmission = {
        "box/remote": {
          member: { machineId: "box", slot: "remote" },
          revision: "1",
          state: "live",
          namespace: {},
          historical: null,
        },
      };
      layoutAdmittedAt = { "box/remote": Date.now() };
      const { rerender } = renderList();
      await act(async () => {});
      expect(unpinAgents).not.toHaveBeenCalled();
      // Another Machine is still being connected: its layout is to come too.
      useMachinesStore.setState({
        byTeam: {
          "team-1": [
            { machineId: "lab", label: "Lab", kind: "ssh", status: { state: "connecting" } },
          ],
        } as never,
      });
      layoutMembers = { "box/remote": { workspaces: {} } };
      rerender(<DockSlotHost teamKey="team-1" visible />);
      await act(async () => {});
      expect(unpinAgents).not.toHaveBeenCalled();
      // Both layouts are read and the session runs nowhere: now it has ended.
      act(() =>
        useMachinesStore.setState({
          byTeam: {
            "team-1": [
              { machineId: "lab", label: "Lab", kind: "ssh", status: { state: "connected" } },
            ],
          } as never,
        }),
      );
      layoutMembers = { "box/remote": { workspaces: {} }, "lab/remote": { workspaces: {} } };
      rerender(<DockSlotHost teamKey="team-1" visible />);
      await waitFor(() =>
        expect(unpinAgents).toHaveBeenCalledWith("team-1", [sessionPinId("team-1", "far")]),
      );
      agentPins = {};
    });

    it("keeps a tabless, story-less pinned session across a team switch", async () => {
      // No tab and no story: its row follows the active team.
      listResult = [row({ handle: "solo", sessionId: "sid-solo" })];
      agentPins = { "team-1": [pinOf("team-1", "solo", "sid-solo")] };
      const { rerender } = renderList();
      await screen.findByTestId("agents-pinned");
      rerender(<DockSlotHost teamKey="acme:ops" visible />);
      await waitFor(() => expect(rowOf("solo")).not.toBeNull());
      await act(async () => {});
      expect(unpinAgents).not.toHaveBeenCalled();
      agentPins = {};
    });

    it("keeps a pinned session whose tab the tab index has not caught up with", async () => {
      // Its tab sits in acme:ops, so the pin is acme's; the index lags and lists it nowhere yet.
      workspaces = { "acme:ops": [{ id: "ws-x" }] };
      listResult = [row({ handle: "far", sessionId: "sid-far" })];
      agentPins = { "acme:ops": [pinOf("acme:ops", "far", "sid-far")] };
      const { rerender } = renderList();
      await waitFor(() => expect(rowOf("far")).not.toBeNull());
      await act(async () => {});
      expect(unpinAgents).not.toHaveBeenCalled();
      // The index catches up: the row moves to its team, and its pin shows.
      terminalTabIndex = { "acme:ops": { "ws-x": [{ id: "t-far", sessionId: "sid-far" }] } };
      rerender(<DockSlotHost teamKey="team-1" visible />);
      const pinned = await screen.findByTestId("agents-pinned");
      expect(within(pinned).getByRole("button", { name: agentRow(/far/) })).toBeInTheDocument();
      expect(unpinAgents).not.toHaveBeenCalled();
      agentPins = {};
    });

    it("shows a pin kept by its session id in Pinned while the trees place its row in another team, and Pin there unpins it", async () => {
      // The pin is acme's; the index has not caught up, so the row follows the active team.
      workspaces = { "acme:ops": [{ id: "ws-x" }] };
      listResult = [row({ handle: "far", sessionId: "sid-far" })];
      agentPins = { "acme:ops": [pinOf("acme:ops", "far", "sid-far")] };
      renderList();
      const treeRow = () => rowByKey(`agent:${agentRowKey("team-1", "far")}`);
      await waitFor(() => expect(treeRow()).not.toBeNull());
      const pinned = await screen.findByTestId("agents-pinned");
      expect(within(pinned).getAllByRole("button", { name: agentRow(/far/) })).toHaveLength(1);
      expect(treeRow()!.querySelector(".ag-row")).toHaveAttribute("data-pinned");
      fireEvent.click(within(treeRow()!).getByTestId("agent-row-pin"));
      expect(pinAgent).not.toHaveBeenCalled();
      expect(unpinAgents).toHaveBeenCalledWith("acme:ops", [sessionPinId("acme:ops", "far")]);
      agentPins = {};
    });
  });

  it("a pinned twin of acme is left out of the Pinned group when a search keeps only team-1's twin", async () => {
    workspaces = {
      "team-1": [{ id: "ws-a", kind: "generic", status: "ready", cwd: "/alpha-dir" }],
      "acme:ops": [{ id: "ws-x", kind: "generic", status: "ready", cwd: "/xray-dir" }],
    };
    terminalTabIndex = {
      "team-1": { "ws-a": [{ id: "t1", sessionId: "sid-twin-1" }] },
      "acme:ops": { "ws-x": [{ id: "t2", sessionId: "sid-twin-2" }] },
    };
    listResult = [
      row({ handle: "twin", sessionId: "sid-twin-1" }),
      row({ handle: "twin", sessionId: "sid-twin-2" }),
    ];
    agentPins = {
      "acme:ops": [
        { kind: "session", teamKey: "acme:ops", handle: "twin", sessionId: "sid-twin-2", name: "" },
      ],
    };
    renderList();
    await screen.findByTestId("agents-pinned");
    act(() => storeState().setAgentsPaneSearch({ query: "alpha" }));
    await waitFor(() => expect(screen.queryByTestId("agents-pinned")).toBeNull());
    expect(rowByKey(`agent:${agentRowKey("team-1", "twin")}`)).not.toBeNull();
    agentPins = {};
  });

  it("a row on a machine listed connected by another team reads offline by its own team's list, as its card does", async () => {
    useMachinesStore.setState({
      byTeam: {
        "acme:ops": [
          { machineId: "box", label: "Studio", kind: "ssh", status: { state: "connected" } },
        ],
        "team-1": [
          {
            machineId: "box",
            label: "Studio",
            kind: "ssh",
            status: { state: "unreachable", lastReachedAtMs: Date.UTC(2026, 0, 1, 14, 5) },
          },
        ],
      } as never,
    });
    workspaces = {
      "team-1": [{ id: "ws-r", status: "ready", location: { kind: "machine", machineId: "box" } }],
    };
    terminalTabIndex = {
      "team-1": {
        "ws-r": [
          {
            id: "t-r",
            sessionId: "sid-r",
            origin: { machineId: "box", member: "remote:box", handle: "h-r" },
          } as Tab,
        ],
      },
    };
    listResult = [row({ handle: "r", sessionId: "sid-r" })];
    renderList();
    await waitFor(() => expect(rowOf("r")).not.toBeNull());
    const card = screen.getByTestId("agents-workspace-ws-r-machine");
    expect(card).toHaveAttribute("data-offline", "true");
    const agent = document.querySelector<HTMLElement>('[data-agent-handle="r"]')!;
    expect(agent.textContent).toMatch(/offline/i);
    const rowMark = agent.querySelector<HTMLElement>("[data-offline]");
    expect(rowMark).toHaveAttribute("data-offline", "true");
  });

  it("a row and its card on a machine no list knows never show its raw id", async () => {
    workspaces = {
      "team-1": [
        { id: "ws-r", status: "ready", location: { kind: "machine", machineId: "m-7f3a9" } },
      ],
    };
    terminalTabIndex = {
      "team-1": {
        "ws-r": [
          {
            id: "t-r",
            sessionId: "sid-r",
            origin: { machineId: "m-7f3a9", member: "remote:m-7f3a9", handle: "h-r" },
          } as Tab,
        ],
      },
    };
    listResult = [row({ handle: "r", sessionId: "sid-r" })];
    renderList();
    await waitFor(() => expect(rowOf("r")).not.toBeNull());
    const pane = document.querySelector<HTMLElement>("[role='region'][aria-label='Agents']")!;
    const shown = [
      pane.textContent ?? "",
      ...[...pane.querySelectorAll<HTMLElement>("[title], [aria-label]")].flatMap((el) => [
        el.getAttribute("title") ?? "",
        el.getAttribute("aria-label") ?? "",
      ]),
    ].join(" ");
    expect(shown).not.toContain("m-7f3a9");
    expect(shown).toContain("a remote machine");
    // Its id can still be searched for.
    act(() => storeState().setAgentsPaneSearch({ query: "m-7f3a9" }));
    await waitFor(() => expect(screen.getByTestId("agents-count")).toHaveTextContent("1/1"));
  });

  it("a row whose tab is hidden on a remote machine names that machine, and searching it finds the row", async () => {
    useMachinesStore.setState({
      byTeam: {
        "team-1": [
          { machineId: "box", label: "Studio", kind: "ssh", status: { state: "connected" } },
        ],
      } as never,
    });
    workspaces = { "team-1": [{ id: "ws-a", status: "ready" }] };
    hiddenTabIndex = {
      "team-1": {
        "ws-a": [
          {
            id: "t-h",
            sessionId: "sid-h",
            origin: { machineId: "box", member: "remote:box", handle: "h-h" },
          } as Tab,
        ],
      },
    };
    listResult = [row({ handle: "tucked", sessionId: "sid-h" })];
    renderList();
    await waitFor(() => expect(rowOf("tucked")).not.toBeNull());
    act(() => storeState().setAgentsPaneSearch({ query: "Studio" }));
    await waitFor(() => expect(screen.getByTestId("agents-count")).toHaveTextContent("1/1"));
    act(() => storeState().setAgentsPaneSearch({ query: "This Mac" }));
    await waitFor(() => expect(screen.getByTestId("agents-count")).toHaveTextContent("0/1"));
  });

  it("loading another team's packs keeps the first team's faces, so its rows do not re-render", async () => {
    let resolveOps: (v: unknown) => void = () => {};
    avatarPacksListForTeam.mockImplementation((teamKey: string) =>
      teamKey === "acme:ops"
        ? new Promise((resolve) => {
            resolveOps = resolve as (v: unknown) => void;
          })
        : Promise.resolve({
            packs: [],
            warnings: [],
            default_pack: null,
            agent_avatars: {},
            teamKey,
          }),
    );
    const { useAvatarPacksStore } = await import("@/shared/hooks/use-avatar-packs");
    useAvatarPacksStore.setState({ byTeam: {} });
    workspaces = { "team-1": [{ id: "ws-a" }], "acme:ops": [{ id: "ws-x" }] };
    terminalTabIndex = { "team-1": { "ws-a": [{ id: "t-a", sessionId: "sid-a" }] } };
    listResult = [row({ handle: "a", sessionId: "sid-a" })];
    renderList();
    await waitFor(() => expect(rowOf("a")).not.toBeNull());
    await waitFor(() => expect(useAvatarPacksStore.getState().byTeam["team-1"]).toBeDefined());
    await act(async () => {});
    const before = rowRenders.get("sid-a") ?? 0;
    const teamOneEntry = useAvatarPacksStore.getState().byTeam["team-1"];
    await act(async () =>
      resolveOps({ packs: [], warnings: [], default_pack: null, agent_avatars: {} }),
    );
    await waitFor(() => expect(useAvatarPacksStore.getState().byTeam["acme:ops"]).toBeDefined());
    await act(async () => {});
    expect(useAvatarPacksStore.getState().byTeam["team-1"]).toBe(teamOneEntry);
    expect(rowRenders.get("sid-a")).toBe(before);
    avatarPacksListForTeam.mockImplementation((teamKey: string) =>
      Promise.resolve({ packs: [], warnings: [], default_pack: null, agent_avatars: {}, teamKey }),
    );
  });
});

describe("the Agents pane, review fixes", () => {
  /** One handle running in two teams: team-1's in alpha-dir, acme's in xray-dir, and a working
   *  agent beside acme's twin. */
  function twins() {
    workspaces = {
      "team-1": [{ id: "ws-a", kind: "generic", status: "ready", cwd: "/alpha-dir" }],
      "acme:ops": [{ id: "ws-x", kind: "generic", status: "ready", cwd: "/xray-dir" }],
    };
    terminalTabIndex = {
      "team-1": { "ws-a": [{ id: "t1", sessionId: "sid-twin-1" }] },
      "acme:ops": {
        "ws-x": [
          { id: "t2", sessionId: "sid-twin-2" },
          { id: "t3", sessionId: "sid-busy" },
        ],
      },
    };
    listResult = [
      row({ handle: "twin", sessionId: "sid-twin-1" }),
      row({ handle: "twin", sessionId: "sid-twin-2" }),
      row({ handle: "busy", sessionId: "sid-busy" }),
    ];
  }
  const teamNeed = (teamKey: string) =>
    document.querySelector(`[data-team-key="${teamKey}"] .ag-team-need`);
  const cardCount = (groupKey: string) =>
    document.querySelector(`[data-group-key="${groupKey}"] .ag-ws-count`);

  afterEach(() => useOrgStore.setState({ orgs: [], workspaceRepos: {} }));

  it("needs you is the waiting twin's alone: its card and its folded team say it, the other team's do not", async () => {
    twins();
    hooked("sid-busy");
    awaitingAnswer("sid-twin-2");
    renderList();
    await waitFor(() => expect(cardCount("acme:ops|ws-x")).not.toBeNull());
    expect(cardCount("acme:ops|ws-x")).toHaveAttribute("data-has-needs");
    expect(cardCount("team-1|ws-a")).not.toHaveAttribute("data-has-needs");

    act(() => {
      paneFolds = { "#team-fold:team-1": true, "#team-fold:acme:ops": true };
      notifyPaneStore();
    });
    await waitFor(() => expect(teamNeed("acme:ops")).toHaveTextContent("1"));
    expect(teamNeed("team-1")).toBeNull();

    // A Show filter keeps the folds: the folded team still counts the agent that needs you,
    // though the filter leaves only its working one in view.
    act(() => storeState().setAgentsPaneSearch({ status: "working" }));
    await waitFor(() => expect(screen.getByTestId("agents-count")).toHaveTextContent("1/3"));
    expect(teamNeed("acme:ops")).toHaveTextContent("1");
  });

  it("a pin of one team's twin goes when that session ends, though the other team's twin runs on", async () => {
    twins();
    // team-1's twin has ended; acme's runs on, placed in acme by its tab.
    terminalTabIndex = { "acme:ops": terminalTabIndex["acme:ops"]! };
    listResult = listResult.filter((r) => r.sessionId !== "sid-twin-1");
    agentPins = {
      "team-1": [
        {
          kind: "session",
          teamKey: "team-1",
          handle: "twin",
          sessionId: "sid-twin-1",
          name: "twin",
        },
      ],
    };
    renderList();
    await waitFor(() =>
      expect(unpinAgents).toHaveBeenCalledWith("team-1", [sessionPinId("team-1", "twin")]),
    );
  });

  it("a saved workspace filter waits for the roster, and clears once it has loaded without the workspace's agents", async () => {
    workspaces = { "team-1": [{ id: "ws-a" }, { id: "ws-b" }] };
    terminalTabIndex = { "team-1": { "ws-b": [{ id: "t-b", sessionId: "sid-b" }] } };
    paneWorkspaceFilter = { teamKey: "team-1", workspaceId: "ws-a" };
    // The roster has not loaded yet: the filter stays.
    const view = render(<DockSlotHost teamKey="team-1" visible />);
    await act(async () => {});
    expect(setAgentsWorkspaceFilter).not.toHaveBeenCalled();
    // Loaded, with an agent in ws-a: the filter still has something to show.
    listResult = [
      row({ handle: "a", sessionId: "sid-a" }),
      row({ handle: "b", sessionId: "sid-b" }),
    ];
    terminalTabIndex = {
      "team-1": {
        "ws-a": [{ id: "t-a", sessionId: "sid-a" }],
        "ws-b": [{ id: "t-b", sessionId: "sid-b" }],
      },
    };
    act(() => publishProjectionOwners({ sessions: listResult, error: null }));
    view.rerender(<DockSlotHost teamKey="team-1" visible />);
    await act(async () => {});
    expect(setAgentsWorkspaceFilter).not.toHaveBeenCalled();
    // ws-a's agent has gone: the filter clears rather than show an empty list.
    listResult = [row({ handle: "b", sessionId: "sid-b" })];
    terminalTabIndex = { "team-1": { "ws-b": [{ id: "t-b", sessionId: "sid-b" }] } };
    act(() => publishProjectionOwners({ sessions: listResult, error: null }));
    view.rerender(<DockSlotHost teamKey="team-1" visible />);
    await waitFor(() => expect(setAgentsWorkspaceFilter).toHaveBeenCalledWith(null));
  });

  it("another team's card wears that team's repo glyph", async () => {
    const repo = (team: string, slug: string) => ({
      repoPath: `workspace:acme:${team}:${slug}`,
      repoName: slug,
      contextRepoPath: "",
      teamContentPath: "",
    });
    useOrgStore.setState({
      workspaceRepos: { a: repo("ops", "acme/api"), b: repo("ops", "acme/web") } as never,
    });
    workspaces = {
      "team-1": [{ id: "ws-a" }],
      "acme:ops": [{ id: "ws-o", kind: "story", storyId: "OPS-1", sourceRepoSlug: "acme/api" }],
    };
    terminalTabIndex = {
      "team-1": { "ws-a": [{ id: "t-a", sessionId: "sid-a" }] },
      "acme:ops": { "ws-o": [{ id: "t-o", sessionId: "sid-o" }] },
    };
    listResult = [
      row({ handle: "a", sessionId: "sid-a" }),
      row({ handle: "o", sessionId: "sid-o" }),
    ];
    // The active team is team-1, which has one repo; acme:ops has two.
    renderList("team-1");
    await waitFor(() => expect(rowOf("o")).not.toBeNull());
    expect(screen.getByTestId("agents-workspace-ws-o-repo")).toBeInTheDocument();
    expect(screen.queryByTestId("agents-workspace-ws-a-repo")).toBeNull();
  });

  it("an open quick view stays open when its agent's card folds", async () => {
    workspaces = { "team-1": [{ id: "ws-a" }] };
    terminalTabIndex = { "team-1": { "ws-a": [{ id: "t-1", sessionId: "sid-1" }] } };
    listResult = [row({ handle: "mover", sessionId: "sid-1" })];
    renderList();
    await waitFor(() => expect(rowOf("mover")).not.toBeNull());
    fireEvent.keyDown(rowOf("mover"), { key: "q" });
    await act(async () => {});
    expect(screen.getByRole("dialog", { name: "Quick view of mover" })).toBeInTheDocument();
    act(() => {
      paneFolds = { "#ws-fold:team-1|ws-a": true };
      notifyPaneStore();
    });
    await waitFor(() => expect(rowOf("mover")).toBeNull());
    expect(screen.getByRole("dialog", { name: "Quick view of mover" })).toBeInTheDocument();
  });

  it('two open teams named "ops" read as "org › ops" in their funnel, its chip and a Status row', async () => {
    useOrgStore.setState({
      orgs: [
        { slug: "acme", display_name: "Acme Co" },
        { slug: "beta", display_name: "Beta Inc" },
      ] as never,
    });
    openedTeamsMock = ["acme:ops", "beta:ops"];
    workspaces = { "acme:ops": [{ id: "ws-1" }], "beta:ops": [{ id: "ws-2" }] };
    terminalTabIndex = {
      "acme:ops": { "ws-1": [{ id: "t1", sessionId: "sid-1" }] },
      "beta:ops": { "ws-2": [{ id: "t2", sessionId: "sid-2" }] },
    };
    listResult = [
      row({ handle: "one", sessionId: "sid-1" }),
      row({ handle: "two", sessionId: "sid-2" }),
    ];
    renderList("acme:ops");
    const funnel = await screen.findByRole("button", { name: "Filter to Beta Inc › ops" });
    fireEvent.click(funnel);
    await waitFor(() =>
      expect(document.querySelector('.ag-chip[data-chip="team"]')).toHaveTextContent(
        "Team: Beta Inc › ops",
      ),
    );
    act(() => {
      hiddenPaneTeams = [];
      paneArrange = "status";
      notifyPaneStore();
    });
    await waitFor(() =>
      expect(
        document.querySelector('[data-agent-handle="two"] [data-testid="agent-detail"]'),
      ).toHaveTextContent(/^Beta Inc › ops/),
    );
  });

  it("orders teams by their org's display name, as the headers read, not by its slug", async () => {
    useOrgStore.setState({
      orgs: [
        { slug: "acme", display_name: "Zulu Works" },
        { slug: "zeta", display_name: "Alpha Labs" },
      ] as never,
    });
    workspaces = { "acme:ops": [{ id: "ws-1" }], "zeta:core": [{ id: "ws-2" }] };
    terminalTabIndex = {
      "acme:ops": { "ws-1": [{ id: "t1", sessionId: "sid-1" }] },
      "zeta:core": { "ws-2": [{ id: "t2", sessionId: "sid-2" }] },
    };
    listResult = [
      row({ handle: "one", sessionId: "sid-1" }),
      row({ handle: "two", sessionId: "sid-2" }),
    ];
    projectedTeamOrder = ["zeta:core", "acme:ops"];
    renderList("acme:ops");
    await waitFor(() => expect(rowOf("two")).not.toBeNull());
    const order = [
      ...document.querySelectorAll<HTMLElement>("[data-testid='agents-team-group']"),
    ].map((el) => el.dataset.teamKey);
    expect(order).toEqual(["zeta:core", "acme:ops"]);
  });

  it('searching "hidden" finds the agents whose tab is hidden', async () => {
    workspaces = { "team-1": [{ id: "ws-a" }] };
    terminalTabIndex = { "team-1": { "ws-a": [{ id: "t-a", sessionId: "sid-a" }] } };
    hiddenTabIndex = { "team-1": { "ws-a": [{ id: "t-h", sessionId: "sid-h" }] } };
    listResult = [
      row({ handle: "tucked", sessionId: "sid-h" }),
      row({ handle: "plain", sessionId: "sid-a" }),
    ];
    renderList();
    await waitFor(() => expect(rowOf("tucked")).not.toBeNull());
    act(() => storeState().setAgentsPaneSearch({ query: "hidden" }));
    await waitFor(() => expect(screen.getByTestId("agents-count")).toHaveTextContent("1/2"));
    expect(rowOf("tucked")).not.toBeNull();
    expect(rowOf("plain")).toBeNull();
  });

  it("the header's Run history button shows the finished runs in the pane, and again the agents", async () => {
    workspaces = { "team-1": [{ id: "ws-a" }] };
    terminalTabIndex = { "team-1": { "ws-a": [{ id: "t-a", sessionId: "sid-a" }] } };
    listResult = [row({ handle: "a", sessionId: "sid-a" })];
    renderList();
    await waitFor(() => expect(rowOf("a")).not.toBeNull());
    const button = screen.getByRole("button", { name: "Run history" });
    expect(button).toHaveAttribute("aria-pressed", "false");
    fireEvent.click(button);
    expect(await screen.findByRole("region", { name: "Run history" })).toBeInTheDocument();
    expect(button).toHaveAttribute("aria-pressed", "true");
    expect(rowOf("a")).toBeNull();
    fireEvent.click(button);
    await waitFor(() => expect(rowOf("a")).not.toBeNull());
    expect(screen.queryByRole("region", { name: "Run history" })).toBeNull();
  });

  it("the menu's Show and Arrange choices are radio groups the arrow keys move through", async () => {
    workspaces = { "team-1": [{ id: "ws-a" }] };
    terminalTabIndex = { "team-1": { "ws-a": [{ id: "t-a", sessionId: "sid-a" }] } };
    listResult = [row({ handle: "a", sessionId: "sid-a" })];
    renderList();
    await waitFor(() => expect(rowOf("a")).not.toBeNull());
    const show = within(filterMenu()).getByRole("radiogroup", { name: "Show" });
    const all = within(show).getByRole("radio", { name: /All/ });
    expect(all).toHaveAttribute("aria-checked", "true");
    all.focus();
    fireEvent.keyDown(all, { key: "ArrowDown" });
    expect(paneSearch.status).toBe("needs");
    await waitFor(() =>
      expect(document.activeElement).toBe(within(show).getByRole("radio", { name: /Needs you/ })),
    );
    const arrange = within(filterMenu()).getByRole("radiogroup", { name: "Arrange" });
    fireEvent.keyDown(within(arrange).getByRole("radio", { name: "By team" }), {
      key: "ArrowRight",
    });
    expect(paneArrange).toBe("status");
  });
});

describe("the Agents pane, review fixes round two", () => {
  const pinOf = (teamKey: string, handle: string, sessionId: string): Pin => ({
    kind: "session",
    teamKey,
    handle,
    sessionId,
    name: handle,
  });

  it("a saved workspace filter waits for the tabs and workspaces, not only the roster", async () => {
    workspaces = { "team-1": [{ id: "ws-a" }, { id: "ws-b" }] };
    terminalTabIndex = { "team-1": { "ws-b": [{ id: "t-b", sessionId: "sid-b" }] } };
    paneWorkspaceFilter = { teamKey: "team-1", workspaceId: "ws-a" };
    listResult = [row({ handle: "b", sessionId: "sid-b" })];
    // The roster is back but the tabs are not: ws-a's agent may still be on its way.
    layoutHydrated = false;
    const view = renderList();
    await act(async () => {});
    expect(setAgentsWorkspaceFilter).not.toHaveBeenCalled();
    layoutHydrated = true;
    persistedStateRestored = false;
    view.rerender(<DockSlotHost teamKey="team-1" visible />);
    await act(async () => {});
    expect(setAgentsWorkspaceFilter).not.toHaveBeenCalled();
    // The whole picture is back and ws-a holds no agent: the filter clears.
    persistedStateRestored = true;
    view.rerender(<DockSlotHost teamKey="team-1" visible />);
    await waitFor(() => expect(setAgentsWorkspaceFilter).toHaveBeenCalledWith(null));
  });

  it("a Machine stuck connecting stops holding a pin once the layout wait runs out", async () => {
    vi.useFakeTimers();
    try {
      useMachinesStore.setState({
        byTeam: {
          "team-1": [
            { machineId: "lab", label: "Lab", kind: "ssh", status: { state: "connecting" } },
          ],
        } as never,
      });
      listResult = [];
      agentPins = { "team-1": [pinOf("team-1", "far", "sid-far")] };
      renderList();
      await act(async () => {});
      expect(unpinAgents).not.toHaveBeenCalled();
      await act(async () => {
        vi.advanceTimersByTime(REMOTE_LAYOUT_WAIT_MS - 1000);
      });
      expect(unpinAgents).not.toHaveBeenCalled();
      // The wait ends with no other change: the list re-reads on its own.
      await act(async () => {
        vi.advanceTimersByTime(1000);
      });
      expect(unpinAgents).toHaveBeenCalledWith("team-1", [sessionPinId("team-1", "far")]);
    } finally {
      vi.useRealTimers();
      agentPins = {};
    }
  });

  it("looks are pruned once the tabs are back, and a team whose choices arrive later is pruned too", async () => {
    agentIdentity = { "team-1": { dev: { face: "🦊" }, gone: { face: "🐙" } } };
    listResult = [row({ handle: "dev" })];
    layoutHydrated = false;
    const view = renderList();
    await screen.findByRole("button", { name: agentRow(/dev/) });
    expect(pruneAgentIdentity).not.toHaveBeenCalled();
    layoutHydrated = true;
    view.rerender(<DockSlotHost teamKey="team-1" visible />);
    await waitFor(() => expect(pruneAgentIdentity).toHaveBeenCalledTimes(1));
    expect(pruneAgentIdentity.mock.calls[0]![0]).toBe("team-1");
    agentIdentity = { ...agentIdentity, "acme:ops": { "ended-ops": { role: "reviewer" } } };
    view.rerender(<DockSlotHost teamKey="team-1" visible />);
    await waitFor(() => expect(pruneAgentIdentity).toHaveBeenCalledTimes(2));
    expect(pruneAgentIdentity.mock.calls[1]![0]).toBe("acme:ops");
    // Each team is pruned once.
    view.rerender(<DockSlotHost teamKey="team-1" visible />);
    await act(async () => {});
    expect(pruneAgentIdentity).toHaveBeenCalledTimes(2);
  });

  it.each(["local", ""])("a tab whose origin names This Mac (%j) reads as This Mac", async (id) => {
    workspaces = { "team-1": [{ id: "ws-a" }] };
    terminalTabIndex = {
      "team-1": {
        "ws-a": [{ id: "t-a", sessionId: "sid-a", origin: { machineId: id } } as unknown as Tab],
      },
    };
    listResult = [row({ handle: "here", sessionId: "sid-a" })];
    renderList();
    await waitFor(() => expect(rowOf("here")).not.toBeNull());
    expect(within(rowOf("here")).queryByTestId("agent-row-machine")).toBeNull();
    expect(rowOf("here").getAttribute("title")).toContain("This Mac");
    expect(rowOf("here").getAttribute("title")).not.toContain("Unknown machine");
  });

  it("the Show menu's arrow keys step from a value none of its options holds", async () => {
    workspaces = { "team-1": [{ id: "ws-a" }] };
    terminalTabIndex = { "team-1": { "ws-a": [{ id: "t-a", sessionId: "sid-a" }] } };
    listResult = [row({ handle: "a", sessionId: "sid-a" })];
    paneSearch = { query: "", status: "bogus" };
    renderList();
    const show = within(filterMenu()).getByRole("radiogroup", { name: "Show" });
    const [first] = within(show).getAllByRole("radio");
    fireEvent.keyDown(first!, { key: "ArrowUp" });
    expect(paneSearch.status).toBe("idle");
    paneSearch = { query: "", status: "bogus" };
    fireEvent.keyDown(first!, { key: "ArrowDown" });
    expect(paneSearch.status).toBe("all");
  });

  it("a row of another listed team says the roles it holds, and offers Release in its team", async () => {
    roleConfigs["acme:ops"] = [
      { id: "triage", name: "Triage", icon: "builtin-chess:knight", defaultHandler: null },
    ];
    workspaces = { "acme:backend": [{ id: "ws-a" }], "acme:ops": [{ id: "ws-x" }] };
    terminalTabIndex = {
      "acme:backend": { "ws-a": [{ id: "t-a", sessionId: "sid-home" }] },
      "acme:ops": { "ws-x": [{ id: "t-x", sessionId: "sid-away" }] },
    };
    listResult = [
      row({ handle: "home", sessionId: "sid-home" }),
      row({ handle: "away", sessionId: "sid-away" }),
    ];
    rolesResult = {
      roles: [
        {
          role: "triage",
          roleName: "Triage",
          registrations: [
            {
              instanceId: "sid-away",
              instanceName: null,
              generation: 2,
              registeredAtMs: 1,
              lastRenewedAtMs: 1,
              expiresAtMs: 9e15,
              live: true,
            },
          ],
          counts: {},
          deliveries: [],
          deliveriesTruncated: false,
        },
      ],
    };
    publishProjectionOwners({ sessions: listResult, error: null });
    render(<DockSlotHost teamKey="acme:backend" visible />);
    const holds = await screen.findByTestId("agent-holds");
    const chip = await within(holds).findByRole("button", {
      name: "Queue role: Triage (role:triage)",
    });
    expect(chip).toHaveTextContent(/^Triage$/);
    expect(chip.querySelector("img")).toHaveAttribute(
      "src",
      BUILTIN_FACE_SOURCE.resolve("builtin-chess:knight"),
    );
    expect(rowOf("away")).not.toContainElement(chip);
    expect(holds.parentElement).toBe(rowOf("away").parentElement);
    expect(agentReadRoles).toHaveBeenCalledWith({ org: "acme", team: "ops" });
    fireEvent.contextMenu(rowOf("away"));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Release role Triage" }));
    await waitFor(() =>
      expect(releaseRole).toHaveBeenCalledWith(
        { org: "acme", team: "ops" },
        "triage",
        "sid-away",
        2,
      ),
    );
  });

  it("a story card keeps its story's title searchable and in its tooltip", async () => {
    workspaces = {
      "team-1": [
        {
          id: "ws-s",
          kind: "story",
          status: "ready",
          storyId: "SW-9",
          storyTitle: "Checkout redesign",
          storyFilePath: "/ctx/stories/SW-9.md",
        },
      ],
    };
    terminalTabIndex = { "team-1": { "ws-s": [{ id: "t-s", sessionId: "sid-s" }] } };
    listResult = [row({ handle: "dev", sessionId: "sid-s" })];
    renderList();
    await waitFor(() => expect(rowOf("dev")).not.toBeNull());
    const name = document.querySelector<HTMLElement>('[data-group-key="team-1|ws-s"] .ag-ws-name')!;
    expect(name).toHaveTextContent("SW-9");
    expect(name.getAttribute("title")).toContain("SW-9 · Checkout redesign");
    act(() => storeState().setAgentsPaneSearch({ query: "checkout" }));
    await waitFor(() => expect(screen.getByTestId("agents-count")).toHaveTextContent("1/1"));
    expect(rowOf("dev")).not.toBeNull();
  });

  it("Go to a launcher opens its folded card, shows its tab and reveals its row", async () => {
    workspaces = { "team-1": [{ id: "ws-a" }, { id: "ws-b" }] };
    terminalTabIndex = {
      "team-1": {
        "ws-a": [{ id: "tab-lead", sessionId: "sid-lead" }],
        "ws-b": [{ id: "tab-w", sessionId: "sid-worker" }],
      },
    };
    listResult = [
      row({ handle: "lead", sessionId: "sid-lead" }),
      row({ handle: "worker", sessionId: "sid-worker", launchedBy: "lead" }),
    ];
    paneFolds = { "#ws-fold:team-1|ws-a": true };
    renderList();
    await waitFor(() => expect(rowOf("worker")).not.toBeNull());
    expect(rowOf("lead")).toBeNull();
    fireEvent.contextMenu(rowOf("worker"));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Go to lead" }));
    expect(setAgentsPaneFold).not.toHaveBeenCalled();
    expect(paneFolds["#ws-fold:team-1|ws-a"]).toBe(true);
    expect(setActiveTerminalTab).toHaveBeenCalledWith("team-1", "tab-lead", "ws-a");
    await waitFor(() => expect(rowOf("lead")).not.toBeNull());
    expect(toast).not.toHaveBeenCalled();
  });

  it("a via chip to a launcher that is not listed says so", async () => {
    workspaces = { "team-1": [{ id: "ws-a" }] };
    terminalTabIndex = { "team-1": { "ws-a": [{ id: "tab-w", sessionId: "sid-worker" }] } };
    listResult = [row({ handle: "worker", sessionId: "sid-worker", launchedBy: "ghost" })];
    renderList();
    const pill = await screen.findByTestId("agent-manager-pill");
    fireEvent.click(pill);
    expect(toast).toHaveBeenCalledWith("ghost is not listed right now.");
  });

  it("an ended row's line two reads Ended with no clock, and line one does not repeat it", async () => {
    workspaces = { "team-1": [{ id: "ws-a" }] };
    terminalTabIndex = { "team-1": { "ws-a": [{ id: "t-e", sessionId: "sid-done" }] } };
    listResult = [
      row({ handle: "done", sessionId: "sid-done", startedAtMs: Date.now() - 125_000 }),
    ];
    useAgentStateStore.getState().recordSignal("sid-done", {
      tier: 1,
      signal: { kind: "claude-hook", event: "UserPromptSubmit" },
    });
    useAgentStateStore.getState().recordProcess("sid-done", { kind: "exited", code: 0 }, Infinity);
    projectSessionStatus("sid-done", "done");
    renderList();
    await waitFor(() => expect(rowOf("done")).not.toBeNull());
    const button = rowOf("done");
    expect(button.querySelector(".ag-avatar-dot")).toHaveAttribute("data-mark", "ended");
    const detail = within(button).getByTestId("agent-detail");
    expect(detail).toHaveTextContent("Ended");
    expect(detail.textContent).not.toMatch(/\d+[smh]\b|\d+:\d\d/);
    expect(button.querySelector(".ag-title")).not.toHaveTextContent("Ended");
  });

  it.each(["Enter", " "])("%j on an offline row shows it and opens no popup", async (key) => {
    useMachinesStore.setState({
      byTeam: {
        "team-1": [
          {
            machineId: "box",
            label: "Studio Mac",
            kind: "ssh",
            status: { state: "disconnected" },
          } as never,
        ],
      },
    });
    workspaces = { "team-1": [{ id: "ws-a" }] };
    terminalTabIndex = {
      "team-1": {
        "ws-a": [
          {
            id: "remote-1",
            sessionId: "remote-1",
            origin: { machineId: "box", member: "remote:box", handle: "h-1" },
          } as Tab,
        ],
      },
    };
    listResult = [row({ handle: "far", sessionId: "remote-1" })];
    renderList();
    await waitFor(() => expect(rowOf("far")).not.toBeNull());
    fireEvent.keyDown(rowOf("far"), { key });
    await act(async () => {});
    expect(activateTrayWorkspace).toHaveBeenCalledWith("team-1", "ws-a");
    expect(screen.queryByRole("dialog", { name: /Quick view/ })).toBeNull();
    expect(screen.queryByTestId("agent-quick-send")).toBeNull();
  });
});

describe("the Agents pane at scale", () => {
  it("with 150 agents, one agent's status change re-renders only its own row, quickly", async () => {
    const N = 150;
    const ids = Array.from({ length: 10 }, (_, i) => `ws-${i}`);
    workspaces = { "team-1": ids.map((id) => ({ id })) };
    terminalTabIndex = {
      "team-1": Object.fromEntries(
        ids.map((id, w) => [
          id,
          Array.from({ length: N / ids.length }, (_, k) => {
            const n = w * (N / ids.length) + k;
            return { id: `t-${n}`, sessionId: `sid-${n}` };
          }),
        ]),
      ),
    };
    listResult = Array.from({ length: N }, (_, n) =>
      row({ handle: `a${n}`, sessionId: `sid-${n}` }),
    );
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval"] });
    try {
      const mountStart = performance.now();
      renderList();
      // useSettled's 800 ms transition re-renders every mounted row once. The scale
      // test introduced in PR #2330 must finish it before measuring a status update.
      await act(async () => {
        await vi.runOnlyPendingTimersAsync();
      });
      // Another drain must leave every row alone: startup has actually settled,
      // without duplicating the production timer's delay here.
      const settledRenders = new Map(rowRenders);
      await act(async () => {
        await vi.runOnlyPendingTimersAsync();
      });
      vi.useRealTimers();
      expect(rowRenders).toEqual(settledRenders);
      expect(rowOf("a0")).not.toBeNull();
      const mountMs = performance.now() - mountStart;
      expect(screen.getByTestId("agents-count")).toHaveTextContent(String(N));
      const drawn = document.querySelectorAll(".ag-row").length;
      const before = new Map(rowRenders);
      const start = performance.now();
      await act(async () => hooked("sid-3"));
      expect(rowOf("a3").closest(".ag-row")).toHaveTextContent("Thinking");
      const updateMs = performance.now() - start;
      const delta = (sid: string) => (rowRenders.get(sid) ?? 0) - (before.get(sid) ?? 0);
      const changedRows = [...rowRenders.keys()].filter((sid) => delta(sid) > 0);
      // The numbers the story's verification notes record.
      process.stdout.write(
        `[SV-3 scale] agents=${N} drawn=${drawn} mount=${mountMs.toFixed(0)}ms ` +
          `update=${updateMs.toFixed(0)}ms changed-row-renders=${delta("sid-3")} ` +
          `other-rows-re-rendered=${changedRows.filter((s) => s !== "sid-3").length}\n`,
      );
      expect(changedRows).toEqual(["sid-3"]);
      expect(delta("sid-3")).toBeLessThanOrEqual(3);
      expect(updateMs).toBeLessThan(1000);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("the Agents pane, round four", () => {
  const pinOf = (teamKey: string, handle: string, sessionId: string): Pin => ({
    kind: "session",
    teamKey,
    handle,
    sessionId,
    name: handle,
  });
  /** team-1's one machine, offline, and `sid` a tab on it in ws-a. */
  function offlineAgent(handle: string, sid: string) {
    useMachinesStore.setState({
      byTeam: {
        "team-1": [
          { machineId: "box", label: "Studio", kind: "ssh", status: { state: "disconnected" } },
        ],
      } as never,
    });
    workspaces = { "team-1": [{ id: "ws-a" }] };
    terminalTabIndex = {
      "team-1": {
        "ws-a": [
          {
            id: sid,
            sessionId: sid,
            origin: { machineId: "box", member: "remote:box", handle: "h" },
          } as Tab,
        ],
      },
    };
    listResult = [row({ handle, sessionId: sid })];
  }

  it("a roster read that missed a source prunes no looks and drops no pins", async () => {
    agentIdentity = { "team-1": { gone: { face: "🐙" } } };
    agentPins = { "team-1": [pinOf("team-1", "gone", "sid-gone")] };
    listResult = [];
    publishProjectionOwners({ sessions: [], error: null, partial: true });
    const { rerender } = render(<DockSlotHost teamKey="team-1" visible />);
    await act(async () => {});
    expect(pruneAgentIdentity).not.toHaveBeenCalled();
    expect(unpinAgents).not.toHaveBeenCalled();
    // A whole read: now the ended session's look and pin go.
    act(() => publishProjectionOwners({ partial: false }));
    rerender(<DockSlotHost teamKey="team-1" visible />);
    await waitFor(() => expect(pruneAgentIdentity).toHaveBeenCalled());
    await waitFor(() => expect(unpinAgents).toHaveBeenCalled());
    agentPins = {};
  });

  it("pruning keeps the looks of hand-started agents whose tab is minimized or hidden", async () => {
    agentIdentity = { "team-1": { "sid-hid": { face: "🦊" } } };
    hiddenTabIndex = { "team-1": { "ws-a": [{ id: "t-h", sessionId: "sid-hid" }] } };
    renderList();
    await waitFor(() => expect(pruneAgentIdentity).toHaveBeenCalled());
    const [, keep] = pruneAgentIdentity.mock.calls[0] as unknown as [string, Set<string>];
    expect(keep.has("sid-hid")).toBe(true);
  });

  it("an open popup stays while a Show filter or a search hides its row, and closes once its agent is gone", async () => {
    workspaces = { "team-1": [{ id: "ws-a" }] };
    terminalTabIndex = { "team-1": { "ws-a": [{ id: "t-1", sessionId: "sid-1" }] } };
    listResult = [row({ handle: "mover", sessionId: "sid-1" })];
    const { rerender } = renderList();
    await waitFor(() => expect(rowOf("mover")).not.toBeNull());
    fireEvent.keyDown(rowOf("mover"), { key: "q" });
    await act(async () => {});
    act(() => storeState().setAgentsPaneSearch({ status: "needs" }));
    await waitFor(() => expect(rowOf("mover")).toBeNull());
    expect(screen.getByRole("dialog", { name: "Quick view of mover" })).toBeInTheDocument();
    act(() => storeState().setAgentsPaneSearch({ status: "all", query: "nobody" }));
    expect(screen.getByRole("dialog", { name: "Quick view of mover" })).toBeInTheDocument();
    listResult = [];
    act(() => publishProjectionOwners({ sessions: [] }));
    rerender(<DockSlotHost teamKey="team-1" visible />);
    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: "Quick view of mover" })).toBeNull(),
    );
  });

  it("an agent on an offline machine is not read as needing you: no bar, no ask line, no count", async () => {
    offlineAgent("far", "remote-1");
    awaitingAnswer("remote-1");
    renderList();
    await waitFor(() => expect(rowOf("far")).not.toBeNull());
    const far = rowOf("far").closest<HTMLElement>(".ag-row")!;
    expect(far).not.toHaveAttribute("data-needs");
    expect(far.querySelector(".ag-ask")).toBeNull();
    expect(rowOf("far").title).not.toContain("hooks are not connected");
    expect(document.querySelector("[data-has-needs]")).toBeNull();
    expect(showCount("Needs you")).toBe(0);
  });

  it("only the agent's own row is current, not its Pinned copy", async () => {
    workspaces = { "team-1": [{ id: "ws-a" }] };
    terminalTabIndex = { "team-1": { "ws-a": [{ id: "t-a", sessionId: "sid-a" }] } };
    activeWorkspaceMock = { "team-1": "ws-a" };
    activeTabMock = { "team-1|ws-a": "sid-a" };
    agentPins = { "team-1": [pinOf("team-1", "a", "sid-a")] };
    listResult = [row({ handle: "a", sessionId: "sid-a" })];
    renderList();
    const pinned = await screen.findByTestId("agents-pinned");
    const copies = document.querySelectorAll<HTMLElement>('[data-agent-handle="a"] .ag-main');
    expect(copies).toHaveLength(2);
    expect(pinned.querySelector('[data-agent-handle="a"] .ag-main')).not.toHaveAttribute(
      "aria-current",
    );
    const own = [...copies].find((el) => !pinned.contains(el))!;
    expect(own).toHaveAttribute("aria-current", "true");
    agentPins = {};
  });

  it("Run history opened from outside shows the pane on it, with its title, count and a needs-you badge", async () => {
    const { openPaneRunHistory } = await import("@/features/viewer/lib/agents-pane-runs");
    const prev = useOrgStore.getState();
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "ops" });
    try {
      workspaces = { "acme:ops": [{ id: "ws-a" }] };
      terminalTabIndex = { "acme:ops": { "ws-a": [{ id: "t-a", sessionId: "sid-a" }] } };
      listResult = [row({ handle: "a", sessionId: "sid-a" })];
      awaitingAnswer("sid-a");
      dockCollapsed = true;
      // Asked while no list is drawn: the pane opens, and the list shows the runs once it draws.
      act(() => openPaneRunHistory());
      expect(setWorkspacePanelCollapsed).toHaveBeenCalledWith(false);
      expect(revealTerminalInPlace).toHaveBeenCalledWith();
      renderList("acme:ops");
      expect(await screen.findByRole("region", { name: "Run history" })).toBeInTheDocument();
      expect(document.querySelector(".ag-pane-title")).toHaveTextContent("Run history");
      expect(screen.getByRole("button", { name: "Back to agents" })).toHaveAttribute(
        "aria-pressed",
        "true",
      );
      expect(screen.getByTestId("agents-runs-needs")).toHaveAccessibleName("1 agent needs you");
      fireEvent.click(screen.getByRole("button", { name: "Back to agents" }));
      await waitFor(() => expect(rowOf("a")).not.toBeNull());
      expect(document.querySelector(".ag-pane-title")).toHaveTextContent("Agents");
      expect(screen.queryByTestId("agents-runs-needs")).toBeNull();
    } finally {
      useOrgStore.setState({
        activeOrgSlug: prev.activeOrgSlug,
        activeTeamName: prev.activeTeamName,
      });
    }
  });

  it("a Show filter chip's × says what it clears", async () => {
    listResult = [row({ handle: "a" })];
    paneSearch = { query: "", status: "needs" };
    renderList();
    fireEvent.click(await screen.findByRole("button", { name: "Clear the Needs you filter" }));
    expect(paneSearch.status).toBe("all");
  });

  it("a row nothing places stays in the same team when the active team changes", async () => {
    openedTeamsMock = ["acme:ops", "team-1"];
    projectedHomeTeam = "acme:ops";
    listResult = [row({ handle: "loose", sessionId: "sid-loose" })];
    const teamOf = () =>
      rowOf("loose")
        .closest<HTMLElement>("[data-row-key]")!
        .dataset.rowKey!.includes(JSON.stringify("acme:ops"));
    const { rerender } = renderList("team-1");
    await waitFor(() => expect(rowOf("loose")).not.toBeNull());
    expect(teamOf()).toBe(true);
    rerender(<DockSlotHost teamKey="acme:ops" visible />);
    await waitFor(() => expect(rowOf("loose")).not.toBeNull());
    expect(teamOf()).toBe(true);
  });

  it("closing a popup whose row is no longer drawn gives focus to a row in view", async () => {
    workspaces = { "team-1": [{ id: "ws-a" }] };
    terminalTabIndex = {
      "team-1": {
        "ws-a": [
          { id: "t-1", sessionId: "sid-1" },
          { id: "t-2", sessionId: "sid-2" },
        ],
      },
    };
    listResult = [
      row({ handle: "mover", sessionId: "sid-1" }),
      row({ handle: "other", sessionId: "sid-2" }),
    ];
    renderList();
    await waitFor(() => expect(rowOf("mover")).not.toBeNull());
    fireEvent.keyDown(rowOf("mover"), { key: "q" });
    await act(async () => {});
    // Its card folds: the row is no longer drawn, the popup stays.
    act(() => {
      paneFolds = { "#ws-fold:team-1|ws-a": true };
      notifyPaneStore();
    });
    await waitFor(() => expect(rowOf("mover")).toBeNull());
    act(() => {
      paneFolds = {};
      notifyPaneStore();
    });
    await waitFor(() => expect(rowOf("other")).not.toBeNull());
    const dialog = screen.getByRole("dialog", { name: "Quick view of mover" });
    fireEvent.click(within(dialog).getByRole("button", { name: /close/i }));
    await act(async () => {});
    expect(document.activeElement?.closest(".ag-row")).not.toBeNull();
  });

  it("offers no Share with team for a personal pack on another team's row", async () => {
    const { useAvatarPacksStore } = await import("@/shared/hooks/use-avatar-packs");
    const prev = useOrgStore.getState();
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "ops" });
    try {
      const packs = {
        packs: [
          {
            id: "mine",
            name: "My Pack",
            scope: "personal",
            faces: [{ id: "f", ref: "mine:f", label: "F", src: "data:," }],
            warnings: [],
          },
        ] as never,
        defaultPack: null,
        agentAvatars: {},
        warnings: [],
      };
      useAvatarPacksStore.setState({ byTeam: { "acme:ops": packs, "acme:dev": packs } });
      workspaces = { "acme:dev": [{ id: "ws-d" }] };
      terminalTabIndex = { "acme:dev": { "ws-d": [{ id: "t-d", sessionId: "sid-dev" }] } };
      listResult = [row({ handle: "dev", sessionId: "sid-dev" })];
      renderList("acme:ops");
      await waitFor(() => expect(rowOf("dev")).not.toBeNull());
      fireEvent.contextMenu(rowOf("dev"));
      fireEvent.click(await screen.findByRole("menuitem", { name: "Face" }));
      fireEvent.click(await screen.findByRole("menuitem", { name: /My Pack/ }));
      expect(screen.queryByRole("menuitem", { name: "Share with team" })).toBeNull();
    } finally {
      useAvatarPacksStore.setState({ byTeam: {} });
      useOrgStore.setState({
        activeOrgSlug: prev.activeOrgSlug,
        activeTeamName: prev.activeTeamName,
      });
    }
  });
});

describe("session names and actions", () => {
  it.each([0, 3])(
    "renders a multiline live prompt on one title line with its full single-line accessible name (%i subagents)",
    async (subagentsOpen) => {
      const live = "$bmad-code-review Review  navigation\nFocus on:\n- Check labels";
      workspaces = { "team-1": [{ id: "ws-a" }] };
      terminalTabIndex = {
        "team-1": { "ws-a": [{ id: "sid-review", sessionId: "sid-review", name: live }] },
      };
      listResult = [
        row({
          handle: "review",
          startedWith: live,
          rowNames: {
            primary: "Review navigation",
            secondary: null,
            full: "$bmad-code-review Review navigation Focus on: - Check labels",
          },
        }),
      ];
      if (subagentsOpen) {
        useAgentStateStore.getState().recordActivity(
          {
            session: "sid-review",
            identity: { hostBootId: "boot", sessionIncarnation: "pty", invocationId: "run" },
            revision: 1,
            turnGeneration: 1,
            provider: "claude",
            state: "working",
            observed: true,
            limited: false,
            exit: null,
            asks: [],
            toolOpen: true,
            activityAgeMs: 0,
            subagentsOpen,
          },
          Infinity,
        );
        projectSessionStatus("sid-review", "tool-running", { subagents: subagentsOpen });
      }

      renderList();
      await waitFor(() =>
        expect(rowOf("review")?.querySelector(".ag-name")).toHaveTextContent("Review navigation"),
      );
      const name = rowOf("review").querySelector(".ag-name")!;
      expect(name.textContent).toBe("Review navigation");
      expect(name.closest(subagentsOpen ? ".ag-title-lead" : ".ag-title")).toHaveClass(
        subagentsOpen ? "ag-title-lead" : "ag-title-line",
      );
      expect(name).toHaveAttribute(
        "title",
        "$bmad-code-review Review navigation Focus on: - Check labels",
      );
      expect(name).toHaveAccessibleName(
        "$bmad-code-review Review navigation Focus on: - Check labels",
      );
    },
  );

  it("renders distinct live titles for two saved instances, searches both identities and follows title updates without moving rows", async () => {
    const { useAllAgentRowsStore } = await import("@/features/agents/lib/agent-names");
    useAllAgentRowsStore.setState({
      rows: [
        {
          key: "acme/team/dev",
          name: "Developer",
          runtime: "claude",
        } as import("@/tauri/commands").AgentRow,
      ],
    });
    try {
      workspaces = { "team-1": [{ id: "ws-a" }] };
      terminalTabIndex = {
        "team-1": {
          "ws-a": [
            { id: "sid-first", sessionId: "sid-first", name: "Fix tabs" },
            { id: "sid-second", sessionId: "sid-second", name: "Fix scroll" },
          ],
        },
      };
      listResult = [
        row({
          handle: "first",
          displayName: "Developer",
          agentRevision: "acme/team/dev@1",
          rowNames: { primary: "Fix tabs", secondary: "Developer", full: "Fix tabs · Developer" },
        }),
        row({
          handle: "second",
          displayName: "Developer",
          agentRevision: "acme/team/dev@1",
          rowNames: {
            primary: "Fix scroll",
            secondary: "Developer",
            full: "Fix scroll · Developer",
          },
        }),
      ];
      renderList();
      await waitFor(() => expect(rowOf("first")).toHaveTextContent("Fix tabs"));
      expect(rowOf("second")).toHaveTextContent("Fix scroll");
      expect(rowOf("first")).toHaveTextContent("Developer");
      act(() => storeState().setAgentsPaneSearch({ query: "Developer" }));
      expect(rowOf("first")).toBeTruthy();
      expect(rowOf("second")).toBeTruthy();
      act(() => storeState().setAgentsPaneSearch({ query: "Fix tabs" }));
      expect(rowOf("first")).toBeTruthy();
      expect(rowOf("second")).toBeNull();
      act(() => storeState().setAgentsPaneSearch({ query: "" }));

      const area = document.querySelector<HTMLElement>(".ag-scroll")!;
      area.scrollTop = 70;
      const handles = () =>
        Array.from(document.querySelectorAll<HTMLElement>("[data-agent-handle]")).map(
          (el) => el.dataset.agentHandle,
        );
      const before = handles();
      act(() => {
        terminalTabIndex = {
          "team-1": {
            "ws-a": terminalTabIndex["team-1"]!["ws-a"]!.map((t) =>
              t.sessionId === "sid-first" ? { ...t, name: "Review tabs" } : t,
            ),
          },
        };
        listResult = listResult.map((row) =>
          row.handle === "first"
            ? {
                ...row,
                rowNames: {
                  primary: "Review tabs",
                  secondary: "Developer",
                  full: "Review tabs · Developer",
                },
              }
            : row,
        );
        publishProjectionOwners({ sessions: [...listResult] });
      });
      await waitFor(() => expect(rowOf("first")).toHaveTextContent("Review tabs"));
      expect(handles()).toEqual(before);
      expect(area.scrollTop).toBe(70);
      fireEvent.contextMenu(rowOf("first"));
      expect(await screen.findByRole("menuitem", { name: "Edit saved agent" })).toBeEnabled();
      fireEvent.click(screen.getByRole("menuitem", { name: "Stop…" }));
      expect(await screen.findByRole("alertdialog")).toHaveTextContent("Review tabs · Developer");
      fireEvent.click(screen.getByRole("button", { name: "Stop" }));
      await waitFor(() => expect(dashboardKillByHandle).toHaveBeenCalledWith("first"));
      expect(dashboardKillByHandle).toHaveBeenCalledTimes(1);
      expect(rowOf("second")).toHaveTextContent("Fix scroll");
    } finally {
      useAllAgentRowsStore.setState({ rows: null });
    }
  });

  it.each([false, true])(
    "updates projected progress titles in place without repeating the note, and stops by stable name (undeclared=%s)",
    async (undeclared) => {
      if (undeclared) rosterStatus["sid-first"] = "dispatch";
      const names = (primary: string) => ({
        primary,
        secondary: "dev-worker",
        full: `${primary} · dev-worker`,
        source: "progress" as const,
      });
      listResult = [
        row({ handle: "first", avatar: "#00aa00", rowNames: names("Review round one") }),
        row({ handle: "second" }),
      ];
      progressBySessionMock = {
        "sid-first": { phase: "review", note: "Review round one", step: 2, total: 5 },
      };
      const view = renderList();
      await waitFor(() => expect(rowOf("first")).toHaveTextContent("Review round one"));
      const first = rowOf("first");
      const area = view.container.querySelector<HTMLElement>(".ag-scroll")!;
      const order = () =>
        [...view.container.querySelectorAll("[data-agent-handle]")].map((el) =>
          el.getAttribute("data-agent-handle"),
        );
      const before = order();
      area.scrollTop = 70;
      expect(first.querySelector(".ag-activity")).toHaveTextContent("2 of 5 · review");
      act(() => {
        progressBySessionMock = { "sid-first": { phase: "review", note: "Review round two" } };
        listResult = [{ ...listResult[0], rowNames: names("Review round two") }, listResult[1]];
        publishProjectionOwners({ sessions: listResult });
      });
      await waitFor(() => expect(rowOf("first")).toHaveTextContent("Review round two"));
      expect(rowOf("first")).toBe(first);
      expect(first.querySelector(".ag-face")).toHaveTextContent("DW");
      expect(first.querySelector(".ag-activity")).toBeNull();
      expect(order()).toEqual(before);
      expect(area.scrollTop).toBe(70);
      fireEvent.contextMenu(first);
      fireEvent.click(await screen.findByRole("menuitem", { name: "Stop…" }));
      const dialog = await screen.findByRole("alertdialog");
      expect(within(dialog).getByRole("heading")).toHaveTextContent(/^Stop dev-worker\?$/);
      expect(dialog).toHaveTextContent("Currently: Review round two");
    },
  );

  it("confirms that stopping a hand-started agent closes only its terminal, using the existing kill path", async () => {
    listResult = [
      row({
        handle: "hand",
        sessionId: "sid-hand",
        kind: "hand-started",
        displayName: "Investigate logs",
      }),
    ];
    renderList();
    fireEvent.contextMenu(
      await screen.findByRole("button", { name: agentRow(/Investigate logs/) }),
    );
    fireEvent.click(await screen.findByRole("menuitem", { name: "Stop…" }));
    expect(await screen.findByRole("alertdialog")).toHaveTextContent("closes this terminal tab");
    expect(removeTerminalTabBySession).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Stop" }));
    await waitFor(() =>
      expect(removeTerminalTabBySession).toHaveBeenCalledWith("sid-hand", { kill: true }),
    );
    expect(dashboardKillByHandle).not.toHaveBeenCalled();
  });
});

it.each(["generic", "story"])(
  "moves a tabless %s agent out of No workspace when its workspace record arrives",
  async (kind) => {
    listResult = [
      row({
        handle: "late-worker",
        workspace: "/wt/late",
        story: kind === "story" ? "STORY-1" : null,
      }),
    ];
    const view = renderList();
    await waitFor(() => expect(rowOf("late-worker")).toBeTruthy());
    expect(document.querySelector('[data-testid="agents-workspace-group"]')).toHaveAttribute(
      "data-workspace-id",
      "",
    );
    workspaces = {
      "team-1": [
        { id: "late", kind, cwd: "/wt/late", storyId: kind === "story" ? "STORY-1" : undefined },
      ],
    };
    projectedOverrides = {
      "sid-late-worker": { workspaceRef: "late", storyRef: kind === "story" ? "late" : null },
    };
    act(() => publishProjectionOwners({ sessions: listResult }));
    view.rerender(<DockSlotHost teamKey="team-1" visible />);
    await waitFor(() =>
      expect(document.querySelector('[data-testid="agents-workspace-group"]')).toHaveAttribute(
        "data-workspace-id",
        "late",
      ),
    );
    expect(screen.getByTestId("agents-workspace-late-crew")).toBeInTheDocument();
    expect(revealAgentsWorkspace).not.toHaveBeenCalled();
    expect(setWorkspacePanelCollapsed).not.toHaveBeenCalled();
  },
);

it("reveals a workspace once without saving temporary folds, moving focus or replaying after a manual scroll", async () => {
  workspaces = { "team-1": [{ id: "ws-a" }, { id: "ws-b" }] };
  terminalTabIndex = {
    "team-1": {
      "ws-a": [{ id: "a", sessionId: "sid-a" }],
      "ws-b": [{ id: "b", sessionId: "sid-b" }],
    },
  };
  listResult = [row({ handle: "a" }), row({ handle: "b" })];
  paneFolds = { "#ws-fold:team-1|ws-b": true };
  const view = renderList();
  await waitFor(() => expect(rowOf("a")).toBeTruthy());
  const terminal = document.createElement("textarea");
  document.body.append(terminal);
  terminal.focus();
  try {
    headerScroll.outsideWindow = true;
    headerScroll.rows = [];
    paneWorkspaceReveal = { teamKey: "team-1", workspaceId: "ws-b", nonce: 1 };
    view.rerender(<DockSlotHost teamKey="team-1" visible />);
    await waitFor(() => expect(clearAgentsWorkspaceReveal).toHaveBeenCalled());
    expect(rowOf("b")).toBeTruthy();
    expect(headerScroll.rows).toHaveLength(1);
    const requested = document.querySelector(`[data-row-index="${headerScroll.rows[0]}"]`);
    expect(requested?.querySelector('[data-workspace-id="ws-b"]')).toBeTruthy();
    expect(setAgentsPaneFold).not.toHaveBeenCalled();
    expect(paneFolds["#ws-fold:team-1|ws-b"]).toBe(true);
    expect(document.activeElement).toBe(terminal);
    const area = document.querySelector<HTMLElement>(".ag-scroll")!;
    act(() => {
      area.scrollTop = 90;
      fireEvent.scroll(area);
    });
    view.rerender(<DockSlotHost teamKey="team-1" visible />);
    expect(area.scrollTop).toBe(90);
    expect(headerScroll.rows).toHaveLength(1);
    expect(document.querySelector("[data-flash], [data-team-flash], .ag-flash-box")).toBeNull();
  } finally {
    headerScroll.outsideWindow = false;
    headerScroll.rows = [];
    terminal.remove();
  }
});

it("does not scroll an already visible workspace or an empty/filtered target", async () => {
  workspaces = { "team-1": [{ id: "ws-a" }] };
  terminalTabIndex = { "team-1": { "ws-a": [{ id: "a", sessionId: "sid-a" }] } };
  listResult = [row({ handle: "a" })];
  const view = renderList();
  await waitFor(() => expect(rowOf("a")).toBeTruthy());
  const area = document.querySelector<HTMLElement>(".ag-scroll")!;
  const scrollTo = vi.spyOn(area, "scrollTo");
  paneWorkspaceReveal = { teamKey: "team-1", workspaceId: "ws-a", nonce: 1 };
  view.rerender(<DockSlotHost teamKey="team-1" visible />);
  await act(async () => {});
  expect(scrollTo).not.toHaveBeenCalled();
  paneWorkspaceReveal = { teamKey: "team-1", workspaceId: "missing", nonce: 2 };
  view.rerender(<DockSlotHost teamKey="team-1" visible />);
  await act(async () => {});
  expect(scrollTo).not.toHaveBeenCalled();
  scrollTo.mockRestore();
});

it("selects a bottom row across workspaces in the rendered virtual list without resetting scroll", async () => {
  scrollAreaPx = 200;
  const handles = Array.from({ length: 50 }, (_, i) => `worker-${String(i).padStart(2, "0")}`);
  workspaces = { "team-1": [{ id: "ws-a" }, { id: "ws-b" }] };
  terminalTabIndex = {
    "team-1": {
      "ws-a": handles.slice(0, 25).map((h) => ({ id: `sid-${h}`, sessionId: `sid-${h}` })),
      "ws-b": handles.slice(25).map((h) => ({ id: `sid-${h}`, sessionId: `sid-${h}` })),
    },
  };
  listResult = handles.map((handle, i) => row({ handle, startedAtMs: 1000 + i }));
  const view = renderList();
  await waitFor(() =>
    expect(document.querySelectorAll("[data-agent-handle]").length).toBeGreaterThan(0),
  );
  const first = document.querySelector<HTMLElement>("[data-agent-handle] .ag-main")!;
  const firstWorkspace =
    Number(first.closest<HTMLElement>("[data-agent-handle]")!.dataset.agentHandle?.slice(-2)) >= 25
      ? "ws-b"
      : "ws-a";
  fireEvent.click(first);
  const area = document.querySelector<HTMLElement>(".ag-scroll")!;
  act(() => {
    area.scrollTop = 1600;
    fireEvent.scroll(area);
  });
  await waitFor(() =>
    expect(
      Array.from(document.querySelectorAll<HTMLElement>("[data-agent-handle]")).find(
        (el) => Number(el.dataset.agentHandle?.slice(-2)) >= 25,
      ),
    ).toBeTruthy(),
  );
  const rowStart = (el: HTMLElement) =>
    Number(el.style.transform.match(/translateY\(([-.\d]+)px\)/)?.[1] ?? 0);
  const visibleRows = Array.from(
    document.querySelectorAll<HTMLElement>('[data-row-kind="agent"]'),
  ).filter((el) => rowStart(el) >= area.scrollTop && rowStart(el) < area.scrollTop + scrollAreaPx);
  const bottom = visibleRows.at(-1)!.querySelector<HTMLElement>("[data-agent-handle]")!;
  const selected = bottom.dataset.agentHandle!;
  const top = area.scrollTop;
  fireEvent.click(rowOf(selected));
  await act(async () => {});
  const selectedWorkspace = Number(selected.slice(-2)) >= 25 ? "ws-b" : "ws-a";
  expect(selectedWorkspace).not.toBe(firstWorkspace);
  expect(setActiveTerminalTab).toHaveBeenLastCalledWith(
    "team-1",
    `sid-${selected}`,
    selectedWorkspace,
  );
  expect(focusActiveWorkspaceTerminal).toHaveBeenCalledWith("team-1");
  expect(area.scrollTop).toBe(top);
  expect(rowOf(selected)).toBeTruthy();
  expect(revealAgentsWorkspace).not.toHaveBeenCalled();
  view.rerender(<DockSlotHost teamKey="team-1" visible={false} />);
  view.rerender(<DockSlotHost teamKey="team-1" visible />);
  await waitFor(() => expect(rowOf(selected)).toBeTruthy());
  const restored = document.querySelector<HTMLElement>(".ag-scroll")!;
  const restoredRow = rowOf(selected).closest<HTMLElement>('[data-row-kind="agent"]')!;
  expect(restored.scrollTop).toBeGreaterThan(0);
  expect(rowStart(restoredRow)).toBeLessThan(restored.scrollTop + scrollAreaPx);
  expect(rowStart(restoredRow) + 40).toBeGreaterThan(restored.scrollTop);
});

it("closes Run history through explicit workspace navigation", async () => {
  listResult = [row({ handle: "worker" })];
  workspaces = { "team-1": [{ id: "ws-a" }] };
  terminalTabIndex = { "team-1": { "ws-a": [{ id: "worker", sessionId: "sid-worker" }] } };
  const view = renderList();
  fireEvent.click(screen.getByRole("button", { name: "Run history" }));
  expect(screen.getByRole("button", { name: "Back to agents" })).toBeInTheDocument();
  paneWorkspaceReveal = { teamKey: "team-1", workspaceId: "ws-a", nonce: 1 };
  view.rerender(<DockSlotHost teamKey="team-1" visible />);
  await waitFor(() => expect(rowOf("worker")).toBeTruthy());
  expect(screen.getByRole("button", { name: "Run history" })).toBeInTheDocument();
});

it("waits for a post-hydration roster before pruning restored managers and renders their relationship", async () => {
  layoutHydrated = false;
  agentIdentity = { "team-1": { worker: { manager: "lead" } } };
  paneFolds = { "#ws-fold:team-1|other": true };
  let finish!: (rows: AgentSessionRow[]) => void;
  lateRoster = new Promise((resolve) => {
    finish = resolve;
  });
  const view = renderList();
  act(() => publishProjectionOwners({ sessions: [], error: null, partial: false }));
  await act(async () => {});
  expect(pruneAgentIdentity).not.toHaveBeenCalled();
  workspaces = { "team-1": [{ id: "ws-a" }] };
  layoutHydrated = true;
  view.rerender(<DockSlotHost teamKey="team-1" visible />);
  await act(async () => {});
  expect(pruneAgentIdentity).not.toHaveBeenCalled();
  listResult = [row({ handle: "lead", startedAtMs: 1 }), row({ handle: "worker", startedAtMs: 2 })];
  terminalTabIndex = {
    "team-1": { "ws-a": listResult.map((r) => ({ id: r.sessionId!, sessionId: r.sessionId! })) },
  };
  await act(async () => {
    finish(listResult);
  });
  await waitFor(() => expect(rowOf("worker")).toBeTruthy());
  await waitFor(() => expect(pruneAgentIdentity).toHaveBeenCalled());
  const kept = pruneAgentIdentity.mock.calls[0] as unknown as [string, Set<string>];
  expect([...kept[1]]).toContain("worker");
  expect(agentIdentity["team-1"]?.worker?.manager).toBe("lead");
  expect(
    parseFloat(rowOf("worker").closest<HTMLElement>(".ag-row")!.style.paddingLeft),
  ).toBeGreaterThan(parseFloat(rowOf("lead").closest<HTMLElement>(".ag-row")!.style.paddingLeft));
  expect(paneFolds["#ws-fold:team-1|other"]).toBe(true);
});

it("opens an archived running agent's saved definition through the editor action", async () => {
  const { useAllAgentRowsStore } = await import("@/features/agents/lib/agent-names");
  const { openAgentEditor } = await import("@/features/agents/agents-virtual-tab");
  useAllAgentRowsStore.setState({
    rows: [
      {
        key: "acme/team/dev",
        name: "Developer",
        runtime: "claude",
        archived: true,
        scope: "team",
      } as import("@/tauri/commands").AgentRow,
    ],
  });
  try {
    listResult = [row({ handle: "worker", agentRevision: "acme/team/dev@1" })];
    renderList();
    await waitFor(() => expect(rowOf("worker")).toBeTruthy());
    fireEvent.contextMenu(rowOf("worker"));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Edit saved agent" }));
    expect(openAgentEditor).toHaveBeenCalledWith("acme/team/dev", true);
  } finally {
    useAllAgentRowsStore.setState({ rows: null });
  }
});

it("an explicit workspace reveal brings a genuinely offscreen header into the virtualized viewport", async () => {
  scrollAreaPx = 200;
  const scrollTo = vi.spyOn(HTMLElement.prototype, "scrollTo").mockImplementation(function (
    this: HTMLElement,
    options: ScrollToOptions | number,
    y?: number,
  ) {
    this.scrollTop = typeof options === "number" ? (y ?? 0) : (options.top ?? this.scrollTop);
    this.dispatchEvent(new Event("scroll"));
  });
  const height = vi
    .spyOn(HTMLElement.prototype, "scrollHeight", "get")
    .mockImplementation(function (this: HTMLElement) {
      return this.classList.contains("ag-scroll")
        ? parseFloat(this.querySelector<HTMLElement>('[role="list"]')?.style.height ?? "0")
        : 0;
    });
  const client = vi
    .spyOn(HTMLElement.prototype, "clientHeight", "get")
    .mockImplementation(function (this: HTMLElement) {
      return this.classList.contains("ag-scroll") ? scrollAreaPx : 40;
    });
  try {
    workspaces = { "team-1": [{ id: "ws-a" }, { id: "ws-b" }] };
    const handles = Array.from({ length: 60 }, (_, i) => `top-${i}`);
    listResult = [
      ...handles.map((handle, i) => row({ handle, startedAtMs: i + 1 })),
      row({ handle: "target", startedAtMs: 0 }),
    ];
    terminalTabIndex = {
      "team-1": {
        "ws-a": handles.map((h) => ({ id: `sid-${h}`, sessionId: `sid-${h}` })),
        "ws-b": [{ id: "sid-target", sessionId: "sid-target" }],
      },
    };
    workspaceOrderState = { "team-1": ["ws-a", "ws-b"] };
    projectedOverrides = Object.fromEntries(
      handles.map((handle, i) => [`sid-${handle}`, { order: 59 - i }]),
    );
    const view = renderList();
    await waitFor(() => expect(rowOf("top-59")).toBeTruthy());
    expect(document.querySelector('[data-workspace-id="ws-b"]')).toBeNull();
    paneWorkspaceReveal = { teamKey: "team-1", workspaceId: "ws-b", nonce: 1 };
    view.rerender(<DockSlotHost teamKey="team-1" visible />);
    await waitFor(() => {
      const header = document.querySelector<HTMLElement>('[data-workspace-id="ws-b"]');
      expect(header).toBeTruthy();
      const top = Number(
        header!
          .closest<HTMLElement>("[data-row-index]")!
          .style.transform.match(/translateY\(([-.\d]+)px\)/)?.[1],
      );
      const area = document.querySelector<HTMLElement>(".ag-scroll")!;
      expect(top).toBeGreaterThanOrEqual(area.scrollTop - 1);
      expect(top).toBeLessThan(area.scrollTop + scrollAreaPx);
    });
  } finally {
    scrollTo.mockRestore();
    height.mockRestore();
    client.mockRestore();
  }
});

it.each(["story", "generic"])(
  "attributes a tabless %s agent to its canonical team despite a duplicate story id",
  async (kind) => {
    openedTeamsMock = ["acme:a", "acme:b"];
    workspaces = {
      "acme:a": [
        {
          id: "a",
          kind,
          cwd: "/wt/a",
          storyId: "TASK-1",
          storyFilePath: "/a/task.md",
          storyStatus: "in-progress",
          status: "ready",
        },
      ],
      "acme:b": [
        {
          id: "b",
          kind,
          cwd: "/wt/b",
          storyId: "TASK-1",
          storyFilePath: "/b/task.md",
          storyStatus: "in-progress",
          status: "ready",
        },
      ],
    };
    listResult = [
      row({
        handle: "custom-worker",
        sessionId: null,
        workspace: "/wt/b",
        story: kind === "story" ? "TASK-1" : null,
      }),
    ];
    projectedOverrides = {
      "custom-worker": {
        teamKey: "acme:b",
        workspaceRef: "b",
        storyRef: kind === "story" ? "b" : null,
      },
    };
    renderList("acme:a");
    await waitFor(() => expect(rowOf("custom-worker")).toBeTruthy());
    expect(document.querySelector('[data-group-key="acme:b|b"]')).toBeTruthy();
    expect(document.querySelector('[data-group-key="acme:a|b"]')).toBeNull();
    expect(rowOf("custom-worker")).toHaveAttribute(
      "aria-description",
      expect.stringContaining("Team b"),
    );
  },
);

it("keeps manager choices when the roster arrives before launch restoration", async () => {
  layoutHydrated = false;
  agentIdentity = {
    "team-1": {
      worker: { manager: "lead" },
      lead: { manager: "@none" },
      gone: { manager: "lead" },
    },
  };
  let finish!: (rows: AgentSessionRow[]) => void;
  lateRoster = new Promise((resolve) => {
    finish = resolve;
  });
  const view = renderList();
  workspaces = { "team-1": [{ id: "ws-a" }] };
  layoutMembers = {
    local: {
      workspaces: {
        "team-1|ws-a": {
          tabs: ["lead", "worker"]
            .map((handle) => ({
              session: `sid-${handle}`,
              state: { kind: "live" },
              tags: { launchHandle: handle },
            }))
            .concat([
              { session: "sid-gone", state: { kind: "exited" }, tags: { launchHandle: "gone" } },
            ]),
        },
      },
    },
  };
  terminalTabIndex = {
    "team-1": {
      "ws-a": ["lead", "worker"].map((h) => ({ id: `sid-${h}`, sessionId: `sid-${h}` })),
    },
  };
  layoutHydrated = true;
  pruneAgentIdentity.mockImplementationOnce((team: string, keep: Set<string>) => {
    agentIdentity = {
      ...agentIdentity,
      [team]: Object.fromEntries(
        Object.entries(agentIdentity[team] ?? {}).filter(([handle]) => keep.has(handle)),
      ),
    };
  });
  view.rerender(<DockSlotHost teamKey="team-1" visible />);
  await act(async () => {
    finish([
      row({ handle: "sid-lead", sessionId: "sid-lead", kind: "live-pty" }),
      row({ handle: "sid-worker", sessionId: "sid-worker", kind: "live-pty" }),
    ]);
  });
  await waitFor(() => expect(pruneAgentIdentity).toHaveBeenCalled());
  expect(agentIdentity["team-1"]?.worker?.manager).toBe("lead");
  expect(agentIdentity["team-1"]?.lead?.manager).toBe("@none");
  expect(agentIdentity["team-1"]?.gone).toBeUndefined();
  act(() =>
    publishProjectionOwners({
      sessions: [row({ handle: "lead" }), row({ handle: "worker" })],
      error: null,
      partial: false,
    }),
  );
  await waitFor(() => expect(rowOf("worker")).toBeTruthy());
  expect(
    parseFloat(rowOf("worker").closest<HTMLElement>(".ag-row")!.style.paddingLeft),
  ).toBeGreaterThan(parseFloat(rowOf("lead").closest<HTMLElement>(".ag-row")!.style.paddingLeft));
});

describe("Manager view receiving fleet", () => {
  function fleet() {
    workspaces = {
      "team-1": [
        {
          id: "ws-a",
          kind: "story",
          storyId: "NAV-1",
          storyTitle: "A long navigation workspace title",
        },
        { id: "ws-b", kind: "story", storyId: "ROLE-1", storyTitle: "Roles" },
        { id: "ws-c", kind: "generic", cwd: "/release" },
        { id: "empty", kind: "story", storyId: "EMPTY-1" },
      ],
    };
    terminalTabIndex = {
      "team-1": {
        "ws-a": [
          { id: "lead-tab", sessionId: "sid-lead" },
          { id: "solo-tab", sessionId: "sid-solo" },
        ],
        "ws-b": [
          { id: "mid-tab", sessionId: "sid-mid" },
          { id: "other-tab", sessionId: "sid-other" },
        ],
        "ws-c": [
          { id: "leaf-tab", sessionId: "sid-leaf" },
          { id: "chosen-tab", sessionId: "sid-chosen" },
          { id: "shell-tab", sessionId: "sid-shell", name: "Plain shell" },
        ],
      },
    };
    listResult = [
      row({ handle: "lead" }),
      row({ handle: "mid", launchedBy: "lead" }),
      row({ handle: "leaf", launchedBy: "mid" }),
      row({ handle: "other" }),
      row({ handle: "chosen", launchedBy: "other" }),
      row({ handle: "solo" }),
      row({ handle: "shell", kind: "live-pty" }),
    ];
    agentIdentity = { "team-1": { chosen: { manager: "lead" } } };
    activeWorkspaceMock = { "team-1": "ws-a" };
    activeTabMock = { "team-1|ws-a": "lead-tab" };
    railKeysMock = {
      "team-1": {
        "ws-a": "pre-beta-fixes",
        "ws-b": "pre-beta-fixes",
        "ws-c": "",
        empty: "empty-epic",
      },
    };
  }
  it("keeps nested managers across three workspaces, labels chosen vs launcher, and never promotes a plain shell", async () => {
    fleet();
    renderList();
    await waitFor(() => expect(rowOf("leaf")).not.toBeNull());
    expect(screen.getByRole("button", { name: "Workspace" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    fireEvent.click(screen.getByRole("button", { name: "Manager" }));
    const handles = [...document.querySelectorAll("[data-agent-handle]")].map((el) =>
      el.getAttribute("data-agent-handle"),
    );
    expect(handles.indexOf("lead")).toBeLessThan(handles.indexOf("mid"));
    expect(handles.indexOf("mid")).toBeLessThan(handles.indexOf("leaf"));
    expect(rowOf("leaf").closest(".ag-row")!.getAttribute("style")).toContain("padding-left: 54px");
    expect(rowOf("chosen")).toHaveTextContent("Chosen manager: lead");
    expect(rowOf("chosen").getAttribute("title")).toContain("Launched by other");
    expect(rowOf("leaf").closest(".ag-row")).toHaveAttribute("data-parent-source", "launcher");
    const accents = ["lead", "mid", "leaf", "chosen"].map((h) =>
      (rowOf(h).closest(".ag-row") as HTMLElement).style.getPropertyValue("--ag-guide"),
    );
    expect(new Set(accents).size).toBe(1);
    expect(accents[0]).not.toBe("");
    expect(rowOf("solo").closest(".ag-row")!.querySelector(".ag-tree-guide")).toBeNull();
    expect(screen.getByRole("heading", { name: "No manager" })).toBeInTheDocument();
    expect(rowOf("shell")).toBeNull();
    expect(rowOf("leaf").querySelector(".ag-workspace-badge")).toHaveTextContent("release");
    expect(rowOf("lead").querySelector(".ag-workspace-badge")).toHaveAttribute(
      "data-current-workspace",
    );
    expect(activeWorkspaceMock["team-1"]).toBe("ws-a");
  });
  it("folded crew counts every descendant and arrow keys preserve focus", async () => {
    fleet();
    paneArrange = "manager";
    renderList();
    await waitFor(() => expect(rowOf("leaf")).not.toBeNull());
    const lead = rowOf("lead");
    lead.focus();
    fireEvent.keyDown(lead, { key: "ArrowLeft" });
    expect(screen.getByRole("img", { name: "3 agents folded" })).toHaveTextContent("+3");
    expect(rowOf("leaf")).toBeNull();
    expect(rowOf("lead")).toHaveFocus();
    fireEvent.keyDown(rowOf("lead"), { key: "ArrowRight" });
    expect(rowOf("leaf")).not.toBeNull();
    expect(screen.queryByRole("img", { name: "3 agents folded" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Collapse mid's crew" }));
    const count = screen.getByRole("img", { name: "1 agent folded" });
    expect(count).toHaveTextContent("+1");
    expect(count).toHaveAttribute("title", "1 agent folded");
    act(() => awaitingAnswer("sid-leaf"));
    const needsCount = screen.getByRole("img", { name: "1 agent folded, 1 needs you" });
    expect(needsCount).toHaveAttribute("data-needs", "true");
    expect(rowOf("mid").closest(".ag-row")).not.toHaveAttribute("data-needs");
  });
  it("remembers separate manager/workspace folds through view switches and keeps selection and scroll", async () => {
    fleet();
    const view = renderList();
    await waitFor(() => expect(rowOf("leaf")).not.toBeNull());
    const scroll = document.querySelector<HTMLElement>(".ag-scroll")!;
    scroll.scrollTop = 80;
    fireEvent.scroll(scroll);
    fireEvent.click(screen.getByRole("button", { name: "Manager" }));
    fireEvent.click(screen.getByRole("button", { name: "Collapse mid's crew" }));
    expect(rowOf("leaf")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Workspace" }));
    expect(rowOf("leaf")).not.toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Manager" }));
    expect(rowOf("leaf")).toBeNull();
    expect(activeTabMock["team-1|ws-a"]).toBe("lead-tab");
    expect(paneFolds["#manager-fold:team-1|mid"]).toBe(true);
    view.unmount();
    renderList();
    expect(screen.getByRole("button", { name: "Manager" })).toHaveAttribute("aria-pressed", "true");
    expect(rowOf("leaf")).toBeNull();
  });
  it("uses tray epic/ungrouped metadata and scopes one tree without changing assignments; empty scope stays truthful", async () => {
    fleet();
    trayModesMock = { "team-1": "grouped" };
    renderList();
    await waitFor(() => expect(rowOf("leaf")).not.toBeNull());
    expect(screen.getByRole("heading", { name: "Pre Beta Fixes" })).toHaveAttribute(
      "data-group-kind",
      "epic",
    );
    expect(screen.getByRole("heading", { name: "Ungrouped" })).toHaveAttribute(
      "data-group-kind",
      "ungrouped",
    );
    fireEvent.click(screen.getByRole("button", { name: "Manager" }));
    fireEvent.change(screen.getByRole("combobox", { name: "Workspace group scope" }), {
      target: { value: JSON.stringify(["team-1", ""]) },
    });
    expect(rowOf("lead")).not.toBeNull();
    expect(rowOf("mid")).not.toBeNull();
    expect(rowOf("leaf")).not.toBeNull();
    expect(rowOf("solo")).toBeNull();
    expect(rowOf("lead")).toHaveTextContent("Manager context");
    expect(screen.getByTestId("agents-count")).toHaveTextContent("2/6");
    expect(document.querySelectorAll('[data-agent-handle="lead"]')).toHaveLength(1);
    expect(agentIdentity["team-1"].chosen.manager).toBe("lead");
    fireEvent.change(screen.getByRole("combobox", { name: "Workspace group scope" }), {
      target: { value: JSON.stringify(["team-1", "empty-epic"]) },
    });
    expect(screen.getByTestId("agents-no-match")).toBeInTheDocument();
    expect(activeWorkspaceMock["team-1"]).toBe("ws-a");
  });
  it("keeps the visible session anchor when switching a scrolled virtual tree and selecting a worker", async () => {
    fleet();
    paneArrange = "manager";
    scrollAreaPx = 180;
    const workers = Array.from({ length: 40 }, (_, i) => `long-worker-${i}`);
    listResult.push(
      ...workers.map((handle, i) => row({ handle, launchedBy: "lead", startedAtMs: 100 + i })),
    );
    workers.forEach((h, i) =>
      terminalTabIndex["team-1"][i % 2 ? "ws-b" : "ws-c"].push({ id: h, sessionId: `sid-${h}` }),
    );
    const beforeRect = vi
      .mocked(HTMLElement.prototype.getBoundingClientRect)
      .getMockImplementation()!;
    vi.mocked(HTMLElement.prototype.getBoundingClientRect).mockImplementation(function (
      this: HTMLElement,
    ) {
      const r = beforeRect.call(this);
      if (this.dataset.rowKey) {
        const start = Number(this.style.transform.match(/translateY\(([-.\d]+)px\)/)?.[1] ?? 0);
        const top = start - (this.closest<HTMLElement>(".ag-scroll")?.scrollTop ?? 0);
        return { ...r, top, bottom: top + 40, y: top };
      }
      return r;
    });
    const scrolling = vi.spyOn(HTMLElement.prototype, "scrollTo").mockImplementation(function (
      this: HTMLElement,
      options: ScrollToOptions | number,
      y?: number,
    ) {
      this.scrollTop = typeof options === "number" ? (y ?? 0) : (options.top ?? this.scrollTop);
      this.dispatchEvent(new Event("scroll"));
    });
    const height = vi
      .spyOn(HTMLElement.prototype, "scrollHeight", "get")
      .mockImplementation(function (this: HTMLElement) {
        return this.classList.contains("ag-scroll")
          ? parseFloat(this.querySelector<HTMLElement>('[role="list"]')?.style.height ?? "0")
          : 0;
      });
    const client = vi
      .spyOn(HTMLElement.prototype, "clientHeight", "get")
      .mockImplementation(function (this: HTMLElement) {
        return this.classList.contains("ag-scroll") ? scrollAreaPx : 40;
      });
    try {
      renderList();
      await waitFor(() => expect(rowOf("lead")).not.toBeNull());
      const area = document.querySelector<HTMLElement>(".ag-scroll")!;
      act(() => {
        area.scrollTop = 800;
        fireEvent.scroll(area);
      });
      await waitFor(() =>
        expect(document.querySelector('[data-agent-handle="long-worker-20"]')).not.toBeNull(),
      );
      const anchor = [...area.querySelectorAll<HTMLElement>('[data-row-kind="agent"]')].find(
        (el) => el.getBoundingClientRect().bottom > 0,
      )!;
      const handle = anchor.querySelector<HTMLElement>("[data-agent-handle]")!.dataset.agentHandle!;
      const withinTop = anchor.getBoundingClientRect().top;
      for (const name of ["Workspace", "Manager"]) {
        fireEvent.click(screen.getByRole("button", { name }));
        await waitFor(() => expect(rowOf(handle)).not.toBeNull());
        const now = rowOf(handle).closest<HTMLElement>("[data-row-key]")!;
        await waitFor(() => expect(now.getBoundingClientRect().top).toBeCloseTo(withinTop, 0));
      }
      const offset = area.scrollTop;
      fireEvent.click(rowOf(handle));
      expect(area.scrollTop).toBe(offset);
      expect(revealAgentsWorkspace).not.toHaveBeenCalled();
      expect(setActiveTerminalTab).toHaveBeenCalled();
      act(() => {
        area.scrollTop = 1200;
        fireEvent.scroll(area);
      });
      fireEvent.click(screen.getByRole("button", { name: "Run history" }));
      fireEvent.click(screen.getByRole("button", { name: "Back to agents" }));
      await act(async () => {
        await new Promise((resolve) => requestAnimationFrame(resolve));
      });
      expect(document.querySelector<HTMLElement>(".ag-scroll")!.scrollTop).toBe(1200);
    } finally {
      vi.mocked(HTMLElement.prototype.getBoundingClientRect).mockImplementation(beforeRect);
      scrolling.mockRestore();
      height.mockRestore();
      client.mockRestore();
    }
  });
  it("shares custom group names and membership with both fleet views, including empty scopes", async () => {
    fleet();
    trayModesMock = { "team-1": "grouped" };
    customGroupsMock = {
      "team-1": [
        { id: "release", name: "Release checks", workspaceIds: ["ws-b"] },
        { id: "empty", name: "Empty checks", workspaceIds: [] },
        { id: "quiet", name: "Quiet checks", workspaceIds: ["empty"] },
      ],
    };
    railKeysMock = {
      "team-1": {
        "ws-a": "pre-beta-fixes",
        "ws-b": " group:release",
        "ws-c": "",
        empty: " group:quiet",
      },
    };
    const view = renderList();
    await waitFor(() => expect(rowOf("leaf")).not.toBeNull());
    expect(screen.getByRole("heading", { name: "Custom group Release checks" })).toHaveAttribute(
      "data-group-kind",
      "custom",
    );
    expect(screen.getByRole("heading", { name: "Pre Beta Fixes" })).toHaveAttribute(
      "data-group-kind",
      "epic",
    );
    expect(screen.getByRole("heading", { name: "Ungrouped" })).toBeInTheDocument();
    customGroupsMock = {
      "team-1": [
        { ...customGroupsMock["team-1"][0], name: "Ship checks", workspaceIds: ["ws-c"] },
        customGroupsMock["team-1"][1],
        customGroupsMock["team-1"][2],
      ],
    };
    railKeysMock = {
      "team-1": {
        "ws-a": "pre-beta-fixes",
        "ws-b": "pre-beta-fixes",
        "ws-c": " group:release",
        empty: " group:quiet",
      },
    };
    view.rerender(<DockSlotHost teamKey="team-1" visible />);
    expect(
      screen.queryByRole("heading", { name: "Custom group Release checks" }),
    ).not.toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Custom group Ship checks" })).toBeInTheDocument();
    const identities = JSON.stringify(agentIdentity);
    for (const mode of ["Workspace", "Manager"]) {
      fireEvent.click(screen.getByRole("button", { name: mode }));
      const scope = screen.getByRole("combobox", { name: "Workspace group scope" });
      fireEvent.change(scope, { target: { value: JSON.stringify(["team-1", " group:release"]) } });
      expect(scope).toHaveDisplayValue("team-1 · Ship checks (custom group)");
      expect(scope).not.toHaveTextContent("Ship checks (no workspaces)");
      expect(rowOf("leaf")).not.toBeNull();
      fireEvent.change(scope, { target: { value: JSON.stringify(["team-1", " group:quiet"]) } });
      expect(scope).toHaveDisplayValue("team-1 · Quiet checks (custom group)");
      expect(scope).not.toHaveTextContent("Quiet checks (no workspaces)");
      expect(screen.getByTestId("agents-no-match")).toBeInTheDocument();
      fireEvent.change(scope, { target: { value: JSON.stringify(["team-1", " group:empty"]) } });
      expect(screen.getByTestId("agents-no-match")).toBeInTheDocument();
      expect(screen.getByRole("button", { name: /Clear.*filters/ })).toBeInTheDocument();
      fireEvent.change(scope, { target: { value: "" } });
    }
    expect(JSON.stringify(agentIdentity)).toBe(identities);
    expect(customGroupsMock["team-1"][0].workspaceIds).toEqual(["ws-c"]);
    customGroupsMock = {};
    railKeysMock = {
      "team-1": {
        "ws-a": "pre-beta-fixes",
        "ws-b": "pre-beta-fixes",
        "ws-c": "",
        empty: "empty-epic",
      },
    };
    view.rerender(<DockSlotHost teamKey="team-1" visible />);
    fireEvent.click(screen.getByRole("button", { name: "Workspace" }));
    expect(rowOf("solo")).not.toBeNull();
    expect(
      screen.queryByRole("heading", { name: "Custom group Ship checks" }),
    ).not.toBeInTheDocument();
  });
  it("keeps an explicit empty scope after its last workspace moves to another epic", async () => {
    fleet();
    paneArrange = "manager";
    const view = renderList();
    await waitFor(() => expect(rowOf("leaf")).not.toBeNull());
    const scope = screen.getByRole("combobox", { name: "Workspace group scope" });
    fireEvent.change(scope, { target: { value: JSON.stringify(["team-1", "pre-beta-fixes"]) } });
    railKeysMock = {
      "team-1": { "ws-a": "other-epic", "ws-b": "other-epic", "ws-c": "", empty: "empty-epic" },
    };
    view.rerender(<DockSlotHost teamKey="team-1" visible />);
    expect(scope).toHaveDisplayValue("team-1 · Pre Beta Fixes (no workspaces)");
    expect(screen.getByTestId("agents-no-match")).toBeInTheDocument();
    fireEvent.change(scope, { target: { value: "" } });
    expect(rowOf("leaf")).not.toBeNull();
  });
  it("keeps full-graph Manager root order, classification and accents as Working children change", async () => {
    paneArrange = "manager";
    workspaces = { "team-1": [{ id: "ws-a" }, { id: "ws-b" }] };
    listResult = [
      row({ handle: "first-manager", startedAtMs: 200 }),
      row({ handle: "first-worker", launchedBy: "first-manager" }),
      row({ handle: "second-manager", startedAtMs: 100 }),
      row({ handle: "second-worker", launchedBy: "second-manager" }),
      row({ handle: "solo" }),
    ];
    terminalTabIndex = {
      "team-1": {
        "ws-a": ["first-manager", "first-worker", "solo"].map((h) => ({
          id: h,
          sessionId: `sid-${h}`,
        })),
        "ws-b": ["second-manager", "second-worker"].map((h) => ({ id: h, sessionId: `sid-${h}` })),
      },
    };
    hooked("sid-first-manager", "sid-second-manager", "sid-solo");
    renderList();
    await waitFor(() => expect(rowOf("second-worker")).not.toBeNull());
    const handles = () =>
      [
        ...document.querySelectorAll<HTMLElement>('[data-row-kind="agent"] [data-agent-handle]'),
      ].map((el) => el.dataset.agentHandle);
    const accent = (handle: string) =>
      rowOf(handle).closest<HTMLElement>(".ag-row")!.style.getPropertyValue("--ag-guide");
    const firstAccent = accent("first-manager"),
      secondAccent = accent("second-manager");
    expect(firstAccent).not.toBe("");
    expect(secondAccent).not.toBe("");
    fireEvent.click(showOption("Working"));
    expect(handles()).toEqual(["first-manager", "second-manager", "solo"]);
    expect(accent("first-manager")).toBe(firstAccent);
    expect(accent("second-manager")).toBe(secondAccent);
    expect(
      rowOf("second-manager")
        .closest("[data-row-key]")!
        .compareDocumentPosition(screen.getByRole("heading", { name: "No manager" })) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    act(() => hooked("sid-second-worker"));
    expect(handles()).toEqual(["first-manager", "second-manager", "second-worker", "solo"]);
    expect(accent("first-manager")).toBe(firstAccent);
    expect(accent("second-worker")).toBe(secondAccent);
    act(() => hooked("sid-first-worker"));
    expect(handles()).toEqual([
      "first-manager",
      "first-worker",
      "second-manager",
      "second-worker",
      "solo",
    ]);
  });
  it.each(["teams", "manager"] as const)(
    "a chip miss inside a group says Nothing matches in %s view",
    async (arrange) => {
      fleet();
      paneArrange = arrange;
      listResult = listResult.map((r) =>
        r.handle === "lead" ? { ...r, agentRevision: "acme/backend/triage@3" } : r,
      );
      agentsListFilter = { agentKey: "acme/backend/triage", name: "Triage" };
      renderList();
      await waitFor(() => expect(rowOf("lead")).not.toBeNull());
      fireEvent.change(screen.getByRole("combobox", { name: "Workspace group scope" }), {
        target: { value: JSON.stringify(["team-1", "empty-epic"]) },
      });
      expect(screen.queryByText("No Triage sessions running")).toBeNull();
      const note = screen.getByTestId("agents-no-match");
      fireEvent.click(
        within(note).getByRole("button", { name: "Clear search, filters and teams" }),
      );
      expect(rowOf("lead")).not.toBeNull();
    },
  );
  it.each(["teams", "manager"] as const)(
    "a status miss inside a group stays scoped in %s view",
    async (arrange) => {
      fleet();
      paneArrange = arrange;
      paneSearch = { query: "", status: "working" };
      hooked("sid-lead");
      renderList();
      await waitFor(() => expect(rowOf("lead")).not.toBeNull());
      fireEvent.change(screen.getByRole("combobox", { name: "Workspace group scope" }), {
        target: { value: JSON.stringify(["team-1", "empty-epic"]) },
      });
      expect(screen.queryByTestId("agents-show-empty")).toBeNull();
      expect(screen.getByTestId("agents-no-match")).toBeInTheDocument();
    },
  );
  it.each([false, true])(
    "open teams keep their workspace-group choices without agents (last agent exits=%s)",
    async (exits) => {
      fleet();
      paneArrange = "manager";
      openedTeamsMock = ["team-1", "acme:empty"];
      workspaces["acme:empty"] = [{ id: "empty-ws", kind: "story", storyId: "EMPTY-2" }];
      railKeysMock["acme:empty"] = { "empty-ws": "release-checks" };
      if (exits) {
        listResult.push(row({ handle: "empty-agent" }));
        terminalTabIndex["acme:empty"] = {
          "empty-ws": [{ id: "empty-tab", sessionId: "sid-empty-agent" }],
        };
      }
      const view = renderList();
      await waitFor(() => expect(rowOf("lead")).not.toBeNull());
      const scope = screen.getByRole("combobox", { name: "Workspace group scope" });
      expect(
        within(scope).getByRole("option", { name: "empty · Release Checks" }),
      ).toBeInTheDocument();
      fireEvent.change(scope, {
        target: { value: JSON.stringify(["acme:empty", "release-checks"]) },
      });
      if (exits) {
        terminalTabIndex["acme:empty"] = {};
        act(() =>
          publishProjectionOwners({
            sessions: listResult.filter((r) => r.handle !== "empty-agent"),
            error: null,
          }),
        );
        view.rerender(<DockSlotHost teamKey="team-1" visible />);
      }
      expect(scope).toHaveDisplayValue("empty · Release Checks");
      expect(screen.getByTestId("agents-no-match")).toBeInTheDocument();
      expect(scope).not.toHaveTextContent("no workspaces");
      terminalTabIndex = {};
      act(() => publishProjectionOwners({ sessions: [], error: null }));
      view.rerender(<DockSlotHost teamKey="team-1" visible />);
      expect(scope).toHaveDisplayValue("empty · Release Checks");
      expect(screen.getByTestId("agents-no-match")).toBeInTheDocument();
      expect(activeWorkspaceMock["team-1"]).toBe("ws-a");
    },
  );
  it("opens native count details in the parent's Quick view without fabricating a child session", async () => {
    fleet();
    paneArrange = "manager";
    useAgentStateStore.getState().recordActivity(
      {
        session: "sid-mid",
        identity: { hostBootId: "boot", sessionIncarnation: "pty", invocationId: "run" },
        revision: 1,
        turnGeneration: 1,
        provider: "claude",
        state: "working",
        observed: true,
        limited: false,
        exit: null,
        asks: [],
        toolOpen: true,
        activityAgeMs: 0,
        subagentsOpen: 2,
      },
      Infinity,
    );
    projectSessionStatus("sid-mid", "tool-running", { subagents: 2 });
    renderList();
    fireEvent.click(
      await screen.findByRole("button", { name: "2 native subagents working · Quick view of mid" }),
    );
    const popup = await screen.findByRole("dialog", { name: "Quick view of mid" });
    expect(within(popup).getByRole("img", { name: "2 subagents running" })).toHaveTextContent("2");
    expect(within(popup).queryByRole("button", { name: /Stop/ })).toBeNull();
    expect(document.querySelectorAll('[data-agent-handle="mid"]')).toHaveLength(1);
    expect(activeTabMock["team-1|ws-a"]).toBe("lead-tab");
  });
});

describe("a session says when it is typed by hand or its release is not supported", () => {
  const cliRows = REGISTERED_AGENT_CLIS.map((cli) => ({
    runtime: cli.runtime,
    label: agentCliLabel(cli.runtime),
  }));
  /** A notice line of `handle`'s row: its own line under the row's button, never inside it. */
  const noticeOf = (handle: string) =>
    document.querySelector<HTMLElement>(`[data-agent-handle="${handle}"] .ag-notes`);
  const handTab = (runtime: string, over: Partial<AgentSessionRow> = {}) => {
    workspaces = { "team-1": [{ id: "ws-a", kind: "generic", status: "ready", cwd: "/ws" }] };
    terminalTabIndex = { "team-1": { "ws-a": [{ id: "tab-hand", sessionId: "pty-hand" }] } };
    listResult = [
      row({
        handle: "shell-3",
        sessionId: "pty-hand",
        kind: "hand-started",
        rowNames: { primary: "shell-3", secondary: null, full: "shell-3" },
        detected: { cli: runtime, dot: null, processOnly: true },
        inboxWaiter: false,
        unread: 2,
        ...over,
      }),
    ];
  };
  const showHand = async (runtime: string) => {
    renderList();
    await waitFor(() => expect(rowOf("shell-3")).toBeTruthy());
    act(() => useAgentStateStore.getState().setProcessAgent("pty-hand", runtime));
    return rowOf("shell-3");
  };

  describe.each(cliRows)("typed by hand: $label", ({ runtime, label }) => {
    it("keeps its status word, says the mail waits unwoken, and offers Start managed", async () => {
      handTab(runtime);
      const main = await showHand(runtime);
      expect(main.querySelector(".ag-task")).toHaveTextContent("Activity unknown");
      const notice = noticeOf("shell-3");
      expect(notice).toHaveTextContent("Typed by hand · 2 unread, not woken · Start managed");
      expect(main.contains(notice)).toBe(false);
      const title = main.getAttribute("title") ?? "";
      expect(title).toContain("Typed by hand, so CodeSense doesn't manage it.");
      expect(title).toContain(
        "Its 2 unread messages are stored and readable here, but nothing wakes it.",
      );
      expect(title).toContain(
        `Start managed ${label} opens a new session with inbox wake and activity; this tab and its mail stay as they are.`,
      );
      const opened = vi.fn();
      window.addEventListener(NEW_WORKSPACE_TERMINAL_EVENT, opened);
      fireEvent.click(within(notice!).getByRole("button", { name: `Start managed ${label}` }));
      await waitFor(() => expect(opened).toHaveBeenCalled());
      window.removeEventListener(NEW_WORKSPACE_TERMINAL_EVENT, opened);
      expect((opened.mock.calls[0]![0] as CustomEvent).detail).toEqual({
        teamKey: "team-1",
        workspaceId: "ws-a",
        launch: runtime,
        cwd: "/work/pty-hand",
        machineId: "local",
      });
    });

    it("counts one unread message, and none", async () => {
      handTab(runtime, { unread: 1 });
      const main = await showHand(runtime);
      expect(noticeOf("shell-3")).toHaveTextContent("Typed by hand · 1 unread, not woken");
      expect(main.getAttribute("title")).toContain(
        "Its 1 unread message is stored and readable here, but nothing wakes it.",
      );
    });

    it("with no unread mail says only that it is not woken", async () => {
      handTab(runtime, { unread: 0 });
      const main = await showHand(runtime);
      expect(noticeOf("shell-3")).toHaveTextContent("Typed by hand · not woken · Start managed");
      expect(main.getAttribute("title")).toContain(
        "Mail sent to it is stored and readable, but nothing wakes it.",
      );
    });

    it.each([true, "unknown" as const])(
      "says nothing extra when its waiter is %s, its line one included",
      async (inboxWaiter) => {
        handTab(runtime, { inboxWaiter });
        const main = await showHand(runtime);
        expect(noticeOf("shell-3")).toBeNull();
        expect(main.querySelector(".ag-task")).toBeNull();
        expect(main.getAttribute("title")).not.toContain("Typed by hand");
      },
    );

    it("Start managed shows the tab's workspace first, so its chooser can open", async () => {
      handTab(runtime);
      // The hand-typed tab is in a workspace that is not the one on screen.
      workspaces = {
        "team-1": [
          { id: "ws-a", kind: "generic", status: "ready", cwd: "/ws" },
          { id: "ws-b", kind: "generic", status: "ready", cwd: "/other" },
        ],
      };
      terminalTabIndex = {
        "team-1": {
          "ws-a": [{ id: "tab-other", sessionId: "pty-other" }],
          "ws-b": [{ id: "tab-hand", sessionId: "pty-hand" }],
        },
      };
      activeWorkspaceMock = { "team-1": "ws-a" };
      await showHand(runtime);
      const opened = vi.fn();
      window.addEventListener(NEW_WORKSPACE_TERMINAL_EVENT, opened);
      fireEvent.click(
        within(noticeOf("shell-3")!).getByRole("button", { name: `Start managed ${label}` }),
      );
      await waitFor(() => expect(opened).toHaveBeenCalled());
      window.removeEventListener(NEW_WORKSPACE_TERMINAL_EVENT, opened);
      // The row's own reveal shows the tab's workspace, where its chooser mounts.
      expect(activateTrayWorkspace).toHaveBeenCalledWith("team-1", "ws-b");
      expect(setActiveTerminalTab).toHaveBeenCalledWith("team-1", "tab-hand", "ws-b");
      expect((opened.mock.calls[0]![0] as CustomEvent).detail).toMatchObject({
        workspaceId: "ws-b",
        launch: runtime,
      });
    });

    it("Start managed says so when the tab can no longer be found", async () => {
      handTab(runtime);
      await showHand(runtime);
      terminalTabIndex = {};
      const opened = vi.fn();
      window.addEventListener(NEW_WORKSPACE_TERMINAL_EVENT, opened);
      fireEvent.click(
        within(noticeOf("shell-3")!).getByRole("button", { name: `Start managed ${label}` }),
      );
      await waitFor(() => expect(toast.error).toHaveBeenCalled());
      window.removeEventListener(NEW_WORKSPACE_TERMINAL_EVENT, opened);
      expect(opened).not.toHaveBeenCalled();
    });
  });

  describe.each(cliRows)("release not supported: $label", ({ runtime, label }) => {
    const note =
      "This CLI version (0.1.0) isn't supported yet. Status, inbox wake and usage may not work.";
    const launched = (over: Partial<AgentSessionRow> = {}) => {
      listResult = [
        row({ handle: "reviewer-2", releaseQualified: false, releaseNote: note, ...over }),
      ];
      useAgentStateStore.setState({
        launchBySession: {
          "sid-reviewer-2": { line: runtime, agent: runtime, spec: {} as never },
        },
      });
    };

    it("says so while it works, quotes the launch warning, and checks the version", async () => {
      launched();
      projectSessionStatus("sid-reviewer-2", "thinking");
      renderList();
      await waitFor(() => expect(rowOf("reviewer-2")).toBeTruthy());
      const main = rowOf("reviewer-2");
      expect(main.querySelector(".ag-task")).toHaveTextContent("Thinking");
      const notice = noticeOf("reviewer-2");
      expect(notice).toHaveTextContent("Release not supported · Check version");
      const title = main.getAttribute("title") ?? "";
      expect(title).toContain(
        `${note}\nMail is stored and readable. Check version opens Settings → CLIs, which shows the installed version.`,
      );
      fireEvent.click(within(notice!).getByRole("button", { name: `Check ${label} version` }));
      // It lands on the CLIs list itself, not the top of General.
      expect(openSettingsTab).toHaveBeenCalledWith("general", "agentClis");
    });

    it("on another machine keeps the notice but not the action", async () => {
      launched({ machineId: "studio-mini" });
      projectSessionStatus("sid-reviewer-2", "idle");
      useMachinesStore.setState({
        byTeam: {
          "team-1": [
            {
              machineId: "studio-mini",
              label: "studio-mini",
              kind: "ssh",
              status: { state: "connected" },
            },
          ],
        },
      } as never);
      renderList();
      await waitFor(() => expect(rowOf("reviewer-2")).toBeTruthy());
      const notice = noticeOf("reviewer-2");
      expect(notice).toHaveTextContent("Release not supported");
      expect(within(notice!).queryByRole("button")).toBeNull();
    });
  });
});

describe("line one keeps its status word beside the hover actions", () => {
  it("the name gives way first; the word holds a floor its own length", async () => {
    listResult = [
      row({
        handle: "a-rather-long-agent-name",
        sessionId: "sid-long",
        kind: "hand-started",
        inboxWaiter: false,
        effectiveStatus: "unknown",
      }),
    ];
    renderList();
    await waitFor(() => expect(rowOf("a-rather-long-agent-name")).toBeTruthy());
    const title = rowOf("a-rather-long-agent-name").querySelector(".ag-title-line")!;
    const task = title.querySelector<HTMLElement>(".ag-title-task")!;
    expect(task).toHaveTextContent("Activity unknown");
    // The word is its own part that never shrinks; only the name and a prompt truncate.
    expect(task.querySelector(".ag-title-word")).toHaveTextContent("Activity unknown");
    const css = readFileSync(resolve(__dirname, "../../../styles/agents.css"), "utf-8");
    expect(css).toMatch(/\.ag-title-word \{\s*flex: none;/);
    expect(css).toMatch(/\.ag-title-name \{[^}]*flex: 0 1000 auto;[^}]*min-width: 3ch;/);
    // The prompt takes only the room left and truncates; the real-layout check is tests/e2e-mock.
    expect(css).toMatch(/\.ag-title-task \{\s*display: contents;/);
    expect(css).toMatch(/\.ag-title-more \{[^}]*flex: 1 1 0;[^}]*min-width: 0;/);
    // A narrow row's hover actions take line two's end, not line one's room.
    expect(css).toMatch(/@container ag-row \(max-width: 300px\)/);
  });
});

describe("line one with a launch prompt", () => {
  it("puts the prompt after the word, in its own part; a prompt-only line has no word part", async () => {
    listResult = [
      row({ handle: "briefed", sessionId: "sid-briefed", startedWith: "Refactor the auth module" }),
      row({
        handle: "quiet-brief",
        sessionId: "sid-quiet-brief",
        startedWith: "Sweep the support channel",
        effectiveStatus: "unknown",
      }),
    ];
    projectSessionStatus("sid-briefed", "thinking");
    renderList();
    await waitFor(() => expect(rowOf("briefed")).toBeTruthy());
    const briefed = rowOf("briefed");
    expect(briefed.querySelector(".ag-title-word")).toHaveTextContent("Thinking");
    expect(briefed.querySelector(".ag-title-more")).toHaveTextContent("· Refactor the auth module");
    // The whole task still reads as one part of line one.
    expect(briefed.querySelector(".ag-task")).toHaveTextContent(
      "Thinking · Refactor the auth module",
    );
    const quiet = rowOf("quiet-brief");
    expect(quiet.querySelector(".ag-title-word")).toBeNull();
    expect(quiet.querySelector(".ag-title-more")).toHaveTextContent("Sweep the support channel");
  });
});
