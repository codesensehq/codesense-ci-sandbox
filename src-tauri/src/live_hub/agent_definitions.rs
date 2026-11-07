// SPDX-License-Identifier: MIT
//! Saved definitions and account-dependent views, shared by native UI and pages.
use super::projection::{Delta, Snapshot, TopicRegistry};
use crate::agents::{self, service, AgentKey, AgentRevisionRef};
use crate::commands::{
    agents::{AgentDetailView, AgentRow},
    routines::Caller,
};
use crate::domain::Principal;
use crate::install::agent_account_store::{self, AgentProvider};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::Path,
    time::SystemTime,
};
use tauri::Manager;

pub const TOPIC: &str = "agentDefinitions";

#[cfg(test)]
mod caller_scope_tests;

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Scope {
    Native,
    Page(Option<(String, String)>),
    PageCaller(String, String),
    PageTeam(String, String, String, String),
}
impl Scope {
    fn admits(
        &self,
        row: &AgentRow,
        caller: &Caller,
        scope: &crate::context_repo::caller_scope::CallerScope,
    ) -> bool {
        match self {
            Self::PageTeam(_, _, org, team) => {
                (matches!(row.scope, agents::AgentScope::Builtin)
                    || (&row.org == org && &row.team == team))
                    && AgentKey::parse(&row.key)
                        .is_some_and(|key| scope.definition(&caller.home, &caller.principal, &key))
            }
            Self::PageCaller(..) => AgentKey::parse(&row.key)
                .is_some_and(|key| scope.definition(&caller.home, &caller.principal, &key)),
            Self::Page(Some((org, team))) => {
                matches!(row.scope, agents::AgentScope::Builtin)
                    || (matches!(row.scope, agents::AgentScope::Team)
                        && &row.org == org
                        && &row.team == team)
            }
            _ => true,
        }
    }
}
struct View {
    principal: String,
    owner: String,
    scope: Scope,
    selected: Option<String>,
    sequence: u64,
    visible: bool,
}
type Loaded = (service::AgentDetail, AgentRevisionRef);
struct Source {
    row: AgentRow,
    choice: Option<agents::resolve::AgentAccountChoice>,
    order: usize,
    loaded: Option<Loaded>,
    detail: Option<AgentDetailView>,
    stamp: Option<(SystemTime, u64)>,
    version: u64,
}
impl Source {
    fn refresh_account(&mut self, caller: &Caller) {
        let choice = agents::resolve::account_preview(
            &caller.home,
            &caller.principal,
            &AgentKey::parse(&self.row.key).unwrap(),
            self.row.runtime,
        );
        self.set_account(choice);
    }
    fn set_account(&mut self, choice: agents::resolve::AgentAccountChoice) {
        self.row.account = (&choice).into();
        self.choice = Some(choice);
        if let Some(detail) = self.detail.as_mut() {
            detail.resolved.account = self.row.account.clone();
        }
    }
    fn resolve_account(
        &mut self,
        profiles: &[agent_account_store::AgentAccountProfile],
        active: Option<&agent_account_store::ActiveAgentAccount>,
    ) {
        let provider = agents::adapter::provider_of(self.row.runtime);
        let policy = agents::resolve::account_policy(self.row.runtime);
        let Some(provider) = provider else {
            let preview = agent_account_store::resolve_without_managed_account();
            self.set_account(agents::resolve::AgentAccountChoice {
                policy,
                override_account_id: None,
                source: agents::resolve::AccountSource::RuntimeDefault,
                preview,
                store_error: None,
                capture_raw_terminal: false,
            });
            return;
        };
        let bound = self.row.account.bound_account_id.clone();
        let generation = bound
            .as_deref()
            .and_then(|id| profiles.iter().find(|p| p.id == id))
            .map(|p| p.generation);
        let preview = agent_account_store::resolve_agent_account(
            policy.requirement,
            provider,
            profiles,
            active,
            bound.as_deref(),
            generation,
        );
        self.set_account(agents::resolve::AgentAccountChoice {
            policy,
            source: if bound.is_some() {
                agents::resolve::AccountSource::Binding
            } else {
                agents::resolve::AccountSource::RuntimeDefault
            },
            override_account_id: bound,
            preview,
            store_error: None,
            capture_raw_terminal: self.row.account.capture_raw_terminal,
        });
    }
}
struct Catalog {
    caller: Caller,
    sources: BTreeMap<String, Source>,
    next_order: usize,
    roots: BTreeSet<(String, String)>,
}
#[derive(Default)]
pub struct Registry {
    catalogs: BTreeMap<String, Catalog>,
    views: BTreeMap<String, View>,
    pending: BTreeMap<String, bool>,
    routines_dirty: bool,
    membership_dirty: bool,
    accounts_dirty: bool,
    routines_revision: u64,
    activity_revision: u64,
    active: String,
    #[cfg(test)]
    reads: usize,
    #[cfg(test)]
    scans: usize,
}
fn stamp(caller: &Caller, key: &AgentKey) -> Option<(SystemTime, u64)> {
    let root = agents::roots::team_agents_dir(&caller.home, &caller.principal, &key.org, &key.team)
        .ok()??;
    let metadata = std::fs::metadata(root.join(&key.id).join(agents::defs::AGENT_FILE)).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}
impl Registry {
    fn seed(
        &mut self,
        topics: &mut TopicRegistry,
        caller: &Caller,
        discover: bool,
    ) -> Result<(), String> {
        let principal = caller.principal.to_string();
        if self.active != principal {
            for key in self.views.keys() {
                topics.remove_view(TOPIC, key);
            }
            self.views.clear();
            self.catalogs.clear();
        }
        self.active = principal.clone();
        if !discover && self.catalogs.contains_key(&principal) {
            return Ok(());
        }
        let roots: BTreeSet<_> = agents::roots::team_roots(&caller.home, &caller.principal)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|(root, _)| (root.org, root.team))
            .collect();
        self.membership_dirty = false;
        if self
            .catalogs
            .get(&principal)
            .is_some_and(|catalog| catalog.roots == roots)
        {
            return Ok(());
        }
        let rows = crate::commands::agents::list(caller, true, &HashMap::new())?;
        #[cfg(test)]
        {
            self.scans += 1;
        }
        let mut previous = self
            .catalogs
            .remove(&principal)
            .map(|c| c.sources)
            .unwrap_or_default();
        let next_order = rows.len();
        let sources = rows
            .into_iter()
            .enumerate()
            .map(|(order, mut row)| {
                let key = AgentKey::parse(&row.key).unwrap();
                let stamp = stamp(caller, &key);
                let before = previous.remove(&row.key);
                row.running_now = before
                    .as_ref()
                    .map_or(row.running_now, |s| s.row.running_now);
                let version = before
                    .as_ref()
                    .map_or(1, |s| s.version + u64::from(s.stamp != stamp));
                let loaded = before.filter(|s| s.stamp == stamp).and_then(|s| s.loaded);
                (
                    row.key.clone(),
                    Source {
                        stamp,
                        row,
                        order,
                        choice: None,
                        loaded,
                        detail: None,
                        version,
                    },
                )
            })
            .collect();
        self.catalogs.insert(
            principal,
            Catalog {
                caller: caller.clone(),
                sources,
                next_order,
                roots,
            },
        );
        Ok(())
    }
    pub(super) fn note(&mut self, key: &str, force: bool) {
        if let Some(key) = AgentKey::parse(key).filter(|k| !agents::builtin::is_starter(k)) {
            if self
                .catalogs
                .values()
                .any(|c| !c.roots.contains(&(key.org.clone(), key.team.clone())))
            {
                self.membership_dirty = true;
            }
        }
        self.pending
            .entry(key.into())
            .and_modify(|f| *f |= force)
            .or_insert(force);
        // An unattended starter may inherit its ordinary twin's account binding.
        if force {
            if let Some(runtime) = codesense_pty_core::agent_cli::AgentCli::ALL
                .into_iter()
                .find(|runtime| agents::builtin::key(*runtime).to_string() == key)
            {
                self.pending
                    .insert(agents::builtin::unattended_key(runtime).to_string(), true);
            }
        }
    }
    pub(super) fn note_membership(&mut self) {
        self.membership_dirty = true;
    }
    /// An account changed: login, re-check, reattach, detach or rename. Every row re-resolves,
    /// since an unpinned row follows the provider's Active account and names neither.
    pub(super) fn note_account(&mut self) {
        self.accounts_dirty = true;
    }
    pub(super) fn note_routines(&mut self) {
        if !self.routines_dirty {
            self.routines_revision += 1;
        }
        self.routines_dirty = true;
    }
    pub(super) fn note_activity(&mut self) {
        self.activity_revision += 1;
    }

    fn refresh(&mut self) -> Result<(), String> {
        let pending = std::mem::take(&mut self.pending);
        let accounts = std::mem::take(&mut self.accounts_dirty);
        for catalog in self.catalogs.values_mut() {
            let mut inserted = BTreeSet::new();
            for (rendered, force) in &pending {
                let Some(key) =
                    AgentKey::parse(rendered).filter(|k| k.resolves_for(&catalog.caller.principal))
                else {
                    continue;
                };
                if !agents::builtin::is_starter(&key)
                    && !catalog.roots.contains(&(key.org.clone(), key.team.clone()))
                {
                    continue;
                }
                let stamp = stamp(&catalog.caller, &key);
                if !force
                    && catalog
                        .sources
                        .get(rendered)
                        .is_some_and(|s| s.stamp == stamp)
                {
                    continue;
                }
                #[cfg(test)]
                {
                    self.reads += 1;
                }
                match service::get_snapshot(&catalog.caller.home, &catalog.caller.principal, &key) {
                    Ok(loaded) => {
                        let previous = catalog.sources.remove(rendered);
                        if previous.is_none() {
                            inserted.insert(rendered.clone());
                        }
                        let choice = agents::resolve::account_preview(
                            &catalog.caller.home,
                            &catalog.caller.principal,
                            &key,
                            loaded.0.agent.runtime,
                        );
                        let row = crate::commands::agents::row_from_detail(
                            &catalog.caller,
                            &loaded.0,
                            previous.as_ref().map(|s| &s.row),
                            (&choice).into(),
                        );
                        let order = previous.as_ref().map_or_else(
                            || {
                                let n = catalog.next_order;
                                catalog.next_order += 1;
                                n
                            },
                            |s| s.order,
                        );
                        catalog.sources.insert(
                            rendered.clone(),
                            Source {
                                row,
                                choice: Some(choice),
                                order,
                                loaded: Some(loaded),
                                detail: None,
                                stamp,
                                version: previous.map_or(1, |s| s.version + 1),
                            },
                        );
                    }
                    Err(service::AgentError::NotFound(_)) => {
                        catalog.sources.remove(rendered);
                    }
                    Err(error) => log::warn!(target: "agents", "definition {rendered}: {error}"),
                }
            }
            if self.routines_dirty || !inserted.is_empty() {
                crate::commands::agents::refresh_facts(
                    &catalog.caller,
                    catalog
                        .sources
                        .values_mut()
                        .filter(|s| self.routines_dirty || inserted.contains(&s.row.key))
                        .map(|s| &mut s.row),
                );
            }
            if accounts {
                let profiles = agent_account_store::list_profiles(
                    &catalog.caller.home,
                    &catalog.caller.principal,
                )
                .map_err(|e| e.to_string())?;
                for provider in AgentProvider::ALL {
                    let mut affected = catalog
                        .sources
                        .values_mut()
                        .filter(|s| agents::adapter::provider_of(s.row.runtime) == Some(provider))
                        .peekable();
                    if affected.peek().is_none() {
                        continue;
                    }
                    let active = agent_account_store::get_active_account(
                        &catalog.caller.home,
                        &catalog.caller.principal,
                        provider,
                    )
                    .map_err(|e| e.to_string())?;
                    for source in affected {
                        source.resolve_account(&profiles, active.as_ref());
                    }
                }
            }
        }
        self.routines_dirty = false;
        let selected: BTreeSet<_> = self
            .views
            .values()
            .filter(|v| v.visible && v.scope == Scope::Native)
            .filter_map(|v| {
                v.selected
                    .as_ref()
                    .map(|key| (v.principal.clone(), key.clone()))
            })
            .collect();
        for (principal, key) in selected {
            if self
                .catalogs
                .get(&principal)
                .is_some_and(|c| c.sources.contains_key(&key))
            {
                self.detail(&principal, &key).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }
    fn detail(&mut self, principal: &str, rendered: &str) -> Result<(), service::AgentError> {
        let key = AgentKey::parse(rendered)
            .ok_or_else(|| service::AgentError::InvalidLocation("invalid agent key".into()))?;
        let catalog = self
            .catalogs
            .get_mut(principal)
            .ok_or_else(|| service::AgentError::Io("unavailable: agent catalog".into()))?;
        let source = catalog
            .sources
            .get_mut(rendered)
            .ok_or_else(|| service::AgentError::NotFound(key.clone()))?;
        if source.detail.is_some() {
            return Ok(());
        }
        if source.loaded.is_none() {
            #[cfg(test)]
            {
                self.reads += 1;
            }
            source.loaded = Some(service::get_snapshot(
                &catalog.caller.home,
                &catalog.caller.principal,
                &key,
            )?);
            let current = stamp(&catalog.caller, &key);
            if current != source.stamp {
                source.version += 1;
            }
            source.stamp = current;
        }
        let loaded = source.loaded.as_ref().unwrap();
        let choice = source.choice.get_or_insert_with(|| {
            agents::resolve::account_preview(
                &catalog.caller.home,
                &catalog.caller.principal,
                &loaded.0.key,
                loaded.0.agent.runtime,
            )
        });
        source.row = crate::commands::agents::row_from_detail(
            &catalog.caller,
            &loaded.0,
            Some(&source.row),
            (&*choice).into(),
        );
        source.detail = Some(
            crate::commands::agents::detail_from_snapshot(
                &catalog.caller,
                loaded.0.clone(),
                loaded.1.clone(),
                choice.clone(),
            )
            .map_err(service::AgentError::Io)?,
        );
        Ok(())
    }
    pub(super) fn project(&self, topics: &mut TopicRegistry) -> Result<Vec<Delta>, String> {
        let mut deltas = Vec::new();
        for (key, view) in &self.views {
            let Some(catalog) = self.catalogs.get(&view.principal) else {
                continue;
            };
            let caller_scope = match &view.scope {
                Scope::PageCaller(org, team) | Scope::PageTeam(org, team, _, _) => {
                    crate::context_repo::caller_scope::CallerScope::team(
                        &catalog.caller.home,
                        &catalog.caller.principal,
                        &crate::commands::dashboard_launch::LaunchTeam {
                            org: org.clone(),
                            team: team.clone(),
                        },
                    )
                }
                _ => Default::default(),
            };
            let mut rows = Vec::new();
            if view.scope == Scope::Native {
                rows.push(json!({"id":"facts","rowType":"facts","routinesRevision":self.routines_revision}));
            } else {
                // The documented callback also wakes role and human-status views.
                rows.push(json!({"id":"compatibility","rowType":"compatibility","activityRevision":self.activity_revision}));
            }
            for source in catalog
                .sources
                .values()
                .filter(|s| view.scope.admits(&s.row, &catalog.caller, &caller_scope))
            {
                let agent = &source.row;
                if !matches!(view.scope, Scope::Native) {
                    if !agent.archived {
                        rows.push(json!({"id":format!("definition:{}",agent.key),"rowType":"definition","key":agent.key,"sourceRevision":source.version,
                            "row":{"key":agent.key,"name":agent.name,"description":agent.description,"runtime":agent.runtime,"scope":agent.scope,"team":agent.team,"avatar":agent.avatar,"runningNow":agent.running_now}}));
                    }
                    continue;
                }
                let mut row = serde_json::to_value(agent).unwrap();
                row.as_object_mut().unwrap().remove("account");
                rows.push(json!({"id":format!("definition:{}",agent.key),"rowType":"definition","key":agent.key,"sourceRevision":source.version,"row":row}));
                rows.push(json!({"id":format!("account:{}",agent.key),"rowType":"account","key":agent.key,"account":agent.account}));
                if view.selected.as_deref() == Some(&agent.key) {
                    if let Some(detail) = &source.detail {
                        let mut detail = serde_json::to_value(detail).unwrap();
                        detail["resolved"]
                            .as_object_mut()
                            .unwrap()
                            .remove("account");
                        rows.push(json!({"id":format!("detail:{}",agent.key),"rowType":"detail","key":agent.key,"detail":detail}));
                    }
                }
            }
            deltas.extend(
                topics.replace_view(
                    TOPIC,
                    key,
                    rows.into_iter()
                        .map(|row| (Some(key.clone()), row))
                        .collect(),
                )?,
            );
        }
        Ok(deltas)
    }
    fn open(
        &mut self,
        topics: &mut TopicRegistry,
        caller: &Caller,
        owner: &str,
        scope: Scope,
        selected: Option<String>,
        visible: bool,
        sequence: u64,
    ) -> Result<(Snapshot, Vec<Delta>), String> {
        self.seed(topics, caller, true)?;
        let suffix = match &scope {
            Scope::Native => "native".into(),
            Scope::Page(team) => format!("frame:{owner}:{}", serde_json::to_string(team).unwrap()),
            Scope::PageCaller(org, team) => format!("frame:{owner}:caller:{org}:{team}"),
            Scope::PageTeam(org, team, source_org, source_team) => {
                format!("frame:{owner}:caller:{org}:{team}:source:{source_org}:{source_team}")
            }
        };
        let key = format!("{TOPIC}:{}:{suffix}", caller.principal);
        let view = self.views.entry(key.clone()).or_insert_with(|| View {
            principal: caller.principal.to_string(),
            owner: owner.into(),
            scope: scope.clone(),
            selected: None,
            sequence: 0,
            visible,
        });
        if sequence >= view.sequence {
            view.sequence = sequence;
            view.visible = visible;
            view.selected = if scope == Scope::Native && visible {
                selected
            } else {
                None
            };
            // Explicit selection re-derives composition; only definition bytes stay cached.
            if let Some(source) = view.selected.as_ref().and_then(|key| {
                self.catalogs
                    .get_mut(caller.principal.as_str())?
                    .sources
                    .get_mut(key)
            }) {
                source.detail = None;
                source.choice = None;
            }
        }
        self.refresh()?;
        let deltas = self.project(topics)?;
        Ok((topics.snapshot(TOPIC, &key)?, deltas))
    }
    pub(super) fn names(&self) -> BTreeMap<String, Value> {
        self.catalogs
            .get(&self.active)
            .into_iter()
            .flat_map(|c| c.sources.values())
            .map(|s| {
                (
                    s.row.key.clone(),
                    json!({"name":s.row.name,"runtime":s.row.runtime}),
                )
            })
            .collect()
    }
    fn account_changed(
        &mut self,
        principal: &Principal,
        provider: AgentProvider,
    ) -> Result<(), String> {
        let Some(catalog) = self.catalogs.get_mut(principal.as_str()) else {
            return Ok(());
        };
        let profiles = agent_account_store::list_profiles(&catalog.caller.home, principal)
            .map_err(|e| e.to_string())?;
        let active =
            agent_account_store::get_active_account(&catalog.caller.home, principal, provider)
                .map_err(|e| e.to_string())?;
        for source in catalog.sources.values_mut().filter(|s| {
            s.row.account.source == "runtime_default"
                && agents::adapter::provider_of(s.row.runtime) == Some(provider)
        }) {
            source.resolve_account(&profiles, active.as_ref());
        }
        Ok(())
    }
    pub(super) fn sessions(&mut self, sessions: &[crate::mcp::session_list::SessionListRow]) {
        use crate::mcp::session_list::SessionState;
        let mut running = HashMap::new();
        for session in sessions.iter().filter(|s| {
            matches!(
                s.state,
                SessionState::Running | SessionState::Idle | SessionState::NeedsYou
            )
        }) {
            if let Some(key) = session.agent.as_deref().and_then(AgentKey::parse) {
                *running.entry(key).or_insert(0) += 1;
            }
        }
        for catalog in self.catalogs.values_mut() {
            for source in catalog.sources.values_mut() {
                source.row.running_now = AgentKey::parse(&source.row.key)
                    .and_then(|key| running.get(&key).copied())
                    .unwrap_or(0);
            }
        }
    }
}

pub(crate) fn membership_changed<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(hub) = app.try_state::<super::LiveHub>() {
        hub.model
            .lock()
            .unwrap()
            .agent_definitions
            .note_membership();
        hub.invalidate(super::REBUILD | super::DEFINITIONS);
    }
}

pub(crate) fn prepare<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    caller: &Caller,
    owner: &str,
    scope: Scope,
    selected: Option<String>,
    visible: bool,
    sequence: u64,
) -> Result<Snapshot, String> {
    if crate::commands::local_profile::active(app).is_ok_and(|p| p.principal != caller.principal) {
        return Err("unavailable: profile changed".into());
    }
    let hub = app.state::<super::LiveHub>();
    let (snapshot, deltas) = {
        let mut model = hub.model.lock().unwrap();
        let super::Model {
            agent_definitions,
            projections,
            ..
        } = &mut *model;
        agent_definitions.open(
            projections,
            caller,
            owner,
            scope,
            selected,
            visible,
            sequence,
        )?
    };
    super::publish(app, deltas);
    Ok(snapshot)
}
pub(super) fn refresh<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    caller: &Caller,
) -> Result<(), String> {
    if crate::commands::local_profile::active(app).is_ok_and(|p| p.principal != caller.principal) {
        return Err("unavailable: profile changed".into());
    }
    let hub = app.state::<super::LiveHub>();
    let deltas = {
        let mut model = hub.model.lock().unwrap();
        let super::Model {
            agent_definitions,
            projections,
            ..
        } = &mut *model;
        let discover = std::mem::take(&mut agent_definitions.membership_dirty);
        agent_definitions.seed(projections, caller, discover)?;
        // The native catalog is also the source of saved names on running sessions.
        if !agent_definitions
            .views
            .contains_key(&format!("{TOPIC}:{}:native", caller.principal))
        {
            agent_definitions
                .open(projections, caller, "native", Scope::Native, None, false, 0)?
                .1
        } else {
            agent_definitions.refresh()?;
            agent_definitions.project(projections)?
        }
    };
    super::publish(app, deltas);
    Ok(())
}
pub(crate) fn account_changed<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    principal: &Principal,
    provider: AgentProvider,
) -> Result<(), String> {
    let Some(hub) = app.try_state::<super::LiveHub>() else {
        return Ok(());
    };
    let deltas = {
        let mut model = hub.model.lock().unwrap();
        let super::Model {
            agent_definitions,
            projections,
            ..
        } = &mut *model;
        agent_definitions.account_changed(principal, provider)?;
        agent_definitions.project(projections)?
    };
    super::publish(app, deltas);
    Ok(())
}
pub(crate) fn list<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    caller: &Caller,
    archived: bool,
) -> Result<Vec<AgentRow>, String> {
    {
        let hub = app.state::<super::LiveHub>();
        hub.model.lock().unwrap().agent_definitions.membership_dirty = true;
    }
    refresh(app, caller)?;
    let hub = app.state::<super::LiveHub>();
    let mut model = hub.model.lock().unwrap();
    for source in model
        .agent_definitions
        .catalogs
        .get_mut(caller.principal.as_str())
        .unwrap()
        .sources
        .values_mut()
    {
        source.refresh_account(caller);
    }
    let mut sources: Vec<_> = model.agent_definitions.catalogs[caller.principal.as_str()]
        .sources
        .values()
        .filter(|s| archived || !s.row.archived)
        .collect();
    sources.sort_by_key(|s| s.order);
    let rows = sources.into_iter().map(|s| s.row.clone()).collect();
    let super::Model {
        agent_definitions,
        projections,
        ..
    } = &mut *model;
    let deltas = agent_definitions.project(projections)?;
    drop(model);
    super::publish(app, deltas);
    Ok(rows)
}
pub(crate) fn detail<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    caller: &Caller,
    key: &str,
) -> Result<AgentDetailView, service::AgentError> {
    refresh(app, caller).map_err(service::AgentError::Io)?;
    let hub = app.state::<super::LiveHub>();
    let (detail, deltas) = {
        let mut model = hub.model.lock().unwrap();
        let super::Model {
            agent_definitions,
            projections,
            ..
        } = &mut *model;
        if let Some(source) = agent_definitions
            .catalogs
            .get_mut(caller.principal.as_str())
            .and_then(|c| c.sources.get_mut(key))
        {
            source.detail = None;
            source.choice = None;
        }
        agent_definitions.detail(caller.principal.as_str(), key)?;
        (
            agent_definitions.catalogs[caller.principal.as_str()].sources[key]
                .detail
                .clone()
                .unwrap(),
            agent_definitions
                .project(projections)
                .map_err(service::AgentError::Io)?,
        )
    };
    super::publish(app, deltas);
    Ok(detail)
}
pub(crate) fn release<R: tauri::Runtime>(app: &tauri::AppHandle<R>, owner: &str) {
    let Some(hub) = app.try_state::<super::LiveHub>() else {
        return;
    };
    let mut model = hub.model.lock().unwrap();
    let keys: Vec<_> = model
        .agent_definitions
        .views
        .iter()
        .filter(|(_, v)| v.owner == owner)
        .map(|(key, _)| key.clone())
        .collect();
    for key in keys {
        model.agent_definitions.views.remove(&key);
        model.projections.remove_view(TOPIC, &key);
    }
}

/// Parse only the canonical config tree; a content copy is never definition authority.
pub(crate) fn key_at_path(
    principal: &Principal,
    team_dir: &Path,
    org: &str,
    team: &str,
    path: &Path,
) -> Option<AgentKey> {
    let relative = path.strip_prefix(team_dir).ok()?;
    let parts: Vec<_> = relative.iter().map(|p| p.to_str()).collect::<Option<_>>()?;
    if parts.len() < 3
        || parts[0] != "config"
        || parts[1] != "agents"
        || !agents::model::is_valid_id(parts[2])
    {
        return None;
    }
    match &parts[3..] {
        ["agent.yaml"] | [] => {}
        _ => return None,
    }
    Some(AgentKey::scoped(principal, org, team, parts[2]))
}
pub(crate) fn note_path<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    principal: &Principal,
    team_dir: &Path,
    org: &str,
    team: &str,
    path: &Path,
) {
    if let Some(key) = key_at_path(principal, team_dir, org, team, path) {
        if let Some(hub) = app.try_state::<super::LiveHub>() {
            hub.model
                .lock()
                .unwrap()
                .agent_definitions
                .note(&key.to_string(), false);
            hub.invalidate(super::REBUILD | super::DEFINITIONS);
        }
    }
}

#[tauri::command]
pub async fn agent_definitions_prepare(
    app: tauri::AppHandle,
    frame_id: Option<String>,
    detail_key: Option<String>,
    visible: Option<bool>,
    sequence: Option<u64>,
) -> Result<Snapshot, String> {
    let (home, principal) = crate::commands::route_to_context::resolve_ambient(&app).await?;
    let caller = Caller {
        home,
        principal: principal.into(),
    };
    let (owner, scope) = if let Some(id) = frame_id {
        let frame = app
            .state::<std::sync::Arc<crate::dashboard_stream::frame::FrameStore>>()
            .by_id(&id)
            .ok_or("denied: frame released")?;
        crate::verbs::check_frame_capability(&frame, "agents", &json!({}))
            .map_err(str::to_owned)?;
        if frame
            .records_binding
            .as_ref()
            .is_none_or(|binding| binding.profile != caller.principal.as_str())
        {
            return Err("denied: frame profile changed".into());
        }
        let host = crate::context_repo::caller_scope::for_page(&app, None, Some(&id))?;
        let (org, team) = match host {
            crate::context_repo::caller_scope::CallerScope::Team(view) => {
                (view.org().to_string(), view.reader_team().to_string())
            }
            _ => (String::new(), String::new()),
        };
        let scope = if frame.grant.scope == "team" {
            let source = frame.grant.team.ok_or("denied: frame has no team")?;
            Scope::PageTeam(org, team, source.org, source.team)
        } else {
            Scope::PageCaller(org, team)
        };
        (id, scope)
    } else {
        ("native".into(), Scope::Native)
    };
    crate::commands::await_blocking("agent_definitions.prepare", move || {
        prepare(
            &app,
            &caller,
            &owner,
            scope,
            detail_key,
            visible.unwrap_or(true),
            sequence.unwrap_or(0),
        )
    })
    .await
}

#[cfg(test)]
mod tests;
