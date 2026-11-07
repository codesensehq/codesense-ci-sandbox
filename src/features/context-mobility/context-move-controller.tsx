// SPDX-License-Identifier: MIT
import { useCallback, useState } from "react";

import type { Context } from "@/shared/lib/context-model";
import type { ContextMobilityOp } from "@/shared/lib/move-gate-ladder";
import type { Org } from "@/shared/types/domain";
import { MoveActionMenu } from "@/features/context-mobility/move-action-menu";
import { MoveTargetSheet } from "@/features/context-mobility/move-target-sheet";
import { MoveSheet } from "@/features/context-mobility/move-sheet";
import { RemoveSheet } from "@/features/context-mobility/remove-sheet";
import type { PendingCrossOrgOp } from "@/features/context-mobility/cross-org-resume";

/**
 * SLW-PT-16 — the per-file mobility ENTRY POINT, wired end to end.
 *
 * Mounts the ⋯ "Move to… / Copy to…" action on a nav/file row and drives the
 * two-stage flow:
 *
 *   1. `MoveTargetSheet` (this story) — the org-scoped picker. The SOURCE is
 *      already known; the picker lists same-org targets + the scratch space, and cross-org
 *      targets disabled-with-reason. Choosing a same-org target advances to the
 *      SLW-PT-12 gate sheet; choosing a cross-org target switches org + resumes.
 *   2. `MoveSheet` (SLW-PT-12/13) — the privacy/sole-copy/collision gate ladder,
 *      then the backend move/copy.
 *
 * This component OWNS the flow state (which sheet is open, the chosen verb, the
 * chosen target, and the pending cross-org op across the org switch). The host
 * supplies the source, the item, the org context, and the collision facts
 * the gate sheet needs — plus the org-switch + create-team seams, which live in
 * the app shell.
 */
export interface ContextMoveControllerProps {
  /** The item's source context (already known — the picker never asks). */
  source: Context;
  logicalName: string;
  fileName: string;
  /** The active org slug + every loaded org (drives the picker rows). */
  activeOrgSlug: string;
  orgs: readonly Org[];
  /** The local profile's id. */
  userId: number;
  /** Whether an item already exists at the chosen target (collision gate). */
  targetCollision?: (target: Context) => boolean;
  /** Whether a candidate logical name exists at the target (Keep-both rename). */
  targetHas?: (target: Context, candidate: string) => boolean;
  /** Switch the active org (the cross-org carry-switch-resume seam, §K). Called
   *  with the captured op; the host switches org, then the controller re-presents
   *  the picker in resume mode. */
  onSwitchOrg: (pending: PendingCrossOrgOp) => void;
  /** Zero-teams "Create a team" seam (§E). */
  onCreateTeam?: () => void;
  /** Refresh after a completed move/copy. */
  onMoved?: () => void;
  /** Refresh after a completed remove. */
  onRemoved?: () => void;
}

type Stage =
  | { kind: "idle" }
  | { kind: "picking"; op: ContextMobilityOp; resuming: PendingCrossOrgOp | null }
  | { kind: "gating"; op: ContextMobilityOp; target: Context }
  | { kind: "removing" };

export function ContextMoveController(props: ContextMoveControllerProps): React.JSX.Element {
  const [stage, setStage] = useState<Stage>({ kind: "idle" });

  const startPicking = useCallback((op: ContextMobilityOp) => {
    setStage({ kind: "picking", op, resuming: null });
  }, []);

  const startRemoving = useCallback(() => setStage({ kind: "removing" }), []);

  const close = useCallback(() => setStage({ kind: "idle" }), []);

  const pickTarget = useCallback((target: Context) => {
    setStage((s) => (s.kind === "picking" ? { kind: "gating", op: s.op, target } : s));
  }, []);

  const switchOrg = useCallback(
    (pending: PendingCrossOrgOp) => {
      // Hand the capture up so the host switches org; then re-present the picker
      // in resume mode against the (now-active) target org.
      props.onSwitchOrg(pending);
      setStage({ kind: "picking", op: pending.op, resuming: pending });
    },
    [props],
  );

  const targetCollision =
    stage.kind === "gating" ? (props.targetCollision?.(stage.target) ?? false) : false;

  return (
    <>
      <MoveActionMenu
        source={props.source}
        logicalName={props.logicalName}
        fileName={props.fileName}
        onStart={startPicking}
        onRemove={startRemoving}
      />

      {stage.kind === "picking" && (
        <MoveTargetSheet
          open
          op={stage.op}
          logicalName={props.logicalName}
          fileName={props.fileName}
          source={props.source}
          activeOrgSlug={props.activeOrgSlug}
          orgs={props.orgs}
          resuming={stage.resuming}
          onPickTarget={pickTarget}
          onSwitchOrg={switchOrg}
          onCreateTeam={props.onCreateTeam}
          onClose={close}
        />
      )}

      {stage.kind === "gating" && (
        <MoveSheet
          open
          op={stage.op}
          logicalName={props.logicalName}
          fileName={props.fileName}
          source={props.source}
          target={stage.target}
          userId={props.userId}
          targetCollision={targetCollision}
          targetHas={(candidate) => props.targetHas?.(stage.target, candidate) ?? false}
          onMoved={props.onMoved}
          onClose={close}
        />
      )}

      {stage.kind === "removing" && (
        <RemoveSheet
          open
          logicalName={props.logicalName}
          fileName={props.fileName}
          source={props.source}
          userId={props.userId}
          onRemoved={props.onRemoved}
          onClose={close}
        />
      )}
    </>
  );
}
