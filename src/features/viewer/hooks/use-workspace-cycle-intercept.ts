// SPDX-License-Identifier: MIT
import { useEffect } from "react";

import { focusedWorkspaceOf } from "@/shared/hooks/use-active-workspace-context";
import { isRowPopupFocused, editorOwnsShortcut } from "@/shared/lib/focus-utils";
import { isDispatchable } from "@/shared/lib/shortcuts/is-dispatchable";
import { matchesKeystroke } from "@/shared/lib/shortcuts/keystroke";
import { SHORTCUT_ACTIONS } from "@/shared/lib/shortcuts/registry";
import { getActiveTeamKey, NO_TEAM_SENTINEL } from "@/shared/lib/team-key";
import { getEffectiveShortcut } from "@/shared/stores/shortcuts-store";
import { useUiStore } from "@/shared/stores/ui-store";
import { getViewerState } from "@/features/viewer/lib/cli-workspace/store/viewer-access";

import { CLI_HOST_SELECTOR } from "../lib/cli-host";

/**
 * Document-level capture-phase keydown intercept for the navigation shortcuts
 * that would otherwise leak into the PTY — the critical AC-20 mitigation.
 *
 * These chords (Cmd+Opt+brackets, Cmd+Opt+arrows, digits, Cmd+Shift+brackets,
 * Cmd+Shift+Backspace) collide with shell-internal input (zsh/bash/vim/tmux
 * word-nav, cursor keys, digit combos) if they reach xterm.js. Registering at
 * document level with `capture: true` consumes the keystroke BEFORE xterm's own
 * keydown listener, so nothing leaks to the PTY.
 *
 * Gated on `event.target.closest('[data-xterm-host]')` with a matching
 * `data-repo-path` — only fires when the keystroke originates inside THIS repo's
 * terminal host. This covers both shared-mode and console-mode terminal subtrees
 * with a single mount site, eliminating ref-plumbing across layout modes.
 *
 * Every recognized chord dispatches through the registry action's `run()`, so the
 * command behavior lives in exactly ONE place (the registry) — the capture-phase
 * path and the bubble-phase `useShortcutHandler` invoke the same function. Adding
 * a new intercepted command is a single entry in the list below.
 *
 * Non-terminal keystrokes fall through to the bubble-phase `useShortcutHandler`. So does every
 * keystroke while focus is in a row popup of the Agents pane (the quick view, the inbox chat),
 * whose gate blocks the CLI's verbs there. A null `repoPath` (no active team) listens for
 * nothing. An empty repo path is a valid generic workspace.
 */

/** The registry action ids caught at capture phase before xterm sees them. Order
 *  is the match precedence (conflicts are prevented at the registry level via
 *  `findConflict`, so it only matters for genuine collisions). Exported so a parity
 *  test can assert every id resolves to a real `SHORTCUT_ACTIONS` entry — a drifted
 *  id would `preventDefault` the chord yet run nothing (silent key loss). */
export const PTY_INTERCEPTED_ACTION_IDS: readonly string[] = [
  "nav.context-back",
  "nav.context-forward",
  "viewer.cycle-workspace-next",
  "viewer.cycle-workspace-prev",
  "viewer.cycle-epic-next",
  "viewer.cycle-epic-prev",
  "viewer.cycle-terminal-tab-next",
  "viewer.cycle-terminal-tab-prev",
  "viewer.flip-last-workspace",
  ...Array.from({ length: 9 }, (_, i) => `viewer.jump-workspace-${i + 1}`),
  "viewer.focus-pane-left",
  "viewer.focus-pane-right",
  "viewer.focus-pane-up",
  "viewer.focus-pane-down",
  "viewer.hard-close-workspace",
  // Terminal tab verbs. ⌘T / ⌘F reach the bubble handler from xterm too, but are
  // claimed here so the viewer's own ⌘T / ⌘F listeners never also act on them;
  // ⌘⌥digits would otherwise be written to the PTY. Not F2 (rename): in the
  // terminal it is the running program's key; the bubble handler takes it on a pill.
  "viewer.new-terminal-tab",
  "viewer.new-terminal-on-machine",
  "viewer.split-terminal",
  "viewer.reopen-closed-tab",
  "viewer.find-in-terminal",
  "viewer.clear-terminal",
  "viewer.toggle-terminal-zoom",
  "viewer.focus-tile-question",
  ...Array.from({ length: 9 }, (_, i) => `viewer.jump-terminal-tab-${i + 1}`),
];

export function useWorkspaceCycleIntercept(repoPath: string | null): void {
  useEffect(() => {
    if (repoPath === null) return;

    const handler = (event: KeyboardEvent) => {
      if (event.isComposing) return;
      // Ctrl+[ is Escape for terminal programs; user overrides must preserve it too.
      if (
        event.ctrlKey &&
        !event.metaKey &&
        !event.altKey &&
        !event.shiftKey &&
        (event.key === "[" || event.key === "]")
      )
        return;
      // Only intercept inside terminal host elements belonging to this repo.
      const target = event.target as Element | null;
      const host = target?.closest?.(CLI_HOST_SELECTOR);
      if (!host || host.getAttribute("data-repo-path") !== repoPath) return;

      const teamKey = getActiveTeamKey();
      const gate = {
        hasTeam: teamKey !== NO_TEAM_SENTINEL,
        hasActiveWorkspace: focusedWorkspaceOf(getViewerState(), teamKey) !== null,
        settingsPanelOpen: useUiStore.getState().settingsPanelOpen,
        rowPopupFocused: isRowPopupFocused(),
      };
      for (const id of PTY_INTERCEPTED_ACTION_IDS) {
        const action = SHORTCUT_ACTIONS.find((a) => a.id === id);
        if (!action || !isDispatchable(action, gate) || editorOwnsShortcut(id, event.target))
          continue;
        const keystroke = getEffectiveShortcut(id);
        // ⌘ only on macOS: a Control chord from the terminal is shell input.
        if (keystroke && matchesKeystroke(event, keystroke, { macCmdOnly: true })) {
          event.preventDefault();
          event.stopPropagation();
          action.run();
          return;
        }
      }
    };

    document.addEventListener("keydown", handler, { capture: true });
    return () => document.removeEventListener("keydown", handler, { capture: true });
  }, [repoPath]);
}
