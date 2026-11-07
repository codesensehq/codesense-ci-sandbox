// SPDX-License-Identifier: MIT
//! Pane identities and source revisions, shared by native slot consumers.
use super::projection::{Delta, Snapshot, TopicRegistry};
use crate::context_repo::{path_to_org_root, scratch_space};
use crate::domain::Principal;
use crate::panes::{
    self, watch::PaneChange, DashboardRef, DashboardScope, PaneEntry, PaneListing, PaneScope,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use tauri::Manager;

pub const TOPIC: &str = "paneSources";
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ReaderKey {
    home: PathBuf,
    org_root: PathBuf,
    principal: Principal,
    org: String,
    team: String,
}
type SourceKey = (ReaderKey, String);

pub(crate) struct ViewRequest<'a> {
    pub home: &'a Path,
    pub principal: &'a Principal,
    pub owner: &'a str,
    pub sequence: u64,
    pub org: &'a str,
    pub team: &'a str,
    pub selected: Vec<DashboardRef>,
    pub retry: Option<DashboardRef>,
}

struct Source {
    reference: DashboardRef,
    entry: Option<PaneEntry>,
    revision: u64,
    document_revision: u64,
}
#[derive(Clone)]
struct View {
    sequence: u64,
    home: PathBuf,
    principal: Principal,
    org: String,
    team: String,
    context: PaneListing,
    org_root: PathBuf,
    selected: Vec<DashboardRef>,
}
impl View {
    fn reader_key(&self) -> ReaderKey {
        ReaderKey {
            home: self.home.clone(),
            org_root: self.org_root.clone(),
            principal: self.principal.clone(),
            org: self.org.clone(),
            team: self.team.clone(),
        }
    }
    fn inheritance(&self) -> std::sync::Arc<crate::context_repo::inheritance::TeamView> {
        crate::context_repo::inheritance::team_view(&self.org_root, &self.org, &self.team)
    }
    fn authority_root(&self, reference: &DashboardRef) -> Result<String, String> {
        if reference.scope == DashboardScope::Team {
            self.inheritance()
                .pane_content_root(reference.team.as_deref().ok_or("Missing team")?)
                .map(|path| path.to_string_lossy().into_owned())
                .ok_or_else(|| "not found".into())
        } else {
            Ok(self.context.content_root.clone())
        }
    }
    fn file_for(&self, reference: &DashboardRef) -> Option<PathBuf> {
        let view = self.inheritance();
        match reference.scope {
            DashboardScope::Team => view.pane_file(reference.team.as_deref()?, &reference.id),
            DashboardScope::Private => {
                let space =
                    scratch_space::org_space_at(&self.home, &self.principal, &reference.org)
                        .ok()?;
                let (dir, id) = if let Some(source) = &reference.installed_from {
                    (
                        space
                            .panes_dir()
                            .join("installed")
                            .join(source.team.as_deref()?)
                            .join(&source.id),
                        source.id.as_str(),
                    )
                } else {
                    (space.panes_dir(), reference.id.as_str())
                };
                let path = dir.join(format!("{id}.html"));
                view.read_path(&path).ok()?;
                Some(path)
            }
            DashboardScope::BuiltIn => None,
        }
    }
    fn visible(&self, reference: &DashboardRef) -> bool {
        reference.org == self.org
            && (reference.scope == DashboardScope::BuiltIn || self.file_for(reference).is_some())
    }
    fn lists_identity(&self, reference: &DashboardRef) -> bool {
        reference.org == self.org
            && match reference.scope {
                DashboardScope::Team => reference
                    .team
                    .as_ref()
                    .is_some_and(|t| self.context.chain.contains(t)),
                _ => true,
            }
    }
    fn lists(&self, reference: &DashboardRef) -> bool {
        self.lists_identity(reference) && self.visible(reference)
    }
    fn admits(&self, reference: &DashboardRef) -> bool {
        self.lists_identity(reference) || self.selected.iter().any(|r| r.key() == reference.key())
    }
}

/// How many documents whose last live source was pruned stay remembered. A reader that leaves an
/// org and comes back finds its panes again, so a small recent window is enough; the oldest
/// is forgotten first.
const PRUNED_DOCUMENT_CAP: usize = 64;

/// A document whose last live source was pruned: the entry it served and the revision it had.
struct PrunedDocument {
    key: String,
    reference: DashboardRef,
    entry: PaneEntry,
    revision: u64,
}

#[derive(Default)]
pub struct Registry {
    pruned: Vec<PrunedDocument>,
    views: BTreeMap<String, View>,
    sources: BTreeMap<SourceKey, Source>,
    revision: u64,
    document_revision: u64,
    document_revisions: BTreeMap<String, u64>,
    #[cfg(test)]
    entry_reads: usize,
    #[cfg(test)]
    layer_scans: usize,
}
impl PrunedDocument {
    fn entry_matches(&self, entry: Option<&PaneEntry>) -> bool {
        entry == Some(&self.entry)
    }
}
impl Registry {
    fn put(
        &mut self,
        view: &View,
        reference: DashboardRef,
        entry: Option<PaneEntry>,
        changed: bool,
    ) {
        let key = (view.reader_key(), reference.key());
        let reference = self
            .sources
            .get(&key)
            .map(|s| s.reference.clone())
            .unwrap_or(reference);
        let document_key = reference.key();
        let previous_document = self
            .sources
            .values()
            .find(|source| source.reference.key() == document_key)
            .map(|source| source.entry.as_ref());
        let mut document_changed =
            previous_document.is_none_or(|previous| previous != entry.as_ref());
        if entry.is_some() {
            // A document that was pruned and comes back byte-for-byte as it left keeps its
            // revision: a renumber would re-mint the page for every reader that returns.
            let pruned = self
                .pruned
                .iter()
                .position(|p| p.key == document_key)
                .map(|at| self.pruned.remove(at));
            if let Some(pruned) = pruned {
                if document_changed && pruned.entry_matches(entry.as_ref()) {
                    self.document_revisions
                        .insert(document_key.clone(), pruned.revision);
                    document_changed = false;
                }
            }
        }
        if document_changed {
            self.document_revision += 1;
            self.document_revisions
                .insert(document_key.clone(), self.document_revision);
        }
        if !self.document_revisions.contains_key(&document_key) {
            self.document_revision += 1;
            self.document_revisions
                .insert(document_key.clone(), self.document_revision);
        }
        let document_revision = self.document_revisions[&document_key];
        let mut visible_readers: BTreeSet<_> = self
            .views
            .values()
            .filter(|reader| reader.visible(&reference))
            .map(View::reader_key)
            .collect();
        visible_readers.insert(key.0.clone());
        for ((reader, _), source) in &mut self.sources {
            if source.reference.key() == document_key
                && source.entry.is_some()
                && visible_readers.contains(reader)
            {
                source.document_revision = document_revision;
            }
        }
        if self
            .sources
            .get(&key)
            .is_some_and(|s| s.entry == entry && (!changed || entry.is_none()))
        {
            return;
        }
        self.revision += 1;
        self.sources.insert(
            key,
            Source {
                reference,
                entry,
                revision: self.revision,
                document_revision,
            },
        );
    }
    fn read(&mut self, view: &View, reference: &DashboardRef) -> Result<Option<PaneEntry>, String> {
        let key = (view.reader_key(), reference.key());
        let reference = self
            .sources
            .get(&key)
            .map(|s| s.reference.clone())
            .unwrap_or_else(|| reference.clone());
        if let Some(team) = &reference.team {
            if let Some(reason) = crate::content_engine::validate_path_segment(team) {
                return Err(reason.into());
            }
        }
        if !panes::is_valid_pane_id(&reference.id) {
            return Err("Invalid pane id".into());
        }
        if reference.scope == DashboardScope::BuiltIn {
            return Ok(
                panes::builtin_panes(&view.home, &view.principal, &reference.org)
                    .into_iter()
                    .find(|p| p.dashboard_ref.key() == reference.key()),
            );
        }
        let Some(path) = view.file_for(&reference) else {
            return Ok(None);
        };
        let dir = path.parent().ok_or("Missing pane directory")?;
        let scope = if reference.scope == DashboardScope::Team {
            PaneScope::Team
        } else {
            PaneScope::Private
        };
        #[cfg(test)]
        {
            self.entry_reads += 1;
        }
        let inheritance = view.inheritance();
        let mut entry = panes::read_pane_for_view(
            &inheritance,
            &path,
            scope,
            &reference.org,
            reference.team.as_deref(),
        );
        if let Some(entry) = &mut entry {
            entry.dashboard_ref = reference.clone();
            if reference.installed_from.is_some() {
                entry.id = reference.id.clone();
                entry.panel_id =
                    panes::pane_panel_id(PaneScope::Private, &reference.org, None, &reference.id);
                entry.install_source_version = inheritance
                    .read_path(&dir.join("install.json"))
                    .ok()
                    .and_then(|_| panes::install::source_version(dir));
                if let Some(previous) = self.sources.get(&key).and_then(|s| s.entry.as_ref()) {
                    entry.update_available = previous.update_available;
                }
            }
        }
        Ok(entry)
    }
    // Registry edits and explicit refreshes reconcile membership, never every pane ping.
    fn reconcile_chain(&mut self, view: &mut View) -> Result<(), String> {
        let chain = if scratch_space::is_scratch_team(&view.team) {
            Vec::new()
        } else {
            view.inheritance().team_names()
        };
        if chain == view.context.chain {
            return Ok(());
        }
        let reader = view.reader_key();
        for team in chain
            .iter()
            .filter(|team| !view.context.chain.contains(team))
        {
            let cached = self
                .views
                .values()
                .find(|v| v.reader_key() == reader && v.context.chain.contains(team))
                .map(|v| {
                    v.context
                        .manifests
                        .iter()
                        .filter(|m| m.team.as_ref() == Some(team))
                        .cloned()
                        .collect::<Vec<_>>()
                });
            let manifests = if let Some(manifests) = cached {
                manifests
            } else {
                #[cfg(test)]
                {
                    self.layer_scans += 1;
                }
                let (entries, manifests) =
                    panes::discover_team_panes(&view.org_root, &view.org, &view.team, team);
                for entry in entries {
                    self.put(view, entry.dashboard_ref.clone(), Some(entry), false);
                }
                manifests
            };
            view.context.manifests.extend(manifests);
        }
        view.context.chain = chain;
        view.context.manifests.retain(|m| {
            m.scope == PaneScope::Private
                || m.team
                    .as_ref()
                    .is_some_and(|t| view.context.chain.contains(t))
        });
        let chain = &view.context.chain;
        view.context.manifests.sort_by_key(|m| {
            chain
                .iter()
                .position(|t| Some(t) == m.team.as_ref())
                .unwrap_or(usize::MAX)
        });
        Ok(())
    }
    fn registry_changed(
        &mut self,
        topics: &mut TopicRegistry,
        principal: &Principal,
        org: &str,
    ) -> Result<Vec<Delta>, String> {
        // A registry edit can remove or rename a team (or the org's membership), so a remembered
        // pane of that org may name a team that no longer exists. Dropping them costs at most one
        // renumber on return; the memory is capped at PRUNED_DOCUMENT_CAP either way, so this is
        // tidiness, not a leak guard.
        self.pruned.retain(|p| p.reference.org != org);
        let keys: Vec<_> = self
            .views
            .iter()
            .filter(|(_, v)| &v.principal == principal && v.org == org)
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            let mut view = self.views.remove(&key).unwrap();
            let result = self.reconcile_chain(&mut view);
            self.views.insert(key, view);
            result?;
        }
        self.prune();
        self.project(topics)
    }
    pub(crate) fn open(
        &mut self,
        topics: &mut TopicRegistry,
        request: ViewRequest<'_>,
    ) -> Result<(PaneListing, Snapshot, Vec<Delta>), String> {
        let ViewRequest {
            home,
            principal,
            owner,
            sequence,
            org,
            team,
            selected,
            retry,
        } = request;
        let key = format!("paneSources:{}:{owner}", principal.as_str());
        if let Some(view) = self
            .views
            .get(&key)
            .filter(|view| sequence <= view.sequence)
        {
            return Ok((
                self.listing(view),
                topics.snapshot(TOPIC, &key)?,
                Vec::new(),
            ));
        }
        let org_root = path_to_org_root(home, principal, org)?;
        let cached = self
            .views
            .values()
            .find(|v| {
                v.principal == *principal
                    && v.org == org
                    && v.team == team
                    && v.home == home
                    && v.org_root == org_root
            })
            .map(|v| v.context.clone());
        self.views.remove(&key);
        let mut context = if let Some(context) = cached {
            context
        } else {
            #[cfg(test)]
            {
                self.layer_scans += 1;
            }
            // Cache identities independently; listing() applies built-in shadowing live.
            let mut listing = panes::list_space_panes(home, principal, org, team)?;
            listing
                .panes
                .extend(panes::builtin_panes(home, principal, org));
            listing
        };
        let entries = std::mem::take(&mut context.panes);
        let mut view = View {
            sequence,
            home: home.to_owned(),
            principal: principal.clone(),
            org: org.into(),
            team: team.into(),
            context,
            org_root,
            selected: selected.into_iter().filter(|r| r.org == org).collect(),
        };
        self.reconcile_chain(&mut view)?;
        for entry in entries {
            let reference = entry.dashboard_ref.clone();
            self.put(&view, reference, Some(entry), false);
        }
        for reference in view.selected.iter().filter(|r| view.admits(r)) {
            if !self
                .sources
                .contains_key(&(view.reader_key(), reference.key()))
            {
                let entry = self.read(&view, reference)?;
                self.put(&view, reference.clone(), entry, false);
            }
        }
        if let Some(reference) = retry.filter(|r| view.admits(r)) {
            let entry = self.read(&view, &reference)?;
            self.put(&view, reference, entry, false);
        }
        self.views.insert(key.clone(), view);
        self.prune();
        let deltas = self.project(topics)?;
        let listing = self.listing(&self.views[&key]);
        Ok((listing, topics.snapshot(TOPIC, &key)?, deltas))
    }
    fn listing(&self, view: &View) -> PaneListing {
        let mut listing = view.context.clone();
        let reader = view.reader_key();
        listing.panes = self
            .sources
            .iter()
            .filter(|((key, _), s)| key == &reader && view.lists(&s.reference))
            .filter_map(|(_, s)| s.entry.clone())
            .collect();
        let shadowed: BTreeSet<_> = listing
            .panes
            .iter()
            .filter(|p| p.dashboard_ref.scope != DashboardScope::BuiltIn)
            .map(|p| p.id.clone())
            .collect();
        listing.panes.retain(|p| {
            p.dashboard_ref.scope != DashboardScope::BuiltIn || !shadowed.contains(&p.id)
        });
        listing.panes.sort_by_key(|p| {
            (
                if p.dashboard_ref.scope == DashboardScope::BuiltIn {
                    usize::MAX
                } else if p.scope == PaneScope::Private {
                    usize::MAX - 1
                } else {
                    listing
                        .chain
                        .iter()
                        .position(|t| Some(t) == p.team.as_ref())
                        .unwrap_or(usize::MAX)
                },
                p.id.clone(),
            )
        });
        listing
    }
    fn project(&self, topics: &mut TopicRegistry) -> Result<Vec<Delta>, String> {
        let mut deltas = Vec::new();
        for (key, view) in &self.views {
            let listing = self.listing(view);
            let listed: BTreeSet<_> = listing
                .panes
                .iter()
                .map(|p| p.dashboard_ref.key())
                .collect();
            let mut context = view.context.clone();
            context.panes.clear();
            let mut rows = vec![json!({"id":"view", "listing":context})];
            let reader = view.reader_key();
            for (_, source) in self
                .sources
                .iter()
                .filter(|((key, _), source)| key == &reader && view.admits(&source.reference))
            {
                let entry = source
                    .entry
                    .as_ref()
                    .filter(|_| view.visible(&source.reference));
                rows.push(json!({ "id":source.reference.key(), "dashboardRef":source.reference, "entry":entry,
                        "contentRoot":view.authority_root(&source.reference)?, "sourceRevision":source.revision,
                        "documentRevision":source.document_revision,
                    "listed":listed.contains(&source.reference.key()) }));
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
    fn remember_pruned(&mut self, key: String, entry: PaneEntry, revision: u64) {
        self.pruned.retain(|p| p.key != key);
        if self.pruned.len() >= PRUNED_DOCUMENT_CAP {
            self.pruned.remove(0);
        }
        let reference = entry.dashboard_ref.clone();
        self.pruned.push(PrunedDocument {
            key,
            reference,
            entry,
            revision,
        });
    }
    fn prune(&mut self) {
        let live = |sources: &BTreeMap<SourceKey, Source>| -> BTreeMap<String, PaneEntry> {
            sources
                .values()
                .filter_map(|s| Some((s.reference.key(), s.entry.clone()?)))
                .collect()
        };
        let before = live(&self.sources);
        self.prune_sources();
        let after = live(&self.sources);
        for (key, entry) in before {
            if !after.contains_key(&key) {
                let revision = self.document_revisions.get(&key).copied().unwrap_or(0);
                self.remember_pruned(key, entry, revision);
            }
        }
    }
    fn prune_sources(&mut self) {
        for ((reader, _), source) in &mut self.sources {
            if !self
                .views
                .values()
                .any(|v| &v.reader_key() == reader && v.visible(&source.reference))
            {
                source.entry = None;
            }
        }
        self.sources.retain(|(reader, _), source| {
            self.views.values().any(|v| {
                &v.reader_key() == reader
                    && v.admits(&source.reference)
                    && (source.entry.is_some()
                        || v.selected.iter().any(|r| r.key() == source.reference.key()))
            })
        });
    }
    fn recover_sources(
        &mut self,
        topics: &mut TopicRegistry,
        scope: Option<&Path>,
        activation: bool,
    ) -> Result<Vec<Delta>, String> {
        let views: Vec<View> = self.views.values().cloned().collect();
        let mut read = BTreeSet::new();
        for view in &views {
            if activation
                && !scope.is_some_and(|scope| {
                    let own = view.inheritance().own_team_dir();
                    crate::content_engine::dispatcher::stable_key(&own)
                        == crate::content_engine::dispatcher::stable_key(scope)
                })
            {
                continue;
            }
            let reader = view.reader_key();
            let references: BTreeMap<String, DashboardRef> = self
                .sources
                .iter()
                .filter(|((key, _), source)| {
                    key == &reader && View::admits(view, &source.reference)
                })
                .map(|(_, source)| {
                    (
                        DashboardRef::key(&source.reference),
                        source.reference.clone(),
                    )
                })
                .chain(
                    view.selected
                        .iter()
                        .map(|reference| (DashboardRef::key(reference), reference.clone())),
                )
                .collect();
            for reference in references.into_values() {
                if reference.scope == DashboardScope::BuiltIn {
                    continue;
                }
                let path = view.file_for(&reference).or_else(|| match reference.scope {
                    DashboardScope::Team => view
                        .inheritance()
                        .pane_recovery_path(reference.team.as_ref()?, &reference.id),
                    DashboardScope::Private => {
                        let space: scratch_space::ScratchOrgSpace =
                            Result::ok(scratch_space::org_space_at(
                                &view.home,
                                &view.principal,
                                &reference.org,
                            ))?;
                        let mut dir = space.panes_dir();
                        let id = if let Some(source) = &reference.installed_from {
                            dir = dir
                                .join("installed")
                                .join(source.team.as_ref()?)
                                .join(&source.id);
                            &source.id
                        } else {
                            &reference.id
                        };
                        Some(dir.join(format!("{id}.html")))
                    }
                    DashboardScope::BuiltIn => None,
                });
                if !activation
                    && !scope.is_none_or(|scope| {
                        path.as_ref().is_some_and(|path| {
                            crate::content_engine::dispatcher::paths_overlap(path, scope)
                        })
                    })
                {
                    continue;
                }
                if read.insert((reader.clone(), reference.key())) {
                    let entry = self.read(view, &reference)?;
                    self.put(view, reference, entry, false);
                }
            }
        }
        self.prune();
        self.project(topics)
    }
    pub fn changed(
        &mut self,
        topics: &mut TopicRegistry,
        change: &PaneChange,
    ) -> Result<Vec<Delta>, String> {
        let matches = |reference: &DashboardRef| {
            reference.org == change.org
                && match change.scope {
                    PaneScope::Team => {
                        reference.scope == DashboardScope::Team && reference.team == change.team
                    }
                    PaneScope::Private => reference.scope == DashboardScope::Private,
                }
                && change.id.as_ref().is_none_or(|id| &reference.id == id)
                && change.installed.as_ref().is_none_or(|(team, id)| {
                    reference.installed_from.as_ref().is_some_and(|source| {
                        source.team.as_ref() == Some(team) && &source.id == id
                    })
                })
        };
        // Pane files changed on disk may be ones nobody is reading; a remembered copy is stale.
        self.pruned.retain(|p| !matches(&p.reference));
        let views = std::mem::take(&mut self.views);
        let mut read = BTreeSet::new();
        let mut manifests = BTreeMap::new();
        let mut scanned = BTreeSet::new();
        let mut changed_document_keys = BTreeSet::new();
        for view in views.values().filter(|v| v.org == change.org) {
            let reader = view.reader_key();
            let mut refs: Vec<_> = self
                .sources
                .iter()
                .filter(|((key, _), s)| {
                    key == &reader && view.admits(&s.reference) && matches(&s.reference)
                })
                .map(|(_, s)| s.reference.clone())
                .collect();
            if let Some((team, id)) = change.installed.as_ref().filter(|_| refs.is_empty()) {
                let dir = scratch_space::org_space_at(&view.home, &view.principal, &view.org)
                    .map_err(|e| e.to_string())?
                    .panes_dir()
                    .join("installed")
                    .join(team)
                    .join(id);
                if view
                    .inheritance()
                    .read_path(&dir.join("install.json"))
                    .is_ok()
                {
                    if let Some(reference) =
                        panes::install::reference_at(&dir).filter(|r| view.admits(r))
                    {
                        refs.push(reference);
                    }
                }
            }
            if let Some(id) = &change.id {
                let reference = DashboardRef {
                    scope: if change.scope == PaneScope::Team {
                        DashboardScope::Team
                    } else {
                        DashboardScope::Private
                    },
                    org: change.org.clone(),
                    team: change.team.clone(),
                    id: id.clone(),
                    installed_from: None,
                };
                if view.admits(&reference) && !refs.iter().any(|r| r.key() == reference.key()) {
                    refs.push(reference);
                }
            }
            if change.id.is_none() && change.declarations && change.installed.is_none() {
                let active_layer = change.scope == PaneScope::Private
                    || change
                        .team
                        .as_ref()
                        .is_some_and(|t| view.context.chain.contains(t));
                if active_layer {
                    if !scanned.insert((reader.clone(), change.team.clone())) {
                        continue;
                    }
                    #[cfg(test)]
                    {
                        self.layer_scans += 1;
                    }
                    let (entries, overlays) = if change.scope == PaneScope::Team {
                        panes::discover_team_panes(
                            &view.org_root,
                            &view.org,
                            &view.team,
                            change.team.as_deref().ok_or("Missing team")?,
                        )
                    } else {
                        let dir =
                            scratch_space::org_space_at(&view.home, &view.principal, &view.org)
                                .map_err(|e| e.to_string())?
                                .panes_dir();
                        panes::discover_private_panes(&dir, &view.org, Some(&view.inheritance()))
                    };
                    manifests.insert(
                        reader.clone(),
                        overlays
                            .into_iter()
                            .find(|m| m.scope == change.scope && m.team == change.team)
                            .map(|m| m.manifest),
                    );
                    let present: BTreeSet<_> =
                        entries.iter().map(|e| e.dashboard_ref.key()).collect();
                    for reference in &refs {
                        if !present.contains(&reference.key()) {
                            self.put(view, reference.clone(), None, true);
                        }
                    }
                    for entry in entries {
                        let reference = entry.dashboard_ref.clone();
                        self.put(view, reference.clone(), Some(entry), true);
                        read.insert((reader.clone(), reference.key()));
                    }
                    continue;
                }
            }
            for reference in refs {
                let key = (reader.clone(), reference.key());
                if !read.insert(key.clone()) {
                    continue;
                }
                if change.id.is_some() || change.declarations {
                    let entry = self.read(view, &reference)?;
                    self.put(view, reference, entry, true);
                } else if view.visible(&reference) {
                    if let Some(source) = self.sources.get_mut(&key).filter(|s| s.entry.is_some()) {
                        self.revision += 1;
                        source.revision = self.revision;
                        changed_document_keys.insert(reference.key());
                    }
                }
            }
        }
        for document_key in changed_document_keys {
            self.document_revision += 1;
            self.document_revisions
                .insert(document_key.clone(), self.document_revision);
            for ((reader, _), source) in &mut self.sources {
                if source.reference.key() == document_key
                    && source.entry.is_some()
                    && views
                        .values()
                        .any(|view| view.reader_key().eq(reader) && view.visible(&source.reference))
                {
                    source.document_revision = self.document_revision;
                }
            }
        }
        self.views = views;
        for view in self.views.values_mut().filter(|v| v.org == change.org) {
            if let Some(manifest) = manifests.get(&view.reader_key()) {
                view.context
                    .manifests
                    .retain(|m| m.scope != change.scope || m.team != change.team);
                if let Some(manifest) = manifest {
                    view.context.manifests.push(panes::PaneManifestLayer {
                        scope: change.scope,
                        team: change.team.clone(),
                        manifest: manifest.clone(),
                    });
                }
            }
        }
        self.prune();
        self.project(topics)
    }
    fn release(&mut self, topics: &mut TopicRegistry, key: &str) {
        self.views.remove(key);
        topics.remove_view(TOPIC, key);
        self.prune();
    }
}

pub(crate) fn registry_changed<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    principal: &Principal,
    org: &str,
) -> Result<(), String> {
    let Some(hub) = app.try_state::<super::LiveHub>() else {
        return Ok(());
    };
    let deltas = {
        let mut model = hub.model.lock().map_err(|e| e.to_string())?;
        let super::Model {
            pane_sources,
            projections,
            ..
        } = &mut *model;
        pane_sources.registry_changed(projections, principal, org)?
    };
    super::publish(app, deltas);
    Ok(())
}

pub(crate) fn recover<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    scope: Option<&Path>,
    activation: bool,
) -> Result<(), String> {
    let Some(hub) = app.try_state::<super::LiveHub>() else {
        return Ok(());
    };
    let deltas = {
        let mut model = hub.model.lock().map_err(|e| e.to_string())?;
        let super::Model {
            pane_sources,
            projections,
            ..
        } = &mut *model;
        pane_sources.recover_sources(projections, scope, activation)?
    };
    super::publish(app, deltas);
    Ok(())
}

pub(crate) fn changed<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    change: &PaneChange,
) -> Result<(), String> {
    let Some(hub) = app.try_state::<super::LiveHub>() else {
        return Ok(());
    };
    let deltas = {
        let mut model = hub.model.lock().map_err(|e| e.to_string())?;
        let super::Model {
            pane_sources,
            projections,
            ..
        } = &mut *model;
        pane_sources.changed(projections, change)?
    };
    super::publish(app, deltas);
    Ok(())
}
pub(crate) fn release<R: tauri::Runtime>(app: &tauri::AppHandle<R>, key: &str) {
    let hub = app.state::<super::LiveHub>();
    let mut model = hub.model.lock().unwrap();
    let super::Model {
        pane_sources,
        projections,
        ..
    } = &mut *model;
    pane_sources.release(projections, key);
}

pub(crate) fn open<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    request: ViewRequest<'_>,
) -> Result<Value, String> {
    let hub = app.state::<super::LiveHub>();
    let (listing, snapshot, deltas) = {
        let mut model = hub.model.lock().map_err(|e| e.to_string())?;
        let super::Model {
            pane_sources,
            projections,
            ..
        } = &mut *model;
        pane_sources.open(projections, request)?
    };
    super::publish(app, deltas);
    let mut result = serde_json::to_value(listing).map_err(|e| e.to_string())?;
    result["sources"] = serde_json::to_value(snapshot).map_err(|e| e.to_string())?;
    Ok(result)
}

#[cfg(test)]
mod tests;
