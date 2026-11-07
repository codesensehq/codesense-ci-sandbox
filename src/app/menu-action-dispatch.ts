// SPDX-License-Identifier: MIT
import { beginContextNavigation } from "@/shared/lib/context-navigation";
import { handlePanelMenuAction } from "@/features/shell/right-pane/panel-menu";
import { isRowPopupFocused, editorOwnsShortcut } from "@/shared/lib/focus-utils";
import { isDispatchable } from "@/shared/lib/shortcuts/is-dispatchable";
import { findShortcutAction } from "@/shared/lib/shortcuts/registry";
import { NO_TEAM_SENTINEL } from "@/shared/lib/team-key";
import { getActiveWorkspaceContext } from "@/shared/hooks/use-active-workspace-context";
import { useUiStore } from "@/shared/stores/ui-store";

/**
 * Dispatch a native `menu:action` payload through the shortcut registry.
 *
 * On macOS a menu key-equivalent is consumed by the menu before the webview
 * keydown fires, so a menu-triggered action must obey the SAME gate the
 * keyboard handler enforces: workspace-scope actions need a focused workspace
 * in the active team, only a few global actions fire while focus is in a row popup of the Agents pane,
 * and on the Settings tab only the terminal-section workspace actions
 * (`SETTINGS_TAB_WORKSPACE_ALLOWED`) and the team actions outside
 * `SETTINGS_TAB_BLOCKED` dispatch, exactly as on the keyboard.
 * This routes through the same `isDispatchable` gate as `useShortcutHandler`,
 * sourcing the gate state imperatively because it runs outside the React tree.
 */
export function dispatchMenuAction(payload: string): void {
  // Panel toggles (id `panel:<id>`) route to the right-pane layout; everything
  // else dispatches via the shortcut registry.
  if (payload.startsWith("panel:")) beginContextNavigation();
  if (handlePanelMenuAction(payload)) return;
  const action = findShortcutAction(payload);
  if (!action || editorOwnsShortcut(action.id, document.activeElement)) return;

  // The active team's focused workspace (`focusedWorkspaceOf`), the same read the keyboard
  // handler gates on.
  const dispatchable = isDispatchable(action, getMenuActionGate());
  if (!dispatchable) return;

  action.run();
}

/** Read the current UI gate for imperative menu and iframe shortcut handlers. */
export function getMenuActionGate() {
  const { teamKey, workspace } = getActiveWorkspaceContext();
  return {
    settingsPanelOpen: useUiStore.getState().settingsPanelOpen,
    hasActiveWorkspace: workspace !== null,
    hasTeam: teamKey !== NO_TEAM_SENTINEL,
    rowPopupFocused: isRowPopupFocused(),
  };
}
