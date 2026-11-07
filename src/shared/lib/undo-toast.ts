// SPDX-License-Identifier: MIT
import { toast } from "sonner";
import { toastError } from "@/shared/lib/with-error-toast";

// ---------------------------------------------------------------------------
// SW-SB-16 · The ONE toast-with-Undo primitive for board writes.
//
// Every undoable board edit (status move, points, sprint tag) fires its success
// toast through here so the wording and the Undo affordance can't diverge across
// surfaces, and so the "revert best-effort, never throw" contract lives in one
// place. No React, no store access — pure Sonner orchestration.
// ---------------------------------------------------------------------------

/**
 * Fire a low-stakes success toast carrying an "Undo" action. `undo` runs on the
 * toast-action click (fire-and-forget from the toast's perspective).
 */
export function toastWithUndo(label: string, undo: () => void): void {
  toast.success(label, { action: { label: "Undo", onClick: undo } });
}

/**
 * Run an undo's revert best-effort: re-apply the prior value, `refresh` on
 * success, and on failure surface an error toast WITHOUT throwing — a
 * toast-action click must never leak an unhandled rejection. Resolves once the
 * revert settles (so callers/tests can await it).
 */
export function runUndo(
  revert: () => Promise<void>,
  refresh: () => void,
  errorLabel: string,
): Promise<void> {
  return revert()
    .then(() => refresh())
    .catch((e: unknown) => {
      toastError(`${errorLabel}: `, e);
    });
}
