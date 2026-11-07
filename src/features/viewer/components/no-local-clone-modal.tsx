// SPDX-License-Identifier: MIT
import { useEffect, useMemo, useState } from "react";
import { FolderX } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { AppButton as Button } from "@/shared/components/app-button";
import { parseSyntheticRepoKey } from "@/shared/lib/build-synthetic-repo";
import { LOCAL_CLONE_EXPLAINER } from "@/shared/lib/onboarding-copy";
import { AttachLocalCloneDialog } from "@/features/attachment/components/attach-local-clone-dialog";

interface NoLocalCloneModalProps {
  /** Synthetic repoPath (e.g. `"workspace:9north:default:acme/api"`) of the repo
   *  with no matching `attachments.yml` entry. Null = modal hidden. */
  repoPath: string | null;
  onDismiss: () => void;
}

/**
 * F-6 AC-8: surfaced when the user attempts a source-code-path action
 * (worktree icon, send-to-terminal, open-in-editor, story-open) on a
 * synthetic workspace repo that has no matching `attachments.yml` entry.
 *
 * Renders a modal explaining the situation with an "Attach local clone"
 * CTA that opens `AttachLocalCloneDialog` pre-populated with the
 * workspace repo's `(owner, repo)`. After a successful attach, the
 * workspace-sync hook's `attachmentsVersion` re-trigger refreshes the
 * synthetic's `localPath`, so the user can re-try the action without
 * switching workspaces.
 */
export function NoLocalCloneModal({ repoPath, onDismiss }: NoLocalCloneModalProps) {
  const [attachOpen, setAttachOpen] = useState(false);
  const parsed = useMemo(() => (repoPath ? parseSyntheticRepoKey(repoPath) : null), [repoPath]);

  const open = repoPath !== null && parsed !== null;
  const displayLabel = parsed ? `${parsed.owner}/${parsed.repo}` : "this repo";

  // R5C-P2: if a non-null `repoPath` was passed but `parseSyntheticRepoKey`
  // returned null (corrupted persisted state, future code path passing a
  // non-synthetic key through `showNoClonePrompt`), the Dialog never opens
  // and `onOpenChange` never fires. Without this effect, `noClonePromptRepo`
  // stays set forever in viewer-store and `isHandledWorktreeError` would
  // (pre-R5C-P3) silently suppress unrelated worktree errors. Auto-dismiss
  // to clear the orphan state.
  useEffect(() => {
    if (repoPath !== null && parsed === null) {
      onDismiss();
    }
  }, [repoPath, parsed, onDismiss]);

  return (
    <>
      <Dialog
        open={open && !attachOpen}
        onOpenChange={(v) => {
          if (!v) onDismiss();
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <FolderX size={18} />
              No local clone attached
            </DialogTitle>
            <DialogDescription>
              <code className="text-xs">{displayLabel}</code> is tracked in this team, but CodeSense
              doesn't know where the local clone lives on disk. Attach one to use worktrees,
              terminals, and editor actions. {LOCAL_CLONE_EXPLAINER}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={onDismiss}>
              Cancel
            </Button>
            <Button onClick={() => setAttachOpen(true)} disabled={!parsed}>
              Attach local clone
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {parsed && repoPath && (
        <AttachLocalCloneDialog
          owner={parsed.owner}
          repo={parsed.repo}
          host={parsed.host}
          displayLabel={displayLabel}
          targetRepoPath={repoPath}
          open={attachOpen}
          onOpenChange={(v) => {
            setAttachOpen(v);
            // F-6 auto-retry: on success the dialog itself triggers
            // `consumeNoClonePromptRetry` (after observing the synthetic's
            // `localPath` populate), which clears `noClonePromptRepo` and
            // fires the retry callback. We only need to handle the cancel
            // path here — call onDismiss to clear viewer-store state.
            // On success, `noClonePromptRepo` will already be null when
            // this runs, so calling onDismiss is harmless (idempotent).
            if (!v) onDismiss();
          }}
        />
      )}
    </>
  );
}
