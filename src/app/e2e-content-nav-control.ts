// SPDX-License-Identifier: MIT
import { resolveTeamPipelineRepoPath } from "@/shared/hooks/use-team-pipeline-repo-path";
import { resolveContentSource } from "@/shared/lib/resolve-content-source";
import { scanTeamPipelineTree } from "@/shared/lib/scan-team-pipeline-tree";
import { useConfigStore } from "@/shared/stores/config-store";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { useUiStore } from "@/shared/stores/ui-store";
import { type LayoutMode, useViewerStore } from "@/shared/stores/viewer-store";
import { teamAllContextId } from "@/shared/lib/context-model";
import type { Context } from "@/shared/lib/context-model";
import type { AgentContextSelection } from "@/features/agent-context/lib/agent-context-model";
import { resolveTeamTargetContentRoot } from "@/features/context-mobility/context-target-facts";
import { setSlotDashboard } from "@/features/dashboard/lib/dashboard-profile";
import { readSharedUiState } from "@/features/dashboard/lib/shared-ui-state";
import { openSettingsTab } from "@/features/settings/lib/open-settings-tab";

// ---------------------------------------------------------------------------
// SLW-PT-18 — WebDriver-only control door for the content-mobility ⋯ nav action.
//
// PRODUCTION-FAITHFUL NATURAL BOOT. The content-repo org tree is materialized
// ON DISK by the harness (`tests/e2e-real/support/org-tree.ts`)
// under `{HOME}/.codesense/users/{userId}/orgs/{owner}/{repo}/…`, so the app's
// OWN boot discovers it exactly as production does:
//
//   useCurrentUser  (harness GITHUB_ONLY_SEED, userId=1)  →
//   useOrgDiscovery → discoverLocalOrgs + loadOrg → setOrgs(loadedOrgs)  →
//     (each org carries backend-resolved `resolved_repository_paths`, the
//      source of the non-empty `contextRepoPath` the nav's `usePipelineTree`
//      `canScan` gate needs — the exact field a post-boot store swap omitted) →
//   resolveTeamContentContext derives the registered team content root →
//   usePipelineTree scans that dir independent of source repos or workspaces.
//
// The door therefore NEVER swaps store state to fake the tree. It only:
//   1. reports when boot/discovery has settled (`discoverySettled`), and
//   2. drives the journey ENTRY seams — enter a discovered team through the
//      SAME production `openTeam` + `setActiveTeam` route App uses, selecting
//      the All-of-Team or Personal context — plus the cross-org switch and
//      create-team seams the picker journeys exercise.
//
// Behind App.tsx's `MODE === "webdriver"` guard, tree-shaken from — and proven
// absent (`scripts/verify-webdriver-absence.ts`) in — the shipped prod build.
// ---------------------------------------------------------------------------

/** Deterministic team key the harness + door share (`{orgSlug}:{teamName}`). */
function teamKeyOf(orgSlug: string, teamName: string): string {
  return `${orgSlug}:${teamName}`;
}

export interface ContentNavEnterSnapshot {
  activeOrgSlug: string | null;
  activeTeamName: string | null;
  teamKey: string;
  teamContentPath: string | null;
  /** SW-XS-15: the left-nav pivot (`null` = Team (everything)). */
  selectedAgentContext: AgentContextSelection | null;
}

/** True once the app's natural boot has settled enough for the pipeline nav to
 *  scan: config initialized, viewer state loaded + scope recovery complete, and
 *  org discovery has populated at least one org. The driver polls this before
 *  entering a team so the seam can't race a late boot flag. */
function discoverySettled(): boolean {
  const config = useConfigStore.getState();
  const viewer = useViewerStore.getState();
  const org = useOrgStore.getState();
  return (
    config.initialized &&
    viewer.stateLoaded &&
    viewer.scopeRecoveryComplete &&
    org.stateLoaded &&
    org.orgs.length > 0
  );
}

/** Use the product's team-content identity, including source-free teams. */
function navRepoPathFor(teamKey: string): string {
  return resolveTeamPipelineRepoPath(teamKey, useOrgStore.getState().orgs);
}

function teamContentPathFor(teamKey: string): string | null {
  return resolveContentSource(navRepoPathFor(teamKey))?.teamContentPath ?? null;
}

function snapshotFor(teamKey: string): ContentNavEnterSnapshot {
  const org = useOrgStore.getState();
  return {
    activeOrgSlug: org.activeOrgSlug,
    activeTeamName: org.activeTeamName,
    teamKey,
    teamContentPath: teamContentPathFor(teamKey),
    selectedAgentContext: useViewerStore.getState().selectedAgentContext?.[teamKey] ?? null,
  };
}

/**
 * Enter a DISCOVERED team through the production route, selecting the given
 * Context. Reads the org/team from the store the natural boot already
 * populated — it does NOT create or swap org state. Returns a snapshot so the
 * driver can assert the resolved team-content path.
 */
function enterTeam(orgSlug: string, teamName: string): ContentNavEnterSnapshot {
  const teamKey = teamKeyOf(orgSlug, teamName);

  // SW-XS-15: browse the team's own content (Team (everything)): no agent overlay.
  useViewerStore.getState().setSelectedAgentContext(teamKey, null);

  // Enter through the same production route App uses to mount TeamWorkspaceView.
  useOrgStore.getState().setActiveOrg(orgSlug);
  useViewerStore.getState().openTeam(teamKey);
  useOrgStore.getState().setActiveTeam(teamName);

  bumpScan();
  return snapshotFor(teamKey);
}

/** Switch the active org through the real store action — the §K cross-org
 *  carry-switch-resume leg the picker's disabled cross-org row drives. */
function switchOrg(slug: string): void {
  useOrgStore.getState().setActiveOrg(slug);
}

/** Open the team-management surface focused on the new-team field — the §E
 *  zero-teams "Create a team" seam the picker's next-step offers. */
function openCreateTeam(): void {
  useUiStore.getState().setManageTeamsIntent("create");
}

/** The live team-management intent — `"create"` once the zero-teams "Create a
 *  team" next step has opened the management surface. The driver reads this to
 *  prove the §E create-team seam actually fired (the button really opened the
 *  create surface, not just rendered). */
function manageTeamsIntent(): "list" | "create" | null {
  return useUiStore.getState().manageTeamsIntent;
}

/** Reset the team-management intent to idle between journeys, so a prior run's
 *  "create" can't be mistaken for a fresh fire. */
function resetManageTeamsIntent(): void {
  useUiStore.getState().setManageTeamsIntent(null);
}

/** The currently active org slug — the driver reads this to confirm a
 *  cross-org / zero-teams org switch actually landed. */
function activeOrgSlug(): string | null {
  return useOrgStore.getState().activeOrgSlug;
}

/** Re-kick the pipeline scan for every registered team content root — the driver calls
 *  this while polling for the doc rows, so a scan that raced the re-render is
 *  retried through the SAME per-repo refresh counter the production watcher
 *  bumps. */
function bumpScan(): void {
  for (const org of useOrgStore.getState().orgs) {
    for (const team of org.teams) {
      const key = navRepoPathFor(teamKeyOf(org.slug, team.name));
      if (key) useFileRefreshStore.getState().bumpFileRefresh(key);
    }
  }
}

/** Read-only oracle: how many `[data-testid="move-action-trigger"]` are mounted
 *  in the pipeline nav — the single "the nav mounted a content-doc ⋯ row"
 *  assertion. */
function moveActionTriggerCount(): number {
  if (typeof document === "undefined") return 0;
  return document.querySelectorAll('[data-testid="move-action-trigger"]').length;
}

/** Probe the exact scan the nav's `usePipelineTree` runs for the active team —
 *  reports the doc count, so a driver can distinguish "the scan path is broken"
 *  from "the React render didn't commit". Diagnostic-only. */
async function probeScan(teamKey: string): Promise<Record<string, unknown>> {
  const repoPath = navRepoPathFor(teamKey);
  const source = resolveContentSource(repoPath);
  if (!source) return { error: "team content unavailable", teamKey };
  const tree = await scanTeamPipelineTree(teamKey);
  if (!tree) return { error: "team content scan failed", repoPath };
  return {
    repoPath,
    teamContentPath: source.teamContentPath,
    contextRepoPath: source.contextRepoPath,
    docCount: tree.docs?.length ?? 0,
    docNames: (tree.docs ?? []).map((d) => d.fileName),
  };
}

/** Diagnostic-only: invoke the REAL move backend directly (bypassing the sheet)
 *  so the driver can read the raw outcome / error string a failing click only
 *  shows as a generic toast. Isolates a product backend failure from an FE flow
 *  issue. `from`/`to` are team-all endpoints by org+team; a null team means the
 *  Personal endpoint. */
async function probeMove(
  fromOrg: string,
  fromTeam: string | null,
  toOrg: string,
  toTeam: string | null,
  logicalName: string,
  userId: number,
): Promise<Record<string, unknown>> {
  try {
    const { moveContentBetweenContexts } = await import("@/tauri/commands");
    const endpoint = (org: string, team: string | null) =>
      team === null
        ? ({ kind: "personal" } as const)
        : { kind: "teamAll" as const, orgSlug: org, teamName: team };
    const outcome = await moveContentBetweenContexts({
      from: endpoint(fromOrg, fromTeam),
      to: endpoint(toOrg, toTeam),
      logicalName,
      userId,
    });
    return { ok: true, outcome };
  } catch (e) {
    return { ok: false, error: String(e) };
  }
}

/** Diagnostic-only: resolve a move-TARGET team the SAME way the §P collision
 *  gate does (`resolveTeamTargetContentRoot` off the loaded org's `org_root`) and
 *  list its content dir, so a driver can see whether the FE can observe an
 *  existing doc at the target (the fact that fires the collision chooser).
 *  Isolates "collision fact never resolved" from "gate wiring broken". */
async function probeTargetFacts(
  targetOrg: string,
  targetTeam: string,
  logicalName: string,
): Promise<Record<string, unknown>> {
  const orgs = useOrgStore.getState().orgs;
  const orgRoots = orgs.map((o) => ({ slug: o.slug, org_root: o.org_root }));
  const target: Context = {
    id: teamAllContextId(targetOrg, targetTeam),
    kind: "team-all",
    sharing: "shared",
    label: `All of ${targetTeam}`,
    teamName: targetTeam,
  };
  const root = resolveTeamTargetContentRoot(target, orgs);
  if (!root) return { error: "no target content root resolved", targetId: target.id, orgRoots };
  try {
    const { listContextFiles } = await import("@/tauri/commands");
    const files = await listContextFiles(root);
    const rels = files.map((f) => f.relativePath);
    return { root, files: rels, has: rels.includes(logicalName), logicalName, orgRoots };
  } catch (e) {
    return { error: String(e), root, logicalName, orgRoots };
  }
}

/** Diagnostic oracle for the seeded nav-mount path — the resolved live state a
 *  driver reads to explain a not-mounted nav. */
function diagnostics(teamKey: string): Record<string, unknown> {
  const org = useOrgStore.getState();
  const viewer = useViewerStore.getState();
  const config = useConfigStore.getState();
  const navRepoPath = navRepoPathFor(teamKey);
  const registered = resolveContentSource(navRepoPath);
  const navRepoRegistered = Boolean(registered);
  return {
    navRepoPath,
    navRepoRegistered,
    navTeamContentPath: registered?.teamContentPath ?? null,
    navContextRepoPath: registered?.contextRepoPath ?? null,
    activeOrgSlug: org.activeOrgSlug,
    activeTeamName: org.activeTeamName,
    orgSlugs: org.orgs.map((o) => o.slug),
    workspaceRepoKeys: Object.keys(org.workspaceRepos),
    discoveryComplete: org.discoveryComplete,
    orgs: org.orgs.map((item) => ({
      slug: item.slug,
      root: item.org_root,
      teams: item.teams.map((team) => ({ name: team.name, sources: team.source_repos.length })),
    })),
    workspaceIds: (viewer.workspaces[teamKey] ?? []).map((workspace) => workspace.id),
    configInitialized: config.initialized,
    viewerStateLoaded: viewer.stateLoaded,
    orgStateLoaded: org.stateLoaded,
    scopeRecoveryComplete: viewer.scopeRecoveryComplete,
    openedTeams: viewer.openedTeams,
    selectedAgentContext: viewer.selectedAgentContext?.[teamKey] ?? null,
  };
}

/** The WebDriver-only content-nav fixture control surface. */
export interface ContentNavE2eControl {
  /** True once natural boot + org discovery have settled. */
  discoverySettled: () => boolean;
  /** Enter a discovered team through the production route + select a Context. */
  enterTeam: (orgSlug: string, teamName: string) => ContentNavEnterSnapshot;
  snapshot: (teamKey: string) => ContentNavEnterSnapshot;
  /** Drive real layout actions without exposing dev-only stores in the production renderer.
   *  The layout is one for every team (D39). */
  setLayout: (mode: LayoutMode) => void;
  /** Open a content file through the same viewer action as a nav click. */
  openContentFile: (teamKey: string, path: string) => void;
  chooseFileNav: (org: string, dashboard: boolean) => Promise<void>;
  /** The host's copy of a team's `ui:file-nav` state (selection, expanded folders):
   *  what a driver can observe of a cross-origin file-nav pane. */
  fileNavUi: (org: string, team: string) => Promise<Record<string, unknown>>;
  collapseFiles: (teamKey: string) => void;
  closeContentTabs: (teamKey: string) => void;
  openAppearance: () => void;
  switchOrg: (slug: string) => void;
  openCreateTeam: () => void;
  /** The live team-management intent — proves the §E create-team seam fired. */
  manageTeamsIntent: () => "list" | "create" | null;
  /** Reset the team-management intent to idle between journeys. */
  resetManageTeamsIntent: () => void;
  /** The active org slug — confirms an org switch landed. */
  activeOrgSlug: () => string | null;
  /** How many ⋯ move-action triggers are currently mounted in the nav. */
  moveActionTriggerCount: () => number;
  /** Resolved live nav-mount state — explains a not-mounted nav. */
  diagnostics: (teamKey: string) => Record<string, unknown>;
  /** Re-kick the pipeline scan for every registered team content root. */
  bumpScan: () => void;
  /** Probe the nav's exact scan path and report the doc count. */
  probeScan: (teamKey: string) => Promise<Record<string, unknown>>;
  /** Invoke the REAL move backend directly and report the raw outcome/error. */
  probeMove: (
    fromOrg: string,
    fromTeam: string | null,
    toOrg: string,
    toTeam: string | null,
    logicalName: string,
    userId: number,
  ) => Promise<Record<string, unknown>>;
  /** Resolve + list a move-target team's content dir the way the collision gate
   *  does — reports whether the FE can see an existing doc at the target. */
  probeTargetFacts: (
    targetOrg: string,
    targetTeam: string,
    logicalName: string,
  ) => Promise<Record<string, unknown>>;
}

declare global {
  interface Window {
    __CODESENSE_CONTENT_NAV_E2E__?: ContentNavE2eControl;
  }
}

export function registerContentNavE2eControl(): void {
  window.__CODESENSE_CONTENT_NAV_E2E__ = {
    discoverySettled,
    enterTeam,
    snapshot: snapshotFor,
    setLayout: (mode) => {
      useViewerStore.getState().setLayoutMode(mode);
    },
    openContentFile: (teamKey, path) => {
      const identity = navRepoPathFor(teamKey);
      if (identity) useViewerStore.getState().openFile(teamKey, identity, path);
    },
    fileNavUi: (org, team) =>
      readSharedUiState(JSON.stringify(["ui:file-nav", `team:${org}/${team}`])),
    chooseFileNav: (org, dashboard) =>
      setSlotDashboard(
        org,
        "file-nav",
        dashboard ? { scope: "built-in", org, id: "files-tree" } : "native",
      ),
    collapseFiles: (teamKey) => {
      const viewer = useViewerStore.getState();
      for (const path of viewer.statusNavExpandedPaths[teamKey] ?? [])
        useViewerStore.getState().toggleStatusNavExpanded(teamKey, path);
    },
    closeContentTabs: (teamKey) => useViewerStore.getState().closeAllTabs(teamKey),
    openAppearance: () => openSettingsTab("appearance"),
    switchOrg,
    openCreateTeam,
    manageTeamsIntent,
    resetManageTeamsIntent,
    activeOrgSlug,
    moveActionTriggerCount,
    diagnostics,
    bumpScan,
    probeScan,
    probeMove,
    probeTargetFacts,
  };
}
