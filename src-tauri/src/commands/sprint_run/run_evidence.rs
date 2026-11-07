// SPDX-License-Identifier: MIT
//! The evidence-aggregate read the `RunEvidence` view opens (SW-AU-82A Slice 5).
//!
//! Folds the current JSONL store **once** (inside one `await_blocking`, AC-10 UI-thread guarantee)
//! into `{ immutable audit header, causal identity spine }` and
//! scrubbed at render over the ONE `redact_secrets` routine (AC-6). The header is **partial** on the
//! current store: the config/outcome fields are captured from the durable [`RunOutcomeEvent`], but
//! the envelope-derived fields (causing event, adapter/connection, causation, correlation) are not
//! persisted per run until SW-AU-74's producer cutover — so a run with no captured event renders an
//! honest [`TriggerFact::PreEnvelope`] step, never a fabricated one (AC-11 / AD-15).
//!
//! Evidence **bodies** (transcript / artifact / raw terminal per attempt) are NOT re-read here: the FE
//! fetches them through the existing per-run readers keyed by `Attempt.run_id` (the compat key), so
//! there is one evidence-read path, not a second (DRY).

use serde::{Deserialize, Serialize};

use super::durable_run_identity;
use super::run_identity::{project_run_identity, AutomationRun};
use super::run_store::{
    self, ArtifactKind, FootprintChange, FootprintEntry, RunOutcomeEvent, StepArtifact, StepOutput,
    StepResultEvent, StoreRecord, DEFAULT_STEP_NODE_ID,
};
use crate::commands::async_helpers::{with_blocking, BlockingCtx};
use crate::coordinator::{default_store_path, SqliteOrchestrationStore};
use crate::dashboard_stream::SessionStatus;
use crate::shared::secret_scan::redact_secrets;

/// Honest trigger provenance for the run's inspectable trigger step (AC-11 / AD-15). On the current
/// store the causing `AutomationEvent` is not persisted per run (AU-74 cutover), so a run either
/// carries a coarse trigger `source` label (`Attributed`) or renders an explicit `PreEnvelope` step —
/// never a fabricated event (forward-only attribution, AD-12 precedent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TriggerFact {
    Attributed { source: String },
    PreEnvelope,
}

impl TriggerFact {
    /// The one mapping from an optional, possibly-blank trigger `source` label to a
    /// `TriggerFact`: a non-blank (trimmed) source is `Attributed`, anything else is the
    /// honest `PreEnvelope` (AC-11 / AD-15 — never a fabricated attribution). The current-store header
    /// ([`RunAuditHeader::from_outcome`]) uses this so the two never disagree on what counts
    /// as attributed. Does NOT scrub — the persist path auto-scrubs and the render path
    /// re-scrubs (`from_outcome` / `durable_evidence`); scrubbing here would double-encode.
    pub(crate) fn from_source(source: Option<&str>) -> Self {
        match source.map(str::trim) {
            Some(source) if !source.is_empty() => TriggerFact::Attributed {
                source: source.to_string(),
            },
            _ => TriggerFact::PreEnvelope,
        }
    }
}

/// The immutable, scrubbed audit header (AC-5 — **partial** on the current store). Captured from the
/// durable [`RunOutcomeEvent`]; the envelope-derived fields land when AU-74 persists the causing event
/// per run. Rebuildable from the source records (AC-7 / ARCH-INV-9) — never a second source of truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAuditHeader {
    pub run_id: String,
    pub story_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automation_id: Option<String>,
    /// The inspectable trigger step (AC-11). `PreEnvelope` when no trigger source was captured.
    pub trigger: TriggerFact,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fired_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// Terminal facts — `None` on a live (non-terminal) Run. On the current store a header
    /// is only built from a terminal outcome so both are always `Some`; the durable
    /// authority (SW-AU-82B) projects them from `run.completed`/`gate.failed`, so a live
    /// durable Run renders honestly rather than fabricating an outcome (AC-6).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<SessionStatus>,
    /// Scrubbed at render (AC-6) — the summary is the one free-text field on the header.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// The pinned immutable `WorkflowRevision` id the Run was accepted for (AU-79A / AU-80E). Folded
    /// from the durable `run.accepted` provenance; `None` on the current-store fallback authority
    /// (`RunOutcomeEvent` pins no revision). Lets a downstream reader join the Run to its frozen
    /// definition without re-opening the durable projection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_revision_id: Option<String>,
}

impl RunAuditHeader {
    fn from_outcome(o: &RunOutcomeEvent) -> Self {
        // One source→fact mapping (shared with the durable producer), then scrub the label at
        // render too (AC-6): a trigger source could carry a URL/token.
        let trigger = match TriggerFact::from_source(o.trigger_source.as_deref()) {
            TriggerFact::Attributed { source } => TriggerFact::Attributed {
                source: redact_secrets(&source),
            },
            TriggerFact::PreEnvelope => TriggerFact::PreEnvelope,
        };
        RunAuditHeader {
            run_id: o.run_id.clone(),
            story_id: o.story_id.clone(),
            automation_id: o.automation_id.clone(),
            trigger,
            scheduled_at: o.scheduled_at.clone(),
            fired_at: o.fired_at.clone(),
            started_at: o.started_at.clone(),
            ended_at: Some(o.ended_at.clone()),
            status: Some(o.status),
            summary: o.summary.as_deref().map(redact_secrets),
            // The current-store outcome record pins no revision — only the durable `run.accepted`
            // provenance carries it (propagated in `durable_evidence`).
            workflow_revision_id: None,
        }
    }
}

/// A per-node reported result (AD-15 layer 1). SW-AU-96 fills the four declared channels — a short
/// `summary`, typed `outputs`, produced `artifacts`, and the touched `footprint` — folded from the
/// `StepResult` ledger events (scrubbed at write, keyed by `node_id`/`run_id`, rebuildable) plus the
/// harness-captured run work product. SW-AU-97 renders them. The channel types are the ONE definition
/// owned by `run_store` (no parallel result DTO — AC-4).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PerNodeResult {
    pub node_id: String,
    /// The latest reported short summary for the node ("what I did / produced / touched").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Typed key/value outputs, latest-wins per key.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<StepOutput>,
    /// Named artifacts the node produced, union-deduped by `(kind, reference)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<StepArtifact>,
    /// Entities the node created or modified, union-deduped by `(kind, reference)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub footprint: Vec<FootprintEntry>,
}

/// Which identity authority served this evidence (SW-AU-82B, AC-12/AC-14). The FE renders
/// an identical layout either way; `CurrentStoreFallback` only adds a quiet "durable
/// authority: unavailable" provenance cue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RunAuthority {
    /// Served from the durable `OrchestrationStore` projection (the primary authority).
    Durable,
    /// The durable store was unavailable, so AU-82A's current-store resolver served it.
    CurrentStoreFallback,
}

/// The one DTO the `RunEvidence` view opens: the immutable audit header + the causal identity spine
/// (`AutomationRun → StepExecution → Attempt`) + the (currently empty) per-node result slot + the
/// identity authority that served it. One identity, projected — every FE surface joins the same ids
/// (ARCH-INV-10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunEvidence {
    pub header: RunAuditHeader,
    pub run: AutomationRun,
    pub per_node_results: Vec<PerNodeResult>,
    pub authority: RunAuthority,
}

/// Pure aggregate over `records` (read **once** by the caller): the run's evidence, or `None` when the
/// run has no recorded outcome (unknown / in-flight → the view renders empty/unavailable). Deterministic
/// and rebuildable — same records → equal `RunEvidence` (AC-7).
pub fn build_run_evidence(
    records: &[StoreRecord],
    story_id: &str,
    run_id: &str,
) -> Option<RunEvidence> {
    let story_id = story_id.trim();
    let run_id = run_id.trim();
    // A blank id is not a real run — reject it rather than fold an unattributed `run_id: ""`
    // outcome record into a fabricated header (`latest_run_outcome` matches ids exactly).
    if story_id.is_empty() || run_id.is_empty() {
        return None;
    }
    let outcome = run_store::latest_run_outcome(records, story_id, run_id)?;
    let header = RunAuditHeader::from_outcome(outcome);
    let run = project_run_identity(records, story_id, run_id);
    Some(RunEvidence {
        header,
        run,
        per_node_results: fold_reported_results(records, story_id, run_id, Some(outcome)),
        authority: RunAuthority::CurrentStoreFallback,
    })
}

/// SW-AU-96: fold a run's reported step results (summary/outputs/artifacts/footprint) plus the
/// deterministic work-product auto-capture into `PerNodeResult`s, from the JSONL run-store `records`.
/// Reported results live in the JSONL ledger — NOT the durable `OrchestrationStore` (SW-AU-82B) — so
/// BOTH evidence paths (the current-store `build_run_evidence` and the durable `resolve_run_evidence`)
/// call this against the same records, or the durable authority would silently drop every reported
/// result. One fold, both authorities (DRY). The caller passes the JSONL `outcome` it already
/// resolved (no second scan); work-product auto-capture is skipped when it is `None` — a durable run
/// whose JSONL outcome is absent (compacted / durable-only) still folds its agent-reported results,
/// just without an auto-captured work product (its only home is the JSONL outcome today). Results are
/// story-scoped, so a `run_id` whose JSONL records carry a different `story_id` folds to empty rather
/// than foreign results.
fn fold_reported_results(
    records: &[StoreRecord],
    story_id: &str,
    run_id: &str,
    outcome: Option<&RunOutcomeEvent>,
) -> Vec<PerNodeResult> {
    let mut per_node_results =
        fold_per_node_results(&run_store::step_result_events(records, story_id, run_id));
    if let Some(outcome) = outcome {
        capture_work_product(&mut per_node_results, outcome);
    }
    per_node_results
}

/// Convert a durable `RunProjection` into the shared `RunEvidence` DTO (SW-AU-82B). The
/// header's free-text fields are re-scrubbed at render through the ONE `redact_secrets`
/// owner (AC-9, defense-in-depth) even though the durable payloads were already scrubbed
/// at persist — the render boundary re-scrubs uniformly, adding no second routine.
fn durable_evidence(proj: durable_run_identity::RunProjection, run_id: &str) -> RunEvidence {
    let durable_run_identity::RunProjection {
        header, mut run, ..
    } = proj;
    let trigger = match header.trigger {
        TriggerFact::Attributed { source } => TriggerFact::Attributed {
            source: redact_secrets(&source),
        },
        TriggerFact::PreEnvelope => TriggerFact::PreEnvelope,
    };
    // The durable stream is partitioned by `durable_run_key`, so the rebuilt projection carries
    // that composite in `run_id`. Present the bare `run_id` the caller joins on (SW-AU-82C) —
    // the composite is a storage partition, never surfaced.
    run.run_id = run_id.to_string();
    // The durable projection builds steps in execution (event-arrival) order; the current-store
    // fallback (`project_run_identity` → `run_step_verdicts`) yields them in `node_id` order (a
    // `BTreeMap<step_id>` fold). Sort to match so the authority switch is a pure data change —
    // the same steps in the same order, never a reordered Evidence view (AC-2 parity).
    run.steps.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    RunEvidence {
        header: RunAuditHeader {
            run_id: run_id.to_string(),
            story_id: header.story_id,
            automation_id: header.automation_id,
            trigger,
            scheduled_at: header.scheduled_at,
            fired_at: header.fired_at,
            started_at: header.started_at,
            ended_at: header.ended_at,
            status: header.status,
            summary: header.summary.as_deref().map(redact_secrets),
            // Propagate the pinned revision the durable projection captured — previously dropped here,
            // so a run-forensics reader could not reach the Run's frozen revision (AU-90).
            workflow_revision_id: header.workflow_revision_id,
        },
        run,
        per_node_results: Vec::new(),
        authority: RunAuthority::Durable,
    }
}

/// SW-AU-96: a durable projection's evidence with the JSONL-sourced reported results grafted on. The
/// durable `OrchestrationStore` does not hold reported results (they live in the JSONL run-store), so
/// this is where the durable authority path folds them in. Extracted from the resolver so the
/// durable-plus-reported composition is unit-testable against a hand-built `records` slice (the JSONL
/// read seam is not path-injectable). See [`fold_reported_results`] for the story-scoping and
/// work-product-source caveats.
pub(crate) fn durable_evidence_with_reported(
    proj: durable_run_identity::RunProjection,
    records: &[StoreRecord],
    story_id: &str,
    run_id: &str,
) -> RunEvidence {
    let outcome = run_store::latest_run_outcome(records, story_id, run_id);
    let mut evidence = durable_evidence(proj, run_id);
    evidence.per_node_results = fold_reported_results(records, story_id, run_id, outcome);
    evidence
}

/// Resolve one Run's evidence, selecting the identity authority by **store availability**
/// (AC-7, no flag day). When the durable `OrchestrationStore` opens and holds the Run, its
/// projection serves (`authority: Durable`); when the store is unavailable, or the Run
/// predates the durable authority, AU-82A's current-store resolver serves it
/// (`authority: CurrentStoreFallback`). A durable **read** failure is surfaced as an error
/// (the FE's retry state, AC-12), distinct from an unknown Run (`None`, unavailable).
/// Blocking throughout — the caller wraps it in `await_blocking` (AC-13). Resolves the
/// durable store path from `codesense_home` and delegates to [`resolve_run_evidence_at`].
fn resolve_run_evidence(story_id: &str, run_id: &str) -> Result<Option<RunEvidence>, String> {
    let durable_path = crate::commands::paths::codesense_home_dir().map(|h| default_store_path(&h));
    resolve_run_evidence_at(durable_path.as_deref(), story_id, run_id)
}

/// The availability-gated core, with the durable store path injected so it is testable
/// against a temp DB (AC-7/AC-11). When the durable store opens and holds the Run, its
/// projection serves; otherwise the current-store resolver does.
fn resolve_run_evidence_at(
    durable_path: Option<&std::path::Path>,
    story_id: &str,
    run_id: &str,
) -> Result<Option<RunEvidence>, String> {
    let story_id = story_id.trim();
    let run_id = run_id.trim();
    // A blank id is not a real run — reject before any store lookup, mirroring
    // `build_run_evidence`, so the durable path never serves an unattributed run.
    if story_id.is_empty() || run_id.is_empty() {
        return Ok(None);
    }
    // Store won't open (no WAL, corrupt file, no home): degrade to the current store.
    if let Some(store) = durable_path.and_then(|path| SqliteOrchestrationStore::open(path).ok()) {
        // SW-AU-82C partitions the durable stream by `(story_id, run_id)` (a sprint/epic tick
        // shares one `run_id` across stories, so the bare key would collide them). Read the
        // composite key first; fall back to the bare `run_id` for a pre-cutover stream keyed by
        // it alone. Because the producer always writes the composite, the bare read is reached
        // only when no composite stream exists (legacy) — never masking a live multi-story run.
        let composite = durable_run_identity::durable_run_key(run_id, story_id);
        for key in [composite.as_str(), run_id] {
            match store.run_events_for(key) {
                Ok(events) => {
                    if let Some(proj) = durable_run_identity::RunIdentityProjector::rebuild(&events)
                    {
                        // Evidence is never served by `run_id` alone (AC-10): the requested
                        // `story_id` must scope the projected run. A mismatch (or a run that
                        // predates the durable authority) falls through to the current store,
                        // which also scopes by `story_id` — never leaking a foreign run.
                        if proj.header.story_id == story_id {
                            // SW-AU-96 reported results live in the JSONL run-store, not the durable
                            // OrchestrationStore, so fold them onto the durable evidence too —
                            // otherwise the durable authority path serves an empty
                            // `per_node_results`. A JSONL read failure degrades to no reported
                            // results (best-effort enrichment) and must NOT sink an otherwise-good
                            // durable read (which is the primary authority).
                            let records = match run_store::read_default_records_checked() {
                                Ok((records, _health)) => records,
                                Err(e) => {
                                    log::warn!(
                                        target: "run_evidence",
                                        "durable evidence: JSONL read failed, serving without reported results: {e}"
                                    );
                                    Vec::new()
                                }
                            };
                            return Ok(Some(durable_evidence_with_reported(
                                proj, &records, story_id, run_id,
                            )));
                        }
                    }
                }
                // Rebuild/read failed against an available store: a distinct honest failure
                // (retry), never conflated with an empty/unknown Run (AC-12).
                Err(e) => return Err(format!("durable run projection failed: {e}")),
            }
        }
    }
    let (records, _health) = run_store::read_default_records_checked()?;
    Ok(build_run_evidence(&records, story_id, run_id))
}

/// AC-1/AC-3/AC-4: fold a run's ordered `StepResult` events into one [`PerNodeResult`] per node.
/// Scalar channels (`summary`, `outputs`) are latest-wins; `artifacts` and `footprint` are a
/// union deduped by `(kind, reference)` so an agent report and a harness auto-capture of the same
/// entity converge to one row. Node order is first-seen, for stable rendering. The events arrive
/// already scrubbed (write-time, AC-5), so this render fold trusts them.
fn fold_per_node_results(events: &[&StepResultEvent]) -> Vec<PerNodeResult> {
    let mut order: Vec<String> = Vec::new();
    let mut by_node: std::collections::HashMap<String, PerNodeResult> =
        std::collections::HashMap::new();
    for event in events {
        let node = by_node.entry(event.node_id.clone()).or_insert_with(|| {
            order.push(event.node_id.clone());
            PerNodeResult {
                node_id: event.node_id.clone(),
                ..PerNodeResult::default()
            }
        });
        // Latest NON-EMPTY summary wins — a later empty/whitespace report never clobbers a real one.
        if event
            .summary
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
        {
            node.summary = event.summary.clone();
        }
        for output in &event.outputs {
            upsert_output(&mut node.outputs, output.clone());
        }
        for artifact in &event.artifacts {
            upsert_artifact(&mut node.artifacts, artifact.clone());
        }
        for entry in &event.footprint {
            upsert_footprint(&mut node.footprint, entry.clone());
        }
    }
    order
        .into_iter()
        .filter_map(|id| by_node.remove(&id))
        .collect()
}

/// Latest-wins upsert of a typed output keyed by `key`.
fn upsert_output(outputs: &mut Vec<StepOutput>, output: StepOutput) {
    match outputs
        .iter_mut()
        .find(|existing| existing.key == output.key)
    {
        Some(existing) => existing.value = output.value,
        None => outputs.push(output),
    }
}

/// Union-dedup of an artifact by `(kind, reference)`. A later report updates the label, but a `None`
/// label (e.g. from harness auto-capture) never wipes an agent-provided one — labels only improve.
fn upsert_artifact(artifacts: &mut Vec<StepArtifact>, artifact: StepArtifact) {
    match artifacts
        .iter_mut()
        .find(|existing| existing.kind == artifact.kind && existing.reference == artifact.reference)
    {
        Some(existing) => {
            if artifact.label.is_some() {
                existing.label = artifact.label;
            }
        }
        None => artifacts.push(artifact),
    }
}

/// Union-dedup of a footprint entry by `(kind, reference)`. `Created` wins over `Modified` on a
/// collision — a create-then-modify of the same entity is still a create for provenance.
fn upsert_footprint(footprint: &mut Vec<FootprintEntry>, entry: FootprintEntry) {
    match footprint
        .iter_mut()
        .find(|existing| existing.kind == entry.kind && existing.reference == entry.reference)
    {
        Some(existing) => {
            if entry.change == FootprintChange::Created {
                existing.change = FootprintChange::Created;
            }
        }
        None => footprint.push(entry),
    }
}

/// AC-2 (auto-capture half): the run's work product (the `owner/repo@branch` the harness captured
/// deterministically at completion, SW-OR-17) surfaces on the reporting node — **even if the agent
/// reported nothing** — so completeness never depends on agent goodwill. It is recorded as an
/// `Entity` (a branch reference, honestly — not a resolved PR URL; SW-AU-97/94 can enrich to the PR),
/// deduped by `(kind, reference)`. It attaches to the SAME [`DEFAULT_STEP_NODE_ID`] the reporting
/// tools default to, so an agent's own reports and this auto-capture converge on one node rather than
/// splitting into two. A run with no work product is a no-op. The reference is scrubbed for parity.
fn capture_work_product(per_node_results: &mut Vec<PerNodeResult>, outcome: &RunOutcomeEvent) {
    let Some(work_product) = &outcome.work_product else {
        return;
    };
    let reference = redact_secrets(&format!(
        "{}@{}",
        work_product.source_repo, work_product.branch
    ));
    let node = match per_node_results
        .iter_mut()
        .find(|result| result.node_id == DEFAULT_STEP_NODE_ID)
    {
        Some(existing) => existing,
        None => {
            per_node_results.push(PerNodeResult {
                node_id: DEFAULT_STEP_NODE_ID.to_string(),
                ..PerNodeResult::default()
            });
            per_node_results.last_mut().expect("just pushed the node")
        }
    };
    upsert_artifact(
        &mut node.artifacts,
        StepArtifact {
            kind: ArtifactKind::Entity,
            reference: reference.clone(),
            label: Some("run work product (branch)".into()),
        },
    );
    upsert_footprint(
        &mut node.footprint,
        FootprintEntry {
            kind: ArtifactKind::Entity,
            reference,
            change: FootprintChange::Created,
        },
    );
}

/// SW-AU-82A (AC-10 backend, AC-9): open one run's evidence for the `RunEvidence` view.
///
/// Viewing evidence is always permitted for the local user (CodeSense has no auth boundary).
/// Evidence is never exposed by `run_id` alone: the command also requires `story_id`.
///
/// **AC-10/AC-13 UI thread:** the whole resolution — durable store open + event replay, or
/// the current-store fold — runs inside ONE `await_blocking` closure wrapping
/// `with_blocking` (both paths are synchronous/blocking), so opening the view never stalls
/// the renderer or spins the terminal. The `BlockingCtx` witness type-checks only off the
/// async runtime, proving the SQLite/JSONL work is off-thread.
#[tauri::command]
pub async fn read_run_evidence(
    story_id: String,
    run_id: String,
) -> Result<Option<RunEvidence>, String> {
    crate::commands::await_blocking("read_run_evidence", move || {
        // RI-2-ALLOW: rusqlite (durable store) and the JSONL fallback are blocking; the
        // witness proves this runs off the async runtime so a contended read (busy_timeout /
        // WAL checkpoint) can never stall the renderer or spin the terminal.
        with_blocking(|_ctx: &BlockingCtx| resolve_run_evidence(&story_id, &run_id))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::sprint_run::run_store::{
        OutcomeSource, RunEvent, StoreRecord, VerdictEvent,
    };
    use crate::commands::sprint_run::run_store::{StepResult, Via};

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

    fn outcome(
        run_id: &str,
        trigger_source: Option<&str>,
        summary: Option<&str>,
    ) -> RunOutcomeEvent {
        RunOutcomeEvent {
            story_id: "SW-AU-82A".into(),
            run_id: run_id.into(),
            automation_id: Some("todo->done".into()),
            trigger_source: trigger_source.map(str::to_string),
            workspace: None,
            scheduled_at: None,
            fired_at: Some("2026-09-02T00:00:00Z".into()),
            work_product: None,
            status: SessionStatus::Success,
            summary: summary.map(str::to_string),
            exit_code: Some(0),
            started_at: Some("2026-09-02T00:00:00Z".into()),
            ended_at: "2026-09-02T00:01:00Z".into(),
            artifact_ref: None,
            validator_step: None,
            structured_declared: None,
            org: None,
            team: None,
            session_ids: Vec::new(),
            source: OutcomeSource::ProcessExit,
        }
    }

    /// AC-5/AC-11: a run with a trigger source captures an Attributed trigger step; the header carries
    /// the config/outcome fields and the identity spine folds in.
    #[test]
    fn builds_header_and_identity_with_an_attributed_trigger() {
        let records = vec![
            rec(
                0,
                RunEvent::Verdict(verdict("run-1", "dev", StepResult::Passed)),
            ),
            rec(
                1,
                RunEvent::Outcome(Box::new(outcome(
                    "run-1",
                    Some("watch:ci"),
                    Some("all green"),
                ))),
            ),
        ];
        let ev = build_run_evidence(&records, "SW-AU-82A", "run-1").expect("has evidence");
        assert_eq!(ev.header.run_id, "run-1");
        assert_eq!(
            ev.header.trigger,
            TriggerFact::Attributed {
                source: "watch:ci".into()
            }
        );
        assert_eq!(ev.header.status, Some(SessionStatus::Success));
        assert!(
            ev.header.ended_at.is_some(),
            "current-store header (from a terminal outcome) always carries terminal fields"
        );
        assert_eq!(ev.authority, RunAuthority::CurrentStoreFallback);
        assert_eq!(ev.run.steps.len(), 1, "identity spine folded in");
        assert_eq!(ev.run.steps[0].node_id, "dev");
        assert!(
            ev.per_node_results.is_empty(),
            "AC-11 slot empty on the current store"
        );
    }

    /// AC-11: a pre-envelope run (no trigger source) renders an explicit PreEnvelope trigger — never
    /// a fabricated event.
    #[test]
    fn a_pre_envelope_run_renders_an_honest_pre_envelope_trigger() {
        let records = vec![rec(
            0,
            RunEvent::Outcome(Box::new(outcome("run-2", None, None))),
        )];
        let ev = build_run_evidence(&records, "SW-AU-82A", "run-2").expect("has evidence");
        assert_eq!(ev.header.trigger, TriggerFact::PreEnvelope);
    }

    /// AC-6: the header summary is scrubbed at render.
    #[test]
    fn header_summary_is_scrubbed_at_render() {
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123";
        let records = vec![rec(
            0,
            RunEvent::Outcome(Box::new(outcome(
                "run-3",
                None,
                Some(&format!("leaked {secret}")),
            ))),
        )];
        let ev = build_run_evidence(&records, "SW-AU-82A", "run-3").expect("has evidence");
        let summary = ev.header.summary.unwrap();
        assert!(!summary.contains(secret), "secret scrubbed from header");
        assert!(summary.contains("[redacted]"));
    }

    /// AC-7: the aggregate is a rebuildable projection — same records → equal RunEvidence.
    #[test]
    fn rebuildable_from_the_same_records() {
        let records = vec![
            rec(
                0,
                RunEvent::Verdict(verdict("run-1", "dev", StepResult::Passed)),
            ),
            rec(
                1,
                RunEvent::Outcome(Box::new(outcome("run-1", Some("manual"), Some("ok")))),
            ),
        ];
        let a = build_run_evidence(&records, "SW-AU-82A", "run-1");
        let b = build_run_evidence(&records, "SW-AU-82A", "run-1");
        assert_eq!(a, b, "deterministic, rebuildable projection");
    }

    /// An unknown run (no recorded outcome) yields no evidence — the view degrades to empty/unavailable,
    /// never a fabricated header.
    #[test]
    fn unknown_run_yields_no_evidence() {
        let records = vec![rec(
            0,
            RunEvent::Verdict(verdict("run-1", "dev", StepResult::Passed)),
        )];
        assert!(build_run_evidence(&records, "SW-AU-82A", "ghost").is_none());
        // A verdict-only run with no outcome is still in-flight → no header yet.
        assert!(build_run_evidence(&records, "SW-AU-82A", "run-1").is_none());
    }

    /// A blank / whitespace id never folds an unattributed `run_id: ""` record into a header.
    #[test]
    fn blank_ids_yield_no_evidence() {
        let records = vec![rec(
            0,
            RunEvent::Outcome(Box::new(outcome("", None, Some("orphan")))),
        )];
        assert!(build_run_evidence(&records, "SW-AU-82A", "").is_none());
        assert!(build_run_evidence(&records, "SW-AU-82A", "   ").is_none());
        assert!(build_run_evidence(&records, "  ", "run-1").is_none());
    }

    /// AC-11: an empty / whitespace trigger source degrades to an honest PreEnvelope trigger.
    #[test]
    fn whitespace_trigger_source_degrades_to_pre_envelope() {
        let records = vec![rec(
            0,
            RunEvent::Outcome(Box::new(outcome("run-9", Some("   "), None))),
        )];
        let ev = build_run_evidence(&records, "SW-AU-82A", "run-9").expect("has evidence");
        assert_eq!(ev.header.trigger, TriggerFact::PreEnvelope);
    }

    /// AC-6: the trigger source is scrubbed at render too (a source label could carry a token).
    #[test]
    fn trigger_source_is_scrubbed_at_render() {
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123";
        let records = vec![rec(
            0,
            RunEvent::Outcome(Box::new(outcome(
                "run-9",
                Some(&format!("connector token={secret}")),
                None,
            ))),
        )];
        let ev = build_run_evidence(&records, "SW-AU-82A", "run-9").expect("has evidence");
        match ev.header.trigger {
            TriggerFact::Attributed { source } => {
                assert!(
                    !source.contains(secret),
                    "secret scrubbed from trigger source"
                );
                assert!(source.contains("[redacted]"));
            }
            other => panic!("expected attributed trigger, got {other:?}"),
        }
    }

    /// SW-AU-96B (AC-3/AC-4): the auto-captured per-file `File` footprint and an agent-reported one
    /// for the same `(kind, reference)` converge to a single entry through the ONE `build_run_evidence`
    /// reader — no double-count, `Created` winning over `Modified` — and a second changed file rides
    /// alongside. Proves the completion-time capture folds identically to an agent report.
    #[test]
    fn per_file_footprint_folds_and_dedupes_with_agent_report() {
        use crate::commands::sprint_run::run_store::{
            ArtifactKind, FootprintChange, FootprintEntry,
        };

        let entry = |reference: &str, change| FootprintEntry {
            kind: ArtifactKind::File,
            reference: reference.into(),
            change,
        };
        let step = |seq: u64, footprint: Vec<FootprintEntry>| {
            rec(
                seq,
                RunEvent::StepResult(StepResultEvent {
                    story_id: "SW-AU-82A".into(),
                    run_id: "run-1".into(),
                    node_id: DEFAULT_STEP_NODE_ID.into(),
                    summary: None,
                    outputs: Vec::new(),
                    artifacts: Vec::new(),
                    footprint,
                }),
            )
        };
        let records = vec![
            // Agent reported one file as modified…
            step(0, vec![entry("a.rs", FootprintChange::Modified)]),
            // …the harness auto-capture reports the same file as created + a second new file.
            step(
                1,
                vec![
                    entry("a.rs", FootprintChange::Created),
                    entry("b.rs", FootprintChange::Created),
                ],
            ),
            rec(
                2,
                RunEvent::Outcome(Box::new(outcome("run-1", Some("manual"), None))),
            ),
        ];

        let ev = build_run_evidence(&records, "SW-AU-82A", "run-1").expect("has evidence");
        let node = ev
            .per_node_results
            .iter()
            .find(|n| n.node_id == DEFAULT_STEP_NODE_ID)
            .expect("reporting node");
        let a: Vec<_> = node
            .footprint
            .iter()
            .filter(|f| f.reference == "a.rs")
            .collect();
        assert_eq!(a.len(), 1, "agent + auto File a.rs dedupe to one entry");
        assert_eq!(
            a[0].change,
            FootprintChange::Created,
            "Created wins over Modified on collision"
        );
        assert!(node
            .footprint
            .iter()
            .any(|f| f.reference == "b.rs" && f.change == FootprintChange::Created));
    }

    // ---- SW-AU-82B: the durable authority path (AC-7/9/11/13/14) ----
    mod durable {
        use super::super::*;
        use crate::commands::async_helpers::{await_blocking, with_blocking, BlockingCtx};
        use crate::commands::sprint_run::durable_run_identity::test_support::{
            accept_run, attach_attempt, complete_run, start_step,
        };
        use crate::commands::sprint_run::durable_run_identity::{
            durable_run_key, AttemptOutputPayload, ImmutableAuditProvenance, RunAcceptedPayload,
            RunTerminalPayload, StepStartedPayload,
        };
        use crate::commands::sprint_run::run_identity::{attempt_id, step_execution_id};
        use crate::commands::sprint_run::run_store::StepResult;
        use crate::coordinator::SqliteOrchestrationStore;

        fn author_full_run(
            store: &SqliteOrchestrationStore,
            run_id: &str,
            parent: Option<&str>,
            summary: Option<&str>,
        ) {
            accept_run(
                store,
                run_id,
                &RunAcceptedPayload {
                    parent_run_id: parent.map(str::to_string),
                    origin_node_id: parent.map(|_| "spawn-node".to_string()),
                    correlation_id: parent.map(str::to_string).unwrap_or_else(|| run_id.into()),
                    provenance: ImmutableAuditProvenance {
                        story_id: "SW-AU-82B".into(),
                        automation_id: Some("auto-1".into()),
                        trigger: TriggerFact::Attributed {
                            source: "schedule".into(),
                        },
                        scheduled_at: None,
                        fired_at: None,
                        started_at: Some("2026-09-02T00:00:00Z".into()),
                        workflow_revision_id: Some("rev-1".into()),
                        subject_ref: None,
                        child_automation_revision: None,
                        invocation_generation: None,
                    },
                },
            )
            .unwrap();
            let sxid = step_execution_id(run_id, "dev");
            start_step(
                store,
                run_id,
                &StepStartedPayload {
                    step_execution_id: sxid.clone(),
                    node_id: "dev".into(),
                    revision_id: "rev-1".into(),
                },
            )
            .unwrap();
            attach_attempt(
                store,
                run_id,
                &AttemptOutputPayload {
                    step_execution_id: sxid.clone(),
                    attempt_id: attempt_id(&sxid, 1),
                    attempt_ordinal: 1,
                    run_id: run_id.into(),
                    result: StepResult::Passed,
                },
            )
            .unwrap();
            complete_run(
                store,
                run_id,
                &RunTerminalPayload {
                    status: SessionStatus::Success,
                    ended_at: "2026-09-02T00:05:00Z".into(),
                    summary: summary.map(str::to_string),
                },
            )
            .unwrap();
        }

        fn temp_store() -> (
            tempfile::TempDir,
            std::path::PathBuf,
            SqliteOrchestrationStore,
        ) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("coordinator.sqlite3");
            let store = SqliteOrchestrationStore::open(&path).unwrap();
            (dir, path, store)
        }

        /// AC-7/AC-14: when the durable store holds the Run, the resolver serves its
        /// projection — header + step spine + per-attempt evidence — with `Durable` authority.
        #[test]
        fn durable_path_serves_projected_evidence() {
            let (_dir, path, store) = temp_store();
            author_full_run(&store, "run-1", None, Some("all green"));

            let ev = resolve_run_evidence_at(Some(&path), "SW-AU-82B", "run-1")
                .expect("resolve ok")
                .expect("durable evidence");
            assert_eq!(ev.authority, RunAuthority::Durable);
            assert_eq!(ev.header.story_id, "SW-AU-82B");
            assert_eq!(ev.header.status, Some(SessionStatus::Success));
            assert_eq!(ev.header.ended_at.as_deref(), Some("2026-09-02T00:05:00Z"));
            assert_eq!(ev.run.steps.len(), 1);
            assert_eq!(ev.run.steps[0].node_id, "dev");
            assert_eq!(ev.run.steps[0].attempts.len(), 1);
            assert_eq!(ev.run.steps[0].attempts[0].attempt_ordinal, 1);
        }

        /// SW-AU-82C: a run authored under the composite `(story_id, run_id)` stream key — the
        /// shape the retired durable producer wrote (composite partition, but bare-`run_id`-based
        /// step/attempt ids) — resolves with `Durable` authority and presents the BARE `run_id`.
        /// Proves the resolver's composite read + bare-id presentation across the producer→resolver
        /// seam; the bare key would collide sibling stories sharing this `run_id`.
        #[test]
        fn composite_keyed_run_resolves_durable_and_presents_bare_run_id() {
            let (_dir, path, store) = temp_store();
            let (story, run) = ("SW-AU-82B", "run-shared-7");
            let key = durable_run_key(run, story);
            accept_run(
                &store,
                &key,
                &RunAcceptedPayload {
                    parent_run_id: None,
                    origin_node_id: None,
                    correlation_id: run.into(),
                    provenance: ImmutableAuditProvenance {
                        story_id: story.into(),
                        automation_id: None,
                        trigger: TriggerFact::PreEnvelope,
                        scheduled_at: None,
                        fired_at: None,
                        started_at: None,
                        workflow_revision_id: None,
                        subject_ref: None,
                        child_automation_revision: None,
                        invocation_generation: None,
                    },
                },
            )
            .unwrap();
            let sxid = step_execution_id(run, "dev");
            start_step(
                &store,
                &key,
                &StepStartedPayload {
                    step_execution_id: sxid.clone(),
                    node_id: "dev".into(),
                    revision_id: "rev-1".into(),
                },
            )
            .unwrap();
            attach_attempt(
                &store,
                &key,
                &AttemptOutputPayload {
                    step_execution_id: sxid.clone(),
                    attempt_id: attempt_id(&sxid, 1),
                    attempt_ordinal: 1,
                    run_id: run.into(),
                    result: StepResult::Passed,
                },
            )
            .unwrap();

            let ev = resolve_run_evidence_at(Some(&path), story, run)
                .expect("resolve ok")
                .expect("durable evidence");
            assert_eq!(ev.authority, RunAuthority::Durable);
            assert_eq!(
                ev.header.run_id, run,
                "presents the bare run_id, not the composite stream key"
            );
            assert_eq!(ev.run.run_id, run);
            assert_eq!(ev.run.steps.len(), 1);
            assert_eq!(ev.run.steps[0].step_execution_id, sxid);
            assert_eq!(
                ev.run.steps[0].attempts[0].run_id, run,
                "attempt carries the bare run_id compat key"
            );
        }

        /// SW-AU-82C: the durable authority presents steps in `node_id` order, matching the
        /// current-store fallback (`project_run_identity` folds through a `BTreeMap<step_id>`),
        /// even though the durable stream records them in execution order — so flipping to the
        /// durable authority never reorders the Evidence view (AC-2 order parity).
        #[test]
        fn durable_steps_are_presented_in_node_id_order() {
            let (_dir, path, store) = temp_store();
            let (story, run) = ("SW-AU-82B", "run-ord");
            let key = durable_run_key(run, story);
            accept_run(
                &store,
                &key,
                &RunAcceptedPayload {
                    parent_run_id: None,
                    origin_node_id: None,
                    correlation_id: run.into(),
                    provenance: ImmutableAuditProvenance {
                        story_id: story.into(),
                        automation_id: None,
                        trigger: TriggerFact::PreEnvelope,
                        scheduled_at: None,
                        fired_at: None,
                        started_at: None,
                        workflow_revision_id: None,
                        subject_ref: None,
                        child_automation_revision: None,
                        invocation_generation: None,
                    },
                },
            )
            .unwrap();
            // Author in reverse-lexicographic execution order: `review` then `dev`.
            for node in ["review", "dev"] {
                let sxid = step_execution_id(run, node);
                start_step(
                    &store,
                    &key,
                    &StepStartedPayload {
                        step_execution_id: sxid.clone(),
                        node_id: node.into(),
                        revision_id: "r".into(),
                    },
                )
                .unwrap();
                attach_attempt(
                    &store,
                    &key,
                    &AttemptOutputPayload {
                        step_execution_id: sxid.clone(),
                        attempt_id: attempt_id(&sxid, 1),
                        attempt_ordinal: 1,
                        run_id: run.into(),
                        result: StepResult::Passed,
                    },
                )
                .unwrap();
            }
            let ev = resolve_run_evidence_at(Some(&path), story, run)
                .expect("resolve ok")
                .expect("durable evidence");
            let order: Vec<&str> = ev.run.steps.iter().map(|s| s.node_id.as_str()).collect();
            assert_eq!(
                order,
                ["dev", "review"],
                "steps presented in node_id order, not execution order"
            );
        }

        fn reported_record(
            story_id: &str,
            run_id: &str,
            node_id: &str,
            summary: &str,
        ) -> StoreRecord {
            StoreRecord {
                schema_version: run_store::SCHEMA_VERSION,
                seq: 0,
                ts: 0,
                event: run_store::RunEvent::StepResult(StepResultEvent {
                    story_id: story_id.into(),
                    run_id: run_id.into(),
                    node_id: node_id.into(),
                    summary: Some(summary.into()),
                    outputs: Vec::new(),
                    artifacts: Vec::new(),
                    footprint: Vec::new(),
                }),
            }
        }

        /// SW-AU-96: reported step results (which live in the JSONL run-store, NOT the durable
        /// OrchestrationStore) fold onto the DURABLE authority path too — without the graft the
        /// durable authority (the primary path) would serve an empty `per_node_results`.
        #[test]
        fn durable_path_folds_reported_results() {
            use crate::commands::sprint_run::durable_run_identity::RunIdentityProjector;

            let (_dir, _path, store) = temp_store();
            author_full_run(&store, "run-1", None, Some("all green"));
            let events = store.run_events_for("run-1").expect("events");
            let proj = RunIdentityProjector::rebuild(&events).expect("durable projection");

            let records = vec![reported_record("SW-AU-82B", "run-1", "dev", "did the work")];
            let ev = durable_evidence_with_reported(proj, &records, "SW-AU-82B", "run-1");

            assert_eq!(ev.authority, RunAuthority::Durable);
            assert_eq!(
                ev.per_node_results.len(),
                1,
                "reported result folds on the durable path"
            );
            assert_eq!(ev.per_node_results[0].node_id, "dev");
            assert_eq!(
                ev.per_node_results[0].summary.as_deref(),
                Some("did the work")
            );
        }

        /// SW-AU-96: a reported result whose JSONL `story_id` disagrees with the requested story
        /// folds to empty (never foreign results) — the durable header and JSONL results are
        /// consistently story-scoped.
        #[test]
        fn durable_reported_results_are_story_scoped() {
            use crate::commands::sprint_run::durable_run_identity::RunIdentityProjector;

            let (_dir, _path, store) = temp_store();
            author_full_run(&store, "run-1", None, None);
            let events = store.run_events_for("run-1").expect("events");
            let proj = RunIdentityProjector::rebuild(&events).expect("durable projection");

            // Same run_id, different story_id — must NOT fold in.
            let records = vec![reported_record("SW-OTHER", "run-1", "dev", "foreign")];
            let ev = durable_evidence_with_reported(proj, &records, "SW-AU-82B", "run-1");

            assert!(
                ev.per_node_results.is_empty(),
                "foreign-story results never fold onto a durable run"
            );
        }

        /// AC-14 / ARCH-INV-20: a child Run projects its parent causation link by
        /// `parent_run_id` under one `correlation_id` — the "Started by" lane's data.
        #[test]
        fn durable_child_carries_parent_causation() {
            let (_dir, path, store) = temp_store();
            author_full_run(&store, "parent-1", None, None);
            author_full_run(&store, "child-1", Some("parent-1"), None);

            let child = resolve_run_evidence_at(Some(&path), "SW-AU-82B", "child-1")
                .unwrap()
                .unwrap();
            assert_eq!(child.run.parent_run_id.as_deref(), Some("parent-1"));
            assert_eq!(child.run.origin_node_id.as_deref(), Some("spawn-node"));
            assert_eq!(child.run.correlation_id, "parent-1");
        }

        /// AC-9: a secret in an event payload is absent from BOTH the persisted store row
        /// (scrubbed at persist by `insert_run_event`) and the rendered projection
        /// (re-scrubbed at render) — one scrub owner, no second routine.
        #[test]
        fn secret_absent_from_store_row_and_rendered_projection() {
            let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123";
            let (_dir, path, store) = temp_store();
            author_full_run(&store, "run-1", None, Some(&format!("leaked {secret}")));

            assert_eq!(
                store.count_event_payloads_containing(secret).unwrap(),
                0,
                "secret never lands unscrubbed in a DB row"
            );
            let ev = resolve_run_evidence_at(Some(&path), "SW-AU-82B", "run-1")
                .unwrap()
                .unwrap();
            let summary = ev.header.summary.unwrap();
            assert!(!summary.contains(secret), "secret scrubbed from render");
            assert!(summary.contains("[redacted]"));
        }

        /// AC-11: after teardown + re-open from the same path, the projection surfaces the
        /// full identity + causal graph + captured header unchanged — no new CAS semantics.
        #[test]
        fn identity_survives_restart() {
            let (_dir, path, _store) = temp_store();
            {
                let store = SqliteOrchestrationStore::open(&path).unwrap();
                author_full_run(&store, "run-1", Some("parent-9"), Some("ok"));
                // store dropped here — simulates process teardown
            }
            let ev = resolve_run_evidence_at(Some(&path), "SW-AU-82B", "run-1")
                .unwrap()
                .expect("evidence survives restart");
            assert_eq!(ev.run.run_id, "run-1");
            assert_eq!(ev.run.parent_run_id.as_deref(), Some("parent-9"));
            assert_eq!(ev.run.steps.len(), 1);
            assert_eq!(ev.header.status, Some(SessionStatus::Success));

            // The captured immutable `workflow_revision` survives the restart unchanged —
            // asserted at the projection layer where it lives (AC-11).
            let reopened = SqliteOrchestrationStore::open(&path).unwrap();
            let proj =
                crate::commands::sprint_run::durable_run_identity::RunIdentityProjector::rebuild(
                    &reopened.run_events_for("run-1").unwrap(),
                )
                .unwrap();
            assert_eq!(proj.header.workflow_revision_id.as_deref(), Some("rev-1"));
        }

        /// SW-AU-90 R1: the pinned `workflow_revision_id` captured on the durable provenance now flows
        /// THROUGH `durable_evidence` onto the shared `RunEvidence.header` — it was previously dropped, so
        /// a run-forensics reader composing over `read_run_evidence` could not reach the Run's frozen
        /// revision. Guards that latent drop from returning.
        #[test]
        fn durable_evidence_surfaces_pinned_revision_on_header() {
            let (_dir, path, store) = temp_store();
            author_full_run(&store, "run-1", None, Some("ok"));
            let ev = resolve_run_evidence_at(Some(&path), "SW-AU-82B", "run-1")
                .unwrap()
                .expect("durable evidence resolves");
            assert_eq!(ev.authority, RunAuthority::Durable);
            assert_eq!(ev.header.workflow_revision_id.as_deref(), Some("rev-1"));
        }

        /// A Run absent from an available durable store falls through to the current-store
        /// resolver (legacy compatibility) — never a durable error (AC-7).
        #[test]
        fn absent_durable_run_falls_through_without_error() {
            let (_dir, path, _store) = temp_store();
            let resolved = resolve_run_evidence_at(Some(&path), "SW-AU-82B", "ghost").unwrap();
            assert!(resolved.is_none());
        }

        /// AC-13: the whole durable resolution runs off the async runtime. The `BlockingCtx`
        /// witness minted inside the closure only type-checks off the runtime, so this
        /// compiling + passing proves the SQLite work never stalls the renderer / spins the
        /// terminal.
        #[tokio::test]
        async fn resolution_runs_off_the_async_runtime() {
            let (_dir, path, store) = temp_store();
            author_full_run(&store, "run-1", None, Some("ok"));

            let ev = await_blocking("read_run_evidence_test", move || {
                // RI-2-ALLOW: witness test — proves the resolver's SQLite work is off-thread.
                with_blocking(|_ctx: &BlockingCtx| {
                    resolve_run_evidence_at(Some(&path), "SW-AU-82B", "run-1")
                })
            })
            .await
            .expect("resolve ok")
            .expect("durable evidence");
            assert_eq!(ev.authority, RunAuthority::Durable);
            assert_eq!(ev.run.steps.len(), 1);
        }

        /// AC-10: evidence is never served by `run_id` alone — the durable path requires the
        /// requested `story_id` to scope the projected run, and blank ids short-circuit.
        #[test]
        fn durable_evidence_requires_matching_story_id() {
            let (_dir, path, store) = temp_store();
            author_full_run(&store, "run-1", None, None); // story_id = SW-AU-82B

            let ok = resolve_run_evidence_at(Some(&path), "SW-AU-82B", "run-1").unwrap();
            assert!(matches!(ok, Some(ev) if ev.authority == RunAuthority::Durable));

            // A known run_id under a foreign story must not be exposed — it falls through to
            // the (story-scoped) current store, which does not hold it.
            let mismatched = resolve_run_evidence_at(Some(&path), "OTHER-STORY", "run-1").unwrap();
            assert!(
                mismatched.is_none(),
                "run_id alone must not expose evidence"
            );

            // Blank ids short-circuit before any store lookup.
            assert!(resolve_run_evidence_at(Some(&path), "", "run-1")
                .unwrap()
                .is_none());
            assert!(resolve_run_evidence_at(Some(&path), "SW-AU-82B", "  ")
                .unwrap()
                .is_none());
        }

        /// AC-9: a secret in a NON-summary payload field (the trigger source, carried in the
        /// `run.accepted` payload) is absent from the persisted store row AND the render.
        #[test]
        fn secret_in_non_summary_payload_is_scrubbed_at_persist_and_render() {
            let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123";
            let (_dir, path, store) = temp_store();
            accept_run(
                &store,
                "run-1",
                &RunAcceptedPayload {
                    parent_run_id: None,
                    origin_node_id: None,
                    correlation_id: "run-1".into(),
                    provenance: ImmutableAuditProvenance {
                        story_id: "SW-AU-82B".into(),
                        automation_id: None,
                        trigger: TriggerFact::Attributed {
                            source: format!("connector token={secret}"),
                        },
                        scheduled_at: None,
                        fired_at: None,
                        started_at: None,
                        workflow_revision_id: None,
                        subject_ref: None,
                        child_automation_revision: None,
                        invocation_generation: None,
                    },
                },
            )
            .unwrap();

            assert_eq!(
                store.count_event_payloads_containing(secret).unwrap(),
                0,
                "secret in the trigger source is scrubbed at persist"
            );
            let ev = resolve_run_evidence_at(Some(&path), "SW-AU-82B", "run-1")
                .unwrap()
                .unwrap();
            match ev.header.trigger {
                TriggerFact::Attributed { source } => {
                    assert!(!source.contains(secret), "scrubbed at render");
                    assert!(source.contains("[redacted]"));
                }
                other => panic!("expected attributed trigger, got {other:?}"),
            }
        }
    }

    fn step_result(run_id: &str, node_id: &str) -> StepResultEvent {
        StepResultEvent {
            story_id: "SW-AU-82A".into(),
            run_id: run_id.into(),
            node_id: node_id.into(),
            summary: None,
            outputs: Vec::new(),
            artifacts: Vec::new(),
            footprint: Vec::new(),
        }
    }

    fn artifact(kind: ArtifactKind, reference: &str) -> StepArtifact {
        StepArtifact {
            kind,
            reference: reference.into(),
            label: None,
        }
    }

    fn touched(kind: ArtifactKind, reference: &str, change: FootprintChange) -> FootprintEntry {
        FootprintEntry {
            kind,
            reference: reference.into(),
            change,
        }
    }

    /// AC-1/AC-4: an agent reports summary + typed outputs + artifacts; all three fold onto the
    /// node's evidence through the ONE reader.
    #[test]
    fn folds_reported_channels_onto_the_node() {
        let mut sr = step_result("run-1", "dev");
        sr.summary = Some("implemented the reporting seam".into());
        sr.outputs = vec![StepOutput {
            key: "tests_added".into(),
            value: serde_json::json!(5),
        }];
        sr.artifacts = vec![artifact(ArtifactKind::File, "src-tauri/src/x.rs")];
        let records = vec![
            rec(
                0,
                RunEvent::Outcome(Box::new(outcome("run-1", Some("manual"), None))),
            ),
            rec(1, RunEvent::StepResult(sr)),
        ];
        let ev = build_run_evidence(&records, "SW-AU-82A", "run-1").expect("has evidence");
        assert_eq!(ev.per_node_results.len(), 1);
        let node = &ev.per_node_results[0];
        assert_eq!(node.node_id, "dev");
        assert_eq!(
            node.summary.as_deref(),
            Some("implemented the reporting seam")
        );
        assert_eq!(node.outputs.len(), 1);
        assert_eq!(node.outputs[0].key, "tests_added");
        assert_eq!(node.artifacts.len(), 1);
        assert_eq!(node.artifacts[0].reference, "src-tauri/src/x.rs");
    }

    /// AC-3: a step that edits two files and opens a PR reports a three-entry footprint.
    #[test]
    fn footprint_reports_two_files_and_a_pr() {
        let mut sr = step_result("run-1", "dev");
        sr.footprint = vec![
            touched(ArtifactKind::File, "a.rs", FootprintChange::Modified),
            touched(ArtifactKind::File, "b.rs", FootprintChange::Modified),
            touched(
                ArtifactKind::Pr,
                "https://example/pr/1",
                FootprintChange::Created,
            ),
        ];
        let records = vec![
            rec(0, RunEvent::Outcome(Box::new(outcome("run-1", None, None)))),
            rec(1, RunEvent::StepResult(sr)),
        ];
        let ev = build_run_evidence(&records, "SW-AU-82A", "run-1").expect("has evidence");
        assert_eq!(
            ev.per_node_results[0].footprint.len(),
            3,
            "two files + one PR"
        );
    }

    /// AC-1/AC-3: repeated reports for one node accumulate; the same entity touched twice (across two
    /// separate events) collapses to a single footprint entry, `Created` winning over `Modified`.
    #[test]
    fn footprint_dedups_by_kind_and_reference() {
        let mut first = step_result("run-1", "dev");
        first.footprint = vec![touched(
            ArtifactKind::File,
            "a.rs",
            FootprintChange::Modified,
        )];
        let mut second = step_result("run-1", "dev");
        second.footprint = vec![touched(
            ArtifactKind::File,
            "a.rs",
            FootprintChange::Created,
        )];
        let records = vec![
            rec(0, RunEvent::Outcome(Box::new(outcome("run-1", None, None)))),
            rec(1, RunEvent::StepResult(first)),
            rec(2, RunEvent::StepResult(second)),
        ];
        let ev = build_run_evidence(&records, "SW-AU-82A", "run-1").expect("has evidence");
        let footprint = &ev.per_node_results[0].footprint;
        assert_eq!(
            footprint.len(),
            1,
            "same (kind, ref) collapses to one entry"
        );
        assert_eq!(
            footprint[0].change,
            FootprintChange::Created,
            "Created wins"
        );
    }

    fn outcome_with_work_product(run_id: &str, branch: &str) -> RunOutcomeEvent {
        let mut o = outcome(run_id, None, None);
        o.work_product = Some(
            crate::commands::sprint_run::run_store::RunWorkProductProvenance {
                source_repo: crate::domain::SourceRepoSlug::new("alderco", "codesense")
                    .expect("valid slug"),
                branch: branch.into(),
                credential_ref: None,
            },
        );
        o
    }

    /// AC-2 (auto-capture): the run's work product surfaces even when the agent reported nothing and
    /// there is no identity spine — completeness never depends on agent goodwill. It lands on the
    /// shared default node as an honest branch `Entity` (not a fabricated PR URL).
    #[test]
    fn work_product_is_auto_captured_without_any_report() {
        let records = vec![rec(
            0,
            RunEvent::Outcome(Box::new(outcome_with_work_product("run-1", "feat/x"))),
        )];
        let ev = build_run_evidence(&records, "SW-AU-82A", "run-1").expect("has evidence");
        let node = ev
            .per_node_results
            .iter()
            .find(|n| n.node_id == DEFAULT_STEP_NODE_ID)
            .expect("work product lands on the default node with no spine");
        assert!(node
            .artifacts
            .iter()
            .any(|a| a.kind == ArtifactKind::Entity));
        assert!(node
            .footprint
            .iter()
            .any(|f| f.kind == ArtifactKind::Entity));
    }

    /// AC-2/AC-4: an agent report and the work-product auto-capture converge on ONE node (the shared
    /// default) rather than splitting into two rows for one agent step.
    #[test]
    fn agent_report_and_work_product_converge_on_one_node() {
        let mut sr = step_result("run-1", DEFAULT_STEP_NODE_ID);
        sr.summary = Some("did the work".into());
        let records = vec![
            rec(
                0,
                RunEvent::Outcome(Box::new(outcome_with_work_product("run-1", "feat/x"))),
            ),
            rec(1, RunEvent::StepResult(sr)),
        ];
        let ev = build_run_evidence(&records, "SW-AU-82A", "run-1").expect("has evidence");
        assert_eq!(ev.per_node_results.len(), 1, "one converged node");
        let node = &ev.per_node_results[0];
        assert_eq!(node.summary.as_deref(), Some("did the work"));
        assert!(
            node.artifacts
                .iter()
                .any(|a| a.kind == ArtifactKind::Entity),
            "work product folded onto the same node as the report"
        );
    }

    /// SW-AU-96C (AC-1): an AUTOMATION-DISPATCHED run — an outcome carrying a work product PLUS an
    /// `automation_id` + `trigger_source`, the shape `dispatch_one` records — seeds real `StoreRecord`s
    /// and surfaces BOTH the branch `Entity` (SW-AU-96) and the per-file `File` footprint (SW-AU-96B)
    /// through the shared `build_run_evidence` reader fold (`fold_reported_results` + `capture_work_product`,
    /// the one both the current-store and durable read paths delegate to). A launch-spine refactor that
    /// dropped work-product capture from the dispatch path — or a reader change that hid it for automation
    /// runs — fails here rather than shipping a blank footprint. The contrast tail proves the branch
    /// `Entity` is sourced from the work product (a run without one surfaces none), so the test is not
    /// vacuous.
    #[test]
    fn dispatched_run_surfaces_branch_entity_and_per_file_footprint() {
        use crate::commands::sprint_run::run_store::{
            ArtifactKind, FootprintChange, FootprintEntry,
        };

        // The dispatched shape: `automation_id` (from `outcome`) + an attributed trigger + a work product.
        let mut dispatched = outcome_with_work_product("run-96c", "feat/SW-AU-96C");
        dispatched.trigger_source = Some("watch:sprint".into());

        let file = |reference: &str, change| FootprintEntry {
            kind: ArtifactKind::File,
            reference: reference.into(),
            change,
        };
        // The per-file entries SW-AU-96B's completion capture appends on the shared default node
        // (reuse the shipped `step_result` builder — no second spelling of the event struct).
        let mut per_file = step_result("run-96c", DEFAULT_STEP_NODE_ID);
        per_file.footprint = vec![
            file("src/a.rs", FootprintChange::Modified),
            file("src/b.rs", FootprintChange::Created),
        ];
        let records = vec![
            rec(0, RunEvent::Outcome(Box::new(dispatched))),
            rec(1, RunEvent::StepResult(per_file)),
        ];

        let ev = build_run_evidence(&records, "SW-AU-82A", "run-96c")
            .expect("dispatched run has evidence");
        assert_eq!(
            ev.header.trigger,
            TriggerFact::Attributed {
                source: "watch:sprint".into()
            },
            "the run is the dispatched/automation shape, not an interactive open"
        );
        let node = ev
            .per_node_results
            .iter()
            .find(|n| n.node_id == DEFAULT_STEP_NODE_ID)
            .expect("work product + footprint land on the shared default node");
        // (a) the branch Entity (SW-AU-96)
        assert!(
            node.footprint
                .iter()
                .any(|f| f.kind == ArtifactKind::Entity),
            "dispatched run carries the branch Entity footprint"
        );
        assert!(
            node.artifacts
                .iter()
                .any(|a| a.kind == ArtifactKind::Entity),
            "dispatched run carries the branch Entity artifact"
        );
        // (b) the per-file File footprint (SW-AU-96B)
        let files: Vec<_> = node
            .footprint
            .iter()
            .filter(|f| f.kind == ArtifactKind::File)
            .collect();
        assert!(
            files
                .iter()
                .any(|f| f.reference == "src/a.rs" && f.change == FootprintChange::Modified),
            "the modified file surfaces on a dispatched run"
        );
        assert!(
            files
                .iter()
                .any(|f| f.reference == "src/b.rs" && f.change == FootprintChange::Created),
            "the created file surfaces on a dispatched run"
        );
        assert_eq!(
            files.len(),
            2,
            "exactly the two reported files — no duplicated or spurious footprint entries"
        );
        // The dispatched identity itself survives onto the run header (not merely the borrowed default
        // in `outcome` — assert it so a dropped `automation_id` propagation can't pass as "dispatched").
        assert_eq!(
            ev.header.automation_id.as_deref(),
            Some("todo->done"),
            "the run's automation_id surfaces on the evidence header"
        );

        // Contrast (non-vacuity + honest degrade): the SAME dispatched shape WITHOUT a work product
        // surfaces the `File` footprint but NO branch `Entity` — proving the `Entity` is sourced from the
        // work product, and that a run whose branch does not resolve degrades to no `Entity` rather than a
        // fabricated one.
        let no_wp = outcome("run-96c", Some("watch:sprint"), None); // work_product: None
        let mut per_file_only = step_result("run-96c", DEFAULT_STEP_NODE_ID);
        per_file_only.footprint = vec![file("src/a.rs", FootprintChange::Modified)];
        let ev_no_wp = build_run_evidence(
            &[
                rec(0, RunEvent::Outcome(Box::new(no_wp))),
                rec(1, RunEvent::StepResult(per_file_only)),
            ],
            "SW-AU-82A",
            "run-96c",
        )
        .expect("run without a work product still has evidence");
        let node_no_wp = ev_no_wp
            .per_node_results
            .iter()
            .find(|n| n.node_id == DEFAULT_STEP_NODE_ID)
            .expect("the File footprint lands regardless of the work product");
        assert!(
            node_no_wp
                .footprint
                .iter()
                .any(|f| f.kind == ArtifactKind::File),
            "the File footprint is independent of the work product"
        );
        assert!(
            !node_no_wp
                .footprint
                .iter()
                .any(|f| f.kind == ArtifactKind::Entity),
            "no work product ⇒ no branch Entity (the Entity is sourced from the work product)"
        );
    }

    /// SW-AU-96C (AC-2): the work-product → footprint fold is origin-blind. A dispatched-shape outcome
    /// (`automation_id` + `trigger_source`) and an interactive-shape one (neither) that carry the SAME
    /// work product yield the SAME branch `Entity` footprint. The launch spine (`launch_story_session`)
    /// fills `work_product` for ANY `Some(provenance)` on a resolvable branch, so the reader must never
    /// re-gate that capture on how the run was launched — a regression that hid automation-run work
    /// products would diverge the two counts here. (The upstream inline fill in `launch_story_session`
    /// is not unit-lockable without a production extract or a live worktree; this pins the reader half
    /// of the contract.)
    #[test]
    fn work_product_footprint_is_not_gated_on_run_origin() {
        use crate::commands::sprint_run::run_store::ArtifactKind;

        let entity_footprints = |dispatched: bool| {
            let mut o = outcome_with_work_product("run-x", "feat/x");
            if dispatched {
                // `automation_id` is already `Some` from `outcome`; add the trigger source.
                o.trigger_source = Some("watch:sprint".into());
            } else {
                o.automation_id = None;
                o.trigger_source = None;
            }
            let ev = build_run_evidence(
                &[rec(0, RunEvent::Outcome(Box::new(o)))],
                "SW-AU-82A",
                "run-x",
            )
            .expect("has evidence");
            ev.per_node_results
                .iter()
                .find(|n| n.node_id == DEFAULT_STEP_NODE_ID)
                .expect("work product node")
                .footprint
                .iter()
                .filter(|f| f.kind == ArtifactKind::Entity)
                .count()
        };

        assert_eq!(
            entity_footprints(true),
            entity_footprints(false),
            "work-product capture is origin-blind (dispatched ≡ interactive)"
        );
        assert!(
            entity_footprints(true) >= 1,
            "the dispatched run captures the branch Entity"
        );
    }

    /// A later empty/whitespace summary never clobbers an earlier real one.
    #[test]
    fn empty_summary_does_not_clobber_a_real_one() {
        let mut first = step_result("run-1", "dev");
        first.summary = Some("real summary".into());
        let mut second = step_result("run-1", "dev");
        second.summary = Some("   ".into());
        let records = vec![
            rec(0, RunEvent::Outcome(Box::new(outcome("run-1", None, None)))),
            rec(1, RunEvent::StepResult(first)),
            rec(2, RunEvent::StepResult(second)),
        ];
        let ev = build_run_evidence(&records, "SW-AU-82A", "run-1").expect("has evidence");
        assert_eq!(
            ev.per_node_results[0].summary.as_deref(),
            Some("real summary")
        );
    }

    /// A `None`-label upsert (e.g. auto-capture) never wipes an agent-provided label for the same ref.
    #[test]
    fn label_is_preserved_across_upserts() {
        let mut labeled = artifact(ArtifactKind::File, "a.rs");
        labeled.label = Some("the source".into());
        let mut with_label = step_result("run-1", "dev");
        with_label.artifacts = vec![labeled];
        let mut without_label = step_result("run-1", "dev");
        without_label.artifacts = vec![artifact(ArtifactKind::File, "a.rs")]; // label None
        let records = vec![
            rec(0, RunEvent::Outcome(Box::new(outcome("run-1", None, None)))),
            rec(1, RunEvent::StepResult(with_label)),
            rec(2, RunEvent::StepResult(without_label)),
        ];
        let ev = build_run_evidence(&records, "SW-AU-82A", "run-1").expect("has evidence");
        let artifacts = &ev.per_node_results[0].artifacts;
        assert_eq!(artifacts.len(), 1, "same ref deduped");
        assert_eq!(
            artifacts[0].label.as_deref(),
            Some("the source"),
            "a None label never wipes a real one"
        );
    }
}
