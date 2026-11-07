// SPDX-License-Identifier: MIT
//! SW-XS-8: Step 3 — the want sets of explicit hydrates (SPEC §5, §11.1,
//! §11.11). An explicit hydrate is the same engine as an agent launch
//! ([`super::prepare`]) with a caller-supplied want set and a
//! `step3:<operationId>` origin, so what it writes enters the managed record
//! and the workspace's exclude block exactly like Step 2's copies:
//!
//! - `hydrate_worktree {paths | sets}`: the named files and context sets,
//!   resolved through the same tiers as an agent's `context` (the target
//!   repo's tier, the user's scratch space, the team chain);
//! - `hydrate_worktree` with neither: the runtime defaults (what a plain
//!   `claude` or `codex` would get, SPEC §12.2) — the remap of the legacy
//!   scope-less forms;
//! - `hydrate_story`: the story's epic closure, file by file.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::want::WantSet;
use crate::agents::context::{
    default_context, normalize_entries, unsafe_destination, ContextEntry,
};
use crate::agents::manifest_resolve::{
    content_layers, ContextRequest, ContextResolution, ManifestTarget,
};
use crate::domain::Principal;
use codesense_pty_core::agent_cli::AgentCli;

/// At most this many paths and sets one explicit hydrate may name.
pub const MAX_SELECTION: usize = 1_024;

/// An agent-shaped carrier for a caller-supplied context: resolution treats
/// it exactly like a saved agent's `context` (no startup paths).
fn carrier(entries: Vec<ContextEntry>) -> crate::agents::Agent {
    crate::agents::Agent {
        id: "explicit-hydrate".into(),
        name: "explicit hydrate".into(),
        description: String::new(),
        created_by: String::new(),
        runtime: crate::agents::adapter::PREFERRED_RUNTIME,
        args: Vec::new(),
        launch: Default::default(),
        mcp: Default::default(),
        startup: Vec::new(),
        context: Some(entries),
        permissions: None,
        default_prompt: String::new(),
        avatar: None,
        role: None,
        allow_wake: false,
        resume_after_restart: false,
        budget_usd: None,
        revision: 0,
        archived_at: None,
        extra: Default::default(),
        env: Default::default(),
    }
}

/// The entries a selection names: `paths` and `sets` alike are workspace
/// prefixes (a file or a directory); none at all means the runtime defaults.
pub fn selection_entries(paths: &[String], sets: &[String]) -> Result<Vec<ContextEntry>, String> {
    let named: Vec<&String> = paths.iter().chain(sets).collect();
    if named.len() > MAX_SELECTION {
        return Err(format!(
            "at most {MAX_SELECTION} paths and sets in one hydrate; name a set instead"
        ));
    }
    if named.is_empty() {
        return Ok(AgentCli::ALL
            .into_iter()
            .flat_map(|runtime| default_context(runtime, false))
            .collect());
    }
    let mut seen = BTreeSet::new();
    let mut entries = Vec::new();
    for path in named {
        let path = path.trim_end_matches('/').to_string();
        if let Some(why) = unsafe_destination(&path) {
            return Err(format!("{path} can't be hydrated: {why}"));
        }
        if seen.insert(path.clone()) {
            entries.push(ContextEntry::new(path));
        }
    }
    // Validate as the agent `context` is validated (relative, no `..`, …),
    // in groups the resolver accepts.
    for group in entries.chunks(crate::agents::context::MAX_CONTEXT_ENTRIES) {
        normalize_entries(group).map_err(|e| e.to_string())?;
    }
    Ok(entries)
}

/// The want set of an explicit hydrate of `paths` / `sets` at `target`.
pub fn want_for_selection(
    home: &Path,
    principal: &Principal,
    target: &ManifestTarget,
    paths: &[String],
    sets: &[String],
) -> Result<WantSet, String> {
    want_for_selection_for(home, principal, target, paths, sets, &Default::default())
}

pub(crate) fn want_for_selection_for(
    home: &Path,
    principal: &Principal,
    target: &ManifestTarget,
    paths: &[String],
    sets: &[String],
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<WantSet, String> {
    let entries = selection_entries(paths, sets)?;
    let mut merged: Option<ContextResolution> = None;
    for group in entries.chunks(crate::agents::context::MAX_CONTEXT_ENTRIES) {
        let agent = carrier(group.to_vec());
        let resolution = crate::agents::manifest_resolve::context::resolve_context_with(
            scope,
            home,
            principal,
            ContextRequest {
                agent: &agent,
                target,
                repo_override: None,
                team_workspace: false,
            },
        );
        merged = Some(match merged {
            None => resolution,
            Some(mut all) => {
                let taken: BTreeSet<String> =
                    all.files.iter().map(|f| f.destination.clone()).collect();
                all.files.extend(
                    resolution
                        .files
                        .into_iter()
                        .filter(|f| !taken.contains(&f.destination)),
                );
                all.files.sort_by(|a, b| a.destination.cmp(&b.destination));
                all.entries.extend(resolution.entries);
                all.warnings.extend(resolution.warnings);
                all
            }
        });
    }
    let resolution = merged.ok_or("nothing to hydrate")?;
    let mut set = WantSet::from_resolution(
        home,
        principal,
        None,
        target,
        &resolution,
        super::want::source_roots(home, principal, target),
    )?;
    // The call's own scope, unfolded: what a prune may reach (review r1).
    set.scope = entries
        .iter()
        .map(|entry| entry.dest.clone().unwrap_or_else(|| entry.path.clone()))
        .collect();
    for file in &mut set.files {
        file.source_context = target.source_context(&file.layer);
    }
    Ok(set)
}

/// The content roots of `target`, each with the label its files' managed
/// entries carry (`team:org/team`, `repo:host/owner/repo`, `personal:org`):
/// the target repo's tier first when there is one.
pub(crate) fn labelled_roots(
    home: &Path,
    principal: &Principal,
    target: &ManifestTarget,
) -> Vec<(String, crate::context_repo::inheritance::ContentRoot)> {
    let mut roots = Vec::new();
    if let (Some(root), Some(slug)) = (
        crate::agents::manifest_resolve::context::repo_tier_path(home, principal, target),
        target.repo.as_ref(),
    ) {
        if let Ok(label) = crate::agents::context::repo_label(slug) {
            roots.push((
                format!("repo:{label}"),
                crate::context_repo::inheritance::ContentRoot::own(root),
            ));
        }
    }
    for (layer, root) in content_layers(home, principal, target) {
        let label = match layer {
            crate::agents::manifest::ContentLayer::Team { org, team } => {
                format!("team:{org}/{team}")
            }
            crate::agents::manifest::ContentLayer::Personal { org: Some(org), .. } => {
                format!("personal:{org}")
            }
            crate::agents::manifest::ContentLayer::Personal { org: None, .. } => "personal".into(),
        };
        roots.push((label, root));
    }
    roots
}

/// The want set of `hydrate_story`: exactly the closure's files, each placed
/// at its path within its content tier. Files outside every tier of
/// `target`, and paths a workspace never carries, are counted, not wanted.
pub fn want_for_files(
    home: &Path,
    principal: &Principal,
    target: &ManifestTarget,
    files: &[PathBuf],
) -> (WantSet, usize) {
    want_for_files_for(home, principal, target, files, &Default::default())
}

pub(crate) fn want_for_files_for(
    home: &Path,
    principal: &Principal,
    target: &ManifestTarget,
    files: &[PathBuf],
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> (WantSet, usize) {
    use codesense_pty_core::remote_manifest::normalized_mode;
    use unicode_normalization::UnicodeNormalization;
    let roots = labelled_roots(home, principal, target);
    let view = crate::agents::roots::team_view(
        home,
        principal,
        &target.org,
        &target.team,
        target.context_root.as_ref(),
    )
    .ok();
    let mut outside = 0;
    let mut wanted = Vec::new();
    let mut prefixes = BTreeSet::new();
    for file in files {
        if !scope.allows(file) {
            continue;
        }
        let Some(source) = view.as_ref().and_then(|v| v.read_path(file).ok()) else {
            outside += 1;
            continue;
        };
        // The deepest root containing it (a repo tier sits inside its team).
        let Some((label, relative)) = roots
            .iter()
            .filter_map(|(label, root)| {
                root.relative_for_read(file).map(|rel| (label.clone(), rel))
            })
            .min_by_key(|(_, rel)| rel.components().count())
        else {
            outside += 1;
            continue;
        };
        let label = roots
            .iter()
            .filter_map(|(label, root)| root.relative_for_read(&source).map(|rel| (label, rel)))
            .min_by_key(|(_, rel)| rel.components().count())
            .map(|(label, _)| label.clone())
            .unwrap_or(label);
        let Some(dest) = relative
            .to_str()
            .map(|d| d.replace('\\', "/").nfc().collect::<String>())
        else {
            outside += 1;
            continue;
        };
        if unsafe_destination(&dest).is_some() {
            outside += 1;
            continue;
        }
        let Ok(meta) = std::fs::metadata(&source) else {
            outside += 1;
            continue;
        };
        if !meta.file_type().is_file() {
            outside += 1;
            continue;
        }
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            meta.permissions().mode()
        };
        #[cfg(not(unix))]
        let mode = 0o644;
        if let Some((parent, _)) = dest.rsplit_once('/') {
            prefixes.insert(parent.to_string());
        } else {
            prefixes.insert(dest.clone());
        }
        wanted.push(super::WantFile {
            source_context: target.source_context(&label),
            dest,
            source,
            bytes: meta.len(),
            mode: normalized_mode(mode),
            layer: label,
            pinned: None,
        });
    }
    wanted.sort_by(|a, b| a.dest.cmp(&b.dest));
    wanted.dedup_by(|a, b| a.dest == b.dest);
    let set = WantSet {
        files: wanted,
        prefixes: super::want::fold_prefixes(prefixes.into_iter().collect()).unwrap_or_default(),
        source_roots: roots
            .into_iter()
            .map(|(_, root)| root.safety_boundary())
            .collect(),
        warnings: Vec::new(),
        scope: Vec::new(),
    };
    (set, outside)
}

/// One explicit hydrate: the engine run plus its terminal event.
pub struct Hydrate<'a> {
    pub home: &'a Path,
    pub principal: &'a Principal,
    pub destination: super::Destination<'a>,
    pub want: &'a WantSet,
    pub operation: &'a str,
    /// For progress: the workspace and who asked (the calling agent).
    pub workspace: String,
    pub agent: String,
    /// Remove unwanted unmodified managed files (`prune:true`).
    pub prune: bool,
    pub overwrite_modified: bool,
    pub progress: &'a dyn Fn(&super::PrepareProgress),
    pub validate: &'a dyn Fn() -> Result<(), String>,
    pub cancel: Option<&'a dyn Fn() -> bool>,
    pub legacy: &'a dyn Fn() -> Result<super::LegacyRows, String>,
}

/// Run an explicit hydrate on the engine (origin `step3:<operation>`) and
/// emit its terminal event (`ready` / `failed` / `cancelled`).
pub fn hydrate(request: Hydrate<'_>) -> Result<super::PrepareReport, String> {
    let report = super::prepare(super::PrepareRequest {
        home: request.home,
        principal: request.principal,
        destination: request.destination,
        want: request.want,
        origin: format!("step3:{}", request.operation),
        workspace: request.workspace.clone(),
        agent: request.agent.clone(),
        legacy: request.legacy,
        prune: request.prune,
        progress: request.progress,
        validate: request.validate,
        explicit: super::Explicit {
            overwrite_modified: request.overwrite_modified,
            cancel: request.cancel,
            operation: Some(request.operation),
            // A prune stays inside what this call names (its paths / sets,
            // or the runtime defaults it hydrated).
            // Never the folded `prefixes`: past 32 they widen to top-level
            // folders. A want set without a scope (a story's files) prunes
            // nothing outside its own files' folders.
            prune_under: Some(if request.want.scope.is_empty() {
                &request.want.prefixes
            } else {
                &request.want.scope
            }),
        },
    })?;
    super::progress::finish_operation(
        request.progress,
        &report,
        &request.workspace,
        &request.agent,
        "team content",
        Some(request.operation),
    );
    Ok(report)
}

/// Remember the epic directories `hydrate_story` brought into a workspace
/// (`list_hydrated` reads them back from the managed record).
pub fn record_hydrated_epics(
    home: &Path,
    principal: &Principal,
    identity: &crate::content_engine::destination::DestinationIdentity,
    epics: impl IntoIterator<Item = String>,
) -> Result<(), String> {
    let lock = super::step_lock(identity);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut record = super::ManagedRecord::load(home, principal, identity);
    let before = record.hydrated_epics.len();
    record.hydrated_epics.extend(epics);
    if record.hydrated_epics.len() != before {
        record.save(home, principal, identity)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "step3_tests.rs"]
pub(crate) mod tests;
