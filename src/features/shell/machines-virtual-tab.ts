// SPDX-License-Identifier: MIT
import { openManagementTab } from "@/shared/lib/open-management-tab";
import { registerVirtualTab } from "@/shared/lib/virtual-viewer-registry";
import { useUiStore } from "@/shared/stores/ui-store";

import { MachinesTab } from "./components/machines-tab";

/**
 * SW-UX (app-flow redesign) — the unified Machines surface opens as a virtual
 * viewer tab (`virtual://machines`) instead of a modal. Managing machines is
 * a place you return to, so it lives in the tab strip like every other CodeSense
 * surface: `renderViewer` resolves the sentinel path via the virtual-tab registry.
 *
 * The surface is app-global in concept; the tab lives in the active team's content
 * area, or in the no-team tab host when no team is active (SW-FU-B7).
 */
export const MACHINES_VIRTUAL_PATH = "virtual://machines";

let registered = false;

/** Register the Machines surface under its virtual path. Idempotent. */
export function registerMachinesTab(): void {
  if (registered) return;
  registered = true;
  registerVirtualTab(MACHINES_VIRTUAL_PATH, "Machines", MachinesTab);
}

/**
 * Open (or focus) the Machines surface as a tab. Singleton: re-opening focuses
 * the existing tab. Works with or without an active team.
 */
export function openMachines(): void {
  registerMachinesTab();
  useUiStore.getState().closeSettings();
  openManagementTab(MACHINES_VIRTUAL_PATH);
}
