// SPDX-License-Identifier: MIT
import { useEffect } from "react";

import {
  getViewerState,
  selectMinimizedTerminalTabIndex,
  selectRunningTerminalTabIndex,
  subscribeViewerState,
  type TerminalTabIndex,
} from "@/features/viewer/lib/cli-workspace/store/viewer-access";
import { terminalManager, type TerminalManager } from "@/shared/terminal/terminal-manager";

function collect(index: TerminalTabIndex, into: Set<string>): void {
  for (const byWorkspace of Object.values(index)) {
    for (const tabs of Object.values(byWorkspace)) for (const tab of tabs) into.add(tab.sessionId);
  }
}

/** Every session a tab still holds: shown, Hidden, or in a minimized workspace. */
export function tabbedSessionIds(s: ReturnType<typeof getViewerState>): Set<string> {
  const ids = new Set<string>();
  collect(selectRunningTerminalTabIndex(s), ids);
  collect(selectMinimizedTerminalTabIndex(s), ids);
  return ids;
}

/**
 * A session's display lives as long as its tab: once no tab holds the session
 * (closed here, closed by another viewer, or its host is gone), its display is
 * disposed. Hiding a tab or minimizing its workspace keeps it.
 */
export function useTerminalDisplayGc(manager: TerminalManager = terminalManager): void {
  useEffect(() => {
    const initial = getViewerState();
    let running = selectRunningTerminalTabIndex(initial);
    let minimized = selectMinimizedTerminalTabIndex(initial);
    let known = tabbedSessionIds(initial);
    return subscribeViewerState((s) => {
      const nextRunning = selectRunningTerminalTabIndex(s);
      const nextMinimized = selectMinimizedTerminalTabIndex(s);
      if (nextRunning === running && nextMinimized === minimized) return;
      running = nextRunning;
      minimized = nextMinimized;
      const next = tabbedSessionIds(s);
      for (const id of known) if (!next.has(id)) manager.dispose(id);
      known = next;
    });
  }, [manager]);
}
