// SPDX-License-Identifier: MIT
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
import { buttonVariants } from "@/components/ui/button";
import { settleEndConfirm, useEndConfirmStore } from "../lib/terminal-close";

/**
 * The in-app "End?" confirmation for terminals that are still running. One
 * app-wide instance, driven by `terminal-close`'s store; renders nothing until
 * an End needs asking. Cancel holds focus, so Enter keeps everything running
 * and Esc cancels too — only an explicit End kills.
 */
export function EndConfirmDialog() {
  const request = useEndConfirmStore((s) => s.request);
  return (
    <AlertDialog
      key={request?.id}
      open={request !== null}
      onOpenChange={(open) => !open && settleEndConfirm(false)}
    >
      {request && (
        <AlertDialogContent data-testid="end-confirm-dialog">
          <AlertDialogHeader>
            <AlertDialogTitle>{request.title}</AlertDialogTitle>
            {/* The running list lives in the description so it is announced. */}
            <AlertDialogDescription asChild>
              <div>
                {request.requester && <p className="mb-2">{request.requester}</p>}
                <p>Ending stops these now:</p>
                <ul
                  className="mt-1 list-disc space-y-0.5 pl-5 text-foreground"
                  data-testid="end-confirm-running"
                >
                  {request.running.map((line, i) => (
                    <li key={i}>{line}</li>
                  ))}
                </ul>
                {request.note && (
                  <p className="mt-2" data-testid="end-confirm-note">
                    {request.note}
                  </p>
                )}
              </div>
            </AlertDialogDescription>
          </AlertDialogHeader>
          {request.details}
          {request.onAlways && (
            <label className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                onChange={(event) => request.onAlways?.(event.target.checked)}
              />
              Always allow for this dashboard
            </label>
          )}
          <AlertDialogFooter>
            <AlertDialogCancel autoFocus>Cancel</AlertDialogCancel>
            <AlertDialogAction
              className={buttonVariants({ variant: "destructive" })}
              onClick={(event) => {
                if (request.canConfirm && !request.canConfirm()) {
                  event.preventDefault();
                  return;
                }
                settleEndConfirm(true);
              }}
            >
              {request.confirmLabel}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      )}
    </AlertDialog>
  );
}
