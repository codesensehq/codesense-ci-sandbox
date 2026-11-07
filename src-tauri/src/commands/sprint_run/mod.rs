// SPDX-License-Identifier: MIT
//! Trusted-side run plumbing shared by routines and managed agent launches: the durable claim
//! loop, run identity and evidence, run artifacts/transcripts, and loop protection.

/// Causal lineage for loop protection, and the scrub-and-bound pass a persisted payload runs.
pub mod causation;
/// SW-AU-80E: the claim-driven dispatcher — the single consumer that drains the durable runnable
/// queue routine fires enqueue (each run executes once), plus restart re-drive.
pub mod claim_loop;
pub mod compaction;
/// SW-AU-82B: the same Run identity re-homed onto the durable `OrchestrationStore` as
/// append-only run events, rebuilt on read as an in-memory projection (AD-4, ARCH-INV-9).
pub mod durable_run_identity;
pub mod loop_guard;
pub mod run_artifact;
/// SW-AU-82A Slice 5: the evidence-aggregate command (`read_run_evidence`) — one off-thread fold
/// into `{ audit header, identity spine }`, gated + scrubbed at render.
pub mod run_evidence;
/// SW-AU-82A: causal Run identity (`AutomationRun`/`StepExecution`/`Attempt`) projected
/// over the current store — the definition-`node` vs runtime-`step` split (AD-6).
pub mod run_identity;
pub mod run_store;
/// SW-OR-16: the adapter-declared structured-transcript parser + `{run_id}.turns.json` store — the
/// optional turn-by-turn upgrade layered on `run_artifact`'s text floor.
pub mod run_transcript;
/// SW-AU-96B: per-file footprint auto-capture from a run's work-product worktree, folded through the
/// shipped `record_step_result` harness auto-capture seam.
pub mod work_product_footprint;
