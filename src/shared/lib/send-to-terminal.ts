// SPDX-License-Identifier: MIT
import {
  getViewerState,
  promptIdentityFromRepoPath,
  subscribeViewerState,
} from "@/features/viewer/lib/cli-workspace/store/viewer-access";
import { useOrgStore } from "@/shared/stores/org-store";
import { resolveRepo } from "@/shared/lib/resolve-repo";
import { resolveSourceRepoPath } from "@/shared/lib/resolve-source-repo-path";
import {
  getActiveWorkspaceTabs,
  getActiveWorkspaceActiveTab,
} from "@/features/viewer/hooks/use-active-workspace-terminal-tabs";
import { getActiveWorkspaceContext } from "@/shared/hooks/use-active-workspace-context";
import { getKindCapabilities } from "@/shared/lib/workspace-kind-capabilities";
import {
  writePty,
  submitPtyLine,
} from "@/features/viewer/lib/cli-workspace/transport/terminal-session-source";
import { toast } from "sonner";
import { getErrorMessage } from "@/shared/lib/error-message";
import { SESSION_NOT_FOUND } from "@/features/viewer/lib/terminal-attach";

const NO_WORKSPACE_TOAST = "No workspace open";
const NO_TERMINAL_TOAST = "No terminal active";
const EPIC_STARTING_TOAST =
  "Epic workspace is still starting its terminal — try again in a moment.";

/** Send text to the currently active terminal.
 *
 *  T.6: reads the active workspace's `sourceRepoPath` via
 *  `getActiveWorkspaceContext()` — the action targets whichever workspace
 *  is currently focused in the active team's bar.
 *
 *  If the focused workspace's source repo is a synthetic workspace repo
 *  with no attached clone, surfaces the no-clone modal with an auto-retry
 *  callback so the user can attach + have the send re-fire automatically. */
export function sendTextToTerminal(text: string, isRetry = false): void {
  // T.6: `repoPath` is the synthetic workspace key, NOT
  // `workspace.sourceRepoPath` (M.2 on-disk clone path). `resolveRepo` +
  // `promptIdentityFromRepoPath` are keyed on the synthetic form.
  const { teamKey, repoPath, workspace } = getActiveWorkspaceContext();
  if (!workspace) {
    toast(NO_WORKSPACE_TOAST, { duration: 3000 });
    return;
  }

  // SW-AW-5 (AC-2): real-dir-cwd kinds skip the synthetic-clone attach gate.
  // A slug-less workspace (epic → cwd=epic dir, git-sync → cwd=org_root, root →
  // cwd=repo root) has a real terminal cwd and no source clone, so its identity
  // `repoPath` is null/"" and the no-clone-attach gate below is both inapplicable
  // and unreachable for it — skip straight to resolving its own active terminal
  // tab. Only `story` (cwd = a synthetic-clone that must be attached first)
  // consults the gate. Replaces the old `kind === "epic"` literal, which routed
  // git-sync into the `!repoPath` "No workspace open" early-return (deferred-work.md:6).
  const caps = getKindCapabilities(workspace.kind);

  if (!caps.skipsCloneGate) {
    if (!repoPath) {
      toast(NO_WORKSPACE_TOAST, { duration: 3000 });
      return;
    }

    // Gate on synthetic-no-clone. For legacy repos, repoPath is itself
    // a real path and `resolveSourceRepoPath` returns it unchanged.
    const repo = resolveRepo(repoPath);
    if (repo && resolveSourceRepoPath(repo) === null) {
      // If we got here via the auto-retry, the attach was supposed
      // to populate `localPath`. Returning null again means the attached
      // clone didn't actually match the synthetic.
      if (isRetry) {
        toast.error("Couldn't send to terminal — attached clone doesn't resolve.");
        return;
      }
      // Capture `repoPath` so the auto-retry verifies the user
      // is still on the same workspace at fire time.
      const capturedRepoPath = repoPath;
      const promptIdentity = promptIdentityFromRepoPath(repoPath);
      if (promptIdentity) {
        getViewerState().showNoClonePrompt(
          promptIdentity.teamKey,
          promptIdentity.sourceRepoSlug,
          () => {
            const currentRepoPath = getActiveWorkspaceContext().repoPath;
            if (currentRepoPath !== capturedRepoPath) {
              return;
            }
            sendTextToTerminal(text, true);
          },
        );
      } else {
        toast.error("No local clone attached for this workspace repo");
      }
      return;
    }
  }

  const activeTab = activeTabOf(teamKey);
  if (!activeTab) {
    noTerminalToast(workspace.kind);
    return;
  }

  // Fire-and-forget from the active-tab command path: swallow the transient
  // spawn/attach-window race quietly (SW-EW-39), surface genuine failures.
  void sendTextToSession(activeTab.sessionId, text).catch((e: unknown) => {
    const msg = getErrorMessage(e);
    if (msg.includes(SESSION_NOT_FOUND)) return;
    toast.error(`Failed to send to terminal: ${msg}`);
  });
}

/** The selected terminal tab of the team's focused workspace: the terminal the person
 *  used last there, which stays selected while focus is in a pane. */
function activeTabOf(teamKey: string) {
  const activeTabId = getActiveWorkspaceActiveTab(teamKey);
  return activeTabId
    ? getActiveWorkspaceTabs(teamKey).find((t) => t.id === activeTabId)
    : undefined;
}

/** Shows the no-terminal toast for this kind of workspace and returns its text. */
function noTerminalToast(kind: string): string {
  if (kind === "epic") {
    // SW-FX-28 (AC-4): the only legitimate no-target case — the epic dir
    // hasn't resolved yet, so no rooted terminal exists (SW-EW-7 won't
    // `chdir("")`). Surface an actionable, non-transient signal rather than
    // the silent 3s flash. (Epic-specific display string — a per-kind render
    // concern, distinct from the behavioral `skipsCloneGate` capability above.)
    toast.error(EPIC_STARTING_TOAST);
    return EPIC_STARTING_TOAST;
  }
  toast(NO_TERMINAL_TOAST, { duration: 3000 });
  return NO_TERMINAL_TOAST;
}

/** What a paste into the active terminal did: typed (written as is), or why nothing was. */
export type PasteStatus = "typed" | "no-workspace" | "no-terminal";

/** A paste's status and, when it typed nothing, the text of the toast the host showed. */
export type PasteOutcome = { status: PasteStatus; message: string | null };

/** The active tab while its process runs: an exited tab stays open read-only. */
function pasteTabOf(teamKey: string) {
  const tab = activeTabOf(teamKey);
  return tab && !tab.exit ? tab : undefined;
}

/** Whether `pasteToActiveTerminal` has a terminal to type into right now. */
export function canPasteToTerminal(): boolean {
  const { teamKey, workspace } = getActiveWorkspaceContext();
  return workspace !== null && pasteTabOf(teamKey) !== undefined;
}

/** Calls `listener` with the new answer each time `canPasteToTerminal()` flips; returns the
 *  unsubscribe. The answer follows the active team, its focused workspace and that workspace's
 *  selected terminal tab and whether its process still runs. */
export function subscribePasteTarget(listener: (canPaste: boolean) => void): () => void {
  let last = canPasteToTerminal();
  const check = () => {
    const next = canPasteToTerminal();
    if (next === last) return;
    last = next;
    listener(next);
  };
  // The viewer store changes on every status and progress tick; only these slices move the answer.
  const offViewer = subscribeViewerState((s, prev) => {
    if (
      s.activeWorkspace !== prev.activeWorkspace ||
      s.workspaces !== prev.workspaces ||
      s.softClosedWorkspaces !== prev.softClosedWorkspaces ||
      s.terminalLayout !== prev.terminalLayout
    )
      check();
  });
  const offOrg = useOrgStore.subscribe((s, prev) => {
    if (s.activeOrgSlug !== prev.activeOrgSlug || s.activeTeamName !== prev.activeTeamName) check();
  });
  return () => {
    offViewer();
    offOrg();
  };
}

/** Writes `text` into the active terminal as typed, adding no Enter (a newline in it acts as
 *  Enter), and reports what happened. Unlike `sendTextToTerminal` it never opens the
 *  attach-a-clone prompt: it pastes exactly when `canPasteToTerminal()` is true. With nothing
 *  to paste into it shows the same toast and returns its text. */
export async function pasteToActiveTerminal(text: string): Promise<PasteOutcome> {
  const { teamKey, workspace } = getActiveWorkspaceContext();
  if (!workspace) {
    toast(NO_WORKSPACE_TOAST, { duration: 3000 });
    return { status: "no-workspace", message: NO_WORKSPACE_TOAST };
  }
  const activeTab = pasteTabOf(teamKey);
  if (!activeTab) {
    return { status: "no-terminal", message: noTerminalToast(workspace.kind) };
  }
  try {
    await sendTextToSession(activeTab.sessionId, text);
  } catch (e) {
    const msg = getErrorMessage(e);
    // The tab's session ended between the lookup and the write.
    if (msg.includes(SESSION_NOT_FOUND)) {
      return { status: "no-terminal", message: noTerminalToast(workspace.kind) };
    }
    throw e;
  }
  return { status: "typed", message: null };
}

/**
 * Send text to a SPECIFIC PTY session by id, then (by default) focus the
 * visible terminal.
 *
 * The `writePty`-targeting half of `sendTextToTerminal`, extracted so callers
 * that already KNOW the session (SW-GS-1's Git Sync chat box targets the BOUND
 * git-sync session — AC-18 — not the active tab) can address it directly
 * instead of going through active-workspace resolution.
 *
 * `focusVisibleTerminal` defaults to `true` (preserves `sendTextToTerminal`'s
 * behavior). Callers whose target session is NOT the visible terminal (e.g. the
 * chat box, whose own textarea should keep focus) pass `false` so the post-write
 * focus hop doesn't yank focus onto an unrelated visible xterm.
 *
 * SW-GS-23: returns a `Promise<void>` that RESOLVES only once the write is
 * flushed and REJECTS on any write failure (including a gone session) — so a
 * caller that awaits can clear/record the message only on confirmed delivery and
 * never records a phantom "sent". Fire-and-forget callers `void`-call it and
 * handle the transient race themselves. The `{ submit }` path routes through
 * `submitPtyLine` (settle → write → confirm-from-output → discrete-Enter nudge),
 * so the trailing Enter reliably registers; `submitPtyLine` owns the submit key,
 * so submit callers pass the RAW message (no hand-appended `\r`). The raw path is
 * an immediate `writePty` (arbitrary terminal input, no auto-submit).
 */
export function sendTextToSession(
  sessionId: string,
  text: string,
  {
    focusVisibleTerminal = true,
    submit = false,
  }: { focusVisibleTerminal?: boolean; submit?: boolean } = {},
): Promise<void> {
  // Schedule the post-send focus hop up front so awaiting the write never delays
  // or drops it (rAF defers to the next frame regardless of the write's timing).
  if (focusVisibleTerminal) {
    requestAnimationFrame(() => {
      const panels = document.querySelectorAll<HTMLElement>(".xterm textarea");
      for (const el of panels) {
        const container = el.closest<HTMLElement>("[style*='display']");
        if (!container || container.style.display !== "none") {
          el.focus();
          return;
        }
      }
    });
  }

  // Await + propagate failure (no internal swallow): a dropped write must not
  // look like success to an awaiting caller. The chat/conflict send only needs
  // delivery (reject-on-failure), not the SW-GS-30 `{ submitted }` confirmation —
  // discard it to preserve the `Promise<void>` contract. (The guarded auto-inject
  // path consumes `submitted` via the injector's `submit` seam instead.)
  return submit ? submitPtyLine(sessionId, text).then(() => undefined) : writePty(sessionId, text);
}
