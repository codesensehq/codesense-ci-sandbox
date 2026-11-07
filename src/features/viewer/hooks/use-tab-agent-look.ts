// SPDX-License-Identifier: MIT
import { useMemo } from "react";

import type { AgentLook } from "@/features/viewer/components/agent-identity-menus";
import { useTabTeamOf } from "@/features/viewer/hooks/use-face-tags";
import { useAgentIdentityChoices } from "@/features/viewer/hooks/use-listed-agents";
import { isAgentSession } from "@/features/viewer/lib/hand-agents";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { useAvatarPacks } from "@/shared/hooks/use-avatar-packs";
import { useViewerStore } from "@/shared/stores/viewer-store";

/**
 * The Role and Face a person can give the agent running in a terminal tab, kept with the
 * team whose workspace holds the tab — the same choice its agents-list row reads and sets.
 * `undefined` for a plain shell, or a tab no team holds.
 */
export function useTabAgentLook(sessionId: string | undefined): AgentLook | undefined {
  const sessions = useAgentListProjectionStore((s) => s.sessions);
  const teamKey = useTabTeamOf(sessionId);
  const choices = useAgentIdentityChoices(teamKey ?? undefined);
  const avatars = useAvatarPacks();
  const setAgentIdentity = useViewerStore((s) => s.setAgentIdentity);
  return useMemo(() => {
    if (!sessionId || teamKey === null) return undefined;
    const row = sessions?.find((r) => isAgentSession(r) && r.sessionId === sessionId);
    if (!row) return undefined;
    return {
      choice: choices[row.handle],
      avatars,
      onChoose: (patch) => setAgentIdentity(teamKey, row.handle, patch),
    };
  }, [sessionId, teamKey, sessions, choices, avatars, setAgentIdentity]);
}
