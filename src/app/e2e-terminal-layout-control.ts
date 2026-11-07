// SPDX-License-Identifier: MIT
import {
  createTerminalTab,
  selectActiveTerminalTab,
  selectHiddenTerminalTabs,
  selectWorkspaceTabs,
  useViewerStore,
  type LayoutPreset,
} from "@/shared/stores/viewer-store";
import { closeTab, hideWorkspace } from "@/features/viewer/lib/terminal-close";
import { activateAndFocus } from "@/features/viewer/lib/focus-cascade";
import { makeRealLaunchDeps } from "@/shared/lib/agent-session-launcher";
import { whenTerminalLayoutHydrated } from "@/shared/lib/terminal-layout";
import { writePty } from "@/tauri/commands";
import { useOrgStore } from "@/shared/stores/org-store";
import {
  fixtureHydrationSettled,
  plantFixtureOrg,
  seedWorkspaceFixture,
  type WorkspaceFixturePlan,
} from "./e2e-cli-workspace-control";

/**
 * The WebDriver-only control surface of the terminal-layout suite: a fixture
 * team whose terminal tabs are its terminal host's (the real in-process host
 * or session server), driven through the same store actions a user's clicks
 * run. Every read is the viewer's projection of the host's layout.
 */

export const TERMINAL_LAYOUT_FIXTURE_TEAM_KEY = "rt7b:layout";
export const TERMINAL_LAYOUT_FIXTURE_WORKSPACE_IDS = ["rt7b-shells", "rt7b-empty"] as const;

export interface TerminalLayoutFixtureInput {
  root: string;
  cwdShells: string;
  cwdEmpty: string;
}

export interface ShownTab {
  sessionId: string;
  name: string;
  role?: string;
  exitCode?: number | null;
}

function plan(input: TerminalLayoutFixtureInput): WorkspaceFixturePlan {
  const [shells, empty] = TERMINAL_LAYOUT_FIXTURE_WORKSPACE_IDS;
  return {
    orgSlug: "rt7b",
    teamName: "layout",
    teamKey: TERMINAL_LAYOUT_FIXTURE_TEAM_KEY,
    sourceRepo: { owner: "fixture", repo: "layout", provider: "github" },
    root: input.root,
    displayName: "RT-7b Layout",
    workspaces: [
      { id: shells, storyId: "RT7B-SHELLS", parentEpic: "rt7b-epic", cwd: input.cwdShells },
      { id: empty, storyId: "RT7B-EMPTY", parentEpic: "rt7b-epic", cwd: input.cwdEmpty },
    ],
  };
}

async function seed(input: TerminalLayoutFixtureInput): Promise<void> {
  await seedWorkspaceFixture(plan(input));
}

/** After a webview reload: the fixture org back in the org store and its team
 *  entered through the production route. The viewer's workspaces came back
 *  from its persisted state, and their terminals from the hosts' snapshot. */
function reenter(input: TerminalLayoutFixtureInput): void {
  plantFixtureOrg(plan(input));
  useViewerStore.getState().openTeam(TERMINAL_LAYOUT_FIXTURE_TEAM_KEY);
  useOrgStore.getState().setActiveTeam("layout");
}

function tabs(workspaceId: string): ShownTab[] {
  return selectWorkspaceTabs(
    useViewerStore.getState(),
    TERMINAL_LAYOUT_FIXTURE_TEAM_KEY,
    workspaceId,
  ).map((tab) => ({
    sessionId: tab.sessionId,
    name: tab.name,
    role: tab.role,
    exitCode: tab.exit ? tab.exit.code : undefined,
  }));
}

/** Open a terminal tab (the tab strip's +), optionally a role's; resolves with
 *  its session once its host placed it. */
async function openTab(workspaceId: string, name?: string, role?: string): Promise<string> {
  const tab = { ...createTerminalTab(name), ...(role ? { role } : {}) };
  await useViewerStore
    .getState()
    .addTerminalTab(TERMINAL_LAYOUT_FIXTURE_TEAM_KEY, tab, workspaceId);
  return tab.sessionId;
}

/** End every terminal of the workspace, as the user's × with kill — never
 *  replaced by a fresh one. */
function endAll(workspaceId: string): void {
  for (const tab of tabs(workspaceId)) {
    useViewerStore
      .getState()
      .removeTerminalTab(TERMINAL_LAYOUT_FIXTURE_TEAM_KEY, tab.sessionId, workspaceId, {
        kill: true,
        replace: false,
      });
  }
}

function focusTab(workspaceId: string, sessionId: string): string | undefined {
  useViewerStore
    .getState()
    .setActiveTerminalTab(TERMINAL_LAYOUT_FIXTURE_TEAM_KEY, sessionId, workspaceId);
  return selectActiveTerminalTab(
    useViewerStore.getState(),
    TERMINAL_LAYOUT_FIXTURE_TEAM_KEY,
    workspaceId,
  );
}

/** A role agent's reconnect: the real launcher's lookup, which adopts the
 *  running session from the host layout (never opens a second). */
async function findRoleSession(role: string): Promise<string | undefined> {
  const found = await makeRealLaunchDeps(async () => {}).findExisting(
    TERMINAL_LAYOUT_FIXTURE_TEAM_KEY,
    role,
  );
  return found?.sessionId;
}

/** The tabs Hidden from a tile (listed under its tray chip). */
function hiddenTabs(workspaceId: string): string[] {
  return selectHiddenTerminalTabs(
    useViewerStore.getState(),
    TERMINAL_LAYOUT_FIXTURE_TEAM_KEY,
    workspaceId,
  ).map((tab) => tab.sessionId);
}

/** Arrange the fixture team's tiles by a mosaic preset (the layout menu). */
function setMosaicPreset(preset: LayoutPreset): void {
  useViewerStore.getState().setForegroundPreset(TERMINAL_LAYOUT_FIXTURE_TEAM_KEY, preset);
}

/** A click in a tile's terminal: the tile becomes the focused one, the clicked
 *  tab its active tab, and its terminal takes the keyboard. */
function clickTile(workspaceId: string, sessionId: string): void {
  const store = useViewerStore.getState();
  store.switchWorkspace(TERMINAL_LAYOUT_FIXTURE_TEAM_KEY, workspaceId);
  store.setActiveTerminalTab(TERMINAL_LAYOUT_FIXTURE_TEAM_KEY, sessionId, workspaceId);
  activateAndFocus({ teamKey: TERMINAL_LAYOUT_FIXTURE_TEAM_KEY, workspaceId });
}

/** Whether the tile is hidden in the tray. */
function inTray(workspaceId: string): boolean {
  return !!useViewerStore.getState().softClosedWorkspaces[TERMINAL_LAYOUT_FIXTURE_TEAM_KEY]?.[
    workspaceId
  ];
}

export interface TerminalLayoutE2eControl {
  hydrationSettled: () => boolean;
  layoutHydrated: () => Promise<boolean>;
  seed: (input: TerminalLayoutFixtureInput) => Promise<void>;
  reenter: (input: TerminalLayoutFixtureInput) => void;
  tabs: (workspaceId: string) => ShownTab[];
  openTab: (workspaceId: string, name?: string, role?: string) => Promise<string>;
  type: (sessionId: string, text: string) => Promise<void>;
  endAll: (workspaceId: string) => void;
  focusTab: (workspaceId: string, sessionId: string) => string | undefined;
  findRoleSession: (role: string) => Promise<string | undefined>;
  /** The tab `×`: the default close (Hide when running, End otherwise). */
  closeTab: (workspaceId: string, sessionId: string) => Promise<void>;
  /** The tile `–`: Hide it into the tray. */
  hideTile: (workspaceId: string) => void;
  hiddenTabs: (workspaceId: string) => string[];
  inTray: (workspaceId: string) => boolean;
  setMosaicPreset: (preset: LayoutPreset) => void;
  clickTile: (workspaceId: string, sessionId: string) => void;
}

export function registerTerminalLayoutE2eControl(): void {
  window.__CODESENSE_TERMINAL_LAYOUT_E2E__ = {
    hydrationSettled: fixtureHydrationSettled,
    layoutHydrated: () => whenTerminalLayoutHydrated().then(() => true),
    seed,
    reenter,
    tabs,
    openTab,
    type: (sessionId, text) => writePty(sessionId, text),
    endAll,
    focusTab,
    findRoleSession,
    closeTab: async (workspaceId, sessionId) => {
      await closeTab(TERMINAL_LAYOUT_FIXTURE_TEAM_KEY, workspaceId, sessionId);
    },
    hideTile: (workspaceId) => hideWorkspace(TERMINAL_LAYOUT_FIXTURE_TEAM_KEY, workspaceId),
    hiddenTabs,
    inTray,
    setMosaicPreset,
    clickTile,
  };
}

declare global {
  interface Window {
    __CODESENSE_TERMINAL_LAYOUT_E2E__?: TerminalLayoutE2eControl;
  }
}
