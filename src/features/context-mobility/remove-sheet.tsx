// SPDX-License-Identifier: MIT
import { useCallback, useMemo, useState } from "react";
import { toast } from "sonner";

import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import type { Context } from "@/shared/lib/context-model";
import { planRemoveGate, removeGateCopy, removeOutcomeToast } from "@/shared/lib/remove-gate";
import { endpointForContextId } from "@/features/context-mobility/move-controller";
import { removeContentFromContext, type RemoveFromContextOutcome } from "@/tauri/commands";

/**
 * SLW-PT-14 — the REMOVE confirm sheet. A SHARED item gets a plain confirm; a
 * scratch item gets the §J sole-copy warning:
 *
 *   - `normal`        → "Remove '<file>'?" + Remove.
 *   - `soleCopyBlock` → "may exist only on this machine": Delete permanently /
 *                       Cancel.
 *
 * The removal fires the resolved `remove_content_from_context` backend and every
 * outcome closes with a toast — no dead-end, no silent loss. NO banned internal
 * word appears in any string (copy is centralized in `remove-gate.ts`).
 */
export interface RemoveSheetProps {
  open: boolean;
  logicalName: string;
  fileName: string;
  /** The Context the item lives in (already known). */
  source: Context;
  /** The local profile's id. */
  userId: number;
  /** Called after a successful remove so the caller can refresh. */
  onRemoved?: () => void;
  /** Close the sheet (Cancel or completion). */
  onClose: () => void;
}

type Phase = "confirm" | "running";

export function RemoveSheet(props: RemoveSheetProps): React.JSX.Element {
  const { open, logicalName, fileName, source, userId } = props;
  const gate = useMemo(() => planRemoveGate(source.id), [source.id]);
  const copy = useMemo(() => removeGateCopy(fileName, gate), [fileName, gate]);
  const [phase, setPhase] = useState<Phase>("confirm");

  const doRemove = useCallback(async () => {
    const endpoint = endpointForContextId(source.id);
    if (!endpoint) {
      toast.error(`Couldn't remove '${fileName}' — try again in a moment`);
      props.onClose();
      return;
    }
    setPhase("running");
    try {
      const outcome: RemoveFromContextOutcome = await removeContentFromContext({
        context: endpoint,
        logicalName,
        userId,
      });
      const t = removeOutcomeToast(outcome.kind, fileName);
      toast[t.kind](t.message);
      if (outcome.kind === "removed") props.onRemoved?.();
      props.onClose();
    } catch {
      // A real I/O failure: the item is intact. Invite a retry (never a dead-end).
      toast.error(`Couldn't remove '${fileName}' — it's still here. Try again?`);
      props.onClose();
    }
  }, [source.id, logicalName, userId, fileName, props]);

  return (
    <Dialog open={open} onOpenChange={(o) => (!o ? props.onClose() : undefined)}>
      <DialogContent data-testid="remove-sheet">
        <DialogHeader>
          <DialogTitle data-testid="remove-sheet-header">{copy.title}</DialogTitle>
        </DialogHeader>

        {phase === "running" && (
          <p className="text-sm text-muted-foreground" data-testid="remove-sheet-running">
            Removing…
          </p>
        )}

        {phase === "confirm" && (
          <div data-testid={`remove-gate-${gate.kind}`}>
            {copy.body && <p className="text-sm">{copy.body}</p>}
            <DialogFooter className="gap-2 sm:justify-between">
              <button
                type="button"
                data-testid="remove-sheet-cancel"
                onClick={props.onClose}
                className="text-sm text-muted-foreground"
              >
                Cancel
              </button>
              <button
                type="button"
                data-testid="remove-sheet-confirm"
                className="text-destructive"
                onClick={() => void doRemove()}
              >
                {copy.confirmLabel}
              </button>
            </DialogFooter>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
