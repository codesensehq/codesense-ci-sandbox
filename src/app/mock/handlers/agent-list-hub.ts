// SPDX-License-Identifier: MIT
import type { HubRow, HubSnapshot } from "@/features/dashboard/lib/live-hub";
import { NEEDS_YOU_STATUSES, WORKING_STATUSES } from "@/shared/lib/agent-status-config";
import { SESSION_STATE_CHANGED_EVENT } from "@/features/viewer/lib/session-events";
import type { Workspace } from "@/shared/stores/viewer-store";
import { AGENTS_CHANGED_EVENT, AGENT_INBOX_CHANGED_EVENT } from "@/tauri/events";
import type { AgentSessionRow, SpendCapView } from "@/tauri/commands";
import type { MockEventBus } from "../mock-events";
import type { CommandHandler } from "../mock-router";
import {
  AGENT_TEAM_ASK,
  AGENT_TEAM_STORY_WORKSPACES,
  AGENT_TEAM_SUBAGENTS,
  SECOND_TEAM_STATUSES,
  SECOND_TEAM_KEY,
  SECOND_TEAM_TABS,
  SECOND_TEAM_WORKSPACES,
  TEAM_KEY,
  agentTeamTabs,
  isAgentTeamsWorld,
} from "./agents-team";

const VIEW = "all";
const STATUS_VIEW = "all|status";

/** The viewer's last pushed snapshot (`push_viewer_state`): the workspaces it holds and the order
 *  it resolved for them, which the backend's projection reads. */
let pushedViewer: {
  workspaces?: Record<string, Workspace[]>;
  softClosedWorkspaces?: Record<string, Record<string, { workspace: Workspace }>>;
  resolvedWorkspaceOrderByTeam?: Record<string, string[]>;
} = {};

/** The mock's `push_viewer_state`: keeps the snapshot and tells the hub to project again. */
export function pushViewerStateHandler(events: MockEventBus): CommandHandler {
  return (args) => {
    pushedViewer = (args.snapshot ?? {}) as typeof pushedViewer;
    events.emit(VIEWER_STATE_PUSHED_EVENT, null);
    return undefined;
  };
}

const VIEWER_STATE_PUSHED_EVENT = "mock:viewer-state-pushed";

const idOf = (team: string, handle: string) => JSON.stringify([team, handle]);

/** The team and workspace a session's tab sits in. */
export type Placement = { team: string; workspaceId: string };

type Placed = {
  row: AgentSessionRow;
  team: string;
  workspace: Workspace | undefined;
  /** The session this one was launched by, when that session is listed in the same team. */
  parent: Placed | undefined;
  children: Placed[];
};

function workspaceRef(ws: Workspace) {
  return {
    id: ws.id,
    label: (ws.kind === "story" && ws.storyId) || ws.storyTitle || ws.id.replace(/^generic:/, ""),
    kind: ws.kind === "story" ? "story" : "folder",
    ...(ws.kind === "story" ? { storyStatus: ws.storyStatus, storyTitle: ws.storyTitle } : {}),
  };
}

/** Roots first, depth-first, each node's children in launch order. */
function depthFirst(nodes: readonly Placed[]): Placed[] {
  return nodes.flatMap((node) => [node, ...depthFirst(node.children)]);
}

/** Only live sessions are listed (`tree.rs` `build_tree`); the `sessions` topic keeps the rest. */
const listed = (sessions: readonly AgentSessionRow[]) => sessions.filter((row) => row.alive);

/**
 * Link each session to the session that launched it, among `placed`, the way `tree.rs`
 * `launch_tree` does: newest first (handle breaks ties), roots with children ahead of childless
 * roots. Returns the roots.
 */
function launchTree(placed: readonly Placed[]): Placed[] {
  const linked = placed
    .map((p) => ({ ...p, parent: undefined as Placed | undefined, children: [] as Placed[] }))
    .sort(
      (a, b) =>
        (b.row.startedAtMs ?? 0) - (a.row.startedAtMs ?? 0) ||
        (a.row.handle < b.row.handle ? -1 : a.row.handle > b.row.handle ? 1 : 0),
    );
  const byHandle = new Map(linked.map((p) => [p.row.handle, p]));
  for (const p of linked) {
    const launcher = p.row.launchedBy ? byHandle.get(p.row.launchedBy) : undefined;
    if (!launcher || launcher === p) continue;
    p.parent = launcher;
    launcher.children.push(p);
  }
  const roots = linked.filter((p) => !p.parent);
  return [...roots.filter((p) => p.children.length), ...roots.filter((p) => !p.children.length)];
}

/**
 * The rows the Rust `agentList` projection serves, for the mock team worlds: where each session
 * sits (team, workspace), its place in the launch tree, and its workspace's crew (the same
 * launch tree, within one workspace, led by the root with the most agents under it).
 */
export function agentListRows(
  sessions: readonly AgentSessionRow[],
  placements: ReadonlyMap<string, { team: string; workspace: Workspace | undefined }>,
  teamOrder: readonly string[],
  workspaceOrder: Readonly<Record<string, readonly string[]>> = {},
): HubRow[] {
  const placed: Placed[] = listed(sessions).map((row) => ({
    row,
    team: placements.get(row.sessionId ?? "")?.team ?? teamOrder[0],
    workspace: placements.get(row.sessionId ?? "")?.workspace,
    parent: undefined,
    children: [],
  }));
  const rows = new Map<string, Record<string, unknown>>();
  let order = 0;
  for (const team of teamOrder) {
    const members = placed.filter((p) => p.team === team);
    const workspaces: string[] = [];
    for (const p of members)
      if (p.workspace && !workspaces.includes(p.workspace.id)) workspaces.push(p.workspace.id);
    // The cards follow the order the viewer keeps its workspaces in.
    const kept = workspaceOrder[team] ?? [];
    const rank = (id: string) => (kept.includes(id) ? kept.indexOf(id) : kept.length);
    workspaces.sort((a, b) => rank(a) - rank(b));
    for (const node of depthFirst(launchTree(members))) {
      const { row, workspace } = node;
      const launcher = node.parent;
      rows.set(idOf(team, row.handle), {
        id: idOf(team, row.handle),
        sessionRef: row.sessionId ?? row.handle,
        inboxRef: `counts:team:${team.replace(":", "/")}`,
        teamKey: team,
        placementTeamRef: null,
        parentId: launcher ? idOf(team, launcher.row.handle) : null,
        parentSource: launcher ? "launcher" : null,
        order: order++,
        workspaceRef: workspace?.id ?? null,
        ...(workspace ? { workspace: workspaceRef(workspace) } : {}),
        storyRef: workspace?.kind === "story" ? workspace.id : null,
        workspaceRank: Math.max(0, workspaces.indexOf(workspace?.id ?? "")),
        crewParentId: null,
        crewOrder: null,
        crewLead: false,
      });
    }
    for (const id of workspaces) {
      const crew = launchTree(members.filter((p) => p.workspace?.id === id));
      const lead = crew.reduce<Placed | undefined>(
        (best, node) =>
          !best || depthFirst([node]).length > depthFirst([best]).length ? node : best,
        undefined,
      );
      depthFirst(crew).forEach((node, index) => {
        Object.assign(rows.get(idOf(team, node.row.handle))!, {
          crewParentId: node.parent ? idOf(team, node.parent.row.handle) : null,
          crewOrder: index,
          crewLead: node === lead,
        });
      });
    }
  }
  return [...rows.values()] as HubRow[];
}

/** `views.rs` `rank`: 0 waits on a person, 1 at work, 2 idle, 3 when the session is unknown. */
const statusRank = (row: AgentSessionRow | undefined) =>
  !row?.effectiveStatus
    ? 3
    : NEEDS_YOU_STATUSES.includes(row.effectiveStatus)
      ? 0
      : WORKING_STATUSES.includes(row.effectiveStatus)
        ? 1
        : 2;

/** `views.rs` `compare_handles`: punctuation, digits, then case-folded letters; case only breaks
 *  a complete primary tie, lowercase first. */
const PUNCTUATION = " _-./#";
const primary = (c: string): [number, number] =>
  PUNCTUATION.includes(c)
    ? [0, PUNCTUATION.indexOf(c)]
    : /[0-9]/.test(c)
      ? [1, c.charCodeAt(0)]
      : /[a-z]/i.test(c)
        ? [2, c.toLowerCase().charCodeAt(0)]
        : [3, c.charCodeAt(0)];
function compareHandles(left: string, right: string) {
  const [a, b] = [[...left].map(primary), [...right].map(primary)];
  for (let i = 0; i < Math.min(a.length, b.length); i++)
    if (a[i][0] !== b[i][0] || a[i][1] !== b[i][1]) return a[i][0] - b[i][0] || a[i][1] - b[i][1];
  if (a.length !== b.length) return a.length - b.length;
  const upper = (h: string) => [...h].map((c) => (/[A-Z]/.test(c) ? 1 : 0));
  const [ua, ub] = [upper(left), upper(right)];
  for (let i = 0; i < ua.length; i++) if (ua[i] !== ub[i]) return ua[i] - ub[i];
  return left < right ? -1 : left > right ? 1 : 0;
}

/**
 * The `all|status` view (`views.rs` `replace_status`): the same rows ranked by their session's
 * status, then unread mail (most first), then handle, each carrying its `statusGroup` and its
 * place in that order. `unread` counts the unread messages to each handle.
 */
export function agentListStatusRows(
  rows: readonly HubRow[],
  sessions: readonly AgentSessionRow[],
  unread: Readonly<Record<string, number>> = {},
): HubRow[] {
  const unreadOf = (row?: AgentSessionRow) => unread[row?.handle ?? ""] ?? 0;
  const session = (row: HubRow) =>
    sessions.find((s) => (s.sessionId ?? s.handle) === row.sessionRef);
  return rows
    .map((row) => ({ row, session: session(row) }))
    .sort(
      (a, b) =>
        statusRank(a.session) - statusRank(b.session) ||
        unreadOf(b.session) - unreadOf(a.session) ||
        compareHandles(a.session?.handle ?? "", b.session?.handle ?? ""),
    )
    .map(({ row, session: s }, order) => ({ ...row, order, statusGroup: statusRank(s) }));
}

/** What a mock world tells the projection: its sessions, where they sit, and how they read. */
export interface AgentListWorld {
  sessions: () => AgentSessionRow[];
  /** The team and workspace each session's tab sits in, by session id. */
  placements: ReadonlyMap<string, Placement>;
  /** The world's own workspace records, for any the viewer has not pushed yet. */
  workspaces: Readonly<Record<string, readonly Workspace[]>>;
  teamOrder: readonly string[];
  /** What the backend reads of a session's hooks, asks and subagents into its status. */
  statusFields: (row: AgentSessionRow) => Partial<AgentSessionRow>;
  /** Unread messages to each session handle, which order the Status view within a group. */
  unread?: () => Readonly<Record<string, number>>;
  /** Each scope's spend (`agent:<key>`, `routine:<key>`), served as `spend` rows. */
  spend?: () => Readonly<Record<string, SpendCapView>>;
}

const split = (team: string) => {
  const [org, name] = [team.slice(0, team.indexOf(":")), team.slice(team.indexOf(":") + 1)];
  return { org, team: name };
};

/**
 * `sessions` and `agentList` for a mock world, through the same live-hub snapshot and delta path
 * the app reads in production. Later session changes (`window.__mockAgents.addSession`) arrive
 * as deltas.
 */
export function agentListHub(events: MockEventBus, world: AgentListWorld) {
  /** Each session's place, its workspace as the viewer holds it (open or soft-closed). */
  const resolved = () =>
    new Map(
      [...world.placements].map(([session, { team, workspaceId }]) => [
        session,
        {
          team,
          workspace:
            pushedViewer.workspaces?.[team]?.find((ws) => ws.id === workspaceId) ??
            pushedViewer.softClosedWorkspaces?.[team]?.[workspaceId]?.workspace ??
            world.workspaces[team]?.find((ws) => ws.id === workspaceId),
        },
      ]),
    );
  const project = (): Record<string, HubRow[]> => {
    const sessions = world.sessions();
    const placements = resolved();
    const sessionRows = sessions.map((row) => ({
      ...row,
      hasAgent: true,
      ...world.statusFields(row),
      id: row.sessionId ?? row.handle,
      team: split(placements.get(row.sessionId ?? "")?.team ?? world.teamOrder[0]),
    }));
    const rows = agentListRows(
      sessions,
      placements,
      world.teamOrder,
      pushedViewer.resolvedWorkspaceOrderByTeam,
    );
    const spendRows = Object.entries(world.spend?.() ?? {}).map(([scope, spend]) => ({
      id: `agentList:spend:${scope}`,
      rowType: "spend",
      scopeKind: scope.slice(0, scope.indexOf(":")),
      key: scope.slice(scope.indexOf(":") + 1),
      spend: spend as unknown as HubRow,
      order: Number.MAX_VALUE,
    }));
    const state = {
      id: "agentList:state",
      rowType: "state",
      ready: true,
      homeTeam: world.teamOrder[0],
      teamOrder: world.teamOrder,
      order: Number.MAX_VALUE,
    };
    return {
      "sessions|all": sessionRows,
      "agentList|all": [...rows, ...spendRows, state],
      [`agentList|${STATUS_VIEW}`]: [
        ...agentListStatusRows(rows, sessionRows, world.unread?.()),
        state,
      ],
    };
  };
  const rev: Record<string, number> = {};
  let current = project();
  const refresh = () => {
    const next = project();
    for (const key of Object.keys(next)) {
      const [topic, ...view] = key.split("|");
      const before = new Map((current[key] ?? []).map((row) => [row.id, JSON.stringify(row)]));
      const upserts = next[key].filter((row) => before.get(row.id) !== JSON.stringify(row));
      const removes = (current[key] ?? [])
        .filter((row) => !next[key].some((n) => n.id === row.id))
        .map((row) => row.id);
      if (!upserts.length && !removes.length) continue;
      events.emit("codesense:delta", {
        topic,
        viewKey: view.join("|"),
        rev: (rev[key] = (rev[key] ?? 0) + 1),
        upserts,
        removes,
      });
    }
    current = next;
  };
  for (const event of [
    SESSION_STATE_CHANGED_EVENT,
    AGENTS_CHANGED_EVENT,
    "pty-exit",
    AGENT_INBOX_CHANGED_EVENT,
    VIEWER_STATE_PUSHED_EVENT,
  ])
    events.observe(event, refresh);

  return {
    snapshot: (topic: string, viewKey: string): HubSnapshot | undefined => {
      // `sessions` has the one view; `agentList` serves `all` and `all|status`.
      const key = `${topic}|${topic === "agentList" && viewKey === STATUS_VIEW ? viewKey : VIEW}`;
      const rows = current[key];
      return rows
        ? { topic, viewKey, rev: rev[key] ?? 0, snapshot: rows, total: rows.length }
        : undefined;
    },
  };
}

/** Sessions of `tabs` placed into their workspaces of `team`. */
export function placeTabs(
  placements: Map<string, Placement>,
  team: string,
  tabs: readonly { workspaceId: string; session: string }[],
) {
  for (const tab of tabs) placements.set(tab.session, { team, workspaceId: tab.workspaceId });
}

/** The `?agents=team` and `?agents=teams` worlds: sessions join their workspace by the tab that
 *  holds them. */
export function agentTeamListWorld(
  search: string,
  sessions: () => AgentSessionRow[],
): AgentListWorld {
  const secondTeam = isAgentTeamsWorld(search);
  const placements = new Map<string, Placement>();
  placeTabs(placements, TEAM_KEY, agentTeamTabs(search));
  if (secondTeam) placeTabs(placements, SECOND_TEAM_KEY, SECOND_TEAM_TABS);
  return {
    sessions,
    placements,
    workspaces: {
      [TEAM_KEY]: AGENT_TEAM_STORY_WORKSPACES as Workspace[],
      [SECOND_TEAM_KEY]: SECOND_TEAM_WORKSPACES as Workspace[],
    },
    teamOrder: secondTeam ? [TEAM_KEY, SECOND_TEAM_KEY] : [TEAM_KEY],
    statusFields: (row) => {
      const subagents = AGENT_TEAM_SUBAGENTS.find(
        (a) => a.session === row.sessionId && (secondTeam || !a.secondTeam),
      )?.subagents;
      const hook = secondTeam
        ? SECOND_TEAM_STATUSES.find((h) => h.tab_session_id === row.sessionId)?.status
        : undefined;
      const effectiveStatus = !row.alive
        ? "completed"
        : row.sessionId === AGENT_TEAM_ASK.sessionId
          ? "awaiting-answer"
          : subagents || hook === "running"
            ? "tool-running"
            : hook === "done"
              ? "idle"
              : row.status === "paused"
                ? "paused"
                : "running";
      return {
        effectiveStatus: row.effectiveStatus ?? effectiveStatus,
        ...(subagents ? { subagents } : {}),
      };
    },
  };
}
