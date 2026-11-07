// SPDX-License-Identifier: MIT
import { useEffect } from "react";

import {
  formatEpicAssemblyProgress,
  formatEpicPartialFailure,
  useEpicAssemblyStore,
  useEpicBatchStatus,
  type EpicBatchStatus,
} from "@/features/shell/stores/epic-assembly-store";
import {
  useSurfaceSyncStatus,
  type SurfaceSyncStatus,
} from "@/features/shell/stores/surface-sync-store";
import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";
import type { WorkspaceSetupState } from "@/features/viewer/lib/workspace-setup-state";

// SLW-CO-7 (review): the materialize-START watchdog window. Only arms while the
// surface has shown NO signal yet — a real wave's `started` fires within ms, so
// 20s comfortably distinguishes "a signal is coming" from "none ever will". Not
// a UX timing threshold — a stuck-state escape hatch (see the hook below).
export const MATERIALIZE_START_WATCHDOG_MS = 20_000;

// The product-assembly report bound. Assembly (worktree-adds across every repo)
// is legitimately slow and sends no progress until it reports, so the story
// start window above does not fit. A product that has not reported (seeded or
// failed) within this bound is shown as a failed assembly: the in-pane error
// replaces the cover instead of hiding the terminal forever. A late report still
// supersedes it (`seedEpicAssembly` clears the provisional error). Installs that run
// after the report are not bounded here — they report their own completions.
export const EPIC_ASSEMBLY_REPORT_WATCHDOG_MS = 300_000;

export const EPIC_ASSEMBLY_NO_REPORT_ERROR =
  "Product setup hasn't reported back. It replaces this message if it finishes; otherwise restart CodeSense to try again.";

/** What the terminal body needs to render its init/failure overlays. */
export interface WorkspaceOverlayState {
  /** Cover the terminal with the "Setting up…" overlay. */
  visible: boolean;
  /** Hand off to the in-pane `WorkspaceMaterializeError` (blocking). */
  failed: boolean;
  /** The overlay's one continuous sub-label (epic assembly, or a materialize
   *  count) when the workspace has no typed setup line. */
  subLabel: string | undefined;
  /** SW-XS-10: the typed setup line while the workspace is being created. */
  setupState: WorkspaceSetupState | undefined;
  /** Reason shown in the in-pane error (when `failed`). */
  failureReason: string | undefined;
  /** SLW-CO-8 (AC-4): a settled-but-usable epic with some failed repos — show a
   *  non-blocking in-pane flag over the (usable) terminal. */
  partialFailure: boolean;
  /** The partial-failure flag's message (when `partialFailure`). */
  partialFailureLabel: string | undefined;
  /** Team context is still landing in this workspace (the materialize wave has
   *  not settled). Terminals hold a typed command until it has. */
  contextLanding: boolean;
  /** The context actually landed (the wave settled usable) — as opposed to the
   *  overlay latch being dropped by the start watchdog with no wave seen. */
  contextLanded: boolean;
  /** The first landing failed for good (the wave failed, or its auto-retries ran
   *  out) — the context-landing chip's "failed" reason. */
  contextFailed: boolean;
}

/**
 * Pure derivation of the overlay state from a workspace + its sync signals.
 * Split from the hook (which owns the stores + effects) so the story-vs-epic
 * branching is unit-testable without React. See {@link useWorkspaceOverlayState}.
 *
 * - **Story:** the overlay rides the per-surface `WorktreeDerive` wave — up while
 *   `undefined` (pre-start gap) or `syncing`, drops on `synced`, in-pane error on
 *   `failed`.
 * - **Epic (SLW-CO-8):** the overlay rides the whole multi-repo BATCH aggregate
 *   (source worktree assembly + N dev-env installs) — up until settled; a TOTAL failure
 *   (assemble threw) → in-pane error, a PARTIAL failure → usable epic + flag.
 *
 * The `needsSyncOverlay` latch disambiguates the pre-start ambiguity in both.
 */
export function deriveWorkspaceOverlay(
  ws: Pick<Workspace, "kind" | "status" | "needsSyncOverlay" | "setupState">,
  surface: SurfaceSyncStatus | undefined,
  epicBatch: EpicBatchStatus | undefined,
): WorkspaceOverlayState {
  const isEpic = ws.kind === "epic";
  const surfaceState = surface?.state;
  const materializePending = isEpic
    ? !!ws.needsSyncOverlay && (epicBatch === undefined || !epicBatch.settled)
    : !!ws.needsSyncOverlay && (surfaceState === undefined || surfaceState === "syncing");
  const failed = isEpic
    ? !!ws.needsSyncOverlay && epicBatch?.totalFailure === true
    : !!ws.needsSyncOverlay && surfaceState === "failed";
  const visible = ws.status === "creating" || materializePending;
  const contextLanded = isEpic ? false : surfaceState === "synced";
  // The context-landing signal (the chip and the typed-command hold): a first
  // landing is still on its way while its wave runs.
  const contextFailed = isEpic ? false : !!ws.needsSyncOverlay && surfaceState === "failed";
  const contextLanding =
    !contextFailed &&
    (isEpic
      ? false
      : !!ws.needsSyncOverlay && (surfaceState === undefined || surfaceState === "syncing"));
  const setupState = ws.status === "creating" ? ws.setupState : undefined;
  const subLabel = isEpic
    ? epicBatch
      ? formatEpicAssemblyProgress(epicBatch)
      : setupState
        ? undefined
        : "Setting up epic…"
    : undefined;
  const partialFailure = isEpic && epicBatch?.partialFailure === true;
  return {
    visible,
    failed,
    subLabel,
    setupState,
    failureReason: isEpic ? epicBatch?.assemblyError : surface?.errorMessage,
    partialFailure,
    partialFailureLabel:
      partialFailure && epicBatch ? formatEpicPartialFailure(epicBatch) : undefined,
    contextLanding,
    contextLanded,
    contextFailed,
  };
}

/**
 * The terminal body's overlay-state hook: reads the per-surface + epic-batch
 * sync signals, owns the two anti-stuck effects, and returns the derived
 * {@link WorkspaceOverlayState}.
 *
 * Effects:
 * - **One-shot clear-on-settle:** drop the `needsSyncOverlay` latch the moment
 *   the workspace is genuinely usable (story `synced`; epic batch
 *   settled and not a total failure) so a LATER unrelated wave on the same `cwd`
 *   can't re-cover a live terminal. `failed`/`totalFailure` keep the latch (the
 *   in-pane error owns the pane).
 * - **Story start watchdog:** drops the latch when no wave ever starts.
 * - **Product report watchdog:** a product whose assembly has not reported
 *   (seeded or failed) within {@link EPIC_ASSEMBLY_REPORT_WATCHDOG_MS} is marked
 *   as a failed assembly, so the in-pane error replaces the cover.
 */
export function useWorkspaceOverlayState(ws: Workspace, teamKey: string): WorkspaceOverlayState {
  const surface = useSurfaceSyncStatus(ws.cwd);
  const isEpic = ws.kind === "epic";
  const epicBatch = useEpicBatchStatus(isEpic ? ws.cwd : undefined);
  const surfaceState = surface?.state;

  const epicSettledUsable = isEpic && epicBatch?.settled === true && !epicBatch.totalFailure;
  useEffect(() => {
    if (!ws.needsSyncOverlay) return;
    const shouldClear = isEpic ? epicSettledUsable : surfaceState === "synced";
    if (shouldClear) {
      useViewerStore.getState().clearSyncOverlay(teamKey, ws.id);
    }
  }, [ws.needsSyncOverlay, isEpic, epicSettledUsable, surfaceState, teamKey, ws.id]);

  useEffect(() => {
    const armed = !isEpic && ws.status !== "creating" && surfaceState === undefined;
    if (!ws.needsSyncOverlay || !armed) return;
    const t = setTimeout(() => {
      useViewerStore.getState().clearSyncOverlay(teamKey, ws.id);
    }, MATERIALIZE_START_WATCHDOG_MS);
    return () => clearTimeout(t);
  }, [ws.needsSyncOverlay, isEpic, ws.status, surfaceState, teamKey, ws.id]);

  const epicAwaitingReport = isEpic && epicBatch?.seeded !== true;
  useEffect(() => {
    if (!ws.needsSyncOverlay || !epicAwaitingReport || ws.status === "creating") return;
    const t = setTimeout(() => {
      useEpicAssemblyStore.getState().markEpicAssemblyError(ws.cwd, EPIC_ASSEMBLY_NO_REPORT_ERROR);
    }, EPIC_ASSEMBLY_REPORT_WATCHDOG_MS);
    return () => clearTimeout(t);
  }, [ws.needsSyncOverlay, epicAwaitingReport, ws.status, ws.cwd]);

  return deriveWorkspaceOverlay(ws, surface, epicBatch);
}
