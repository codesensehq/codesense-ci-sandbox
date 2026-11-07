// SPDX-License-Identifier: MIT
import { isPersonalContext } from "@/shared/lib/move-gate-ladder";
import type { ContextId } from "@/shared/lib/context-model";

/**
 * SLW-PT-14 — the pure gate decision for a REMOVE (delete a content item from a
 * single Context). Mirrors the SLW-PT-12 sole-copy gate (§J), scoped to a plain
 * delete:
 *
 *   - A SHARED (team) item is never a sole-copy loss → a NORMAL confirm.
 *   - A scratch item may be the only copy (the scratch space is the user's own
 *     folder; CodeSense never copies it anywhere) → the sole-copy warning with
 *     Delete permanently / Cancel.
 *
 * Pure and unit-tested so the copy can never silently drift, and so no internal
 * word leaks into any user-facing string.
 */

/** The kind of confirm a REMOVE presents. */
export type RemoveGateKind =
  /** A plain "Remove '<file>'?" confirm — no sole-copy risk. */
  | "normal"
  /** A scratch item: warn that this may be its only copy. */
  | "soleCopyBlock";

export interface RemoveGate {
  kind: RemoveGateKind;
}

/** Decide the REMOVE gate for an item from its context. */
export function planRemoveGate(source: ContextId): RemoveGate {
  return { kind: isPersonalContext(source) ? "soleCopyBlock" : "normal" };
}

/** The copy for each REMOVE gate. The sole-copy warning names the file and says
 *  plainly it may exist "only on this machine". */
export interface RemoveGateCopy {
  /** The confirm heading. */
  title: string;
  /** The body sentence (empty for a bare `normal` confirm). */
  body: string;
  /** The discarding action's label. */
  confirmLabel: string;
}

export function removeGateCopy(fileName: string, gate: RemoveGate): RemoveGateCopy {
  switch (gate.kind) {
    case "normal":
      return {
        title: `Remove '${fileName}'?`,
        body: "",
        confirmLabel: "Remove",
      };
    case "soleCopyBlock":
      return {
        title: `'${fileName}' may exist only on this machine`,
        body: `Private is yours to keep; CodeSense keeps no other copy of '${fileName}'. Deleting it can't be undone.`,
        confirmLabel: "Delete permanently",
      };
  }
}

/** The success toast for a completed REMOVE. */
export function removeSuccessToast(fileName: string): string {
  return `Removed '${fileName}'`;
}

/** Map a soft-reject REMOVE outcome to a toast — no dead-end, no banned word. A
 *  source-missing remove means the item was already gone (the user's goal), so
 *  it reads as an informative confirmation, not an error. */
export function removeOutcomeToast(
  outcomeKind: "removed" | "rejectedSourceMissing" | "rejectedUnbacked",
  fileName: string,
): { kind: "success" | "info" | "error"; message: string } {
  switch (outcomeKind) {
    case "removed":
      return { kind: "success", message: removeSuccessToast(fileName) };
    case "rejectedSourceMissing":
      return { kind: "info", message: `'${fileName}' is already gone` };
    case "rejectedUnbacked":
      return { kind: "error", message: `Couldn't remove '${fileName}' — try again in a moment` };
  }
}
