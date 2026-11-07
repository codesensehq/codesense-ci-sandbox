// SPDX-License-Identifier: MIT
/**
 * The agents pane's one pure plan: which teams show, which agents a search, Show filter, team
 * scope, workspace arrow and agent chip leave in view, and the header's visible/total counts. The
 * view renders this plan as is; these filters are the only narrowing.
 */

import { flattenNodes, NO_WORKSPACE_ID } from "@/features/agents/lib/agent-list-layout";
import { agentKeyOfRevision } from "@/features/viewer/lib/agent-rows";
import {
  countNodes,
  filteredRowKeys,
  passesStatusFilter,
  type AgentStatusFilter,
  type AgentTreeNode,
} from "@/features/viewer/lib/agent-list-view";
import { agentRowKey } from "@/shared/lib/agent-pins";
import type { AgentStatus } from "@/shared/lib/agent-status-config";
import type { AgentSessionRow } from "@/tauri/commands";

/** One team's agents as the launch trees the model built for it. */
export interface PaneTeam {
  teamKey: string;
  nodes: readonly AgentTreeNode[];
}

/** The arrow filter: one workspace of one team, and the remote machine it runs on, when it
 *  does. */
export interface PaneWorkspaceFilter {
  teamKey: string;
  workspaceId: string;
  machineId?: string;
}

export interface PaneRailFilter {
  teamKey: string;
  railKey: string;
}

export interface PaneFilter {
  rail?: PaneRailFilter | null;
  query: string;
  status: AgentStatusFilter;
  workspace: PaneWorkspaceFilter | null;
  /** The agent chip: only sessions of this saved agent, by its key. */
  agentKey?: string | null;
}

export interface PaneRules {
  manager?: boolean;
  railKeyOfWorkspace?: (teamKey: string, workspaceId: string) => string | undefined;
  statusOf: (row: AgentSessionRow) => AgentStatus;
  textOf: (node: AgentTreeNode) => string;
}

/** One team's rows that stay in view, flattened: the pane shows team › workspace › agent. */
export interface PlannedTeam {
  teamKey: string;
  rows: AgentTreeNode[];
  /** Each launched agent's launcher in the team's launch tree, by handle (a top-level agent has
   *  none). The rows decide the indent from it, within the group they are drawn in. */
  launchers: Record<string, string>;
}

export interface PanePlan {
  /** The visible teams, in the order given, each with its rows that stay in view. */
  teams: PlannedTeam[];
  /** The agents a search or Show filter keeps, by `agentRowKey` (one handle can run in two
   *  teams), or null when neither narrows the list. */
  keep: ReadonlySet<string> | null;
  /** Every listed agent in the teams in view (a hidden team's are not counted), before any
   *  search, Show filter, workspace arrow or agent chip. The filter
   *  menu's All ({@link PanePlan.counts}) counts after the chip and the arrow, so it equals this
   *  only while neither is on. */
  total: number;
  /** Every listed agent, a hidden team's included: the header's "of" count, so a pane with a team
   *  hidden reads visible/all. */
  everyTotal: number;
  /** Agents in view after every filter. */
  visible: number;
  /** Whether a search, Show, workspace or agent filter narrows the teams in view, so the count
   *  shows "visible/total". */
  filtering: boolean;
  /** Whether folds are frozen: only a typed search or a workspace chip freezes them, and opens
   *  every group they keep a match in. A Show filter or a hidden team leaves folds working, so a
   *  folded group stays folded under it. */
  freezesFolds: boolean;
  /** Show-filter counts over the teams in view, after the agent chip and the workspace arrow: a
   *  bucket counts the rows it would show, so with no search typed the bucket in use counts
   *  {@link PanePlan.visible}. With neither chip nor arrow on, All equals {@link PanePlan.total}. */
  counts: Record<AgentStatusFilter, number>;
  /** The hidden teams the agent chip's agent runs in, when the chip is on. */
  chipHiddenTeams: string[];
}

/** Each launched agent's launcher in a launch tree, by handle. */
function launchersOf(
  nodes: readonly AgentTreeNode[],
  out: Record<string, string> = {},
): Record<string, string> {
  for (const node of nodes) {
    for (const child of node.children) out[child.row.handle] = node.row.handle;
    launchersOf(node.children, out);
  }
  return out;
}

/** Plan the pane for one render. Hidden teams drop out whole; a workspace filter keeps only its
 *  workspace's rows, and only in its own team; the agent chip keeps only its agent's sessions. */
export function planAgentsPane(
  teams: readonly PaneTeam[],
  hiddenTeams: readonly string[],
  filter: PaneFilter,
  rules: PaneRules,
): PanePlan {
  // A hidden team whose agents have all ended is not on the pane: the hidden list counts only
  // teams the pane still shows.
  const hidden = hiddenTeams.filter((key) => teams.some((t) => t.teamKey === key));
  const visibleTeams = teams.filter((t) => !hidden.includes(t.teamKey));
  const total = countNodes(visibleTeams.flatMap((t) => t.nodes));
  const everyTotal = countNodes(teams.flatMap((t) => t.nodes));
  const searching = filter.query.trim() !== "" || filter.status !== "all";
  const keep = searching ? new Set<string>() : null;
  if (keep) {
    for (const team of visibleTeams) {
      const kept = filteredRowKeys(
        team.nodes,
        team.teamKey,
        { query: filter.query, status: filter.status },
        rules.statusOf,
        rules.textOf,
      );
      for (const key of kept) keep.add(key);
    }
  }
  const freezesFolds = filter.query.trim() !== "" || filter.workspace !== null || !!filter.rail;
  const agentKey = filter.agentKey ?? null;
  const filtering = searching || filter.workspace !== null || agentKey !== null || !!filter.rail;
  const ofChip = (n: AgentTreeNode) =>
    agentKey === null || agentKeyOfRevision(n.row.agentRevision) === agentKey;

  const planned: PlannedTeam[] = [];
  const counts: Record<AgentStatusFilter, number> = { all: 0, working: 0, needs: 0, idle: 0 };
  for (const team of visibleTeams) {
    // The rows the chip and the arrow leave: what every Show bucket is counted over.
    const allRows = flattenNodes(team.nodes).map((n) => ({ ...n, children: [] }) as AgentTreeNode);
    const launchers = launchersOf(team.nodes);
    let rows = allRows.filter(ofChip);
    if (filter.workspace) {
      rows =
        filter.workspace.teamKey === team.teamKey
          ? rows.filter(
              (n) => (n.workspace?.id ?? NO_WORKSPACE_ID) === filter.workspace!.workspaceId,
            )
          : [];
    }
    if (filter.rail) {
      rows =
        team.teamKey === filter.rail.teamKey
          ? rows.filter(
              (n) =>
                n.workspace &&
                rules.railKeyOfWorkspace?.(team.teamKey, n.workspace.id) === filter.rail!.railKey,
            )
          : [];
    }
    for (const n of rows) {
      const status = rules.statusOf(n.row);
      for (const id of Object.keys(counts) as AgentStatusFilter[]) {
        if (passesStatusFilter(status, id)) counts[id] += 1;
      }
    }
    if (keep) {
      rows = rows.filter((n) => keep.has(agentRowKey(n.teamKey ?? team.teamKey, n.row.handle)));
    }
    if (rules.manager && rows.length) {
      const matches = new Set(rows.map((n) => n.row.handle));
      const retained = new Set(matches);
      for (const node of rows) {
        let parent = launchers[node.row.handle];
        while (parent) {
          retained.add(parent);
          parent = launchers[parent];
        }
      }
      rows = allRows
        .filter((n) => retained.has(n.row.handle))
        .map((n) => ({ ...n, scopeContext: !matches.has(n.row.handle) }));
    }
    if (rows.length > 0) planned.push({ teamKey: team.teamKey, rows, launchers });
  }
  const visible = planned.reduce((n, t) => n + t.rows.filter((row) => !row.scopeContext).length, 0);
  const chipHiddenTeams =
    agentKey === null
      ? []
      : teams
          .filter((t) => hidden.includes(t.teamKey) && flattenNodes(t.nodes).some(ofChip))
          .map((t) => t.teamKey);
  return {
    teams: planned,
    keep,
    total,
    everyTotal,
    visible,
    filtering,
    freezesFolds,
    counts,
    chipHiddenTeams,
  };
}
