// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";

import { openSettingsTab, SETTINGS_VIRTUAL_PATH } from "./open-settings-tab";
import { useOrgStore } from "@/shared/stores/org-store";
import { useUiStore } from "@/shared/stores/ui-store";
import { useViewerStore } from "@/shared/stores/viewer-store";

// SW-FU-B7: with no team (org active, no team) the openers open the Settings TAB in
// the no-team tab host — never a dialog — and otherwise open it in the team.
describe("open-settings-tab — no-team tab host (SW-FU-B7)", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    useUiStore.setState({
      noTeamTabs: [],
      noTeamActiveTab: null,
      settingsInitialSection: null,
    });
  });

  function setNoTeam() {
    // Active org, null team → teamKey `${slug}:_none`, `teamKeyHasTeam` false.
    useOrgStore.setState({ activeOrgSlug: "local/my-laptop", activeTeamName: null });
  }
  function setWithTeam() {
    useOrgStore.setState({ activeOrgSlug: "local/my-laptop", activeTeamName: "general" });
  }

  it("openSettingsTab opens the tab in the no-team host when no team is active", () => {
    setNoTeam();
    const openFile = vi.spyOn(useViewerStore.getState(), "openFile");
    openSettingsTab("general");
    expect(useUiStore.getState().noTeamActiveTab).toBe(SETTINGS_VIRTUAL_PATH);
    expect(useUiStore.getState().settingsInitialSection).toBe("general");
    // Never an unrenderable `_none` viewer tab.
    expect(openFile).not.toHaveBeenCalled();
  });

  it("openSettingsTab opens the viewer tab in the team when a team IS active", () => {
    setWithTeam();
    const openFile = vi.spyOn(useViewerStore.getState(), "openFile");
    openSettingsTab("general");
    expect(useUiStore.getState().noTeamTabs).toEqual([]);
    expect(openFile).toHaveBeenCalledWith(
      "local/my-laptop:general",
      "",
      SETTINGS_VIRTUAL_PATH,
      undefined,
    );
  });
});
