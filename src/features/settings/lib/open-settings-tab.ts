// SPDX-License-Identifier: MIT
import { openManagementTab } from "@/shared/lib/open-management-tab";
import { useUiStore } from "@/shared/stores/ui-store";
import type { SectionKey } from "@/features/settings/lib/types";

/**
 * SW-UX (app-flow redesign) — the LIGHT openers for the Settings tab.
 *
 * Deliberately does NOT import `SettingsPanel` (or its subtree). The tab COMPONENT
 * + registration live in `settings-virtual-tab.tsx` (imported only by App at boot).
 * Keeping the openers here breaks the import cycle that would otherwise form —
 * `shortcuts registry → open-settings → SettingsPanel → shortcuts-section →
 * registry` — since these openers only touch stores + the sentinel path, and App
 * has already registered the tab by the time any UI can call them.
 */
export const SETTINGS_VIRTUAL_PATH = "virtual://settings";

/**
 * Open (or focus) the Settings tab, jumped to `section`. Singleton: re-opening
 * focuses the existing tab and re-jumps via `settingsInitialSection`. With `row`, the row whose
 * `SettingRow` names it as its `target` scrolls into view and takes focus once it shows.
 *
 * Settings must be reachable BEFORE a team exists, so with no team it opens in the
 * no-team tab host (SW-FU-B7) — never team-gated, never a dialog.
 */
export function openSettingsTab(section: SectionKey = "general", row?: string): void {
  useUiStore.setState((s) => ({
    settingsInitialSection: section,
    settingsInitialRow: row ?? null,
    settingsJump: s.settingsJump + 1,
  }));
  openManagementTab(SETTINGS_VIRTUAL_PATH);
}
