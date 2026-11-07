// SPDX-License-Identifier: MIT
import { applyFieldPatch } from "@/shared/lib/field-patch";
import { CODESENSE_DOCS_README, CODESENSE_DOCS_TEAM } from "@/shared/lib/codesense-docs";
import type { HandlerRegistry } from "../mock-router";
import type { Org, Team } from "@/shared/types/domain";
import type { LoadedOrg } from "@/shared/types/registry";
import {
  MOCK_CODESENSE_ORG_SLUG,
  MOCK_TEAM_NAME,
  mockAppState,
  mockCodesenseLoadedOrg,
  mockCodesenseOrgSummary,
  mockDependencyStatus,
  mockFirstRunAppState,
  mockLoadedOrg,
  mockOrg,
  mockOrgSummary,
  mockSourceRepoSlug,
  MOCK_USER,
} from "../fixtures";
import type { MockScenario } from "../scenario";
import type { LocalProfileStatus } from "@/shared/stores/local-profile-store";

/** Project the org's team list from the scenario: no team past the
 *  NoTeamInOrg rung, otherwise the single default team carrying
 *  `sourceRepoCount` declared repos (which the NoSourceRepos rung drops to
 *  zero). */
function scenarioTeams(scenario: MockScenario): Team[] {
  if (!scenario.hasTeam) return [];
  const source_repos = Array.from({ length: scenario.sourceRepoCount }, () => ({
    ...mockSourceRepoSlug,
  }));
  return [{ name: MOCK_TEAM_NAME, source_repos }];
}

const mockLocalProfile = {
  principal: String(MOCK_USER.userId),
  displayName: MOCK_USER.displayName,
  machineOwner: { kind: "legacyGithub" as const, id: MOCK_USER.userId },
};

const mockLocalProfileStatus: LocalProfileStatus = {
  active: mockLocalProfile,
  profiles: [mockLocalProfile],
  privateAuthority: null,
  metadataRecovery: { pending: false, error: null },
};

/** The handful of commands the boot sequence awaits before it can land in a
 *  workspace: local profile, dependency check, persisted-state load, and org
 *  discovery. Returning ready + fixture shapes drives the REAL boot flow past
 *  every gate — no store-internal seeding required.
 *
 *  Org discovery is scenario-driven so the empty-state ladder's upper rungs are
 *  reachable: `hasOrg` gates whether any org is discoverable (NoOrg), and the
 *  loaded org's team list reflects `hasTeam` / `sourceRepoCount` (NoTeamInOrg,
 *  NoSourceRepos). Everything else is scenario-independent. */
export function bootHandlers(scenario: MockScenario): HandlerRegistry {
  let savedAppState: string | null = null;
  const teams = scenarioTeams(scenario);
  const org: Org = { ...mockOrg, teams };
  const loadedOrg: LoadedOrg = { ...mockLoadedOrg, org };
  // SW-UX (app-flow redesign): the default "CodeSense" reference org is ALWAYS
  // discoverable (it ships auto-added on launch), listed FIRST; the user's own org
  // follows per scenario. This is what makes the workspace switcher show the
  // pinned CodeSense help workspace alongside the user's work.
  const orgSummaries = [mockCodesenseOrgSummary, ...(scenario.hasOrg ? [mockOrgSummary] : [])];

  return {
    // The mock user is the selected local profile, so every principal-scoped read
    // (persisted state, shortcuts, watchers) resolves to them.
    local_profile_status: () => mockLocalProfileStatus,
    get_dependency_status: () => mockDependencyStatus,
    // SW-UX (app-flow redesign): a brand-new user with NO org of their own
    // (`!hasOrg`) lands IN the default CodeSense reference org (help content);
    // everyone else resumes their own workspace.
    // App state round-trips in memory for the session (a reload starts fresh), so
    // what the app saves — layout, settings, dashboard prefs — reads back.
    load_app_state: () => savedAppState ?? (scenario.hasOrg ? mockAppState : mockFirstRunAppState),
    save_app_state: (args) => {
      if (typeof args.state === "string")
        savedAppState = args.patch
          ? JSON.stringify(
              applyFieldPatch(
                JSON.parse(
                  savedAppState ?? (scenario.hasOrg ? mockAppState : mockFirstRunAppState),
                ),
                JSON.parse(args.state),
              ),
            )
          : args.state;
      return undefined;
    },
    // SW-LN-B20: the frontend declares the local guest session; nothing to track in mock mode.
    set_guest_session: () => undefined,
    // SW-LN-B47: no on-disk guest marker in mock mode.
    get_guest_session: () => false,
    discover_local_orgs: () => orgSummaries,
    // SW-OC-16: the bundled docs repo is already placed in mock mode.
    ensure_codesense_docs: () => ({
      org_slug: MOCK_CODESENSE_ORG_SLUG,
      org_root: mockCodesenseOrgSummary.org_root,
      team: CODESENSE_DOCS_TEAM,
      readme_path: `${mockCodesenseOrgSummary.org_root}/${CODESENSE_DOCS_README}`,
      placement: "kept",
    }),
    // SW-ODC-11: the bundled sample source repo (always available in mock mode).
    ensure_sample_repo: () => ({
      root: `${mockCodesenseOrgSummary.org_root}/sample-source-repo`,
      bundledCommit: "0000000000000000000000000000000000000000",
      placement: "kept",
      available: true,
      refusedReason: null,
    }),
    // Resolve the requested org by slug — the CodeSense reference org or the
    // user's own org — so switching between them in the dropdown loads the right one.
    load_org_cmd: (args) =>
      String(args.slug) === MOCK_CODESENSE_ORG_SLUG ? mockCodesenseLoadedOrg : loadedOrg,

    // R-10 per-repo workspace overrides. A first-run workspace has no overrides
    // yet, so an empty config is the honest answer — and it clears the
    // "Workspace config failed to load" degraded-toast the unmocked throw
    // produced on team resolution.
    load_workspace_config: () => ({ repos: {} }),
    save_workspace_config: () => undefined,

    // The agent roster the right pane would render. The automations/agents surface is carved OUT of the
    // onboarding branch, so there is no seeded fleet here — an empty roster is the honest answer.
    dashboard_list_sessions: () => [],
    wv_list_roster: () => [],
    // No agents, so no messages between them.
    list_inbox_messages: () => ({ messages: [], humanUnread: 0 }),
    // The page bridge's agents read model: nothing running, nothing queued.
    agent_read_sessions: () => [],
    agent_read_inbox: () => ({ messages: [], humanUnread: 0 }),
    list_inbox_rollup: () => ({ messages: [], roles: [] }),
    agent_read_roles: () => ({ roles: [] }),

    // The pipeline tree scan. The automation story subjects are carved OUT of the onboarding branch, so an
    // empty tree (no epics, no stories) is the honest answer for a fresh onboarding surface.
    scan_pipeline_tree: (args) => ({
      dirPath: "",
      dirName: "",
      absoluteDirPath: String(args.teamContentPath).replace(/\/+$/, ""),
      stories: [],
      subEpics: [],
      orderError: null,
      mainDoc: null,
    }),

    // On-disk worktrees. No seeded story worktrees on the onboarding branch — an empty list is honest.
    list_worktrees: () => [],

    // Inert backend infrastructure, not UI data: mock mode has no filesystem
    // watcher, no update channel, and no native OS menu. A loud stub here would
    // only spam the console on every boot with a gap no UI author can act on, so
    // these are honest no-ops.
    start_watching: () => undefined,
    check_for_update: () => null,
    rebuild_menu: () => undefined,

    // SW-AU-UI-1: RF-29 has NO PTY. Terminals open through the mock terminal host
    // (`handlers/terminal-layout`); typing and resizing are inert — no live output
    // ever fires, so a terminal shows only its canned scrollback. The real backend
    // streams the live shell here.
    write_pty: () => undefined,
    resize_pty: () => undefined,

    // SW-LN-35: Settings → Terminal shell. Mock mode has no login shell to read,
    // so it reports the macOS default and echoes back whatever override is set.
    terminal_shell_info: () => mockShellInfo(undefined),
    set_terminal_shell_override: (args) => mockShellInfo(args.shell as string | null | undefined),
  };
}

function mockShellInfo(override: string | null | undefined) {
  const overridePath = override?.trim() || null;
  const usable = overridePath != null && /^\/(.*\/)?(zsh|bash|fish)$/.test(overridePath);
  return {
    path: usable ? overridePath : "/bin/zsh",
    kind: usable ? (overridePath.split("/").pop() as "zsh" | "bash" | "fish") : "zsh",
    source: usable ? "override" : "env",
    detectedPath: "/bin/zsh",
    overridePath,
    overrideRejected: overridePath != null && !usable,
    unsupportedShell: null,
  };
}
