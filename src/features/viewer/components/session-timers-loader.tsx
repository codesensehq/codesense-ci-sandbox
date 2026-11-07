// SPDX-License-Identifier: MIT
import { useEffect, useMemo } from "react";

import type { TerminalTab, TerminalTabIndex } from "@/shared/stores/viewer-store";
import {
  useMinimizedTerminalTabIndex,
  useRunningTerminalTabIndex,
} from "@/features/viewer/lib/cli-workspace/store/terminal-selectors";

import { useTabTimerAgents } from "../hooks/use-tab-timer-agent";
import { useKeepTabTimers } from "../hooks/use-session-timers";
import { timerTargetKey } from "../lib/session-timers";
import { watchSessionTimers } from "../stores/session-timers-store";

/** Every running tab (shown, Hidden, or in a minimized workspace), once, by
 *  machine + session: session ids are only unique per terminal server. */
function allTabs(indexes: TerminalTabIndex[]): TerminalTab[] {
  const seen = new Map<string, TerminalTab>();
  for (const index of indexes)
    for (const byWorkspace of Object.values(index))
      for (const tabs of Object.values(byWorkspace))
        for (const tab of tabs) {
          const key = timerTargetKey({
            sessionId: tab.sessionId,
            machineId: tab.origin?.machineId,
          });
          if (!seen.has(key)) seen.set(key, tab);
        }
  return [...seen.values()];
}

/** One agent tab's timers, kept current (exported for tests). */
export function KeepTabTimers({ tab }: { tab: TerminalTab }) {
  useKeepTabTimers(tab);
  return null;
}

/**
 * SW-AGR-6: keeps the timers of every agent tab current — saved or unnamed,
 * local or remote, on screen or not — so a tab pill's glyph, a background
 * tab's wake marker and the caption chip all read the same state. Mounted
 * once by the app. Plain shells and headless runs are never asked.
 */
export function SessionTimersLoader() {
  const running = useRunningTerminalTabIndex();
  const minimized = useMinimizedTerminalTabIndex();
  const agents = useTabTimerAgents();
  const tabs = useMemo(
    () => allTabs([running, minimized]).filter((t) => agents.has(t.sessionId)),
    [running, minimized, agents],
  );
  // Deliveries (the caption, the scrollback marker) are heard for every tab.
  useEffect(() => watchSessionTimers(), []);
  return (
    <>
      {tabs.map((tab) => (
        <KeepTabTimers
          key={timerTargetKey({ sessionId: tab.sessionId, machineId: tab.origin?.machineId })}
          tab={tab}
        />
      ))}
    </>
  );
}
