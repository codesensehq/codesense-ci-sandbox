// SPDX-License-Identifier: MIT
import { openManagementTab } from "@/shared/lib/open-management-tab";
import { useUiStore } from "@/shared/stores/ui-store";

/**
 * SW-UX (app-flow redesign) — the LIGHT openers for the Workspaces tab.
 *
 * Does NOT import `OrgPicker` (the heavy tab body lives in
 * `workspaces-virtual-tab.tsx`, imported only by App at boot). Keeps the openers
 * dependency-free so any entry point can call them without pulling the picker tree.
 */
export const WORKSPACES_VIRTUAL_PATH = "virtual://workspaces";

/**
 * Open (or focus) the Workspaces tab at `screen` — the create/join/home surface for
 * workspace management. SW-FU-B7: with no team (or no org) it opens in the no-team
 * tab host, so the menu item and ⌘⌥W always land on the tab. `keepLayout` as in
 * `openManagementTab`.
 */
export function openWorkspaces(
  screen: "home" | "create" | "join" = "home",
  opts?: { keepLayout?: boolean },
): void {
  useUiStore.getState().setWorkspacesScreen(screen);
  useUiStore.getState().closeSettings();
  openManagementTab(WORKSPACES_VIRTUAL_PATH, opts);
}
