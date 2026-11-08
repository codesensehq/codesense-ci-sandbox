// SPDX-License-Identifier: MIT
import { usePanelRegistry } from "@/features/shell/right-pane/panel-registry";
import { selectRenderableLayout } from "@/features/shell/right-pane/right-pane-layout";
import { runDashboardCommand } from "@/features/dashboard/lib/dashboard-commands";
import { dashboardRefKey } from "@/features/dashboard/lib/dashboard-ref";
import { hasLiveFrames } from "@/features/dashboard/lib/frame-lifecycle";
import { resolveSlot, revealSlot } from "@/features/shell/slots/reveal-slot";
import { isRunningAgent } from "@/features/viewer/lib/hand-agents";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { beginContextNavigation } from "@/shared/lib/context-navigation";
import { create } from "zustand";
import { isMac, isReservedForDashboardCommand } from "./keystroke";
import { requestNewWorkspaceTerminal } from "@/features/viewer/lib/new-workspace-terminal";
import {
  closeAgentInboxRail,
  openAgentInboxRail,
  useAgentInboxRailStore,
} from "@/features/viewer/stores/agent-inbox-rail-store";
import { toast } from "sonner";
import { openAppSettings } from "@/features/settings/lib/open-settings";
import { canExportToPdf, exportViewerToPdf } from "@/features/viewer/lib/export-pdf";
import {
  toggleNavCollapsed,
  toggleRightPaneWithFocus,
  revealOrToggleTerminal,
} from "@/features/viewer/lib/focus-cascade";
import { useConfigStore } from "@/shared/stores/config-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { parseRootWorkspaceId, NO_TEAM_SENTINEL } from "@/shared/lib/team-key";
import { useUiStore } from "@/shared/stores/ui-store";
import { visibleTeams } from "@/shared/lib/user-teams";
import {
  useViewerStore,
  expandedGridWorkspaceId,
  selectActiveTerminalTab,
  workspacePanedTabIds,
} from "@/shared/stores/viewer-store";
import { getActiveWorkspaceContext } from "@/shared/hooks/use-active-workspace-context";
import {
  contextBack,
  contextForward,
  switchContext,
  recordContextChange,
} from "@/features/viewer/lib/context-switch";
import { getKindCapabilities } from "@/shared/lib/workspace-kind-capabilities";
import { focusAdjacentPane, type FocusDirection } from "@/features/viewer/lib/directional-focus";
import {
  focusActiveWorkspaceTerminal,
  navigateEpicRail,
  revealCollapsedTerminal,
} from "@/features/viewer/lib/focus-cascade";
import { flashBlockedNavForWorkspace } from "@/features/viewer/lib/blocked-nav-cue";
import { endWorkspace, hideWorkspace } from "@/features/viewer/lib/terminal-close";
import {
  clearFocusedTerminal,
  findInFocusedTerminal,
  jumpToTerminalTab,
  splitTerminal,
  newTerminalTab,
  isFocusInOtherEditor,
  renameFocusedTab,
  reopenClosedTab,
  stepExpand,
  jumpToWhatNeedsYou,
} from "@/features/viewer/lib/terminal-tab-keys";

/**
 * Canonical action registry for every configurable keyboard shortcut +
 * menu-mirrored action. Single source of truth consumed by:
 *   - the registry-driven keyboard handler (`useShortcutHandler`)
 *   - the Settings → Shortcuts UI
 *   - the Rust menu's frontend dispatcher (via `menu:action` events)
 *   - all tooltip hints that display the current keystroke
 *
 * `run()` is called imperatively (outside React render). Each action
 * mirrors the behavior of the inline `keydown` branch it replaces,
 * including focus side-effects (via the `focus-cascade` util) and
 * precondition guards (e.g., Switch Workspace requires 2+ workspaces).
 *
 * Every action acts on the active team (`getActiveWorkspaceContext`): the CLI, the bar, the nav,
 * the document and the right pane all show that one team. Workspace-scoped actions
 * (`scope: "workspace"`) no-op when the active team has no focused workspace.
 */

/** Prefer the focused pane, then the visible overlay, then the first expanded section. */
function activeRightPanePanel(teamKey: string): string | undefined {
  const viewer = useViewerStore.getState();
  if (!viewer.rightPaneOpen) return;
  const registered = usePanelRegistry.getState().panels;
  const panels = selectRenderableLayout(viewer.rightPaneLayout, (id) => registered[id] != null)
    .sections.filter((s) => !s.collapsed)
    .map((s) => s.group.activePanelId);
  return (
    panels.find((panelId) =>
      hasLiveFrames(
        (frame) =>
          frame.slotId === "right-pane" &&
          frame.panelId === panelId &&
          frame.element === document.activeElement,
      ),
    ) ??
    panels.find((panelId) => panelId === viewer.maximizedPanel[teamKey]) ??
    panels[0]
  );
}

/** Positional workspace jumps (`Cmd+1..9`). One source of truth for the count so
 *  the generated `viewer.jump-workspace-N` actions and the nav-axis grouping that
 *  lists them can never disagree. */
const JUMP_WORKSPACE_COUNT = 9;

/** The id of the Nth positional workspace jump — the single owner of the id
 *  format, shared by the action generator and the nav-axis grouping below. */
const jumpWorkspaceActionId = (n: number): string => `viewer.jump-workspace-${n}`;

/** Positional terminal-tab jumps (`Cmd+Opt+1..9`) within the focused tile. */
const JUMP_TERMINAL_TAB_COUNT = 9;

/** The id of the Nth positional terminal-tab jump. */
const jumpTerminalTabActionId = (n: number): string => `viewer.jump-terminal-tab-${n}`;

/** Directional pane-focus axes, in reading order. Single source for both the
 *  generated `viewer.focus-pane-*` actions and the nav-axis grouping. */
const FOCUS_PANE_DIRECTIONS = [
  { dir: "left", arrow: "ArrowLeft", label: "Left" },
  { dir: "right", arrow: "ArrowRight", label: "Right" },
  { dir: "up", arrow: "ArrowUp", label: "Up" },
  { dir: "down", arrow: "ArrowDown", label: "Down" },
] as const;

/** The id of a directional pane-focus action — the single owner of the id
 *  format, shared by the action generator and the nav-axis grouping below. */
const focusPaneActionId = (dir: string): string => `viewer.focus-pane-${dir}`;

/**
 * SW-EW-46 (D6): resolve the `(activePaneId, switchFn)` that drives directional
 * pane focus for the CURRENT mode. When the expanded workspace shows its terminals
 * as a grid, the panes are its cells — each carries its tab id as
 * `data-pane-id`, so nav moves the active tab and lands the cursor in
 * that cell. Otherwise the panes are the workspace tiles (today's behavior). Both
 * feed the same geometric `focusAdjacentPane`; only the id space + switch action
 * differ, so the branch lives here once and the bubble action + the capture-phase
 * intercept (which dispatches the same `run()`) share it.
 */
function resolvePaneNav(
  teamKey: string,
  fallbackWorkspaceId: string,
): { activePaneId: string; switchFn: (tk: string, id: string) => void } {
  const store = useViewerStore.getState();
  const gridWs = expandedGridWorkspaceId(store, teamKey);
  if (gridWs) {
    // Start traversal from the active cell — but only if the active tab is actually
    // paned; otherwise (e.g. the user selected a bar tab that isn't a cell) fall back
    // to the first cell so arrows never start from an off-screen pane id and no-op.
    // `gridWs` is gridded, so the paned list is guaranteed non-empty.
    const paned = workspacePanedTabIds(store, teamKey, gridWs);
    const active = selectActiveTerminalTab(store, teamKey, gridWs);
    const cellId = active && paned.includes(active) ? active : paned[0];
    return {
      activePaneId: cellId,
      switchFn: (tk, tabId) => useViewerStore.getState().setActiveTerminalTab(tk, tabId, gridWs),
    };
  }
  return {
    activePaneId: fallbackWorkspaceId,
    switchFn: (tk, id) => switchContext({ teamKey: tk, workspaceId: id }, "nav"),
  };
}

/**
 * Close a tab's docked inbox. When focus is inside it (⌘I pressed in the composer), it goes
 * to that tab's terminal first, rather than falling to the page body as the rail unmounts.
 */
function closeInboxRailKeepingFocus(sessionId: string): void {
  const active = typeof document === "undefined" ? null : document.activeElement;
  const rail = active?.closest<HTMLElement>('[data-testid="inbox-rail"]');
  if (rail && rail.dataset.sessionId === sessionId) {
    const pane = rail.closest("[data-pane-id], [data-pane-active]");
    pane?.querySelector<HTMLTextAreaElement>(".xterm textarea")?.focus();
  }
  closeAgentInboxRail(sessionId);
}

export interface ShortcutAction {
  id: string;
  label: string;
  /** Two-tuple: [submenu, item]. Omit when the action is not in the menu. */
  menuPath?: readonly [string, string];
  defaultKeystroke: string;
  /** `global` — always fires. `workspace` — only when a workspace is focused in the active
   *  team, so not in the empty/minimized-roster state; on the
   *  Settings tab only the terminal section's. `team` — fires whenever a team is open, including
   *  the empty state (so tray nav like Cmd+1..9 / flip can reopen a minimized chip when nothing is
   *  focused); on the Settings tab all but the document's. */
  scope: "global" | "workspace" | "team";
  /** On macOS, match ⌘ only for `CmdOrCtrl` — never the Control chord, which is
   *  shell input when the key comes from a terminal (⌃T, ⌃F, ⌃K…). */
  macCmdOnly?: boolean;
  /** A slot dashboard's declared command (the dynamic layer). */
  command?: DashboardCommandInfo;
  /** Checked by the dispatch gate: whether the action may fire now. */
  available?: () => boolean;
  run: () => void;
}

/** What the registry knows of a dashboard command beyond its `ShortcutAction` shape. */
export interface DashboardCommandInfo {
  /** `dashboardRefKey` of the owning dashboard; the action id is `<dashboard>/<id>`. */
  dashboard: string;
  dashboardTitle: string;
  /** The dashboard's scope: team-safe keeps a private dashboard's commands for humans. */
  dashboardScope: "built-in" | "team" | "private";
  icon?: string;
  /** The manifest suggests keeping agents off this command. */
  humanOnly: boolean;
  /** In the dashboard's current `commands.register` set. */
  active: boolean;
}

const shortcutActions: readonly ShortcutAction[] = [
  {
    id: "settings.open",
    label: "Open Settings",
    menuPath: ["File", "Open Settings"],
    defaultKeystroke: "CmdOrCtrl+,",
    scope: "global",
    run: () => {
      openAppSettings("general");
    },
  },
  // SW-UX (app-flow redesign): open the management surfaces (now virtual tabs) from
  // the keyboard + the native Go menu. `run` uses a dynamic import so the registry
  // (loaded very early) never pulls the heavy tab bodies into its static graph —
  // the tab module loads on first open. With no team they open in the no-team tab host.
  {
    id: "nav.open-machines",
    label: "Open Machines",
    menuPath: ["Go", "Machines"],
    defaultKeystroke: "CmdOrCtrl+Alt+C",
    scope: "global",
    run: () => void import("@/features/shell/machines-virtual-tab").then((m) => m.openMachines()),
  },
  {
    id: "nav.open-source-repos",
    label: "Open Source Repos",
    menuPath: ["Go", "Source Repos"],
    defaultKeystroke: "CmdOrCtrl+Alt+R",
    scope: "global",
    run: () =>
      void import("@/features/dashboard/source-repos-virtual-tab").then((m) =>
        m.openSourceRepos("list"),
      ),
  },
  {
    id: "nav.open-teams",
    label: "Open Teams",
    menuPath: ["Go", "Teams"],
    defaultKeystroke: "CmdOrCtrl+Alt+T",
    scope: "global",
    run: () =>
      void import("@/features/shell/manage-teams-virtual-tab").then((m) => m.openManageTeams()),
  },
  {
    id: "nav.open-workspaces",
    label: "Open context repos",
    menuPath: ["Go", "Context Repos"],
    defaultKeystroke: "CmdOrCtrl+Alt+W",
    scope: "global",
    run: () => void import("@/features/org/lib/open-workspaces").then((m) => m.openWorkspaces()),
  },
  // SW-EW-33: `nav.switch-workspace` (Cmd+Shift+P) retired. It was a narrower,
  // older duplicate of the workspace palette (which lists open AND openable
  // stories), and carried a vestigial "Attach source repo" CTA. This palette is
  // now the single workspace open/switch path; Cmd+Shift+P is freed.
  {
    // SW-EW-33: the single workspace open/switch palette (was a local
    // team-view keydown handler; promoted to the registry so it's discoverable
    // + rebindable). `team` scope so it works in the empty/minimized state too
    // (open a workspace when none are focused). The active team's
    // `TeamWorkspaceView` renders the dialog off `ui-store.workspacePaletteOpen`.
    // SW-FX-49: bound to ⌘K (was ⌘⇧K) — ⌘K is the near-universal quick-switch
    // key and the deliberate counterpart to ⌘P's file peek (which no longer
    // opens a workspace as a side-effect). User-rebindable in Settings.
    id: "nav.open-workspace",
    label: "Open Workspace",
    menuPath: ["Go", "Open Workspace"],
    defaultKeystroke: "CmdOrCtrl+K",
    scope: "team",
    run: () => {
      const { teamKey } = getActiveWorkspaceContext();
      if (teamKey === NO_TEAM_SENTINEL) return;
      useUiStore.getState().setWorkspacePaletteOpen(true);
    },
  },
  {
    // The file quick-open (Cmd+P): the file palette for the whole app, terminal
    // included. In the registry so a dashboard command wanting Cmd+P is an ordinary
    // conflict, and frames reach it like any other chord.
    id: "nav.quick-open",
    label: "Quick Open File",
    menuPath: ["Go", "Quick Open File"],
    defaultKeystroke: "CmdOrCtrl+P",
    scope: "team",
    macCmdOnly: true,
    run: () => {
      const { teamKey } = getActiveWorkspaceContext();
      if (teamKey === NO_TEAM_SENTINEL) return;
      useUiStore.getState().setFileQuickOpenOpen(true);
    },
  },
  {
    // SW-SN-3: the note quick-switcher (Cmd+O) — the keyboard accelerator over
    // the SN-2 browse rail's list (a second door, not the spine). `team` scope
    // (notes are global per-install, but the dialog only mounts in the active
    // team's view, mirroring the workspace palette above). Flips the global
    // `noteSwitcherOpen` flag; the active `TeamWorkspaceView` renders the dialog.
    id: "nav.open-note-switcher",
    label: "Open Note Switcher",
    menuPath: ["Go", "Open Note Switcher"],
    defaultKeystroke: "CmdOrCtrl+O",
    scope: "team",
    run: () => {
      const { teamKey } = getActiveWorkspaceContext();
      if (teamKey === NO_TEAM_SENTINEL) return;
      useUiStore.getState().setNoteSwitcherOpen(true);
    },
  },
  {
    id: "nav.goto-heading",
    label: "Go to Heading",
    menuPath: ["Go", "Go to Heading"],
    defaultKeystroke: "CmdOrCtrl+Shift+O",
    scope: "team",
    run: () => {
      const { teamKey } = getActiveWorkspaceContext();
      if (teamKey === NO_TEAM_SENTINEL) return;
      const { dashboard } = resolveSlot("right-pane", { surface: "notes" });
      if (dashboard?.dashboardRef) {
        runDashboardCommand(
          dashboardCommandActionId(dashboardRefKey(dashboard.dashboardRef), "goto-heading"),
        );
        return;
      }
      useUiStore.getState().setHeadingJumpOpen(true);
    },
  },
  {
    // SW-HB-6: the ⌘⇧P command palette over local objects (teams / source repos /
    // epic-files) + setup actions. `global` scope — unlike the team-scoped
    // workspace/note palettes, this must open from the Home Base hub too (no
    // active team required), so it flips the global `commandPaletteOpen` flag with
    // NO team gate. Mounted app-globally in `App.tsx` (`CommandPaletteMount`),
    // not in `TeamWorkspaceView`.
    // Keystroke: ⌘⇧P — the near-universal "command palette" chord (VS Code),
    // freed when SW-EW-33 retired `nav.switch-workspace`. NOT ⌘K: SW-FX-49
    // re-bound the workspace open/switch palette to ⌘K, so the two are
    // deliberately split — ⌘K = switch workspace (intra-team), ⌘⇧P = run a
    // command / jump across the org.
    id: "nav.open-command-palette",
    label: "Command Palette",
    menuPath: ["Go", "Command Palette"],
    defaultKeystroke: "CmdOrCtrl+Shift+P",
    scope: "global",
    run: () => {
      useUiStore.getState().setCommandPaletteOpen(true);
    },
  },
  {
    // SW-LNS-1: focus the left-nav search bar at the top of the pipeline nav.
    // `Cmd+Shift+F` reads as "find in files" (VS Code chord) — a natural
    // sibling of `Cmd+F` (viewer / terminal find within the active surface)
    // and distinct from `Cmd+P` (file quick-open palette). `team` scope so
    // it fires whenever a team is open; the request flows through the
    // `leftNavSearchFocusNonce` ui-store counter the active team's nav
    // subscribes to.
    id: "nav.focus-left-search",
    label: "Focus Nav Search",
    menuPath: ["Go", "Focus Nav Search"],
    defaultKeystroke: "CmdOrCtrl+Shift+F",
    scope: "team",
    run: () => {
      const { teamKey } = getActiveWorkspaceContext();
      if (teamKey === NO_TEAM_SENTINEL) return;
      useUiStore.getState().requestFocusLeftNavSearch();
    },
  },
  {
    id: "nav.switch-org",
    label: "Switch Context Repo",
    menuPath: ["Go", "Switch Context Repo"],
    // SW-SN-6: rebound from Cmd+Shift+O → Cmd+Shift+G to free the VS-Code-style
    // "Go to Symbol" chord for `nav.goto-heading` (EXPERIENCE.md spec). Org-switch
    // is the rarer keyboard action and stays reachable via its UI; both defaults
    // are user-rebindable in Settings → Shortcuts.
    defaultKeystroke: "CmdOrCtrl+Shift+G",
    scope: "global",
    run: () => {
      // SW-OC-28: opens the identity menu at its Context repo section. Always
      // opens — even with 1 context repo the section ends with Connect / Create.
      useUiStore.getState().toggleOrgSwitcher();
    },
  },
  {
    // Back and forward over the teams and workspaces the user switched between (not the file
    // history, which stays on the mouse's back and forward buttons). ⌘ only on macOS: ⌃[ is
    // Escape in a terminal.
    id: "nav.context-back",
    label: "Back",
    menuPath: ["Go", "Back"],
    get defaultKeystroke() {
      return isMac() ? "CmdOrCtrl+[" : "Alt+ArrowLeft";
    },
    scope: "global",
    macCmdOnly: true,
    run: () => {
      contextBack();
    },
  },
  {
    id: "nav.context-forward",
    label: "Forward",
    menuPath: ["Go", "Forward"],
    get defaultKeystroke() {
      return isMac() ? "CmdOrCtrl+]" : "Alt+ArrowRight";
    },
    scope: "global",
    macCmdOnly: true,
    run: () => {
      contextForward();
    },
  },
  {
    id: "nav.switch-team",
    label: "Switch Team",
    menuPath: ["Go", "Switch Team"],
    // ⌘⇧T is Reopen Closed Tab (the browser/terminal idiom), so the team switcher
    // moved to the Alt sibling. Rebindable in Settings → Shortcuts.
    defaultKeystroke: "CmdOrCtrl+Alt+Shift+T",
    scope: "global",
    run: () => {
      const state = useOrgStore.getState();
      const activeOrg = state.orgs.find((o) => o.slug === state.activeOrgSlug);
      if (!activeOrg) return;
      // SW-OB-7: all org teams are selectable (no membership gate). SW-OB-9:
      // gate on the user's local shown set (imperative — a point-in-time read
      // is correct). Open the identity menu at its Team section whenever ≥1 team
      // is shown so Create team stays reachable.
      if (visibleTeams(activeOrg, state.shownTeamsByOrgSlug).length < 1) return;
      useUiStore.getState().toggleTeamSwitcher();
    },
  },
  {
    id: "viewer.toggle-terminal",
    label: "Toggle Terminal",
    menuPath: ["View", "Toggle Terminal"],
    defaultKeystroke: "CmdOrCtrl+\\",
    scope: "team",
    run: () => {
      // The terminal layout is one for every team (D39), so this acts even in the empty
      // state (last workspace closed): it needs a team, not a focused workspace.
      const { teamKey } = getActiveWorkspaceContext();
      if (teamKey === NO_TEAM_SENTINEL) return;
      // SV-PBF-184: from non-CLI focus reveal/focus the existing CLI; from CLI focus
      // keep the ordinary toggle-to-hide. An empty CLI has nothing to focus, so it
      // hides (SV-GHI-2). Divider clicks use toggleTerminalWithFocus.
      revealOrToggleTerminal(teamKey);
    },
  },
  {
    id: "viewer.toggle-nav",
    label: "Toggle Sidebar",
    menuPath: ["View", "Toggle Sidebar"],
    defaultKeystroke: "CmdOrCtrl+B",
    scope: "team",
    run: () => {
      // SW-EW-35: the pipeline nav is a TEAM surface (navCollapsed is team-keyed
      // and the nav renders the team pipeline with no workspace open) — toggle it
      // in the empty state too. Gate on team presence, not a focused workspace.
      const { teamKey } = getActiveWorkspaceContext();
      if (teamKey === NO_TEAM_SENTINEL) return;
      // Shared with the nav-divider click (SW-FX-8) so they can't drift.
      toggleNavCollapsed(teamKey);
    },
  },
  {
    id: "viewer.toggle-right-pane",
    label: "Toggle Right Pane",
    menuPath: ["View", "Toggle Right Pane"],
    defaultKeystroke: "CmdOrCtrl+J",
    scope: "team",
    run: () => {
      // SW-EW-35 / SW-FX-33: the right pane (team-level) renders
      // without a focused workspace, so its toggle must work in the empty state
      // too. `rightPaneOpen` is global; gate only on team presence.
      const { teamKey } = getActiveWorkspaceContext();
      if (teamKey === NO_TEAM_SENTINEL) return;
      toggleRightPaneWithFocus(teamKey);
    },
  },
  {
    id: "viewer.toggle-pane-full-screen",
    label: "Toggle Pane Full Screen",
    menuPath: ["View", "Toggle Pane Full Screen"],
    defaultKeystroke: "",
    scope: "team",
    run: () => {
      const { teamKey } = getActiveWorkspaceContext();
      if (teamKey === NO_TEAM_SENTINEL) return;
      const panelId = activeRightPanePanel(teamKey);
      if (panelId) useViewerStore.getState().toggleMaximized(teamKey, panelId);
    },
  },
  {
    // SW-EW-35: full-screen / console toggle. Promoted from the hardcoded,
    // non-rebindable `Cmd+Shift+\` branch in `team-workspace-view` so it's
    // discoverable + rebindable, and its CLI-chrome tooltip can show the
    // keystroke. `toggleConsoleShared` flips console↔shared (it records
    // `lastTerminalMode` for both, so a manual round-trip would never leave
    // console — the store action is the correct single call).
    id: "viewer.toggle-console",
    label: "Toggle Console",
    menuPath: ["View", "Toggle Console"],
    defaultKeystroke: "CmdOrCtrl+Shift+\\",
    scope: "team",
    run: () => {
      // Console↔shared is a flip of the CLI's layout, one for every team; it works in the
      // empty state, like the terminal toggle above.
      const { teamKey } = getActiveWorkspaceContext();
      if (teamKey === NO_TEAM_SENTINEL) return;
      useViewerStore.getState().toggleConsoleShared();
    },
  },
  {
    // SW-CW-7: cycle the split-terminal arrangement (columns → rows → grid →
    // columns) from the keyboard. Sits beside toggle-console — both are layout
    // flips that work whenever a team is open. `CmdOrCtrl+Shift+L` (L =
    // Layout) is not terminal input, so it needs no capture-phase intercept; the
    // bubble registry dispatch is enough. The store action inherits slot-targeting,
    // direction sync, custom-root reset and persistence, and the
    // on-screen CliLayoutControl glyph re-renders to the new preset for free.
    //
    // SW-EW-46 (D4): mode branch — when the expanded workspace is showing its
    // terminals as a grid, the same chord cycles that TAB-grid's preset (reusing
    // PRESET_RING) instead of the workspace mosaic. An expanded+zoomed workspace
    // leaves the mosaic with one filling tile and nothing to cycle, so retargeting
    // is unambiguous and keeps the muscle memory.
    id: "viewer.cycle-foreground-layout",
    label: "Cycle Terminal Layout",
    menuPath: ["View", "Cycle Terminal Layout"],
    defaultKeystroke: "CmdOrCtrl+Shift+L",
    scope: "team",
    run: () => {
      const { teamKey } = getActiveWorkspaceContext();
      if (teamKey === NO_TEAM_SENTINEL) return;
      const store = useViewerStore.getState();
      const gridWs = expandedGridWorkspaceId(store, teamKey);
      if (gridWs) {
        store.cycleTabGridPreset(teamKey, gridWs);
      } else {
        store.cycleForegroundPreset(teamKey);
      }
    },
  },
  // SLW-TL-4: the Activity board's Dispatch action was retired — the dispatch-brief
  // form it opened is gone (§2.3: "Giving an agent work is NOT a form"), so the
  // shortcut is removed rather than left as a silent no-op. New work starts by opening
  // an agent's CLI (SLW-TL-7's need→action wiring).
  // SW-AV-5: J-ump to the next Activity row that needs you — cycle the YOU-OWE rows that
  // carry a live agent and drill each in turn (the "get to the agent that needs me" path).
  // Same mnemonic Alt+Shift band as the D/P/A/G cluster; the board publishes the closure
  // (`weft-jump-store`). Without the board it runs the same jump as ⌘⇧J.
  {
    id: "weft.jump-to-next-needs-you",
    label: "Jump to Next Needs-You",
    menuPath: ["Go", "Jump to Next Needs-You"],
    defaultKeystroke: "CmdOrCtrl+Alt+Shift+J",
    scope: "team",
    run: () => {
      const { teamKey } = getActiveWorkspaceContext();
      if (teamKey === NO_TEAM_SENTINEL) return;
      void import("@/features/weft/stores/weft-jump-store").then((m) => {
        const boardJump = m.useWeftJumpStore.getState().jumpToNextNeedsYou;
        if (boardJump) boardJump();
        else if (!jumpToWhatNeedsYou(teamKey))
          flashBlockedNavForWorkspace(getActiveWorkspaceContext().workspace?.id, "pulse");
      });
    },
  },
  // SW-EW-33: `viewer.toggle-workspace-panel` (Cmd+Shift+B) removed with the left
  // WorkspaceManagementPanel — the bottom tray has no collapse/toggle.
  // The one "take me to what needs me" chord: the focused tile's waiting question,
  // else the next tab needing you (permission / question / crash) across the team —
  // Hidden tabs and tray tiles restored — landing in its question banner or its
  // terminal. `team` scope, so it works with everything hidden. (The id keeps its
  // original name so saved rebinds carry over.)
  {
    id: "viewer.focus-tile-question",
    label: "Jump to What Needs You",
    menuPath: ["Go", "Jump to What Needs You"],
    defaultKeystroke: "CmdOrCtrl+Shift+J",
    scope: "team",
    macCmdOnly: true,
    run: () => {
      // The focused tile's question, or the next tab needing you: both are the CLI's.
      const { teamKey, workspace } = getActiveWorkspaceContext();
      if (teamKey === NO_TEAM_SENTINEL) return;
      if (!jumpToWhatNeedsYou(teamKey)) flashBlockedNavForWorkspace(workspace?.id, "pulse");
    },
  },
  {
    id: "viewer.toggle-story-hud",
    label: "Toggle Story HUD",
    menuPath: ["View", "Toggle Story HUD"],
    defaultKeystroke: "CmdOrCtrl+Shift+H",
    scope: "workspace",
    run: () => {
      // SW-EW-28: show/hide the FOCUSED tile's story HUD (same per-workspace
      // state the chrome toggle drives).
      const { workspace } = getActiveWorkspaceContext();
      if (!workspace) return;
      useViewerStore.getState().toggleStoryHudShown(workspace.id);
    },
  },
  {
    id: "viewer.toggle-story-hud-size",
    label: "Expand / Collapse Story HUD",
    menuPath: ["View", "Expand / Collapse Story HUD"],
    defaultKeystroke: "CmdOrCtrl+Shift+U",
    scope: "workspace",
    run: () => {
      // SW-EW-28: expand/collapse the FOCUSED tile's HUD (the card's maximize and
      // the panel's minimize buttons drive the same per-workspace size state). If
      // the HUD is hidden, open it straight into the expanded view.
      const { workspace } = getActiveWorkspaceContext();
      if (!workspace) return;
      const v = useViewerStore.getState();
      if (!v.storyHudShown[workspace.id]) {
        v.toggleStoryHudShown(workspace.id, true);
        v.toggleStoryHudSize(workspace.id, true);
      } else {
        v.toggleStoryHudSize(workspace.id);
      }
    },
  },
  {
    id: "viewer.toggle-source-diff",
    label: "Toggle Source Diff",
    menuPath: ["View", "Toggle Source Diff"],
    defaultKeystroke: "CmdOrCtrl+Shift+D",
    scope: "workspace",
    run: () => {
      // Show/hide the FOCUSED tile's ACTIVE TAB's source-diff review overlay
      // (per-tab visibility). Resolves the active tab's sessionId via the live
      // terminal projection; without an active tab (idle tile) the hotkey is
      // inert.
      const { teamKey, workspace } = getActiveWorkspaceContext();
      // SW-VE-12: source-diff tracks `diffCapable` (root/story/generic) —
      // `epic`/`git-sync` cwd dirs are inside their own git parent (the
      // context repo) but a diff of the enclosing repo in those tabs is
      // misleading, so the hotkey (like the icon) stays off there.
      if (!workspace || !getKindCapabilities(workspace.kind).diffCapable) return;
      const activeTabId = selectActiveTerminalTab(useViewerStore.getState(), teamKey, workspace.id);
      if (!activeTabId) return;
      useViewerStore.getState().toggleSourceDiffShown({
        sessionId: activeTabId,
        workspaceId: workspace.id,
        teamKey,
      });
    },
  },
  {
    id: "viewer.toggle-code-editor",
    label: "Toggle Code Editor",
    menuPath: ["View", "Toggle Code Editor"],
    defaultKeystroke: "CmdOrCtrl+Shift+C",
    scope: "workspace",
    run: () => {
      // Toggle the FOCUSED tile's ACTIVE TAB's worktree code editor — opening
      // resumes the tile's remembered file (`worktreeEditorFile[workspaceId]`,
      // per-workspace session memory). Same per-tab scoping as source diff.
      const { teamKey, workspace } = getActiveWorkspaceContext();
      // SW-VE-12: code-editor fires on every folder-rooted kind (every live
      // kind qualifies for `sourceCapable`) — editor renders against the
      // tab's cwd whether or not the cwd is a git working tree.
      if (!workspace || !getKindCapabilities(workspace.kind).sourceCapable) return;
      const activeTabId = selectActiveTerminalTab(useViewerStore.getState(), teamKey, workspace.id);
      if (!activeTabId) return;
      useViewerStore.getState().toggleWorktreeEditorOpen({
        sessionId: activeTabId,
        workspaceId: workspace.id,
        teamKey,
      });
    },
  },
  {
    // Toggle the FOCUSED tile's ACTIVE TAB's docked inbox. Per-session scoping
    // via `agent-inbox-rail-store`; inert when no active tab exists. ⌘ only on
    // macOS: ⌃I is Tab to a shell.
    id: "viewer.toggle-agent-inbox",
    label: "Toggle Agent Inbox",
    menuPath: ["View", "Toggle Agent Inbox"],
    defaultKeystroke: "CmdOrCtrl+I",
    scope: "workspace",
    macCmdOnly: true,
    run: () => {
      const { teamKey, workspace } = getActiveWorkspaceContext();
      if (!workspace) return;
      const activeTabId = selectActiveTerminalTab(useViewerStore.getState(), teamKey, workspace.id);
      if (!activeTabId || !isRunningAgent(useAgentStateStore.getState(), activeTabId)) return;
      const isOpen = !!useAgentInboxRailStore.getState().bySession[activeTabId];
      if (isOpen) closeInboxRailKeepingFocus(activeTabId);
      else openAgentInboxRail(activeTabId);
    },
  },
  {
    id: "viewer.open-group-chat",
    label: "Open Group Chat",
    menuPath: ["View", "Open Group Chat"],
    defaultKeystroke: "CmdOrCtrl+Alt+G",
    scope: "workspace",
    macCmdOnly: true,
    run: () => {
      const { teamKey, workspace } = getActiveWorkspaceContext();
      if (!workspace) return;
      const session = selectActiveTerminalTab(useViewerStore.getState(), teamKey, workspace.id);
      if (session) useAgentInboxRailStore.getState().openGroup(session);
    },
  },
  {
    // Keep the persisted shortcut id while opening the Inbox pane. The item sits in
    // View's Panels submenu.
    id: "viewer.toggle-inbox-rollup",
    label: "Inbox",
    menuPath: ["View", "Panels → Inbox"],
    defaultKeystroke: "CmdOrCtrl+Shift+I",
    scope: "team",
    // ⌘ only on macOS, like ⌘I: ⌃⇧I reaches a shell as Tab.
    macCmdOnly: true,
    run: () => revealSlot("right-pane", { surface: "inbox" }),
  },
  {
    id: "viewer.toggle-terminal-zoom",
    label: "Expand In / Out",
    menuPath: ["View", "Expand In / Out"],
    defaultKeystroke: "CmdOrCtrl+Shift+M",
    scope: "workspace",
    run: () => {
      // One key in and out: the workspace view → Expanded → Expanded · all tabs,
      // then back out, turning round at each end (the tile's ⛶ button and
      // double-click stay a plain expand / restore).
      const { teamKey, workspace } = getActiveWorkspaceContext();
      if (!workspace) return;
      if (!stepExpand(teamKey)) flashBlockedNavForWorkspace(workspace.id, "pulse");
    },
  },
  {
    id: "viewer.cycle-workspace-next",
    label: "Cycle Story Workspace Next",
    menuPath: ["Go", "Cycle Story Workspace Next"],
    defaultKeystroke: "CmdOrCtrl+Alt+]",
    scope: "workspace",
    run: () => {
      const { teamKey, workspace } = getActiveWorkspaceContext();
      if (!workspace) return;
      let moved = false;
      recordContextChange("nav", () => {
        moved = useViewerStore.getState().cycleWorkspace(teamKey, "next");
      });
      if (moved) focusActiveWorkspaceTerminal(teamKey);
      else flashBlockedNavForWorkspace(workspace.id, "pulse");
    },
  },
  {
    id: "viewer.cycle-workspace-prev",
    label: "Cycle Story Workspace Prev",
    menuPath: ["Go", "Cycle Story Workspace Prev"],
    defaultKeystroke: "CmdOrCtrl+Alt+[",
    scope: "workspace",
    run: () => {
      const { teamKey, workspace } = getActiveWorkspaceContext();
      if (!workspace) return;
      let moved = false;
      recordContextChange("nav", () => {
        moved = useViewerStore.getState().cycleWorkspace(teamKey, "prev");
      });
      if (moved) focusActiveWorkspaceTerminal(teamKey);
      else flashBlockedNavForWorkspace(workspace.id, "pulse");
    },
  },
  // OC-7: cycle the active workspace's terminal TABS — the fifth keyboard-nav
  // axis (workspace / epic / tab / pane / jump-flip), completing keyboard nav.
  // Bracket family: workspace = Cmd+Alt+[/], epic = Cmd+Alt+Shift+[/], tab =
  // Cmd+Shift+[/] (Alt distinguishes the axes). Bracket combos leak into the PTY
  // as shell word-nav escapes, so these are caught at capture phase
  // (use-workspace-cycle-intercept) via this entry's run() delegate. run() cycles
  // the tab then re-focuses the newly-active tab's terminal (the tab-bar click
  // re-focuses too) so keystrokes land in the now-visible PTY, not the old one.
  {
    id: "viewer.cycle-terminal-tab-next",
    label: "Cycle Terminal Tab Next",
    menuPath: ["Go", "Cycle Terminal Tab Next"],
    defaultKeystroke: "CmdOrCtrl+Shift+]",
    scope: "workspace",
    run: () => {
      const { teamKey, workspace } = getActiveWorkspaceContext();
      if (!workspace) return;
      if (useViewerStore.getState().cycleTerminalTab(teamKey, "next"))
        focusActiveWorkspaceTerminal(teamKey);
      else flashBlockedNavForWorkspace(workspace.id, "pulse");
    },
  },
  {
    id: "viewer.cycle-terminal-tab-prev",
    label: "Cycle Terminal Tab Prev",
    menuPath: ["Go", "Cycle Terminal Tab Prev"],
    defaultKeystroke: "CmdOrCtrl+Shift+[",
    scope: "workspace",
    run: () => {
      const { teamKey, workspace } = getActiveWorkspaceContext();
      if (!workspace) return;
      if (useViewerStore.getState().cycleTerminalTab(teamKey, "prev"))
        focusActiveWorkspaceTerminal(teamKey);
      else flashBlockedNavForWorkspace(workspace.id, "pulse");
    },
  },
  // OC-6: previous/next EPIC in grouped tray mode — the epic-group axis (the
  // workspace-cycle above stays WITHIN a rail; these move BETWEEN rails). The
  // Shift sibling of the workspace-cycle chord, conflict-free against the known
  // bindings. Both restore the target epic's MRU workspace + focus its terminal
  // through the shared `navigateEpicRail` → `activateAndFocusTrayWorkspace`
  // coordinator (no second focus path). Bracket combos leak into the PTY, so
  // these are ALSO caught at capture phase (use-workspace-cycle-intercept) via
  // this entry's run() delegate — the registry stays the single command home.
  {
    id: "viewer.cycle-epic-next",
    label: "Cycle Epic Group Next",
    menuPath: ["Go", "Cycle Epic Group Next"],
    defaultKeystroke: "CmdOrCtrl+Alt+Shift+]",
    scope: "workspace",
    run: () => {
      const { teamKey, workspace } = getActiveWorkspaceContext();
      if (!workspace) return;
      if (!navigateEpicRail(teamKey, "next")) flashBlockedNavForWorkspace(workspace.id, "pulse");
    },
  },
  {
    id: "viewer.cycle-epic-prev",
    label: "Cycle Epic Group Prev",
    menuPath: ["Go", "Cycle Epic Group Prev"],
    defaultKeystroke: "CmdOrCtrl+Alt+Shift+[",
    scope: "workspace",
    run: () => {
      const { teamKey, workspace } = getActiveWorkspaceContext();
      if (!workspace) return;
      if (!navigateEpicRail(teamKey, "prev")) flashBlockedNavForWorkspace(workspace.id, "pulse");
    },
  },
  // SW-EW-33: directional pane focus (Cmd+Opt+arrows) — tmux/i3-style move-focus
  // by direction across the mosaic. Geometric (any nested-split arrangement, not
  // just a grid); focus-only (no layout change). Arrows are core terminal input,
  // so these are ALSO caught at capture phase (use-workspace-cycle-intercept)
  // before xterm — this registry entry is the bubble/non-terminal + discoverable
  // + rebindable path.
  ...FOCUS_PANE_DIRECTIONS.map(
    ({ dir, arrow, label }): ShortcutAction => ({
      id: focusPaneActionId(dir),
      label: `Focus Pane ${label}`,
      menuPath: ["Go", `Focus Pane ${label}`],
      defaultKeystroke: `CmdOrCtrl+Alt+${arrow}`,
      scope: "workspace",
      run: () => {
        const { teamKey, workspace } = getActiveWorkspaceContext();
        if (!workspace) return;
        // SW-EW-46 (D6): the panes are the tab-grid CELLS when the expanded
        // workspace is gridded, else the workspace tiles (today). Same geometric
        // resolver either way — only the pane id + switch action differ.
        const { activePaneId, switchFn } = resolvePaneNav(teamKey, workspace.id);
        const moved = focusAdjacentPane(teamKey, activePaneId, dir as FocusDirection, switchFn);
        if (!moved) flashBlockedNavForWorkspace(activePaneId, `nudge-${dir}`);
      },
    }),
  ),
  {
    id: "viewer.jump-root-workspace",
    label: "Jump to Root Workspace",
    menuPath: ["Go", "Jump to Root Workspace"],
    defaultKeystroke: "CmdOrCtrl+0",
    scope: "workspace",
    run: () => {
      const { teamKey, workspace } = getActiveWorkspaceContext();
      if (!workspace) return;
      let moved = false;
      recordContextChange("nav", () => {
        moved = useViewerStore.getState().jumpToRootWorkspace(teamKey);
      });
      if (!moved) flashBlockedNavForWorkspace(workspace.id, "pulse");
    },
  },
  {
    // Hide the active workspace into the tray — its terminals keep running.
    id: "viewer.close-workspace",
    label: "Hide Workspace",
    menuPath: ["Go", "Hide Workspace"],
    defaultKeystroke: "CmdOrCtrl+Shift+W",
    scope: "workspace",
    run: () => {
      const { teamKey, workspace } = getActiveWorkspaceContext();
      if (!workspace) return;
      // Cannot hide a root workspace — only stories close via this path.
      if (parseRootWorkspaceId(workspace.id) !== null) return;
      hideWorkspace(teamKey, workspace.id);
    },
  },
  {
    // End the active workspace — kills its terminals, asking in-app first when
    // anything is still running.
    id: "viewer.hard-close-workspace",
    label: "End Workspace",
    menuPath: ["Go", "End Workspace"],
    defaultKeystroke: "CmdOrCtrl+Shift+Backspace",
    scope: "workspace",
    run: () => {
      const { teamKey, workspace } = getActiveWorkspaceContext();
      if (!workspace) return;
      // Roots can't be ended (parity with Hide above).
      if (parseRootWorkspaceId(workspace.id) !== null) return;
      void endWorkspace(teamKey, workspace.id);
    },
  },
  {
    id: "viewer.edit-file",
    label: "Edit File",
    menuPath: ["File", "Edit File"],
    defaultKeystroke: "CmdOrCtrl+E",
    // SW-VE-8: team-scoped so it works in the root viewer (not just a workspace
    // tile). The registry drives STORE state (it can't reach the view's edit
    // gate ref); the active team view honors the request through the SAME
    // edit-entry gate the ⌘E keydown used, so it no-ops on a read-only /
    // inherited / non-editable / already-editing file. Replaces the former
    // inline ⌘E keydown → now discoverable in Settings → Shortcuts + File menu
    // and user-rebindable (consistent with viewer.export-pdf, ⌘⇧E).
    scope: "team",
    run: () => {
      // Same team-resolution idiom as every other team-scoped action.
      const { teamKey } = getActiveWorkspaceContext();
      if (teamKey === NO_TEAM_SENTINEL) return;
      useViewerStore.getState().requestEditActiveFile(teamKey);
    },
  },
  {
    id: "viewer.export-pdf",
    label: "Export to PDF",
    menuPath: ["File", "Export to PDF"],
    defaultKeystroke: "CmdOrCtrl+Shift+E",
    scope: "workspace",
    run: () => {
      const { teamKey, workspace, sourceRepoSlug, repoPath } = getActiveWorkspaceContext();
      if (!workspace) return;
      // T.4 Phase 5: activeTab is team-keyed.
      const tab = useViewerStore.getState().activeTab[teamKey];
      if (!tab) return;
      if (!canExportToPdf(tab)) return;
      // T.6: resolve repo via the active workspace's synthetic `repoPath`
      // (the canonical `workspaceRepos` key); fall back to `sourceRepoSlug`
      // for legacy config-store entries that key by slug.
      const lookupKey = repoPath ?? sourceRepoSlug;
      if (!lookupKey) {
        toast.error("Couldn't export — workspace has no source repo");
        return;
      }
      const fromConfig = useConfigStore.getState().repos.find((r) => r.repoPath === lookupKey);
      const fromWorkspace = useOrgStore.getState().workspaceRepos[lookupKey];
      const resolved = fromConfig ?? fromWorkspace;
      // Surface feedback on empty teamContentPath — the earlier silent
      // no-op left the user with no signal when Cmd+Shift+E did nothing on
      // a visible tab. The other PDF export sites (view, tab context menu)
      // toast on the same condition; matching here keeps the UX consistent
      // since the precondition isn't visually verifiable (unlike `!workspace`
      // or `!tab` above, which the user can see).
      if (resolved?.teamContentPath) {
        void exportViewerToPdf(tab, resolved.teamContentPath);
      } else {
        toast.error("Couldn't export — content not ready");
      }
    },
  },
  // SW-EW-33: positional jump `Cmd/Ctrl+1..9` → the Nth chip in the rendered
  // tray order (i3 `mod+N` / VS Code `Cmd+N` family, under the existing Cmd+0 =
  // root). The shared state-rule (minimized → replace, on-screen → focus) lives
  // in the store's `jumpToTrayIndex` → `activateTrayWorkspace`.
  ...Array.from({ length: JUMP_WORKSPACE_COUNT }, (_, i): ShortcutAction => {
    const n = i + 1;
    return {
      id: jumpWorkspaceActionId(n),
      label: `Jump to Workspace ${n}`,
      menuPath: ["Go", `Jump to Workspace ${n}`],
      defaultKeystroke: `CmdOrCtrl+${n}`,
      // `team` (not `workspace`): must fire in the empty/minimized-roster state
      // (active = root:_no_repos sentinel, no focused workspace) to reopen a chip.
      scope: "team",
      run: () => {
        // NB: guard on TEAM, not on an active workspace — the tray (and its
        // chips) exist even when nothing is focused (everything minimized →
        // active is the root:_no_repos sentinel). A `!workspace` guard here would
        // make Cmd+N a no-op in exactly the empty/minimized-roster state where
        // the user needs it to reopen a chip.
        const { teamKey, workspace } = getActiveWorkspaceContext();
        if (teamKey === NO_TEAM_SENTINEL) return;
        const store = useViewerStore.getState();
        // The bar (and its 1..9 legend) stays on screen with the CLI closed: a jump
        // to a chip that exists reopens the panel in place, like clicking that chip
        // — even the already-focused one, which the store reports as no move.
        const reopened =
          store.trayOrderByTeam[teamKey]?.[i] !== undefined && revealCollapsedTerminal();
        let jumped = false;
        recordContextChange("nav", () => {
          jumped = store.jumpToTrayIndex(teamKey, i);
        });
        if (jumped || reopened) focusActiveWorkspaceTerminal(teamKey);
        // A blocked jump (already on that chip / out of range) flashes the active
        // tile; in the empty/minimized-roster state there is no tile to cue, so
        // the helper resolves nothing and the flash is a safe no-op.
        else flashBlockedNavForWorkspace(workspace?.id, "pulse");
      },
    };
  }),
  {
    // SW-EW-33: flip-to-last (recency 2-way toggle). The held-scan Ctrl+Tab MRU
    // is deferred (D10 — needs new keyboard infra); this single binding delivers
    // the dominant recency win with zero new infra.
    id: "viewer.flip-last-workspace",
    label: "Flip to Last Workspace",
    menuPath: ["Go", "Flip to Last Workspace"],
    defaultKeystroke: "CmdOrCtrl+Alt+;",
    // `team` scope (see jump-workspace-N): fires in the empty state too.
    scope: "team",
    run: () => {
      // Guard on TEAM, not active workspace (see jump-workspace-N above) — flip
      // must work from the empty/minimized-roster state too.
      const { teamKey, workspace } = getActiveWorkspaceContext();
      if (teamKey === NO_TEAM_SENTINEL) return;
      let moved = false;
      recordContextChange("nav", () => {
        moved = useViewerStore.getState().flipLastWorkspace(teamKey);
      });
      if (moved) focusActiveWorkspaceTerminal(teamKey);
      // Blocked flip (no distinct prior workspace) → cue the active tile; the
      // helper no-ops when the empty roster leaves nothing focused.
      else flashBlockedNavForWorkspace(workspace?.id, "pulse");
    },
  },
  // Keyboard verbs on terminal tabs. Each is a thin call into `terminal-tab-keys`.
  // All are Cmd chords (safe from the shell) except F2, and all are also caught at
  // capture phase inside the terminal area (use-workspace-cycle-intercept) so none
  // reaches the PTY. ⌘W stays the App-level close decision tree (Hide / End).
  {
    id: "viewer.new-terminal-tab",
    macCmdOnly: true,
    label: "New Terminal Tab",
    menuPath: ["File", "New Terminal Tab"],
    defaultKeystroke: "CmdOrCtrl+T",
    scope: "workspace",
    run: () => {
      const { teamKey, workspace } = getActiveWorkspaceContext();
      if (!workspace) return;
      newTerminalTab(teamKey, workspace.id);
    },
  },
  {
    // The quick ⌘T always opens on this Mac; choosing another machine (or an
    // agent launch there) is this deliberate verb.
    id: "viewer.new-terminal-on-machine",
    macCmdOnly: true,
    label: "New Terminal On…",
    menuPath: ["File", "New Terminal On…"],
    defaultKeystroke: "CmdOrCtrl+Shift+N",
    scope: "workspace",
    run: () => {
      const { teamKey, workspace } = getActiveWorkspaceContext();
      if (!workspace) return;
      requestNewWorkspaceTerminal(teamKey, workspace.id);
    },
  },
  {
    // A new terminal beside the focused one, side by side, with the cursor in it.
    // ⌘D as in iTerm2 and Terminal: ⌃D stays the shell's EOF on macOS.
    id: "viewer.split-terminal",
    macCmdOnly: true,
    label: "Split Terminal",
    menuPath: ["View", "Split Terminal"],
    defaultKeystroke: "CmdOrCtrl+D",
    scope: "workspace",
    run: () => {
      const { teamKey, workspace } = getActiveWorkspaceContext();
      // A text field or code editor keeps its own ⌘D.
      if (!workspace || isFocusInOtherEditor()) return;
      if (!splitTerminal(teamKey)) flashBlockedNavForWorkspace(workspace.id, "pulse");
    },
  },
  {
    // Reopens the last Hidden tab on its live session, or opens a fresh shell for
    // the last Ended one. `team` scope: it can bring back a tab whose whole tile
    // is hidden in the tray, with nothing focused.
    id: "viewer.reopen-closed-tab",
    macCmdOnly: true,
    label: "Reopen Closed Tab",
    menuPath: ["File", "Reopen Closed Tab"],
    defaultKeystroke: "CmdOrCtrl+Shift+T",
    scope: "team",
    run: () => {
      const { teamKey, workspace } = getActiveWorkspaceContext();
      if (teamKey === NO_TEAM_SENTINEL) return;
      if (!reopenClosedTab(teamKey)) flashBlockedNavForWorkspace(workspace?.id, "pulse");
    },
  },
  {
    // F2 on a focused tab pill. Not claimed from the terminal itself, where F2
    // belongs to the program running there (htop, mc, nano).
    id: "viewer.rename-terminal-tab",
    label: "Rename Terminal Tab",
    menuPath: ["View", "Rename Terminal Tab"],
    defaultKeystroke: "F2",
    scope: "workspace",
    run: () => {
      renameFocusedTab();
    },
  },
  {
    // ⌘F in the terminal area finds in that terminal; anywhere else the viewer's
    // own ⌘F (doc find, pipeline search, editor search) keeps it.
    id: "viewer.find-in-terminal",
    macCmdOnly: true,
    label: "Find in Terminal",
    menuPath: ["View", "Find in Terminal"],
    defaultKeystroke: "CmdOrCtrl+F",
    scope: "workspace",
    run: () => {
      findInFocusedTerminal(getActiveWorkspaceContext().teamKey);
    },
  },
  {
    id: "viewer.clear-terminal",
    macCmdOnly: true,
    label: "Clear Terminal",
    menuPath: ["View", "Clear Terminal"],
    defaultKeystroke: "CmdOrCtrl+Shift+K",
    scope: "workspace",
    run: () => {
      const { teamKey, workspace } = getActiveWorkspaceContext();
      if (!clearFocusedTerminal(teamKey)) flashBlockedNavForWorkspace(workspace?.id, "pulse");
    },
  },
  // ⌘⌥1..9 → the focused tile's tab N (9 = last). ⌘1..9 stays the workspace jump.
  // Not ⌃1..9: `CmdOrCtrl` also matches Ctrl, so ⌃N is already the workspace
  // jump, and Ctrl+2..8 are live control characters in the shell.
  ...Array.from({ length: JUMP_TERMINAL_TAB_COUNT }, (_, i): ShortcutAction => {
    const n = i + 1;
    return {
      id: jumpTerminalTabActionId(n),
      macCmdOnly: true,
      label: n === 9 ? "Jump to Last Terminal Tab" : `Jump to Terminal Tab ${n}`,
      menuPath: ["Go", n === 9 ? "Jump to Last Terminal Tab" : `Jump to Terminal Tab ${n}`],
      defaultKeystroke: `CmdOrCtrl+Alt+${n}`,
      scope: "workspace",
      run: () => {
        const { teamKey, workspace } = getActiveWorkspaceContext();
        if (!workspace) return;
        if (!jumpToTerminalTab(teamKey, n)) flashBlockedNavForWorkspace(workspace.id, "pulse");
      },
    };
  }),
  {
    // One-action agent launch: the default launch profile on the story in focus
    // (the focused story workspace, else the open story file) — the same action
    // as the first "Launch agent" item on a board card / nav row.
    id: "story.launch-agent",
    label: "Launch Agent on Story",
    menuPath: ["File", "Launch Agent on Story"],
    defaultKeystroke: "CmdOrCtrl+Shift+A",
    scope: "team",
    run: () => {
      // Loaded on demand: the launch module pulls in the dashboard launch spine.
      void import("@/features/story-launch/story-agent-launch")
        .then((m) => m.launchDefaultAgentForActiveStory())
        .catch((err: unknown) => toast.error(`Couldn't launch agent: ${String(err)}`));
    },
  },
];

// Every registry dispatch is an explicit keyboard/menu command, including
// rebound keys and native accelerators that never emit a document keydown.
export const SHORTCUT_ACTIONS: readonly ShortcutAction[] = shortcutActions.map((action) => ({
  ...action,
  run: () => {
    beginContextNavigation();
    action.run();
  },
}));

/** A named keyboard-navigation axis: one dimension of CLI-workspace movement and
 *  the ordered commands that travel along it. */
export interface NavAxis {
  label: string;
  actionIds: readonly string[];
  /** False keeps the axis out of the native Go menu (Settings still lists it).
   *  For chords that act on whatever holds keyboard focus: a native accelerator
   *  would take the key before the page, so ⌘F could no longer reach doc find. */
  inNativeMenu?: boolean;
}

/** The keyboard-navigation axes, in the order a discoverable surface should show
 *  them. This is the single authoritative grouping over the nav commands in
 *  {@link SHORTCUT_ACTIONS}: the in-app shortcuts reference reads it so the axes
 *  can never drift from the registry (each id resolves its live, rebound key
 *  through the registry, not a copied string). Non-nav actions are intentionally
 *  absent — this describes movement, not the whole command set. */
export const NAV_AXES: readonly NavAxis[] = [
  {
    label: "Team History",
    actionIds: ["nav.context-back", "nav.context-forward"],
  },
  {
    label: "Story Workspace",
    actionIds: ["viewer.cycle-workspace-next", "viewer.cycle-workspace-prev"],
  },
  {
    label: "Terminal Tab",
    actionIds: ["viewer.cycle-terminal-tab-next", "viewer.cycle-terminal-tab-prev"],
  },
  {
    label: "Epic Group",
    actionIds: ["viewer.cycle-epic-next", "viewer.cycle-epic-prev"],
  },
  {
    label: "Pane Focus",
    actionIds: FOCUS_PANE_DIRECTIONS.map((d) => focusPaneActionId(d.dir)),
  },
  {
    label: "Workspace Position",
    actionIds: [
      "viewer.jump-root-workspace",
      ...Array.from({ length: JUMP_WORKSPACE_COUNT }, (_, i) => jumpWorkspaceActionId(i + 1)),
    ],
  },
  {
    label: "Recent Workspace",
    actionIds: ["viewer.flip-last-workspace"],
  },
  {
    label: "Terminal Tab Actions",
    actionIds: [
      "viewer.new-terminal-tab",
      "viewer.new-terminal-on-machine",
      "viewer.split-terminal",
      "viewer.reopen-closed-tab",
      "viewer.rename-terminal-tab",
      "viewer.find-in-terminal",
      "viewer.clear-terminal",
      ...Array.from({ length: JUMP_TERMINAL_TAB_COUNT }, (_, i) => jumpTerminalTabActionId(i + 1)),
    ],
    inNativeMenu: false,
  },
  // SW-UX (app-flow redesign): the management surfaces (virtual tabs) in the native
  // Go menu. Not a cycle-pair axis — each id is its own "open X" menu item.
  {
    label: "Management",
    actionIds: [
      "nav.open-workspaces",
      "nav.open-teams",
      "nav.open-machines",
      "nav.open-source-repos",
    ],
  },
];

/** The nine positional workspace jumps (`viewer.jump-workspace-1..9`). Omitted
 *  from the native Go menu — `Cmd+1..9` is a self-evident OS idiom — but kept in
 *  the Settings shortcuts reference. Derived from the same count + id format as
 *  the generated actions so the exclusion can't drift. */
const POSITIONAL_JUMP_IDS: ReadonlySet<string> = new Set(
  Array.from({ length: JUMP_WORKSPACE_COUNT }, (_, i) => jumpWorkspaceActionId(i + 1)),
);

/** One curated nav command surfaced in the native macOS Go menu. */
export interface NavMenuItem {
  id: string;
  /** The menu label, taken from the action's registry `menuPath`/`label`. */
  label: string;
}

/** The curated nav commands surfaced in the native macOS Go menu: every axis
 *  representative from {@link NAV_AXES} except the nine positional workspace
 *  jumps and the axes kept out of the menu (`inNativeMenu: false`). Derived from the axes — never hand-listed — so the menu can't drift
 *  from the registry; the frontend resolves each id's live accelerator at build
 *  time via `getEffectiveShortcut`, so no keystroke is duplicated here. */
export const NAV_MENU_ITEMS: readonly NavMenuItem[] = NAV_AXES.filter(
  (axis) => axis.inNativeMenu !== false,
).flatMap((axis) =>
  axis.actionIds
    .filter((id) => !POSITIONAL_JUMP_IDS.has(id))
    .map((id) => {
      const action = SHORTCUT_ACTIONS.find((a) => a.id === id);
      return { id, label: action?.menuPath?.[1] ?? action?.label ?? id };
    }),
);

/**
 * The dynamic layer: slot dashboards' declared commands, set as a whole whenever the
 * profile, the pane listing or a dashboard's active set changes. One registry with
 * {@link SHORTCUT_ACTIONS}: every matcher, conflict check and list reads
 * {@link allShortcutActions}.
 */
export const useDynamicShortcutActions = create<{
  actions: readonly ShortcutAction[];
  all: readonly ShortcutAction[];
}>(() => ({ actions: [], all: SHORTCUT_ACTIONS }));

export function setDynamicShortcutActions(actions: readonly ShortcutAction[]): void {
  useDynamicShortcutActions.setState({ actions, all: [...SHORTCUT_ACTIONS, ...actions] });
}

/** Built-in actions, then slot dashboards' commands in slot order. */
export function allShortcutActions(): readonly ShortcutAction[] {
  return useDynamicShortcutActions.getState().all;
}

/** The namespaced id of a dashboard command. */
export const dashboardCommandActionId = (dashboard: string, id: string): string =>
  `${dashboard}/${id}`;

/** Whether an override key belongs to a dashboard command, installed or not, so it
 *  survives uninstall and reinstall. */
export const isDashboardCommandActionId = (id: string): boolean => id.startsWith("dashboard:");

/** Why an action has no chord. */
export type UnboundReason =
  /** The person removed it (an override of `""`), or another command took it. */
  | { kind: "cleared" }
  /** The dashboard declared no default. */
  | { kind: "none" }
  /** The default is a chord the OS or the app reserves. */
  | { kind: "reserved"; wanted: string; message: string }
  /** The default is taken: built-ins win, then the earlier dashboard. */
  | { kind: "conflict"; wanted: string; winner: string };

export interface ResolvedBinding {
  /** `""` when unbound. */
  keystroke: string;
  unbound?: UnboundReason;
}

let resolvedCache: {
  shortcuts: Record<string, string>;
  actions: readonly ShortcutAction[];
  bindings: Map<string, ResolvedBinding>;
} | null = null;

/**
 * Every action's effective chord under `shortcuts`. An override wins (`""` clears it).
 * A dashboard command without one gets its declared default unless that is reserved,
 * or already held by a built-in, an explicit override or an earlier dashboard command;
 * then it is unbound and Settings lists it under Conflicts.
 */
export function resolveBindings(shortcuts: Record<string, string>): Map<string, ResolvedBinding> {
  const actions = allShortcutActions();
  if (resolvedCache?.shortcuts === shortcuts && resolvedCache.actions === actions)
    return resolvedCache.bindings;
  const bindings = new Map<string, ResolvedBinding>();
  const owners = new Map<string, string>();
  const bind = (id: string, keystroke: string) => {
    bindings.set(id, { keystroke });
    if (!owners.has(keystroke)) owners.set(keystroke, id);
  };
  const defaults: ShortcutAction[] = [];
  for (const action of actions) {
    const override = shortcuts[action.id];
    if (override === "") bindings.set(action.id, { keystroke: "", unbound: { kind: "cleared" } });
    else if (override !== undefined) bind(action.id, override);
    else if (!action.command) bind(action.id, action.defaultKeystroke);
    else defaults.push(action);
  }
  for (const action of defaults) {
    const wanted = action.defaultKeystroke;
    const reserved = wanted ? isReservedForDashboardCommand(wanted) : null;
    const winner = owners.get(wanted);
    if (!wanted) bindings.set(action.id, { keystroke: "", unbound: { kind: "none" } });
    else if (reserved)
      bindings.set(action.id, {
        keystroke: "",
        unbound: { kind: "reserved", wanted, message: reserved.message },
      });
    else if (winner)
      bindings.set(action.id, { keystroke: "", unbound: { kind: "conflict", wanted, winner } });
    else bind(action.id, wanted);
  }
  resolvedCache = { shortcuts, actions, bindings };
  return bindings;
}

/** The registered action with this id, built-in or dashboard command. */
export function findShortcutAction(id: string): ShortcutAction | undefined {
  return allShortcutActions().find((a) => a.id === id);
}

/** How a person reads an action's name: a dashboard command names its dashboard. */
export function shortcutActionLabel(action: ShortcutAction): string {
  return action.command ? `${action.command.dashboardTitle} → ${action.label}` : action.label;
}

/** {@link shortcutActionLabel} by id; the id itself when nothing is registered under it. */
export function shortcutLabelById(id: string): string {
  const action = findShortcutAction(id);
  return action ? shortcutActionLabel(action) : id;
}

/** Find the action whose current keystroke conflicts with `keystroke`. */
export function findConflict(
  keystroke: string,
  shortcuts: Record<string, string>,
  forActionId: string,
): string | null {
  for (const [id, binding] of resolveBindings(shortcuts)) {
    if (id !== forActionId && binding.keystroke === keystroke) return id;
  }
  return null;
}
