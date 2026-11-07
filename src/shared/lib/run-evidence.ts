// SPDX-License-Identifier: MIT
import type { DoneStatus } from "@/shared/lib/managed-session";

/**
 * SW-AU-82A — the FE mirror of the Rust `run_evidence` aggregate DTO (camelCase on the wire).
 * The `RunEvidence` view (Slice 6) opens `read_run_evidence(storyId, runId)` and renders the
 * immutable audit header + the causal identity spine. Evidence bodies (transcript/artifact per
 * attempt) are fetched through the existing per-run readers keyed by `Attempt.runId`.
 */

/** Mirrors Rust `run_store::StepResult` (serde lowercase). */
export type StepResult = "passed" | "failed" | "pending";

/**
 * The inspectable trigger step (AC-11). On the current store the causing event is not persisted
 * per run (AU-74), so a run is either `attributed` to a coarse source or renders an explicit
 * `preEnvelope` step — never a fabricated event.
 */
export type TriggerFact = { kind: "attributed"; source: string } | { kind: "preEnvelope" };

/** One attempt at a step execution — owns the run's transcript/artifact evidence via `runId`. */
export interface Attempt {
  attemptId: string;
  attemptOrdinal: number;
  runId: string;
  result: StepResult;
}

/** The runtime execution of one definition `node` (AD-6): `nodeId` = definition, `stepExecutionId` = runtime. */
export interface StepExecution {
  stepExecutionId: string;
  nodeId: string;
  attempts: Attempt[];
}

/** The causal identity spine of one Automation Run (AD-6). */
export interface AutomationRun {
  runId: string;
  /** Cross-run causation — unpopulated on the current store (child-Run identity is AU-81A). */
  parentRunId?: string;
  originNodeId?: string;
  correlationId: string;
  steps: StepExecution[];
}

/** The immutable, scrubbed audit header (partial on the current store; full population is AU-74). */
export interface RunAuditHeader {
  runId: string;
  storyId: string;
  automationId?: string;
  trigger: TriggerFact;
  scheduledAt?: string;
  firedAt?: string;
  startedAt?: string;
  /**
   * Terminal facts — absent on a live (non-terminal) Run. Always present when the header comes
   * from the current store (built from a terminal outcome); the durable authority (SW-AU-82B)
   * projects them from `run.completed`, so a live durable Run renders honestly (in progress).
   */
  endedAt?: string;
  status?: DoneStatus;
  summary?: string;
  /**
   * The pinned immutable `WorkflowRevision` id the Run was accepted for (AU-79A/AU-80E), folded from
   * the durable provenance; absent on the current-store fallback authority. SW-AU-90 reads this to
   * fetch the Run's frozen graph — the overlay shows "pinned revision unavailable" when it is absent.
   */
  workflowRevisionId?: string;
}

/**
 * Which identity authority served this evidence (SW-AU-82B). The layout is identical either way;
 * `currentStoreFallback` only adds a quiet "durable authority: unavailable" provenance cue.
 */
export type RunAuthority = "durable" | "currentStoreFallback";

/**
 * The kind of a reported artifact or footprint entry — a file, a PR/MR, or an external entity.
 * Drives SW-AU-97's per-kind affordance (preview / open / deep-link). Mirrors Rust `ArtifactKind`.
 */
export type ArtifactKind = "file" | "pr" | "entity";

/** Whether a footprint entry was created or modified by the step. Mirrors Rust `FootprintChange`. */
export type FootprintChange = "created" | "modified";

/** One named artifact a step produced — a stable ref (path / URL / id) plus its kind. */
export interface StepArtifact {
  kind: ArtifactKind;
  reference: string;
  label?: string;
}

/** One entity a step created or modified (Dagster-materialization style). */
export interface FootprintEntry {
  kind: ArtifactKind;
  reference: string;
  change: FootprintChange;
}

/** One typed key/value output a step declared; `value` is arbitrary JSON (table/JSON toggle). */
export interface StepOutput {
  key: string;
  value: unknown;
}

/**
 * Per-node reported result (AD-15 layer 1) — SW-AU-96 fills the four declared channels
 * (summary / typed outputs / produced artifacts / touched footprint); SW-AU-97 renders them.
 * Mirrors Rust `run_evidence::PerNodeResult`.
 */
export interface PerNodeResult {
  nodeId: string;
  summary?: string;
  outputs?: StepOutput[];
  artifacts?: StepArtifact[];
  footprint?: FootprintEntry[];
}

/** The one DTO the `RunEvidence` view opens. */
export interface RunEvidence {
  header: RunAuditHeader;
  run: AutomationRun;
  perNodeResults: PerNodeResult[];
  /** The identity authority that served this evidence (SW-AU-82B). */
  authority: RunAuthority;
}
