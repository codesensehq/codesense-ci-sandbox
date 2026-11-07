// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach } from "vitest";
import { useUiStore } from "./ui-store";

function resetStore() {
  useUiStore.setState({
    identityMenuOpen: false,
    orgSwitcherOpen: false,
    teamSwitcherOpen: false,
    settingsPanelOpen: false,
    settingsInitialSection: null,
    sourceReposIntent: null,
    noTeamTabs: [],
    noTeamActiveTab: null,
  });
}

beforeEach(() => {
  resetStore();
});

describe("ui-store", () => {
  it("all flags start false / null", () => {
    const s = useUiStore.getState();
    expect(s.orgSwitcherOpen).toBe(false);
    expect(s.teamSwitcherOpen).toBe(false);
    expect(s.settingsPanelOpen).toBe(false);
    expect(s.settingsInitialSection).toBe(null);
    expect(s.sourceReposIntent).toBe(null);
    expect(s.noTeamTabs).toEqual([]);
    expect(s.noTeamActiveTab).toBe(null);
  });

  it("setOrgSwitcherOpen sets the flag", () => {
    useUiStore.getState().setOrgSwitcherOpen(true);
    expect(useUiStore.getState().orgSwitcherOpen).toBe(true);
    useUiStore.getState().setOrgSwitcherOpen(false);
    expect(useUiStore.getState().orgSwitcherOpen).toBe(false);
  });

  it("toggleOrgSwitcher flips the flag", () => {
    useUiStore.getState().toggleOrgSwitcher();
    expect(useUiStore.getState().orgSwitcherOpen).toBe(true);
    useUiStore.getState().toggleOrgSwitcher();
    expect(useUiStore.getState().orgSwitcherOpen).toBe(false);
  });

  it("SW-OC-28: a click opens the identity menu; a section flag moves it; close clears every flag", () => {
    const ui = useUiStore.getState();
    ui.setIdentityMenuOpen(true);
    expect(useUiStore.getState().identityMenuOpen).toBe(true);
    // A switch shortcut while it's open moves the same menu to its section.
    useUiStore.getState().toggleTeamSwitcher();
    expect(useUiStore.getState()).toMatchObject({
      identityMenuOpen: false,
      teamSwitcherOpen: true,
    });
    useUiStore.setState({ identityMenuOpen: true, orgSwitcherOpen: true });
    useUiStore.getState().closeIdentityMenu();
    expect(useUiStore.getState()).toMatchObject({
      identityMenuOpen: false,
      orgSwitcherOpen: false,
      teamSwitcherOpen: false,
    });
  });

  it("opening the command palette closes the identity menu (mutex)", () => {
    useUiStore.getState().setIdentityMenuOpen(true);
    useUiStore.getState().setCommandPaletteOpen(true);
    expect(useUiStore.getState().identityMenuOpen).toBe(false);
  });

  it("toggleTeamSwitcher opens the team flag", () => {
    useUiStore.getState().toggleTeamSwitcher();
    expect(useUiStore.getState().teamSwitcherOpen).toBe(true);
  });

  it("setWorkspacePaletteOpen sets the flag (SW-EW-33 palette, ⌘K post-SW-FX-49)", () => {
    useUiStore.getState().setWorkspacePaletteOpen(true);
    expect(useUiStore.getState().workspacePaletteOpen).toBe(true);
    useUiStore.getState().setWorkspacePaletteOpen(false);
    expect(useUiStore.getState().workspacePaletteOpen).toBe(false);
  });

  it("opening the workspace palette closes the other switchers (mutex)", () => {
    useUiStore.setState({ orgSwitcherOpen: true, teamSwitcherOpen: true });
    useUiStore.getState().setWorkspacePaletteOpen(true);
    const s = useUiStore.getState();
    expect(s.workspacePaletteOpen).toBe(true);
    expect(s.orgSwitcherOpen).toBe(false);
    expect(s.teamSwitcherOpen).toBe(false);
  });

  it("opening a switcher closes the workspace palette (mutex, reverse)", () => {
    useUiStore.setState({ workspacePaletteOpen: true });
    useUiStore.getState().setOrgSwitcherOpen(true);
    expect(useUiStore.getState().workspacePaletteOpen).toBe(false);
  });

  describe("mutual exclusion mutex", () => {
    it("opening org switcher closes the team switcher", () => {
      useUiStore.setState({ teamSwitcherOpen: true });
      useUiStore.getState().setOrgSwitcherOpen(true);
      const s = useUiStore.getState();
      expect(s.orgSwitcherOpen).toBe(true);
      expect(s.teamSwitcherOpen).toBe(false);
    });

    it("opening team switcher closes the other switchers", () => {
      useUiStore.setState({ orgSwitcherOpen: true });
      useUiStore.getState().setTeamSwitcherOpen(true);
      const s = useUiStore.getState();
      expect(s.teamSwitcherOpen).toBe(true);
      expect(s.orgSwitcherOpen).toBe(false);
    });

    it("closing a flag (setX(false)) does NOT touch siblings", () => {
      useUiStore.setState({ orgSwitcherOpen: true, teamSwitcherOpen: true });
      useUiStore.getState().setOrgSwitcherOpen(false);
      const s = useUiStore.getState();
      expect(s.orgSwitcherOpen).toBe(false);
      expect(s.teamSwitcherOpen).toBe(true);
    });
  });

  describe("closeSettings", () => {
    function openSettingsState() {
      useUiStore.setState({
        settingsPanelOpen: true,
        settingsInitialSection: "workspace",
      });
    }

    it("closeSettings() resets the Settings fields", () => {
      openSettingsState();
      useUiStore.getState().closeSettings();
      const s = useUiStore.getState();
      expect(s.settingsPanelOpen).toBe(false);
      expect(s.settingsInitialSection).toBe(null);
    });

    it("setOrgSwitcherOpen(true) closes Settings", () => {
      openSettingsState();
      useUiStore.getState().setOrgSwitcherOpen(true);
      const s = useUiStore.getState();
      expect(s.settingsPanelOpen).toBe(false);
      expect(s.settingsInitialSection).toBe(null);
      expect(s.orgSwitcherOpen).toBe(true);
    });

    it("toggleTeamSwitcher closes Settings", () => {
      openSettingsState();
      useUiStore.getState().toggleTeamSwitcher();
      const s = useUiStore.getState();
      expect(s.settingsPanelOpen).toBe(false);
      expect(s.teamSwitcherOpen).toBe(true);
    });
  });

  describe("no-team tab host (SW-FU-B7)", () => {
    it("openNoTeamTab adds and activates a tab, de-duplicating by path", () => {
      const { openNoTeamTab } = useUiStore.getState();
      openNoTeamTab("virtual://settings");
      openNoTeamTab("virtual://workspaces");
      openNoTeamTab("virtual://settings");
      const s = useUiStore.getState();
      expect(s.noTeamTabs).toEqual(["virtual://settings", "virtual://workspaces"]);
      expect(s.noTeamActiveTab).toBe("virtual://settings");
    });

    it("closeNoTeamTab focuses the next tab, else the previous, else the empty state", () => {
      useUiStore.setState({
        noTeamTabs: ["virtual://a", "virtual://b", "virtual://c"],
        noTeamActiveTab: "virtual://b",
      });
      useUiStore.getState().closeNoTeamTab("virtual://b");
      expect(useUiStore.getState().noTeamActiveTab).toBe("virtual://c");
      useUiStore.getState().closeNoTeamTab("virtual://c");
      expect(useUiStore.getState().noTeamActiveTab).toBe("virtual://a");
      useUiStore.getState().closeNoTeamTab("virtual://a");
      expect(useUiStore.getState().noTeamTabs).toEqual([]);
      expect(useUiStore.getState().noTeamActiveTab).toBe(null);
    });

    it("closing an inactive tab keeps the active one", () => {
      useUiStore.setState({
        noTeamTabs: ["virtual://a", "virtual://b"],
        noTeamActiveTab: "virtual://a",
      });
      useUiStore.getState().closeNoTeamTab("virtual://b");
      expect(useUiStore.getState().noTeamActiveTab).toBe("virtual://a");
    });

    it("setNoTeamActiveTab ignores a path that is not open", () => {
      useUiStore.setState({ noTeamTabs: ["virtual://a"], noTeamActiveTab: "virtual://a" });
      useUiStore.getState().setNoTeamActiveTab("virtual://zzz");
      expect(useUiStore.getState().noTeamActiveTab).toBe(null);
      useUiStore.getState().setNoTeamActiveTab("virtual://a");
      expect(useUiStore.getState().noTeamActiveTab).toBe("virtual://a");
    });
  });

  describe("setSourceReposIntent (SW-UX: source-repos tab)", () => {
    it("sets and clears the intent that seeds the source-repos tab's reserve state", () => {
      useUiStore.getState().setSourceReposIntent("add");
      expect(useUiStore.getState().sourceReposIntent).toBe("add");
      useUiStore.getState().setSourceReposIntent("list");
      expect(useUiStore.getState().sourceReposIntent).toBe("list");
      useUiStore.getState().setSourceReposIntent(null);
      expect(useUiStore.getState().sourceReposIntent).toBe(null);
    });

    it("is NOT a mutex flag — opening a switcher leaves the intent alone", () => {
      // The source-repos surface is a viewer tab, not a sibling dialog, so its
      // intent is not cleared by CLOSED_SIBLINGS when another surface opens.
      useUiStore.getState().setSourceReposIntent("add");
      useUiStore.getState().setOrgSwitcherOpen(true);
      expect(useUiStore.getState().sourceReposIntent).toBe("add");
      expect(useUiStore.getState().orgSwitcherOpen).toBe(true);
    });
  });
});
