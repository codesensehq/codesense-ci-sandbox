// SPDX-License-Identifier: MIT
import { useRef } from "react";
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

export interface WorktreeSetupApprovalModalProps {
  /** The pending gate request, or null when hidden. */
  request: { steps: string[]; repoLabel: string } | null;
  onApprove: () => void;
  onDecline: () => void;
}

/**
 * SW-WT-9: one-time preview-and-approve gate shown before a worktree.yaml
 * setup of **team-synced** provenance runs for the first time. Lists the
 * exact ordered commands verbatim (AC-2). Approving runs setup; declining
 * leaves the worktree usable with setup skipped. Controlled — the parent
 * (viewer-store) owns the pending request and resolves the user's choice.
 */
export function WorktreeSetupApprovalModal({
  request,
  onApprove,
  onDecline,
}: WorktreeSetupApprovalModalProps) {
  // Radix fires onOpenChange(false) on any close — including after the
  // approve action. Track which path closed so the spurious onOpenChange
  // doesn't double-fire decline.
  const approvedRef = useRef(false);

  const handleApprove = () => {
    approvedRef.current = true;
    onApprove();
  };

  const handleOpenChange = (open: boolean) => {
    if (open) {
      approvedRef.current = false;
      return;
    }
    if (approvedRef.current) {
      approvedRef.current = false;
      return;
    }
    onDecline();
  };

  return (
    <AlertDialog open={request !== null} onOpenChange={handleOpenChange}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>
            Run setup from your team for {request?.repoLabel ?? "this repo"}?
          </AlertDialogTitle>
          <AlertDialogDescription>
            This repo&apos;s worktree setup came from your team&apos;s shared context repo, not from
            you. The following commands will run on your machine — review them, then approve to run
            or skip to open the worktree without setup. To run it later, choose Retry on the
            workspace&apos;s setup message and approve the commands then.
          </AlertDialogDescription>
        </AlertDialogHeader>
        {request && request.steps.length > 0 && (
          <pre className="max-h-60 overflow-auto rounded-md bg-muted p-3 text-xs font-mono whitespace-pre-wrap break-words">
            {request.steps.map((step, i) => `${i + 1}. ${step}`).join("\n")}
          </pre>
        )}
        <AlertDialogFooter>
          <AlertDialogCancel>Skip setup</AlertDialogCancel>
          <AlertDialogAction onClick={handleApprove}>Approve &amp; run</AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
