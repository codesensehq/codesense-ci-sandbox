// SPDX-License-Identifier: MIT
import { registerVirtualTab } from "@/shared/lib/virtual-viewer-registry";

import { SETTINGS_VIRTUAL_PATH } from "./lib/open-settings-tab";
import { SettingsPanel } from "./components/settings-panel";

/**
 * SW-UX (app-flow redesign) — Settings opens as a virtual viewer tab
 * (`virtual://settings`) instead of a full-screen overlay. It's a place you return
 * to (tweak a setting, go work, come back), so it belongs in the tab strip like
 * every other CodeSense surface. `renderViewer` resolves the sentinel path here.
 *
 * The tab hosts `SettingsPanel` (tab chrome only — no Back button / global Escape /
 * focus-restore). The legacy
 * `settingsPanelOpen` flag is now DERIVED (App keeps it in sync with "the settings
 * tab is the active tab") so the shortcut-gating / chrome consumers keep working.
 *
 * This module imports `SettingsPanel` (its whole subtree), so it is imported ONLY
 * by App at boot for registration. The lightweight opener (`openSettingsTab`) lives in `./lib/open-settings-tab` to avoid a cycle.
 */
export { SETTINGS_VIRTUAL_PATH } from "./lib/open-settings-tab";

/** The tab surface: the full Settings panel. */
function SettingsTab() {
  return <SettingsPanel />;
}

let registered = false;

/** Register the Settings surface under its virtual path. Idempotent. */
export function registerSettingsTab(): void {
  if (registered) return;
  registered = true;
  registerVirtualTab(SETTINGS_VIRTUAL_PATH, "Settings", SettingsTab);
}
