// SPDX-License-Identifier: MIT
import { useState } from "react";

import { Loader2 } from "lucide-react";

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
import { cn } from "@/lib/utils";
import { wvRatify, wvRevoke, wvSettleApproval } from "@/tauri/commands";

/**
 * SW-WV-F5 (AC-1, AC-2, AC-8) — the Ratify / Revoke pill on a YOU-OWE endorsement row.
 *
 * The ONLY human-actionable path to move the endorsement wall (the wall itself is
 * enforced in Rust by E1/E3). Nothing merges automatically; every Ratify is a
 * deliberate click.
 *
 *  - PENDING → amber "PENDING" badge; Ratify + Revoke buttons revealed on hover/focus.
 *  - RATIFIED / REVOKED → the matching badge; no action buttons.
 *  - A `busy` guard disables the buttons and shows a spinner during in-flight async so a
 *    rapid double-click fires the command exactly once (AC-8).
 *  - A failed call shows an inline `role="alert"` error beside the button with a Retry
 *    affordance — never a separate toast (AC-1).
 *  - `requiresDialog` (a T3 explicit-endorsement approval) opens a blocking
 *    confirmation AlertDialog before ratifying (AC-2); Revoke never opens a dialog.
 *
 * The command routing is by backing row: an `approvalId` (a `wv_approvals` T2/T3 row)
 * settles via `wvSettleApproval`; a plain assertion row (no `approvalId`) uses
 * `wvRatify` / `wvRevoke`.
 */
export interface EndorsementPillProps {
  id: number;
  disposition: "pending" | "ratified" | "revoked";
  /** Set when backed by a T3 approval needing explicit confirmation before ratify. */
  requiresDialog?: boolean;
  /** The backing `wv_approvals` id (T2/T3). `undefined` for a plain assertion row. */
  approvalId?: number;
  /** The full claim text — shown in the T3 confirm dialog. */
  claim?: string;
  onRatified: (id: number) => void;
  onRevoked: (id: number) => void;
}

type Action = "ratify" | "revoke";

export function EndorsementPill({
  id,
  disposition,
  requiresDialog = false,
  approvalId,
  claim,
  onRatified,
  onRevoked,
}: EndorsementPillProps): React.JSX.Element {
  // Optimistic local disposition — flips immediately on a settled call so the pill
  // reads RATIFIED/REVOKED before the LiveList tombstones the row.
  const [local, setLocal] = useState(disposition);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [dialogOpen, setDialogOpen] = useState(false);
  // The last attempted action — drives the spinner position and the inline Retry's verb,
  // so it is render-relevant state (not a ref).
  const [lastAction, setLastAction] = useState<Action | null>(null);

  const run = (action: Action) => {
    if (busy) return; // AC-8 busy guard — one in-flight action at a time.
    setLastAction(action);
    setBusy(true);
    setError(null);

    const settled = action === "ratify" ? "ratified" : "revoked";
    const call: Promise<unknown> =
      approvalId != null
        ? wvSettleApproval(approvalId, settled)
        : action === "ratify"
          ? wvRatify(id)
          : wvRevoke(id);

    void call.then(
      () => {
        setBusy(false);
        setLocal(settled);
        if (action === "ratify") onRatified(id);
        else onRevoked(id);
      },
      (e: unknown) => {
        setBusy(false);
        setError(e instanceof Error ? e.message : String(e));
      },
    );
  };

  const onRatifyClick = () => {
    if (busy) return;
    // A T3 explicit-endorsement needs a blocking confirm before it ratifies (AC-2).
    if (requiresDialog) {
      setDialogOpen(true);
      return;
    }
    run("ratify");
  };

  if (local !== "pending") {
    return (
      <span
        data-testid="endorsement-pill"
        data-disposition={local}
        className={cn(
          "rounded px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wide",
          local === "ratified"
            ? "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400"
            : "bg-muted text-muted-foreground",
        )}
      >
        {local}
      </span>
    );
  }

  return (
    <span
      data-testid="endorsement-pill"
      data-disposition="pending"
      className="group/pill flex items-center gap-1.5"
    >
      <span className="rounded bg-amber-500/15 px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wide text-amber-600 dark:text-amber-400">
        Pending
      </span>
      {/* Actions reveal on hover/focus of the row group; focus-within keeps them
          reachable by keyboard even before hover. */}
      <span className="flex items-center gap-1 opacity-0 transition group-hover/pill:opacity-100 group-focus-within/pill:opacity-100">
        {error ? (
          <span className="flex items-center gap-1">
            <span role="alert" className="text-[10px] text-destructive">
              {error}
            </span>
            <button
              type="button"
              data-testid="endorsement-retry"
              onClick={() => run(lastAction ?? "ratify")}
              disabled={busy}
              className="rounded px-1 py-0.5 text-[10px] font-medium text-foreground hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            >
              Retry
            </button>
          </span>
        ) : (
          <>
            <button
              type="button"
              data-testid="endorsement-ratify"
              aria-label="Ratify"
              onClick={onRatifyClick}
              disabled={busy}
              className="flex items-center gap-1 rounded bg-emerald-500/15 px-1.5 py-0.5 text-[10px] font-medium text-emerald-600 hover:bg-emerald-500/25 disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring dark:text-emerald-400"
            >
              {busy && lastAction === "ratify" ? (
                <Loader2 size={10} className="animate-spin" aria-hidden />
              ) : null}
              Ratify
            </button>
            <button
              type="button"
              data-testid="endorsement-revoke"
              aria-label="Revoke"
              onClick={() => run("revoke")}
              disabled={busy}
              className="flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground hover:bg-accent disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            >
              {busy && lastAction === "revoke" ? (
                <Loader2 size={10} className="animate-spin" aria-hidden />
              ) : null}
              Revoke
            </button>
          </>
        )}
      </span>

      {requiresDialog && (
        <AlertDialog open={dialogOpen} onOpenChange={setDialogOpen}>
          <AlertDialogContent data-testid="endorsement-confirm-dialog">
            <AlertDialogHeader>
              <AlertDialogTitle>Confirm explicit endorsement</AlertDialogTitle>
              <AlertDialogDescription>{claim ?? "Ratify this assertion?"}</AlertDialogDescription>
            </AlertDialogHeader>
            <AlertDialogFooter>
              <AlertDialogCancel data-testid="endorsement-confirm-cancel">Cancel</AlertDialogCancel>
              <AlertDialogAction
                data-testid="endorsement-confirm-ratify"
                onClick={() => {
                  setDialogOpen(false);
                  run("ratify");
                }}
              >
                Ratify
              </AlertDialogAction>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>
      )}
    </span>
  );
}
