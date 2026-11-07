// SPDX-License-Identifier: MIT
//! Metadata transitions observed at the context index's publication boundary.
use std::collections::{HashMap, HashSet};

use super::{
    admit::{self, Admission, EventCause, Fire},
    discover::DiscoveredRoutine,
    model::{MetadataCondition, RoutineTrigger, EVENT_METADATA_CHANGE},
    state::RoutineStateStore,
    tick::{Account, RunSource},
};
use crate::context_query::{
    engine::{self, Viewer},
    model::{Layer, OrgScope},
    service::SnapshotChange,
};
use crate::coordinator::StoreResult;

fn conditions(def: &super::model::RoutineDef) -> impl Iterator<Item = &MetadataCondition> {
    def.triggers.iter().filter_map(|t| match t {
        RoutineTrigger::Event(e) if e.kind.trim() == EVENT_METADATA_CHANGE => e.metadata.as_ref(),
        _ => None,
    })
}

/// Offer only observed entries into a target predicate. Selection, visibility,
/// aliases and equality all belong to the context query evaluator.
pub fn offer(
    account: &Account,
    routines: &[DiscoveredRoutine],
    store: &RoutineStateStore,
    change: &SnapshotChange,
    viewer: &Viewer,
    now: i64,
) -> StoreResult<Vec<(String, Admission)>> {
    if change.owner.is_none()
        || change.owner != account.1.numeric_id().ok()
        || change.replaced
        || !change.previous.ready
        || !change.current.ready
        || !routines
            .iter()
            .any(|r| r.armed_here && !r.held() && conditions(&r.def).next().is_some())
    {
        return Ok(vec![]);
    }
    let old: HashMap<_, _> = change
        .previous
        .records
        .iter()
        .map(|r| (r.path.as_str(), r.as_ref()))
        .collect();
    let paths: HashSet<_> = change.change.paths.iter().map(String::as_str).collect();
    let changed = |r: &crate::context_query::record::ContextRecord| {
        old.get(r.path.as_str()).is_some_and(|before| {
            r.metadata_valid
                && before.metadata_valid
                && if change.change.truncated {
                    *before != r
                } else {
                    paths.contains(r.path.as_str())
                }
        })
    };
    let host =
        super::host::local_host_id(&account.0).map_err(crate::coordinator::StoreError::Io)?;
    let mut admissions = vec![];
    for routine in routines {
        if !routine.armed_here
            || routine.held()
            || !store.is_enabled(&routine.key, &routine.def, &host)?
        {
            continue;
        }
        // Overlapping conditions on one field share an occurrence; different fields retain their own context.
        let mut offered = HashSet::new();
        for condition in conditions(&routine.def) {
            let mut selection = condition.selection.clone();
            // The caller's selection may narrow reach, never expand it. Explicit
            // org also gives the evaluator the correct private visibility.
            if let OrgScope::List(orgs) = &selection.scope.orgs {
                if !orgs.contains(&routine.key.org) {
                    continue;
                }
            }
            selection.scope.orgs = OrgScope::List(vec![routine.key.org.clone()]);
            let in_reach = |r: &crate::context_query::record::ContextRecord| {
                changed(r)
                    && r.org.as_deref() == Some(routine.key.org.as_str())
                    && if routine.key.account.is_some() {
                        r.layer == Layer::Private
                    } else {
                        r.layer == Layer::Team
                            && r.team.as_deref() == Some(routine.key.team.as_str())
                    }
            };
            let indices = match engine::matching(
                &change.current.corpus(),
                &selection,
                viewer,
                &in_reach,
            ) {
                Ok(indices) => indices,
                Err(e) => {
                    log::warn!(target: "routines", "routine {}: metadata selection: {e}", routine.key);
                    continue;
                }
            };
            for i in indices {
                let new = &change.current.records[i];
                let before = old[new.path.as_str()];
                if engine::clause_matches(before, &condition.target, viewer)
                    || !engine::clause_matches(new, &condition.target, viewer)
                    || condition
                        .previous
                        .as_ref()
                        .is_some_and(|c| !engine::clause_matches(before, c, viewer))
                    || !offered.insert((
                        new.path.clone(),
                        engine::normalize_key(&condition.target.key),
                    ))
                {
                    continue;
                }
                let previous = engine::field_value(before, &condition.target.key);
                let next = engine::field_value(new, &condition.target.key);
                let payload = serde_json::json!({
                    "kind": EVENT_METADATA_CHANGE, "org": new.org, "team": new.team,
                    "path": new.path, "rel_path": new.rel_path,
                    "story_id": new.fields.get("story_id"), "field": condition.target.key,
                    "previous_present": previous.is_some(), "previous_value": previous,
                    "new_value": next,
                });
                let occurrence = format!(
                    "{}@metadata:{}:{}:{}:{}",
                    routine.key,
                    change.observer_id,
                    change.current.generation,
                    new.path,
                    engine::normalize_key(&condition.target.key)
                );
                let causation = Default::default();
                let fire = Fire {
                    key: &routine.key,
                    principal: &account.1,
                    def: &routine.def,
                    source: RunSource::Event,
                    occurrence_key: Some(&occurrence),
                    scheduled_at: None,
                    now,
                    variables: Default::default(),
                    trigger_payload: Some(payload),
                    cause: Some(EventCause {
                        origin: "metadata change",
                        causation: &causation,
                        rate_limited: false,
                    }),
                };
                admissions.push((
                    routine.key.to_string(),
                    store.transaction(|tx| admit::admit(tx, &account.0, &fire))?,
                ));
            }
        }
    }
    Ok(admissions)
}

pub(crate) fn words(condition: &MetadataCondition) -> String {
    let kind = condition
        .selection
        .scope
        .kinds
        .as_ref()
        .filter(|k| k.len() == 1)
        .map(|k| k[0].as_str())
        .unwrap_or("document");
    let article = if kind == "epic" { "an" } else { "a" };
    let value = |v: &Option<serde_json::Value>| {
        v.as_ref()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| v.to_string())
            })
            .unwrap_or_default()
    };
    let mut words = if condition.list {
        format!(
            "when {article} {kind} gains {} in {}",
            value(&condition.target.value),
            condition.target.key
        )
    } else if let Some(previous) = &condition.previous {
        format!(
            "when {article} {kind}'s {} moves from {} to {}",
            condition.target.key,
            value(&previous.value),
            value(&condition.target.value)
        )
    } else {
        format!(
            "when {article} {kind}'s {} becomes {}",
            condition.target.key,
            value(&condition.target.value)
        )
    };
    if let Some(folder) = &condition.selection.scope.path_prefix {
        words.push_str(&format!(", under {folder}"));
    }
    words
}
