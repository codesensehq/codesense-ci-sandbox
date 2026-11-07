// SPDX-License-Identifier: MIT
import { useEffect, useRef, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { AppButton as Button } from "@/shared/components/app-button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { attachSourceRepoClone, isAddRepoError } from "@/tauri/commands";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore, promptIdentityFromRepoPath } from "@/shared/stores/viewer-store";
import { getCurrentUser } from "@/shared/lib/get-current-user";
import { logger } from "@/shared/lib/logger";
import { notifyLocalCloneAttached } from "@/features/attachment/lib/notify-clone-attached";
import { LOCAL_CLONE_EXPLAINER } from "@/shared/lib/onboarding-copy";
import { InlineAlert } from "@/components/ui/inline-alert";
import { userFacingErrorMessage } from "@/shared/lib/error-message";
import { FolderOpen } from "lucide-react";
import { AttachSuccessNotice } from "./attach-success-notice";

interface AttachLocalCloneDialogProps {
  /** Owner segment of the workspace-tracked repo we're attaching for. */
  owner: string;
  /** Repo segment of the workspace-tracked repo we're attaching for. */
  repo: string;
  /** Forge identity used to distinguish equal owner/repo slugs across hosts. */
  provider?: string;
  host?: string;
  /** Friendly label shown in the title (e.g. `"acme/api"`). */
  displayLabel: string;
  /** Synthetic repo key (e.g. `"workspace:9north:default:acme/api"`) — used by
   *  the auto-retry observer to detect when the synthetic's `localPath`
   *  populates after a successful attach. Optional: when omitted, no
   *  auto-retry is wired (still works for non-modal callers). */
  targetRepoPath?: string;
  /** Controlled-mode props. The dialog has no internal trigger. */
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

type DialogPhase =
  | { kind: "picker" }
  | { kind: "saving" }
  /** SW-LN-29: attached; says where context lands. */
  | { kind: "success"; managed: boolean };

/** Subscribes to `org-store.workspaceRepos[repoPath].localPath` and resolves
 *  `true` when it transitions from null/undefined to a non-empty string.
 *  Resolves `false` after `timeoutMs` if the transition never happens.
 *  Used by `AttachLocalCloneDialog` to fire the no-clone-retry callback at
 *  exactly the right moment after a successful attach. */
function waitForLocalPathPopulated(repoPath: string, timeoutMs: number): Promise<boolean> {
  return new Promise((resolve) => {
    // If already populated (rare race — soft refresh somehow completed first),
    // resolve immediately.
    const current = useOrgStore.getState().workspaceRepos[repoPath]?.localPath;
    if (typeof current === "string" && current.length > 0) {
      resolve(true);
      return;
    }
    let settled = false;
    const finish = (populated: boolean) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      unsubscribe();
      resolve(populated);
    };
    const unsubscribe = useOrgStore.subscribe((state) => {
      const next = state.workspaceRepos[repoPath]?.localPath;
      if (typeof next === "string" && next.length > 0) {
        finish(true);
      }
    });
    const timer = setTimeout(() => finish(false), timeoutMs);
  });
}

/** What to tell the user when the clone they picked can't be bound to this repo.
 *  The checks themselves run in Rust; this is the copy for each refusal. */
function attachRefusalMessage(e: unknown, picked: string): string {
  if (!isAddRepoError(e)) return `Couldn't save the attachment: ${userFacingErrorMessage(e)}`;
  switch (e.kind) {
    case "notAGitRepository":
      return `That folder isn't a git repository — ${picked} has no .git directory. Pick one you cloned with git.`;
    case "noOrigin":
      return `No git remote is set on ${picked} — CodeSense needs one to know which repo this is.`;
    case "unsupportedRemote":
      return `"${e.url}" cannot identify this repository: ${e.reason}`;
    case "notRepoRoot":
      return `${picked} is inside the git repository at ${e.root}. Pick ${e.root} itself.`;
    case "originMismatch":
      return `That clone is ${e.actual}, but this team's repo is ${e.expected}. Choose a clone of ${e.expected}.`;
    default:
      return `Couldn't save the attachment: ${userFacingErrorMessage(e)}`;
  }
}

/**
 * F-6 AC-8: lets the user attach a LOCAL CLONE PATH to a workspace-tracked
 * synthetic source repo. Writes to `attachments.yml` via `attachSourceRepoClone`
 * (NOT to the workspace registry — that's `AttachRepoDialog`'s job).
 *
 * Flow:
 *   1. User clicks "Browse" → directory picker.
 *   2. `attachSourceRepoClone` checks the folder is a git working tree whose
 *      `origin` is this repo and saves the binding — the same Rust core an
 *      agent's `attach_source_repo_path` uses. A refusal shows inline.
 *   3. On success, bump `attachmentsVersion` so `use-workspace-viewer-sync`
 *      re-resolves the synthetic's `localPath` without a workspace switch.
 */
export function AttachLocalCloneDialog({
  owner,
  repo,
  provider,
  host,
  displayLabel,
  targetRepoPath,
  open,
  onOpenChange,
}: AttachLocalCloneDialogProps) {
  const [phase, setPhase] = useState<DialogPhase>({ kind: "picker" });
  const [error, setError] = useState<string | null>(null);
  const targetProvider = (provider ?? "github").toLowerCase();
  // Track mount status to suppress setState-after-unmount warnings on
  // long-running IPC flows (P6 patch).
  const isMountedRef = useRef(true);
  const operationInFlightRef = useRef(false);
  const operationGenerationRef = useRef(0);
  const dialogSessionRef = useRef(0);
  useEffect(() => {
    isMountedRef.current = true;
    return () => {
      isMountedRef.current = false;
    };
  }, []);

  const resetAndClose = () => {
    dialogSessionRef.current += 1;
    if (!operationInFlightRef.current) {
      setPhase({ kind: "picker" });
      setError(null);
    }
    onOpenChange(false);
  };

  const handleBrowse = async () => {
    if (!isMountedRef.current || operationInFlightRef.current) return;
    const browseSession = dialogSessionRef.current;
    setError(null);
    // SW-MC-6 (AC-9 hardening): clear a prior "already attached" info alert when
    // the user starts a new attempt, so it can't linger under a later error.
    const picked = await openDialog({
      directory: true,
      title: `Select local clone for ${displayLabel}`,
    });
    if (!picked || typeof picked !== "string") return;
    // R2-P11 patch: guard post-await setState against unmount. The user
    // can close the dialog while origin-URL parsing is in flight.
    if (!isMountedRef.current || browseSession !== dialogSessionRef.current) return;

    await saveAndClose(picked);
  };

  const saveAndClose = async (pickedPath: string) => {
    if (operationInFlightRef.current) return;
    setPhase({ kind: "saving" });
    setError(null);
    const currentUser = getCurrentUser();
    if (!currentUser) {
      setError("No active user — please log in and try again.");
      setPhase({ kind: "picker" });
      return;
    }
    const operation = ++operationGenerationRef.current;
    const operationSession = dialogSessionRef.current;
    operationInFlightRef.current = true;
    try {
      await attachSourceRepoClone({
        githubUserId: currentUser.github_user_id,
        repo: { owner, repo, provider: targetProvider, host: host ?? null },
        path: pickedPath,
        // SW-FG-8 (AC-8): bind to the picked connection (defaults to active).
      });
    } catch (e) {
      logger.warn("attach-clone", `attachSourceRepoClone failed: ${e}`);
      if (operation === operationGenerationRef.current) {
        operationInFlightRef.current = false;
      }
      if (isMountedRef.current && operationSession === dialogSessionRef.current) {
        setError(attachRefusalMessage(e, pickedPath));
        setPhase({ kind: "picker" });
      } else if (isMountedRef.current) {
        setPhase({ kind: "picker" });
      }
      return;
    }
    await finishAttachSuccess(operation, operationSession, false);
  };

  /** Refresh local attachment consumers and resume a pending workspace action. */
  const finishAttachSuccess = async (
    operation: number,
    operationSession: number,
    managed: boolean,
  ) => {
    // Trigger workspace-sync soft refresh so the synthetic re-resolves.
    useOrgStore.getState().bumpAttachmentsVersion();
    // SW-OB-3 (AC-2): confirm via the shared helper so the copy matches the
    // silent-record path exactly. Use the workspace repo's identity (owner/
    // repo), not the picked path, so the message reads consistently.
    notifyLocalCloneAttached({ owner, repo });

    // Auto-retry: if a target synthetic was supplied, await its `localPath`
    // populating (workspace-sync soft refresh) then fire the pending
    // no-clone retry callback. Stay in `saving` phase during the wait so
    // the user sees "Saving…" not the picker UI.
    // SW-LN-29: whether a pending no-clone action was retried — then the
    // user's next step is already under way, so the dialog just closes.
    let retried = false;
    if (targetRepoPath) {
      // T.4 AC-3: noClonePromptRepo is `{teamKey, sourceRepoSlug}` now —
      // derive the target identity once for both guard comparisons.
      const targetIdentity = promptIdentityFromRepoPath(targetRepoPath);
      const promptMatchesTarget = () => {
        const current = useViewerStore.getState().noClonePromptRepo;
        if (!current || !targetIdentity) return false;
        return (
          current.teamKey === targetIdentity.teamKey &&
          current.sourceRepoSlug === targetIdentity.sourceRepoSlug
        );
      };
      try {
        const populated = await waitForLocalPathPopulated(targetRepoPath, 5000);
        if (populated) {
          // R3-P5: only fire the retry if the prompt is still for OUR repo.
          // If during the 5s wait the user dismissed the original modal and
          // triggered a fresh no-clone action for a DIFFERENT synthetic,
          // consuming would fire the unrelated repo's retry callback.
          if (promptMatchesTarget()) {
            useViewerStore.getState().consumeNoClonePromptRetry();
            retried = true;
          }
        } else {
          logger.warn(
            "attach-clone",
            `localPath did not populate within 5s for ${targetRepoPath} — skipping auto-retry`,
          );
          // R2-P3 patch: only dismiss if the prompt is still for OUR repo.
          if (promptMatchesTarget()) {
            useViewerStore.getState().dismissNoClonePrompt();
          }
        }
      } catch (e) {
        logger.warn("attach-clone", `auto-retry watcher errored: ${e}`);
      }
    }

    if (operation !== operationGenerationRef.current) return;
    operationInFlightRef.current = false;
    if (isMountedRef.current && operationSession === dialogSessionRef.current) {
      // SW-LN-29: a retried no-clone action is the user's next step, so close;
      // otherwise stay open on the success state that says where context lands.
      if (retried) resetAndClose();
      else setPhase({ kind: "success", managed });
    } else if (isMountedRef.current) {
      // Closing/reopening starts a new dialog session, but does not cancel the
      // original backend operation. Release its local busy state without letting
      // that older completion close the newly opened dialog.
      setPhase({ kind: "picker" });
    }
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(v) => {
        if (!v) resetAndClose();
        else onOpenChange(true);
      }}
    >
      <DialogContent aria-busy={phase.kind === "saving"}>
        <DialogHeader>
          <DialogTitle>Attach local clone for {displayLabel}</DialogTitle>
          <DialogDescription>
            Point CodeSense at the local clone of this team-tracked repo so worktrees, terminals,
            and editor actions can find the source code. {LOCAL_CLONE_EXPLAINER}
          </DialogDescription>
        </DialogHeader>

        {phase.kind === "picker" && (
          <div className="flex flex-col gap-4 py-2">
            <Button onClick={handleBrowse} variant="outline" className="w-full">
              <FolderOpen size={14} className="mr-1.5" />
              Browse for clone directory
            </Button>
            {error && <InlineAlert severity="error">{error}</InlineAlert>}
          </div>
        )}

        {phase.kind === "success" && (
          <AttachSuccessNotice label={displayLabel} managed={phase.managed} />
        )}

        {phase.kind === "saving" && (
          <p role="status" aria-live="polite" className="text-sm text-muted-foreground">
            Finishing this repository connection… You can close this dialog and keep working.
          </p>
        )}

        <DialogFooter>
          <Button variant="outline" onClick={resetAndClose}>
            {phase.kind === "saving" ? "Close" : phase.kind === "success" ? "Done" : "Cancel"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
