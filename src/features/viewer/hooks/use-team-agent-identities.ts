// SPDX-License-Identifier: MIT
import { useAgentListNodes } from "@/features/viewer/lib/agent-list-projection";
import { useMemo } from "react";

import { useFaceTags } from "@/features/viewer/hooks/use-face-tags";
import { useAgentIdentityChoices } from "@/features/viewer/hooks/use-listed-agents";
import {
  resolveAgentFace,
  resolveAgentRole,
  type AgentFace,
  type AgentRole,
  type AgentTreeNode,
} from "@/features/viewer/lib/agent-list-view";
import { agentRowLabel } from "@/features/viewer/lib/agent-row-presentation";
import { agentKeyOfRevision } from "@/features/viewer/lib/agent-rows";
import { useAvatarPacks } from "@/shared/hooks/use-avatar-packs";
import type { AgentIdentityChoice } from "@/shared/lib/agent-identity-choice";
import type { FaceSource } from "@/shared/lib/avatar-packs";
import type { AgentSessionRow } from "@/tauri/commands";

/** One running agent of a team, as the agents list draws it. */
export interface TeamAgentIdentity {
  row: AgentSessionRow;
  /** How deep it sits in the launch tree (0 = top level). */
  depth: number;
  label: string;
  face: AgentFace;
  role: AgentRole;
  /** Its number among running agents wearing the same face. */
  tag?: number;
  /** The agent definition it runs, when its launch named one. */
  agentKey: string | null;
}

/**
 * A team's running agents in launch-tree order, each with the face, role and name the agents
 * list gives it, so a picker or a page outside the list shows them the same way. Reads the
 * shared roster; it never starts a fetch of its own.
 */
export function useTeamAgentIdentities(teamKey: string): TeamAgentIdentity[] {
  const nodes = useAgentListNodes(teamKey);
  const choices = useAgentIdentityChoices(teamKey);
  const avatars = useAvatarPacks();
  const faceTags = useFaceTags(teamKey);
  return useMemo(
    () => teamAgentIdentitiesOf(nodes, { choices, faces: avatars.faces, tags: faceTags }),
    [nodes, choices, avatars.faces, faceTags],
  );
}

/**
 * One team's launch trees as its running agents in launch-tree order, each with the face, role
 * and name its row wears: the role reads how many agents it has under it in the tree, as the row
 * does. The agents pane builds its Manager menu from its own trees with this, and
 * {@link useTeamAgentIdentities} from the team's.
 */
export function teamAgentIdentitiesOf(
  nodes: readonly AgentTreeNode[],
  read: {
    choices: Readonly<Record<string, AgentIdentityChoice>>;
    faces: FaceSource;
    tags: ReadonlyMap<string, number>;
  },
): TeamAgentIdentity[] {
  const out: TeamAgentIdentity[] = [];
  const walk = (list: readonly AgentTreeNode[], depth: number) => {
    for (const node of list) {
      const { row } = node;
      const label = agentRowLabel(row);
      const choice = read.choices[row.handle];
      out.push({
        row,
        depth,
        label,
        role: resolveAgentRole(row, { launched: node.children.length, choice }),
        face: resolveAgentFace(row, { name: label, choice, faces: read.faces }),
        tag: read.tags.get(row.handle),
        agentKey: agentKeyOfRevision(row.agentRevision),
      });
      walk(node.children, depth + 1);
    }
  };
  walk(nodes, 0);
  return out;
}

/**
 * The agents `handle` can be shown under: every other running agent of the team except the
 * ones below it (that would make a loop), in tree order.
 */
export function managerOptions(
  agents: readonly TeamAgentIdentity[],
  handle: string,
): TeamAgentIdentity[] {
  const at = agents.findIndex((a) => a.row.handle === handle);
  if (at < 0) return agents.filter((a) => a.row.handle !== handle);
  const depth = agents[at].depth;
  let end = at + 1;
  while (end < agents.length && agents[end].depth > depth) end++;
  return [...agents.slice(0, at), ...agents.slice(end)];
}
