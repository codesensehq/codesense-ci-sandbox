// SPDX-License-Identifier: MIT
// Worktree config artifact + git operations for pipeline-driven worktree
// management (story 29.2).
//
// Tasks 1–3 from the story:
// - Parse and validate `.codesense/worktree.yaml` (schema version 1)
// - Provide pipeline-runtime Tauri commands for git worktree CRUD
// - Run setup + validate subprocesses with streamed per-line events
//
// The discovery skill (Task 4) is a separate Claude skill and does NOT
// call these commands — it uses raw git CLI inside its PTY session.
// These commands are for the validate-config button (Task 5) and the
// pipeline runtime in stories 29.3+.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use log::info;
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::sync::oneshot;

use crate::commands::pty_backend::Liveness;
use crate::context_repo::try_path_to_worktree_config;
use crate::domain::Principal;
use crate::domain::SourceRepoSlug;
use crate::operations::git_remote::derive_repository_identity;

/// Identity context required to choose the authoritative physical home for a
/// tracked worktree config. Keeping these fields together prevents readers and
/// writers from accidentally applying only part of the migration epoch key.
#[derive(Clone, Debug)]
pub struct WorktreeConfigAuthority {
    pub codesense_home: PathBuf,
    pub principal: Principal,
    pub team: String,
}

impl WorktreeConfigAuthority {
    fn resolve(&self, org_root: &Path, slug: &SourceRepoSlug) -> Result<PathBuf, String> {
        super::layout_resolver::resolve_worktree_config_path(
            &self.codesense_home,
            Some(&self.principal),
            org_root,
            &self.team,
            slug,
        )
    }
}

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

/// Schema version constant. The reader hard-rejects any other value so
/// future migrations have a clean break point.
pub const WORKTREE_CONFIG_VERSION: u32 = 1;

/// Default per-command timeout (10 minutes). Overridable via the
/// `timeout_seconds` field on the config.
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 600;

/// Upper bound on draining a killed command's stdout/stderr and reaping it after
/// a timeout/cancel. On the normal path the pipes hit EOF in microseconds once
/// the process group is dead, so this cap is never reached — it exists only so a
/// descendant that escaped the group kill (e.g. one that called `setsid`) and
/// still holds the pipe open cannot hang the run indefinitely.
const POST_KILL_GRACE: Duration = Duration::from_secs(2);

/// Default directory pattern for worktree locations.
pub const DEFAULT_WORKTREE_DIR: &str = "../{repo}-wt/{slug}";

/// Default branch prefix.
pub const DEFAULT_BRANCH_PREFIX: &str = "feat/";

/// SLW-WL-2: Tauri topic emitted when the managed-worktree set CHANGES — a worktree
/// leaves (manual remove or auto merge-teardown) OR, since SLW-WL-9, a new one is
/// established (`establish_worktree_story_link`). The FE mirror is
/// `WORKTREE_CHANGED_EVENT` in `src/tauri/events.ts` — keep the two strings in
/// lockstep. `pub(crate)` so the establish path (`content_engine`) can emit the same
/// topic without minting a second string (SLW-WL-9 AC-1).
pub(crate) const WORKTREE_CHANGED_EVENT: &str = "worktree-changed";

/// Nudge any open Worktrees panel to refetch after the managed-worktree set
/// changed (add or remove). The single emit seam for [`WORKTREE_CHANGED_EVENT`]
/// so the topic string lives in exactly one place and every caller shares the
/// best-effort idiom. Best-effort (`let _`): a missing/closed listener must never
/// fail the operation that triggered the change.
pub(crate) fn emit_worktree_changed<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let _ = app.emit(WORKTREE_CHANGED_EVENT, ());
}

/// Worktree config — deserialized from the worktree.yaml at the SS-8 canonical
/// location (or the legacy in-repo path during the transition window).
///
/// SW-WT-6: the schema is **preferences only**. Every field is optional with a
/// serde default, so a hand-written config containing only `version: 1` — or an
/// empty/absent file — is valid (`read_worktree_config` maps empty → defaults).
/// Removed fields — the SW-WT-6 materialization keys (`copy_from_main`,
/// `symlink_from_main`, `symlink_to_main`; the sync/wave engine owns
/// materialization now) and the SW-WT-11 inert `validate` key (its executor/UI
/// was deleted in SW-WT-8, so the name only implied a readiness gate that ran
/// nothing) — all load fine on legacy configs: serde silently ignores unknown
/// fields (no `deny_unknown_fields`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeConfig {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub setup: Vec<String>,
    #[serde(default)]
    pub cleanup: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default = "default_branch_prefix")]
    pub branch_prefix: String,
    /// Default start point for a new story branch when no dependency branch exists.
    /// `None` means the repo's default branch ([`resolve_story_start_point`]).
    #[serde(default)]
    pub default_start_point: Option<String>,
    #[serde(default = "default_worktree_dir")]
    pub worktree_dir: String,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
    /// SW-XS-5: an older client's per-surface directory designation. Directory
    /// ownership is gone and nothing reads this; it is kept opaque so a
    /// read → write round-trip carries an older client's value through untouched
    /// until the minimum supported client version passes this release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<serde_yaml::Value>,
}

/// A content-free config (every field at its serde default — the same value an
/// empty/absent `worktree.yaml` deserializes to). Derived `Default` would wrongly
/// zero `version` and `timeout_seconds`; this routes through the serde-default
/// fns so `timeout_seconds` is the real default, not an instant timeout. Used as
/// the fallback config for the SW-EW-7 epic dev-env loop (the mid-layer prefers
/// the on-disk config; this is only used when none resolves).
impl Default for WorktreeConfig {
    fn default() -> Self {
        Self {
            version: default_version(),
            setup: Vec::new(),
            cleanup: Vec::new(),
            env: HashMap::new(),
            branch_prefix: default_branch_prefix(),
            default_start_point: None,
            worktree_dir: default_worktree_dir(),
            timeout_seconds: default_timeout_seconds(),
            scope: None,
        }
    }
}

/// SW-WT-6: `version` defaults to the current schema version so an
/// absent/empty file deserializes to a valid current-version config rather
/// than `0` (which the version gate would reject).
fn default_version() -> u32 {
    WORKTREE_CONFIG_VERSION
}

fn default_branch_prefix() -> String {
    DEFAULT_BRANCH_PREFIX.to_string()
}

fn default_worktree_dir() -> String {
    DEFAULT_WORKTREE_DIR.to_string()
}

fn default_timeout_seconds() -> u64 {
    DEFAULT_TIMEOUT_SECONDS
}

// ---------------------------------------------------------------------------
// Config reading + validation
// ---------------------------------------------------------------------------

/// Read and validate `worktree.yaml` for a source repo.
///
/// SS.8 relocated this config from `<source>/.codesense/worktree.yaml` to
/// `{org_root}/_codesense/worktree/{owner}/{repo}.yaml`. The reader checks
/// the new path first (when a slug can be derived from the source repo's
/// `origin` remote) and falls back to the legacy path for the transition
/// window. SS.12 owns the one-time migration; once it ships, the legacy
/// branch is exercised only by repos that never re-ran discovery.
///
/// Returns:
/// - `Ok(None)` if neither path exists (normal state for new repos)
/// - `Ok(Some(config))` on successful parse + validation from whichever
///   path won the resolution
/// - `Err(msg)` if the chosen path exists but is malformed, has
///   `version != 1`, fails a static validation rule, or is otherwise
///   unusable
///
/// When both paths exist, the new path wins — this is the "half-migrated"
/// state SS.12 will tidy up.
///
/// Internal Rust function — the Tauri wrappers (`probe_worktree_config`,
/// `get_worktree_config`) all thread `org_root` through from the
/// active-org TS state.
/// Resolve the canonical SS.8 worktree.yaml path
/// (`{org_root}/_codesense/worktree/{owner}/{repo}.yaml`) for a source repo.
///
/// SW-WT-4: shared by the read side (`read_worktree_config`'s canonical
/// branch) and the write side (`write_worktree_config`) so the two can never
/// drift on how the SS.8 path is derived. A repo with no `origin` is keyed by its
/// local identity. Returns `Err` when no identity derives (not a git repo, or an
/// unrecognized `origin`) — the read side treats that as "fall through to
/// the legacy path", the write side surfaces it as a hard error (it must not
/// guess a path).
fn resolve_canonical_worktree_config_path(
    org_root: &Path,
    source_repo_path: &Path,
) -> Result<PathBuf, String> {
    let slug = derive_repository_identity(source_repo_path).ok_or_else(|| {
        format!(
            "cannot derive repository identity from {}: not a git repo, or its `origin` remote isn't a recognized forge URL",
            source_repo_path.display()
        )
    })?;
    try_path_to_worktree_config(org_root, &slug)
        .map_err(|e| format!("invalid physical repository path: {e}"))
}

/// SW-WT-9: the SS-8 path with the org-root-usable guard, as an `Option` for
/// the provenance reader. Delegates the slug→path math to
/// `resolve_canonical_worktree_config_path` so the SS-8 layout has one owner.
/// Returns `None` when `org_root` isn't a usable absolute path (a misconfigured
/// frontend can pass `""`, which would make `path_to_worktree_config` produce a
/// CWD-relative path) or no slug can be derived.
pub(crate) fn ss8_worktree_config_path(
    source_repo_path: &Path,
    org_root: &Path,
) -> Option<PathBuf> {
    if org_root.as_os_str().is_empty() || !org_root.is_absolute() {
        return None;
    }
    resolve_canonical_worktree_config_path(org_root, source_repo_path).ok()
}

pub fn read_worktree_config(
    source_repo_path: &Path,
    org_root: &Path,
) -> Result<Option<WorktreeConfig>, String> {
    // Defense-in-depth: a misconfigured frontend can pass `org_root: ""`
    // (TS `resolveOrgRoot` returns `""` when `RepoState.contextRepoPath`
    // is unresolved). An empty or non-absolute `org_root` would make
    // `path_to_worktree_config` produce a CWD-relative path, and the
    // README seeder would create `./_codesense/README.md` in whatever
    // dir the Tauri process was launched from. Skip both side-effects
    // when `org_root` doesn't look like a real org root and let the
    // legacy fallback handle resolution.
    let org_root_usable = !org_root.as_os_str().is_empty() && org_root.is_absolute();
    if org_root_usable {
        // SW-WT-4: canonical-path resolution is shared with the write side
        // via `resolve_canonical_worktree_config_path`. `Err` means no
        // derivable slug — fall through to the legacy path, exactly as the
        // pre-extraction `if let Some(slug)` did.
        if let Ok(new_path) = resolve_canonical_worktree_config_path(org_root, source_repo_path) {
            // Seed `_codesense/README.md` lazily — this is the first place
            // the SS.8 namespace gets touched in production (a real source
            // repo with a real `origin` remote, against a real org root).
            // Idempotent; failure is logged and ignored so a missing README
            // never blocks worktree.yaml resolution.
            seed_codesense_namespace_readme(org_root);

            if new_path.exists() {
                return parse_and_validate(&new_path).map(Some);
            }
        }
    }
    let legacy_path = source_repo_path.join(".codesense").join("worktree.yaml");
    if legacy_path.exists() {
        log_legacy_read_once(&legacy_path);
        return parse_and_validate(&legacy_path).map(Some);
    }
    Ok(None)
}

pub fn read_worktree_config_with_authority(
    source_repo_path: &Path,
    org_root: &Path,
    authority: &WorktreeConfigAuthority,
) -> Result<Option<WorktreeConfig>, String> {
    let slug = derive_repository_identity(source_repo_path)
        .ok_or_else(|| "cannot derive repository identity for worktree authority".to_owned())?;
    let path = authority.resolve(org_root, &slug)?;
    if path.exists() {
        parse_and_validate(&path).map(Some)
    } else {
        Ok(None)
    }
}

/// Shared parse + validate body. Both branches of `read_worktree_config`
/// go through this so a malformed yaml is surfaced identically regardless
/// of which path holds it.
fn parse_and_validate(path: &Path) -> Result<WorktreeConfig, String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
    // Parse typed first so genuine errors keep their `line N column M` context.
    let config: WorktreeConfig = match serde_yaml::from_str(&contents) {
        Ok(config) => config,
        // SW-WT-6: a content-free config is a first-class "all defaults" state
        // (AC-2: absence is first-class). "Content-free" = an empty/whitespace
        // file (serde_yaml errors with EOF), a comment-only file, or an explicit
        // `null`/`~` — all deserialize to a YAML null, which the typed parse
        // rejects. Treat those as all-defaults; any other failure is a real
        // error and surfaces with its original location.
        Err(_) if yaml_is_content_free(&contents) => default_worktree_config(),
        Err(e) => return Err(format!("failed to parse {}: {}", path.display(), e)),
    };
    validate_config(&config)?;
    Ok(config)
}

/// True when `contents` is a YAML document with no content — empty/whitespace,
/// comments only, or an explicit `null`/`~`. Such a document deserializes to a
/// YAML null (which a struct deserialize rejects), so the reader treats it as
/// an all-defaults config rather than an error.
fn yaml_is_content_free(contents: &str) -> bool {
    contents.trim().is_empty()
        || matches!(
            serde_yaml::from_str::<serde_yaml::Value>(contents),
            Ok(serde_yaml::Value::Null)
        )
}

/// An all-defaults `WorktreeConfig` (every field via its serde default,
/// including `version` → `WORKTREE_CONFIG_VERSION`). Deserializing an empty
/// mapping is the one place those field defaults are applied uniformly, so a
/// content-free file and a `version: 1`-only file converge on the same value.
fn default_worktree_config() -> WorktreeConfig {
    serde_yaml::from_str("{}").expect("empty mapping deserializes via serde field defaults")
}

/// SW-WT-4: write `yaml` to the canonical SS.8 worktree config path
/// (`{org_root}/_codesense/worktree/{owner}/{repo}.yaml`) for a source repo.
///
/// Surface-agnostic core: the MCP tool (`mcp::handler`) resolves `org_root`
/// from active-org state and calls this; a future in-app Tauri command
/// (SW-WT-8) would pass the frontend-resolved root to the same core, mirroring
/// the read side's one-core/many-wrappers shape.
///
/// Order matters:
/// 1. Parse + `validate_config` (the SAME validator the read path uses, so the
///    write side inherits any SW-WT-5/6 schema changes for free) — BEFORE any
///    filesystem mutation. A malformed/invalid payload returns `Err` and never
///    touches disk: no parent dirs, no partial file.
/// 2. Resolve the canonical path (shared helper; `Err` if the repo has no
///    `origin` remote — the write side never falls back to a guessed path).
/// 3. Atomic write via the shared `write_yaml_bytes_atomic` primitive (creates
///    parent dirs, temp sibling + `rename`, cleans up on failure). The rename is
///    atomic against process interruption and against concurrent writers, so the
///    read side never observes a half-written file as the loud `Invalid` state.
///    (This is rename-atomicity, not fsync-durability — a power loss before the
///    fs flushes is out of scope, as it is for the other atomic-write callers.)
///
/// The caller's exact `yaml` bytes are persisted verbatim — round-tripping
/// through `WorktreeConfig` would drop comments and reorder keys, hence the
/// bytes variant rather than `write_yaml_atomic`. Returns the absolute written
/// path (`org_root` is already absolute), un-canonicalized so it matches the
/// path the read side computes.
#[cfg(test)]
pub fn write_worktree_config(
    source_repo_path: &Path,
    org_root: &Path,
    yaml: &str,
) -> Result<PathBuf, String> {
    // (1) Validate before any side effect.
    let config: WorktreeConfig =
        serde_yaml::from_str(yaml).map_err(|e| format!("invalid worktree.yaml: {}", e))?;
    validate_config(&config)?;

    // (2) Canonical path — Err (no slug) propagates; never guess.
    let dest = resolve_canonical_worktree_config_path(org_root, source_repo_path)?;

    // (3) Atomic write, preserving the caller's exact bytes.
    crate::shared::yaml_io::write_yaml_bytes_atomic(&dest, yaml.as_bytes())
        .map_err(|e| format!("failed to write {}: {}", dest.display(), e))?;

    Ok(dest)
}

pub fn write_worktree_config_with_authority(
    source_repo_path: &Path,
    org_root: &Path,
    authority: &WorktreeConfigAuthority,
    yaml: &str,
) -> Result<PathBuf, String> {
    let config: WorktreeConfig =
        serde_yaml::from_str(yaml).map_err(|e| format!("invalid worktree.yaml: {}", e))?;
    validate_config(&config)?;
    let slug = derive_repository_identity(source_repo_path)
        .ok_or_else(|| "cannot derive repository identity for worktree authority".to_owned())?;
    let dest = authority.resolve(org_root, &slug)?;
    crate::shared::yaml_io::write_yaml_bytes_atomic(&dest, yaml.as_bytes())
        .map_err(|e| format!("failed to write {}: {}", dest.display(), e))?;
    Ok(dest)
}

/// SLW-NC-7 (AC-1/AC-8): add or remove a TEAM nav pin — a git-shared shortcut to a
/// nav path (a dir OR a file) that everyone on the team sees pinned to the top of
/// the left nav. Persists to the git-shared team pins file under the team content
/// root ([`crate::install::team_pins`]), so a pin propagates by the ordinary
/// context-repo pull/publish — no new git file or mechanism.
///
/// `path` is repo-relative (normalized in the store). Team-scoped only (personal
/// pins are machine-local app-state, never a command). Fail-safe (AC-10): on any
/// error the prior pin set is left intact and the caller re-reads via
/// [`get_team_nav_pins`], so a failed write can't diverge the display from disk.
#[tauri::command]
pub async fn set_team_nav_pin(
    org_root: String,
    team: String,
    path: String,
    pinned: bool,
) -> Result<(), String> {
    let org_root_path = PathBuf::from(&org_root);
    if org_root.is_empty() || !org_root_path.is_absolute() {
        return Err(format!("invalid org_root: {org_root:?}"));
    }
    let team = team.trim().to_string();
    if team.is_empty() {
        return Err("team must be provided for a team nav pin".to_string());
    }
    if path.trim().is_empty() {
        return Err("pin path must not be empty".to_string());
    }
    tokio::task::spawn_blocking(move || {
        let content_dir = crate::context_repo::path_to_team_content_dir(&org_root_path, &team);
        crate::install::team_pins::set_team_pin(&content_dir, &path, pinned).map(|_| ())
    })
    .await
    .map_err(|e| format!("set_team_nav_pin task join failed: {e}"))?
}

/// SLW-NC-7 (AC-8/AC-10): the team's git-shared nav pins (repo-relative paths).
/// Read off the nav render path and re-fetched on the SLW-NC-5 composite-refresh
/// nonce, so a teammate's pulled pin appears without a restart. Fail-open: an
/// empty/relative `org_root`, an empty team, or a missing/malformed pins file
/// ⇒ an empty list, never an error — a nav shortcut hint must
/// never break the render.
#[tauri::command]
pub async fn get_team_nav_pins(org_root: String, team: String) -> Vec<String> {
    tokio::task::spawn_blocking(move || {
        let org_root_path = PathBuf::from(&org_root);
        if org_root.is_empty() || !org_root_path.is_absolute() || team.trim().is_empty() {
            return Vec::new();
        }
        let content_dir =
            crate::context_repo::path_to_team_content_dir(&org_root_path, team.trim());
        crate::install::team_pins::load_team_pins(&content_dir).team_pins()
    })
    .await
    .unwrap_or_default()
}

/// Log a single `info` line the first time a given legacy-path read
/// succeeds in this process. Repeated reads of the same legacy file (e.g.
/// the watcher firing every `.codesense/worktree.yaml` save) stay quiet.
/// The non-spammy log is enough signal to know the transition window is
/// still active without spamming the console once SS.12 has migrated.
fn log_legacy_read_once(path: &Path) {
    static SEEN: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = seen.lock().unwrap_or_else(|e| e.into_inner());
    if guard.insert(path.to_path_buf()) {
        info!(
            target: "worktree",
            "worktree.yaml read from legacy path {} — SS.12 will migrate",
            path.display()
        );
    }
}

/// SS.8 (AC-8): Drop a documentation README at `{org_root}/_codesense/README.md`
/// the first time a real source repo resolves into the new namespace.
/// Idempotent — only writes when the file is absent. Per-org-root memoized
/// so we don't restat across every `read_worktree_config` call.
///
/// Lives next to `read_worktree_config` (not in `context_repo::path`)
/// because `context_repo::path` is pure path math; the README seed is the
/// one place SS.8 owns a write into `_codesense/`.
fn seed_codesense_namespace_readme(org_root: &Path) {
    static SEEDED: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    let seeded = SEEDED.get_or_init(|| Mutex::new(HashSet::new()));
    {
        let guard = seeded.lock().unwrap_or_else(|e| e.into_inner());
        if guard.contains(org_root) {
            return;
        }
    }

    let dir = org_root.join(crate::context_repo::CODESENSE_INTERNAL_DIR);
    let readme = dir.join("README.md");
    if readme.exists() {
        let mut guard = seeded.lock().unwrap_or_else(|e| e.into_inner());
        guard.insert(org_root.to_path_buf());
        return;
    }

    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::warn!(
            target: "worktree",
            "could not create {}: {} (README seed skipped)",
            dir.display(),
            e
        );
        return;
    }
    if let Err(e) = std::fs::write(&readme, CODESENSE_NAMESPACE_README) {
        log::warn!(
            target: "worktree",
            "could not write {}: {}",
            readme.display(),
            e
        );
        return;
    }

    let mut guard = seeded.lock().unwrap_or_else(|e| e.into_inner());
    guard.insert(org_root.to_path_buf());
}

/// Static README content explaining the `_codesense/` namespace. Lives
/// here (not in a separate `.md` file) so the binary doesn't need extra
/// runtime resources to bootstrap the dir.
const CODESENSE_NAMESPACE_README: &str = "\
# `_codesense/`

Reserved directory at the org root for codesense-internal configuration.
Sibling of each team's `{team}/content/` (per-team user content). The
leading underscore guarantees this directory can never collide with a
`{owner}/{repo}/` source-repo subtree on the default branch — GitHub
usernames cannot start with `_`.

## Distinction from `.codesense/`

This org root has two reserved codesense directories with different
purposes:

- **`_codesense/`** (this directory) — first-class codesense-internal
  **config**. Visible. Git-tracked on main. Currently holds
  `worktree/{owner}/{repo}.yaml` files; future codesense-internal config
  lands here without forcing the layered content model.
- **`.codesense/`** — hidden ephemeral **markers** (e.g.
  `pts-migration.json`). Also git-tracked, but conceptually transient
  state rather than configuration.

## Current inhabitants

- `worktree/{owner}/{repo}.yaml` — per-source-repo worktree setup
  preferences. Read by codesense's worktree commands (`probe_worktree_config`,
  `get_worktree_config`). One yaml per source
  repo, deliberately shared across every team in this org that attaches it —
  its body is setup preferences only (open shell command-lines),
  identical across machines, with no absolute machine-local paths.

## What does NOT live here

Machine-local install state — the resolver lockfiles that record an
absolute `code_repo_path` — is deliberately kept OUTSIDE this clone, at
`{codesense_home}/users/{uid}/installs/{org_slug}/...` (a sibling of
`orgs/`). Nothing machine-specific belongs under `_codesense/`, so a
broad `git add` can never leak a local path into the context repo.
(Separate and unrelated: the legacy install lockfile root at
`{codesense_home}/installs/` — also outside any clone, not user-scoped.)

## Git tracking

This directory is git-tracked on the main branch. Edits committed to the
context repo and pushed to the remote share with every teammate who
pulls. CodeSense itself does not auto-commit — that follows the same
discipline as the rest of the org context repo.

This README is created automatically the first time codesense touches
the `_codesense/` namespace. Edit or replace freely — it's just
documentation.
";

/// Static validation — runs at load time, so errors surface immediately
/// rather than mid-run. Order matches the story's AC/Task breakdown.
fn validate_config(config: &WorktreeConfig) -> Result<(), String> {
    // Version gate — reject anything we don't know how to read.
    if config.version != WORKTREE_CONFIG_VERSION {
        return Err(format!(
            "unsupported worktree config version: {} (expected {})",
            config.version, WORKTREE_CONFIG_VERSION
        ));
    }

    // Empty-command-string validation on every command. Applies to setup and
    // cleanup — env is a map so it's not parsed as a command (SW-WT-11 dropped
    // the `validate` link with the field). Shell syntax is intentionally
    // unrestricted (commands run via /bin/sh -c).
    for cmd in config.setup.iter().chain(config.cleanup.iter()) {
        validate_command_string(cmd)?;
    }

    // worktree_dir token whitelist — only {repo} and {slug} are allowed.
    validate_worktree_dir_tokens(&config.worktree_dir)?;

    Ok(())
}

/// Commands run via `/bin/sh -c` (see `run_one_command`), so any shell syntax —
/// pipes, `&&`, `$VAR`, redirects, subshells — is intentionally allowed. The
/// only load-time rejection left is an empty/whitespace-only command line, which
/// is always a config mistake. Shell-syntax errors surface at run time as a
/// non-zero exit, streamed to the UI like any other failure.
fn validate_command_string(cmd: &str) -> Result<(), String> {
    if cmd.trim().is_empty() {
        return Err("command string is empty".to_string());
    }
    Ok(())
}

/// SW-OR-1a: render a `worktree_dir` template into an absolute path, mirroring
/// the FE `resolveWorktreePath` (`src/shared/lib/worktree/resolve.ts`) EXACTLY so
/// an MCP-created worktree lands on the SAME path an interactive FE open would.
///
/// - `{repo}` → the repo dir's basename; `{slug}` → the branch name with `/`→`-`.
/// - A relative template resolves against `repo_path`; an absolute template is
///   taken as-is (parity with the FE's `rendered.startsWith("/")` branch).
/// - Reuses [`validate_worktree_dir_tokens`] so an unknown `{token}` is rejected
///   identically to config load (DRY), and [`normalize_path_components`] — the
///   SAME lexical `.`/`..` collapse [`is_codesense_managed_target`] applies — so
///   the rendered path and the managed-target check agree byte-for-byte.
///
/// Note on confinement (AC-2): a stricter "must stay under the clone's parent"
/// guard is deliberately NOT added — it would diverge from FE parity (the FE
/// honors an absolute custom `worktree_dir`) without adding safety the shipped
/// path doesn't already accept. Materialize-safety rests on the attached-clone
/// source + `write_record` registration + the spine's post-resolve
/// `is_codesense_managed_target` guard, exactly as the FE create path does. The
/// one sanity assertion kept is absoluteness (a relative result would mean a
/// non-absolute `repo_path` — a caller bug, not a valid target).
pub(crate) fn render_worktree_dir(
    template: &str,
    repo_path: &Path,
    branch_name: &str,
) -> Result<PathBuf, String> {
    validate_worktree_dir_tokens(template)?;
    let repo_basename = repo_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repo");
    let slug = branch_name.replace('/', "-");
    let rendered = template
        .replace("{repo}", repo_basename)
        .replace("{slug}", &slug);
    let joined = if Path::new(&rendered).is_absolute() {
        PathBuf::from(rendered)
    } else {
        repo_path.join(rendered)
    };
    let normalized = codesense_fs_core::gate::normalize_path_components(&joined);
    if !normalized.is_absolute() {
        return Err(format!(
            "worktree_dir '{}' rendered to a non-absolute path '{}' (repo_path must be absolute)",
            template,
            normalized.display()
        ));
    }
    Ok(normalized)
}

/// SW-OR-1a: resolve the branch to create a story's worktree on, mirroring the FE
/// `resolveWorktreeParams` (`resolve.ts`): the story's INTENDED `branch_name`
/// frontmatter when set (trimmed, non-empty), else the `{branch_prefix}{story_id}`
/// fallback (default `feat/{story_id}`). NOT `dev_branch` — that is the *observed*
/// branch, auto-written only after a worktree exists, so it is absent for exactly
/// the never-opened (cold) story this path serves.
pub(crate) fn resolve_story_branch(
    branch_name: Option<&str>,
    branch_prefix: &str,
    story_id: &str,
) -> String {
    branch_name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{branch_prefix}{story_id}"))
}

/// worktree_dir token whitelist. Only `{repo}` and `{slug}` are allowed.
/// Any other `{...}` produces a validation error.
fn validate_worktree_dir_tokens(pattern: &str) -> Result<(), String> {
    let mut rest = pattern;
    while let Some(open) = rest.find('{') {
        let after_open = &rest[open + 1..];
        let close = after_open
            .find('}')
            .ok_or_else(|| format!("worktree_dir '{}' has unclosed '{{' token", pattern))?;
        let token = &after_open[..close];
        if token != "repo" && token != "slug" {
            return Err(format!(
                "worktree_dir '{}' uses unsupported token '{{{}}}' (only {{repo}} and {{slug}} are allowed)",
                pattern, token
            ));
        }
        rest = &after_open[close + 1..];
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tauri command wrappers
// ---------------------------------------------------------------------------

/// Three-state status of a repo's worktree config file. Lets the frontend
/// distinguish "no config yet, go run discovery" (Missing) from "config
/// exists but is broken — fix it" (Invalid) without collapsing both into
/// a silent disabled button.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorktreeConfigStatus {
    Missing,
    Present,
    Invalid { error: String },
}

/// SW-SO-2 (③): the validity STATUS and the parsed CONFIG of a repo's
/// `worktree.yaml`, from a SINGLE [`read_worktree_config`]. The story-open path
/// (`ensureWorktree`) previously called `has_worktree_config` then
/// `get_worktree_config` back-to-back — two commands, two reads of the same
/// file. This fuses them: one read, both answers.
///
/// `config` is `Some` iff `status` is `Present`; `Missing`/`Invalid` carry
/// `None` — the open path throws on `Invalid` before it would use a config, and a
/// `Missing` config resolves against `resolve.ts`'s null-defaults. Reusing the
/// single reader keeps the loud/calm `Invalid`-vs-`Missing` split identical to
/// the retired `has_worktree_config` — no forked validation logic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeConfigProbe {
    pub status: WorktreeConfigStatus,
    pub config: Option<WorktreeConfig>,
}

async fn active_worktree_authority(
    app: &tauri::AppHandle,
    requested_org_root: &Path,
) -> Result<WorktreeConfigAuthority, String> {
    let ctx = crate::commands::route_to_context::derive_active_context(app).await?;
    let expected_org_root = crate::context_repo::path_to_org_root(
        &ctx.codesense_home,
        &ctx.principal(),
        &ctx.org_slug,
    )?;
    if requested_org_root != expected_org_root {
        return Err("worktree config org does not match the active principal and team".to_owned());
    }
    let principal = ctx.principal();
    Ok(WorktreeConfigAuthority {
        codesense_home: ctx.codesense_home,
        principal,
        team: ctx
            .team_name
            .ok_or_else(|| "no active team selected — pick a team first".to_owned())?,
    })
}

fn probe_worktree_config_core(
    repo: &Path,
    org: &Path,
    authority: Option<&WorktreeConfigAuthority>,
) -> WorktreeConfigProbe {
    let result = authority.map_or_else(
        || read_worktree_config(repo, org),
        |authority| read_worktree_config_with_authority(repo, org, authority),
    );
    match result {
        Ok(Some(config)) => WorktreeConfigProbe {
            status: WorktreeConfigStatus::Present,
            config: Some(config),
        },
        Ok(None) => WorktreeConfigProbe {
            status: WorktreeConfigStatus::Missing,
            config: None,
        },
        Err(error) => WorktreeConfigProbe {
            status: WorktreeConfigStatus::Invalid { error },
            config: None,
        },
    }
}

/// Fused status + config probe for the worktree-open path — replaces the
/// `has_worktree_config` + `get_worktree_config` pair at that single call site
/// (`get_worktree_config` survives for its other callers). Off-thread via
/// `await_blocking`, exactly like both predecessors.
///
/// SS.8 resolution semantics are inherited from [`read_worktree_config`] (new
/// `{org_root}/_codesense/worktree/{owner}/{repo}.yaml` path, legacy
/// `<source>/.codesense/worktree.yaml` fallback).
#[tauri::command]
pub async fn probe_worktree_config(
    app: tauri::AppHandle,
    repo_path: String,
    org_root: String,
) -> Result<WorktreeConfigProbe, String> {
    let org = PathBuf::from(&org_root);
    let authority = active_worktree_authority(&app, &org).await?;
    crate::commands::await_blocking("probe_worktree_config", move || {
        let repo = PathBuf::from(&repo_path);
        Ok(probe_worktree_config_core(&repo, &org, Some(&authority)))
    })
    .await
}

/// Load and return the validated `worktree.yaml` for a source repo.
/// Config-only counterpart to `probe_worktree_config` (which returns status +
/// config in one read for the open path); this bare loader still backs the
/// callers that only need the `WorktreeConfig` (source-diff, setup, default
/// start-point). Returns `Ok(None)` when the file doesn't exist; `Err` when
/// present but invalid.
///
/// SS.8 added `org_root`; see [`read_worktree_config`] for resolution
/// semantics.
#[tauri::command]
pub async fn get_worktree_config(
    app: tauri::AppHandle,
    repo_path: String,
    org_root: String,
) -> Result<Option<WorktreeConfig>, String> {
    let org = PathBuf::from(&org_root);
    let authority = active_worktree_authority(&app, &org).await?;
    crate::commands::await_blocking("get_worktree_config", move || {
        let repo = PathBuf::from(&repo_path);
        read_worktree_config_with_authority(&repo, &org, &authority)
    })
    .await
}

// ---------------------------------------------------------------------------
// Git worktree operations — Task 2
//
// These are the pipeline-runtime API used by (a) the validate-config
// button (Task 5) and (b) stories 29.3+. The discovery skill does NOT
// call these — it uses raw git CLI inside its Claude PTY session.
//
// All three commands shell out to the git CLI. We do NOT use git2
// because its worktree support is limited and buggy; git CLI's
// `worktree` subcommand handles every edge case we care about.
// ---------------------------------------------------------------------------

/// Pass-2 Flag 2 (SW-WS-1): `pub(crate)` so sibling modules
/// (`mcp::move_workspace`) can call git via the same resolver — honouring
/// `CODESENSE_GIT_BIN` + the bundled fallback the rest of the crate uses.
pub(crate) fn git_bin() -> String {
    // Prefer the bundled git under ~/.codesense/bin if present, mirroring
    // the pattern in commands/deps.rs and commands/git.rs. Fall back to
    // PATH git so tests run in CI without the managed install.
    if let Some(home) = super::paths::codesense_home_dir() {
        let bundled = home.join("bin").join("git");
        if bundled.exists() {
            return bundled.to_string_lossy().into_owned();
        }
    }
    "git".to_string()
}

pub(crate) fn run_git(args: &[&str], cwd: &Path) -> Result<String, String> {
    let output = Command::new(git_bin())
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("failed to spawn git: {}", e))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed ({}): {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Ensure the parent directory of a worktree path exists. Uses unconditional
/// `create_dir_all` (idempotent) rather than guarding with `.exists()` first
/// — avoids a TOCTOU race window without any benefit. Matches the pattern in
/// `context_repo/worktree.rs`.
fn ensure_parent_dir(worktree_path: &Path) -> Result<(), String> {
    if let Some(parent) = worktree_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| {
                format!(
                    "failed to create worktree parent dir {}: {}",
                    parent.display(),
                    e
                )
            })?;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn create_worktree(
    repo_path: String,
    branch_name: String,
    worktree_path: String,
) -> Result<String, String> {
    crate::commands::await_blocking("create_worktree", move || {
        let repo = PathBuf::from(&repo_path);
        if !repo.exists() {
            return Err(format!("repo path does not exist: {}", repo_path));
        }
        ensure_parent_dir(Path::new(&worktree_path))?;
        // Idempotent creation: check if the branch already exists locally.
        // If it does, reuse it instead of `-b` (which fails on existing branches).
        let branch_exists = Command::new(git_bin())
            .args([
                "rev-parse",
                "--verify",
                &format!("refs/heads/{}", branch_name),
            ])
            .current_dir(&repo)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        if branch_exists {
            // Check if a worktree is already using this branch.
            // parse_worktree_porcelain strips "refs/heads/" so compare bare names.
            let porcelain = run_git(&["worktree", "list", "--porcelain"], &repo)?;
            for entry in parse_worktree_porcelain(&porcelain) {
                if entry.branch.as_deref() == Some(&branch_name) {
                    // Branch is already checked out in an existing worktree —
                    // return that path so the caller can adopt it.
                    return Ok(entry.path);
                }
            }
            // Branch exists but no worktree uses it — attach existing branch.
            // `--` separates positionals so a hostile branch/path can't be read
            // as a flag (mirrors `fetch_and_create_worktree`).
            run_git(
                &["worktree", "add", "--", &worktree_path, &branch_name],
                &repo,
            )?;
        } else {
            // Branch doesn't exist — create new branch at current HEAD.
            run_git(
                &["worktree", "add", "-b", &branch_name, "--", &worktree_path],
                &repo,
            )?;
        }
        Ok(worktree_path)
    })
    .await
}

#[derive(Debug, Serialize)]
pub struct FetchAndCreateResult {
    pub worktree_path: String,
    pub from_remote: bool,
}

/// Compare two filesystem paths by resolved (canonical) form, tolerating the
/// macOS `/var` → `/private/var` symlink and trailing-slash differences. Falls
/// back to a string compare when either path can't be canonicalized (e.g. it
/// doesn't exist).
pub(crate) fn paths_equal(a: &str, b: &str) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(pa), Ok(pb)) => pa == pb,
        _ => a == b,
    }
}

/// Reject a value bound for a `git` argument that could be a path-traversal
/// (`..`) or a flag injection (leading `-`). Shared by the worktree checkout /
/// switch primitives so this security guard has exactly one audited definition.
fn reject_unsafe_git_arg(label: &str, val: &str) -> Result<(), String> {
    if val.contains("..") || val.starts_with('-') {
        return Err(format!("invalid {}: {}", label, val));
    }
    Ok(())
}

/// Shared branch-worktree checkout using prepared local refs.
///
/// Two policies, selected by `start_point`:
///
/// **Story-worktree mode (`Some(sp)`)** — idempotent, local-branch-first:
/// 1. Local `refs/heads/{branch}` exists → adopt its worktree, else attach it.
/// 2. Else remote `origin/{branch}` resolves → new branch from the remote tip.
/// 3. Else → mint a new branch at `sp` (a dep branch or the repo's default branch).
///
/// **Review mode (`None`)** uses the locally cached `origin/{branch}` tip.
/// Freshness against the server is unverified: prepare or refresh refs with an
/// external agent or Git CLI before opening a review. Existing review worktrees
/// fast-forward to that cached tip only when safe; divergent local work is refused.
/// Missing cached refs fail with the exact ref preparation instructions.
/// Architecture: preparation and move verification must select the same existing branch before fallback.
pub(crate) struct StoryWorktreeStart {
    pub reference: String,
    local_branch_exists: bool,
    remote_exists: bool,
}

pub(crate) fn story_worktree_start_point(
    repo: &Path,
    branch: &str,
    fallback: &str,
) -> StoryWorktreeStart {
    let local = format!("refs/heads/{branch}");
    let remote = format!("refs/remotes/origin/{branch}");
    let local_branch_exists = run_git(&["rev-parse", "--verify", &local], repo).is_ok();
    let remote_exists = run_git(&["rev-parse", "--verify", &remote], repo).is_ok();
    let reference = if local_branch_exists {
        local
    } else if remote_exists {
        remote
    } else {
        fallback.to_string()
    };
    StoryWorktreeStart {
        reference,
        local_branch_exists,
        remote_exists,
    }
}

fn checkout_branch_worktree_inner(
    repo: &Path,
    branch_name: &str,
    worktree_path: &str,
    start_point: Option<&str>,
    preparation_refspec: Option<&str>,
) -> Result<FetchAndCreateResult, String> {
    if !repo.exists() {
        return Err(format!("repo path does not exist: {}", repo.display()));
    }
    // Reject inputs with path-traversal or flag-injection sequences.
    reject_unsafe_git_arg("branch name", branch_name)?;
    reject_unsafe_git_arg("worktree path", worktree_path)?;
    if let Some(sp) = start_point {
        reject_unsafe_git_arg("start point", sp)?;
    }
    ensure_parent_dir(Path::new(worktree_path))?;

    let selected = story_worktree_start_point(repo, branch_name, start_point.unwrap_or("HEAD"));
    let remote_exists = selected.remote_exists;
    let local_branch_exists = selected.local_branch_exists;

    let remote_start = format!("origin/{}", branch_name);

    // Review uses only the locally cached remote ref; no server freshness claim.
    if start_point.is_none() {
        if remote_exists {
            // Is the branch already checked out in a worktree?
            let porcelain = run_git(&["worktree", "list", "--porcelain"], repo)?;
            if let Some(entry) = parse_worktree_porcelain(&porcelain)
                .into_iter()
                .find(|e| e.branch.as_deref() == Some(branch_name))
            {
                // Only ever mutate OUR derived managed worktree. If some other
                // worktree (e.g. the user's primary checkout) holds the branch,
                // don't touch it — surface it.
                if !paths_equal(&entry.path, worktree_path) {
                    return Err(format!(
                        "unavailable: branch '{branch_name}' is already checked out at {} — close it to open a fresh review worktree",
                        entry.path
                    ));
                }
                // Ours — fast-forward it to the PR head. A non-FF (diverged) or
                // dirty tree can't be advanced: error rather than review stale code.
                run_git(&["merge", "--ff-only", &remote_start], Path::new(&entry.path)).map_err(
                    |e| {
                        format!(
                            "unavailable: the review worktree for '{branch_name}' can't be updated to the PR head (diverged or uncommitted local changes) — resolve or remove it, then retry ({e})"
                        )
                    },
                )?;
                return Ok(FetchAndCreateResult {
                    worktree_path: entry.path,
                    from_remote: true,
                });
            }
            // Past here we create a NEW worktree at `worktree_path` (no registered
            // worktree holds the branch). A stale leftover directory there would
            // make `git worktree add` fail with an untyped error — surface it typed
            // so the review surface stays legible.
            if Path::new(worktree_path).exists() {
                return Err(format!(
                    "unavailable: a stale directory already exists at the review worktree path {worktree_path} — remove it, then retry"
                ));
            }
            if local_branch_exists {
                // A local branch of this name exists (often minted by a prior open)
                // but no worktree holds it. Advance it to the PR head when that's a
                // safe fast-forward (local is an ancestor of the remote); if it has
                // diverged, error rather than open the reviewer's stale/divergent
                // snapshot as if it were the PR.
                let ff_safe = Command::new(git_bin())
                    .args(["merge-base", "--is-ancestor", branch_name, &remote_start])
                    .current_dir(repo)
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false);
                if !ff_safe {
                    return Err(format!(
                        "unavailable: local branch '{branch_name}' has diverged from the PR head — remove it or review it manually, then retry"
                    ));
                }
                run_git(&["branch", "-f", branch_name, &remote_start], repo)?;
                run_git(&["worktree", "add", "--", worktree_path, branch_name], repo)?;
                return Ok(FetchAndCreateResult {
                    worktree_path: worktree_path.to_string(),
                    from_remote: true,
                });
            }
            // No local branch — create a fresh named branch at the PR head.
            run_git(
                &[
                    "worktree",
                    "add",
                    "-b",
                    branch_name,
                    "--",
                    worktree_path,
                    &remote_start,
                ],
                repo,
            )?;
            return Ok(FetchAndCreateResult {
                worktree_path: worktree_path.to_string(),
                from_remote: true,
            });
        }
        let refspec = preparation_refspec.map(str::to_owned).unwrap_or_else(|| {
            format!("refs/heads/{branch_name}:refs/remotes/origin/{branch_name}")
        });
        let quoted_refspec = format!("'{}'", refspec.replace('\'', "'\\''"));
        return Err(format!(
            "unavailable: PR branch '{branch_name}' is unavailable locally. In repository {}, prepare the origin source/destination refspec `{refspec}` using your agent or Git CLI (`git fetch origin {quoted_refspec}`), then retry. CodeSense does not fetch or verify remote freshness.", repo.display()
        ));
    }

    // ── STORY-WORKTREE MODE (start_point == Some): idempotent
    // local-branch-first adopt/attach, else remote tip, else mint at start_point.
    if local_branch_exists {
        // Check if a worktree is already using this branch.
        let porcelain = run_git(&["worktree", "list", "--porcelain"], repo)?;
        for entry in parse_worktree_porcelain(&porcelain) {
            if entry.branch.as_deref() == Some(branch_name) {
                return Ok(FetchAndCreateResult {
                    worktree_path: entry.path,
                    from_remote: false,
                });
            }
        }
        // Branch exists but no worktree uses it — attach existing branch.
        run_git(&["worktree", "add", "--", worktree_path, branch_name], repo)?;
        Ok(FetchAndCreateResult {
            worktree_path: worktree_path.to_string(),
            from_remote: false,
        })
    } else if remote_exists {
        run_git(
            &[
                "worktree",
                "add",
                "-b",
                branch_name,
                "--",
                worktree_path,
                &selected.reference,
            ],
            repo,
        )?;
        Ok(FetchAndCreateResult {
            worktree_path: worktree_path.to_string(),
            from_remote: true,
        })
    } else {
        match start_point {
            Some(_) => {
                run_git(
                    &["worktree", "add", "-b", branch_name, "--", worktree_path, &selected.reference],
                    repo,
                )?;
                Ok(FetchAndCreateResult {
                    worktree_path: worktree_path.to_string(),
                    from_remote: false,
                })
            }
            None => Err(format!(
                "unavailable: PR branch '{branch_name}' not found on origin or locally — cannot open for review (is the PR head pushed to this repo?)"
            )),
        }
    }
}

/// Open a story branch in a managed worktree from local refs only (nothing is fetched):
/// adopt or attach an existing local branch, else start from cached `origin/{branch_name}`,
/// else mint the branch at `start_point`, used verbatim. With no `start_point`, a new
/// branch starts from the worktree.yaml `default_start_point` as [`configured_start_point`]
/// resolves it, else from the repository's default branch ([`resolve_story_start_point`]).
/// Either is resolved only when the branch is new, so reopening an existing branch never
/// needs a resolvable default. A branch minted from a remote-tracking ref never tracks it.
#[tauri::command]
pub async fn fetch_and_create_worktree(
    repo_path: String,
    branch_name: String,
    worktree_path: String,
    start_point: Option<String>,
    default_start_point: Option<String>,
) -> Result<FetchAndCreateResult, String> {
    crate::commands::await_blocking("fetch_and_create_worktree", move || {
        story_branch_worktree_inner(
            &PathBuf::from(&repo_path),
            &branch_name,
            &worktree_path,
            start_point.as_deref(),
            default_start_point.as_deref(),
        )
    })
    .await
}

/// Open a story branch as [`fetch_and_create_worktree`] does with no explicit start point
/// and the repo's configured `default_start_point`.
pub(crate) async fn open_story_worktree(
    repo_path: String,
    branch_name: String,
    worktree_path: String,
    default_start_point: Option<String>,
) -> Result<FetchAndCreateResult, String> {
    crate::commands::await_blocking("open_story_worktree", move || {
        story_branch_worktree_inner(
            &PathBuf::from(&repo_path),
            &branch_name,
            &worktree_path,
            None,
            default_start_point.as_deref(),
        )
    })
    .await
}

fn story_branch_worktree_inner(
    repo: &Path,
    branch_name: &str,
    worktree_path: &str,
    start_point: Option<&str>,
    default_start_point: Option<&str>,
) -> Result<FetchAndCreateResult, String> {
    if !repo.exists() {
        return Err(format!("repo path does not exist: {}", repo.display()));
    }
    reject_unsafe_git_arg("branch name", branch_name)?;
    let existing = story_worktree_start_point(repo, branch_name, "");
    let minted = !existing.local_branch_exists && !existing.remote_exists;
    // An empty configured value is unset, as on the TS side.
    let default_start_point = default_start_point.filter(|configured| !configured.is_empty());
    let start_point = match (start_point, default_start_point) {
        (Some(start_point), _) => start_point.to_string(),
        // An existing branch is opened as it stands: it is its own start point.
        (None, _) if !minted => existing.reference,
        (None, Some(configured)) => configured_start_point(repo, configured),
        (None, None) => resolve_story_start_point(repo)?,
    };
    let opened =
        checkout_branch_worktree_inner(repo, branch_name, worktree_path, Some(&start_point), None)?;
    // Git makes a branch started from a remote-tracking ref track it (unless the user's
    // `branch.autoSetupMerge` says otherwise); a new story branch must not push to it.
    let tracks = || {
        run_git(
            &["config", "--get", &format!("branch.{branch_name}.merge")],
            repo,
        )
    };
    if minted && start_point.starts_with("refs/remotes/") && tracks().is_ok() {
        run_git(&["branch", "--unset-upstream", branch_name], repo)?;
    }
    if minted {
        let commit =
            run_git(&["rev-parse", &format!("refs/heads/{branch_name}")], repo).unwrap_or_default();
        info!(
            "story branch {branch_name} cut from {start_point} at {} in {}",
            commit.trim(),
            repo.display()
        );
    }
    Ok(opened)
}

/// The ref a new story branch starts from when worktree.yaml sets `default_start_point`:
/// a branch name resolves to its cached `refs/remotes/origin/` ref when one exists (so a
/// stale local branch of that name is not the base), else it is used unchanged (a local
/// branch, a full ref or a commit).
pub(crate) fn configured_start_point(repo: &Path, configured: &str) -> String {
    let remote = format!("refs/remotes/origin/{configured}");
    let names_a_branch = configured != "HEAD" && !configured.starts_with("refs/");
    if names_a_branch && run_git(&["rev-parse", "--verify", "--quiet", &remote], repo).is_ok() {
        remote
    } else {
        configured.to_string()
    }
}

/// Open the locally cached `origin/{branch}` tip for review. Remote freshness is
/// unverified; callers must prepare refs externally. Never mint at an unrelated HEAD.
pub async fn checkout_branch_worktree_for_review(
    repo_path: String,
    branch_name: String,
    worktree_path: String,
) -> Result<String, String> {
    crate::commands::await_blocking("checkout_branch_worktree_for_review", move || {
        let repo = PathBuf::from(&repo_path);
        checkout_branch_worktree_inner(&repo, &branch_name, &worktree_path, None, None)
            .map(|r| r.worktree_path)
    })
    .await
}

/// The branch `refs/remotes/origin/HEAD` names, read locally (never fetched).
fn remote_default_branch(repo: &Path) -> Option<String> {
    run_git(
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
        repo,
    )
    .ok()
    .and_then(|target| {
        target
            .trim()
            .strip_prefix("refs/remotes/origin/")
            .map(str::to_owned)
    })
}

/// The branch a brand-new agent branch starts from, read from local refs only: the
/// remote's default branch name (`origin/HEAD`), then `main` / `master` / `develop`,
/// whichever exists as a local branch; else the cached remote-tracking copy of one
/// (`origin/<name>`, never fetched); else `HEAD`. The branch the user happens to have
/// checked out is never mistaken for the default while any default is known.
pub fn local_default_start_point(repo: &Path) -> String {
    let has_ref = |reference: String| {
        run_git(&["rev-parse", "--verify", "--quiet", &reference], repo).is_ok()
    };
    let candidates: Vec<String> = remote_default_branch(repo)
        .into_iter()
        .chain(["main", "master", "develop"].map(String::from))
        .collect();
    candidates
        .iter()
        .find(|name| has_ref(format!("refs/heads/{name}")))
        .cloned()
        .or_else(|| {
            candidates
                .iter()
                .find(|name| has_ref(format!("refs/remotes/origin/{name}")))
                .map(|name| format!("origin/{name}"))
        })
        .unwrap_or_else(|| "HEAD".to_string())
}

/// The start point a new story branch takes when no explicit one is set, from local refs
/// only: the branch `origin/HEAD` names, then `main` / `master` / `develop`, each as its
/// cached `refs/remotes/origin/` ref, else as a local branch. A local branch is used only
/// when there is no remote ref of that name (a local-only repo), so a stale local `main`
/// never becomes the base. A remote ref comes back as its full ref, a local `main` /
/// `master` / `develop` as its short name and any other local branch as its full ref.
/// Where [`local_default_start_point`] would fall back to `HEAD`, a story refuses
/// instead, naming every ref tried and the fix.
pub(crate) fn resolve_story_start_point(repo: &Path) -> Result<String, String> {
    let remote_default = remote_default_branch(repo);
    let fallbacks = ["main", "master", "develop"];
    let mut tried: Vec<String> = Vec::new();
    for reference in remote_default
        .iter()
        .map(String::as_str)
        .chain(fallbacks)
        .flat_map(|name| {
            [
                format!("refs/remotes/origin/{name}"),
                format!("refs/heads/{name}"),
            ]
        })
    {
        if !tried.contains(&reference) {
            tried.push(reference);
        }
    }
    let found = tried
        .iter()
        .find(|reference| run_git(&["rev-parse", "--verify", "--quiet", reference], repo).is_ok());
    if let Some(reference) = found {
        // Any other short name could be ambiguous to `git branch` (a local `origin/x` beside
        // `refs/remotes/origin/x`), so only the conventional names go back short.
        return Ok(match reference.strip_prefix("refs/heads/") {
            Some(local) if fallbacks.contains(&local) => local.to_string(),
            _ => reference.clone(),
        });
    }
    let origin_head = match remote_default {
        Some(name) => format!("refs/remotes/origin/HEAD (→ {name})"),
        None => "refs/remotes/origin/HEAD (not set)".to_string(),
    };
    Err(format!(
        "unavailable: can't start a new story branch in {repo}: no default branch found. Tried {origin_head}, {tried}. Run `git remote set-head origin --auto` in {repo}, or set `default_start_point` in the repo's worktree.yaml (an agent can use write_worktree_config), then retry.",
        repo = repo.display(),
        tried = tried.join(", "),
    ))
}

/// The start point a new story branch in `repo_path` takes when no explicit one is set:
/// the worktree.yaml `default_start_point` as [`configured_start_point`] resolves it, else
/// the repo's default branch ([`resolve_story_start_point`]), or the refusal naming what
/// was tried. An empty configured value is unset.
#[tauri::command]
pub async fn story_default_start_point(
    repo_path: String,
    default_start_point: Option<String>,
) -> Result<String, String> {
    crate::commands::await_blocking("story_default_start_point", move || {
        let repo = PathBuf::from(&repo_path);
        if !repo.exists() {
            return Err(format!("repo path does not exist: {repo_path}"));
        }
        match default_start_point.as_deref() {
            Some(configured) if !configured.is_empty() => {
                Ok(configured_start_point(&repo, configured))
            }
            _ => resolve_story_start_point(&repo),
        }
    })
    .await
}

/// The branch a new agent worktree in `repo_path` would start from, so the launch
/// picker can name it before anything is created.
#[tauri::command]
pub async fn local_default_branch(repo_path: String) -> Result<String, String> {
    crate::commands::await_blocking("local_default_branch", move || {
        let repo = PathBuf::from(&repo_path);
        if !repo.exists() {
            return Err(format!("repo path does not exist: {repo_path}"));
        }
        Ok(local_default_start_point(&repo))
    })
    .await
}

/// Open `branch_name` in a managed worktree for an agent launch, from local refs only.
///
/// `create` mints a new branch from the repository's local default branch
/// ([`local_default_start_point`]); an existing branch is refused so a launch
/// never silently lands on someone's work. Otherwise the branch must already
/// exist locally (or as a cached `origin/` ref). A branch another worktree holds
/// stays that worktree's: the launch points there instead of moving in. Both
/// reuse the story-worktree policy of [`checkout_branch_worktree_inner`]. Nothing
/// is fetched: the user's agent owns network git.
fn agent_branch_worktree_inner(
    repo: &Path,
    branch_name: &str,
    worktree_path: &str,
    create: bool,
) -> Result<FetchAndCreateResult, String> {
    if !repo.exists() {
        return Err(format!("repo path does not exist: {}", repo.display()));
    }
    reject_unsafe_git_arg("branch name", branch_name)?;
    if create && run_git(&["check-ref-format", "--branch", branch_name], repo).is_err() {
        return Err(format!(
            "unavailable: `{branch_name}` is not a valid branch name."
        ));
    }
    let exists = |reference: String| run_git(&["rev-parse", "--verify", &reference], repo).is_ok();
    let branch_exists = exists(format!("refs/heads/{branch_name}"))
        || exists(format!("refs/remotes/origin/{branch_name}"));
    if create && branch_exists {
        return Err(format!(
            "unavailable: Branch `{branch_name}` already exists — use Repo + branch to open it."
        ));
    }
    if !create && !branch_exists {
        return Err(format!(
            "unavailable: No branch `{branch_name}` in this repo — choose New worktree to create it, or fetch it with your agent / git first."
        ));
    }
    let porcelain = run_git(&["worktree", "list", "--porcelain"], repo)?;
    match parse_worktree_porcelain(&porcelain)
        .into_iter()
        .find(|e| e.branch.as_deref() == Some(branch_name))
    {
        Some(holder) if !Path::new(&holder.path).exists() => {
            return Err(format!(
                "unavailable: the worktree holding `{branch_name}` at {} is missing — run `git worktree prune` in {}, then retry",
                holder.path,
                repo.display()
            ));
        }
        Some(holder) if !paths_equal(&holder.path, worktree_path) => {
            return Err(format!(
                "unavailable: Branch `{branch_name}` is already checked out at {} — choose Existing worktree to work there.",
                holder.path
            ));
        }
        Some(_) => {}
        None if Path::new(worktree_path).exists() => {
            return Err(format!(
                "unavailable: a stale directory already exists at the branch worktree path {worktree_path} — remove it, then retry"
            ));
        }
        None => {}
    }
    // An existing branch is adopted or attached as it stands: it is its own start point.
    // A full ref, so a tag that shares the default branch's name can't win.
    let start_point = if create {
        match local_default_start_point(repo).as_str() {
            "HEAD" => "HEAD".to_string(),
            name => match name.strip_prefix("origin/") {
                Some(remote) => format!("refs/remotes/origin/{remote}"),
                None => format!("refs/heads/{name}"),
            },
        }
    } else {
        branch_name.to_string()
    };
    let opened =
        checkout_branch_worktree_inner(repo, branch_name, worktree_path, Some(&start_point), None)?;
    // Started from a remote-tracking ref, git makes the new branch track it; a new agent
    // branch must not push to the default branch.
    if create && start_point.starts_with("refs/remotes/") {
        run_git(&["branch", "--unset-upstream", branch_name], repo)?;
    }
    Ok(opened)
}

/// Open an existing branch (`create == false`) or create a new one from the local
/// default branch (`create == true`) in a managed worktree for an agent launch.
pub async fn checkout_branch_worktree_for_agent(
    repo_path: String,
    branch_name: String,
    worktree_path: String,
    create: bool,
) -> Result<String, String> {
    crate::commands::await_blocking("checkout_branch_worktree_for_agent", move || {
        agent_branch_worktree_inner(
            &PathBuf::from(&repo_path),
            &branch_name,
            &worktree_path,
            create,
        )
        .map(|r| r.worktree_path)
    })
    .await
}

/// Open a previously prepared GitHub pull ref under `refs/remotes/origin/codesense-pr/N`.
/// Missing refs report the exact origin refspec; no acquisition or freshness check runs.
pub async fn checkout_pull_request_worktree_for_review(
    repo_path: String,
    pull_number: u64,
    worktree_path: String,
) -> Result<String, String> {
    crate::commands::await_blocking("checkout_pull_request_worktree_for_review", move || {
        let repo = PathBuf::from(&repo_path);
        let local_ref = format!("codesense-pr/{pull_number}");
        let fetch_ref = format!("refs/pull/{pull_number}/head:refs/remotes/origin/{local_ref}");
        checkout_branch_worktree_inner(&repo, &local_ref, &worktree_path, None, Some(&fetch_ref))
            .map(|result| result.worktree_path)
            .map_err(|error| {
                format!(
                    "unavailable: GitHub pull request {pull_number} could not be opened: {error}"
                )
            })
    })
    .await
}

/// SW-WB-1: switch an EXISTING worktree **in place** onto `branch_name`, carrying
/// any uncommitted changes across. This is the *migrate-in-place* reconcile path
/// — used when the worktree is dirty and we must preserve WIP + the running
/// terminal session (the clean case uses recreate, which mints a fresh worktree
/// at the new branch-derived path and prunes the old one).
///
/// Unlike [`checkout_branch_worktree_inner`], which only ever *adds* worktrees,
/// this moves an existing worktree's `HEAD` via `git switch`. NEVER-CLOBBER: git
/// itself refuses to overwrite local modifications on a switch (surfaced as a
/// typed error → the caller can offer recreate), and a branch already checked
/// out in another worktree is rejected rather than force-moved. Branch
/// resolution uses only existing local refs:
///   1. Already on `branch_name` → no-op success (idempotent).
///   2. Local `refs/heads/{branch}` exists → `git switch` onto it (carry WIP).
///   3. Else remote `origin/{branch}` exists → create a tracking branch from the
///      remote tip and switch onto it (carry WIP).
///   4. Else → create `branch_name` at the worktree's current `HEAD`, carrying
///      WIP — the "I did work under the wrong name, rename it" case.
///
/// We deliberately do NOT fast-forward an existing local branch to its remote
/// here: migrating preserves the worktree's local state; refreshing remote refs is the external agent or Git CLI's job.
fn switch_worktree_branch_inner(
    repo: &Path,
    worktree_path: &str,
    branch_name: &str,
) -> Result<FetchAndCreateResult, String> {
    if !repo.exists() {
        return Err(format!("repo path does not exist: {}", repo.display()));
    }
    // Reject inputs with path-traversal or flag-injection sequences (parity with
    // `checkout_branch_worktree_inner`).
    reject_unsafe_git_arg("branch name", branch_name)?;
    reject_unsafe_git_arg("worktree path", worktree_path)?;
    let worktree = Path::new(worktree_path);
    if !worktree.exists() {
        return Err(format!(
            "unavailable: worktree path does not exist: {worktree_path} — nothing to migrate in place (use recreate)"
        ));
    }
    // Idempotent: already on the target branch → nothing to do.
    if current_branch_of_worktree(worktree).as_deref() == Some(branch_name) {
        return Ok(FetchAndCreateResult {
            worktree_path: worktree_path.to_string(),
            from_remote: false,
        });
    }

    let remote_exists = run_git(
        &[
            "rev-parse",
            "--verify",
            &format!("refs/remotes/origin/{}", branch_name),
        ],
        repo,
    )
    .is_ok();
    let local_branch_exists = Command::new(git_bin())
        .args([
            "rev-parse",
            "--verify",
            &format!("refs/heads/{}", branch_name),
        ])
        .current_dir(repo)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    // If the target branch is already checked out in ANOTHER worktree, git would
    // refuse the switch anyway — surface it typed and point at recreate.
    let porcelain = run_git(&["worktree", "list", "--porcelain"], repo)?;
    if let Some(entry) = parse_worktree_porcelain(&porcelain)
        .into_iter()
        .find(|e| e.branch.as_deref() == Some(branch_name))
    {
        if !paths_equal(&entry.path, worktree_path) {
            return Err(format!(
                "unavailable: branch '{branch_name}' is already checked out at {} — close it, or use recreate",
                entry.path
            ));
        }
    }

    // `git switch` carries uncommitted changes when there's no conflict and
    // REFUSES (typed error) rather than overwrite them — that IS never-clobber.
    let remote_start = format!("origin/{}", branch_name);
    let switch_args: Vec<&str> = if local_branch_exists {
        vec!["switch", branch_name]
    } else if remote_exists {
        // New local tracking branch from the remote tip (git auto-tracks a
        // remote-tracking start point), switching onto it in place.
        vec!["switch", "-c", branch_name, &remote_start]
    } else {
        // No local, no remote — start the branch at the worktree's current HEAD,
        // carrying WIP (the rename-my-work case).
        vec!["switch", "-c", branch_name]
    };

    run_git(&switch_args, worktree).map_err(|e| {
        format!(
            "unavailable: can't switch the worktree onto '{branch_name}' — the switch was refused (most often uncommitted changes that would be overwritten; commit or stash first), or the branch is unavailable. You can recreate the worktree instead ({e})"
        )
    })?;

    Ok(FetchAndCreateResult {
        worktree_path: worktree_path.to_string(),
        from_remote: remote_exists && !local_branch_exists,
    })
}

/// SW-WB-1: Tauri entry for the migrate-in-place reconcile — switch an existing
/// worktree onto `branch_name`, preserving WIP + session. See
/// [`switch_worktree_branch_inner`] for the never-clobber semantics.
#[tauri::command]
pub async fn switch_worktree_branch(
    repo_path: String,
    worktree_path: String,
    branch_name: String,
) -> Result<FetchAndCreateResult, String> {
    crate::commands::await_blocking("switch_worktree_branch", move || {
        let repo = PathBuf::from(&repo_path);
        switch_worktree_branch_inner(&repo, &worktree_path, &branch_name)
    })
    .await
}

#[tauri::command]
pub async fn remove_worktree(
    app: tauri::AppHandle,
    repo_path: String,
    worktree_path: String,
    force: Option<bool>,
    discard_context: Option<bool>,
) -> Result<(), String> {
    let worktree = PathBuf::from(&worktree_path);
    let consent = RemovalConsent::from_command_args(force, discard_context);
    let result = prune_worktree_locked(&app, &worktree, repo_path, worktree_path, consent).await;

    // RF-perf-6 (AC-2): now that the git prune has run and released this repo's
    // per-repo wave lock, evict its per-repo coordinator-map + claim-latch entries
    // so backend RSS tracks the live working set, not every worktree ever seen.
    // Done HERE (not inside `flush_and_drop_lockfile`) because
    // `prune_worktree_locked` re-acquires `repo_locks[worktree]` across the git
    // prune — an earlier eviction would just be re-inserted. Only on success: a
    // failed remove leaves the worktree live, so its coordinator state stays (it
    // re-latches cheaply on next use regardless — AC-3).
    if result.is_ok() {
        use tauri::Manager;
        if let Some(coord) = app
            .try_state::<std::sync::Arc<crate::content_engine::sync::WaveCoordinator>>()
            .map(|s| std::sync::Arc::clone(s.inner()))
        {
            coord.prune_repo(&worktree);
        }
    }
    result
}

/// SLW-RS-4 (review H3): run the git prune under the worktree's per-repo wave lock
/// — the SAME lock reverse/claim waves take — so `git worktree remove` cannot race
/// an in-flight periodic sweep wave writing into the tree (the invariant
/// [`teardown_repo`](crate::content_engine::sync::teardown_repo) already holds). The
/// flush already ran + dropped the lockfile before this; the lock fences the prune
/// against any INDEPENDENT sweep wave that started after the flush. The guard is
/// [`tokio::sync::OwnedMutexGuard`], held across the blocking prune. If the
/// coordinator is unavailable (never, in a running app) the prune runs unlocked.
/// SW-WM-1 (AC-4): the removal-path managed-target gate, pure + testable. Returns a
/// user-facing refusal (never a panic) when `worktree_path` is not a codesense-managed
/// target, so a force-remove can never delete a user's own checkout. Uses the raw (non-
/// canonicalized) predicate deliberately: the checked path and the registry roots both
/// come from the same source (`list_managed_worktrees`), so they compare in the same form.
fn ensure_managed_removal_target(
    worktree_path: &Path,
    codesense_home: &Path,
) -> Result<(), String> {
    if crate::install::worktree_index::is_codesense_managed_target(worktree_path, codesense_home) {
        return Ok(());
    }
    log::warn!(
        target: "commands::worktree",
        "refusing to remove non-managed worktree target {}",
        worktree_path.display()
    );
    Err(format!(
        "refusing to remove a path CodeSense doesn't manage: {}",
        worktree_path.display()
    ))
}

/// Ignored files are deliberately invisible to Git's ordinary dirty check.
/// Without a complete baseline for them, preserve them unless the user explicitly
/// chose force removal. This also covers untracked context from older releases.
fn ensure_context_removal_safe(_home: &Path, root: &Path, force: bool) -> Result<(), String> {
    if force {
        return Ok(());
    }
    // `--directory` keeps the inventory bounded on a repo with a built
    // `target/` tree (dozens of MB of per-file paths without it): git lists
    // the containing directory when every entry under it is ignored.
    let output = Command::new(git_bin())
        .args([
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
            "-z",
        ])
        .current_dir(root)
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("cannot verify ignored workspace context: {e}"))?;
    if !output.status.success() {
        return Err("workspace contains ignored files whose context cleanliness cannot be proved; review or publish the files, then explicitly force removal to discard them".into());
    }
    let has_user_ignored = output
        .stdout
        .split(|b| *b == 0)
        .any(|slice| !slice.is_empty());
    if has_user_ignored {
        return Err("workspace contains ignored files whose context cleanliness cannot be proved; review or publish the files, then explicitly force removal to discard them".into());
    }
    Ok(())
}

/// SW-XS-4: what a removal may discard, as two SEPARATE consents. Discarding
/// unpublished (ignored, managed) context files never implies discarding
/// uncommitted code: `discard_context` lifts only the context-removal check, while
/// `discard_code` alone passes `--force` to `git worktree remove`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RemovalConsent {
    pub discard_context: bool,
    pub discard_code: bool,
}

impl RemovalConsent {
    /// The consent the `remove_worktree` command's `force` / `discardContext`
    /// arguments carry (both default to no consent).
    pub(crate) fn from_command_args(force: Option<bool>, discard_context: Option<bool>) -> Self {
        Self {
            discard_code: force.unwrap_or(false),
            discard_context: discard_context.unwrap_or(false),
        }
    }
}

fn remove_registered_worktree(
    home: &Path,
    root: &Path,
    repo: &str,
    worktree: &str,
    consent: RemovalConsent,
) -> Result<(), String> {
    let identity = crate::content_engine::context_lifecycle::local_identity(root);
    let _lease = crate::content_engine::context_lifecycle::reserve_selected(home, &identity)?;
    ensure_managed_removal_target(root, home)?;
    ensure_context_removal_safe(home, root, consent.discard_context)?;
    // SV-CDE-82: keep the story branch's tip in the story's tips before the worktree (and,
    // after it, its record) goes.
    crate::install::story_tips::sight_worktree(home, root);
    // SW-XS-7: the worktree goes first (its prepared files stay excluded, so a
    // non-forced remove sees a clean tree); its exclude block and Step 2
    // managed record go only once it is gone.
    crate::agents::prepare::teardown_local_destination(home, root, || {
        remove_worktree_prune_core(repo, worktree, Some(consent.discard_code))
    })
}

pub(crate) async fn prune_worktree_locked(
    app: &tauri::AppHandle,
    code_repo_path: &Path,
    repo_path: String,
    worktree_path: String,
    consent: RemovalConsent,
) -> Result<(), String> {
    use tauri::Manager;
    let coord = app
        .try_state::<std::sync::Arc<crate::content_engine::sync::WaveCoordinator>>()
        .map(|s| std::sync::Arc::clone(s.inner()));
    let _lock = match &coord {
        Some(c) => Some(c.acquire_repo_lock(code_repo_path).await),
        None => None,
    };
    // Resolve the active context ONCE — the managed-target guard (below) and the
    // post-remove record prune both need it. `None` = no context loaded.
    let ctx = coord.as_ref().and_then(|c| c.active_ctx());
    let lifecycle_home = ctx
        .as_ref()
        .map(|ctx| ctx.codesense_home.clone())
        .or_else(crate::commands::paths::codesense_home_dir)
        .ok_or_else(|| "cannot verify workspace context before prune".to_string())?;
    let lifecycle_root = code_repo_path.to_path_buf();
    let cleanup_home = lifecycle_home.clone();
    // Recheck after the repo lock in the final blocking destructive receiver.
    // The thread-bound lease spans removal; no async thread can inherit it.
    let result = crate::commands::await_blocking("prune_worktree_locked", move || {
        remove_registered_worktree(
            &lifecycle_home,
            &lifecycle_root,
            &repo_path,
            &worktree_path,
            consent,
        )
    })
    .await;
    if result.is_ok() {
        crate::content_engine::sync::wave::reap_detached_worktree(&cleanup_home, code_repo_path);
        // SW-WA-F13: the trust key SW-WA-F13 wrote for this worktree is only
        // meaningful while the dir exists. Drop it best-effort — a failed
        // untrust leaves a stale `projects["<path>"]` entry Claude will ignore
        // (the path is gone), never a correctness problem.
        let reap_principal = ctx.as_ref().map(|c| c.principal());
        crate::agents::workspace_trust::untrust_managed_launch_dir(
            &cleanup_home,
            reap_principal.as_ref(),
            code_repo_path,
        );
        // SLW-WL-2: the tree is gone → prune its worktree-index record so
        // `list_managed_worktrees` (and the teardown poll) stop seeing a worktree
        // that no longer exists. Without this the MANUAL remove path left a stale
        // record that the panel's refetch resurfaced as a phantom "(detached)" row.
        // Done HERE (the single prune funnel for both the manual command and the auto
        // merge-teardown) and BEFORE the emit, so the panel's refetch sees it gone.
        // Best-effort; the launch reconcile (`prune_if_stale`) remains the backstop.
        if let Some(ref ctx) = ctx {
            if let Err(e) = crate::install::worktree_index::prune_record_for_path(
                &ctx.codesense_home,
                &ctx.principal(),
                code_repo_path,
            ) {
                log::warn!(
                    target: "commands::worktree",
                    "prune index record after removing {} failed: {}",
                    code_repo_path.display(), e
                );
            }
        }
        // SLW-WL-2 (OQ-3): a worktree just left the managed set — nudge any open
        // Worktrees panel to refetch, no polling timer. The explicit
        // `remove_worktree` command is the only caller.
        emit_worktree_changed(app);
    }
    result
}

/// SLW-WL-2: the tri-state core of the worktree dirty check. Runs `git status
/// --porcelain` (materialized context files are `.git/info/exclude`-hidden, so this
/// sees only genuine tracked/untracked developer work) and returns
/// `Some(true/false)` on a clean read, or `None` when git can't be spawned or exits
/// non-zero — an honest "unknown". The two consumers want opposite postures on that
/// unknown, so the fold lives at each call site: the Worktrees panel keeps `None`
/// (renders as neither dirty nor a false "clean"); the auto-teardown gate folds
/// `None → true` via [`worktree_git_dirty`] (never auto-destroy on an ambiguous read).
pub(crate) fn worktree_git_dirty_opt(worktree: &Path) -> Option<bool> {
    let out = Command::new(git_bin())
        .args(["status", "--porcelain"])
        .current_dir(worktree)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(!String::from_utf8_lossy(&out.stdout).trim().is_empty())
}

/// SLW-RS-4 (review M6/L9): the worktree's currently checked-out branch
/// (`git rev-parse --abbrev-ref HEAD`), or `None` on a detached HEAD / git error.
pub(crate) fn current_branch_of_worktree(worktree: &Path) -> Option<String> {
    let out = Command::new(git_bin())
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(worktree)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let branch = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if branch.is_empty() || branch == "HEAD" {
        return None; // detached HEAD
    }
    Some(branch)
}

/// Verify a managed worktree is checked out on the expected branch — the pure gate the
/// run-launch spine applies before snapshotting a run's work product. Returns `Err` when
/// the worktree is on a different branch or has no branch at all (detached HEAD), so the
/// caller degrades to "no work product" instead of attributing a run to the wrong branch.
/// Blocking (shells out via [`current_branch_of_worktree`]) — call inside `await_blocking`.
pub(crate) fn verify_worktree_branch(worktree: &Path, expected_branch: &str) -> Result<(), String> {
    let observed = current_branch_of_worktree(worktree)
        .ok_or_else(|| "worktree has no current branch".to_string())?;
    if observed != expected_branch {
        return Err(format!(
            "worktree branch {observed:?} does not match expected branch {expected_branch:?}"
        ));
    }
    Ok(())
}

/// SLW-RS-4: derive the **main** repository of a linked worktree — the working
/// tree `git worktree remove <worktree>` must be run from. `git rev-parse
/// --git-common-dir` from inside a linked worktree resolves to the main repo's
/// `.git`; its parent is the main working tree. `--path-format=absolute` makes the
/// result independent of the process CWD. `None` if `worktree` isn't a git
/// worktree or git is unavailable — the caller skips the auto-teardown (the manual
/// path, which is handed an explicit `repo_path`, is unaffected).
pub(crate) fn main_repo_of_worktree(worktree: &Path) -> Option<PathBuf> {
    let out = Command::new(git_bin())
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(worktree)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let common_dir = PathBuf::from(String::from_utf8(out.stdout).ok()?.trim());
    // `<main>/.git` → `<main>`. A bare/edge layout without a parent is skipped.
    common_dir.parent().map(Path::to_path_buf)
}

/// SLW-RS-4: the blocking `git worktree remove` prune, split out of
/// [`remove_worktree`] so the flush (which needs an `AppHandle`) and the pure git
/// prune are separately testable. Runs on the blocking pool via `await_blocking`.
#[cfg(test)]
pub(crate) async fn remove_worktree_prune(
    repo_path: String,
    worktree_path: String,
    force: Option<bool>,
) -> Result<(), String> {
    crate::commands::await_blocking("remove_worktree", move || {
        remove_worktree_prune_core(&repo_path, &worktree_path, force)
    })
    .await
}

fn remove_worktree_prune_core(
    repo_path: &str,
    worktree_path: &str,
    force: Option<bool>,
) -> Result<(), String> {
    let repo = PathBuf::from(repo_path);
    if !repo.exists() {
        return Err(format!("repo path does not exist: {repo_path}"));
    }
    if force.unwrap_or(false) {
        run_git(&["worktree", "remove", worktree_path, "--force"], &repo)?;
    } else {
        run_git(&["worktree", "remove", worktree_path], &repo)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeDirtyStatus {
    pub dirty: bool,
}

#[tauri::command]
pub async fn check_worktree_dirty(worktree_path: String) -> Result<WorktreeDirtyStatus, String> {
    crate::commands::await_blocking("check_worktree_dirty", move || {
        let wt_path = PathBuf::from(&worktree_path);
        if !wt_path.is_absolute() {
            return Err("worktree path must be absolute".to_string());
        }
        if !wt_path.is_dir() {
            return Err(format!("worktree path does not exist: {}", worktree_path));
        }
        let output = Command::new(git_bin())
            .args(["status", "--porcelain"])
            .current_dir(&wt_path)
            .output()
            .map_err(|e| format!("failed to spawn git: {}", e))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("git status failed: {}", stderr.trim()));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(WorktreeDirtyStatus {
            dirty: !stdout.trim().is_empty(),
        })
    })
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub path: String,
    pub branch: Option<String>,
    pub head_sha: String,
}

#[tauri::command]
pub async fn list_worktrees(repo_path: String) -> Result<Vec<WorktreeInfo>, String> {
    crate::commands::await_blocking("list_worktrees", move || {
        let repo = PathBuf::from(&repo_path);
        if !repo.exists() {
            return Err(format!("repo path does not exist: {}", repo_path));
        }
        let porcelain = run_git(&["worktree", "list", "--porcelain"], &repo)?;
        Ok(parse_worktree_porcelain(&porcelain))
    })
    .await
}

/// SW-WU-10: git-first worktree **existence** enumerator — layer 1 of the unified
/// discovery pipeline (`DESIGN-unified-worktree-discovery.md` §2.1/§2.2). Asks git
/// directly (`git worktree list --porcelain`) across every distinct clone reachable
/// from the active team's attached repo set, so the eventual Worktrees pane can show
/// worktrees regardless of who created them — an external `git worktree add`, a CLI
/// agent, or epic assembly — not only the index-recorded ones `list_managed_worktrees`
/// sees today.
///
/// `slugs` come from [`crate::content_engine::sync::WaveCoordinator::active_target_repos`].
/// The attached set is *worktree* paths ("main + N worktrees of the same slug"), so the
/// work of turning that into discovered worktrees lives in [`discover_worktrees_in`];
/// this wrapper only resolves the attached paths (via the already-tested
/// [`crate::content_engine::lockfile::enumerate_attached_code_repos_for_team`]) and hands
/// them off. An enumeration error yields an empty set (fail-soft) rather than surfacing.
///
/// A plain SYNC fn: meant to run INSIDE the caller's `await_blocking` closure (SW-WU-11
/// folds it into [`list_managed_worktrees`]) so every git subprocess stays off the async
/// worker and can't jank the UI.
pub(crate) fn discover_worktrees(
    ctx: &crate::content_engine::ResolverContext,
    slugs: &[SourceRepoSlug],
) -> Vec<WorktreeInfo> {
    discover_worktrees_in(&attached_code_repo_paths(ctx, slugs), &ctx.codesense_home)
}

/// The active team's attached `code_repo_path`s ("main + N worktrees of the same slug"),
/// read once from the lockfile. Shared by [`discover_worktrees`] (the scan) and
/// [`resolve_managed_clone_dirs`] (the SW-WU-14 watch) so both draw the SAME repo set;
/// a read failure fails soft to empty (logged), never a partial set.
fn attached_code_repo_paths(
    ctx: &crate::content_engine::ResolverContext,
    slugs: &[SourceRepoSlug],
) -> Vec<PathBuf> {
    match crate::content_engine::lockfile::enumerate_attached_code_repos_for_team(ctx, slugs) {
        Ok(v) => v.into_iter().map(|r| r.code_repo_path).collect(),
        Err(e) => {
            log::warn!(
                target: "commands::worktree",
                "attached_code_repo_paths: enumerate_attached_code_repos_for_team failed: {e}"
            );
            Vec::new()
        }
    }
}

/// The distinct clone git-dirs behind a set of attached worktree paths: each path's owning
/// clone via [`main_repo_of_worktree`], deduped by canonical identity (symlink-safe, so a
/// `/private`-prefixed or symlinked duplicate isn't returned twice), with an unresolvable
/// path skipped fail-soft. Factored out of [`discover_worktrees_in`] so the SW-WU-14 FS
/// watcher can attach to EXACTLY the clones the scan enumerates (watch-set ≡ scan-set) —
/// a clone can never be scanned-but-unwatched nor watched-but-unscanned.
pub(crate) fn distinct_clone_dirs(attached_worktree_paths: &[PathBuf]) -> Vec<PathBuf> {
    use crate::content_engine::sync::WaveCoordinator;

    let mut clones: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for wt_path in attached_worktree_paths {
        let Some(clone) = main_repo_of_worktree(wt_path) else {
            continue; // AC-4: unresolvable clone — skip fail-soft.
        };
        if seen.insert(WaveCoordinator::normalize_target_path(&clone)) {
            clones.push(clone);
        }
    }
    clones
}

/// The deduped managed-clone git-dirs for a team's attached repos — the SAME set
/// [`discover_worktrees`] enumerates (attached code repos → [`distinct_clone_dirs`]),
/// exposed so the SW-WU-14 FS watcher attaches its `.git/worktrees/` watch to exactly the
/// clones the scan reads. Pass the epic-expanded slug set (as `set_active_team_for_wave`
/// does) so epic-integration clones are covered by the same one watch.
pub(crate) fn resolve_managed_clone_dirs(
    ctx: &crate::content_engine::ResolverContext,
    slugs: &[SourceRepoSlug],
) -> Vec<PathBuf> {
    distinct_clone_dirs(&attached_code_repo_paths(ctx, slugs))
}

/// The pure core of [`discover_worktrees`], split out so the git-enumeration logic
/// (dedup → union → main-exclusion → managed gate → fail-soft) is unit-testable against
/// real on-disk clones without a lockfile/attachment fixture. `attached_worktree_paths`
/// are the attached `code_repo_path`s ("main + N worktrees of the same slug").
///
/// 1. Dedup the attached paths to their distinct clone git-dirs via
///    [`main_repo_of_worktree`] — normalized before the dedup insert so a symlinked /
///    `/private`-prefixed duplicate isn't enumerated twice; a path whose clone can't be
///    resolved is skipped fail-soft (AC-4).
/// 2. `git worktree list --porcelain` each clone (union), dropping the clone's own main
///    working copy — it is the clone, not a worktree (AC-2, symlink-safe compare). One
///    clone whose git errors is skipped+logged; the others still return (AC-4).
/// 3. Keep only codesense-managed surfaces (AC-3). The gate is the SAME boundary as
///    [`crate::install::worktree_index::is_codesense_managed_target`] — the `users/` tree
///    OR a registered worktree root — but the roots are resolved ONCE for the whole call
///    (that predicate's own contract: gate once per wave, never per file) and normalized to
///    the same canonical identity as the discovered paths, so a symlinked `codesense_home`
///    can't false-miss. Gating the discovered *worktrees* (not the clone-main) is
///    deliberate: a raw-attached repo's primary checkout is itself unmanaged, yet we must
///    still enumerate FROM it to reach its managed `../{repo}-wt/{slug}` worktrees.
fn discover_worktrees_in(
    attached_worktree_paths: &[PathBuf],
    codesense_home: &Path,
) -> Vec<WorktreeInfo> {
    use crate::content_engine::sync::WaveCoordinator;

    // (1) Distinct clone git-dirs (symlink-safe dedup, unresolvable skipped) — the shared
    // resolver the SW-WU-14 watcher also uses, so watch-set ≡ scan-set.
    let clones = distinct_clone_dirs(attached_worktree_paths);

    // (3) Managed-surface roots, resolved ONCE (the `is_codesense_managed_target` boundary
    // in batch form), each normalized to the SAME canonical identity the discovered paths
    // and clone-main use below. Git emits symlink-resolved porcelain paths, so matching them
    // against a raw (possibly symlinked, e.g. macOS `/var`→`/private/var`) `codesense_home`
    // would false-miss and silently blank the set — canonicalize both sides.
    let managed_roots: Vec<PathBuf> =
        crate::install::worktree_index::managed_surface_roots(codesense_home)
            .iter()
            .map(|r| WaveCoordinator::normalize_target_path(r))
            .collect();

    // (2) Enumerate + filter, deduping the union by canonical path so a worktree that
    // surfaces under two clones is emitted once.
    let mut out: Vec<WorktreeInfo> = Vec::new();
    let mut emitted: HashSet<PathBuf> = HashSet::new();
    for clone in &clones {
        let porcelain = match run_git(&["worktree", "list", "--porcelain"], clone) {
            Ok(s) => s,
            Err(e) => {
                log::warn!(
                    target: "commands::worktree",
                    "discover_worktrees: skipping clone {} — git worktree list failed: {e}",
                    clone.display()
                );
                continue; // AC-4: one bad clone never blanks the set.
            }
        };
        // The main working copy path. For a standard/managed clone (`.git` dir inside the
        // working tree) this equals `main_repo_of_worktree`'s common-dir parent, so it
        // matches the porcelain main entry below. A separated git dir
        // (`git init --separate-git-dir`, `core.worktree`, submodule gitlink) would break
        // that equality — out of scope here: managed clones are plain `git clone`s, and any
        // odd raw-attached layout resolves to a non-working-tree parent whose
        // `git worktree list` fails the fail-soft skip above. WU-13's unified row shape
        // revisits main identity.
        let clone_main = WaveCoordinator::normalize_target_path(clone);
        for wt in parse_worktree_porcelain(&porcelain) {
            let wt_norm = WaveCoordinator::normalize_target_path(&PathBuf::from(&wt.path));
            // AC-2: drop the clone's own main working copy (symlink-safe compare).
            if wt_norm == clone_main {
                continue;
            }
            // AC-3: keep only codesense-managed surfaces (canonical identity on both sides).
            if !codesense_fs_core::gate::path_within_any_root(&wt_norm, &managed_roots) {
                continue;
            }
            if emitted.insert(wt_norm) {
                out.push(wt);
            }
        }
    }
    out
}

/// SW-WU-11 (ADOPT): the association layer's write half. For each `discovered` worktree
/// the index has never seen, if its branch resolves to a **unique, live** story, backfill
/// a record (via the one writer, `backfill_from_branch`) and return it so the caller can
/// append it to the survivor set — it then renders as a Story row through the SAME
/// `enrich_rows`/`managed_row_from_record` path native records use. No new row shape.
///
/// Fail-safe skips (no write, AC-2) — a discovered path already in `existing` (record
/// wins, deduped by canonical path), a **gone** worktree dir (a `prunable` entry git
/// still lists after the working copy was removed out-of-band), a detached HEAD, a branch
/// matching no/2+ stories (`single_by_story_id` arity gate), a matched story whose file is
/// gone, or a record another writer has materialized since the snapshot (including an
/// **unreadable/forward-schema** record we must never overwrite). The
/// `entry.path.exists()` liveness gate is the CALLER's policy — it *reduces* adopt→orphan
/// churn on a briefly-stale snapshot (DESIGN §6 #3); a delete racing the write is caught
/// by the later reap pass, and `backfill_from_branch` itself stays the pure branch→record
/// writer. A no-match discovered path is not adopted here — [`list_managed_worktrees`]
/// renders it as a `kind=Orphan` [`WorktreeRow`] (SW-WU-13).
fn adopt_discovered_externals(
    discovered: &[WorktreeInfo],
    existing: &[crate::install::worktree_index::WorktreeStoryRecord],
    cache: &crate::install::frontmatter_cache::FrontmatterCache,
    codesense_home: &Path,
    principal: &Principal,
    org_slug: &str,
) -> Vec<crate::install::worktree_index::WorktreeStoryRecord> {
    use crate::content_engine::sync::WaveCoordinator;
    use crate::install::worktree_index::{backfill_from_branch, read_record, story_id_from_branch};

    let known: HashSet<PathBuf> = existing
        .iter()
        .map(|r| WaveCoordinator::normalize_target_path(&r.code_repo_path))
        .collect();
    let mut adopted = Vec::new();
    for wt in discovered {
        let path = PathBuf::from(&wt.path);
        if known.contains(&WaveCoordinator::normalize_target_path(&path)) {
            continue; // AC-3: an already-recorded path keeps its record row.
        }
        // Only adopt a worktree whose working dir is present. `git worktree list` still
        // emits an entry for a directory removed out-of-band (a `prunable` worktree the
        // user rm'd without `git worktree prune`); adopting it would write a record that
        // the next fetch's `reap_confirmed_gone_records` deletes — a resurrection loop
        // plus a misleading row for a vanished worktree. `is_dir` (not `exists`) also
        // rejects a stray non-directory path — a worktree is always a directory.
        if !path.is_dir() {
            continue;
        }
        let Some(branch) = wt.branch.as_deref() else {
            continue; // Detached HEAD — nothing to resolve a story from.
        };
        // AC-5 liveness gate. `backfill_from_branch` re-derives the id + entry as the
        // self-contained single writer; we resolve here only to read `entry.path` and
        // confirm the story file is still on disk before any write. The arity gate
        // (`single_by_story_id` → Err on 0/2+) collapses no-match and ambiguous to a skip.
        let Some(story_id) = story_id_from_branch(branch) else {
            continue;
        };
        let Ok(entry) = cache.single_by_story_id(&story_id) else {
            continue;
        };
        if !entry.path.exists() {
            continue;
        }
        // Close the window between the pre-reap `known` snapshot and this write: another
        // in-flight fetch or the sync wave may have materialized a record for this path
        // since. Skipping a now-present record avoids clobbering a concurrently-written
        // record's sync state (last-writer-wins) and redundant re-writes.
        match read_record(codesense_home, principal, &path) {
            Ok(None) => {}           // No record yet — safe to adopt.
            Ok(Some(_)) => continue, // Materialized since the snapshot — don't re-write.
            // Unreadable record: a corrupt file OR one written by a NEWER build whose
            // schema this build can't parse (`UnsupportedSchemaVersion`). Never blindly
            // overwrite it — a v1 backfill would downgrade-clobber a forward-schema
            // record. Leave it; a later fetch re-derives once the record is legible.
            Err(_) => continue, // The index reader already diagnosed this file revision.
        }
        match backfill_from_branch(codesense_home, principal, &path, org_slug, branch, cache) {
            Ok(Some(record)) => adopted.push(record),
            Ok(None) => {} // e.g. the story frontmatter carries no `source_repo` slug.
            Err(e) => log::warn!(
                target: "commands::worktree",
                "adopt: backfill_from_branch failed for {}: {e}", path.display()
            ),
        }
    }
    adopted
}

/// SLW-WL-2 (AC-2): one codesense-managed worktree as the right-pane management
/// panel sees it — the enriched join of a [`WorktreeStoryRecord`] (story link + sync
/// intent) with per-worktree git facts (branch, dirty, parent repo).
///
/// This is a **durable, SDK-shaped** contract: a forthcoming epic rebuilds the
/// right-pane surfaces as dashboards that consume it via `codesense.invoke`, so its
/// fields are **camelCase** (deliberately unlike the raw snake_case
/// [`WorktreeInfo`], which is the git-porcelain list used elsewhere — untouched).
/// `branch`/`dirty`/`repoPath` are `null` when the worktree can't be read; the row
/// is still emitted, never dropped (a worktree you can't stat is exactly one you
/// want to SEE and manage).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ManagedWorktreeRow {
    /// Absolute path of the worktree on disk (the record's `code_repo_path`).
    pub path: String,
    /// Currently checked-out branch, or `null` on detached HEAD / git error.
    pub branch: Option<String>,
    /// The story id this worktree is working (frontmatter id, e.g. `SW-TS-1`).
    pub story_id: String,
    /// `owner/repo` slug of the source repo.
    pub source_repo: String,
    /// Index-side sync intent — serializes kebab-case (`"syncing"`, `"synced"`,
    /// `"failed"`, `"pending"`) per [`SyncStatus`].
    pub sync_status: crate::install::worktree_index::SyncStatus,
    /// Plain-language reason the last sync failed (set with `syncStatus == failed`).
    pub failure_reason: Option<String>,
    /// Uncommitted-changes flag; `null` when the dirty check couldn't run.
    pub dirty: Option<bool>,
    /// Parent working copy `git worktree remove` must run from (derived via
    /// [`main_repo_of_worktree`]). `null` ⇒ vanished/detached worktree — the FE
    /// disables Remove for that row so it never calls remove without a repo path.
    pub repo_path: Option<String>,
    /// Absolute path to the story file this worktree works — the `storyFilePath`
    /// the FE hands `openOrFocusStoryWorkspace` to open it as a live workspace.
    pub story_source_path: String,
    /// Org slug the worktree belongs to. With `team`, the FE builds the workspace
    /// `teamKey` (`buildTeamKey(orgSlug, team)`) for the open action.
    pub org_slug: String,
    /// Team the worktree's story resolves to (the `_team/<team>/` segment), or
    /// `null` for a neutral story. Display/keying only — never a sync input.
    pub team: Option<String>,
    /// SLW-WL-5 (AC-2): does this worktree currently have a live CLI session? The
    /// WL-4 reverse-sync liveness signal, surfaced so the panel can show the
    /// *effective* reverse-sync state (`Auto → syncing` vs `Auto → dormant`) beside
    /// the per-row override control. Derived from the same
    /// [`live_worktree_roots`](crate::commands::worktree_liveness::live_worktree_roots) helper the
    /// reverse filter uses (single source of truth). Best-effort: a momentarily-
    /// contended session lock reads every worktree dormant this fetch (cosmetic; the
    /// panel refetches) — it never blocks.
    pub live_cli_session: bool,
    /// SW-WM-3 (AC-1/AC-4): is this worktree's git/registry linkage broken? `true` when
    /// its parent working copy has vanished (`repo_path == None` — git worktree link or
    /// parent repo gone) OR its backing story file no longer exists (`story_source_path`
    /// deleted — registry points at a dead story). Cheap, local, non-network signals
    /// only (keeps enrichment non-blocking). Drives the "Orphaned" state + one-click
    /// Reconcile.
    pub orphaned: bool,
    /// RF-perf-24 (AC-5): unix-ms of the last reverse-sync that promoted ≥1 dest edit
    /// back into the content layer, or `null` before this worktree ever reverse-synced.
    /// Read straight off the record — drives the row's "Synced by … · <ago>" sub-line.
    pub last_reverse_sync_at: Option<u64>,
    /// RF-perf-24 (AC-5): who triggered [`Self::last_reverse_sync_at`] — `auto` / `user`
    /// / `agent` — or `null` before the first reverse-sync.
    pub last_reverse_sync_by: Option<crate::install::worktree_index::SyncInitiator>,
    /// SV-CDE-7: whether line-attribution capture was working at the last agent launch
    /// here (`active`, `off`, `not_installed`, ...); `null` before any launch.
    pub attribution: Option<crate::attribution::AttributionState>,
}

/// SLW-WL-2: enrich one index record into a [`ManagedWorktreeRow`] — the record's
/// stored fields verbatim, plus the three git-derived facts. Each git read degrades
/// independently to `None` (the row is retained regardless). Pure + testable
/// (shells out to git in the worktree dir); the command below maps records through
/// it on the blocking pool. `live_cli_session` (SLW-WL-5) is supplied by the caller,
/// which resolves it once against the live-session set (kept out of here so this
/// stays a pure per-record git enrichment).
pub(crate) fn managed_row_from_record(
    r: crate::install::worktree_index::WorktreeStoryRecord,
    live_cli_session: bool,
) -> ManagedWorktreeRow {
    let wt = r.code_repo_path.as_path();
    let branch = current_branch_of_worktree(wt);
    let dirty = worktree_git_dirty_opt(wt);
    let repo_path = main_repo_of_worktree(wt).map(|p| p.to_string_lossy().into_owned());
    // SW-WM-3 (AC-1/AC-4): orphaned = parent working copy gone (git-link/parent broken →
    // `repo_path` unresolved) OR backing story file gone (registry points at a dead
    // story). Local, non-network signals only — one `exists()` stat on top of the
    // already-computed `repo_path`. Independent of merge state: a merged worktree whose
    // story file was deleted is Orphaned (ranked above Merged), while a healthy post-merge
    // worktree with both parent + story intact is not.
    let orphaned = repo_path.is_none() || !r.story_source_path.exists();
    ManagedWorktreeRow {
        path: r.code_repo_path.to_string_lossy().into_owned(),
        branch,
        source_repo: r.source_repo.to_string(),
        story_id: r.story_id,
        sync_status: r.status,
        failure_reason: r.failure_reason,
        dirty,
        repo_path,
        story_source_path: r.story_source_path.to_string_lossy().into_owned(),
        org_slug: r.org_slug,
        team: r.intended_team,
        live_cli_session,
        orphaned,
        last_reverse_sync_at: r.last_reverse_sync_at,
        last_reverse_sync_by: r.last_reverse_sync_by,
        attribution: r.attribution,
    }
}

/// SW-WU-13: which of the three unified worktree kinds a [`WorktreeRow`] is — the
/// discriminant that lets one row type span story / epic-integration / orphan
/// worktrees (DESIGN §2.4). Serialized camelCase (`"story"`, `"epicIntegration"`,
/// `"orphan"`) so the FE can partition + group on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WorktreeKind {
    Context,
    Story,
    EpicIntegration,
    Orphan,
}

/// SW-WU-13: the unified Worktrees-panel row — ONE shape spanning all three
/// [`WorktreeKind`]s so the panel renders story, epic-integration and orphan
/// worktrees from a single `Vec` (DESIGN §2.4). Superset of the old
/// `ManagedWorktreeRow` (story) + `EpicWorktreeRow` (epic, now deleted): the common
/// git facts (`path`/`branch`/`dirty`/`repo_path`/`live_cli_session`) are always
/// present; story-only and epic-only fields are `Some` only for their kind.
/// `workspace_key` is the grouping key the FE folds on — `story_id` (story = a group
/// of one), `epic_id` (epic = a group of N repos), or the parent-clone path (orphan)
/// — realizing "a workspace owns 1..N worktrees" as data, not inference. camelCase
/// like the rest of the worktree API.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeRow {
    pub kind: WorktreeKind,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextWorktreeFields>,
    pub workspace_key: String,
    pub path: String,
    pub branch: Option<String>,
    pub dirty: Option<bool>,
    /// Parent clone (`git worktree remove` runs from here); `null` ⇒ vanished/detached.
    pub repo_path: Option<String>,
    pub live_cli_session: bool,
    // --- Story-only (None for epic + orphan) ---
    pub story_id: Option<String>,
    pub source_repo: Option<String>,
    pub sync_status: Option<crate::install::worktree_index::SyncStatus>,
    pub failure_reason: Option<String>,
    pub story_source_path: Option<String>,
    pub org_slug: Option<String>,
    pub team: Option<String>,
    pub orphaned: Option<bool>,
    /// RF-perf-24 (AC-5): last reverse-sync provenance (story rows only) — unix-ms of
    /// the last dest→content-layer promote and who triggered it (`auto`/`user`/`agent`).
    pub last_reverse_sync_at: Option<u64>,
    pub last_reverse_sync_by: Option<crate::install::worktree_index::SyncInitiator>,
    /// SV-CDE-7: whether line-attribution capture was working at the last agent launch
    /// here (`active`, `off`, `not_installed`, ...); `null` before any launch.
    pub attribution: Option<crate::attribution::AttributionState>,
    // --- Epic-only (None for story + orphan) ---
    pub epic_id: Option<String>,
    pub owner: Option<String>,
    pub repo: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextWorktreeFields {
    pub id: String,
    pub name: String,
    pub launch_cwd: String,
    pub base_sha: String,
    pub created_at: u64,
    pub last_used_at: u64,
    pub agents: Vec<ContextWorktreeAgent>,
}
#[derive(Debug, Clone, serde::Serialize)]
pub struct ContextWorktreeAgent {
    pub handle: String,
    pub name: String,
    pub role: Option<String>,
}

impl WorktreeRow {
    /// Story arm: wrap an enriched [`ManagedWorktreeRow`] (built by the REUSED
    /// [`managed_row_from_record`]) as a unified row — no story enrichment logic is
    /// duplicated. `workspace_key` = the story id, or the worktree path when the record
    /// is neutral (empty `story_id`, i.e. an unlinkable worktree) so a group of storyless
    /// rows stays one-per-worktree instead of collapsing into a single `""` bucket
    /// (mirrors the orphan arm's clone-unresolved fallback).
    pub(crate) fn story(r: ManagedWorktreeRow) -> Self {
        let workspace_key = if r.story_id.is_empty() {
            r.path.clone()
        } else {
            r.story_id.clone()
        };
        WorktreeRow {
            kind: WorktreeKind::Story,
            context: None,
            workspace_key,
            path: r.path,
            branch: r.branch,
            dirty: r.dirty,
            repo_path: r.repo_path,
            live_cli_session: r.live_cli_session,
            story_id: Some(r.story_id),
            source_repo: Some(r.source_repo),
            sync_status: Some(r.sync_status),
            failure_reason: r.failure_reason,
            story_source_path: Some(r.story_source_path),
            org_slug: Some(r.org_slug),
            team: r.team,
            orphaned: Some(r.orphaned),
            last_reverse_sync_at: r.last_reverse_sync_at,
            last_reverse_sync_by: r.last_reverse_sync_by,
            attribution: r.attribution,
            epic_id: None,
            owner: None,
            repo: None,
        }
    }

    /// Common non-story constructor for the git-only kinds (epic + orphan): every
    /// story/sync/hydration field is `None`; `branch` is the porcelain branch the caller
    /// already parsed from `git worktree list` (no re-read), and dirty/repo_path are read
    /// fail-soft off `path`. Keeps the two arms below from repeating the 15-field
    /// `None` boilerplate.
    fn git_only(
        kind: WorktreeKind,
        workspace_key: String,
        path: &Path,
        branch: Option<String>,
        live_cli_session: bool,
    ) -> Self {
        WorktreeRow {
            kind,
            context: None,
            workspace_key,
            path: path.to_string_lossy().into_owned(),
            branch,
            dirty: worktree_git_dirty_opt(path),
            repo_path: main_repo_of_worktree(path).map(|p| p.to_string_lossy().into_owned()),
            live_cli_session,
            story_id: None,
            source_repo: None,
            sync_status: None,
            failure_reason: None,
            story_source_path: None,
            org_slug: None,
            team: None,
            orphaned: None,
            last_reverse_sync_at: None,
            last_reverse_sync_by: None,
            attribution: None,
            epic_id: None,
            owner: None,
            repo: None,
        }
    }

    /// EpicIntegration arm: one discovered `_epic/{org}/{epic}/{owner}/{repo}`
    /// worktree, git-enriched. `workspace_key` = the epic id (epic = a group of N
    /// repos). Carries no record, so no story/sync/hydration state to fabricate.
    /// `branch` is the porcelain branch already in hand (same source the orphan arm
    /// trusts) — no extra git subprocess, and it can't disagree with a HEAD that moved
    /// mid-listing.
    fn epic(
        coords: crate::context_repo::EpicWorktreeCoords,
        path: &Path,
        branch: Option<String>,
        live: bool,
    ) -> Self {
        let mut row = Self::git_only(
            WorktreeKind::EpicIntegration,
            coords.epic_id.clone(),
            path,
            branch,
            live,
        );
        row.epic_id = Some(coords.epic_id);
        row.owner = Some(coords.owner);
        row.repo = Some(coords.repo);
        row
    }

    /// Orphan arm: a discovered worktree under a managed clone that matched no
    /// record, no unique-live story branch, and no `_epic/` shape (DESIGN §2.4).
    /// `workspace_key` = the parent clone (orphans group by the clone they hang
    /// off), falling back to the worktree path itself if the clone can't be resolved
    /// so the row still renders in a stable one-member group. Reveal-only in the
    /// panel — there is no story/epic affordance to offer.
    fn orphan(path: &Path, branch: Option<String>, live: bool) -> Self {
        let workspace_key = main_repo_of_worktree(path)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        Self::git_only(WorktreeKind::Orphan, workspace_key, path, branch, live)
    }
}

/// SLW-WL-9 (AC-1b/1c): the Worktrees panel's list result — the enriched rows PLUS
/// how many ghost records the reconcile-against-disk pass reaped this call. `reaped`
/// feeds the user-initiated Refresh receipt ("Removed N worktrees no longer on disk");
/// the background/event-driven load ignores it (silent self-heal). SW-WU-13: `rows`
/// is now the ONE unified `Vec<WorktreeRow>` (story + epic + orphan), replacing the
/// pre-flip split of story `rows` + a separate `epic_worktrees` field. camelCase like
/// the rest of the worktree API.
#[derive(Debug, Default, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedWorktreeListing {
    pub rows: Vec<WorktreeRow>,
    pub reaped: usize,
}

/// SLW-WL-2 (AC-2): the management view for the Worktrees panel — every
/// codesense-managed worktree for the active user, enriched with branch/dirty/
/// parent-repo. User-global (no `repo_path` arg): [`iter_records`] is per-user across
/// all repos, the SAME source of truth (and removable set) the merge-teardown poll
/// keys off (OQ-1). No active team ⇒ empty listing (mirrors the teardown tick's early
/// return). The record enumeration (disk I/O) AND all per-worktree git are folded on
/// ONE blocking-pool thread via `await_blocking`, never the async worker — the panel's
/// `list` call can't jank the UI even with many worktrees.
///
/// SLW-WL-9 (AC-1b): before enriching, reconcile the registry against disk — reap
/// every record whose path is [`path_confirmed_gone`](crate::install::worktree_index::path_confirmed_gone)
/// (gone-vs-unmounted gate) so a worktree removed out-of-band (terminal
/// `git worktree remove`) can never linger as a ghost row. The reap runs INSIDE the
/// same blocking closure, so it adds no async-worker stall.
#[tauri::command]
pub async fn list_managed_worktrees(
    app: tauri::AppHandle,
    coord: tauri::State<'_, std::sync::Arc<crate::content_engine::sync::WaveCoordinator>>,
    cache: tauri::State<'_, crate::install::frontmatter_cache::SharedFrontmatterCache>,
) -> Result<ManagedWorktreeListing, String> {
    // SW-WU-11: `active_target_repos` gives BOTH the ctx and the attached source-repo
    // `slugs` — the input `discover_worktrees` needs to git-enumerate the managed clones
    // (the same set the sync wave draws). `active_ctx` alone couldn't reach the slugs.
    let Some((ctx, slugs)) = coord.active_target_repos() else {
        return Ok(ManagedWorktreeListing::default());
    };
    let codesense_home = ctx.codesense_home.clone();
    let user_id = ctx.user_id;
    // SW-WU-11: ONE O(1) frontmatter-cache snapshot (Arc clone) taken on the async side
    // and moved into the blocking closure — ADOPT resolves each external worktree's
    // branch → story against it. NEVER `build_from_scan` here: it walks the whole story
    // tree and would jank the panel.
    let cache = crate::install::frontmatter_cache::FrontmatterCache::snapshot(cache.inner());
    // SLW-WL-5 (AC-2): snapshot every terminal host's live-session home dirs up front,
    // off the async workers (unknown on contention or an unreachable host → every
    // worktree reads live this fetch, so none is offered for cleanup on a guess;
    // self-healing on the next refetch). The resulting
    // `Vec<PathBuf>` moves into the closure, so the actual liveness derivation (and all
    // the git enrichment) still runs off the async worker inside `await_blocking`.
    let live_home_dirs =
        crate::commands::pty_backend::live_session_home_dirs_off_thread(&app).await;
    let sessions = app
        .try_state::<super::dashboard_launch::DashboardLaunchState>()
        .map(|s| s.snapshot())
        .unwrap_or_default();
    let live_app = app.clone();
    let ids: Vec<_> = sessions
        .iter()
        .filter_map(|s| s.session_id.clone())
        .collect();
    let alive = crate::commands::await_blocking("context_worktree.live_agents", move || {
        let ids: Vec<_> = ids.iter().map(String::as_str).collect();
        Ok(crate::commands::pty_backend::alive_session_ids(
            &live_app, &ids,
        ))
    })
    .await
    .unwrap_or(Liveness::Unknown);
    crate::commands::await_blocking("list_managed_worktrees", move || {
        let mut listing = list_managed_worktrees_core(
            &ctx,
            &slugs,
            &codesense_home,
            user_id,
            &cache,
            &live_home_dirs,
        )?;
        for record in crate::install::worktree_index::context_records(
            &codesense_home,
            &crate::domain::Principal::from(user_id),
        )
        .map_err(|e| e.to_string())?
        {
            // Retain the registration for explicit stale-id errors, but don't offer a vanished checkout.
            if !record.launch_cwd.is_dir() {
                continue;
            }
            let agents: Vec<_> = sessions
                .iter()
                .filter(|s| {
                    s.worktree_dir.as_ref() == Some(&record.launch_cwd)
                        && s.session_id.as_ref().is_some_and(|sid| match &alive {
                            Liveness::Known(ids) => ids.contains(sid),
                            Liveness::Unknown => false,
                        })
                })
                .map(|s| ContextWorktreeAgent {
                    handle: s.handle.clone(),
                    name: s
                        .meta
                        .display_name
                        .clone()
                        .unwrap_or_else(|| s.handle.clone()),
                    role: s.meta.role.clone(),
                })
                .collect();
            let mut row = WorktreeRow::git_only(
                WorktreeKind::Context,
                record.id.clone(),
                &record.root,
                Some(record.branch),
                !agents.is_empty(),
            );
            row.org_slug = Some(record.org);
            row.team = Some(record.team);
            row.context = Some(ContextWorktreeFields {
                id: record.id,
                name: record.name,
                launch_cwd: record.launch_cwd.to_string_lossy().into_owned(),
                base_sha: record.base_sha,
                created_at: record.created_at,
                last_used_at: record.last_used_at,
                agents,
            });
            listing.rows.push(row);
        }
        Ok(listing)
    })
    .await
}

/// The blocking core of [`list_managed_worktrees`]: reconcile the records against
/// disk (dropping only confirmed-gone ones), adopt external story worktrees, and
/// enrich every row. Split out so a test can drive the whole listing without an
/// `AppHandle`.
fn list_managed_worktrees_core(
    ctx: &crate::content_engine::ResolverContext,
    slugs: &[SourceRepoSlug],
    codesense_home: &Path,
    user_id: u64,
    cache: &crate::install::frontmatter_cache::FrontmatterCache,
    live_home_dirs: &Liveness<Vec<PathBuf>>,
) -> Result<ManagedWorktreeListing, String> {
    use crate::content_engine::sync::WaveCoordinator;
    let records = crate::install::worktree_index::iter_story_records(
        codesense_home,
        &Principal::from(user_id),
    )
    .map_err(|e| e.to_string())?;
    // SW-WU-13: git/disk is the authority for existence & liveness. Discover FIRST
    // (git-enumerate the active team's managed clones — every subprocess on THIS
    // blocking thread, off the async worker), then reconcile the user-global record
    // set against that discovered set. `iter_records` is RETAINED (not ripped out):
    // it is the durable, user-global Story memory, so a record for another synced
    // team's worktree still renders — the flip does not narrow the panel's scope
    // (DESIGN OQ-1, resolved to the union model). The index is demoted from
    // visibility gatekeeper to enrichment cache: a discovered worktree with no record
    // now appears (as an adopted Story, an epic, or an orphan) instead of being
    // invisible.
    let discovered = discover_worktrees(ctx, slugs);
    // Discovered worktrees that are PRESENT directories, each paired with its canonical
    // path — normalized (a `canonicalize` syscall) ONCE here and reused for both the
    // reap cross-check set and the epic/orphan fold, so a discovered path is not
    // re-canonicalized per consumer. Present (not a `git`-prunable ghost) is the
    // liveness signal the reap keys off; the extra-row build reuses it to skip a
    // directory removed out-of-band (git still lists a `prunable` worktree after the
    // working copy is gone — an orphan row for a vanished dir would be a ghost).
    // `is_dir` (not `exists`) also rejects a stray non-directory path.
    let present: Vec<(WorktreeInfo, PathBuf)> = discovered
        .iter()
        .filter(|w| PathBuf::from(&w.path).is_dir())
        .map(|w| {
            (
                w.clone(),
                WaveCoordinator::normalize_target_path(&PathBuf::from(&w.path)),
            )
        })
        .collect();
    let present_norm: std::collections::HashSet<PathBuf> =
        present.iter().map(|(_, n)| n.clone()).collect();
    // SW-WU-13 (AC-2): reap a record IFF it is `path_confirmed_gone` AND NOT in the
    // discovered set. Partition the discovered-present records out FIRST (see
    // [`partition_reap_candidates`]) — they are git-listed on disk, so they are
    // protected from the gone-check regardless of any `try_exists` race. The rest go
    // through the SAME shared reap (SLW-WL-9) the background `reconcile_worktrees_with`
    // uses, so a gone worktree still disappears by the next list/refresh.
    let (protected, reap_candidates) = partition_reap_candidates(records, &present_norm);
    let (mut survivors, reaped) = crate::install::worktree_index::reap_confirmed_gone_records(
        codesense_home,
        &Principal::from(user_id),
        reap_candidates,
    );
    survivors.extend(protected);
    // SW-WU-11 (ADOPT): externally-created STORY worktrees the index has never seen —
    // each discovered path whose branch resolves to a unique live story is backfilled
    // into a record and appended, so it renders as a Story row through the SAME
    // `enrich_rows` path below (no bespoke shape).
    let adopted = adopt_discovered_externals(
        &discovered,
        &survivors,
        cache,
        codesense_home,
        &Principal::from(user_id),
        &ctx.org_slug,
    );
    survivors.extend(adopted);
    // Story rows: the user-global record set through the reused enrichment path,
    // wrapped as unified rows (no story enrichment duplicated).
    let mut rows: Vec<WorktreeRow> = enrich_rows(survivors, live_home_dirs)
        .into_iter()
        .map(WorktreeRow::story)
        .collect();
    // Paths already carried by a Story row — so a discovered path WITH a record
    // renders exactly once (AC-3 dedup, record wins). The remaining discovered-present
    // paths fold into epic/orphan rows.
    let storied: std::collections::HashSet<PathBuf> = rows
        .iter()
        .map(|r| WaveCoordinator::normalize_target_path(&PathBuf::from(&r.path)))
        .collect();
    rows.extend(discovered_extra_rows(present, &storied, live_home_dirs));
    Ok(ManagedWorktreeListing {
        rows,
        reaped: reaped.len(),
    })
}

/// SW-WU-13 (AC-2): split records into `(protected, reap_candidates)`. A record whose
/// canonical path is in the discovered-present set is **protected** from the gone-check —
/// git lists it on disk, so it is never a ghost, regardless of any `try_exists` race. This
/// is the AND-guard that keeps a live cross-team record (present on disk but outside the
/// active team's discovered set) unreaped, and hands only the unprotected records to the
/// shared [`reap_confirmed_gone_records`](crate::install::worktree_index::reap_confirmed_gone_records).
fn partition_reap_candidates(
    records: Vec<crate::install::worktree_index::WorktreeStoryRecord>,
    present_norm: &std::collections::HashSet<PathBuf>,
) -> (
    Vec<crate::install::worktree_index::WorktreeStoryRecord>,
    Vec<crate::install::worktree_index::WorktreeStoryRecord>,
) {
    use crate::content_engine::sync::WaveCoordinator;
    records.into_iter().partition(|r| {
        present_norm.contains(&WaveCoordinator::normalize_target_path(&r.code_repo_path))
    })
}

/// SW-WU-13: fold the discovered-present set into unified epic + orphan rows. `present` is
/// each present-directory discovered worktree paired with its canonical path (built +
/// normalized ONCE by the caller). A worktree becomes a row unless it is already carried by
/// a Story row (`storied` — record wins, AC-3 dedup). The `_epic/{org}/…` path shape (via
/// [`classify_epic_worktree_path`]) splits it into a `kind=EpicIntegration` row, else a
/// `kind=Orphan` row (AC-4) — orphans first render here. Liveness is derived ONCE over the
/// kept roots via the shared [`live_worktree_roots`](crate::commands::worktree_liveness::live_worktree_roots)
/// helper (DRY — the same "which worktrees are live" definition `enrich_rows` uses). The
/// per-row git enrichment runs off `path`, so like the rest of the listing it belongs on
/// the blocking pool ([`list_managed_worktrees`]'s `await_blocking`).
fn discovered_extra_rows(
    present: Vec<(WorktreeInfo, PathBuf)>,
    storied: &std::collections::HashSet<PathBuf>,
    live_home_dirs: &Liveness<Vec<PathBuf>>,
) -> Vec<WorktreeRow> {
    let extra: Vec<(WorktreeInfo, PathBuf)> = present
        .into_iter()
        .filter(|(_, norm)| !storied.contains(norm))
        .collect();
    let roots: Vec<PathBuf> = extra.iter().map(|(_, n)| n.clone()).collect();
    let live =
        crate::commands::worktree_liveness::possibly_live_worktree_roots(live_home_dirs, &roots);
    extra
        .into_iter()
        .map(|(w, root)| {
            let path = PathBuf::from(&w.path);
            let is_live = live.contains(&root);
            match crate::context_repo::classify_epic_worktree_path_with_branch(
                &path,
                w.branch.as_deref(),
            ) {
                Some(coords) => WorktreeRow::epic(coords, &path, w.branch, is_live),
                None => WorktreeRow::orphan(&path, w.branch, is_live),
            }
        })
        .collect()
}

/// SLW-WL-9: enrich a set of (already reconciled-against-disk) records into panel rows
/// — normalize each worktree root ONCE, resolve the live-CLI set via the shared
/// [`live_worktree_roots`](crate::commands::worktree_liveness::live_worktree_roots) helper the
/// reverse filter uses (DRY — one definition of "which worktrees are live"), and map
/// each record through [`managed_row_from_record`]. Extracted so the reaped and
/// no-reap branches of [`list_managed_worktrees`] share ONE enrichment path.
fn enrich_rows(
    records: Vec<crate::install::worktree_index::WorktreeStoryRecord>,
    live_home_dirs: &Liveness<Vec<std::path::PathBuf>>,
) -> Vec<ManagedWorktreeRow> {
    let roots: Vec<std::path::PathBuf> = records
        .iter()
        .map(|r| {
            crate::content_engine::sync::WaveCoordinator::normalize_target_path(&r.code_repo_path)
        })
        .collect();
    let live =
        crate::commands::worktree_liveness::possibly_live_worktree_roots(live_home_dirs, &roots);
    records
        .into_iter()
        .zip(roots)
        .map(|(r, root)| managed_row_from_record(r, live.contains(&root)))
        .collect()
}

/// Parse `git worktree list --porcelain` output. Each worktree is a
/// record separated by blank lines with `key value` lines inside:
///
/// ```text
/// worktree /path/to/wt
/// HEAD abcdef1234
/// branch refs/heads/feat/something
/// ```
///
/// Bare and detached worktrees may omit `branch`. We ignore any keys
/// we don't recognize for forward compat.
fn parse_worktree_porcelain(out: &str) -> Vec<WorktreeInfo> {
    let mut result = Vec::new();
    let mut cur: Option<WorktreeInfoBuilder> = None;
    for line in out.lines() {
        if line.is_empty() {
            if let Some(b) = cur.take() {
                if let Some(info) = b.build() {
                    result.push(info);
                }
            }
            continue;
        }
        let (key, value) = match line.split_once(' ') {
            Some(kv) => kv,
            None => (line, ""),
        };
        let entry = cur.get_or_insert_with(WorktreeInfoBuilder::default);
        match key {
            "worktree" => entry.path = Some(value.to_string()),
            "HEAD" => entry.head_sha = Some(value.to_string()),
            "branch" => entry.branch = Some(value.trim_start_matches("refs/heads/").to_string()),
            _ => {}
        }
    }
    if let Some(b) = cur {
        if let Some(info) = b.build() {
            result.push(info);
        }
    }
    result
}

#[derive(Default)]
struct WorktreeInfoBuilder {
    path: Option<String>,
    branch: Option<String>,
    head_sha: Option<String>,
}

impl WorktreeInfoBuilder {
    fn build(self) -> Option<WorktreeInfo> {
        Some(WorktreeInfo {
            path: self.path?,
            branch: self.branch,
            head_sha: self.head_sha?,
        })
    }
}

// ---------------------------------------------------------------------------
// EventSink abstraction (Task 7.3) — lets us swap the production
// `AppHandle::emit` for a `RecordingEventSink` in unit tests. The
// `#[tauri::command]` wrappers construct a `TauriEventSink(app_handle)`
// and pass it to the inner functions via `&dyn EventSink`.
// ---------------------------------------------------------------------------

pub trait EventSink: Send + Sync {
    fn emit(&self, channel: &str, payload: serde_json::Value);
}

pub struct TauriEventSink(pub tauri::AppHandle);

impl EventSink for TauriEventSink {
    fn emit(&self, channel: &str, payload: serde_json::Value) {
        let _ = self.0.emit(channel, payload);
    }
}

/// SW-OR-1a: the headless (no-renderer) sink for a trusted-side worktree create.
/// An MCP orchestrator has no window to receive `sync-wave`/setup progress
/// events, so this drops the UI emit and mirrors it to `trace!` instead — the
/// setup still runs; only the (unobserved) progress stream is discarded.
#[cfg(test)]
pub struct NullEventSink;

#[cfg(test)]
impl EventSink for NullEventSink {
    fn emit(&self, channel: &str, payload: serde_json::Value) {
        log::trace!("headless worktree setup event (dropped): channel={channel} payload={payload}");
    }
}

// ---------------------------------------------------------------------------
// WorktreeRunRegistry (Task 5.2) — maps `run_id` → oneshot sender used
// by `cancel_worktree_run` to signal the streaming loop. Stored as a
// Tauri managed state and accessed via `State<'_, WorktreeRunRegistry>`.
// ---------------------------------------------------------------------------

/// Monotonic sequence counter used to identity-tag each inserted sender
/// so late-dropping RegistryGuards don't clobber a fresher entry for the
/// same run_id. This matters when React 18 strict mode (or any other
/// caller) fires two backend calls concurrently with the same run_id:
/// the first run's guard only removes its own entry, not the second's.
static NEXT_REGISTRATION_SEQ: AtomicU64 = AtomicU64::new(1);

type WorktreeRunEntry = (u64, oneshot::Sender<()>);
type WorktreeRunMap = HashMap<String, WorktreeRunEntry>;

#[derive(Default)]
pub struct WorktreeRunRegistry(Arc<Mutex<WorktreeRunMap>>);

impl WorktreeRunRegistry {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(HashMap::new())))
    }

    /// Insert a new sender for `run_id`, returning the unique sequence
    /// number assigned to this registration. If an entry already existed
    /// under the same `run_id`, the previous sender is dropped (which
    /// fires its receiver and cancels the older run — intentional).
    fn insert(&self, run_id: String, sender: oneshot::Sender<()>) -> u64 {
        let seq = NEXT_REGISTRATION_SEQ.fetch_add(1, Ordering::Relaxed);
        // Use into_inner on poison so cancellation doesn't become permanently
        // broken after any panic in a prior worktree run.
        let mut map = self.0.lock().unwrap_or_else(|e| e.into_inner());
        map.insert(run_id, (seq, sender));
        seq
    }

    /// Remove the entry for `run_id` **only if** its sequence number
    /// matches `seq`. This is what makes RegistryGuard identity-aware:
    /// a late-dropping guard from run A won't remove run B's sender.
    fn remove_if_seq(&self, run_id: &str, seq: u64) {
        let mut map = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((existing_seq, _)) = map.get(run_id) {
            if *existing_seq == seq {
                map.remove(run_id);
            }
        }
    }

    /// Pull the sender out of the map regardless of sequence number.
    /// Used by `cancel_worktree_run` — the caller wants to cancel
    /// whatever is currently registered under `run_id`.
    fn take(&self, run_id: &str) -> Option<oneshot::Sender<()>> {
        let mut map = self.0.lock().unwrap_or_else(|e| e.into_inner());
        map.remove(run_id).map(|(_, tx)| tx)
    }
}

/// Scope guard that removes a `run_id` from the registry on every exit
/// path (success, Err, panic), **but only if** the entry still belongs
/// to this guard's registration sequence. Construct via
/// `WorktreeRunRegistry::insert`'s returned seq and let the value drop
/// naturally — do not manually remove.
struct RegistryGuard<'a> {
    registry: &'a WorktreeRunRegistry,
    run_id: String,
    seq: u64,
}

impl<'a> Drop for RegistryGuard<'a> {
    fn drop(&mut self) {
        self.registry.remove_if_seq(&self.run_id, self.seq);
    }
}

// ---------------------------------------------------------------------------
// Streaming subprocess runner (Task 3)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandResult {
    pub command: String,
    pub passed: bool,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub phase: String,
    pub step_index: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunResult {
    pub results: Vec<CommandResult>,
    pub exit_code: i32,
}

fn now_ms() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as u64,
        Err(e) => {
            log::warn!(
                target: "worktree",
                "now_ms: system clock is before UNIX_EPOCH ({}); returning 0 — event ordering may be affected",
                e
            );
            0
        }
    }
}

fn emit_line(
    sink: &dyn EventSink,
    channel: &str,
    stream: &str,
    phase: &str,
    step_index: u32,
    line: &str,
) {
    sink.emit(
        channel,
        serde_json::json!({
            "stream": stream,
            "line": line,
            "phase": phase,
            "step_index": step_index,
            "ts_ms": now_ms(),
        }),
    );
}

/// SW-WT-12 (S2): per-command lifecycle boundaries on the SAME
/// `worktree-run-{run_id}` channel as `emit_line`, so the create UI can render a
/// live per-step checklist (queued -> running -> done/failed) instead of one
/// opaque "Running setup…" label. Carries an `event` discriminator that the
/// existing per-line consumer (no `event` field) simply ignores — additive,
/// leaving the line stream and `RunResult` untouched. `NullEventSink` drops these
/// (headless), same as the line stream.
fn emit_step_started(
    sink: &dyn EventSink,
    channel: &str,
    phase: &str,
    step_index: u32,
    command: &str,
) {
    sink.emit(
        channel,
        serde_json::json!({
            "event": "step-started",
            "phase": phase,
            "step_index": step_index,
            "command": command,
            "ts_ms": now_ms(),
        }),
    );
}

fn emit_step_finished(
    sink: &dyn EventSink,
    channel: &str,
    phase: &str,
    step_index: u32,
    passed: bool,
    exit_code: Option<i32>,
) {
    sink.emit(
        channel,
        serde_json::json!({
            "event": "step-finished",
            "phase": phase,
            "step_index": step_index,
            "passed": passed,
            "exit_code": exit_code,
            "ts_ms": now_ms(),
        }),
    );
}

/// Reason a command run ended. Used internally to decide how to emit
/// the final event and what exit code to record.
#[derive(Debug)]
enum StepOutcome {
    Exited(i32),
    TimedOut,
    Cancelled,
}

/// Execute a single command with streaming output, per-command timeout,
/// and cancellation via the given `cancel_rx`. Returns the result paired
/// with the explicit `StepOutcome` so callers use the enum variant to
/// detect cancellation instead of inferring from the exit code (which
/// collides with signaled-child unwrap_or(-1)).
/// Execution context shared by every step in a worktree command sequence.
/// Exists to keep `run_one_command` / `run_command_sequence` parameter
/// counts reasonable — all five fields are identical for every step in a
/// given run.
struct CommandExecCtx<'a> {
    sink: &'a dyn EventSink,
    channel: &'a str,
    phase: &'a str,
    cwd: &'a Path,
    /// Absolute path of the source/main worktree, injected into each command as
    /// `$CODESENSE_SOURCE_ROOT` (SW-WT-6) so setup steps can pull machine-local,
    /// gitignored files the context repo must not carry — e.g.
    /// `cp -R "$CODESENSE_SOURCE_ROOT/src-tauri/binaries/." src-tauri/binaries/`.
    /// `None` on the on-demand validate path, which runs in-worktree and has no
    /// source handle threaded in.
    source_root: Option<&'a Path>,
    /// PATH the setup subprocess should run with, resolved from the user's
    /// login shell (nvm/Homebrew/asdf/npm-global). A Finder-launched `.app`
    /// inherits only the minimal launchd PATH, so without this `npm`/`node`
    /// aren't found and setup fails with exit 127. `None` = probe failed or not
    /// resolved; the command then falls back to the inherited process PATH
    /// (pre-fix behavior — no regression). NOTE: a worktree config `env: { PATH }`
    /// is layered AFTER (via `cmd.envs(env)`) and still wins — an explicit user
    /// override takes precedence over this resolved default.
    path_override: Option<&'a str>,
    env: &'a HashMap<String, String>,
    timeout: Duration,
}

/// Terminate a running command. On Unix the command is spawned as its own
/// process-group leader (`process_group(0)`), so we signal the whole group with
/// `kill(-pid, SIGKILL)` — this reaps any grandchildren the shell started
/// (`npm`, `cargo`, …). Without the group signal, SIGKILL'ing `/bin/sh` alone
/// would orphan its children to init and they'd keep running after a
/// timeout/cancel. `start_kill()` remains as the leader/non-Unix backstop.
///
/// `pid` is the leader pid captured at spawn time (NOT re-read from
/// `child.id()`, which returns `None` once the child is reaped and could, in a
/// reap-then-reuse race, name an unrelated group).
fn kill_process_tree(child: &mut tokio::process::Child, pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        // Guard against pgid 0/1 (whole-session / init) before negating.
        if pid > 1 {
            // SAFETY: `pid` is this command's group-leader id captured at spawn;
            // negating it targets that group. Same `libc::kill` pattern as
            // http/registry.rs.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
    let _ = child.start_kill();
}

/// RF-perf-14 (Prong 2a): the `(program, args)` for running a setup command line
/// under a DE-PRIORITIZED shell. On unix: `nice -n 19 /bin/sh -c <cmd>` — CPU
/// niceness 19 (lowest priority) so a heavy `npm ci` / build yields to the UI
/// render process. `nice` execs the shell in-place (no fork), so the caller's
/// `process_group(0)` + kill-on-cancel semantics are preserved (nice's pid
/// becomes the shell's). `nice` is a standard POSIX utility resolved via PATH
/// (kept in the child's `ENV_ALLOWLIST`) — universally present on macOS/Linux,
/// so in practice this is as reliable as the shell itself, though (unlike the
/// absolute `/bin/sh`) it is a PATH-resolved binary. IO-nice (Linux-only
/// `ionice -c3`) is a deferred bonus (OQ-4: CPU-nice is the must-have). On
/// non-unix: a plain `/bin/sh -c <cmd>` (unchanged). Pure so the wrapping is
/// unit-tested without spawning a process.
fn setup_shell_argv(command: &str) -> (&'static str, Vec<String>) {
    #[cfg(unix)]
    {
        (
            "nice",
            vec![
                "-n".to_string(),
                "19".to_string(),
                "/bin/sh".to_string(),
                "-c".to_string(),
                command.to_string(),
            ],
        )
    }
    #[cfg(not(unix))]
    {
        ("/bin/sh", vec!["-c".to_string(), command.to_string()])
    }
}

async fn run_one_command(
    ctx: &CommandExecCtx<'_>,
    step_index: u32,
    command: &str,
    cancel_rx: &mut oneshot::Receiver<()>,
) -> (CommandResult, StepOutcome) {
    let sink = ctx.sink;
    let channel = ctx.channel;
    let phase = ctx.phase;
    let cwd = ctx.cwd;
    let env = ctx.env;
    let timeout = ctx.timeout;
    // Run the command line through a shell so users get pipes, `&&`, `$VAR`,
    // redirects, subshells, and direct script invocation (`bash scripts/x.sh`)
    // without committing a wrapper script. The shell IS the spawned child; its
    // own children are killed via the process group on timeout/cancel below.
    //
    // Build the subprocess environment from a minimal allowlist rather than
    // inheriting the full Tauri parent env. This prevents any API keys,
    // OAuth tokens, or other secrets loaded by the codesense process from
    // leaking into cargo/npm/shell subprocesses. Users who need additional
    // env vars must declare them in the worktree config's `env` map (which
    // is still layered on top below).
    const ENV_ALLOWLIST: &[&str] = &[
        "PATH", "HOME", "USER", "LANG", "LC_ALL", "SHELL", "TERM", "TMPDIR",
    ];

    // RF-perf-14 (Prong 2a): de-prioritize the setup subprocess so a heavy
    // `npm ci` / build YIELDS CPU to the UI/webview render process instead of
    // competing with it — the OS-level saturation that made a batch of worktree
    // creates *look* frozen. `setup_shell_argv` runs the shell under `nice` on
    // unix; `nice` execs the shell in-place (no fork), so the process-group +
    // kill-on-cancel semantics below are UNCHANGED (nice's pid becomes the
    // shell's, stays the group leader). `nice` resolves via the child's PATH
    // (kept in ENV_ALLOWLIST); it's a universally-present POSIX utility.
    let (program, args) = setup_shell_argv(command);
    let mut cmd = TokioCommand::new(program);
    cmd.args(&args).current_dir(cwd).env_clear();
    for key in ENV_ALLOWLIST {
        // A Finder-launched `.app` inherits the minimal launchd PATH, so tools
        // installed via nvm/Homebrew/asdf (npm, node, cargo) aren't found and
        // setup dies with exit 127. When we resolved the user's login-shell
        // PATH, hand the subprocess THAT instead of our inherited PATH so setup
        // runs with the same tools the user's terminal has. Everything else in
        // the allowlist is inherited as before.
        if *key == "PATH" {
            if let Some(path) = ctx.path_override {
                cmd.env("PATH", path);
                continue;
            }
        }
        if let Ok(val) = std::env::var(key) {
            cmd.env(key, val);
        }
    }
    // SW-WT-6: expose the source/main worktree root so a setup step can reach
    // machine-local, gitignored files in the source checkout that the context
    // repo must not carry (the open executor replaces `copy_from_main`).
    // Layered before the user `env:` map so a config can still override it.
    if let Some(source_root) = ctx.source_root {
        cmd.env("CODESENSE_SOURCE_ROOT", source_root);
    }
    cmd.envs(env)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    // Put the shell in its own process group (it becomes group leader, so its
    // pgid == its pid). On timeout/cancel we signal the whole group with
    // `kill(-pgid, ...)` so the shell's children die too — without this, a
    // SIGKILL to `/bin/sh` alone leaves `npm ci` / `cargo build` reparented to
    // init and still running after the user cancels.
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return (
                CommandResult {
                    command: command.to_string(),
                    passed: false,
                    stdout: String::new(),
                    stderr: format!("failed to spawn: {}", e),
                    exit_code: None,
                    phase: phase.to_string(),
                    step_index,
                },
                StepOutcome::Exited(-1),
            );
        }
    };

    // Capture the group-leader pid now, before any `wait()` poll can reap the
    // child and clear `child.id()`. Used by `kill_process_tree` on timeout/cancel.
    let child_pid = child.id();

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let mut stdout_reader = BufReader::new(stdout).lines();
    let mut stderr_reader = BufReader::new(stderr).lines();

    let mut stdout_buf = String::new();
    let mut stderr_buf = String::new();
    // Once a reader returns Ok(None) (EOF) or Err (decode failure), we mark
    // it done and disable that branch via the select! `if` guard. Otherwise
    // a closed reader's `next_line()` immediately resolves Ready(Ok(None))
    // forever, spinning the select loop until the timeout fires.
    let mut stdout_done = false;
    let mut stderr_done = false;
    let outcome;

    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            biased;

            // Cancellation wins over everything.
            _ = &mut *cancel_rx => {
                kill_process_tree(&mut child, child_pid);
                outcome = StepOutcome::Cancelled;
                break;
            }

            // Timeout.
            _ = &mut deadline => {
                kill_process_tree(&mut child, child_pid);
                outcome = StepOutcome::TimedOut;
                break;
            }

            // Stdout line. Explicit match on the inner Result so invalid UTF-8
            // from a subprocess produces a visible decode-error event instead
            // of silently hanging the select loop until the timeout fires.
            maybe_line = stdout_reader.next_line(), if !stdout_done => {
                match maybe_line {
                    Ok(Some(raw)) => {
                        for line in raw.split('\r').filter(|s| !s.is_empty()) {
                            emit_line(sink, channel, "stdout", phase, step_index, line);
                            stdout_buf.push_str(line);
                            stdout_buf.push('\n');
                        }
                    }
                    Ok(None) => {
                        // EOF on stdout. Disable this branch so the select
                        // doesn't spin on an already-closed reader; stderr
                        // and child.wait() arms still progress.
                        stdout_done = true;
                    }
                    Err(e) => {
                        let msg = format!("stdout decode error: {}", e);
                        emit_line(sink, channel, "stderr", phase, step_index, &msg);
                        stderr_buf.push_str(&msg);
                        stderr_buf.push('\n');
                        stdout_done = true;
                    }
                }
            }

            // Stderr line — same explicit-match treatment + done-flag guard.
            maybe_line = stderr_reader.next_line(), if !stderr_done => {
                match maybe_line {
                    Ok(Some(raw)) => {
                        for line in raw.split('\r').filter(|s| !s.is_empty()) {
                            emit_line(sink, channel, "stderr", phase, step_index, line);
                            stderr_buf.push_str(line);
                            stderr_buf.push('\n');
                        }
                    }
                    Ok(None) => {
                        stderr_done = true;
                    }
                    Err(e) => {
                        let msg = format!("stderr decode error: {}", e);
                        emit_line(sink, channel, "stderr", phase, step_index, &msg);
                        stderr_buf.push_str(&msg);
                        stderr_buf.push('\n');
                        stderr_done = true;
                    }
                }
            }

            // Process exit.
            status = child.wait() => {
                outcome = StepOutcome::Exited(
                    status.ok().and_then(|s| s.code()).unwrap_or(-1),
                );
                break;
            }
        }
    }

    // Drain any lines the readers produced after the select branch fired. For
    // Exited this catches the tail of buffered output; for TimedOut/Cancelled
    // the killed group's pipes are already at EOF so it returns at once. Bounded
    // by `POST_KILL_GRACE` so a descendant that escaped the group kill and still
    // holds the pipe open cannot block this drain forever.
    let drain_stdout = async {
        while let Ok(Some(raw)) = stdout_reader.next_line().await {
            for line in raw.split('\r').filter(|s| !s.is_empty()) {
                emit_line(sink, channel, "stdout", phase, step_index, line);
                stdout_buf.push_str(line);
                stdout_buf.push('\n');
            }
        }
    };
    let drain_stderr = async {
        while let Ok(Some(raw)) = stderr_reader.next_line().await {
            for line in raw.split('\r').filter(|s| !s.is_empty()) {
                emit_line(sink, channel, "stderr", phase, step_index, line);
                stderr_buf.push_str(line);
                stderr_buf.push('\n');
            }
        }
    };
    // Drain both streams concurrently under one deadline so a still-open stdout
    // can't starve the stderr drain (and vice versa).
    let _ = tokio::time::timeout(POST_KILL_GRACE, async {
        tokio::join!(drain_stdout, drain_stderr);
    })
    .await;

    match outcome {
        StepOutcome::Exited(code) => (
            CommandResult {
                command: command.to_string(),
                passed: code == 0,
                stdout: stdout_buf,
                stderr: stderr_buf,
                exit_code: Some(code),
                phase: phase.to_string(),
                step_index,
            },
            StepOutcome::Exited(code),
        ),
        StepOutcome::TimedOut => {
            emit_line(
                sink,
                channel,
                "timeout",
                phase,
                step_index,
                &format!("timed out after {}s", timeout.as_secs()),
            );
            let _ = tokio::time::timeout(POST_KILL_GRACE, child.wait()).await;
            stderr_buf.push_str(&format!("\ntimed out after {}s\n", timeout.as_secs()));
            (
                CommandResult {
                    command: command.to_string(),
                    passed: false,
                    stdout: stdout_buf,
                    stderr: stderr_buf,
                    exit_code: Some(-2),
                    phase: phase.to_string(),
                    step_index,
                },
                StepOutcome::TimedOut,
            )
        }
        StepOutcome::Cancelled => {
            emit_line(
                sink,
                channel,
                "cancel",
                phase,
                step_index,
                "cancelled by user",
            );
            let _ = tokio::time::timeout(POST_KILL_GRACE, child.wait()).await;
            (
                CommandResult {
                    command: command.to_string(),
                    passed: false,
                    stdout: stdout_buf,
                    stderr: stderr_buf,
                    exit_code: Some(-1),
                    phase: phase.to_string(),
                    step_index,
                },
                StepOutcome::Cancelled,
            )
        }
    }
}

/// Helper: run a sequence of commands with optional abort-on-first-failure.
/// Returns `(results, overall_exit_code, cancelled)`.
async fn run_command_sequence(
    ctx: &CommandExecCtx<'_>,
    commands: &[String],
    cancel_rx: &mut oneshot::Receiver<()>,
    abort_on_failure: bool,
) -> (Vec<CommandResult>, i32, bool) {
    let phase = ctx.phase;
    let mut results = Vec::with_capacity(commands.len());
    let mut overall = 0;
    let mut cancelled = false;

    for (idx, cmd) in commands.iter().enumerate() {
        // SW-WT-12 (S2): mark the step running before it emits any output, so the
        // checklist row flips queued -> running immediately (a slow `pnpm install`
        // reads as running, not stalled).
        emit_step_started(ctx.sink, ctx.channel, phase, idx as u32, cmd);
        let (result, outcome) = run_one_command(ctx, idx as u32, cmd, cancel_rx).await;
        // SW-WT-12 (S2): a user-cancelled step is teardown, not a failure — omit
        // its finished event so the checklist row doesn't flash ✗ as the workspace
        // closes. A genuine non-zero exit still emits finished(passed:false).
        if !matches!(outcome, StepOutcome::Cancelled) {
            emit_step_finished(
                ctx.sink,
                ctx.channel,
                phase,
                idx as u32,
                result.passed,
                result.exit_code,
            );
        }

        // Cancellation is driven by the explicit StepOutcome variant, NOT by
        // an exit-code sentinel — a child killed by signal (SIGSEGV, OOM)
        // also produces exit_code Some(-1) via `status.code().unwrap_or(-1)`
        // and would otherwise be misreported as a user cancellation.
        if matches!(outcome, StepOutcome::Cancelled) {
            cancelled = true;
        }
        if !result.passed {
            overall = result.exit_code.unwrap_or(-1);
        }
        let failed = !result.passed;
        results.push(result);

        if cancelled {
            // Append "skipped due to cancellation" placeholders for remaining
            // commands so the caller sees a complete list (AC-10b — placeholder
            // extension, not truncation).
            for (skip_idx, skip_cmd) in commands.iter().enumerate().skip(idx + 1) {
                results.push(CommandResult {
                    command: skip_cmd.clone(),
                    passed: false,
                    stdout: String::new(),
                    stderr: "skipped due to cancellation".to_string(),
                    exit_code: None,
                    phase: phase.to_string(),
                    step_index: skip_idx as u32,
                });
            }
            break;
        }

        if abort_on_failure && failed {
            for (skip_idx, skip_cmd) in commands.iter().enumerate().skip(idx + 1) {
                results.push(CommandResult {
                    command: skip_cmd.clone(),
                    passed: false,
                    stdout: String::new(),
                    stderr: "skipped due to earlier failure".to_string(),
                    exit_code: None,
                    phase: phase.to_string(),
                    step_index: skip_idx as u32,
                });
            }
            break;
        }
    }

    (results, overall, cancelled)
}

// ---------------------------------------------------------------------------
// Inner functions — logic testable without a real Tauri AppHandle
// ---------------------------------------------------------------------------

pub async fn run_worktree_setup_inner(
    sink: &dyn EventSink,
    registry: &WorktreeRunRegistry,
    worktree_path: &Path,
    source_worktree_path: &Path,
    config: &WorktreeConfig,
    run_id: &str,
) -> Result<RunResult, String> {
    let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
    let seq = registry.insert(run_id.to_string(), cancel_tx);
    let _guard = RegistryGuard {
        registry,
        run_id: run_id.to_string(),
        seq,
    };

    let channel = format!("worktree-run-{}", run_id);

    // Resolve the user's login-shell PATH once (memoized process-wide) on a
    // blocking thread so a Finder-launched app can run `npm ci`/`cargo` etc.
    // with the same tools the user's terminal has. `None` (probe failed) →
    // each command falls back to the inherited process PATH (no regression).
    let login_path = tokio::task::spawn_blocking(super::deps::cached_login_shell_path)
        .await
        .unwrap_or(None);

    let timeout = Duration::from_secs(config.timeout_seconds);
    let ctx = CommandExecCtx {
        sink,
        channel: &channel,
        phase: "setup",
        cwd: worktree_path,
        source_root: Some(source_worktree_path),
        path_override: login_path.as_deref(),
        env: &config.env,
        timeout,
    };
    let (results, exit_code, _cancelled) = run_command_sequence(
        &ctx,
        &config.setup,
        &mut cancel_rx,
        /* abort_on_failure */ true,
    )
    .await;

    Ok(RunResult { results, exit_code })
}

/// SW-EW-7 (T5): the dev-env execution mid-layer — everything the
/// `run_worktree_setup` command does **minus the IPC unpacking**:
/// `validate_config` → disk-authoritative config re-read (SW-WT-9, binds the
/// provenance gate to the exact bytes executed) → `require_provenance_approval`
/// (TOCTOU-safe; refuses unapproved `TeamSynced` setup, never prompts) →
/// `run_worktree_setup_inner`.
///
/// Extracted so the epic-workspace dev-env loop (SW-EW-7 AC-2) reuses the
/// **same** provenance gate + config-resolution path per assembled inner
/// worktree instead of duplicating it — the provenance gate lives in exactly
/// one place. `config` is the caller's fallback, used only when no on-disk
/// `worktree.yaml` resolves. An empty `setup` Vec runs as a no-op (the
/// data-driven run/skip — there is no `should_run_setup` predicate).
#[allow(clippy::too_many_arguments)]
pub async fn run_setup_for_worktree(
    sink: &dyn EventSink,
    registry: &WorktreeRunRegistry,
    worktree_path: &Path,
    source_worktree_path: &Path,
    org_root: &Path,
    authority: &WorktreeConfigAuthority,
    config: WorktreeConfig,
    approved_config_hash: Option<&str>,
    run_id: &str,
) -> Result<RunResult, String> {
    let _setup = crate::install::worktree_index::setup_lock(worktree_path).await;
    let registrations = crate::install::worktree_index::story_registrations(
        &authority.codesense_home,
        worktree_path,
    )
    .map_err(|e| e.to_string())?;
    if registrations.len() > 1
        || registrations.iter().any(|(owner, record)| {
            owner != &authority.principal
                || crate::install::worktree_index::resolve_team(record).as_deref()
                    != Some(authority.team.as_str())
        })
    {
        return Err("story setup authority does not match its durable registration".into());
    }
    let story = registrations.into_iter().next().map(|(_, record)| record);
    let story = story
        .map(|record| {
            crate::install::worktree_index::update_record_with(
                &authority.codesense_home,
                &authority.principal,
                &record.code_repo_path,
                |current| {
                    let mut current =
                        current.ok_or("story registration disappeared before setup")?;
                    if current.org_slug != record.org_slug
                        || current.source_repo != record.source_repo
                        || current.story_source_path != record.story_source_path
                        || current.story_id != record.story_id
                    {
                        return Err("story ownership changed before setup".into());
                    }
                    current.setup_complete = false;
                    Ok(current)
                },
            )
        })
        .transpose()?;
    validate_config(&config)?;
    // Execute the config as it exists on disk — the exact bytes the provenance
    // gate hashes — falling back to the caller's config only when no on-disk
    // config resolves.
    let config =
        match read_worktree_config_with_authority(source_worktree_path, org_root, authority) {
            Ok(Some(disk)) => disk,
            _ => config,
        };
    crate::commands::worktree_provenance::require_provenance_approval(
        source_worktree_path,
        org_root,
        approved_config_hash,
        !config.setup.is_empty(),
    )?;
    let result = run_worktree_setup_inner(
        sink,
        registry,
        worktree_path,
        source_worktree_path,
        &config,
        run_id,
    )
    .await?;
    if let Some(record) = story {
        crate::install::worktree_index::update_record_with(
            &authority.codesense_home,
            &authority.principal,
            &record.code_repo_path,
            |current| {
                let mut current = current.ok_or("story registration disappeared during setup")?;
                if current.org_slug != record.org_slug
                    || current.source_repo != record.source_repo
                    || current.story_source_path != record.story_source_path
                    || current.story_id != record.story_id
                {
                    return Err("story ownership changed during setup; completion refused".into());
                }
                current.setup_complete = result.exit_code == 0;
                Ok(current)
            },
        )?;
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// Tauri command wrappers
// ---------------------------------------------------------------------------

// Tauri command payload: app, config, source/worktree paths, branch +
// start point, org_root, team — all distinct IPC inputs the
// frontend supplies; a wrapper struct would just relocate the same fields.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn run_worktree_setup(
    app: tauri::AppHandle,
    worktree_path: String,
    source_worktree_path: String,
    config: WorktreeConfig,
    run_id: String,
    // org_root resolves the on-disk worktree.yaml for the provenance gate +
    // execution (SW-WT-9). The frontend is the single source of truth. Setup
    // copies no context (SW-XS-3), so `team` is not
    // threaded in.
    org_root: String,
    // SW-WT-9: hash of the config the user approved running. `None` when the
    // frontend hasn't gated (provenance is local, or it's an older caller).
    // The guard re-derives provenance from disk and only requires this for
    // `TeamSynced` configs — see `require_provenance_approval`.
    approved_config_hash: Option<String>,
) -> Result<RunResult, String> {
    // Thin IPC wrapper over the shared `run_setup_for_worktree` mid-layer
    // (SW-EW-7 T5): unpack the AppHandle into a sink + registry, delegate.
    //
    // SW-XS-3: setup runs `worktree.yaml` only. Context reaches the worktree
    // through agent preparation, never from here.
    let registry = app.state::<WorktreeRunRegistry>().inner().clone_arc();
    let sink = TauriEventSink(app.clone());
    let org_root_path = PathBuf::from(&org_root);
    let authority = active_worktree_authority(&app, &org_root_path).await?;
    run_setup_for_worktree(
        &sink,
        &registry,
        Path::new(&worktree_path),
        Path::new(&source_worktree_path),
        &org_root_path,
        &authority,
        config,
        approved_config_hash.as_deref(),
        &run_id,
    )
    .await
}

#[tauri::command]
pub fn cancel_worktree_run(app: tauri::AppHandle, run_id: String) -> Result<(), String> {
    let registry = app.state::<WorktreeRunRegistry>();
    if let Some(sender) = registry.take(&run_id) {
        let _ = sender.send(());
    }
    // No-op if run already resolved — per AC-10b contract.
    Ok(())
}

// Arc-cloning helper so Tauri's State<'_, WorktreeRunRegistry> can be
// dropped before we enter the async body.
impl WorktreeRunRegistry {
    pub(crate) fn clone_arc(&self) -> WorktreeRunRegistry {
        WorktreeRunRegistry(Arc::clone(&self.0))
    }
}

// ---------------------------------------------------------------------------
// Branch-existence pre-flight check (F-29)
// ---------------------------------------------------------------------------

/// Check whether a branch exists locally or on the remote. Used by the
/// frontend's pre-flight guard to detect missing dependency branches
/// before attempting `git worktree add`.
///
/// Reuses `local_branch_exists` / `remote_branch_exists` from
/// `context_repo::branch_common` (promoted to `pub(crate)` for F-29).
#[tauri::command]
pub async fn check_branch_exists(repo_path: String, branch_name: String) -> Result<bool, String> {
    crate::commands::await_blocking("check_branch_exists", move || {
        let repo = git2::Repository::open(&repo_path)
            .map_err(|e| format!("failed to open repo {}: {}", repo_path, e))?;

        let local = crate::context_repo::branch_common::local_branch_exists(&repo, &branch_name)
            .map_err(|e| format!("local branch check failed: {}", e))?;
        if local {
            return Ok(true);
        }

        let remote = crate::context_repo::branch_common::remote_branch_exists(&repo, &branch_name)
            .map_err(|e| format!("remote branch check failed: {}", e))?;
        Ok(remote)
    })
    .await
}

// ---------------------------------------------------------------------------
// Tests (Task 7.1)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn committed_repo_without_origin(path: &Path) -> git2::Repository {
        let repo = git2::Repository::init(path).unwrap();
        fs::write(path.join("README.md"), "sample").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("README.md")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        {
            let tree = repo.find_tree(tree_id).unwrap();
            let sig = git2::Signature::now("t", "t@example.com").unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
                .unwrap();
        }
        repo
    }

    #[test]
    fn a_repo_without_origin_has_a_worktree_config_identity() {
        let home = TempDir::new().unwrap();
        let org = TempDir::new().unwrap();
        let repos = TempDir::new().unwrap();
        let source = repos.path().join("sample-api");
        let repo = committed_repo_without_origin(&source);
        let linked = repos.path().join("sample-api-wt-feat");
        repo.worktree("feat", &linked, None).unwrap();
        let authority = WorktreeConfigAuthority {
            codesense_home: home.path().to_path_buf(),
            principal: Principal::from(7u64),
            team: "default".to_owned(),
        };

        assert_eq!(
            read_worktree_config_with_authority(&source, org.path(), &authority),
            Ok(None),
            "a local repo with no remote reads as a missing config, not an invalid one"
        );
        write_worktree_config_with_authority(
            &source,
            org.path(),
            &authority,
            "version: 1\nbranch_prefix: local/\n",
        )
        .unwrap();
        for path in [&source, &linked] {
            let config = read_worktree_config_with_authority(path, org.path(), &authority)
                .unwrap()
                .expect("config written for the source repo resolves");
            assert_eq!(config.branch_prefix, "local/");
        }
    }
    use std::fs;
    use tempfile::TempDir;

    // RF-perf-14 (Prong 2a): the setup subprocess is run under `nice` so a heavy
    // `npm ci` yields CPU to the UI. Assert the de-prioritized argv (the spawn
    // config) — the shell is still `/bin/sh -c <cmd>`, just wrapped by `nice`.
    #[cfg(unix)]
    #[test]
    fn setup_shell_argv_deprioritizes_the_shell_with_nice() {
        let (program, args) = setup_shell_argv("npm ci && npm run build");
        assert_eq!(program, "nice");
        assert_eq!(
            args,
            vec!["-n", "19", "/bin/sh", "-c", "npm ci && npm run build"]
        );
    }

    #[cfg(not(unix))]
    #[test]
    fn setup_shell_argv_is_a_plain_shell_off_unix() {
        let (program, args) = setup_shell_argv("echo hi");
        assert_eq!(program, "/bin/sh");
        assert_eq!(args, vec!["-c", "echo hi"]);
    }

    fn write_config(dir: &Path, contents: &str) -> PathBuf {
        let codesense_dir = dir.join(".codesense");
        fs::create_dir_all(&codesense_dir).unwrap();
        let config_path = codesense_dir.join("worktree.yaml");
        fs::write(&config_path, contents).unwrap();
        config_path
    }

    /// SS.8: write a yaml at the new
    /// `{org_root}/_codesense/worktree/{host}/{owner}/{repo}.yaml` location. Used
    /// by the SS.8 readers tests to drop the new-namespace yaml on disk.
    fn write_new_path_config(org_root: &Path, owner: &str, repo: &str, contents: &str) -> PathBuf {
        let dir = org_root
            .join("_codesense")
            .join("worktree")
            .join("github.com")
            .join(owner);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.yaml", repo));
        fs::write(&path, contents).unwrap();
        path
    }

    /// SS.8: init a git repo at `path` with an `origin` remote URL so
    /// `derive_slug_from_repo` returns the expected `(owner, repo)` slug.
    /// Mirrors the equivalent helper in `install/team_switch.rs` tests.
    fn init_git_repo_with_remote(path: &Path, remote_url: &str) {
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("master");
        let repo = git2::Repository::init_opts(path, &opts).unwrap();
        repo.remote("origin", remote_url).unwrap();
    }

    /// Init a git repo at `dir` with one empty commit so HEAD is born on `branch`.
    /// Returns the commit oid so a caller can detach HEAD onto it.
    fn init_repo_on_branch(dir: &Path, branch: &str) -> git2::Oid {
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head(branch);
        let repo = git2::Repository::init_opts(dir, &opts).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap()
    }

    #[test]
    fn current_branch_of_worktree_reads_the_checked_out_branch() {
        let tmp = TempDir::new().unwrap();
        init_repo_on_branch(tmp.path(), "feat/x");
        assert_eq!(
            current_branch_of_worktree(tmp.path()),
            Some("feat/x".to_string())
        );
    }

    #[test]
    fn current_branch_of_worktree_is_none_on_detached_head() {
        let tmp = TempDir::new().unwrap();
        let oid = init_repo_on_branch(tmp.path(), "main");
        git2::Repository::open(tmp.path())
            .unwrap()
            .set_head_detached(oid)
            .unwrap();
        assert_eq!(current_branch_of_worktree(tmp.path()), None);
    }

    #[test]
    fn current_branch_of_worktree_is_none_outside_a_git_repo() {
        // A non-repo dir makes `git rev-parse` fail (non-zero exit) — the honest degrade is
        // None, distinct from the detached-HEAD path that exits zero with "HEAD".
        let tmp = TempDir::new().unwrap();
        assert_eq!(current_branch_of_worktree(tmp.path()), None);
    }

    #[test]
    fn verify_worktree_branch_ok_on_match_err_otherwise() {
        let tmp = TempDir::new().unwrap();
        let oid = init_repo_on_branch(tmp.path(), "feat/x");
        assert!(
            verify_worktree_branch(tmp.path(), "feat/x").is_ok(),
            "on the expected branch → Ok"
        );
        assert!(
            verify_worktree_branch(tmp.path(), "feat/other").is_err(),
            "a different branch → Err, so no work product is attributed to the wrong branch"
        );
        git2::Repository::open(tmp.path())
            .unwrap()
            .set_head_detached(oid)
            .unwrap();
        assert!(
            verify_worktree_branch(tmp.path(), "feat/x").is_err(),
            "detached HEAD → Err (honest degrade to no work product)"
        );
    }

    fn minimal_valid_yaml() -> &'static str {
        r#"
version: 1
setup:
  - npm ci
"#
    }

    #[test]
    fn test_read_missing_file_returns_ok_none() {
        let tmp = TempDir::new().unwrap();
        assert!(matches!(
            read_worktree_config(tmp.path(), tmp.path()),
            Ok(None)
        ));
    }

    #[test]
    fn test_read_valid_minimal_config() {
        let tmp = TempDir::new().unwrap();
        write_config(tmp.path(), minimal_valid_yaml());
        let config = read_worktree_config(tmp.path(), tmp.path())
            .unwrap()
            .unwrap();
        assert_eq!(config.version, 1);
        assert_eq!(config.setup, vec!["npm ci"]);
        // Optional fields default.
        assert!(config.cleanup.is_empty());
        assert!(config.env.is_empty());
        assert_eq!(config.branch_prefix, DEFAULT_BRANCH_PREFIX);
        assert_eq!(config.worktree_dir, DEFAULT_WORKTREE_DIR);
        assert_eq!(config.timeout_seconds, DEFAULT_TIMEOUT_SECONDS);
    }

    #[test]
    fn test_read_full_config() {
        let tmp = TempDir::new().unwrap();
        let full = r#"
version: 1
setup:
  - npm ci
  - bash scripts/build-bridge.sh
cleanup:
  - echo cleanup
env:
  CODESENSE_DISABLE_MCP: "1"
branch_prefix: "story/"
worktree_dir: "../{repo}-wt/{slug}"
timeout_seconds: 900
"#;
        write_config(tmp.path(), full);
        let config = read_worktree_config(tmp.path(), tmp.path())
            .unwrap()
            .unwrap();
        assert_eq!(config.setup.len(), 2);
        assert_eq!(config.cleanup, vec!["echo cleanup"]);
        assert_eq!(config.env.get("CODESENSE_DISABLE_MCP").unwrap(), "1");
        assert_eq!(config.branch_prefix, "story/");
        assert_eq!(config.worktree_dir, "../{repo}-wt/{slug}");
        assert_eq!(config.timeout_seconds, 900);
        assert_eq!(config.default_start_point, None);
    }

    /// SW-WT-6 (AC-2): every field is optional. A config containing only
    /// `version: 1` is valid and defaults everything else.
    #[test]
    fn test_read_version_only_config_defaults_everything() {
        let tmp = TempDir::new().unwrap();
        write_config(tmp.path(), "version: 1\n");
        let config = read_worktree_config(tmp.path(), tmp.path())
            .unwrap()
            .unwrap();
        assert_eq!(config.version, 1);
        assert!(config.setup.is_empty());
        assert!(config.cleanup.is_empty());
        assert!(config.env.is_empty());
        assert_eq!(config.branch_prefix, DEFAULT_BRANCH_PREFIX);
        assert_eq!(config.worktree_dir, DEFAULT_WORKTREE_DIR);
        assert_eq!(config.timeout_seconds, DEFAULT_TIMEOUT_SECONDS);
        assert!(config.scope.is_none());
    }

    /// SW-XS-5 (review): an older client's `scope:` designation is inert but
    /// survives a read → write round-trip untouched.
    #[test]
    fn xs5_legacy_scope_round_trips_untouched() {
        let tmp = TempDir::new().unwrap();
        let legacy = "version: 1\nsetup:\n  - pnpm install\nscope:\n  context:\n    - ' docs '\n  install: []\n  by-future: 3\n";
        write_config(tmp.path(), legacy);
        let config = read_worktree_config(tmp.path(), tmp.path())
            .unwrap()
            .unwrap();
        let written = serde_yaml::to_string(&config).unwrap();
        let before: serde_yaml::Mapping = serde_yaml::from_str(legacy).unwrap();
        let after: serde_yaml::Mapping = serde_yaml::from_str(&written).unwrap();
        assert_eq!(after.get("scope"), before.get("scope"));
        assert_eq!(after.get("setup"), before.get("setup"));
    }

    /// SLW-NC-7: the team pin write rejects an empty team / relative org_root /
    /// blank path so a mis-call can't write pins to the wrong (or process-CWD)
    /// location. The happy-path write is covered by the store's `set_team_pin`
    /// tests — this guards the command's input contract.
    #[tokio::test]
    async fn set_team_nav_pin_rejects_bad_inputs() {
        // relative org_root
        assert!(set_team_nav_pin(
            "relative/root".to_string(),
            "default".to_string(),
            "a/b".to_string(),
            true,
        )
        .await
        .is_err());
        // empty team
        assert!(set_team_nav_pin(
            "/org/root".to_string(),
            "  ".to_string(),
            "a/b".to_string(),
            true,
        )
        .await
        .is_err());
        // blank path
        assert!(set_team_nav_pin(
            "/org/root".to_string(),
            "default".to_string(),
            "   ".to_string(),
            true,
        )
        .await
        .is_err());
    }

    /// SLW-NC-7 (AC-10): the pin read is fail-open — bad inputs yield an empty list,
    /// never an error, so a nav shortcut hint can't break the render.
    #[tokio::test]
    async fn get_team_nav_pins_is_fail_open() {
        assert!(
            get_team_nav_pins("relative/root".to_string(), "default".to_string())
                .await
                .is_empty()
        );
        assert!(get_team_nav_pins("/org/root".to_string(), "".to_string())
            .await
            .is_empty());
    }

    /// SW-WT-6 (AC-2): an empty (whitespace-only) file is a first-class
    /// "all defaults" config — `version` defaults to the current schema
    /// version, so it passes the version gate rather than erroring.
    #[test]
    fn test_read_empty_file_is_all_defaults() {
        let tmp = TempDir::new().unwrap();
        write_config(tmp.path(), "\n   \n");
        let config = read_worktree_config(tmp.path(), tmp.path())
            .unwrap()
            .unwrap();
        assert_eq!(config.version, WORKTREE_CONFIG_VERSION);
        assert!(config.setup.is_empty());
    }

    /// SW-WT-6: a content-free config that is NOT whitespace-empty — a
    /// comment-only file or an explicit `null`/`~` — parses to a YAML null and
    /// must also collapse to all-defaults, not error. (Code-review hardening:
    /// the first cut only special-cased the whitespace-empty branch.)
    #[test]
    fn test_read_null_and_comment_only_configs_are_all_defaults() {
        for body in ["# just a note, fill in later\n", "null\n", "~\n"] {
            let tmp = TempDir::new().unwrap();
            write_config(tmp.path(), body);
            let config = read_worktree_config(tmp.path(), tmp.path())
                .unwrap_or_else(|e| panic!("body {body:?} should parse: {e}"))
                .unwrap_or_else(|| panic!("body {body:?} should be Some"));
            assert_eq!(config.version, WORKTREE_CONFIG_VERSION, "body {body:?}");
            assert!(config.setup.is_empty(), "body {body:?}");
        }
    }

    // -----------------------------------------------------------------
    // SS.8: read-side resolution between new + legacy yaml locations
    // -----------------------------------------------------------------

    #[test]
    fn test_ss8_reads_from_new_path_when_slug_resolves_and_yaml_exists() {
        // Source repo with an `origin` remote that normalizes to
        // (alderco, codesense) lives next to the org root in TempDir.
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source");
        let org_root = tmp.path().join("org");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&org_root).unwrap();
        init_git_repo_with_remote(&source, "https://github.com/alderco/codesense.git");
        write_new_path_config(&org_root, "alderco", "codesense", minimal_valid_yaml());

        let config = read_worktree_config(&source, &org_root).unwrap().unwrap();
        assert_eq!(config.version, 1);
        assert_eq!(config.setup, vec!["npm ci"]);
    }

    // -----------------------------------------------------------------
    // SW-WT-4: write_worktree_config — resolve org_root + write SS.8 path
    // -----------------------------------------------------------------

    /// Build a `(source_repo, org_root)` pair under one TempDir: a git repo
    /// whose `origin` normalizes to (alderco, codesense), and an absolute
    /// org root. Returns `(tmp, source, org_root)` — keep `tmp` alive.
    fn write_fixture() -> (TempDir, PathBuf, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source");
        let org_root = tmp.path().join("org");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&org_root).unwrap();
        init_git_repo_with_remote(&source, "https://github.com/alderco/codesense.git");
        (tmp, source, org_root)
    }

    #[test]
    fn test_ww4_writes_canonical_path_and_returns_it() {
        let (_tmp, source, org_root) = write_fixture();

        let written = write_worktree_config(&source, &org_root, minimal_valid_yaml()).unwrap();

        // AC-1/AC-4: path equals the shared path helper, is absolute, and the
        // file is byte-equal to the caller's input (parents auto-created).
        let expected = org_root
            .join("_codesense")
            .join("worktree")
            .join("github.com")
            .join("alderco")
            .join("codesense.yaml");
        assert_eq!(written, expected);
        assert!(written.is_absolute());
        assert!(written.exists());
        assert_eq!(fs::read_to_string(&written).unwrap(), minimal_valid_yaml());

        // The written config round-trips through the read side identically.
        let read_back = read_worktree_config(&source, &org_root).unwrap().unwrap();
        assert_eq!(read_back.version, 1);
        assert_eq!(read_back.setup, vec!["npm ci"]);
    }

    #[test]
    fn test_ww4_never_creates_codesense_dir_in_source_repo() {
        let (_tmp, source, org_root) = write_fixture();
        write_worktree_config(&source, &org_root, minimal_valid_yaml()).unwrap();
        // AC-4: nothing is written inside the source repo.
        assert!(!source.join(".codesense").exists());
    }

    #[test]
    fn test_ww4_malformed_yaml_writes_nothing() {
        let (_tmp, source, org_root) = write_fixture();
        let err =
            write_worktree_config(&source, &org_root, "this: : not: valid: yaml:").unwrap_err();
        assert!(err.contains("invalid worktree.yaml"), "got: {err}");
        // AC-3: no parent dir, no file, no stray tmp.
        assert!(!org_root.join("_codesense").exists());
    }

    #[test]
    fn test_ww4_invalid_version_writes_nothing() {
        let (_tmp, source, org_root) = write_fixture();
        let bad = "version: 2\nsetup: []\n";
        let err = write_worktree_config(&source, &org_root, bad).unwrap_err();
        assert!(err.contains("version"), "got: {err}");
        // AC-3: validation precedes any filesystem mutation.
        assert!(!org_root.join("_codesense").exists());
    }

    #[test]
    fn test_ww4_no_origin_remote_errors() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source");
        let org_root = tmp.path().join("org");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&org_root).unwrap();
        // A bare dir with no git repo → no slug derivable.
        let err = write_worktree_config(&source, &org_root, minimal_valid_yaml()).unwrap_err();
        assert!(err.contains("origin"), "got: {err}");
        // AC-8: never guess a path / fall back.
        assert!(!org_root.join("_codesense").exists());
    }

    #[test]
    fn test_ww4_overwrite_is_idempotent() {
        let (_tmp, source, org_root) = write_fixture();
        write_worktree_config(&source, &org_root, minimal_valid_yaml()).unwrap();

        let second = "version: 1\nsetup:\n  - npm ci\n  - cargo build\n";
        let written = write_worktree_config(&source, &org_root, second).unwrap();
        // AC-6: re-write replaces in place; no leftover tmp file beside it.
        assert_eq!(fs::read_to_string(&written).unwrap(), second);
        let dir = written.parent().unwrap();
        let strays: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(strays.is_empty(), "stray tmp files: {strays:?}");
    }

    #[test]
    fn test_ss8_falls_back_to_legacy_when_new_path_absent() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source");
        let org_root = tmp.path().join("org");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&org_root).unwrap();
        init_git_repo_with_remote(&source, "https://github.com/alderco/codesense.git");
        // Legacy yaml exists; new path is absent under org_root.
        write_config(&source, minimal_valid_yaml());

        let config = read_worktree_config(&source, &org_root).unwrap().unwrap();
        assert_eq!(config.version, 1);
    }

    #[test]
    fn test_ss8_new_path_wins_when_both_present() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source");
        let org_root = tmp.path().join("org");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&org_root).unwrap();
        init_git_repo_with_remote(&source, "https://github.com/alderco/codesense.git");

        // Legacy yaml has version 1 + npm ci; new yaml has a different
        // setup command so we can tell them apart in the assertion.
        write_config(&source, minimal_valid_yaml());
        let new_yaml = r#"
version: 1
setup:
  - cargo build
"#;
        write_new_path_config(&org_root, "alderco", "codesense", new_yaml);

        let config = read_worktree_config(&source, &org_root).unwrap().unwrap();
        // New path wins → its setup command lands in the parsed config.
        assert_eq!(config.setup, vec!["cargo build"]);
    }

    #[test]
    fn test_ss8_no_remote_skips_new_path_and_falls_through_to_legacy() {
        // Non-git source dir → `derive_slug_from_repo` returns None →
        // new-path branch is skipped entirely. Legacy yaml on disk under
        // `<source>/.codesense/worktree.yaml` is what `read_worktree_config`
        // finds.
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source");
        let org_root = tmp.path().join("org");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&org_root).unwrap();
        // Drop a new-path yaml that would otherwise win — but without a
        // resolvable slug, the reader never looks at it.
        let new_yaml_distinguishable = r#"
version: 1
setup:
  - this-should-NOT-be-picked
"#;
        write_new_path_config(&org_root, "alderco", "codesense", new_yaml_distinguishable);
        write_config(&source, minimal_valid_yaml());

        let config = read_worktree_config(&source, &org_root).unwrap().unwrap();
        // Legacy yaml's `npm ci` is what we get — new path was bypassed.
        assert_eq!(config.setup, vec!["npm ci"]);
    }

    #[test]
    fn test_ss8_returns_none_when_neither_path_exists() {
        // Slug DOES resolve, but no yaml exists at either location.
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source");
        let org_root = tmp.path().join("org");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&org_root).unwrap();
        init_git_repo_with_remote(&source, "https://github.com/alderco/codesense.git");

        assert!(matches!(read_worktree_config(&source, &org_root), Ok(None)));
    }

    #[test]
    fn test_ss8_empty_org_root_skips_new_path_and_seeder() {
        // Code-review patch #1: an empty `org_root` (TS bug where
        // `RepoState.contextRepoPath` is unresolved and `resolveOrgRoot`
        // returns "") must NOT cause the README seeder to fire — that
        // would create `./_codesense/README.md` in the Tauri process's
        // CWD. The reader skips the new-path branch entirely and falls
        // through to the legacy path.
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source");
        fs::create_dir_all(&source).unwrap();
        init_git_repo_with_remote(&source, "https://github.com/alderco/codesense.git");
        // Legacy yaml exists; new-path lookup with org_root="" should
        // bypass the slug+seed branch and resolve via legacy.
        write_config(&source, minimal_valid_yaml());

        let empty_org_root = Path::new("");
        let config = read_worktree_config(&source, empty_org_root)
            .unwrap()
            .unwrap();
        assert_eq!(config.setup, vec!["npm ci"]);

        // No README seeded under CWD (we don't write there).
        // The seeder is gated on `org_root.is_absolute()` so calling
        // it with "" would short-circuit at the absolutness check
        // BEFORE any filesystem touch happens. Belt-and-suspenders
        // assertion below verifies the seeded-set memo wasn't
        // populated for "" either: a follow-up call with a real
        // absolute org_root still seeds correctly.
        let real_org_root = tmp.path().join("org");
        fs::create_dir_all(&real_org_root).unwrap();
        let _ = read_worktree_config(&source, &real_org_root);
        assert!(
            real_org_root.join("_codesense").join("README.md").exists(),
            "real org_root must seed the README even after an earlier empty-org_root call"
        );
    }

    #[test]
    fn test_ss8_seeds_readme_when_slug_resolves() {
        // AC-8: first successful slug resolution against a real org_root
        // drops `_codesense/README.md`. Idempotent: a second call with an
        // existing README must not panic and must not overwrite.
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source");
        let org_root = tmp.path().join("org");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&org_root).unwrap();
        init_git_repo_with_remote(&source, "https://github.com/alderco/codesense.git");

        let _ = read_worktree_config(&source, &org_root);

        let readme = org_root.join("_codesense").join("README.md");
        assert!(
            readme.exists(),
            "AC-8: _codesense/README.md must exist after first slug-resolving read"
        );
        let original_bytes = fs::read(&readme).unwrap();

        // Second call: idempotent — bytes should be unchanged (memoized
        // seeded-set short-circuits before re-writing).
        let _ = read_worktree_config(&source, &org_root);
        let second_bytes = fs::read(&readme).unwrap();
        assert_eq!(
            original_bytes, second_bytes,
            "AC-8 seeder must be idempotent"
        );
    }

    #[test]
    fn test_default_start_point_parsed_when_present() {
        let tmp = TempDir::new().unwrap();
        let yaml = r#"
version: 1
setup:
  - npm ci
default_start_point: "develop"
"#;
        write_config(tmp.path(), yaml);
        let config = read_worktree_config(tmp.path(), tmp.path())
            .unwrap()
            .unwrap();
        assert_eq!(config.default_start_point, Some("develop".to_string()));
    }

    #[test]
    fn test_default_start_point_none_when_absent() {
        let tmp = TempDir::new().unwrap();
        write_config(tmp.path(), minimal_valid_yaml());
        let config = read_worktree_config(tmp.path(), tmp.path())
            .unwrap()
            .unwrap();
        assert_eq!(config.default_start_point, None);
    }

    #[test]
    fn test_malformed_yaml_returns_err() {
        let tmp = TempDir::new().unwrap();
        write_config(tmp.path(), "version: [this is not a number");
        assert!(read_worktree_config(tmp.path(), tmp.path()).is_err());
    }

    #[test]
    fn test_version_2_hard_rejected() {
        let tmp = TempDir::new().unwrap();
        write_config(
            tmp.path(),
            r#"
version: 2
setup: []
"#,
        );
        let err = read_worktree_config(tmp.path(), tmp.path()).unwrap_err();
        assert!(err.contains("version"));
    }

    /// SW-WT-6: the materialization fields (`copy_from_main`,
    /// `symlink_from_main`) and the earlier-removed `symlink_to_main` are gone
    /// from the schema — the sync/wave engine owns materialization now.
    /// SW-WT-11 (AC-2): the inert `validate` field is also gone and joins this
    /// set of silently-ignored legacy keys. `WorktreeConfig` has no
    /// `#[serde(deny_unknown_fields)]`, so a legacy config still carrying any of
    /// these keys must deserialize cleanly with the keys silently ignored — no
    /// hard-fail during the transition window (SS-12 owns the on-disk
    /// migration). The recognized preference fields are still read.
    #[test]
    fn test_legacy_removed_keys_are_ignored() {
        let tmp = TempDir::new().unwrap();
        write_config(
            tmp.path(),
            r#"
version: 1
setup:
  - npm ci
validate:
  - npm test
copy_from_main:
  - .env.local
  - src-tauri/binaries/
symlink_from_main:
  - _bmad
symlink_to_main:
  - _bmad-output
"#,
        );
        let config = read_worktree_config(tmp.path(), tmp.path())
            .unwrap()
            .unwrap();
        // Parses without error; the removed keys — including SW-WT-11's
        // `validate` — are silently dropped and the recognized preference fields
        // still load.
        assert_eq!(config.version, 1);
        assert_eq!(config.setup, vec!["npm ci"]);
    }

    #[test]
    fn test_open_executor_loads_shell_syntax_configs() {
        // SW-WT-5 (D1): commands run via `/bin/sh -c`, so convenience syntax
        // that the old metachar blacklist rejected (pipes, redirects, `$VAR`,
        // backticks, `&&`) must now LOAD. Each of these used to be a load-time
        // error. Runtime behavior is covered by the exec tests below.
        let cases = [
            "cat foo | grep bar",
            "echo hi > out",
            "echo $HOME",
            "echo `date`",
            "cargo build && ./scripts/bootstrap.sh",
            "cp ../$REPO/.env.local .",
        ];
        for cmd in cases {
            let tmp = TempDir::new().unwrap();
            write_config(tmp.path(), &format!("version: 1\nsetup:\n  - {:?}\n", cmd));
            assert!(
                read_worktree_config(tmp.path(), tmp.path()).is_ok(),
                "expected shell-syntax command to load now: {cmd:?}"
            );
        }
    }

    #[test]
    fn test_command_rejects_empty_string() {
        // The one load-time command rejection that survives D1: an empty /
        // whitespace-only command line is always a config mistake.
        let tmp = TempDir::new().unwrap();
        write_config(tmp.path(), "version: 1\nsetup:\n  - \"   \"\n");
        let err = read_worktree_config(tmp.path(), tmp.path()).unwrap_err();
        assert!(err.contains("empty"), "unexpected error: {err}");
    }

    #[test]
    fn test_command_accepts_quoted_arg() {
        let tmp = TempDir::new().unwrap();
        write_config(
            tmp.path(),
            r#"
version: 1
setup:
  - 'bash scripts/build-bridge.sh'
"#,
        );
        assert!(read_worktree_config(tmp.path(), tmp.path()).is_ok());
    }

    #[test]
    fn test_worktree_dir_rejects_unknown_token() {
        let tmp = TempDir::new().unwrap();
        write_config(
            tmp.path(),
            r#"
version: 1
setup: []
worktree_dir: "../{repo}-wt/{foo}"
"#,
        );
        let err = read_worktree_config(tmp.path(), tmp.path()).unwrap_err();
        assert!(err.contains("foo"));
    }

    #[test]
    fn test_worktree_dir_rejects_unclosed_brace() {
        let tmp = TempDir::new().unwrap();
        write_config(
            tmp.path(),
            r#"
version: 1
setup: []
worktree_dir: "../{repo-wt/{slug}"
"#,
        );
        assert!(read_worktree_config(tmp.path(), tmp.path()).is_err());
    }

    #[test]
    fn test_worktree_dir_accepts_defaults() {
        let tmp = TempDir::new().unwrap();
        write_config(tmp.path(), minimal_valid_yaml());
        let config = read_worktree_config(tmp.path(), tmp.path())
            .unwrap()
            .unwrap();
        assert_eq!(config.worktree_dir, DEFAULT_WORKTREE_DIR);
    }

    // ── SW-OR-1a: render_worktree_dir parity with FE `resolveWorktreePath` ──────

    #[test]
    fn render_default_template_yields_repo_sibling() {
        // Parity with the FE default `../{repo}-wt/{slug}`: a sibling of the clone,
        // {repo}=basename, {slug}=branch with `/`→`-`. Matches this very worktree's
        // layout (`codesense-wt/feat-…`).
        let got = render_worktree_dir(
            DEFAULT_WORKTREE_DIR,
            Path::new("/a/b/codesense"),
            "feat/SW-OR-1a-x",
        )
        .unwrap();
        assert_eq!(got, PathBuf::from("/a/b/codesense-wt/feat-SW-OR-1a-x"));
    }

    #[test]
    fn render_branch_slashes_become_dashes() {
        let got = render_worktree_dir(
            "../{repo}-wt/{slug}",
            Path::new("/x/widget"),
            "feature/nested/topic",
        )
        .unwrap();
        assert_eq!(got, PathBuf::from("/x/widget-wt/feature-nested-topic"));
    }

    #[test]
    fn render_absolute_template_taken_as_is() {
        // An absolute template is NOT resolved against repo_path (FE parity).
        let got =
            render_worktree_dir("/tmp/wt/{repo}/{slug}", Path::new("/a/b/repo"), "main").unwrap();
        assert_eq!(got, PathBuf::from("/tmp/wt/repo/main"));
    }

    #[test]
    fn render_relative_custom_template_resolves_against_repo() {
        let got = render_worktree_dir("./siblings/{slug}", Path::new("/a/b/repo"), "dev").unwrap();
        assert_eq!(got, PathBuf::from("/a/b/repo/siblings/dev"));
    }

    #[test]
    fn render_reuses_token_validator_rejecting_unknown_token() {
        // DRY: same rejection as config load (`validate_worktree_dir_tokens`).
        let err =
            render_worktree_dir("../{repo}-wt/{foo}", Path::new("/a/b/repo"), "main").unwrap_err();
        assert!(err.contains("foo"), "unexpected error: {err}");
    }

    #[test]
    fn render_rejects_non_absolute_result() {
        // A relative repo_path + relative template can't yield a valid target.
        let err =
            render_worktree_dir("../{repo}-wt/{slug}", Path::new("rel/repo"), "main").unwrap_err();
        assert!(err.contains("non-absolute"), "unexpected error: {err}");
    }

    // ── SW-OR-1a: resolve_story_branch parity with FE resolveWorktreeParams ─────

    #[test]
    fn resolve_branch_cold_story_falls_back_to_feat_story_id() {
        // The cold-open case: no intended branch_name → `{prefix}{story_id}`.
        // This is why dev_branch is WRONG here (it's absent for a cold story).
        assert_eq!(
            resolve_story_branch(None, DEFAULT_BRANCH_PREFIX, "SW-OR-1a"),
            "feat/SW-OR-1a"
        );
    }

    #[test]
    fn resolve_branch_uses_intended_branch_name_when_set() {
        assert_eq!(
            resolve_story_branch(Some("fix/custom-branch"), DEFAULT_BRANCH_PREFIX, "SW-OR-1a"),
            "fix/custom-branch"
        );
    }

    #[test]
    fn resolve_branch_treats_blank_branch_name_as_absent() {
        // Mirrors the FE `parseBranchNameFromFrontmatter` empty→null → fallback.
        assert_eq!(
            resolve_story_branch(Some("   "), DEFAULT_BRANCH_PREFIX, "SW-OR-1a"),
            "feat/SW-OR-1a"
        );
    }

    #[test]
    fn resolve_branch_honors_custom_prefix_in_fallback() {
        assert_eq!(resolve_story_branch(None, "wip/", "SW-XX-9"), "wip/SW-XX-9");
    }

    // SW-SO-2 (③): migrated from the retired `has_worktree_config` to the fused
    // `probe_worktree_config`. Each case now also asserts the `config` payload,
    // which the old two-call path fetched via a SECOND read (`get_worktree_config`);
    // the fuse must carry it on `Present` and leave it `None` otherwise.
    #[test]
    fn test_probe_worktree_config_present_carries_config() {
        let tmp = TempDir::new().unwrap();
        write_config(tmp.path(), minimal_valid_yaml());
        // SS.8: org_root passed identical to repo_path. Since tmp is not a
        // git repo, slug derivation returns None and the legacy fallback
        // hits `<tmp>/.codesense/worktree.yaml` (where the fixture wrote).
        let result = probe_worktree_config_core(tmp.path(), tmp.path(), None);
        assert_eq!(result.status, WorktreeConfigStatus::Present);
        // Present ⇒ the parsed config rides along (no second read needed).
        assert!(result.config.is_some());
    }

    #[test]
    fn test_probe_worktree_config_missing_when_absent() {
        let tmp = TempDir::new().unwrap();
        // SS.8: org_root passed identical to repo_path. Since tmp is not a
        // git repo, slug derivation returns None and the legacy fallback
        // hits `<tmp>/.codesense/worktree.yaml` (where the fixture wrote).
        let result = probe_worktree_config_core(tmp.path(), tmp.path(), None);
        assert_eq!(result.status, WorktreeConfigStatus::Missing);
        assert!(result.config.is_none());
    }

    #[test]
    fn test_probe_worktree_config_invalid_when_malformed() {
        // A malformed config surfaces as Invalid with a parse error so the
        // frontend can render a distinct error state (button still disabled but
        // with a clear reason), not silently indistinguishable from a missing
        // file. The open path throws on this before it would read `config`.
        let tmp = TempDir::new().unwrap();
        write_config(tmp.path(), "version: 2\nsetup: []");
        // SS.8: org_root passed identical to repo_path. Since tmp is not a
        // git repo, slug derivation returns None and the legacy fallback
        // hits `<tmp>/.codesense/worktree.yaml` (where the fixture wrote).
        let result = probe_worktree_config_core(tmp.path(), tmp.path(), None);
        assert!(matches!(
            result.status,
            WorktreeConfigStatus::Invalid { .. }
        ));
        assert!(result.config.is_none());
    }

    // ----- Git worktree ops (Task 2 / Task 7.2) -----

    #[tokio::test]
    async fn explicit_story_manual_removal_preserves_ignored_edits_and_force_is_explicit() {
        let home = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo_with_commit(repo.path());
        let parent = TempDir::new().unwrap();
        let root = parent.path().join("story");
        create_worktree(
            repo.path().to_string_lossy().into(),
            "feat/explicit-removal".into(),
            root.to_string_lossy().into(),
        )
        .await
        .unwrap();
        let record = wl2_record(&root, "S-1");
        crate::install::worktree_index::write_record(home.path(), &Principal::from(42), &record)
            .unwrap();
        crate::content_engine::git_exclude::add_entries(&root, &["/context.md"]).unwrap();
        fs::write(root.join("context.md"), "unpublished edit").unwrap();
        assert_eq!(worktree_git_dirty_opt(&root), Some(false));
        let args = (repo.path().to_string_lossy(), root.to_string_lossy());
        assert!(remove_registered_worktree(
            home.path(),
            &root,
            &args.0,
            &args.1,
            RemovalConsent::default()
        )
        .unwrap_err()
        .contains("ignored files"));
        assert_eq!(
            fs::read_to_string(root.join("context.md")).unwrap(),
            "unpublished edit"
        );
        assert!(crate::install::worktree_index::read_record(
            home.path(),
            &Principal::from(42),
            &root
        )
        .unwrap()
        .is_some());
        remove_registered_worktree(
            home.path(),
            &root,
            &args.0,
            &args.1,
            RemovalConsent {
                discard_context: true,
                discard_code: false,
            },
        )
        .unwrap();
        assert!(!root.exists());
    }

    /// The app's "Remove worktree…" passes `force: false, discardContext: true`
    /// after its own uncommitted-code and unpublished-context checks pass. A clean
    /// worktree holding ignored build output (`node_modules/`) must then remove;
    /// with `discardContext: false` the ignored-file guard refuses it.
    #[tokio::test]
    async fn the_apps_remove_args_remove_a_clean_worktree_with_ignored_node_modules() {
        let home = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo_with_commit(repo.path());
        let parent = TempDir::new().unwrap();
        let root = parent.path().join("story");
        create_worktree(
            repo.path().to_string_lossy().into(),
            "feat/ignored-deps".into(),
            root.to_string_lossy().into(),
        )
        .await
        .unwrap();
        crate::install::worktree_index::write_record(
            home.path(),
            &Principal::from(42),
            &wl2_record(&root, "S-1"),
        )
        .unwrap();
        crate::content_engine::git_exclude::add_entries(&root, &["/node_modules/"]).unwrap();
        fs::create_dir_all(root.join("node_modules/left-pad")).unwrap();
        fs::write(
            root.join("node_modules/left-pad/index.js"),
            "module.exports = 1;",
        )
        .unwrap();
        assert_eq!(worktree_git_dirty_opt(&root), Some(false));
        let args = (repo.path().to_string_lossy(), root.to_string_lossy());
        assert!(remove_registered_worktree(
            home.path(),
            &root,
            &args.0,
            &args.1,
            RemovalConsent::from_command_args(Some(false), Some(false)),
        )
        .unwrap_err()
        .contains("ignored files"));
        assert!(root.exists());
        remove_registered_worktree(
            home.path(),
            &root,
            &args.0,
            &args.1,
            RemovalConsent::from_command_args(Some(false), Some(true)),
        )
        .unwrap();
        assert!(!root.exists());
    }

    /// SV-CDE-82 (c): removing a story worktree first keeps its branch tip in the story's
    /// tips, which outlive the worktree and its record (the removal tail's reap and the
    /// reconcile reap both delete the record). Red on the base: nothing kept the tip.
    #[tokio::test]
    async fn removing_a_worktree_keeps_its_tip_in_the_storys_tips() {
        let home = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo_with_commit(repo.path());
        let parent = TempDir::new().unwrap();
        let root = parent.path().join("story");
        create_worktree(
            repo.path().to_string_lossy().into(),
            "feat/S-1-kept-tip".into(),
            root.to_string_lossy().into(),
        )
        .await
        .unwrap();
        let principal = Principal::from(42);
        let record = wl2_record(&root, "S-1");
        crate::install::worktree_index::write_record(home.path(), &principal, &record).unwrap();
        let tip = git2::Repository::open(&root)
            .unwrap()
            .head()
            .unwrap()
            .target()
            .unwrap();
        let args = (repo.path().to_string_lossy(), root.to_string_lossy());
        remove_registered_worktree(
            home.path(),
            &root,
            &args.0,
            &args.1,
            RemovalConsent::from_command_args(Some(false), Some(true)),
        )
        .unwrap();
        assert!(!root.exists());
        crate::content_engine::sync::wave::reap_detached_worktree(home.path(), &root);
        crate::install::worktree_index::reap_confirmed_gone_records(
            home.path(),
            &principal,
            vec![record],
        );
        assert!(
            crate::install::worktree_index::read_record(home.path(), &principal, &root)
                .unwrap()
                .is_none(),
            "the record is gone"
        );
        let tips = crate::install::story_tips::read(home.path(), "S-1");
        assert_eq!(tips.len(), 1, "{tips:?}");
        assert_eq!(tips[0].sha, tip.to_string());
        assert_eq!(tips[0].branch, "feat/S-1-kept-tip");
    }

    /// SW-XS-4: consenting to discard unpublished CONTEXT edits never discards
    /// uncommitted CODE — the git-dirty worktree is refused and kept intact until
    /// the code discard is consented to separately.
    #[tokio::test]
    async fn discarding_context_never_discards_uncommitted_code() {
        let home = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo_with_commit(repo.path());
        let parent = TempDir::new().unwrap();
        let root = parent.path().join("story");
        create_worktree(
            repo.path().to_string_lossy().into(),
            "feat/split-consent".into(),
            root.to_string_lossy().into(),
        )
        .await
        .unwrap();
        crate::install::worktree_index::write_record(
            home.path(),
            &Principal::from(42),
            &wl2_record(&root, "S-1"),
        )
        .unwrap();
        crate::content_engine::git_exclude::add_entries(&root, &["/context.md"]).unwrap();
        fs::write(root.join("context.md"), "unpublished edit").unwrap();
        fs::write(root.join("code.rs"), "uncommitted code").unwrap();
        assert_eq!(worktree_git_dirty_opt(&root), Some(true));
        let args = (repo.path().to_string_lossy(), root.to_string_lossy());
        let context_only = RemovalConsent {
            discard_context: true,
            discard_code: false,
        };
        assert!(
            remove_registered_worktree(home.path(), &root, &args.0, &args.1, context_only).is_err(),
            "context consent alone must not delete uncommitted code"
        );
        assert_eq!(
            fs::read_to_string(root.join("code.rs")).unwrap(),
            "uncommitted code"
        );
        assert_eq!(
            fs::read_to_string(root.join("context.md")).unwrap(),
            "unpublished edit"
        );
        let code_only = RemovalConsent {
            discard_context: false,
            discard_code: true,
        };
        assert!(
            remove_registered_worktree(home.path(), &root, &args.0, &args.1, code_only)
                .unwrap_err()
                .contains("ignored files"),
            "code consent alone must not discard unpublished context"
        );
        assert!(root.exists());
        remove_registered_worktree(
            home.path(),
            &root,
            &args.0,
            &args.1,
            RemovalConsent {
                discard_context: true,
                discard_code: true,
            },
        )
        .unwrap();
        assert!(!root.exists());
    }

    /// `create_worktree` writes no status hook into the worktree: Claude's status hooks
    /// run the script CodeSense's host installed, so the worktree carries nothing of it
    /// and `git status` stays clean.
    #[tokio::test]
    async fn create_worktree_writes_no_status_hook() {
        let repo = TempDir::new().unwrap();
        init_repo_with_commit(repo.path());
        let parent = TempDir::new().unwrap();
        let root = parent.path().join("story");
        for _adopt in 0..2 {
            create_worktree(
                repo.path().to_string_lossy().into(),
                "feat/sw-an-49-hook".into(),
                root.to_string_lossy().into(),
            )
            .await
            .unwrap();
            assert!(!root.join(".claude").exists());
        }
        let exclude =
            std::fs::read_to_string(repo.path().join(".git/info/exclude")).unwrap_or_default();
        assert!(!exclude.contains("codesense-notify"), "{exclude}");
        let status = std::process::Command::new(git_bin())
            .args(["status", "--porcelain"])
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(status.status.success());
        assert!(status.stdout.is_empty());
    }

    /// A worktree with a user-authored ignored file trips the removal guard.
    #[cfg(unix)]
    #[tokio::test]
    async fn user_authored_ignored_file_still_trips_removal_guard() {
        let home = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo_with_commit(repo.path());
        let parent = TempDir::new().unwrap();
        let root = parent.path().join("story-user-ignored");
        create_worktree(
            repo.path().to_string_lossy().into(),
            "feat/sw-an-49-user-ignored".into(),
            root.to_string_lossy().into(),
        )
        .await
        .unwrap();
        // A user-ignored file under `.claude/`.
        crate::content_engine::git_exclude::add_entries(&root, &["/notes.md"]).unwrap();
        std::fs::write(root.join("notes.md"), "WIP\n").unwrap();
        crate::install::worktree_index::write_record(
            home.path(),
            &Principal::from(42),
            &wl2_record(&root, "S-USR"),
        )
        .unwrap();
        let err = remove_registered_worktree(
            home.path(),
            &root,
            &repo.path().to_string_lossy(),
            &root.to_string_lossy(),
            RemovalConsent::default(),
        )
        .unwrap_err();
        assert!(err.contains("ignored files"), "{err}");
        assert!(root.exists(), "guard trips; the worktree is left in place");
    }

    #[tokio::test]
    async fn explicit_story_clean_manual_removal_uses_existing_primitive() {
        let home = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo_with_commit(repo.path());
        let parent = TempDir::new().unwrap();
        let root = parent.path().join("story");
        create_worktree(
            repo.path().to_string_lossy().into(),
            "feat/explicit-clean".into(),
            root.to_string_lossy().into(),
        )
        .await
        .unwrap();
        crate::install::worktree_index::write_record(
            home.path(),
            &Principal::from(42),
            &wl2_record(&root, "S-1"),
        )
        .unwrap();
        remove_registered_worktree(
            home.path(),
            &root,
            &repo.path().to_string_lossy(),
            &root.to_string_lossy(),
            RemovalConsent::default(),
        )
        .unwrap();
        assert!(!root.exists());
    }

    fn init_repo_with_commit(dir: &Path) {
        // Mirror the convention from commands/git.rs tests: initial branch
        // is pinned to `master` via init.defaultBranch so behavior is the
        // same regardless of the developer's global git config.
        std::process::Command::new(git_bin())
            .args(["-c", "init.defaultBranch=master", "init"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new(git_bin())
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new(git_bin())
            .args(["config", "user.name", "Test"])
            .current_dir(dir)
            .output()
            .unwrap();
        fs::write(dir.join("README.md"), "# Test").unwrap();
        std::process::Command::new(git_bin())
            .args(["add", "README.md"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new(git_bin())
            .args(["commit", "-m", "initial"])
            .current_dir(dir)
            .output()
            .unwrap();
    }

    #[test]
    fn test_parse_worktree_porcelain_single() {
        let out = "\
worktree /tmp/repo
HEAD abc123
branch refs/heads/master

";
        let parsed = parse_worktree_porcelain(out);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].path, "/tmp/repo");
        assert_eq!(parsed[0].head_sha, "abc123");
        assert_eq!(parsed[0].branch.as_deref(), Some("master"));
    }

    #[test]
    fn test_parse_worktree_porcelain_multiple() {
        let out = "\
worktree /tmp/main
HEAD aaa
branch refs/heads/master

worktree /tmp/main-wt/feat
HEAD bbb
branch refs/heads/feat/story

";
        let parsed = parse_worktree_porcelain(out);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].branch.as_deref(), Some("master"));
        assert_eq!(parsed[1].branch.as_deref(), Some("feat/story"));
    }

    #[test]
    fn test_parse_worktree_porcelain_detached_head() {
        let out = "\
worktree /tmp/main
HEAD abc123
detached

";
        let parsed = parse_worktree_porcelain(out);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].branch, None);
    }

    #[tokio::test]
    async fn test_create_and_list_worktree() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        init_repo_with_commit(&repo);

        let wt_path = tmp
            .path()
            .join("repo-wt-feat")
            .to_string_lossy()
            .into_owned();
        create_worktree(
            repo.to_string_lossy().into_owned(),
            "feat/test-branch".to_string(),
            wt_path.clone(),
        )
        .await
        .unwrap();

        let listed = list_worktrees(repo.to_string_lossy().into_owned())
            .await
            .unwrap();
        // Main worktree + the new one.
        assert_eq!(listed.len(), 2);
        assert!(listed
            .iter()
            .any(|w| w.branch.as_deref() == Some("feat/test-branch")));
    }

    // ── SLW-WL-2 ──────────────────────────────────────────────────────────────
    use crate::domain::SourceRepoSlug;
    use crate::install::worktree_index::{SyncStatus, WorktreeStoryRecord};

    fn wl2_record(code_repo_path: &Path, story_id: &str) -> WorktreeStoryRecord {
        WorktreeStoryRecord::new(
            code_repo_path,
            "alderco",
            SourceRepoSlug::new("alderco", "codesense").unwrap(),
            story_id,
            code_repo_path.join("story.md"),
        )
    }

    // SW-WM-3: a minimal resolver context for the row-enrichment tests. `codesense_home`
    // is the field the lockfile registration reads (it locates the lockfile).
    fn wm3_test_ctx(codesense_home: &Path) -> crate::content_engine::ResolverContext {
        crate::content_engine::ResolverContext {
            context_root: None,
            codesense_home: codesense_home.to_path_buf(),
            user_id: 42,
            // `org_slug` is validated as an `owner/repo` slug when it composes a lockfile
            // path (matches the scaffold's `acme/api` form) — a bare word is rejected.
            org_slug: "alderco/codesense".to_string(),
            team_name: None,
        }
    }

    /// SW-WU-15 (AC-5): a worktree dirty **only** on projected content — paths the
    /// content engine has registered in `.git/info/exclude` (`docs/`,
    /// `project-context.md`) — must read **clean** so a merged, code-clean worktree
    /// stays cleanup-eligible. This is the AN-6 / WG-1 regression: those trees had zero
    /// code diff yet were stranded because re-materialized projected content tripped
    /// the dirty guard. The pin also guards the reverse direction — real untracked
    /// code still reads dirty — so a future guard rewrite can't silently re-block
    /// cleanup or hide real work.
    #[test]
    fn wu15_projected_paths_hidden_by_git_exclude_read_clean() {
        use crate::content_engine::git_exclude;

        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        init_repo_with_commit(&repo);
        // Hermetic: neutralize any global `core.excludesFile` so the `scratch.rs`
        // control below reflects THIS repo's ignore state, not the dev/CI box's.
        std::process::Command::new(git_bin())
            .args(["config", "core.excludesFile", "/dev/null"])
            .current_dir(&repo)
            .output()
            .unwrap();

        // Register through the REAL engine writer with the exact entry forms
        // production emits — dir projection `docs/`, file projection
        // `/project-context.md` (leading-slash-anchored; see
        // story_linker::projection_file_exclude_entry). Driving `add_entries`
        // (not a hand-written string) pins the test to the engine's actual
        // managed-block format, so a registration-format regression is caught here.
        git_exclude::add_entries(&repo, &["docs/", "/project-context.md"]).unwrap();

        // The content engine re-materializes projected content into the tree.
        // Untracked, but excluded → invisible to `git status --porcelain`.
        fs::create_dir(repo.join("docs")).unwrap();
        fs::write(repo.join("docs/guide.md"), "projected").unwrap();
        fs::write(repo.join("project-context.md"), "projected").unwrap();
        assert_eq!(
            worktree_git_dirty_opt(&repo),
            Some(false),
            "projected content hidden by .git/info/exclude must read clean"
        );

        // Control: a real untracked code file is NOT excluded → still dirty.
        fs::write(repo.join("scratch.rs"), "fn main() {}").unwrap();
        assert_eq!(
            worktree_git_dirty_opt(&repo),
            Some(true),
            "real untracked developer work must still read dirty"
        );
    }

    #[tokio::test]
    async fn wl2_managed_row_joins_record_with_git_facts() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        init_repo_with_commit(&repo);
        let wt_path = tmp.path().join("repo-wt").to_string_lossy().into_owned();
        create_worktree(
            repo.to_string_lossy().into_owned(),
            "feat/wl2".to_string(),
            wt_path.clone(),
        )
        .await
        .unwrap();
        let wt = PathBuf::from(&wt_path);

        let mut record = wl2_record(&wt, "SLW-WL-2");
        record.status = SyncStatus::Synced;
        let row = managed_row_from_record(record, true);

        assert_eq!(row.story_id, "SLW-WL-2");
        assert_eq!(row.source_repo, "alderco/codesense");
        assert_eq!(row.sync_status, SyncStatus::Synced);
        // SLW-WL-5: the caller-supplied liveness flag is carried through verbatim.
        assert!(row.live_cli_session, "live flag propagates onto the row");
        // Open-workspace fields carried through verbatim from the record.
        assert_eq!(row.org_slug, "alderco");
        assert_eq!(row.story_source_path, wt.join("story.md").to_string_lossy());
        assert_eq!(row.branch.as_deref(), Some("feat/wl2"));
        assert_eq!(row.dirty, Some(false)); // fresh worktree is clean
                                            // `repoPath` resolves to the MAIN repo (git canonicalizes /var→/private/var
                                            // on macOS, so compare canonical forms).
        let resolved = PathBuf::from(row.repo_path.expect("repo_path should resolve"));
        assert_eq!(
            fs::canonicalize(&resolved).unwrap_or(resolved),
            fs::canonicalize(&repo).unwrap_or(repo),
        );
    }

    #[test]
    fn wl2_managed_row_retains_unreadable_worktree_with_nulls() {
        // A record whose worktree path doesn't exist / isn't a git worktree is
        // STILL listed — the git-derived fields degrade to None, never dropped.
        let bogus = PathBuf::from("/nonexistent/slw-wl2/ghost-worktree");
        let row = managed_row_from_record(wl2_record(&bogus, "SLW-WL-2"), false);

        assert_eq!(row.path, bogus.to_string_lossy());
        assert_eq!(row.story_id, "SLW-WL-2");
        assert_eq!(row.source_repo, "alderco/codesense");
        assert_eq!(row.branch, None);
        assert_eq!(row.dirty, None);
        assert_eq!(row.repo_path, None);
        assert!(
            !row.live_cli_session,
            "dormant flag propagates onto the row"
        );
        // SW-WM-3 (AC-1): no parent repo resolves ⇒ orphaned.
        assert!(row.orphaned, "vanished parent repo ⇒ orphaned");
    }

    /// SW-RT-6d (AC-6): unknown is not empty. While the live sessions are unknown (a
    /// terminal host that cannot be asked may run an agent in any worktree), the panel
    /// row reads live.
    /// SV-CDE-7: the row carries the attribution status of the last launch, from the
    /// record, in the `list_worktrees` shape.
    #[test]
    fn story_rows_carry_the_attribution_status_from_the_record() {
        use crate::attribution::{AttributionState, AttributionStatus, OffReason};
        let tmp = TempDir::new().unwrap();
        let mut record = wl2_record(&tmp.path().join("wt"), "SV-CDE-7");
        assert!(managed_row_from_record(record.clone(), false)
            .attribution
            .is_none());
        record.attribution = Some(AttributionState {
            status: AttributionStatus::Off {
                reason: OffReason::NotesWouldPush,
            },
            git_ai_version: Some("1.7.5".into()),
            checked_at: 1_790_000_000_000,
        });
        let row = WorktreeRow::story(managed_row_from_record(record, false));
        assert_eq!(
            serde_json::to_value(&row).unwrap()["attribution"],
            serde_json::json!({
                "status": {"kind": "off", "reason": "notes_would_push"},
                "gitAiVersion": "1.7.5",
                "checkedAt": 1_790_000_000_000u64,
            })
        );
    }

    #[tokio::test]
    async fn unknown_liveness_row_reads_live() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        init_repo_with_commit(&repo);
        let wt_path = tmp
            .path()
            .join("repo-wt-rt6d")
            .to_string_lossy()
            .into_owned();
        create_worktree(
            repo.to_string_lossy().into_owned(),
            "feat/rt6d".to_string(),
            wt_path.clone(),
        )
        .await
        .unwrap();
        let wt = PathBuf::from(&wt_path);
        let records = vec![wl2_record(&wt, "RT-6d")];

        let rows = enrich_rows(records, &Liveness::Unknown);
        assert!(rows[0].live_cli_session);
    }

    /// SW-XS-17 (AC-2): a clean, not-live, attached worktree last touched 30 days ago
    /// (its store row's `updated_at`, the recency the removed reaper read) survives
    /// both the launch/in-session reconcile and the Worktrees listing: the directory
    /// and its record stay, and its row carries no automatic-removal flag. (The red
    /// run of the first version of this test got `reapEligible: true` for a clock 30
    /// days ahead.)
    #[tokio::test]
    async fn xs17_thirty_day_idle_worktree_is_never_marked_for_removal() {
        use crate::content_engine::lockfile::register_attached_code_repo;
        use crate::content_engine::sync_state_store::{system_time_to_unix_nanos, TargetKey};
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        init_repo_with_commit(&repo);
        let wt_path = tmp.path().join("repo-wt-xs17");
        create_worktree(
            repo.to_string_lossy().into_owned(),
            "feat/xs17-idle".to_string(),
            wt_path.to_string_lossy().into_owned(),
        )
        .await
        .unwrap();
        let ctx = wm3_test_ctx(&home);
        let slug = SourceRepoSlug::new("alderco", "codesense").unwrap();
        register_attached_code_repo(&ctx, &slug, &wt_path).unwrap();
        let principal = Principal::from(ctx.user_id);
        crate::install::worktree_index::write_record(
            &home,
            &principal,
            &wl2_record(&wt_path, "XS-17"),
        )
        .unwrap();

        // Age the target 30 days.
        let store = crate::content_engine::lockfile::open_sync_state_store(&home).unwrap();
        let lock = store
            .read(&TargetKey::from_ctx(&ctx, &wt_path))
            .unwrap()
            .expect("the attached target has a store row");
        let thirty_days_ago = SystemTime::now() - Duration::from_secs(30 * 24 * 60 * 60);
        // `updated_at` only moves forward, so re-seed the row rather than rewind it.
        store.delete_all_for_path(&wt_path).unwrap();
        store
            .upsert_with_updated_at(
                ctx.user_id,
                &slug,
                &lock,
                system_time_to_unix_nanos(thirty_days_ago),
            )
            .unwrap();
        assert!(store.updated_at(&wt_path).unwrap().unwrap() <= thirty_days_ago);

        crate::commands::content_engine::reconcile_worktrees_with(&home, ctx.user_id);
        let cache = crate::install::frontmatter_cache::FrontmatterCache::build_from_scan(&home);
        let listing = list_managed_worktrees_core(
            &ctx,
            &[slug],
            &home,
            ctx.user_id,
            &cache,
            &Liveness::Known(Vec::new()),
        )
        .unwrap();

        assert!(wt_path.exists(), "an idle worktree stays on disk");
        assert_eq!(listing.reaped, 0);
        let records =
            crate::install::worktree_index::iter_story_records(&home, &principal).unwrap();
        assert!(
            records.iter().any(|r| r.code_repo_path == wt_path),
            "its record stays"
        );
        let row = listing
            .rows
            .iter()
            .find(|r| r.story_id.as_deref() == Some("XS-17"))
            .expect("the idle worktree is listed");
        assert_eq!(row.dirty, Some(false), "clean");
        assert!(!row.live_cli_session, "not live");
        let json = serde_json::to_value(row).unwrap();
        assert!(
            json.get("reapEligible").is_none(),
            "no automatic-removal flag on a worktree row: {json}"
        );
    }

    /// SW-XS-17 (AC-2): the only path that deletes a worktree from disk is the
    /// explicit `remove_worktree` command. Every production caller of the prune
    /// chain (`remove_worktree` → `prune_worktree_locked` → `remove_registered_worktree`
    /// → `remove_worktree_prune_core`) is the link above it; nothing else in the
    /// crate reaches it.
    #[test]
    fn worktree_prune_is_reachable_only_from_remove_worktree() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let allowed: [(&str, &[&str]); 3] = [
            ("prune_worktree_locked", &["remove_worktree"]),
            ("remove_registered_worktree", &["prune_worktree_locked"]),
            (
                "remove_worktree_prune_core",
                // `remove_worktree_prune` is `#[cfg(test)]` (asserted below).
                &["remove_registered_worktree", "remove_worktree_prune"],
            ),
        ];
        let this = include_str!("worktree.rs");
        assert!(
            this.contains("#[cfg(test)]\npub(crate) async fn remove_worktree_prune("),
            "remove_worktree_prune stays test-only"
        );
        for (primitive, callers_ok) in allowed {
            let call = format!("{primitive}(");
            let mut seen = 0;
            for entry in walkdir::WalkDir::new(&src)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|e| e.path().extension().is_some_and(|x| x == "rs"))
            {
                let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
                let prod = crate::test_support::source_guard::production_text(&text);
                let prod = prod.as_str();
                for (at, _) in prod.match_indices(&call) {
                    let before = &prod[..at];
                    if before.ends_with("fn ")
                        || !before.ends_with(|c: char| !c.is_alphanumeric() && c != '_')
                    {
                        continue; // the definition, or a longer identifier
                    }
                    let caller = enclosing_fn(prod, at);
                    seen += 1;
                    assert!(
                        caller.is_some_and(|c| callers_ok.contains(&c)),
                        "`{primitive}` is reached from {:?} in {}",
                        caller,
                        entry.path().display()
                    );
                }
            }
            assert!(
                seen > 0,
                "`{primitive}` is still on the remove_worktree path"
            );
        }
    }

    /// The name of the `fn` whose body contains byte offset `at` (the nearest `fn`
    /// item before it).
    fn enclosing_fn(src: &str, at: usize) -> Option<&str> {
        let fn_at = src[..at].rfind("fn ")?;
        let name = &src[fn_at + 3..];
        Some(&name[..name.find(['(', '<'])?])
    }

    /// SW-WM-3 (AC-1/AC-4): orphaned fires on a vanished parent repo OR a deleted backing
    /// story file — and NOT on a healthy worktree whose story file is present. Local,
    /// non-network signals only.
    #[tokio::test]
    async fn wm3_orphaned_when_parent_or_story_gone() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        init_repo_with_commit(&repo);
        let wt_path = tmp
            .path()
            .join("repo-wt-orphan")
            .to_string_lossy()
            .into_owned();
        create_worktree(
            repo.to_string_lossy().into_owned(),
            "feat/wm3-orphan".to_string(),
            wt_path.clone(),
        )
        .await
        .unwrap();
        let wt = PathBuf::from(&wt_path);
        let slug = SourceRepoSlug::new("alderco", "codesense").unwrap();

        // Healthy: parent resolves + backing story file exists ⇒ not orphaned.
        let story = wt.join("story.md");
        fs::write(&story, "# story").unwrap();
        let healthy =
            WorktreeStoryRecord::new(&wt, "alderco", slug.clone(), "WM-3", story.clone());
        let row = managed_row_from_record(healthy, false);
        assert!(!row.orphaned, "parent + story present ⇒ not orphaned");

        // Story file deleted ⇒ orphaned (registry points at a dead story).
        fs::remove_file(&story).unwrap();
        let missing_story =
            WorktreeStoryRecord::new(&wt, "alderco", slug.clone(), "WM-3", story.clone());
        let row_story_gone = managed_row_from_record(missing_story, false);
        assert!(row_story_gone.orphaned, "deleted story file ⇒ orphaned");

        // Parent repo gone (bogus path, no git link) ⇒ orphaned.
        let bogus = PathBuf::from("/nonexistent/wm3/ghost");
        let row_parent_gone = managed_row_from_record(wl2_record(&bogus, "WM-3"), false);
        assert!(row_parent_gone.orphaned, "vanished parent ⇒ orphaned");
    }

    #[tokio::test]
    async fn test_remove_worktree_keeps_branch() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        init_repo_with_commit(&repo);

        let wt_path = tmp
            .path()
            .join("repo-wt-keep")
            .to_string_lossy()
            .into_owned();
        create_worktree(
            repo.to_string_lossy().into_owned(),
            "feat/keep-branch".to_string(),
            wt_path.clone(),
        )
        .await
        .unwrap();

        remove_worktree_prune(repo.to_string_lossy().into_owned(), wt_path, Some(true))
            .await
            .unwrap();

        let listed = list_worktrees(repo.to_string_lossy().into_owned())
            .await
            .unwrap();
        assert_eq!(listed.len(), 1); // main only

        // Branch still exists per Task 2.2 contract.
        let branches = std::process::Command::new(git_bin())
            .args(["branch", "--list"])
            .current_dir(&repo)
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&branches.stdout);
        assert!(out.contains("feat/keep-branch"));
    }

    #[test]
    fn test_ensure_managed_removal_target_gate() {
        // SW-WM-1 (AC-4): the guard admits a codesense-managed path and refuses anything
        // else — the defense-in-depth that keeps a force-remove off a user's own checkout.
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        // A path under `{home}/users` is managed (pure prefix, no registry needed).
        let managed = home.join("users").join("1").join("wt").join("feat-x");
        assert!(ensure_managed_removal_target(&managed, home).is_ok());
        // A path outside the managed tree with no registry entry is refused.
        let outside = tmp.path().join("some-random-checkout");
        let err = ensure_managed_removal_target(&outside, home).unwrap_err();
        assert!(err.contains("doesn't manage"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn test_remove_worktree_prune_absent_force_does_not_force() {
        // SW-WM-1 (AC-4): an absent `force` must NOT escalate to `--force`. Proven with a
        // dirty worktree — git refuses to remove it without `--force`, so `None` (the
        // fail-safe default) errs, while an explicit `Some(true)` still removes it.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        init_repo_with_commit(&repo);

        let wt_path = tmp
            .path()
            .join("repo-wt-dirty")
            .to_string_lossy()
            .into_owned();
        create_worktree(
            repo.to_string_lossy().into_owned(),
            "feat/dirty".to_string(),
            wt_path.clone(),
        )
        .await
        .unwrap();
        // Dirty the worktree with an untracked file so git will refuse a non-forced remove.
        fs::write(PathBuf::from(&wt_path).join("scratch.txt"), "wip").unwrap();

        let refused =
            remove_worktree_prune(repo.to_string_lossy().into_owned(), wt_path.clone(), None).await;
        assert!(
            refused.is_err(),
            "absent force must fail safe (not force-remove) a dirty worktree"
        );

        // Explicit force still removes it — the escape hatch behind "Remove anyway".
        remove_worktree_prune(repo.to_string_lossy().into_owned(), wt_path, Some(true))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_create_worktree_idempotent_returns_existing_path() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        init_repo_with_commit(&repo);

        let wt_path = tmp
            .path()
            .join("repo-wt-dup")
            .to_string_lossy()
            .into_owned();
        create_worktree(
            repo.to_string_lossy().into_owned(),
            "feat/dup".to_string(),
            wt_path.clone(),
        )
        .await
        .unwrap();

        // Second create with the same branch returns the existing worktree path
        // (idempotent — does not error). Git may canonicalize the path on macOS
        // (/var → /private/var), so compare canonical forms.
        let result = create_worktree(
            repo.to_string_lossy().into_owned(),
            "feat/dup".to_string(),
            wt_path.clone(),
        )
        .await;
        assert!(result.is_ok());
        let returned = PathBuf::from(result.unwrap());
        let expected = fs::canonicalize(&wt_path).unwrap_or_else(|_| PathBuf::from(&wt_path));
        assert_eq!(fs::canonicalize(&returned).unwrap_or(returned), expected);
    }

    #[tokio::test]
    async fn test_create_worktree_reuses_orphaned_branch() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        init_repo_with_commit(&repo);

        // Create worktree, then remove it — leaves the branch behind.
        let wt_path_1 = tmp
            .path()
            .join("repo-wt-orphan")
            .to_string_lossy()
            .into_owned();
        create_worktree(
            repo.to_string_lossy().into_owned(),
            "feat/orphan".to_string(),
            wt_path_1.clone(),
        )
        .await
        .unwrap();
        remove_worktree_prune(repo.to_string_lossy().into_owned(), wt_path_1, Some(true))
            .await
            .unwrap();

        // Branch still exists but no worktree uses it — create_worktree
        // should reuse the existing branch (no -b).
        let wt_path_2 = tmp
            .path()
            .join("repo-wt-orphan-retry")
            .to_string_lossy()
            .into_owned();
        let result = create_worktree(
            repo.to_string_lossy().into_owned(),
            "feat/orphan".to_string(),
            wt_path_2.clone(),
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), wt_path_2);

        // Confirm the worktree is listed with the correct branch.
        let listed = list_worktrees(repo.to_string_lossy().into_owned())
            .await
            .unwrap();
        assert!(listed
            .iter()
            .any(|w| w.branch.as_deref() == Some("feat/orphan")));
    }

    // ----- Streaming subprocess runner (Task 3 / Task 7.3) -----

    #[derive(Default, Clone)]
    struct RecordingEventSink(Arc<Mutex<Vec<(String, serde_json::Value)>>>);

    impl RecordingEventSink {
        fn new() -> Self {
            Self::default()
        }
        fn events(&self) -> Vec<(String, serde_json::Value)> {
            self.0.lock().unwrap().clone()
        }
    }

    impl EventSink for RecordingEventSink {
        fn emit(&self, channel: &str, payload: serde_json::Value) {
            self.0.lock().unwrap().push((channel.to_string(), payload));
        }
    }

    fn cfg_with_setup(setup: Vec<String>) -> WorktreeConfig {
        WorktreeConfig {
            version: 1,
            setup,
            cleanup: vec![],
            env: HashMap::new(),
            branch_prefix: DEFAULT_BRANCH_PREFIX.to_string(),
            default_start_point: None,
            worktree_dir: DEFAULT_WORKTREE_DIR.to_string(),
            timeout_seconds: 60,
            scope: None,
        }
    }

    /// Collect all emitted `stdout` stream lines from a recording sink, in order.
    fn stdout_lines(sink: &RecordingEventSink) -> Vec<String> {
        sink.events()
            .iter()
            .filter(|(_, v)| v.get("stream").and_then(|s| s.as_str()) == Some("stdout"))
            .filter_map(|(_, v)| v.get("line").and_then(|l| l.as_str()).map(String::from))
            .collect()
    }

    /// Run a single setup command through the executor and return (result, sink).
    async fn run_one_setup(tmp: &TempDir, command: &str, run_id: &str) -> RecordingEventSink {
        let sink = RecordingEventSink::new();
        let registry = WorktreeRunRegistry::new();
        let config = cfg_with_setup(vec![command.to_string()]);
        let result =
            run_worktree_setup_inner(&sink, &registry, tmp.path(), tmp.path(), &config, run_id)
                .await
                .unwrap();
        assert_eq!(result.results.len(), 1);
        assert!(
            result.results[0].passed,
            "command failed: {command:?} stderr={:?}",
            result.results[0].stderr
        );
        assert_eq!(result.exit_code, 0);
        sink
    }

    /// SW-WT-12 (S2): collect emitted step-lifecycle events (the checklist feed)
    /// as `(event, step_index, passed, exit_code)`, in emission order.
    fn step_events(sink: &RecordingEventSink) -> Vec<(String, u64, Option<bool>, Option<i64>)> {
        sink.events()
            .iter()
            .filter_map(|(_, v)| {
                let event = v.get("event").and_then(|e| e.as_str())?.to_string();
                let step_index = v.get("step_index").and_then(|s| s.as_u64())?;
                let passed = v.get("passed").and_then(|p| p.as_bool());
                let exit_code = v.get("exit_code").and_then(|c| c.as_i64());
                Some((event, step_index, passed, exit_code))
            })
            .collect()
    }

    #[tokio::test]
    async fn swwt12_emits_step_started_and_finished_per_command() {
        // S2: each configured command emits step-started (running) then
        // step-finished (✓/✗) so the FE can render a live per-step checklist.
        // abort-on-failure stops the run, so a command after a failure emits no
        // step events (its checklist row stays queued/skipped on the FE).
        let tmp = TempDir::new().unwrap();
        let sink = RecordingEventSink::new();
        let registry = WorktreeRunRegistry::new();
        let config = cfg_with_setup(vec![
            "true".to_string(),
            "false".to_string(),
            "echo unreached".to_string(),
        ]);
        run_worktree_setup_inner(
            &sink,
            &registry,
            tmp.path(),
            tmp.path(),
            &config,
            "run-steps",
        )
        .await
        .unwrap();

        assert_eq!(
            step_events(&sink),
            vec![
                ("step-started".to_string(), 0, None, None),
                ("step-finished".to_string(), 0, Some(true), Some(0)),
                ("step-started".to_string(), 1, None, None),
                ("step-finished".to_string(), 1, Some(false), Some(1)),
            ],
            "each run command brackets with started+finished; the post-failure command is not started"
        );

        // The step events ride the SAME per-run channel as the line stream and
        // carry the command text on start (checklist rows are labelled from it).
        let started: Vec<String> = sink
            .events()
            .into_iter()
            .filter(|(_, v)| v.get("event").and_then(|e| e.as_str()) == Some("step-started"))
            .filter_map(|(_, v)| v.get("command").and_then(|c| c.as_str()).map(String::from))
            .collect();
        assert_eq!(started, vec!["true".to_string(), "false".to_string()]);
    }

    #[tokio::test]
    async fn test_shell_exec_pipe_runs() {
        // AC-1: `|` works now that commands run via `/bin/sh -c`.
        let tmp = TempDir::new().unwrap();
        let sink = run_one_setup(&tmp, "echo hello | tr a-z A-Z", "run-pipe").await;
        assert!(
            stdout_lines(&sink).iter().any(|l| l == "HELLO"),
            "expected piped output HELLO, got: {:?}",
            stdout_lines(&sink)
        );
    }

    #[tokio::test]
    async fn test_shell_exec_and_chain_runs() {
        // AC-1: `&&` chains run in one step.
        let tmp = TempDir::new().unwrap();
        let sink = run_one_setup(&tmp, "true && echo chained", "run-and").await;
        assert!(
            stdout_lines(&sink).iter().any(|l| l == "chained"),
            "expected `&&` chain to run, got: {:?}",
            stdout_lines(&sink)
        );
    }

    #[tokio::test]
    async fn test_shell_exec_var_expansion_runs() {
        // AC-1/AC-3: `$VAR` expands against the layered env (allowlist + the
        // user's `env` map), proving the shell sees the configured env.
        let tmp = TempDir::new().unwrap();
        let sink = RecordingEventSink::new();
        let registry = WorktreeRunRegistry::new();
        let mut config = cfg_with_setup(vec!["echo $WT_GREETING".to_string()]);
        config
            .env
            .insert("WT_GREETING".to_string(), "hi-there".to_string());
        let result =
            run_worktree_setup_inner(&sink, &registry, tmp.path(), tmp.path(), &config, "run-var")
                .await
                .unwrap();
        assert!(result.results[0].passed);
        assert!(
            stdout_lines(&sink).iter().any(|l| l == "hi-there"),
            "expected $WT_GREETING to expand to hi-there, got: {:?}",
            stdout_lines(&sink)
        );
    }

    #[tokio::test]
    async fn test_setup_runs_with_login_shell_path() {
        // Setup PATH fix: a Finder-launched app inherits the minimal launchd
        // PATH, so setup steps couldn't find nvm/Homebrew tools (npm → exit
        // 127). The executor now resolves the login-shell PATH and hands it to
        // the subprocess. When the probe resolves, the PATH the setup command
        // sees must equal the resolved login PATH (not the process PATH).
        let Some(login_path) = crate::commands::deps::cached_login_shell_path() else {
            // No login shell in this environment (e.g. minimal CI container) —
            // the fallback path is covered by the existing $VAR/env tests.
            return;
        };
        let tmp = TempDir::new().unwrap();
        let sink = run_one_setup(&tmp, "echo \"$PATH\"", "run-login-path").await;
        assert!(
            stdout_lines(&sink).iter().any(|l| l == &login_path),
            "expected setup PATH to equal login-shell PATH {login_path:?}, got: {:?}",
            stdout_lines(&sink)
        );
    }

    #[tokio::test]
    async fn test_shell_exec_script_invocation_runs() {
        // AC-1: invoking a script directly works — no committed wrapper needed.
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("s.sh"), "#!/bin/sh\necho from-script\n").unwrap();
        let sink = run_one_setup(&tmp, "sh s.sh", "run-script").await;
        assert!(
            stdout_lines(&sink).iter().any(|l| l == "from-script"),
            "expected script output, got: {:?}",
            stdout_lines(&sink)
        );
    }

    #[tokio::test]
    async fn test_setup_command_sees_codesense_source_root() {
        // SW-WT-6 (AC-5): the executor injects `$CODESENSE_SOURCE_ROOT` (the
        // source/main worktree) so a setup step can copy machine-local,
        // gitignored files the context repo must not carry. `run_one_setup`
        // passes `tmp` as both worktree and source, so the var resolves to it.
        let tmp = TempDir::new().unwrap();
        let sink = run_one_setup(&tmp, "echo \"$CODESENSE_SOURCE_ROOT\"", "run-source-root").await;
        let expected = tmp.path().display().to_string();
        assert!(
            stdout_lines(&sink).iter().any(|l| l == &expected),
            "expected $CODESENSE_SOURCE_ROOT to expand to {expected:?}, got: {:?}",
            stdout_lines(&sink)
        );
    }

    #[tokio::test]
    async fn test_stream_successful_command_emits_stdout_lines() {
        let tmp = TempDir::new().unwrap();
        let sink = RecordingEventSink::new();
        let registry = WorktreeRunRegistry::new();
        let config = cfg_with_setup(vec!["echo hello".to_string(), "echo world".to_string()]);
        let result = run_worktree_setup_inner(
            &sink,
            &registry,
            tmp.path(),
            tmp.path(),
            &config,
            "run-success",
        )
        .await
        .unwrap();

        assert_eq!(result.exit_code, 0);
        assert_eq!(result.results.len(), 2);
        assert!(result.results.iter().all(|r| r.passed));
        // Every stdout line from the subprocesses landed on the channel.
        let events = sink.events();
        assert!(events
            .iter()
            .all(|(ch, _)| ch == "worktree-run-run-success"));
        let out = stdout_lines(&sink);
        assert!(out.iter().any(|l| l == "hello"));
        assert!(out.iter().any(|l| l == "world"));
        // phase is always "setup" for run_worktree_setup_inner
        assert!(events
            .iter()
            .filter(|(_, v)| v.get("stream").and_then(|s| s.as_str()) == Some("stdout"))
            .all(|(_, v)| v.get("phase").and_then(|s| s.as_str()) == Some("setup")));
        // step_index increments per-command within the phase.
        let indices: Vec<u32> = result.results.iter().map(|r| r.step_index).collect();
        assert_eq!(indices, vec![0, 1]);
    }

    #[tokio::test]
    async fn test_setup_aborts_on_first_failure() {
        let tmp = TempDir::new().unwrap();
        let sink = RecordingEventSink::new();
        let registry = WorktreeRunRegistry::new();
        let config = cfg_with_setup(vec!["false".to_string(), "echo should-not-run".to_string()]);
        let result = run_worktree_setup_inner(
            &sink,
            &registry,
            tmp.path(),
            tmp.path(),
            &config,
            "run-abort",
        )
        .await
        .unwrap();

        assert_eq!(result.results.len(), 2);
        assert!(!result.results[0].passed);
        assert!(!result.results[1].passed);
        assert!(result.results[1].stderr.contains("skipped"));
        assert!(result.results[1].exit_code.is_none());
    }

    #[tokio::test]
    async fn test_stream_carriage_return_is_stripped() {
        let tmp = TempDir::new().unwrap();
        let sink = RecordingEventSink::new();
        let registry = WorktreeRunRegistry::new();
        // Write a fixture file containing real CR bytes (what npm ci and
        // cargo test emit as progress-bar updates). Reading it via `cat`
        // exercises the stream's `\r` stripping on actual control chars.
        fs::write(tmp.path().join("cr_fixture"), b"a\rb\rc\n").unwrap();
        let config = cfg_with_setup(vec!["cat cr_fixture".to_string()]);
        let result =
            run_worktree_setup_inner(&sink, &registry, tmp.path(), tmp.path(), &config, "run-cr")
                .await
                .unwrap();
        assert_eq!(result.exit_code, 0);
        // Three stdout events, one per \r-separated segment — not one
        // single "a\rb\rc" blob.
        let lines = stdout_lines(&sink);
        assert!(
            lines.iter().any(|l| l == "a"),
            "expected 'a' in stdout lines, got: {:?}",
            lines
        );
        assert!(
            lines.iter().any(|l| l == "b"),
            "expected 'b' in stdout lines, got: {:?}",
            lines
        );
        assert!(
            lines.iter().any(|l| l == "c"),
            "expected 'c' in stdout lines, got: {:?}",
            lines
        );
        // Assert NO line contains an embedded \r (meaning the split worked).
        assert!(
            lines.iter().all(|l| !l.contains('\r')),
            "no emitted line should contain a raw \\r, got: {:?}",
            lines
        );
    }

    #[tokio::test]
    async fn test_stream_timeout_kills_subprocess() {
        let tmp = TempDir::new().unwrap();
        let sink = RecordingEventSink::new();
        let registry = WorktreeRunRegistry::new();
        // `sleep` is a grandchild of our process (child of `/bin/sh`). It only
        // touches the sentinel AFTER sleeping — so if the timeout kill reaped
        // the shell alone and orphaned the sleep, the sentinel would still
        // appear ~3s in. We assert it never does (AC-5: kill the whole tree).
        let mut config = cfg_with_setup(vec!["sleep 3 && touch survived".to_string()]);
        config.timeout_seconds = 1; // force a timeout
        let start = std::time::Instant::now();
        let result = run_worktree_setup_inner(
            &sink,
            &registry,
            tmp.path(),
            tmp.path(),
            &config,
            "run-timeout",
        )
        .await
        .unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(5),
            "expected subprocess to be killed quickly, took {:?}",
            elapsed
        );
        assert_eq!(result.results.len(), 1);
        assert!(!result.results[0].passed);
        assert_eq!(result.results[0].exit_code, Some(-2));
        let events = sink.events();
        let has_timeout = events
            .iter()
            .any(|(_, v)| v.get("stream").and_then(|s| s.as_str()) == Some("timeout"));
        assert!(has_timeout, "expected a 'timeout' stream event");
        // Wait past the grandchild's sleep, then confirm it was killed too.
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(
            !tmp.path().join("survived").exists(),
            "grandchild process survived the timeout — process group was not killed"
        );
    }

    #[tokio::test]
    async fn test_concurrent_runs_are_isolated_by_run_id() {
        let tmp = TempDir::new().unwrap();
        let sink_a = RecordingEventSink::new();
        let sink_b = RecordingEventSink::new();
        let registry = Arc::new(WorktreeRunRegistry::new());
        let config_a = cfg_with_setup(vec!["echo alpha".to_string()]);
        let config_b = cfg_with_setup(vec!["echo beta".to_string()]);
        let reg_a = registry.clone();
        let reg_b = registry.clone();
        let path_a = tmp.path().to_path_buf();
        let path_b = tmp.path().to_path_buf();
        let sa = sink_a.clone();
        let sb = sink_b.clone();
        let a = tokio::spawn(async move {
            run_worktree_setup_inner(&sa, reg_a.as_ref(), &path_a, &path_a, &config_a, "run-a")
                .await
                .unwrap()
        });
        let b = tokio::spawn(async move {
            run_worktree_setup_inner(&sb, reg_b.as_ref(), &path_b, &path_b, &config_b, "run-b")
                .await
                .unwrap()
        });
        let (res_a, res_b) = tokio::join!(a, b);
        res_a.unwrap();
        res_b.unwrap();

        // Each sink only sees its own channel.
        let events_a = sink_a.events();
        let events_b = sink_b.events();
        assert!(events_a.iter().all(|(ch, _)| ch == "worktree-run-run-a"));
        assert!(events_b.iter().all(|(ch, _)| ch == "worktree-run-run-b"));

        // And the content differs — cross-contamination would show up as
        // matching line text.
        let text_a: Vec<_> = events_a
            .iter()
            .filter_map(|(_, v)| v.get("line").and_then(|l| l.as_str()).map(String::from))
            .collect();
        let text_b: Vec<_> = events_b
            .iter()
            .filter_map(|(_, v)| v.get("line").and_then(|l| l.as_str()).map(String::from))
            .collect();
        assert!(text_a.iter().any(|l| l == "alpha"));
        assert!(text_b.iter().any(|l| l == "beta"));
        assert!(!text_a.iter().any(|l| l == "beta"));
        assert!(!text_b.iter().any(|l| l == "alpha"));
    }

    #[tokio::test]
    async fn test_cancellation_resolves_run_quickly() {
        let tmp = TempDir::new().unwrap();
        let sink = RecordingEventSink::new();
        let registry = Arc::new(WorktreeRunRegistry::new());
        // Grandchild (`sleep`, child of `/bin/sh`) touches the sentinel only
        // after sleeping. Cancel fires at 100ms; if cancel reaped the shell
        // alone, the orphaned sleep would still create the sentinel ~3s later.
        // Assert it never does (AC-5: cancel kills the whole tree).
        let config = cfg_with_setup(vec!["sleep 3 && touch survived".to_string()]);
        let reg_clone = registry.clone();

        // Fire the cancel after a short delay.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Some(sender) = reg_clone.take("run-cancel") {
                let _ = sender.send(());
            }
        });

        let start = std::time::Instant::now();
        let result = run_worktree_setup_inner(
            &sink,
            registry.as_ref(),
            tmp.path(),
            tmp.path(),
            &config,
            "run-cancel",
        )
        .await
        .unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(5),
            "expected cancel to resolve quickly, took {:?}",
            elapsed
        );
        assert_eq!(result.results.len(), 1);
        assert_eq!(result.results[0].exit_code, Some(-1));
        let events = sink.events();
        assert!(events
            .iter()
            .any(|(_, v)| v.get("stream").and_then(|s| s.as_str()) == Some("cancel")));
        // Wait past the grandchild's sleep, then confirm it was killed too.
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(
            !tmp.path().join("survived").exists(),
            "grandchild process survived cancel — process group was not killed"
        );
    }

    #[tokio::test]
    async fn test_registry_guard_identity_aware_no_clobber() {
        // Simulate React 18 strict-mode double-invoke: two concurrent
        // runs register under the same run_id. The first insert returns
        // seq=N; the second insert returns seq=N+1 and drops the first
        // sender. When the first run's guard eventually drops, it must
        // NOT remove the entry (because the entry now belongs to the
        // second run's seq=N+1). If it did, the second run's sender
        // would be dropped too, making both runs cancel each other —
        // exactly the bug we're guarding against.
        let registry = WorktreeRunRegistry::new();
        let run_id = "strict-mode-run";

        let (tx1, _rx1) = oneshot::channel::<()>();
        let seq1 = registry.insert(run_id.to_string(), tx1);
        let guard1 = RegistryGuard {
            registry: &registry,
            run_id: run_id.to_string(),
            seq: seq1,
        };

        let (tx2, mut rx2) = oneshot::channel::<()>();
        let seq2 = registry.insert(run_id.to_string(), tx2);
        assert_ne!(seq1, seq2);
        let _guard2 = RegistryGuard {
            registry: &registry,
            run_id: run_id.to_string(),
            seq: seq2,
        };

        // Drop the FIRST guard. It should NOT remove the entry because
        // the entry in the map now belongs to seq2, not seq1.
        drop(guard1);

        // Verify the second run's receiver is still Pending (the second
        // sender is still alive in the registry). If the first guard
        // incorrectly removed the entry, tx2 would have dropped and
        // rx2 would immediately resolve with RecvError.
        match rx2.try_recv() {
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                // Expected — sender still alive, no message sent yet.
            }
            other => panic!(
                "expected rx2 to be Empty (sender still alive), got {:?}",
                other
            ),
        }

        // And the registry still has an entry for run_id with seq2.
        assert!(registry.take(run_id).is_some());
    }

    #[tokio::test]
    async fn test_registry_removes_run_id_on_exit() {
        let tmp = TempDir::new().unwrap();
        let sink = RecordingEventSink::new();
        let registry = WorktreeRunRegistry::new();
        let config = cfg_with_setup(vec!["echo ok".to_string()]);
        run_worktree_setup_inner(
            &sink,
            &registry,
            tmp.path(),
            tmp.path(),
            &config,
            "run-guard",
        )
        .await
        .unwrap();
        // Guard should have removed the run_id on exit.
        assert!(registry.take("run-guard").is_none());
    }

    // ── ensure_parent_dir tests ─────────────────────────────────────────

    #[test]
    fn ensure_parent_dir_creates_parent_unconditionally() {
        let tmp = TempDir::new().unwrap();
        let deep = tmp.path().join("a/b/c/worktree");
        ensure_parent_dir(&deep).unwrap();
        assert!(tmp.path().join("a/b/c").is_dir());
    }

    #[test]
    fn ensure_parent_dir_idempotent_when_parent_exists() {
        let tmp = TempDir::new().unwrap();
        let parent = tmp.path().join("existing");
        fs::create_dir_all(&parent).unwrap();
        let wt = parent.join("worktree");
        // Should succeed without error even though parent already exists
        ensure_parent_dir(&wt).unwrap();
        assert!(parent.is_dir());
    }

    fn prepare_review_refs(repo: &Path, refspec: Option<&str>) {
        let mut command = Command::new("git");
        command.args(["fetch", "origin"]);
        if let Some(refspec) = refspec {
            command.arg(refspec);
        }
        let result = command.current_dir(repo).output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    // ── fetch_and_create_worktree tests ─────────────────────────────────

    /// Helper: init a bare git repo and a clone, so fetch tests have a remote.
    fn setup_repo_with_remote(tmp: &Path) -> (PathBuf, PathBuf) {
        let bare = tmp.join("bare.git");
        let clone = tmp.join("clone");
        // Init bare repo with explicit main branch
        Command::new("git")
            .args(["init", "--bare", "--initial-branch=main"])
            .arg(&bare)
            .output()
            .unwrap();
        // Clone it
        Command::new("git")
            .args(["clone"])
            .arg(&bare)
            .arg(&clone)
            .output()
            .unwrap();
        // Configure git user for commits
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&clone)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(&clone)
            .output()
            .unwrap();
        // Create an initial commit so main exists
        fs::write(clone.join("README.md"), "init").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&clone)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&clone)
            .output()
            .unwrap();
        Command::new("git")
            .args(["push", "origin", "main"])
            .current_dir(&clone)
            .output()
            .unwrap();
        (bare, clone)
    }

    #[tokio::test]
    async fn fetch_and_create_worktree_from_main_when_no_remote_branch() {
        let tmp = TempDir::new().unwrap();
        let (_bare, clone) = setup_repo_with_remote(tmp.path());
        let wt_path = tmp.path().join("wt-new");

        let result = fetch_and_create_worktree(
            clone.to_string_lossy().into_owned(),
            "feat/new-branch".to_string(),
            wt_path.to_string_lossy().into_owned(),
            Some("main".to_string()),
            None,
        )
        .await
        .unwrap();

        assert!(!result.from_remote);
        assert!(wt_path.is_dir());
    }

    #[tokio::test]
    async fn fetch_and_create_worktree_tracks_remote_when_branch_exists() {
        let tmp = TempDir::new().unwrap();
        let (bare, clone) = setup_repo_with_remote(tmp.path());

        // Create a second clone to push the feature branch from — this way
        // the original clone never has a local `feat/existing` branch.
        let pusher = tmp.path().join("pusher");
        Command::new("git")
            .args(["clone"])
            .arg(&bare)
            .arg(&pusher)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&pusher)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(&pusher)
            .output()
            .unwrap();
        Command::new("git")
            .args(["checkout", "-b", "feat/existing"])
            .current_dir(&pusher)
            .output()
            .unwrap();
        fs::write(pusher.join("feature.txt"), "feature work").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&pusher)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "feature work"])
            .current_dir(&pusher)
            .output()
            .unwrap();
        Command::new("git")
            .args(["push", "origin", "feat/existing"])
            .current_dir(&pusher)
            .output()
            .unwrap();

        prepare_review_refs(&clone, None);
        let wt_path = tmp.path().join("wt-existing");
        let result = fetch_and_create_worktree(
            clone.to_string_lossy().into_owned(),
            "feat/existing".to_string(),
            wt_path.to_string_lossy().into_owned(),
            Some("main".to_string()),
            None,
        )
        .await
        .unwrap();

        assert!(result.from_remote);
        assert!(wt_path.is_dir());
        // The worktree should have the feature file from the remote branch
        assert!(wt_path.join("feature.txt").exists());
    }

    /// An attached in-place user clone with the story branch checked out as
    /// its HEAD must stay the user's. `fetch_and_create_worktree` adopts the
    /// branch at the primary clone (returns the clone's path), and
    /// `paths_equal` against the derived worktree is false — exactly the
    /// signal the step-2 wrapper in `ensure_story_worktree` relies on to
    /// refuse rather than run setup / `write_record` / spawn into the primary
    /// checkout.
    #[tokio::test]
    async fn fetch_and_create_worktree_adopts_primary_checkout_leaves_derived_path_distinct() {
        let tmp = TempDir::new().unwrap();
        let (_bare, clone) = setup_repo_with_remote(tmp.path());
        // The user's primary clone has the story branch checked out as HEAD.
        Command::new("git")
            .args(["switch", "-c", "feat/story"])
            .current_dir(&clone)
            .output()
            .unwrap();
        let derived_wt = tmp.path().join("codesense-managed-wt");

        let result = fetch_and_create_worktree(
            clone.to_string_lossy().into_owned(),
            "feat/story".to_string(),
            derived_wt.to_string_lossy().into_owned(),
            Some("main".to_string()),
            None,
        )
        .await
        .unwrap();

        // Adopted at the primary clone, NOT the derived path — the step-2 guard
        // refuses on this inequality so the user's checkout is never written to.
        assert!(
            paths_equal(&result.worktree_path, &clone.to_string_lossy()),
            "adopts primary checkout, got {}",
            result.worktree_path
        );
        assert!(
            !paths_equal(&result.worktree_path, &derived_wt.to_string_lossy()),
            "returned path must differ from the derived worktree so the derived-path guard refuses"
        );
        assert!(
            !derived_wt.exists(),
            "no derived worktree directory should have been created"
        );
    }

    // ── agent launch checkout (existing / new branch) ───────────────────

    fn git_out(repo: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn commit_file(repo: &Path, name: &str, body: &str) {
        fs::write(repo.join(name), body).unwrap();
        git_out(repo, &["add", name]);
        git_out(repo, &["commit", "-m", name]);
    }

    /// A clone whose local `main` is one commit ahead of `origin/main`, while the
    /// remote itself has moved on: a fetch would change `origin/main`, and a branch
    /// started from anything but the LOCAL default would miss `local.txt`.
    fn clone_with_diverged_remote(tmp: &Path) -> PathBuf {
        let (bare, clone) = setup_repo_with_remote(tmp);
        // Record the remote's default branch locally (no network: an explicit name).
        git_out(&clone, &["remote", "set-head", "origin", "main"]);
        commit_file(&clone, "local.txt", "unpushed local work");
        let other = tmp.join("other");
        Command::new("git")
            .arg("clone")
            .arg(&bare)
            .arg(&other)
            .output()
            .unwrap();
        git_out(&other, &["config", "user.email", "test@test.com"]);
        git_out(&other, &["config", "user.name", "Test"]);
        commit_file(&other, "remote.txt", "remote-only work");
        git_out(&other, &["push", "origin", "main"]);
        clone
    }

    #[tokio::test]
    async fn agent_new_branch_starts_from_local_default_branch_without_fetching() {
        let tmp = TempDir::new().unwrap();
        let clone = clone_with_diverged_remote(tmp.path());
        // The user is on another branch: the base is still the default branch, not HEAD.
        git_out(&clone, &["switch", "-c", "scratch"]);
        commit_file(&clone, "scratch.txt", "elsewhere");
        let origin_main_before = git_out(&clone, &["rev-parse", "refs/remotes/origin/main"]);
        let local_main = git_out(&clone, &["rev-parse", "refs/heads/main"]);
        let wt = tmp.path().join("wt-agent-new");

        let opened = checkout_branch_worktree_for_agent(
            clone.to_string_lossy().into_owned(),
            "feat/fresh".to_string(),
            wt.to_string_lossy().into_owned(),
            true,
        )
        .await
        .unwrap();

        assert!(paths_equal(&opened, &wt.to_string_lossy()));
        assert_eq!(
            git_out(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "feat/fresh"
        );
        assert_eq!(git_out(&wt, &["rev-parse", "HEAD"]), local_main);
        assert!(wt.join("local.txt").exists());
        assert!(!wt.join("remote.txt").exists());
        assert!(!wt.join("scratch.txt").exists());
        assert_eq!(
            git_out(&clone, &["rev-parse", "refs/remotes/origin/main"]),
            origin_main_before,
            "no fetch may run"
        );
    }

    #[tokio::test]
    async fn agent_new_branch_refuses_a_branch_that_already_exists() {
        let tmp = TempDir::new().unwrap();
        let (_bare, clone) = setup_repo_with_remote(tmp.path());
        git_out(&clone, &["branch", "feat/taken"]);
        let wt = tmp.path().join("wt-taken");

        let err = checkout_branch_worktree_for_agent(
            clone.to_string_lossy().into_owned(),
            "feat/taken".to_string(),
            wt.to_string_lossy().into_owned(),
            true,
        )
        .await
        .unwrap_err();

        assert_eq!(
            err,
            "unavailable: Branch `feat/taken` already exists — use Repo + branch to open it."
        );
        assert!(!wt.exists());
    }

    #[tokio::test]
    async fn agent_existing_branch_missing_names_new_worktree_not_a_pr_refspec() {
        let tmp = TempDir::new().unwrap();
        let (_bare, clone) = setup_repo_with_remote(tmp.path());
        let wt = tmp.path().join("wt-missing");

        let err = checkout_branch_worktree_for_agent(
            clone.to_string_lossy().into_owned(),
            "feat/nowhere".to_string(),
            wt.to_string_lossy().into_owned(),
            false,
        )
        .await
        .unwrap_err();

        assert_eq!(
            err,
            "unavailable: No branch `feat/nowhere` in this repo — choose New worktree to create it, or fetch it with your agent / git first."
        );
        assert!(!err.contains("refspec"));
        assert!(!wt.exists());
    }

    #[tokio::test]
    async fn agent_existing_branch_opens_a_local_only_branch() {
        let tmp = TempDir::new().unwrap();
        let (_bare, clone) = setup_repo_with_remote(tmp.path());
        git_out(&clone, &["switch", "-c", "feat/local-only"]);
        commit_file(&clone, "mine.txt", "unpushed");
        let tip = git_out(&clone, &["rev-parse", "HEAD"]);
        git_out(&clone, &["switch", "main"]);
        let wt = tmp.path().join("wt-local-only");

        checkout_branch_worktree_for_agent(
            clone.to_string_lossy().into_owned(),
            "feat/local-only".to_string(),
            wt.to_string_lossy().into_owned(),
            false,
        )
        .await
        .unwrap();

        assert_eq!(git_out(&wt, &["rev-parse", "HEAD"]), tip);
    }

    #[tokio::test]
    async fn local_default_branch_names_the_default_not_the_checked_out_branch() {
        let tmp = TempDir::new().unwrap();
        let (_bare, clone) = setup_repo_with_remote(tmp.path());
        git_out(&clone, &["switch", "-c", "scratch"]);
        let base = |clone: &Path| local_default_start_point(clone);

        // No `origin/HEAD` recorded: the conventional default, never the current branch.
        let _ = Command::new("git")
            .args(["remote", "set-head", "origin", "--delete"])
            .current_dir(&clone)
            .output();
        assert_eq!(base(&clone), "main");
        // A recorded remote default wins over the conventional names.
        git_out(&clone, &["branch", "trunk"]);
        git_out(&clone, &["update-ref", "refs/remotes/origin/trunk", "HEAD"]);
        git_out(
            &clone,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/trunk",
            ],
        );
        assert_eq!(
            local_default_branch(clone.to_string_lossy().into_owned())
                .await
                .unwrap(),
            "trunk"
        );
    }

    /// The repo states of the story-branch base case table. Each starts from a clone with
    /// local `main` and `origin/main`; nothing here touches the network.
    #[derive(Clone, Copy, Debug)]
    enum DefaultBranchState {
        /// `origin/HEAD` → `main`, with local `main` at the same commit.
        OriginHeadMain,
        /// `origin/HEAD` → `main`, with local `main` one commit behind `origin/main`.
        OriginHeadMainWithStaleLocalMain,
        /// `origin/HEAD` → `develop`, which exists only as a cached remote ref; no `main`
        /// anywhere and no worktree.yaml (GitHub issue #2840).
        OriginHeadDevelopOnly,
        /// As [`Self::OriginHeadDevelopOnly`], for `master`.
        OriginHeadMasterOnly,
        /// No `origin/` refs at all; only a local `main` (a local-only repo).
        LocalOnlyMain,
        /// `origin/HEAD` → `cs/v1.7.5`, which exists only as a cached remote ref; no `main`.
        OriginHeadReleaseBranch,
        /// As [`Self::OriginHeadReleaseBranch`], but an old local `main` (at another
        /// commit) is kept, like git-ai's local `main` workaround.
        OriginHeadReleaseBranchWithStaleMain,
        /// `origin/HEAD` names `origin/release`, held by a local branch of that name, while
        /// an unrelated cached `refs/remotes/origin/release` sits at another commit.
        LocalBranchNamedLikeARemote,
        /// No `origin/HEAD`; the only default is a local `master`.
        UnsetMasterOnly,
        /// No `origin/HEAD` and no conventional branch, locally or cached.
        UnsetNeither,
    }

    /// Put `clone` (from [`setup_repo_with_remote`]) into `state`. Returns the commit a
    /// new story branch must start from, or `None` when nothing resolves.
    fn arrange_default_branch(clone: &Path, state: DefaultBranchState) -> Option<String> {
        let drop_origin_head = || {
            let _ = Command::new("git")
                .args(["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"])
                .current_dir(clone)
                .output();
        };
        let drop_main = || {
            git_out(clone, &["checkout", "--detach"]);
            git_out(clone, &["branch", "-D", "main"]);
            git_out(clone, &["update-ref", "-d", "refs/remotes/origin/main"]);
        };
        // `origin/HEAD` → `name`, held only by a cached remote ref at a new commit; the
        // checked-out commit moves on so it can't be mistaken for the default.
        let remote_only_default = |name: &str| {
            drop_main();
            commit_file(clone, &format!("{name}.txt"), "default branch work");
            let tip = git_out(clone, &["rev-parse", "HEAD"]);
            let remote = format!("refs/remotes/origin/{name}");
            git_out(clone, &["update-ref", &remote, &tip]);
            git_out(
                clone,
                &["symbolic-ref", "refs/remotes/origin/HEAD", &remote],
            );
            commit_file(clone, "detached.txt", "detached work");
            tip
        };
        match state {
            DefaultBranchState::OriginHeadMain => {
                git_out(
                    clone,
                    &[
                        "symbolic-ref",
                        "refs/remotes/origin/HEAD",
                        "refs/remotes/origin/main",
                    ],
                );
                Some(git_out(clone, &["rev-parse", "refs/heads/main"]))
            }
            DefaultBranchState::OriginHeadMainWithStaleLocalMain => {
                git_out(clone, &["checkout", "--detach"]);
                commit_file(clone, "upstream.txt", "work merged upstream");
                let tip = git_out(clone, &["rev-parse", "HEAD"]);
                git_out(clone, &["update-ref", "refs/remotes/origin/main", &tip]);
                git_out(
                    clone,
                    &[
                        "symbolic-ref",
                        "refs/remotes/origin/HEAD",
                        "refs/remotes/origin/main",
                    ],
                );
                git_out(clone, &["checkout", "main"]);
                assert_eq!(
                    git_out(clone, &["rev-parse", "refs/heads/main^{commit}"]),
                    git_out(clone, &["rev-parse", &format!("{tip}~1")]),
                    "local main is one commit behind origin/main"
                );
                Some(tip)
            }
            DefaultBranchState::OriginHeadDevelopOnly => Some(remote_only_default("develop")),
            DefaultBranchState::OriginHeadMasterOnly => Some(remote_only_default("master")),
            DefaultBranchState::LocalOnlyMain => {
                drop_origin_head();
                git_out(clone, &["update-ref", "-d", "refs/remotes/origin/main"]);
                Some(git_out(clone, &["rev-parse", "refs/heads/main"]))
            }
            DefaultBranchState::OriginHeadReleaseBranch => {
                drop_main();
                commit_file(clone, "release.txt", "release branch work");
                let tip = git_out(clone, &["rev-parse", "HEAD"]);
                git_out(
                    clone,
                    &["update-ref", "refs/remotes/origin/cs/v1.7.5", &tip],
                );
                git_out(
                    clone,
                    &[
                        "symbolic-ref",
                        "refs/remotes/origin/HEAD",
                        "refs/remotes/origin/cs/v1.7.5",
                    ],
                );
                // The checked-out commit must not be mistaken for the default.
                commit_file(clone, "detached.txt", "detached work");
                Some(tip)
            }
            DefaultBranchState::OriginHeadReleaseBranchWithStaleMain => {
                git_out(clone, &["checkout", "--detach"]);
                commit_file(clone, "release.txt", "release branch work");
                let tip = git_out(clone, &["rev-parse", "HEAD"]);
                git_out(
                    clone,
                    &["update-ref", "refs/remotes/origin/cs/v1.7.5", &tip],
                );
                git_out(
                    clone,
                    &[
                        "symbolic-ref",
                        "refs/remotes/origin/HEAD",
                        "refs/remotes/origin/cs/v1.7.5",
                    ],
                );
                git_out(clone, &["checkout", "main"]);
                assert_ne!(git_out(clone, &["rev-parse", "refs/heads/main"]), tip);
                Some(tip)
            }
            DefaultBranchState::LocalBranchNamedLikeARemote => {
                git_out(clone, &["checkout", "--detach"]);
                commit_file(clone, "local-release.txt", "local branch work");
                let local = git_out(clone, &["rev-parse", "HEAD"]);
                commit_file(clone, "remote-release.txt", "unrelated remote work");
                let remote = git_out(clone, &["rev-parse", "HEAD"]);
                git_out(
                    clone,
                    &["update-ref", "refs/remotes/origin/release", &remote],
                );
                git_out(clone, &["branch", "origin/release", &local]);
                git_out(clone, &["checkout", "main"]);
                git_out(
                    clone,
                    &[
                        "symbolic-ref",
                        "refs/remotes/origin/HEAD",
                        "refs/remotes/origin/origin/release",
                    ],
                );
                Some(local)
            }
            DefaultBranchState::UnsetMasterOnly => {
                drop_origin_head();
                git_out(clone, &["branch", "-m", "main", "master"]);
                git_out(clone, &["update-ref", "-d", "refs/remotes/origin/main"]);
                Some(git_out(clone, &["rev-parse", "refs/heads/master"]))
            }
            DefaultBranchState::UnsetNeither => {
                drop_origin_head();
                drop_main();
                None
            }
        }
    }

    #[tokio::test]
    async fn new_story_branch_starts_from_the_repos_default_branch_or_refuses() {
        use DefaultBranchState::*;
        for state in [
            OriginHeadMain,
            OriginHeadMainWithStaleLocalMain,
            OriginHeadDevelopOnly,
            OriginHeadMasterOnly,
            LocalOnlyMain,
            OriginHeadReleaseBranch,
            OriginHeadReleaseBranchWithStaleMain,
            LocalBranchNamedLikeARemote,
            UnsetMasterOnly,
            UnsetNeither,
        ] {
            let tmp = TempDir::new().unwrap();
            let (_bare, clone) = setup_repo_with_remote(tmp.path());
            let expected = arrange_default_branch(&clone, state);
            let wt = tmp.path().join("wt-story");

            let result = fetch_and_create_worktree(
                clone.to_string_lossy().into_owned(),
                "feat/SV-1-story".to_string(),
                wt.to_string_lossy().into_owned(),
                None,
                None,
            )
            .await;

            match expected {
                Some(tip) => {
                    let opened = result.unwrap_or_else(|e| panic!("{state:?}: {e}"));
                    assert!(!opened.from_remote, "{state:?}");
                    assert_eq!(git_out(&wt, &["rev-parse", "HEAD"]), tip, "{state:?}");
                    assert_eq!(
                        git_out(&wt, &["symbolic-ref", "--short", "HEAD"]),
                        "feat/SV-1-story",
                        "{state:?}"
                    );
                    // A new story branch never tracks (and so never pushes to) the default.
                    let upstream = Command::new("git")
                        .args(["rev-parse", "--abbrev-ref", "feat/SV-1-story@{upstream}"])
                        .current_dir(&clone)
                        .output()
                        .unwrap();
                    assert!(
                        !upstream.status.success(),
                        "{state:?}: tracks {}",
                        String::from_utf8_lossy(&upstream.stdout)
                    );
                }
                None => {
                    let error = result.expect_err("nothing resolves: refuse");
                    for named in [
                        "refs/remotes/origin/HEAD",
                        "refs/heads/main",
                        "refs/heads/master",
                        "refs/heads/develop",
                        "refs/remotes/origin/main",
                        "refs/remotes/origin/master",
                        "refs/remotes/origin/develop",
                        "git remote set-head origin --auto",
                        "default_start_point",
                        "worktree.yaml",
                    ] {
                        assert!(error.contains(named), "refusal omits `{named}`: {error}");
                    }
                    assert!(
                        error.contains("can't start a new story branch in ")
                            && error.contains(": no default branch found. Tried "),
                        "the refusal leads with the outcome: {error}"
                    );
                    assert!(error.contains(&clone.display().to_string()), "{error}");
                    assert!(error.contains("write_worktree_config"), "{error}");
                    assert!(!error.contains("invalid reference"), "{error}");
                    assert!(!wt.exists(), "a refusal creates nothing");
                    let branch = Command::new("git")
                        .args(["rev-parse", "--verify", "refs/heads/feat/SV-1-story"])
                        .current_dir(&clone)
                        .output()
                        .unwrap();
                    assert!(!branch.status.success(), "a refusal mints no branch");
                }
            }
        }
    }

    #[test]
    fn story_start_point_prefers_remote_defaults_as_full_refs_and_keeps_local_only_names() {
        use DefaultBranchState::*;
        for (state, want) in [
            (OriginHeadMain, Ok("refs/remotes/origin/main")),
            (
                OriginHeadMainWithStaleLocalMain,
                Ok("refs/remotes/origin/main"),
            ),
            (OriginHeadDevelopOnly, Ok("refs/remotes/origin/develop")),
            (OriginHeadMasterOnly, Ok("refs/remotes/origin/master")),
            (LocalOnlyMain, Ok("main")),
            (OriginHeadReleaseBranch, Ok("refs/remotes/origin/cs/v1.7.5")),
            (
                OriginHeadReleaseBranchWithStaleMain,
                Ok("refs/remotes/origin/cs/v1.7.5"),
            ),
            (LocalBranchNamedLikeARemote, Ok("refs/heads/origin/release")),
            (UnsetMasterOnly, Ok("master")),
            (UnsetNeither, Err(())),
        ] {
            let tmp = TempDir::new().unwrap();
            let (_bare, clone) = setup_repo_with_remote(tmp.path());
            arrange_default_branch(&clone, state);
            let got = resolve_story_start_point(&clone);
            match want {
                Ok(name) => assert_eq!(got.as_deref(), Ok(name), "{state:?}"),
                Err(()) => assert!(got.is_err(), "{state:?}: {got:?}"),
            }
            // Agent launches keep their own fallback, `HEAD` included.
            let launch = local_default_start_point(&clone);
            match state {
                OriginHeadMain | OriginHeadMainWithStaleLocalMain => assert_eq!(launch, "main"),
                OriginHeadDevelopOnly => assert_eq!(launch, "origin/develop"),
                OriginHeadMasterOnly => assert_eq!(launch, "origin/master"),
                OriginHeadReleaseBranch => assert_eq!(launch, "origin/cs/v1.7.5"),
                OriginHeadReleaseBranchWithStaleMain => assert_eq!(launch, "main"),
                LocalBranchNamedLikeARemote => assert_eq!(launch, "origin/release"),
                UnsetNeither => assert_eq!(launch, "HEAD"),
                _ => assert_eq!(Ok(launch.as_str()), got.as_deref(), "{state:?}"),
            }
        }
    }

    #[tokio::test]
    async fn configured_start_point_prefers_the_remote_ref() {
        // `default_start_point: develop` with a local `develop` and an `origin/develop`
        // one commit ahead of it; then with only the local branch; then a commit.
        for case in ["both", "local-only", "commit"] {
            let tmp = TempDir::new().unwrap();
            let (_bare, clone) = setup_repo_with_remote(tmp.path());
            let local = git_out(&clone, &["rev-parse", "HEAD"]);
            git_out(&clone, &["branch", "develop", &local]);
            git_out(&clone, &["checkout", "--detach"]);
            commit_file(&clone, "upstream.txt", "work merged upstream");
            let remote = git_out(&clone, &["rev-parse", "HEAD"]);
            git_out(
                &clone,
                &["update-ref", "refs/remotes/origin/develop", &remote],
            );
            commit_file(&clone, "detached.txt", "detached work");
            let (configured, want_ref, want_tip) = match case {
                "both" => (
                    "develop".to_string(),
                    "refs/remotes/origin/develop",
                    &remote,
                ),
                "local-only" => {
                    git_out(&clone, &["update-ref", "-d", "refs/remotes/origin/develop"]);
                    ("develop".to_string(), "develop", &local)
                }
                _ => (remote.clone(), remote.as_str(), &remote),
            };
            assert_eq!(
                configured_start_point(&clone, &configured),
                want_ref,
                "{case}"
            );
            let wt = tmp.path().join("wt-configured");

            let opened = open_story_worktree(
                clone.to_string_lossy().into_owned(),
                "feat/SV-3-story".to_string(),
                wt.to_string_lossy().into_owned(),
                Some(configured),
            )
            .await
            .unwrap_or_else(|e| panic!("{case}: {e}"));

            assert!(!opened.from_remote, "{case}");
            assert_eq!(&git_out(&wt, &["rev-parse", "HEAD"]), want_tip, "{case}");
            let upstream = Command::new("git")
                .args(["rev-parse", "--abbrev-ref", "feat/SV-3-story@{upstream}"])
                .current_dir(&clone)
                .output()
                .unwrap();
            assert!(
                !upstream.status.success(),
                "{case}: a new story branch tracks its base"
            );
        }
    }

    #[tokio::test]
    async fn ui_opens_resolve_a_configured_start_point_like_the_mcp_open() {
        // `default_start_point: develop` sent as the configured value (the UI and machine
        // opens), with only `origin/develop` (the #2840 clone) or a stale local `develop`
        // behind it; an empty value is unset; an explicit start point stays verbatim.
        for case in ["remote-only", "stale-local", "empty", "explicit"] {
            let tmp = TempDir::new().unwrap();
            let (_bare, clone) = setup_repo_with_remote(tmp.path());
            let local = git_out(&clone, &["rev-parse", "HEAD"]);
            git_out(&clone, &["checkout", "--detach"]);
            commit_file(&clone, "upstream.txt", "work merged upstream");
            let remote = git_out(&clone, &["rev-parse", "HEAD"]);
            git_out(
                &clone,
                &["update-ref", "refs/remotes/origin/develop", &remote],
            );
            if case != "remote-only" {
                git_out(&clone, &["branch", "develop", &local]);
            }
            commit_file(&clone, "detached.txt", "detached work");
            let repo = clone.to_string_lossy().into_owned();
            let (start_point, configured) = match case {
                "empty" => (None, Some(String::new())),
                "explicit" => (Some("develop".to_string()), None),
                _ => (None, Some("develop".to_string())),
            };

            let base = story_default_start_point(repo.clone(), configured.clone()).await;
            match case {
                "empty" => assert_eq!(base, resolve_story_start_point(&clone), "{case}"),
                "explicit" => {}
                _ => assert_eq!(base.as_deref(), Ok("refs/remotes/origin/develop"), "{case}"),
            }

            let wt = tmp.path().join("wt-ui");
            fetch_and_create_worktree(
                repo,
                "feat/SV-4-story".to_string(),
                wt.to_string_lossy().into_owned(),
                start_point,
                configured,
            )
            .await
            .unwrap_or_else(|e| panic!("{case}: {e}"));

            let tip = git_out(&wt, &["rev-parse", "HEAD"]);
            match case {
                "empty" => {
                    let default = resolve_story_start_point(&clone).unwrap();
                    assert_eq!(tip, git_out(&clone, &["rev-parse", &default]), "{case}");
                }
                "explicit" => assert_eq!(tip, local, "{case}"),
                _ => assert_eq!(tip, remote, "{case}"),
            }
            let upstream = Command::new("git")
                .args(["rev-parse", "--abbrev-ref", "feat/SV-4-story@{upstream}"])
                .current_dir(&clone)
                .output()
                .unwrap();
            assert!(
                !upstream.status.success(),
                "{case}: a new story branch tracks its base"
            );
        }
    }

    #[tokio::test]
    async fn reopening_an_existing_story_branch_never_needs_a_resolvable_default() {
        for remote_only in [false, true] {
            let tmp = TempDir::new().unwrap();
            let (_bare, clone) = setup_repo_with_remote(tmp.path());
            commit_file(&clone, "story.txt", "story work");
            let tip = git_out(&clone, &["rev-parse", "HEAD"]);
            git_out(
                &clone,
                &["update-ref", "refs/remotes/origin/feat/SV-2-story", &tip],
            );
            if !remote_only {
                git_out(&clone, &["branch", "feat/SV-2-story", &tip]);
            }
            git_out(&clone, &["reset", "--hard", "HEAD~1"]);
            arrange_default_branch(&clone, DefaultBranchState::UnsetNeither);
            let wt = tmp.path().join("wt-reopen");

            let opened = fetch_and_create_worktree(
                clone.to_string_lossy().into_owned(),
                "feat/SV-2-story".to_string(),
                wt.to_string_lossy().into_owned(),
                None,
                None,
            )
            .await
            .unwrap_or_else(|e| panic!("remote_only={remote_only}: {e}"));

            assert_eq!(opened.from_remote, remote_only);
            assert_eq!(git_out(&wt, &["rev-parse", "HEAD"]), tip);
        }
    }

    #[tokio::test]
    async fn agent_checkout_points_at_another_worktree_holding_the_branch() {
        let tmp = TempDir::new().unwrap();
        let (_bare, clone) = setup_repo_with_remote(tmp.path());
        let elsewhere = tmp.path().join("story-wt");
        git_out(
            &clone,
            &[
                "worktree",
                "add",
                "-b",
                "feat/held",
                "--",
                &elsewhere.to_string_lossy(),
            ],
        );
        let wt = tmp.path().join("wt-held");

        let err = checkout_branch_worktree_for_agent(
            clone.to_string_lossy().into_owned(),
            "feat/held".to_string(),
            wt.to_string_lossy().into_owned(),
            false,
        )
        .await
        .unwrap_err();

        assert!(
            err.starts_with("unavailable: Branch `feat/held` is already checked out at ")
                && err.ends_with(" — choose Existing worktree to work there."),
            "{err}"
        );
        assert!(!wt.exists());
    }

    #[tokio::test]
    async fn agent_new_branch_starts_from_the_default_branch_not_a_same_named_tag() {
        let tmp = TempDir::new().unwrap();
        let (_bare, clone) = setup_repo_with_remote(tmp.path());
        let tagged = git_out(&clone, &["rev-parse", "HEAD"]);
        git_out(&clone, &["tag", "main"]);
        commit_file(&clone, "after-tag.txt", "newer");
        let main = git_out(&clone, &["rev-parse", "refs/heads/main"]);
        assert_ne!(main, tagged);
        let wt = tmp.path().join("wt-tag");

        checkout_branch_worktree_for_agent(
            clone.to_string_lossy().into_owned(),
            "feat/after-tag".to_string(),
            wt.to_string_lossy().into_owned(),
            true,
        )
        .await
        .unwrap();

        assert_eq!(git_out(&wt, &["rev-parse", "HEAD"]), main);
    }

    #[tokio::test]
    async fn agent_new_branch_starts_from_the_cached_remote_default_when_no_local_default() {
        let tmp = TempDir::new().unwrap();
        let (_bare, clone) = setup_repo_with_remote(tmp.path());
        git_out(&clone, &["remote", "set-head", "origin", "main"]);
        let remote_main = git_out(&clone, &["rev-parse", "refs/remotes/origin/main"]);
        // The user works on `scratch` and has no local default branch left.
        git_out(&clone, &["switch", "-c", "scratch"]);
        commit_file(&clone, "scratch.txt", "elsewhere");
        git_out(&clone, &["branch", "-D", "main"]);
        assert_eq!(local_default_start_point(&clone), "origin/main");
        let wt = tmp.path().join("wt-remote-default");

        checkout_branch_worktree_for_agent(
            clone.to_string_lossy().into_owned(),
            "feat/from-remote".to_string(),
            wt.to_string_lossy().into_owned(),
            true,
        )
        .await
        .unwrap();

        assert_eq!(git_out(&wt, &["rev-parse", "HEAD"]), remote_main);
        assert!(!wt.join("scratch.txt").exists());
        let upstream = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "feat/from-remote@{upstream}"])
            .current_dir(&clone)
            .output()
            .unwrap();
        assert!(
            !upstream.status.success(),
            "the new branch must not track the default"
        );
    }

    #[tokio::test]
    async fn agent_existing_branch_names_a_deleted_worktree_instead_of_adopting_it() {
        let tmp = TempDir::new().unwrap();
        let (_bare, clone) = setup_repo_with_remote(tmp.path());
        let wt = tmp.path().join("wt-gone");
        let path = wt.to_string_lossy().into_owned();
        git_out(&clone, &["worktree", "add", "-b", "feat/gone", "--", &path]);
        fs::remove_dir_all(&wt).unwrap();

        let err = checkout_branch_worktree_for_agent(
            clone.to_string_lossy().into_owned(),
            "feat/gone".to_string(),
            path,
            false,
        )
        .await
        .unwrap_err();

        assert!(
            err.starts_with("unavailable: the worktree holding `feat/gone` at ")
                && err.contains("is missing — run `git worktree prune`"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn agent_checkout_names_a_stale_directory_and_an_invalid_branch() {
        let tmp = TempDir::new().unwrap();
        let (_bare, clone) = setup_repo_with_remote(tmp.path());
        let wt = tmp.path().join("wt-stale");
        fs::create_dir_all(&wt).unwrap();
        let open = |branch: &str| {
            checkout_branch_worktree_for_agent(
                clone.to_string_lossy().into_owned(),
                branch.to_string(),
                wt.to_string_lossy().into_owned(),
                true,
            )
        };

        let stale = open("feat/fresh").await.unwrap_err();
        assert!(
            stale.starts_with("unavailable: a stale directory already exists"),
            "{stale}"
        );
        let invalid = open("feat/has space").await.unwrap_err();
        assert_eq!(
            invalid,
            "unavailable: `feat/has space` is not a valid branch name."
        );
    }

    #[tokio::test]
    async fn fetch_and_create_worktree_with_dep_start_point() {
        let tmp = TempDir::new().unwrap();
        let (_bare, clone) = setup_repo_with_remote(tmp.path());

        // Create a dep branch locally
        Command::new("git")
            .args(["checkout", "-b", "feat/34-4-dep"])
            .current_dir(&clone)
            .output()
            .unwrap();
        fs::write(clone.join("dep.txt"), "dep work").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&clone)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "dep work"])
            .current_dir(&clone)
            .output()
            .unwrap();
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(&clone)
            .output()
            .unwrap();

        let wt_path = tmp.path().join("wt-dep");
        let result = fetch_and_create_worktree(
            clone.to_string_lossy().into_owned(),
            "feat/new-from-dep".to_string(),
            wt_path.to_string_lossy().into_owned(),
            Some("feat/34-4-dep".to_string()),
            None,
        )
        .await
        .unwrap();

        assert!(!result.from_remote);
        assert!(wt_path.is_dir());
        // Should have the dep file since we branched from the dep branch
        assert!(wt_path.join("dep.txt").exists());
    }

    #[tokio::test]
    async fn fetch_and_create_worktree_with_unreachable_remote_falls_back() {
        let tmp = TempDir::new().unwrap();
        let (_bare, clone) = setup_repo_with_remote(tmp.path());

        // Remove the remote so fetch fails
        Command::new("git")
            .args(["remote", "remove", "origin"])
            .current_dir(&clone)
            .output()
            .unwrap();

        let wt_path = tmp.path().join("wt-no-remote");
        let result = fetch_and_create_worktree(
            clone.to_string_lossy().into_owned(),
            "feat/offline".to_string(),
            wt_path.to_string_lossy().into_owned(),
            Some("main".to_string()),
            None,
        )
        .await
        .unwrap();

        assert!(!result.from_remote);
        assert!(wt_path.is_dir());
    }

    #[tokio::test]
    async fn fetch_and_create_worktree_reuses_orphaned_local_branch() {
        let tmp = TempDir::new().unwrap();
        let (_bare, clone) = setup_repo_with_remote(tmp.path());

        // Create worktree, then remove it — leaves the local branch behind.
        let wt_path_1 = tmp.path().join("wt-orphan").to_string_lossy().into_owned();
        fetch_and_create_worktree(
            clone.to_string_lossy().into_owned(),
            "feat/orphan-fetch".to_string(),
            wt_path_1.clone(),
            Some("main".to_string()),
            None,
        )
        .await
        .unwrap();
        remove_worktree_prune(clone.to_string_lossy().into_owned(), wt_path_1, Some(true))
            .await
            .unwrap();

        // Branch still exists locally but no worktree uses it —
        // fetch_and_create_worktree should reuse the existing branch.
        let wt_path_2 = tmp
            .path()
            .join("wt-orphan-retry")
            .to_string_lossy()
            .into_owned();
        let result = fetch_and_create_worktree(
            clone.to_string_lossy().into_owned(),
            "feat/orphan-fetch".to_string(),
            wt_path_2.clone(),
            Some("main".to_string()),
            None,
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().worktree_path, wt_path_2);

        // Confirm the worktree is listed with the correct branch.
        let listed = list_worktrees(clone.to_string_lossy().into_owned())
            .await
            .unwrap();
        assert!(listed
            .iter()
            .any(|w| w.branch.as_deref() == Some("feat/orphan-fetch")));
    }

    // ── checkout_branch_worktree_for_review tests (SW-DX-19) ────────────────

    #[tokio::test]
    async fn review_checkout_starts_from_origin_when_remote_branch_exists() {
        // The PR head is on origin but NOT a local branch — the reviewer's
        // common case. The caller prepares refs before review mode opens them.
        let tmp = TempDir::new().unwrap();
        let (bare, clone) = setup_repo_with_remote(tmp.path());

        // Push feat/pr from a separate clone so `clone` has no local branch.
        let pusher = tmp.path().join("pusher");
        Command::new("git")
            .args(["clone"])
            .arg(&bare)
            .arg(&pusher)
            .output()
            .unwrap();
        for (k, v) in [("user.email", "test@test.com"), ("user.name", "Test")] {
            Command::new("git")
                .args(["config", k, v])
                .current_dir(&pusher)
                .output()
                .unwrap();
        }
        Command::new("git")
            .args(["checkout", "-b", "feat/pr"])
            .current_dir(&pusher)
            .output()
            .unwrap();
        fs::write(pusher.join("pr-work.txt"), "pr head").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&pusher)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "pr work"])
            .current_dir(&pusher)
            .output()
            .unwrap();
        Command::new("git")
            .args(["push", "origin", "feat/pr"])
            .current_dir(&pusher)
            .output()
            .unwrap();

        let wt_path = tmp.path().join("wt-review");
        let error = checkout_branch_worktree_for_review(
            clone.to_string_lossy().into_owned(),
            "feat/pr".into(),
            wt_path.to_string_lossy().into_owned(),
        )
        .await
        .unwrap_err();
        assert!(error.contains("refs/heads/feat/pr:refs/remotes/origin/feat/pr"));
        assert!(!wt_path.exists());
        assert!(git2::Repository::open(&clone)
            .unwrap()
            .find_reference("refs/remotes/origin/feat/pr")
            .is_err());
        prepare_review_refs(&clone, None);

        let path = checkout_branch_worktree_for_review(
            clone.to_string_lossy().into_owned(),
            "feat/pr".to_string(),
            wt_path.to_string_lossy().into_owned(),
        )
        .await
        .unwrap();

        assert_eq!(path, wt_path.to_string_lossy());
        assert!(wt_path.is_dir());
        // Proves the worktree holds the PR's commits, not the clone's HEAD.
        assert!(wt_path.join("pr-work.txt").exists());
    }

    #[tokio::test]
    async fn fork_review_checkout_uses_the_base_remote_pull_ref() {
        let tmp = TempDir::new().unwrap();
        let (bare, clone) = setup_repo_with_remote(tmp.path());
        let pusher = tmp.path().join("fork-pusher");
        Command::new("git")
            .args(["clone"])
            .arg(&bare)
            .arg(&pusher)
            .output()
            .unwrap();
        for (key, value) in [("user.email", "test@test.com"), ("user.name", "Test")] {
            Command::new("git")
                .args(["config", key, value])
                .current_dir(&pusher)
                .output()
                .unwrap();
        }
        fs::write(pusher.join("fork-pr.txt"), "fork head").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&pusher)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "fork pr"])
            .current_dir(&pusher)
            .output()
            .unwrap();
        Command::new("git")
            .args(["push", "origin", "HEAD:refs/pull/42/head"])
            .current_dir(&pusher)
            .output()
            .unwrap();

        let worktree = tmp.path().join("wt-fork");
        let error = checkout_pull_request_worktree_for_review(
            clone.to_string_lossy().into_owned(),
            42,
            worktree.to_string_lossy().into_owned(),
        )
        .await
        .unwrap_err();
        let refspec = "refs/pull/42/head:refs/remotes/origin/codesense-pr/42";
        assert!(error.contains(refspec), "{error}");
        assert!(!worktree.exists());
        assert!(git2::Repository::open(&clone)
            .unwrap()
            .find_reference("refs/remotes/origin/codesense-pr/42")
            .is_err());
        prepare_review_refs(&clone, Some(refspec));

        checkout_pull_request_worktree_for_review(
            clone.to_string_lossy().into_owned(),
            42,
            worktree.to_string_lossy().into_owned(),
        )
        .await
        .unwrap();
        assert_eq!(
            fs::read_to_string(worktree.join("fork-pr.txt")).unwrap(),
            "fork head"
        );
    }

    #[tokio::test]
    async fn review_checkout_errors_when_remote_head_cannot_be_verified() {
        // Branch exists neither on origin nor locally — review mode must fail
        // loudly, NOT silently mint a branch at the clone's HEAD.
        let tmp = TempDir::new().unwrap();
        let (_bare, clone) = setup_repo_with_remote(tmp.path());

        let wt_path = tmp.path().join("wt-missing");
        let result = checkout_branch_worktree_for_review(
            clone.to_string_lossy().into_owned(),
            "feat/never-pushed".to_string(),
            wt_path.to_string_lossy().into_owned(),
        )
        .await;

        let error = result.unwrap_err();
        assert!(
            error.contains("refs/heads/feat/never-pushed:refs/remotes/origin/feat/never-pushed"),
            "unexpected review checkout error: {error}"
        );
        // No worktree may be minted at HEAD.
        assert!(!wt_path.exists());
    }

    #[tokio::test]
    async fn review_checkout_rejects_an_unverifiable_local_only_branch() {
        // A local branch cannot prove the current provider head. Review mode
        // fails closed when origin has no matching ref.
        let tmp = TempDir::new().unwrap();
        let (_bare, clone) = setup_repo_with_remote(tmp.path());

        Command::new("git")
            .args(["checkout", "-b", "feat/local"])
            .current_dir(&clone)
            .output()
            .unwrap();
        fs::write(clone.join("local.txt"), "local work").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&clone)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "local work"])
            .current_dir(&clone)
            .output()
            .unwrap();
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(&clone)
            .output()
            .unwrap();

        let wt_path = tmp.path().join("wt-local");
        let result = checkout_branch_worktree_for_review(
            clone.to_string_lossy().into_owned(),
            "feat/local".to_string(),
            wt_path.to_string_lossy().into_owned(),
        )
        .await
        .unwrap_err();

        assert!(result.contains("unavailable locally"));
        assert!(!wt_path.exists());
    }

    /// Helper: create a clone that pushes `branch` to the shared bare remote with
    /// file `pr.txt` = `content`, advancing the remote head each call.
    fn push_branch_head(bare: &Path, tmp: &Path, name: &str, branch: &str, content: &str) {
        let pusher = tmp.join(name);
        if !pusher.exists() {
            Command::new("git")
                .args(["clone"])
                .arg(bare)
                .arg(&pusher)
                .output()
                .unwrap();
            for (k, v) in [("user.email", "t@t.com"), ("user.name", "T")] {
                Command::new("git")
                    .args(["config", k, v])
                    .current_dir(&pusher)
                    .output()
                    .unwrap();
            }
            Command::new("git")
                .args(["checkout", "-b", branch])
                .current_dir(&pusher)
                .output()
                .unwrap();
        }
        fs::write(pusher.join("pr.txt"), content).unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&pusher)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", content])
            .current_dir(&pusher)
            .output()
            .unwrap();
        Command::new("git")
            .args(["push", "origin", branch])
            .current_dir(&pusher)
            .output()
            .unwrap();
    }

    #[tokio::test]
    async fn review_checkout_uses_cached_ref_without_fetching_advanced_remote() {
        // The remote advances independently; opening a review keeps the prepared local tip.
        let tmp = TempDir::new().unwrap();
        let (bare, clone) = setup_repo_with_remote(tmp.path());

        push_branch_head(&bare, tmp.path(), "pusher", "feat/pr", "v1");
        // Reviewer ends up with a local branch at v1 (as a prior review open would).
        Command::new("git")
            .args(["fetch", "origin"])
            .current_dir(&clone)
            .output()
            .unwrap();
        Command::new("git")
            .args(["branch", "feat/pr", "origin/feat/pr"])
            .current_dir(&clone)
            .output()
            .unwrap();
        // Author advances the PR to v2.
        push_branch_head(&bare, tmp.path(), "pusher", "feat/pr", "v2");

        let cached = git2::Repository::open(&clone)
            .unwrap()
            .refname_to_id("refs/remotes/origin/feat/pr")
            .unwrap();
        let wt = tmp.path().join("wt-fresh");
        checkout_branch_worktree_for_review(
            clone.to_string_lossy().into_owned(),
            "feat/pr".to_string(),
            wt.to_string_lossy().into_owned(),
        )
        .await
        .unwrap();

        assert_eq!(fs::read_to_string(wt.join("pr.txt")).unwrap().trim(), "v1");
        assert_eq!(
            git2::Repository::open(&clone)
                .unwrap()
                .refname_to_id("refs/remotes/origin/feat/pr")
                .unwrap(),
            cached
        );
    }

    #[tokio::test]
    async fn review_checkout_refreshes_existing_worktree_to_new_head() {
        // Re-review of an advanced PR into the SAME managed path must fast-forward
        // the existing worktree to the new head.
        let tmp = TempDir::new().unwrap();
        let (bare, clone) = setup_repo_with_remote(tmp.path());

        push_branch_head(&bare, tmp.path(), "pusher", "feat/pr", "v1");
        prepare_review_refs(&clone, None);
        let wt = tmp.path().join("wt-refresh").to_string_lossy().into_owned();
        checkout_branch_worktree_for_review(
            clone.to_string_lossy().into_owned(),
            "feat/pr".to_string(),
            wt.clone(),
        )
        .await
        .unwrap();
        assert_eq!(
            fs::read_to_string(PathBuf::from(&wt).join("pr.txt"))
                .unwrap()
                .trim(),
            "v1"
        );

        push_branch_head(&bare, tmp.path(), "pusher", "feat/pr", "v2");
        checkout_branch_worktree_for_review(
            clone.to_string_lossy().into_owned(),
            "feat/pr".into(),
            wt.clone(),
        )
        .await
        .unwrap();
        assert_eq!(
            fs::read_to_string(PathBuf::from(&wt).join("pr.txt"))
                .unwrap()
                .trim(),
            "v1"
        );
        prepare_review_refs(&clone, None);

        let path = checkout_branch_worktree_for_review(
            clone.to_string_lossy().into_owned(),
            "feat/pr".to_string(),
            wt.clone(),
        )
        .await
        .unwrap();

        // The refresh path returns git's canonicalized worktree path; compare
        // resolved paths, not raw strings (macOS /var → /private/var symlink).
        assert_eq!(
            fs::canonicalize(&path).unwrap(),
            fs::canonicalize(&wt).unwrap()
        );
        assert_eq!(
            fs::read_to_string(PathBuf::from(&wt).join("pr.txt"))
                .unwrap()
                .trim(),
            "v2"
        );
    }

    #[tokio::test]
    async fn review_checkout_errors_on_diverged_local_branch() {
        // A local branch that has DIVERGED from the PR head (reviewer committed on
        // it) must not be opened as if it were the PR — review must error loudly,
        // never silently show stale/divergent commits.
        let tmp = TempDir::new().unwrap();
        let (bare, clone) = setup_repo_with_remote(tmp.path());

        push_branch_head(&bare, tmp.path(), "pusher", "feat/pr", "v1");
        // Reviewer's local feat/pr = v1 + a local-only commit (diverges from origin).
        Command::new("git")
            .args(["fetch", "origin"])
            .current_dir(&clone)
            .output()
            .unwrap();
        Command::new("git")
            .args(["branch", "feat/pr", "origin/feat/pr"])
            .current_dir(&clone)
            .output()
            .unwrap();
        Command::new("git")
            .args(["checkout", "feat/pr"])
            .current_dir(&clone)
            .output()
            .unwrap();
        fs::write(clone.join("mine.txt"), "local work").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&clone)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "diverge"])
            .current_dir(&clone)
            .output()
            .unwrap();
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(&clone)
            .output()
            .unwrap();
        // Author advances origin independently → the two histories diverge.
        push_branch_head(&bare, tmp.path(), "pusher", "feat/pr", "v2");
        prepare_review_refs(&clone, None);

        let wt = tmp.path().join("wt-diverged");
        let result = checkout_branch_worktree_for_review(
            clone.to_string_lossy().into_owned(),
            "feat/pr".to_string(),
            wt.to_string_lossy().into_owned(),
        )
        .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("diverged"));
        assert!(!wt.exists());
    }

    #[tokio::test]
    async fn review_checkout_errors_typed_on_stale_worktree_dir() {
        // A leftover directory at the derived path must yield a typed `unavailable:`
        // error (legible on the review surface), not a raw `git worktree add` fatal.
        let tmp = TempDir::new().unwrap();
        let (bare, clone) = setup_repo_with_remote(tmp.path());
        push_branch_head(&bare, tmp.path(), "pusher", "feat/pr", "v1");

        prepare_review_refs(&clone, None);
        let wt = tmp.path().join("wt-stale");
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join("leftover.txt"), "x").unwrap();

        let result = checkout_branch_worktree_for_review(
            clone.to_string_lossy().into_owned(),
            "feat/pr".to_string(),
            wt.to_string_lossy().into_owned(),
        )
        .await;

        let err = result.unwrap_err();
        assert!(
            err.starts_with("unavailable:"),
            "expected typed error, got: {err}"
        );
        assert!(err.contains("stale directory"));
    }

    // -----------------------------------------------------------------------
    // switch_worktree_branch (SW-WB-1) — migrate-in-place reconcile
    // -----------------------------------------------------------------------

    /// Add a worktree at `wt` on a fresh branch `branch` off `main`.
    fn add_worktree_on_branch(clone: &Path, wt: &Path, branch: &str) {
        Command::new("git")
            .args(["worktree", "add", "-b", branch])
            .arg(wt)
            .arg("main")
            .current_dir(clone)
            .output()
            .unwrap();
    }

    #[tokio::test]
    async fn switch_worktree_branch_is_idempotent_when_already_on_branch() {
        let tmp = TempDir::new().unwrap();
        let (_bare, clone) = setup_repo_with_remote(tmp.path());
        let wt = tmp.path().join("wt");
        add_worktree_on_branch(&clone, &wt, "feat/a");

        let res = switch_worktree_branch(
            clone.to_string_lossy().into_owned(),
            wt.to_string_lossy().into_owned(),
            "feat/a".to_string(),
        )
        .await
        .unwrap();
        assert_eq!(res.worktree_path, wt.to_string_lossy());
        assert_eq!(current_branch_of_worktree(&wt).as_deref(), Some("feat/a"));
    }

    #[tokio::test]
    async fn switch_worktree_branch_creates_at_head_carrying_wip_when_absent() {
        // Rename-my-work case: the target branch exists nowhere → create it at the
        // worktree's HEAD, carrying uncommitted changes across (WIP + session kept).
        let tmp = TempDir::new().unwrap();
        let (_bare, clone) = setup_repo_with_remote(tmp.path());
        let wt = tmp.path().join("wt");
        add_worktree_on_branch(&clone, &wt, "feat/a");
        fs::write(wt.join("wip.txt"), "work in progress").unwrap();

        let res = switch_worktree_branch(
            clone.to_string_lossy().into_owned(),
            wt.to_string_lossy().into_owned(),
            "feat/renamed".to_string(),
        )
        .await
        .unwrap();
        assert!(!res.from_remote);
        assert_eq!(
            current_branch_of_worktree(&wt).as_deref(),
            Some("feat/renamed")
        );
        // WIP survived the in-place switch (the whole point of migrate).
        assert_eq!(
            fs::read_to_string(wt.join("wip.txt")).unwrap(),
            "work in progress"
        );
    }

    #[tokio::test]
    async fn switch_worktree_branch_onto_existing_local_branch() {
        let tmp = TempDir::new().unwrap();
        let (_bare, clone) = setup_repo_with_remote(tmp.path());
        // Pre-create a local branch feat/b (not checked out in any worktree).
        Command::new("git")
            .args(["branch", "feat/b"])
            .current_dir(&clone)
            .output()
            .unwrap();
        let wt = tmp.path().join("wt");
        add_worktree_on_branch(&clone, &wt, "feat/a");

        switch_worktree_branch(
            clone.to_string_lossy().into_owned(),
            wt.to_string_lossy().into_owned(),
            "feat/b".to_string(),
        )
        .await
        .unwrap();
        assert_eq!(current_branch_of_worktree(&wt).as_deref(), Some("feat/b"));
    }

    #[tokio::test]
    async fn switch_worktree_branch_rejects_branch_checked_out_elsewhere() {
        // feat/b is checked out in ANOTHER worktree → typed error; never force-move.
        let tmp = TempDir::new().unwrap();
        let (_bare, clone) = setup_repo_with_remote(tmp.path());
        let other = tmp.path().join("other");
        add_worktree_on_branch(&clone, &other, "feat/b");
        let wt = tmp.path().join("wt");
        add_worktree_on_branch(&clone, &wt, "feat/a");

        let err = switch_worktree_branch(
            clone.to_string_lossy().into_owned(),
            wt.to_string_lossy().into_owned(),
            "feat/b".to_string(),
        )
        .await
        .unwrap_err();
        assert!(err.starts_with("unavailable:"), "typed error, got: {err}");
        assert!(err.contains("already checked out"));
        // The other worktree is untouched.
        assert_eq!(
            current_branch_of_worktree(&other).as_deref(),
            Some("feat/b")
        );
    }

    #[tokio::test]
    async fn switch_worktree_branch_errors_typed_when_worktree_missing() {
        let tmp = TempDir::new().unwrap();
        let (_bare, clone) = setup_repo_with_remote(tmp.path());
        let missing = tmp.path().join("nope");
        let err = switch_worktree_branch(
            clone.to_string_lossy().into_owned(),
            missing.to_string_lossy().into_owned(),
            "feat/x".to_string(),
        )
        .await
        .unwrap_err();
        assert!(err.starts_with("unavailable:"));
        assert!(err.contains("does not exist"));
    }

    // -----------------------------------------------------------------------
    // check_branch_exists (F-29)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn check_branch_exists_local_branch_returns_true() {
        let tmp = TempDir::new().unwrap();
        init_repo_with_commit(tmp.path());
        // "master" is created by init_repo_with_commit
        let result = check_branch_exists(
            tmp.path().to_string_lossy().into_owned(),
            "master".to_string(),
        )
        .await;
        assert!(result.unwrap());
    }

    #[tokio::test]
    async fn check_branch_exists_nonexistent_returns_false() {
        let tmp = TempDir::new().unwrap();
        init_repo_with_commit(tmp.path());
        let result = check_branch_exists(
            tmp.path().to_string_lossy().into_owned(),
            "feat/does-not-exist".to_string(),
        )
        .await;
        assert!(!result.unwrap());
    }

    #[tokio::test]
    async fn check_branch_exists_remote_only_returns_true() {
        // Set up an origin repo with a branch, then clone it so the
        // clone has a remote-tracking ref but no local branch.
        let origin = TempDir::new().unwrap();
        init_repo_with_commit(origin.path());
        // Create a branch on origin
        std::process::Command::new(git_bin())
            .args(["branch", "feat/remote-only"])
            .current_dir(origin.path())
            .output()
            .unwrap();

        // Clone (only master is checked out locally)
        let clone_dir = TempDir::new().unwrap();
        std::process::Command::new(git_bin())
            .args([
                "clone",
                &origin.path().to_string_lossy(),
                &clone_dir.path().to_string_lossy(),
            ])
            .output()
            .unwrap();

        // "feat/remote-only" exists on remote but not locally
        let result = check_branch_exists(
            clone_dir.path().to_string_lossy().into_owned(),
            "feat/remote-only".to_string(),
        )
        .await;
        assert!(result.unwrap());

        // Confirm it's NOT a local branch
        let local_only = crate::context_repo::branch_common::local_branch_exists(
            &git2::Repository::open(clone_dir.path()).unwrap(),
            "feat/remote-only",
        )
        .unwrap();
        assert!(!local_only);
    }

    // ---------------------------------------------------------------------
    // SS-FU-7 AC-5/AC-6: post-create projection + per-path isolation
    // ---------------------------------------------------------------------

    // -----------------------------------------------------------------
    // SW-WU-10: discover_worktrees git-first enumerator
    // -----------------------------------------------------------------

    /// `git worktree add -b <branch> <path>` off `clone`, creating parents. Returns
    /// the worktree path.
    fn add_worktree(clone: &Path, branch: &str, path: &Path) -> PathBuf {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        run_git(
            &[
                "worktree",
                "add",
                "-b",
                branch,
                "--",
                path.to_str().unwrap(),
            ],
            clone,
        )
        .unwrap();
        path.to_path_buf()
    }

    /// A canonicalized temp root, so git's (symlink-resolved) porcelain paths match
    /// the roots the managed gate compares against on macOS (`/var`→`/private/var`).
    fn canon_home(tmp: &TempDir) -> PathBuf {
        tmp.path().canonicalize().unwrap()
    }

    #[test]
    fn discover_worktrees_in_unions_clones_and_excludes_main() {
        let tmp = TempDir::new().unwrap();
        let home = canon_home(&tmp);
        // Two clones, both under `users/` so every worktree passes the managed gate;
        // this test isolates the union + main-exclusion behavior.
        let clone_a = home.join("users/1234567/sources/a");
        let clone_b = home.join("users/1234567/sources/b");
        crate::context_repo::test_fixtures::init_source_clone(&clone_a, "main");
        crate::context_repo::test_fixtures::init_source_clone(&clone_b, "main");
        let a1 = add_worktree(&clone_a, "a1", &home.join("users/1234567/wt/a1"));
        let a2 = add_worktree(&clone_a, "a2", &home.join("users/1234567/wt/a2"));
        let b1 = add_worktree(&clone_b, "b1", &home.join("users/1234567/wt/b1"));

        // The attached set is worktree paths (a1/a2 dedup to clone_a); pass them all.
        let got = discover_worktrees_in(&[a1.clone(), a2.clone(), b1.clone()], &home);

        let paths: HashSet<PathBuf> = got.iter().map(|w| PathBuf::from(&w.path)).collect();
        assert_eq!(paths.len(), 3, "union of both clones' linked worktrees");
        assert!(paths.contains(&a1) && paths.contains(&a2) && paths.contains(&b1));
        // AC-2: neither clone's own main working copy appears.
        assert!(!paths.contains(&clone_a) && !paths.contains(&clone_b));
        // Branch metadata rides through from the porcelain parser.
        let branches: HashSet<Option<String>> = got.iter().map(|w| w.branch.clone()).collect();
        assert!(branches.contains(&Some("a1".to_string())));
        assert!(branches.contains(&Some("b1".to_string())));
    }

    #[test]
    fn discover_worktrees_in_skips_unresolvable_clone_keeps_rest() {
        let tmp = TempDir::new().unwrap();
        let home = canon_home(&tmp);
        let clone = home.join("users/1234567/sources/good");
        crate::context_repo::test_fixtures::init_source_clone(&clone, "main");
        let good = add_worktree(&clone, "good", &home.join("users/1234567/wt/good"));
        // A plain non-git directory: `main_repo_of_worktree` can't resolve its clone,
        // so it is skipped fail-soft (AC-4) — the good worktree still returns.
        let junk = home.join("not-a-repo");
        fs::create_dir_all(&junk).unwrap();

        let got = discover_worktrees_in(&[junk, good.clone()], &home);

        let paths: HashSet<PathBuf> = got.iter().map(|w| PathBuf::from(&w.path)).collect();
        assert_eq!(
            paths,
            HashSet::from([good]),
            "one bad entry never blanks the set"
        );
    }

    #[test]
    fn discover_worktrees_in_gates_worktrees_not_clone_main() {
        let tmp = TempDir::new().unwrap();
        let home = canon_home(&tmp);
        // Model a raw-attached repo: the clone (primary checkout) lives OUTSIDE
        // `users/` and is itself unmanaged — yet we must still enumerate FROM it.
        let clone = home.join("primary-checkout");
        crate::context_repo::test_fixtures::init_source_clone(&clone, "main");
        // One worktree on a managed surface (under `users/`), one hand-made worktree
        // on an unmanaged path.
        let managed = add_worktree(&clone, "wtm", &home.join("users/1234567/wt/managed"));
        let unmanaged = add_worktree(&clone, "wtu", &home.join("scratch/hand-made"));

        let got = discover_worktrees_in(&[managed.clone(), unmanaged.clone()], &home);

        let paths: HashSet<PathBuf> = got.iter().map(|w| PathBuf::from(&w.path)).collect();
        // AC-3: the managed worktree surfaces; the unmanaged one and the unmanaged
        // clone-main do not.
        assert_eq!(paths, HashSet::from([managed]));
        assert!(!paths.contains(&unmanaged));
        assert!(!paths.contains(&clone));
    }

    /// AC-3 regression: git emits symlink-resolved porcelain paths, so the managed-surface
    /// gate must canonicalize both sides — otherwise a symlinked `codesense_home` (macOS
    /// `/var`→`/private/var`) false-misses and blanks the set.
    #[test]
    #[cfg(unix)]
    fn discover_worktrees_in_survives_symlinked_home() {
        let tmp = TempDir::new().unwrap();
        let real = tmp.path().canonicalize().unwrap();
        let real_home = real.join("real");
        fs::create_dir_all(&real_home).unwrap();
        // A symlinked view of the home dir.
        let link_home = real.join("link");
        std::os::unix::fs::symlink(&real_home, &link_home).unwrap();

        let clone = link_home.join("users/1234567/sources/a");
        crate::context_repo::test_fixtures::init_source_clone(&clone, "main");
        let wt = add_worktree(&clone, "a1", &link_home.join("users/1234567/wt/a1"));

        // Gate with the NON-canonical (symlinked) home — must still surface the worktree.
        let got = discover_worktrees_in(&[wt], &link_home);
        assert_eq!(
            got.len(),
            1,
            "managed worktree must survive a symlinked codesense_home"
        );
    }

    /// SW-WU-14 (Task 1): the shared clone-dir resolver the FS watcher and the scan both use.
    /// Every worktree of a clone — and the clone-main itself — collapses to ONE clone dir, an
    /// unresolvable path is skipped fail-soft, so the watch attaches once per distinct clone.
    #[test]
    fn distinct_clone_dirs_dedups_same_clone_and_skips_unresolvable() {
        use crate::content_engine::sync::WaveCoordinator;
        let tmp = TempDir::new().unwrap();
        let home = canon_home(&tmp);
        let clone = home.join("users/1234567/sources/a");
        crate::context_repo::test_fixtures::init_source_clone(&clone, "main");
        let a1 = add_worktree(&clone, "a1", &home.join("users/1234567/wt/a1"));
        let a2 = add_worktree(&clone, "a2", &home.join("users/1234567/wt/a2"));
        // A non-git directory: `main_repo_of_worktree` can't resolve its clone → skipped.
        let junk = home.join("not-a-repo");
        fs::create_dir_all(&junk).unwrap();

        // Two worktrees + the clone-main + a junk path → exactly ONE distinct clone dir.
        let got = distinct_clone_dirs(&[a1, a2, clone.clone(), junk]);
        let norm: HashSet<PathBuf> = got
            .iter()
            .map(|c| WaveCoordinator::normalize_target_path(c))
            .collect();
        assert_eq!(
            norm,
            HashSet::from([WaveCoordinator::normalize_target_path(&clone)]),
            "all worktrees + main of one clone dedup to a single clone dir; junk skipped"
        );
    }

    // ---- SW-WU-11: adopt_discovered_externals (ASSOCIATE + ADOPT) --------------------
    // These exercise the ADOPT decision on constructed `WorktreeInfo` rows (git
    // enumeration itself is covered by the `discover_worktrees_in_*` tests above), so
    // they stay fast and hermetic — no clones, just the branch→story→backfill policy.

    fn wt_info(path: &Path, branch: Option<&str>) -> WorktreeInfo {
        WorktreeInfo {
            path: path.to_string_lossy().into_owned(),
            branch: branch.map(str::to_string),
            head_sha: "deadbeef".to_string(),
        }
    }

    /// A one-story org + its built cache. The cache entry's `path` points at the story
    /// file on disk, so the liveness gate reads `true` until the file is removed.
    fn cache_with_story(
        story_id: &str,
    ) -> (
        TempDir,
        PathBuf,
        crate::install::frontmatter_cache::FrontmatterCache,
    ) {
        let org = TempDir::new().unwrap();
        let content = org.path().join("alpha").join("content");
        fs::create_dir_all(&content).unwrap();
        let file = content.join(format!("{story_id}-some-slug.md"));
        fs::write(
            &file,
            format!(
                "---\nstatus: todo\nstory_id: {story_id}\nsource_repo: acme/codesense\n---\n# Story"
            ),
        )
        .unwrap();
        let cache =
            crate::install::frontmatter_cache::FrontmatterCache::build_from_scan(org.path());
        (org, file, cache)
    }

    fn records_on_disk(home: &Path, principal: &Principal) -> Vec<WorktreeStoryRecord> {
        crate::install::worktree_index::iter_story_records(home, principal).unwrap()
    }

    /// A worktree dir that actually exists on disk, so the adopt existence guard (the
    /// `prunable`-worktree defense) passes and the branch/arity/liveness gates are what's
    /// under test. Adoption never stats it as a git worktree, so a plain dir suffices.
    fn live_wt(home: &Path, name: &str) -> PathBuf {
        let p = home.join(name);
        fs::create_dir_all(&p).unwrap();
        p
    }

    /// 4.1: external story worktree (no record, branch matches a live story) → adopted +
    /// record persisted through the one writer.
    #[test]
    fn adopt_backfills_external_story_worktree() {
        let home = TempDir::new().unwrap();
        let principal = Principal::from(1234567u64);
        let (_org, _story, cache) = cache_with_story("SW-TS-1");
        let wt = live_wt(home.path(), "ext-wt");
        let discovered = vec![wt_info(&wt, Some("feat/SW-TS-1-adopt-me"))];

        let adopted =
            adopt_discovered_externals(&discovered, &[], &cache, home.path(), &principal, "acme");

        assert_eq!(adopted.len(), 1, "the live-story external is adopted");
        assert_eq!(adopted[0].story_id, "SW-TS-1");
        let recs = records_on_disk(home.path(), &principal);
        assert_eq!(
            recs.len(),
            1,
            "record persisted → renders on the same fetch"
        );
        assert_eq!(recs[0].story_id, "SW-TS-1");
    }

    /// 4.2: no branch match, non-convention branch, and detached HEAD all fail safe —
    /// no record, not adopted (orphan rendering is SW-WU-13).
    #[test]
    fn adopt_skips_unmatched_and_detached() {
        let home = TempDir::new().unwrap();
        let principal = Principal::from(1234567u64);
        let (_org, _story, cache) = cache_with_story("SW-TS-1");
        let discovered = vec![
            wt_info(&live_wt(home.path(), "scratch"), Some("feat/scratch-work")), // no story id
            wt_info(&live_wt(home.path(), "ghost"), Some("feat/SW-TS-9-ghost")),  // unindexed id
            wt_info(&live_wt(home.path(), "detached"), None),                     // detached HEAD
        ];

        let adopted =
            adopt_discovered_externals(&discovered, &[], &cache, home.path(), &principal, "acme");

        assert!(adopted.is_empty(), "no branch resolves to a live story");
        assert!(records_on_disk(home.path(), &principal).is_empty());
    }

    /// 4.3: an ambiguous branch (2 stories share the id) fails the arity gate — no
    /// record, never mis-linked.
    #[test]
    fn adopt_skips_ambiguous_branch() {
        let home = TempDir::new().unwrap();
        let principal = Principal::from(1234567u64);
        let org = TempDir::new().unwrap();
        for team in ["alpha", "beta"] {
            let content = org.path().join(team).join("content");
            fs::create_dir_all(&content).unwrap();
            fs::write(
                content.join("SW-TS-1-x.md"),
                "---\nstatus: todo\nstory_id: SW-TS-1\nsource_repo: acme/codesense\n---\n# S",
            )
            .unwrap();
        }
        let cache =
            crate::install::frontmatter_cache::FrontmatterCache::build_from_scan(org.path());
        let discovered = vec![wt_info(
            &live_wt(home.path(), "ext"),
            Some("feat/SW-TS-1-x"),
        )];

        let adopted =
            adopt_discovered_externals(&discovered, &[], &cache, home.path(), &principal, "acme");

        assert!(adopted.is_empty(), "ambiguous id → no mis-link");
        assert!(records_on_disk(home.path(), &principal).is_empty());
    }

    /// 4.4: liveness gate — the branch matches a cache entry whose story file has been
    /// deleted (stale snapshot). No record is written (avoids adopt→orphan churn).
    #[test]
    fn adopt_skips_deleted_story_file() {
        let home = TempDir::new().unwrap();
        let principal = Principal::from(1234567u64);
        let (_org, story, cache) = cache_with_story("SW-TS-1");
        fs::remove_file(&story).unwrap(); // cache still lists it; the file is gone.
        let discovered = vec![wt_info(
            &live_wt(home.path(), "ext"),
            Some("feat/SW-TS-1-x"),
        )];

        let adopted =
            adopt_discovered_externals(&discovered, &[], &cache, home.path(), &principal, "acme");

        assert!(adopted.is_empty(), "deleted story file → no adopt");
        assert!(records_on_disk(home.path(), &principal).is_empty());
    }

    /// 4.5: a discovered path already carrying a record is skipped (record wins, AC-3) —
    /// no re-adopt even when its branch would otherwise match.
    #[test]
    fn adopt_skips_path_already_recorded() {
        let home = TempDir::new().unwrap();
        let principal = Principal::from(1234567u64);
        let (_org, _story, cache) = cache_with_story("SW-TS-1");
        let wt = home.path().join("existing-wt");
        let slug = SourceRepoSlug::new("acme", "codesense").unwrap();
        let existing = vec![WorktreeStoryRecord::new(
            &wt,
            "acme",
            slug,
            "SW-TS-1",
            PathBuf::from("SW-TS-1-some-slug.md"),
        )];
        let discovered = vec![wt_info(&wt, Some("feat/SW-TS-1-dup"))];

        let adopted = adopt_discovered_externals(
            &discovered,
            &existing,
            &cache,
            home.path(),
            &principal,
            "acme",
        );

        assert!(
            adopted.is_empty(),
            "already-recorded path keeps its record row"
        );
        assert!(records_on_disk(home.path(), &principal).is_empty());
    }

    /// Fix A: a discovered worktree whose working dir is GONE (a `prunable` entry git still
    /// lists after an out-of-band `rm`) is never adopted, even with a matching live branch —
    /// otherwise the next fetch's reap deletes the record, a resurrection loop.
    #[test]
    fn adopt_skips_gone_worktree_dir() {
        let home = TempDir::new().unwrap();
        let principal = Principal::from(1234567u64);
        let (_org, _story, cache) = cache_with_story("SW-TS-1");
        let gone = home.path().join("removed-wt"); // deliberately never created on disk
        let discovered = vec![wt_info(&gone, Some("feat/SW-TS-1-x"))];

        let adopted =
            adopt_discovered_externals(&discovered, &[], &cache, home.path(), &principal, "acme");

        assert!(adopted.is_empty(), "a vanished worktree dir is not adopted");
        assert!(records_on_disk(home.path(), &principal).is_empty());
    }

    /// Fix B: a record materialized on disk since the pre-reap snapshot (a concurrent fetch
    /// or the sync wave) is not re-written — adopt skips it, so a concurrently-written sync
    /// state is never clobbered and no duplicate write occurs.
    #[test]
    fn adopt_skips_record_written_since_snapshot() {
        let home = TempDir::new().unwrap();
        let principal = Principal::from(1234567u64);
        let (_org, _story, cache) = cache_with_story("SW-TS-1");
        let wt = live_wt(home.path(), "raced-wt");
        // Seed a record on disk (as a concurrent writer would), but pass an EMPTY `existing`
        // to model a snapshot taken before that write.
        crate::install::worktree_index::backfill_from_branch(
            home.path(),
            &principal,
            &wt,
            "acme",
            "feat/SW-TS-1-x",
            &cache,
        )
        .unwrap()
        .expect("seed record written");
        assert_eq!(records_on_disk(home.path(), &principal).len(), 1);

        let discovered = vec![wt_info(&wt, Some("feat/SW-TS-1-x"))];
        let adopted =
            adopt_discovered_externals(&discovered, &[], &cache, home.path(), &principal, "acme");

        assert!(
            adopted.is_empty(),
            "an already-present record is not re-adopted"
        );
        assert_eq!(
            records_on_disk(home.path(), &principal).len(),
            1,
            "no duplicate/clobbering write"
        );
    }

    /// Fix D: an on-disk record the current build can't parse (corrupt, or a newer build's
    /// forward-schema record → `UnsupportedSchemaVersion`) is skipped, NOT overwritten — a
    /// v1 backfill must never downgrade-clobber a record it cannot read.
    #[test]
    fn adopt_skips_unreadable_record_never_overwrites() {
        let home = TempDir::new().unwrap();
        let principal = Principal::from(1234567u64);
        let (_org, _story, cache) = cache_with_story("SW-TS-1");
        let wt = live_wt(home.path(), "corrupt-wt");
        // Seed a valid record, then mangle its file to model an unreadable/forward-schema one.
        crate::install::worktree_index::backfill_from_branch(
            home.path(),
            &principal,
            &wt,
            "acme",
            "feat/SW-TS-1-x",
            &cache,
        )
        .unwrap()
        .expect("seed record written");
        let wt_dir = home.path().join("users").join("1234567").join("worktrees");
        let rec_file = fs::read_dir(&wt_dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .find(|p| p.extension().is_some_and(|x| x == "json"))
            .expect("record file present");
        let corrupt = b"{ not valid json".to_vec();
        fs::write(&rec_file, &corrupt).unwrap();

        let discovered = vec![wt_info(&wt, Some("feat/SW-TS-1-x"))];
        let adopted =
            adopt_discovered_externals(&discovered, &[], &cache, home.path(), &principal, "acme");

        assert!(adopted.is_empty(), "an unreadable record is not re-adopted");
        assert_eq!(
            fs::read(&rec_file).unwrap(),
            corrupt,
            "the unreadable record must NOT be overwritten"
        );
    }

    #[test]
    fn adopt_preserves_unsupported_worktree_record() {
        let home = TempDir::new().unwrap();
        let principal = Principal::from(1234567u64);
        let (_org, _story, cache) = cache_with_story("SW-TS-1");
        let wt = live_wt(home.path(), "newer-wt");
        let mut record = crate::install::worktree_index::backfill_from_branch(
            home.path(),
            &principal,
            &wt,
            "acme",
            "feat/SW-TS-1-x",
            &cache,
        )
        .unwrap()
        .unwrap();
        record.schema_version = 2;
        record.setup_complete = false;
        record.status = crate::install::worktree_index::SyncStatus::Synced;
        record.lazy = true;
        record.hydrated_epics.push(home.path().join("epic"));
        let dir = home.path().join("users/1234567/worktrees");
        let path = fs::read_dir(dir).unwrap().next().unwrap().unwrap().path();
        let mut newer = serde_json::to_value(record).unwrap();
        newer["newer_metadata"] = serde_json::json!({"keep": true});
        let bytes = serde_json::to_vec_pretty(&newer).unwrap();
        fs::write(&path, &bytes).unwrap();

        let adopted = adopt_discovered_externals(
            &[wt_info(&wt, Some("feat/SW-TS-1-x"))],
            &[],
            &cache,
            home.path(),
            &principal,
            "acme",
        );
        assert!(adopted.is_empty());
        assert_eq!(fs::read(path).unwrap(), bytes);
    }

    /// Fix E: a discovered path that exists but is NOT a directory (a stray file where a
    /// worktree would be) is not a worktree → not adopted.
    #[test]
    fn adopt_skips_non_directory_path() {
        let home = TempDir::new().unwrap();
        let principal = Principal::from(1234567u64);
        let (_org, _story, cache) = cache_with_story("SW-TS-1");
        let file_path = home.path().join("not-a-dir");
        fs::write(&file_path, b"x").unwrap();
        let discovered = vec![wt_info(&file_path, Some("feat/SW-TS-1-x"))];

        let adopted =
            adopt_discovered_externals(&discovered, &[], &cache, home.path(), &principal, "acme");

        assert!(adopted.is_empty(), "a non-directory path is not adopted");
        assert!(records_on_disk(home.path(), &principal).is_empty());
    }

    /// A story indexed by id but whose frontmatter carries no usable `source_repo` slug is
    /// a no-op: `backfill_from_branch` returns `Ok(None)`, so adopt writes nothing and
    /// never emits a half-populated row.
    #[test]
    fn adopt_skips_story_without_source_repo() {
        let home = TempDir::new().unwrap();
        let principal = Principal::from(1234567u64);
        let org = TempDir::new().unwrap();
        let content = org.path().join("alpha").join("content");
        fs::create_dir_all(&content).unwrap();
        // `story_id` present, but no `source_repo` line → no derivable slug.
        fs::write(
            content.join("SW-TS-1-x.md"),
            "---\nstatus: todo\nstory_id: SW-TS-1\n---\n# S",
        )
        .unwrap();
        let cache =
            crate::install::frontmatter_cache::FrontmatterCache::build_from_scan(org.path());
        let wt = live_wt(home.path(), "no-slug-wt");
        let discovered = vec![wt_info(&wt, Some("feat/SW-TS-1-x"))];

        let adopted =
            adopt_discovered_externals(&discovered, &[], &cache, home.path(), &principal, "acme");

        assert!(
            adopted.is_empty(),
            "a story with no source_repo slug is not adopted"
        );
        assert!(records_on_disk(home.path(), &principal).is_empty());
    }

    /// AC-1 (row-level): an adopted external renders through the SAME `managed_row_from_record`
    /// path as a native record — a live git worktree + present story file yields a non-orphan
    /// Story row carrying the story id (never the story-linked `orphaned` bandaid path).
    #[test]
    fn adopt_row_renders_as_non_orphan_story() {
        let tmp = TempDir::new().unwrap();
        let home = canon_home(&tmp);
        let clone = home.join("users/1234567/sources/a");
        crate::context_repo::test_fixtures::init_source_clone(&clone, "main");
        let wt = add_worktree(
            &clone,
            "feat/SW-TS-1-live",
            &home.join("users/1234567/wt/a1"),
        );
        let principal = Principal::from(1234567u64);
        let (_org, _story, cache) = cache_with_story("SW-TS-1");

        let discovered = vec![wt_info(&wt, Some("feat/SW-TS-1-live"))];
        let mut adopted =
            adopt_discovered_externals(&discovered, &[], &cache, &home, &principal, "acme");
        assert_eq!(adopted.len(), 1, "the live external is adopted");

        let row = managed_row_from_record(adopted.pop().unwrap(), false);
        assert_eq!(row.story_id, "SW-TS-1");
        assert!(
            !row.orphaned,
            "live worktree + present story file → Story row, not orphaned"
        );
    }

    /// A worktree off a BARE repo: `main_repo_of_worktree` resolves its clone to the bare
    /// dir's (non-git) parent, so `git worktree list` there errors — the AC-4 `run_git`
    /// failure branch (distinct from the unresolvable-clone `None` branch).
    fn bare_repo_worktree(root: &Path) -> PathBuf {
        let seed = root.join("seed");
        crate::context_repo::test_fixtures::init_source_clone(&seed, "main");
        let bare = root.join("bareland").join("bare.git");
        fs::create_dir_all(bare.parent().unwrap()).unwrap();
        // Local bare clone (no network) carries the seed's `main` commit.
        let out = Command::new(git_bin())
            .args([
                "clone",
                "--bare",
                seed.to_str().unwrap(),
                bare.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "bare clone failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let wt = root.join("bare-wt");
        run_git(
            &[
                "worktree",
                "add",
                "-b",
                "bwt",
                "--",
                wt.to_str().unwrap(),
                "main",
            ],
            &bare,
        )
        .unwrap();
        wt
    }

    #[test]
    fn discover_worktrees_in_skips_clone_whose_git_list_fails_keeps_rest() {
        let tmp = TempDir::new().unwrap();
        let home = canon_home(&tmp);
        let clone = home.join("users/1234567/sources/good");
        crate::context_repo::test_fixtures::init_source_clone(&clone, "main");
        let good = add_worktree(&clone, "good", &home.join("users/1234567/wt/good"));
        let bare_wt = bare_repo_worktree(&home);

        let got = discover_worktrees_in(&[bare_wt, good.clone()], &home);

        let paths: HashSet<PathBuf> = got.iter().map(|w| PathBuf::from(&w.path)).collect();
        assert_eq!(
            paths,
            HashSet::from([good]),
            "a git worktree list failure on one clone never blanks the rest"
        );
    }

    // -----------------------------------------------------------------
    // SW-WU-12: fold epic integration worktrees into the discovered set
    // -----------------------------------------------------------------

    /// SW-WU-13 AC-5: on a real assembled-epic layout, the unified `discover_worktrees_in`
    /// — once classified by path shape — surfaces exactly the epic integration worktree,
    /// with the `(epic_id, owner, repo)` leaf parsed out. Pinned to the inline expected
    /// tuple `(normalized path, epic_id, owner, repo)` rather than the SW-WU-8
    /// `enumerate_epic_worktrees` oracle (deleted in this story). Normalizing the path so
    /// git's symlink-resolved porcelain can't diverge from the raw fixture path (the
    /// AC-3.1 canonicalization landmine).
    #[test]
    fn epic_classifier_over_discover_matches_expected_leaf() {
        use crate::content_engine::sync::WaveCoordinator;
        let tmp = TempDir::new().unwrap();
        let home = canon_home(&tmp);
        let principal = Principal::from(1234567u64);
        // A representative TWO-segment org_slug (`owner/repo`, the production shape) —
        // so the leaf carries five segments after `_epic`, exercising the classifier's
        // arity-agnostic last-three parse rather than a fixed offset.
        let (org, epic_id, owner, repo) = (
            "acme/team-context",
            "SW-EW-1",
            "alderco",
            "codesense",
        );

        // A real source clone, and its epic integration worktree laid at the exact
        // `_epic/{org}/{epic_id}/{owner}/{repo}` path the builders compute — a linked
        // worktree off that same clone (the real assembly shape, minus the lockfile).
        let clone = home.join("users/1234567/sources/codesense");
        crate::context_repo::test_fixtures::init_source_clone(&clone, "main");
        let epic_wt = crate::context_repo::path_to_integration_worktree(
            &crate::context_repo::path_to_epic_workspace(&home, &principal, org, epic_id),
            &SourceRepoSlug::new(owner, repo).unwrap(),
        );
        add_worktree(&clone, "integration/SW-EW-1", &epic_wt);

        // The identity tuple both sides reduce to: path is normalized so git's
        // symlink-resolved porcelain can't diverge from the oracle's raw read_dir path.
        let id = |p: &Path, epic_id: &str, owner: &str, repo: &str| {
            (
                WaveCoordinator::normalize_target_path(p),
                epic_id.to_owned(),
                owner.to_owned(),
                repo.to_owned(),
            )
        };

        // Expected: exactly the one epic worktree, keyed on its canonical identity.
        let expected: HashSet<_> = std::iter::once(id(&epic_wt, epic_id, owner, repo)).collect();

        // Unified: discover_worktrees_in over the same clone, filtered + parsed by the
        // path classifier.
        let discovered_set: HashSet<_> = discover_worktrees_in(std::slice::from_ref(&clone), &home)
            .iter()
            .filter_map(|w| {
                let path = PathBuf::from(&w.path);
                let c = crate::context_repo::classify_epic_worktree_path(&path)?;
                Some(id(&path, &c.epic_id, &c.owner, &c.repo))
            })
            .collect();

        assert_eq!(
            discovered_set, expected,
            "classified discover set must be exactly the assembled epic worktree leaf"
        );
    }

    // -----------------------------------------------------------------
    // SW-WU-13: flip authority + unify the row shape
    // -----------------------------------------------------------------

    /// A normalized `HashSet` of the given paths — the `storied` shape the unified
    /// assembly keys off.
    fn norm_set(paths: &[&Path]) -> std::collections::HashSet<PathBuf> {
        use crate::content_engine::sync::WaveCoordinator;
        paths
            .iter()
            .map(|p| WaveCoordinator::normalize_target_path(p))
            .collect()
    }

    /// One `(WorktreeInfo, canonical path)` pair — the present-and-normalized shape
    /// `list_managed_worktrees` builds once and hands to [`discovered_extra_rows`].
    fn present_pair(path: &Path, branch: Option<&str>) -> (WorktreeInfo, PathBuf) {
        use crate::content_engine::sync::WaveCoordinator;
        (
            wt_info(path, branch),
            WaveCoordinator::normalize_target_path(path),
        )
    }

    /// AC-3/AC-4: the discovered fold tags an `_epic/…` path `EpicIntegration` (with the
    /// leaf triple + `workspace_key = epic_id`) and every other managed worktree `Orphan`
    /// (with `workspace_key = parent clone`).
    #[test]
    fn discovered_extra_rows_classifies_epic_and_orphan() {
        let tmp = TempDir::new().unwrap();
        let home = canon_home(&tmp);
        let principal = Principal::from(1234567u64);
        let clone = home.join("users/1234567/sources/codesense");
        crate::context_repo::test_fixtures::init_source_clone(&clone, "main");

        // An epic integration worktree at the exact assembled path, and a plain managed
        // worktree (branch matches no story) that must fall through to Orphan.
        let epic_wt = crate::context_repo::path_to_integration_worktree(
            &crate::context_repo::path_to_epic_workspace(&home, &principal, "acme", "SW-EW-1"),
            &SourceRepoSlug::new("alderco", "codesense").unwrap(),
        );
        add_worktree(&clone, "integration/SW-EW-1", &epic_wt);
        let orphan_wt = add_worktree(
            &clone,
            "feat/hand-made-scratch",
            &home.join("users/1234567/wt/scratch"),
        );
        // The caller passes only present-and-normalized pairs (a `git`-prunable ghost dir
        // is filtered out upstream), so the fold sees exactly the two real worktrees.
        let present = vec![
            present_pair(&epic_wt, Some("integration/SW-EW-1")),
            present_pair(&orphan_wt, Some("feat/hand-made-scratch")),
        ];
        let rows =
            discovered_extra_rows(present, &Default::default(), &Liveness::Known(Vec::new()));

        assert_eq!(rows.len(), 2);
        let epic = rows
            .iter()
            .find(|r| r.kind == WorktreeKind::EpicIntegration)
            .unwrap();
        assert_eq!(epic.epic_id.as_deref(), Some("SW-EW-1"));
        assert_eq!(epic.owner.as_deref(), Some("alderco"));
        assert_eq!(epic.repo.as_deref(), Some("codesense"));
        assert_eq!(epic.workspace_key, "SW-EW-1", "epic groups by epic_id");
        assert_eq!(epic.branch.as_deref(), Some("integration/SW-EW-1"));
        assert!(epic.story_id.is_none(), "epic rows carry no story state");

        let orphan = rows
            .iter()
            .find(|r| r.kind == WorktreeKind::Orphan)
            .unwrap();
        assert!(orphan.story_id.is_none() && orphan.epic_id.is_none());
        assert_eq!(orphan.branch.as_deref(), Some("feat/hand-made-scratch"));
        // Orphans group by the parent clone (canonical) — never the worktree path itself.
        use crate::content_engine::sync::WaveCoordinator;
        let clone_norm = WaveCoordinator::normalize_target_path(&clone);
        assert_eq!(
            WaveCoordinator::normalize_target_path(Path::new(&orphan.workspace_key)),
            clone_norm,
            "orphan workspace_key is the parent clone"
        );
    }

    /// AC-3 dedup: a discovered path already carried by a Story row (`storied`) produces
    /// no extra row — the record wins, so it renders exactly once.
    #[test]
    fn discovered_extra_rows_dedupes_storied_paths() {
        let tmp = TempDir::new().unwrap();
        let home = canon_home(&tmp);
        let clone = home.join("users/1234567/sources/codesense");
        crate::context_repo::test_fixtures::init_source_clone(&clone, "main");
        let wt = add_worktree(
            &clone,
            "feat/SW-TS-1-live",
            &home.join("users/1234567/wt/a1"),
        );

        let present = vec![present_pair(&wt, Some("feat/SW-TS-1-live"))];
        let storied = norm_set(&[&wt]); // already a Story row.
        let rows = discovered_extra_rows(present, &storied, &Liveness::Known(Vec::new()));

        assert!(
            rows.is_empty(),
            "a storied path yields no epic/orphan duplicate"
        );
    }

    /// AC-3: `WorktreeRow::story` wraps an enriched record row as `kind=Story` with
    /// `workspace_key = story_id`, carrying the story fields and no epic fields — the
    /// story enrichment path is reused verbatim, only tagged.
    #[test]
    fn story_row_wraps_managed_row_with_kind_and_key() {
        let tmp = TempDir::new().unwrap();
        let home = canon_home(&tmp);
        let clone = home.join("users/1234567/sources/a");
        crate::context_repo::test_fixtures::init_source_clone(&clone, "main");
        let wt = add_worktree(
            &clone,
            "feat/SW-TS-1-live",
            &home.join("users/1234567/wt/a1"),
        );
        let principal = Principal::from(1234567u64);
        let (_org, _story, cache) = cache_with_story("SW-TS-1");
        let mut adopted = adopt_discovered_externals(
            &[wt_info(&wt, Some("feat/SW-TS-1-live"))],
            &[],
            &cache,
            &home,
            &principal,
            "acme",
        );
        let managed = managed_row_from_record(adopted.pop().unwrap(), false);
        let managed_path = managed.path.clone();

        let row = WorktreeRow::story(managed);
        assert_eq!(row.kind, WorktreeKind::Story);
        assert_eq!(row.workspace_key, "SW-TS-1", "story groups by story_id");
        assert_eq!(row.story_id.as_deref(), Some("SW-TS-1"));
        assert_eq!(row.path, managed_path, "the underlying row is preserved");
        assert!(row.epic_id.is_none() && row.owner.is_none());
    }

    /// A neutral record (empty `story_id`, an unlinkable worktree) falls its
    /// `workspace_key` back to the worktree path — so storyless rows stay one group each
    /// instead of collapsing into a single `""` bucket.
    #[test]
    fn story_row_neutral_record_keys_by_path() {
        let tmp = TempDir::new().unwrap();
        let home = canon_home(&tmp);
        let wt = home.join("users/1234567/wt/neutral");
        let slug = SourceRepoSlug::new("acme", "codesense").unwrap();
        let record = WorktreeStoryRecord::new(&wt, "acme", slug, "", PathBuf::from("s.md"));
        let managed = managed_row_from_record(record, false);

        let row = WorktreeRow::story(managed);
        assert_eq!(
            row.story_id.as_deref(),
            Some(""),
            "the neutral id is preserved"
        );
        assert_eq!(
            row.workspace_key, row.path,
            "an empty story_id keys the group by path, not by \"\""
        );
    }

    /// AC-2 (union guard): a record whose path is in the discovered-present set is
    /// protected from the reap-candidate set — so a live cross-team worktree (present on
    /// disk, outside the active team's discovered set) is never handed to the gone-reap.
    #[test]
    fn partition_reap_candidates_protects_discovered_present() {
        let tmp = TempDir::new().unwrap();
        let home = canon_home(&tmp);
        let slug = SourceRepoSlug::new("acme", "codesense").unwrap();
        let present_wt = home.join("users/1234567/wt/live");
        let other_wt = home.join("users/1234567/wt/cross-team");
        let rec = |p: &Path| {
            WorktreeStoryRecord::new(p, "acme", slug.clone(), "SW-TS-1", PathBuf::from("s.md"))
        };
        let records = vec![rec(&present_wt), rec(&other_wt)];
        let present = norm_set(&[&present_wt]); // only the first is git-discovered.

        let (protected, candidates) = partition_reap_candidates(records, &present);
        assert_eq!(protected.len(), 1);
        assert_eq!(
            protected[0].code_repo_path, present_wt,
            "discovered-present is protected"
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].code_repo_path, other_wt,
            "the non-discovered record is the only reap candidate"
        );
    }

    /// AC-1 (parsing) + AC-3 (rejection): supported current and legacy layouts parse
    /// deterministically by component count; a path with no `_epic` anchor or too few
    /// trailing segments returns `None`. Pure — no fixture needed. (A path nested BELOW
    /// a worktree leaf is out of contract because git only yields worktree roots.)
    #[test]
    fn classify_epic_worktree_path_parses_leaf_and_rejects_the_rest() {
        use crate::context_repo::classify_epic_worktree_path;
        let coords = |p: &str| {
            classify_epic_worktree_path(Path::new(p)).map(|c| (c.epic_id, c.owner, c.repo))
        };
        let leaf = Some(("SW-EW-1".into(), "alderco".into(), "codesense".into()));

        // Two-segment org_slug (production shape): epic_id/owner/repo are the last three.
        assert_eq!(
            coords("/home/u/.codesense/users/42/_epic/acme/team-context/SW-EW-1/alderco/codesense"),
            leaf
        );
        // One-segment org_slug (legacy): the SAME triple — arity-agnostic.
        assert_eq!(
            coords("/home/u/.codesense/users/42/_epic/acme/SW-EW-1/alderco/codesense"),
            leaf
        );
        // No `_epic/` anchor (a story worktree) → None.
        assert_eq!(coords("/home/u/.codesense/users/42/wt/feat-x"), None);
        // `_epic/` present but too few trailing segments to hold org+epic+owner+repo.
        assert_eq!(
            coords("/home/u/.codesense/users/42/_epic/acme/SW-EW-1"),
            None
        );
    }
}
