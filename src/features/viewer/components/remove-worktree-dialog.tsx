// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";
import { toast } from "sonner";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import {
  checkWorktreeDirty,
  dashboardProbeSessionsAlive,
  getWorktreeUnpublishedContext,
  removeWorktree,
  worktreeLiveSessionCount,
  type WorktreeRow,
} from "@/tauri/commands";
import { getErrorMessage } from "@/shared/lib/error-message";
import type { Workspace } from "@/shared/stores/viewer-store";
import {
  getViewerState,
  selectHiddenTerminalTabs,
  selectWorkspaceTabs,
} from "@/features/viewer/lib/cli-workspace/store/viewer-access";
import { endWorkspace } from "../lib/terminal-close";

/** The worktree a workspace's Remove acts on: the workspace plus its roster row. */
export interface RemoveWorktreeTarget {
  teamKey: string;
  workspaceId: string;
  /** The worktree folder (the workspace's cwd). */
  path: string;
  branch: string | null;
  /** The clone the worktree belongs to (`git worktree remove` runs there). */
  repoPath: string;
  sourceRepoSlug?: string;
  storyFilePath?: string;
}

const trimSlash = (p: string) => p.replace(/\/+$/, "");

/**
 * The roster row that backs a workspace — a local workspace whose folder is a
 * worktree CodeSense made (a story or epic worktree) with a known clone. `null`
 * for anything else: a generic folder, a workspace on a machine, a worktree whose
 * clone vanished, or a worktree made outside CodeSense (an `orphan` row).
 */
export function removeWorktreeTargetFor(
  teamKey: string,
  ws: Pick<Workspace, "id" | "cwd" | "location" | "sourceRepoSlug" | "storyFilePath">,
  rows: readonly WorktreeRow[] | null,
): RemoveWorktreeTarget | null {
  if (ws.location || !ws.cwd || !rows) return null;
  const cwd = trimSlash(ws.cwd);
  const row = rows.find((r) => trimSlash(r.path) === cwd);
  if (!row || row.kind === "orphan" || row.kind === "context" || !row.repoPath) return null;
  return {
    teamKey,
    workspaceId: ws.id,
    path: row.path,
    branch: row.branch,
    repoPath: row.repoPath,
    sourceRepoSlug: ws.sourceRepoSlug,
    storyFilePath: ws.storyFilePath,
  };
}

const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

/** This workspace's own terminal sessions (shown or hidden), which ending it stops. */
function ownSessionIds(target: RemoveWorktreeTarget): string[] {
  const s = getViewerState();
  return [
    ...selectWorkspaceTabs(s, target.teamKey, target.workspaceId),
    ...selectHiddenTerminalTabs(s, target.teamKey, target.workspaceId),
  ].map((t) => t.sessionId);
}

/**
 * Why removing the worktree now would lose work or pull a folder out from under a
 * running session, or null when it is safe. Runs before anything is ended.
 */
export async function removalBlocker(target: RemoveWorktreeTarget): Promise<string | null> {
  const { path } = target;
  let dirty: boolean;
  try {
    dirty = (await checkWorktreeDirty(path)).dirty;
  } catch (err) {
    return `CodeSense couldn't check this worktree for uncommitted changes (${getErrorMessage(err)}), so it won't remove it.`;
  }
  if (dirty) {
    return "This worktree has uncommitted changes. Commit or discard them, then remove it.";
  }
  let unpublished: number;
  try {
    unpublished = (await getWorktreeUnpublishedContext(path)).count;
  } catch (err) {
    return `CodeSense couldn't check this worktree for unpublished context edits (${getErrorMessage(err)}), so it won't remove it.`;
  }
  if (unpublished > 0) {
    return `This worktree holds ${plural(unpublished, "unpublished context edit", "unpublished context edits")}; removing it would discard them.`;
  }
  let inFolder: number | null;
  let ownAlive: number;
  try {
    inFolder = await worktreeLiveSessionCount(path);
    const own = ownSessionIds(target);
    ownAlive = own.length > 0 ? (await dashboardProbeSessionsAlive(own)).length : 0;
  } catch (err) {
    return `CodeSense couldn't check what is running in this folder (${getErrorMessage(err)}), so it won't remove it.`;
  }
  if (inFolder === null) {
    return "CodeSense couldn't check what is running in this folder, so it won't remove it.";
  }
  const others = inFolder - ownAlive;
  if (others > 0) {
    return `Something else is running in this folder: ${plural(others, "session", "sessions")} outside this workspace (another workspace, a headless agent or a routine). Stop ${others === 1 ? "it" : "them"}, then remove the worktree.`;
  }
  return null;
}

const EXIT_WAIT_MS = 5000;
const EXIT_POLL_MS = 250;

/** Waits until no session is running in `path`; false if one still is after the wait. */
async function waitForFolderIdle(path: string): Promise<boolean> {
  const deadline = Date.now() + EXIT_WAIT_MS;
  for (;;) {
    const live = await worktreeLiveSessionCount(path).catch(() => null);
    if (live === 0) return true;
    if (Date.now() >= deadline) return false;
    await new Promise((resolve) => setTimeout(resolve, EXIT_POLL_MS));
  }
}

/**
 * Ends the workspace (its tabs close first), waits for its sessions to exit, then
 * removes the worktree folder — ignored files such as `node_modules/` go with it.
 * The branch stays. Call only after {@link removalBlocker} found nothing.
 */
export async function removeWorkspaceWorktree(target: RemoveWorktreeTarget): Promise<void> {
  if (!(await endWorkspace(target.teamKey, target.workspaceId))) return;
  const toastId = toast.loading("Removing worktree…");
  try {
    if (!(await waitForFolderIdle(target.path))) {
      toast.error(
        "Couldn't remove worktree: a session is still running in its folder. Nothing was removed.",
        { id: toastId },
      );
      return;
    }
    // The workspace's own agent could have edited context while it was ending.
    const late = (await getWorktreeUnpublishedContext(target.path)).count;
    if (late > 0) {
      toast.error(
        `Couldn't remove worktree: it now holds ${plural(late, "unpublished context edit", "unpublished context edits")}. Nothing was removed.`,
        { id: toastId },
      );
      return;
    }
    // The dialog checked for uncommitted code and unpublished context edits, so the
    // remaining ignored files (dependencies, build output) are discarded; `force`
    // stays off so git still refuses a tracked change made since.
    await removeWorktree(target.repoPath, target.path, false, true);
    if (target.sourceRepoSlug && target.storyFilePath) {
      getViewerState().removeOnDiskWorktree(
        target.teamKey,
        target.sourceRepoSlug,
        target.storyFilePath,
      );
    }
    toast.success("Worktree removed", { id: toastId });
  } catch (err) {
    toast.error(`Couldn't remove worktree: ${getErrorMessage(err)}`, { id: toastId });
  }
}

type Check = { kind: "checking" } | { kind: "ready" } | { kind: "blocked"; reason: string };

/**
 * "Remove worktree…" confirmation: names the folder and branch, says ignored files
 * go with it, refuses while the worktree holds uncommitted code or unpublished
 * context edits or something outside this workspace runs in it, and re-checks on
 * confirm before ending the workspace's tabs and removing the worktree. The branch
 * is kept.
 */
export function RemoveWorktreeDialog({
  target,
  onClose,
}: {
  target: RemoveWorktreeTarget | null;
  onClose: () => void;
}) {
  // The check result belongs to the target it ran for; each opening checks afresh.
  const [result, setResult] = useState<{
    for: RemoveWorktreeTarget;
    reason: string | null;
  } | null>(null);
  const [confirming, setConfirming] = useState(false);
  const check: Check =
    !target || result?.for !== target
      ? { kind: "checking" }
      : result.reason
        ? { kind: "blocked", reason: result.reason }
        : { kind: "ready" };

  useEffect(() => {
    if (!target) return;
    let cancelled = false;
    void removalBlocker(target).then((reason) => {
      if (!cancelled) setResult({ for: target, reason });
    });
    return () => {
      cancelled = true;
    };
  }, [target]);

  const confirm = async () => {
    if (!target || confirming) return;
    setConfirming(true);
    try {
      // Things may have changed since the dialog opened: check again before ending anything.
      const reason = await removalBlocker(target);
      if (reason) {
        setResult({ for: target, reason });
        return;
      }
      onClose();
      await removeWorkspaceWorktree(target);
    } finally {
      setConfirming(false);
    }
  };

  return (
    <AlertDialog open={target !== null} onOpenChange={(open) => !open && onClose()}>
      <AlertDialogContent data-testid="remove-worktree-dialog">
        <AlertDialogHeader>
          <AlertDialogTitle>Remove worktree?</AlertDialogTitle>
          <AlertDialogDescription>
            Deletes the worktree folder <code className="break-all">{target?.path}</code>
            {target?.branch ? (
              <>
                {" "}
                on branch <code className="break-all">{target.branch}</code>
              </>
            ) : null}{" "}
            with everything in it, including ignored files such as <code>node_modules</code>, build
            output and local <code>.env</code> files. Its commits stay in the repo
            {target?.branch ? ", and so does the branch" : ""}. The workspace&apos;s tabs are closed
            first.
          </AlertDialogDescription>
        </AlertDialogHeader>
        {check.kind === "checking" && (
          <p className="text-sm text-muted-foreground" data-testid="remove-worktree-checking">
            Checking for uncommitted changes and running sessions…
          </p>
        )}
        {check.kind === "blocked" && (
          <p
            className="text-sm text-destructive"
            role="alert"
            data-testid="remove-worktree-blocked"
          >
            {check.reason} Nothing was removed.
          </p>
        )}
        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <AlertDialogAction
            disabled={check.kind !== "ready" || confirming}
            data-testid="remove-worktree-confirm"
            onClick={(e) => {
              // Keep the dialog open while the re-check runs; it closes itself on success.
              e.preventDefault();
              void confirm();
            }}
          >
            Remove worktree
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
