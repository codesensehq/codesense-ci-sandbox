// SPDX-License-Identifier: MIT
//! Canonical Run identity re-homed onto the durable `OrchestrationStore` (SW-AU-82B).
//!
//! AU-82A established the identity types (`AutomationRun` / `StepExecution` / `Attempt`)
//! and the `RunAuditHeader` DTO as a projection over the current JSONL store. This module
//! re-homes their **authority** onto the durable store: identity and the
//! `parent_run_id` / `origin_node_id` / `correlation_id` causal spine are authored as
//! append-only run events, and every read model is a rebuildable in-memory projection of
//! that event stream (ARCH-INV-9). No new SQLite table is added — events are the flat
//! AU-80A `RunEvent { run_id, kind, payload }` envelope, with the typed payloads below
//! JSON-serialized into `payload` (auto-scrubbed at persist by `insert_run_event`).
//!
//! The payload structs are deliberately distinct from both the orchestration envelope
//! `RunEvent` and `run_store::RunEvent` (the JSONL enum) to avoid the two existing name
//! collisions.

use serde::{Deserialize, Serialize};

use crate::coordinator::RunEvent;
use crate::dashboard_stream::SessionStatus;

use super::run_evidence::TriggerFact;
use super::run_identity::{Attempt, AutomationRun, StepExecution};
use super::run_store::StepResult;

/// The append-only run-event vocabulary (AC-1). `kind` is the flat envelope's string
/// label; the typed payload lives in `RunEvent::payload`.
pub mod kind {
    pub const RUN_ACCEPTED: &str = "run.accepted";
    pub const STEP_STARTED: &str = "step.started";
    pub const ATTEMPT_OUTPUT_ATTACHED: &str = "attempt.output_attached";
    pub const STEP_WAITING: &str = "step.waiting";
    pub const STEP_RESUMED: &str = "step.resumed";
    pub const GATE_FAILED: &str = "gate.failed";
    pub const EFFECT_APPLIED: &str = "effect.applied";
    pub const RUN_COMPLETED: &str = "run.completed";
    /// SW-AU-81A: a fan-out child was linked to this parent Run — the parent stores the link
    /// (`child_run_id` + causal ids + started/non-started state), never the child's steps (AC-8).
    pub const CHILD_LINKED: &str = "child.linked";
}

/// The immutable audit-header **provenance** captured once, verbatim, into `run.accepted`
/// (AC-6). This is the subset of `RunAuditHeader` that is fixed at acceptance — the
/// terminal `status` / `ended_at` / `summary` are **not** here: they do not exist yet and
/// are re-derived by the projection from later events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImmutableAuditProvenance {
    pub story_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automation_id: Option<String>,
    pub trigger: TriggerFact,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fired_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// The pinned immutable `WorkflowRevision` id (AU-79A) the Run was accepted for, so
    /// its identity joins against a frozen definition (ARCH-INV-4). Optional until the
    /// producer threads it (SW-AU-74 / AU-79A wiring). For a fan-out child (SW-AU-81A) this is the
    /// pinned `child_workflow_revision`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_revision_id: Option<String>,
    /// The execution subject's stable id — the member `SubjectRef.stable_id` a fan-out child
    /// (SW-AU-81A) runs against. `None` for a root/parent run. Additive to the immutable provenance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_ref: Option<String>,
    /// The pinned child Automation definition revision a fan-out child was admitted for
    /// (SW-AU-81A `ChildKey.child_automation_revision`). `None` for a non-child run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_automation_revision: Option<String>,
    /// The fan-out invocation generation shared by every sibling of one `run_automation` node
    /// (SW-AU-81A) — siblings cannot drift revisions. `None` for a non-child run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_generation: Option<u32>,
}

/// `run.accepted` payload — the causal spine + the immutable provenance (AC-2, AC-6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAcceptedPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_node_id: Option<String>,
    pub correlation_id: String,
    pub provenance: ImmutableAuditProvenance,
}

/// Whether a fan-out member launched a child Run or is an explicit non-started child (AC-8/AC-9/AC-14).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ChildLinkState {
    /// The child Run was admitted and launched.
    Started,
    /// The member did not launch — a truthful non-started child carrying its legible reason.
    NonStarted { reason: String },
}

/// `child.linked` payload — a fan-out child (or non-started member) linked to the parent Run (AC-8).
/// The parent stores this **link**, never the child's steps; the child owns its own evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildLinkedPayload {
    /// The `run_automation` node that spawned the child (AD-6).
    pub run_node_id: String,
    /// The child Run's id — the deep-link target. Empty for a non-started member.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub child_run_id: String,
    /// The member `SubjectRef.stable_id` the child runs against.
    pub subject_id: String,
    pub child_automation_revision: String,
    pub state: ChildLinkState,
}

/// `step.started` payload — the definition `node_id` + pinned `revision_id` AND the
/// runtime `step_execution_id` (AC-3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepStartedPayload {
    pub step_execution_id: String,
    pub node_id: String,
    pub revision_id: String,
}

/// `attempt.output_attached` payload — one attempt's identity + result + its evidence ref
/// (AC-3, inherited deferral: per-Attempt evidence).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttemptOutputPayload {
    pub step_execution_id: String,
    pub attempt_id: String,
    pub attempt_ordinal: u32,
    /// The store `run_id` under which this attempt's evidence was recorded — the
    /// compatibility read key, so `{run_id}.*` evidence resolves through the identity.
    pub run_id: String,
    pub result: StepResult,
}

/// `run.completed` / `gate.failed` payload — the terminal facts the projection derives
/// into the header (AC-6). Never captured at `run.accepted`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunTerminalPayload {
    pub status: SessionStatus,
    pub ended_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// The header the projection reassembles: the immutable provenance (from `run.accepted`)
/// plus terminal fields (`status` / `ended_at` / `summary`) that are **absent until the
/// terminating event is appended** (AC-6). Deliberately optional-terminal so a live
/// (non-terminal) Run renders honestly rather than fabricating an outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectedHeader {
    pub run_id: String,
    pub story_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automation_id: Option<String>,
    pub trigger: TriggerFact,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fired_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_revision_id: Option<String>,
    /// Fan-out child provenance (SW-AU-81A), projected from `run.accepted`. `None` for a root run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_automation_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_generation: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<SessionStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// One Run rebuilt from its durable event stream — the sole read model, in-memory and
/// rebuildable, never authoritative (AC-4, ARCH-INV-9). Equal events → equal projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunProjection {
    pub header: ProjectedHeader,
    pub run: AutomationRun,
    /// SW-AU-81A: the fan-out children (and non-started members) this parent Run linked — the parent's
    /// aggregate/deep-links (AC-8). Never the children's steps; each child owns its own evidence. Empty
    /// for a run that spawned no children.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_links: Vec<ChildLinkedPayload>,
}

/// The cross-run causal spine captured from `run.accepted` (AC-2). Held as a named value
/// during replay so the projector never confuses the three positional ids.
struct CausalSpine {
    parent_run_id: Option<String>,
    origin_node_id: Option<String>,
    correlation_id: String,
}

/// Deterministic replay of a durable run-event stream into the in-memory projection.
pub struct RunIdentityProjector;

impl RunIdentityProjector {
    /// Rebuild one Run's projection from its ordered event stream (AC-4). Pure over the
    /// slice — no store, no clock — so a rebuild is a value function of the log. Events
    /// with an unrecognized `kind` or an unparseable payload are skipped honestly rather
    /// than corrupting the projection. Returns `None` when the stream has no
    /// `run.accepted` (an unknown / not-yet-accepted Run — the caller degrades to
    /// unavailable, never a fabricated Run).
    pub fn rebuild(events: &[RunEvent]) -> Option<RunProjection> {
        let mut header: Option<ProjectedHeader> = None;
        let mut steps: Vec<StepExecution> = Vec::new();
        let mut spine: Option<CausalSpine> = None;
        let mut child_links: Vec<ChildLinkedPayload> = Vec::new();

        for ev in events {
            match ev.kind.as_str() {
                kind::RUN_ACCEPTED => {
                    // First-seen wins: a live Run's provenance is captured exactly once
                    // and never re-captured on a later event (AC-6).
                    if header.is_some() {
                        continue;
                    }
                    let Ok(p) = serde_json::from_str::<RunAcceptedPayload>(&ev.payload) else {
                        continue;
                    };
                    spine = Some(CausalSpine {
                        parent_run_id: p.parent_run_id.clone(),
                        origin_node_id: p.origin_node_id.clone(),
                        correlation_id: p.correlation_id.clone(),
                    });
                    header = Some(ProjectedHeader {
                        run_id: ev.run_id.clone(),
                        story_id: p.provenance.story_id,
                        automation_id: p.provenance.automation_id,
                        trigger: p.provenance.trigger,
                        scheduled_at: p.provenance.scheduled_at,
                        fired_at: p.provenance.fired_at,
                        started_at: p.provenance.started_at,
                        workflow_revision_id: p.provenance.workflow_revision_id,
                        subject_ref: p.provenance.subject_ref,
                        child_automation_revision: p.provenance.child_automation_revision,
                        invocation_generation: p.provenance.invocation_generation,
                        status: None,
                        ended_at: None,
                        summary: None,
                    });
                }
                kind::STEP_STARTED => {
                    let Ok(p) = serde_json::from_str::<StepStartedPayload>(&ev.payload) else {
                        continue;
                    };
                    if !steps
                        .iter()
                        .any(|s| s.step_execution_id == p.step_execution_id)
                    {
                        steps.push(StepExecution {
                            step_execution_id: p.step_execution_id,
                            node_id: p.node_id,
                            attempts: Vec::new(),
                        });
                    }
                }
                kind::ATTEMPT_OUTPUT_ATTACHED => {
                    let Ok(p) = serde_json::from_str::<AttemptOutputPayload>(&ev.payload) else {
                        continue;
                    };
                    let attempt = Attempt {
                        attempt_id: p.attempt_id,
                        attempt_ordinal: p.attempt_ordinal,
                        run_id: p.run_id,
                        result: p.result,
                    };
                    if let Some(step) = steps
                        .iter_mut()
                        .find(|s| s.step_execution_id == p.step_execution_id)
                    {
                        // Dedup by attempt_id so a replayed / at-least-once event never
                        // doubles an Attempt, mirroring the step.started dedup above.
                        if !step
                            .attempts
                            .iter()
                            .any(|a| a.attempt_id == attempt.attempt_id)
                        {
                            step.attempts.push(attempt);
                        }
                    }
                }
                kind::RUN_COMPLETED | kind::GATE_FAILED => {
                    let Ok(p) = serde_json::from_str::<RunTerminalPayload>(&ev.payload) else {
                        continue;
                    };
                    if let Some(h) = header.as_mut() {
                        // First terminal wins — a Run terminates once; a later terminal event
                        // (or a replay) never overwrites the recorded outcome, mirroring the
                        // first-seen-wins provenance capture on `run.accepted` (AC-6).
                        if h.ended_at.is_none() {
                            h.status = Some(p.status);
                            h.ended_at = Some(p.ended_at);
                            h.summary = p.summary;
                        }
                    }
                }
                // These kinds carry no identity the Run-detail projection renders: the
                // wait/resume runtime is AU-92 and the effect ledger is AU-80A/AU-93. They
                // stay in the log for those consumers; naming them here keeps the handling
                // exhaustive rather than swallowing them in a blanket arm.
                kind::CHILD_LINKED => {
                    // SW-AU-81A: append the parent's child link (AC-8). Unparseable payloads are
                    // skipped honestly rather than corrupting the projection.
                    if let Ok(link) = serde_json::from_str::<ChildLinkedPayload>(&ev.payload) {
                        child_links.push(link);
                    }
                }
                kind::STEP_WAITING | kind::STEP_RESUMED | kind::EFFECT_APPLIED => {}
                _ => {}
            }
        }

        let header = header?;
        let spine = spine.unwrap_or(CausalSpine {
            parent_run_id: None,
            origin_node_id: None,
            correlation_id: header.run_id.clone(),
        });
        let run = AutomationRun {
            run_id: header.run_id.clone(),
            parent_run_id: spine.parent_run_id,
            origin_node_id: spine.origin_node_id,
            correlation_id: spine.correlation_id,
            steps,
        };
        // SW-AU-81A: dedupe the parent's child-links by `(run_node_id, subject_id)`, keeping the last
        // event for each member. A started child is already deduped at admission (a replayed drive
        // short-circuits at `AlreadyAdmitted` before re-linking), but a non-started member has no
        // admission gate, so a re-driven parent would otherwise re-append its link every drive. Keeping
        // last makes the aggregate idempotent under scheduler replay and lets a later state supersede an
        // earlier one for the same member.
        let child_links = dedupe_child_links_last(child_links);
        Some(RunProjection {
            header,
            run,
            child_links,
        })
    }
}

/// Keep only the last `child.linked` per `(run_node_id, subject_id)`, preserving first-append order
/// among the survivors so the projection is deterministic and idempotent under replay (SW-AU-81A).
fn dedupe_child_links_last(links: Vec<ChildLinkedPayload>) -> Vec<ChildLinkedPayload> {
    use std::collections::BTreeMap;
    let mut last: BTreeMap<(String, String), usize> = BTreeMap::new();
    for (i, l) in links.iter().enumerate() {
        last.insert((l.run_node_id.clone(), l.subject_id.clone()), i);
    }
    let mut keep: Vec<usize> = last.into_values().collect();
    keep.sort_unstable();
    keep.into_iter().map(|i| links[i].clone()).collect()
}

/// The durable stream partition key (SW-AU-82C run-lifecycle producers). The evidence identity
/// is the `(story_id, run_id)` PAIR (the FE reads `readRunEvidence(story, run)`; the JSONL folds
/// scope by both) — but a sprint/epic dispatch tick shares ONE `run_id` across every story it
/// fans out to. Keying the durable event stream by the bare `run_id` would collide those stories
/// in one stream (identical `step_execution_id`s across stories, one header), so the stream is
/// partitioned by the pair. The bare `run_id` stays the evidence/join key (it is what the
/// projection presents and what `AttemptOutputPayload.run_id` carries); this composite is a
/// storage partition only, never surfaced.
///
/// `story_id` leads because it is separator-free (`SW-XX-N`), so the `::` delimiter is
/// unambiguous even when `run_id` itself contains `::` (reactive run ids are colon-laden,
/// e.g. `event:{delivery_key}:{automation_id}`, and a delivery key is arbitrary) — the key is
/// injective over `(story_id, run_id)`. Run-id-first would not be.
pub fn durable_run_key(run_id: &str, story_id: &str) -> String {
    format!("{story_id}::{run_id}")
}

// The legacy compatibility projection (AC-5). Contract-tested here; its runtime consumer
// is the Run-*list* read model (which enumerates historical runs mapped to identities),
// outside this story's single-Run detail vertical slice — so it carries the same
// `#[allow(dead_code)]`-until-consumer marker as the durable write API above.
/// Where a Run's identity is served from — the durable event log, or a legacy
/// compatibility projection over the JSONL store (AC-5). `Historical` rows carry no
/// executable delivery: no history is replayed and no mutation is re-applied.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RunProvenance {
    Durable,
    Historical,
}

/// One legacy Run's compatibility mapping onto the identity spine (AC-5). The legacy
/// `run_id` is retained verbatim as the new identity's `run_id` — it is a compatibility
/// **key**, not a re-key — so every historical Run stays readable and deep-linkable
/// without minting a second identity space. Joins carry `story_id` / `action_identity`.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyRunMapping {
    pub run_id: String,
    pub story_id: String,
    pub action_identity: String,
    pub provenance: RunProvenance,
}

/// A read-only projection mapping legacy `run_id`s (from every minter — `gate.rs`,
/// `connector_trigger`) onto the canonical
/// identities (AC-5). Additive and lossless: it replays nothing, rewrites no JSONL, and
/// drops no legacy id. Same-Workflow worker fan-out stays a compatibility projection
/// until AU-81A establishes native child-Run identity.
#[allow(dead_code)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LegacyRunKeyMap {
    by_run_id: std::collections::BTreeMap<String, LegacyRunMapping>,
}

#[allow(dead_code)]
impl LegacyRunKeyMap {
    /// Build the compatibility projection from the legacy verdict identities the JSONL
    /// store already holds. First-seen wins per `run_id` (a Run's identity is fixed at its
    /// first recorded verdict); a repeated `run_id` is the same Run, never a duplicate.
    pub fn from_verdict_identities<'a>(
        identities: impl IntoIterator<Item = &'a super::run_store::VerdictIdentity>,
    ) -> Self {
        let mut by_run_id = std::collections::BTreeMap::new();
        for id in identities {
            by_run_id
                .entry(id.run_id.clone())
                .or_insert_with(|| LegacyRunMapping {
                    run_id: id.run_id.clone(),
                    story_id: id.story_id.clone(),
                    action_identity: id.action_identity.clone(),
                    provenance: RunProvenance::Historical,
                });
        }
        Self { by_run_id }
    }

    /// Resolve a legacy `run_id` to its canonical mapping, if this Run predates the
    /// durable authority.
    pub fn resolve(&self, run_id: &str) -> Option<&LegacyRunMapping> {
        self.by_run_id.get(run_id)
    }

    pub fn len(&self) -> usize {
        self.by_run_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_run_id.is_empty()
    }
}

/// The durable write API. No production path authors run-identity streams any more; existing
/// streams still decode through `RunIdentityProjector`, and tests author them through these.
#[cfg(test)]
pub(crate) mod test_support {
    use serde::Serialize;

    use super::{
        kind, AttemptOutputPayload, RunAcceptedPayload, RunEvent, RunTerminalPayload,
        StepStartedPayload,
    };
    use crate::coordinator::{OrchestrationStore, StoreResult};

    /// Author a `run.accepted` event through the durable append boundary (AC-1). Nothing produces
    /// these since the sprint dispatch retired; old events still decode, and tests author them
    /// through this.
    pub(crate) fn accept_run(
        store: &impl OrchestrationStore,
        run_id: &str,
        payload: &RunAcceptedPayload,
    ) -> StoreResult<i64> {
        append(store, run_id, kind::RUN_ACCEPTED, payload)
    }

    /// Author a `step.started` event (AC-3). Nothing produces these since the status gate
    /// retired; old events still decode, and tests author them through this.
    pub(crate) fn start_step(
        store: &impl OrchestrationStore,
        run_id: &str,
        payload: &StepStartedPayload,
    ) -> StoreResult<i64> {
        append(store, run_id, kind::STEP_STARTED, payload)
    }

    /// Author an `attempt.output_attached` event (AC-3). Like [`start_step`], test-only.
    pub(crate) fn attach_attempt(
        store: &impl OrchestrationStore,
        run_id: &str,
        payload: &AttemptOutputPayload,
    ) -> StoreResult<i64> {
        append(store, run_id, kind::ATTEMPT_OUTPUT_ATTACHED, payload)
    }

    /// Author a terminal `run.completed` event (AC-6). Like [`start_step`], test-only.
    pub(crate) fn complete_run(
        store: &impl OrchestrationStore,
        run_id: &str,
        payload: &RunTerminalPayload,
    ) -> StoreResult<i64> {
        append(store, run_id, kind::RUN_COMPLETED, payload)
    }

    /// Serialize a typed payload into the flat envelope and append it. One authoring path so
    /// every identity event goes through the same (auto-scrubbing) `append` boundary.
    fn append<T: Serialize>(
        store: &impl OrchestrationStore,
        run_id: &str,
        kind: &str,
        payload: &T,
    ) -> StoreResult<i64> {
        let payload =
            serde_json::to_string(payload).expect("payload structs are always serializable");
        store.append(RunEvent {
            run_id: run_id.to_string(),
            kind: kind.to_string(),
            payload,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{accept_run, attach_attempt, complete_run, start_step};
    use super::*;
    use crate::commands::sprint_run::run_identity::{attempt_id, step_execution_id};
    use crate::commands::sprint_run::run_store::VerdictIdentity;
    use crate::coordinator::SqliteOrchestrationStore;

    fn provenance(story_id: &str) -> ImmutableAuditProvenance {
        ImmutableAuditProvenance {
            story_id: story_id.into(),
            automation_id: Some("auto-1".into()),
            trigger: TriggerFact::Attributed {
                source: "schedule".into(),
            },
            scheduled_at: Some("2026-09-02T00:00:00Z".into()),
            fired_at: Some("2026-09-02T00:00:01Z".into()),
            started_at: Some("2026-09-02T00:00:02Z".into()),
            workflow_revision_id: Some("rev-1".into()),
            subject_ref: None,
            child_automation_revision: None,
            invocation_generation: None,
        }
    }

    fn accepted(correlation_id: &str, parent: Option<&str>) -> RunAcceptedPayload {
        RunAcceptedPayload {
            parent_run_id: parent.map(str::to_string),
            origin_node_id: parent.map(|_| "spawn-node".to_string()),
            correlation_id: correlation_id.into(),
            provenance: provenance("SW-AU-82B"),
        }
    }

    fn ev(run_id: &str, kind: &str, payload: &impl Serialize) -> RunEvent {
        RunEvent {
            run_id: run_id.into(),
            kind: kind.into(),
            payload: serde_json::to_string(payload).unwrap(),
        }
    }

    // A shared on-disk store: the coordinator opens a fresh connection per operation, so
    // `:memory:` (a distinct DB per connection) would not share the schema. The returned
    // `TempDir` must be held for the test's lifetime.
    fn store() -> (tempfile::TempDir, SqliteOrchestrationStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store =
            SqliteOrchestrationStore::open(dir.path().join("dri.sqlite3")).expect("open store");
        (dir, store)
    }

    /// AC-1: identity is authored through the flat envelope and read back in order; a
    /// projection advances only by appending, never by rewriting a prior event.
    #[test]
    fn authors_flat_envelope_and_rebuilds_in_order() {
        let (_dir, store) = store();
        accept_run(&store, "run-1", &accepted("corr-1", None)).unwrap();
        start_step(
            &store,
            "run-1",
            &StepStartedPayload {
                step_execution_id: step_execution_id("run-1", "dev"),
                node_id: "dev".into(),
                revision_id: "rev-1".into(),
            },
        )
        .unwrap();
        let sxid = step_execution_id("run-1", "dev");
        attach_attempt(
            &store,
            "run-1",
            &AttemptOutputPayload {
                step_execution_id: sxid.clone(),
                attempt_id: attempt_id(&sxid, 1),
                attempt_ordinal: 1,
                run_id: "run-1".into(),
                result: StepResult::Passed,
            },
        )
        .unwrap();

        let events = store.run_events_for("run-1").unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].kind, kind::RUN_ACCEPTED);
        assert_eq!(events[2].kind, kind::ATTEMPT_OUTPUT_ATTACHED);

        let proj = RunIdentityProjector::rebuild(&events).expect("run.accepted present");
        assert_eq!(proj.run.run_id, "run-1");
        assert_eq!(proj.run.steps.len(), 1);
        assert_eq!(proj.run.steps[0].node_id, "dev");
        assert_eq!(proj.run.steps[0].step_execution_id, sxid);
        assert_eq!(proj.run.steps[0].attempts.len(), 1);
        assert_eq!(proj.run.steps[0].attempts[0].attempt_ordinal, 1);
    }

    /// AC-2: parent + child streams project so the child joins to its parent by
    /// `parent_run_id` and both share one `correlation_id` — by stable id, never label.
    #[test]
    fn causal_spine_joins_child_to_parent_by_id() {
        let (_dir, store) = store();
        accept_run(&store, "parent-1", &accepted("fam-1", None)).unwrap();
        accept_run(&store, "child-1", &accepted("fam-1", Some("parent-1"))).unwrap();

        let parent = RunIdentityProjector::rebuild(&store.run_events_for("parent-1").unwrap())
            .expect("parent");
        let child = RunIdentityProjector::rebuild(&store.run_events_for("child-1").unwrap())
            .expect("child");

        assert_eq!(child.run.parent_run_id.as_deref(), Some("parent-1"));
        assert_eq!(child.run.origin_node_id.as_deref(), Some("spawn-node"));
        assert!(parent.run.parent_run_id.is_none());
        assert_eq!(child.run.correlation_id, parent.run.correlation_id);
        assert_eq!(child.run.correlation_id, "fam-1");
    }

    /// AC-3 / ARCH-INV-22: two nodes sharing a display label resolve to distinct
    /// `step_execution_id`s by id — no label join.
    #[test]
    fn identical_labels_resolve_to_distinct_step_executions() {
        let (_dir, store) = store();
        accept_run(&store, "run-1", &accepted("corr-1", None)).unwrap();
        for node in ["alpha", "beta"] {
            start_step(
                &store,
                "run-1",
                &StepStartedPayload {
                    step_execution_id: step_execution_id("run-1", node),
                    node_id: node.into(),
                    revision_id: "rev-1".into(),
                },
            )
            .unwrap();
        }
        let proj = RunIdentityProjector::rebuild(&store.run_events_for("run-1").unwrap()).unwrap();
        assert_eq!(proj.run.steps.len(), 2);
        assert_ne!(
            proj.run.steps[0].step_execution_id,
            proj.run.steps[1].step_execution_id
        );
    }

    /// AC-4: rebuilding twice from the same events yields `PartialEq`-equal projections,
    /// and a projection built after N more events reflects only those events.
    #[test]
    fn rebuild_is_deterministic_and_incremental() {
        let (_dir, store) = store();
        accept_run(&store, "run-1", &accepted("corr-1", None)).unwrap();
        start_step(
            &store,
            "run-1",
            &StepStartedPayload {
                step_execution_id: step_execution_id("run-1", "dev"),
                node_id: "dev".into(),
                revision_id: "rev-1".into(),
            },
        )
        .unwrap();

        let a = RunIdentityProjector::rebuild(&store.run_events_for("run-1").unwrap()).unwrap();
        let b = RunIdentityProjector::rebuild(&store.run_events_for("run-1").unwrap()).unwrap();
        assert_eq!(a, b, "same events → equal projection");

        start_step(
            &store,
            "run-1",
            &StepStartedPayload {
                step_execution_id: step_execution_id("run-1", "review"),
                node_id: "review".into(),
                revision_id: "rev-1".into(),
            },
        )
        .unwrap();
        let c = RunIdentityProjector::rebuild(&store.run_events_for("run-1").unwrap()).unwrap();
        assert_eq!(c.run.steps.len(), 2, "reflects the appended event");
        assert_ne!(a, c);
    }

    /// AC-6: the immutable provenance round-trips unchanged; `status`/`ended_at` are
    /// absent until the terminating event is appended (projected, not stored), and a
    /// second `run.accepted` never re-captures the header.
    #[test]
    fn terminal_fields_absent_until_completion_and_provenance_not_recaptured() {
        let (_dir, store) = store();
        accept_run(&store, "run-1", &accepted("corr-1", None)).unwrap();

        let live = RunIdentityProjector::rebuild(&store.run_events_for("run-1").unwrap()).unwrap();
        assert!(
            live.header.status.is_none(),
            "no fabricated terminal status"
        );
        assert!(live.header.ended_at.is_none());
        assert_eq!(live.header.story_id, "SW-AU-82B");
        assert_eq!(live.header.workflow_revision_id.as_deref(), Some("rev-1"));

        // A stray second run.accepted (e.g. a replayed producer) must not re-capture.
        let mut recaptured = accepted("corr-1", None);
        recaptured.provenance.story_id = "TAMPERED".into();
        let events = {
            let mut e = store.run_events_for("run-1").unwrap();
            e.push(ev("run-1", kind::RUN_ACCEPTED, &recaptured));
            e.push(ev(
                "run-1",
                kind::RUN_COMPLETED,
                &RunTerminalPayload {
                    status: SessionStatus::Success,
                    ended_at: "2026-09-02T00:05:00Z".into(),
                    summary: Some("done".into()),
                },
            ));
            e
        };
        let done = RunIdentityProjector::rebuild(&events).unwrap();
        assert_eq!(
            done.header.story_id, "SW-AU-82B",
            "provenance not re-captured"
        );
        assert_eq!(done.header.status, Some(SessionStatus::Success));
        assert_eq!(
            done.header.ended_at.as_deref(),
            Some("2026-09-02T00:05:00Z")
        );
        assert_eq!(done.header.summary.as_deref(), Some("done"));
    }

    /// An unknown / not-yet-accepted Run projects `None` — never a fabricated Run.
    #[test]
    fn stream_without_acceptance_projects_none() {
        let (_dir, store) = store();
        assert!(RunIdentityProjector::rebuild(&store.run_events_for("ghost").unwrap()).is_none());
    }

    fn legacy(run_id: &str) -> VerdictIdentity {
        VerdictIdentity {
            story_id: "SW-AU-82B".into(),
            action_identity: "todo->done".into(),
            run_id: run_id.into(),
        }
    }

    /// AC-5: legacy `run_id`s from every minter map onto the identity spine without loss,
    /// carry `historical` provenance, and a repeated id is not duplicated.
    #[test]
    fn legacy_run_ids_map_without_loss_as_historical() {
        // One representative id per minter shape (gate / connector / watch / reactive).
        let ids = [
            legacy("run-1700000000000-7"),         // gate.rs::mint_run_id
            legacy("connector:auto-42"),           // connector_trigger::run_id_for
            legacy("org/team/todo->done/auto-42"), // a retired slash-delimited cadence id
            legacy("event:deliv-9:auto-42"),       // reactive_trigger::run_id_for
            legacy("run-1700000000000-7"),         // duplicate — same Run, not a second entry
        ];
        let map = LegacyRunKeyMap::from_verdict_identities(ids.iter());
        assert_eq!(
            map.len(),
            4,
            "duplicate run_id collapses; no legacy id dropped"
        );
        for run_id in [
            "run-1700000000000-7",
            "connector:auto-42",
            "org/team/todo->done/auto-42",
            "event:deliv-9:auto-42",
        ] {
            let m = map.resolve(run_id).expect("legacy id resolves");
            assert_eq!(
                m.run_id, run_id,
                "run_id retained verbatim (compat key, no re-key)"
            );
            assert_eq!(m.provenance, RunProvenance::Historical);
            assert_eq!(m.action_identity, "todo->done");
        }
        assert!(map.resolve("never-seen").is_none());
    }

    /// AC-6 consistency: a Run terminates once — the first terminal event wins and a later
    /// terminal (or replay) never overwrites `status`/`ended_at`/`summary`.
    #[test]
    fn first_terminal_event_wins() {
        let (_dir, store) = store();
        accept_run(&store, "run-1", &accepted("corr-1", None)).unwrap();
        complete_run(
            &store,
            "run-1",
            &RunTerminalPayload {
                status: SessionStatus::Failed,
                ended_at: "t-first".into(),
                summary: Some("first".into()),
            },
        )
        .unwrap();
        // A second (later) terminal event — must not overwrite the first outcome.
        let events = {
            let mut e = store.run_events_for("run-1").unwrap();
            e.push(ev(
                "run-1",
                kind::RUN_COMPLETED,
                &RunTerminalPayload {
                    status: SessionStatus::Success,
                    ended_at: "t-second".into(),
                    summary: Some("second".into()),
                },
            ));
            e
        };
        let proj = RunIdentityProjector::rebuild(&events).unwrap();
        assert_eq!(proj.header.status, Some(SessionStatus::Failed));
        assert_eq!(proj.header.ended_at.as_deref(), Some("t-first"));
        assert_eq!(proj.header.summary.as_deref(), Some("first"));
    }

    /// AC-1: a replayed `attempt.output_attached` (same `attempt_id`) is deduped, not doubled.
    #[test]
    fn duplicate_attempt_event_is_deduped_by_id() {
        let (_dir, store) = store();
        accept_run(&store, "run-1", &accepted("corr-1", None)).unwrap();
        let sxid = step_execution_id("run-1", "dev");
        start_step(
            &store,
            "run-1",
            &StepStartedPayload {
                step_execution_id: sxid.clone(),
                node_id: "dev".into(),
                revision_id: "rev-1".into(),
            },
        )
        .unwrap();
        let attempt = AttemptOutputPayload {
            step_execution_id: sxid.clone(),
            attempt_id: attempt_id(&sxid, 1),
            attempt_ordinal: 1,
            run_id: "run-1".into(),
            result: StepResult::Passed,
        };
        attach_attempt(&store, "run-1", &attempt).unwrap();
        attach_attempt(&store, "run-1", &attempt).unwrap(); // replay

        let proj = RunIdentityProjector::rebuild(&store.run_events_for("run-1").unwrap()).unwrap();
        assert_eq!(proj.run.steps.len(), 1);
        assert_eq!(
            proj.run.steps[0].attempts.len(),
            1,
            "replayed attempt is deduped by attempt_id"
        );
    }

    /// AC-1: state advances only by appending — a prior event is never rewritten in place.
    /// A later append leaves every earlier row byte-for-byte unchanged.
    #[test]
    fn prior_events_are_never_rewritten() {
        let (_dir, store) = store();
        accept_run(&store, "run-1", &accepted("corr-1", None)).unwrap();
        let before = store.run_events_for("run-1").unwrap();
        assert_eq!(before.len(), 1);

        complete_run(
            &store,
            "run-1",
            &RunTerminalPayload {
                status: SessionStatus::Success,
                ended_at: "t1".into(),
                summary: None,
            },
        )
        .unwrap();
        let after = store.run_events_for("run-1").unwrap();
        assert_eq!(after.len(), 2, "state advanced by appending a new event");
        assert_eq!(
            after[0], before[0],
            "the prior run.accepted event is unchanged — append-only, never rewritten"
        );
    }
}
