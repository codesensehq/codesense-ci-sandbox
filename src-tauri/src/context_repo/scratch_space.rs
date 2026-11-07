// SPDX-License-Identifier: MIT
//! The Scratch space: ONE local git repo per local profile holding everything
//! that is "only me", with every item scoped by the folder it is filed under.
//!
//! ```text
//! {codesense_home}/users/{profile}/scratch/      ← the repo (git-initialised here, never pushed)
//!   all-orgs/                                    ← inert carry-everywhere items only:
//!                                                  skills, prompts, snippets, templates
//!   orgs/<owner>/<repo>/                         ← everything for ONE org, laid out like a team dir:
//!     content/   notes/   config/   libraries/
//! ```
//!
//! This module is the single owner of every scratch path. Nothing else joins a
//! scratch segment: every personal fold (dispatch, reactive triggers,
//! canvas, notes, routines, agents, the content overlay, move/copy/remove)
//! asks this module for the ACTIVE org's space, so an item filed under org A is
//! invisible and inert in org B. `all-orgs/` is the one place that resolves in
//! every org, and it refuses anything that is not an inert kind.
//!
//! Identity is the local profile ([`Principal`]); the repo is local only.
//! CodeSense initialises it for history and never adds a remote or pushes it —
//! backing it up is the user's (or an agent's) business.

use std::path::{Path, PathBuf};

use super::org_slug::validate_org_slug;
use super::path::{
    path_to_team_config_dir, path_to_team_content_dir, path_to_team_libraries_dir,
    path_to_team_notes_dir, path_to_team_panes_dir, path_to_user_home,
};
use crate::content_engine::is_safe_logical_name;
use crate::domain::Principal;

const SCRATCH_DIR: &str = "scratch";
const ALL_ORGS_DIR: &str = "all-orgs";
const ORGS_DIR: &str = "orgs";

/// The team name an org's scratch space takes wherever an identity is
/// `<org>/<team>`-shaped (agent and routine keys, content layers, events). A
/// leading underscore is not a valid team name, so it never names a real team.
pub const SCRATCH_TEAM: &str = "_scratch";

/// The projection-key prefixes the content overlay gives scratch entries: an
/// org's scratch space (`~scratch:<entry>`) and the all-orgs folder
/// (`~all-orgs:<entry>`). A lockfile entry under the all-orgs prefix is never a
/// reverse-sync target (no edit-back, adopt, delete, rename or move).
pub const SCRATCH_PROJECTION_PREFIX: &str = "~scratch:";
pub const ALL_ORGS_PROJECTION_PREFIX: &str = "~all-orgs:";

/// Whether `team` is the scratch-space marker rather than a real team.
pub fn is_scratch_team(team: &str) -> bool {
    team == SCRATCH_TEAM
}

/// Why a scratch path could not be resolved or written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScratchError {
    /// The principal is not a local profile (e.g. the signed-out sentinel).
    NoProfile,
    /// The org slug is not a safe `<owner>/<repo>`.
    InvalidOrg(String),
    /// The logical path is unsafe (absolute, `..`, control chars, …).
    UnsafePath(String),
    /// `all-orgs/` accepts inert kinds only.
    NotInert(String),
    /// Filesystem or git failure.
    Io(String),
}

impl std::fmt::Display for ScratchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoProfile => f.write_str("no local profile is selected"),
            Self::InvalidOrg(why) => write!(f, "invalid org for the private space: {why}"),
            Self::UnsafePath(path) => write!(f, "unsafe private-space path: {path:?}"),
            Self::NotInert(path) => write!(
                f,
                "{path:?} cannot go in all-orgs: only skills, prompts, snippets and templates \
                 apply in every org; file it under an org instead"
            ),
            Self::Io(e) => f.write_str(e),
        }
    }
}

impl From<ScratchError> for String {
    fn from(e: ScratchError) -> Self {
        e.to_string()
    }
}

fn check_profile(principal: &Principal) -> Result<(), ScratchError> {
    Principal::parse_local(principal.as_str())
        .map(|_| ())
        .map_err(|_| ScratchError::NoProfile)
}

/// The profile's scratch repo root. Pure path math.
pub fn scratch_root(codesense_home: &Path, principal: &Principal) -> PathBuf {
    path_to_user_home(codesense_home, principal).join(SCRATCH_DIR)
}

/// The inert carry-everywhere root. Pure path math.
pub fn scratch_all_orgs_root(codesense_home: &Path, principal: &Principal) -> PathBuf {
    scratch_root(codesense_home, principal).join(ALL_ORGS_DIR)
}

/// The root of `org_slug`'s scratch space. Validates the slug, so a hostile
/// slug can never point outside `orgs/`. Pure path math otherwise.
pub fn scratch_org_root(
    codesense_home: &Path,
    principal: &Principal,
    org_slug: &str,
) -> Result<PathBuf, ScratchError> {
    if let Some(why) = validate_org_slug(org_slug) {
        return Err(ScratchError::InvalidOrg(why.to_string()));
    }
    Ok(scratch_root(codesense_home, principal)
        .join(ORGS_DIR)
        .join(org_slug))
}

/// Whether `logical` is agent runtime config an agent launch writes into its
/// working dir: `.mcp.json`, anything under `.codex/`, or
/// `.claude/settings.local.json`. Machine-generated, never the user's item, so
/// the scratch overlay never projects one into a worktree.
pub fn is_agent_runtime_file(logical: &str) -> bool {
    let logical = logical.trim_start_matches("./").to_ascii_lowercase();
    logical == ".mcp.json"
        || logical.starts_with(".codex/")
        || logical == ".claude/settings.local.json"
}

/// The folder of an org's scratch space that personal routine runs work in.
pub const ROUTINE_RUNS_DIR: &str = ".run";

/// One org's scratch space. Laid out like a team directory, so its parts are
/// named by the same segment constants a team's are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScratchOrgSpace {
    /// `orgs/<owner>` — the team-layout parent of the space.
    parent: PathBuf,
    /// `<repo>` — the team-layout leaf of the space.
    leaf: String,
}

impl ScratchOrgSpace {
    fn at(
        codesense_home: &Path,
        principal: &Principal,
        org_slug: &str,
    ) -> Result<Self, ScratchError> {
        Self::from_root(
            scratch_org_root(codesense_home, principal, org_slug)?,
            org_slug,
        )
    }

    fn from_root(root: PathBuf, org_slug: &str) -> Result<Self, ScratchError> {
        let (parent, leaf) = match (root.parent(), root.file_name()) {
            (Some(parent), Some(leaf)) => {
                (parent.to_path_buf(), leaf.to_string_lossy().into_owned())
            }
            _ => return Err(ScratchError::InvalidOrg(org_slug.to_string())),
        };
        Ok(Self { parent, leaf })
    }

    /// `orgs/<owner>/<repo>/`.
    pub fn root(&self) -> PathBuf {
        self.parent.join(&self.leaf)
    }

    /// Personal context / overlay files, automations and pack payloads.
    pub fn content_dir(&self) -> PathBuf {
        path_to_team_content_dir(&self.parent, &self.leaf)
    }

    /// The org's scratch notes store.
    pub fn notes_dir(&self) -> PathBuf {
        path_to_team_notes_dir(&self.parent, &self.leaf)
    }

    /// Routines, agents and other app-managed config.
    pub fn config_dir(&self) -> PathBuf {
        path_to_team_config_dir(&self.parent, &self.leaf)
    }

    /// Personal custom right panes (`panes/<id>.html`), local only like `config/`.
    pub fn panes_dir(&self) -> PathBuf {
        path_to_team_panes_dir(&self.parent, &self.leaf)
    }

    /// Where a run of the personal routine `routine_id` works: a folder of the
    /// space OUTSIDE `content/`, so what an agent launch writes into its working
    /// dir (`.mcp.json`, CLI settings) is never overlaid into the org's
    /// worktrees. Excluded from the scratch repo's git status.
    pub fn routine_run_dir(&self, routine_id: &str) -> PathBuf {
        self.root().join(ROUTINE_RUNS_DIR).join(routine_id)
    }

    /// Canvas libraries.
    pub fn libraries_dir(&self) -> PathBuf {
        path_to_team_libraries_dir(&self.parent, &self.leaf)
    }

    /// The space as the `(org_root, team)` pair team-layout code takes
    /// (`path_to_team_*_dir(org_root, team)` lands inside the space). Only for
    /// handing the space to code that computes team sub-dirs; never an
    /// identity — use the org slug and [`SCRATCH_TEAM`] for that.
    #[cfg(test)]
    pub fn team_layout(&self) -> (PathBuf, String) {
        (self.parent.clone(), self.leaf.clone())
    }
}

/// The app's active org per CodeSense home — a mirror of the UI's active team for
/// the `AppHandle`-free services. VIEWPORT ONLY: it narrows what the Routines and
/// Agents tabs list. It must never gate what fires, runs, materializes or advances
/// (CodeSense is a control plane): every run derives its org from the work itself —
/// the routine's key, the worktree's context, the event's owning team, the launch
/// target. Set when a team is activated, cleared when the profile switches or the
/// org detaches.
static ACTIVE_ORGS: std::sync::Mutex<std::collections::BTreeMap<PathBuf, String>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

fn active_orgs() -> std::sync::MutexGuard<'static, std::collections::BTreeMap<PathBuf, String>> {
    ACTIVE_ORGS.lock().unwrap_or_else(|e| e.into_inner())
}

/// The active team (of the active org) per CodeSense home — VIEWPORT ONLY, like
/// [`ACTIVE_ORGS`]: it lets the Agents tab hide personal agents in a team that has
/// switched scratch off. Never gates firing or running.
static ACTIVE_TEAMS: std::sync::Mutex<std::collections::BTreeMap<PathBuf, String>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

fn active_teams() -> std::sync::MutexGuard<'static, std::collections::BTreeMap<PathBuf, String>> {
    ACTIVE_TEAMS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Record (or clear, with `None`) the active org under `codesense_home`.
pub fn set_active_org(codesense_home: &Path, org_slug: Option<&str>) {
    let mut orgs = active_orgs();
    match org_slug {
        Some(org) => {
            orgs.insert(codesense_home.to_path_buf(), org.to_string());
        }
        None => {
            orgs.remove(codesense_home);
            active_teams().remove(codesense_home);
        }
    }
}

/// Record (or clear) the active team of the active org under `codesense_home`.
pub fn set_active_team(codesense_home: &Path, team: Option<&str>) {
    let mut teams = active_teams();
    match team {
        Some(team) => {
            teams.insert(codesense_home.to_path_buf(), team.to_string());
        }
        None => {
            teams.remove(codesense_home);
        }
    }
}

/// Forget `org_slug` wherever it is the active org (it was detached).
pub fn clear_active_org_if(org_slug: &str) {
    let mut orgs = active_orgs();
    let gone: Vec<PathBuf> = orgs
        .iter()
        .filter(|(_, org)| org.as_str() == org_slug)
        .map(|(home, _)| home.clone())
        .collect();
    orgs.retain(|_, org| org != org_slug);
    let mut teams = active_teams();
    for home in gone {
        teams.remove(&home);
    }
}

/// Forget every active org (the local profile switched).
pub fn clear_active_orgs() {
    active_orgs().clear();
    active_teams().clear();
}

/// The active org under `codesense_home`, if a team is active.
pub fn active_org(codesense_home: &Path) -> Option<String> {
    active_orgs().get(codesense_home).cloned()
}

/// The active `(org, team)` under `codesense_home`, if both are known.
pub fn active_team(codesense_home: &Path) -> Option<(String, String)> {
    let org = active_org(codesense_home)?;
    let team = active_teams().get(codesense_home).cloned()?;
    Some((org, team))
}

/// `org_slug`'s scratch space whether or not it exists yet — for addressing an
/// item that may be absent (a lookup that should report not-found). Writes go
/// through [`ensure_org_space`].
pub fn org_space_at(
    codesense_home: &Path,
    principal: &Principal,
    org_slug: &str,
) -> Result<ScratchOrgSpace, ScratchError> {
    check_profile(principal)?;
    ScratchOrgSpace::at(codesense_home, principal, org_slug)
}

/// `org_slug`'s scratch space under an already-known `scratch_root`, whether or not it exists.
/// Pure path math for a watcher that holds the scratch root but not the profile.
pub fn org_space_in(scratch_root: &Path, org_slug: &str) -> Option<ScratchOrgSpace> {
    if validate_org_slug(org_slug).is_some() {
        return None;
    }
    ScratchOrgSpace::from_root(scratch_root.join(ORGS_DIR).join(org_slug), org_slug).ok()
}

/// `org_slug`'s scratch space when it exists on disk; `None` when it was never
/// created, the slug is invalid, or there is no local profile. Read-only.
pub fn org_space(
    codesense_home: &Path,
    principal: &Principal,
    org_slug: &str,
) -> Option<ScratchOrgSpace> {
    check_profile(principal).ok()?;
    let space = ScratchOrgSpace::at(codesense_home, principal, org_slug).ok()?;
    space.root().is_dir().then_some(space)
}

/// `org_slug`'s scratch space, creating it (and the scratch repo) on first use.
/// Returns whether this call created the space.
pub fn ensure_org_space(
    codesense_home: &Path,
    principal: &Principal,
    org_slug: &str,
) -> Result<(ScratchOrgSpace, bool), ScratchError> {
    check_profile(principal)?;
    let space = ScratchOrgSpace::at(codesense_home, principal, org_slug)?;
    ensure_scratch_repo(codesense_home, principal)?;
    let created = !space.root().is_dir();
    std::fs::create_dir_all(space.content_dir())
        .map_err(|e| ScratchError::Io(format!("cannot create the private space: {e}")))?;
    Ok((space, created))
}

/// The scratch repo root, `git init`-ed on first use. Local only: no remote is
/// ever added here and nothing in CodeSense pushes it.
pub fn ensure_scratch_repo(
    codesense_home: &Path,
    principal: &Principal,
) -> Result<PathBuf, ScratchError> {
    check_profile(principal)?;
    let root = scratch_root(codesense_home, principal);
    if root.join(".git").exists() {
        return Ok(root);
    }
    std::fs::create_dir_all(&root)
        .map_err(|e| ScratchError::Io(format!("cannot create the private space: {e}")))?;
    git2::Repository::init(&root).map_err(|e| {
        ScratchError::Io(format!("cannot initialise the private space's repo: {e}"))
    })?;
    Ok(root)
}

/// The private repo has no publish target; all of its readers compare against HEAD.
pub fn is_private_repo(repo: &git2::Repository) -> bool {
    repo.config()
        .ok()
        .and_then(|c| c.get_bool("codesense.private").ok())
        .unwrap_or(false)
}

/// One marker per org lives in git metadata, outside the private working tree.
pub fn content_baseline_exists(root: &Path, org: &str) -> bool {
    git2::Repository::open(root).ok().is_some_and(|repo| {
        repo.find_reference(&format!("refs/codesense/private-baselines/{org}"))
            .is_ok()
    })
}

/// Establish the current private content as the starting point, preserving other orgs
/// and staged work. Called only by an explicit source selection, on the blocking pool.
pub fn ensure_content_baseline(
    home: &Path,
    principal: &Principal,
    org: &str,
) -> Result<bool, String> {
    let _history = crate::commands::notes_history::hold_history();
    let (space, _) = ensure_org_space(home, principal, org)?;
    let root = scratch_root(home, principal);
    let repo = git2::Repository::open(&root).map_err(|e| e.to_string())?;
    repo.config()
        .and_then(|mut c| c.set_bool("codesense.private", true))
        .map_err(|e| e.to_string())?;
    let marker = format!("refs/codesense/private-baselines/{org}");
    if repo.find_reference(&marker).is_ok() {
        return Ok(false);
    }
    let prefix = space
        .content_dir()
        .strip_prefix(&root)
        .map_err(|e| e.to_string())?
        .to_string_lossy()
        .into_owned();
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let mut original = repo.index().map_err(|e| e.to_string())?;
    let saved: Vec<_> = original.iter().collect();
    original.clear().map_err(|e| e.to_string())?;
    if let Some(parent) = &parent {
        original
            .read_tree(&parent.tree().map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    }
    original
        .remove_all([prefix.as_str()], None)
        .map_err(|e| e.to_string())?;
    original
        .add_all([prefix.as_str()], git2::IndexAddOption::DEFAULT, None)
        .map_err(|e| e.to_string())?;
    let tree_id = original.write_tree().map_err(|e| e.to_string())?;
    let tree = repo.find_tree(tree_id).map_err(|e| e.to_string())?;
    let changed = parent.as_ref().is_none_or(|p| p.tree_id() != tree_id);
    let commit_id = if changed {
        let sig =
            git2::Signature::now("CodeSense", "local@codesense").map_err(|e| e.to_string())?;
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            "Start tracking private files",
            &tree,
            &parent.iter().collect::<Vec<_>>(),
        )
        .map_err(|e| e.to_string())?
    } else {
        parent.as_ref().unwrap().id()
    };
    // Keep exactly the baseline's content entries and the original index everywhere else.
    let scoped: Vec<_> = original
        .iter()
        .filter(|e| String::from_utf8_lossy(&e.path).starts_with(&format!("{prefix}/")))
        .collect();
    original.clear().map_err(|e| e.to_string())?;
    for entry in saved
        .into_iter()
        .filter(|e| !String::from_utf8_lossy(&e.path).starts_with(&format!("{prefix}/")))
        .chain(scoped)
    {
        original.add(&entry).map_err(|e| e.to_string())?;
    }
    original.write().map_err(|e| e.to_string())?;
    repo.reference(&marker, commit_id, false, "private content baseline")
        .map_err(|e| e.to_string())?;
    Ok(changed)
}

/// The kinds `all-orgs/` accepts: inert text that never runs, connects or
/// holds a credential, so carrying it into every org leaks nothing executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InertKind {
    Skill,
    Prompt,
    Snippet,
    Template,
}

/// The regular files under `root` (never following a symlink), each with its
/// `/`-joined path relative to `root`. An absent root has none. Blocking.
pub fn regular_files(root: &Path) -> impl Iterator<Item = (PathBuf, String)> + '_ {
    walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter_map(move |entry| {
            let rel = entry.path().strip_prefix(root).ok()?;
            let logical = rel
                .components()
                .map(|c| c.as_os_str().to_str())
                .collect::<Option<Vec<_>>>()?
                .join("/");
            Some((entry.path().to_path_buf(), logical))
        })
}

/// The inert kind of a content-relative FILE path — a strict per-file
/// allowlist. `None` for anything else.
///
/// - skills: `.claude/skills/<name>/…/*.md`, `.agents/skills/<name>/…/*.md`,
///   `skills/<name>/…/*.md`
/// - prompts / snippets / templates: `prompts/…`, `snippets/…`, `templates/…`
///   ending `.md` or `.txt`
///
/// Never: settings, `.mcp.json`, hooks, agents, `.claude/commands`, scripts,
/// executables or any non-text file. Callers that hold the bytes also check
/// [`is_inert_content`].
pub fn inert_kind(logical: &str) -> Option<InertKind> {
    if !is_safe_logical_name(logical) || logical.ends_with('/') {
        return None;
    }
    let lower = logical.to_ascii_lowercase();
    let is_md = lower.ends_with(".md");
    let is_text = is_md || lower.ends_with(".txt");
    const SKILL_ROOTS: &[&str] = &[".claude/skills/", ".agents/skills/", "skills/"];
    for root in SKILL_ROOTS {
        if let Some(rest) = logical.strip_prefix(root) {
            // `<name>/<file>` at least: a bare file directly under `skills/` is
            // not a skill.
            let in_a_skill = rest.split('/').filter(|s| !s.is_empty()).count() >= 2;
            return (in_a_skill && is_md).then_some(InertKind::Skill);
        }
    }
    const TEXT_ROOTS: &[(&str, InertKind)] = &[
        ("prompts/", InertKind::Prompt),
        ("snippets/", InertKind::Snippet),
        ("templates/", InertKind::Template),
    ];
    TEXT_ROOTS.iter().find_map(|(root, kind)| {
        (logical.starts_with(root) && logical.len() > root.len() && is_text).then_some(*kind)
    })
}

/// Whether file bytes are inert text: valid UTF-8, no NUL, and no
/// `` !` `` shell-injection block (Claude Code runs those when a skill or
/// prompt loads).
pub fn is_inert_content(bytes: &[u8]) -> bool {
    match std::str::from_utf8(bytes) {
        Ok(text) => !text.contains('\0') && !text.contains("!`"),
        Err(_) => false,
    }
}

/// Whether the all-orgs file at `abs` (logical path `logical`) may be read
/// into any org: an allowlisted path, a regular file (never a symlink), and
/// inert content. The one READ gate every fold that consumes all-orgs applies.
pub fn is_inert_file(abs: &Path, logical: &str) -> bool {
    if inert_kind(logical).is_none() {
        return false;
    }
    match std::fs::symlink_metadata(abs) {
        Ok(meta) if meta.file_type().is_file() => {}
        _ => return false,
    }
    std::fs::read(abs).is_ok_and(|bytes| is_inert_content(&bytes))
}

/// Where `logical` lands in `all-orgs/`, refusing anything that is not an
/// allowlisted inert file. The one WRITE gate into the carry-everywhere root;
/// a writer holding the bytes also checks [`is_inert_content`].
pub fn all_orgs_target(
    codesense_home: &Path,
    principal: &Principal,
    logical: &str,
) -> Result<PathBuf, ScratchError> {
    check_profile(principal)?;
    if !is_safe_logical_name(logical) {
        return Err(ScratchError::UnsafePath(logical.to_string()));
    }
    if inert_kind(logical).is_none() {
        return Err(ScratchError::NotInert(logical.to_string()));
    }
    Ok(scratch_all_orgs_root(codesense_home, principal).join(logical))
}

// ---------------------------------------------------------------------------
// Per-team use: the "Use my scratch items in ‹Team›" switch and the per-file
// "Use team's" choice. Stored IN the scratch repo, in the org's own space
// (`orgs/<org>/config/teams.yml`), so it is the user's data, survives restart and
// travels with their backup. Every backend fold that applies scratch items to a
// team reads it with the team the WORK belongs to (the worktree's, the run's, the
// event's) — never the team on screen.
// ---------------------------------------------------------------------------

const TEAMS_FILE: &str = "teams.yml";

fn default_true() -> bool {
    true
}

fn is_true(v: &bool) -> bool {
    *v
}

/// One team's settings. Absent = defaults (scratch applies, no exclusions).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TeamScratchUse {
    /// "Use my scratch items in ‹Team›". Off → no overlay, automation, trigger,
    /// pack or personal agent of this org's scratch space (nor all-orgs items)
    /// applies in the team.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub use_scratch: bool,
    /// Logical content paths where the team's file wins over the scratch file
    /// at the same path ("Use team's"). The scratch file stays in scratch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub use_team_files: Vec<String>,
    /// Keys a newer build wrote: kept on rewrite, never a parse error.
    #[serde(
        flatten,
        default,
        skip_serializing_if = "std::collections::BTreeMap::is_empty"
    )]
    pub extra: std::collections::BTreeMap<String, serde_yaml::Value>,
}

impl Default for TeamScratchUse {
    fn default() -> Self {
        Self {
            use_scratch: true,
            use_team_files: Vec::new(),
            extra: std::collections::BTreeMap::new(),
        }
    }
}

impl TeamScratchUse {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct TeamsFile {
    #[serde(default)]
    teams: std::collections::BTreeMap<String, TeamScratchUse>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "std::collections::BTreeMap::is_empty"
    )]
    extra: std::collections::BTreeMap<String, serde_yaml::Value>,
}

fn check_team(team: &str) -> Result<(), ScratchError> {
    let ok = !team.trim().is_empty()
        && !is_scratch_team(team)
        && !team.contains(['/', '\\', '\0'])
        && team != "."
        && team != "..";
    if ok {
        Ok(())
    } else {
        Err(ScratchError::UnsafePath(team.to_string()))
    }
}

fn teams_file(space: &ScratchOrgSpace) -> PathBuf {
    space.config_dir().join(TEAMS_FILE)
}

/// Serializes the read-modify-write of every `teams.yml`.
static TEAMS_FILE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// `org/team`'s scratch-use settings. A team never set (or an org with no
/// scratch space) reads the defaults. A `teams.yml` that cannot be read or
/// parsed fails CLOSED for that org — scratch is off — so a damaged switch
/// never silently turns personal items back on.
pub fn team_scratch_use(
    codesense_home: &Path,
    principal: &Principal,
    org_slug: &str,
    team: &str,
) -> TeamScratchUse {
    let Some(space) = org_space(codesense_home, principal, org_slug) else {
        return TeamScratchUse::default();
    };
    match crate::shared::yaml_io::read_yaml_or_default::<TeamsFile>(&teams_file(&space)) {
        Ok(file) => file.teams.get(team).cloned().unwrap_or_default(),
        Err(e) => {
            log::warn!(
                target: "scratch_space",
                "unreadable {} ({e}); scratch items are off in {org_slug}/{team}",
                teams_file(&space).display()
            );
            TeamScratchUse {
                use_scratch: false,
                ..TeamScratchUse::default()
            }
        }
    }
}

/// Whether the user's scratch items apply in `org/team`. The one gate every
/// fold reads (overlay, dispatch, reactive triggers, packs, agents, routines).
pub fn team_uses_scratch(
    codesense_home: &Path,
    principal: &Principal,
    org_slug: &str,
    team: &str,
) -> bool {
    if is_scratch_team(team) {
        return true;
    }
    team_scratch_use(codesense_home, principal, org_slug, team).use_scratch
}

fn update_team_use(
    codesense_home: &Path,
    principal: &Principal,
    org_slug: &str,
    team: &str,
    change: impl FnOnce(&mut TeamScratchUse),
) -> Result<(), ScratchError> {
    check_team(team)?;
    let (space, _) = ensure_org_space(codesense_home, principal, org_slug)?;
    let path = teams_file(&space);
    let _guard = TEAMS_FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // A damaged file is never overwritten: every team of the org reads "off" until
    // the user fixes or deletes it, and its other teams' choices stay on disk.
    let mut file =
        crate::shared::yaml_io::read_yaml_or_default::<TeamsFile>(&path).map_err(|e| {
            ScratchError::Io(format!(
                "{} is damaged ({e}); fix or delete it to change private settings",
                path.display()
            ))
        })?;
    let mut entry = file.teams.remove(team).unwrap_or_default();
    change(&mut entry);
    entry.use_team_files.sort();
    entry.use_team_files.dedup();
    if !entry.is_default() {
        file.teams.insert(team.to_string(), entry);
    }
    crate::shared::yaml_io::write_yaml_atomic(&path, &file)
        .map_err(|e| ScratchError::Io(format!("cannot save {}: {e}", path.display())))
}

/// Where an org's scratch items are switched off.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScratchOffTeams {
    /// Teams with "Use my scratch items" off, sorted.
    pub teams: Vec<String>,
    /// The settings file cannot be read: EVERY team of the org reads off.
    pub damaged: bool,
}

/// The teams of `org_slug` with "Use my scratch items" switched off.
pub fn teams_with_scratch_off(
    codesense_home: &Path,
    principal: &Principal,
    org_slug: &str,
) -> ScratchOffTeams {
    let Some(space) = org_space(codesense_home, principal, org_slug) else {
        return ScratchOffTeams::default();
    };
    match crate::shared::yaml_io::read_yaml_or_default::<TeamsFile>(&teams_file(&space)) {
        Ok(file) => ScratchOffTeams {
            teams: file
                .teams
                .into_iter()
                .filter(|(_, entry)| !entry.use_scratch)
                .map(|(team, _)| team)
                .collect(),
            damaged: false,
        },
        Err(_) => ScratchOffTeams {
            teams: Vec::new(),
            damaged: true,
        },
    }
}

/// Turn "Use my scratch items in ‹team›" on or off.
pub fn set_team_uses_scratch(
    codesense_home: &Path,
    principal: &Principal,
    org_slug: &str,
    team: &str,
    on: bool,
) -> Result<(), ScratchError> {
    update_team_use(codesense_home, principal, org_slug, team, |entry| {
        entry.use_scratch = on;
    })
}

/// "Use team's" (`use_team = true`) for the scratch file at `logical` in
/// `org/team`, or back to "mine wins" (`false`). The scratch file is untouched.
pub fn set_team_file_uses_team(
    codesense_home: &Path,
    principal: &Principal,
    org_slug: &str,
    team: &str,
    logical: &str,
    use_team: bool,
) -> Result<(), ScratchError> {
    if !is_safe_logical_name(logical) || logical.ends_with('/') {
        return Err(ScratchError::UnsafePath(logical.to_string()));
    }
    update_team_use(codesense_home, principal, org_slug, team, |entry| {
        entry.use_team_files.retain(|p| p != logical);
        if use_team {
            entry.use_team_files.push(logical.to_string());
        }
    })
}

/// Where in a scratch repo a path lies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScratchLocation {
    /// The all-orgs folder of a profile.
    AllOrgs,
    /// One org's space (`<owner>/<repo>`, lowercased: the file system usually
    /// ignores case on macOS).
    Org(String),
    /// Elsewhere in a scratch repo (its root, `.git`, `orgs/` itself).
    Repo,
}

/// The scratch location `path` names (`…/users/<profile>/scratch/…`), judged on
/// its lexically normalised components (`.` and `..` resolved) and ignoring
/// ASCII case. `None` outside every scratch repo. Symlinks are NOT followed:
/// see [`write_refusal`] for the check that resolves them.
pub fn scratch_location(path: &Path) -> Option<ScratchLocation> {
    use std::path::Component;
    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(part) => parts.push(part.to_string_lossy().to_ascii_lowercase()),
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }
    let at = parts
        .windows(3)
        .position(|w| w[0] == super::path::USERS_DIR && w[2] == SCRATCH_DIR)?;
    let rest = &parts[at + 3..];
    Some(match rest {
        [first, ..] if first == ALL_ORGS_DIR => ScratchLocation::AllOrgs,
        [first, owner, repo, ..] if first == ORGS_DIR => {
            ScratchLocation::Org(format!("{owner}/{repo}"))
        }
        _ => ScratchLocation::Repo,
    })
}

/// `path` with its deepest existing ancestor resolved through symlinks and the
/// not-yet-existing rest appended, i.e. where a write to `path` really lands.
/// `None` when an existing entry on the way cannot be resolved (a dangling
/// link). Blocking (stats the file system).
pub fn resolve_write_target(path: &Path) -> Option<PathBuf> {
    let mut existing = path.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    while std::fs::symlink_metadata(&existing).is_err() {
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name.to_os_string());
                existing = parent.to_path_buf();
            }
            // `..` or a root that does not exist: nothing to resolve.
            _ => return Some(path.to_path_buf()),
        }
    }
    let real = std::fs::canonicalize(&existing).ok()?;
    Some(rest.iter().rev().fold(real, |p, name| p.join(name)))
}

/// Why a write to `path` must be refused, or `None`. The one gate for a
/// path-addressed writer:
///
/// - the path, once symlinks are resolved, must land in the same scratch
///   location it names: never a link from a team, another org's space or
///   anywhere else into a scratch space or all-orgs, and never a link out of a
///   scratch space (to a team's shared files or anywhere else);
/// - unless `allow_all_orgs`, it must not land in all-orgs at all — only the
///   allowlisted copy path ([`all_orgs_target`]) writes there.
///
/// Blocking (stats the file system).
pub fn write_refusal(path: &Path, allow_all_orgs: bool) -> Option<String> {
    let named = scratch_location(path);
    let Some(real_target) = resolve_write_target(path) else {
        return Some(format!("{} resolves through a broken link", path.display()));
    };
    let lands = scratch_location(&real_target);
    if !allow_all_orgs
        && (named == Some(ScratchLocation::AllOrgs) || lands == Some(ScratchLocation::AllOrgs))
    {
        return Some(format!(
            "{} is in all-orgs, which only copy_between_contexts, move_between_contexts and \
             remove_from_context (context `all-orgs`) write, for allowlisted skills, prompts, \
             snippets and templates",
            path.display()
        ));
    }
    // Any link that takes a write somewhere other than the location it names —
    // into a scratch space, or out of one (to a team's shared files or anywhere).
    if (lands.is_some() || named.is_some()) && lands != named {
        return Some(format!(
            "{} resolves through a link into {}, which is not where it says it is",
            path.display(),
            real_target.display()
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn a_space_under_a_known_scratch_root_addresses_the_same_content_dir() {
        let tmp = tempdir().unwrap();
        let home = tmp.path();
        let root = scratch_root(home, &me());
        let known = org_space_at(home, &me(), "acme/ctx").unwrap();
        let by_root = org_space_in(&root, "acme/ctx").unwrap();
        assert_eq!(by_root, known);
        assert_eq!(by_root.content_dir(), known.content_dir());
        assert!(org_space_in(&root, "../escape").is_none());
    }

    #[test]
    fn team_use_defaults_on_and_round_trips_per_team_in_the_orgs_space() {
        let tmp = tempdir().unwrap();
        let home = tmp.path();
        assert!(team_uses_scratch(home, &me(), "acme/ctx", "default"));
        set_team_uses_scratch(home, &me(), "acme/ctx", "default", false).unwrap();
        assert!(!team_uses_scratch(home, &me(), "acme/ctx", "default"));
        assert!(
            team_uses_scratch(home, &me(), "acme/ctx", "other"),
            "per team"
        );
        assert!(
            team_uses_scratch(home, &me(), "acme/b", "default"),
            "per org: the same team name elsewhere is untouched"
        );
        let file = scratch_org_root(home, &me(), "acme/ctx")
            .unwrap()
            .join("config/teams.yml");
        assert!(file.is_file(), "stored in the scratch repo");
        set_team_uses_scratch(home, &me(), "acme/ctx", "default", true).unwrap();
        assert!(team_uses_scratch(home, &me(), "acme/ctx", "default"));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap().trim(),
            "teams: {}",
            "a team back at the defaults leaves no entry"
        );
    }

    #[test]
    fn use_team_files_are_per_team_and_reversible() {
        let tmp = tempdir().unwrap();
        let home = tmp.path();
        let path = ".claude/skills/review/SKILL.md";
        set_team_file_uses_team(home, &me(), "acme/ctx", "default", path, true).unwrap();
        set_team_file_uses_team(home, &me(), "acme/ctx", "default", path, true).unwrap();
        assert_eq!(
            team_scratch_use(home, &me(), "acme/ctx", "default").use_team_files,
            vec![path.to_string()]
        );
        assert!(team_scratch_use(home, &me(), "acme/ctx", "other")
            .use_team_files
            .is_empty());
        set_team_file_uses_team(home, &me(), "acme/ctx", "default", path, false).unwrap();
        assert!(team_scratch_use(home, &me(), "acme/ctx", "default")
            .use_team_files
            .is_empty());
        assert!(set_team_file_uses_team(home, &me(), "acme/ctx", "default", "../x", true).is_err());
        assert!(set_team_uses_scratch(home, &me(), "acme/ctx", "a/b", false).is_err());
        assert!(set_team_uses_scratch(home, &me(), "acme/ctx", SCRATCH_TEAM, false).is_err());
    }

    #[test]
    fn a_damaged_settings_file_fails_closed() {
        let tmp = tempdir().unwrap();
        let home = tmp.path();
        let (space, _) = ensure_org_space(home, &me(), "acme/ctx").unwrap();
        std::fs::create_dir_all(space.config_dir()).unwrap();
        std::fs::write(space.config_dir().join("teams.yml"), "teams: [nope").unwrap();
        assert!(!team_uses_scratch(home, &me(), "acme/ctx", "default"));
        assert_eq!(
            teams_with_scratch_off(home, &me(), "acme/ctx"),
            ScratchOffTeams {
                teams: Vec::new(),
                damaged: true
            }
        );
        // A write refuses rather than overwriting what it could not read.
        let err = set_team_uses_scratch(home, &me(), "acme/ctx", "other", false).unwrap_err();
        assert!(err.to_string().contains("damaged"), "{err}");
        assert_eq!(
            std::fs::read_to_string(space.config_dir().join("teams.yml")).unwrap(),
            "teams: [nope"
        );
    }

    #[test]
    fn unknown_settings_keys_are_kept_not_fatal() {
        let tmp = tempdir().unwrap();
        let home = tmp.path();
        let (space, _) = ensure_org_space(home, &me(), "acme/ctx").unwrap();
        std::fs::create_dir_all(space.config_dir()).unwrap();
        let file = space.config_dir().join("teams.yml");
        std::fs::write(
            &file,
            "future: 1\nteams:\n  default:\n    use_scratch: false\n    colour: blue\n",
        )
        .unwrap();
        assert!(!team_uses_scratch(home, &me(), "acme/ctx", "default"));
        set_team_uses_scratch(home, &me(), "acme/ctx", "other", false).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(
            text.contains("future: 1") && text.contains("colour: blue"),
            "{text}"
        );
        assert_eq!(
            teams_with_scratch_off(home, &me(), "acme/ctx").teams,
            vec!["default".to_string(), "other".to_string()]
        );
    }

    #[test]
    fn the_off_switch_is_per_team() {
        let tmp = tempdir().unwrap();
        let home = tmp.path();
        ensure_org_space(home, &me(), "acme/ctx").unwrap();
        assert!(team_uses_scratch(home, &me(), "acme/ctx", "default"));
        set_team_uses_scratch(home, &me(), "acme/ctx", "default", false).unwrap();
        assert!(!team_uses_scratch(home, &me(), "acme/ctx", "default"));
        assert!(team_uses_scratch(home, &me(), "acme/ctx", "other"));
    }

    #[test]
    fn an_all_orgs_path_is_recognised_however_it_is_spelled() {
        let is_in_all_orgs = |path: &Path| scratch_location(path) == Some(ScratchLocation::AllOrgs);
        let home = Path::new("/h/.codesense");
        let me = Principal::from(7u64);
        let all_orgs = scratch_all_orgs_root(home, &me);
        assert!(is_in_all_orgs(&all_orgs.join("prompts/a.md")));
        assert!(is_in_all_orgs(&all_orgs));
        let sneaky = scratch_org_root(home, &me, "acme/a")
            .unwrap()
            .join("../../../all-orgs/hooks.json");
        assert!(is_in_all_orgs(&sneaky));
        assert!(is_in_all_orgs(Path::new(
            "/h/.codesense/Users/7/Scratch/ALL-ORGS/x.excalidraw"
        )));
        assert_eq!(
            scratch_location(
                &scratch_org_root(home, &me, "acme/a")
                    .unwrap()
                    .join("content/x.md")
            ),
            Some(ScratchLocation::Org("acme/a".into()))
        );
        assert_eq!(
            scratch_location(&all_orgs.join("../orgs/acme/a/content/x.md")),
            Some(ScratchLocation::Org("acme/a".into()))
        );
        assert_eq!(
            scratch_location(Path::new("/h/.codesense/users/7/orgs/acme/a")),
            None
        );
    }

    /// A real symlink inside a team (or another org's space) that points into
    /// all-orgs or an org's scratch space is refused: the gate judges where a
    /// write lands, not how the path is spelled.
    #[test]
    fn a_symlink_into_a_scratch_space_is_refused() {
        let tmp = tempdir().unwrap();
        let home = tmp.path();
        let me = Principal::from(7u64);
        let all_orgs = scratch_all_orgs_root(home, &me);
        let space_a = scratch_org_root(home, &me, "acme/a").unwrap();
        let space_b = scratch_org_root(home, &me, "acme/b").unwrap();
        for dir in [
            &all_orgs,
            &space_a.join("content"),
            &space_b.join("content"),
        ] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let team = home.join("users/7/orgs/acme/a/default/content");
        std::fs::create_dir_all(&team).unwrap();
        std::os::unix::fs::symlink(&all_orgs, team.join("carried")).unwrap();
        std::os::unix::fs::symlink(space_b.join("content"), team.join("b")).unwrap();
        std::os::unix::fs::symlink(space_b.join("content"), space_a.join("content/b")).unwrap();
        std::os::unix::fs::symlink(team.join("nowhere"), team.join("dangling")).unwrap();

        // Through a link from a team into all-orgs: refused even where all-orgs
        // writes are allowed, as it does not name all-orgs.
        for allow in [false, true] {
            assert!(write_refusal(&team.join("carried/new.excalidraw"), allow).is_some());
        }
        // From a team or from org A's space into org B's space.
        assert!(write_refusal(&team.join("b/x.md"), true).is_some());
        assert!(write_refusal(&space_a.join("content/b/x.md"), true).is_some());
        assert!(write_refusal(&team.join("dangling"), true).is_some());
        // Direct paths: a team file and an org's own space pass; all-orgs only
        // where it is allowed.
        assert_eq!(write_refusal(&team.join("x/new.md"), false), None);
        assert_eq!(write_refusal(&space_a.join("content/x.md"), false), None);
        assert!(write_refusal(&all_orgs.join("prompts/p.md"), false).is_some());
        assert_eq!(write_refusal(&all_orgs.join("prompts/p.md"), true), None);
    }

    #[test]
    fn a_symlink_out_of_a_scratch_space_is_refused() {
        let tmp = tempdir().unwrap();
        let home = tmp.path();
        let me = Principal::from(7u64);
        let space = scratch_org_root(home, &me, "acme/a").unwrap();
        std::fs::create_dir_all(space.join("content")).unwrap();
        let team = home.join("users/7/orgs/acme/a/default/content");
        std::fs::create_dir_all(team.join("docs")).unwrap();
        // A link inside the scratch space pointing at the team's shared files.
        std::os::unix::fs::symlink(team.join("docs"), space.join("content/docs")).unwrap();
        let outside = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, space.join("content/out")).unwrap();

        assert!(write_refusal(&space.join("content/docs/idea.md"), false).is_some());
        assert!(write_refusal(&space.join("content/out/idea.md"), true).is_some());
        assert_eq!(
            write_refusal(&space.join("content/plain/idea.md"), false),
            None
        );
    }

    fn me() -> Principal {
        Principal::from(42u64)
    }

    #[test]
    fn layout_is_one_repo_per_profile_with_all_orgs_and_per_org_folders() {
        let home = Path::new("/h");
        let root = scratch_root(home, &me());
        assert_eq!(root, Path::new("/h/users/42/scratch"));
        assert_eq!(scratch_all_orgs_root(home, &me()), root.join("all-orgs"));
        assert_eq!(
            scratch_org_root(home, &me(), "acme/ctx").unwrap(),
            root.join("orgs/acme/ctx")
        );
    }

    #[test]
    fn org_space_parts_sit_directly_under_the_org_folder() {
        let tmp = tempdir().unwrap();
        let (space, created) = ensure_org_space(tmp.path(), &me(), "acme/ctx").unwrap();
        assert!(created);
        let root = scratch_root(tmp.path(), &me()).join("orgs/acme/ctx");
        assert_eq!(space.root(), root);
        assert_eq!(space.notes_dir(), root.join("notes"));
        assert_eq!(space.content_dir(), root.join("content"));
        assert_eq!(space.config_dir(), root.join("config"));
        assert_eq!(space.libraries_dir(), root.join("libraries"));
        let (org_root, team) = space.team_layout();
        assert_eq!(
            path_to_team_content_dir(&org_root, &team),
            space.content_dir()
        );
        let (_, again) = ensure_org_space(tmp.path(), &me(), "acme/ctx").unwrap();
        assert!(!again, "second ensure reuses the space");
    }

    #[test]
    fn invalid_org_slugs_never_resolve() {
        // SW-CR-1 relaxed `validate_org_slug` to accept one or two safe
        // segments, so a bare `acme` is a valid slug and resolves. Single-
        // segment unsafe cases (whitespace, traversal, NUL) still reject,
        // and the three reserved single-segment owner prefixes do too.
        let tmp = tempdir().unwrap();
        for bad in [
            "",
            "acme/../x",
            "../etc",
            "a/b/c",
            // SW-CR-1 N2: single-segment unsafe shapes.
            ".",
            "..",
            " ",
            " acme",
            "no\0null",
            // SW-CR-1 M2: single-segment reserved owner prefixes.
            "_personal",
            "builtin",
            "local",
        ] {
            assert!(scratch_org_root(tmp.path(), &me(), bad).is_err(), "{bad}");
            assert!(ensure_org_space(tmp.path(), &me(), bad).is_err(), "{bad}");
            assert!(org_space(tmp.path(), &me(), bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn signed_out_sentinel_has_no_scratch_space() {
        let tmp = tempdir().unwrap();
        let nobody = Principal::from(0u64);
        assert_eq!(
            ensure_org_space(tmp.path(), &nobody, "acme/ctx").unwrap_err(),
            ScratchError::NoProfile
        );
    }

    #[test]
    fn org_space_is_exists_gated_and_per_org() {
        let tmp = tempdir().unwrap();
        assert!(org_space(tmp.path(), &me(), "acme/ctx").is_none());
        ensure_org_space(tmp.path(), &me(), "acme/ctx").unwrap();
        assert!(org_space(tmp.path(), &me(), "acme/ctx").is_some());
        assert!(
            org_space(tmp.path(), &me(), "other/ctx").is_none(),
            "org A's space never answers for org B"
        );
    }

    #[test]
    fn the_repo_is_git_initialised_locally_with_no_remote() {
        let tmp = tempdir().unwrap();
        ensure_org_space(tmp.path(), &me(), "acme/ctx").unwrap();
        let root = scratch_root(tmp.path(), &me());
        let repo = git2::Repository::open(&root).unwrap();
        assert!(repo.remotes().unwrap().is_empty());
    }

    #[test]
    fn all_orgs_accepts_allowlisted_inert_files_only() {
        let tmp = tempdir().unwrap();
        for ok in [
            ".claude/skills/review/SKILL.md",
            ".claude/skills/review/steps/one.md",
            ".agents/skills/review/SKILL.md",
            "skills/review/SKILL.md",
            "prompts/triage.md",
            "prompts/triage.txt",
            "snippets/sql.md",
            "templates/story.md",
        ] {
            assert!(all_orgs_target(tmp.path(), &me(), ok).is_ok(), "{ok}");
        }
        for refused in [
            "pipeline.yml",
            "notes/a.md",
            "CLAUDE.md",
            ".claude/settings.json",
            ".claude/settings.local.json",
            ".mcp.json",
            ".claude/hooks/notify.sh",
            ".claude/agents/reviewer.md",
            ".claude/commands/standup.md",
            ".claude/skills/review/run.sh",
            ".claude/skills/review/config.json",
            ".claude/skills/README.md",
            "skills/x.md",
            "prompts/tool.py",
        ] {
            assert_eq!(
                all_orgs_target(tmp.path(), &me(), refused).unwrap_err(),
                ScratchError::NotInert(refused.to_string()),
                "{refused}"
            );
        }
        assert!(matches!(
            all_orgs_target(tmp.path(), &me(), "skills/../pipeline.yml"),
            Err(ScratchError::UnsafePath(_))
        ));
    }

    #[test]
    fn inert_content_refuses_shell_injection_and_binary() {
        assert!(is_inert_content(b"# Review\nRead the diff.\n"));
        assert!(!is_inert_content(b"Context: !`git log -5`\n"));
        assert!(!is_inert_content(&[0xff, 0xfe, 0x00]));
        assert!(!is_inert_content(b"a\0b"));
    }

    #[test]
    fn inert_file_gate_reads_bytes_and_refuses_symlinks() {
        let tmp = tempdir().unwrap();
        let ok = tmp.path().join("ok.md");
        std::fs::write(&ok, "plain").unwrap();
        assert!(is_inert_file(&ok, "prompts/ok.md"));
        let bad = tmp.path().join("bad.md");
        std::fs::write(&bad, "!`rm -rf ~`").unwrap();
        assert!(!is_inert_file(&bad, "prompts/bad.md"));
        assert!(!is_inert_file(&ok, ".claude/settings.local.json"));
        #[cfg(unix)]
        {
            let link = tmp.path().join("link.md");
            std::os::unix::fs::symlink(&ok, &link).unwrap();
            assert!(!is_inert_file(&link, "prompts/link.md"));
        }
    }
}

#[cfg(test)]
mod private_nav_tests {
    use super::*;
    #[test]
    fn baseline_is_once_per_org_preserves_other_orgs_and_reports_later_edits() {
        let tmp = tempfile::tempdir().unwrap();
        let me = Principal::from(42u64);
        let (a, _) = ensure_org_space(tmp.path(), &me, "acme/a").unwrap();
        let (b, _) = ensure_org_space(tmp.path(), &me, "acme/b").unwrap();
        std::fs::write(a.content_dir().join("plan.md"), "initial").unwrap();
        std::fs::write(b.content_dir().join("secret.md"), "other org").unwrap();
        let root = scratch_root(tmp.path(), &me);
        let repo = git2::Repository::open(&root).unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_path(Path::new("orgs/acme/b/content/secret.md"))
            .unwrap();
        index.write().unwrap();
        assert!(ensure_content_baseline(tmp.path(), &me, "acme/a").unwrap());
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        assert!(head
            .tree()
            .unwrap()
            .get_path(Path::new("orgs/acme/a/content/plan.md"))
            .is_ok());
        assert!(head
            .tree()
            .unwrap()
            .get_path(Path::new("orgs/acme/b/content/secret.md"))
            .is_err());
        assert!(repo
            .index()
            .unwrap()
            .get_path(Path::new("orgs/acme/b/content/secret.md"), 0)
            .is_some());
        assert!(repo.remotes().unwrap().is_empty());
        std::fs::write(a.content_dir().join("plan.md"), "edited").unwrap();
        assert!(!ensure_content_baseline(tmp.path(), &me, "acme/a").unwrap());
        assert_eq!(repo.head().unwrap().target(), Some(head.id()));
        let set =
            crate::commands::git_sync::publish_set_impl(&root, None, Some("orgs/acme/a/content"))
                .unwrap();
        assert_eq!(set.general.files.len(), 1);
        assert_eq!(set.general.files[0].path, "orgs/acme/a/content/plan.md");
        let redline = crate::commands::git_sync::file_redline_impl(
            &root,
            "orgs/acme/a/content/plan.md",
            None,
        )
        .unwrap();
        assert_eq!(redline.before, "initial");
        assert_eq!(redline.after, "edited");
        assert_eq!(
            redline.baseline_sha,
            Some(head.id().to_string()[..7].to_string())
        );
    }
}
