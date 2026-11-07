// SPDX-License-Identifier: MIT
//! Run review reads share cached inputs; committed writes update only the affected run.
use super::{
    projection::{Delta, TopicRegistry},
    LiveHub,
};
use crate::commands::sprint_run::{durable_run_identity as durable, run_evidence, run_store};
use crate::coordinator::{RunEvent, SqliteOrchestrationStore};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex, Weak},
};
use tauri::Manager;

pub const TOPICS: [&str; 2] = ["runEndorsements", "runEvidence"];
type RecordsByRun = BTreeMap<(String, String), Vec<run_store::StoreRecord>>;
type Shared = Arc<Mutex<Owner>>;
type Observer = (Weak<Mutex<Owner>>, Weak<tokio::sync::Notify>);
static SOURCES: LazyLock<Mutex<BTreeMap<PathBuf, Vec<Observer>>>> = LazyLock::new(Default::default);

pub enum Written {
    Assertion(Value),
    Settled { run_id: String, id: i64 },
    Record(run_store::StoreRecord),
    Durable { seq: i64, event: RunEvent },
}

pub fn written(path: &Path, change: Written) {
    let observers = {
        let mut sources = SOURCES.lock().unwrap();
        let Some(observers) = sources.get_mut(path) else {
            return;
        };
        observers.retain(|(owner, _)| owner.strong_count() > 0);
        observers.clone()
    };
    for (owner, wake) in observers {
        if let Some(owner) = owner.upgrade() {
            if owner.lock().unwrap().written(path, &change) {
                if let Some(wake) = wake.upgrade() {
                    wake.notify_one();
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Binding {
    principal: String,
    weft: PathBuf,
    jsonl: PathBuf,
    durable: PathBuf,
}
#[derive(Clone)]
struct View {
    binding: Binding,
    run: String,
    story: String,
    assertions: BTreeMap<i64, Value>,
    dirty: bool,
}
#[derive(Default)]
pub struct Owner {
    views: BTreeMap<(String, String), View>,
    interests: BTreeMap<String, (String, String)>,
    records: BTreeMap<PathBuf, Result<RecordsByRun, String>>,
    streams: BTreeMap<(PathBuf, String), Result<BTreeMap<i64, RunEvent>, String>>,
    #[cfg(test)]
    reads: usize,
    #[cfg(test)]
    folds: usize,
}

fn observe(owner: &Shared, wake: &Arc<tokio::sync::Notify>, binding: &Binding) {
    let mut sources = SOURCES.lock().unwrap();
    for path in [&binding.weft, &binding.jsonl, &binding.durable] {
        let observers = sources.entry(path.clone()).or_default();
        if !observers
            .iter()
            .any(|(o, _)| o.ptr_eq(&Arc::downgrade(owner)))
        {
            observers.push((Arc::downgrade(owner), Arc::downgrade(wake)));
        }
    }
}

fn record_key(record: &run_store::StoreRecord) -> Option<(&str, &str)> {
    use run_store::RunEvent::*;
    match &record.event {
        Outcome(e) => Some((&e.story_id, &e.run_id)),
        StepResult(e) => Some((&e.story_id, &e.run_id)),
        Verdict(e) => Some((&e.story_id, &e.run_id)),
        _ => None,
    }
}

impl Owner {
    fn prepare(
        &mut self,
        topic: &str,
        key: &str,
        binding: Binding,
        story: &str,
        run: &str,
        id: &str,
    ) -> Result<(), String> {
        let identity = (topic.to_owned(), key.to_owned());
        if self
            .views
            .get(&identity)
            .is_some_and(|v| v.binding != binding)
        {
            return Err("run review belongs to another principal".into());
        }
        if topic == "runEvidence"
            && (self.records.get(&binding.jsonl).is_some_and(Result::is_err)
                || self.streams.iter().any(|((path, stream), result)| {
                    path == &binding.durable
                        && (stream == run || stream == &durable::durable_run_key(run, story))
                        && result.is_err()
                }))
        {
            self.views.remove(&identity);
        }
        if !self.views.contains_key(&identity) {
            let mut view = View {
                binding: binding.clone(),
                story: story.into(),
                run: run.into(),
                assertions: BTreeMap::new(),
                dirty: true,
            };
            if topic == "runEndorsements" {
                let store =
                    crate::weft::WeftStore::open(&binding.weft).map_err(|e| e.to_string())?;
                for row in crate::weft::wv_read_pending_assertions(&store, run)
                    .map_err(|e| e.to_string())?
                {
                    view.assertions.insert(
                        row.id,
                        serde_json::to_value(
                            crate::commands::weft_ledger::WvPendingAssertionDto::from(row),
                        )
                        .map_err(|e| e.to_string())?,
                    );
                }
                #[cfg(test)]
                {
                    self.reads += 1;
                }
            } else {
                if self.records.get(&binding.jsonl).is_none_or(Result::is_err) {
                    self.records.insert(
                        binding.jsonl.clone(),
                        run_store::read_records_checked(&binding.jsonl).map(|(records, _)| {
                            let mut by_run = RecordsByRun::new();
                            for record in records {
                                if let Some((story, run)) = record_key(&record) {
                                    by_run
                                        .entry((story.into(), run.into()))
                                        .or_default()
                                        .push(record);
                                }
                            }
                            by_run
                        }),
                    );
                    #[cfg(test)]
                    {
                        self.reads += 1;
                    }
                }
                // Preserve composite-first authority and the legacy bare stream fallback.
                if let Ok(store) = SqliteOrchestrationStore::open(&binding.durable) {
                    for stream in [durable::durable_run_key(run, story), run.into()] {
                        let stream_key = (binding.durable.clone(), stream.clone());
                        if self.streams.get(&stream_key).is_none_or(Result::is_err) {
                            self.streams.insert(
                                stream_key,
                                store
                                    .run_events_numbered(&stream)
                                    .map(|rows| rows.into_iter().collect())
                                    .map_err(|e| e.to_string()),
                            );
                            #[cfg(test)]
                            {
                                self.reads += 1;
                            }
                        }
                    }
                }
            }
            self.views.insert(identity.clone(), view);
        }
        self.interests.insert(id.into(), identity);
        Ok(())
    }
    pub(super) fn unsubscribe(&mut self, id: &str) {
        self.interests.remove(id);
    }

    fn written(&mut self, path: &Path, change: &Written) -> bool {
        if let Written::Record(record) = change {
            if let (Some(Ok(by_run)), Some((story, run))) =
                (self.records.get_mut(path), record_key(record))
            {
                let records = by_run.entry((story.into(), run.into())).or_default();
                if !records
                    .iter()
                    .any(|r| r.seq == record.seq && r.ts == record.ts)
                {
                    records.push(record.clone());
                }
            }
        }
        if let Written::Durable { seq, event } = change {
            if let Some(Ok(events)) = self.streams.get_mut(&(path.into(), event.run_id.clone())) {
                events.insert(*seq, event.clone());
            }
        }
        let mut wake = false;
        for (identity, view) in &mut self.views {
            let affected = match change {
                Written::Assertion(row)
                    if view.binding.weft == path
                        && identity.0 == "runEndorsements"
                        && row["runId"].as_str() == Some(&view.run) =>
                {
                    view.assertions
                        .insert(row["id"].as_i64().unwrap(), row.clone());
                    true
                }
                Written::Settled { run_id, id }
                    if view.binding.weft == path
                        && identity.0 == "runEndorsements"
                        && run_id == &view.run =>
                {
                    view.assertions.remove(id);
                    true
                }
                Written::Record(record) => {
                    identity.0 == "runEvidence"
                        && view.binding.jsonl == path
                        && record_key(record) == Some((view.story.as_str(), view.run.as_str()))
                }
                Written::Durable { event, .. } => {
                    identity.0 == "runEvidence"
                        && view.binding.durable == path
                        && (event.run_id == view.run
                            || event.run_id == durable::durable_run_key(&view.run, &view.story))
                }
                _ => false,
            };
            if affected {
                view.dirty = true;
                wake |= self.interests.values().any(|i| i == identity);
            }
        }
        wake
    }

    fn evidence(&self, view: &View) -> Result<Option<run_evidence::RunEvidence>, String> {
        let records = self
            .records
            .get(&view.binding.jsonl)
            .and_then(|r| r.as_ref().ok())
            .and_then(|runs| runs.get(&(view.story.clone(), view.run.clone())));
        for stream in [
            durable::durable_run_key(&view.run, &view.story),
            view.run.clone(),
        ] {
            if let Some(events) = self.streams.get(&(view.binding.durable.clone(), stream)) {
                let events = events
                    .as_ref()
                    .map_err(|e| format!("durable run projection failed: {e}"))?;
                if let Some(proj) = durable::RunIdentityProjector::rebuild(
                    &events.values().cloned().collect::<Vec<_>>(),
                ) {
                    if proj.header.story_id == view.story {
                        return Ok(Some(run_evidence::durable_evidence_with_reported(
                            proj,
                            records.map_or(&[], Vec::as_slice),
                            &view.story,
                            &view.run,
                        )));
                    }
                }
            }
        }
        let records = self
            .records
            .get(&view.binding.jsonl)
            .ok_or("run store unavailable")?
            .as_ref()
            .map_err(Clone::clone)?;
        Ok(run_evidence::build_run_evidence(
            records
                .get(&(view.story.clone(), view.run.clone()))
                .map_or(&[], Vec::as_slice),
            &view.story,
            &view.run,
        ))
    }

    fn project(&mut self, projections: &mut TopicRegistry) -> Result<Vec<Delta>, String> {
        let mut deltas = Vec::new();
        for (topic, key, rows) in self.drain() {
            deltas.extend(projections.replace_view(
                &topic,
                &key,
                rows.into_iter().map(|r| (Some(key.clone()), r)).collect(),
            )?);
        }
        Ok(deltas)
    }

    fn drain(&mut self) -> Vec<(String, String, Vec<Value>)> {
        let active: BTreeSet<_> = self.interests.values().cloned().collect();
        let mut changes = Vec::new();
        for identity in active {
            let Some(view) = self.views.get(&identity).filter(|v| v.dirty) else {
                continue;
            };
            let rows = if identity.0 == "runEndorsements" {
                view.assertions.iter().map(|(id, assertion)| json!({"id":format!("assertion:{id}"),"assertion":assertion})).collect()
            } else {
                #[cfg(test)]
                {
                    self.folds += 1;
                }
                match self.evidence(view) {
                    Ok(Some(evidence)) => {
                        let mut value =
                            serde_json::to_value(evidence).expect("serializable evidence");
                        let nodes = value
                            .as_object_mut()
                            .unwrap()
                            .remove("perNodeResults")
                            .unwrap_or(json!([]));
                        let mut rows = vec![json!({"id":"evidence","evidence":value})];
                        rows.extend(nodes.as_array().unwrap().iter().enumerate().map(|(order, node)| json!({"id":format!("node:{}", node["nodeId"].as_str().unwrap()),"result":node,"order":order})));
                        rows
                    }
                    Ok(None) => Vec::new(),
                    Err(error) => vec![json!({"id":"error","error":error})],
                }
            };
            self.views.get_mut(&identity).unwrap().dirty = false;
            changes.push((identity.0, identity.1, rows));
        }
        changes
    }
}

pub async fn prepare(
    app: &tauri::AppHandle,
    topic: &str,
    key: &str,
    id: &str,
) -> Result<(), String> {
    let (principal, team, story, run): (String, String, String, String) = serde_json::from_str(
        key.strip_prefix("runReview:")
            .ok_or("invalid run review key")?,
    )
    .map_err(|e| e.to_string())?;
    let (org, team) = team.split_once(':').ok_or("run review requires a team")?;
    let context =
        match crate::commands::route_to_context::derive_context_for(app, org, team).await? {
            crate::commands::route_to_context::ContextResolution::Ok(ctx) => ctx,
            _ => return Err("run review team is unavailable".into()),
        };
    if principal != context.principal().to_string() {
        return Err("run review principal changed".into());
    }
    let (app, topic, key, id) = (app.clone(), topic.to_owned(), key.to_owned(), id.to_owned());
    crate::commands::await_blocking("run_review.prepare", move || {
        let binding = Binding {
            principal: context.principal().to_string(),
            weft: crate::weft::resolve_weft_store_path(
                &context.codesense_home,
                &context.principal(),
                &context.org_slug,
                context
                    .team_name
                    .as_deref()
                    .ok_or("run review requires a team")?,
            )?,
            jsonl: context.codesense_home.join("run-store.jsonl"),
            durable: crate::coordinator::default_store_path(&context.codesense_home),
        };
        let hub = app.state::<LiveHub>();
        observe(&hub.run_review, &hub.wake, &binding);
        app.state::<LiveHub>()
            .run_review
            .lock()
            .unwrap()
            .prepare(&topic, &key, binding, &story, &run, &id)?;
        refresh(&app)
    })
    .await
}

pub(super) fn refresh<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Result<(), String> {
    let hub = app.state::<LiveHub>();
    let mut owner = hub.run_review.lock().unwrap();
    let deltas = owner.project(&mut hub.model.lock().unwrap().projections)?;
    super::publish(app, deltas);
    drop(owner);
    Ok(())
}

#[cfg(test)]
mod tests;
