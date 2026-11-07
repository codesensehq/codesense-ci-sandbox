// SPDX-License-Identifier: MIT
import { flushBeforeQuit } from "@/features/dashboard/lib/quit-flush";
import { installUserNavigationCapture } from "@/shared/lib/context-navigation";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useConfigStore } from "@/shared/stores/config-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useWorkspaceArchiveStore } from "@/shared/stores/workspace-archive-store";
import { useOrgStore } from "@/shared/stores/org-store";
import {
  buildTeamKey,
  firstRootIdForTeam,
  getActiveTeamKey,
  ROOT_NO_REPOS_SENTINEL,
  teamKeyHasTeam,
} from "@/shared/lib/team-key";
import { useLocalProfileStore } from "@/shared/stores/local-profile-store";
import { AccountSection } from "@/features/settings/components/account-section";
import { useShortcutsStore } from "@/shared/stores/shortcuts-store";
import { useUiStore } from "@/shared/stores/ui-store";
import { useShortcutHandler } from "@/shared/hooks/use-shortcut-handler";
import { dispatchMenuAction } from "@/app/menu-action-dispatch";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { startWatching, stopWatching } from "@/tauri/commands";
import { CONTEXT_FILE_CHANGE_EVENT, type ContextFileChangePayload } from "@/tauri/events";
import { registerWorkspaceWatchBridge } from "@/app/lib/workspace-watch-bridge";
import { listen, emit } from "@tauri-apps/api/event";
import { safeUnlisten } from "@/shared/lib/tauri-listeners";
import { openUrl } from "@tauri-apps/plugin-opener";
import { initLogging, logger } from "@/shared/lib/logger";
import { isCmdW, isRowPopupFocused, isTerminalFocused } from "@/shared/lib/focus-utils";
import { closeAgentResult } from "@/features/viewer/components/agent-result-panel";
import { isRunOutputFocused } from "@/features/viewer/components/agent-run-output-host";
import { closeActiveTerminalTab } from "@/features/viewer/lib/terminal-close";
import { useMcpActions } from "@/shared/hooks/use-mcp-actions";
import { useRightPaneMenuSync } from "@/features/shell/right-pane/panel-menu";
import { useDashboardCommandsSync } from "@/features/dashboard/lib/dashboard-commands";
import { useAgentMessageListener } from "@/shared/hooks/use-agent-message-listener";
import { useRenderedDocumentBridge } from "@/shared/hooks/use-rendered-document-bridge";
import { useCanvasRenderBridge } from "@/shared/hooks/use-canvas-render-bridge";
import { useManagedSessionBridge } from "@/shared/hooks/use-managed-session-bridge";
import { useTerminalStatusEvents } from "@/shared/hooks/use-terminal-status-events";
import { useTerminalDisplayGc } from "@/features/viewer/hooks/use-terminal-display-gc";
import { useTerminalDisplayWarm } from "@/features/viewer/hooks/use-terminal-display-warm";
import { useMarkActiveSessionSeen } from "@/features/viewer/hooks/use-mark-active-session-seen";
import { useWarmPool } from "@/features/viewer/hooks/use-warm-pool";
import { useNeedsYouNotifications } from "@/features/weft/hooks/use-needs-you-notifications";
import { useAttentionAlerts } from "@/features/viewer/hooks/use-attention-alerts";
import { SessionTimersLoader } from "@/features/viewer/components/session-timers-loader";
import { useQuitOnCloseSetting } from "@/features/viewer/hooks/use-quit-on-close-setting";
import { useTerminalServerMenuBarSetting } from "@/features/settings/hooks/use-terminal-server-menu-bar-setting";
import { useBackgroundModeNotification } from "@/features/viewer/hooks/use-background-mode-notification";
import { useBackgroundAgentLimit } from "@/features/viewer/hooks/use-background-agent-limit";
import { useBackgroundSetupCap } from "@/features/viewer/hooks/use-background-setup-cap";
import { hydrateTerminalLayout } from "@/shared/lib/terminal-layout";
import { useWorkspaceViewerSync } from "@/app/hooks/use-workspace-viewer-sync";
import { useTeamLandingPage } from "@/features/viewer/hooks/use-team-landing-page";
import { useStaleArrowFilterCleanup } from "@/features/viewer/hooks/use-stale-arrow-filter";
import { useShellScrollContainment } from "@/app/hooks/use-shell-scroll-containment";
import { useResolvedRepositoryPathsSync } from "@/app/hooks/use-resolved-repository-paths-sync";
import { useSyncWaveRescan } from "@/app/hooks/use-sync-wave-rescan";
import { useCompositeOverlayRefresh } from "@/app/hooks/use-composite-overlay-refresh";
import { useStatusTaxonomy } from "@/features/viewer/hooks/use-status-taxonomy";
import { useActiveTeamKey } from "@/shared/lib/team-key";
import { useDashboardStream } from "@/app/hooks/use-dashboard-stream";
import { useEpicReposReassembly } from "@/app/hooks/use-epic-repos-reassembly";
import { useOrgRegistrySync } from "@/features/org/hooks/use-org-registry-sync";
import { useMoveWorkspaceSync } from "@/features/workspace/hooks/use-move-workspace-sync";
import { useNotesProjection } from "@/features/notes/lib/use-notes-projection";
import { useConfigurationLive } from "@/features/dashboard/lib/configuration-live";
import { usePrefsLive } from "@/features/dashboard/lib/prefs-live";
import { useAgentDefinitions } from "@/features/agents/lib/agent-definitions";
import { useContextProgressListener } from "@/features/shell/hooks/use-context-progress-listener";
import { useAgentRosterListener } from "@/features/viewer/hooks/use-agent-roster-listener";
import { useAgentTabTitles } from "@/features/viewer/lib/agent-tab-titles";
import { useAttendedAgentRelease } from "@/features/viewer/hooks/use-attended-agent-release";
import { useOpenWorkspaceReveal } from "@/shared/hooks/use-open-workspace-reveal";
import { WorkspaceWhereDialog } from "@/features/machines/components/workspace-where-dialog";
// Installs the viewer store's driver for workspaces that live on a machine.
import "@/features/machines/lib/remote-story-workspace";
import { startMachineMemory } from "@/features/machines/lib/machine-memory-boot";
import { useWorktreeRosterListener } from "@/features/viewer/hooks/use-worktree-roster-listener";
import { useEpicAssemblyListener } from "@/features/shell/hooks/use-epic-assembly-listener";
import { useWindowTitle } from "@/app/hooks/use-window-title";
import { useSourceRepoAttachment } from "@/features/attachment/hooks/use-source-repo-attachment";
import { useActiveWorkspaceContext } from "@/shared/hooks/use-active-workspace-context";
import { closeActiveFileTab } from "@/shared/lib/close-active-file-tab";
import { createKeyedDebounce } from "@/shared/lib/keyed-debounce";
import { bumpFileRefreshForSourceRepo } from "@/shared/lib/refresh-for-source-repo";
import { bumpKeyForContextFileChange } from "@/shared/lib/build-synthetic-repo";
import { flushRefreshDebounce } from "@/app/refresh-debounce";
import { CommandPaletteMount } from "@/features/home-base/command-palette/command-palette-mount";
import { PanePanelsMount } from "@/features/shell/right-pane/panes/use-register-pane-panels";
import { useOrgDiscovery } from "@/features/org/hooks/use-org-discovery";
import { OrgPicker } from "@/features/org/components/org-picker";
import { usePendingJoinLanding } from "@/features/org/hooks/use-pending-join-landing";
import { registerManageTeamsTab } from "@/features/shell/manage-teams-virtual-tab";
import { registerWorkspacesTab } from "@/features/org/workspaces-virtual-tab";
import { registerMessageTab } from "@/features/weft/components/inbox-message-dashboard";
import { NoTeamTabHost } from "@/features/shell/components/no-team-tab-host";
import { Workbench } from "@/features/shell/components/workbench";
import { registerDashboardsTab } from "@/features/dashboard-publishing/dashboards-virtual-tab";
import { registerMachinesTab } from "@/features/shell/machines-virtual-tab";
import { registerRoutinesTab } from "@/features/routines/routines-virtual-tab";
import { registerAgentsTab } from "@/features/agents/agents-virtual-tab";
import { registerOpenDiffTab } from "@/features/git-sync/lib/open-diff";
import { registerScratchSpaceTab } from "@/features/scratch-space/scratch-space-virtual-tab";
import { registerSourceReposTab } from "@/features/dashboard/source-repos-virtual-tab";
import { AppShell } from "@/features/shell/components/app-shell";
import { WatchedSplash } from "@/features/shell/components/boot-wait";
import { useCurrentUser } from "@/shared/lib/get-current-user";
import { registerSettingsTab } from "@/features/settings/settings-virtual-tab";
import { SETTINGS_VIRTUAL_PATH } from "@/features/settings/lib/open-settings-tab";
import { DependencyInstallView } from "@/features/deps/views/dependency-install-view";
import { getDependencyStatus, type DependencyStatus } from "@/tauri/commands";
import { computeDepsReady, UNAVAILABLE_DEP_STATUS } from "@/app/deps-gate";
import { startUpdateCheck } from "@/app/update-check";
import { DEFAULT_AGENT_CLI } from "@/shared/lib/default-agent";

registerMachinesTab();
registerDashboardsTab();
registerRoutinesTab();
registerAgentsTab();
registerScratchSpaceTab();
registerOpenDiffTab();
registerSourceReposTab();
registerSettingsTab();
registerManageTeamsTab();
registerWorkspacesTab();
registerMessageTab();

if (import.meta.env.MODE === "webdriver") {
  void import("./e2e-configuration-control").then(({ registerConfigurationE2eControl }) =>
    registerConfigurationE2eControl(),
  );
  void import("./e2e-activity-control").then(({ registerActivityE2eControl }) =>
    registerActivityE2eControl(),
  );
  void import("./e2e-cli-workspace-control").then(({ registerCliWorkspaceE2eControl }) =>
    registerCliWorkspaceE2eControl(),
  );
  void import("./e2e-terminal-webgl-control").then(({ registerTerminalWebglE2eControl }) =>
    registerTerminalWebglE2eControl(),
  );
  void import("./e2e-org-team-control").then(({ registerOrgTeamE2eControl }) =>
    registerOrgTeamE2eControl(),
  );
  void import("./e2e-weft-control").then(({ registerWeftE2eControl }) => registerWeftE2eControl());
  void import("./e2e-content-nav-control").then(({ registerContentNavE2eControl }) =>
    registerContentNavE2eControl(),
  );
  void import("./e2e-close-reopen-control").then(({ registerCloseReopenE2eControl }) =>
    registerCloseReopenE2eControl(),
  );
  void import("./e2e-terminal-layout-control").then(({ registerTerminalLayoutE2eControl }) =>
    registerTerminalLayoutE2eControl(),
  );
}

// SW-FX-75: the loading splash is gated on several async boot flags
// (`initialized`, `authInitialized`, `stateLoaded`, `dependenciesReady`). Each
// fails open on an *exception* but none on a *hang* — a never-settling await
// anywhere in a gate owner strands the splash forever (the production boot
// hang). This is the last-line watchdog: at a generous deadline it force-opens
// any still-stuck gate and logs exactly which one (the diagnostic beacon).
// Pure `setTimeout`, off the render thread; only fires on a genuine wedge, so
// it never slows a healthy (even slow) boot.
const BOOT_GATE_WATCHDOG_MS = 12_000;

function App() {
  // SW-FX-97: keep the app document pinned — no focus-reveal / scroll-into-view
  // may displace the shell (black band at the bottom). Enforced app-wide in ONE
  // place rather than per scroll-into-view caller.
  useShellScrollContainment();
  const loadState = useConfigStore((s) => s.loadState);
  const initialized = useConfigStore((s) => s.initialized);
  const stateLoaded = useViewerStore((s) => s.stateLoaded);
  const orgCount = useOrgStore((s) => s.orgs.length);
  const activeOrgSlug = useOrgStore((s) => s.activeOrgSlug);
  const activeTeamName = useOrgStore((s) => s.activeTeamName);

  const activeTeamKey = useMemo(
    () => (activeOrgSlug !== null ? buildTeamKey(activeOrgSlug, activeTeamName) : null),
    [activeOrgSlug, activeTeamName],
  );
  const settingsPanelOpen = useUiStore((s) => s.settingsPanelOpen);
  // SW-UX (app-flow redesign): Settings is now the `virtual://settings` TAB, so the
  // legacy `settingsPanelOpen` flag is DERIVED — it means "the settings tab is the
  // active tab in the active team". This effect is its sole in-app writer, kept in
  // sync with the active tab; the consumers that gate on it (shortcut dispatch, CLI
  // chrome, menu state) keep working unchanged. Deps include `settingsPanelOpen` so
  // a stray write (e.g. a CLOSED_SIBLINGS clear when opening a switcher) self-corrects.
  const activeSettingsTabPath = useViewerStore((s) =>
    activeTeamKey ? s.activeTab[activeTeamKey] : undefined,
  );
  useEffect(() => {
    const shouldBeOpen = activeSettingsTabPath === SETTINGS_VIRTUAL_PATH;
    if (settingsPanelOpen !== shouldBeOpen) {
      useUiStore.setState({ settingsPanelOpen: shouldBeOpen });
    }
  }, [activeSettingsTabPath, settingsPanelOpen]);
  const [dependenciesReady, setDependenciesReady] = useState<boolean | null>(null);
  const [depStatus, setDepStatus] = useState<DependencyStatus | null>(null);
  const depsCheckedRef = useRef(false);
  const depsReadyRef = useRef(dependenciesReady);

  const authInitialized = useLocalProfileStore((s) => s.initialized);
  const profileError = useLocalProfileStore((s) => s.error);
  const initializeAuth = useLocalProfileStore((s) => s.initialize);
  const entered = useLocalProfileStore((s) => s.status?.active != null);

  const handleDepsComplete = useCallback(() => setDependenciesReady(true), []);

  // Registry-driven global keyboard handler. Replaces the inline
  // Cmd+, / switcher (App.tsx) and Cmd+\ / ⌘J (TeamWorkspaceView)
  // branches. Reads user overrides from `useShortcutsStore`; matches
  // against the canonical registry (`allShortcutActions`).
  useEffect(() => installUserNavigationCapture(document), []);
  useShortcutHandler();

  // Listen for MCP action events from the Rust backend
  useMcpActions();

  // SW-RP-1: keep the native View → Panels submenu in sync with the registry.
  useRightPaneMenuSync();
  // Slot dashboards' declared commands join the shortcut registry.
  useDashboardCommandsSync();

  // SW-GS-2b (B): the agent→UI chat channel — append `post_agent_message`
  // pushes to the per-session transcript store.
  useAgentMessageListener();

  // SW-GS-10 (Plumbing 1): the structured conflict-status channel —
  // `gs_report_status` pushes drive the session-keyed conflict store (✓/⚠
  // checklist + resolved-vs-ours redline + publish gate).

  // SW-SC-14: serve `render_canvas` MCP requests — render a canvas in-app and
  // hand the PNG sidecar path back over the bridge.
  useCanvasRenderBridge();
  useRenderedDocumentBridge();

  // SW-AW-4 (B/D): the managed task-session bridge — `ask_user` (blocking
  // question) + `signal_done` (completion) drive the sessionId-keyed managed
  // state store; the answer leg resolves the Rust oneshot by correlationId.
  useManagedSessionBridge();

  // Listen for terminal status events from the HTTP API (Claude Code Stop hook)
  useTerminalStatusEvents();
  // A terminal's display lives as long as its tab.
  useTerminalDisplayGc();
  // …and exists from the moment the tab does, whether or not it is on screen.
  useTerminalDisplayWarm();
  // The tab the user is looking at: its crash/error dot settles once seen.
  useMarkActiveSessionSeen();

  // PT-8: keep the Rust warm terminal pool sized to settings (fills at launch).
  useWarmPool();

  // RF-perf-14d: size the background worktree-setup concurrency gate to the
  // machine + the user setting (applies live; the net-new caller of the cap).
  useBackgroundSetupCap();

  // SW-DX-17d: push the user-owned background-agent limit to Rust (applies live).
  useBackgroundAgentLimit();

  // SW-WA-E11 (AC-1/AC-2): fire a native OS notification when a needs-you (YOU-OWE)
  // item arrives while the app window is not focused — quiet while focused. Passive:
  // reuses the board's attention read; no polling, deduped one-per-arrival.
  useNeedsYouNotifications();

  // The dock badge counts terminal tabs that need you (a permission prompt, a
  // question, a crash); in the background each new one also raises an OS
  // notification, and returning to the app lands on that tab.
  useAttentionAlerts();

  // SW-AU-34 (AC-5): push the quit-on-close preference to Rust so the window-close
  // handler decides hide-to-background vs. quit live, and surface the one-time
  // "still running in the background" explainer the first time a close backgrounds.
  useQuitOnCloseSetting();
  useTerminalServerMenuBarSetting();
  useBackgroundModeNotification();

  // Discover existing orgs on disk once GitHub auth is available.
  const { discoveryComplete } = useOrgDiscovery();

  // SW-OC-16: discovery places the bundled CodeSense docs context repo; the very first
  // launch lands in it with its README open, later launches reopen the last one used.
  // SW-FU-B3: past the entry gate with no user (an expired session at boot, or
  // an old auth file with no user id) there is nothing to discover: Login.
  const currentUser = useCurrentUser();
  // SW-FU-B6: a join started before a guest's sign-in reload reopens once there
  // is a workspace to show it in (with none, the join form is shown on its own below).
  usePendingJoinLanding(discoveryComplete && currentUser !== null && orgCount > 0);

  // Synchronize the viewer with the active workspace (W.1).
  useWorkspaceViewerSync();
  // A team with nothing open shows its landing page (CodeSense Docs → README.html).
  useTeamLandingPage();
  // The agents pane's saved arrow filter goes once nothing can bring its workspace back, even
  // while the pane is not on screen.
  useStaleArrowFilterCleanup();

  // SW-XS-10: the one consumer of `agent-prepare-progress` — a launch's Step 2
  // drives its workspace's setup line, an explicit operation the pill's chip.
  useContextProgressListener();

  // SW-AM-9: the ONE subscriber to the agent-roster lifecycle events — warms the
  // app-root roster store and keeps it live, so the agents list reads
  // one source of truth (no drift, warm on cold open).
  useAgentRosterListener();
  // An agent's tabs follow its name when it is renamed.
  useAgentTabTitles();
  // A watched agent never holds a background-limit slot.
  useAttendedAgentRelease();

  // An agent's `open_workspace` shows the new session; a focused open brings it forward.
  useOpenWorkspaceReveal();

  // SW-WU-9: the ONE subscriber to the worktree-roster lifecycle events (changed +
  // reaped) — warms the app-root worktree store and keeps it live, so the Worktrees
  // pane is realtime with no manual Refresh (sibling of the agent roster above).
  useWorktreeRosterListener();

  // SLW-CO-8 (LD-B): route per-repo dev-env install completions into the
  // epic-assembly batch aggregator so the epic overlay rides the whole batch.
  useEpicAssemblyListener();

  // SW-LN-B51: a repo added mid-session gets its context path, and open worktree watches follow.
  useResolvedRepositoryPathsSync();

  // SW-FX-9 Part 2A: a content materialize is watcher-suppressed, so re-scan
  // the affected pipelines on wave completion — otherwise a newly-materialized
  // supporting_docs/status change waits for the next unrelated scan.
  useSyncWaveRescan();

  // SLW-NC-5: live-refresh the source-repo composite overlay — on an external
  // layer-store change (install / explicit publish / agent / raw write) for the
  // active team, invalidate the memoized composite so the nav overlay + shadow
  // badge update without a restart. Debounced, non-blocking, active-team-scoped.
  useCompositeOverlayRefresh();

  // SW-GT-3: hydrate the status-taxonomy snapshot for the active team, app-wide, so
  // every switcher/board surface derives from the team's states (not the compile-time
  // seed). Store is seeded at import → this never gates paint; it swaps in a custom
  // taxonomy once the off-thread load settles, and re-loads on team switch (AC-3).
  useStatusTaxonomy(useActiveTeamKey());

  // SW-DX-3: the ONE app-root listener for the live status stream — funnels
  // context-file-change / agent-done / agent-message into the dashboard-stream
  // hub, powering `onStatus` subscriptions + the live/stale badge. Coalesced,
  // non-blocking, tears down cleanly on unmount.
  useDashboardStream();

  // SW-SR-9: app-root bridge that re-assembles an epic's worktrees after an
  // agent edits its `repos:` via the `set_epic_repos` MCP tool (emit → the same
  // fire-and-forget assemble the UI uses; never blocks the tool call).
  useEpicReposReassembly();

  // App-root bridge for registry edits and team switches made outside the UI
  // (an agent over MCP): reload the changed org, follow an agent's team switch.
  useOrgRegistrySync();

  // SW-WS-1: FE bridge for the Rust-side `move_workspace` pipeline. Pulls
  // open intents on init (so a restart that happens mid-move rehydrates the
  // moving-banner immediately) and listens for `move-workspace-intent-*`
  // events while running.
  useMoveWorkspaceSync();

  // The notes follow the org's records projection, so an agent's change shows at once.
  useNotesProjection();
  useAgentDefinitions();
  useConfigurationLive();
  usePrefsLive();

  // SF-8 (AC-1): keep the Tauri window title synced with active org/team
  // chrome. Mounted alongside `useWorkspaceViewerSync` so a switch click
  // updates the title in the same animation frame as the rest of the
  // chrome (org/team switcher, status bar, active-team pill).
  useWindowTitle();

  // Source repo attachment — silent record + track prompt (W.2).
  // T.6: post-`selectedRepoPath` the input is the active workspace's
  // **synthetic key** (NOT `workspace.sourceRepoPath`, the M.2 on-disk
  // clone path). Pre-T.6 the hook saw `selectedRepoPath` which was a
  // synthetic key for workspace-bar selections — `isSyntheticRepoPath`
  // short-circuited inside the hook and skipped the 3-call IPC flow.
  // Feeding `sourceRepoPath` (a real filesystem path) bypassed that
  // short-circuit and fired `resolveSourceRepoAttachment` +
  // `readSourceRepoOriginUrl` + `parseGitRemote` on every workspace
  // switch. Feeding the synthetic key restores the pre-T.6 short-circuit.
  const { repoPath: activeWorkspaceRepoPath } = useActiveWorkspaceContext();
  useSourceRepoAttachment(activeWorkspaceRepoPath);

  useEffect(() => {
    depsReadyRef.current = dependenciesReady;
  }, [dependenciesReady]);

  // Remote Machines' last-known census and last-reached times.
  useEffect(() => startMachineMemory(), []);

  useEffect(() => {
    // Kick off org-store rehydrate OUTSIDE the `await initLogging()`
    // chain so it fires even if logging init fails. F-25 removed the
    // currentUser projection (and its two-gate race), so the rehydrate
    // no longer needs to serialize against an auth-projection hook —
    // it only needs to restore the persisted session block before any
    // setter writes live state on top of it.
    useOrgStore
      .getState()
      .loadPersistedState()
      .catch(() => {
        // Failures are logged inside loadPersistedState.
      });

    // Shortcut overrides load in parallel with org-store. No
    // ordering constraint — the keyboard handler reads whatever is
    // in the store at match time; pre-rehydrate it uses the
    // registry's defaults, post-rehydrate it uses the user's overrides.
    // `loadPersistedState` logs its own failures; this tail-catch
    // guards sync throws before the promise chain.
    useShortcutsStore
      .getState()
      .loadPersistedState()
      .catch((e) => {
        logger.warn("App", `shortcuts loadPersistedState failed — ${e}`);
      });

    (async () => {
      await initLogging();
      loadState();
      initializeAuth();
    })().catch(() => {});
  }, [loadState, initializeAuth]);

  // SW-FX-75: boot-gate watchdog. If any splash-blocking gate is still stuck
  // after the deadline (a never-settling await in its owner), force it open and
  // log which gate(s) tripped — turning an infinite hang into a logged,
  // recoverable fail-open. `discoveryComplete` is bounded inside
  // `useOrgDiscovery` itself; this covers the store- and dep-owned gates. Reads
  // live state (Zustand `getState`, `depsReadyRef`) so it never acts on a stale
  // render closure. Runs once; the timer is cleared on unmount.
  useEffect(() => {
    const timer = setTimeout(() => {
      const stuck: string[] = [];
      if (!useConfigStore.getState().initialized) {
        stuck.push("config.initialized");
        useConfigStore.setState({ initialized: true });
      }

      if (!useViewerStore.getState().stateLoaded) {
        stuck.push("viewer.stateLoaded");
        useViewerStore.setState({ stateLoaded: true });
      }
      if (depsReadyRef.current === null) {
        // Fail OPEN, like the other three gates — never fail-closed to the
        // dependency-guidance view on mere slowness. `getDependencyStatus()` is
        // a fast local probe that realistically can't hang 12s; if it somehow
        // does, proceeding (rather than pinning the "unavailable" view) matches
        // the never-wedge intent, and the real probe's later result still
        // corrects downward if deps are genuinely missing (point-of-use paths
        // re-check and surface their own errors).
        stuck.push("dependenciesReady");
        setDependenciesReady(true);
      }
      if (stuck.length > 0) {
        logger.warn(
          "boot",
          `boot-gate watchdog tripped after ${BOOT_GATE_WATCHDOG_MS}ms — forced open: ${stuck.join(", ")}`,
        );
      }
    }, BOOT_GATE_WATCHDOG_MS);
    return () => clearTimeout(timer);
  }, []);

  // Intercept external link clicks — open in system browser via Tauri opener
  useEffect(() => {
    const handler = (e: MouseEvent) => {
      const anchor = (e.target as HTMLElement).closest("a[href]") as HTMLAnchorElement | null;
      if (!anchor) return;
      const href = anchor.getAttribute("href");
      if (href && (href.startsWith("https://") || href.startsWith("http://"))) {
        e.preventDefault();
        e.stopPropagation();
        openUrl(href).catch(() => {});
      }
    };
    document.addEventListener("click", handler, true);
    return () => document.removeEventListener("click", handler, true);
  }, []);

  // Check dependency status once after auth completes
  useEffect(() => {
    if (!entered || depsCheckedRef.current) return;
    depsCheckedRef.current = true;
    getDependencyStatus()
      .then((status) => {
        setDepStatus(status);
        setDependenciesReady(computeDepsReady(status, DEFAULT_AGENT_CLI));
      })
      .catch(() => {
        setDepStatus(UNAVAILABLE_DEP_STATUS);
        setDependenciesReady(false);
      });
  }, [entered]);

  // Listen for dep-status-changed events from Rust file watcher
  // SW-WC-1: prime the workspace-archive mirror from disk and subscribe to
  // `codesense:workspace-archive-changed` so every pill re-renders at the
  // same tick as an archive/unarchive. One-shot per app lifetime.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | null = null;
    useWorkspaceArchiveStore
      .getState()
      .initialize()
      .then((fn) => {
        if (cancelled) safeUnlisten(fn);
        else unlisten = fn;
      })
      .catch((error) => {
        logger.warn("App", `workspace-archive initialize failed — ${error}`);
      });
    return () => {
      cancelled = true;
      if (unlisten) safeUnlisten(unlisten);
    };
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    listen("dep-status-changed", () => {
      // SW-OB-12: skip while the git-guidance gate is up (dependenciesReady ===
      // false → DependencyInstallView mounted); that view drives its own
      // detection re-check. Proceed when ready (true) AND on the splash (null) —
      // the latter is the initial background claude resolve completing, which the
      // old `!== true` guard swallowed (would strand the app on the splash).
      if (depsReadyRef.current === false) return;

      depsCheckedRef.current = false;
      getDependencyStatus()
        .then((status) => {
          setDepStatus(status);
          setDependenciesReady(computeDepsReady(status, DEFAULT_AGENT_CLI));
        })
        .catch(() => {
          setDepStatus(UNAVAILABLE_DEP_STATUS);
          setDependenciesReady(false);
        });
    })
      .then((fn) => {
        if (cancelled) safeUnlisten(fn);
        else unlisten = fn;
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
      if (unlisten) safeUnlisten(unlisten);
    };
  }, []);

  // Check for updates after auth is complete (Toaster must be mounted). Internal builds
  // only: in the beta channels updates follow the license check (SW-PB-13 AC-2).
  useEffect(() => {
    if (initialized && entered) void startUpdateCheck();
  }, [initialized, entered]);

  // Listen for macOS menu-item clicks. One event, one listener, one
  // registry lookup — see C.16's design note on why the Rust side is
  // a dumb forwarder.
  //
  // The `cancelled` flag is load-bearing under React StrictMode (dev):
  // the first mount's cleanup runs BEFORE `listen(...).then(...)`
  // resolves, so the naive `let unlisten: …; listen().then(fn =>
  // unlisten = fn); return () => unlisten?.();` pattern leaks the
  // first mount's listener. That leak is invisible for idempotent
  // events (auth-expired, etc.) but catastrophic for menu:action:
  // each toggle would fire twice and net-zero (cancel itself out),
  // which is exactly what was observed for Switch Repo / Toggle
  // Terminal / Toggle Scratch Note. The fix ensures late-arriving
  // promise resolutions unlisten immediately when the effect is
  // already cleaned up.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | null = null;
    // dispatchMenuAction applies the same scope/Settings/auth gate as the
    // keyboard handler and routes panel toggles — see menu-action-dispatch.ts.
    listen<string>("menu:action", (e) => dispatchMenuAction(e.payload))
      .then((fn) => {
        if (cancelled) {
          safeUnlisten(fn);
        } else {
          unlisten = fn;
        }
      })
      .catch((err) => {
        logger.warn("App", `menu:action listen failed — ${err}`);
      });
    return () => {
      cancelled = true;
      if (unlisten) safeUnlisten(unlisten);
    };
  }, []);

  // SLW-WDE-3: route the worktree watcher's `worktree-file-change` events to the
  // open editor inset. The Rust watcher (attached per-inset at `ws.cwd`) tags
  // each event with its `wsId`, so this listener just bumps the per-workspace
  // signal — no reverse cwd→ws lookup — and `useWorktreeFileEditor` reacts when
  // the changed path is the file it has open (clean → reload, dirty → warn). The
  // `cancelled` flag mirrors the menu:action idiom (non-idempotent event: a
  // leaked StrictMode listener would double-fire reloads).
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | null = null;
    listen<{ wsId: string; changedPath: string }>("worktree-file-change", (e) => {
      const { wsId, changedPath } = e.payload;
      useViewerStore.getState().bumpWorktreeChangedPath(wsId, changedPath);
    })
      .then((fn) => {
        if (cancelled) safeUnlisten(fn);
        else unlisten = fn;
      })
      .catch((err) => {
        logger.warn("App", `worktree-file-change listen failed — ${err}`);
      });
    return () => {
      cancelled = true;
      if (unlisten) safeUnlisten(unlisten);
    };
  }, []);

  useEffect(() => {
    const subscription = listen("local-profiles-changed", () => void initializeAuth());
    return () => {
      void subscription.then((unlisten) => unlisten());
    };
  }, [initializeAuth]);

  // DESIGN-remote-phase-2.md §0 / B-3 — register the one-shot fsmonitor-nudge
  // handler. The hook calls this ONCE per (machineId, handle) when its first
  // tracked-paths probe lands over `LARGE_REPO_THRESHOLD` (5000); the banner
  // teaches `git config core.fsmonitor true` so `git status` on the remote
  // doesn't walk the tree. Dismiss is the toast's default close.
  useEffect(() => {
    void import("@/features/viewer/lib/use-remote-cadence").then(
      ({ registerFsmonitorNudgeHandler }) => {
        registerFsmonitorNudgeHandler((_machineId, _handle, count) => {
          import("sonner").then(({ toast }) => {
            toast(`Large remote repo (${count.toLocaleString()} tracked files)`, {
              description:
                "Enable git fsmonitor on the remote machine for faster diff refresh: `git config core.fsmonitor true` (Git ≥ 2.37).",
              duration: 12000,
            });
          });
        });
      },
    );
    return () => {
      void import("@/features/viewer/lib/use-remote-cadence").then(
        ({ registerFsmonitorNudgeHandler }) => {
          registerFsmonitorNudgeHandler(null);
        },
      );
    };
  }, []);

  // SW-OB-6: removed the `claude-auth-expired` listener, the Claude auth
  // background poll, and the `heartbeatJira` token heartbeat. Claude is no
  // longer a login provider (CLI `claude` login handles auth), and Jira left
  // the gate — so neither runs a background timer here anymore.

  // Global keyboard shortcuts
  useEffect(() => {
    // Global shortcut listener. Meta-chorded shortcuts (Cmd+,,
    // Cmd+Shift+P / O / T) are deliberately NOT gated by a
    // "typing target" guard — the Meta modifier makes them
    // unambiguous with ordinary typing, so the guard would just
    // block users from pivoting to a switcher while their cursor
    // is in the Settings search input (or any other input).
    const handler = (e: KeyboardEvent) => {
      // Cmd+Shift+P/O/T (switchers) and Cmd+, (Settings) are dispatched
      // by `useShortcutHandler` via the canonical action registry —
      // they were moved there so users can rebind them in Settings →
      // Shortcuts. Only non-configurable shortcuts remain inline below.
      //
      // Cmd+\ (terminal toggle) and Cmd+J (scratch note) are ALSO in
      // the registry; their old homes in `TeamWorkspaceView`'s listener
      // have been removed too.
      //
      // Cmd+W: close active tab or window
      // F-15: Skip during auth overlay — main app is hidden

      // A row popup of the Agents pane (the quick view, the inbox chat) closes itself on the same
      // ⌘W chord (`isCmdW`): nothing behind it closes.
      if (isCmdW(e)) {
        if (isRowPopupFocused()) return;
        // SW-UX (app-flow redesign): Settings is a virtual TAB now, so ⌘W closes it
        // through the normal file-tab path below (branch b) — no special-case.
        // T.6 AC-4.4: locked Cmd+W decision tree.
        // (a) terminal tab focused → close that terminal tab
        // (b) elif file tab open → close active file tab
        // (c) elif workspace exists in active team's bar → focus the
        //     team's first root (jumpToRootWorkspace)
        // (d) else → Tauri-default-close-window
        // The worktree code editor closes its own tabs on ⌘W (with its unsaved-
        // changes guard); its handler runs after this capture-phase one.
        if ((document.activeElement as HTMLElement | null)?.closest?.("[data-worktree-editor]")) {
          return;
        }
        const state = useViewerStore.getState();
        const teamKey = getActiveTeamKey();

        // SW-FU-B7: with no team, the management tabs live in the no-team tab
        // host — ⌘W closes its active tab like any other tab.
        if (!teamKeyHasTeam(teamKey)) {
          const ui = useUiStore.getState();
          if (ui.noTeamActiveTab) {
            e.preventDefault();
            ui.closeNoTeamTab(ui.noTeamActiveTab);
            return;
          }
        }

        // A run output laid over the terminals closes itself before anything underneath.
        if (isRunOutputFocused()) {
          e.preventDefault();
          closeAgentResult();
          return;
        }

        // (a) Terminal tab focused?
        if (isTerminalFocused()) {
          e.preventDefault();
          // The default close: Hide when running, End when idle.
          void closeActiveTerminalTab(teamKey);
          return;
        }

        // (b) File tab open?
        // SW-FX-38: gate the close on `activeTab` alone — `repoPath` is never
        // a precondition (it's best-effort for the post-close reveal). The
        // "close the active file tab" contract lives in `closeActiveFileTab`,
        // the single source of truth all close affordances route through.
        const activeTab = state.activeTab[teamKey];
        if (activeTab) {
          e.preventDefault();
          closeActiveFileTab(activeTab);
          return;
        }

        // (c) Workspace exists in active team's bar → focus first root.
        const teamWorkspaces = state.workspaces[teamKey] ?? [];
        if (teamWorkspaces.length > 0) {
          const activeWsId = state.activeWorkspace[teamKey];
          const firstRootId = firstRootIdForTeam(teamKey, state.workspaces);
          if (firstRootId !== ROOT_NO_REPOS_SENTINEL && firstRootId !== activeWsId) {
            e.preventDefault();
            state.switchWorkspace(teamKey, firstRootId);
            return;
          }
        }

        // (d) Nothing to close — let Tauri close the window.
      }
    };
    document.addEventListener("keydown", handler, { capture: true });
    return () => document.removeEventListener("keydown", handler, { capture: true });
  }, []);

  // Restore viewer state after config loads, auto-detect tools on first run.
  useEffect(() => {
    if (initialized) {
      useViewerStore
        .getState()
        .loadPersistedState()
        .then(async () => {
          try {
            const { runPipelineScopeRecovery } =
              await import("@/shared/lib/pipeline-graph/run-scope-recovery");
            await runPipelineScopeRecovery({
              repos: [
                ...useConfigStore.getState().repos,
                ...Object.values(useOrgStore.getState().workspaceRepos),
              ],
              pipelineScope: useViewerStore.getState().pipelineScope,
              setPipelineScope: useViewerStore.getState().setPipelineScope,
            });
          } catch {
            // Recovery is best-effort; any failure is logged inside the runner.
          }
          useViewerStore.getState().setScopeRecoveryComplete(true);

          const settings = useViewerStore.getState().settings;
          if (!settings.terminal || !settings.editor) {
            try {
              const { detectTools } = await import("@/tauri/commands");
              const tools = await detectTools();
              useViewerStore.getState().updateSettings({
                ...(!settings.terminal && { terminal: tools.default_terminal }),
                ...(!settings.editor && { editor: tools.default_editor }),
              });
            } catch {
              // Detection failed — user can set manually in settings
            }
          }
          // Show window after state is fully loaded — prevents picker flash
          getCurrentWindow()
            .show()
            .catch(() => {});
        });
    }
  }, [initialized]);

  // Flush any pending debounced persist before the window closes.
  // `saveAppState` is async Tauri IPC — `beforeunload` can't await it,
  // so we use Tauri's `onCloseRequested` which supports async work.
  // Uses the cancelled-flag pattern (see menu:action listener above) to
  // handle React StrictMode's mount-unmount-remount cycle.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | null = null;
    getCurrentWindow()
      .onCloseRequested(async (event) => {
        event.preventDefault();
        try {
          await flushBeforeQuit();
        } finally {
          // destroy() bypasses the event system — close() would re-emit
          // closeRequested, creating an infinite loop.
          getCurrentWindow()
            .destroy()
            .catch(() => {});
        }
      })
      .then((fn) => {
        if (cancelled) {
          safeUnlisten(fn);
        } else {
          unlisten = fn;
        }
      });
    return () => {
      cancelled = true;
      if (unlisten) safeUnlisten(unlisten);
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    let off: (() => void) | undefined;
    void listen("app:quit-flush", async () => {
      await flushBeforeQuit();
      await emit("app:quit-flush-ack");
    }).then((unlisten) => {
      if (cancelled) safeUnlisten(unlisten);
      else off = unlisten;
    });
    return () => {
      cancelled = true;
      if (off) safeUnlisten(off);
    };
  }, []);

  // The terminal hosts own the terminal layout: restoring it is subscribing to
  // their changes and fetching their snapshots. It never opens a terminal.
  useEffect(() => {
    if (!initialized) return;
    let teardown: (() => void) | null = null;
    let cancelled = false;
    hydrateTerminalLayout()
      .then((stop) => {
        if (cancelled) stop();
        else teardown = stop;
      })
      .catch((e) => logger.warn("terminal", `terminal layout restore failed: ${e}`));
    return () => {
      cancelled = true;
      teardown?.();
    };
  }, [initialized]);

  useEffect(() => {
    if (!initialized) return;

    let cancelled = false;
    let unlisten: (() => void) | null = null;

    async function setup() {
      // Bootstrap the Rust watcher instance with no directories — workspace-sync
      // owns which directories are watched via addWatch/removeWatch.
      try {
        await startWatching([]);
      } catch (e) {
        console.error("Failed to start watcher:", e);
      }

      // SW-WL-3 (AC-2): route the WorkspaceOpenSet open/close transitions to
      // the surface-bound watcher commands. Idempotent registration.
      registerWorkspaceWatchBridge();

      if (cancelled) {
        stopWatching().catch(() => {});
        return;
      }

      // SS-9: the `fs-change` listener + its `verifyDebounce` were retired
      // with sync-store's install-health half. Post-SS-4c, `validate_install`
      // is a no-op returning "auto-synced"; polling it on every fs-change
      // had zero informational value. SW-WL-5 then retired the Rust-side
      // `fs-change` emit (and its `.claude`-specific watch dirs) entirely —
      // Rust no longer emits it. SW-CP-18 retired the `install-file-change`
      // listener with the capture axis. The surviving `context-file-change`
      // listener below drives seamless-sync's watcher → viewer update path.

      // Coalesce burst context-file changes (e.g. git checkout touching 200+
      // files) into one bumpFileRefresh call per repo per window. See NB-5.
      const COALESCE_WINDOW_MS = 300;

      // Same coalescing for context content changes. The side map tracks
      // distinct file paths so per-path bumps preserve F-26's Layer 2
      // fast-path even when multiple files change in the window (SF-3 Bug B).
      const changedPaths = new Map<string, Set<string>>();
      // SW-SY-4 (AC-2): leading-edge throttle, not a trailing debounce. Under a
      // sustained watcher burst (fold move / `git checkout` / agent bulk write,
      // ~28 events/s) a trailing debounce resets every event and NEVER fires
      // until the burst ends — the renderer starves for 0.5–1.5s then stampedes.
      // Leading-edge fires the first change immediately, then coalesces at a
      // bounded ~COALESCE_WINDOW_MS cadence, with a trailing flush so the final
      // burst state always lands. The per-key path Set below accumulates across
      // the window, so each flush carries every path changed since the last one.
      const refreshDebounce = createKeyedDebounce<{ repo_path: string }>(
        COALESCE_WINDOW_MS,
        (key) => {
          const paths = changedPaths.get(key);
          changedPaths.delete(key);
          // SF-3: emit one bump per path so every viewer whose `filePath`
          // doesn't match the bump takes the Layer 2 cache-token-forwarding
          // fast-path (zero `readFile` IPC). `key` is the SW-FX-16-resolved
          // bump identity: the synthetic workspace key
          // (`workspace:{org}:{team}:{owner}/{repo}`) when `context_repo_path`
          // resolved one, else the raw emitted `repo_path`.
          // `bumpFileRefreshForSourceRepo` then fans out a synthetic to sibling
          // synthetics via `(org, team)` (PTS-5 bug 1) or a source path to
          // synthetics via `localPath`, per-path.
          flushRefreshDebounce(key, paths, bumpFileRefreshForSourceRepo, logger);
        },
        { leading: true },
      );

      const unlistenContext = await listen<ContextFileChangePayload>(
        CONTEXT_FILE_CHANGE_EVENT,
        (event) => {
          const { file_path } = event.payload;
          // SW-FX-16: the emitted `repo_path` is the surface's `ws.cwd`, which for
          // a worktree surface matches no `localPath` and bumps a dead key. Route
          // by the surface-independent `context_repo_path` → synthetic key so
          // worktree-surface edits reach the tree/viewer subscription; fall back
          // to `repo_path` when no workspace repo owns the dir (e.g. the
          // `_codesense` internal handler). G-8: a team-level change (no
          // workspace subscriber) names no repo and routes by its content path.
          const bumpKey = bumpKeyForContextFileChange(event.payload);
          if (bumpKey === null) return;
          if (file_path) {
            let set = changedPaths.get(bumpKey);
            if (!set) {
              set = new Set();
              changedPaths.set(bumpKey, set);
            }
            set.add(file_path);
          }
          refreshDebounce.schedule(bumpKey, { repo_path: bumpKey });
        },
      );

      // SW-CP-18: the `install-file-change` listener is retired with the
      // capture / install-handler axis. Captured-file rescans now ride the
      // `context-file-change` cascade (the reverse engine writes into the team
      // content dir, which the context watcher observes); reverse-sync
      // conflicts surface via CP-4's `ConflictRegistry` events, not a
      // capture-conflict toast.

      unlisten = () => {
        safeUnlisten(unlistenContext);
        refreshDebounce.cancelAll();
        changedPaths.clear();
      };
    }

    setup();

    return () => {
      cancelled = true;
      unlisten?.();
      stopWatching().catch(() => {});
    };
  }, [initialized]);

  // Profile recovery must remain reachable even if namespace-dependent loading failed.
  if (authInitialized && (!entered || profileError))
    return (
      <AppShell>
        <AccountSection />
      </AppShell>
    );
  if (!initialized || !stateLoaded || !authInitialized) {
    return (
      <AppShell>
        <WatchedSplash stage="startup" />
      </AppShell>
    );
  }

  // Gate: show splash while dep check is pending
  if (dependenciesReady === null) {
    return (
      <AppShell>
        <WatchedSplash stage="startup" />
      </AppShell>
    );
  }

  // Gate: show dependency install screen if any managed dependency is missing
  if (dependenciesReady === false && depStatus) {
    return (
      <AppShell>
        <DependencyInstallView initialStatus={depStatus} onComplete={handleDepsComplete} />
      </AppShell>
    );
  }

  // SW-OB-6: the Claude-auth entry gates (splash-while-checking + block-until-
  // Claude-authenticated) were removed. Claude is no longer a login provider —
  // the `claude` CLI handles auth at the point of use.

  // Gate: org discovery still running — show splash, not the picker. SW-FU-B3:
  // this gate and the next wait on the backend, so their splash is watched: it
  // names the wait and offers Try again / Open logs after a deadline.
  if (!discoveryComplete) {
    return (
      <AppShell>
        <WatchedSplash stage="discovery" />
      </AppShell>
    );
  }

  // SW-OC-16: every launch has the bundled CodeSense docs context repo, so this only
  // shows if placing it failed — and even then it is no dead end: connect or create one.
  if (orgCount === 0) {
    return (
      <AppShell>
        <NoTeamTabHost>
          <div data-testid="no-org-setup" className="flex-1 min-h-0 overflow-y-auto">
            <OrgPicker canDismiss={false} />
          </div>
        </NoTeamTabHost>
      </AppShell>
    );
  }

  return (
    <>
      {/* SW-AGR-6: every agent tab's timers, shown or not. */}
      <SessionTimersLoader />
      <AppShell>
        {/* The window's main row: the team views, the CLI and the right pane (the shell's
            workbench). It holds the team-scoped empty-state ladder too. */}
        {stateLoaded && <Workbench />}
      </AppShell>
      {/* SW-EW-33: QuickWorkspaceDialog (Cmd+Shift+P) retired — the Cmd+K
        workspace palette (re-keyed SW-FX-49) is the single open/switch surface. */}
      {/* SW-HB-6: the ⌘K command palette. Mounted app-globally
          so its `scope: "global"` shortcut opens it from the Home Base hub as
          well as inside a team view. */}
      <CommandPaletteMount />
      {/* Custom right panes (`<space>/panes/<id>.html`): one app-wide owner. */}
      <PanePanelsMount />
      <WorkspaceWhereDialog />
    </>
  );
}

export default App;
