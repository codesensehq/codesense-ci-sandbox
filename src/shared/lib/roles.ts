// SPDX-License-Identifier: MIT
/**
 * SW-GW-4 — the semantic **role** contract (TS mirror of `src-tauri/src/roles.rs`).
 *
 * A role is the ONE finite thing the live-progress `phase` (SW-GW-4) and the
 * durable, team-configurable story `status` (SW-SC-1) agree on. The UI keys the
 * progress chip off the *role*, never off a renameable status literal — so a team
 * can rename/reorder/add/remove its `status` states without breaking anything.
 *
 * This module owns the FIXED `phase → role` map. SW-SC-1 extends it with the
 * data-driven `status → role` map from the team taxonomy. The Rust and TS wire
 * tokens are byte-identical (pinned by the parity coverage in SW-GW-4 T6).
 */

/** The finite semantic role set. Wire tokens are snake_case, matching Rust. */
export type Role = "not_started" | "active" | "review" | "done" | "blocked";

/**
 * The FIXED live-progress lifecycle phase. Distinct from durable `status`: it
 * tracks where in the run a story is. The set is closed (a new phase is a code
 * change, not caller-supplied) so the trust surface + UI stay finite (AC-2).
 */
export type Phase = "planning" | "dev" | "review" | "pr" | "blocked" | "done";

/** Every phase, in lifecycle order — mirror of the Rust `PHASES` slice. */
export const PHASES: readonly Phase[] = ["planning", "dev", "review", "pr", "blocked", "done"];

/**
 * The fixed `phase → role` map — the phase half of the shared contract.
 * `planning → not_started` and `pr → review` keep the phase's role aligned with
 * the durable status of the same word, so chip and resting status render alike.
 */
export const PHASE_ROLE: Record<Phase, Role> = {
  planning: "not_started",
  dev: "active",
  review: "review",
  pr: "review",
  blocked: "blocked",
  done: "done",
};

/** Resolve a phase to its role. */
export function roleForPhase(phase: Phase): Role {
  return PHASE_ROLE[phase];
}

/** Narrow an untrusted string to a `Phase` (returns null when off-enum). */
export function asPhase(s: string): Phase | null {
  return (PHASES as readonly string[]).includes(s) ? (s as Phase) : null;
}
