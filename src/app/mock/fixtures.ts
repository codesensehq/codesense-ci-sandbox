// SPDX-License-Identifier: MIT
import { AGENT_RUNTIMES } from "@/shared/lib/agent-runtimes";
import {
  CODESENSE_DOCS_DISPLAY_NAME,
  CODESENSE_DOCS_SLUG,
  CODESENSE_DOCS_TEAM,
} from "@/shared/lib/codesense-docs";
import type { AgentBinaryStatus, DependencyStatus } from "@/tauri/commands";
import type { Attachment, Org, OrgSummary, SourceRepoSlug } from "@/shared/types/domain";
import type { LoadedOrg } from "@/shared/types/registry";
import { sourceRepoRemoteKey } from "@/shared/lib/source-repo-key";

export const MOCK_USER = {
  userId: 4242,
  username: "mock-dev",
  displayName: "Mock Dev",
} as const;

export const MOCK_ORG_SLUG = "acme/widgets";
export const MOCK_TEAM_NAME = "default";

/** The single source repo a team declares once it's past the NoSourceRepos
 *  rung. Owner/repo mirror the org's GitHub remote so the source-repos panel
 *  and the context-repository row read consistently. */
export const mockSourceRepoSlug: SourceRepoSlug = { owner: "acme", repo: "widgets" };

const MOCK_AGENT_INSTALLS: Record<string, AgentBinaryStatus> = {
  claude: {
    available: true,
    path: "/usr/local/bin/claude",
    resolving: false,
    version: "2.1.296",
    source: "loginPath",
    shadowed: [],
    qualified: null,
    supported: null,
  },
  hermes: {
    available: true,
    path: "/usr/local/bin/hermes",
    resolving: false,
    version: "0.22.5",
    source: "loginPath",
    shadowed: [],
    qualified: true,
    supported: "Hermes Agent 0.22.5",
  },
};

/** Dependency discovery includes every registered CLI in registry order, installed or absent. */
export const mockDependencyStatus: DependencyStatus = {
  git: { available: true, path: "/usr/bin/git", version: "2.43.0" },
  agents: Object.fromEntries(
    AGENT_RUNTIMES.map((cli) => [
      cli,
      MOCK_AGENT_INSTALLS[cli] ?? {
        available: false,
        path: null,
        resolving: false,
        version: null,
        source: null,
        shadowed: [],
        qualified: null,
        supported: null,
      },
    ]),
  ),
};

/** The one source repo the fixture team tracks, with a pre-resolved local clone —
 *  so boot lands directly in the workspace instead of the onboarding gates (which
 *  need a real file picker + git that mock mode can't provide). */
export const MOCK_SOURCE_REPO: SourceRepoSlug = { owner: "acme", repo: "widgets" };
export const MOCK_SOURCE_REPO_URL = "https://github.com/acme/widgets";
/** The clone path the resolved attachment reports; not a real dir (mock never
 *  touches disk) — it exists only to satisfy the `localPath != null` clone gate. */
export const MOCK_CLONE_PATH = "/mock/clones/widgets";

export const mockOrg: Org = {
  slug: MOCK_ORG_SLUG,
  display_name: "Acme Widgets",
  created_at: "2026-01-01T00:00:00Z",
  remote_url: "https://github.com/acme/widgets.git",
  config_flags: {},
  people: [],
  // Teams here are the DEFAULT; boot's `bootHandlers` overrides the discovered org's team list with
  // `scenarioTeams(scenario)` so the empty-state ladder is reachable. AU keeps a source_repo on the
  // default so non-scenario readers of `mockOrg` still resolve the fixture repo.
  teams: [{ name: MOCK_TEAM_NAME, source_repos: [MOCK_SOURCE_REPO] }],
  // The mock never touches disk, so the concrete prefix is irrelevant; TEAM_CONTENT_PATH derives
  // from this, so every seeded run/artifact path stays internally consistent.
  org_root: `/mock/users/${MOCK_USER.userId}/orgs/${MOCK_ORG_SLUG}`,
  // The resolved `contextRepoPath` per `${team}\n${remoteKey}` — the ONLY source
  // `buildSyntheticRepo` reads for a synthetic repo's context path (Rust populates it in prod;
  // the mock must too). It serves BOTH stacked features and the `${team}/content` layer-marker
  // shape below satisfies both:
  //  - SW-AU-UI-1: without a non-empty context path the repo is "unavailable", so
  //    `usePipelineTree`'s `canScan` (needs BOTH context + content paths) never fires → the
  //    pipeline tree stays null → story workspaces fall to Ungrouped instead of their epic pill.
  //  - SW-SV-16: `deriveOrgRoot` parses `{org_root}` back out of the `{team}/content` tail so the
  //    two-repo sync view can source a publish set. (`MOCK_CLONE_PATH` has no layer marker →
  //    `deriveOrgRoot` returns it unchanged + warns, breaking the sync view; the clone path is
  //    already provided separately via `seedAttachmentsByRemote`.)
  resolved_repository_paths: {
    [`${MOCK_TEAM_NAME}\n${sourceRepoRemoteKey(MOCK_SOURCE_REPO)}`]: `/mock/users/${MOCK_USER.userId}/orgs/${MOCK_ORG_SLUG}/${MOCK_TEAM_NAME}/content`,
  },
};

/** The `attachments.yml` projection `list_attachments_by_remote` returns — keyed by
 *  the lowercase remote key `buildSyntheticRepo` looks up, so the fixture team's
 *  synthetic repo resolves a `localPath` and the required-clone gate is satisfied. */
export function seedAttachmentsByRemote(): Record<string, Attachment> {
  return {
    [sourceRepoRemoteKey(MOCK_SOURCE_REPO)]: {
      repo_path: MOCK_CLONE_PATH,
      remote_url: MOCK_SOURCE_REPO_URL,
      attached_at: "2026-01-01T00:00:00Z",
      tracking_prompt_dismissed_at: null,
      credential_ref: null,
    },
  };
}

export const mockOrgSummary: OrgSummary = {
  slug: MOCK_ORG_SLUG,
  display_name: "Acme Widgets",
  has_remote: true,
  remote_url: mockOrg.remote_url,
  org_root: mockOrg.org_root,
};

// The team's content dir (`{org_root}/{team}/content`) the left nav lists under. Defined HERE (early) so
// the demo run's minted-doc artifacts can point their `reference` at real, openable content files — the
// run's artifact panel diffs them in-app AND "Open in workspace" opens the actual doc in the viewer.
export const TEAM_CONTENT_PATH = `${mockOrg.org_root}/${MOCK_TEAM_NAME}/content`;

export const mockLoadedOrg: LoadedOrg = {
  slug: MOCK_ORG_SLUG,
  root: mockOrg.org_root,
  org: mockOrg,
  partial: { org_yml: null, people_yml: null, teams_yml: null },
  file_errors: [],
};

// ── The bundled CodeSense docs context repo (SW-OC-16) ───────────────────────
// Every install ships it (a git clone placed on first launch, read-only in the app,
// pinned in the switcher). On the mock it's always discoverable alongside the user's.
export const MOCK_CODESENSE_ORG_SLUG = CODESENSE_DOCS_SLUG;

export const mockCodesenseOrg: Org = {
  slug: MOCK_CODESENSE_ORG_SLUG,
  display_name: CODESENSE_DOCS_DISPLAY_NAME,
  created_at: "2026-01-01T00:00:00Z",
  remote_url: null,
  config_flags: {},
  people: [],
  teams: [{ name: CODESENSE_DOCS_TEAM, source_repos: [], landing_page: "README.html" }],
  org_root: `/mock/.codesense/users/${MOCK_USER.userId}/codesense-docs`,
};

export const mockCodesenseOrgSummary: OrgSummary = {
  slug: MOCK_CODESENSE_ORG_SLUG,
  display_name: CODESENSE_DOCS_DISPLAY_NAME,
  has_remote: false,
  remote_url: null,
  org_root: mockCodesenseOrg.org_root,
};

export const mockCodesenseLoadedOrg: LoadedOrg = {
  slug: MOCK_CODESENSE_ORG_SLUG,
  root: mockCodesenseOrg.org_root,
  org: mockCodesenseOrg,
  partial: { org_yml: null, people_yml: null, teams_yml: null },
  file_errors: [],
};

/** The persisted-state blob both `useConfigStore.loadState` and
 *  `useOrgStore.loadPersistedState` read (via `load_app_state`), pre-selecting
 *  the fixture org + team so boot lands deterministically. */
export const mockAppState = JSON.stringify({
  repos: [],
  org: {
    activeOrgSlug: MOCK_ORG_SLUG,
    activeTeamName: MOCK_TEAM_NAME,
    lastTeamByOrgSlug: { [MOCK_ORG_SLUG]: MOCK_TEAM_NAME },
  },
  // SW-UX (app-flow redesign): the launch model drops you straight into the org
  // workspace. Boot lands in the team surface when a team is active AND that team
  // is in `openedTeams`; seeding it here proves that target on the mock.
  viewer: {
    openedTeams: [`${MOCK_ORG_SLUG}:${MOCK_TEAM_NAME}`],
  },
});

/** The FIRST-RUN persisted blob — a brand-new user with no context repo of their own
 *  lands IN the bundled CodeSense docs (SW-OC-16), team `getting-started`. Once they
 *  connect or create one, later launches resume there (the `mockAppState` blob above). */
const CODESENSE_TEAM = CODESENSE_DOCS_TEAM;
export const mockFirstRunAppState = JSON.stringify({
  repos: [],
  org: {
    activeOrgSlug: MOCK_CODESENSE_ORG_SLUG,
    activeTeamName: CODESENSE_TEAM,
    lastTeamByOrgSlug: { [MOCK_CODESENSE_ORG_SLUG]: CODESENSE_TEAM },
  },
  viewer: {
    openedTeams: [`${MOCK_CODESENSE_ORG_SLUG}:${CODESENSE_TEAM}`],
  },
});
