// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";

import { openAppSettings } from "./open-settings";

// SW-UX (app-flow redesign): Settings is the `virtual://settings` TAB now, so the
// helper just opens/focuses that tab (via openSettingsTab). The legacy focus-capture
// trigger is a no-op for a tab.
const { mockOpenSettingsTab } = vi.hoisted(() => ({ mockOpenSettingsTab: vi.fn() }));
vi.mock("@/features/settings/lib/open-settings-tab", () => ({
  openSettingsTab: mockOpenSettingsTab,
}));

describe("openAppSettings", () => {
  beforeEach(() => {
    mockOpenSettingsTab.mockClear();
  });

  it("defaults section to 'general' when called with no args", () => {
    openAppSettings();
    expect(mockOpenSettingsTab).toHaveBeenCalledTimes(1);
    expect(mockOpenSettingsTab).toHaveBeenCalledWith("general");
  });

  it("opens the settings tab at the requested section", () => {
    openAppSettings("account");
    expect(mockOpenSettingsTab).toHaveBeenCalledWith("account");
  });

  it("ignores the legacy trigger arg (no overlay focus to restore for a tab)", () => {
    const explicit = document.createElement("button");
    openAppSettings("workspace", explicit);
    expect(mockOpenSettingsTab).toHaveBeenCalledWith("workspace");
  });
});
