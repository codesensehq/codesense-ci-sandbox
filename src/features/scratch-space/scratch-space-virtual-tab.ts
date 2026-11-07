// SPDX-License-Identifier: MIT
import { openManagementTab } from "@/shared/lib/open-management-tab";
import { registerVirtualTab } from "@/shared/lib/virtual-viewer-registry";
import { useUiStore } from "@/shared/stores/ui-store";

import { ScratchSpaceTab } from "./components/scratch-space-tab";

/** The Scratch space tab's sentinel path. */
export const SCRATCH_SPACE_VIRTUAL_PATH = "virtual://scratch";

let registered = false;

/** Register the Scratch space tab under its virtual path. Idempotent. */
export function registerScratchSpaceTab(): void {
  if (registered) return;
  registered = true;
  registerVirtualTab(SCRATCH_SPACE_VIRTUAL_PATH, "Private", ScratchSpaceTab);
}

/** Open (or focus) the Scratch space tab, with or without an active team. */
export function openScratchSpace(): void {
  registerScratchSpaceTab();
  useUiStore.getState().closeSettings();
  openManagementTab(SCRATCH_SPACE_VIRTUAL_PATH);
}
