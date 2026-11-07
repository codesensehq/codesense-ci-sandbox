// SPDX-License-Identifier: MIT
/**
 * Pure arrangements of the resolved agent tree: workspace cards, management trees, or status.
 * The viewer store persists the person's global arrangement preference.
 */

import type { AgentTreeNode } from "@/features/viewer/lib/agent-list-view";
import { NO_WORKSPACE_ID } from "@/shared/lib/workspace-key";

/** Workspace cards (the default), manager trees, or status sections. */
export type AgentListArrange = "teams" | "status" | "manager";

/** The no-workspace bucket: an agent whose tab and story name no workspace. */
export { NO_WORKSPACE_ID };

/** Where a flat row is listed, for the workspace headers. */
export interface WorkspaceSessionGroup {
  /** The workspace record id (`generic:<dir>`, a story id, …), or {@link NO_WORKSPACE_ID}. */
  workspaceId: string;
  /** Display label: the story title / workspace name, or "No workspace" for the bucket. */
  label: string;
  /** Flat sessions in launch-tree order (so a manager always precedes its workers). */
  nodes: AgentTreeNode[];
  /** The workspace shape, for a header's icon. */
  kind: "story" | "folder" | "none";
  /** A story workspace's story status. */
  storyStatus?: string;
  /** A story workspace's story title: the label is its story id. */
  storyTitle?: string;
  /** The `owner/repo` the workspace works in, when it has one. */
  sourceRepoSlug?: string;
  /** The remote machine the workspace runs on; absent on This Mac. */
  machineId?: string;
}

export function groupByWorkspace(
  nodes: readonly AgentTreeNode[],
  _order?: readonly string[],
): WorkspaceSessionGroup[] {
  const groups = new Map<string, WorkspaceSessionGroup & { rank: number }>();
  for (const node of flattenNodes(nodes)) {
    const ws = node.workspace;
    const id = ws?.id ?? NO_WORKSPACE_ID;
    let group = groups.get(id);
    if (!group) {
      group = {
        workspaceId: id,
        label: ws?.label ?? "No workspace",
        kind: ws?.kind ?? "none",
        nodes: [],
        rank: node.workspaceRank ?? 0,
        ...(ws?.storyStatus ? { storyStatus: ws.storyStatus } : {}),
        ...(ws?.storyTitle ? { storyTitle: ws.storyTitle } : {}),
        ...(ws?.sourceRepoSlug ? { sourceRepoSlug: ws.sourceRepoSlug } : {}),
        ...(ws?.machineId ? { machineId: ws.machineId } : {}),
      };
      groups.set(id, group);
    }
    group.nodes.push({ ...node, children: [] });
  }
  return [...groups.values()].sort((a, b) => a.rank - b.rank);
}

/** Every node of the tree, depth-first, launched ones included. */
export function flattenNodes(nodes: readonly AgentTreeNode[]): AgentTreeNode[] {
  const out: AgentTreeNode[] = [];
  const walk = (list: readonly AgentTreeNode[]) => {
    for (const n of list) {
      out.push(n);
      if (n.children.length > 0) walk(n.children);
    }
  };
  walk(nodes);
  return out;
}

/** Keep the resolved management graph while projecting the plan's visible sessions. */
export function groupByManager(
  nodes: readonly AgentTreeNode[],
  _launchers?: Readonly<Record<string, string>>,
): AgentTreeNode[] {
  const flat = flattenNodes(nodes);
  const indexed = new Map(
    flat.map((node) => [
      node.projectedId ?? node.row.handle,
      { ...node, children: [] as AgentTreeNode[] },
    ]),
  );
  const roots: AgentTreeNode[] = [];
  for (const node of indexed.values()) {
    const parent = node.projectedParentId ? indexed.get(node.projectedParentId) : undefined;
    if (parent) parent.children.push(node);
    else roots.push(node);
  }
  return roots;
}
