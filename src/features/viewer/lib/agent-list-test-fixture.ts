// SPDX-License-Identifier: MIT
import {
  joinAgentList,
  useAgentListProjectionStore,
  type AgentListReference,
} from "./agent-list-projection";
import type { AgentSessionRow } from "@/tauri/commands";

/** Explicit backend fixture references for rendering tests; no production model runs here. */
export function seedAgentListFixture(
  sessions: AgentSessionRow[],
  placements: ReadonlyMap<
    string,
    {
      teamKey: string;
      workspaceId?: string | null;
      storyId?: string | null;
      parentHandle?: string | null;
      parentSource?: "chosen" | "launcher";
      crewParentHandle?: string | null;
      crewLead?: boolean;
      workspaceRank?: number;
    }
  >,
  partial = false,
) {
  const refs: AgentListReference[] = [];
  for (const [order, row] of sessions.entries()) {
    const place =
      placements.get(row.id ?? row.sessionId ?? row.handle) ?? placements.get(row.handle);
    if (!place || !row.alive) continue;
    const id = JSON.stringify([place.teamKey, row.handle]);
    refs.push({
      id,
      sessionRef: row.id ?? row.sessionId ?? row.handle,
      inboxRef: `counts:team:${place.teamKey.replace(":", "/")}`,
      teamKey: place.teamKey,
      placementTeamRef: place.workspaceId ? place.teamKey : null,
      parentId: place.parentHandle ? JSON.stringify([place.teamKey, place.parentHandle]) : null,
      parentSource: place.parentHandle ? (place.parentSource ?? "launcher") : null,
      order,
      workspaceRef: place.workspaceId ?? null,
      storyRef: place.storyId ?? null,
      workspaceRank: place.workspaceRank ?? 0,
      crewParentId: place.crewParentHandle
        ? JSON.stringify([place.teamKey, place.crewParentHandle])
        : null,
      crewOrder: order,
      crewLead: place.crewLead ?? !place.crewParentHandle,
    });
  }
  useAgentListProjectionStore.setState({
    sessions,
    references: refs,
    partial,
    error: null,
    homeTeam: refs[0]?.teamKey ?? null,
    teamOrder: [...new Set(refs.map((ref) => ref.teamKey))],
  });
}

/** Render a declared fixture graph through the same reference join as real server rows. */
export function fixtureTeamNodes(
  teamKey: string,
  sessions: AgentSessionRow[],
  workspaceIds: Record<string, string> = {},
) {
  const workspaceOrder = [...new Set(Object.values(workspaceIds))];
  const references = sessions.map((row, order) => ({
    id: JSON.stringify([teamKey, row.handle]),
    sessionRef: row.sessionId ?? row.handle,
    inboxRef: `counts:${teamKey}`,
    teamKey,
    parentId: row.launchedBy ? JSON.stringify([teamKey, row.launchedBy]) : null,
    parentSource: row.launchedBy ? "launcher" : null,
    order,
    workspaceRef: workspaceIds[row.handle] ?? null,
    storyRef: null,
    workspaceRank: workspaceIds[row.handle]
      ? workspaceOrder.indexOf(workspaceIds[row.handle])
      : workspaceOrder.length,
  }));
  return (
    joinAgentList(
      references,
      sessions,
      {
        [teamKey]: workspaceOrder.map((id) => ({
          id,
          kind: "generic" as const,
          cwd: `/fixture/${id}`,
          status: "ready" as const,
          name: id,
        })),
      },
      {},
    ).get(teamKey) ?? []
  );
}
