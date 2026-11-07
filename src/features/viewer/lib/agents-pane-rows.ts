// SPDX-License-Identifier: MIT
/**
 * The agents pane as one flat list of rows, in the order the pane draws them: the Pinned group,
 * then each team's header, its workspace cards and their agents (or the status sections). The
 * list virtualizes over these rows.
 */

import {
  groupByWorkspace,
  groupByManager,
  type AgentListArrange,
  type WorkspaceSessionGroup,
} from "@/features/agents/lib/agent-list-layout";
import type { AgentTreeNode, PinnedEntry } from "@/features/viewer/lib/agent-list-view";
import { railInfo } from "@/features/viewer/lib/group-workspaces-by-epic";
import type { PlannedTeam } from "@/features/viewer/lib/agents-pane-plan";
import { agentRowKey } from "@/shared/lib/agent-pins";
import {
  NO_WORKSPACE_ID,
  managerFoldKey,
  teamFoldKey,
  workspaceFoldKey,
  workspaceGroupKey,
} from "@/shared/lib/workspace-key";

export type PaneRow =
  | { kind: "manager-head"; key: string; label: string }
  | {
      kind: "rail-head";
      key: string;
      title: string;
      railKind: "epic" | "roots" | "ungrouped" | "custom";
    }
  | {
      kind: "pinned";
      key: string;
      /** Each pinned row, in pin order, with the team its pin belongs to. */
      entries: readonly { key: string; node: AgentTreeNode; teamKey: string }[];
      /** Rows past the visible limit, folded under "+n more pinned". */
      more: number;
      expanded: boolean;
    }
  | {
      kind: "team";
      key: string;
      teamKey: string;
      folded: boolean;
      needCount: number;
    }
  | {
      kind: "ws";
      key: string;
      teamKey: string;
      group: WorkspaceSessionGroup;
      folded: boolean;
      arrowOn: boolean;
      /** This card is being dragged: its agents fold out of the list until the drop. */
      dragging: boolean;
    }
  | {
      kind: "agent";
      key: string;
      teamKey: string;
      workspaceId: string | null;
      node: AgentTreeNode;
      /** Its indent: one below its launcher when that is drawn above it in the same card, else 0. */
      depth: number;
      /** The launcher it is drawn under, if any. */
      parentHandle: string | null;
      treeHandle?: string;
      managerFolded?: boolean;
      managerCanFold?: boolean;
    }
  | { kind: "status-head"; key: string; id: string; label: string; count: number }
  | {
      kind: "status-agent";
      key: string;
      section: string;
      /** The agent's own team: a status section spans teams. */
      teamKey: string;
      node: AgentTreeNode;
    };

/** A status arrangement's sections, in attention order. */
export const STATUS_SECTIONS: readonly { id: string; label: string; rank: 0 | 1 | 2 | 3 }[] = [
  { id: "needs", label: "Needs you", rank: 0 },
  { id: "working", label: "Working", rank: 1 },
  { id: "ready", label: "Ready", rank: 2 },
  { id: "other", label: "Other", rank: 3 },
];

export interface PaneRowsInput {
  customGroups?: Record<string, import("./workspace-groups").WorkspaceGroup[]>;
  /** The Pinned group's entries, already narrowed to what the plan shows. */
  pinned: readonly PinnedEntry[];
  pinnedExpanded: boolean;
  /** How many pinned rows show before "+n more pinned". */
  pinnedVisibleMax: number;
  teams: readonly PlannedTeam[];
  arrange: AgentListArrange;
  /** Agents for the status arrangement, sorted by attention, with their rank and team. */
  statusRows: readonly { node: AgentTreeNode; teamKey: string; rank: 0 | 1 | 2 | 3 }[];
  /** Fold state, by fold key. */
  collapsed: Readonly<Record<string, boolean>> | undefined;
  /** Folds are frozen (a search or the workspace chip): every group opens, and the saved folds
   *  stay as they are, so they come back when the filter clears. */
  filtering: boolean;
  /** The agents that need a person, by `agentRowKey`. */
  needs: ReadonlySet<string>;
  /** Each team's need count over its whole tree, by team key: a folded team says it whatever a
   *  filter leaves in view. A team missing here counts its rows in view. */
  teamNeedCounts?: Readonly<Record<string, number>>;
  arrowFilter: { teamKey: string; workspaceId: string } | null;
  /** Each team's shared workspace order (the tray's pills), by team key. */
  groupedTeams?: Readonly<Record<string, string>>;
  workspaceRails?: Readonly<Record<string, Readonly<Record<string, string>>>>;
  workspaceOrder?: Readonly<Record<string, readonly string[]>>;
  /** The workspace card being dragged, if any: its agent rows are left out while it moves. */
  dragging?: { teamKey: string; workspaceId: string } | null;
}

/** Hands out row keys that never repeat: a key already given out names its row's session too
 *  (one handle can be listed twice in a team, by two sessions), then a count if that repeats. */
function uniqueKeys(): (base: string, node: AgentTreeNode) => string {
  const used = new Set<string>();
  return (base, node) => {
    let key = base;
    if (used.has(key)) key = `${base}|${node.row.sessionId ?? ""}`;
    for (let n = 2; used.has(key); n += 1) key = `${base}|${node.row.sessionId ?? ""}#${n}`;
    used.add(key);
    return key;
  };
}

/** Every row of the pane, top to bottom. */
export function buildPaneRows(input: PaneRowsInput): PaneRow[] {
  const rows: PaneRow[] = [];
  const keyOf = uniqueKeys();
  if (input.pinned.length > 0) {
    // The Pinned group is one row: it is small, and its label, rows and rule stay together.
    const pinnedRows = input.pinned.flatMap((entry) =>
      entry.nodes.map((node) => ({
        key: keyOf(`pinned:${entry.id}:${node.row.handle}`, node),
        node,
        teamKey: entry.teamKey,
      })),
    );
    const shown = input.pinnedExpanded ? pinnedRows : pinnedRows.slice(0, input.pinnedVisibleMax);
    rows.push({
      kind: "pinned",
      key: "pinned",
      entries: shown,
      more: pinnedRows.length - shown.length,
      // Only a group longer than the visible limit has anything to fold back.
      expanded: input.pinnedExpanded && pinnedRows.length > input.pinnedVisibleMax,
    });
  }

  if (input.arrange === "status") {
    for (const section of STATUS_SECTIONS) {
      const members = input.statusRows.filter((r) => r.rank === section.rank);
      if (members.length === 0) continue;
      rows.push({
        kind: "status-head",
        key: `status:${section.id}`,
        id: section.id,
        label: section.label,
        count: members.length,
      });
      for (const { node, teamKey } of members) {
        rows.push({
          kind: "status-agent",
          // One handle can run in two teams: the key names the team, as an agent row's does.
          key: keyOf(`status-agent:${agentRowKey(teamKey, node.row.handle)}`, node),
          section: section.id,
          teamKey,
          node,
        });
      }
    }
    return rows;
  }

  for (const team of input.teams) {
    const teamKey = team.teamKey;
    const folded = !input.filtering && input.collapsed?.[teamFoldKey(teamKey)] === true;
    const needCount =
      input.teamNeedCounts?.[teamKey] ??
      team.rows.filter((r) => input.needs.has(agentRowKey(r.teamKey ?? teamKey, r.row.handle)))
        .length;
    rows.push({
      kind: "team",
      key: `team:${teamKey}`,
      teamKey,
      folded,
      needCount,
    });
    if (folded) continue;
    if (input.arrange === "manager") {
      const roots = groupByManager(team.rows, team.launchers);
      // The full resolved graph owns manager identity, even when every report is filtered out.
      const managers = new Set(Object.values(team.launchers));
      const walk = (
        node: AgentTreeNode,
        depth: number,
        parentHandle: string | null,
        treeHandle?: string,
      ) => {
        const managerFolded =
          !input.filtering && input.collapsed?.[managerFoldKey(teamKey, node.row.handle)] === true;
        rows.push({
          kind: "agent",
          key: keyOf(`agent:${agentRowKey(teamKey, node.row.handle)}`, node),
          teamKey,
          workspaceId: node.workspace?.id ?? null,
          node,
          depth,
          parentHandle,
          treeHandle,
          managerFolded,
          managerCanFold: managers.has(node.row.handle),
        });
        if (!managerFolded)
          for (const child of node.children) walk(child, depth + 1, node.row.handle, treeHandle);
      };
      for (const root of roots.filter((n) => managers.has(n.row.handle)))
        walk(root, 0, null, root.row.handle);
      const unmanaged = roots.filter((n) => !managers.has(n.row.handle));
      if (unmanaged.length) {
        rows.push({ kind: "manager-head", key: `unmanaged:${teamKey}`, label: "No manager" });
        for (const root of unmanaged) walk(root, 0, null);
      }
      continue;
    }
    let previousRail: string | undefined;
    for (const group of groupByWorkspace(team.rows, input.workspaceOrder?.[teamKey])) {
      const railKey = input.workspaceRails?.[teamKey]?.[group.workspaceId];
      if (
        input.groupedTeams?.[teamKey] === "grouped" &&
        railKey !== undefined &&
        railKey !== previousRail
      ) {
        const info = railInfo(teamKey, railKey, input.customGroups?.[teamKey]);
        rows.push({
          kind: "rail-head",
          key: `rail:${teamKey}|${railKey}|${group.workspaceId}`,
          title: info.title,
          railKind: info.kind,
        });
      }
      previousRail = railKey;
      const wsKey = workspaceGroupKey(teamKey, group.workspaceId);
      const wsFolded =
        !input.filtering &&
        input.collapsed?.[workspaceFoldKey(teamKey, group.workspaceId)] === true;
      const arrowOn =
        input.arrowFilter?.teamKey === teamKey &&
        input.arrowFilter.workspaceId === group.workspaceId;
      const dragging =
        input.dragging?.teamKey === teamKey && input.dragging.workspaceId === group.workspaceId;
      rows.push({
        kind: "ws",
        key: `ws:${wsKey}`,
        teamKey,
        group,
        folded: wsFolded,
        arrowOn,
        dragging,
      });
      if (wsFolded || dragging) continue;
      // The indent is read within the card: an agent sits under its launcher only when that is
      // drawn above it here. A launcher in another card, another team or out of the filter
      // leaves it at the card's top level.
      const depthOf = new Map<string, number>();
      for (const node of group.nodes) {
        const launcher = team.launchers[node.row.handle];
        const above = launcher === undefined ? undefined : depthOf.get(launcher);
        const depth = above === undefined ? 0 : above + 1;
        depthOf.set(node.row.handle, depth);
        rows.push({
          kind: "agent",
          key: keyOf(`agent:${agentRowKey(teamKey, node.row.handle)}`, node),
          teamKey,
          workspaceId: group.workspaceId,
          node,
          depth,
          parentHandle: above === undefined ? null : launcher!,
        });
      }
    }
  }
  return rows;
}

/** Where an agent row sits in the pane, for a screen reader: the flat list cannot nest it. */
export interface PaneRowPlace {
  teamKey: string;
  /** Its workspace card's name, or null for the No workspace bucket. */
  workspace: string | null;
  /** The launcher it is drawn under (an agent row with an indent), if any. */
  underHandle: string | null;
  /** The group it is listed in, when that is not its team's tree: Pinned or a status section. */
  group: string | null;
}

/** What a row says to a screen reader, read off the plan so a row that is not drawn has it too. */
export interface PaneRowSemantics {
  /** The row's place in the whole list, 1-based, and the list's length. */
  posInSet: number;
  setSize: number;
  /** A header row's heading level: the Pinned group, a team and a status section are level 3,
   *  a workspace card under its team is level 4. Null for an agent row. */
  headingLevel: 3 | 4 | null;
  /** An agent row's place. The Pinned row holds several agents: one place per entry. */
  places: readonly PaneRowPlace[];
}

/** The heading level a team header and a status section take: the pane's own groups. */
export const PANE_GROUP_HEADING_LEVEL = 3;
/** A workspace card's heading level, one under its team. */
export const PANE_CARD_HEADING_LEVEL = 4;

/** The workspace an agent's node names, as its card would: none for the No workspace bucket. */
function nodeWorkspace(node: AgentTreeNode): string | null {
  return node.workspace?.label ?? null;
}

/** Every row's screen-reader semantics, in row order. */
export function paneRowSemantics(rows: readonly PaneRow[]): PaneRowSemantics[] {
  const setSize = rows.length;
  // The card an agent row is listed under: the last card above it, in its team.
  let card: Extract<PaneRow, { kind: "ws" }> | null = null;
  let section: string | null = null;
  return rows.map((row, index): PaneRowSemantics => {
    const at = { posInSet: index + 1, setSize };
    switch (row.kind) {
      case "pinned":
        return {
          ...at,
          headingLevel: PANE_GROUP_HEADING_LEVEL,
          places: row.entries.map(({ node, teamKey }) => ({
            teamKey: node.teamKey ?? teamKey,
            workspace: nodeWorkspace(node),
            underHandle: null,
            group: "Pinned",
          })),
        };
      case "team":
        card = null;
        return { ...at, headingLevel: PANE_GROUP_HEADING_LEVEL, places: [] };
      case "ws":
        card = row;
        return { ...at, headingLevel: PANE_CARD_HEADING_LEVEL, places: [] };
      case "rail-head":
      case "manager-head":
        card = null;
        return { ...at, headingLevel: PANE_CARD_HEADING_LEVEL, places: [] };
      case "status-head":
        section = row.label;
        return { ...at, headingLevel: PANE_GROUP_HEADING_LEVEL, places: [] };
      case "agent": {
        const inCard =
          card !== null &&
          card.teamKey === row.teamKey &&
          card.group.workspaceId === row.workspaceId
            ? card
            : null;
        return {
          ...at,
          headingLevel: null,
          places: [
            {
              teamKey: row.teamKey,
              workspace:
                row.workspaceId === null || row.workspaceId === NO_WORKSPACE_ID
                  ? null
                  : (inCard?.group.label ?? nodeWorkspace(row.node)),
              underHandle: row.parentHandle,
              group: null,
            },
          ],
        };
      }
      case "status-agent":
        return {
          ...at,
          headingLevel: null,
          places: [
            {
              teamKey: row.teamKey,
              workspace: nodeWorkspace(row.node),
              underHandle: null,
              group: section,
            },
          ],
        };
    }
  });
}

/** An agent row's place in words, as its description leads with it:
 *  "Pinned · Team Alpha · Workspace ws-a · Under Lead". */
export function paneRowPlaceWords(
  place: PaneRowPlace,
  names: { teamLabel: string; underName?: string | null },
): string {
  return [
    place.group,
    `Team ${names.teamLabel}`,
    place.workspace === null ? "No workspace" : `Workspace ${place.workspace}`,
    place.underHandle !== null && `Under ${names.underName ?? place.underHandle}`,
  ]
    .filter(Boolean)
    .join(" · ");
}
