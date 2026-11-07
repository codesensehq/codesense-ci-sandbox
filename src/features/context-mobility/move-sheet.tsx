// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";

import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import type { Context } from "@/shared/lib/context-model";
import type { ContextMobilityOp, MoveGateKind } from "@/shared/lib/move-gate-ladder";
import {
  endpointForContextId,
  keepBothName,
  moveFailureToast,
  moveHeader,
  moveOutcomeToast,
  planMove,
  privacyGateCopy,
} from "@/features/context-mobility/move-controller";
import { moveContentBetweenContexts, type MoveBetweenContextsOutcome } from "@/tauri/commands";

/**
 * SLW-PT-12/13 — the ONE stepped MOVE/COPY sheet (§P). A single sheet renders
 * only the gates that apply, in the fixed precedence privacy ->
 * collision (cross_org step 1 is SLW-PT-16), with a persistent header restating
 * the op. Back/Cancel at any step leaves both contexts untouched. For a MOVE the
 * destructive removal fires ONLY after the target write confirms (§N); a COPY
 * (SLW-PT-13) retains the source entirely — same sheet, `op="copy"` keeps the Personal original on a Personal->shared share, and
 * surfaces the copy verb + renamed result.
 */

export interface MoveSheetProps {
  open: boolean;
  /** SLW-PT-13 — MOVE (default) or COPY. A COPY retains the source. */
  op?: ContextMobilityOp;
  /** The item being moved (its content-root-relative logical name + display). */
  logicalName: string;
  fileName: string;
  /** The source Context (already known — the picker never asks which team you
   *  are demoting from). */
  source: Context;
  /** The chosen target Context. */
  target: Context;
  /** The local profile's id. */
  userId: number;
  /** Whether an item already exists at the target under this logical name. */
  targetCollision: boolean;
  /** Whether a candidate logical name already exists at the target (drives the
   *  Keep-both auto-rename). */
  targetHas: (candidate: string) => boolean;
  /** Called after a successful move (source lost it, target gained it) so the
   *  caller can refresh. */
  onMoved?: () => void;
  /** Close the sheet (Cancel or completion). */
  onClose: () => void;
  /** Gates the caller already put to the user in its own sheet (e.g. "Share
   *  with ‹Team›" is the privacy confirm) — skipped here, never re-asked. */
  preconfirmed?: readonly MoveGateKind[];
}

type Phase = "gates" | "running";

export function MoveSheet(props: MoveSheetProps): React.JSX.Element {
  const { open, logicalName, fileName, source, target, userId, targetCollision } = props;
  const op: ContextMobilityOp = props.op ?? "move";
  const isCopy = op === "copy";

  const preconfirmedKey = (props.preconfirmed ?? []).join(",");
  const plan = useMemo(() => {
    const planned = planMove(source.id, target.id, { targetCollision, op });
    const done = new Set(preconfirmedKey.split(","));
    return { ...planned, steps: planned.steps.filter((s) => !done.has(s.kind)) };
  }, [source.id, target.id, targetCollision, op, preconfirmedKey]);

  // The step cursor within the planned ladder.
  const [stepIndex, setStepIndex] = useState(0);
  const [phase, setPhase] = useState<Phase>("gates");
  // A Keep-both choice pre-computes the renamed target name; when set the move
  // lands under it (never a silent collision).
  const [renamedTo, setRenamedTo] = useState<string | undefined>(undefined);
  const [forceReplace, setForceReplace] = useState(false);

  const steps = plan.steps;
  const currentStep = steps[stepIndex];

  const doMove = useCallback(
    async (opts: { toLogicalName?: string; force?: boolean }) => {
      const fromEndpoint = endpointForContextId(source.id);
      const toEndpoint = endpointForContextId(target.id);
      if (!fromEndpoint || !toEndpoint) {
        toast.error(`Couldn't ${isCopy ? "copy" : "move"} '${fileName}' — try again in a moment`);
        props.onClose();
        return;
      }
      setPhase("running");
      try {
        const outcome: MoveBetweenContextsOutcome = await moveContentBetweenContexts({
          from: fromEndpoint,
          to: toEndpoint,
          logicalName,
          toLogicalName: opts.toLogicalName,
          userId,
          force: opts.force,
          retainSource: isCopy,
        });
        const t = moveOutcomeToast(outcome, {
          fileName,
          target,
          renamedTo: opts.toLogicalName,
        });
        toast[t.kind](t.message);
        if (
          outcome.kind === "moved" ||
          outcome.kind === "copied" ||
          outcome.kind === "movedSourceKept"
        )
          props.onMoved?.();
        props.onClose();
      } catch {
        // §N: target-write failure — the source is intact (a COPY never touches
        // it; a MOVE rolls back before the remove). Invite a retry.
        toast.error(moveFailureToast(fileName, op));
        props.onClose();
      }
    },
    [source.id, logicalName, userId, fileName, target, isCopy, op, props],
  );

  // Advance one gate; when the last gate is cleared, execute the move.
  const advance = useCallback(
    (next: { toLogicalName?: string; force?: boolean }) => {
      const merged = {
        toLogicalName: next.toLogicalName ?? renamedTo,
        force: next.force ?? forceReplace,
      };
      if (next.toLogicalName) setRenamedTo(next.toLogicalName);
      if (next.force) setForceReplace(true);
      if (stepIndex + 1 < steps.length) {
        setStepIndex((i) => i + 1);
      } else {
        void doMove(merged);
      }
    },
    [stepIndex, steps.length, renamedTo, forceReplace, doMove],
  );

  const back = useCallback(() => {
    if (stepIndex > 0) setStepIndex((i) => i - 1);
    else props.onClose();
  }, [stepIndex, props]);

  // No gates apply (e.g. a clean shared->Personal no-collision move): execute
  // immediately once the sheet opens, showing only a brief running state.
  const noGates = steps.length === 0;
  const startedRef = useRef(false);
  useEffect(() => {
    if (open && noGates && !startedRef.current) {
      startedRef.current = true;
      // Defer out of the effect body so the move's `setPhase("running")` runs
      // in its own task, not a cascading render synchronous with mount.
      queueMicrotask(() => void doMove({}));
    }
    if (!open) startedRef.current = false;
  }, [open, noGates, doMove]);

  const header = currentStep
    ? moveHeader(fileName, target, stepIndex, steps.length, op)
    : `${isCopy ? "Copying" : "Moving"} '${fileName}' → ${target.label}`;

  return (
    <Dialog open={open} onOpenChange={(o) => (!o ? props.onClose() : undefined)}>
      <DialogContent data-testid="move-sheet">
        <DialogHeader>
          <DialogTitle data-testid="move-sheet-header">{header}</DialogTitle>
        </DialogHeader>

        {phase === "running" && (
          <p className="text-sm text-muted-foreground" data-testid="move-sheet-running">
            {isCopy ? "Copying…" : "Moving…"}
          </p>
        )}

        {phase === "gates" && currentStep?.kind === "privacy" && (
          <PrivacyGate
            fileName={fileName}
            target={target}
            op={op}
            onCancel={props.onClose}
            onBack={back}
            canBack={stepIndex > 0}
            onConfirm={() => advance({})}
          />
        )}

        {phase === "gates" && currentStep?.kind === "collision" && (
          <CollisionGate
            target={target}
            canBack={stepIndex > 0}
            onBack={back}
            onCancel={props.onClose}
            onReplace={() => advance({ force: true })}
            onKeepBoth={() =>
              advance({ toLogicalName: keepBothName(logicalName, props.targetHas) })
            }
          />
        )}
      </DialogContent>
    </Dialog>
  );
}

function GateFooter({
  canBack,
  onBack,
  onCancel,
  children,
}: {
  canBack: boolean;
  onBack: () => void;
  onCancel: () => void;
  children: React.ReactNode;
}): React.JSX.Element {
  return (
    <DialogFooter className="gap-2 sm:justify-between">
      <div className="flex gap-2">
        {canBack && (
          <button type="button" data-testid="move-sheet-back" onClick={onBack} className="text-sm">
            Back
          </button>
        )}
        <button
          type="button"
          data-testid="move-sheet-cancel"
          onClick={onCancel}
          className="text-sm text-muted-foreground"
        >
          Cancel
        </button>
      </div>
      <div className="flex gap-2">{children}</div>
    </DialogFooter>
  );
}

function PrivacyGate({
  fileName,
  target,
  op,
  canBack,
  onBack,
  onCancel,
  onConfirm,
}: {
  fileName: string;
  target: Context;
  op: ContextMobilityOp;
  canBack: boolean;
  onBack: () => void;
  onCancel: () => void;
  onConfirm: () => void;
}): React.JSX.Element {
  const { body, confirmLabel } = privacyGateCopy(fileName, target, op);
  return (
    <div data-testid="move-gate-privacy">
      <p className="text-sm">{body}</p>
      <GateFooter canBack={canBack} onBack={onBack} onCancel={onCancel}>
        <button type="button" data-testid="move-gate-privacy-confirm" onClick={onConfirm}>
          {confirmLabel}
        </button>
      </GateFooter>
    </div>
  );
}

function CollisionGate({
  target,
  canBack,
  onBack,
  onCancel,
  onReplace,
  onKeepBoth,
}: {
  target: Context;
  canBack: boolean;
  onBack: () => void;
  onCancel: () => void;
  onReplace: () => void;
  onKeepBoth: () => void;
}): React.JSX.Element {
  return (
    <div data-testid="move-gate-collision">
      <p className="text-sm">
        Something with that name already exists at {target.label}. Keep both, or replace it?
      </p>
      <GateFooter canBack={canBack} onBack={onBack} onCancel={onCancel}>
        <button type="button" data-testid="move-gate-collision-keep-both" onClick={onKeepBoth}>
          Keep both
        </button>
        <button type="button" data-testid="move-gate-collision-replace" onClick={onReplace}>
          Replace
        </button>
      </GateFooter>
    </div>
  );
}
