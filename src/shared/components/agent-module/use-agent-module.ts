// SPDX-License-Identifier: MIT
import { useCallback, useEffect } from "react";

import { useAgentWorkspaceSession } from "@/shared/hooks/use-agent-workspace-session";
import { useManagedSessionStore } from "@/shared/stores/use-managed-session-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import {
  addPanelAsTopSection,
  removePanel,
  isPanelVisible,
} from "@/features/shell/right-pane/right-pane-layout";
import { getPanel } from "@/features/shell/right-pane/panel-registry";
import { logger } from "@/shared/lib/logger";

import type { AgentModuleConfig, AgentModuleApi } from "./agent-module-config";

/**
 * SW-AW-5 (AC-4/AC-4a) — the HEADLESS, panel-agnostic half of the reusable agent
 * module. Binds one warm CLI-workspace agent at the caller's cwd via
 * `useAgentWorkspaceSession` (keyed by `session.targetWorkspace` → the AC-7
 * single-session mechanism) and returns the session handle plus imperative
 * `promote()`/`minimize()`.
 *
 * A consumer that wants the agent WITHOUT a panel (GS-12, headless background) uses
 * ONLY this hook — it never mounts the panel seam. GS-10 uses this hook + the panel
 * seam. Both share ONE bound session iff they pass the same `targetWorkspace`.
 *
 * Non-blocking (AC-4b): the underlying spawn is born-minimized/headless and connects
 * asynchronously inside the session config's `connect` closure — this hook never
 * `await`s on a render path.
 */
export function useAgentModule(config: AgentModuleConfig): AgentModuleApi {
  const session = useAgentWorkspaceSession(config.session);
  const { boundSessionId } = session;

  // Arm supervision when the agent binds — so `ask_user`/`signal_done`/needs-
  // attention transitions apply to the answerable-prompt card + task badge. This
  // lives in the hook (not the panel body) so a HEADLESS consumer arms it too.
  // `markRunning` only arms from unmanaged/running, never clobbering a terminal state.
  useEffect(() => {
    if (boundSessionId) {
      useManagedSessionStore.getState().markRunning(boundSessionId);
    }
  }, [boundSessionId]);

  const { panelId } = config;

  // Reactive: "minimized" = not currently surfaced as the active panel of an
  // expanded right-pane section (registered-but-hidden, collapsed, or absent).
  const layout = useViewerStore((s) => s.rightPaneLayout);
  const isMinimized = !isPanelVisible(layout, panelId);

  const promote = useCallback(() => {
    // AC-8 precondition: only a REGISTERED panel has a body. A headless-only
    // consumer never mounts the panel seam, so guard here — never add an orphan
    // empty section for an unregistered id.
    if (!getPanel(panelId)) {
      logger.debug("agent-module", `promote() ignored — panel "${panelId}" not registered`);
      return;
    }
    const store = useViewerStore.getState();
    store.setRightPaneLayout(addPanelAsTopSection(store.rightPaneLayout, panelId));
    // Surfacing the panel is pointless if the pane is closed (its container is
    // `display:none`) — open it, mirroring the View → Panels menu (`panel-menu.ts`).
    if (!store.rightPaneOpen) store.toggleRightPane();
  }, [panelId]);

  const minimize = useCallback(() => {
    const store = useViewerStore.getState();
    const next = removePanel(store.rightPaneLayout, panelId);
    store.setRightPaneLayout(next);
    // Close the pane if removing this panel emptied the layout (mirrors panel-menu).
    if (next.sections.length === 0 && store.rightPaneOpen) store.toggleRightPane();
  }, [panelId]);

  return { session, isMinimized, promote, minimize };
}
