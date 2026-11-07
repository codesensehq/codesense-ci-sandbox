// SPDX-License-Identifier: MIT
import { parseContextId, type ContextId, type ContextSharing } from "@/shared/lib/context-model";

/**
 * SLW-PT-12 — the ONE deterministic gate ladder for a content MOVE between two
 * Contexts (§P). A MOVE closes with no dead-ends: every gate that *applies* is
 * surfaced, exactly once, in a fixed precedence, as ONE stepped flow.
 *
 * Precedence (§P) — evaluated one gate at a time, in this order:
 *
 *   1. cross_org  — the two contexts live in different orgs. Rejected /
 *                   disabled-with-reason here (the carry-switch-resume flow is
 *                   SLW-PT-16; this module only *detects* it and stops the
 *                   ladder so nothing downstream fires against a cross-org
 *                   target).
 *   2. privacy    — moving a scratch item into any shared (team) context
 *                   makes it visible to everyone on that team (§I). A "Share it"
 *                   confirm gates it. shared -> scratch (making it MORE private)
 *                   needs NO gate.
 *   3. collision  — the target already holds an item at this logical id. The
 *                   Keep-both / Replace / Cancel chooser (§P step 4, LAST).
 *
 * A move or copy never asks about a sole copy: the item lands somewhere (a team,
 * all orgs, another org's scratch space), so nothing is lost. That warning
 * belongs to REMOVE (`remove-gate.ts`).
 *
 * This module is PURE: it decides which gates apply and in what order from the
 * two ContextIds plus a small collision fact bag. The stepped sheet and
 * the backend move are wired on top; the ordering itself is unit-tested here so
 * the §P precedence can never silently drift.
 */

/** The gate kinds, in §P precedence order. */
export type MoveGateKind = "crossOrg" | "privacy" | "collision";

/** SLW-PT-13 — the operation the ladder is planning. A COPY retains the source;
 *  the gates and their precedence are shared with MOVE (only the cross-org rule
 *  for scratch-to-scratch differs). */
export type ContextMobilityOp = "move" | "copy";

/** The facts the ladder needs beyond the two ContextIds. */
export interface MoveGateFacts {
  /** The stable logical id of the source context. */
  from: ContextId;
  /** The stable logical id of the target context. */
  to: ContextId;
  /** Whether an item already exists at the target under this logical name. */
  targetCollision: boolean;
  /** SLW-PT-13 — MOVE (default) or COPY. A COPY retains the source. */
  op?: ContextMobilityOp;
}

/** One planned gate step. */
export interface MoveGateStep {
  kind: MoveGateKind;
}

/** The planned ladder for a move. `blocked` is set for a cross-org move: the
 *  ladder stops at the cross-org gate and the move must not proceed here (the
 *  carry-switch-resume flow lives in SLW-PT-16). */
export interface MoveGatePlan {
  /** The gate steps that apply, in §P precedence order. */
  steps: MoveGateStep[];
  /** `true` when the move is cross-org and cannot proceed in this story. */
  blocked: boolean;
}

/** Whether a ContextId denotes the scratch space. */
export function isPersonalContext(id: ContextId): boolean {
  return parseContextId(id)?.kind === "personal";
}

/** The org a ContextId belongs to, for the cross-org gate. The scratch id names
 *  the OPEN org's space, so it is `null` here and never triggers cross-org on its
 *  own. View-only repos are also org-agnostic (`null`). */
export function contextOrg(id: ContextId): string | null {
  const parsed = parseContextId(id);
  if (!parsed) return null;
  switch (parsed.kind) {
    case "team-all":
    case "team-repo":
      return parsed.orgSlug ?? null;
    case "personal":
      // A named org's scratch space carries its org; the bare id (the org on
      // screen) and all-orgs never make a pair cross-org on their own.
      return parsed.orgSlug ?? null;
    case "view-only-repo":
      return null;
  }
}

/** Whether a ContextId names one org's scratch space explicitly. */
function isNamedScratchSpace(id: ContextId): boolean {
  const parsed = parseContextId(id);
  return parsed?.kind === "personal" && parsed.orgSlug !== undefined;
}

/** The privacy classification of a ContextId (§I): scratch is `personal`,
 *  every team/shared context is `shared`. */
export function contextSharing(id: ContextId): ContextSharing {
  return isPersonalContext(id) ? "personal" : "shared";
}

/** Two contexts are cross-org when BOTH resolve to a concrete org and those
 *  orgs differ. Scratch / view-only (org-agnostic here) never make a pair
 *  cross-org. */
export function isCrossOrgMove(from: ContextId, to: ContextId): boolean {
  const a = contextOrg(from);
  const b = contextOrg(to);
  return a !== null && b !== null && a !== b;
}

/**
 * Plan the §P gate ladder for a move. Deterministic and pure: the returned
 * `steps` are exactly the gates that apply, in the fixed precedence order
 * `crossOrg -> privacy -> collision`.
 *
 * - **crossOrg**: present (and `blocked`) iff the two contexts are in different
 *   orgs. When blocked, no other gate is planned — the move can't proceed here.
 * - **privacy**: present iff the source is scratch and the target is shared
 *   (scratch -> team). shared -> scratch is omitted (no gate to make something
 *   MORE private).
 * - **collision**: present iff `targetCollision`. Always LAST.
 */
export function planMoveGates(facts: MoveGateFacts): MoveGatePlan {
  const { from, to, targetCollision, op = "move" } = facts;

  // 1. cross_org — evaluated first; when it applies the ladder stops. "Copy to
  //    another org" (one org's scratch space into another's) names both orgs and
  //    keeps the original, so it needs no org switch.
  const scratchToScratchCopy =
    op === "copy" && isNamedScratchSpace(from) && isNamedScratchSpace(to);
  if (!scratchToScratchCopy && isCrossOrgMove(from, to)) {
    return { steps: [{ kind: "crossOrg" }], blocked: true };
  }

  const steps: MoveGateStep[] = [];
  const sourcePersonal = isPersonalContext(from);
  const targetShared = contextSharing(to) === "shared";

  // 2. privacy — scratch -> shared only (§I). shared -> scratch needs no gate.
  if (sourcePersonal && targetShared) {
    steps.push({ kind: "privacy" });
  }

  // 3. collision — LAST (§P step 4).
  if (targetCollision) {
    steps.push({ kind: "collision" });
  }

  return { steps, blocked: false };
}
