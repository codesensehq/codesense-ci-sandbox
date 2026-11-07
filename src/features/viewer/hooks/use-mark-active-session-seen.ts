// SPDX-License-Identifier: MIT
import { useEffect } from "react";

import {
  selectWatchedSessionId,
  watchDotSince,
} from "@/features/viewer/lib/cli-workspace/store/terminal-tab-selectors";
import { useWindowFocused } from "@/shared/hooks/use-window-focused";
import { useActiveTeamKey } from "@/shared/lib/team-key";
import { markSessionSeen, useAgentStateStore } from "@/shared/stores/agent-state-store";
import { useViewerStore } from "@/shared/stores/viewer-store";

/**
 * Marks the session the user is looking at as seen: the active tab of the
 * active team's active, non-minimized workspace, while the CLI pane is shown and
 * the window is focused. It re-marks whenever that tab's dot changes
 * under the user's eyes (a crash in the watched tab is seen at once) and when
 * the pane is reopened. Also keeps each dot's start time current
 * (`watchDotSince`), which "seen" compares against.
 */
export function useMarkActiveSessionSeen(): void {
  useEffect(() => watchDotSince(), []);
  const teamKey = useActiveTeamKey();
  const focused = useWindowFocused();
  const sessionId = useViewerStore((s) => selectWatchedSessionId(s, teamKey));
  const since = useAgentStateStore((a) => (sessionId ? a.stateSince[sessionId] : undefined));
  useEffect(() => {
    if (focused && sessionId) markSessionSeen(sessionId);
  }, [focused, sessionId, since]);
}
