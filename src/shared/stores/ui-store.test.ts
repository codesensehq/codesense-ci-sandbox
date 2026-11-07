// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it } from "vitest";

import { useUiStore } from "./ui-store";

function reset() {
  useUiStore.setState({
    orgSwitcherOpen: false,
    teamSwitcherOpen: false,
    workspacePaletteOpen: false,
    noteSwitcherOpen: false,
    headingJumpOpen: false,
    settingsPanelOpen: false,
    settingsInitialSection: null,
    sourceReposIntent: null,
    commandPaletteOpen: false,
  });
}

describe("ui-store note switcher (SW-SN-3)", () => {
  beforeEach(reset);

  it("setNoteSwitcherOpen(true) opens the switcher", () => {
    useUiStore.getState().setNoteSwitcherOpen(true);
    expect(useUiStore.getState().noteSwitcherOpen).toBe(true);
  });

  it("setNoteSwitcherOpen(false) closes only itself", () => {
    useUiStore.setState({ noteSwitcherOpen: true, orgSwitcherOpen: true });
    useUiStore.getState().setNoteSwitcherOpen(false);
    expect(useUiStore.getState().noteSwitcherOpen).toBe(false);
    // Closing does not spread CLOSED_SIBLINGS — an unrelated flag is untouched.
    expect(useUiStore.getState().orgSwitcherOpen).toBe(true);
  });

  it("opening the note switcher closes the other switchers (CLOSED_SIBLINGS mutex)", () => {
    useUiStore.setState({ orgSwitcherOpen: true, workspacePaletteOpen: true });
    useUiStore.getState().setNoteSwitcherOpen(true);
    expect(useUiStore.getState().noteSwitcherOpen).toBe(true);
    expect(useUiStore.getState().orgSwitcherOpen).toBe(false);
    expect(useUiStore.getState().workspacePaletteOpen).toBe(false);
  });

  it("opening another switcher closes the note switcher (reverse mutex)", () => {
    useUiStore.getState().setNoteSwitcherOpen(true);
    useUiStore.getState().setWorkspacePaletteOpen(true);
    expect(useUiStore.getState().workspacePaletteOpen).toBe(true);
    expect(useUiStore.getState().noteSwitcherOpen).toBe(false);
  });
});

describe("ui-store go-to-heading jump (SW-SN-6)", () => {
  beforeEach(reset);

  it("setHeadingJumpOpen(true) opens the jump modal", () => {
    useUiStore.getState().setHeadingJumpOpen(true);
    expect(useUiStore.getState().headingJumpOpen).toBe(true);
  });

  it("setHeadingJumpOpen(false) closes only itself", () => {
    useUiStore.setState({ headingJumpOpen: true, orgSwitcherOpen: true });
    useUiStore.getState().setHeadingJumpOpen(false);
    expect(useUiStore.getState().headingJumpOpen).toBe(false);
    expect(useUiStore.getState().orgSwitcherOpen).toBe(true);
  });

  it("opening the jump modal closes the other switchers (CLOSED_SIBLINGS mutex)", () => {
    useUiStore.setState({ noteSwitcherOpen: true, orgSwitcherOpen: true });
    useUiStore.getState().setHeadingJumpOpen(true);
    expect(useUiStore.getState().headingJumpOpen).toBe(true);
    expect(useUiStore.getState().noteSwitcherOpen).toBe(false);
    expect(useUiStore.getState().orgSwitcherOpen).toBe(false);
  });

  it("opening the note switcher closes the jump modal (reverse mutex)", () => {
    useUiStore.getState().setHeadingJumpOpen(true);
    useUiStore.getState().setNoteSwitcherOpen(true);
    expect(useUiStore.getState().noteSwitcherOpen).toBe(true);
    expect(useUiStore.getState().headingJumpOpen).toBe(false);
  });
});

describe("ui-store manage-teams intent (SW-UX: team-management tab)", () => {
  beforeEach(reset);

  it("setManageTeamsIntent seeds and clears the tab's open intent", () => {
    useUiStore.getState().setManageTeamsIntent("create");
    expect(useUiStore.getState().manageTeamsIntent).toBe("create");
    useUiStore.getState().setManageTeamsIntent(null);
    expect(useUiStore.getState().manageTeamsIntent).toBe(null);
  });

  it("is NOT a mutex flag — opening a switcher leaves the intent alone", () => {
    useUiStore.getState().setManageTeamsIntent("create");
    useUiStore.getState().setOrgSwitcherOpen(true);
    expect(useUiStore.getState().manageTeamsIntent).toBe("create");
    expect(useUiStore.getState().orgSwitcherOpen).toBe(true);
  });
});
