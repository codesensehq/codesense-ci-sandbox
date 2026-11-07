// SPDX-License-Identifier: MIT
//! Native configuration views. File ingress and managed writers share these sources.
use super::projection::{Delta, Snapshot, TopicRegistry};
use crate::domain::Principal;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use tauri::Manager;

pub const TOPICS: [&str; 3] = ["profile", "workspaceConfig", "prefs"];
struct Source {
    topic: String,
    path: PathBuf,
    dirty: bool,
}
#[derive(Default)]
pub struct Registry {
    principal: String,
    sources: BTreeMap<String, Source>,
    values: BTreeMap<PathBuf, Value>,
    /// Bumped by every change to a path, so a read taken before it is never applied after it.
    generations: BTreeMap<PathBuf, u64>,
}
/// A source read the registry needs but must not do while the model lock is held.
pub(super) struct Read {
    topic: String,
    path: PathBuf,
    principal: String,
    generation: u64,
}
pub(super) struct Fetched {
    read: Read,
    value: Result<Value, String>,
}
impl Read {
    pub(super) fn fetch(self) -> Fetched {
        let value = read_source(&self.topic, &self.path, &self.principal);
        Fetched { read: self, value }
    }
}
/// Either the finished result, or a read to run off the model lock before asking again.
pub(super) enum Step<T> {
    Done(T),
    Read(Read),
}
pub(crate) fn profile_path(
    home: &Path,
    principal: &Principal,
    org: &str,
) -> Result<PathBuf, String> {
    Ok(
        crate::context_repo::scratch_space::org_space_at(home, principal, org)?
            .config_dir()
            .join("profile.json"),
    )
}
fn read_value(path: &Path) -> Result<Value, String> {
    #[cfg(test)]
    tests::SOURCE_READS.with(|reads| reads.set(reads.get() + 1));
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Value::Null),
        Err(e) => Err(e.to_string()),
    }
}
fn read_source(topic: &str, path: &Path, principal: &str) -> Result<Value, String> {
    let value = if topic == "prefs" {
        #[cfg(test)]
        tests::SOURCE_READS.with(|reads| reads.set(reads.get() + 1));
        crate::commands::system::profile_workspace::read_prefs_source(path, principal)
    } else {
        read_value(path)
    };
    #[cfg(test)]
    tests::READ_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow().as_ref() {
            hook(path);
        }
    });
    value
}
fn rows(topic: &str, key: &str, mut value: Value) -> Result<Vec<Value>, String> {
    #[cfg(test)]
    tests::VIEW_BUILDS.with(|builds| builds.set(builds.get() + 1));
    if topic == "prefs" {
        let principal = key
            .strip_prefix("prefs:")
            .and_then(|key| key.strip_suffix(":native"));
        let value = crate::commands::system::profile_workspace::prefs_view(&value, principal);
        fn flatten(value: &Value, path: Vec<String>, rows: &mut Vec<Value>) {
            if let Some(fields) = value.as_object().filter(|fields| !fields.is_empty()) {
                for (key, value) in fields {
                    let mut next = path.clone();
                    next.push(key.clone());
                    flatten(value, next, rows);
                }
            } else if !path.is_empty() {
                rows.push(
                    json!({"id":serde_json::to_string(&path).unwrap(),"path":path,"value":value}),
                );
            }
        }
        let mut rows = Vec::new();
        flatten(&value, Vec::new(), &mut rows);
        return Ok(rows);
    }
    if !value.is_null() {
        value = if topic == "profile" {
            serde_json::to_value(
                serde_json::from_value::<crate::panes::profile::Profile>(value)
                    .map_err(|e| e.to_string())?,
            )
            .unwrap()
        } else {
            serde_json::to_value(
                serde_json::from_value::<crate::commands::workspace_config::WorkspaceConfig>(value)
                    .map_err(|e| e.to_string())?,
            )
            .unwrap()
        };
    }
    let mut rows = Vec::new();
    if topic == "workspaceConfig" {
        if let Some(repos) = value["repos"].as_object() {
            rows.extend(
                repos
                    .iter()
                    .map(|(id, value)| json!({"id":id,"value":value})),
            );
        }
    } else if let Some(fields) = value.as_object() {
        for (field, value) in fields {
            if matches!(field.as_str(), "slots" | "rightPaneSurfaces" | "chrome") {
                if let Some(entries) = value.as_object() {
                    rows.extend(entries.iter().map(|(key, value)| json!({"id":format!("{field}:{key}"),"field":field,"key":key,"value":value})));
                }
            } else {
                rows.push(json!({"id":field,"field":field,"value":value}));
            }
        }
    }
    Ok(rows)
}
fn replace(
    topics: &mut TopicRegistry,
    topic: &str,
    key: &str,
    value: Value,
) -> Result<Vec<Delta>, String> {
    topics.replace_view(
        topic,
        key,
        rows(topic, key, value)?
            .into_iter()
            .map(|row| (Some(key.into()), row))
            .collect(),
    )
}
impl Registry {
    fn open(
        &mut self,
        topics: &mut TopicRegistry,
        home: &Path,
        principal: &Principal,
        topic: &str,
        org: Option<&str>,
    ) -> Result<Step<(Snapshot, Vec<Delta>)>, String> {
        if !TOPICS.contains(&topic) {
            return Err("unknown configuration topic".into());
        }
        if self.principal != principal.as_str() {
            for (key, source) in &self.sources {
                if key != "prefs:global" {
                    topics.remove_view(&source.topic, key);
                }
            }
            self.sources.retain(|key, _| key == "prefs:global");
            self.values.clear();
            self.principal = principal.to_string();
        }
        let (key, path) = if topic == "prefs" {
            let key = match org {
                Some("global") => "prefs:global".to_string(),
                Some("principal") => format!("prefs:{principal}:native"),
                _ => return Err("prefs requires global or principal scope".into()),
            };
            (key, home.join("app-state.json"))
        } else if topic == "profile" {
            let org = org.ok_or("profile requires an org")?;
            (
                format!("profile:{principal}:{org}"),
                profile_path(home, principal, org)?,
            )
        } else {
            (
                format!("workspaceConfig:{principal}:native"),
                home.join("workspace-config.json"),
            )
        };
        let deltas = if self.sources.contains_key(&key) {
            match self.refresh(topics, topic, &key)? {
                Step::Done(deltas) => deltas,
                Step::Read(read) => return Ok(Step::Read(read)),
            }
        } else {
            let Some(value) = self.values.get(&path).cloned() else {
                return Ok(Step::Read(self.read_of(topic, path)));
            };
            let deltas = replace(topics, topic, &key, value)?;
            self.sources.insert(
                key.clone(),
                Source {
                    topic: topic.into(),
                    path,
                    dirty: false,
                },
            );
            deltas
        };
        Ok(Step::Done((topics.snapshot(topic, &key)?, deltas)))
    }
    fn read_of(&self, topic: &str, path: PathBuf) -> Read {
        Read {
            topic: topic.into(),
            principal: self.principal.clone(),
            generation: self.generation(&path),
            path,
        }
    }
    fn generation(&self, path: &Path) -> u64 {
        self.generations.get(path).copied().unwrap_or(0)
    }
    /// Keeps a read taken off the model lock unless its path changed, or the principal switched, meanwhile.
    pub(super) fn deposit(&mut self, fetched: Fetched) -> Result<(), String> {
        let Fetched { read, value } = fetched;
        if read.principal == self.principal && read.generation == self.generation(&read.path) {
            self.values.insert(read.path, value?);
        }
        Ok(())
    }
    /// Starts a change to `path`: older cached values and in-flight reads of it are now stale.
    fn invalidate(&mut self, path: &Path) {
        *self.generations.entry(path.to_path_buf()).or_default() += 1;
        self.values.remove(path);
    }
    pub(super) fn refresh(
        &mut self,
        topics: &mut TopicRegistry,
        topic: &str,
        key: &str,
    ) -> Result<Step<Vec<Delta>>, String> {
        let Some(source) = self
            .sources
            .get(key)
            .filter(|s| s.topic == topic && s.dirty)
        else {
            return Ok(Step::Done(Vec::new()));
        };
        let Some(value) = self.values.get(&source.path).cloned() else {
            return Ok(Step::Read(self.read_of(topic, source.path.clone())));
        };
        let deltas = replace(topics, topic, key, value)?;
        if let Some(source) = self.sources.get_mut(key) {
            source.dirty = false;
        }
        Ok(Step::Done(deltas))
    }
    fn changed(
        &mut self,
        topics: &mut TopicRegistry,
        path: &Path,
        value: Option<Value>,
    ) -> Result<Step<Vec<Delta>>, String> {
        let mut deltas = Vec::new();
        let value = value.or_else(|| self.values.get(path).cloned());
        if value.is_none() {
            let wanted = self.sources.iter().find(|(key, source)| {
                source.path == path && topics.has_subscription(&source.topic, key)
            });
            if let Some((_, source)) = wanted {
                return Ok(Step::Read(self.read_of(&source.topic, path.to_path_buf())));
            }
        }
        for (key, source) in &mut self.sources {
            if source.path == path {
                if !topics.has_subscription(&source.topic, key) {
                    source.dirty = true;
                    continue;
                }
                source.dirty = false;
                deltas.extend(replace(
                    topics,
                    &source.topic,
                    key,
                    value.as_ref().unwrap().clone(),
                )?);
            }
        }
        if let Some(value) = value.filter(|_| !deltas.is_empty()) {
            self.values.insert(path.to_path_buf(), value);
        }
        Ok(Step::Done(deltas))
    }
}

pub(crate) fn watch_prefs<R: tauri::Runtime>(app: &tauri::AppHandle<R>, resync: bool) {
    let (Some(hub), Some(watch)) = (
        app.try_state::<super::LiveHub>(),
        app.try_state::<crate::commands::watch::WatcherState>(),
    ) else {
        return;
    };
    let paths: std::collections::BTreeSet<PathBuf> = hub
        .model
        .lock()
        .unwrap()
        .configuration
        .sources
        .values()
        .filter(|source| source.topic == "prefs")
        .map(|source| source.path.clone())
        .collect();
    for path in paths {
        crate::commands::scratch_watch::register_configuration_file_watch(app, &watch, &path);
        if resync {
            if let Err(error) = path_changed(app, &path) {
                log::warn!(target: "configuration", "prefs watcher startup: {error}");
            }
        }
    }
}

pub(crate) fn prepare<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    home: &Path,
    principal: &Principal,
    topic: &str,
    org: Option<&str>,
) -> Result<Snapshot, String> {
    let hub = app.state::<super::LiveHub>();
    let (snapshot, deltas) = with_reads(&hub, |configuration, projections| {
        configuration.open(projections, home, principal, topic, org)
    })?;
    super::publish(app, deltas);
    Ok(snapshot)
}
pub(crate) fn path_changed<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    path: &Path,
) -> Result<(), String> {
    // The managed app-state writer holds this gate through persistence and publication.
    // Observe disk after that publication so a delayed writer cannot replace a newer ingress.
    if path
        .file_name()
        .is_some_and(|name| name == "app-state.json")
    {
        if let Some(profile) = app.try_state::<crate::commands::local_profile::LocalProfileState>()
        {
            let Some(active) = profile.peek_active() else {
                return Ok(());
            };
            return profile
                .with_expected(Some(active.principal.as_str()), || changed(app, path, None));
        }
    }
    changed(app, path, None)
}
pub(crate) fn value_changed<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    path: &Path,
    value: Value,
) -> Result<(), String> {
    changed(app, path, Some(value))
}
fn changed<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    path: &Path,
    value: Option<Value>,
) -> Result<(), String> {
    let Some(hub) = app.try_state::<super::LiveHub>() else {
        return Ok(());
    };
    let mut first = true;
    let deltas = with_reads(&hub, |configuration, projections| {
        if std::mem::take(&mut first) {
            configuration.invalidate(path);
        }
        configuration.changed(projections, path, value.clone())
    })?;
    super::publish(app, deltas);
    Ok(())
}
/// Runs `step` under the model lock. Source reads it asks for happen with the lock released,
/// then `step` runs again and finds them deposited, unless their path changed meanwhile.
fn with_reads<T>(
    hub: &super::LiveHub,
    mut step: impl FnMut(&mut Registry, &mut TopicRegistry) -> Result<Step<T>, String>,
) -> Result<T, String> {
    let mut fetched = None;
    loop {
        let read = {
            let mut model = hub.model.lock().map_err(|e| e.to_string())?;
            let super::Model {
                configuration,
                projections,
                ..
            } = &mut *model;
            if let Some(fetched) = fetched.take() {
                configuration.deposit(fetched)?;
            }
            match step(configuration, projections)? {
                Step::Done(done) => return Ok(done),
                Step::Read(read) => read,
            }
        };
        fetched = Some(read.fetch());
    }
}

#[tauri::command]
pub async fn configuration_prepare(
    app: tauri::AppHandle,
    topic: String,
    org: Option<String>,
    expected_principal: Option<String>,
    scope: Option<String>,
) -> Result<Snapshot, String> {
    let (home, user_id) = crate::commands::route_to_context::resolve_ambient(&app).await?;
    let principal = Principal::from(user_id);
    if expected_principal.as_deref() != Some(principal.as_str()) {
        return Err("unavailable: profile changed".into());
    }
    crate::commands::await_blocking("configuration.prepare", move || {
        app.state::<crate::commands::local_profile::LocalProfileState>()
            .with_expected(expected_principal.as_deref(), || {
                let result = if topic == "prefs" {
                    let path = crate::commands::system::app_state_path(&app)?;
                    prepare(
                        &app,
                        path.parent().unwrap(),
                        &principal,
                        &topic,
                        scope.as_deref(),
                    )
                } else {
                    prepare(&app, &home, &principal, &topic, org.as_deref())
                };
                watch_prefs(&app, false);
                result
            })
    })
    .await
}

#[cfg(test)]
mod offlock_tests;
#[cfg(test)]
mod prefs_tests;
#[cfg(test)]
mod tests;
