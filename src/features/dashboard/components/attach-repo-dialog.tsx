// SPDX-License-Identifier: MIT
import { useState } from "react";
import { Plus } from "lucide-react";
import { AppButton as Button } from "@/shared/components/app-button";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogTrigger,
} from "@/components/ui/dialog";
import { useOrgStore } from "@/shared/stores/org-store";
import { AddRepoForm } from "./add-repo-form";

interface AttachRepoDialogProps {
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  /** Target a specific workspace instead of the active one. */
  workspaceTarget?: { orgSlug: string; teamName: string };
}

/**
 * SW-UX (app-flow redesign) — the standalone "Add a source repo" dialog. A thin
 * wrapper around the reusable {@link AddRepoForm} body (the same body the Manage-repos
 * tab renders). Authenticating a NEW code-host account opens the Connections tab (no
 * inline device flow); the picked account is controlled here.
 */
export function AttachRepoDialog({
  open: controlledOpen,
  onOpenChange,
  workspaceTarget,
}: AttachRepoDialogProps = {}) {
  const [internalOpen, setInternalOpen] = useState(false);
  const isControlled = controlledOpen !== undefined;
  const open = isControlled ? controlledOpen : internalOpen;
  const setOpen = isControlled ? (v: boolean) => onOpenChange?.(v) : setInternalOpen;

  const activeOrgSlug = useOrgStore((s) => s.activeOrgSlug);
  const activeTeamName = useOrgStore((s) => s.activeTeamName);
  const orgSlug = workspaceTarget?.orgSlug ?? activeOrgSlug;
  const teamName = workspaceTarget?.teamName ?? activeTeamName;

  return (
    <Dialog
      open={open}
      onOpenChange={(v) => {
        setOpen(v);
      }}
    >
      {!isControlled && (
        <DialogTrigger asChild>
          <Button variant="outline" size="sm">
            <Plus size={14} className="mr-1.5" />
            Add source repo
          </Button>
        </DialogTrigger>
      )}
      <DialogContent className="p-0">
        <div className="flex min-h-0">
          <div className="flex w-full flex-col p-6">
            <DialogHeader>
              <DialogTitle>Add a source repo</DialogTitle>
              <DialogDescription>
                Point CodeSense at your code — a local folder on this machine, or a remote URL.
              </DialogDescription>
            </DialogHeader>
            <div className="pt-2">
              {orgSlug && teamName ? (
                <AddRepoForm
                  target={{ orgSlug, teamName }}
                  onDone={(added) => {
                    // SW-LN-29: a local attach stays open on its success state
                    // (where context lands).
                    if (added !== "local") setOpen(false);
                  }}
                />
              ) : (
                <p className="text-sm text-muted-foreground">
                  No active workspace. Please select a workspace first.
                </p>
              )}
            </div>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
