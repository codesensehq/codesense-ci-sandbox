// SPDX-License-Identifier: MIT
import { CrewStack } from "@/features/viewer/components/agent-identity";
import { resolveAgentFace } from "@/features/viewer/lib/agent-list-view";
import { useAvatarPacks } from "@/shared/hooks/use-avatar-packs";
import type { ContextRepoStatus } from "@/tauri/commands";

export function ContextWorktreeCrew({
  agents,
}: {
  agents: NonNullable<ContextRepoStatus["agents"]>;
}) {
  const { faces } = useAvatarPacks();
  const crew = agents.slice(0, 4).map((agent) => ({
    key: agent.handle,
    name: agent.name,
    face: resolveAgentFace(
      {
        handle: agent.handle,
        displayName: agent.name,
        avatar: agent.avatar ?? null,
        agentRevision: null,
      },
      { name: agent.name, faces },
    ),
  }));
  return (
    <span
      role="img"
      aria-label={agents.map((a) => a.name).join(", ")}
      title={agents.map((a) => a.name).join(", ")}
    >
      <CrewStack
        lead={null}
        crew={crew}
        more={Math.max(0, agents.length - crew.length)}
        size={15}
      />
    </span>
  );
}
