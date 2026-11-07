// SPDX-License-Identifier: MIT
import { useEffect } from "react";

import { matchesKeystroke, parseKeystroke } from "@/shared/lib/shortcuts/keystroke";
import { allShortcutActions, resolveBindings } from "@/shared/lib/shortcuts/registry";
import { isDispatchable, type DispatchGateState } from "@/shared/lib/shortcuts/is-dispatchable";
import {
  isRowPopupFocused,
  isTextEditorTarget,
  editorOwnsShortcut,
  getActivePaneType,
  isTerminalFocused,
} from "@/shared/lib/focus-utils";
import { useShortcutsStore } from "@/shared/stores/shortcuts-store";
import { useUiStore } from "@/shared/stores/ui-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { focusedWorkspaceOf } from "@/shared/hooks/use-active-workspace-context";
import { NO_TEAM_SENTINEL, useActiveTeamKey } from "@/shared/lib/team-key";

/**
 * Global keyboard handler driven by the shortcut action registry +
 * user overrides from `useShortcutsStore`. Replaces the per-shortcut
 * inline `if (e.metaKey && e.key === …)` branches that used to live in
 * `App.tsx` and `RepoViewerView`.
 *
 * Mount ONCE at the app root. A workspace-scoped action (`scope:
 * "workspace"`) is skipped entirely when no workspace is focused in the
 * active team. The handler does not match, does not `preventDefault`, and the event falls
 * through to whatever would normally handle it (browser / OS). This is the
 * "suppress entirely" behavior: eating a key that couldn't do anything
 * meaningful is worse than leaving it alone.
 *
 * On the Settings tab (a viewer tab; the rest of the team view stays on
 * screen around it) the gate dispatches the team actions outside
 * `SETTINGS_TAB_BLOCKED` and the terminal-section workspace actions in
 * `SETTINGS_TAB_WORKSPACE_ALLOWED`; the document-only ones stay skipped.
 * Behind the full-screen inbox, of the team and workspace actions only its
 * toggle, ⌘\ and ⌘B get through. Global actions (⌘,, switch-org, etc.)
 * always dispatch, on the Settings tab and behind the inbox alike. See
 * `isDispatchable`, which the native menu shares.
 *
 * Bubble phase (not capture) so that in-tree handlers that call
 * `stopPropagation()` — notably the Settings capture field while
 * rebinding — can legitimately claim a key before the global
 * dispatcher sees it. Editors (Monaco / xterm) get the same escape
 * hatch for their own shortcuts.
 *
 * IME composition is suppressed: keydowns fired during a CJK /
 * dead-key composition carry `isComposing=true` and should never
 * trigger app shortcuts.
 *
 * A key a focused text editor (CodeMirror, Tiptap/ProseMirror, any
 * contenteditable) already handled — it called `preventDefault()` — is
 * the editor's: ⌘I in a note is italic, not also the inbox toggle.
 */
export function useShortcutHandler(): void {
  const shortcuts = useShortcutsStore((s) => s.shortcuts);
  // Workspace-scoped actions need a workspace focused in the active team, read the way the
  // native menu reads it. Presence only, selected as booleans: the handler never reads workspace fields, so swapping the
  // focused workspace for another re-renders nothing and re-subscribes nothing.
  const teamKey = useActiveTeamKey();
  const hasActiveWorkspace = useViewerStore((s) => focusedWorkspaceOf(s, teamKey) !== null);
  // `team`-scoped actions (tray nav) fire whenever a team is open, even with no focused
  // workspace (the empty/minimized-roster state).
  const hasTeam = teamKey !== NO_TEAM_SENTINEL;
  const settingsPanelOpen = useUiStore((s) => s.settingsPanelOpen);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.isComposing) return;
      if (
        e.ctrlKey &&
        !e.metaKey &&
        !e.altKey &&
        !e.shiftKey &&
        (e.key === "[" || e.key === "]") &&
        e.target instanceof Element &&
        e.target.closest(".xterm")
      )
        return;
      if (e.defaultPrevented && isTextEditorTarget(e.target)) return;

      // Shared dispatchability gate: a key blocked here matches what the native
      // menu blocks — see `isDispatchable`. Where focus is is read as the key fires.
      const gate = {
        settingsPanelOpen,
        hasActiveWorkspace,
        hasTeam,
        rowPopupFocused: isRowPopupFocused(),
      };
      const action = matchShortcut(e, shortcuts, gate);
      if (!action) return;
      e.preventDefault();
      action.run();
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [shortcuts, hasActiveWorkspace, hasTeam, settingsPanelOpen]);
}

/** Match a key using the current bindings and the same UI gate for host and frame keys. */
export function matchShortcut(
  e: KeyboardEvent,
  shortcuts: Record<string, string>,
  gate: DispatchGateState,
  editorTarget = isTextEditorTarget(e.target),
  textFieldTarget = false,
) {
  if (e.isComposing) return;
  const bindings = resolveBindings(shortcuts);
  const terminal = isTerminalFocused();
  const textField =
    editorTarget ||
    textFieldTarget ||
    (e.target instanceof Element && e.target.closest("input, textarea, select") !== null);
  for (const action of allShortcutActions()) {
    const keystroke = bindings.get(action.id)?.keystroke;
    if (!keystroke) continue;
    // A dashboard command leaves a terminal only on a ⌘ chord, the same rule as
    // `macCmdOnly`: plain, ⌃, ⌥, function-key and Esc keys reach the PTY. In a text
    // field its ⌥ and unmodified chords type characters, so only ⌘ chords fire.
    const commandNeedsCmd = !!action.command && (terminal || textField);
    if (commandNeedsCmd && !parseKeystroke(keystroke).cmdOrCtrl) continue;

    // Editor bracket keys keep their indent/outdent behavior even when the editor did
    // not consume the event itself.
    if (editorOwnsShortcut(action.id, e.target, editorTarget)) continue;
    // A frame's focused field is invisible to the split action's host-DOM check.
    if (action.id === "viewer.split-terminal" && (editorTarget || textFieldTarget)) continue;
    // The viewer owns shifted brackets outside the terminal; its own listener may
    // run before or after this one. Named terminal menu actions stay available.
    if (
      (action.id === "viewer.cycle-terminal-tab-next" ||
        action.id === "viewer.cycle-terminal-tab-prev") &&
      (e.defaultPrevented || getActivePaneType() !== "terminal")
    )
      continue;
    const macCmdOnly = action.macCmdOnly || (!!action.command && terminal);
    if (!matchesKeystroke(e, keystroke, { macCmdOnly })) continue;
    // The gate last: a dashboard command's scope check reads frames and the layout.
    if (!isDispatchable(action, gate)) continue;

    return action;
  }
}
