// SPDX-License-Identifier: MIT
import { useEffect } from "react";

import { resolveTerminalTheme } from "@/themes/terminal";
import {
  getViewerState,
  selectRunningTerminalTabIndex,
  subscribeViewerState,
  type TerminalTabIndex,
} from "@/features/viewer/lib/cli-workspace/store/viewer-access";
import { getEffectiveTerminalFontPx } from "@/shared/lib/font-sizes";
import { terminalManager, type TerminalManager } from "@/shared/terminal/terminal-manager";

type ViewerState = ReturnType<typeof getViewerState>;

/**
 * Every tab that is shown or Hidden has its display from the start, not only the
 * tab on screen: output that arrives while a tab is not shown lands in its
 * display, so showing the tab later is a slot move — never a first build from a
 * snapshot recorded at another width.
 */
export function useTerminalDisplayWarm(manager: TerminalManager = terminalManager): void {
  useEffect(() => {
    let running: TerminalTabIndex | null = null;
    let lives: ViewerState["terminalLives"] | null = null;
    const warm = (s: ViewerState) => {
      const nextRunning = selectRunningTerminalTabIndex(s);
      if (nextRunning === running && s.terminalLives === lives) return;
      running = nextRunning;
      lives = s.terminalLives;
      const { settings } = s;
      const base = {
        fontSizePx: getEffectiveTerminalFontPx(
          settings.terminalFontSize,
          settings.terminalFontSizePx,
        ),
        themeConfig: resolveTerminalTheme(
          settings.terminalTheme,
          settings.appTheme,
          document.documentElement.classList.contains("dark") ? "dark" : "light",
        ),
      };
      for (const byWorkspace of Object.values(running)) {
        for (const tabs of Object.values(byWorkspace)) {
          for (const tab of tabs) {
            void manager.ensure(tab.sessionId, { ...base, life: lives[tab.sessionId] ?? 0 });
          }
        }
      }
    };
    warm(getViewerState());
    return subscribeViewerState(warm);
  }, [manager]);
}
