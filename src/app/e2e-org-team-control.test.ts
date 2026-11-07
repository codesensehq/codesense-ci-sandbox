// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { useOrgStore } from "@/shared/stores/org-store";
import { useUiStore } from "@/shared/stores/ui-store";
import { registerOrgTeamE2eControl } from "./e2e-org-team-control";

describe("SW-OC-B11 org/team/workspace e2e door", () => {
  beforeEach(() => {
    delete window.__CODESENSE_ORG_E2E__;
    useUiStore.getState().setOrgSwitcherOpen(false);
    useUiStore.getState().setTeamSwitcherOpen(false);
  });

  afterEach(() => {
    delete window.__CODESENSE_ORG_E2E__;
  });

  it("is absent until the webdriver door is registered", () => {
    // The door is armed ONLY from App.tsx's `MODE === "webdriver"` block, so in a
    // normal build (no register call) the control never lands on window.
    expect(window.__CODESENSE_ORG_E2E__).toBeUndefined();
  });

  it("registers the full switching control surface under the gate", () => {
    registerOrgTeamE2eControl();
    const door = window.__CODESENSE_ORG_E2E__;
    expect(door).toBeDefined();
    expect(typeof door?.openOrgSwitcher).toBe("function");
    expect(typeof door?.openTeamSwitcher).toBe("function");
    expect(typeof door?.createWorkspace).toBe("function");
    expect(typeof door?.setActiveOrg).toBe("function");
    expect(typeof door?.setActiveTeam).toBe("function");
    expect(typeof door?.createTeam).toBe("function");
    expect(typeof door?.activeSelection).toBe("function");
  });

  it("opens the org and team switchers via their store-controlled flags (no Radix pointer)", () => {
    registerOrgTeamE2eControl();
    const door = window.__CODESENSE_ORG_E2E__!;

    expect(useUiStore.getState().orgSwitcherOpen).toBe(false);
    door.openOrgSwitcher();
    expect(useUiStore.getState().orgSwitcherOpen).toBe(true);

    door.openTeamSwitcher();
    // Opening the team switcher closes sibling popovers (CLOSED_SIBLINGS).
    expect(useUiStore.getState().teamSwitcherOpen).toBe(true);
    expect(useUiStore.getState().orgSwitcherOpen).toBe(false);
  });

  it("drives active-org/active-team selection through the real store actions", () => {
    useOrgStore.setState({
      orgs: [
        {
          slug: "acme",
          display_name: "Acme",
          created_at: new Date(0).toISOString(),
          remote_url: null,
          config_flags: {},
          people: [],
          teams: [{ name: "general", source_repos: [] }],
          org_root: "/acme",
        },
      ],
      activeOrgSlug: null,
      activeTeamName: null,
      stateLoaded: true,
    });

    registerOrgTeamE2eControl();
    const door = window.__CODESENSE_ORG_E2E__!;

    door.setActiveOrg("acme");
    expect(door.activeSelection().activeOrgSlug).toBe("acme");

    door.setActiveTeam("general");
    expect(door.activeSelection().activeTeamName).toBe("general");
  });
});
