// SPDX-License-Identifier: MIT
import { useCallback, useMemo } from "react";
import { Lock } from "lucide-react";

import { Dialog, DialogContent, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import type { Context } from "@/shared/lib/context-model";
import type { ContextMobilityOp } from "@/shared/lib/move-gate-ladder";
import {
  buildTargetPicker,
  type TargetPicker,
  type TargetPickerRow,
} from "@/features/context-mobility/target-picker";
import {
  captureCrossOrgOp,
  resumeRestatement,
  type PendingCrossOrgOp,
} from "@/features/context-mobility/cross-org-resume";
import type { Org } from "@/shared/types/domain";

/**
 * SLW-PT-16 — the move/copy TARGET SHEET: step 1 of the SLW-PT-12/13 stepped
 * sheet (§E/§K). It presents the org-scoped target picker (the caller already
 * knows the SOURCE context — it never asks which team you're demoting from):
 *
 *   - Same-org targets + always the scratch space are selectable; choosing one hands the
 *     (source, target) pair to the caller, which opens the SLW-PT-12 MoveSheet
 *     to run the privacy/sole-copy/collision gates.
 *   - Cross-org targets are disabled-with-reason; choosing one CAPTURES the
 *     pending op, asks the caller to switch org, then RESUMES with the op
 *     restated ("Continue moving '<file>' into <OtherOrg> › <Team>?"). Cancel at
 *     any point leaves both orgs untouched (the capture is dropped).
 *   - Empty cases resolve to a labeled next step (zero teams ⇒ Create a team),
 *     never a bare empty list.
 */
export interface MoveTargetSheetProps {
  open: boolean;
  op?: ContextMobilityOp;
  /** The item being moved/copied (already known). */
  logicalName: string;
  fileName: string;
  /** The source context — never re-asked. */
  source: Context;
  /** The active org slug (same-org rows are selectable). */
  activeOrgSlug: string;
  /** Every loaded org (drives same-org + cross-org rows). */
  orgs: readonly Org[];
  /** The caller picked a same-org target → open the SLW-PT-12 MoveSheet. */
  onPickTarget: (target: Context) => void;
  /** The caller must switch the active org (to `pending.targetOrgSlug`), then
   *  re-open this sheet with the captured op as `resuming` (the op restates +
   *  pre-selects). The full capture is handed up so the caller can restate it. */
  onSwitchOrg: (pending: PendingCrossOrgOp) => void;
  /** Zero-teams create-a-team next step (§E). */
  onCreateTeam?: () => void;
  onClose: () => void;
  /** When resuming after a cross-org switch, the captured op to restate. */
  resuming?: PendingCrossOrgOp | null;
}

export function MoveTargetSheet(props: MoveTargetSheetProps): React.JSX.Element {
  const { open, source, activeOrgSlug, orgs, fileName, logicalName } = props;
  const op: ContextMobilityOp = props.op ?? "move";

  const picker: TargetPicker = useMemo(
    () => buildTargetPicker({ source, activeOrgSlug, orgs, op }),
    [source, activeOrgSlug, orgs, op],
  );

  // Choosing a disabled cross-org row CAPTURES the pending op and asks the
  // caller to switch org; the caller re-opens this sheet with `resuming` set so
  // the op restates + pre-selects. Cancel (below) drops the op — both orgs
  // untouched, no abandoned op.
  const chooseCrossOrg = useCallback(
    (row: TargetPickerRow) => {
      if (!row.orgSlug) return;
      const targetOrg = orgs.find((o) => o.slug === row.orgSlug);
      const captured = captureCrossOrgOp({
        op,
        source,
        logicalName,
        fileName,
        targetOrgSlug: row.orgSlug,
        targetOrgName: targetOrg?.display_name ?? row.orgSlug,
        target: row.context,
      });
      props.onSwitchOrg(captured);
    },
    [orgs, op, source, logicalName, fileName, props],
  );

  const cancel = useCallback(() => {
    props.onClose();
  }, [props]);

  // On resume, the caller re-opens with `resuming` set — restate the op and let
  // the user confirm continuation into the pre-selected cross-org target.
  const resuming = props.resuming ?? null;

  const verb = op === "copy" ? "Copy" : "Move";

  return (
    <Dialog open={open} onOpenChange={(o) => (!o ? cancel() : undefined)}>
      <DialogContent data-testid="move-target-sheet">
        <DialogHeader>
          <DialogTitle data-testid="move-target-header">
            {resuming ? resumeRestatement(resuming) : `${verb} '${fileName}' to…`}
          </DialogTitle>
        </DialogHeader>

        {resuming ? (
          <div data-testid="move-target-resume" className="flex flex-col gap-2">
            <p className="text-sm text-muted-foreground">
              You switched to {resuming.targetOrgName}. Continue placing the item here?
            </p>
            <div className="flex justify-end gap-2">
              <button
                type="button"
                data-testid="move-target-resume-cancel"
                className="text-sm text-muted-foreground"
                onClick={cancel}
              >
                Cancel
              </button>
              <button
                type="button"
                data-testid="move-target-resume-continue"
                className="rounded-md bg-primary px-3 py-1.5 text-sm font-medium text-primary-foreground"
                onClick={() => props.onPickTarget(resuming.target)}
              >
                Continue
              </button>
            </div>
          </div>
        ) : (
          <div className="flex flex-col gap-1">
            {picker.personal && (
              <TargetRow
                row={picker.personal}
                onPick={() => props.onPickTarget(picker.personal!.context)}
              />
            )}

            {picker.sameOrg.map((row) => (
              <TargetRow
                key={row.context.id}
                row={row}
                onPick={() => props.onPickTarget(row.context)}
              />
            ))}

            {/* §E: never a bare empty list — a labeled next step instead. */}
            {picker.nextStep?.kind === "createTeam" && (
              <button
                type="button"
                data-testid="move-target-create-team"
                className="flex w-full items-center rounded px-2 py-1.5 text-left text-sm text-primary hover:bg-accent"
                onClick={() => props.onCreateTeam?.()}
              >
                {picker.nextStep.label}
              </button>
            )}

            {/* Cross-org rows: disabled-with-reason, never omitted (§E). Choosing
                one drives the carry-switch-resume flow (§K). */}
            {picker.crossOrg.length > 0 && (
              <div className="mt-1 px-2 pt-1 text-xs font-medium text-muted-foreground">
                Other orgs
              </div>
            )}
            {picker.crossOrg.map((row) => (
              <button
                key={row.context.id}
                type="button"
                data-testid={`move-target-cross-org-${row.context.id}`}
                aria-disabled="true"
                className="flex w-full flex-col items-start rounded px-2 py-1.5 text-left text-sm opacity-60 hover:bg-accent"
                onClick={() => chooseCrossOrg(row)}
              >
                <span className="truncate">{row.context.label}</span>
                <span className="truncate text-xs text-muted-foreground">{row.reason}</span>
              </button>
            ))}

            <div className="mt-2 flex justify-end">
              <button
                type="button"
                data-testid="move-target-cancel"
                className="text-sm text-muted-foreground"
                onClick={cancel}
              >
                Cancel
              </button>
            </div>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}

function TargetRow({
  row,
  onPick,
}: {
  row: TargetPickerRow;
  onPick: () => void;
}): React.JSX.Element {
  return (
    <button
      type="button"
      data-testid={`move-target-option-${row.context.id}`}
      className="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-sm hover:bg-accent"
      onClick={onPick}
    >
      {row.context.kind === "personal" && (
        <Lock size={12} className="flex-shrink-0" aria-hidden="true" />
      )}
      <span className="truncate">{row.context.label}</span>
    </button>
  );
}
