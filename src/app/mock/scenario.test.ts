// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { resolveScenario, DEFAULT_SCENARIO } from "./scenario";

describe("resolveScenario", () => {
  it("falls back to the default first-run start when no scenario param is present", () => {
    const scenario = resolveScenario("");
    expect(scenario.key).toBe(DEFAULT_SCENARIO);
    expect(scenario.key).toBe("fresh");
  });

  it("falls back to the default when the scenario param is unknown (a bad link degrades, never blanks)", () => {
    expect(resolveScenario("?scenario=bogus").key).toBe(DEFAULT_SCENARIO);
  });

  it("selects the named preset from the query string", () => {
    expect(resolveScenario("?scenario=noOrg").key).toBe("noOrg");
    expect(resolveScenario("?scenario=onboarded").key).toBe("onboarded");
  });

  it("climbs the empty-state ladder: each rung adds exactly one more prerequisite", () => {
    const noOrg = resolveScenario("?scenario=noOrg");
    expect(noOrg.hasOrg).toBe(false);

    const noTeam = resolveScenario("?scenario=noTeamInOrg");
    expect([noTeam.hasOrg, noTeam.hasTeam]).toEqual([true, false]);

    const noRepos = resolveScenario("?scenario=noSourceRepos");
    expect([noRepos.hasTeam, noRepos.sourceRepoCount]).toEqual([true, 0]);

    const noClone = resolveScenario("?scenario=noAttachedClone");
    expect([noClone.sourceRepoCount, noClone.cloneAttached]).toEqual([1, false]);

    const fresh = resolveScenario("?scenario=fresh");
    expect([fresh.cloneAttached, fresh.worktreePresent]).toEqual([true, false]);
  });

  it("a set-up team has a worktree config on disk; a fresh one does not", () => {
    expect(resolveScenario("?scenario=fresh").worktreePresent).toBe(false);
    expect(resolveScenario("?scenario=onboarded").worktreePresent).toBe(true);
  });
});
