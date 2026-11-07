// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { bootHandlers } from "./boot";
import { resolveScenario } from "../scenario";
import type { LoadedOrg } from "@/shared/types/registry";
import type { OrgSummary } from "@/shared/types/domain";
import { numericPrincipal, type LocalProfileStatus } from "@/shared/stores/local-profile-store";
import { MOCK_USER } from "../fixtures";

const loadedOrg = (scenarioSearch: string) => {
  const h = bootHandlers(resolveScenario(scenarioSearch));
  const org = h.load_org_cmd({}) as LoadedOrg;
  // The mock always populates `org.org`; assert it so the ladder assertions
  // below read a non-null Org (the LoadedOrg contract allows null on failure).
  expect(org.org).not.toBeNull();
  return {
    orgs: h.discover_local_orgs({}) as OrgSummary[],
    org: org.org as NonNullable<LoadedOrg["org"]>,
  };
};

describe("bootHandlers scenario projection", () => {
  // SW-UX (app-flow redesign): the default "CodeSense" reference org is ALWAYS
  // discoverable (auto-added on launch), listed first, regardless of scenario.
  it("the CodeSense reference org is always present, listed first", () => {
    for (const s of ["noOrg", "noTeamInOrg", "fresh", "onboarded"]) {
      const orgs = loadedOrg(`?scenario=${s}`).orgs;
      expect(orgs[0]).toMatchObject({ slug: "codesense/docs", display_name: "CodeSense Docs" });
    }
  });

  it("noOrg: only the CodeSense reference org is discovered (user has none yet)", () => {
    const orgs = loadedOrg("?scenario=noOrg").orgs;
    expect(orgs).toHaveLength(1);
    expect(orgs[0].slug).toBe("codesense/docs");
  });

  it("noTeamInOrg: CodeSense + the user's org are discovered; the user's org has no teams", () => {
    const { orgs, org } = loadedOrg("?scenario=noTeamInOrg");
    expect(orgs).toHaveLength(2);
    expect(org.teams).toEqual([]);
  });

  it("noSourceRepos: the team exists but declares no source repos", () => {
    const { org } = loadedOrg("?scenario=noSourceRepos");
    expect(org.teams).toHaveLength(1);
    expect(org.teams[0].source_repos).toEqual([]);
  });

  it("fresh: the team declares its one source repo", () => {
    const { org } = loadedOrg("?scenario=fresh");
    expect(org.teams[0].source_repos).toHaveLength(1);
    expect(org.teams[0].source_repos[0]).toMatchObject({ owner: "acme", repo: "widgets" });
  });

  it("preserves a parseable org_root so codesenseHome derivation never errors", () => {
    const { org } = loadedOrg("?scenario=fresh");
    expect(org.org_root).toMatch(/\/users\/\d+\/orgs\//);
  });

  it("selects the signed-in mock user as the local profile, so principal-scoped reads resolve", () => {
    const status = bootHandlers(resolveScenario("")).local_profile_status({}) as LocalProfileStatus;
    expect(status.active).toEqual({
      principal: String(MOCK_USER.userId),
      displayName: MOCK_USER.displayName,
      machineOwner: { kind: "legacyGithub", id: MOCK_USER.userId },
    });
    expect(status.profiles).toEqual([status.active]);
    expect(numericPrincipal(status.active!.principal)).toBe(MOCK_USER.userId);
  });
});
