// SPDX-License-Identifier: MIT
import { useMemo } from "react";

import type { AgentIdentityChoice } from "@/shared/lib/agent-identity-choice";
import { useViewerStore } from "@/shared/stores/viewer-store";

const NO_CHOICES: Readonly<Record<string, AgentIdentityChoice>> = Object.freeze({});

/**
 * The looks people chose for agents, by session handle: one team's, or (with no team) every
 * team's together, for a surface that does not know which team a session is in.
 */
export function useAgentIdentityChoices(
  teamKey?: string,
): Readonly<Record<string, AgentIdentityChoice>> {
  const all = useViewerStore((s) => s.agentIdentity);
  return useMemo(() => {
    if (!all) return NO_CHOICES;
    if (teamKey !== undefined) return all[teamKey] ?? NO_CHOICES;
    const teams = Object.values(all);
    return teams.length === 0 ? NO_CHOICES : Object.assign({}, ...teams);
  }, [all, teamKey]);
}
