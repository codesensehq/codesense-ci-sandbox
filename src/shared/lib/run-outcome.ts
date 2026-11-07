// SPDX-License-Identifier: MIT
/**
 * SW-OR-12 — the durable run-outcome mirror + the shared outcome→chip adapter.
 *
 * `RunOutcome` mirrors the Rust `run_store::RunOutcomeEvent` (SW-AU-52) on the wire (snake_case:
 * that record carries no serde rename, and changing it would break the append-only store's
 * back-compat — so the mirror matches the other run_store shapes in `commands.ts`). It is returned
 * by the `read_run_outcome` command keyed by `run_id` and drives the `AgentResultPanel` header.
 */

import type { AgentStatus } from "@/shared/lib/agent-status-config";
import type { DoneStatus } from "@/shared/lib/managed-session";

/**
 * The durable terminal outcome for a run — the FE mirror of Rust `run_store::RunOutcomeEvent`.
 * `status` reuses `DoneStatus` (`success | failed | needs-attention`, the same vocab the Rust
 * `SessionStatus` serializes to). `artifact_ref` (SW-OR-12) points at the persisted transcript,
 * but the panel reads the body by `run_id` regardless — the ref is a presence hint, not the path.
 */
export interface RunOutcome {
  story_id: string;
  run_id: string;
  automation_id?: string | null;
  trigger_source?: string | null;
  workspace?: string | null;
  scheduled_at?: string | null;
  fired_at?: string | null;
  status: DoneStatus;
  summary?: string | null;
  exit_code?: number | null;
  started_at?: string | null;
  ended_at: string;
  artifact_ref?: string | null;
  /**
   * SW-OR-15 (AC-9): `true` when this run declared an interactive structured-transcript capability.
   * Lets the panel tell a benign "No output captured" (no capability) apart from "declared but no
   * turns landed" (crash / torn / locked session log) — the interactive arm writes no `.log` floor,
   * so those two otherwise look identical. Absent for a headless run or an undeclared interactive run.
   */
  structured_declared?: boolean | null;
  source?: string;
}

/**
 * SW-BA-8: the **display** state of a completed run — the FE mirror of Rust
 * `run_store::ValidatedRunOutcome` (kebab-case on the wire). Distinct from `DoneStatus` (the raw
 * *process* outcome).
 */
export type ValidatedRunOutcome = "completed" | "needs-attention" | "failed";

/**
 * SW-OR-13 (AC-1): the widened `read_run_outcome` payload — the raw durable process outcome paired
 * with its display state, so ONE read drives both the live-completion chip swap and
 * the durable SW-AU-55 row from the same composed source (they cannot diverge).
 */
export interface RunOutcomeRead {
  outcome: RunOutcome;
  validated: ValidatedRunOutcome;
  /** True only when the managed `{run_id}.turns.json` artifact exists. */
  has_structured_transcript?: boolean;
}

export interface RunDiffStat {
  changedFiles: number;
  additions: number;
  deletions: number;
}

export interface RunWorkProduct {
  branch: string;
  changeRequest?: {
    noun: string;
    number: number;
    url: string;
    state: "draft" | "open" | "merged" | "closed";
    diffStat?: RunDiffStat | null;
  } | null;
}

/**
 * SW-OR-13 (AC-1): the terminal chip — the ONE adapter the live-completion swap AND the durable
 * SW-AU-55 row use, so they cannot drift. Maps the SW-BA-8 `ValidatedRunOutcome` (composed
 * trusted-side) to the SW-BA-5 chip vocabulary: `completed` ✓, the red `failed` chip for a hard
 * failure, else `dropped` (a run merely needing a human).
 *
 * SW-OR-20: the durable `failed` and `needs-attention` arms split — a process-aware hard failure
 * reads its own red chip, distinct from orange `dropped`. Live surfaces use the same vocabulary but
 * can select `failed` only when the agent explicitly attests it.
 */
export function validatedOutcomeChip(validated: ValidatedRunOutcome): AgentStatus {
  switch (validated) {
    case "completed":
      return "completed";
    case "failed":
      return "failed";
    case "needs-attention":
      return "dropped";
  }
}
