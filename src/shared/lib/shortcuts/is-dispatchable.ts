// SPDX-License-Identifier: MIT
import type { ShortcutAction } from "./registry";

/**
 * The runtime state the dispatchability gate reads. Injected by the caller so
 * the predicate stays pure and framework-free: the keyboard handler
 * (`useShortcutHandler`) supplies hook-derived values, while the native
 * `menu:action` dispatcher — a raw Tauri event closure outside the React tree —
 * supplies the same values from imperative `getState()` reads. One gate, two
 * state sources, so the keyboard and menu paths can never drift.
 */
export interface DispatchGateState {
  /** The Settings tab is the active viewer tab. Only the viewer column shows
   *  Settings: the nav, tab strip, terminal section, tray and right pane stay
   *  on screen around it. */
  settingsPanelOpen: boolean;
  /** A workspace is focused in the active team: what a `workspace`-scoped action needs. */
  hasActiveWorkspace: boolean;
  /** A team is open (even with an empty/minimized roster). */
  hasTeam: boolean;
  /** Keyboard focus is in a row popup of the Agents pane (`isRowPopupFocused`), read when the key
   *  or menu item fires. */
  rowPopupFocused: boolean;
}

// On the Settings tab team scope is a denylist and workspace scope an allowlist: team
// actions mostly target chrome that stays on screen, while workspace actions split
// between the terminal section and the viewer document, so a new one starts blocked.
// Both sets are pinned by registry-wide tables in the tests.

/**
 * Team actions that stay blocked while the Settings tab is active: they act on
 * the active viewer document, and Settings is not a document.
 * - `nav.goto-heading`: jumps among the active note's headings; Settings has none.
 * - `viewer.edit-file`: enters edit mode on the active file; Settings is not a file.
 */
export const SETTINGS_TAB_BLOCKED: ReadonlySet<string> = new Set([
  "nav.goto-heading",
  "viewer.edit-file",
]);

/**
 * Workspace actions that dispatch while the Settings tab is active: each acts on the
 * terminal section (the focused tile, its tabs and panes, the tiles' own overlays
 * and insets, the workspace tray), which stays on screen beneath the tab. An
 * allowlist, so a new workspace action stays off the Settings tab until someone
 * decides its target is visible there. Left out: `viewer.export-pdf`, which exports
 * the active viewer document (Settings is not a document).
 */
export const SETTINGS_TAB_WORKSPACE_ALLOWED: ReadonlySet<string> = new Set([
  "viewer.toggle-story-hud",
  "viewer.toggle-story-hud-size",
  "viewer.toggle-source-diff",
  "viewer.toggle-code-editor",
  "viewer.toggle-agent-inbox",
  "viewer.open-group-chat",
  "viewer.toggle-terminal-zoom",
  "viewer.cycle-workspace-next",
  "viewer.cycle-workspace-prev",
  "viewer.cycle-terminal-tab-next",
  "viewer.cycle-terminal-tab-prev",
  "viewer.cycle-epic-next",
  "viewer.cycle-epic-prev",
  "viewer.focus-pane-left",
  "viewer.focus-pane-right",
  "viewer.focus-pane-up",
  "viewer.focus-pane-down",
  "viewer.jump-root-workspace",
  "viewer.close-workspace",
  "viewer.hard-close-workspace",
  "viewer.new-terminal-tab",
  "viewer.new-terminal-on-machine",
  "viewer.split-terminal",
  "viewer.rename-terminal-tab",
  "viewer.find-in-terminal",
  "viewer.clear-terminal",
  ...Array.from({ length: 9 }, (_, i) => `viewer.jump-terminal-tab-${i + 1}`),
]);

/**
 * The only actions that fire while focus is in a row popup of the Agents pane (the quick view, the
 * inbox chat). Every other shortcut is blocked there, so a key meant for the popup never opens,
 * closes, cycles, jumps, zooms or rearranges anything in the CLI, its tiles, the tray or the
 * workspaces behind it. These are global and change none of that: Settings, the command palette,
 * and the management tabs (Machines, Source Repos, Teams, context repos). Not the org and team
 * switchers, which change the team the tray and workspaces belong to.
 */
export const ROW_POPUP_ALLOWED: ReadonlySet<string> = new Set([
  "settings.open",
  "nav.open-command-palette",
  "nav.open-machines",
  "nav.open-source-repos",
  "nav.open-teams",
  "nav.open-workspaces",
]);

/**
 * Whether `action` may fire right now. The single gate shared by the keyboard
 * handler and the native-menu dispatcher.
 *
 * - `workspace` scope needs a focused workspace in the active team. On the Settings tab only
 *   `SETTINGS_TAB_WORKSPACE_ALLOWED` dispatches (the terminal-section actions);
 * - `team` scope needs an open team. On the Settings tab only `SETTINGS_TAB_BLOCKED`
 *   is suppressed;
 * - `global` dispatches whatever the state;
 * - a dashboard command also needs its own `available()`: active, and its scope
 *   (`global`, `slot-visible`, `slot-focused`) matching where its dashboard is;
 * - with focus in a row popup (`state.rowPopupFocused`) only `ROW_POPUP_ALLOWED` does.
 */
export function isDispatchable(action: ShortcutAction, state: DispatchGateState): boolean {
  // A dashboard command's own gate: it is active and its scope matches.
  if (action.available && !action.available()) return false;
  if (!ROW_POPUP_ALLOWED.has(action.id) && state.rowPopupFocused) return false;
  if (action.scope === "workspace") {
    return (
      state.hasActiveWorkspace &&
      (!state.settingsPanelOpen || SETTINGS_TAB_WORKSPACE_ALLOWED.has(action.id))
    );
  }
  if (action.scope === "team") {
    return state.hasTeam && !(state.settingsPanelOpen && SETTINGS_TAB_BLOCKED.has(action.id));
  }
  return true;
}
