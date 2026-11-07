// SPDX-License-Identifier: MIT
//! Completion triggers share ordinary discovery, placement and admission.
use super::model::{AfterOn, RoutineDef, RoutineKey, ValidationError};
use crate::domain::Principal;
use std::collections::BTreeMap;
use std::path::Path;

pub fn validate(
    home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    def: &RoutineDef,
) -> Result<(), ValidationError> {
    if def.after.is_none() && def.armed_machine().is_none() {
        return Ok(());
    }
    let config = super::service::space_config_dir(home, principal, &key.org, &key.team)
        .map_err(|e| ValidationError::Chain(e.to_string()))?;
    let mut all: BTreeMap<String, RoutineDef> = super::discover::space_routine_dirs(&config)
        .into_iter()
        .filter_map(|(_, _, d)| d.ok())
        .map(|d| (d.id.clone(), d))
        .collect();
    all.insert(def.id.clone(), def.clone());
    if def.armed_machine().is_some() {
        let dependents: Vec<_> = all
            .values()
            .filter(|d| !d.is_archived())
            .filter(|d| {
                d.after
                    .as_ref()
                    .is_some_and(|after| after.routine == def.id)
            })
            .map(|d| format!("{} ({})", d.name, d.id))
            .collect();
        if !dependents.is_empty() {
            return Err(ValidationError::Chain(format!("{} can't run on a machine while these routines run after it: {}. Remove their Run after triggers first.", def.name, dependents.join(", "))));
        }
    }
    let Some(after) = &def.after else {
        return Ok(());
    };
    let upstream = &after.routine;
    if !all.contains_key(upstream) {
        return Err(ValidationError::Chain(format!("{upstream} no longer exists, so this never fires. Pick another routine or remove this trigger.")));
    }
    if all
        .get(upstream)
        .is_some_and(|d| d.armed_machine().is_some())
    {
        return Err(ValidationError::Chain(format!("{upstream} is armed on a machine. Run after supports desktop routines only; pick another routine.")));
    }
    let mut path = vec![def.id.clone()];
    let mut next = upstream.clone();
    loop {
        if let Some(at) = path.iter().position(|id| id == &next) {
            path.push(next);
            let names = path[at..]
                .iter()
                .map(|id| all.get(id).map(|d| d.name.as_str()).unwrap_or(id))
                .collect::<Vec<_>>();
            return Err(ValidationError::Chain(format!(
                "This would make a loop: {}. Pick another routine or remove its after trigger.",
                names.join(" → ")
            )));
        }
        path.push(next.clone());
        let Some(after) = all.get(&next).and_then(|d| d.after.as_ref()) else {
            break;
        };
        next = after.routine.clone();
    }
    Ok(())
}

pub fn offer(store_path: &Path, run_id: &str) -> Result<(), String> {
    use super::admit::{self, EventCause, Fire};
    use crate::commands::sprint_run::causation::{CausalEdge, Causation};
    let Some(home) = store_path.parent().and_then(Path::parent) else {
        return Ok(());
    };
    if crate::coordinator::default_store_path(home) != store_path {
        return Ok(());
    }
    let store = super::state::RoutineStateStore::open_at(store_path).map_err(|e| e.to_string())?;
    let Some(run) = store.run(run_id).map_err(|e| e.to_string())? else {
        return Ok(());
    };
    if !matches!(
        run.phase.as_str(),
        "succeeded" | "failed" | "timed_out" | "needs_you" | "ended"
    ) {
        return Ok(());
    }
    let json: Option<String> = store
        .transaction(|tx| {
            use rusqlite::OptionalExtension;
            tx.query_row(
                "SELECT request FROM run_requests WHERE run_id = ?1",
                [run_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(Into::into)
        })
        .map_err(|e| e.to_string())?;
    let Some(pointer) = json
        .as_deref()
        .and_then(admit::parse_pointer)
        .and_then(Result::ok)
    else {
        return Ok(());
    };
    let principal = Principal::from(pointer.principal.as_str());
    let Some(key) = RoutineKey::parse(&run.routine_key) else {
        return Ok(());
    };
    let name = super::service::get_current(home, &principal, &key)
        .map(|r| r.def.name)
        .unwrap_or_else(|_| key.id.clone());
    let cause = run
        .causation
        .as_deref()
        .and_then(|s| serde_json::from_str::<Causation>(s).ok())
        .unwrap_or_else(|| {
            Causation {
                origin_run_id: Some(run_id.into()),
                ..Default::default()
            }
            .descend(CausalEdge::new(key.to_string(), key.to_string()))
        });
    let payload = serde_json::json!({"kind":"routine_finished", "run_id": run_id, "routine": key.id, "routine_key": key.to_string(), "routine_name": name, "phase": run.phase, "result_text": run.result_text.unwrap_or_default(), "detail": run.detail.unwrap_or_default()});
    let origin = format!("finished routine {} run {run_id}", key.id);
    for routine in
        super::discover::discover_routines(home, &principal).map_err(|e| e.to_string())?
    {
        if routine.held()
            || !routine.armed_here
            || routine.key.org != key.org
            || routine.key.team != key.team
            || routine.key.account != key.account
        {
            continue;
        }
        let Some(after) = &routine.def.after else {
            continue;
        };
        if after.routine != key.id || (after.on == AfterOn::Succeeded && run.phase != "succeeded") {
            continue;
        }
        let occurrence = format!("{}@after:{run_id}", routine.key);
        let admission = store
            .transaction(|tx| {
                admit::admit(
                    tx,
                    home,
                    &Fire {
                        key: &routine.key,
                        principal: &principal,
                        def: &routine.def,
                        source: super::tick::RunSource::Event,
                        occurrence_key: Some(&occurrence),
                        scheduled_at: None,
                        now: super::finish::now_ms(),
                        variables: Default::default(),
                        trigger_payload: Some(payload.clone()),
                        cause: Some(EventCause {
                            origin: &origin,
                            causation: &cause,
                            rate_limited: false,
                        }),
                    },
                )
            })
            .map_err(|e| e.to_string())?;
        if admission.is_queued() {
            crate::commands::sprint_run::claim_loop::wake();
        }
        super::events::changed(&routine.key.to_string(), &admission.run_id);
    }
    Ok(())
}
