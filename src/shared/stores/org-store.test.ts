// SPDX-License-Identifier: MIT
import { CODESENSE_DOCS_SLUG } from "@/shared/lib/codesense-docs";
import { vi, describe, it, expect, beforeEach, afterEach } from "vitest";
import { useOrgStore, resolveRestoredTeam, defaultTeamForOrg } from "./org-store";
import { useViewerStore } from "./viewer-store";
import { warmScopes } from "@/shared/lib/warm-scopes";
import { teamPaneVisible } from "@/features/home-base/lib/home-base-routing";
import { loadAppState, saveAppState } from "@/tauri/commands";
import { commitRegistryEdit } from "@/shared/lib/commit-registry-edit";
import { getCurrentUser } from "@/shared/lib/get-current-user";
import type { Org } from "@/shared/types/domain";
import {
  installTerminalLayoutHarness,
  seedTerminalTabs,
  terminalHost,
} from "@/test/terminal-layout-harness";

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn(() => Promise.resolve()),
  loadAppState: vi.fn(() => Promise.resolve("{}")),
  getRepoPermissions: vi.fn(),
  parseGitRemote: vi.fn(),
}));

vi.mock("@/shared/lib/commit-registry-edit", () => ({
  commitRegistryEdit: vi.fn(() => Promise.resolve()),
}));

vi.mock("@/shared/lib/get-current-user", () => ({
  getCurrentUser: vi.fn(() => ({ github_user_id: 42, github_username: "creator" })),
}));

const mockedCommitRegistryEdit = vi.mocked(commitRegistryEdit);
const mockedGetCurrentUser = vi.mocked(getCurrentUser);

const mockedSaveAppState = vi.mocked(saveAppState);
const mockedLoadAppState = vi.mocked(loadAppState);

installTerminalLayoutHarness();

function makeOrg(overrides: Partial<Org> = {}): Org {
  return {
    slug: "acme",
    display_name: "Acme Corp",
    created_at: "2026-01-01T00:00:00Z",
    org_root: "/fake/org",
    remote_url: null,
    config_flags: {},
    teams: [
      {
        name: "default",
        source_repos: [
          { owner: "alderco", repo: "codesense" },
          { owner: "org", repo: "other" },
        ],
      },
    ],
    people: [],
    ...overrides,
  };
}

describe("removeTeamSourceRepo", () => {
  beforeEach(() => {
    useOrgStore.setState({
      orgs: [makeOrg()],
      workspaceRepos: {
        "workspace:acme:default:alderco/codesense": {
          repoPath: "workspace:acme:default:alderco/codesense",
          repoName: "codesense (Acme Corp)",
          contextRepoPath: "/fake/org/_team/default/repos/alderco/codesense",
          teamContentPath: "",
          hasRemote: false,
          remoteUrl: null,
          mainDoc: "",
          localPath: null,
        },
        "workspace:acme:default:org/other": {
          repoPath: "workspace:acme:default:org/other",
          repoName: "other (Acme Corp)",
          contextRepoPath: "/fake/org/_team/default/repos/org/other",
          teamContentPath: "",
          hasRemote: false,
          remoteUrl: null,
          mainDoc: "",
          localPath: null,
        },
      },
    });
  });

  it("removes slug from source_repos and synthetic from workspaceRepos", () => {
    useOrgStore.getState().removeTeamSourceRepo("acme", "default", {
      owner: "alderco",
      repo: "codesense",
    });

    const state = useOrgStore.getState();
    const team = state.orgs[0].teams[0];
    expect(team.source_repos).toHaveLength(1);
    expect(team.source_repos[0]).toEqual({ owner: "org", repo: "other" });
    expect(state.workspaceRepos["workspace:acme:default:alderco/codesense"]).toBeUndefined();
    expect(state.workspaceRepos["workspace:acme:default:org/other"]).toBeDefined();
  });

  it("matches case-insensitively", () => {
    useOrgStore.getState().removeTeamSourceRepo("acme", "default", {
      owner: "ALDERCO",
      repo: "CODESENSE",
    });

    const team = useOrgStore.getState().orgs[0].teams[0];
    expect(team.source_repos).toHaveLength(1);
    expect(team.source_repos[0]).toEqual({ owner: "org", repo: "other" });
  });

  it("no-op if slug not found", () => {
    useOrgStore.getState().removeTeamSourceRepo("acme", "default", {
      owner: "nonexistent",
      repo: "repo",
    });

    const state = useOrgStore.getState();
    const team = state.orgs[0].teams[0];
    expect(team.source_repos).toHaveLength(2);
    expect(Object.keys(state.workspaceRepos)).toHaveLength(2);
  });

  it("no-op if org not found", () => {
    useOrgStore.getState().removeTeamSourceRepo("unknown", "default", {
      owner: "alderco",
      repo: "codesense",
    });

    const team = useOrgStore.getState().orgs[0].teams[0];
    expect(team.source_repos).toHaveLength(2);
  });

  it("no-op if team not found", () => {
    useOrgStore.getState().removeTeamSourceRepo("acme", "nonexistent", {
      owner: "alderco",
      repo: "codesense",
    });

    const team = useOrgStore.getState().orgs[0].teams[0];
    expect(team.source_repos).toHaveLength(2);
  });
});

// ---------------------------------------------------------------------------
// T-13: recentlyRemovedRepos (slug-stale-after-removal disambiguation)
// ---------------------------------------------------------------------------

describe("recentlyRemovedRepos", () => {
  beforeEach(() => {
    useOrgStore.setState({
      orgs: [makeOrg()],
      recentlyRemovedRepos: {},
      workspaceRepos: {
        "workspace:acme:default:alderco/codesense": {
          repoPath: "workspace:acme:default:alderco/codesense",
          repoName: "codesense",
          contextRepoPath: "/fake/ctx",
          teamContentPath: "",
          hasRemote: false,
          remoteUrl: null,
          mainDoc: "",
          localPath: null,
        },
      },
    });
  });

  it("records the removed slug under the team key on removeTeamSourceRepo", () => {
    useOrgStore.getState().removeTeamSourceRepo("acme", "default", {
      owner: "alderco",
      repo: "codesense",
    });

    const recorded = useOrgStore.getState().recentlyRemovedRepos["acme:default"];
    expect(recorded).toBeDefined();
    expect(recorded.has("alderco/codesense")).toBe(true);
  });

  it("normalizes the recorded slug to lowercase", () => {
    useOrgStore.getState().removeTeamSourceRepo("acme", "default", {
      owner: "ALDERCO",
      repo: "CODESENSE",
    });

    const recorded = useOrgStore.getState().recentlyRemovedRepos["acme:default"];
    expect(recorded.has("alderco/codesense")).toBe(true);
  });

  it("prunes the slug when the repo is re-added via updateTeamSourceRepos", () => {
    useOrgStore.getState().removeTeamSourceRepo("acme", "default", {
      owner: "alderco",
      repo: "codesense",
    });
    expect(
      useOrgStore.getState().recentlyRemovedRepos["acme:default"].has("alderco/codesense"),
    ).toBe(true);

    useOrgStore.getState().updateTeamSourceRepos("acme", "default", {
      owner: "alderco",
      repo: "codesense",
    });

    expect(
      useOrgStore.getState().recentlyRemovedRepos["acme:default"]?.has("alderco/codesense") ??
        false,
    ).toBe(false);
  });

  it("does not double-add a repo a registry reload already listed", () => {
    useOrgStore.getState().updateTeamSourceRepos("acme", "default", {
      owner: "alderco",
      repo: "codesense",
    });

    const repos = useOrgStore.getState().orgs[0].teams[0].source_repos;
    expect(repos.filter((r) => r.repo === "codesense")).toHaveLength(1);
  });

  it("is NOT populated by clearWorkspaceRepos (transient teardown is not a removal)", () => {
    useOrgStore.getState().clearWorkspaceRepos();

    expect(useOrgStore.getState().recentlyRemovedRepos["acme:default"]).toBeUndefined();
  });
});

// ---------------------------------------------------------------------------
// F-19: suspendWorkspaceRepos / resumeWorkspaceRepos / evictSuspendedWorkspace
// ---------------------------------------------------------------------------

const REPO_A = {
  repoPath: "workspace:acme:default:alderco/codesense",
  repoName: "codesense",
  contextRepoPath: "/fake/ctx",
  teamContentPath: "",
  hasRemote: false,
  remoteUrl: null,
  mainDoc: "",
  localPath: null,
};

const REPO_B = {
  repoPath: "workspace:acme:default:org/other",
  repoName: "other",
  contextRepoPath: "/fake/ctx2",
  teamContentPath: "",
  hasRemote: false,
  remoteUrl: null,
  mainDoc: "",
  localPath: null,
};

describe("suspendWorkspaceRepos", () => {
  beforeEach(() => {
    useOrgStore.setState({
      workspaceRepos: { [REPO_A.repoPath]: REPO_A, [REPO_B.repoPath]: REPO_B },
      suspendedWorkspaceRepos: {},
    });
  });

  it("moves workspaceRepos into suspendedWorkspaceRepos under the given key", () => {
    useOrgStore.getState().suspendWorkspaceRepos("acme:default");

    const s = useOrgStore.getState();
    expect(Object.keys(s.workspaceRepos)).toHaveLength(0);
    expect(s.suspendedWorkspaceRepos["acme:default"]).toBeDefined();
    expect(Object.keys(s.suspendedWorkspaceRepos["acme:default"])).toHaveLength(2);
    expect(s.suspendedWorkspaceRepos["acme:default"][REPO_A.repoPath]).toEqual(REPO_A);
  });

  it("does not clobber other suspended entries", () => {
    // Pre-seed another suspended workspace
    useOrgStore.setState({
      suspendedWorkspaceRepos: {
        "other-org:_none": { "workspace:other:_none:x/y": REPO_B },
      },
    });

    useOrgStore.getState().suspendWorkspaceRepos("acme:default");

    const s = useOrgStore.getState();
    expect(s.suspendedWorkspaceRepos["other-org:_none"]).toBeDefined();
    expect(s.suspendedWorkspaceRepos["acme:default"]).toBeDefined();
  });
});

describe("resumeWorkspaceRepos", () => {
  beforeEach(() => {
    useOrgStore.setState({
      workspaceRepos: {},
      suspendedWorkspaceRepos: {
        "acme:default": { [REPO_A.repoPath]: REPO_A, [REPO_B.repoPath]: REPO_B },
      },
    });
  });

  it("restores suspended repos to workspaceRepos and deletes the suspended entry", () => {
    useOrgStore.getState().resumeWorkspaceRepos("acme:default");

    const s = useOrgStore.getState();
    expect(Object.keys(s.workspaceRepos)).toHaveLength(2);
    expect(s.workspaceRepos[REPO_A.repoPath]).toEqual(REPO_A);
    expect(s.suspendedWorkspaceRepos["acme:default"]).toBeUndefined();
  });

  it("is a no-op when key not found", () => {
    useOrgStore.getState().resumeWorkspaceRepos("nonexistent:key");

    const s = useOrgStore.getState();
    expect(Object.keys(s.workspaceRepos)).toHaveLength(0);
    expect(s.suspendedWorkspaceRepos["acme:default"]).toBeDefined();
  });

  it("round-trip preserves all repo data", () => {
    // Start with active repos
    useOrgStore.setState({
      workspaceRepos: { [REPO_A.repoPath]: REPO_A },
      suspendedWorkspaceRepos: {},
    });

    // Suspend
    useOrgStore.getState().suspendWorkspaceRepos("acme:default");
    expect(useOrgStore.getState().workspaceRepos[REPO_A.repoPath]).toBeUndefined();

    // Resume
    useOrgStore.getState().resumeWorkspaceRepos("acme:default");
    expect(useOrgStore.getState().workspaceRepos[REPO_A.repoPath]).toEqual(REPO_A);
    expect(useOrgStore.getState().suspendedWorkspaceRepos["acme:default"]).toBeUndefined();
  });
});

describe("evictSuspendedWorkspace", () => {
  beforeEach(() => {
    useOrgStore.setState({
      suspendedWorkspaceRepos: {
        "acme:default": { [REPO_A.repoPath]: REPO_A, [REPO_B.repoPath]: REPO_B },
      },
    });
  });

  it("returns repo paths and removes the suspended entry", () => {
    const paths = useOrgStore.getState().evictSuspendedWorkspace("acme:default");

    expect(paths).toHaveLength(2);
    expect(paths).toContain(REPO_A.repoPath);
    expect(paths).toContain(REPO_B.repoPath);
    expect(useOrgStore.getState().suspendedWorkspaceRepos["acme:default"]).toBeUndefined();
  });

  it("returns empty array when key not found", () => {
    const paths = useOrgStore.getState().evictSuspendedWorkspace("nonexistent:key");

    expect(paths).toHaveLength(0);
    expect(useOrgStore.getState().suspendedWorkspaceRepos["acme:default"]).toBeDefined();
  });
});

// ---------------------------------------------------------------------------
// F-39: per-org last-team memory (lastTeamByOrgSlug)
// ---------------------------------------------------------------------------

function makeMultiTeamOrg(slug: string, teamNames: string[]): Org {
  return makeOrg({
    slug,
    display_name: slug,
    org_root: `/fake/${slug}`,
    teams: teamNames.map((name) => ({
      name,
      source_repos: [],
    })),
  });
}

describe("SW-UX setOrgDisabled (disable/enable orgs, ≥1 invariant)", () => {
  beforeEach(() => {
    useOrgStore.setState({
      orgs: [
        makeMultiTeamOrg("codesense/codesense", ["docs"]),
        makeMultiTeamOrg("acme", ["default"]),
      ],
      activeOrgSlug: "acme",
      activeTeamName: "default",
      disabledOrgSlugs: [],
    });
  });

  it("disables an org (soft-remove) and re-enables it", () => {
    useOrgStore.getState().setOrgDisabled("codesense/codesense", true);
    expect(useOrgStore.getState().disabledOrgSlugs).toContain("codesense/codesense");
    useOrgStore.getState().setOrgDisabled("codesense/codesense", false);
    expect(useOrgStore.getState().disabledOrgSlugs).not.toContain("codesense/codesense");
  });

  it("SW-OC-16: the bundled CodeSense docs can't be disabled", () => {
    useOrgStore.setState({
      orgs: [
        makeMultiTeamOrg(CODESENSE_DOCS_SLUG, ["getting-started"]),
        makeMultiTeamOrg("acme", ["core"]),
      ],
      activeOrgSlug: "acme",
      disabledOrgSlugs: [],
    });
    useOrgStore.getState().setOrgDisabled(CODESENSE_DOCS_SLUG, true);
    expect(useOrgStore.getState().disabledOrgSlugs).toEqual([]);
  });

  it("refuses to disable the LAST enabled org (≥1 invariant)", () => {
    // Disable one of two → ok, leaves one enabled.
    useOrgStore.getState().setOrgDisabled("codesense/codesense", true);
    // Disabling the remaining enabled org is a no-op (would leave zero).
    useOrgStore.getState().setOrgDisabled("acme", true);
    expect(useOrgStore.getState().disabledOrgSlugs).not.toContain("acme");
  });

  it("switches the active org away when the active org is disabled", () => {
    // acme is active; disabling it switches active to the remaining enabled org.
    useOrgStore.getState().setOrgDisabled("acme", true);
    expect(useOrgStore.getState().disabledOrgSlugs).toContain("acme");
    expect(useOrgStore.getState().activeOrgSlug).toBe("codesense/codesense");
  });
});

describe("F-39 resolveRestoredTeam", () => {
  it("returns the stored team when present and still shown in the org", () => {
    const org = makeMultiTeamOrg("9north", ["default", "smart-web"]);
    expect(resolveRestoredTeam(org, { "9north": "smart-web" }, {})).toBe("smart-web");
  });

  it("returns null when the slug is absent from the map", () => {
    const org = makeMultiTeamOrg("9north", ["default", "smart-web"]);
    expect(resolveRestoredTeam(org, {}, {})).toBeNull();
    expect(resolveRestoredTeam(org, { other: "x" }, {})).toBeNull();
  });

  it("returns null when the stored team is no longer in the org", () => {
    const org = makeMultiTeamOrg("9north", ["default"]);
    expect(resolveRestoredTeam(org, { "9north": "smart-web" }, {})).toBeNull();
  });

  it("SW-OB-9: returns null when the stored team exists but is HIDDEN (AC-6)", () => {
    const org = makeMultiTeamOrg("9north", ["default", "smart-web"]);
    // smart-web remembered, but the shown set hides it (only 'default' shown).
    expect(
      resolveRestoredTeam(org, { "9north": "smart-web" }, { "9north": ["default"] }),
    ).toBeNull();
    // still restorable when it IS shown.
    expect(resolveRestoredTeam(org, { "9north": "smart-web" }, { "9north": ["smart-web"] })).toBe(
      "smart-web",
    );
  });
});

describe("SW-OB-7 defaultTeamForOrg (open-access landing team)", () => {
  it("prefers a team literally named 'default'", () => {
    expect(defaultTeamForOrg(makeMultiTeamOrg("9north", ["smart-web", "default"]), {})).toBe(
      "default",
    );
  });

  it("falls back to the first team when there is no 'default'", () => {
    expect(defaultTeamForOrg(makeMultiTeamOrg("9north", ["alpha", "beta"]), {})).toBe("alpha");
  });

  it("returns null only for a genuinely team-less org", () => {
    expect(defaultTeamForOrg(makeMultiTeamOrg("9north", []), {})).toBeNull();
  });

  it("SW-OB-9: picks the first SHOWN team; returns null when all hidden (AC-6)", () => {
    const org = makeMultiTeamOrg("9north", ["default", "alpha", "beta"]);
    // 'default' is hidden → first shown in org order is 'alpha'.
    expect(defaultTeamForOrg(org, { "9north": ["beta", "alpha"] })).toBe("alpha");
    // all hidden (explicit empty shown set) → null → no-team state.
    expect(defaultTeamForOrg(org, { "9north": [] })).toBeNull();
    // absent shown set ⇒ all shown ⇒ prefers 'default'.
    expect(defaultTeamForOrg(org, {})).toBe("default");
  });
});

describe("SW-OB-7 blank-screen regression: setOrgs never leaves an active org team-less", () => {
  it("defaults activeTeamName to the org's default team when persisted state has org set but team null", () => {
    // Exact repro of the blank screen: a prior launch persisted
    // { activeOrgSlug: X, activeTeamName: null } while the viewer opened X:default.
    // A null active team → teamKey `X:_none` ≠ the opened `X:default`, so App.tsx
    // hides the only opened team (display:none) → blank. setOrgs must reconcile it.
    useOrgStore.setState({
      orgs: [],
      activeOrgSlug: "acme/team-context",
      activeTeamName: null,
      lastTeamByOrgSlug: {},
      hasUserIntent: false,
      stateLoaded: false,
    });
    useOrgStore.getState().setOrgs([makeMultiTeamOrg("acme/team-context", ["default"])]);
    expect(useOrgStore.getState().activeTeamName).toBe("default");
  });
});

describe("F-39 setActiveTeam writes to lastTeamByOrgSlug", () => {
  beforeEach(() => {
    useOrgStore.setState({
      orgs: [makeMultiTeamOrg("9north", ["default", "smart-web"])],
      activeOrgSlug: "9north",
      activeTeamName: null,
      lastTeamByOrgSlug: {},
      hasUserIntent: false,
      stateLoaded: false,
    });
  });

  it("writes the slug→team mapping on non-null setActiveTeam", () => {
    useOrgStore.getState().setActiveTeam("smart-web");
    expect(useOrgStore.getState().lastTeamByOrgSlug).toEqual({ "9north": "smart-web" });
    expect(useOrgStore.getState().activeTeamName).toBe("smart-web");
  });

  it("does NOT mutate the map on setActiveTeam(null)", () => {
    useOrgStore.setState({
      lastTeamByOrgSlug: { "9north": "smart-web" },
      activeTeamName: "smart-web",
    });
    useOrgStore.getState().setActiveTeam(null);
    expect(useOrgStore.getState().lastTeamByOrgSlug).toEqual({ "9north": "smart-web" });
    expect(useOrgStore.getState().activeTeamName).toBeNull();
  });

  it("overwrites existing entry when a different team is selected for the same org", () => {
    useOrgStore.setState({ lastTeamByOrgSlug: { "9north": "default" } });
    useOrgStore.getState().setActiveTeam("smart-web");
    expect(useOrgStore.getState().lastTeamByOrgSlug).toEqual({ "9north": "smart-web" });
  });

  it("does NOT write a map entry when the team name doesn't exist in the active org", () => {
    useOrgStore.getState().setActiveTeam("phantom");
    expect(useOrgStore.getState().lastTeamByOrgSlug).toEqual({});
    expect(useOrgStore.getState().activeTeamName).toBeNull();
  });
});

describe("F-39 setActiveOrg restores from lastTeamByOrgSlug", () => {
  beforeEach(() => {
    useOrgStore.setState({
      orgs: [
        makeMultiTeamOrg("9north", ["default", "smart-web"]),
        makeMultiTeamOrg("codesense", ["default", "platform"]),
      ],
      activeOrgSlug: null,
      activeTeamName: null,
      lastTeamByOrgSlug: {},
      hasUserIntent: false,
      stateLoaded: false,
    });
  });

  it("restores the stored team on org switch when valid", () => {
    useOrgStore.setState({ lastTeamByOrgSlug: { "9north": "smart-web" } });
    useOrgStore.getState().setActiveOrg("9north");
    expect(useOrgStore.getState().activeTeamName).toBe("smart-web");
  });

  it("SW-OB-7: lands on the default team when the stored team is no longer in the org", () => {
    useOrgStore.setState({ lastTeamByOrgSlug: { "9north": "phantom-team" } });
    useOrgStore.getState().setActiveOrg("9north");
    expect(useOrgStore.getState().activeTeamName).toBe("default");
  });

  it("SW-OB-7: lands on the default team when no memory exists for the target org", () => {
    useOrgStore.getState().setActiveOrg("9north");
    expect(useOrgStore.getState().activeTeamName).toBe("default");
  });

  it("does NOT mutate the map on setActiveOrg(null)", () => {
    useOrgStore.setState({
      activeOrgSlug: "9north",
      activeTeamName: "smart-web",
      lastTeamByOrgSlug: { "9north": "smart-web" },
    });
    useOrgStore.getState().setActiveOrg(null);
    expect(useOrgStore.getState().lastTeamByOrgSlug).toEqual({ "9north": "smart-web" });
    expect(useOrgStore.getState().activeOrgSlug).toBeNull();
    expect(useOrgStore.getState().activeTeamName).toBeNull();
  });

  it("does NOT mutate the map on clearActive()", () => {
    useOrgStore.setState({
      activeOrgSlug: "9north",
      activeTeamName: "smart-web",
      lastTeamByOrgSlug: { "9north": "smart-web" },
    });
    useOrgStore.getState().clearActive();
    expect(useOrgStore.getState().lastTeamByOrgSlug).toEqual({ "9north": "smart-web" });
    expect(useOrgStore.getState().activeOrgSlug).toBeNull();
  });

  it("F-39 review P2: same-slug re-selection restores from map when activeTeamName is null", () => {
    // Reachable after rehydrate when a prior session ended with
    // setActiveTeam(null) but the per-org memory survived (AC-5).
    useOrgStore.setState({
      activeOrgSlug: "9north",
      activeTeamName: null,
      lastTeamByOrgSlug: { "9north": "smart-web" },
    });

    useOrgStore.getState().setActiveOrg("9north");

    expect(useOrgStore.getState().activeOrgSlug).toBe("9north");
    expect(useOrgStore.getState().activeTeamName).toBe("smart-web");
  });

  it("F-39 review P2: same-slug re-selection is a no-op when activeTeamName is already set", () => {
    // The StrictMode-clobber guard must still hold for active selections.
    useOrgStore.setState({
      activeOrgSlug: "9north",
      activeTeamName: "default",
      lastTeamByOrgSlug: { "9north": "smart-web" },
    });

    useOrgStore.getState().setActiveOrg("9north");

    // Should NOT clobber "default" with the remembered "smart-web".
    expect(useOrgStore.getState().activeTeamName).toBe("default");
  });

  it("SW-OB-7: same-slug re-selection with null team + no memory lands on the default team", () => {
    // Previously a no-op (stayed null). Under open access the active team must
    // not sit at null while the org has teams (blank-screen bug), so it lands on
    // the default team. StrictMode-safe: the second invoke sees a non-null team
    // and the re-select guard short-circuits.
    useOrgStore.setState({
      activeOrgSlug: "9north",
      activeTeamName: null,
      lastTeamByOrgSlug: {},
    });

    useOrgStore.getState().setActiveOrg("9north");

    expect(useOrgStore.getState().activeTeamName).toBe("default");
  });

  it("round-trips org switch: A → smart-web → B → A restores smart-web", () => {
    // Start on 9north, select smart-web.
    useOrgStore.getState().setActiveOrg("9north");
    useOrgStore.getState().setActiveTeam("smart-web");
    expect(useOrgStore.getState().activeTeamName).toBe("smart-web");

    // Switch to codesense → no memory yet, so land on its default team (SW-OB-7).
    useOrgStore.getState().setActiveOrg("codesense");
    expect(useOrgStore.getState().activeOrgSlug).toBe("codesense");
    expect(useOrgStore.getState().activeTeamName).toBe("default");

    // Switch back to 9north → smart-web restored from map.
    useOrgStore.getState().setActiveOrg("9north");
    expect(useOrgStore.getState().activeOrgSlug).toBe("9north");
    expect(useOrgStore.getState().activeTeamName).toBe("smart-web");
  });
});

describe("F-39 setOrgs prefers per-org memory when active team is removed", () => {
  beforeEach(() => {
    useOrgStore.setState({
      orgs: [makeMultiTeamOrg("9north", ["default", "smart-web", "platform"])],
      activeOrgSlug: "9north",
      activeTeamName: "platform",
      lastTeamByOrgSlug: { "9north": "smart-web" },
      hasUserIntent: false,
      stateLoaded: false,
    });
  });

  it("falls back to lastTeamByOrgSlug when the previously-active team is removed from the refresh", () => {
    // Refresh: org list still contains 9north, but `platform` (current activeTeamName)
    // is gone. The per-org memory has smart-web which is still present.
    const refreshed = makeMultiTeamOrg("9north", ["default", "smart-web"]);
    useOrgStore.getState().setOrgs([refreshed]);
    expect(useOrgStore.getState().activeTeamName).toBe("smart-web");
  });

  it("SW-OB-7: lands on the default team when neither current team nor memory is valid", () => {
    useOrgStore.setState({ lastTeamByOrgSlug: { "9north": "also-gone" } });
    const refreshed = makeMultiTeamOrg("9north", ["default"]);
    useOrgStore.getState().setOrgs([refreshed]);
    expect(useOrgStore.getState().activeTeamName).toBe("default");
  });

  it("keeps the current team when it still exists, ignoring memory", () => {
    // Current is `platform`, still in refresh; memory says `smart-web` but
    // we shouldn't override a still-valid current selection.
    const refreshed = makeMultiTeamOrg("9north", ["default", "smart-web", "platform"]);
    useOrgStore.getState().setOrgs([refreshed]);
    expect(useOrgStore.getState().activeTeamName).toBe("platform");
  });

  it("preserves the stale map entry when the active org is removed entirely (no auto-clean)", () => {
    useOrgStore.getState().setOrgs([makeMultiTeamOrg("codesense", ["default"])]);
    expect(useOrgStore.getState().activeOrgSlug).toBeNull();
    expect(useOrgStore.getState().activeTeamName).toBeNull();
    // Map entry for 9north is left as-is — AC-5: append-or-update only.
    expect(useOrgStore.getState().lastTeamByOrgSlug).toEqual({ "9north": "smart-web" });
  });
});

describe("F-39 persistence round-trip", () => {
  beforeEach(() => {
    mockedSaveAppState.mockClear();
    mockedSaveAppState.mockResolvedValue(undefined);
    useOrgStore.setState({
      orgs: [makeMultiTeamOrg("9north", ["default", "smart-web"])],
      activeOrgSlug: "9north",
      activeTeamName: null,
      lastTeamByOrgSlug: {},
      hasUserIntent: false,
      stateLoaded: true,
    });
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("persists lastTeamByOrgSlug after setActiveTeam", async () => {
    useOrgStore.getState().setActiveTeam("smart-web");
    await vi.advanceTimersByTimeAsync(350);

    expect(mockedSaveAppState).toHaveBeenCalled();
    const lastCall = mockedSaveAppState.mock.calls.at(-1);
    expect(lastCall).toBeDefined();
    const parsed = JSON.parse(lastCall![0] as string);
    expect(parsed.org).toMatchObject({
      activeOrgSlug: "9north",
      activeTeamName: "smart-web",
      lastTeamByOrgSlug: { "9north": "smart-web" },
    });
  });

  // SW-OC-3 (S3, AC-4): the lifecycle — switch / disable (soft-remove) / enable
  // + the active workspace — survives a REAL restart with no backend, proven by
  // driving the actual persist writer, then feeding its exact on-disk blob into a
  // cold `loadPersistedState` (a fresh process reading the same appstate file).
  it("switch + disable + enable + active workspace round-trip a real restart", async () => {
    // Two live orgs; switch to 9north + smart-web, disable then re-enable a repo.
    useOrgStore.setState({
      orgs: [
        makeMultiTeamOrg("9north", ["default", "smart-web"]),
        makeMultiTeamOrg("codesense", ["platform"]),
      ],
      activeOrgSlug: null,
      activeTeamName: null,
      lastTeamByOrgSlug: {},
      disabledOrgSlugs: [],
      hasUserIntent: false,
      stateLoaded: true,
    });

    // Switch (active workspace) + a disable/enable cycle.
    useOrgStore.getState().setActiveOrg("9north");
    useOrgStore.getState().setActiveTeam("smart-web");
    useOrgStore.getState().setOrgDisabled("codesense", true);
    useOrgStore.getState().setOrgDisabled("codesense", false);
    // Disable one and leave it disabled across the restart.
    useOrgStore.getState().setOrgDisabled("codesense", true);

    // Flush the debounced persist writer — capture the EXACT blob written to disk.
    await vi.advanceTimersByTimeAsync(350);
    const written = mockedSaveAppState.mock.calls.at(-1);
    expect(written).toBeDefined();
    const onDisk = written![0] as string;
    // Sanity: the disabled set + active selection made it into the persisted blob.
    expect(JSON.parse(onDisk).org).toMatchObject({
      activeOrgSlug: "9north",
      activeTeamName: "smart-web",
      disabledOrgSlugs: ["codesense"],
    });

    // ---- RESTART: fresh process. Cold store, orgs re-discovered from disk,
    // no user intent yet; the same appstate file is read back. ----
    useOrgStore.setState({
      orgs: [
        makeMultiTeamOrg("9north", ["default", "smart-web"]),
        makeMultiTeamOrg("codesense", ["platform"]),
      ],
      activeOrgSlug: null,
      activeTeamName: null,
      lastTeamByOrgSlug: {},
      shownTeamsByOrgSlug: {},
      disabledOrgSlugs: [],
      hasUserIntent: false,
      stateLoaded: false,
    });
    mockedLoadAppState.mockResolvedValueOnce(onDisk);

    await useOrgStore.getState().loadPersistedState();

    // The active workspace (org + team) and the disabled set survived the restart —
    // no backend call, purely appstate-driven.
    const after = useOrgStore.getState();
    expect(after.stateLoaded).toBe(true);
    expect(after.activeOrgSlug).toBe("9north");
    expect(after.activeTeamName).toBe("smart-web");
    expect(after.disabledOrgSlugs).toEqual(["codesense"]);
    // Re-enabled orgs do NOT resurrect: the just-toggled-off then final-on cycle
    // for the ACTIVE org left it enabled, so only "codesense" persists as disabled.
    expect(after.disabledOrgSlugs).not.toContain("9north");
  });
});

describe("F-39 loadPersistedState migration", () => {
  beforeEach(() => {
    mockedLoadAppState.mockReset();
    useOrgStore.setState({
      orgs: [],
      activeOrgSlug: null,
      activeTeamName: null,
      lastTeamByOrgSlug: {},
      hasUserIntent: false,
      stateLoaded: false,
    });
  });

  it("seeds lastTeamByOrgSlug from pre-upgrade pair when map field is absent", async () => {
    mockedLoadAppState.mockResolvedValue(
      JSON.stringify({
        org: { activeOrgSlug: "9north", activeTeamName: "smart-web" },
      }),
    );

    await useOrgStore.getState().loadPersistedState();

    expect(useOrgStore.getState().lastTeamByOrgSlug).toEqual({ "9north": "smart-web" });
    expect(useOrgStore.getState().activeOrgSlug).toBe("9north");
    expect(useOrgStore.getState().activeTeamName).toBe("smart-web");
    expect(useOrgStore.getState().stateLoaded).toBe(true);
  });

  it("defaults to empty map when pre-upgrade JSON has null pair and no map", async () => {
    mockedLoadAppState.mockResolvedValue(
      JSON.stringify({
        org: { activeOrgSlug: null, activeTeamName: null },
      }),
    );

    await useOrgStore.getState().loadPersistedState();

    expect(useOrgStore.getState().lastTeamByOrgSlug).toEqual({});
  });

  it("preserves the persisted map verbatim when present", async () => {
    mockedLoadAppState.mockResolvedValue(
      JSON.stringify({
        org: {
          activeOrgSlug: "9north",
          activeTeamName: "smart-web",
          lastTeamByOrgSlug: { "9north": "smart-web", codesense: "platform" },
        },
      }),
    );

    await useOrgStore.getState().loadPersistedState();

    expect(useOrgStore.getState().lastTeamByOrgSlug).toEqual({
      "9north": "smart-web",
      codesense: "platform",
    });
  });

  it("seeds map even when persisted slug/team will be invalidated against current orgs", async () => {
    // Live orgs already loaded; persisted slug isn't in the list. The
    // validation block will clear activeOrgSlug/activeTeamName, but the
    // map seed should still capture the persisted pair (per AC-7 — read
    // raw values BEFORE validation).
    useOrgStore.setState({ orgs: [makeMultiTeamOrg("codesense", ["default"])] });

    mockedLoadAppState.mockResolvedValue(
      JSON.stringify({
        org: { activeOrgSlug: "9north", activeTeamName: "smart-web" },
      }),
    );

    await useOrgStore.getState().loadPersistedState();

    expect(useOrgStore.getState().activeOrgSlug).toBeNull();
    expect(useOrgStore.getState().activeTeamName).toBeNull();
    // But the map kept the raw seed — harmless until 9north reappears.
    expect(useOrgStore.getState().lastTeamByOrgSlug).toEqual({ "9north": "smart-web" });
  });

  it("defaults to empty map and stateLoaded:true when no persisted org block exists", async () => {
    mockedLoadAppState.mockResolvedValue("{}");

    await useOrgStore.getState().loadPersistedState();

    expect(useOrgStore.getState().lastTeamByOrgSlug).toEqual({});
    expect(useOrgStore.getState().stateLoaded).toBe(true);
  });

  it("F-39 review P1: merges persisted lastTeamByOrgSlug when hasUserIntent already fired", async () => {
    // Race: a setter (e.g., setActiveTeam) fires before loadAppState resolves.
    // Without the patch, the persisted map is discarded — and the next
    // persist write clobbers every other org's remembered team on disk.
    useOrgStore.setState({
      hasUserIntent: true,
      activeOrgSlug: "9north",
      activeTeamName: "smart-web",
      lastTeamByOrgSlug: { "9north": "smart-web" },
    });
    mockedLoadAppState.mockResolvedValue(
      JSON.stringify({
        org: {
          activeOrgSlug: "codesense",
          activeTeamName: "platform",
          lastTeamByOrgSlug: { codesense: "platform", retired: "core" },
        },
      }),
    );

    await useOrgStore.getState().loadPersistedState();

    const map = useOrgStore.getState().lastTeamByOrgSlug;
    expect(map).toEqual({
      // Live entry preserved (key collision wins for live state).
      "9north": "smart-web",
      // Persisted entries for other orgs merged in.
      codesense: "platform",
      retired: "core",
    });
    // Live slug/team pair untouched.
    expect(useOrgStore.getState().activeOrgSlug).toBe("9north");
    expect(useOrgStore.getState().activeTeamName).toBe("smart-web");
    expect(useOrgStore.getState().stateLoaded).toBe(true);
  });

  it("F-39 review P1: live entry wins over persisted on key collision", async () => {
    useOrgStore.setState({
      hasUserIntent: true,
      activeOrgSlug: "9north",
      activeTeamName: "platform",
      lastTeamByOrgSlug: { "9north": "platform" },
    });
    mockedLoadAppState.mockResolvedValue(
      JSON.stringify({
        org: {
          activeOrgSlug: "9north",
          activeTeamName: "smart-web",
          lastTeamByOrgSlug: { "9north": "smart-web" },
        },
      }),
    );

    await useOrgStore.getState().loadPersistedState();

    // Live value wins.
    expect(useOrgStore.getState().lastTeamByOrgSlug).toEqual({ "9north": "platform" });
  });

  it("F-39 review P1: no-op when hasUserIntent set and persisted map is empty or missing", async () => {
    useOrgStore.setState({
      hasUserIntent: true,
      lastTeamByOrgSlug: { "9north": "smart-web" },
    });
    mockedLoadAppState.mockResolvedValue(
      JSON.stringify({ org: { activeOrgSlug: null, activeTeamName: null } }),
    );

    await useOrgStore.getState().loadPersistedState();

    expect(useOrgStore.getState().lastTeamByOrgSlug).toEqual({ "9north": "smart-web" });
  });

  it("F-39 review iter1 P1: hasUserIntent merge path seeds migration from pre-upgrade pair when map field missing", async () => {
    // Race: pre-upgrade JSON (no `lastTeamByOrgSlug` field) + setter
    // fires before rehydrate. Without this patch the migration seed
    // would only run in the non-hasUserIntent branch, silently dropping
    // the user's pre-upgrade team memory.
    useOrgStore.setState({
      hasUserIntent: true,
      activeOrgSlug: "codesense",
      activeTeamName: "platform",
      lastTeamByOrgSlug: {}, // setter that fired hasn't touched the map yet
    });
    mockedLoadAppState.mockResolvedValue(
      JSON.stringify({ org: { activeOrgSlug: "9north", activeTeamName: "smart-web" } }),
    );

    await useOrgStore.getState().loadPersistedState();

    // Pre-upgrade pair was seeded into the merge.
    expect(useOrgStore.getState().lastTeamByOrgSlug).toEqual({ "9north": "smart-web" });
    // Live slug/team pair untouched.
    expect(useOrgStore.getState().activeOrgSlug).toBe("codesense");
    expect(useOrgStore.getState().activeTeamName).toBe("platform");
  });

  it("F-39 review iter1 P4: cold-start restores team for live activeOrgSlug after merging persisted map", async () => {
    // Race: useOrgDiscovery picked `loadedOrgs[0]` via setActiveOrg
    // before loadPersistedState resolved. activeTeamName was set to
    // null because the map was empty at that moment. The merged-in
    // persisted entry should now restore the remembered team.
    useOrgStore.setState({
      orgs: [makeMultiTeamOrg("9north", ["default", "smart-web"])],
      hasUserIntent: true,
      activeOrgSlug: "9north",
      activeTeamName: null,
      lastTeamByOrgSlug: {},
    });
    mockedLoadAppState.mockResolvedValue(
      JSON.stringify({
        org: {
          activeOrgSlug: "9north",
          activeTeamName: "smart-web",
          lastTeamByOrgSlug: { "9north": "smart-web" },
        },
      }),
    );

    await useOrgStore.getState().loadPersistedState();

    expect(useOrgStore.getState().lastTeamByOrgSlug).toEqual({ "9north": "smart-web" });
    // Critical: the merged-in entry surfaced as a live team selection.
    expect(useOrgStore.getState().activeTeamName).toBe("smart-web");
  });

  it("F-39 review iter1 P4: cold-start does NOT clobber a team already chosen by the racing setter", async () => {
    useOrgStore.setState({
      orgs: [makeMultiTeamOrg("9north", ["default", "smart-web"])],
      hasUserIntent: true,
      activeOrgSlug: "9north",
      activeTeamName: "default", // setter already chose a team
      lastTeamByOrgSlug: { "9north": "default" },
    });
    mockedLoadAppState.mockResolvedValue(
      JSON.stringify({
        org: {
          activeOrgSlug: "9north",
          activeTeamName: "smart-web",
          lastTeamByOrgSlug: { "9north": "smart-web" },
        },
      }),
    );

    await useOrgStore.getState().loadPersistedState();

    // Live team wins (live-wins-on-collision invariant).
    expect(useOrgStore.getState().lastTeamByOrgSlug).toEqual({ "9north": "default" });
    expect(useOrgStore.getState().activeTeamName).toBe("default");
  });

  it("F-39 review iter1 P4: cold-start does NOT restore when merged team is no longer in the org", async () => {
    useOrgStore.setState({
      orgs: [makeMultiTeamOrg("9north", ["default"])], // smart-web removed
      hasUserIntent: true,
      activeOrgSlug: "9north",
      activeTeamName: null,
      lastTeamByOrgSlug: {},
    });
    mockedLoadAppState.mockResolvedValue(
      JSON.stringify({
        org: { activeOrgSlug: "9north", activeTeamName: "smart-web" },
      }),
    );

    await useOrgStore.getState().loadPersistedState();

    expect(useOrgStore.getState().activeTeamName).toBeNull();
  });
});

describe("createTeam (SW-OB-1)", () => {
  beforeEach(() => {
    mockedCommitRegistryEdit.mockClear();
    mockedCommitRegistryEdit.mockResolvedValue(undefined);
    mockedGetCurrentUser.mockReturnValue({
      github_user_id: 42,
      github_username: "creator",
      display_name: "Creator",
      avatar_url: null,
    } as ReturnType<typeof getCurrentUser>);
    useOrgStore.setState({
      orgs: [makeOrg({ teams: [{ name: "platform", source_repos: [] }] })],
      activeOrgSlug: "acme",
      activeTeamName: "platform",
    });
  });

  it("commits locally, adds the team in-memory, and switches into it", async () => {
    await useOrgStore.getState().createTeam("payments");

    // Local commit with the CreateTeam edit carrying the creator profile.
    expect(mockedCommitRegistryEdit).toHaveBeenCalledTimes(1);
    expect(mockedCommitRegistryEdit).toHaveBeenCalledWith(42, "acme", {
      type: "CreateTeam",
      team_name: "payments",
      creator_id: 42,
      creator_username: "creator",
      creator_display_name: "Creator",
      creator_avatar_url: null,
    });

    const state = useOrgStore.getState();
    const org = state.orgs.find((o) => o.slug === "acme")!;
    const created = org.teams.find((t) => t.name === "payments");
    expect(created).toBeDefined();
    expect(created!.source_repos).toEqual([]);
    // Switched into the new team.
    expect(state.activeTeamName).toBe("payments");
  });

  it("adds the team to the store BEFORE switching (setActiveTeam rejects unknown teams)", async () => {
    await useOrgStore.getState().createTeam("infra");
    // If the order were reversed, setActiveTeam would have logged a warning
    // and left activeTeamName unchanged. Reaching the new team proves order.
    expect(useOrgStore.getState().activeTeamName).toBe("infra");
  });

  it("trims the name before committing", async () => {
    await useOrgStore.getState().createTeam("  payments  ");
    expect(mockedCommitRegistryEdit).toHaveBeenCalledWith(
      42,
      "acme",
      expect.objectContaining({ team_name: "payments" }),
    );
  });

  it("propagates a backend commit failure and does NOT add the team", async () => {
    mockedCommitRegistryEdit.mockRejectedValueOnce(new Error("team 'platform' already exists"));

    await expect(useOrgStore.getState().createTeam("platform")).rejects.toThrow("already exists");

    const org = useOrgStore.getState().orgs.find((o) => o.slug === "acme")!;
    // Only the original team remains — no partial in-memory add.
    expect(org.teams.map((t) => t.name)).toEqual(["platform"]);
  });

  it("throws when there is no active org and does not commit", async () => {
    useOrgStore.setState({ activeOrgSlug: null });
    await expect(useOrgStore.getState().createTeam("payments")).rejects.toThrow(
      "No active workspace",
    );
    expect(mockedCommitRegistryEdit).not.toHaveBeenCalled();
  });

  it("throws when not signed in and does not commit", async () => {
    mockedGetCurrentUser.mockReturnValue(null);
    await expect(useOrgStore.getState().createTeam("payments")).rejects.toThrow("signed in");
    expect(mockedCommitRegistryEdit).not.toHaveBeenCalled();
  });

  it("addTeamToStore is idempotent — never double-adds an existing team", () => {
    useOrgStore.getState().addTeamToStore("acme", { name: "platform", source_repos: [] });
    const org = useOrgStore.getState().orgs.find((o) => o.slug === "acme")!;
    expect(org.teams.filter((t) => t.name === "platform")).toHaveLength(1);
  });

  it("addTeamToStore does not persist (orgs are ephemeral)", () => {
    mockedSaveAppState.mockClear();
    useOrgStore.getState().addTeamToStore("acme", { name: "infra", source_repos: [] });
    expect(mockedSaveAppState).not.toHaveBeenCalled();
  });
});

describe("renameTeam (SW-HB-8)", () => {
  beforeEach(() => {
    mockedCommitRegistryEdit.mockClear();
    mockedCommitRegistryEdit.mockResolvedValue(undefined);
    mockedGetCurrentUser.mockReturnValue({
      github_user_id: 42,
      github_username: "creator",
    } as ReturnType<typeof getCurrentUser>);
    useOrgStore.setState({
      orgs: [
        makeOrg({
          teams: [
            { name: "payments", source_repos: [] },
            { name: "keep", source_repos: [] },
          ],
        }),
      ],
      activeOrgSlug: "acme",
      activeTeamName: "payments",
      lastTeamByOrgSlug: { acme: "payments" },
      shownTeamsByOrgSlug: {},
      suspendedWorkspaceRepos: {},
      recentlyRemovedRepos: {},
    });
    useViewerStore.setState({
      openedTeams: ["acme:payments"],
      // A content-path tab (absolute, embeds `_team/{team}/content`) + a
      // source-repo tab (no `_team/` segment) to prove the rewrite touches only
      // the former.
      openTabs: {
        "acme:payments": [
          { path: "/org/_team/payments/content/e/s.md", name: "s" },
          { path: "/clone/src/main.rs", name: "main.rs" },
        ],
      },
      drillPaths: { "acme:payments": "/org/_team/payments/content/e" },
      navHistory: { "acme:payments": ["/org/_team/payments/content/e/s.md"] },
      // A persisted, teamKey-keyed slice that must ride the rename.
      navCollapsed: { "acme:payments": true },
    });
  });

  it("commits RenameTeam and re-keys BOTH org + viewer state (tabs survive)", async () => {
    await useOrgStore.getState().renameTeam("payments", "billing");

    expect(mockedCommitRegistryEdit).toHaveBeenCalledWith(42, "acme", {
      type: "RenameTeam",
      old_name: "payments",
      new_name: "billing",
    });

    const state = useOrgStore.getState();
    expect(state.orgs.find((o) => o.slug === "acme")!.teams.map((t) => t.name)).toEqual([
      "billing",
      "keep",
    ]);
    // Active team follows the rename — never falls to `_none`.
    expect(state.activeTeamName).toBe("billing");
    expect(state.lastTeamByOrgSlug).toEqual({ acme: "billing" });
    // suspendedWorkspaceRepos is DROPPED (stale inner synthetic keys) so the renamed team
    // re-mints cleanly on next activation.
    expect(state.suspendedWorkspaceRepos).toEqual({});

    // Viewer state re-keyed: open tabs moved to the new teamKey, none dangling.
    const vs = useViewerStore.getState();
    expect(vs.openedTeams).toEqual(["acme:billing"]);
    expect(vs.openTabs["acme:payments"]).toBeUndefined();
    // Content-path tab re-pointed to `_team/billing/…` (survives, not "deleted");
    // source-repo tab untouched (no `/_team/` segment).
    expect(vs.openTabs["acme:billing"]).toEqual([
      { path: "/org/_team/billing/content/e/s.md", name: "s" },
      { path: "/clone/src/main.rs", name: "main.rs" },
    ]);
    // drillPaths + navHistory content paths re-pointed too (nav-back won't
    // resurrect a "file deleted" tab).
    expect(vs.drillPaths["acme:billing"]).toBe("/org/_team/billing/content/e");
    expect(vs.navHistory["acme:billing"]).toEqual(["/org/_team/billing/content/e/s.md"]);
    // navCollapsed (persisted teamKey slice) moved too — none dangling.
    expect(vs.navCollapsed["acme:billing"]).toBe(true);
    expect(vs.navCollapsed["acme:payments"]).toBeUndefined();
  });

  it("moves the team's warm scope before the viewer's open teams drop the old key", async () => {
    const order: string[] = [];
    const rekey = vi.spyOn(warmScopes, "rekey").mockImplementation(() => {
      order.push("scope");
    });
    const stop = useViewerStore.subscribe((s, prev) => {
      if (s.openedTeams !== prev.openedTeams) order.push("viewer");
    });
    try {
      await useOrgStore.getState().renameTeam("payments", "billing");
      expect(rekey).toHaveBeenCalledWith("acme:payments", "acme:billing");
      expect(order).toEqual(["scope", "viewer"]);
    } finally {
      stop();
      rekey.mockRestore();
    }
  });

  it("re-mount from the renamed key survives under the new teamKey", async () => {
    await useOrgStore.getState().renameTeam("payments", "billing");

    // The mount survives under the NEW teamKey (App.tsx renders one pane per
    // openedTeams entry, React-keyed by teamKey).
    expect(useViewerStore.getState().openedTeams).toEqual(["acme:billing"]);

    // A pane hidden behind the settings overlay never flashes a blank on
    // the React-key-driven re-mount. Proven via the routing predicate.
    expect(
      teamPaneVisible({
        settingsPanelOpen: true,
        isActiveTeam: true,
      }),
    ).toBe(false);
  });

  it("trims the new name before committing", async () => {
    await useOrgStore.getState().renameTeam("payments", "  billing  ");
    expect(mockedCommitRegistryEdit).toHaveBeenCalledWith(
      42,
      "acme",
      expect.objectContaining({ new_name: "billing" }),
    );
  });

  it("propagates a backend failure and does NOT re-key", async () => {
    mockedCommitRegistryEdit.mockRejectedValueOnce(
      new Error("duplicate: team 'keep' already exists"),
    );
    await expect(useOrgStore.getState().renameTeam("payments", "keep")).rejects.toThrow(
      "already exists",
    );
    const state = useOrgStore.getState();
    expect(state.orgs.find((o) => o.slug === "acme")!.teams.map((t) => t.name)).toEqual([
      "payments",
      "keep",
    ]);
    expect(state.activeTeamName).toBe("payments");
  });

  it("no-ops (no commit) when the new name equals the old", async () => {
    await useOrgStore.getState().renameTeam("payments", "payments");
    expect(mockedCommitRegistryEdit).not.toHaveBeenCalled();
  });
});

describe("setTeamParent (SLW-IN-5)", () => {
  beforeEach(() => {
    mockedCommitRegistryEdit.mockClear();
    mockedCommitRegistryEdit.mockResolvedValue(undefined);
    mockedGetCurrentUser.mockReturnValue({
      github_user_id: 42,
      github_username: "creator",
    } as ReturnType<typeof getCurrentUser>);
    useOrgStore.setState({
      orgs: [
        makeOrg({
          teams: [
            { name: "child", source_repos: [] },
            { name: "parent", source_repos: [] },
          ],
        }),
      ],
      activeOrgSlug: "acme",
      activeTeamName: "child",
      shownTeamsByOrgSlug: {},
    });
  });

  it("commits SetTeamParent and reflects parent_team in-memory", async () => {
    await useOrgStore.getState().setTeamParent("child", "parent");

    expect(mockedCommitRegistryEdit).toHaveBeenCalledWith(42, "acme", {
      type: "SetTeamParent",
      team_name: "child",
      parent_team: "parent",
    });
    const team = useOrgStore
      .getState()
      .orgs.find((o) => o.slug === "acme")!
      .teams.find((t) => t.name === "child")!;
    expect(team.parent_team).toBe("parent");
  });

  it("clears the parent when passed null (team becomes a root)", async () => {
    useOrgStore.setState({
      orgs: [
        makeOrg({
          teams: [
            {
              name: "child",
              source_repos: [],
              parent_team: "parent",
            },
            { name: "parent", source_repos: [] },
          ],
        }),
      ],
      activeOrgSlug: "acme",
    });
    await useOrgStore.getState().setTeamParent("child", null);

    expect(mockedCommitRegistryEdit).toHaveBeenCalledWith(42, "acme", {
      type: "SetTeamParent",
      team_name: "child",
      parent_team: null,
    });
    const team = useOrgStore
      .getState()
      .orgs.find((o) => o.slug === "acme")!
      .teams.find((t) => t.name === "child")!;
    expect(team.parent_team ?? null).toBeNull();
  });

  it("no-ops (no commit) when the parent is unchanged", async () => {
    useOrgStore.setState({
      orgs: [
        makeOrg({
          teams: [
            {
              name: "child",
              source_repos: [],
              parent_team: "parent",
            },
            { name: "parent", source_repos: [] },
          ],
        }),
      ],
      activeOrgSlug: "acme",
    });
    await useOrgStore.getState().setTeamParent("child", "parent");
    expect(mockedCommitRegistryEdit).not.toHaveBeenCalled();
  });

  it("no-ops when clearing an already-root team", async () => {
    await useOrgStore.getState().setTeamParent("child", null);
    expect(mockedCommitRegistryEdit).not.toHaveBeenCalled();
  });

  it("propagates a backend failure and does NOT reflect", async () => {
    mockedCommitRegistryEdit.mockRejectedValueOnce(new Error("io: teams.yml"));
    await expect(useOrgStore.getState().setTeamParent("child", "parent")).rejects.toThrow(
      "teams.yml",
    );
    const team = useOrgStore
      .getState()
      .orgs.find((o) => o.slug === "acme")!
      .teams.find((t) => t.name === "child")!;
    expect(team.parent_team ?? null).toBeNull();
  });
});

describe("setShownTeams (SW-OB-9)", () => {
  const threeTeams = [
    { name: "a", source_repos: [] },
    { name: "b", source_repos: [] },
    { name: "c", source_repos: [] },
  ];

  beforeEach(() => {
    vi.clearAllMocks();
    useOrgStore.setState({
      orgs: [makeOrg({ slug: "acme", teams: threeTeams })],
      activeOrgSlug: "acme",
      activeTeamName: "a",
      lastTeamByOrgSlug: { acme: "a" },
      shownTeamsByOrgSlug: {},
      stateLoaded: true,
      hasUserIntent: false,
    });
  });

  it("writes the per-org shown set", () => {
    useOrgStore.getState().setShownTeams("acme", ["a", "b"]);
    expect(useOrgStore.getState().shownTeamsByOrgSlug).toEqual({ acme: ["a", "b"] });
  });

  it("persists the write (debounced)", async () => {
    vi.useFakeTimers();
    try {
      useOrgStore.getState().setShownTeams("acme", ["a"]);
      await vi.advanceTimersByTimeAsync(350);
      expect(mockedSaveAppState).toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps the active team when it stays in the shown set", () => {
    useOrgStore.getState().setShownTeams("acme", ["a", "c"]);
    expect(useOrgStore.getState().activeTeamName).toBe("a");
  });

  it("AC-6: removing the active team switches active to the first remaining shown team (org order)", () => {
    // Drop "a" (active); shown set given as ["c","b"] — reconciliation must
    // follow ORG order (b before c), NOT the caller's array order.
    useOrgStore.getState().setShownTeams("acme", ["c", "b"]);
    expect(useOrgStore.getState().activeTeamName).toBe("b");
  });

  it("AC-6: removing the last shown team clears the active team (→ no-team state)", () => {
    useOrgStore.getState().setShownTeams("acme", []);
    expect(useOrgStore.getState().activeTeamName).toBeNull();
  });

  it("AC-6: re-adding from an empty set auto-selects the first shown team", () => {
    useOrgStore.getState().setShownTeams("acme", []);
    expect(useOrgStore.getState().activeTeamName).toBeNull();
    useOrgStore.getState().setShownTeams("acme", ["b", "c"]);
    expect(useOrgStore.getState().activeTeamName).toBe("b");
  });

  it("updates lastTeamByOrgSlug when the active team changes, leaves it untouched when active clears", () => {
    useOrgStore.getState().setShownTeams("acme", ["b", "c"]); // active a→b
    expect(useOrgStore.getState().lastTeamByOrgSlug.acme).toBe("b");
    useOrgStore.getState().setShownTeams("acme", []); // active b→null
    expect(useOrgStore.getState().lastTeamByOrgSlug.acme).toBe("b"); // memory preserved
  });

  it("does NOT reconcile the active team when curating a NON-active org", () => {
    useOrgStore.setState({
      orgs: [
        makeOrg({ slug: "acme", teams: threeTeams }),
        makeOrg({ slug: "beta", teams: threeTeams }),
      ],
    });
    useOrgStore.getState().setShownTeams("beta", ["c"]);
    expect(useOrgStore.getState().activeTeamName).toBe("a"); // active org untouched
    expect(useOrgStore.getState().shownTeamsByOrgSlug.beta).toEqual(["c"]);
  });

  it("never reads or writes membership (no commitRegistryEdit)", () => {
    useOrgStore.getState().setShownTeams("acme", ["a"]);
    expect(mockedCommitRegistryEdit).not.toHaveBeenCalled();
  });
});

describe("shownTeamsByOrgSlug persistence (SW-OB-9)", () => {
  beforeEach(() => {
    mockedLoadAppState.mockReset();
    mockedSaveAppState.mockClear();
    useOrgStore.setState({
      orgs: [],
      activeOrgSlug: null,
      activeTeamName: null,
      lastTeamByOrgSlug: {},
      shownTeamsByOrgSlug: {},
      hasUserIntent: false,
      stateLoaded: false,
    });
  });

  it("restores the persisted shown map verbatim (cold start)", async () => {
    mockedLoadAppState.mockResolvedValue(
      JSON.stringify({
        org: {
          activeOrgSlug: null,
          activeTeamName: null,
          shownTeamsByOrgSlug: { acme: ["a", "c"], beta: [] },
        },
      }),
    );
    await useOrgStore.getState().loadPersistedState();
    expect(useOrgStore.getState().shownTeamsByOrgSlug).toEqual({ acme: ["a", "c"], beta: [] });
  });

  it("defaults to empty map when the field is absent (pre-upgrade JSON)", async () => {
    mockedLoadAppState.mockResolvedValue(
      JSON.stringify({ org: { activeOrgSlug: "acme", activeTeamName: "a" } }),
    );
    await useOrgStore.getState().loadPersistedState();
    expect(useOrgStore.getState().shownTeamsByOrgSlug).toEqual({});
  });

  it("persists the shown map in the org block", async () => {
    vi.useFakeTimers();
    try {
      useOrgStore.setState({
        // Multi-team org so ["a"] is a genuine SUBSET (not all-covered, which
        // would collapse to absent per the SW-OB-9 show-all collapse).
        orgs: [
          makeOrg({
            slug: "acme",
            teams: [
              { name: "a", source_repos: [] },
              { name: "b", source_repos: [] },
            ],
          }),
        ],
        activeOrgSlug: "acme",
        activeTeamName: "a",
        stateLoaded: true,
      });
      useOrgStore.getState().setShownTeams("acme", ["a"]);
      await vi.advanceTimersByTimeAsync(350);
      const lastCall = mockedSaveAppState.mock.calls.at(-1);
      const parsed = JSON.parse(lastCall![0] as string);
      expect(parsed.org.shownTeamsByOrgSlug).toEqual({ acme: ["a"] });
    } finally {
      vi.useRealTimers();
    }
  });

  it("merge-don't-drop: under hasUserIntent, persisted entries for OTHER orgs survive", async () => {
    // A live setter fired (hasUserIntent) and curated only "acme". The persisted
    // block also holds "beta" curation. Dropping it would wipe beta on next
    // persist — the F-39 P1 data-loss trap, mirrored for the shown map.
    useOrgStore.setState({
      hasUserIntent: true,
      shownTeamsByOrgSlug: { acme: ["a"] },
    });
    mockedLoadAppState.mockResolvedValue(
      JSON.stringify({
        org: {
          activeOrgSlug: "acme",
          activeTeamName: "a",
          shownTeamsByOrgSlug: { acme: ["OLD"], beta: ["b1"] },
        },
      }),
    );
    await useOrgStore.getState().loadPersistedState();
    // Live wins on the colliding key; the independent "beta" entry is preserved.
    expect(useOrgStore.getState().shownTeamsByOrgSlug).toEqual({ acme: ["a"], beta: ["b1"] });
  });

  it("merge-don't-drop works even when the lastTeam map is empty (early-return path)", async () => {
    // hasUserIntent with NO remembered-team memory (seededMap empty → early
    // return) but WITH persisted visibility curation. The shown map must still
    // merge, not get skipped by the early return.
    useOrgStore.setState({
      hasUserIntent: true,
      lastTeamByOrgSlug: {},
      shownTeamsByOrgSlug: { acme: ["a"] },
    });
    mockedLoadAppState.mockResolvedValue(
      JSON.stringify({
        org: { activeOrgSlug: null, activeTeamName: null, shownTeamsByOrgSlug: { beta: ["b1"] } },
      }),
    );
    await useOrgStore.getState().loadPersistedState();
    expect(useOrgStore.getState().shownTeamsByOrgSlug).toEqual({ acme: ["a"], beta: ["b1"] });
  });

  it("round-trips a curated set through persist → restore", async () => {
    vi.useFakeTimers();
    try {
      useOrgStore.setState({
        orgs: [
          makeOrg({
            slug: "acme",
            teams: [
              { name: "a", source_repos: [] },
              { name: "b", source_repos: [] },
            ],
          }),
        ],
        activeOrgSlug: "acme",
        activeTeamName: "a",
        stateLoaded: true,
        hasUserIntent: false,
      });
      useOrgStore.getState().setShownTeams("acme", ["a"]);
      await vi.advanceTimersByTimeAsync(350);
      const written = JSON.parse(mockedSaveAppState.mock.calls.at(-1)![0] as string);

      // Fresh store; replay the written blob through loadPersistedState.
      useOrgStore.setState({ shownTeamsByOrgSlug: {}, hasUserIntent: false, stateLoaded: false });
      mockedLoadAppState.mockResolvedValue(JSON.stringify(written));
      await useOrgStore.getState().loadPersistedState();
      expect(useOrgStore.getState().shownTeamsByOrgSlug).toEqual({ acme: ["a"] });
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("SW-OB-9 AC-6 invariant across ALL active-team writers", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useOrgStore.setState({
      orgs: [],
      activeOrgSlug: null,
      activeTeamName: null,
      lastTeamByOrgSlug: {},
      shownTeamsByOrgSlug: {},
      hasUserIntent: false,
      stateLoaded: true,
    });
  });

  it("setActiveOrg: switching into a curated org does NOT land on a hidden team", () => {
    useOrgStore.setState({
      orgs: [makeMultiTeamOrg("acme", ["default", "a", "b"])],
      // remembered team is 'a', but the user has hidden 'a' (shown: only 'b').
      lastTeamByOrgSlug: { acme: "a" },
      shownTeamsByOrgSlug: { acme: ["b"] },
    });
    useOrgStore.getState().setActiveOrg("acme");
    // Must NOT restore the hidden 'a'; lands on the only shown team 'b'.
    expect(useOrgStore.getState().activeTeamName).toBe("b");
  });

  it("setActiveOrg: switching into an all-hidden org lands null (→ no-team state)", () => {
    useOrgStore.setState({
      orgs: [makeMultiTeamOrg("acme", ["a", "b"])],
      lastTeamByOrgSlug: { acme: "a" },
      shownTeamsByOrgSlug: { acme: [] },
    });
    useOrgStore.getState().setActiveOrg("acme");
    expect(useOrgStore.getState().activeTeamName).toBeNull();
  });

  it("setOrgs: reconciliation never lands active on a hidden team", () => {
    // active 'a' is currently shown; a refresh arrives where the user's shown
    // set now excludes 'a' — reconcile off it to the shown 'b'.
    useOrgStore.setState({
      orgs: [makeMultiTeamOrg("acme", ["a", "b"])],
      activeOrgSlug: "acme",
      activeTeamName: "a",
      shownTeamsByOrgSlug: { acme: ["b"] },
    });
    useOrgStore.getState().setOrgs([makeMultiTeamOrg("acme", ["a", "b"])]);
    expect(useOrgStore.getState().activeTeamName).toBe("b");
  });

  it("loadPersistedState: cold restore does not resurrect a hidden persisted active team", async () => {
    useOrgStore.setState({
      orgs: [makeMultiTeamOrg("acme", ["a", "b"])],
      activeOrgSlug: null,
      activeTeamName: null,
      hasUserIntent: false,
      stateLoaded: false,
    });
    mockedLoadAppState.mockResolvedValue(
      JSON.stringify({
        org: {
          activeOrgSlug: "acme",
          activeTeamName: "a", // persisted active team...
          lastTeamByOrgSlug: { acme: "a" },
          shownTeamsByOrgSlug: { acme: ["b"] }, // ...but 'a' is hidden now.
        },
      }),
    );
    await useOrgStore.getState().loadPersistedState();
    // 'a' is hidden → cleared → land on the shown 'b' (not the hidden 'a').
    expect(useOrgStore.getState().activeTeamName).toBe("b");
  });

  it("loadPersistedState: all-hidden persisted org restores to null (no-team state)", async () => {
    useOrgStore.setState({
      orgs: [makeMultiTeamOrg("acme", ["a", "b"])],
      activeOrgSlug: null,
      activeTeamName: null,
      hasUserIntent: false,
      stateLoaded: false,
    });
    mockedLoadAppState.mockResolvedValue(
      JSON.stringify({
        org: {
          activeOrgSlug: "acme",
          activeTeamName: null,
          lastTeamByOrgSlug: { acme: "a" },
          shownTeamsByOrgSlug: { acme: [] },
        },
      }),
    );
    await useOrgStore.getState().loadPersistedState();
    expect(useOrgStore.getState().activeTeamName).toBeNull();
  });

  it("createTeam: a new team created under a curated shown set is shown AND becomes active", async () => {
    mockedGetCurrentUser.mockReturnValue({
      github_user_id: 42,
      github_username: "creator",
    } as ReturnType<typeof getCurrentUser>);
    useOrgStore.setState({
      orgs: [makeMultiTeamOrg("acme", ["a", "b"])],
      activeOrgSlug: "acme",
      activeTeamName: "a",
      shownTeamsByOrgSlug: { acme: ["a"] }, // curated: only 'a' shown ('b' hidden).
    });
    await useOrgStore.getState().createTeam("payments");
    const shown = useOrgStore.getState().shownTeamsByOrgSlug.acme;
    expect(shown).toContain("payments"); // new team joined the shown set...
    expect(shown).not.toContain("b"); // ...without un-hiding 'b'.
    expect(useOrgStore.getState().activeTeamName).toBe("payments"); // AC-6: active ⊆ shown.
  });

  it("loadPersistedState hasUserIntent: reconciles a NON-null live active team hidden by the merged shown map", async () => {
    // A pre-rehydrate setter set active to 'a' (hasUserIntent) using the empty
    // live shown map; the persisted shown map hides 'a'. The merge branch must
    // reconcile active off the now-hidden 'a' to the shown 'b' — not leave it
    // pointing at a hidden team (round-2 finding).
    useOrgStore.setState({
      orgs: [makeMultiTeamOrg("acme", ["a", "b"])],
      activeOrgSlug: "acme",
      activeTeamName: "a",
      lastTeamByOrgSlug: { acme: "b" },
      shownTeamsByOrgSlug: {}, // live map empty (setter fired before rehydrate)
      hasUserIntent: true,
      stateLoaded: false,
    });
    mockedLoadAppState.mockResolvedValue(
      JSON.stringify({
        org: {
          activeOrgSlug: "acme",
          activeTeamName: "b",
          lastTeamByOrgSlug: { acme: "b" },
          shownTeamsByOrgSlug: { acme: ["b"] }, // hides 'a'
        },
      }),
    );
    await useOrgStore.getState().loadPersistedState();
    expect(useOrgStore.getState().shownTeamsByOrgSlug).toEqual({ acme: ["b"] });
    expect(useOrgStore.getState().activeTeamName).toBe("b"); // reconciled off hidden 'a'
  });
});

describe("setShownTeams all-covered → absent collapse (SW-OB-9)", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useOrgStore.setState({
      orgs: [makeMultiTeamOrg("acme", ["a", "b", "c"])],
      activeOrgSlug: "acme",
      activeTeamName: "a",
      lastTeamByOrgSlug: { acme: "a" },
      shownTeamsByOrgSlug: { acme: ["a"] },
      hasUserIntent: false,
      stateLoaded: true,
    });
  });

  it("selecting ALL teams stores ABSENT (show-all), not an explicit all-list", () => {
    useOrgStore.getState().setShownTeams("acme", ["a", "b", "c"]);
    // Key removed → absent → visibleTeams shows all, including future teams.
    expect(useOrgStore.getState().shownTeamsByOrgSlug.acme).toBeUndefined();
    // Active team unaffected (all shown).
    expect(useOrgStore.getState().activeTeamName).toBe("a");
  });

  it("a real subset stays explicit", () => {
    useOrgStore.getState().setShownTeams("acme", ["a", "b"]);
    expect(useOrgStore.getState().shownTeamsByOrgSlug.acme).toEqual(["a", "b"]);
  });
});

// SW-HB-7: team lifecycle (delete / archive) store actions + reconciliation.
describe("SW-HB-7 deleteTeam", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockedGetCurrentUser.mockReturnValue({
      github_user_id: 42,
      github_username: "creator",
    } as ReturnType<typeof getCurrentUser>);
    useOrgStore.setState({
      orgs: [makeMultiTeamOrg("acme", ["a", "b"])],
      activeOrgSlug: "acme",
      activeTeamName: "a",
      lastTeamByOrgSlug: { acme: "a" },
      shownTeamsByOrgSlug: { acme: ["a", "b"] },
      hasUserIntent: true,
      stateLoaded: true,
    });
  });

  // The SW-WG-9 cases below mutate the imported viewer-store's cross-store generic
  // `maximizedPanel` slice; reset it so it never leaks into other suites.
  afterEach(async () => {
    const { useViewerStore } = await import("./viewer-store");
    useViewerStore.setState({ maximizedPanel: {} });
  });

  it("commits a DeleteTeam edit and removes the team from the store (AC-7)", async () => {
    await useOrgStore.getState().deleteTeam("b");
    expect(mockedCommitRegistryEdit).toHaveBeenCalledWith(42, "acme", {
      type: "DeleteTeam",
      team_name: "b",
    });
    const org = useOrgStore.getState().orgs.find((o) => o.slug === "acme")!;
    expect(org.teams.map((t) => t.name)).toEqual(["a"]);
  });

  it("deletes an age-evicted team and its retained terminal state", async () => {
    const { useViewerStore } = await import("./viewer-store");
    const teamKey = "acme:b";
    const workspace = {
      id: "W-1",
      kind: "story" as const,
      cwd: "/wt/W-1",
      status: "ready" as const,
    };
    useViewerStore.setState({ workspaces: { [teamKey]: [workspace] } });
    seedTerminalTabs(teamKey, workspace.id, ["sess-evicted"]);
    useViewerStore.getState().closeTeam(teamKey, { preserveTerminalSessions: true });
    expect(terminalHost().contains("sess-evicted")).toBe(true);

    await useOrgStore.getState().deleteTeam("b");

    expect(terminalHost().contains("sess-evicted")).toBe(false);
    expect(useViewerStore.getState().workspaces[teamKey]).toBeUndefined();
    useOrgStore.getState().addTeamToStore("acme", { name: "b", source_repos: [] });
    expect(useViewerStore.getState().workspaces[teamKey]).toBeUndefined();
  });

  it("deleting the ACTIVE team reconciles active + prunes shown/last maps (AC-5)", async () => {
    await useOrgStore.getState().deleteTeam("a");
    const s = useOrgStore.getState();
    // Active flips to the remaining valid team.
    expect(s.activeTeamName).toBe("b");
    // Deleted name no longer dangles in either curation map.
    expect(s.shownTeamsByOrgSlug.acme ?? []).not.toContain("a");
    expect(Object.values(s.lastTeamByOrgSlug)).not.toContain("a");
  });

  // SW-WG-9: the maximized-panel pointer is a viewer-store slice keyed by teamKey;
  // deleting the team must clear it so a future same-named team (same teamKey) can't
  // resurface already-maximized (the delete twin of closeTeam). One generic slice now,
  // so a single clear covers every surface — no per-panel asymmetry.
  it("SW-WG-9: clears the deleted team's maximized panel", async () => {
    const { useViewerStore } = await import("./viewer-store");
    useViewerStore.setState({ maximizedPanel: { "acme:b": "weft-board" } });
    await useOrgStore.getState().deleteTeam("b");
    expect(useViewerStore.getState().maximizedPanel["acme:b"]).toBeUndefined();
  });

  it("SW-WG-9: leaves a maximized panel at a DIFFERENT team intact on delete", async () => {
    const { useViewerStore } = await import("./viewer-store");
    useViewerStore.setState({ maximizedPanel: { "acme:a": "weft-board" } });
    await useOrgStore.getState().deleteTeam("b");
    expect(useViewerStore.getState().maximizedPanel["acme:a"]).toBe("weft-board");
  });

  it("SW-WG-9: clears non-pipeline/board surfaces too (fixes the former 2-of-4 asymmetry)", async () => {
    const { useViewerStore } = await import("./viewer-store");
    useViewerStore.setState({ maximizedPanel: { "acme:b": "automations" } });
    await useOrgStore.getState().deleteTeam("b");
    expect(useViewerStore.getState().maximizedPanel["acme:b"]).toBeUndefined();
  });
});

describe("SW-HB-7 setTeamArchived", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockedGetCurrentUser.mockReturnValue({
      github_user_id: 42,
      github_username: "creator",
    } as ReturnType<typeof getCurrentUser>);
    useOrgStore.setState({
      orgs: [makeMultiTeamOrg("acme", ["a", "b"])],
      activeOrgSlug: "acme",
      activeTeamName: "a",
      lastTeamByOrgSlug: { acme: "a" },
      shownTeamsByOrgSlug: { acme: ["a", "b"] },
      hasUserIntent: true,
      stateLoaded: true,
    });
  });

  const archivedFlag = (name: string) =>
    useOrgStore
      .getState()
      .orgs.find((o) => o.slug === "acme")!
      .teams.find((t) => t.name === name)?.archived;

  it("reflects OPTIMISTICALLY before the commit resolves (V-4)", async () => {
    let resolveCommit!: () => void;
    mockedCommitRegistryEdit.mockReturnValueOnce(
      new Promise<void>((r) => {
        resolveCommit = r;
      }),
    );
    const p = useOrgStore.getState().setTeamArchived("b", true);
    // Synchronous optimistic flip already happened — before the commit resolves.
    expect(archivedFlag("b")).toBe(true);
    resolveCommit();
    await p;
    expect(mockedCommitRegistryEdit).toHaveBeenCalledWith(42, "acme", {
      type: "ArchiveTeam",
      team_name: "b",
      archived: true,
    });
  });

  it("does NOT prune the personal shown set (V-2 reversibility)", async () => {
    await useOrgStore.getState().setTeamArchived("b", true);
    // The name stays in the explicit shown set so unarchive restores it fully.
    expect(useOrgStore.getState().shownTeamsByOrgSlug.acme).toContain("b");
  });

  it("archiving the ACTIVE team reconciles active off it (V-2)", async () => {
    await useOrgStore.getState().setTeamArchived("a", true);
    expect(useOrgStore.getState().activeTeamName).toBe("b");
  });

  it("rolls the optimistic flip back when the commit fails (V-4)", async () => {
    mockedCommitRegistryEdit.mockRejectedValueOnce(new Error("boom"));
    await expect(useOrgStore.getState().setTeamArchived("b", true)).rejects.toThrow();
    expect(archivedFlag("b")).toBe(false);
  });

  it("restores the ACTIVE team on rollback when the archived team was active (V-4)", async () => {
    // Archiving the active team optimistically reconciles active off it; a
    // failed commit must undo BOTH the flag and the active pointer.
    mockedCommitRegistryEdit.mockRejectedValueOnce(new Error("boom"));
    await expect(useOrgStore.getState().setTeamArchived("a", true)).rejects.toThrow();
    expect(archivedFlag("a")).toBe(false);
    expect(useOrgStore.getState().activeTeamName).toBe("a");
  });

  it("restores active to null on rollback when the LAST team's archive fails (V-4)", async () => {
    // The stranding scenario: single active team → optimistic archive drives
    // active to null → commit fails → active must return to the (now un-archived)
    // team, NOT stay stuck in the no-team state.
    useOrgStore.setState({
      orgs: [makeMultiTeamOrg("acme", ["solo"])],
      activeOrgSlug: "acme",
      activeTeamName: "solo",
      lastTeamByOrgSlug: { acme: "solo" },
      shownTeamsByOrgSlug: {},
      hasUserIntent: true,
      stateLoaded: true,
    });
    mockedCommitRegistryEdit.mockRejectedValueOnce(new Error("boom"));
    await expect(useOrgStore.getState().setTeamArchived("solo", true)).rejects.toThrow();
    expect(archivedFlag("solo")).toBe(false);
    expect(useOrgStore.getState().activeTeamName).toBe("solo");
  });
});

describe("SW-HB-7 setShownTeams archived-awareness (V-3)", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    const org = makeMultiTeamOrg("acme", ["a", "b"]);
    org.teams[1].archived = true; // 'b' archived
    useOrgStore.setState({
      orgs: [org],
      activeOrgSlug: "acme",
      activeTeamName: "a",
      lastTeamByOrgSlug: { acme: "a" },
      shownTeamsByOrgSlug: { acme: ["a"] },
      hasUserIntent: true,
      stateLoaded: true,
    });
  });

  it("the fallback active pick never lands on an archived team", () => {
    // Curate to none: the reconcile fallback must skip archived 'b' → null,
    // NOT resurrect the archived team as active (AC-4).
    useOrgStore.getState().setShownTeams("acme", []);
    expect(useOrgStore.getState().activeTeamName).toBeNull();
  });

  it("selecting all ACTIVE teams collapses the shown set to absent despite an archived team", () => {
    useOrgStore.getState().setShownTeams("acme", ["a"]);
    // 'a' is the only active team → coversAll over active teams → absent.
    expect(useOrgStore.getState().shownTeamsByOrgSlug.acme).toBeUndefined();
  });
});

describe("setTeamLandingPage (team starter page)", () => {
  beforeEach(() => {
    mockedCommitRegistryEdit.mockClear();
    mockedCommitRegistryEdit.mockResolvedValue(undefined);
    mockedGetCurrentUser.mockReturnValue({
      github_user_id: 42,
      github_username: "creator",
    } as ReturnType<typeof getCurrentUser>);
    useOrgStore.setState({
      orgs: [makeOrg({ teams: [{ name: "web", source_repos: [] }] })],
      activeOrgSlug: "acme",
      activeTeamName: "web",
    });
  });

  const web = () =>
    useOrgStore
      .getState()
      .orgs.find((o) => o.slug === "acme")!
      .teams.find((t) => t.name === "web")!;

  it("commits SetTeamLandingPage, then reflects it; null clears", async () => {
    await useOrgStore.getState().setTeamLandingPage("web", " guide/start.md ");
    expect(mockedCommitRegistryEdit).toHaveBeenCalledWith(42, "acme", {
      type: "SetTeamLandingPage",
      team_name: "web",
      landing_page: "guide/start.md",
    });
    expect(web().landing_page).toBe("guide/start.md");

    await useOrgStore.getState().setTeamLandingPage("web", null);
    expect(mockedCommitRegistryEdit).toHaveBeenLastCalledWith(42, "acme", {
      type: "SetTeamLandingPage",
      team_name: "web",
      landing_page: null,
    });
    expect(web().landing_page).toBeNull();
  });

  it("no-ops when unchanged, and leaves state alone when the commit is refused", async () => {
    await useOrgStore.getState().setTeamLandingPage("web", null);
    expect(mockedCommitRegistryEdit).not.toHaveBeenCalled();

    mockedCommitRegistryEdit.mockRejectedValueOnce(new Error("not_found: no file"));
    await expect(useOrgStore.getState().setTeamLandingPage("web", "nope.md")).rejects.toThrow(
      "no file",
    );
    expect(web().landing_page ?? null).toBeNull();
  });
});
