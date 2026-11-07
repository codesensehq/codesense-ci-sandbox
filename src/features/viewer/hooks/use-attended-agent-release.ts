// SPDX-License-Identifier: MIT
import { useEffect } from "react";

import { isAttendedBackgrounded } from "@/features/viewer/hooks/use-corrected-agent-roster";
import { useTerminalTabIndex } from "@/features/viewer/lib/cli-workspace/store/terminal-selectors";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { logger } from "@/shared/lib/logger";
import { dashboardForegroundSession } from "@/tauri/commands";

/**
 * Release the background-limit slot of every agent the user is watching. An agent the
 * server still counts as backgrounded but that already has a live foreground tab is
 * attended, so it is foregrounded server-side (idempotent); the next roster fetch then
 * converges the flag that `useCorrectedAgentRoster` already corrects for display.
 *
 * Mounted once at the app root so the release happens whether or not any agent surface
 * is on screen, and fires once rather than once per roster consumer.
 */
export function useAttendedAgentRelease(): void {
  const sessions = useAgentListProjectionStore((s) => s.sessions);
  const terminalTabIndex = useTerminalTabIndex();
  useEffect(() => {
    if (!sessions) return;
    for (const r of sessions) {
      if (isAttendedBackgrounded(r, terminalTabIndex)) {
        dashboardForegroundSession(r.handle).catch((e) =>
          logger.debug("dashboard", `auto-foreground of an attended agent failed: ${String(e)}`),
        );
      }
    }
  }, [sessions, terminalTabIndex]);
}
