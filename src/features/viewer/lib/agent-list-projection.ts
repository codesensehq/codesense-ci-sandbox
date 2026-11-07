// SPDX-License-Identifier: MIT
import { useManagedSessionStore } from "@/shared/stores/use-managed-session-store";
import { useDashboardStreamStore } from "@/shared/stores/dashboard-stream-store";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { liveHub, type HubRow } from "@/features/dashboard/lib/live-hub";
import { countRows } from "@/features/inbox/projection";
import { workspaceDisplayLabel } from "@/shared/lib/worktree/resolve";
import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";
import { agentListRefresh, type AgentSessionRow, type SpendCapView } from "@/tauri/commands";
import { useEffect, useMemo } from "react";
import { create } from "zustand";
import type { AgentTreeNode, AgentWorkspaceRef } from "./agent-list-view";
import { sessionStoryOfWorkspace } from "./session-story";

export interface AgentListReference {
  id: string;
  sessionRef: string;
  inboxRef: string;
  teamKey: string;
  placementTeamRef?: string | null;
  parentId: string | null;
  parentSource: string | null;
  order: number;
  workspaceRef: string | null;
  storyRef: string | null;
  workspaceRank: number;
  crewParentId?: string | null;
  crewOrder?: number | null;
  crewLead?: boolean;
  statusGroup?: 0 | 1 | 2 | 3;
}
type WorkspaceMap = Record<string, readonly Workspace[]>;
type ClosedMap = Record<string, Record<string, { workspace: Workspace }>>;
const EMPTY_NODES: AgentTreeNode[] = [];
const EMPTY_REFS: AgentListReference[] = [];

/** Decode parent and workspace references; all placement decisions came from Rust. */
export function joinAgentList(
  references: readonly AgentListReference[],
  sessions: readonly AgentSessionRow[],
  workspaces: WorkspaceMap,
  closed: ClosedMap,
): Map<string, AgentTreeNode[]> {
  const owners = new Map<string, AgentSessionRow>();
  for (const row of sessions) {
    owners.set(row.handle, row);
    if (row.sessionId) owners.set(row.sessionId, row);
    if (row.id) owners.set(row.id, row);
  }
  const nodes = new Map<string, AgentTreeNode>();
  const teams = new Map<string, AgentTreeNode[]>();
  const workspace = (team: string, id: string | null) =>
    id === null
      ? undefined
      : (workspaces[team]?.find((w) => w.id === id) ?? closed[team]?.[id]?.workspace);
  const ordered = [...references].sort((a, b) => a.order - b.order);
  for (const ref of ordered) {
    const row = owners.get(ref.sessionRef);
    if (!row) continue;
    const ws = workspace(ref.teamKey, ref.workspaceRef);
    const storyWs = workspace(ref.teamKey, ref.storyRef);
    const resolved: AgentWorkspaceRef | null = ws
      ? {
          id: ws.id,
          label: (ws.kind === "story" && ws.storyId) || ws.storyTitle || workspaceDisplayLabel(ws),
          kind: ws.kind === "story" ? "story" : "folder",
          storyStatus: ws.kind === "story" ? (ws.storyStatus ?? undefined) : undefined,
          storyTitle: ws.kind === "story" ? ws.storyTitle || undefined : undefined,
          sourceRepoSlug: ws.sourceRepoSlug,
          machineId:
            ws.location?.kind === "machine" && ws.location.machineId !== "local"
              ? ws.location.machineId
              : undefined,
        }
      : null;
    nodes.set(ref.id, {
      projectedId: ref.id,
      projectedParentId: ref.parentId,
      projectedOrder: ref.order,
      workspaceRank: ref.workspaceRank,
      crewLead: ref.crewLead,
      row,
      workspace: resolved,
      story: storyWs ? sessionStoryOfWorkspace(storyWs, ref.teamKey, storyWs.id) : null,
      children: [],
      teamKey: ref.teamKey,
      ...(ref.parentSource === "chosen" || ref.parentSource === "launcher"
        ? { parentSource: ref.parentSource }
        : {}),
    });
  }
  for (const ref of ordered) {
    const node = nodes.get(ref.id);
    if (!node) continue;
    const parent = ref.parentId ? nodes.get(ref.parentId) : undefined;
    if (parent) parent.children.push(node);
    else {
      const roots = teams.get(ref.teamKey) ?? [];
      roots.push(node);
      teams.set(ref.teamKey, roots);
    }
  }
  return teams;
}
export type SessionsUpdater =
  | AgentSessionRow[]
  | null
  | ((previous: AgentSessionRow[] | null) => AgentSessionRow[] | null);
export const useAgentListProjectionStore = create<{
  spend: Record<string, SpendCapView>;
  references: AgentListReference[];
  statusReferences: AgentListReference[];
  sessions: AgentSessionRow[] | null;
  error: string | null;
  partial: boolean;
  homeTeam: string | null;
  teamOrder: string[];
  reload: () => Promise<void>;
  setSessions: (next: SessionsUpdater) => void;
}>()((set) => ({
  spend: {},
  references: EMPTY_REFS,
  statusReferences: EMPTY_REFS,
  sessions: null,
  error: null,
  partial: true,
  homeTeam: null,
  teamOrder: [],
  reload: async () => {
    await agentListRefresh();
  },
  setSessions: (next) =>
    set((state) => ({ sessions: typeof next === "function" ? next(state.sessions) : next })),
}));

// Headless output has no mounted terminal to feed the shared activity clock.
useAgentListProjectionStore.subscribe((state, previous) => {
  if (state.sessions === previous.sessions) return;
  const recordOutput = useAgentStateStore.getState().recordOutput;
  for (const row of state.sessions ?? []) {
    if (!row.sessionId) continue;
    if (row.lastOutputMs != null) recordOutput(row.sessionId, row.lastOutputMs);
    const previousOwner = previous.sessions?.find((before) => before.sessionId === row.sessionId);
    if (row.managedState && row.managedState !== previousOwner?.managedState) {
      const managed = useManagedSessionStore.getState();
      const changed = managed.states[row.sessionId] !== row.managedState;
      managed.receiveProjectedState(row.sessionId, row.managedState);
      if (row.managedState === "needs-attention" && changed)
        useDashboardStreamStore.getState().pushSessionDelta({
          origin: "session",
          kind: "session",
          sessionId: row.sessionId,
          status: "needs-attention",
        });
    }
  }
});

export function joinAgentOwners(
  sessions: readonly HubRow[],
  inbox: HubRow[],
  previous: readonly AgentSessionRow[] = [],
): AgentSessionRow[] {
  const counts = countRows(inbox);
  const before = new Map(previous.map((row) => [row.id ?? row.sessionId ?? row.handle, row]));
  return sessions.map((row) => {
    const team = row.team as { org: string; team: string } | null | undefined;
    const fields = counts.find(
      (count) => count.team.org === team?.org && count.team.team === team?.team,
    )?.sessions[String(row.handle)];
    const owned = Object.fromEntries(
      Object.entries(fields ?? {}).filter(([key]) => key !== "team"),
    );
    const joined = { ...row, ...owned } as unknown as AgentSessionRow;
    const old = before.get(joined.id ?? joined.sessionId ?? joined.handle);
    return old &&
      Object.keys(old).length === Object.keys(joined).length &&
      Object.entries(joined).every(([key, value]) => old[key as keyof AgentSessionRow] === value)
      ? old
      : joined;
  });
}

/** An early bridge action can read the same owners before the app-root watcher warms. */
export async function readAgentListSessions(): Promise<AgentSessionRow[]> {
  const cached = useAgentListProjectionStore.getState().sessions;
  if (cached !== null) return cached;
  const [sessions, inbox] = await Promise.all([
    liveHub.snapshot("sessions", "all"),
    liveHub.snapshot("inbox", "all"),
  ]);
  return joinAgentOwners(sessions.snapshot, inbox.snapshot);
}

/** Use the same placement team as the native agent list, including before its watcher warms. */
export async function readAgentListTeam(row: AgentSessionRow): Promise<string | null> {
  let references: readonly Pick<AgentListReference, "sessionRef" | "teamKey">[] =
    useAgentListProjectionStore.getState().references;
  const find = () =>
    references.find((ref) => [row.id, row.sessionId, row.handle].includes(ref.sessionRef));
  if (!find()) {
    const value = await liveHub.snapshot("agentList", "all");
    references = value.snapshot.filter(
      (ref) => typeof ref.sessionRef === "string",
    ) as unknown as AgentListReference[];
  }
  return find()?.teamKey ?? null;
}

let watchers = 0;
let stops: (() => void)[] = [];
export function watchAgentList(): () => void {
  if (++watchers === 1) {
    let sessions: HubRow[] | null = null;
    let inbox: HubRow[] = [];
    const joinOwners = () => {
      if (!sessions) return;
      const previous = useAgentListProjectionStore.getState().sessions ?? [];
      const next = joinAgentOwners(sessions, inbox, previous);
      if (
        useAgentListProjectionStore.getState().sessions === null ||
        next.length !== previous.length ||
        next.some((row, index) => row !== previous[index])
      )
        useAgentListProjectionStore.setState({ sessions: next });
    };
    stops = [
      liveHub.watch("sessions", "all", (value) => {
        sessions = value.snapshot;
        joinOwners();
      }),
      liveHub.watch("inbox", "all", (value) => {
        inbox = value.snapshot;
        joinOwners();
      }),
      liveHub.watch("agentList", "all", (value) =>
        useAgentListProjectionStore.setState({
          spend: Object.fromEntries(
            value.snapshot
              .filter((row) => row.rowType === "spend")
              .map((row) => [`${row.scopeKind}:${row.key}`, row.spend as unknown as SpendCapView]),
          ),
          homeTeam:
            (value.snapshot.find((row) => row.rowType === "state")?.homeTeam as string) ?? null,
          teamOrder:
            (value.snapshot.find((row) => row.rowType === "state")?.teamOrder as string[]) ?? [],
          references: value.snapshot.filter(
            (row) => typeof row.sessionRef === "string",
          ) as unknown as AgentListReference[],
          partial: !value.snapshot.find((row) => row.rowType === "state")?.ready,
          error: null,
        }),
      ),
    ];
  }
  return () => {
    if (--watchers === 0) {
      for (const stop of stops) stop();
      stops = [];
    }
  };
}
export function useAgentListTrees(): Map<string, AgentTreeNode[]> {
  useEffect(watchAgentList, []);
  const references = useAgentListProjectionStore((s) => s.references);
  const sessions = useAgentListProjectionStore((s) => s.sessions);
  const workspaces = useViewerStore((s) => s.workspaces);
  const closed = useViewerStore((s) => s.softClosedWorkspaces);
  return useMemo(
    () => joinAgentList(references, sessions ?? [], workspaces, closed),
    [references, sessions, workspaces, closed],
  );
}
export function useAgentListNodes(teamKey: string): AgentTreeNode[] {
  return useAgentListTrees().get(teamKey) ?? EMPTY_NODES;
}

/** Decode the workspace crew edges separately from the pane's chosen-manager tree. */
export function projectedWorkspaceTree(
  teamKey: string,
  workspaceId: string,
  sources: Pick<ReturnType<typeof useViewerStore.getState>, "workspaces" | "softClosedWorkspaces">,
): AgentTreeNode[] {
  const state = useAgentListProjectionStore.getState();
  const references = state.references
    .filter(
      (ref) =>
        ref.teamKey === teamKey &&
        ref.workspaceRef === workspaceId &&
        typeof ref.crewOrder === "number",
    )
    .map((ref) => ({ ...ref, parentId: ref.crewParentId ?? null, order: ref.crewOrder ?? 0 }));
  return (
    joinAgentList(
      references,
      state.sessions ?? [],
      sources.workspaces,
      sources.softClosedWorkspaces,
    ).get(teamKey) ?? EMPTY_NODES
  );
}

/** Read the separate Rust order only while the Status arrangement is mounted. */
export function useAgentStatusOrder(enabled: boolean): readonly AgentListReference[] {
  useEffect(() => {
    if (!enabled) return;
    return liveHub.watch("agentList", "all|status", (value) =>
      useAgentListProjectionStore.setState({
        statusReferences: value.snapshot.filter(
          (row) => typeof row.sessionRef === "string",
        ) as unknown as AgentListReference[],
      }),
    );
  }, [enabled]);
  return useAgentListProjectionStore((state) => state.statusReferences);
}

export function projectedStatusNodes(
  references: readonly AgentListReference[],
  nodes: readonly AgentTreeNode[],
) {
  const byId = new Map<string, AgentTreeNode>();
  const visit = (nodes: readonly AgentTreeNode[]) => {
    for (const node of nodes) {
      if (node.projectedId) byId.set(node.projectedId, node);
      visit(node.children);
    }
  };
  visit(nodes);
  return [...references]
    .sort((a, b) => a.order - b.order)
    .flatMap((ref) => {
      const node = byId.get(ref.id);
      return node
        ? [
            {
              node: { ...node, children: [] },
              teamKey: node.teamKey ?? ref.teamKey,
              rank: ref.statusGroup ?? 3,
            },
          ]
        : [];
    });
}

export function projectedSession(sessionId: string): AgentSessionRow | undefined {
  return useAgentListProjectionStore
    .getState()
    .sessions?.find((row) => row.sessionId === sessionId || row.id === sessionId);
}
