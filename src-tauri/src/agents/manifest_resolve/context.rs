// SPDX-License-Identifier: MIT
//! Resolve an agent's `context` (SW-XS §12.2) into the set of files a
//! workspace for it would get: the want set.
//!
//! ```text
//! want = (context ∪ startup) + repo_override.add − repo_override.remove − projection-excludes
//! ```
//!
//! - `context` absent resolves to the runtime's default
//!   ([`default_context`]), at resolution time and never written back.
//! - Each file's source is resolved per path, first tier wins: the target
//!   team's per-repo tier `repos/{host}/{owner}/{repo}/content/` (only when the
//!   target names a repo), then the launching user's scratch space, then the
//!   team chain nearest first. A directory merges per file across tiers.
//! - A `dest` remaps the entry's prefix in the workspace.
//! - A prefix no tier has is a **warning**, never an error: `context` is bulk
//!   and best-effort, unlike `startup`, whose errors keep blocking a launch.
//!
//! This only reads and reports. No launch consumes it yet (SW-XS-7 does).

use codesense_pty_core::agent_cli::descriptor;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use serde::Serialize;
use unicode_normalization::UnicodeNormalization;

use crate::content_engine::{is_projection_excluded, TeamExcludes};
use crate::context_repo::scratch_space::is_scratch_team;
use crate::domain::Principal;
use crate::install::projection_excludes::load_team_excludes_cached;

use super::super::content_reader::{ContentReader, FsContentReader, NodeKind};
use super::super::context::{
    default_context, is_junk, normalize_entries, repo_label, unsafe_destination, ContextEntry,
    RepoOverride,
};
use super::super::manifest::{ContentLayer, ManifestTotals};
use super::super::model::Agent;
use super::super::paths::StartupPath;
use super::super::roots;
use super::{layers, not_carried, symlink_on_the_way, ManifestTarget};

/// What to resolve: whose context, where, with which repo override.
#[derive(Debug, Clone, Copy)]
pub struct ContextRequest<'a> {
    pub agent: &'a Agent,
    pub target: &'a ManifestTarget,
    /// The agent's override for `target.repo`, if it has one.
    pub repo_override: Option<&'a RepoOverride>,
    /// A team-workspace launch (no source repo): its default differs.
    pub team_workspace: bool,
}

/// Which tier a file comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContextTier {
    /// The target team's per-repo tier.
    Repo {
        org: String,
        team: String,
        /// `host/owner/repo`.
        repo: String,
    },
    /// The launching user's scratch space (`org` absent: all-orgs).
    Personal {
        #[serde(skip_serializing_if = "Option::is_none")]
        org: Option<String>,
    },
    /// A team of the target's chain.
    Team { org: String, team: String },
}

/// Why an entry of the want set brings less than it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextWarningCode {
    /// No tier has the prefix.
    Missing,
    /// The prefix is never carried (projection-excludes, cruft, VCS data).
    Excluded,
    /// The prefix exists but holds no file that is carried.
    Empty,
    /// A symlink on the way, or an unreadable path.
    Unreadable,
    /// The entry itself is not valid.
    Invalid,
    /// A tier could not be resolved.
    Layer,
    /// Files whose workspace path may never receive context (VCS data,
    /// cruft, an excluded or a managed path) were skipped.
    UnsafeDestination,
    /// Two entries put different files at the same workspace path (or a file
    /// where the other has a directory); the earlier entry keeps it.
    Collision,
    /// Names that are not usable content paths (glob characters, a
    /// backslash, not UTF-8) were skipped.
    Skipped,
    /// The want set is unusually large.
    Large,
}

/// The size hint: past either, the want set gets a [`ContextWarningCode::Large`].
pub const LARGE_CONTEXT_BYTES: u64 = 50 * 1024 * 1024;
pub const LARGE_CONTEXT_FILES: u64 = 5_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContextWarning {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub code: ContextWarningCode,
    pub message: String,
}

/// Where an entry of the want set comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextEntrySource {
    /// The definition's own `context`.
    Context,
    /// The runtime default, because the definition has no `context`.
    Default,
    /// A `startup` path (always part of the want set).
    Startup,
    /// The repo override's `add`.
    RepoAdd,
}

/// One entry of the want set and what it brought.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedContextEntry {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dest: Option<String>,
    pub source: ContextEntrySource,
    pub files: u64,
    pub bytes: u64,
}

/// One file of the want set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContextFile {
    /// Where it lands in the workspace.
    pub destination: String,
    /// Its path in its tier.
    pub logical: String,
    pub bytes: u64,
    pub mode: u32,
    pub tier: ContextTier,
    /// SW-XS-7: where its bytes are read from (its tier root joined with
    /// `logical`). Never serialized: previews name the tier, not a path.
    #[serde(skip)]
    pub source: PathBuf,
}

/// The resolved want set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContextResolution {
    /// `host/owner/repo` when resolved for a repo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// True when the definition has no `context` and the runtime default
    /// was used.
    pub defaulted: bool,
    pub entries: Vec<ResolvedContextEntry>,
    /// Sorted by destination.
    pub files: Vec<ContextFile>,
    /// True when `files` was cut short for display ([`Self::capped`]);
    /// `totals` always count every file.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    pub totals: ManifestTotals,
    pub warnings: Vec<ContextWarning>,
}

impl ContextResolution {
    /// At most `max` files listed, `truncated` when some were left out.
    pub fn capped(mut self, max: usize) -> Self {
        if self.files.len() > max {
            self.files.truncate(max);
            self.truncated = true;
        }
        self
    }
}

/// What claimed one (folded) workspace path.
struct Claim {
    index: usize,
    entry: String,
}

/// One tier, with the exclude list that applies to it.
struct Tier {
    tier: ContextTier,
    root: crate::context_repo::inheritance::ContentRoot,
    excludes: Arc<TeamExcludes>,
    all_orgs: bool,
    /// On the org scratch tier: the files the team chose "Use team's" for.
    use_team: Vec<String>,
}

impl Tier {
    fn is_team(&self) -> bool {
        matches!(self.tier, ContextTier::Team { .. })
    }
}

/// [`resolve_context_with`] on the real file system.
pub fn resolve_context(
    home: &Path,
    principal: &Principal,
    request: ContextRequest<'_>,
) -> ContextResolution {
    resolve_context_with(&FsContentReader, home, principal, request)
}

/// SW-XS-15: each tier `target` resolves context from, with the directory
/// on this Mac a file's `logical` path is relative to, in precedence order.
/// The left nav uses it to open (and draw) a want-set file where it lives.
pub(crate) fn context_tier_roots(
    home: &Path,
    principal: &Principal,
    target: &ManifestTarget,
) -> Vec<(ContextTier, crate::context_repo::inheritance::ContentRoot)> {
    let mut ignored = Vec::new();
    tiers(&FsContentReader, home, principal, target, &mut ignored)
        .into_iter()
        .map(|t| (t.tier, t.root))
        .collect()
}

/// Resolve the want set of `request.agent` against `request.target`.
pub(crate) fn resolve_context_with(
    reader: &dyn ContentReader,
    home: &Path,
    principal: &Principal,
    request: ContextRequest<'_>,
) -> ContextResolution {
    resolve_context_at(reader, home, principal, request, None)
}

/// Use the retained checkout's team files, including additions absent from canonical main.
pub(crate) fn resolve_context_in_checkout(
    home: &Path,
    principal: &Principal,
    request: ContextRequest<'_>,
    canonical: &Path,
    checkout: &Path,
) -> ContextResolution {
    resolve_context_at(
        &FsContentReader,
        home,
        principal,
        request,
        Some((canonical, checkout)),
    )
}

fn resolve_context_at(
    reader: &dyn ContentReader,
    home: &Path,
    principal: &Principal,
    request: ContextRequest<'_>,
    checkout: Option<(&Path, &Path)>,
) -> ContextResolution {
    let ContextRequest {
        agent,
        target,
        repo_override,
        team_workspace,
    } = request;
    let mut warnings = Vec::new();
    let mut tiers = tiers(reader, home, principal, target, &mut warnings);
    // Every missing path names an ancestor that shares nothing for want of a
    // `config/sharing.yml`, the same whether or not that ancestor holds it.
    let missing_hint = roots::team_view(
        home,
        principal,
        &target.org,
        &target.team,
        target.context_root.as_ref(),
    )
    .ok()
    .and_then(|view| view.missing_path_hint())
    .map(|hint| format!("; {hint}"))
    .unwrap_or_default();
    if let Some((canonical, checkout)) = checkout {
        for tier in &mut tiers {
            tier.root = tier.root.rebased(canonical, checkout);
            tier.excludes = tier.root.excludes();
        }
    }
    // The team-level safety filter: the target team's excludes apply to every
    // tier, on top of each tier's own.
    let team_excludes = tiers
        .iter()
        .find(|t| t.is_team())
        .map(|t| Arc::clone(&t.excludes))
        .unwrap_or_else(|| Arc::new(TeamExcludes::default_only()));

    // The want set, in order: startup first (exact and pinned, so no remapped
    // entry can take its paths), then context (or the default), then the
    // override's adds. Each file goes to the first entry that claims it.
    let mut want: Vec<(ContextEntry, ContextEntrySource)> = Vec::new();
    let defaulted = agent.context.is_none();
    let startup: Vec<ContextEntry> = agent.startup.iter().map(ContextEntry::new).collect();
    for entry in startup {
        match StartupPath::parse(&entry.path) {
            Ok(path) => want.push((
                ContextEntry::new(path.to_string()),
                ContextEntrySource::Startup,
            )),
            Err(e) => warnings.push(ContextWarning {
                path: Some(entry.path.clone()),
                code: ContextWarningCode::Invalid,
                message: e.to_string(),
            }),
        }
    }
    let (declared, source) = match &agent.context {
        Some(entries) => (entries.clone(), ContextEntrySource::Context),
        None => (
            default_context(agent.runtime, team_workspace),
            ContextEntrySource::Default,
        ),
    };
    push_valid(&mut want, &mut warnings, &declared, source);
    let removals = repo_override.map(|o| match o.normalized() {
        Ok(o) => {
            push_valid(
                &mut want,
                &mut warnings,
                &o.add,
                ContextEntrySource::RepoAdd,
            );
            o.remove_set()
        }
        Err(e) => {
            warnings.push(ContextWarning {
                path: None,
                code: ContextWarningCode::Invalid,
                message: e.to_string(),
            });
            globset::GlobSet::empty()
        }
    });
    let removed = |path: &str| {
        removals.as_ref().is_some_and(|set| {
            std::iter::once(path)
                .chain(path.match_indices('/').map(|(i, _)| &path[..i]))
                .any(|p| set.is_match(p))
        })
    };

    let mut files: Vec<ContextFile> = Vec::new();
    // Folded (case-insensitive, NFC) destination → what claimed it.
    let mut claimed: BTreeMap<String, Claim> = BTreeMap::new();
    let mut entries = Vec::with_capacity(want.len());
    // An entry named twice (the root doc in startup and in the default) is
    // one entry, credited to its first naming.
    let mut seen: BTreeSet<(String, Option<String>)> = BTreeSet::new();
    let mut next = 0;
    while next < want.len() {
        let (entry, source) = want[next].clone();
        next += 1;
        if !seen.insert((entry.path.clone(), entry.dest.clone())) {
            continue;
        }
        let mut resolved = ResolvedContextEntry {
            path: entry.path.clone(),
            dest: entry.dest.clone(),
            source,
            files: 0,
            bytes: 0,
        };
        let warn = |code, message: String| ContextWarning {
            path: Some(entry.path.clone()),
            code,
            message,
        };
        if not_carried(&entry.path)
            || is_junk(&entry.path)
            || is_projection_excluded(&entry.path, &team_excludes)
        {
            warnings.push(warn(
                ContextWarningCode::Excluded,
                format!(
                    "{} is in the team's excluded paths, so it is never copied into a workspace",
                    entry.path
                ),
            ));
            entries.push(resolved);
            continue;
        }
        let found = expand(
            reader,
            &tiers,
            &team_excludes,
            &entry.path,
            source != ContextEntrySource::Startup,
        );
        // A path that could not be read is reported as that, not as missing.
        let blocked = !found.problems.is_empty();
        warnings.extend(
            found
                .problems
                .into_iter()
                .map(|m| warn(ContextWarningCode::Unreadable, m)),
        );
        if found.skipped > 0 {
            warnings.push(warn(
                ContextWarningCode::Skipped,
                format!(
                    "{} name(s) under {} are not usable content paths (glob characters, a \
                     backslash, or not UTF-8) and were skipped",
                    found.skipped, entry.path
                ),
            ));
        }
        if !found.present {
            // A default names what a team usually has; one it lacks is ordinary.
            // So is a startup root doc, as in the startup manifest.
            let quiet = source == ContextEntrySource::Default
                || (source == ContextEntrySource::Startup
                    && entry.path == descriptor(agent.runtime).root_doc.as_str());
            if !quiet && !blocked {
                warnings.push(warn(
                    ContextWarningCode::Missing,
                    format!("{} is in no content tier{missing_hint}", entry.path),
                ));
            }
            entries.push(resolved);
            continue;
        }
        if found.files.is_empty() {
            if source != ContextEntrySource::Default {
                warnings.push(warn(
                    ContextWarningCode::Empty,
                    format!("{} holds no file a workspace carries", entry.path),
                ));
            }
            entries.push(resolved);
            continue;
        }
        let mut unsafe_dests: Vec<String> = Vec::new();
        // Collisions grouped by the entry that kept the path: (count, example).
        let mut collided: BTreeMap<String, (usize, String)> = BTreeMap::new();
        let mut requires: Vec<StartupPath> = Vec::new();
        for (logical, (index, bytes, mode)) in found.files {
            let destination = entry.destination_of(&logical);
            // A `remove` never drops a startup path: those are exact and pinned.
            if source != ContextEntrySource::Startup && (removed(&destination) || removed(&logical))
            {
                continue;
            }
            if let Some(why) = unsafe_destination(&destination).or_else(|| {
                is_projection_excluded(&destination, &team_excludes)
                    .then(|| "it is in the team's excluded paths".to_string())
            }) {
                unsafe_dests.push(format!("{destination} ({why})"));
                continue;
            }
            let key = fold(&destination);
            if let Some(earlier) = collision(&claimed, &key) {
                let theirs = &files[earlier.index];
                let same_file = theirs.logical == logical
                    && theirs.tier == tiers[index].tier
                    && theirs.destination == destination;
                if !same_file {
                    let group = collided
                        .entry(earlier.entry.clone())
                        .or_insert_with(|| (0, String::new()));
                    group.0 += 1;
                    if group.1.is_empty() {
                        group.1 = format!("{destination} vs {}", theirs.destination);
                    }
                }
                continue;
            }
            // A startup file's `requires:` come along with it, as in the
            // startup manifest.
            if source == ContextEntrySource::Startup && logical.ends_with(".md") {
                requires.extend(requires_in(reader, &tiers[index].root, &logical));
            }
            resolved.files += 1;
            resolved.bytes += bytes;
            claimed.insert(
                key,
                Claim {
                    index: files.len(),
                    entry: entry.path.clone(),
                },
            );
            files.push(ContextFile {
                source: tiers[index]
                    .root
                    .file_path(&logical)
                    .expect("expanded admitted file"),
                destination,
                logical,
                bytes,
                mode,
                tier: tiers[index].tier.clone(),
            });
        }
        for (keeper, (count, example)) in collided {
            warnings.push(warn(
                ContextWarningCode::Collision,
                format!(
                    "{count} file(s) of {} collide with {keeper}'s at the same workspace paths \
                     (ignoring case, or a file where the other has a directory); {keeper} keeps \
                     them, e.g. {example}",
                    entry.path
                ),
            ));
        }
        // Requirements resolve right after the entry that needs them.
        let pending: Vec<(ContextEntry, ContextEntrySource)> = requires
            .into_iter()
            .filter(|r| !seen.contains(&(r.to_string(), None)))
            .map(|r| {
                (
                    ContextEntry::new(r.to_string()),
                    ContextEntrySource::Startup,
                )
            })
            .collect();
        for (offset, item) in pending.into_iter().enumerate() {
            want.insert(next + offset, item);
        }
        if let Some(first) = unsafe_dests.first() {
            warnings.push(warn(
                ContextWarningCode::UnsafeDestination,
                format!(
                    "{} file(s) of {} would land where context may never go and were skipped, \
                     e.g. {first}",
                    unsafe_dests.len(),
                    entry.path
                ),
            ));
        }
        entries.push(resolved);
    }

    files.sort_by(|a, b| a.destination.cmp(&b.destination));
    let totals = files
        .iter()
        .fold(ManifestTotals::default(), |t, f| ManifestTotals {
            files: t.files + 1,
            bytes: t.bytes + f.bytes,
        });
    if totals.bytes > LARGE_CONTEXT_BYTES || totals.files > LARGE_CONTEXT_FILES {
        warnings.push(ContextWarning {
            path: None,
            code: ContextWarningCode::Large,
            message: format!(
                "the want set is unusually large: {} files, {:.1} MB (hint: {} files or {} MB)",
                totals.files,
                totals.bytes as f64 / (1024.0 * 1024.0),
                LARGE_CONTEXT_FILES,
                LARGE_CONTEXT_BYTES / (1024 * 1024)
            ),
        });
    }
    ContextResolution {
        repo: target.repo.as_ref().and_then(|slug| repo_label(slug).ok()),
        defaulted,
        entries,
        files,
        truncated: false,
        totals,
        warnings,
    }
}

/// The key two workspace paths share when a case-insensitive file system
/// would treat them as one: NFC, then lower case.
fn fold(destination: &str) -> String {
    destination.nfc().collect::<String>().to_lowercase()
}

/// The claim `key` collides with: the same path, a file at one of its
/// ancestors, or a file beneath it.
fn collision<'m>(claimed: &'m BTreeMap<String, Claim>, key: &str) -> Option<&'m Claim> {
    if let Some(same) = claimed.get(key) {
        return Some(same);
    }
    if let Some(ancestor) = key
        .match_indices('/')
        .find_map(|(i, _)| claimed.get(&key[..i]))
    {
        return Some(ancestor);
    }
    let below = format!("{key}/");
    claimed
        .range(below.clone()..)
        .next()
        .filter(|(k, _)| k.starts_with(&below))
        .map(|(_, v)| v)
}

/// The `requires:` a Markdown file's frontmatter declares (unreadable or
/// malformed frontmatter declares none here; the startup manifest reports it).
fn requires_in(
    reader: &dyn ContentReader,
    root: &crate::context_repo::inheritance::ContentRoot,
    logical: &str,
) -> Vec<StartupPath> {
    let Ok(file) = root.open(reader, logical) else {
        return Vec::new();
    };
    let mut head = Vec::new();
    if std::io::Read::read_to_end(
        &mut std::io::Read::take(file, super::MAX_FRONTMATTER_BYTES as u64),
        &mut head,
    )
    .is_err()
    {
        return Vec::new();
    }
    super::requires_of(&head).unwrap_or_default()
}

/// Add `entries` (normalized) to the want set, warning once if they are not
/// valid. An invalid list is skipped whole, as a save would refuse it.
fn push_valid(
    want: &mut Vec<(ContextEntry, ContextEntrySource)>,
    warnings: &mut Vec<ContextWarning>,
    entries: &[ContextEntry],
    source: ContextEntrySource,
) {
    match normalize_entries(entries) {
        Ok(entries) => want.extend(entries.into_iter().map(|e| (e, source))),
        Err(e) => warnings.push(ContextWarning {
            path: None,
            code: ContextWarningCode::Invalid,
            message: e.to_string(),
        }),
    }
}

/// The tiers in precedence order: the target team's repo tier when the
/// target names a repo, then the manifest resolver's own layers.
fn tiers(
    reader: &dyn ContentReader,
    home: &Path,
    principal: &Principal,
    target: &ManifestTarget,
    warnings: &mut Vec<ContextWarning>,
) -> Vec<Tier> {
    let mut out = Vec::new();
    if let Ok(view) = roots::team_view(
        home,
        principal,
        &target.org,
        &target.team,
        target.context_root.as_ref(),
    ) {
        warnings.extend(view.diagnostics.iter().map(|message| ContextWarning {
            path: None,
            code: ContextWarningCode::Layer,
            message: message.clone(),
        }));
    }
    if let Some(root) = repo_tier_root(reader, home, principal, target, warnings) {
        let team_root = roots::org_root(home, principal, &target.org, target.context_root.as_ref())
            .ok()
            .map(|r| crate::context_repo::path_to_team_content_dir(&r, &target.team));
        out.push(Tier {
            tier: ContextTier::Repo {
                org: target.org.clone(),
                team: target.team.clone(),
                repo: target
                    .repo
                    .as_ref()
                    .and_then(|slug| repo_label(slug).ok())
                    .unwrap_or_default(),
            },
            excludes: team_root
                .as_deref()
                .map(load_team_excludes_cached)
                .unwrap_or_else(|| Arc::new(TeamExcludes::default_only())),
            root: crate::context_repo::inheritance::ContentRoot::own(root),
            all_orgs: false,
            use_team: Vec::new(),
        });
    }
    let mut errors = Vec::new();
    for layer in layers(reader, home, principal, target, &mut errors) {
        let (tier, all_orgs) = match &layer.id {
            ContentLayer::Team { org, team } => (
                ContextTier::Team {
                    org: org.clone(),
                    team: team.clone(),
                },
                false,
            ),
            ContentLayer::Personal { org, .. } => {
                (ContextTier::Personal { org: org.clone() }, org.is_none())
            }
        };
        out.push(Tier {
            tier,
            excludes: layer.root.excludes(),
            root: layer.root,
            all_orgs,
            use_team: layer.use_team,
        });
    }
    warnings.extend(errors.into_iter().map(|e| ContextWarning {
        path: None,
        code: ContextWarningCode::Layer,
        message: e.message,
    }));
    out
}

/// The target team's `repos/{host}/{owner}/{repo}/content/` (host-aware, with
/// the legacy `repos/{owner}/{repo}/` layout honoured the way projection
/// resolves it), when the target names a repo and the tier exists.
/// SW-XS-8: the target team's per-repo tier root for `target.repo`, when
/// it exists and is reachable without a symlink.
pub(crate) fn repo_tier_path(
    home: &Path,
    principal: &Principal,
    target: &ManifestTarget,
) -> Option<PathBuf> {
    repo_tier_root(&FsContentReader, home, principal, target, &mut Vec::new())
}

fn repo_tier_root(
    reader: &dyn ContentReader,
    home: &Path,
    principal: &Principal,
    target: &ManifestTarget,
    warnings: &mut Vec<ContextWarning>,
) -> Option<PathBuf> {
    let slug = target.repo.as_ref()?;
    if target.org.is_empty() || target.team.is_empty() || is_scratch_team(&target.team) {
        return None;
    }
    let org_root =
        roots::org_root(home, principal, &target.org, target.context_root.as_ref()).ok()?;
    let root = match crate::commands::layout_resolver::resolve_tracked_repository_path(
        home,
        Some(principal),
        &org_root,
        &target.team,
        slug,
    ) {
        Ok(dir) => dir.join(crate::context_repo::TEAM_CONTENT_DIR),
        Err(e) => {
            warnings.push(ContextWarning {
                path: None,
                code: ContextWarningCode::Layer,
                message: format!("cannot resolve the repo tier: {e}"),
            });
            return None;
        }
    };
    if let Some(link) = symlink_on_the_way(reader, &org_root, &root) {
        warnings.push(ContextWarning {
            path: None,
            code: ContextWarningCode::Unreadable,
            message: format!(
                "the repo tier is reached through the symlink {}",
                link.display()
            ),
        });
        return None;
    }
    match reader.node(&root) {
        Ok(node) if node.kind == NodeKind::Dir => Some(root),
        _ => None,
    }
}

/// What one prefix expands to across the tiers.
struct Expanded {
    /// Whether any tier has the prefix at all.
    present: bool,
    /// Logical path → (winning tier index, bytes, mode).
    files: BTreeMap<String, (usize, u64, u32)>,
    problems: Vec<String>,
    /// Names skipped as unusable content paths.
    skipped: usize,
}

/// Every carried file under `prefix`, each from the first tier that has it.
/// `bulk` is false for startup paths, which keep every file: no ignore rules apply.
fn expand(
    reader: &dyn ContentReader,
    tiers: &[Tier],
    team_excludes: &TeamExcludes,
    prefix: &str,
    bulk: bool,
) -> Expanded {
    let mut out = Expanded {
        present: false,
        files: BTreeMap::new(),
        problems: Vec::new(),
        skipped: 0,
    };
    // Logical path → every tier holding it, in precedence order.
    let mut holders: BTreeMap<String, Vec<(usize, u64, u32)>> = BTreeMap::new();
    for (index, tier) in tiers.iter().enumerate() {
        let kind = match locate(reader, &tier.root, prefix) {
            Ok(Some(kind)) => kind,
            Ok(None) => continue,
            Err(why) => {
                out.problems.push(format!("{prefix}: {why}"));
                continue;
            }
        };
        out.present = true;
        let mut listed = Vec::new();
        match kind {
            NodeKind::Dir => {
                if let Err(why) = walk(
                    reader,
                    tier,
                    team_excludes,
                    prefix,
                    bulk,
                    &mut listed,
                    &mut out.skipped,
                ) {
                    out.problems.push(why);
                }
            }
            NodeKind::File => {
                let ignored = bulk
                    && tier
                        .root
                        .ignore_rules()
                        .is_some_and(|rules| rules.ignored(&[prefix.to_string()]).contains(prefix));
                if carried(tier, team_excludes, prefix) && tier.root.admits(prefix) && !ignored {
                    if let Ok(node) = tier.root.node(reader, prefix) {
                        listed.push((prefix.to_string(), node.mode));
                    }
                }
            }
            _ => continue,
        }
        for (logical, mode) in listed {
            if tier.all_orgs && !inert(reader, &tier.root, &logical) {
                continue;
            }
            let bytes = tier
                .root
                .file_path(&logical)
                .and_then(|p| reader.file_len(&p).ok())
                .unwrap_or(0);
            holders
                .entry(logical)
                .or_default()
                .push((index, bytes, mode));
        }
    }
    for (logical, found) in holders {
        // SLW-PT-20: an org scratch file the team chose "Use team's" for
        // yields to a team tier that has it.
        let pick = found
            .iter()
            .find(|(index, ..)| {
                let tier = &tiers[*index];
                !(tier.use_team.contains(&logical)
                    && found.iter().any(|(other, ..)| tiers[*other].is_team()))
            })
            .or_else(|| found.first())
            .copied();
        if let Some(pick) = pick {
            out.files.insert(logical, pick);
        }
    }
    out
}

/// Whether `logical` is carried from `tier`: not cruft, not excluded by the
/// tier's own list or the team's.
fn carried(tier: &Tier, team_excludes: &TeamExcludes, logical: &str) -> bool {
    !not_carried(logical)
        && !is_junk(logical)
        && !is_projection_excluded(logical, &tier.excludes)
        && !is_projection_excluded(logical, team_excludes)
}

/// All-orgs carries allowlisted inert text only.
fn inert(
    reader: &dyn ContentReader,
    root: &crate::context_repo::inheritance::ContentRoot,
    logical: &str,
) -> bool {
    crate::context_repo::scratch_space::inert_kind(logical).is_some()
        && root.open(reader, logical).is_ok_and(|mut f| {
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut f, &mut bytes).is_ok()
                && crate::context_repo::scratch_space::is_inert_content(&bytes)
        })
}

/// What `prefix` names under `root`, spelled exactly so (a case-insensitive
/// file system finds `Docs` for `docs`; the content path is `docs` only if
/// it is spelled so). `None` when absent; an error for a symlink on the way.
fn locate(
    reader: &dyn ContentReader,
    root: &crate::context_repo::inheritance::ContentRoot,
    prefix: &str,
) -> Result<Option<NodeKind>, String> {
    let segments: Vec<&str> = prefix.split('/').collect();
    let mut at = String::new();
    for (i, segment) in segments.iter().enumerate() {
        let parent = at.clone();
        if !at.is_empty() {
            at.push('/');
        }
        at.push_str(segment);
        let node = match root.node(reader, &at) {
            Ok(node) => node,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("cannot read {}: {e}", at)),
        };
        let spelled = root
            .list(reader, &parent)
            .map_err(|e| format!("cannot list {}: {e}", parent))?
            .iter()
            // NFC on both sides: a name written decomposed is the same path.
            .any(|n| n.to_str().is_some_and(|n| n.nfc().eq(segment.chars())));
        if !spelled {
            return Ok(None);
        }
        let last = i + 1 == segments.len();
        match node.kind {
            NodeKind::Symlink => return Err(format!("{} is a symlink", segments[..=i].join("/"))),
            NodeKind::Dir | NodeKind::File if last => return Ok(Some(node.kind)),
            NodeKind::Dir => {}
            NodeKind::File | NodeKind::Other => return Ok(None),
        }
    }
    Ok(None)
}

/// Every carried regular file under `dir` in `tier`. Symlinks are skipped
/// (and reported); cruft, build and VCS subtrees and excluded paths too.
fn walk(
    reader: &dyn ContentReader,
    tier: &Tier,
    team_excludes: &TeamExcludes,
    dir: &str,
    bulk: bool,
    out: &mut Vec<(String, u32)>,
    skipped: &mut usize,
) -> Result<(), String> {
    let mut stack = vec![dir.to_string()];
    let mut first_problem = None;
    let rules = if bulk { tier.root.ignore_rules() } else { None };
    while let Some(dir) = stack.pop() {
        let names = match tier.root.list(reader, &dir) {
            Ok(names) => names,
            Err(e) => {
                first_problem.get_or_insert(format!("cannot list {dir}: {e}"));
                continue;
            }
        };
        let total = names.len();
        let mut names: Vec<String> = names
            .into_iter()
            .filter_map(|n| n.into_string().ok())
            .collect();
        *skipped += total - names.len();
        names.sort();
        let logicals: Vec<String> = names.iter().map(|name| format!("{dir}/{name}")).collect();
        let ignored = rules
            .as_ref()
            .map(|rules| rules.ignored(&logicals))
            .unwrap_or_default();
        for name in names.into_iter().rev() {
            let logical = format!("{dir}/{name}");
            if !carried(tier, team_excludes, &logical) {
                continue;
            }
            if StartupPath::parse(&logical).map_or(true, |p| p.as_str() != logical) {
                *skipped += 1;
                continue;
            }
            match tier.root.node(reader, &logical) {
                Ok(node) => match node.kind {
                    NodeKind::Dir => stack.push(logical),
                    // Git-ignored output is expected, not an unusable name, so it is not counted as skipped.
                    NodeKind::File if ignored.contains(&logical) => {}
                    NodeKind::File if tier.root.admits(&logical) => out.push((logical, node.mode)),
                    NodeKind::File => {}
                    NodeKind::Symlink => {
                        first_problem.get_or_insert(format!("{logical} is a symlink"));
                    }
                    NodeKind::Other => {}
                },
                Err(e) => {
                    first_problem.get_or_insert(format!("cannot read {logical}: {e}"));
                }
            }
        }
    }
    out.sort();
    match first_problem {
        Some(why) => Err(why),
        None => Ok(()),
    }
}

/// The git ignore rules of the context repo: the repository that encloses the
/// context folder, as git reads them (`.gitignore` at any depth and
/// `.git/info/exclude`, which is local to each clone). Global excludes are left
/// out, so the global git config of a machine does not change what is carried.
/// A folder outside any git repository has no rules.
pub(crate) struct IgnoreRules {
    root: PathBuf,
}

impl IgnoreRules {
    pub(crate) fn for_root(root: &Path) -> Option<Self> {
        git_output(root, &["rev-parse", "--show-toplevel"])?;
        Some(Self {
            root: root.to_path_buf(),
        })
    }

    /// The logical paths among `logicals` (relative to the root) that git
    /// ignores. Tracked files are never reported, as git's default says. Any
    /// failure reads as "nothing ignored", so the file is carried as before.
    fn ignored(&self, logicals: &[String]) -> HashSet<String> {
        if logicals.is_empty() {
            return HashSet::new();
        }
        let Ok(mut child) = git_command(
            &self.root,
            &[
                "-c",
                "core.excludesFile=/dev/null",
                "check-ignore",
                "--stdin",
                "-z",
            ],
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn() else {
            return HashSet::new();
        };
        let Some(mut stdin) = child.stdin.take() else {
            return HashSet::new();
        };
        let body: Vec<u8> = logicals.iter().flat_map(|p| p.bytes().chain([0])).collect();
        // Written from a thread so a long list cannot fill the output pipe and deadlock.
        let writer = std::thread::spawn(move || stdin.write_all(&body));
        let Ok(output) = child.wait_with_output() else {
            return HashSet::new();
        };
        let _ = writer.join();
        // Exit 0 means some paths are ignored, 1 means none; anything else is an error.
        if !matches!(output.status.code(), Some(0) | Some(1)) {
            return HashSet::new();
        }
        output
            .stdout
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect()
    }
}

/// The git binary: the first `git` on PATH, or in the usual install places,
/// since an app launched from Finder gets a minimal PATH.
fn git_program() -> PathBuf {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    dirs.into_iter()
        .map(|dir| dir.join("git"))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| PathBuf::from("git"))
}

/// A git command in `root`, with the caller's repository overrides removed so
/// only the repository at `root` decides anything.
fn git_command(root: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(git_program());
    command
        .arg("-C")
        .arg(root)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_NAMESPACE")
        .stderr(Stdio::null());
    command
}

fn git_output(root: &Path, args: &[&str]) -> Option<String> {
    let output = git_command(root, args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;
