// SPDX-License-Identifier: MIT
import { switchContext } from "@/features/viewer/lib/context-switch";
import { getActiveTeamKey } from "@/shared/lib/team-key";
import { useCallback } from "react";

import { focusActiveWorkspaceTerminal } from "@/features/viewer/lib/focus-cascade";
import type { SessionStory } from "@/features/viewer/lib/session-story";
import { showAgent } from "@/features/viewer/lib/show-agent";
import type { PipelineStatus } from "@/shared/lib/pipeline-graph/types";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { AgentSessionRow } from "@/tauri/commands";

/** The "drill into an agent" gestures a session row exposes. */
export interface UseAgentDrill {
  /** Show the agent ({@link showAgent}): its tab, restored or adopted into one, its team
   *  switched to, its run output, or a stated reason. */
  onLocate: (session: AgentSessionRow) => void;
  /** The same Show, under the name callers that adopt a tabless agent use. */
  onBringToForeground: (session: AgentSessionRow) => void;
  /** SW-AM-2: open the attached story's doc (never forces a foreground). */
  onOpenStory: (story: SessionStory) => void;
  /** SW-AM-1: edit the attached story's status inline (shared write path). */
  onStoryStatusChange: (story: SessionStory, status: PipelineStatus) => void;
  /** Reveal one tab of this team's workspace (a plain shell), focusing it. */
  onRevealTab: (workspaceId: string, tabId: string) => void;
}

/**
 * SW-SR-4 (AC-5) · The shared "drill into an agent" wiring, so every agent surface
 * (the agents list, the run output, the Activity board) navigates to a worker's
 * session/terminal through the EXACT same shipped, jank-free path instead of
 * re-implementing it: the one Show contract ({@link showAgent}).
 *
 * @param teamKey the surface's team, or `null` for the active team.
 * @param onAfterForeground reconcile hook fired after a foreground slot release —
 *   the Agents panel passes its `load` (re-enumerate); a read-only monitor passes
 *   its own refetch (or omits it).
 */
export function useAgentDrill(
  teamKey: string | null,
  onAfterForeground?: () => void,
): UseAgentDrill {
  const expandTerminalOnOpen = useViewerStore((s) => s.expandTerminalOnOpen);
  const openStoryDocForTeam = useViewerStore((s) => s.openStoryDocForTeam);
  const changeStoryStatus = useViewerStore((s) => s.changeStoryStatus);

  // Every drill is the one Show: focus, restore, adopt into a tab, switch team, open the run
  // output, or say why not. Never a silent no-op.
  const onLocate = useCallback(
    (session: AgentSessionRow) => {
      void showAgent(session, { teamKey, onAfterForeground });
    },
    [teamKey, onAfterForeground],
  );
  const onBringToForeground = onLocate;

  // SW-AM-2: open the story doc a session is attached to. Pure in-memory store
  // action — no Tauri/PTY call, so it can't block the UI thread or spin the CLI.
  const onOpenStory = useCallback(
    (story: SessionStory) => openStoryDocForTeam(story.teamKey, story.storyFilePath),
    [openStoryDocForTeam],
  );

  // SW-AM-1 (AC-3): edit story status inline from a row. Reuses the shipped
  // `changeStoryStatus` write path — identical behavior to the pill / tile / tray.
  const onStoryStatusChange = useCallback(
    (story: SessionStory, newStatus: PipelineStatus) => {
      void changeStoryStatus(story.teamKey, story.workspaceId, newStatus);
    },
    [changeStoryStatus],
  );

  const onRevealTab = useCallback(
    (workspaceId: string, tabId: string) => {
      if (!teamKey) return;
      switchContext({ teamKey, workspaceId, tabId }, "agent");
      if (getActiveTeamKey() !== teamKey) return;
      expandTerminalOnOpen(teamKey);
      focusActiveWorkspaceTerminal(teamKey);
    },
    [teamKey, expandTerminalOnOpen],
  );

  return {
    onLocate,
    onBringToForeground,
    onOpenStory,
    onStoryStatusChange,
    onRevealTab,
  };
}
