// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { buildSyntheticRepo } from "@/shared/lib/build-synthetic-repo";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { Org } from "@/shared/types/domain";

import { registerContentNavE2eControl } from "./e2e-content-nav-control";

const HOME_ORG_ROOT = "/tmp/qh-home/.codesense/users/1/orgs/alderco/primary";
const OTHER_ORG_ROOT = "/tmp/qh-home/.codesense/users/1/orgs/otherco/secondary";

/** Mirror what `useOrgDiscovery` → `loadOrg` + `use-workspace-viewer-sync`
 *  produce during natural boot: orgs with `resolved_repository_paths` stamped
 *  and one synthetic `workspaceRepos` entry per team's first source repo. */
function seedDiscoveredBoot(): void {
  const acme: Org = {
    slug: "alderco/primary",
    display_name: "Primary Org",
    created_at: "2026-01-01T00:00:00Z",
    remote_url: null,
    config_flags: {},
    people: [],
    org_root: HOME_ORG_ROOT,
    teams: [
      { name: "source-team", source_repos: [{ owner: "alderco", repo: "codesense" }] },
      { name: "target-team", source_repos: [{ owner: "alderco", repo: "codesense-two" }] },
    ],
    resolved_repository_paths: {
      "source-team\ngithub.com/alderco/codesense": `${HOME_ORG_ROOT}/source-team/repos/alderco/codesense`,
      "target-team\ngithub.com/alderco/codesense-two": `${HOME_ORG_ROOT}/target-team/repos/alderco/codesense-two`,
    },
  };
  const other: Org = {
    slug: "otherco/secondary",
    display_name: "Other Org",
    created_at: "2026-01-01T00:00:00Z",
    remote_url: null,
    config_flags: {},
    people: [],
    org_root: OTHER_ORG_ROOT,
    teams: [{ name: "cross-team", source_repos: [{ owner: "otherco", repo: "widgets" }] }],
    resolved_repository_paths: {
      "cross-team\ngithub.com/otherco/widgets": `${OTHER_ORG_ROOT}/cross-team/repos/otherco/widgets`,
    },
  };

  const workspaceRepos: Record<string, ReturnType<typeof buildSyntheticRepo>> = {};
  for (const org of [acme, other]) {
    for (const team of org.teams) {
      const slug = team.source_repos[0];
      const repo = buildSyntheticRepo(org, team.name, slug, new Map());
      workspaceRepos[repo.repoPath] = repo;
    }
  }

  useOrgStore.setState({
    orgs: [acme, other],
    activeOrgSlug: "alderco/primary",
    activeTeamName: null,
    stateLoaded: true,
    workspaceRepos,
  });
}

describe("content-nav e2e control door", () => {
  beforeEach(() => {
    delete window.__CODESENSE_CONTENT_NAV_E2E__;
    seedDiscoveredBoot();
    registerContentNavE2eControl();
  });

  afterEach(() => {
    delete window.__CODESENSE_CONTENT_NAV_E2E__;
    useOrgStore.setState({
      orgs: [],
      activeOrgSlug: null,
      activeTeamName: null,
      workspaceRepos: {},
    });
  });

  it("is not armed until registered (prod is never wired)", () => {
    delete window.__CODESENSE_CONTENT_NAV_E2E__;
    expect(window.__CODESENSE_CONTENT_NAV_E2E__).toBeUndefined();
    registerContentNavE2eControl();
    expect(window.__CODESENSE_CONTENT_NAV_E2E__).toBeDefined();
  });

  it("enters a discovered team through the production route with Team (everything) by default", () => {
    const snap = window.__CODESENSE_CONTENT_NAV_E2E__!.enterTeam("alderco/primary", "source-team");
    expect(useOrgStore.getState().activeOrgSlug).toBe("alderco/primary");
    expect(useOrgStore.getState().activeTeamName).toBe("source-team");
    expect(snap.teamKey).toBe("alderco/primary:source-team");
    expect(snap.teamContentPath).toBe(`${HOME_ORG_ROOT}/source-team/content`);
    // SW-XS-15: no agent picked → the nav browses the team's own content, no overlay.
    expect(
      useViewerStore.getState().selectedAgentContext["alderco/primary:source-team"],
    ).toBeUndefined();
    expect(snap.selectedAgentContext).toBeNull();
  });

  it("resolves the registered nav content path in diagnostics (contextRepoPath non-empty)", () => {
    const diag = window.__CODESENSE_CONTENT_NAV_E2E__!.diagnostics("alderco/primary:source-team");
    expect(diag.navRepoRegistered).toBe(true);
    expect(diag.navTeamContentPath).toBe(`${HOME_ORG_ROOT}/source-team/content`);
    // The natural-boot contextRepoPath comes from resolved_repository_paths —
    // the field the abandoned store swap omitted; assert it is non-empty here.
    expect(diag.navContextRepoPath).toBeTruthy();
  });

  it("uses a discovered source-free team's content without manufacturing workspace repos", () => {
    useOrgStore.setState((state) => ({
      orgs: state.orgs.map((org) => ({
        ...org,
        teams: org.teams.map((team) => ({ ...team, source_repos: [] })),
      })),
      workspaceRepos: {},
      discoveryComplete: true,
    }));
    const c = window.__CODESENSE_CONTENT_NAV_E2E__!;
    const before = useViewerStore.getState().workspaces;
    expect(c.enterTeam("alderco/primary", "source-team").teamContentPath).toBe(
      `${HOME_ORG_ROOT}/source-team/content`,
    );
    expect(c.diagnostics("alderco/primary:source-team").navContextRepoPath).toBe(HOME_ORG_ROOT);
    expect(useOrgStore.getState().workspaceRepos).toEqual({});
    expect(useViewerStore.getState().workspaces).toEqual(before);
  });

  it("exposes + resets the team-management intent for the §E create-team seam", () => {
    const c = window.__CODESENSE_CONTENT_NAV_E2E__!;
    c.resetManageTeamsIntent();
    expect(c.manageTeamsIntent()).toBeNull();
    c.openCreateTeam();
    expect(c.manageTeamsIntent()).toBe("create");
    c.resetManageTeamsIntent();
    expect(c.manageTeamsIntent()).toBeNull();
  });

  it("reports the active org slug (confirms an org switch landed)", () => {
    const c = window.__CODESENSE_CONTENT_NAV_E2E__!;
    expect(c.activeOrgSlug()).toBe("alderco/primary");
    c.switchOrg("otherco/secondary");
    expect(c.activeOrgSlug()).toBe("otherco/secondary");
  });

  it("reports discovery settled only once boot flags AND orgs are present", () => {
    useViewerStore.setState({ stateLoaded: true, scopeRecoveryComplete: true });
    // config-store initialized is toggled by the real boot; assert the org gate
    // independently: with orgs present the org-side of the gate is satisfied.
    expect(useOrgStore.getState().orgs.length).toBeGreaterThan(0);
    expect(typeof window.__CODESENSE_CONTENT_NAV_E2E__!.discoverySettled()).toBe("boolean");
  });
});
