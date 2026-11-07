// SPDX-License-Identifier: MIT
//! Agent runs, as the run-detail pane shows them: `list_runs` (the latest outcome of
//! every run, newest first), `read_run` (one run's outcome, structured transcript
//! and captured output), `read_run_evidence` (its audit header, step/attempt spine and
//! reported per-step results) and `read_run_work_product` (the branch it produced). A
//! dashboard's `runs()` / `run({runId})` / `runEvidence({runId, storyId?})` /
//! `runWorkProduct({runId})` return these same shapes, from these same functions.
//! `list_run_events` additionally reads outcomes and advances in a record-time window.

use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::sprint_run::run_artifact::{read_run_artifact, tail_bounded};
use crate::commands::sprint_run::run_evidence::{self, RunEvidence};
use crate::commands::sprint_run::run_store::{
    list_run_outcomes, read_default_records, read_run_outcome, run_work_product, RunEvent,
    RunOutcomeRead, RunWorkProductRead, StoreRecord, Trigger,
};
use crate::commands::sprint_run::run_transcript::{read_run_transcript, SessionTranscript};

/// Default number of rows for run and run-event reads.
pub(crate) const DEFAULT_RUNS_LIMIT: u32 = 50;
/// Maximum number of rows for run and run-event reads.
pub(crate) const MAX_RUNS_LIMIT: u32 = 500;
/// How much of a run's captured output `read_run` returns when the caller does not
/// say: its last 64 KiB.
pub(crate) const DEFAULT_ARTIFACT_BYTES: u32 = 64 * 1024;
/// The most captured output one `read_run` call returns (what a run keeps: 1 MiB).
pub(crate) const MAX_ARTIFACT_BYTES: u32 = 1024 * 1024;

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ListRunsParams {
    /// At most this many runs, newest first (default 50, max 500).
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunEventKind {
    Outcome,
    Advance,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListRunEventsParams {
    /// Inclusive start of the window, Unix epoch milliseconds (record time).
    #[serde(alias = "sinceMs")]
    pub since_ms: u64,
    /// Inclusive end of the window, Unix epoch milliseconds. Omit for no upper bound.
    #[serde(default, alias = "untilMs")]
    pub until_ms: Option<u64>,
    /// Omit for both outcomes and advances; an empty list returns no events.
    #[serde(default)]
    pub kinds: Option<Vec<RunEventKind>>,
    /// At most this many events, newest first (default 50, max 500).
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum RunEventRead {
    Outcome {
        ts: u128,
        story_id: String,
        run_id: String,
        status: crate::dashboard_stream::SessionStatus,
        summary: Option<String>,
        ended_at: String,
    },
    Advance {
        ts: u128,
        story_id: String,
        from: Option<String>,
        to: String,
        trigger: Trigger,
        by: Option<String>,
        run_id: Option<String>,
    },
}

fn events_from_records(records: &[StoreRecord], params: &ListRunEventsParams) -> Vec<RunEventRead> {
    let mut records: Vec<_> = records
        .iter()
        .filter(|r| {
            r.ts >= u128::from(params.since_ms)
                && params.until_ms.is_none_or(|end| r.ts <= u128::from(end))
        })
        .filter(|r| {
            let kind = match r.event {
                RunEvent::Outcome(_) => RunEventKind::Outcome,
                RunEvent::Advance(_) => RunEventKind::Advance,
                _ => return false,
            };
            params
                .kinds
                .as_ref()
                .is_none_or(|kinds| kinds.contains(&kind))
        })
        .collect();
    records.sort_by_key(|r| std::cmp::Reverse((r.ts, r.seq)));
    records.truncate(run_read_limit(params.limit));
    records
        .into_iter()
        .filter_map(|r| match &r.event {
            RunEvent::Outcome(o) => Some(RunEventRead::Outcome {
                ts: r.ts,
                story_id: o.story_id.clone(),
                run_id: o.run_id.clone(),
                status: o.status,
                summary: o.summary.clone(),
                ended_at: o.ended_at.clone(),
            }),
            RunEvent::Advance(a) => Some(RunEventRead::Advance {
                ts: r.ts,
                story_id: a.story_id.clone(),
                from: a.from.clone(),
                to: a.to.clone(),
                trigger: a.trigger,
                by: a.by.clone(),
                run_id: a.run_id.clone(),
            }),
            _ => None,
        })
        .collect()
}

pub(crate) async fn list_run_events(
    params: ListRunEventsParams,
) -> Result<Vec<RunEventRead>, String> {
    crate::commands::await_blocking("list_run_events", move || {
        let (records, _) = read_default_records();
        Ok::<_, String>(events_from_records(&records, &params))
    })
    .await
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReadRunParams {
    /// The run's id: a `list_runs` row's `outcome.run_id`.
    #[serde(alias = "run_id")]
    pub run_id: String,
    /// Return at most the last this-many bytes of its captured output (default
    /// 65536, max 1048576).
    #[serde(default, alias = "max_artifact_bytes")]
    pub max_artifact_bytes: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReadRunEvidenceParams {
    /// The run's id: a `list_runs` row's `outcome.run_id`.
    #[serde(alias = "run_id")]
    pub run_id: String,
    /// The story the run worked (`outcome.story_id`). Omit it to use the story the
    /// run's latest outcome names.
    #[serde(default, alias = "story_id")]
    pub story_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReadRunWorkProductParams {
    /// The run's id: a `list_runs` row's `outcome.run_id`.
    #[serde(alias = "run_id")]
    pub run_id: String,
}

/// One run in full (`read_run`, and a dashboard's `run({runId})`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RunRead {
    /// Its latest recorded outcome (a `list_runs` row), or null while it is still
    /// running or when it recorded none.
    pub outcome: Option<RunOutcomeRead>,
    /// Its structured transcript (`{turns: [...]}`), or null when its agent CLI
    /// gives none.
    pub transcript: Option<SessionTranscript>,
    /// The tail of its captured terminal output (redacted), or null when it left
    /// none.
    pub artifact: Option<String>,
    /// The output was longer than returned: `artifact` is its last part.
    pub artifact_truncated: bool,
}

/// The newest `limit` of `runs` (already newest first).
pub(crate) fn newest(mut runs: Vec<RunOutcomeRead>, limit: Option<u32>) -> Vec<RunOutcomeRead> {
    runs.truncate(run_read_limit(limit));
    runs
}

fn run_read_limit(limit: Option<u32>) -> usize {
    limit
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_RUNS_LIMIT)
        .min(MAX_RUNS_LIMIT) as usize
}

/// The last `max_bytes` of `text` (default [`DEFAULT_ARTIFACT_BYTES`]), and whether
/// anything was cut.
pub(crate) fn bound_artifact(text: String, max_bytes: Option<u32>) -> (String, bool) {
    let budget = max_bytes
        .filter(|b| *b > 0)
        .unwrap_or(DEFAULT_ARTIFACT_BYTES)
        .min(MAX_ARTIFACT_BYTES) as usize;
    if text.len() <= budget {
        return (text, false);
    }
    (tail_bounded(text.as_bytes(), budget), true)
}

/// The latest outcome of every run, newest first (`list_runs`).
pub(crate) async fn list_runs(params: ListRunsParams) -> Result<Vec<RunOutcomeRead>, String> {
    Ok(newest(list_run_outcomes().await?, params.limit))
}

/// One run's outcome, transcript and bounded output (`read_run`).
pub(crate) async fn read_run(params: ReadRunParams) -> Result<RunRead, String> {
    let run_id = required_run_id(&params.run_id)?;
    let (outcome, transcript, artifact) = tokio::try_join!(
        read_run_outcome(run_id.clone()),
        read_run_transcript(run_id.clone()),
        read_run_artifact(run_id),
    )?;
    let (artifact, artifact_truncated) = match artifact {
        Some(text) => {
            let (text, cut) = bound_artifact(text, params.max_artifact_bytes);
            (Some(text), cut)
        }
        None => (None, false),
    };
    Ok(RunRead {
        outcome,
        transcript,
        artifact,
        artifact_truncated,
    })
}

/// A required run id, trimmed.
fn required_run_id(run_id: &str) -> Result<String, String> {
    let run_id = run_id.trim();
    if run_id.is_empty() {
        return Err("runId is required".into());
    }
    Ok(run_id.to_string())
}

/// The story to scope a run's evidence to: the caller's (when non-blank), else the one
/// its latest outcome names (when non-blank), else none.
pub(crate) fn evidence_story_id(
    given: Option<String>,
    outcome: Option<&RunOutcomeRead>,
) -> Option<String> {
    given
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            outcome
                .map(|o| o.outcome.story_id.trim().to_string())
                .filter(|s| !s.is_empty())
        })
}

/// One run's evidence as the run-evidence view opens it (`read_run_evidence`), or
/// `None` when it has none: no recorded outcome yet, or no story to scope it to.
pub(crate) async fn read_run_evidence(
    params: ReadRunEvidenceParams,
) -> Result<Option<RunEvidence>, String> {
    let run_id = required_run_id(&params.run_id)?;
    let given = params.story_id.filter(|s| !s.trim().is_empty());
    let outcome = match given {
        Some(_) => None,
        None => read_run_outcome(run_id.clone()).await?,
    };
    let Some(story_id) = evidence_story_id(given, outcome.as_ref()) else {
        return Ok(None);
    };
    run_evidence::read_run_evidence(story_id, run_id).await
}

/// The branch a run produced (`read_run_work_product`), or `None` when it recorded none.
pub(crate) async fn read_run_work_product(
    params: ReadRunWorkProductParams,
) -> Result<Option<RunWorkProductRead>, String> {
    run_work_product(required_run_id(&params.run_id)?).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::sprint_run::run_store::{work_product_from_records, RunOutcomeEvent};
    use serde_json::json;

    fn outcome(run_id: &str) -> RunOutcomeRead {
        serde_json::from_value(json!({
            "outcome": {
                "story_id": "SW-1",
                "run_id": run_id,
                "status": "success",
                "ended_at": "2026-10-04T10:00:00Z",
                "source": "signal"
            },
            "validated": "completed"
        }))
        .expect("a run outcome")
    }

    #[test]
    fn list_runs_keeps_the_newest_and_caps_the_limit() {
        let runs: Vec<_> = (0..600).map(|i| outcome(&format!("r{i}"))).collect();
        assert_eq!(newest(runs.clone(), None).len(), 50);
        assert_eq!(newest(runs.clone(), Some(0)).len(), 50);
        let three = newest(runs.clone(), Some(3));
        assert_eq!(
            three
                .iter()
                .map(|r| r.outcome.run_id.as_str())
                .collect::<Vec<_>>(),
            ["r0", "r1", "r2"]
        );
        assert_eq!(newest(runs, Some(10_000)).len(), 500);
    }

    #[test]
    fn read_run_returns_the_tail_of_long_output() {
        let (short, cut) = bound_artifact("all of it".into(), None);
        assert_eq!((short.as_str(), cut), ("all of it", false));
        let long = format!("{}END", "x".repeat(200));
        let (tail, cut) = bound_artifact(long, Some(32));
        assert!(cut);
        assert!(tail.ends_with("END"));
        assert!(tail.len() <= 32 + 4, "bounded: {} bytes", tail.len());
        let huge = "y".repeat(2 * 1024 * 1024);
        let (tail, cut) = bound_artifact(huge, Some(u32::MAX));
        assert!(cut);
        assert!(tail.len() <= MAX_ARTIFACT_BYTES as usize + 4);
    }

    #[test]
    fn a_run_reads_as_its_wire_shape() {
        let read = RunRead {
            outcome: Some(outcome("r1")),
            transcript: None,
            artifact: Some("done".into()),
            artifact_truncated: false,
        };
        let wire = serde_json::to_value(&read).unwrap();
        assert_eq!(wire["outcome"]["outcome"]["run_id"], "r1");
        assert_eq!(wire["outcome"]["validated"], "completed");
        assert_eq!(wire["transcript"], serde_json::Value::Null);
        assert_eq!(wire["artifact"], "done");
        assert_eq!(wire["artifactTruncated"], false);
    }

    #[test]
    fn the_agent_arguments_parse_in_either_spelling() {
        let p: ReadRunParams = serde_json::from_value(json!({"runId": "r1"})).unwrap();
        assert_eq!(p.run_id, "r1");
        let p: ReadRunParams =
            serde_json::from_value(json!({"run_id": "r1", "max_artifact_bytes": 10})).unwrap();
        assert_eq!(p.max_artifact_bytes, Some(10));
        let p: ListRunsParams = serde_json::from_value(json!({})).unwrap();
        assert_eq!(p.limit, None);
    }

    fn outcome_record(seq: u64, outcome: serde_json::Value) -> StoreRecord {
        StoreRecord {
            schema_version: 1,
            seq,
            ts: u128::from(seq),
            event: RunEvent::Outcome(Box::new(
                serde_json::from_value::<RunOutcomeEvent>(outcome).expect("an outcome event"),
            )),
        }
    }

    fn review_records() -> Vec<StoreRecord> {
        vec![
            outcome_record(
                10,
                json!({"story_id":"S-1", "run_id":"r1", "status":"success",
                "summary":"Shipped", "ended_at":"2026-10-20T10:00:00Z", "source":"signal"}),
            ),
            serde_json::from_value(json!({"schema_version":1,"seq":2,"ts":20,"kind":"advance",
                "story_id":"S-1","from":"todo","to":"done","trigger":"human",
                "by":"human:sam","run_id":"r1"}))
            .unwrap(),
            outcome_record(
                30,
                json!({"story_id":"S-2", "run_id":"r2", "status":"failed",
                "ended_at":"2026-10-20T09:00:00Z", "source":"signal"}),
            ),
        ]
    }

    #[test]
    fn run_events_filter_record_time_inclusively_and_return_newest_first() {
        let params: ListRunEventsParams =
            serde_json::from_value(json!({"since_ms":10,"until_ms":20})).unwrap();
        let rows = events_from_records(&review_records(), &params);
        let wire = serde_json::to_value(rows).unwrap();
        assert_eq!(wire.as_array().unwrap().len(), 2);
        assert_eq!(
            wire[0],
            json!({"kind":"advance","ts":20,"story_id":"S-1","from":"todo",
            "to":"done","trigger":"human","by":"human:sam","run_id":"r1"})
        );
        assert_eq!(
            wire[1],
            json!({"kind":"outcome","ts":10,"story_id":"S-1","run_id":"r1",
            "status":"success","summary":"Shipped","ended_at":"2026-10-20T10:00:00Z"})
        );
    }

    #[test]
    fn run_events_filter_kinds_and_limit_after_filtering() {
        let params: ListRunEventsParams =
            serde_json::from_value(json!({"since_ms":0,"kinds":["outcome"],"limit":1})).unwrap();
        let wire = serde_json::to_value(events_from_records(&review_records(), &params)).unwrap();
        assert_eq!(wire.as_array().unwrap().len(), 1);
        assert_eq!(wire[0]["run_id"], "r2");
        let params: ListRunEventsParams =
            serde_json::from_value(json!({"since_ms":0,"kinds":["advance"]})).unwrap();
        assert_eq!(events_from_records(&review_records(), &params).len(), 1);
        let params: ListRunEventsParams =
            serde_json::from_value(json!({"since_ms":0,"kinds":[]})).unwrap();
        assert!(events_from_records(&review_records(), &params).is_empty());
    }

    #[test]
    fn run_events_empty_store_and_bounded_limit() {
        let params: ListRunEventsParams =
            serde_json::from_value(json!({"sinceMs":0,"limit":10000})).unwrap();
        assert!(events_from_records(&[], &params).is_empty());
        let records = vec![review_records()[0].clone(); 600];
        assert_eq!(events_from_records(&records, &params).len(), 500);
        let params: ListRunEventsParams = serde_json::from_value(json!({"since_ms":0})).unwrap();
        assert_eq!(events_from_records(&records, &params).len(), 50);
    }

    #[test]
    fn evidence_is_scoped_to_the_given_story_else_the_runs_own() {
        let row = outcome("r1");
        assert_eq!(
            evidence_story_id(Some(" SW-9 ".into()), Some(&row)).as_deref(),
            Some("SW-9")
        );
        assert_eq!(
            evidence_story_id(Some("  ".into()), Some(&row)).as_deref(),
            Some("SW-1")
        );
        assert_eq!(evidence_story_id(None, Some(&row)).as_deref(), Some("SW-1"));
        assert_eq!(evidence_story_id(None, None), None);
        let mut storyless = outcome("r2");
        storyless.outcome.story_id = String::new();
        assert_eq!(evidence_story_id(None, Some(&storyless)), None);
    }

    /// `read_run_evidence` (and a dashboard's `runEvidence`) return the run-evidence
    /// view's own DTO: these are its wire keys.
    #[test]
    fn run_evidence_reads_as_its_wire_shape() {
        let records = vec![outcome_record(
            0,
            json!({
                "story_id": "SW-1",
                "run_id": "r1",
                "automation_id": "todo->done",
                "trigger_source": "manual",
                "status": "needs-attention",
                "summary": "blocked on review",
                "ended_at": "2026-10-04T10:00:00Z",
                "source": "signal",
                "work_product": {
                    "source_repo": {"owner": "acme", "repo": "api"},
                    "branch": "feat/x"
                }
            }),
        )];
        let evidence = run_evidence::build_run_evidence(&records, "SW-1", "r1").unwrap();
        let wire = serde_json::to_value(&evidence).unwrap();
        assert_eq!(wire["header"]["runId"], "r1");
        assert_eq!(wire["header"]["storyId"], "SW-1");
        assert_eq!(wire["header"]["automationId"], "todo->done");
        assert_eq!(
            wire["header"]["trigger"],
            json!({"kind": "attributed", "source": "manual"})
        );
        assert_eq!(wire["header"]["status"], "needs-attention");
        assert_eq!(wire["header"]["endedAt"], "2026-10-04T10:00:00Z");
        assert_eq!(wire["run"]["runId"], "r1");
        assert_eq!(wire["run"]["correlationId"], "r1");
        assert!(wire["run"]["steps"].is_array());
        assert_eq!(wire["authority"], "currentStoreFallback");
        let node = &wire["perNodeResults"][0];
        assert_eq!(node["nodeId"], "agent");
        assert_eq!(node["artifacts"][0]["kind"], "entity");
        assert_eq!(node["footprint"][0]["change"], "created");
        assert!(run_evidence::build_run_evidence(&records, "SW-2", "r1").is_none());
    }

    /// `read_run_work_product` (and a dashboard's `runWorkProduct`) return `{branch}`
    /// from the run's latest outcome, or null when it recorded none.
    #[test]
    fn run_work_product_reads_as_its_wire_shape() {
        let base = json!({
            "story_id": "SW-1",
            "run_id": "r1",
            "status": "success",
            "ended_at": "2026-10-04T10:00:00Z",
            "source": "signal"
        });
        let mut with = base.clone();
        with["work_product"] = json!({
            "source_repo": {"owner": "acme", "repo": "api"},
            "branch": " feat/x "
        });
        let records = vec![outcome_record(0, with)];
        let read = work_product_from_records(&records, "r1").unwrap();
        assert_eq!(
            serde_json::to_value(&read).unwrap(),
            json!({"branch": "feat/x"})
        );
        assert_eq!(work_product_from_records(&records, "other"), None);
        let none = vec![outcome_record(0, base)];
        assert_eq!(work_product_from_records(&none, "r1"), None);
    }

    #[tokio::test]
    async fn evidence_and_work_product_need_a_run_id() {
        let err = read_run_evidence(ReadRunEvidenceParams {
            run_id: " ".into(),
            story_id: Some("SW-1".into()),
        })
        .await
        .unwrap_err();
        assert_eq!(err, "runId is required");
        let err = read_run_work_product(ReadRunWorkProductParams { run_id: "".into() })
            .await
            .unwrap_err();
        assert_eq!(err, "runId is required");
        let p: ReadRunEvidenceParams =
            serde_json::from_value(json!({"run_id": "r1", "story_id": "SW-1"})).unwrap();
        assert_eq!(
            (p.run_id.as_str(), p.story_id.as_deref()),
            ("r1", Some("SW-1"))
        );
        let p: ReadRunEvidenceParams = serde_json::from_value(json!({"runId": "r1"})).unwrap();
        assert_eq!(p.story_id, None);
        let p: ReadRunWorkProductParams = serde_json::from_value(json!({"runId": "r1"})).unwrap();
        assert_eq!(p.run_id, "r1");
    }
}
