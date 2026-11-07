// SPDX-License-Identifier: MIT
import { useAgentRows } from "@/features/agents/lib/agent-names";
import { AgentFaceView } from "@/features/viewer/components/agent-identity";
import { resolveAgentFace } from "@/features/viewer/lib/agent-list-view";
import { useAvatarPacks } from "@/shared/hooks/use-avatar-packs";
import type { AgentRow } from "@/tauri/commands";

/** An agent definition's face, as the agents list deals it. */
export function AgentRowFace({
  agent,
  size = 18,
}: {
  agent: AgentRow;
  size?: number;
}): React.JSX.Element {
  const { faces } = useAvatarPacks();
  const face = resolveAgentFace(
    {
      avatar: agent.avatar,
      agentRevision: `${agent.key}@${agent.revision}`,
      handle: agent.key,
      displayName: agent.name,
    },
    { name: agent.name, faces },
  );
  return <AgentFaceView face={face} size={size} />;
}

/** The agent definition an agent's display name belongs to, if any. */
export function useAgentRowNamed(name: string | undefined): AgentRow | undefined {
  const { rows } = useAgentRows();
  if (!name || !rows) return undefined;
  const wanted = name.trim().toLowerCase();
  return rows.find((r) => !r.archived && r.name.trim().toLowerCase() === wanted);
}
