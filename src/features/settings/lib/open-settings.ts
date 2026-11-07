// SPDX-License-Identifier: MIT
import type { SectionKey } from "@/features/settings/lib/types";
import { openSettingsTab } from "@/features/settings/lib/open-settings-tab";

/**
 * Open the app-wide Settings surface, jumped to `section`.
 *
 * SW-UX (app-flow redesign): Settings is now the `virtual://settings` TAB (not an
 * overlay), so this opens/focuses that tab in the active team. The old `trigger`
 * focus-capture is a no-op for a tab (no overlay to restore focus from on close),
 * so it's accepted for source compatibility but ignored.
 */
export function openAppSettings(
  section: SectionKey = "general",
  _trigger?: HTMLElement | null,
): void {
  openSettingsTab(section);
}
