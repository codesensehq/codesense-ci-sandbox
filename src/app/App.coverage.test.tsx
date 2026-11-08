// SPDX-License-Identifier: MIT
vi.mock("@/features/dashboard/lib/prefs-live", () => ({ usePrefsLive: vi.fn() }));
vi.mock("@/features/viewer/hooks/use-agent-roster-listener", () => ({
  useAgentRosterListener: () => {},
}));
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { useFirstRunStore } from "@/shared/stores/first-run-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { useUiStore } from "@/shared/stores/ui-store";
import { EMPTY_TERMINAL_LAYOUT } from "@/shared/lib/terminal-layout-projection";
import { render, screen, waitFor, act, fireEvent } from "@testing-library/react";

// ---------------------------------------------------------------------------
// Mocks — all external dependencies must be stubbed
// vi.hoisted() ensures these are available when vi.mock factories run
// ---------------------------------------------------------------------------

const {
  mockListen,
  mockShow,
  mockStartWatching,
  mockStopWatching,
  mockDetectTools,
  mockRebuildMenu,
  mockGetDependencyStatus,
  mockBumpFileRefreshForSourceRepo,
  mockToastWarning,
  mockLoggerWarn,
} = vi.hoisted(() => ({
  mockListen: vi.fn<(event: string, cb: (...args: unknown[]) => void) => Promise<() => void>>(() =>
    Promise.resolve(() => {}),
  ),
  mockShow: vi.fn(() => Promise.resolve()),
  mockStartWatching: vi.fn(() => Promise.resolve()),
  mockStopWatching: vi.fn(() => Promise.resolve()),
  mockDetectTools: vi.fn(() =>
    Promise.resolve({
      terminals: [],
      editors: [],
      default_terminal: "iTerm",
      default_editor: "code",
    }),
  ),
  mockRebuildMenu: vi.fn(() => Promise.resolve()),
  mockGetDependencyStatus: vi.fn(() =>
    Promise.resolve({
      git: {
        available: true,
        path: "/usr/bin/git" as string | null,
        version: "2.44.0" as string | null,
      },
      agents: {
        claude: { available: true, path: "/usr/bin/claude" as string | null, resolving: false },
      },
    }),
  ),
  // SF-6: assert install-file-change burst → 1 bump per repo per window.
  mockBumpFileRefreshForSourceRepo: vi.fn(),
  // SF-6 review iter-1: assert AC-2 conflict toast actually fires
  // (otherwise the conflict test would silently green if the toast call
  // is ever removed) and AC-4 broad-bump warn actually surfaces
  // (otherwise a silenced warn breaks the "regressions surface" contract
  // from refresh-debounce.ts:43-46).
  mockToastWarning: vi.fn(),
  mockLoggerWarn: vi.fn(),
}));

// SW-UX (app-flow redesign): Settings is a virtual TAB now — opening it goes
// through these light openers (mocked, since the viewer store here is mocked so a
// real openFile wouldn't flip the derived `settingsPanelOpen`). Tests assert the
// opener was invoked instead of the old overlay flag.
const { mockOpenSettingsTab } = vi.hoisted(() => ({
  mockOpenSettingsTab: vi.fn(),
}));
// ⌘W on a focused terminal routes through the close-verb decision point.
const { mockIsTerminalFocused, mockCloseActiveTerminalTab } = vi.hoisted(() => ({
  mockIsTerminalFocused: vi.fn(() => false),
  mockCloseActiveTerminalTab: vi.fn(() => Promise.resolve()),
}));
// SW-AGR-6: the session-timers loader reads the live tab index this file's
// partial viewer-store mock leaves out; it has its own tests.
vi.mock("@/features/viewer/components/session-timers-loader", () => ({
  SessionTimersLoader: () => null,
}));
vi.mock("@/shared/lib/focus-utils", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/shared/lib/focus-utils")>()),
  isTerminalFocused: mockIsTerminalFocused,
}));
vi.mock("@/features/viewer/lib/terminal-close", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/features/viewer/lib/terminal-close")>()),
  closeActiveTerminalTab: mockCloseActiveTerminalTab,
}));
vi.mock("@/features/settings/lib/open-settings-tab", () => ({
  SETTINGS_VIRTUAL_PATH: "virtual://settings",
  openSettingsTab: mockOpenSettingsTab,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: (...a: Parameters<typeof mockListen>) => mockListen(...a),
}));
// The rendered-document bridge listens on the main webview window.
vi.mock("@tauri-apps/api/webviewWindow", () => ({
  getCurrentWebviewWindow: () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }),
}));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    startDragging: vi.fn(),
    show: mockShow,
    close: vi.fn(() => Promise.resolve()),
    onCloseRequested: vi.fn(() => Promise.resolve(() => {})),
    // SF-8 (AC-1): `useWindowTitle` calls `setTitle` on chrome change.
    setTitle: vi.fn(() => Promise.resolve()),
    // SW-AU-UI-1: the relocated `useIncomingCount` probe registers a window
    // focus listener to re-poll on refocus; return a no-op unlisten.
    onFocusChanged: vi.fn(() => Promise.resolve(() => {})),
    // SW-WA-E11: useNeedsYouNotifications reads the initial focus state.
    isFocused: vi.fn(() => Promise.resolve(true)),
  }),
}));
vi.mock("@/tauri/commands", () => ({
  // Each team draws its agents with its own faces.
  avatarPacksListForTeam: vi.fn().mockResolvedValue({ packs: [], warnings: [] }),
  // Custom right panes: the app-wide pane registration lists them.
  listPanes: vi.fn().mockResolvedValue({ contentRoot: "", chain: [], panes: [] }),
  // The dock lists the built-in agent panes for the active team.
  listBuiltinPanes: vi.fn().mockResolvedValue({ contentRoot: "", chain: [], panes: [] }),
  // Agent tabs follow their agents' names: the app reads the agents on mount.
  agentsList: vi.fn().mockResolvedValue([]),
  listMachines: vi.fn().mockResolvedValue([]),
  // SW-OC-16: the context repo switcher and the Context repos tab list connections.
  listContextAttachments: vi.fn().mockResolvedValue([]),
  ensureCodesenseDocs: vi.fn(() => Promise.reject(new Error("no bundled docs in this test"))),
  // SW-ODC-11: no sample repo bundled in this test environment.
  ensureSampleRepo: vi.fn(() => Promise.reject(new Error("no bundled sample repo in this test"))),
  LOCAL_MACHINE_ID: "local",
  // SW-LN-B20: the backend guest-session declaration.
  setGuestSession: vi.fn(() => Promise.resolve()),
  getGuestSession: vi.fn(() => Promise.resolve(false)),
  // The shell's terminal-server prompt: a compatible server.
  terminalServerStatus: vi.fn(() => Promise.resolve(null)),
  restartTerminalServer: vi.fn(() => Promise.resolve(null)),
  startWatching: mockStartWatching,
  stopWatching: mockStopWatching,
  detectTools: mockDetectTools,
  reconfigureWarmPool: vi.fn(() => Promise.resolve()),
  setTerminalShellOverride: vi.fn(() => Promise.resolve()),
  dashboardSetBackgroundLimit: vi.fn(() => Promise.resolve()),
  rebuildMenu: mockRebuildMenu,
  // SW-AU-34: App mounts useQuitOnCloseSetting, which pushes the preference to Rust.
  setQuitOnClose: vi.fn(() => Promise.resolve()),
  // SV-8: App mounts useTerminalServerMenuBarSetting, which pushes the setting to Rust.
  setTerminalServerMenuBar: vi.fn(() => Promise.resolve()),
  // SW-AM-9: App mounts useAgentRosterListener, which warms the roster store on mount.
  dashboardListSessions: vi.fn(() => Promise.resolve([])),
  // SW-WV-B3: the roster store loads the durable roster via wvListRoster on mount
  // (origin/main baseline). Stub it so the listener resolves to an empty roster.
  wvListRoster: vi.fn(() => Promise.resolve([])),
  // SW-WA-E11: App mounts useNeedsYouNotifications, which reads the attention board.
  listAttentionBoard: vi.fn(() =>
    Promise.resolve({ youOwe: [], owedToYou: [], onAgent: [], parked: [], doneUnacked: [] }),
  ),
  getLeaseStatus: vi.fn(() =>
    Promise.resolve({ held: false, holderSessionId: null, heartbeatAt: null }),
  ),
  // SW-WU-9: App mounts useWorktreeRosterListener, which warms the worktree store on mount.
  listManagedWorktrees: vi.fn(() => Promise.resolve({ rows: [], reaped: 0 })),
  getDependencyStatus: mockGetDependencyStatus,
  // SW-AU-UI-1: the title bar now mounts WorkspaceModeControls, whose
  // publish/pull probes (change-count + incoming + auto-pull) fire for an
  // active team. Stub their transports so the App routing tests don't hit real
  // invoke()s.
  getPublishSet: vi.fn(() => Promise.resolve({ buckets: [], general: { count: 0, files: [] } })),
  incomingStatus: vi.fn(() => Promise.resolve({ incoming: 0, canFastForward: false })),
  addWatch: vi.fn(() => Promise.resolve()),
  removeWatch: vi.fn(() => Promise.resolve()),
  toWatchConfig: (repo: { repoPath: string; contextRepoPath: string }) => ({
    repo_path: repo.repoPath,
    context_repo_path: repo.contextRepoPath,
  }),
  discoverLocalOrgsCmd: vi.fn(() => Promise.resolve([])),
  createWorkspaceCmd: vi.fn(() =>
    Promise.resolve({
      org: { slug: "test", root: "/mock/test", org: null, partial: {}, file_errors: [] },
      remote_warning: null,
    }),
  ),
  connectToOrgCmd: vi.fn(() =>
    Promise.resolve({
      org_slug: "test",
      org_root: "/mock/test",
      clone_outcome: "cloned",
      loaded_org: { slug: "test", root: "/mock/test", org: null, partial: {}, file_errors: [] },
    }),
  ),
}));
vi.mock("@tauri-apps/plugin-process", () => ({
  relaunch: vi.fn(() => Promise.resolve()),
}));

// Titlebar helper
vi.mock("@/shared/lib/titlebar", () => ({
  startDragging: vi.fn(),
}));

// SF-6: mock the bump callback so the install-file-change debounce-flush
// test can assert call shape (count, key, Set contents) without depending
// on the real file-refresh store / synthetic fan-out.
vi.mock("@/shared/lib/refresh-for-source-repo", () => ({
  bumpFileRefreshForSourceRepo: mockBumpFileRefreshForSourceRepo,
}));

// SF-6 review iter-1: mock the logger so the AC-4 broad-bump test can
// assert the intentional warn at refresh-debounce.ts:43-46 fired. Other
// logger.warn call sites in App.tsx (shortcuts load failure, menu:action
// listen failure) are no-ops in tests, so this mock is safe globally.
vi.mock("@/shared/lib/logger", () => ({
  initLogging: vi.fn(() => Promise.resolve()),
  logger: {
    error: vi.fn(),
    warn: mockLoggerWarn,
    info: vi.fn(),
    debug: vi.fn(),
  },
}));

// Sonner toast
vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), {
    loading: vi.fn(),
    success: vi.fn(),
    error: vi.fn(),
    // use-workspace-viewer-sync.ts:710 calls toast.dismiss(id) on PTS-2
    // migration failure paths exercised by these tests via the cmd+shift+T
    // setup. Mock it to no-op so the unhandled rejection from the missing
    // export doesn't pollute unrelated test runs.
    dismiss: vi.fn(),
    // SF-6 review iter-1: hoisted so the conflict-branch test can assert
    // toast.warning actually fires (was previously asserting only the
    // negative — that no bump fires).
    warning: mockToastWarning,
  }),
  Toaster: () => <div data-testid="toaster" />,
}));

// `runUpdateCheck` is preserved because App.tsx dynamic-imports it.
vi.mock("@/app/update-toast", () => ({
  navigateToSettings: vi.fn(),
  runUpdateCheck: vi.fn(),
}));

// Child views — stub them to simple divs so we can detect which renders
vi.mock("@/features/dashboard/components/team-workspace-empty-state", () => ({
  TeamWorkspaceEmptyState: () => <div data-testid="team-workspace-empty-state" />,
}));
vi.mock("@/features/viewer/views/team-workspace-view", () => ({
  TeamWorkspaceView: ({ teamKey }: { teamKey: string }) => (
    <div data-testid={`team-workspace-${teamKey}`} />
  ),
}));
// The workbench's CLI surfaces, stubbed like the team view: this file tests App's routing.
vi.mock("@/features/viewer/components/global-cli", () => ({
  GlobalCli: ({ teamKey }: { teamKey: string | null }) => (
    <div data-testid="global-cli" data-team={teamKey ?? ""} />
  ),
}));
vi.mock("@/features/viewer/components/shared-workspace-tray", () => ({
  SharedWorkspaceTray: () => <div data-testid="workspace-tray" />,
}));
vi.mock("@/features/viewer/components/agents-list", () => ({ AgentsList: () => null }));
vi.mock("@/features/viewer/components/workspace-drag-ghost", () => ({
  WorkspaceDragGhost: () => null,
}));
vi.mock("@/features/home-base/command-palette/command-palette-mount", () => ({
  CommandPaletteMount: () => <div data-testid="command-palette-portal" />,
}));
vi.mock("@/features/org/components/org-picker", () => ({
  OrgPicker: () => <div data-testid="org-picker" />,
}));
let discoveryCompleteValue = true;
vi.mock("@/features/org/hooks/use-org-discovery", () => ({
  useOrgDiscovery: () => ({ discoveryComplete: discoveryCompleteValue }),
}));
vi.mock("@/features/deps/views/dependency-install-view", () => ({
  DependencyInstallView: ({ onComplete }: { initialStatus: unknown; onComplete: () => void }) => (
    <div data-testid="dependency-install-view">
      <button onClick={onComplete}>deps-complete</button>
    </div>
  ),
}));
// ---------------------------------------------------------------------------
// Store mocks — zustand selector pattern
// ---------------------------------------------------------------------------

// We need fine-grained control, so we mock each store module and let tests
// configure the return values per-selector.

const mockLoadState = vi.fn(() => Promise.resolve());
const mockInitializeProfile = vi.fn(() => Promise.resolve());
const mockLoadPersistedState = vi.fn(() => Promise.resolve());

// Default store values — tests override via helpers below
let configStoreValues: Record<string, unknown> = {};
let viewerStoreValues: Record<string, unknown> = {};

// F-25: getCurrentUser/useCurrentUser are now the source of truth for
// user identity (no longer projected onto org-store). Tests that
// previously seeded `useOrgStore.setState({ currentUser: ... })` now
// set `currentUserValue` directly, which the mock returns.
let currentUserValue: import("@/shared/types/domain").CurrentUser | null = null;
vi.mock("@/shared/lib/get-current-user", () => ({
  getCurrentUser: () => currentUserValue,
  useCurrentUser: () => currentUserValue,
  buildCurrentUser: () => currentUserValue,
}));

// Static getState / setState mocks that tests can inspect
const viewerGetState = vi.fn();

vi.mock("@/shared/stores/config-store", () => {
  const store = (selector: (s: Record<string, unknown>) => unknown) => selector(configStoreValues);
  store.getState = () => configStoreValues;
  store.setState = vi.fn();
  return {
    useConfigStore: store,
    resetWorkspaceConfigView: vi.fn(),
    bindWorkspaceConfigPrincipal: vi.fn(),
    applyWorkspaceConfig: vi.fn(),
    workspaceRepoOverrides: () => ({}),
  };
});

// The OS needs-you alerts read the real layout selectors, which this file's
// store stub does not carry; their own seam suite covers them.
vi.mock("@/features/viewer/hooks/use-attention-alerts", () => ({ useAttentionAlerts: () => {} }));
// Agent activity recovery has its own seam tests; this minimal viewer store has no layout.
vi.mock("@/shared/hooks/use-terminal-activity", () => ({ useTerminalActivity: () => {} }));
vi.mock("@/features/viewer/hooks/use-attended-agent-release", () => ({
  useAttendedAgentRelease: () => {},
}));
// Likewise the terminal-display GC and warm-up, which read the tab indexes.
vi.mock("@/features/viewer/hooks/use-terminal-display-gc", () => ({
  useTerminalDisplayGc: () => {},
}));
vi.mock("@/features/viewer/hooks/use-terminal-display-warm", () => ({
  useTerminalDisplayWarm: () => {},
}));
// The team landing page has its own suites; here only that App mounts it.
const mockUseTeamLandingPage = vi.hoisted(() => vi.fn());
vi.mock("@/features/viewer/hooks/use-stale-arrow-filter", () => ({
  useStaleArrowFilterCleanup: () => {},
}));
vi.mock("@/features/viewer/hooks/use-team-landing-page", () => ({
  useTeamLandingPage: mockUseTeamLandingPage,
}));

vi.mock("@/shared/stores/viewer-store", () => {
  const store = (selector: (s: Record<string, unknown>) => unknown) => selector(viewerStoreValues);
  store.getState = (...args: unknown[]) => viewerGetState(...args);
  store.setState = vi.fn();
  store.subscribe = () => () => {};
  // T.6: selectRepoActive retired. Consumers gate on
  // useActiveWorkspaceContext().workspace !== null instead.
  // SW-EW-32: general-section's SummaryHeadingsRow imports this named const.
  // SLW-WD-7: general-section's DefaultDiffLayoutRow imports this resolver.
  return {
    useViewerStore: store,
    DEFAULT_SUMMARY_HEADINGS: ["Purpose", "Story"],
    defaultSourceDiffInline: (settings: { defaultSourceDiffLayout?: string } | undefined) =>
      settings?.defaultSourceDiffLayout !== "split",
    // The machines store versions each team's machine list by its lifecycle.
    teamLifecycleVersion: () => 0,
  };
});

// SS-9: useSyncStore retired entirely. The file-refresh half migrated to
// useFileRefreshStore (Commit 1); the install-health half deleted with
// sync-store (Commit 6). No mock needed — App.tsx no longer imports it.

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function setDefaultStoreValues(overrides?: {
  config?: Partial<typeof configStoreValues>;
  viewer?: Partial<typeof viewerStoreValues>;
  profile?: { initialized?: boolean; selected?: boolean };
}) {
  configStoreValues = {
    loadState: mockLoadState,
    initialized: false,
    repos: [],
    ...overrides?.config,
  };

  const viewerBase = {
    // T.6: selectedRepoPath / viewMode / openedRepos retired. The new
    // source of truth for "which teams are mounted" is `openedTeams`;
    // the active workspace within a team comes from
    // `workspaces[teamKey]` + `activeWorkspace[teamKey]`.
    openedTeams: [],
    stateLoaded: false,
    settings: { terminal: "", editor: "" },
    openFile: vi.fn(),
    closeTab: vi.fn(),
    setTerminalVisible: vi.fn(),
    removeTerminalTab: vi.fn(),
    loadPersistedState: mockLoadPersistedState,
    updateSettings: vi.fn(),
    openTabs: {},
    activeTab: {},
    terminalTabs: {},
    terminalLayout: EMPTY_TERMINAL_LAYOUT,
    activeTerminalTab: {},
    terminalVisible: false,
    activeTerminalTabByWorkspace: {},
    terminalTabsByWorkspace: {},
    softClosedWorkspaces: {},
    aiStatusBySession: {},
    progressBySession: {},
    setSessionProgress: vi.fn(),
    pipelineScope: {},
    scopeRecoveryComplete: false,
    setPipelineScope: vi.fn(),
    setScopeRecoveryComplete: vi.fn(),
    workspaces: {},
    activeWorkspace: {},
    switchWorkspace: vi.fn(),
    // T.2 (review iter-1, patch 3): the hard-switch path now calls
    // `ensureRootWorkspaces` directly after `setWorkspaceRepo`. The mock
    // must expose it or the IIFE crashes as an unhandled rejection.
    ensureRootWorkspaces: vi.fn(),
    openTeam: vi.fn(),
    // The workbench settles the CLI's first layout once a team shows.
    settleInitialLayout: vi.fn(),
    // SF-5: the T.1 D-4 one-shot sentinel sweep (`sweepSentinelBucket`)
    // calls `closeTeam(NO_TEAM_SENTINEL)` from the workspace-sync IIFE.
    // Without it the IIFE throws an unhandled rejection that poisons
    // vitest's exit code (exit 1 with 0 test failures) — same failure
    // mode as `ensureRootWorkspaces` above.
    closeTeam: vi.fn(),
    // The mock exposes `subscribe`, so App's right-pane native-menu sync
    // (useRightPaneMenuSync) runs for real and reads the layout.
    rightPaneLayout: { sections: [] },
    ...overrides?.viewer,
  };

  viewerStoreValues = viewerBase;
  viewerGetState.mockReturnValue(viewerBase);

  // Who-you-are is the active local profile.
  const profile = { principal: "100", displayName: "U", machineOwner: null };
  const selected = overrides?.profile?.selected === true;
  useLocalProfileStore.setState({
    initialized: overrides?.profile?.initialized ?? false,
    busy: false,
    error: null,
    initialize: mockInitializeProfile,
    status: {
      active: selected ? profile : null,
      profiles: selected ? [profile] : [],
      privateAuthority: null,
    },
  });
}

/** Render App with all stores in "fully loaded" state */
function renderFullyLoaded(overrides?: Parameters<typeof setDefaultStoreValues>[0]) {
  setDefaultStoreValues({
    config: { initialized: true, repos: [], ...overrides?.config },
    viewer: { stateLoaded: true, ...overrides?.viewer },
    profile: { initialized: true, selected: true, ...overrides?.profile },
    ...overrides,
  });
}

import App from "./App";
import { useLocalProfileStore } from "@/shared/stores/local-profile-store";
import { BOOT_SPLASH_WATCHDOG_MS } from "@/features/shell/components/boot-wait";
import { openWorkspaces } from "@/features/org/lib/open-workspaces";

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

beforeEach(() => {
  discoveryCompleteValue = true;
  useFirstRunStore.setState({ guest: false });
  vi.useFakeTimers({ shouldAdvanceTime: true });
  setDefaultStoreValues();
  mockOpenSettingsTab.mockClear();
  // F-25: reset the mocked CurrentUser between tests so identity does
  // not leak across cases. A fully loaded app has a user (SW-FU-B3: past the
  // entry gate with no user, App routes to Login).
  currentUserValue = {
    github_user_id: 100,
    github_username: "u",
    display_name: "U",
    avatar_url: null,
  };
  mockListen.mockImplementation(() => Promise.resolve(() => {}));
  mockLoadPersistedState.mockImplementation(() => Promise.resolve());
  // Mirror the post-init app state so tests that inspect gate logic
  // see the same store shape as a live session (stateLoaded=true is
  // the post-rehydrate state that unlocks the persist path).
  useOrgStore.setState({ stateLoaded: true });
  useOrgStore.getState().setOrgs([
    {
      slug: "test-org",
      display_name: "Test Org",
      created_at: "2026-01-01T00:00:00Z",
      remote_url: null,
      config_flags: {} as Record<string, never>,
      people: [],
      teams: [],
      org_root: "",
    },
  ]);
  // Reset ui-store flags — Cmd+Shift+P toggles workspaceSwitcherOpen, so a
  // dangling `true` from a prior test would cause the shortcut to
  // close the dialog instead of opening it.
  useUiStore.setState({
    orgSwitcherOpen: false,
    teamSwitcherOpen: false,
    settingsPanelOpen: false,
    settingsInitialSection: null,
    noTeamTabs: [],
    noTeamActiveTab: null,
  });
});

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
  // Reset the real useOrgStore so state seeded in beforeEach doesn't
  // leak across test files. Zustand stores are module singletons;
  // another file that imports App after this one would otherwise
  // inherit a populated orgs list.
  useOrgStore.setState({
    orgs: [],
    activeOrgSlug: null,
    activeTeamName: null,
    hasUserIntent: false,
    stateLoaded: false,
    // SW-OB-3: the required-clone gate tests seed synthetic workspaceRepos;
    // clear them so a leaked synthetic doesn't trip another test's
    // workspace-sync teardown (removeWatch) on the next <App/> render.
    workspaceRepos: {},
  });
  useUiStore.setState({
    orgSwitcherOpen: false,
    teamSwitcherOpen: false,
    settingsPanelOpen: false,
    settingsInitialSection: null,
    noTeamTabs: [],
    noTeamActiveTab: null,
  });
});

// ===== INSTALL BUNDLE INITIALIZATION =====

// ===== LOCAL PROFILE INITIALIZATION =====

describe("local profile initialization", () => {
  it("initializes the local profile on mount", async () => {
    // Given: default state
    setDefaultStoreValues();

    // When: App mounts
    await act(async () => {
      render(<App />);
    });

    // Then: the local profile store was initialized
    expect(mockInitializeProfile).toHaveBeenCalled();
  });
});

describe("team landing page", () => {
  it("App mounts the team landing-page hook", async () => {
    setDefaultStoreValues();
    mockUseTeamLandingPage.mockClear();
    await act(async () => {
      render(<App />);
    });
    expect(mockUseTeamLandingPage).toHaveBeenCalled();
  });
});

// ===== AUTH-EXPIRED EVENT LISTENER =====

describe("menu:action event listener (C.16)", () => {
  it("registers a listener for menu:action events", async () => {
    setDefaultStoreValues();
    await act(async () => {
      render(<App />);
    });
    expect(mockListen).toHaveBeenCalledWith("menu:action", expect.any(Function));
  });

  it("rebuilds the native menu (with the right-pane Panels entries) on mount", async () => {
    mockRebuildMenu.mockClear();
    setDefaultStoreValues();
    await act(async () => {
      render(<App />);
    });
    expect(mockRebuildMenu).toHaveBeenCalled();
    const [, panelEntries] = mockRebuildMenu.mock.calls.at(-1) as unknown as [unknown, unknown];
    expect(Array.isArray(panelEntries)).toBe(true);
  });

  it("dispatches a global action by id (Settings)", async () => {
    let menuCb: ((e: { payload: string }) => void) | null = null;
    mockListen.mockImplementation((event: string, cb: (e: { payload: string }) => void) => {
      if (event === "menu:action") menuCb = cb;
      return Promise.resolve(() => {});
    });
    setDefaultStoreValues();
    await act(async () => {
      render(<App />);
    });
    expect(menuCb).not.toBeNull();
    expect(mockOpenSettingsTab).not.toHaveBeenCalled();

    act(() => {
      menuCb!({ payload: "settings.open" });
    });

    // SW-UX: Settings is a tab — the action opens it via the tab opener.
    expect(mockOpenSettingsTab).toHaveBeenCalled();
  });

  it("no-ops when a repo-scoped action fires with no repo active", async () => {
    let menuCb: ((e: { payload: string }) => void) | null = null;
    mockListen.mockImplementation((event: string, cb: (e: { payload: string }) => void) => {
      if (event === "menu:action") menuCb = cb;
      return Promise.resolve(() => {});
    });
    // `selectedRepoPath: null` by default
    setDefaultStoreValues();
    await act(async () => {
      render(<App />);
    });

    // Nothing should happen — no exception, no settings open, etc.
    act(() => {
      menuCb!({ payload: "viewer.toggle-terminal" });
    });
    expect(useUiStore.getState().settingsPanelOpen).toBe(false);
  });

  it("ignores unknown action ids", async () => {
    let menuCb: ((e: { payload: string }) => void) | null = null;
    mockListen.mockImplementation((event: string, cb: (e: { payload: string }) => void) => {
      if (event === "menu:action") menuCb = cb;
      return Promise.resolve(() => {});
    });
    setDefaultStoreValues();
    await act(async () => {
      render(<App />);
    });
    // No crash — the registry lookup returns undefined, handler early-returns
    expect(() => {
      act(() => {
        menuCb!({ payload: "does.not.exist" });
      });
    }).not.toThrow();
  });
});

// SW-OB-6: the token-heartbeat tests were removed with the `heartbeatJira`
// effect — Jira left the auth gate, so there is no background heartbeat here.

// ===== FILE WATCHER SETUP =====

describe("file watcher setup", () => {
  it("bootstraps watcher with empty config on init (workspace-sync owns directory selection)", async () => {
    renderFullyLoaded({ config: { initialized: true } });

    await act(async () => {
      render(<App />);
    });

    // Then: startWatching is called with an empty array — just creates the watcher instance
    await waitFor(() => {
      expect(mockStartWatching).toHaveBeenCalledWith([]);
    });
  });

  it("does not pass config repos to startWatching — workspace-sync owns directory selection", async () => {
    // Given: config has repos with context paths (would have been watched by the old code)
    mockStartWatching.mockClear();
    const repos = [
      {
        repoPath: "/repos/a",
        repoName: "a",
        contextRepoPath: "/context/a",
        hasRemote: false,
        remoteUrl: null,
      },
    ];
    renderFullyLoaded({ config: { initialized: true, repos } });
    configStoreValues = { ...configStoreValues, repos };

    await act(async () => {
      render(<App />);
    });

    // Then: startWatching is called with EMPTY array — repos are not forwarded
    await waitFor(() => {
      expect(mockStartWatching).toHaveBeenCalledWith([]);
      expect(mockStartWatching).toHaveBeenCalledTimes(1);
    });
  });

  it("does NOT start watcher when not initialized", async () => {
    // Given: not initialized, no repos
    setDefaultStoreValues({ config: { initialized: false, repos: [] } });
    configStoreValues = { ...configStoreValues, repos: [] };
    mockStartWatching.mockClear();

    await act(async () => {
      render(<App />);
    });

    expect(mockStartWatching).not.toHaveBeenCalled();
  });

  it("listens for context-file-change events (SW-CP-18: install-file-change retired)", async () => {
    // SS-9: the `fs-change` listener retired with sync-store's install-health
    // half. SW-CP-18: the `install-file-change` listener retired with the
    // capture / install-handler axis — the surviving `context-file-change`
    // listener drives the seamless-sync watcher → viewer update path.
    renderFullyLoaded({ config: { initialized: true } });

    await act(async () => {
      render(<App />);
    });

    await waitFor(() => {
      const calls = mockListen.mock.calls.map((c) => c[0]);
      expect(calls).toContain("context-file-change");
      expect(calls).not.toContain("install-file-change");
      expect(calls).not.toContain("fs-change");
    });
  });
});

describe("context-file-change routing (G-8)", () => {
  it("routes a repo-less team-level change to the team owning the content path", async () => {
    renderFullyLoaded({ config: { initialized: true } });
    const teamContentPath = "/u/.codesense/orgs/acme/default/content";
    let fire: ((e: { payload: unknown }) => void) | undefined;
    mockListen.mockImplementation((event: string, cb: (...args: unknown[]) => void) => {
      if (event === "context-file-change") fire = cb as typeof fire;
      return Promise.resolve(() => {});
    });
    mockBumpFileRefreshForSourceRepo.mockClear();

    await act(async () => {
      render(<App />);
    });
    await waitFor(() => expect(fire).toBeDefined());
    // Seeded after mount: the app's own boot may rebuild the repo map.
    useOrgStore.setState({
      workspaceRepos: {
        "workspace:acme:default:o/r": {
          repoPath: "workspace:acme:default:o/r",
          repoName: "r",
          contextRepoPath: "/u/.codesense/orgs/acme/default/repos/o/r",
          teamContentPath,
          hasRemote: false,
          remoteUrl: null,
          mainDoc: "",
          localPath: null,
        },
      },
    });

    const story = `${teamContentPath}/orphan-epic/ORPH-1.md`;
    act(() => fire!({ payload: { repo_path: "", context_repo_path: "", file_path: story } }));
    await waitFor(() =>
      expect(mockBumpFileRefreshForSourceRepo).toHaveBeenCalledWith(
        "workspace:acme:default:o/r",
        new Set([story]),
      ),
    );

    // A repo-less change no team owns bumps nothing (never an empty key).
    mockBumpFileRefreshForSourceRepo.mockClear();
    act(() => fire!({ payload: { repo_path: "", file_path: "/elsewhere/x.md" } }));
    await new Promise((r) => setTimeout(r, 400));
    expect(mockBumpFileRefreshForSourceRepo).not.toHaveBeenCalled();
  });
});

// ===== KEYBOARD SHORTCUTS =====

describe("keyboard shortcuts", () => {
  it("opens the Settings panel on Cmd+, when a workspace is opened", async () => {
    // Given: fully loaded with an opened team (T.6 replacement for the
    // retired `selectedRepoPath` / `viewMode` / `openedRepos` seeds).
    renderFullyLoaded({
      viewer: {
        stateLoaded: true,
        openedTeams: ["acme:default"],
      },
    });

    await act(async () => {
      render(<App />);
    });

    expect(mockOpenSettingsTab).not.toHaveBeenCalled();

    // When: Cmd+, is pressed
    await act(async () => {
      fireEvent.keyDown(document, { key: ",", metaKey: true });
    });

    // Then: the Settings tab opens at the General section
    expect(mockOpenSettingsTab).toHaveBeenCalledWith("general");
  });

  it("opens the Settings panel on Cmd+, even when no repo is selected", async () => {
    // Given: fully loaded with no selected repo (picker view)
    renderFullyLoaded();

    await act(async () => {
      render(<App />);
    });

    // When: Cmd+, is pressed
    await act(async () => {
      fireEvent.keyDown(document, { key: ",", metaKey: true });
    });

    // Then: Settings still opens — the old repo-gated behavior is gone
    expect(mockOpenSettingsTab).toHaveBeenCalledWith("general");
  });

  it("opens Settings on Cmd+, even when focus is in an input", async () => {
    // Given: fully loaded with an input focused
    renderFullyLoaded();

    await act(async () => {
      render(<App />);
    });

    const input = document.createElement("input");
    document.body.appendChild(input);
    input.focus();

    // When: Cmd+, is pressed with the input as the event target
    await act(async () => {
      fireEvent.keyDown(input, { key: ",", metaKey: true });
    });

    // Then: Settings opens — matches macOS convention that Cmd+,
    // works from any focus context.
    expect(mockOpenSettingsTab).toHaveBeenCalledWith("general");

    document.body.removeChild(input);
  });

  it("closes active tab on Cmd+W (new decision tree path b: file tab open)", async () => {
    // T.6 AC-4.4: Cmd+W decision tree
    //   (a) terminal focused → close terminal tab
    //   (b) elif file tab open → close it
    //   (c) elif workspaces exist → focus first root via jumpToRootWorkspace
    //   (d) else → Tauri-default-close
    // This test exercises (b). closeTab signature is (teamKey, repoPath,
    // filePath); repoPath is the **synthetic workspace key** derived from
    // teamKey + sourceRepoSlug via `getActiveWorkspaceContext()` — NOT
    // `workspace.sourceRepoPath` (the M.2 on-disk clone path which would
    // miss `workspaceRepos[...]` lookups inside `revealOpenedFile`).
    const closeTab = vi.fn();
    renderFullyLoaded();

    // Real team key so `getActiveWorkspaceContext` doesn't sentinel-out.
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
    const teamKey = "acme:default";
    viewerGetState.mockImplementation(() => ({
      ...viewerStoreValues,
      activeTab: { [teamKey]: "readme.md" },
      closeTab,
      openTabs: { [teamKey]: [{ path: "readme.md", name: "readme.md" }] },
      workspaces: {
        [teamKey]: [
          {
            id: "w1",
            kind: "root",
            cwd: "/repos/x",
            sourceRepoPath: "/repos/x",
            sourceRepoSlug: "acme/x",
            status: "ready",
          },
        ],
      },
      activeWorkspace: { [teamKey]: "w1" },
    }));

    await act(async () => {
      render(<App />);
    });

    // When: Cmd+W is pressed
    await act(async () => {
      fireEvent.keyDown(document, { key: "w", metaKey: true });
    });

    // Then: closeTab fires with the synthetic workspace key (NOT
    // sourceRepoPath). `workspace:acme:default:acme/x` is the canonical
    // form `revealOpenedFile`'s `resolveRepo(repoPath)` will hit.
    expect(closeTab).toHaveBeenCalledWith(teamKey, "workspace:acme:default:acme/x", "readme.md", {
      user: true,
    });
  });

  it("Cmd+W with a run output focused closes the output, not the file tab under it", async () => {
    const closeTab = vi.fn();
    renderFullyLoaded();
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
    const teamKey = "acme:default";
    viewerGetState.mockImplementation(() => ({
      ...viewerStoreValues,
      activeTab: { [teamKey]: "readme.md" },
      closeTab,
      openTabs: { [teamKey]: [{ path: "readme.md", name: "readme.md" }] },
    }));
    const { useAgentResultStore } = await import("@/shared/stores/use-agent-result-store");
    useAgentResultStore.getState().setTarget("run-1", null, null, "output", teamKey);
    const output = document.createElement("section");
    output.setAttribute("data-run-output", "");
    output.tabIndex = -1;
    document.body.appendChild(output);
    output.focus();

    await act(async () => {
      render(<App />);
    });
    await act(async () => {
      fireEvent.keyDown(document, { key: "w", metaKey: true });
    });

    expect(useAgentResultStore.getState().runId).toBeNull();
    expect(closeTab).not.toHaveBeenCalled();
    document.body.removeChild(output);
  });

  it("closes active tab on Cmd+W in a slug-less epic workspace (SW-FX-38: repoPath null)", async () => {
    // SW-FX-38: `closeTab` keys off (teamKey, filePath); `repoPath` is
    // best-effort for the post-close reveal (safe no-op on ""). A slug-less
    // `epic` workspace carries no `sourceRepoSlug`, so
    // `getActiveWorkspaceContext().repoPath` is null — but ⌘W must STILL close
    // the file tab. RED on baseline (`activeTab && repoPath` blocked it), GREEN
    // after the guard drops the `repoPath` conjunct.
    const closeTab = vi.fn();
    const switchWorkspace = vi.fn();
    renderFullyLoaded();

    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
    const teamKey = "acme:default";
    viewerGetState.mockImplementation(() => ({
      ...viewerStoreValues,
      activeTab: { [teamKey]: "notes.md" },
      closeTab,
      switchWorkspace,
      openTabs: { [teamKey]: [{ path: "notes.md", name: "notes.md" }] },
      workspaces: {
        // kind:"epic" with NO sourceRepoSlug → repoPath derives to null.
        [teamKey]: [{ id: "e1", kind: "epic", cwd: "/epic", status: "ready" }],
      },
      activeWorkspace: { [teamKey]: "e1" },
    }));

    await act(async () => {
      render(<App />);
    });
    await act(async () => {
      fireEvent.keyDown(document, { key: "w", metaKey: true });
    });

    // repoPath falls back to "" since the slug-less workspace yields null.
    expect(closeTab).toHaveBeenCalledWith(teamKey, "", "notes.md", { user: true });
    // Path (b) consumes the keystroke — (c) focus-first-root must NOT run even
    // though a workspace is present and reachable (proves (b) wins over (c)).
    expect(switchWorkspace).not.toHaveBeenCalled();
  });

  it("closes active tab on Cmd+W when no workspace is focused (SW-FX-38: workspace null)", async () => {
    // SW-FX-38: even with no active workspace (`getActiveWorkspaceContext()
    // .workspace === null`), an open file tab must close on ⌘W. Path (c)
    // focus-first-root is not reached because `activeTab` is truthy so (b)
    // fires first.
    const closeTab = vi.fn();
    renderFullyLoaded();

    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
    const teamKey = "acme:default";
    viewerGetState.mockImplementation(() => ({
      ...viewerStoreValues,
      activeTab: { [teamKey]: "x.md" },
      closeTab,
      openTabs: { [teamKey]: [{ path: "x.md", name: "x.md" }] },
      workspaces: { [teamKey]: [] },
      activeWorkspace: {},
    }));

    await act(async () => {
      render(<App />);
    });
    await act(async () => {
      fireEvent.keyDown(document, { key: "w", metaKey: true });
    });

    expect(closeTab).toHaveBeenCalledWith(teamKey, "", "x.md", { user: true });
  });

  // SW-UX (app-flow redesign): the "⌘W closes Settings (not the hidden tab)"
  // special-case is retired — Settings is a virtual TAB now, so ⌘W closes it via
  // the normal active-tab-close path (branch b), covered by the tab-close tests.

  it("Cmd+W on a focused terminal takes the default close (Hide/End) — never a raw kill", async () => {
    renderFullyLoaded();
    const teamKey = "_no_team:_none";
    const removeTerminalTab = vi.fn();
    viewerGetState.mockImplementation(() => ({ ...viewerStoreValues, removeTerminalTab }));
    mockIsTerminalFocused.mockReturnValue(true);
    try {
      await act(async () => {
        render(<App />);
      });
      await act(async () => {
        fireEvent.keyDown(document, { key: "w", metaKey: true });
      });
      expect(mockCloseActiveTerminalTab).toHaveBeenCalledWith(teamKey);
      expect(removeTerminalTab).not.toHaveBeenCalled();
    } finally {
      mockIsTerminalFocused.mockReturnValue(false);
    }
  });

  it.each([
    [
      "quick view",
      ["data-terminal-popup", "data-row-popup"],
      '<div class="xterm"><textarea></textarea></div>',
    ],
    ["inbox chat", ["data-row-popup"], "<textarea></textarea>"],
  ])(
    "Cmd+W with focus in the Agents pane's %s closes nothing behind it",
    async (_name, attrs, inner) => {
      const closeTab = vi.fn();
      renderFullyLoaded();
      useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
      const teamKey = "acme:default";
      viewerGetState.mockImplementation(() => ({
        ...viewerStoreValues,
        activeTab: { [teamKey]: "readme.md" },
        closeTab,
        openTabs: { [teamKey]: [{ path: "readme.md", name: "readme.md" }] },
      }));
      mockCloseActiveTerminalTab.mockClear();
      // The popup closes itself on ⌘W: its marker tells the app's ⌘W to leave the tabs alone.
      const popup = document.createElement("div");
      for (const attr of attrs) popup.setAttribute(attr, "");
      popup.innerHTML = inner;
      document.body.appendChild(popup);
      try {
        await act(async () => {
          render(<App />);
        });
        popup.querySelector("textarea")!.focus();
        await act(async () => {
          fireEvent.keyDown(document.activeElement!, { key: "w", code: "KeyW", metaKey: true });
        });
        expect(mockCloseActiveTerminalTab).not.toHaveBeenCalled();
        expect(closeTab).not.toHaveBeenCalled();
      } finally {
        popup.remove();
      }
    },
  );

  it.each([
    ["AZERTY ⌘Z (its z sits on W's place)", { key: "z", code: "KeyW" }, false],
    ["Dvorak ⌘, (its , sits on W's place)", { key: ",", code: "KeyW" }, false],
    ["⇧⌘W", { key: "W", code: "KeyW", shiftKey: true }, false],
    ["Dvorak ⌘W (its w sits on the comma's place)", { key: "w", code: "KeyComma" }, true],
    ["a Cyrillic layout's ⌘W (ц on W's place)", { key: "ц", code: "KeyW" }, true],
  ])("Cmd+W reads the chord by its letter: %s", async (_name, init, closes) => {
    const closeTab = vi.fn();
    renderFullyLoaded();
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
    const teamKey = "acme:default";
    viewerGetState.mockImplementation(() => ({
      ...viewerStoreValues,
      activeTab: { [teamKey]: "readme.md" },
      closeTab,
      openTabs: { [teamKey]: [{ path: "readme.md", name: "readme.md" }] },
    }));
    await act(async () => {
      render(<App />);
    });
    await act(async () => {
      fireEvent.keyDown(document, { ...init, metaKey: true });
    });
    expect(closeTab).toHaveBeenCalledTimes(closes ? 1 : 0);
  });

  it("focuses team's first root on Cmd+W when zero tabs open (decision tree path c)", async () => {
    // T.6 AC-4.4: when no terminal tab or file tab to close, focus the
    // active team's first root workspace (replaces the retired
    // goToPicker fallback). switchWorkspace is the canonical action.
    const switchWorkspace = vi.fn();
    renderFullyLoaded();

    const teamKey = "_no_team:_none";
    const rootId = "root:owner/repo";
    viewerGetState.mockImplementation(() => ({
      ...viewerStoreValues,
      activeTab: {}, // no active tab
      switchWorkspace,
      openTabs: { [teamKey]: [] },
      workspaces: {
        [teamKey]: [
          {
            id: rootId,
            kind: "root",
            cwd: "/repos/x",
            sourceRepoPath: "/repos/x",
            status: "ready",
          },
          { id: "story-1", kind: "story", cwd: "/wt/s1", status: "ready" },
        ],
      },
      // Active workspace is the story (not the root) so the (c) branch
      // has something to switch *to*.
      activeWorkspace: { [teamKey]: "story-1" },
    }));

    await act(async () => {
      render(<App />);
    });

    // When: Cmd+W is pressed with no tabs open
    await act(async () => {
      fireEvent.keyDown(document, { key: "w", metaKey: true });
    });

    // Then: switches to the team's first root workspace.
    expect(switchWorkspace).toHaveBeenCalledWith(teamKey, rootId);
  });

  // ===== C.5 shortcuts =====

  it("Cmd+Shift+G opens org switcher when 2+ orgs exist", async () => {
    renderFullyLoaded();
    useOrgStore.setState({
      orgs: [
        {
          slug: "a",
          display_name: "A",
          created_at: "2026-01-01T00:00:00Z",
          remote_url: null,
          config_flags: {} as Record<string, never>,
          people: [],
          teams: [],
          org_root: "",
        },
        {
          slug: "b",
          display_name: "B",
          created_at: "2026-01-01T00:00:00Z",
          remote_url: null,
          config_flags: {} as Record<string, never>,
          people: [],
          teams: [],
          org_root: "",
        },
      ],
      activeOrgSlug: "a",
      stateLoaded: true,
    });

    await act(async () => {
      render(<App />);
    });

    expect(useUiStore.getState().orgSwitcherOpen).toBe(false);
    await act(async () => {
      fireEvent.keyDown(document, { key: "g", metaKey: true, shiftKey: true });
    });
    expect(useUiStore.getState().orgSwitcherOpen).toBe(true);
  });

  it("Cmd+Shift+G opens the org switcher even with 1 org (Create/Join path)", async () => {
    // The switcher is the canonical home for "Create workspace" — it
    // must open even when the user has only 1 workspace so they can
    // discover the create/join entry points in its footer.
    renderFullyLoaded();
    // Default setup has 1 workspace (test-org)
    await act(async () => {
      render(<App />);
    });
    await act(async () => {
      fireEvent.keyDown(document, { key: "g", metaKey: true, shiftKey: true });
    });
    expect(useUiStore.getState().orgSwitcherOpen).toBe(true);
  });

  it("Cmd+Shift+G opens the org switcher even when focus is in an input", async () => {
    // Meta+Shift chords can't collide with typing, so the switcher
    // shortcuts deliberately fire from any focus context — including
    // while the user is typing in the Settings search input.
    renderFullyLoaded();
    useOrgStore.setState({
      orgs: [
        {
          slug: "a",
          display_name: "A",
          created_at: "2026-01-01T00:00:00Z",
          remote_url: null,
          config_flags: {} as Record<string, never>,
          people: [],
          teams: [],
          org_root: "",
        },
        {
          slug: "b",
          display_name: "B",
          created_at: "2026-01-01T00:00:00Z",
          remote_url: null,
          config_flags: {} as Record<string, never>,
          people: [],
          teams: [],
          org_root: "",
        },
      ],
      activeOrgSlug: "a",
      stateLoaded: true,
    });

    await act(async () => {
      render(<App />);
    });

    const input = document.createElement("input");
    document.body.appendChild(input);
    input.focus();

    await act(async () => {
      fireEvent.keyDown(input, { key: "g", metaKey: true, shiftKey: true });
    });
    expect(useUiStore.getState().orgSwitcherOpen).toBe(true);
    document.body.removeChild(input);
  });

  // NOTE: Cmd+\ (terminal) and Cmd+J (scratch note) live in
  // RepoViewerView's keyboard handler, not in App.tsx. The status bar
  // calls the same `useViewerStore` actions. Covered by the existing
  // viewer tests, not App.tsx tests.

  it("Cmd+Alt+Shift+T does nothing when active workspace has fewer than 2 user teams", async () => {
    renderFullyLoaded();
    // Default test-org has no teams
    await act(async () => {
      render(<App />);
    });
    await act(async () => {
      fireEvent.keyDown(document, {
        key: "t",
        code: "KeyT",
        metaKey: true,
        altKey: true,
        shiftKey: true,
      });
    });
    expect(useUiStore.getState().teamSwitcherOpen).toBe(false);
  });

  it("Cmd+Alt+Shift+T opens team switcher when user is on 2+ teams", async () => {
    renderFullyLoaded();

    await act(async () => {
      render(<App />);
    });
    // Seed the org-store with orgs/active state, then the mocked
    // getCurrentUser to mimic an authenticated user. The shortcut
    // handler reads getCurrentUser() synchronously, so the user must
    // be set before the keyboard event fires.
    currentUserValue = {
      github_user_id: 100,
      github_username: "u",
      display_name: "U",
      avatar_url: null,
    };
    await act(async () => {
      useOrgStore.setState({
        orgs: [
          {
            slug: "a",
            display_name: "A",
            created_at: "2026-01-01T00:00:00Z",
            remote_url: null,
            config_flags: {} as Record<string, never>,
            people: [],
            teams: [
              { name: "t1", source_repos: [] },
              { name: "t2", source_repos: [] },
            ],
            org_root: "",
          },
        ],
        activeOrgSlug: "a",
        stateLoaded: true,
      });
    });
    await act(async () => {
      fireEvent.keyDown(document, {
        key: "t",
        code: "KeyT",
        metaKey: true,
        altKey: true,
        shiftKey: true,
      });
    });
    expect(useUiStore.getState().teamSwitcherOpen).toBe(true);
  });

  it("Cmd+Alt+Shift+T opens the team switcher even when focus is in an input", async () => {
    renderFullyLoaded();
    await act(async () => {
      render(<App />);
    });
    currentUserValue = {
      github_user_id: 100,
      github_username: "u",
      display_name: "U",
      avatar_url: null,
    };
    await act(async () => {
      useOrgStore.setState({
        orgs: [
          {
            slug: "a",
            display_name: "A",
            created_at: "2026-01-01T00:00:00Z",
            remote_url: null,
            config_flags: {} as Record<string, never>,
            people: [],
            teams: [
              { name: "t1", source_repos: [] },
              { name: "t2", source_repos: [] },
            ],
            org_root: "",
          },
        ],
        activeOrgSlug: "a",
        stateLoaded: true,
      });
    });

    const input = document.createElement("input");
    document.body.appendChild(input);
    input.focus();

    await act(async () => {
      fireEvent.keyDown(input, {
        key: "t",
        code: "KeyT",
        metaKey: true,
        altKey: true,
        shiftKey: true,
      });
    });
    expect(useUiStore.getState().teamSwitcherOpen).toBe(true);
    document.body.removeChild(input);
  });
});

// ===== VIEW ROUTING =====

describe("view routing", () => {
  it("shows splash/loading screen when not initialized", async () => {
    // Given: not yet initialized
    setDefaultStoreValues();

    // When: App renders
    await act(async () => {
      render(<App />);
    });

    // Then: splash screen with CodeSense is visible
    expect(screen.getByText("CodeSense")).toBeInTheDocument();
    expect(screen.queryByTestId("team-workspace-empty-state")).not.toBeInTheDocument();
  });

  const OB3_ORG_FOR_B6 = {
    slug: "acme",
    display_name: "Acme",
    created_at: "2026-01-01T00:00:00Z",
    remote_url: null,
    config_flags: {} as Record<string, never>,
    people: [],
    teams: [{ name: "default", source_repos: [] }],
    org_root: "",
  };

  it("SW-FU-B6: a join pending across sign-in reopens the join form once the app is in", async () => {
    renderFullyLoaded({ viewer: { stateLoaded: true, openedTeams: ["acme:default"] } });
    useOrgStore.setState({
      orgs: [OB3_ORG_FOR_B6],
      activeOrgSlug: "acme",
      activeTeamName: "default",
    });
    useUiStore.setState({ workspacesScreen: null });
    useFirstRunStore.setState({ joinPending: true });

    await act(async () => {
      render(<App />);
    });

    expect(useUiStore.getState().workspacesScreen).toBe("join");
    // A landing the user did not ask for keeps full-screen CLI on.
    expect(viewerStoreValues.openFile).toHaveBeenCalledWith(
      "acme:default",
      "",
      "virtual://workspaces",
      { keepLayout: true },
    );
    // Landed once: it doesn't reopen on the next launch.
    expect(useFirstRunStore.getState().joinPending).toBe(false);
  });

  it("SW-FU-B6: without a pending join, nothing opens", async () => {
    renderFullyLoaded({ viewer: { stateLoaded: true, openedTeams: ["acme:default"] } });
    useOrgStore.setState({
      orgs: [OB3_ORG_FOR_B6],
      activeOrgSlug: "acme",
      activeTeamName: "default",
    });
    useUiStore.setState({ workspacesScreen: null });
    useFirstRunStore.setState({ joinPending: false });

    await act(async () => {
      render(<App />);
    });

    expect(useUiStore.getState().workspacesScreen).toBeNull();
  });

  it("SW-FU-B3: one splash watchdog spans the boot gates (startup, then discovery)", async () => {
    // Startup gate: the viewer state has not loaded yet.
    renderFullyLoaded({ viewer: { stateLoaded: false } });
    const { rerender } = render(<App />);
    expect(screen.getByText("CodeSense")).toBeInTheDocument();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(BOOT_SPLASH_WATCHDOG_MS - 2000);
    });
    // The startup gate clears; the app now waits on org discovery. The deadline
    // carries over, not restarts.
    discoveryCompleteValue = false;
    renderFullyLoaded();
    await act(async () => {
      rerender(<App />);
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(2000);
    });
    expect(screen.getByTestId("boot-problem")).toBeInTheDocument();
  });

  it("no active local profile shows the profile chooser, never a welcome or sign-in wall", async () => {
    renderFullyLoaded({
      profile: { initialized: true, selected: false },
    });
    await act(async () => {
      render(<App />);
    });
    expect(screen.getByRole("heading", { name: "Local profile" })).toBeInTheDocument();
    expect(screen.queryByTestId("login-view")).not.toBeInTheDocument();
    expect(screen.queryByTestId("welcome-view")).not.toBeInTheDocument();
    expect(screen.queryByTestId("team-workspace-empty-state")).not.toBeInTheDocument();
  });

  it("with no organizations, lands on local organization setup with no welcome or checklist", async () => {
    renderFullyLoaded();
    useOrgStore.setState({ orgs: [], activeOrgSlug: null, activeTeamName: null });
    await act(async () => {
      render(<App />);
    });
    expect(screen.getByTestId("no-org-setup")).toContainElement(screen.getByTestId("org-picker"));
    expect(screen.queryByTestId("welcome-view")).not.toBeInTheDocument();
    expect(screen.queryByTestId("activation-checklist")).not.toBeInTheDocument();
  });

  it("shows team-workspace empty state when fully loaded with no opened teams", async () => {
    // Given: fully loaded, no opened teams
    renderFullyLoaded({ viewer: { stateLoaded: true, openedTeams: [] } });

    await act(async () => {
      render(<App />);
    });

    // Then: empty state is rendered (T.7 replacement for the retired picker)
    expect(screen.getByTestId("team-workspace-empty-state")).toBeInTheDocument();
  });

  it("SW-OC-7: a stale/null active team lands on the team-scoped empty state (Home Base retired)", async () => {
    // Conditions that once landed on the retired hub now route straight to the
    // team-scoped empty state — the only no-active-team surface.
    renderFullyLoaded({ viewer: { stateLoaded: true, openedTeams: [] } });
    useOrgStore.setState({ activeOrgSlug: "test-org", activeTeamName: null });

    await act(async () => {
      render(<App />);
    });

    expect(screen.getByTestId("team-workspace-empty-state")).toBeInTheDocument();
  });

  it("RF-perf-10: mounts only the active team's subtree; backgrounded teams unmount", async () => {
    // RF-perf-10 supersedes the T.6 keep-all-mounted invariant: App.tsx now
    // mounts <TeamWorkspaceView> ONLY for teams in the render set (active +
    // debounce-window + unsaved-edit). A backgrounded team's heavyweight subtree
    // is unmounted to reclaim renderer memory; its Rust PTYs survive (unsubscribe,
    // not kill) and re-hydrate on return. The keyed wrapper div persists for
    // stable list reconciliation, but the inner view is gone.
    renderFullyLoaded({
      viewer: { stateLoaded: true, openedTeams: ["acme:default", "acme:platform"] },
    });
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });

    await act(async () => {
      render(<App />);
    });

    // Active team is mounted...
    expect(screen.getByTestId("team-workspace-acme:default")).toBeInTheDocument();
    // ...the backgrounded team is NOT (unmounted for memory).
    expect(screen.queryByTestId("team-workspace-acme:platform")).not.toBeInTheDocument();
  });

  it("SW-FU-B7: with no active team, Create a workspace lands on the Workspaces tab in the no-team tab host", async () => {
    // No dialog or overlay fallback: the no-team state has a minimal tab host, so the
    // management openers land on the same tab surface as everywhere else (the
    // beforeEach leaves activeOrgSlug null).
    renderFullyLoaded({ viewer: { stateLoaded: true, openedTeams: [] } });

    await act(async () => {
      render(<App />);
    });
    expect(screen.getByTestId("team-workspace-empty-state")).toBeInTheDocument();

    await act(async () => {
      openWorkspaces("create");
    });

    expect(screen.getByTestId("no-team-tab-host")).toBeInTheDocument();
    expect(screen.getByTestId("workspaces-tab")).toBeInTheDocument();
    expect(screen.getByRole("form", { name: "Create a new context repo" })).toBeInTheDocument();
    expect(screen.queryByTestId("team-workspace-empty-state")).not.toBeInTheDocument();
  });

  it("SW-FU-B7: ⌘W closes the active no-team tab (never the window)", async () => {
    renderFullyLoaded({ viewer: { stateLoaded: true, openedTeams: [] } });
    await act(async () => {
      render(<App />);
    });
    await act(async () => {
      openWorkspaces("home");
    });
    expect(screen.getByTestId("workspaces-tab")).toBeInTheDocument();

    const cmdW = new KeyboardEvent("keydown", {
      key: "w",
      metaKey: true,
      bubbles: true,
      cancelable: true,
    });
    await act(async () => {
      document.dispatchEvent(cmdW);
    });

    // preventDefault keeps Tauri from closing the window.
    expect(cmdW.defaultPrevented).toBe(true);
    expect(screen.queryByTestId("workspaces-tab")).not.toBeInTheDocument();
    expect(screen.getByTestId("team-workspace-empty-state")).toBeInTheDocument();
  });

  it("SW-OB-9: shows the empty state when the active team is null even if a team is still opened (no blank/dead-end)", async () => {
    // AC-6 empty-set case: the user hid their last shown team, so
    // `activeTeamName` is null while a now-hidden team lingers in
    // `openedTeams`. Keying the empty state only on `openedTeams.length === 0`
    // would blank the content (the lingering team is display:none and the
    // empty state wouldn't mount). Gate on the null active team instead.
    renderFullyLoaded({
      viewer: { stateLoaded: true, openedTeams: ["acme:a"] },
    });
    useOrgStore.setState({
      orgs: [
        {
          slug: "acme",
          display_name: "Acme",
          created_at: "2026-01-01T00:00:00Z",
          remote_url: null,
          config_flags: {} as Record<string, never>,
          people: [],
          teams: [{ name: "a", source_repos: [] }],
          org_root: "",
        },
      ],
      activeOrgSlug: "acme",
      activeTeamName: null,
    });

    await act(async () => {
      render(<App />);
    });

    // The no-team empty state mounts (its "Browse all teams" CTA is the AC-6
    // re-add surface) rather than a blank content area.
    expect(screen.getByTestId("team-workspace-empty-state")).toBeInTheDocument();
  });

  // SW-OB-3 AC-1: the required-clone gate at the App routing boundary.
  const OB3_ORG = {
    slug: "acme",
    display_name: "Acme",
    created_at: "2026-01-01T00:00:00Z",
    remote_url: null,
    config_flags: {} as Record<string, never>,
    people: [],
    teams: [
      {
        name: "default",
        source_repos: [{ owner: "acme", repo: "web" }],
      },
    ],
    org_root: "",
  };
  function ob3Synthetic(localPath: string | null) {
    return {
      "workspace:acme:default:acme/web": {
        repoPath: "workspace:acme:default:acme/web",
        repoName: "web (Acme)",
        contextRepoPath: "/x",
        teamContentPath: "/x",
        hasRemote: false,
        remoteUrl: null,
        mainDoc: "",
        localPath,
      },
    };
  }

  it("SW-UX: a clone-less active team renders its workspace pane (no full-screen gate)", async () => {
    // SW-UX (app-flow redesign): the required-clone gate that covered the WHOLE app
    // is removed. A team with a context repo but no attached source-repo clone is
    // usable — its workspace renders, and "add a source repo" is an in-app action,
    // not a blocking overlay.
    renderFullyLoaded({ viewer: { stateLoaded: true, openedTeams: ["acme:default"] } });
    useOrgStore.setState({
      orgs: [OB3_ORG],
      activeOrgSlug: "acme",
      activeTeamName: "default",
      workspaceRepos: ob3Synthetic(null), // tracked repo, no clone attached
    });

    await act(async () => {
      render(<App />);
    });

    // No full-screen empty-state overlay...
    expect(screen.queryByTestId("team-workspace-empty-state")).not.toBeInTheDocument();
    // ...and the active team's pane is VISIBLE (the app is usable).
    const view = screen.getByTestId("team-workspace-acme:default");
    const wrapper = view.closest(".flex-1.min-h-0.overflow-hidden") as HTMLElement;
    expect(wrapper.style.display).not.toBe("none");
  });

  it("SW-FU-B2: an active team absent from openedTeams (v0.0.11 upgrade) shows the empty state, never a dead blank", async () => {
    // The upgrade seam: a Home-Base-era `openedTeams` names a different team than
    // the one that resolves active today, and the active team has no repos. The
    // count>0 and named clauses hold, so only the `activeTeamOpened` wiring keeps
    // App off a content area where every pane is `display:none`.
    renderFullyLoaded({ viewer: { stateLoaded: true, openedTeams: ["acme:other"] } });
    useOrgStore.setState({
      orgs: [
        {
          ...OB3_ORG,
          teams: [
            { name: "default", source_repos: [] },
            { name: "other", source_repos: [] },
          ],
        },
      ],
      activeOrgSlug: "acme",
      activeTeamName: "default",
      workspaceRepos: {},
    });

    await act(async () => {
      render(<App />);
    });

    expect(screen.getByTestId("team-workspace-empty-state")).toBeInTheDocument();
    expect(screen.queryByTestId("team-workspace-acme:default")).not.toBeInTheDocument();
  });

  it("an active team with an attached clone renders its pane (no gate)", async () => {
    renderFullyLoaded({ viewer: { stateLoaded: true, openedTeams: ["acme:default"] } });
    useOrgStore.setState({
      orgs: [OB3_ORG],
      activeOrgSlug: "acme",
      activeTeamName: "default",
      workspaceRepos: ob3Synthetic("/home/me/acme/web"), // clone attached
    });

    await act(async () => {
      render(<App />);
    });

    expect(screen.queryByTestId("team-workspace-empty-state")).not.toBeInTheDocument();
    const view = screen.getByTestId("team-workspace-acme:default");
    const wrapper = view.closest(".flex-1.min-h-0.overflow-hidden") as HTMLElement;
    expect(wrapper.style.display).not.toBe("none");
  });
});

// ===== RESTORE VIEWER STATE AND AUTO-DETECT TOOLS =====

describe("viewer state restoration", () => {
  it("loads persisted state and detects tools when initialized", async () => {
    // Given: initialized, no terminal/editor set
    renderFullyLoaded({
      viewer: {
        stateLoaded: true,
        settings: { terminal: "", editor: "" },
      },
    });
    viewerGetState.mockReturnValue({
      ...viewerStoreValues,
      settings: { terminal: "", editor: "" },
      loadPersistedState: mockLoadPersistedState,
      updateSettings: vi.fn(),
    });

    await act(async () => {
      render(<App />);
    });

    // Then: loadPersistedState and detectTools are called
    await waitFor(() => {
      expect(mockLoadPersistedState).toHaveBeenCalled();
    });
  });
});

// ===== UPDATE CHECK =====

describe("update check", () => {
  it("triggers update check after auth completes", async () => {
    // Update check now uses Rust commands (check_for_update) triggered
    // after allRequiredConnected. Full tests in auto-update.test.tsx (Task 6).
    renderFullyLoaded();

    await act(async () => {
      render(<App />);
    });

    // Verify app renders without errors — update module loads lazily
    expect(screen.getByTestId("team-workspace-empty-state")).toBeInTheDocument();
  });
});

// ---------------------------------------------------------------------------
// Dependency gate integration tests (9.4, 9.5)
// ---------------------------------------------------------------------------

describe("dependency install gate", () => {
  it("9.4: shows install screen when dependencies are missing", async () => {
    mockGetDependencyStatus.mockResolvedValueOnce({
      git: { available: false, path: null, version: null },
      agents: { claude: { available: false, path: null, resolving: false } },
    });
    renderFullyLoaded();
    render(<App />);

    await waitFor(() => {
      expect(screen.getByTestId("dependency-install-view")).toBeInTheDocument();
    });

    // Repo picker should NOT be visible
    expect(screen.queryByTestId("team-workspace-empty-state")).not.toBeInTheDocument();
  });

  it("9.5: skips install screen when dependencies already present", async () => {
    mockGetDependencyStatus.mockResolvedValueOnce({
      git: { available: true, path: "/usr/bin/git", version: "2.44.0" },
      agents: { claude: { available: true, path: "/usr/bin/claude", resolving: false } },
    });
    renderFullyLoaded();
    render(<App />);

    await waitFor(() => {
      expect(screen.getByTestId("team-workspace-empty-state")).toBeInTheDocument();
    });

    // Install screen should NOT be visible
    expect(screen.queryByTestId("dependency-install-view")).not.toBeInTheDocument();
  });

  // SW-AN-3 (AC-1): git ready + NO claude (resolved, absent) → app boots straight
  // through. Claude is optional; its absence must not show the install view.
  it("SW-AN-3: boots into the app when git is ready and Claude is absent", async () => {
    mockGetDependencyStatus.mockResolvedValueOnce({
      git: { available: true, path: "/usr/bin/git", version: "2.44.0" },
      agents: { claude: { available: false, path: null, resolving: false } },
    });
    renderFullyLoaded();
    render(<App />);

    await waitFor(() => {
      expect(screen.getByTestId("team-workspace-empty-state")).toBeInTheDocument();
    });

    // No install/guidance screen for the missing (optional) Claude.
    expect(screen.queryByTestId("dependency-install-view")).not.toBeInTheDocument();
  });
});

// ---------------------------------------------------------------------------
// Dependency health watch (dep-status-changed event)
// ---------------------------------------------------------------------------

describe("dep-status-changed event listener", () => {
  it("registers a listener for dep-status-changed events", async () => {
    renderFullyLoaded();

    await act(async () => {
      render(<App />);
    });

    expect(mockListen).toHaveBeenCalledWith("dep-status-changed", expect.any(Function));
  });

  it("re-checks deps and shows install screen when binary is deleted", async () => {
    // Given: app is fully loaded with deps available
    let depChangedCb: (() => void) | null = null;
    mockListen.mockImplementation((event: string, cb: () => void) => {
      if (event === "dep-status-changed") depChangedCb = cb;
      return Promise.resolve(() => {});
    });

    renderFullyLoaded();

    await act(async () => {
      render(<App />);
    });

    // Initially: repo picker is shown (deps are available)
    await waitFor(() => {
      expect(screen.getByTestId("team-workspace-empty-state")).toBeInTheDocument();
    });

    // When: dep-status-changed fires and getDependencyStatus returns missing git
    mockGetDependencyStatus.mockResolvedValueOnce({
      git: { available: false, path: null, version: null },
      agents: { claude: { available: true, path: "/usr/bin/claude", resolving: false } },
    });

    expect(depChangedCb).not.toBeNull();
    await act(async () => {
      depChangedCb!();
    });

    // Then: the git-guidance gate appears when git goes missing
    await waitFor(() => {
      expect(screen.getByTestId("dependency-install-view")).toBeInTheDocument();
    });
  });

  it("stays on repo picker when dep-status-changed fires but deps are still available", async () => {
    // Given: app is fully loaded
    let depChangedCb: (() => void) | null = null;
    mockListen.mockImplementation((event: string, cb: () => void) => {
      if (event === "dep-status-changed") depChangedCb = cb;
      return Promise.resolve(() => {});
    });

    renderFullyLoaded();

    await act(async () => {
      render(<App />);
    });

    await waitFor(() => {
      expect(screen.getByTestId("team-workspace-empty-state")).toBeInTheDocument();
    });

    // When: dep-status-changed fires but deps are still available (e.g., manifest updated)
    mockGetDependencyStatus.mockResolvedValueOnce({
      git: { available: true, path: "/usr/bin/git", version: "2.44.0" },
      agents: { claude: { available: true, path: "/usr/bin/claude", resolving: false } },
    });

    await act(async () => {
      depChangedCb!();
    });

    // Then: still on repo picker, no install screen
    await waitFor(() => {
      expect(screen.getByTestId("team-workspace-empty-state")).toBeInTheDocument();
    });
    expect(screen.queryByTestId("dependency-install-view")).not.toBeInTheDocument();
  });

  // SW-OB-12: while claude detection is in flight (resolving), the gate must
  // stay on the splash — NOT flash the missing-dependency install/guidance view.
  it("SW-OB-12: stays on splash (not install view) while claude is resolving", async () => {
    mockGetDependencyStatus.mockResolvedValueOnce({
      git: { available: true, path: "/usr/bin/git", version: "2.44.0" },
      agents: { claude: { available: false, path: null, resolving: true } },
    });
    renderFullyLoaded();

    await act(async () => {
      render(<App />);
    });

    // Neither the install/guidance view nor the app render while resolving.
    expect(screen.queryByTestId("dependency-install-view")).not.toBeInTheDocument();
    expect(screen.queryByTestId("team-workspace-empty-state")).not.toBeInTheDocument();
  });

  // SW-OB-12 (risk #3): a background claude resolve completing at startup emits
  // dep-status-changed while deps were never "ready" (splash). The old
  // `!== true` guard swallowed it, stranding the app on the splash. The fixed
  // guard must let this resolving→resolved transition reach the gate.
  it("SW-OB-12: resolving→resolved dep-status-changed reaches the gate (guard fix)", async () => {
    let depChangedCb: (() => void) | null = null;
    mockListen.mockImplementation((event: string, cb: () => void) => {
      if (event === "dep-status-changed") depChangedCb = cb;
      return Promise.resolve(() => {});
    });

    // Initial check: claude still resolving → splash.
    mockGetDependencyStatus.mockResolvedValueOnce({
      git: { available: true, path: "/usr/bin/git", version: "2.44.0" },
      agents: { claude: { available: false, path: null, resolving: true } },
    });
    renderFullyLoaded();

    await act(async () => {
      render(<App />);
    });
    expect(screen.queryByTestId("team-workspace-empty-state")).not.toBeInTheDocument();

    // Background resolve completes: claude now available → event fires.
    mockGetDependencyStatus.mockResolvedValueOnce({
      git: { available: true, path: "/usr/bin/git", version: "2.44.0" },
      agents: { claude: { available: true, path: "/opt/homebrew/bin/claude", resolving: false } },
    });
    expect(depChangedCb).not.toBeNull();
    await act(async () => {
      depChangedCb!();
    });

    // Then: the gate advances to the app (the transition was NOT swallowed).
    await waitFor(() => {
      expect(screen.getByTestId("team-workspace-empty-state")).toBeInTheDocument();
    });
  });
});

it("keeps the unsafe-profile recovery chooser reachable before workspace startup completes", async () => {
  const { useLocalProfileStore } = await import("@/shared/stores/local-profile-store");
  const previous = useLocalProfileStore.getState();
  const choose = vi.fn().mockResolvedValue(undefined);
  setDefaultStoreValues({ config: { initialized: false }, viewer: { stateLoaded: false } });
  useLocalProfileStore.setState({
    initialized: true,
    busy: false,
    error: "This historical profile cannot be represented safely.",
    initialize: vi.fn().mockResolvedValue(undefined),
    select: choose,
    status: {
      active: null,
      profiles: [
        { principal: "9007199254740993", displayName: "Old profile", machineOwner: null },
        { principal: "42", displayName: "Usable profile", machineOwner: null },
      ],
      privateAuthority: null,
    },
  });
  try {
    await act(async () => {
      render(<App />);
    });
    expect(screen.getByRole("alert")).toHaveTextContent("represented safely");
    fireEvent.click(screen.getByRole("button", { name: /Usable profile/ }));
    expect(choose).toHaveBeenCalledWith("42");
  } finally {
    useLocalProfileStore.setState(previous);
  }
});
