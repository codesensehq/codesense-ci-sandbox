// SPDX-License-Identifier: MIT
import { projectedWorkspaceTree } from "@/features/viewer/lib/agent-list-projection";
import {
  makePaneWorkspaceSources,
  paneWorkspaceLocationOf,
  type PaneWorkspaceSources,
} from "@/features/viewer/lib/agent-list-workspaces";
import {
  useMinimizedTerminalTabIndex,
  useRunningTerminalTabIndex,
} from "@/features/viewer/lib/cli-workspace/store/terminal-selectors";
import { useMemo } from "react";

import { frozenTabMachineIn } from "@/features/machines/lib/machine-offline";
import { useMachinesStore } from "@/features/machines/stores/machines-store";
import type { CrewMember } from "@/features/viewer/components/agent-identity";
import { useAgentStatusOf } from "@/features/viewer/hooks/use-agent-status-of";
import { useFaceTags } from "@/features/viewer/hooks/use-face-tags";
import { useAgentIdentityChoices } from "@/features/viewer/hooks/use-listed-agents";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import {
  resolveAgentFace,
  resolveAgentRole,
  teamPieceHue,
  workspaceCrew,
  type AgentFace,
  type AgentPiece,
} from "@/features/viewer/lib/agent-list-view";
import { agentRowLabel } from "@/features/viewer/lib/agent-row-presentation";
import { useTerminalTabIndex } from "@/features/viewer/lib/cli-workspace/store/terminal-selectors";
import { isAgentSession } from "@/features/viewer/lib/hand-agents";
import { useAvatarPacks } from "@/shared/hooks/use-avatar-packs";
import { activityQualifier, withActivityQualifiers } from "@/shared/lib/agent-activity-words";
import type { AgentIdentityChoice } from "@/shared/lib/agent-identity-choice";
import type { AgentStatus } from "@/shared/lib/agent-status-config";
import { isNeedsYouStatus } from "@/shared/lib/agent-status-config";
import {
  isQuietMark,
  markOfAgentStatus,
  markWithAvailability,
  rollupWords,
  type StatusMarkKind,
} from "@/shared/lib/status-mark";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import {
  findTabLocationBySessionId,
  selectHiddenTerminalTabIndex,
  useViewerStore,
  type TerminalTabIndex,
  type Workspace,
} from "@/shared/stores/viewer-store";
import type { AgentSessionRow } from "@/tauri/commands";

/** What a workspace's pill shows of the agents working in it. */
export interface PillCrew {
  /** The lead's face, shown first, badged with its role piece. */
  leadFace: AgentFace;
  /** The lead's number among running agents wearing the same face, when another does. */
  leadTag?: number;
  /** The lead's role piece. */
  leadPiece: AgentPiece;
  /** The lead's role in words. */
  leadLabel: string;
  /** The workspace's team colour. */
  hue: number;
  /** The other agents' faces (capped), then how many more. */
  crew: CrewMember[];
  more: number;
  /** Everyone inside as one roll-up mark (`topMark`). */
  mark: StatusMarkKind;
  /** The crew in words, for the pill's accessible description. */
  summary: string;
  /** Agents working in the workspace, and how many of them need a person. */
  count: number;
  needCount: number;
}

const EMPTY: ReadonlyMap<string, PillCrew> = new Map();

/**
 * A workspace's team colour: the piece hue of its place among the team's workspaces — the
 * colour its pill's crew and the agents list wear. Null when it is not listed.
 */
export function useWorkspaceTeamHue(teamKey: string, workspaceId: string): number | null {
  return useViewerStore((s) => {
    const index = (s.workspaces[teamKey] ?? []).findIndex((ws) => ws.id === workspaceId);
    return index === -1 ? null : teamPieceHue(index);
  });
}

/** The inputs {@link crewsFor} reads for one team. */
export interface CrewInput {
  workspaceSources?: PaneWorkspaceSources;
  teamKey: string;
  workspaces: readonly Workspace[] | undefined;
  tabIndex: TerminalTabIndex;
  /** Every running agent session, in any team. */
  live: readonly AgentSessionRow[];
  statusOf: (row: AgentSessionRow) => AgentStatus;
  machines: Parameters<typeof frozenTabMachineIn>[0];
  choices: Readonly<Record<string, AgentIdentityChoice>>;
  faces: ReturnType<typeof useAvatarPacks>["faces"];
  tags: ReadonlyMap<string, number>;
  memberMarks: Readonly<Record<string, StatusMarkKind>>;
  qualifiers: Readonly<Record<string, string | undefined>>;
}

function workspaceSourcesFor(
  input: Pick<CrewInput, "teamKey" | "workspaces" | "tabIndex" | "workspaceSources">,
): PaneWorkspaceSources {
  if (input.workspaceSources) return input.workspaceSources;
  const workspaces = { [input.teamKey]: [...(input.workspaces ?? [])] };
  return makePaneWorkspaceSources(
    {
      workspaces,
      softClosedWorkspaces: {},
      running: input.tabIndex,
      minimized: {},
      hidden: {},
      layoutByKey: {},
    },
    input.teamKey,
  );
}

/** The status mark each running agent of a team's workspaces shows, by session id. A tab held
 *  as it last was (its machine offline, or its layout not read live) reads offline, as its row
 *  and its card in the agents pane do: by the machine as this team lists it, since two teams can
 *  list one machine with different statuses. */
export function memberMarksFor(
  input: Pick<
    CrewInput,
    "teamKey" | "workspaces" | "tabIndex" | "live" | "statusOf" | "machines" | "workspaceSources"
  >,
): Record<string, StatusMarkKind> {
  const { teamKey, tabIndex, live, statusOf, machines } = input;
  const src = workspaceSourcesFor(input);
  return Object.fromEntries(
    live.flatMap((row) => {
      if (!isAgentSession(row) || !row.sessionId) return [];
      const home = paneWorkspaceLocationOf(row, src);
      if (!home || home.teamKey !== teamKey) return [];
      const tab = findTabLocationBySessionId(tabIndex, row.sessionId)?.tab;
      return [
        [
          row.sessionId,
          markWithAvailability(
            markOfAgentStatus(statusOf(row)),
            !!frozenTabMachineIn(machines, tab?.origin, teamKey),
          ),
        ],
      ];
    }),
  ) as Record<string, StatusMarkKind>;
}

/** The activity words each member's crew line shows, by session id (only the ones it shows). */
export function qualifiersFor(
  memberMarks: Readonly<Record<string, StatusMarkKind>>,
  activityBySession: Readonly<Record<string, Parameters<typeof activityQualifier>[0]>>,
): Record<string, string | undefined> {
  return Object.fromEntries(
    Object.entries(memberMarks).flatMap(([id, mark]) => {
      const qualifier = activityQualifier(activityBySession[id], mark);
      return qualifier ? [[id, qualifier]] : [];
    }),
  );
}

/**
 * The crew of each of one team's workspaces, by workspace id: the agents whose tabs sit in it,
 * led by the highest of them in the launch tree. Pure, so the agents list can read every team's.
 */
export function crewsFor(input: CrewInput): ReadonlyMap<string, PillCrew> {
  const { teamKey, workspaces, live, statusOf, choices, faces, tags, memberMarks, qualifiers } =
    input;
  if (live.length === 0) return EMPTY;
  const src = workspaceSourcesFor(input);
  // How many agents each one launched, across every workspace.
  const launched = new Map<string, number>();
  for (const r of live) {
    if (!r.launchedBy) continue;
    const launcher = live.find((l) => l.handle === r.launchedBy || l.sessionId === r.launchedBy);
    if (launcher) launched.set(launcher.handle, (launched.get(launcher.handle) ?? 0) + 1);
  }
  const crews = new Map<string, PillCrew>();
  (workspaces ?? []).forEach((ws, index) => {
    const rows = live.filter((row) => {
      if (!isAgentSession(row)) return false;
      const home = paneWorkspaceLocationOf(row, src);
      return home?.teamKey === teamKey && home.workspace.id === ws.id;
    });
    if (rows.length === 0) return;
    const nodes = projectedWorkspaceTree(teamKey, ws.id, src);
    const crew = workspaceCrew(nodes, statusOf, (row) => memberMarks[row.sessionId!]);
    if (!crew.lead) return;
    const role = resolveAgentRole(crew.lead, {
      launched: launched.get(crew.lead.handle) ?? 0,
      choice: choices[crew.lead.handle],
    });
    const members = crew.crew.map((r) => {
      const name = agentRowLabel(r);
      return {
        key: r.handle,
        name,
        face: resolveAgentFace(r, {
          name,
          choice: choices[r.handle],
          faces,
        }),
        tag: tags.get(r.handle),
      };
    });
    const count = rows.length;
    crews.set(ws.id, {
      leadFace: resolveAgentFace(crew.lead, {
        name: agentRowLabel(crew.lead),
        choice: choices[crew.lead.handle],
        faces,
      }),
      leadTag: tags.get(crew.lead.handle),
      leadPiece: role.piece,
      leadLabel: role.label,
      hue: teamPieceHue(index),
      crew: members,
      more: crew.more,
      mark: crew.mark,
      count,
      needCount: rows.filter((row) => isNeedsYouStatus(statusOf(row))).length,
      summary: withActivityQualifiers(
        `${count} agent${count === 1 ? "" : "s"}, led by ${agentRowLabel(crew.lead)}${
          isQuietMark(crew.mark) ? "" : `: ${rollupWords(crew.counts, ", ")}`
        }`,
        rows.map((row) => qualifiers[row.sessionId!]),
        ", ",
      ),
    });
  });
  return crews;
}

/**
 * The crew of each of a team's workspaces, by workspace id: the agents whose tabs sit in it,
 * led by the highest of them in the launch tree. Roles read the whole launch tree, so a lead
 * that launched agents into other workspaces still wears the manager's piece. Reads the
 * roster the agents list keeps loaded; a workspace with no agents has no entry.
 */
export function useWorkspaceCrews(teamKey: string): ReadonlyMap<string, PillCrew> {
  const projectedReferences = useAgentListProjectionStore((state) => state.references);
  const sessions = useAgentListProjectionStore((s) => s.sessions);
  const choices = useAgentIdentityChoices(teamKey);
  const { faces } = useAvatarPacks();
  const tags = useFaceTags(teamKey);
  const tabIndex = useTerminalTabIndex();
  const allWorkspaces = useViewerStore((s) => s.workspaces);
  const softClosedWorkspaces = useViewerStore((s) => s.softClosedWorkspaces);
  const running = useRunningTerminalTabIndex();
  const minimized = useMinimizedTerminalTabIndex();
  const hidden = useViewerStore(selectHiddenTerminalTabIndex);
  const layoutByKey = useViewerStore((s) => s.terminalLayout.byKey);
  const workspaceSources = useMemo(
    () =>
      makePaneWorkspaceSources(
        {
          workspaces: allWorkspaces,
          softClosedWorkspaces,
          running,
          minimized,
          hidden,
          layoutByKey,
        },
        teamKey,
      ),
    [allWorkspaces, softClosedWorkspaces, running, minimized, hidden, layoutByKey, teamKey],
  );
  const workspaces = allWorkspaces[teamKey];
  const live = useMemo(() => (sessions ?? []).filter((r) => r.alive), [sessions]);
  const statusOf = useAgentStatusOf(live);
  const machines = useMachinesStore((s) => s.byTeam);
  const memberMarks = useMemo(
    () =>
      memberMarksFor({ workspaceSources, teamKey, workspaces, tabIndex, live, statusOf, machines }),
    [
      workspaceSources,
      live,
      workspaces,
      tabIndex,
      teamKey,
      statusOf,
      machines,
      projectedReferences,
    ],
  );
  // Subscribe only to the words this crew exposes, not every activity snapshot.
  const qualifierKey = useAgentStateStore((s) =>
    JSON.stringify(qualifiersFor(memberMarks, s.activityBySession)),
  );
  const qualifiers = useMemo(
    () => JSON.parse(qualifierKey) as Record<string, string | undefined>,
    [qualifierKey],
  );
  return useMemo(
    () =>
      crewsFor({
        workspaceSources,
        teamKey,
        workspaces,
        tabIndex,
        live,
        statusOf,
        machines,
        choices,
        faces,
        tags,
        memberMarks,
        qualifiers,
      }),
    [
      workspaceSources,
      projectedReferences,
      tabIndex,
      teamKey,
      live,
      statusOf,
      workspaces,
      choices,
      faces,
      tags,
      memberMarks,
      qualifiers,
      machines,
    ],
  );
}
