// SPDX-License-Identifier: MIT
import type { Context } from "@/shared/lib/context-model";
import type { ContextMobilityOp } from "@/shared/lib/move-gate-ladder";

/**
 * SLW-PT-16 — cross-org carry-switch-resume (§K).
 *
 * Choosing a disabled cross-org target CAPTURES the pending op (verb, source
 * context + logical id, intended target org/team), switches the active org, then
 * RESUMES — re-presenting the picker pre-scoped with the op restated ("Continue
 * moving '<file>' into <OtherOrg> › <Team>?"). Collision + privacy gates then
 * apply normally (the SLW-PT-12 ladder). Cancel at any point leaves both orgs
 * untouched — the captured op is dropped and nothing fired.
 *
 * This module is the PURE capture record + its restatement copy. The React sheet
 * holds one `PendingCrossOrgOp | null`, switches org, and on resume re-presents
 * the target pre-selected. Keeping the record + copy here keeps the continuation
 * unit-tested and free of banned internal words.
 */

/** The captured pending op — everything needed to resume after the org switch,
 *  and nothing else (so a stale capture can't fire against the wrong item). */
export interface PendingCrossOrgOp {
  op: ContextMobilityOp;
  /** The source context (already known — the picker never re-asks). */
  source: Context;
  /** The item's content-root-relative logical name + display name. */
  logicalName: string;
  fileName: string;
  /** The org to switch INTO to place the content. */
  targetOrgSlug: string;
  /** The target org's display name, for the restatement copy. */
  targetOrgName: string;
  /** The chosen target context (its id is stable across the org switch). */
  target: Context;
}

/** Capture a pending cross-org op from a disabled picker row's choice. Returns
 *  a self-contained record; the caller then switches org and resumes. */
export function captureCrossOrgOp(input: {
  op: ContextMobilityOp;
  source: Context;
  logicalName: string;
  fileName: string;
  targetOrgSlug: string;
  targetOrgName: string;
  target: Context;
}): PendingCrossOrgOp {
  return {
    op: input.op,
    source: input.source,
    logicalName: input.logicalName,
    fileName: input.fileName,
    targetOrgSlug: input.targetOrgSlug,
    targetOrgName: input.targetOrgName,
    target: input.target,
  };
}

/** The resume restatement (§K) shown once the org is switched — restates the op
 *  so the user confirms the continuation, never "where was I?". Uses the target's
 *  human label straight from the Context vocabulary (no internal word). */
export function resumeRestatement(pending: PendingCrossOrgOp): string {
  const verb = pending.op === "copy" ? "copying" : "moving";
  return `Continue ${verb} '${pending.fileName}' into ${pending.targetOrgName} › ${pending.target.label}?`;
}
