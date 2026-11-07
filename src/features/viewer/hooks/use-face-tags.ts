// SPDX-License-Identifier: MIT
import { useMemo } from "react";

import { agentRowLabel } from "@/features/viewer/lib/agent-row-presentation";
import { duplicateFaceTags, resolveAgentFace } from "@/features/viewer/lib/agent-list-view";
import {
  useMinimizedTerminalTabIndex,
  useRunningTerminalTabIndex,
} from "@/features/viewer/lib/cli-workspace/store/terminal-selectors";
import { isAgentSession } from "@/features/viewer/lib/hand-agents";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { useAgentIdentityChoices } from "@/features/viewer/hooks/use-listed-agents";
import { useAvatarPacks } from "@/shared/hooks/use-avatar-packs";
import { findTabLocationBySessionId, type TerminalTabIndex } from "@/shared/stores/viewer-store";
import type { AgentIdentityChoice } from "@/shared/lib/agent-identity-choice";
import type { AgentSessionRow } from "@/tauri/commands";

const NO_TAGS: ReadonlyMap<string, number> = new Map();

/**
 * The number each of a team's running agents wears when another of them shows the same face
 * ({@link duplicateFaceTags}), by session handle. The team's agents are those whose tab sits
 * in one of its workspaces, plus those with no tab at all (the agents list shows them with the
 * team). The agents list, the workspace pills and the tile tabs all read this, so one agent
 * carries one number everywhere. `teamKey` null (a surface that cannot place itself in a
 * team) counts every running agent.
 */
export function useFaceTags(teamKey: string | null): ReadonlyMap<string, number> {
  const sessions = useAgentListProjectionStore((s) => s.sessions);
  const choices = useAgentIdentityChoices(teamKey ?? undefined);
  const { faces } = useAvatarPacks();
  const running = useRunningTerminalTabIndex();
  const minimized = useMinimizedTerminalTabIndex();
  return useMemo(
    () => faceTagsFor(teamKey, { sessions, choices, faces, running, minimized }),
    [teamKey, sessions, choices, faces, running, minimized],
  );
}

/** The inputs {@link faceTagsFor} reads, all from the stores. */
export interface FaceTagInput {
  sessions: readonly AgentSessionRow[] | null;
  choices: Readonly<Record<string, AgentIdentityChoice>>;
  faces: ReturnType<typeof useAvatarPacks>["faces"];
  running: TerminalTabIndex;
  minimized: TerminalTabIndex;
}

/**
 * The face tags of one team's running agents, from the stores. Pure, so the agents list can
 * compute every team's tags at once: a team's agents are the ones whose tab sits in that team.
 */
export function faceTagsFor(
  teamKey: string | null,
  input: FaceTagInput,
): ReadonlyMap<string, number> {
  const { sessions, choices, faces, running, minimized } = input;
  if (!sessions) return NO_TAGS;
  const entries = sessions.flatMap((row) => {
    if (!isAgentSession(row)) return [];
    const loc = row.sessionId
      ? (findTabLocationBySessionId(running, row.sessionId) ??
        findTabLocationBySessionId(minimized, row.sessionId))
      : null;
    if (teamKey !== null && loc && loc.teamKey !== teamKey) return [];
    const face = resolveAgentFace(row, {
      name: agentRowLabel(row),
      choice: choices[row.handle],
      faces,
    });
    return [{ handle: row.handle, startedAtMs: row.startedAtMs, face }];
  });
  const tags = duplicateFaceTags(entries);
  return tags.size === 0 ? NO_TAGS : tags;
}

/** The team whose workspace holds a session's tab, if any. */
export function useTabTeamOf(sessionId: string | undefined): string | null {
  const running = useRunningTerminalTabIndex();
  const minimized = useMinimizedTerminalTabIndex();
  return useMemo(() => {
    if (!sessionId) return null;
    return (
      (
        findTabLocationBySessionId(running, sessionId) ??
        findTabLocationBySessionId(minimized, sessionId)
      )?.teamKey ?? null
    );
  }, [sessionId, running, minimized]);
}
