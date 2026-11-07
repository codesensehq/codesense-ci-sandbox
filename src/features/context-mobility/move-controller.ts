// SPDX-License-Identifier: MIT
import { parseContextId, type Context, type ContextId } from "@/shared/lib/context-model";
import {
  planMoveGates,
  type ContextMobilityOp,
  type MoveGateFacts,
  type MoveGateKind,
  type MoveGatePlan,
  type MoveGateStep,
} from "@/shared/lib/move-gate-ladder";
import type { MoveBetweenContextsOutcome, MoveContextEndpoint } from "@/tauri/commands";

/**
 * SLW-PT-12 — the move CONTROLLER: the pure orchestration between the §P gate
 * ladder (`move-gate-ladder.ts`) and the `move_content_between_contexts`
 * backend. The React move sheet mounts this; keeping it pure keeps the stepped
 * flow, the endpoint mapping, the Keep-both rename, and the outcome copy all
 * unit-tested — no banned internal words, no dead-ends.
 */

/** Map a stable {@link ContextId} to the backend move endpoint. Returns `null`
 *  for a view-only repo (never a move endpoint) or a malformed id. */
export function endpointForContextId(id: ContextId): MoveContextEndpoint | null {
  const parsed = parseContextId(id);
  if (!parsed) return null;
  switch (parsed.kind) {
    case "personal":
      if (parsed.allOrgs) return { kind: "scratchAllOrgs" };
      return parsed.orgSlug ? { kind: "personal", orgSlug: parsed.orgSlug } : { kind: "personal" };
    case "team-all":
      return { kind: "teamAll", orgSlug: parsed.orgSlug ?? "", teamName: parsed.teamName ?? "" };
    case "team-repo":
      return {
        kind: "teamRepo",
        orgSlug: parsed.orgSlug ?? "",
        teamName: parsed.teamName ?? "",
        codeRepoSlug: parsed.repoSlug ?? { owner: "", repo: "" },
      };
    case "view-only-repo":
      return null;
  }
}

/** The verb the op restates in the UI — "Moving" / "Copying". */
function opGerund(op: ContextMobilityOp): string {
  return op === "copy" ? "Copying" : "Moving";
}

/** The persistent stepped-sheet header restating the op (§P) — "Moving '<file>'
 *  → <Team>" (or "Copying" for a COPY), plus "Step N of M" when there is more
 *  than one gate. Uses the target's human label straight from the Context
 *  vocabulary (no internal words). */
export function moveHeader(
  fileName: string,
  target: Context,
  stepIndex: number,
  stepCount: number,
  op: ContextMobilityOp = "move",
): string {
  const base = `${opGerund(op)} '${fileName}' → ${target.label}`;
  return stepCount > 1 ? `${base} — Step ${stepIndex + 1} of ${stepCount}` : base;
}

/** The privacy gate body (§I): scratch -> a team makes it visible to everyone.
 *  For a MOVE it ALSO removes the item from the scratch space; a COPY keeps the
 *  original, so the "removes it" clause is omitted (SLW-PT-13). */
export function privacyGateCopy(
  fileName: string,
  target: Context,
  op: ContextMobilityOp = "move",
): {
  body: string;
  confirmLabel: string;
} {
  const removalClause = op === "copy" ? "" : ", and takes it out of Private";
  return {
    body: `This makes '${fileName}' visible to everyone on ${target.label}${removalClause}. Continue?`,
    confirmLabel: "Share it",
  };
}

/** Compute a Keep-both target logical name by inserting a `-N` suffix before the
 *  extension, incrementing until it doesn't collide. `exists` reports whether a
 *  candidate already lives at the target. Mirrors the never-silent-"notes-2"
 *  rule — the chosen name is surfaced in the success toast. */
export function keepBothName(logicalName: string, exists: (candidate: string) => boolean): string {
  const slash = logicalName.lastIndexOf("/");
  const dir = slash === -1 ? "" : logicalName.slice(0, slash + 1);
  const base = slash === -1 ? logicalName : logicalName.slice(slash + 1);
  const dot = base.lastIndexOf(".");
  const stem = dot <= 0 ? base : base.slice(0, dot);
  const ext = dot <= 0 ? "" : base.slice(dot);
  let n = 2;
  let candidate = `${dir}${stem}-${n}${ext}`;
  while (exists(candidate)) {
    n += 1;
    candidate = `${dir}${stem}-${n}${ext}`;
  }
  return candidate;
}

/** Plan the ladder for a move/copy, from the two contexts + facts. Thin
 *  re-export so the controller and the sheet share one entry point. The op
 *  defaults to MOVE; a COPY drops the sole_copy gate. */
export function planMove(
  from: ContextId,
  to: ContextId,
  facts: Pick<MoveGateFacts, "targetCollision" | "op">,
): MoveGatePlan {
  return planMoveGates({ from, to, ...facts });
}

/** Map a backend {@link MoveBetweenContextsOutcome} to a user-facing toast —
 *  no banned internal word, and the Keep-both rename surfaced by name. */
export function moveOutcomeToast(
  outcome: MoveBetweenContextsOutcome,
  ctx: { fileName: string; target: Context; renamedTo?: string },
): { kind: "success" | "info" | "warning" | "error"; message: string } {
  switch (outcome.kind) {
    case "movedSourceKept":
      return {
        kind: "warning",
        message: `Moved '${ctx.fileName}' to ${ctx.target.label}, but the original couldn't be removed (${outcome.reason}). Try again, or remove it yourself.`,
      };
    case "moved":
      return {
        kind: "success",
        message: ctx.renamedTo
          ? `Moved to ${ctx.target.label} as '${basename(ctx.renamedTo)}'`
          : `Moved '${ctx.fileName}' to ${ctx.target.label}`,
      };
    case "copied":
      return {
        kind: "success",
        message: ctx.renamedTo
          ? `Copied to ${ctx.target.label} as '${basename(ctx.renamedTo)}'`
          : `Copied '${ctx.fileName}' to ${ctx.target.label}`,
      };
    case "rejectedSameContext":
      return { kind: "info", message: `'${ctx.fileName}' is already there` };
    case "rejectedSourceMissing":
      return { kind: "error", message: `Couldn't find '${ctx.fileName}' to move` };
    case "rejectedTargetExists":
      // The FE resolves this via the collision chooser BEFORE calling; a
      // surfaced one means the chooser was skipped — surface, don't dead-end.
      return {
        kind: "error",
        message: `Something already exists at ${ctx.target.label}`,
      };
    case "rejectedUnbacked":
      return {
        kind: "error",
        message: `Couldn't move '${ctx.fileName}' — try again in a moment`,
      };
  }
}

/** The non-destructive retry toast for a target-write failure mid-op (§N). The
 *  source is intact (a COPY never touches it); invite a retry. */
export function moveFailureToast(fileName: string, op: ContextMobilityOp = "move"): string {
  const verb = op === "copy" ? "copy" : "move";
  return `Couldn't ${verb} '${fileName}' — it's still where it was. Try again?`;
}

function basename(logicalName: string): string {
  const slash = logicalName.lastIndexOf("/");
  return slash === -1 ? logicalName : logicalName.slice(slash + 1);
}

export type { ContextMobilityOp, MoveGateKind, MoveGateStep, MoveGatePlan };
