// SPDX-License-Identifier: MIT
//! Resolve an agent's startup paths into the exact files it starts with: the
//! producer of [`StartupManifest`].
//!
//! Each startup path is looked up across content layers in a fixed order, and
//! the first layer where it exists (as a file or a directory) supplies every
//! file of that entry (an entry never mixes layers, though one directory in
//! the workspace may hold entries from different layers):
//!
//! 1. the launching user's scratch space for the target org;
//! 2. the user's all-orgs scratch folder (allowlisted inert files only);
//! 3. the target team's chain, nearest hop first.
//!
//! A source repo's content is never consulted for startup: an agent is not
//! bound to one. (`context` resolution, in [`context`], adds the per-repo tier.)
//! A directory expands to every regular file under it, less what projection
//! never carries either (OS and editor cruft, build and VCS subtrees, the
//! layer's projection excludes). A file lands in the
//! workspace at the same relative path it has in its layer, with no projection
//! remapping.
//!
//! Any Markdown file may declare `requires: [<content path>, …]` in its
//! frontmatter. Each requirement is resolved the same way and follows the
//! entry that declared it. A requirement an earlier entry already carries is
//! dropped; one inside an earlier entry that its layer lacks resolves as its
//! own entry, so a partial personal override of a directory can still pull
//! the rest from the team.
//!
//! A file a layer's `merge-marked.yaml` marks is not carried as it stands: it
//! becomes a generated entry, the deep merge of that file across the layers
//! (SW-XS-14), which the launch writes into the workspace.
//!
//! Resolution only reads: it opens the files under the startup paths (and
//! their requirements) plus each layer's exclude list and merge marks,
//! and lists only the directories along those paths (to match their exact
//! spelling) and under them, never the rest of a layer. It writes nothing.
//! Every problem becomes a [`ManifestError`]; a manifest with errors must not
//! be applied.

use crate::context_repo::inheritance::ContentRoot;
use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::content_engine::{is_projection_excluded, TeamExcludes};
use crate::context_repo::scratch_space::is_scratch_team;
use crate::domain::{Principal, SourceRepoSlug};
use crate::install::merge_marks::MergeMarks;

use super::content_reader::{ContentReader, FsContentReader, Node, NodeKind};
use super::keys::AgentKey;
use super::manifest::{
    content_hash_streamed, entry_hash, json_hash, AgentRevisionRef, ContentLayer, EntryOrigin,
    EssentialEntry, GeneratedEntry, GeneratedTarget, ManifestEntry, ManifestError,
    ManifestErrorCode, ManifestFile, ManifestTotals, StartupManifest,
};
use super::model::{Agent, ValidationError};
use super::paths::StartupPath;
use super::roots;
use codesense_pty_core::agent_cli::AgentCli;

pub mod context;
pub(crate) use context::context_tier_roots;
pub(crate) use context::resolve_context_in_checkout;
pub use context::{
    resolve_context, ContextEntrySource, ContextFile, ContextRequest, ContextResolution,
    ContextTier, ContextWarning, ContextWarningCode, ResolvedContextEntry,
};

/// How much of a Markdown file is read for its frontmatter: a `requires:`
/// block that does not close within it is not seen.
const MAX_FRONTMATTER_BYTES: usize = 1024 * 1024;

/// The team whose content a launch resolves against, and the source repo
/// the workspace is for, when it is for one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextRoot {
    pub id: String,
    pub root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestTarget {
    pub context_root: Option<ContextRoot>,
    pub org: String,
    pub team: String,
    /// SW-XS-13: the repo whose per-repo tier (`repos/{host}/{owner}/{repo}/content/`)
    /// leads context resolution ([`context`]). The startup manifest does not
    /// consult it: startup keeps its own tiers and error semantics.
    pub repo: Option<SourceRepoSlug>,
}

impl ManifestTarget {
    pub(crate) fn source_context(&self, layer: &str) -> Option<String> {
        if layer.starts_with("personal") || layer == "generated" {
            return None;
        }
        Some(
            self.context_root
                .as_ref()
                .map_or("clone", |r| r.id.as_str())
                .to_string(),
        )
    }

    /// `org/team`, with no repo.
    pub fn team(org: impl Into<String>, team: impl Into<String>) -> Self {
        Self {
            context_root: None,
            org: org.into(),
            team: team.into(),
            repo: None,
        }
    }

    /// The launch's target team when it names one, else the agent's own team.
    /// A personal agent with no target resolves against its org's scratch
    /// space (and all-orgs) alone; a starter, which has no team or org of its
    /// own, resolves against all-orgs alone.
    pub fn for_launch(agent: &AgentKey, launch_target: Option<ManifestTarget>) -> Self {
        launch_target.unwrap_or_else(|| {
            if agent.is_builtin() {
                Self::team("", "")
            } else {
                Self::team(agent.org.clone(), agent.team.clone())
            }
        })
    }
}

/// Resolve `agent` (at `revision`) into its startup manifest against
/// `target`, for a launch by `principal`. `generated` is placed in the
/// manifest as given, each entry's hash recomputed from its content.
/// `launch_errors` are problems the caller found outside the content (an
/// unknown MCP server, an invalid definition); they follow the content
/// errors, so the manifest hash covers them too.
pub fn resolve_manifest(
    home: &Path,
    principal: &Principal,
    agent: &Agent,
    revision: &AgentRevisionRef,
    target: ManifestTarget,
    generated: Vec<GeneratedEntry>,
    launch_errors: Vec<ManifestError>,
) -> StartupManifest {
    resolve_manifest_with(
        &FsContentReader,
        home,
        principal,
        agent,
        revision,
        target,
        generated,
        launch_errors,
    )
}

/// The content roots `target` resolves through for `principal`, in
/// precedence order, each with its layer. A layer that cannot be used is left
/// out here; [`resolve_manifest`] reports why.
pub(crate) fn content_layers(
    home: &Path,
    principal: &Principal,
    target: &ManifestTarget,
) -> Vec<(ContentLayer, ContentRoot)> {
    content_layers_with(&FsContentReader, home, principal, target)
}

pub(crate) fn content_layers_with(
    reader: &dyn ContentReader,
    home: &Path,
    principal: &Principal,
    target: &ManifestTarget,
) -> Vec<(ContentLayer, ContentRoot)> {
    layers(reader, home, principal, target, &mut Vec::new())
        .into_iter()
        .map(|layer| (layer.id, layer.root))
        .collect()
}

/// SW-XS-8b (review r1): the merged workspace file a launch at `target`
/// would generate for the merge-marked `logical` — the exact
/// [`GeneratedEntry`] Step 2 writes (the deep merge across the content
/// layers, farthest first).
pub(crate) fn merged_workspace_file(
    home: &Path,
    principal: &Principal,
    target: &ManifestTarget,
    logical: &str,
) -> Result<GeneratedEntry, String> {
    let mut errors = Vec::new();
    let layers = layers(&FsContentReader, home, principal, target, &mut errors);
    let run = Run {
        reader: &FsContentReader,
        layers: &layers,
        errors,
        owned: BTreeMap::new(),
        seen: BTreeSet::new(),
        folded: BTreeMap::new(),
        listings: RefCell::new(HashMap::new()),
        entries: Vec::new(),
        requires: Vec::new(),
        merged: BTreeSet::new(),
    };
    if !run.is_merge_marked(logical) {
        return Err(format!("{logical} is not merge-marked"));
    }
    run.merge_layers(logical)
}

/// [`resolve_manifest`] reading content through `reader`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_manifest_with(
    reader: &dyn ContentReader,
    home: &Path,
    principal: &Principal,
    agent: &Agent,
    revision: &AgentRevisionRef,
    target: ManifestTarget,
    generated: Vec<GeneratedEntry>,
    launch_errors: Vec<ManifestError>,
) -> StartupManifest {
    let mut errors = Vec::new();
    let layers = layers(reader, home, principal, &target, &mut errors);
    let mut run = Run {
        reader,
        layers: &layers,
        errors,
        owned: BTreeMap::new(),
        seen: BTreeSet::new(),
        folded: BTreeMap::new(),
        listings: RefCell::new(HashMap::new()),
        entries: Vec::new(),
        requires: Vec::new(),
        merged: BTreeSet::new(),
    };

    // The definition's own rules (no duplicate, no entry inside another) keep
    // declared entries disjoint, so each file has one declared owner.
    let declared = match agent.startup_paths() {
        Ok(paths) => paths,
        Err(e) => {
            let path = match &e {
                ValidationError::DuplicateStartupPath(p)
                | ValidationError::OverlappingStartupPath { path: p, .. } => {
                    StartupPath::parse(p).ok()
                }
                _ => None,
            };
            run.errors.push(ManifestError {
                path,
                code: ManifestErrorCode::Invalid,
                message: e.to_string(),
            });
            Vec::new()
        }
    };
    run.seen.extend(declared.iter().cloned());

    // Every declared entry claims its files before any dependency is resolved,
    // so a dependency an agent also declares is always the declared entry.
    let mut claimed = Vec::new();
    for path in declared {
        // An entry left with no files had only merge-marked files: those are
        // generated entries now, and the entry itself carries nothing.
        if let Some(resolved) = run
            .resolve(&path, None)
            .filter(|r| !r.files.is_empty())
            .filter(|r| run.admit(r))
        {
            run.claim(&resolved);
            claimed.push(resolved);
        }
    }
    for resolved in claimed {
        let path = resolved.path.clone();
        let requires = resolved.requires();
        run.push(resolved, EntryOrigin::Declared, requires.clone());
        run.close(&path, requires);
    }

    run.report_cycles();
    let merged = run.merged_generated();
    run.errors.extend(launch_errors);

    let entries = run.entries;
    let totals = entries
        .iter()
        .flat_map(|e| &e.files)
        .fold(ManifestTotals::default(), |t, f| ManifestTotals {
            files: t.files + 1,
            bytes: t.bytes + f.bytes,
        });
    let generated = generated
        .into_iter()
        .chain(merged)
        .map(|g| GeneratedEntry {
            hash: json_hash(&g.content),
            ..g
        })
        .collect();
    let mut manifest = StartupManifest {
        agent: revision.clone(),
        runtime: agent.runtime,
        entries,
        essentials: essentials(agent.runtime),
        generated,
        totals,
        errors: run.errors,
        manifest_hash: String::new(),
    };
    manifest.manifest_hash = manifest.compute_hash();
    manifest
}

/// Whether `runtime` raises needs-you through CodeSense's status hooks, which its
/// launch settings carry.
fn runs_status_hooks(runtime: AgentCli) -> bool {
    !codesense_pty_core::agent_cli::adapter(runtime)
        .status_hooks()
        .is_empty()
}

/// The generated items CodeSense always provides for `runtime`.
fn essentials(runtime: AgentCli) -> Vec<EssentialEntry> {
    let mut out = vec![EssentialEntry {
        id: "codesense-bridge".into(),
        reason: "CodeSense MCP bridge — required by CodeSense".into(),
    }];
    if runs_status_hooks(runtime) {
        out.push(EssentialEntry {
            id: "codesense-notify-settings".into(),
            reason: "CodeSense notification hook settings — required by CodeSense".into(),
        });
    }
    out
}

/// One content layer, with the exclude list and merge marks loaded on first
/// use (only a layer that wins an entry is consulted).
struct Layer {
    id: ContentLayer,
    root: ContentRoot,
    rules: OnceCell<(Arc<TeamExcludes>, MergeMarks)>,
    /// SLW-PT-20: on the org's scratch layer, the files the target team chose "Use
    /// team's" for. A startup path that is (or holds) one of them yields to a later
    /// layer that has that file, as the team's worktree does.
    use_team: Vec<String>,
}

impl Layer {
    /// The all-orgs scratch folder: allowlisted inert files only.
    fn is_all_orgs(&self) -> bool {
        matches!(self.id, ContentLayer::Personal { org: None, .. })
    }

    fn rules(&self) -> &(Arc<TeamExcludes>, MergeMarks) {
        self.rules
            .get_or_init(|| (self.root.excludes(), self.root.merge_marks()))
    }
}

/// The layers to try, in precedence order, each content root once. A root
/// reached through a symlink is refused, as bootstrap refuses it.
fn layers(
    reader: &dyn ContentReader,
    home: &Path,
    principal: &Principal,
    target: &ManifestTarget,
    errors: &mut Vec<ManifestError>,
) -> Vec<Layer> {
    let account = principal.as_str().to_string();
    let mut candidates = Vec::new();
    // SLW-PT-20: a target team with "Use my scratch items" off takes no scratch layer (org
    // space or all-orgs); a personal agent's own target (the scratch team) always does.
    let scratch_on = target.org.is_empty()
        || target.team.is_empty()
        || crate::context_repo::scratch_space::team_uses_scratch(
            home,
            principal,
            &target.org,
            &target.team,
        );
    if !target.org.is_empty() && scratch_on {
        candidates.push((
            ContentLayer::Personal {
                account: account.clone(),
                org: Some(target.org.clone()),
            },
            false,
        ));
    }
    if scratch_on {
        candidates.push((ContentLayer::Personal { account, org: None }, false));
    }
    let use_team = if scratch_on && !target.team.is_empty() && !is_scratch_team(&target.team) {
        crate::context_repo::scratch_space::team_scratch_use(
            home,
            principal,
            &target.org,
            &target.team,
        )
        .use_team_files
    } else {
        Vec::new()
    };
    if !target.org.is_empty() && !target.team.is_empty() && !is_scratch_team(&target.team) {
        candidates.push((
            ContentLayer::Team {
                org: target.org.clone(),
                team: target.team.clone(),
            },
            true,
        ));
    }

    let mut out: Vec<Layer> = Vec::new();
    for (id, required) in candidates {
        let root = match &id {
            ContentLayer::Team { org, team } => {
                roots::team_content_dir_at(home, principal, org, team, target.context_root.as_ref())
            }
            _ => id.content_root(home, principal),
        };
        match root {
            Ok(root) => {
                // Attached checkouts may live outside home. Walk their team
                // descendants from the registered checkout boundary as well.
                let boundary = match &id {
                    _ if root.starts_with(home) => home.to_path_buf(),
                    ContentLayer::Personal { .. } => home.to_path_buf(),
                    ContentLayer::Team { org, .. } => {
                        match roots::org_root(home, principal, org, target.context_root.as_ref()) {
                            Ok(boundary) => boundary,
                            Err(error) => {
                                errors.push(ManifestError {
                                    path: None,
                                    code: ManifestErrorCode::Invalid,
                                    message: error.to_string(),
                                });
                                continue;
                            }
                        }
                    }
                };
                if let Some(link) = symlink_on_the_way(reader, &boundary, &root) {
                    errors.push(ManifestError {
                        path: None,
                        code: ManifestErrorCode::EscapesContent,
                        message: format!(
                            "content layer {id:?} is reached through the symlink {}",
                            link.display()
                        ),
                    });
                    continue;
                }
                if !out.iter().any(|l| l.id == id) {
                    let use_team = match &id {
                        ContentLayer::Personal { org: Some(_), .. } => use_team.clone(),
                        _ => Vec::new(),
                    };
                    out.push(Layer {
                        id,
                        root: ContentRoot::own(root),
                        rules: OnceCell::new(),
                        use_team,
                    });
                }
            }
            // No personal space yet simply means no personal layer.
            Err(_) if !required => {}
            Err(e) => errors.push(ManifestError {
                path: None,
                code: ManifestErrorCode::Invalid,
                message: format!("cannot resolve content layer {id:?}: {e}"),
            }),
        }
    }
    if !target.org.is_empty() && !target.team.is_empty() && !is_scratch_team(&target.team) {
        match roots::team_view(
            home,
            principal,
            &target.org,
            &target.team,
            target.context_root.as_ref(),
        ) {
            Ok(view) => {
                out.extend(view.inherited.iter().map(|layer| Layer {
                    id: ContentLayer::Team {
                        org: target.org.clone(),
                        team: layer.team().to_string(),
                    },
                    root: layer.content(),
                    rules: OnceCell::new(),
                    use_team: Vec::new(),
                }));
            }
            Err(e) => errors.push(ManifestError {
                path: None,
                code: ManifestErrorCode::Invalid,
                message: e.to_string(),
            }),
        }
    }
    out
}

/// The first symlink on the way from the trusted boundary down to the layer root `root`
/// (or `root` itself, for a root outside `home`). An absent component ends
/// the walk: there is nothing beneath it to follow.
fn symlink_on_the_way(reader: &dyn ContentReader, home: &Path, root: &Path) -> Option<PathBuf> {
    let mut checked = Vec::new();
    match root.strip_prefix(home) {
        Ok(rest) => {
            let mut at = home.to_path_buf();
            for component in rest.components() {
                at.push(component);
                checked.push(at.clone());
            }
        }
        Err(_) => checked.push(root.to_path_buf()),
    }
    for at in checked {
        match reader.node(&at) {
            Ok(node) if node.kind == NodeKind::Symlink => return Some(at),
            Ok(_) => {}
            Err(_) => return None,
        }
    }
    None
}

/// The key two workspace paths share when the workspace file system would
/// treat them as one: projection's own override key.
fn folded(destination: &str) -> String {
    crate::context_repo::override_wins_key(Path::new(destination))
}

/// A path in `taken` (keyed by [`folded`]) that `key` would collide with:
/// the same path, a file at one of its ancestors, or a file beneath it.
fn collision<'m>(taken: &'m BTreeMap<String, String>, key: &str) -> Option<&'m String> {
    if let Some(same) = taken.get(key) {
        return Some(same);
    }
    let ancestor = key
        .match_indices('/')
        .find_map(|(i, _)| taken.get(&key[..i]));
    if ancestor.is_some() {
        return ancestor;
    }
    let below = format!("{key}/");
    taken
        .range(below.clone()..)
        .next()
        .filter(|(k, _)| k.starts_with(&below))
        .map(|(_, v)| v)
}

/// One resolved file with the requirements its frontmatter declares.
#[derive(Clone)]
struct ResolvedFile {
    file: ManifestFile,
    requires: Vec<StartupPath>,
}

/// A startup path resolved in its winning layer.
struct Resolved {
    path: StartupPath,
    layer: ContentLayer,
    files: Vec<ResolvedFile>,
}

impl Resolved {
    /// Every requirement of every file, first occurrence first.
    fn requires(&self) -> Vec<StartupPath> {
        let mut seen = BTreeSet::new();
        self.files
            .iter()
            .flat_map(|f| f.requires.iter())
            .filter(|p| seen.insert((*p).clone()))
            .cloned()
            .collect()
    }
}

struct Run<'a> {
    reader: &'a dyn ContentReader,
    layers: &'a [Layer],
    errors: Vec<ManifestError>,
    /// Which entry owns each logical file.
    owned: BTreeMap<String, StartupPath>,
    /// Every claimed destination, keyed by [`folded`].
    folded: BTreeMap<String, String>,
    /// The names in each directory `locate` has checked a spelling against.
    listings: RefCell<HashMap<(ContentLayer, String), BTreeSet<OsString>>>,
    /// Every path already taken up as an entry or attempted as one.
    seen: BTreeSet<StartupPath>,
    entries: Vec<ManifestEntry>,
    /// What each entry's files require, parallel to `entries`.
    requires: Vec<Vec<StartupPath>>,
    /// SW-XS-14: merge-marked files the entries named, left out of every entry
    /// and built as generated entries instead (logical paths).
    merged: BTreeSet<String>,
}

/// Whether projection never carries `logical`: OS and editor cruft, and
/// anything inside a build or VCS directory.
pub(crate) fn not_carried(logical: &str) -> bool {
    codesense_fs_core::filters::is_ignored(logical)
        || codesense_fs_core::filters::is_ignored_subtree(logical)
        || logical
            .split('/')
            .any(codesense_fs_core::filters::is_vcs_metadata_dir)
}

impl Run<'_> {
    fn error(&mut self, path: &StartupPath, code: ManifestErrorCode, message: String) {
        self.errors.push(ManifestError {
            path: Some(path.clone()),
            code,
            message,
        });
    }

    /// Record an error and return `true` when `path` is something projection
    /// never carries.
    fn refuse_not_carried(&mut self, path: &StartupPath, context: &str) -> bool {
        if !not_carried(path.as_str()) {
            return false;
        }
        self.error(
            path,
            ManifestErrorCode::EscapesContent,
            format!("{path}{context}: never carried into a workspace (cruft, build or VCS data)"),
        );
        true
    }

    /// Whether `resolved` may join the manifest: none of its files may land
    /// on the same workspace path as another file under a different spelling,
    /// or on a path inside another file, which would collide on a
    /// case-insensitive file system. Records why not.
    fn admit(&mut self, resolved: &Resolved) -> bool {
        let mut own = BTreeMap::new();
        for f in &resolved.files {
            let key = folded(&f.file.destination);
            if let Some(earlier) = collision(&self.folded, &key).or_else(|| collision(&own, &key)) {
                let message = format!(
                    "{} and {earlier} differ only in case, or one lies inside the other, and \
                     would collide in the workspace",
                    f.file.destination
                );
                self.error(&resolved.path, ManifestErrorCode::Invalid, message);
                return false;
            }
            own.insert(key, f.file.destination.clone());
        }
        true
    }

    fn claim(&mut self, resolved: &Resolved) {
        for f in &resolved.files {
            self.folded
                .insert(folded(&f.file.destination), f.file.destination.clone());
            self.owned
                .insert(f.file.logical.clone(), resolved.path.clone());
        }
    }

    fn push(&mut self, resolved: Resolved, origin: EntryOrigin, requires: Vec<StartupPath>) {
        let files: Vec<ManifestFile> = resolved.files.into_iter().map(|f| f.file).collect();
        self.requires.push(requires);
        self.entries.push(ManifestEntry {
            path: resolved.path,
            origin,
            layer: resolved.layer,
            entry_hash: entry_hash(&files),
            files,
        });
    }

    /// Resolve a dependency or required path and add it, minus any file an
    /// earlier entry already owns (dropping it when nothing is left). Returns
    /// the requirements of what was added.
    ///
    /// A path inside an earlier entry that the entry does not carry (its
    /// layer lacks it) resolves on its own, from the first layer that has it.
    /// One the entry's layer holds but excludes still fails there.
    fn add_implied(
        &mut self,
        path: &StartupPath,
        origin: EntryOrigin,
        needed_by: &str,
    ) -> Vec<StartupPath> {
        if self.owned.contains_key(path.as_str()) {
            return Vec::new();
        }
        let Some(resolved) = self.resolve(path, Some(needed_by)) else {
            return Vec::new();
        };
        if resolved.files.is_empty() || !self.admit(&resolved) {
            return Vec::new();
        }
        self.claim(&resolved);
        let requires = resolved.requires();
        self.push(resolved, origin, requires.clone());
        requires
    }

    /// Add the requirements of `dependent`, depth first, each right after the
    /// entry that needs it, each path once. Iterative, so a long requirement
    /// chain in team content cannot exhaust the stack.
    fn close(&mut self, dependent: &StartupPath, requires: Vec<StartupPath>) {
        let mut stack = vec![(dependent.clone(), requires.into_iter())];
        while let Some((of, pending)) = stack.last_mut() {
            let Some(req) = pending.next() else {
                stack.pop();
                continue;
            };
            if !self.seen.insert(req.clone()) {
                continue;
            }
            let of = of.clone();
            let origin = EntryOrigin::Dependency { of: of.clone() };
            let next = self.add_implied(&req, origin, of.as_str());
            stack.push((req, next.into_iter()));
        }
    }

    /// Record one error per group of entries that require each other in a
    /// loop (a strongly connected component of the requirement graph). A
    /// requirement points at the entries carrying it: the entry for that
    /// path, or the entries owning the files it names. One inside its own
    /// entry is no loop, and whether an entry was declared or pulled in does
    /// not matter.
    fn report_cycles(&mut self) {
        let n = self.entries.len();
        let index: BTreeMap<&StartupPath, usize> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| (&e.path, i))
            .collect();
        // The entries that actually carry `req`: its own entry, or the owners
        // of the files it names. A requirement that was refused has none.
        let carriers = |req: &StartupPath| -> Vec<usize> {
            if let Some(&i) = index.get(req) {
                return vec![i];
            }
            let below = format!("{req}/");
            self.owned
                .get(req.as_str())
                .into_iter()
                .chain(
                    self.owned
                        .range(below.clone()..)
                        .take_while(|(k, _)| k.starts_with(&below))
                        .map(|(_, owner)| owner),
                )
                .filter_map(|owner| index.get(owner).copied())
                .collect()
        };
        let edges: Vec<Vec<usize>> = self
            .requires
            .iter()
            .enumerate()
            .map(|(from, reqs)| {
                let mut to: Vec<usize> = reqs
                    .iter()
                    .flat_map(carriers)
                    .filter(|&to| to != from)
                    .collect();
                to.sort_unstable();
                to.dedup();
                to
            })
            .collect();
        let mut reverse = vec![Vec::new(); n];
        for (from, tos) in edges.iter().enumerate() {
            for &to in tos {
                reverse[to].push(from);
            }
        }

        // Kosaraju, both passes iterative: finish order on the graph, then
        // components on the reversed graph in reverse finish order.
        let mut visited = vec![false; n];
        let mut order = Vec::with_capacity(n);
        for start in 0..n {
            if visited[start] {
                continue;
            }
            visited[start] = true;
            let mut stack = vec![(start, 0usize)];
            while let Some((at, next)) = stack.last_mut() {
                if let Some(&to) = edges[*at].get(*next) {
                    *next += 1;
                    if !visited[to] {
                        visited[to] = true;
                        stack.push((to, 0));
                    }
                } else {
                    order.push(*at);
                    stack.pop();
                }
            }
        }
        let mut component = vec![usize::MAX; n];
        let mut groups: Vec<Vec<usize>> = Vec::new();
        for &start in order.iter().rev() {
            if component[start] != usize::MAX {
                continue;
            }
            let id = groups.len();
            let mut members = Vec::new();
            component[start] = id;
            let mut stack = vec![start];
            while let Some(at) = stack.pop() {
                members.push(at);
                for &from in &reverse[at] {
                    if component[from] == usize::MAX {
                        component[from] = id;
                        stack.push(from);
                    }
                }
            }
            groups.push(members);
        }

        let mut loops: Vec<Vec<usize>> = groups.into_iter().filter(|g| g.len() > 1).collect();
        for group in &mut loops {
            group.sort_unstable();
        }
        loops.sort();
        for group in loops {
            let path = self.entries[group[0]].path.clone();
            let members: Vec<String> = group
                .iter()
                .map(|&i| self.entries[i].path.to_string())
                .collect();
            self.error(
                &path,
                ManifestErrorCode::DependencyCycle,
                format!(
                    "these entries require each other in a loop: {}",
                    members.join(", ")
                ),
            );
        }
    }

    /// SW-XS-14: whether any layer's `merge-marked.yaml` marks `logical`. A mark
    /// in a parent team's config covers the child team's copy too, so the
    /// merge does not depend on which layer happens to win the file.
    fn is_merge_marked(&self, logical: &str) -> bool {
        self.layers.iter().any(|layer| {
            layer.root.config_applies_to(logical) && layer.rules().1.is_marked(logical)
        })
    }

    /// SW-XS-14: one generated entry per merge-marked file the entries named,
    /// in path order. A file that cannot be merged is a definition error.
    fn merged_generated(&mut self) -> Vec<GeneratedEntry> {
        let mut out = Vec::new();
        for logical in std::mem::take(&mut self.merged) {
            match self.merge_layers(&logical) {
                Ok(entry) => out.push(entry),
                Err(why) => self.errors.push(ManifestError {
                    path: StartupPath::parse(&logical).ok(),
                    code: ManifestErrorCode::Invalid,
                    message: format!("{logical}: {why}"),
                }),
            }
        }
        out
    }

    /// The deep merge of `logical` across every layer that has it, farthest
    /// first, so a nearer layer's value wins a shared key (RFC 7386 objects;
    /// arrays per the mark's declared keys, else replaced). The all-orgs
    /// folder never contributes, a personal layer never contributes agent
    /// runtime config, and a layer that excludes the path is skipped.
    fn merge_layers(&self, logical: &str) -> Result<GeneratedEntry, String> {
        use crate::shared::config_format::ConfigFormat;
        let format = ConfigFormat::detect(Path::new(logical))
            .ok_or("merge-marked, but not a JSON, YAML or TOML file, so it cannot be merged")?;
        let merge_keys = self
            .layers
            .iter()
            .filter(|layer| layer.root.config_applies_to(logical))
            .map(|layer| &layer.rules().1)
            .find(|marks| marks.is_marked(logical))
            .map(|marks| marks.array_keys(logical))
            .unwrap_or_default();
        let policy = crate::shared::json_io::MapArrayPolicy::new(merge_keys.clone());
        let mut doc: Option<serde_json::Value> = None;
        for layer in self.layers.iter().rev() {
            if layer.is_all_orgs()
                || (matches!(layer.id, ContentLayer::Personal { .. })
                    && crate::context_repo::scratch_space::is_agent_runtime_file(logical))
                || is_projection_excluded(logical, &layer.rules().0)
            {
                continue;
            }
            let Some(source) = layer.root.file_path(logical) else {
                continue;
            };
            match self.reader.node(&source) {
                Ok(node) if node.kind == NodeKind::File => {}
                _ => continue,
            }
            let mut bytes = Vec::new();
            self.reader
                .open(&source)
                .and_then(|mut f| std::io::Read::read_to_end(&mut f, &mut bytes))
                .map_err(|e| format!("cannot read {}: {e}", source.display()))?;
            let value = format
                .parse(&bytes)
                .map_err(|e| format!("cannot merge {}: {e}", source.display()))?;
            match doc.as_mut() {
                None => doc = Some(value),
                Some(base) if !value.is_null() => {
                    crate::shared::json_io::deep_merge_with(base, &value, &policy)
                }
                Some(_) => {}
            }
        }
        let content = doc.ok_or("in no content layer")?;
        Ok(GeneratedEntry {
            id: format!("merge:{logical}"),
            target: GeneratedTarget::WorkspaceFile,
            hash: json_hash(&content),
            content,
            required: Vec::new(),
            path: Some(logical.to_string()),
            merge_keys,
        })
    }

    /// Find `path` in the first layer that has it and expand it there. `None`
    /// after recording why it cannot be used.
    fn resolve(&mut self, path: &StartupPath, needed_by: Option<&str>) -> Option<Resolved> {
        let context = needed_by
            .map(|by| format!(" (required by {by})"))
            .unwrap_or_default();
        if self.refuse_not_carried(path, &context) {
            return None;
        }
        let found = match self.locate(path) {
            Ok(found) => found,
            Err((code, why)) => {
                self.error(path, code, format!("{path}{context}: {why}"));
                return None;
            }
        };
        let Some((index, leaf)) = found else {
            // SW-CR-8: a path missing from every content layer BUT present
            // under the retired `{team}/install/` tree gets a self-describing
            // message that names the heal action instead of the bare "in no
            // content layer" that stops the user from acting.
            let message = match self.find_in_retired_install(path) {
                Some(install) => format!(
                    "{path}{context} is under {} (retired install/ layer); run the heal action \
                     or re-attach the repo to fold it into content/",
                    install.display()
                ),
                None => format!("{path}{context} is in no content layer"),
            };
            self.error(path, ManifestErrorCode::Missing, message);
            return None;
        };
        let layers = self.layers;
        let layer = &layers[index];
        if leaf.kind == NodeKind::Dir {
            if let Some(merged) = self.merge_scratch_folder(index, path, leaf) {
                return match merged {
                    Ok(resolved) => Some(resolved),
                    Err((code, why)) => {
                        self.error(path, code, format!("{path}{context}: {why}"));
                        None
                    }
                };
            }
        }
        match self.expand(layer, path, leaf) {
            Ok(files) => Some(Resolved {
                path: path.clone(),
                layer: layer.id.clone(),
                files,
            }),
            Err((code, why)) => {
                self.error(path, code, format!("{path}{context}: {why}"));
                None
            }
        }
    }

    /// SW-CR-8: the first team layer whose retired `install/` tree still
    /// carries `path`, as a file or a directory. The parent of a team layer's
    /// `content/` root is the team dir, so `install/` is one `parent()` + join
    /// away. Personal / all-orgs layers do not have an install twin.
    /// `symlink_metadata` sees a dir, a file, or a dangling symlink equally
    /// so a dir-shaped startup path still triggers the self-describing message.
    fn find_in_retired_install(&self, path: &StartupPath) -> Option<PathBuf> {
        let rel = path.as_str();
        for layer in self.layers {
            if !matches!(layer.id, ContentLayer::Team { .. }) {
                continue;
            }
            let Some(team_dir) = layer.root.own_path().and_then(Path::parent) else {
                continue;
            };
            let candidate = team_dir.join(crate::context_repo::INSTALL_DIR).join(rel);
            if candidate.symlink_metadata().is_ok() {
                return Some(candidate);
            }
        }
        None
    }

    /// Whether `name` is in the directory `dir` spelled exactly so. A
    /// case-insensitive file system finds `Secrets/x.md` under `secrets/`,
    /// but excludes, merge marks and cruft rules match the exact spelling, so
    /// a path counts as present only as it is spelled on disk, the way a
    /// case-sensitive file system would see it.
    fn spelled(&self, layer: &Layer, dir: &str, name: &str) -> std::io::Result<bool> {
        let key = (layer.id.clone(), dir.to_string());
        if let Some(names) = self.listings.borrow().get(&key) {
            return Ok(names.contains(std::ffi::OsStr::new(name)));
        }
        let names: BTreeSet<OsString> = layer.root.list(self.reader, dir)?.into_iter().collect();
        let found = names.contains(std::ffi::OsStr::new(name));
        self.listings.borrow_mut().insert(key, names);
        Ok(found)
    }

    /// SLW-PT-20: whether the scratch layer at `index` yields the FILE `path` because
    /// the target team chose "Use team's" for it and a later layer (the team's) has it —
    /// the same per-file rule the worktree overlay follows. Folders merge per file
    /// instead ([`Self::merge_scratch_folder`]).
    fn yields_to_team(&self, index: usize, path: &StartupPath) -> bool {
        let file = path.as_str();
        self.layers[index].use_team.iter().any(|f| f == file)
            && self.layers[index + 1..]
                .iter()
                .any(|later| later.root.node(self.reader, file).is_ok())
    }

    /// SLW-PT-20: a startup FOLDER that a scratch layer (the org's space or all-orgs)
    /// holds resolves per file the way the team's worktree does: the org's scratch file
    /// wins at its path unless the team chose "Use team's" there (and has the file), the
    /// team's file next, an inert all-orgs file last. `None` when no scratch layer is
    /// involved (the folder resolves whole from its first layer, as before).
    fn merge_scratch_folder(
        &mut self,
        first: usize,
        path: &StartupPath,
        leaf: Node,
    ) -> Option<Result<Resolved, (ManifestErrorCode, String)>> {
        let layers = self.layers;
        let mut holders = vec![(first, leaf)];
        let mut next = first + 1;
        while let Ok(Some((index, node))) = self.locate_from(next, path) {
            if node.kind == NodeKind::Dir {
                holders.push((index, node));
            }
            next = index + 1;
        }
        let rank = |index: usize| match &layers[index].id {
            ContentLayer::Personal { org: Some(_), .. } => 0,
            ContentLayer::Team { .. } => 1,
            ContentLayer::Personal { org: None, .. } => 2,
        };
        if holders.iter().all(|(index, _)| rank(*index) == 1) {
            return None;
        }
        // The org scratch layer, the NEAREST team layer holding the folder, all-orgs.
        let mut picked: Vec<(usize, Node)> = Vec::new();
        for want in 0..3 {
            if let Some(holder) = holders.iter().find(|(index, _)| rank(*index) == want) {
                picked.push(*holder);
            }
        }
        let mut expanded: Vec<(usize, Vec<ResolvedFile>)> = Vec::new();
        let mut first_error = None;
        for (index, node) in picked {
            match self.expand(&layers[index], path, node) {
                Ok(files) => expanded.push((index, files)),
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        let team_has = |logical: &str| {
            expanded
                .iter()
                .any(|(i, files)| rank(*i) == 1 && files.iter().any(|f| f.file.logical == logical))
        };
        let mut chosen: BTreeMap<String, (usize, ResolvedFile)> = BTreeMap::new();
        for (index, files) in &expanded {
            for file in files {
                let logical = file.file.logical.clone();
                if rank(*index) == 0
                    && layers[*index].use_team.contains(&logical)
                    && team_has(&logical)
                {
                    continue;
                }
                chosen
                    .entry(logical)
                    .or_insert_with(|| (*index, file.clone()));
            }
        }
        let Some(entry_index) = chosen.values().map(|(i, _)| *i).min_by_key(|i| rank(*i)) else {
            return Some(Err(first_error.unwrap_or((
                ManifestErrorCode::Missing,
                "directory has no files".into(),
            ))));
        };
        let entry_layer = layers[entry_index].id.clone();
        let files = chosen
            .into_values()
            .map(|(index, mut file)| {
                if index != entry_index {
                    file.file.layer = Some(layers[index].id.clone());
                }
                file
            })
            .collect();
        Some(Ok(Resolved {
            path: path.clone(),
            layer: entry_layer,
            files,
        }))
    }

    /// The first layer holding `path`, spelled exactly so, with the node it
    /// names there. A symlink at any component is an error, not a reason to
    /// look further.
    fn locate(
        &self,
        path: &StartupPath,
    ) -> Result<Option<(usize, Node)>, (ManifestErrorCode, String)> {
        self.locate_from(0, path)
    }

    /// [`Self::locate`] over the layers from `start` on.
    fn locate_from(
        &self,
        start: usize,
        path: &StartupPath,
    ) -> Result<Option<(usize, Node)>, (ManifestErrorCode, String)> {
        let segments: Vec<&str> = path.as_str().split('/').collect();
        'layers: for (index, layer) in self.layers.iter().enumerate().skip(start) {
            if self.yields_to_team(index, path) {
                continue 'layers;
            }
            let mut at = String::new();
            for (i, segment) in segments.iter().enumerate() {
                let parent = at.clone();
                if !at.is_empty() {
                    at.push('/');
                }
                at.push_str(segment);
                let last = i + 1 == segments.len();
                let unreadable = |e: std::io::Error, at: &str| {
                    (
                        ManifestErrorCode::Invalid,
                        format!("cannot read {}: {e}", at),
                    )
                };
                let node = match layer.root.node(self.reader, &at) {
                    Ok(node) => node,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue 'layers,
                    Err(e) => return Err(unreadable(e, &at)),
                };
                if !self
                    .spelled(layer, &parent, segment)
                    .map_err(|e| unreadable(e, &parent))?
                {
                    continue 'layers;
                }
                match node.kind {
                    NodeKind::Symlink => {
                        return Err((
                            ManifestErrorCode::EscapesContent,
                            format!("{} is a symlink", segments[..=i].join("/")),
                        ))
                    }
                    // all-orgs only ever answers with an allowlisted inert file.
                    NodeKind::File
                        if last
                            && layer.is_all_orgs()
                            && crate::context_repo::scratch_space::inert_kind(path.as_str())
                                .is_none() =>
                    {
                        continue 'layers
                    }
                    NodeKind::Dir | NodeKind::File if last => return Ok(Some((index, node))),
                    NodeKind::Dir => {}
                    NodeKind::Other if last => {
                        return Err((
                            ManifestErrorCode::Invalid,
                            "not a regular file or directory".into(),
                        ))
                    }
                    NodeKind::File | NodeKind::Other => continue 'layers,
                }
            }
        }
        Ok(None)
    }

    /// Every file `path` (whose node in `layer` is `leaf`) names that no
    /// earlier entry owns, checked, hashed and sorted by logical path.
    fn expand(
        &mut self,
        layer: &Layer,
        path: &StartupPath,
        leaf: Node,
    ) -> Result<Vec<ResolvedFile>, (ManifestErrorCode, String)> {
        let (excludes, _) = layer.rules();
        if is_projection_excluded(path.as_str(), excludes) {
            return Err((
                ManifestErrorCode::EscapesContent,
                "in the team's excluded paths".into(),
            ));
        }
        let mut listed: Vec<(String, u32)> = Vec::new();
        if leaf.kind == NodeKind::Dir {
            let mut any = false;
            self.walk(layer, excludes, path.as_str(), &mut listed, &mut any)?;
            if !any {
                return Err((ManifestErrorCode::Missing, "directory has no files".into()));
            }
        } else {
            listed.push((path.as_str().to_string(), leaf.mode));
        }
        listed.sort();
        if layer.is_all_orgs() {
            // SLW-PT-19: all-orgs carries allowlisted inert text only, whatever
            // else sits on disk there.
            listed.retain(|(logical, _)| {
                crate::context_repo::scratch_space::inert_kind(logical).is_some()
                    && layer.root.open(self.reader, logical).is_ok_and(|mut f| {
                        let mut bytes = Vec::new();
                        std::io::Read::read_to_end(&mut f, &mut bytes).is_ok()
                            && crate::context_repo::scratch_space::is_inert_content(&bytes)
                    })
            });
            if listed.is_empty() {
                return Err((
                    ManifestErrorCode::Missing,
                    "no inert skill, prompt, snippet or template file here".into(),
                ));
            }
        }

        // SW-XS-14: a merge-marked file is not copied as it stands: it becomes a
        // generated entry (the deep merge of its layers), built after the run.
        let before = listed.len();
        let mut marked = Vec::new();
        listed.retain(|(logical, _)| {
            let is_marked = self.is_merge_marked(logical);
            if is_marked {
                marked.push(logical.clone());
            }
            !is_marked
        });
        if listed.len() != before {
            self.merged.extend(marked);
            if listed.is_empty() {
                return Ok(Vec::new());
            }
        }

        // There is no size limit: the launcher warns about an unusually large
        // manifest but never refuses one. Files are hashed as they stream, and
        // only the head of a Markdown file (where its frontmatter may declare
        // requirements) is kept in memory.
        let mut files = Vec::with_capacity(listed.len());
        for (logical, mode) in listed {
            let unreadable = |e: std::io::Error| {
                (
                    ManifestErrorCode::Invalid,
                    format!("cannot read {logical}: {e}"),
                )
            };
            let markdown = logical.ends_with(".md");
            let mut kept = Vec::new();
            let mut source = layer.root.open(self.reader, &logical).map_err(unreadable)?;
            let (hash, size) = content_hash_streamed(
                &mut *source,
                markdown.then_some((&mut kept, MAX_FRONTMATTER_BYTES)),
            )
            .map_err(unreadable)?;
            let requires = if markdown {
                requires_of(&kept)
                    .map_err(|why| (ManifestErrorCode::Invalid, format!("{logical}: {why}")))?
            } else {
                Vec::new()
            };
            files.push(ResolvedFile {
                file: ManifestFile {
                    destination: logical.clone(),
                    hash,
                    logical,
                    bytes: size,
                    mode,
                    layer: None,
                },
                requires,
            });
        }
        Ok(files)
    }

    /// Collect every regular file under `dir` (a logical path) in `layer` that
    /// no earlier entry owns, leaving out what projection never carries
    /// either: editor and OS cruft, build and VCS subtrees, and the layer's
    /// projection excludes. `any` records whether the directory held a
    /// carried file at all, owned or not.
    fn walk(
        &self,
        layer: &Layer,
        excludes: &TeamExcludes,
        dir: &str,
        out: &mut Vec<(String, u32)>,
        any: &mut bool,
    ) -> Result<(), (ManifestErrorCode, String)> {
        let names = layer.root.list(self.reader, dir).map_err(|e| {
            (
                ManifestErrorCode::Invalid,
                format!("cannot list {dir}: {e}"),
            )
        })?;
        let mut names: Vec<String> = names
            .into_iter()
            .map(|n| {
                n.into_string().map_err(|n| {
                    (
                        ManifestErrorCode::Invalid,
                        format!("{dir} holds a name that is not UTF-8: {n:?}"),
                    )
                })
            })
            .collect::<Result<_, _>>()?;
        names.sort();
        for name in names {
            let logical = format!("{dir}/{name}");
            if not_carried(&logical) || is_projection_excluded(&logical, excludes) {
                continue;
            }
            // Bootstrap takes every logical path as a content path; a name
            // that is not one (a glob character, a backslash) would be
            // refused there, so refuse it here where the preview shows it.
            if StartupPath::parse(&logical).map_or(true, |p| p.as_str() != logical) {
                return Err((
                    ManifestErrorCode::Invalid,
                    format!("{logical} is not a usable content path (glob character or backslash)"),
                ));
            }
            let node = layer.root.node(self.reader, &logical).map_err(|e| {
                (
                    ManifestErrorCode::Invalid,
                    format!("cannot read {logical}: {e}"),
                )
            })?;
            match node.kind {
                NodeKind::Symlink => {
                    return Err((
                        ManifestErrorCode::EscapesContent,
                        format!("{logical} is a symlink"),
                    ))
                }
                NodeKind::Dir => self.walk(layer, excludes, &logical, out, any)?,
                NodeKind::File => {
                    *any = true;
                    if self.owned.contains_key(&logical) {
                        continue;
                    }
                    out.push((logical, node.mode));
                }
                NodeKind::Other => {}
            }
        }
        Ok(())
    }
}

/// The `requires:` list of a Markdown file's frontmatter: empty when the file
/// has no frontmatter or no such key.
fn requires_of(bytes: &[u8]) -> Result<Vec<StartupPath>, String> {
    // `bytes` may be a file's head cut mid-character: read what decodes.
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(e) if e.error_len().is_none() => {
            std::str::from_utf8(&bytes[..e.valid_up_to()]).expect("valid up to here")
        }
        Err(_) => return Ok(Vec::new()),
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.split_inclusive('\n');
    if lines.next().map(str::trim_end) != Some("---") {
        return Ok(Vec::new());
    }
    let mut block = String::new();
    let mut closed = false;
    for line in lines {
        if line.trim_end() == "---" {
            closed = true;
            break;
        }
        block.push_str(line);
    }
    if !closed {
        return Ok(Vec::new());
    }
    let declares_requires = || block.lines().any(|l| l.starts_with("requires:"));
    let value: serde_yaml::Value = match serde_yaml::from_str(&block) {
        Ok(value) => value,
        // Frontmatter this parser cannot read only matters if it tries to
        // declare requirements.
        Err(e) if declares_requires() => return Err(format!("frontmatter is not valid YAML: {e}")),
        Err(_) => return Ok(Vec::new()),
    };
    let Some(requires) = value.get("requires") else {
        return Ok(Vec::new());
    };
    match requires {
        serde_yaml::Value::Null => Ok(Vec::new()),
        serde_yaml::Value::Sequence(items) => items
            .iter()
            .map(|item| match item {
                serde_yaml::Value::String(raw) => {
                    StartupPath::parse(raw).map_err(|e| format!("requires: {e}"))
                }
                other => Err(format!("requires: {other:?} is not a content path")),
            })
            .collect(),
        _ => Err("requires must be a list of content paths".into()),
    }
}

#[cfg(test)]
mod tests;
