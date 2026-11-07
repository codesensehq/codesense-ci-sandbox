// SPDX-License-Identifier: MIT
//! Private structured results. Admission pins authority and schema; completion alone promotes.
use super::{
    runs::{self, RoutineRun},
    service,
    state::RoutineStateStore,
    RoutineKey,
};
use crate::coordinator::{StoreError, StoreResult};
use crate::domain::Principal;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;
use std::path::Path;

fn bounded(value: &Value, bytes: usize, depth: usize, nodes: usize) -> Result<(), String> {
    let mut pending = vec![(value, 0)];
    let mut count = 0;
    while let Some((value, level)) = pending.pop() {
        count += 1;
        if level > depth || count > nodes {
            return Err("Structured result exceeds its depth or node limit".into());
        }
        match value {
            Value::Array(values) => {
                if values.len() > nodes.saturating_sub(count + pending.len()) {
                    return Err("Structured result exceeds its node limit".into());
                }
                pending.extend(values.iter().map(|v| (v, level + 1)));
            }
            Value::Object(values) => {
                if values.len() > nodes.saturating_sub(count + pending.len()) {
                    return Err("Structured result exceeds its node limit".into());
                }
                pending.extend(values.values().map(|v| (v, level + 1)));
            }
            _ => {}
        }
    }
    // Serialize only after structural bounds pass, without allocating a full JSON string.
    struct ByteBudget(usize);
    impl std::io::Write for ByteBudget {
        fn write(&mut self, chunk: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_sub(chunk.len())
                .ok_or_else(|| std::io::Error::other("byte limit"))?;
            Ok(chunk.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(ByteBudget(bytes), value)
        .map_err(|_| "Structured result exceeds its byte limit".to_string())
}
fn validator(schema: &Value) -> Result<jsonschema::Validator, String> {
    bounded(schema, 32 * 1024, 32, 2048)?;
    let mut pending = vec![(schema, 0)];
    while let Some((value, combinators)) = pending.pop() {
        if combinators > 4 {
            return Err(
                "Result schema combinator nesting exceeds 4; simplify the inline shape".into(),
            );
        }
        let Some(fields) = value.as_object() else {
            continue;
        };
        for (keyword, value) in fields {
            match keyword.as_str() {
                "$ref" | "$dynamicRef" | "$recursiveRef" => {
                    return Err(
                        "Result schema references are unsupported; inline the result shape".into(),
                    )
                }
                "properties" | "patternProperties" | "$defs" | "definitions"
                | "dependentSchemas" => {
                    if let Some(schemas) = value.as_object() {
                        pending.extend(schemas.values().map(|v| (v, combinators)));
                    }
                }
                "dependencies" => {
                    if let Some(schemas) = value.as_object() {
                        pending.extend(
                            schemas
                                .values()
                                .filter(|v| v.is_object() || v.is_boolean())
                                .map(|v| (v, combinators)),
                        );
                    }
                }
                "allOf" | "anyOf" | "oneOf" => {
                    if let Some(schemas) = value.as_array() {
                        pending.extend(schemas.iter().map(|v| (v, combinators + 1)));
                    }
                }
                "not" | "if" | "then" | "else" => pending.push((value, combinators + 1)),
                "items" | "prefixItems" => {
                    if let Some(schemas) = value.as_array() {
                        pending.extend(schemas.iter().map(|v| (v, combinators)));
                    } else {
                        pending.push((value, combinators));
                    }
                }
                "additionalProperties"
                | "unevaluatedProperties"
                | "additionalItems"
                | "unevaluatedItems"
                | "contains"
                | "propertyNames"
                | "contentSchema" => pending.push((value, combinators)),
                _ => {} // const, enum, examples, defaults and extension values are data, not schemas.
            }
        }
    }
    jsonschema::options()
        .offline()
        .with_pattern_options(
            jsonschema::PatternOptions::regex()
                .size_limit(1 << 20)
                .dfa_size_limit(1 << 20),
        )
        .build(schema)
        .map_err(|_| "Invalid inline result JSON Schema".into())
}
pub fn validate_schema(schema: &Value) -> Result<(), String> {
    validator(schema).map(|_| ())
}
pub fn validate_payload(schema: &Value, payload: &Value) -> Result<(), String> {
    bounded(payload, 1024 * 1024, 64, 50_000)?;
    if !validator(schema)?.is_valid(payload) {
        return Err("Result payload does not satisfy the admitted schema".into());
    }
    Ok(())
}

/// Session provenance, never a caller-supplied run id, selects the running candidate.
pub fn publish(
    home: &Path,
    principal: &Principal,
    session: &str,
    payload: Value,
) -> Result<(), String> {
    let store = RoutineStateStore::open(home).map_err(|e| e.to_string())?;
    let authority = store
        .read_transaction(|tx| read_authority(tx, principal, session))
        .map_err(|e| e.to_string())?;
    let key = RoutineKey::parse(&authority.key).ok_or("Invalid routine key")?;
    service::get_current(home, principal, &key).map_err(|e| e.to_string())?;
    let schema: Value =
        serde_json::from_str(&authority.schema).map_err(|_| "Unreadable admitted result schema")?;
    let candidate = validate_payload(&schema, &payload).map(|()| payload.to_string());
    commit_candidate(&store, principal, session, &authority, candidate)
}
#[derive(Debug, PartialEq)]
struct PublicationAuthority {
    run: String,
    key: String,
    schema: String,
}
fn read_authority(
    tx: &Connection,
    principal: &Principal,
    session: &str,
) -> StoreResult<PublicationAuthority> {
    tx.query_row("SELECT r.run_id, r.routine_key, a.schema_json FROM routine_runs r JOIN routine_result_runs a ON a.run_id=r.run_id WHERE r.session_id=?1 AND r.phase='running' AND a.principal=?2",
        params![session, principal.as_str()], |r| Ok(PublicationAuthority { run:r.get(0)?, key:r.get(1)?, schema:r.get(2)? })).optional()?
        .ok_or_else(|| StoreError::Effect("No authorized running result routine for this session".into()))
}
fn commit_candidate(
    store: &RoutineStateStore,
    principal: &Principal,
    session: &str,
    authority: &PublicationAuthority,
    candidate: Result<String, String>,
) -> Result<(), String> {
    store
        .transaction(|tx| {
            if read_authority(tx, principal, session)? != *authority {
                return Err(StoreError::Effect(
                    "Result publication authority changed".into(),
                ));
            }
            tx.execute(
                "UPDATE routine_result_runs SET candidate=?2 WHERE run_id=?1",
                params![authority.run, candidate.as_ref().ok()],
            )?;
            Ok(candidate.map(|_| ()))
        })
        .map_err(|e| e.to_string())?
}

pub(super) fn missing_candidate(tx: &Connection, run: &str) -> StoreResult<bool> {
    Ok(tx
        .query_row(
            "SELECT candidate IS NULL FROM routine_result_runs WHERE run_id=?1",
            params![run],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(false))
}
pub(super) fn complete(tx: &Connection, run: &str, success: bool, now: i64) -> StoreResult<()> {
    if success {
        tx.execute("INSERT INTO routine_results (principal, routine_key, run_id, revision, schema_json, payload, succeeded_at)
            SELECT a.principal, r.routine_key, r.run_id, r.revision, a.schema_json, a.candidate, ?2
            FROM routine_runs r JOIN routine_result_runs a ON a.run_id=r.run_id WHERE r.run_id=?1 AND a.candidate IS NOT NULL
            ON CONFLICT(principal, routine_key) DO UPDATE SET run_id=excluded.run_id, revision=excluded.revision, schema_json=excluded.schema_json, payload=excluded.payload, succeeded_at=excluded.succeeded_at",
            params![run, now])?;
    }
    tx.execute(
        "UPDATE routine_result_runs SET candidate=NULL WHERE run_id=?1",
        params![run],
    )?;
    Ok(())
}
#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub run_id: String,
    pub revision: u32,
    pub succeeded_at: i64,
    pub payload: Value,
}
#[derive(Debug, Serialize)]
pub struct ResultStatus {
    pub routine_key: String,
    pub revision: u32,
    pub schema_compatible: bool,
    pub snapshot: Option<Snapshot>,
    pub latest_attempt: Option<RoutineRun>,
    pub active_attempt: Option<RoutineRun>,
}
/// Cached read only: this path never admits, launches, or invokes an agent.
pub fn read(home: &Path, principal: &Principal, key: &RoutineKey) -> Result<ResultStatus, String> {
    read_for(home, principal, key, &Default::default())
}

pub(crate) fn read_for(
    home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<ResultStatus, String> {
    let current = service::get_current_for(home, principal, key, scope).map_err(|e| {
        if matches!(scope, crate::context_repo::caller_scope::CallerScope::Human) {
            e.to_string()
        } else {
            crate::context_repo::caller_scope::MISSING.into()
        }
    })?;
    let store = RoutineStateStore::open(home).map_err(|e| e.to_string())?;
    store.read_transaction(|tx| {
        let cached: Option<(String, u32, i64, String, String)> = tx.query_row(
            "SELECT run_id, revision, succeeded_at, payload, schema_json FROM routine_results WHERE principal=?1 AND routine_key=?2",
            params![principal.as_str(), key.to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).optional()?;
        let mut compatible = false;
        let snapshot = cached.map(|(run_id, revision, succeeded_at, payload, schema)| {
            let stored_schema: Value = serde_json::from_str(&schema).map_err(|_| StoreError::Effect("Unreadable result schema".into()))?;
            compatible = current.def.result_schema.as_ref() == Some(&stored_schema);
            serde_json::from_str(&payload).map(|payload| Snapshot { run_id, revision, succeeded_at, payload })
                .map_err(|_| StoreError::Effect("Unreadable private result".into()))
        }).transpose()?;
        let latest_attempt = tx.query_row(&format!("SELECT {} FROM routine_runs WHERE routine_key=?1 AND run_id IN (SELECT run_id FROM routine_result_runs WHERE principal=?2) ORDER BY created_at DESC, rowid DESC LIMIT 1", runs::RUN_COLUMNS), params![key.to_string(), principal.as_str()], runs::read_run).optional()?;
        let active_attempt = tx.query_row(&format!("SELECT {} FROM routine_runs WHERE routine_key=?1 AND phase IN ('queued','running') AND run_id IN (SELECT run_id FROM routine_result_runs WHERE principal=?2) ORDER BY CASE WHEN phase='running' THEN 0 WHEN reason IS NULL THEN 1 ELSE 2 END, created_at, rowid LIMIT 1", runs::RUN_COLUMNS), params![key.to_string(), principal.as_str()], runs::read_run).optional()?;
        Ok(ResultStatus { active_attempt, routine_key: key.to_string(), revision: current.def.revision, schema_compatible: compatible, snapshot, latest_attempt })
    }).map_err(|e| e.to_string())
}

pub fn cancel(
    home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    sessions: &dyn super::finish::Sessions,
) -> Result<usize, String> {
    // Visibility follows shared routine content; result attempts remain private
    // to the principal that admitted them, independent of the definition's creator.
    service::get_current(home, principal, key).map_err(|e| e.to_string())?;
    let store = RoutineStateStore::open(home).map_err(|e| e.to_string())?;
    let (runs, finished) = store.transaction(|tx| {
        let mut statement = tx.prepare("SELECT r.run_id, r.session_id, r.wake_machine IS NOT NULL, r.launch_machine FROM routine_runs r JOIN routine_result_runs a ON a.run_id=r.run_id WHERE r.routine_key=?1 AND a.principal=?2 AND r.phase IN ('queued','running')")?;
        let rows = statement.query_map(params![key.to_string(), principal.as_str()], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, bool>(2)?, r.get::<_, Option<String>>(3)?)))?;
        let runs = rows.collect::<Result<Vec<_>, _>>()?;
        let ids = runs.iter().map(|(id, _, _, _)| id.clone()).collect::<Vec<_>>();
        let finished = super::finish::cancel_batch_in(tx, &ids, super::finish::now_ms())?;
        Ok((runs, finished))
    }).map_err(|e| e.to_string())?;
    for ended in &finished {
        let run = runs.iter().find(|(id, _, _, _)| *id == ended.run_id);
        // A wake run woke a live session it does not own: nothing to kill.
        if !run.is_some_and(|(_, _, woke, _)| *woke) {
            sessions.kill_on(
                &ended.run_id,
                run.and_then(|(_, _, _, machine)| machine.as_deref()),
                run.and_then(|(_, session, _, _)| session.as_deref()),
            );
        }
        // A cancelled run's task closes with it, as every other ending's does.
        crate::session_usage::task_outcome::close_run_task(
            store.path(),
            &ended.run_id,
            ended.phase,
        );
        ended.announce();
    }
    Ok(finished.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn schema_contract_bounds_and_offline_references() {
        for schema in [
            json!({"$ref":"https://example.com/schema"}),
            json!({"$ref":"#/definitions/a"}),
            json!({"$dynamicRef":"x"}),
            json!({"type":"invalid"}),
        ] {
            assert!(validate_schema(&schema).is_err());
        }
        let schema = json!({"type":"object", "properties":{"value":{"type":"integer","minimum":0}}, "required":["value"], "additionalProperties":false});
        assert!(validate_payload(&schema, &json!({"value":1})).is_ok());
        assert!(validate_payload(&schema, &json!({"value":-1})).is_err());
        assert!(validate_payload(&schema, &json!({"value":1,"secret":"private"})).is_err());
        assert!(validate_schema(&json!({"description":"x".repeat(32*1024)})).is_err());
        assert!(validate_payload(&json!(true), &json!("x".repeat(1024 * 1024))).is_err());
        let deep = (0..65).fold(json!(0), |v, _| json!([v]));
        assert!(validate_payload(&json!(true), &deep).is_err());
        let nested = (0..5).fold(json!(true), |v, _| json!({"anyOf":[v]}));
        assert!(validate_schema(&nested).is_err());
        let error = validate_payload(&schema, &json!({"value":"PRIVATE"})).unwrap_err();
        assert!(!error.contains("PRIVATE"));
    }
    #[test]
    fn literal_keyword_names_are_data_but_nested_schema_references_are_refused() {
        let schema = json!({"type":"object", "properties":{"$ref":{"type":"string"}, "anyOf":{"const":{"$ref":"literal","anyOf":[1]}}}, "default":{"$dynamicRef":"literal"}, "examples":[{"$ref":"literal"}]});
        assert!(validate_schema(&schema).is_ok());
        for schema in [
            json!({"properties":{"safe":{"$ref":"x"}}}),
            json!({"items":{"$ref":"x"}}),
            json!({"$defs":{"x":{"$recursiveRef":"x"}}}),
            json!({"dependencies":{"x":{"$ref":"x"}}}),
        ] {
            assert!(validate_schema(&schema).is_err());
        }
    }
    #[test]
    fn cached_connection_does_not_reserve_writer_and_publication_rechecks_termination() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.sqlite");
        let store = RoutineStateStore::open_at(&path).unwrap();
        store.transaction(|tx| {
            tx.execute("INSERT INTO routine_runs(run_id,routine_key,revision,source,phase,session_id,created_at,started_at) VALUES('run','a/b/c/d',1,'manual','running','session',0,0)", [])?;
            tx.execute("INSERT INTO routine_result_runs(run_id,principal,schema_json) VALUES('run','1','true')", [])?;
            Ok(())
        }).unwrap();
        let mut writer = Connection::open(&path).unwrap();
        let lock = writer
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        let principal = Principal::from(1u64);
        let authority = store
            .read_transaction(|tx| read_authority(tx, &principal, "session"))
            .unwrap();
        assert!(store
            .read_transaction(|tx| {
                tx.execute("DELETE FROM routine_result_runs", [])?;
                Ok(())
            })
            .is_err());
        lock.rollback().unwrap();
        let payload = json!({"ok":true});
        validate_payload(&json!(true), &payload).unwrap();
        super::super::finish::finish(&store, "run", super::super::finish::Ending::Cancelled, 1)
            .unwrap();
        assert!(commit_candidate(
            &store,
            &principal,
            "session",
            &authority,
            Ok(payload.to_string())
        )
        .is_err());
    }

    // SW-AGR-9 (MUST 1): a delivered wake keeps its run `running` with
    // `delivered_at`, so a result-schema run can still publish and the woken
    // session can still finish it with `routine_done`. Ending statuses end it.
    #[test]
    fn a_delivered_wake_run_stays_running_so_it_can_publish_and_be_done() {
        use crate::delivery::run_status::WakeRunStatus;
        use codesense_pty_core::session_wake::UndeliveredReason;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.sqlite");
        let store = RoutineStateStore::open_at(&path).unwrap();
        store
            .transaction(|tx| {
                for run in ["wake", "lost", "gone"] {
                    tx.execute("INSERT INTO routine_runs(run_id,routine_key,revision,source,phase,created_at) VALUES(?1,'a/b/c/d',1,'schedule','queued',0)", params![run])?;
                }
                tx.execute("INSERT INTO routine_result_runs(run_id,principal,schema_json) VALUES('wake','1','true')", [])?;
                // SW-AGR-2: a result schema keeps a delivered wake open (§9.7).
                tx.execute("INSERT INTO routine_result_runs(run_id,principal,schema_json) VALUES('lost','1','true')", [])?;
                Ok(())
            })
            .unwrap();
        let apply = |run: &str, status: WakeRunStatus| {
            super::super::finish::apply_wake_status(
                &store,
                run,
                &crate::delivery::authority::SessionRef::new("local", "s-barb"),
                &status,
                50,
            )
            .unwrap()
        };
        assert_eq!(
            apply("wake", WakeRunStatus::Queued { deadline_ms: 9 }),
            Some("queued")
        );
        assert_eq!(
            apply(
                "wake",
                WakeRunStatus::Delivered {
                    delivered_at_ms: 42
                }
            ),
            Some("running")
        );
        let run = store.run("wake").unwrap().unwrap();
        assert_eq!(
            (
                run.phase.as_str(),
                run.delivered_at,
                run.started_at,
                run.session_id.as_deref()
            ),
            ("running", Some(42), Some(42), Some("s-barb"))
        );
        // The woken agent publishes its result for the run...
        let principal = Principal::from(1u64);
        let authority = store
            .read_transaction(|tx| read_authority(tx, &principal, "s-barb"))
            .unwrap();
        assert_eq!(authority.run, "wake");
        commit_candidate(
            &store,
            &principal,
            "s-barb",
            &authority,
            Ok(json!({"ok": true}).to_string()),
        )
        .unwrap();
        // ...and finishes it without ending the session.
        let done = super::super::finish::finish_for_session(
            &store,
            "wake",
            "s-barb",
            crate::dashboard_stream::SessionStatus::Success,
            Some("done".into()),
            60,
        )
        .unwrap()
        .unwrap();
        assert_eq!(done.phase, "succeeded");
        // Undelivered and unknown end their runs; unknown keeps its detail.
        assert_eq!(
            apply(
                "lost",
                WakeRunStatus::Unknown {
                    detail: "not confirmed".into()
                }
            ),
            Some("unknown")
        );
        assert_eq!(
            apply(
                "gone",
                WakeRunStatus::Undelivered {
                    reason: UndeliveredReason::SessionEnded
                }
            ),
            Some("undelivered")
        );
        let gone = store.run("gone").unwrap().unwrap();
        assert_eq!(gone.reason.as_deref(), Some("session_ended"));
        assert_eq!(
            store.run("lost").unwrap().unwrap().result_text.as_deref(),
            Some("not confirmed")
        );
        // A lost reply's `unknown` is not terminal for a wake run: its real
        // outcome upgrades it, and the agent can still finish it.
        assert_eq!(
            apply(
                "lost",
                WakeRunStatus::Delivered {
                    delivered_at_ms: 70
                }
            ),
            Some("running")
        );
        let upgraded = store.run("lost").unwrap().unwrap();
        assert_eq!(
            (
                upgraded.phase.as_str(),
                upgraded.result_text,
                upgraded.ended_at
            ),
            ("running", None, None)
        );
        assert!(super::super::finish::finish_for_session(
            &store,
            "lost",
            "s-barb",
            crate::dashboard_stream::SessionStatus::Failed,
            None,
            80,
        )
        .unwrap()
        .is_ok());
        // A run that already ended does not move.
        assert_eq!(
            apply("gone", WakeRunStatus::Delivered { delivered_at_ms: 1 }),
            None
        );
    }
}
