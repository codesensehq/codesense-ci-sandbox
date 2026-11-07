// SPDX-License-Identifier: MIT
import { memo, useCallback, useEffect, useState } from "react";
import { toast } from "sonner";
import { listen } from "@tauri-apps/api/event";
import { toastFailure } from "@/shared/lib/with-error-toast";
import type { Attachment, SourceRepoSlug } from "@/shared/types/domain";
import { checkPathExists, deleteSourceRepoAttachment } from "@/tauri/commands";
import { useOrgStore } from "@/shared/stores/org-store";
import { getCurrentUser } from "@/shared/lib/get-current-user";
import { commitRegistryEdit } from "@/shared/lib/commit-registry-edit";
import { syntheticRepoKey } from "@/shared/lib/build-synthetic-repo";
import { SYNC_REPO_LOOKS_GONE_EVENT, type RepoLooksGonePayload } from "@/tauri/events";
import { safeUnlisten } from "@/shared/lib/tauri-listeners";
import { useDialogTarget } from "@/features/settings/lib/use-dialog-target";
import { ConfirmActionDialog } from "@/features/settings/components/confirm-action-dialog";
import { AttachLocalCloneDialog } from "./attach-local-clone-dialog";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
interface SourceRepoPillProps {
  slug: SourceRepoSlug;
  displayLabel?: string;
  attachment: Attachment | null;
  orgSlug: string;
  teamName: string;
  unavailableLabel?: string;
}
export const SourceRepoPill = memo(function SourceRepoPill({
  slug,
  displayLabel,
  attachment,
  orgSlug,
  teamName,
}: SourceRepoPillProps) {
  const ownerRepo = displayLabel ?? `${slug.owner}/${slug.repo}`;
  const repoPath = attachment?.repo_path ?? null;
  const [pathGone, setPathGone] = useState(false);
  const [popoverOpen, setPopoverOpen] = useState(false);
  const attachDialog = useDialogTarget<true>();
  const detachDialog = useDialogTarget<true>();
  const removeDialog = useDialogTarget<true>();
  const forgetDialog = useDialogTarget<true>();
  const [detaching, setDetaching] = useState(false);
  const [removing, setRemoving] = useState(false);
  const [forgetting, setForgetting] = useState(false);
  useEffect(() => {
    if (!repoPath) {
      setPathGone(false);
      return;
    }
    let cancelled = false;
    let eventSaidGone = false;
    // Render-time check: don't wait for a wave to fire to tell the user.
    void checkPathExists(repoPath)
      .then((exists) => {
        // Don't let a stale "exists" probe override a real-time gone event
        // that landed while the probe was in flight (last-writer race).
        if (!cancelled && !eventSaidGone) setPathGone(!exists);
      })
      .catch(() => {
        // A failed existence probe is not proof of "gone"; leave as-is.
      });
    // Real-time signal: a wave that skipped this path emits it.
    const unlisten = listen<RepoLooksGonePayload>(SYNC_REPO_LOOKS_GONE_EVENT, (event) => {
      if (!cancelled && event.payload.code_repo_path === repoPath) {
        // Ignore a gone event that lands right after a successful Re-clone —
        // it's a stale signal from a wave that observed the path mid-restore.

        eventSaidGone = true;
        setPathGone(true);
      }
    });
    return () => {
      cancelled = true;
      void unlisten.then(safeUnlisten).catch(() => {});
    };
  }, [repoPath]);

  const handleForgetConfirm = useCallback(async () => {
    if (!attachment?.repo_path) return;
    setForgetting(true);
    try {
      const currentUser = getCurrentUser();
      if (!currentUser) {
        toast.error("Can't forget — no active user.");
        return;
      }
      // Detach IS forget: teardown is gone-path-idempotent (missing files
      // are no-ops, the stale lockfile is deleted) and then the attachment
      // record is dropped.
      await deleteSourceRepoAttachment(currentUser.github_user_id, attachment.repo_path);
      useOrgStore.getState().bumpAttachmentsVersion();
      setPathGone(false);
      toast.success(`Forgot ${ownerRepo}`, {
        description: "Dropped the stale install record. Re-attach anytime to re-point it.",
      });
    } catch (e) {
      toastFailure(`Couldn't forget ${ownerRepo}`, e);
    } finally {
      setForgetting(false);
      forgetDialog.clear();
      setPopoverOpen(false);
    }
  }, [attachment?.repo_path, ownerRepo, forgetDialog]);

  const handleDetachConfirm = useCallback(async () => {
    if (!attachment?.repo_path) return;
    setDetaching(true);
    try {
      const currentUser = getCurrentUser();
      if (!currentUser) return;
      const report = await deleteSourceRepoAttachment(
        currentUser.github_user_id,
        attachment.repo_path,
      );
      useOrgStore.getState().bumpAttachmentsVersion();
      // SS-FU-17 AC-2: surface hash-guarded skips AND any per-file errors
      // instead of dropping them — a detach that left files behind must not
      // read as a clean success.
      const skipped = report?.skippedLocallyModified?.length ?? 0;
      const errored = report?.errors?.length ?? 0;
      const parts: string[] = [];
      if (skipped > 0)
        parts.push(`${skipped} locally-edited file${skipped === 1 ? "" : "s"} kept in place`);
      if (errored > 0) parts.push(`${errored} could not be removed (see logs)`);
      if (parts.length > 0) {
        toast.success(`Detached ${ownerRepo}`, { description: `${parts.join("; ")}.` });
      } else {
        toast.success(`Detached clone for ${ownerRepo}`);
      }
    } catch (e) {
      toastFailure(`Couldn't detach ${ownerRepo}`, e);
    } finally {
      setDetaching(false);
      detachDialog.clear();
      setPopoverOpen(false);
    }
  }, [attachment?.repo_path, ownerRepo, detachDialog]);

  const handleRemoveConfirm = useCallback(async () => {
    setRemoving(true);
    try {
      const currentUser = getCurrentUser();
      if (!currentUser) return;

      const edit = {
        type: "RemoveSourceRepoFromTeam" as const,
        team_name: teamName,
        owner: slug.owner,
        repo: slug.repo,
        provider: slug.provider,
        host: slug.host,
      };

      // SW-OB-8: local-only commit regardless of the org's remote.
      await commitRegistryEdit(currentUser.github_user_id, orgSlug, edit);
      toast.success(`Stopped tracking ${ownerRepo}`);
      useOrgStore.getState().removeTeamSourceRepo(orgSlug, teamName, {
        owner: slug.owner,
        repo: slug.repo,
        provider: slug.provider,
        host: slug.host,
      });
    } catch (e) {
      toastFailure(`Couldn't stop tracking ${ownerRepo}`, e);
    } finally {
      setRemoving(false);
      removeDialog.clear();
      setPopoverOpen(false);
    }
  }, [slug, orgSlug, teamName, ownerRepo, removeDialog]);

  return (
    <>
      <Popover open={popoverOpen} onOpenChange={setPopoverOpen}>
        <PopoverTrigger asChild>
          <button
            type="button"
            data-testid={`settings-source-repo-${ownerRepo}`}
            className="rounded-full border px-2.5 py-1 text-xs"
            aria-label={`${ownerRepo}: ${repoPath ? (pathGone ? "folder unavailable" : "attached locally") : "not attached"}`}
          >
            <code>{ownerRepo}</code>
          </button>
        </PopoverTrigger>
        <PopoverContent className="w-72 text-xs" align="start">
          <p>{repoPath ?? "Not attached on this machine"}</p>
          {pathGone && (
            <p role="status">
              This folder is unavailable. Prepare the repository with your agent or CLI, then attach
              its local folder.
            </p>
          )}
          <div className="mt-2 flex flex-wrap gap-2">
            {(!repoPath || pathGone) && (
              <button
                type="button"
                data-testid={`attach-clone-${ownerRepo}`}
                onClick={() => {
                  setPopoverOpen(false);
                  attachDialog.show(true);
                }}
              >
                {pathGone ? "Re-attach folder" : "Attach folder"}
              </button>
            )}
            {repoPath && (
              <button
                type="button"
                data-testid={`detach-clone-${ownerRepo}`}
                onClick={() => {
                  setPopoverOpen(false);
                  detachDialog.show(true);
                }}
              >
                Detach
              </button>
            )}
            {pathGone && (
              <button
                type="button"
                onClick={() => {
                  setPopoverOpen(false);
                  forgetDialog.show(true);
                }}
              >
                Forget attachment
              </button>
            )}
            <button
              type="button"
              data-testid={`remove-repo-${ownerRepo}`}
              onClick={() => {
                setPopoverOpen(false);
                removeDialog.show(true);
              }}
            >
              Stop tracking
            </button>
          </div>
        </PopoverContent>
      </Popover>
      {/* Attach clone dialog */}
      {attachDialog.open && (
        <AttachLocalCloneDialog
          owner={slug.owner}
          repo={slug.repo}
          provider={slug.provider}
          host={slug.host}
          displayLabel={ownerRepo}
          targetRepoPath={syntheticRepoKey(orgSlug, teamName, slug)}
          open={true}
          onOpenChange={(open) => {
            if (!open) attachDialog.clear();
          }}
        />
      )}

      {/* Detach clone confirm dialog */}
      <ConfirmActionDialog
        open={detachDialog.open}
        onOpenChange={(open) => !open && !detaching && detachDialog.clear()}
        title={`Detach clone for ${ownerRepo}?`}
        description="This won't delete your local clone, only stop linking it to this team. You can re-attach anytime."
        confirmLabel="Detach"
        loadingLabel="Detaching\u2026"
        onConfirm={handleDetachConfirm}
        loading={detaching}
      />

      {/* Forget stale install confirm dialog (AC-7) */}
      <ConfirmActionDialog
        open={forgetDialog.open}
        onOpenChange={(open) => !open && !forgetting && forgetDialog.clear()}
        title={`Forget ${ownerRepo}?`}
        description="This repo's folder no longer resolves. Forgetting drops the stale install record (and any unchanged generated files there). Your work is untouched; re-attach anytime."
        confirmLabel="Forget it"
        loadingLabel="Forgetting…"
        onConfirm={handleForgetConfirm}
        loading={forgetting}
      />

      {/* Remove from team confirm dialog */}
      <ConfirmActionDialog
        open={removeDialog.open}
        onOpenChange={(open) => !open && !removing && removeDialog.clear()}
        title={`Stop tracking ${ownerRepo}?`}
        description="This removes it from your team's source repos. Your local clone and any team content are preserved — you can re-add it anytime."
        confirmLabel="Stop Tracking"
        loadingLabel="Removing\u2026"
        onConfirm={handleRemoveConfirm}
        loading={removing}
      />
    </>
  );
});
