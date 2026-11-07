// SPDX-License-Identifier: MIT
//! Causal Run identity over the current JSONL run store (SW-AU-82A).
//!
//! The definition unit is always a `node` (`node_id`); a Run executes each node as a
//! `StepExecution` (`step_execution_id`) with one or more `Attempt`s. "Step" names
//! **only** the runtime record — the definition unit is a node (AD-6). The two axes
//! carry distinct join keys so a read model never has to infer a relationship from a
//! display label (ARCH-INV-22).
//!
//! This is a **projection** over the records the store already holds — it re-keys the
//! existing verdict fold ([`run_store::run_step_verdicts`], reused, not re-folded) onto
//! the identity spine and mints no new persisted fact (ARCH-INV-24). The durable
//! authority for this same identity is re-homed onto the orchestration store by
//! SW-AU-82B, which imports these types unchanged.

use serde::{Deserialize, Serialize};

use super::run_store::StepResult;
use super::run_store::{self, StoreRecord};

/// The causal identity of one Automation Run, projected from the current store (AD-6).
///
/// `parent_run_id` / `origin_node_id` are the cross-run causation spine; on the current
/// store they are unpopulated (child-Run identity is SW-AU-81A) and render as an honest
/// `unavailable`, never a fabricated parent. `correlation_id` defaults to the Run's own
/// id until the normalized event envelope carries one (SW-AU-74).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRun {
    pub run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_node_id: Option<String>,
    pub correlation_id: String,
    pub steps: Vec<StepExecution>,
}

/// The runtime execution of one definition `node` within a Run.
///
/// `node_id` is the **definition** join key; `step_execution_id` is the **runtime** join
/// key. They are distinct by construction — `step_execution_id` is derived from the
/// definition `node_id`, never from a label — so two nodes with identical display labels
/// still resolve to their own `step_execution_id` (ARCH-INV-22).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepExecution {
    pub step_execution_id: String,
    pub node_id: String,
    pub attempts: Vec<Attempt>,
}

/// One attempt at a `StepExecution` — the owner of this step's agent-session /
/// transcript / artifact evidence. A retry appends a **new** `Attempt`; prior Attempt
/// evidence is never overwritten (SW-AU-82A AC-3, ARCH-INV-9). On the current store the
/// existing `run_id`-keyed evidence re-attaches to Attempt #1 (`attempt_ordinal == 1`),
/// which retains `run_id` as the compatibility projection key (AC-4).
///
/// Two numbering axes exist and are intentionally distinct. On disk the evidence rotates so
/// the legacy `{run_id}.<ext>` name is the *newest* attempt and `a1` the *oldest*
/// (`run_artifact::archive_prior_attempt`). This projection currently emits a **single**
/// `Attempt` (`attempt_ordinal == 1`) that points at the run's *latest* evidence via `run_id` —
/// so here ordinal 1 means "the run's evidence on the current store", not "the oldest on disk".
/// Mapping the full `a1..aN` archive set to an ordered multi-`Attempt` list (and routing archived
/// reads through the redacting reader) lands with the aggregate reader (Slice 5 / AU-82B).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attempt {
    pub attempt_id: String,
    /// 1-based position of this attempt within its `StepExecution`. The re-key lands
    /// existing evidence as `1`; SW-AU-82A AC-3b appends `2, 3, …` without overwrite.
    pub attempt_ordinal: u32,
    /// The store `run_id` under which this attempt's evidence was recorded — the
    /// compatibility projection key (AC-4), so existing `{run_id}.*` evidence resolves
    /// through the new identity without a second identity space.
    pub run_id: String,
    pub result: StepResult,
}

/// Mint the runtime `step_execution_id` for a node within a Run. Derived from the Run id
/// and the **definition** `node_id`, so it is stable across reads and label-independent:
/// two nodes with equal labels but distinct ids yield distinct `step_execution_id`s
/// (ARCH-INV-22). Not a UUID — the current store has no minter and this must be
/// re-derivable (ARCH-INV-9 rebuildable projection).
pub fn step_execution_id(run_id: &str, node_id: &str) -> String {
    format!("{run_id}::{node_id}")
}

/// Mint the `attempt_id` for the `n`-th (1-based) attempt of a `StepExecution`.
pub fn attempt_id(step_execution_id: &str, attempt_ordinal: u32) -> String {
    format!("{step_execution_id}#{attempt_ordinal}")
}

/// Project one Run's identity spine from the current store.
///
/// Reuses [`run_store::run_step_verdicts`] — the existing per-run verdict fold — so there
/// is one fold, not a second. Each folded step becomes a `StepExecution` keyed by its
/// definition `node_id` (the folded `step_id`, derived from the node, never a label) with the existing `run_id`-keyed evidence re-attached as
/// Attempt #1. An unknown Run (no matched verdicts) projects an empty `steps` — the
/// caller degrades honestly rather than fabricating a spine.
pub fn project_run_identity(
    records: &[StoreRecord],
    story_id: &str,
    run_id: &str,
) -> AutomationRun {
    let folded = run_store::run_step_verdicts(records, story_id, run_id);
    let steps = folded
        .steps
        .into_iter()
        .map(|view| {
            let node_id = view.step_id;
            let sxid = step_execution_id(run_id, &node_id);
            let attempt = Attempt {
                attempt_id: attempt_id(&sxid, 1),
                attempt_ordinal: 1,
                run_id: run_id.to_string(),
                result: view.result,
            };
            StepExecution {
                step_execution_id: sxid,
                node_id,
                attempts: vec![attempt],
            }
        })
        .collect();
    AutomationRun {
        run_id: run_id.to_string(),
        parent_run_id: None,
        origin_node_id: None,
        correlation_id: run_id.to_string(),
        steps,
    }
}

/// Resolve the single `Attempt` a **runtime** `step_execution_id` addresses, if any.
///
/// A **definition** `node_id` is a different key space and does not resolve here — that
/// is the node-vs-runtime split (AD-6): given a `node_id` you cannot reach runtime
/// evidence; given a `step_execution_id` you reach exactly one Attempt.
// Test-only helper today (the FE resolves attempts by id in TS); a Rust consumer lands with
// SW-AU-82B. Narrow allow rather than the whole module now that Slice 5 wires the rest.
#[allow(dead_code)]
pub fn resolve_attempt<'a>(run: &'a AutomationRun, step_execution_id: &str) -> Option<&'a Attempt> {
    run.steps
        .iter()
        .find(|s| s.step_execution_id == step_execution_id)
        .and_then(|s| s.attempts.iter().find(|a| a.attempt_ordinal == 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::sprint_run::run_store::Via;
    use crate::commands::sprint_run::run_store::{RunEvent, StoreRecord, VerdictEvent};

    fn rec(seq: u64, event: RunEvent) -> StoreRecord {
        StoreRecord {
            schema_version: 1,
            seq,
            ts: u128::from(seq),
            event,
        }
    }

    fn verdict(run_id: &str, node_id: &str, result: StepResult) -> VerdictEvent {
        VerdictEvent {
            story_id: "SW-AU-82A".into(),
            action_identity: "todo->done".into(),
            run_id: run_id.into(),
            step_id: node_id.into(),
            result,
            at: "2026-09-02T00:00:00Z".into(),
            by: Some("agent:dev".into()),
            via: Via::Evaluator,
            evidence: None,
            config_version: "cfgv1".into(),
        }
    }

    /// AC-1: the runtime `step_execution_id` is distinct from the definition `node_id`,
    /// and Attempt #1 re-attaches the existing `run_id`-keyed evidence (AC-3a).
    #[test]
    fn projects_node_and_distinct_step_execution_with_attempt_one() {
        let records = vec![rec(
            0,
            RunEvent::Verdict(verdict("run-1", "dev", StepResult::Passed)),
        )];
        let run = project_run_identity(&records, "SW-AU-82A", "run-1");
        assert_eq!(run.run_id, "run-1");
        assert_eq!(run.correlation_id, "run-1");
        assert!(
            run.parent_run_id.is_none(),
            "no fabricated parent on the current store"
        );
        assert!(run.origin_node_id.is_none());
        assert_eq!(run.steps.len(), 1);
        let step = &run.steps[0];
        assert_eq!(step.node_id, "dev", "definition key");
        assert_ne!(
            step.step_execution_id, step.node_id,
            "runtime key ≠ definition key"
        );
        assert_eq!(step.step_execution_id, "run-1::dev");
        assert_eq!(step.attempts.len(), 1);
        assert_eq!(step.attempts[0].attempt_ordinal, 1);
        assert_eq!(step.attempts[0].attempt_id, "run-1::dev#1");
        assert_eq!(
            step.attempts[0].run_id, "run-1",
            "run_id retained as compat key (AC-4)"
        );
    }

    /// AC-1 outcome: given a `node_id` you cannot reach runtime evidence; given a
    /// `step_execution_id` you resolve exactly one Attempt. The two key spaces do not
    /// collide.
    #[test]
    fn node_id_does_not_resolve_a_runtime_attempt() {
        let records = vec![rec(
            0,
            RunEvent::Verdict(verdict("run-1", "dev", StepResult::Passed)),
        )];
        let run = project_run_identity(&records, "SW-AU-82A", "run-1");
        // The runtime key resolves exactly one Attempt.
        let by_sxid = resolve_attempt(&run, "run-1::dev").expect("runtime key resolves");
        assert_eq!(by_sxid.attempt_ordinal, 1);
        // The bare definition key does not — it is a different join axis (AD-6).
        assert!(
            resolve_attempt(&run, "dev").is_none(),
            "node_id is not a runtime key"
        );
    }

    /// AC-2 (ARCH-INV-22): two steps with distinct node ids resolve to distinct
    /// `step_execution_id`s — the join is by id, never by a display label.
    #[test]
    fn distinct_node_ids_resolve_to_distinct_step_executions() {
        let node_a = "alpha".to_string();
        let node_b = "beta".to_string();

        let records = vec![
            rec(
                0,
                RunEvent::Verdict(verdict("run-1", &node_a, StepResult::Passed)),
            ),
            rec(
                1,
                RunEvent::Verdict(verdict("run-1", &node_b, StepResult::Failed)),
            ),
        ];
        let run = project_run_identity(&records, "SW-AU-82A", "run-1");
        let sx_a = step_execution_id("run-1", &node_a);
        let sx_b = step_execution_id("run-1", &node_b);
        assert_ne!(sx_a, sx_b, "distinct step_execution_ids");
        assert!(resolve_attempt(&run, &sx_a).is_some());
        assert!(resolve_attempt(&run, &sx_b).is_some());
    }

    /// An unknown Run projects an empty spine — no fabricated steps.
    #[test]
    fn unknown_run_projects_empty_spine() {
        let run = project_run_identity(&[], "SW-AU-82A", "ghost");
        assert!(run.steps.is_empty());
        assert_eq!(run.correlation_id, "ghost");
    }

    /// The wire contract SW-AU-82B imports unchanged: the unavailable causation spine
    /// (`parent_run_id`/`origin_node_id`) serializes **omitted, never `null`** — the honest
    /// "unavailable" the story requires — with camelCase keys, and round-trips.
    #[test]
    fn absent_causation_fields_serialize_omitted_not_null() {
        let run = project_run_identity(
            &[rec(
                0,
                RunEvent::Verdict(verdict("run-1", "dev", StepResult::Passed)),
            )],
            "SW-AU-82A",
            "run-1",
        );
        let json = serde_json::to_string(&run).unwrap();
        assert!(!json.contains("parentRunId"), "omitted, not null: {json}");
        assert!(!json.contains("originNodeId"), "omitted, not null: {json}");
        assert!(!json.contains("null"), "no null fields on the wire: {json}");
        assert!(
            json.contains("\"runId\":\"run-1\""),
            "camelCase keys: {json}"
        );
        assert!(json.contains("\"correlationId\":\"run-1\""));
        assert!(json.contains("\"stepExecutionId\":\"run-1::dev\""));
        let back: AutomationRun = serde_json::from_str(&json).unwrap();
        assert_eq!(back, run, "round-trips");
    }
}
