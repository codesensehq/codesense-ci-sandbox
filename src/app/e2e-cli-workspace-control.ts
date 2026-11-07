// SPDX-License-Identifier: MIT
import { getCurrentWindow } from "@tauri-apps/api/window";
import { focusWorkspaceTerminal } from "@/features/viewer/lib/focus-cascade";
import { syntheticRepoKey } from "@/shared/lib/build-synthetic-repo";
import { useOrgStore } from "@/shared/stores/org-store";
import {
  activeForegroundLayout,
  activeForegroundTiles,
  effectiveMosaicRoot,
  expandedGridWorkspaceId,
  expandedWorkspaceId,
  selectExpandedWorkspaceId,
  type ForegroundLayout,
  type LayoutMode,
  selectActiveTerminalTab,
  selectTabGridCollapsed,
  selectWorkspaceKey,
  selectWorkspaceTabs,
  useViewerStore,
  workspacePanedTabIds,
  workspaceTabGridPreset,
} from "@/shared/stores/viewer-store";
import { harnessPaneKey } from "@/features/viewer/lib/harness-pane-key";
import { terminalCloseWorkspace } from "@/tauri/commands";

// ---------------------------------------------------------------------------
// Generic workspace-fixture core
//
// The seed/reset/snapshot/dispose mechanics are identical whichever team a
// harness suite drives — they differ only in the org/team identity and the set
// of workspaces. QH-6's six-pane terminal-webgl fixture reuses this core
// (`src/app/e2e-terminal-webgl-control.ts`) rather than duplicate the store
// wiring; the cli-workspace control below is the original 3-pane consumer.
// ---------------------------------------------------------------------------

/** One workspace to open in a fixture team. */
export interface WorkspaceFixtureSpec {
  id: string;
  storyId: string;
  parentEpic: string;
  cwd: string;
}

/** A fully-resolved fixture: which org/team to build and which workspaces to open. */
export interface WorkspaceFixturePlan {
  orgSlug: string;
  teamName: string;
  teamKey: string;
  sourceRepo: { owner: string; repo: string; provider: string };
  root: string;
  displayName: string;
  workspaces: readonly WorkspaceFixtureSpec[];
  /** Resting layout mode the fixture seeds into. Defaults to `"shared"` (the
   *  cli-workspace fixture's split view). The cli-lifecycle six-pane fixture
   *  overrides this to `"console"` (terminal-primary) so the panes get a real
   *  pixel budget instead of being squished under the file viewer. */
  layoutMode?: LayoutMode;
}

/** The minimal identity reset/snapshot/dispose need once a team is seeded. */
export interface WorkspaceFixtureIdentity {
  teamKey: string;
  teamName: string;
  workspaceIds: readonly string[];
  /** Resting layout mode a reset restores to (matches the seed). Defaults to
   *  `"shared"` so existing callers are unaffected. */
  layoutMode?: LayoutMode;
}

export interface WorkspaceFixtureSnapshot {
  teamKey: string;
  workspaceIds: string[];
  activeWorkspaceId?: string;
  sessionIds: Record<string, string | undefined>;
}

/** Materialize the renderer's canonical preset derivation so fixture startup
 * never depends on a later render-time reconciliation to create tile frames. */
export function createFixtureForegroundLayout(workspaceIds: readonly string[]): ForegroundLayout {
  return {
    sizes: [],
    direction: "row",
    preset: "columns",
    root: effectiveMosaicRoot(undefined, [...workspaceIds]),
  };
}

function installFixtureForegroundLayout(teamKey: string, workspaceIds: readonly string[]): void {
  const layout = createFixtureForegroundLayout(workspaceIds);
  useViewerStore.setState((state) => ({
    foregroundLayout: { ...state.foregroundLayout, [teamKey]: layout },
  }));
}

export function snapshotWorkspaceFixture(teamKey: string): WorkspaceFixtureSnapshot {
  const state = useViewerStore.getState();
  const workspaces = state.workspaces[teamKey] ?? [];
  return {
    teamKey,
    workspaceIds: workspaces.map((workspace) => workspace.id),
    activeWorkspaceId: state.activeWorkspace[teamKey],
    sessionIds: Object.fromEntries(
      workspaces.map((workspace) => [
        workspace.id,
        selectWorkspaceTabs(state, teamKey, workspace.id)[0]?.sessionId,
      ]),
    ),
  };
}

/** One terminal tab of a fixture workspace, with the key its harness seams
 *  (and its `data-qh1-workspace-id` container) register under. */
export interface WorkspaceLayoutTab {
  id: string;
  sessionId: string;
  paneKey: string;
}

/** A READ-ONLY view of the layout state a CLI layout transition changes, read
 *  through the layout selectors: the layout mode, the foreground mosaic (preset +
 *  zoom), and each workspace's tabs, active tab and tab-grid state. The harness
 *  drives every transition through UI gestures; this only reads back what they did. */
export interface WorkspaceLayoutReading {
  teamKey: string;
  layoutMode: LayoutMode;
  preset: string;
  /** The tile expanded over the others (the focused one, while expanded is on). */
  expandedTileId: string | null;
  /** The foreground mosaic tiles, in mosaic order. */
  workspaceIds: string[];
  activeWorkspaceId: string | null;
  expandedWorkspaceId: string | null;
  gridWorkspaceId: string | null;
  tabs: Record<string, WorkspaceLayoutTab[]>;
  activeTabId: Record<string, string | null>;
  panedTabIds: Record<string, string[]>;
  gridCollapsed: Record<string, boolean>;
  tabGridPreset: Record<string, string>;
  lastTerminalMode: LayoutMode;
  rightPaneOpen: boolean;
}

export function readWorkspaceLayout(teamKey: string): WorkspaceLayoutReading {
  const state = useViewerStore.getState();
  const layout = activeForegroundLayout(state, teamKey);
  const workspaceIds = activeForegroundTiles(state, teamKey).map((workspace) => workspace.id);
  const tabs: Record<string, WorkspaceLayoutTab[]> = {};
  const activeTabId: Record<string, string | null> = {};
  const panedTabIds: Record<string, string[]> = {};
  const gridCollapsed: Record<string, boolean> = {};
  const tabGridPreset: Record<string, string> = {};
  for (const workspaceId of workspaceIds) {
    const wsTabs = selectWorkspaceTabs(state, teamKey, workspaceId);
    tabs[workspaceId] = wsTabs.map((tab, index) => ({
      id: tab.id,
      sessionId: tab.sessionId,
      paneKey: harnessPaneKey(workspaceId, index, tab.id),
    }));
    activeTabId[workspaceId] =
      selectActiveTerminalTab(state, teamKey, workspaceId) ?? wsTabs[0]?.id ?? null;
    panedTabIds[workspaceId] = [...workspacePanedTabIds(state, teamKey, workspaceId)];
    gridCollapsed[workspaceId] = selectTabGridCollapsed(state, teamKey, workspaceId);
    tabGridPreset[workspaceId] = workspaceTabGridPreset(state, teamKey, workspaceId);
  }
  return {
    teamKey,
    layoutMode: state.layoutMode ?? "shared",
    preset: layout?.preset ?? "columns",
    expandedTileId: selectExpandedWorkspaceId(state, teamKey),
    workspaceIds,
    activeWorkspaceId: state.activeWorkspace[teamKey] ?? null,
    expandedWorkspaceId: expandedWorkspaceId(state, teamKey),
    gridWorkspaceId: expandedGridWorkspaceId(state, teamKey),
    tabs,
    activeTabId,
    panedTabIds,
    gridCollapsed,
    tabGridPreset,
    lastTerminalMode: state.lastTerminalMode,
    rightPaneOpen: state.rightPaneOpen,
  };
}

export function resetWorkspaceFixture(
  identity: WorkspaceFixtureIdentity,
): WorkspaceFixtureSnapshot {
  const { teamKey, teamName, workspaceIds } = identity;
  // Re-enter the fixture team through the production route.
  useViewerStore.getState().openTeam(teamKey);
  useOrgStore.getState().setActiveTeam(teamName);
  const state = useViewerStore.getState();
  for (const workspaceId of workspaceIds) {
    if (state.softClosedWorkspaces[teamKey]?.[workspaceId]) {
      useViewerStore.getState().reopenSoftClosedWorkspace(teamKey, workspaceId);
    }
  }
  useViewerStore.getState().setLayoutMode(identity.layoutMode ?? "shared");
  useViewerStore.getState().setCliExpanded(false);
  useViewerStore.getState().setTrayMode(teamKey, "flat");
  installFixtureForegroundLayout(teamKey, workspaceIds);
  useViewerStore.getState().switchWorkspace(teamKey, workspaceIds[0]);
  return snapshotWorkspaceFixture(teamKey);
}

export async function disposeWorkspaceFixture(teamKey: string): Promise<void> {
  // End every fixture terminal in its host; the host's layout follows.
  const state = useViewerStore.getState();
  await Promise.all(
    (state.workspaces[teamKey] ?? []).map((workspace) =>
      terminalCloseWorkspace(selectWorkspaceKey(state, teamKey, workspace.id)).catch(
        () => undefined,
      ),
    ),
  );
  useViewerStore.setState({
    openedTeams: [],
    workspaces: {},
    activeWorkspace: {},
    foregroundLayout: {},
    foregroundLayoutByRail: {},
    softClosedWorkspaces: {},
  });
}

/** Plant the fixture org (its one team and source repo) in the org store —
 *  the store a webview reload rebuilds without it. */
export function plantFixtureOrg(plan: WorkspaceFixturePlan): void {
  const repoPath = syntheticRepoKey(plan.orgSlug, plan.teamName, plan.sourceRepo);
  useOrgStore.setState({
    orgs: [
      {
        slug: plan.orgSlug,
        display_name: plan.displayName,
        created_at: new Date(0).toISOString(),
        remote_url: null,
        config_flags: {},
        people: [],
        teams: [{ name: plan.teamName, source_repos: [plan.sourceRepo] }],
        org_root: plan.root,
      },
    ],
    activeOrgSlug: plan.orgSlug,
    activeTeamName: null,
    stateLoaded: true,
    hasUserIntent: true,
    workspaceRepos: {
      [repoPath]: {
        repoPath,
        repoName: plan.sourceRepo.repo,
        contextRepoPath: plan.root,
        teamContentPath: plan.root,
        hasRemote: false,
        remoteUrl: null,
        mainDoc: "",
        localPath: plan.root,
      },
    },
  });
}

export async function seedWorkspaceFixture(
  plan: WorkspaceFixturePlan,
): Promise<WorkspaceFixtureSnapshot> {
  plantFixtureOrg(plan);
  useViewerStore.setState({
    stateLoaded: true,
    openedTeams: [],
    workspaces: { [plan.teamKey]: [] },
    activeWorkspace: {},
    foregroundLayout: {},
    foregroundLayoutByRail: {},
    softClosedWorkspaces: {},
  });
  // Enter through the same production route actions. `openTeam` is App's
  // documented mount source for TeamWorkspaceView; `setActiveTeam` establishes
  // the active route only after the fixture org exists.
  useViewerStore.getState().openTeam(plan.teamKey);
  useOrgStore.getState().setActiveTeam(plan.teamName);
  for (const workspace of plan.workspaces) {
    useViewerStore.getState().openWorkspace({
      teamKey: plan.teamKey,
      workspaceId: workspace.id,
      kind: "story",
      cwd: workspace.cwd,
      recordFields: {
        storyId: workspace.storyId,
        storyTitle: workspace.storyId,
        parentEpic: workspace.parentEpic,
        sourceRepoSlug: `${plan.sourceRepo.owner}/${plan.sourceRepo.repo}`,
      },
      focus: false,
    });
  }
  // The suite waits for mounted viewports before its first per-journey reset.
  // Make the initial fixture foreground state explicit instead of relying on
  // openWorkspace's focus policy to reveal the terminal pane.
  useViewerStore.getState().setLayoutMode(plan.layoutMode ?? "shared");
  useViewerStore.getState().setCliExpanded(false);
  useViewerStore.getState().setTrayMode(plan.teamKey, "flat");
  installFixtureForegroundLayout(
    plan.teamKey,
    plan.workspaces.map((workspace) => workspace.id),
  );
  useViewerStore.getState().switchWorkspace(plan.teamKey, plan.workspaces[0].id);
  useOrgStore.getState().persistState();
  useViewerStore.getState().persistState();
  return snapshotWorkspaceFixture(plan.teamKey);
}

/** OC-9c focus oracle: the workspace id of the terminal that currently owns
 *  keyboard focus, or null if focus is elsewhere (a doc, a control, `<body>`).
 *  Reads the live `document.activeElement` — the only thing the user can feel —
 *  so a focus assertion proves observable behaviour, not a dispatched intent.
 *  Scopes on the same `[data-pane-id]` frame `focusWorkspaceTerminal`
 *  targets, so the oracle and the production focus path agree by construction. */
export function activeTerminalFocusWorkspaceId(): string | null {
  if (typeof document === "undefined") return null;
  const frame = document.activeElement?.closest<HTMLElement>("[data-pane-id]");
  return frame?.getAttribute("data-pane-id") ?? null;
}

/**
 * SW-CL-12: switch the active terminal to workspace `workspaceId` through the REAL
 * store path — the tab-switch a background pane reveals against a mutated shared
 * atlas (Repro 1's tab-switch trigger). Resolves the target's active terminal tab
 * and drives `viewer-store.ts:setActiveTerminalTab` (the tab-activation write +
 * persist), makes it the active workspace (`switchWorkspace`, the reveal), then
 * lands keyboard focus in its pane (`focusWorkspaceTerminal`, the I5 obligation).
 * Returns the workspace id whose pane now owns keyboard focus, or null when the
 * target has no live terminal tab to activate (COULD_NOT_MEASURE upstream).
 */
export function switchWorkspaceTab(teamKey: string, workspaceId: string): string | null {
  const state = useViewerStore.getState();
  const tabs = selectWorkspaceTabs(state, teamKey, workspaceId);
  if (tabs.length === 0) return null;
  const activeTabId = selectActiveTerminalTab(state, teamKey, workspaceId) ?? tabs[0].id;
  useViewerStore.getState().setActiveTerminalTab(teamKey, activeTabId, workspaceId);
  useViewerStore.getState().switchWorkspace(teamKey, workspaceId);
  const focused = focusWorkspaceTerminal(workspaceId);
  return focused ? workspaceId : null;
}

/**
 * SW-CL-12 (SW-FX-98 variant): focus-click a VISIBLE sibling pane without
 * switching the active workspace — the split-sibling focus move that reveals the
 * clicked pane against the atlas its sibling mutated. Drives the same
 * `focusWorkspaceTerminal` the production click path calls. Returns the focused
 * workspace id, or null when that pane has no focusable terminal.
 */
export function focusWorkspacePane(workspaceId: string): string | null {
  return focusWorkspaceTerminal(workspaceId) ? workspaceId : null;
}

/** Hide then re-show the native window after `durationMs` — the production
 *  background↔foreground affordance both harness fixtures drive. */
export async function hideAndResumeWindow(durationMs: number): Promise<void> {
  const current = getCurrentWindow();
  await current.hide();
  window.setTimeout(() => void current.show(), durationMs);
}

/** App's `loadPersistedState()` sets `scopeRecoveryComplete` in its resolve
 *  tail; a stuck-state watchdog force-flips `stateLoaded` (and the hydrated
 *  surface) before then, so seeding on the surface alone races a late hydration
 *  that overwrites the workspaces. Gate the seed on this instead. */
export function fixtureHydrationSettled(): boolean {
  return useViewerStore.getState().scopeRecoveryComplete === true;
}

// ---------------------------------------------------------------------------
// cli-workspace fixture (QH-1 / QH-5 — the original 3-pane consumer)
// ---------------------------------------------------------------------------

export interface CliWorkspaceFixtureInput {
  root: string;
  cwdA: string;
  cwdB: string;
  cwdC: string;
}

/** Canonical fixture identities — the single source of truth every control
 *  helper (seed/reset/dispose/snapshot) derives from, so an id can't drift in
 *  one place only. */
export const CLI_FIXTURE_ORG_SLUG = "qh1";
export const CLI_FIXTURE_TEAM_NAME = "cli";
export const CLI_FIXTURE_TEAM_KEY = `${CLI_FIXTURE_ORG_SLUG}:${CLI_FIXTURE_TEAM_NAME}`;
export const CLI_FIXTURE_WORKSPACE_IDS = ["qh1-story-a", "qh1-story-b", "qh1-story-c"] as const;

export function createCliWorkspaceFixturePlan(
  input: CliWorkspaceFixtureInput,
): WorkspaceFixturePlan {
  const [a, b, c] = CLI_FIXTURE_WORKSPACE_IDS;
  return {
    orgSlug: CLI_FIXTURE_ORG_SLUG,
    teamName: CLI_FIXTURE_TEAM_NAME,
    teamKey: CLI_FIXTURE_TEAM_KEY,
    sourceRepo: { owner: "fixture", repo: "cli", provider: "github" },
    root: input.root,
    displayName: "QH1 CLI",
    workspaces: [
      { id: a, storyId: "QH1-A", parentEpic: "qh1-epic-a", cwd: input.cwdA },
      { id: b, storyId: "QH1-B", parentEpic: "qh1-epic-a", cwd: input.cwdB },
      { id: c, storyId: "QH1-C", parentEpic: "qh1-epic-b", cwd: input.cwdC },
    ],
  };
}

/** Backward-compatible alias — the original name some call sites use. */
export type CliWorkspaceFixtureSnapshot = WorkspaceFixtureSnapshot;

const CLI_FIXTURE_IDENTITY: WorkspaceFixtureIdentity = {
  teamKey: CLI_FIXTURE_TEAM_KEY,
  teamName: CLI_FIXTURE_TEAM_NAME,
  workspaceIds: CLI_FIXTURE_WORKSPACE_IDS,
};

function cliSeed(input: CliWorkspaceFixtureInput): Promise<WorkspaceFixtureSnapshot> {
  return seedWorkspaceFixture(createCliWorkspaceFixturePlan(input));
}

export function registerCliWorkspaceE2eControl(): void {
  window.__CODESENSE_CLI_E2E__ = {
    hydrationSettled: fixtureHydrationSettled,
    seed: cliSeed,
    reset: () => resetWorkspaceFixture(CLI_FIXTURE_IDENTITY),
    dispose: () => disposeWorkspaceFixture(CLI_FIXTURE_TEAM_KEY),
    snapshot: () => snapshotWorkspaceFixture(CLI_FIXTURE_TEAM_KEY),
    activeTerminalFocus: activeTerminalFocusWorkspaceId,
    hideAndResume: hideAndResumeWindow,
  };
}

/** The WebDriver-only fixture control surface. Exported as the single shared
 *  shape so the e2e spec references it via `import type` rather than
 *  maintaining a second, drift-prone declaration. */
export interface CliWorkspaceE2eControl {
  hydrationSettled: () => boolean;
  seed: (input: CliWorkspaceFixtureInput) => Promise<WorkspaceFixtureSnapshot>;
  reset: () => WorkspaceFixtureSnapshot;
  dispose: () => Promise<void>;
  snapshot: () => WorkspaceFixtureSnapshot;
  /** OC-9c: the workspace id whose terminal currently holds keyboard focus (null
   *  if focus is elsewhere) — the `activeElement` oracle for the reveal-focus race. */
  activeTerminalFocus: () => string | null;
  hideAndResume: (durationMs: number) => Promise<void>;
}

declare global {
  interface Window {
    __CODESENSE_CLI_E2E__?: CliWorkspaceE2eControl;
  }
}
