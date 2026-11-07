// SPDX-License-Identifier: MIT
//! The bundled CodeSense docs context repo (SW-OC-16).
//!
//! Every install ships a real git clone of the CodeSense docs repo as a Tauri resource
//! (`resources/codesense-docs.tar.gz`, built from source by
//! `scripts/build-codesense-docs.mjs`). On launch [`ensure_placed`] puts it under the
//! profile's CodeSense home and registers it through the ordinary attachment seam, so it
//! resolves like any other connected context repo. CodeSense never runs git against it:
//! it only checks the bundled commit out locally with libgit2. Updates arrive offline through the explicit update tool or a newer app's bundle.
//!
//! The docs repo is read-only inside CodeSense. One predicate, [`refuse_write_at`], decides
//! it, and it runs at the shared write chokepoints: the atomic file writers every content,
//! config and registry write goes through (`content_engine::atomic_io`,
//! `shared::fs_atomic`, `shared::yaml_io`), the context write gate for deletes and renames
//! (`commands::files::validate_context_write_path_owned`) and the move between contexts. A
//! new write path inherits the refusal by using those writers. Git-ignored paths (the
//! machine-local state CodeSense keeps beside the docs) stay writable and survive refreshes.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use super::context_attachments::{self, ContextAttachment};
use crate::domain::Principal;

/// Shown whenever an edit to the docs repo is refused.
pub const READ_ONLY_MESSAGE: &str = "CodeSense docs are read-only. To make your own editable copy, choose Create a new context repo… and turn on Start from the CodeSense example.";

/// The folder (below the profile's CodeSense home) the docs repo is placed in.
const PLACED_DIR: &str = "codesense-docs";
/// Records the bundled commit last placed, so a newer bundle replaces only an
/// untouched copy, never one the user's agent has pulled forward.
const PLACED_MARKER: &str = "codesense-docs.bundle";
/// Journal of an install or update in progress: the bundle commit being placed. Present
/// at launch only if a crash interrupted one, which the next launch then completes.
const PENDING_MARKER: &str = "codesense-docs.pending";
/// Paths of the bundled repo the "Start from the CodeSense example" copy leaves out:
/// its registry identity (the new repo gets its own) and the docs-update tooling.
const EXAMPLE_SKIPPED_ROOT_FILES: &[&str] = &["org.yml", "people.yml", "README.md"];
/// The docs-update skill, routine and the agent that runs it only make sense in the docs.
const EXAMPLE_SKIPPED_SEGMENTS: &[&str] = &["update-codesense-docs", "docs-helper"];
/// The team README the copy gets instead of the docs' welcome page. Written as plain
/// markdown so the user can edit it immediately, regardless of what `bundle.readme`
/// resolves to in the shipped docs (SW-ODC-2 made that `README.html`).
const EXAMPLE_README: &str = "\
# Getting started

This context repo started from the CodeSense example. Everything here is yours to edit:
the handbook in `handbook/` and the skills and dashboards in `examples/`. Copy an example
skill into `.claude/skills/` to use it.

Rename the team, add your source repos, and replace these files with your own.
The read-only CodeSense docs stay in the context repo menu for reference.
";
/// Where the example README lands inside a copy. Fixed to `.md` so the user gets
/// editable markdown — the shipped docs' `README.html` is a training-manual dashboard
/// (SW-ODC-2) that only makes sense in the docs org.
const EXAMPLE_README_PATH: &str = "getting-started/content/README.md";

/// `codesense-docs/bundle.json`: the identity the generator and the app share.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleInfo {
    pub slug: String,
    pub team: String,
    /// The README the first launch opens, relative to the repo root.
    pub readme: String,
    pub branch: String,
}

pub fn bundle_info() -> &'static BundleInfo {
    static INFO: OnceLock<BundleInfo> = OnceLock::new();
    INFO.get_or_init(|| {
        serde_json::from_str(include_str!("../../../codesense-docs/bundle.json"))
            .expect("codesense-docs/bundle.json is valid (checked by tests)")
    })
}

pub fn is_docs_slug(slug: &str) -> bool {
    slug == bundle_info().slug
}

/// Where the docs repo lives for `principal`. Per profile, because one checkout can
/// only be registered under one principal.
pub fn docs_root(home: &Path, principal: &Principal) -> PathBuf {
    super::path_to_user_home(home, principal).join(PLACED_DIR)
}

fn marker_path(home: &Path, principal: &Principal) -> PathBuf {
    super::path_to_user_home(home, principal).join(PLACED_MARKER)
}

fn pending_path(home: &Path, principal: &Principal) -> PathBuf {
    super::path_to_user_home(home, principal).join(PENDING_MARKER)
}

/// `path` with `.` and `..` resolved lexically (no I/O), so `docs/x/../y` can't slip past.
fn lexically_normalized(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other),
        }
    }
    normalized
}

/// The placed docs checkout a normalized path lies in, if any: an ancestor named
/// `codesense-docs` directly in a profile's home (`…/users/<principal>/codesense-docs`).
fn placed_root_containing(normalized: &Path) -> Option<&Path> {
    normalized.ancestors().find(|dir| {
        dir.file_name() == Some(std::ffi::OsStr::new(PLACED_DIR))
            && dir
                .parent()
                .and_then(Path::parent)
                .and_then(Path::file_name)
                == Some(std::ffi::OsStr::new(super::USERS_DIR))
    })
}

/// THE read-only rule: refuses a write to `path` when it lands on tracked content of the
/// bundled CodeSense docs. Everything outside the docs passes, and so do the docs' `.git`
/// and git-ignored paths. Fails closed: inside the docs, ignore rules that can't be read
/// refuse.
pub fn refuse_write_at(path: &Path) -> Result<(), String> {
    let normalized = lexically_normalized(path);
    let Some(root) = placed_root_containing(&normalized) else {
        return Ok(());
    };
    // Another checkout that merely sits at that path shape is not the docs.
    if let Ok(bytes) = std::fs::read(root.join("org.yml")) {
        if let Ok(raw) = serde_yaml::from_slice::<crate::domain::RawOrgYml>(&bytes) {
            if !is_docs_slug(&raw.slug) {
                return Ok(());
            }
        }
    }
    let relative = normalized.strip_prefix(root).unwrap_or(Path::new(""));
    if relative.starts_with(".git") {
        return Ok(());
    }
    let ignored = !relative.as_os_str().is_empty()
        && match git2::Repository::open(root) {
            Ok(repo) => repo.status_should_ignore(relative).unwrap_or(false),
            // A checkout this libgit2 can't read (see `place`): machine-local state must still
            // work, so judge by the ignore rules the docs ship with.
            Err(_) => shipped_ignore().is_match(relative),
        };
    if ignored {
        Ok(())
    } else {
        Err(READ_ONLY_MESSAGE.to_string())
    }
}

/// The docs' shipped `.gitignore` as globs: the fallback when libgit2 can't read the
/// checkout. Covers the forms that file uses: `dir/` (and everything under it), `*/x/`
/// (one level down), plain names and `*.ext` (at any depth).
fn shipped_ignore() -> &'static globset::GlobSet {
    static SET: OnceLock<globset::GlobSet> = OnceLock::new();
    SET.get_or_init(|| {
        let mut set = globset::GlobSetBuilder::new();
        for line in include_str!("../../../codesense-docs/.gitignore").lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (pattern, dir) = match line.strip_suffix('/') {
                Some(stem) => (stem.trim_start_matches('/'), true),
                None => (line.trim_start_matches('/'), false),
            };
            let anchored = pattern.contains('/') || line.starts_with('/');
            let base = if anchored {
                pattern.to_string()
            } else {
                format!("**/{pattern}")
            };
            let mut globs = vec![base.clone()];
            if dir || !pattern.contains('.') {
                globs.push(format!("{base}/**"));
            }
            for glob in globs {
                if let Ok(glob) = globset::GlobBuilder::new(&glob)
                    .literal_separator(true)
                    .build()
                {
                    set.add(glob);
                }
            }
        }
        set.build().unwrap_or_else(|_| globset::GlobSet::empty())
    })
}

/// [`refuse_write_at`] as an `io::Error`, for the file-writing primitives.
pub fn refuse_io_write_at(path: &Path) -> std::io::Result<()> {
    refuse_write_at(path)
        .map_err(|message| std::io::Error::new(std::io::ErrorKind::PermissionDenied, message))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// First placement on this profile.
    Placed,
    /// An untouched older copy was replaced by this app's newer bundle.
    Updated,
    /// The placed copy was kept (current, pulled forward, or locally changed).
    Kept,
}

#[derive(Debug)]
pub struct PlacedDocs {
    pub attachment: ContextAttachment,
    pub readme_path: PathBuf,
    pub placement: Placement,
}

static PLACEMENT_LOCK: Mutex<()> = Mutex::new(());

/// Places (or refreshes) the bundled docs repo for `principal` and registers it.
/// Idempotent; serialized process-wide so concurrent boots never race an extraction.
/// Crash-safe: every install or update is journaled, and the next launch completes one a
/// crash interrupted.
pub fn ensure_placed(
    home: &Path,
    principal: &Principal,
    archive: &[u8],
) -> Result<PlacedDocs, String> {
    let _guard = PLACEMENT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = docs_root(home, principal);
    let journal = Journal {
        bundled: archive_commit(archive)?,
        marker: marker_path(home, principal),
        pending: pending_path(home, principal),
    };
    let placement = place(&root, archive, &journal)?;
    let attachment = attach_or_registered(home, principal, &root)?;
    let readme_path = attachment.root.join(&bundle_info().readme);
    Ok(PlacedDocs {
        attachment,
        readme_path,
        placement,
    })
}

/// The docs bundle is immutable for the lifetime of a running app.
pub fn bundled_archive(app: &tauri::AppHandle) -> Result<&'static [u8], String> {
    use tauri::Manager;
    static ARCHIVE: OnceLock<Result<Vec<u8>, String>> = OnceLock::new();
    ARCHIVE
        .get_or_init(|| {
            let path = app
                .path()
                .resolve(
                    "resources/codesense-docs.tar.gz",
                    tauri::path::BaseDirectory::Resource,
                )
                .map_err(|e| format!("Cannot locate the bundled CodeSense docs: {e}"))?;
            std::fs::read(path).map_err(|e| format!("Cannot read the bundled CodeSense docs: {e}"))
        })
        .as_ref()
        .map(Vec::as_slice)
        .map_err(Clone::clone)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocsVersion {
    pub bundled_commit: String,
    pub placed_commit: Option<String>,
    pub app_version: &'static str,
    pub state: &'static str,
    pub action: &'static str,
}

fn changed_paths(repo: &git2::Repository) -> Result<Vec<String>, String> {
    let mut options = git2::StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false);
    let statuses = repo
        .statuses(Some(&mut options))
        .map_err(|e| e.to_string())?;
    Ok(statuses
        .iter()
        .filter_map(|s| Result::ok(s.path()).map(str::to_owned))
        .collect())
}

/// Collect on a blocking worker; includes staged, tracked and nonignored untracked work.
pub fn version(home: &Path, principal: &Principal, archive: &[u8]) -> Result<DocsVersion, String> {
    let _guard = PLACEMENT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    version_unlocked(home, principal, archive)
}

fn version_unlocked(
    home: &Path,
    principal: &Principal,
    archive: &[u8],
) -> Result<DocsVersion, String> {
    let bundled_commit = archive_commit(archive)?;
    let root = docs_root(home, principal);
    let (placed_commit, modified) = if root.exists() {
        let repo = git2::Repository::open(&root).map_err(|e| e.to_string())?;
        let head = repo
            .head()
            .and_then(|h| h.peel_to_commit())
            .map_err(|e| e.to_string())?
            .id()
            .to_string();
        let local_history =
            Journal::read(&marker_path(home, principal)).as_deref() != Some(head.as_str());
        (
            Some(head),
            !changed_paths(&repo)?.is_empty() || local_history,
        )
    } else {
        (None, false)
    };
    let state = match (placed_commit.as_deref(), modified) {
        (None, _) => "missing",
        (Some(head), false) if head == bundled_commit => "current",
        (Some(head), true) if head == bundled_commit => "modified",
        (Some(_), false) => "behind",
        (Some(_), true) => "behind-and-modified",
    };
    Ok(DocsVersion {
        bundled_commit,
        placed_commit,
        app_version: env!("CARGO_PKG_VERSION"),
        state,
        action: if state == "current" {
            "Docs match this app's bundle."
        } else {
            "Call update_codesense_docs to select this app's offline bundle; local work is preserved on a named branch."
        },
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocsUpdate {
    pub docs: DocsVersion,
    pub root: PathBuf,
    pub preservation_branch: Option<String>,
    pub changed_paths: Vec<String>,
}

/// Explicit offline replacement. Preserve history and both index/working snapshots first.
pub fn update(home: &Path, principal: &Principal, archive: &[u8]) -> Result<DocsUpdate, String> {
    let _guard = PLACEMENT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = docs_root(home, principal);
    let before = version_unlocked(home, principal, archive)?;
    let mut preservation_branch = None;
    let mut overwrite = Overwrite::Nothing;
    let mut paths = Vec::new();
    if root.exists() {
        let repo = git2::Repository::open(&root).map_err(|e| e.to_string())?;
        paths = changed_paths(&repo)?;
        if before.state != "current" {
            let head = repo
                .head()
                .and_then(|h| h.peel_to_commit())
                .map_err(|e| e.to_string())?;
            let sig = git2::Signature::now("CodeSense docs preservation", "docs@codesense.ai")
                .map_err(|e| e.to_string())?;
            let mut index = repo.index().map_err(|e| e.to_string())?;
            let staged_tree = index
                .write_tree()
                .and_then(|id| repo.find_tree(id))
                .map_err(|e| e.to_string())?;
            let snapshot = |tree: &git2::Tree, parent: &git2::Commit, message: &str| {
                if tree.id() == parent.tree_id() {
                    Ok(parent.id())
                } else {
                    repo.commit(None, &sig, &sig, message, tree, &[parent])
                }
            };
            let staged = snapshot(&staged_tree, &head, "Preserve staged docs content")
                .map_err(|e| e.to_string())?;
            index.update_all(["*"], None).map_err(|e| e.to_string())?;
            index
                .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
                .map_err(|e| e.to_string())?;
            let working = repo
                .find_tree(index.write_tree().map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            let parent = repo.find_commit(staged).map_err(|e| e.to_string())?;
            let saved = snapshot(&working, &parent, "Preserve working docs content")
                .map_err(|e| e.to_string())?;
            let name = format!(
                "codesense-docs-preserved-{}",
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
            );
            repo.branch(
                &name,
                &repo.find_commit(saved).map_err(|e| e.to_string())?,
                false,
            )
            .map_err(|e| e.to_string())?;
            preservation_branch = Some(name);
            overwrite = Overwrite::Preserved(working.id());
        }
    }
    let apply = || -> Result<(), String> {
        if root.exists() {
            if before.state != "current" {
                match update_in_place(archive, &root, &before.bundled_commit, overwrite)?
                {
                    UpdateOutcome::Applied => {}
                    UpdateOutcome::Blocked(path) => {
                        return Err(format!(
                            "Local path would be overwritten or removed without a saved snapshot: {}; its bytes were left in place",
                            path.display()
                        ))
                    }
                }
            }
            remove_origin(&git2::Repository::open(&root).map_err(|e| e.to_string())?)?;
        } else {
            install(archive, &root)?;
        }
        let journal = Journal {
            bundled: before.bundled_commit.clone(),
            marker: marker_path(home, principal),
            pending: pending_path(home, principal),
        };
        journal.finish()?;
        attach_or_registered(home, principal, &root)?;
        Ok(())
    };
    let docs = apply()
        .and_then(|_| version_unlocked(home, principal, archive))
        .map_err(|e| match &preservation_branch {
            Some(branch) => format!(
                "{e}. HEAD/history, staged content and nonignored working content remain on preservation branch {branch}; changed paths: {}",
                paths.join(", ")
            ),
            None => e,
        })?;
    Ok(DocsUpdate {
        docs,
        root,
        preservation_branch,
        changed_paths: paths,
    })
}

fn remove_origin(repo: &git2::Repository) -> Result<(), String> {
    if repo.find_remote("origin").is_ok() {
        repo.remote_delete("origin").map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The placement bookkeeping beside the docs (never inside them).
struct Journal {
    bundled: String,
    marker: PathBuf,
    pending: PathBuf,
}

impl Journal {
    fn read(path: &Path) -> Option<String> {
        std::fs::read_to_string(path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn recorded(&self) -> Option<String> {
        Self::read(&self.marker)
    }

    fn interrupted(&self) -> Option<String> {
        Self::read(&self.pending)
    }

    /// Before any change to the docs: the bundle commit being placed.
    fn begin(&self) -> Result<(), String> {
        crate::shared::fs_atomic::write_bytes_atomic(
            &self.pending,
            format!("{}\n", self.bundled).as_bytes(),
        )
        .map_err(|e| format!("Cannot record the CodeSense docs update: {e}"))
    }

    /// After the docs are fully placed: record the version, then drop the journal.
    fn finish(&self) -> Result<(), String> {
        crate::shared::fs_atomic::write_bytes_atomic(
            &self.marker,
            format!("{}\n", self.bundled).as_bytes(),
        )
        .map_err(|e| format!("Cannot record the CodeSense docs version: {e}"))?;
        self.abandon();
        Ok(())
    }

    fn abandon(&self) {
        let _ = std::fs::remove_file(&self.pending);
    }
}

fn place(root: &Path, archive: &[u8], journal: &Journal) -> Result<Placement, String> {
    if let Some(pending) = journal.interrupted() {
        if let Some(placement) = complete_interrupted(root, archive, journal, &pending)? {
            return Ok(placement);
        }
    }
    if root.exists() && clearly_not_a_repo(root) {
        set_aside(root)?;
    }
    if !root.exists() {
        journal.begin()?;
        install(archive, root)?;
        journal.finish()?;
        return Ok(Placement::Placed);
    }
    let repo = match git2::Repository::open(root) {
        Ok(repo) => repo,
        Err(e) => {
            // A real checkout this libgit2 can't read (say, a newer git's reftable or
            // sha256 format after the user's own git touched it). It's theirs: keep it.
            log::warn!(
                "CodeSense docs at {} can't be read by this version ({e}); leaving them as they are",
                root.display()
            );
            return Ok(Placement::Kept);
        }
    };
    if !should_update(&repo, journal) {
        return Ok(Placement::Kept);
    }
    journal.begin()?;
    match update_in_place(archive, root, &journal.bundled, Overwrite::Nothing)? {
        UpdateOutcome::Applied => {
            journal.finish()?;
            Ok(Placement::Updated)
        }
        UpdateOutcome::Blocked(path) => {
            log::warn!(
                "Not updating the CodeSense docs: {} isn't tracked and the update would overwrite or remove it",
                path.display()
            );
            journal.abandon();
            Ok(Placement::Kept)
        }
    }
}

/// A crash interrupted placing bundle `pending` (an install, or an update from the
/// recorded commit). Decide from where HEAD is now:
/// - at this app's bundle: the update finished up to the refs; record it.
/// - at `pending` (an older app's bundle, refs moved, version not recorded): record
///   `pending`, then carry on with a normal update to this app's bundle.
/// - at the recorded commit: our own write was cut short, so any difference in the tree is
///   ours; place this app's bundle over it (it supersedes an older `pending`).
/// - anywhere else: the user's agent moved the checkout on since; it's theirs. Drop the
///   journal, touch nothing.
///
/// `Ok(None)` means: continue with a normal placement.
fn complete_interrupted(
    root: &Path,
    archive: &[u8],
    journal: &Journal,
    pending: &str,
) -> Result<Option<Placement>, String> {
    if !root.exists() || clearly_not_a_repo(root) {
        // An install that never landed; a normal placement installs afresh.
        journal.abandon();
        return Ok(None);
    }
    let head = git2::Repository::open(root)
        .ok()
        .and_then(|repo| repo.head().ok().and_then(|h| h.target()))
        .map(|id| id.to_string());
    let Some(head) = head else {
        journal.abandon();
        return Ok(None);
    };
    if head == journal.bundled {
        journal.finish()?;
        return Ok(Some(Placement::Updated));
    }
    if head == pending {
        crate::shared::fs_atomic::write_bytes_atomic(
            &journal.marker,
            format!("{pending}\n").as_bytes(),
        )
        .map_err(|e| format!("Cannot record the CodeSense docs version: {e}"))?;
        journal.abandon();
        return Ok(None);
    }
    if journal.recorded().as_deref() == Some(head.as_str()) {
        journal.begin()?;
        update_in_place(
            archive,
            root,
            &journal.bundled,
            Overwrite::OwnInterruptedWork,
        )?;
        journal.finish()?;
        return Ok(Some(Placement::Updated));
    }
    log::warn!(
        "CodeSense docs at {} moved on to {head} after an interrupted update; leaving them as they are",
        root.display()
    );
    journal.abandon();
    Ok(Some(Placement::Kept))
}

/// Only a folder with no git checkout at all is set aside; anything that looks like a
/// repository stays where it is, even when this libgit2 can't open it.
fn clearly_not_a_repo(root: &Path) -> bool {
    !root.join(".git").join("HEAD").is_file()
}

/// Registers the docs, or keeps an existing registration of this same folder when the
/// checkout can't be validated right now (see [`place`]).
fn attach_or_registered(
    home: &Path,
    principal: &Principal,
    root: &Path,
) -> Result<ContextAttachment, String> {
    match context_attachments::attach(home, principal, root) {
        Ok(attachment) => Ok(attachment),
        Err(e) => {
            let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
            context_attachments::list(home, principal)?
                .into_iter()
                .find(|a| a.root == canonical && is_docs_slug(&a.slug))
                .ok_or(e)
        }
    }
}

/// Update only a copy still at the last placed bundle commit with no edits to tracked
/// files or nonignored untracked additions. Local work waits for an explicit update.
fn should_update(repo: &git2::Repository, journal: &Journal) -> bool {
    let Some(recorded) = journal.recorded() else {
        return false;
    };
    let head = repo
        .head()
        .ok()
        .and_then(|h| h.target())
        .map(|id| id.to_string());
    if head.as_deref() != Some(recorded.as_str()) || recorded == journal.bundled {
        return false;
    }
    let mut options = git2::StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false);
    repo.statuses(Some(&mut options))
        .map(|statuses| statuses.is_empty())
        .unwrap_or(false)
}

fn install(archive: &[u8], root: &Path) -> Result<(), String> {
    let parent = root.parent().ok_or("Invalid CodeSense docs location")?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("Cannot create {}: {e}", parent.display()))?;
    let staging = tempfile::Builder::new()
        .prefix(".codesense-docs-")
        .tempdir_in(parent)
        .map_err(|e| format!("Cannot stage the CodeSense docs: {e}"))?;
    let staged = staging.path().join(PLACED_DIR);
    materialize(archive, &staged)?;
    std::fs::rename(&staged, root)
        .map_err(|e| format!("Cannot place the CodeSense docs at {}: {e}", root.display()))
}

/// Which local files an update may overwrite.
#[derive(Clone, Copy)]
enum Overwrite {
    /// Only index-tracked paths: unsaved local content blocks checkout.
    Nothing,
    /// Completing an interrupted update of the same bundle: its half-written files are
    /// its own.
    OwnInterruptedWork,
    /// The working tree saved on the preservation branch before replacement.
    Preserved(git2::Oid),
}

enum UpdateOutcome {
    Applied,
    /// Local content checkout would overwrite or remove without a saved snapshot.
    Blocked(PathBuf),
}

/// Moves the newer bundle's commit into the placed checkout, at the same path (the
/// registration stays valid). Order is crash-safe: objects first, then the working tree,
/// then the refs LAST, so an interrupted update never leaves HEAD on a commit whose
/// objects are missing; the journal lets the next launch finish it. Ignored files (the
/// state CodeSense or agents keep there) are never touched.
fn update_in_place(
    archive: &[u8],
    root: &Path,
    bundled: &str,
    overwrite: Overwrite,
) -> Result<UpdateOutcome, String> {
    let fail = |e: &dyn std::fmt::Display| format!("Cannot update the CodeSense docs: {e}");
    let objects = root.join(".git").join("objects");
    for (relative, bytes) in archive_entries(archive)? {
        let Ok(inside) = relative.strip_prefix(".git/objects") else {
            continue;
        };
        let target = objects.join(inside);
        if target.exists() {
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| fail(&e))?;
        }
        std::fs::write(&target, bytes).map_err(|e| fail(&e))?;
    }
    let repo = git2::Repository::open(root).map_err(|e| fail(&e))?;
    let oid = git2::Oid::from_str(bundled).map_err(|e| fail(&e))?;
    let tree = repo
        .find_commit(oid)
        .and_then(|commit| commit.tree())
        .map_err(|e| fail(&e))?;
    let baseline = repo
        .head()
        .and_then(|head| head.peel_to_tree())
        .map_err(|e| fail(&e))?;
    let current = repo.index().map_err(|e| fail(&e))?;
    let saved = match overwrite {
        Overwrite::Preserved(id) => Some(repo.find_tree(id).map_err(|e| fail(&e))?),
        _ => None,
    };
    let tree_paths = |candidate: &git2::Tree| {
        let mut paths = std::collections::BTreeSet::new();
        candidate.walk(git2::TreeWalkMode::PreOrder, |dir, entry| {
            paths.insert(PathBuf::from(format!(
                "{dir}{}",
                entry.name().unwrap_or_default()
            )));
            git2::TreeWalkResult::Ok
        })?;
        Ok::<_, git2::Error>(paths)
    };
    let mut touched = tree_paths(&baseline).map_err(|e| fail(&e))?;
    touched.extend(tree_paths(&tree).map_err(|e| fail(&e))?);
    // Include tree paths: a local file can obstruct an incoming directory. A directory
    // at a file path is not an indexed/saved leaf; its ignored descendants may be unsaved.
    for path in touched {
        let metadata = match root.join(&path).symlink_metadata() {
            Ok(metadata) => metadata,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                continue
            }
            Err(e) => return Err(fail(&e)),
        };
        let is_leaf = !metadata.is_dir();
        if !is_leaf
            && [&baseline, &tree].iter().all(|candidate| {
                candidate
                    .get_path(&path)
                    .ok()
                    .and_then(|entry| entry.kind())
                    != Some(git2::ObjectType::Blob)
            })
        {
            continue;
        }
        let saved_or_owned = match overwrite {
            Overwrite::Preserved(_) => {
                is_leaf
                    && saved.as_ref().is_some_and(|tree| {
                        tree.get_path(&path).ok().and_then(|entry| entry.kind())
                            == Some(git2::ObjectType::Blob)
                    })
            }
            // The journal identifies nonignored half-written files as our own work.
            Overwrite::OwnInterruptedWork => {
                !repo.status_should_ignore(&path).map_err(|e| fail(&e))?
            }
            Overwrite::Nothing => false,
        };
        if !(is_leaf && current.get_path(&path, 0).is_some() || saved_or_owned) {
            return Ok(UpdateOutcome::Blocked(path));
        }
    }
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.force().remove_ignored(false);
    repo.checkout_tree(tree.as_object(), Some(&mut checkout))
        .map_err(|e| fail(&e))?;
    if let Some(saved) = &saved {
        let mut empty_parents = std::collections::BTreeSet::new();
        for path in tree_paths(saved).map_err(|e| fail(&e))? {
            if saved.get_path(&path).ok().and_then(|entry| entry.kind())
                != Some(git2::ObjectType::Blob)
                || tree.get_path(&path).is_ok()
            {
                continue;
            }
            let full = root.join(&path);
            match full.symlink_metadata() {
                Ok(metadata) if metadata.is_file() || metadata.file_type().is_symlink() => {
                    std::fs::remove_file(&full).map_err(|e| fail(&e))?
                }
                Ok(_) => continue,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) => {}
                Err(e) => return Err(fail(&e)),
            }
            for parent in path
                .ancestors()
                .skip(1)
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                empty_parents.insert(parent.to_path_buf());
            }
        }
        // Empty-only removal leaves ignored siblings and fresh bundle contents intact.
        for parent in empty_parents.iter().rev() {
            let _ = std::fs::remove_dir(root.join(parent));
        }
    }
    let mut index = repo.index().map_err(|e| fail(&e))?;
    index.read_tree(&tree).map_err(|e| fail(&e))?;
    index.write().map_err(|e| fail(&e))?;
    let branch = format!("refs/heads/{}", bundle_info().branch);
    let on_bundle_branch = repo
        .head()
        .map(|head| head.name() == Ok(branch.as_str()))
        .map_err(|e| fail(&e))?;
    if on_bundle_branch {
        repo.reference(&branch, oid, true, "CodeSense docs update")
            .map_err(|e| fail(&e))?;
        repo.set_head(&branch).map_err(|e| fail(&e))?;
    } else {
        // An inactive bundle branch can hold work absent from this checkout. Keep its ref.
        repo.set_head_detached(oid).map_err(|e| fail(&e))?;
    }
    remove_origin(&repo)?;
    Ok(UpdateOutcome::Applied)
}

fn set_aside(root: &Path) -> Result<(), String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let aside = root.with_file_name(format!("{PLACED_DIR}.damaged-{stamp}"));
    std::fs::rename(root, &aside).map_err(|e| {
        format!(
            "The CodeSense docs at {} are damaged and could not be moved aside: {e}",
            root.display()
        )
    })
}

/// Writes the archived `.git` into `dir` and checks out its branch.
fn materialize(archive: &[u8], dir: &Path) -> Result<(), String> {
    for (relative, bytes) in archive_entries(archive)? {
        let target = dir.join(&relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&target, bytes).map_err(|e| e.to_string())?;
    }
    let repo = git2::Repository::open(dir)
        .map_err(|e| format!("The bundled CodeSense docs are not a git repository: {e}"))?;
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.force();
    repo.checkout_head(Some(&mut checkout))
        .map_err(|e| format!("Cannot check out the bundled CodeSense docs: {e}"))
}

/// Every `.git/…` file in the archive, refusing any path that could escape.
fn archive_entries(archive: &[u8]) -> Result<Vec<(PathBuf, Vec<u8>)>, String> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    let mut out = Vec::new();
    for entry in tar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path().map_err(|e| e.to_string())?.into_owned();
        let safe = path.starts_with(".git")
            && path.components().all(|c| matches!(c, Component::Normal(_)));
        if !safe {
            return Err(format!(
                "Unexpected path in the CodeSense docs bundle: {}",
                path.display()
            ));
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        out.push((path, bytes));
    }
    Ok(out)
}

/// The commit the bundle's branch points at.
fn archive_commit(archive: &[u8]) -> Result<String, String> {
    let branch = PathBuf::from(format!(".git/refs/heads/{}", bundle_info().branch));
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in tar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        if entry.path().map_err(|e| e.to_string())?.as_ref() == branch {
            let mut commit = String::new();
            entry
                .read_to_string(&mut commit)
                .map_err(|e| e.to_string())?;
            return Ok(commit.trim().to_string());
        }
    }
    Err("The CodeSense docs bundle has no branch".into())
}

/// Copies the docs repo's committed tree into `dest` for "Start from the CodeSense
/// example", minus its identity files and docs-update tooling. Reads git objects only.
pub fn copy_example(docs_root: &Path, dest: &Path) -> Result<(), String> {
    let unavailable = |e: git2::Error| {
        format!("The CodeSense example isn't available: {e}. Restart CodeSense and try again.")
    };
    let repo = git2::Repository::open(docs_root).map_err(unavailable)?;
    let tree = repo
        .head()
        .and_then(|head| head.peel_to_tree())
        .map_err(unavailable)?;
    let mut failure: Option<String> = None;
    tree.walk(git2::TreeWalkMode::PreOrder, |dir, entry| {
        if entry.kind() != Some(git2::ObjectType::Blob) {
            return git2::TreeWalkResult::Ok;
        }
        let name = entry.name().unwrap_or_default();
        let relative = format!("{dir}{name}");
        if (dir.is_empty() && EXAMPLE_SKIPPED_ROOT_FILES.contains(&name))
            || relative == bundle_info().readme
            || relative
                .split('/')
                .any(|part| EXAMPLE_SKIPPED_SEGMENTS.contains(&part))
        {
            return git2::TreeWalkResult::Ok;
        }
        let written = repo
            .find_blob(entry.id())
            .map_err(|e| e.to_string())
            .and_then(|blob| {
                let target = dest.join(&relative);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                std::fs::write(&target, blob.content()).map_err(|e| e.to_string())
            });
        match written {
            Ok(()) => git2::TreeWalkResult::Ok,
            Err(e) => {
                failure = Some(format!(
                    "Cannot copy the CodeSense example ({relative}): {e}"
                ));
                git2::TreeWalkResult::Abort
            }
        }
    })
    .map_err(|e| failure.clone().unwrap_or_else(|| e.to_string()))?;
    if let Some(e) = failure {
        return Err(e);
    }
    let readme = dest.join(EXAMPLE_README_PATH);
    if let Some(parent) = readme.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&readme, EXAMPLE_README)
        .map_err(|e| format!("Cannot write the example README: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The archive this build bundles (generated by build.rs).
    fn real_bundle() -> &'static [u8] {
        include_bytes!("../../resources/codesense-docs.tar.gz")
    }

    /// A `bundle.json` field the app itself never reads.
    fn bundle_field(name: &str) -> String {
        let value: serde_json::Value =
            serde_json::from_str(include_str!("../../../codesense-docs/bundle.json")).unwrap();
        value[name].as_str().unwrap().to_string()
    }

    /// A one-commit bundle built with libgit2, so tests can make an "older" and a
    /// "newer" bundle without the generator.
    fn bundle_with(readme: &str) -> Vec<u8> {
        bundle_with_files(readme, &[])
    }

    fn bundle_with_files(readme: &str, extra: &[(&str, &str)]) -> Vec<u8> {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let info = bundle_info();
        let repo = git2::Repository::init_opts(
            root,
            git2::RepositoryInitOptions::new().initial_head(&info.branch),
        )
        .unwrap();
        let files = [
            (
                "org.yml".to_string(),
                format!(
                    "slug: {}\ndisplay_name: CodeSense Docs\ncreated_at: 2026-10-01T00:00:00Z\n",
                    info.slug
                ),
            ),
            ("people.yml".to_string(), "people: []\n".to_string()),
            (
                "teams.yml".to_string(),
                format!("teams:\n- name: {}\n  source_repos: []\n", info.team),
            ),
            (info.readme.clone(), readme.to_string()),
        ];
        let extra = extra.iter().map(|(p, b)| (p.to_string(), b.to_string()));
        let mut index = repo.index().unwrap();
        for (path, body) in files.into_iter().chain(extra) {
            let full = root.join(&path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(&full, body).unwrap();
            index.add_path(Path::new(&path)).unwrap();
        }
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig =
            git2::Signature::new("CodeSense", "docs@codesense.ai", &git2::Time::new(0, 0)).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "docs", &tree, &[])
            .unwrap();
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::default(),
        ));
        builder.append_dir_all(".git", root.join(".git")).unwrap();
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn head(root: &Path) -> String {
        git2::Repository::open(root)
            .unwrap()
            .head()
            .unwrap()
            .target()
            .unwrap()
            .to_string()
    }

    #[test]
    fn saved_untracked_files_do_not_remove_ignored_siblings() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let who = Principal::from(7_u64);
        let placed = ensure_placed(
            &home,
            &who,
            &bundle_with_files("v1\n", &[(".gitignore", "*.sqlite3\n")]),
        )
        .unwrap();
        let root = &placed.attachment.root;
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::write(root.join("notes/mine.md"), "my notes").unwrap();
        std::fs::write(root.join("notes/cache.sqlite3"), "machine state").unwrap();
        std::fs::create_dir_all(root.join("drafts")).unwrap();
        std::fs::write(root.join("drafts/mine.md"), "draft").unwrap();
        let updated = update(
            &home,
            &who,
            &bundle_with_files(
                "v2\n",
                &[
                    (".gitignore", "*.sqlite3\n"),
                    ("notes/from-bundle.md", "new bundle file"),
                ],
            ),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(root.join("notes/cache.sqlite3")).unwrap(),
            b"machine state"
        );
        assert!(!root.join("notes/mine.md").exists());
        assert!(!root.join("drafts").exists());
        assert_eq!(
            std::fs::read(root.join("notes/from-bundle.md")).unwrap(),
            b"new bundle file"
        );
        let repo = git2::Repository::open(root).unwrap();
        let saved = repo
            .find_branch(
                &updated.preservation_branch.unwrap(),
                git2::BranchType::Local,
            )
            .unwrap()
            .get()
            .peel_to_tree()
            .unwrap();
        let blob = repo
            .find_blob(saved.get_path(Path::new("notes/mine.md")).unwrap().id())
            .unwrap();
        assert_eq!(blob.content(), b"my notes");
    }

    #[test]
    fn a_directory_is_not_an_indexed_or_saved_leaf_authorizing_replacement() {
        for keep_index_entry in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().join("home");
            let who = Principal::from(7_u64);
            let placed = ensure_placed(
                &home,
                &who,
                &bundle_with_files(
                    "v1\n",
                    &[(".gitignore", "*.sqlite3\n"), ("notes", "old bundle file")],
                ),
            )
            .unwrap();
            let root = &placed.attachment.root;
            let repo = git2::Repository::open(root).unwrap();
            std::fs::remove_file(root.join("notes")).unwrap();
            std::fs::create_dir_all(root.join("notes")).unwrap();
            std::fs::write(root.join("notes/mine.md"), "my notes").unwrap();
            std::fs::write(root.join("notes/cache.sqlite3"), "machine state").unwrap();
            if !keep_index_entry {
                let mut index = repo.index().unwrap();
                index.remove_path(Path::new("notes")).unwrap();
                index.write().unwrap();
            }
            let result = update(
                &home,
                &who,
                &bundle_with_files(
                    "v2\n",
                    &[(".gitignore", "*.sqlite3\n"), ("notes", "new bundle file")],
                ),
            );
            let error = result.expect_err("a directory with unsaved bytes must block replacement");
            assert!(error.contains("notes"), "{error}");
            assert_eq!(
                std::fs::read(root.join("notes/cache.sqlite3")).unwrap(),
                b"machine state"
            );
            assert_eq!(
                std::fs::read(root.join("notes/mine.md")).unwrap(),
                b"my notes"
            );
        }
    }

    #[test]
    fn an_ignored_file_blocks_an_incoming_bundle_directory() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let who = Principal::from(7_u64);
        let placed = ensure_placed(
            &home,
            &who,
            &bundle_with_files("v1\n", &[(".gitignore", "notes\n")]),
        )
        .unwrap();
        let root = &placed.attachment.root;
        std::fs::write(root.join("notes"), "machine state").unwrap();
        let result = update(
            &home,
            &who,
            &bundle_with_files("v2\n", &[("notes/from-bundle.md", "new bundle file")]),
        );
        let error = result.expect_err("an unsaved ignored file must block an incoming directory");
        assert!(error.contains("notes"), "{error}");
        assert_eq!(std::fs::read(root.join("notes")).unwrap(), b"machine state");
    }

    #[test]
    fn helper_can_run_its_offline_workflow_headlessly() {
        use codesense_pty_core::agent_cli::AgentCli;
        let helper: crate::agents::model::Agent = serde_yaml::from_str(include_str!(
            "../../../codesense-docs/getting-started/config/agents/docs-helper/agent.yaml"
        ))
        .unwrap();
        helper.validate().unwrap();
        let settings = crate::agents::permissions::generate(
            &helper,
            AgentCli::Claude,
            crate::agents::model::LaunchMode::Headless,
        );
        let permissions = &settings[0].content["permissions"];
        assert_eq!(permissions["defaultMode"], "dontAsk");
        let allow = permissions["allow"].as_array().unwrap();
        for tool in [
            "mcp__codesense__codesense_overview",
            "mcp__codesense__update_codesense_docs",
            "Read",
            "Skill(update-codesense-docs)",
        ] {
            assert!(
                allow.iter().any(|value| value == tool),
                "headless helper cannot call {tool}: {permissions}"
            );
        }
    }

    #[test]
    fn update_and_later_startup_keep_main_history_when_another_branch_is_active() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let who = Principal::from(7_u64);
        let placed = ensure_placed(&home, &who, &bundle_with("v1\n")).unwrap();
        let root = &placed.attachment.root;
        let repo = git2::Repository::open(root).unwrap();
        let original = repo.head().unwrap().peel_to_commit().unwrap();
        std::fs::write(&placed.readme_path, "main-only user work\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(&bundle_info().readme)).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("me", "me@example.com").unwrap();
        let main_work = repo
            .commit(
                Some("HEAD"),
                &sig,
                &sig,
                "main-only work",
                &tree,
                &[&original],
            )
            .unwrap();
        repo.branch("other", &original, false).unwrap();
        repo.set_head("refs/heads/other").unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
            .unwrap();
        let v2 = bundle_with("v2\n");
        let updated = update(&home, &who, &v2).unwrap();
        assert!(updated.preservation_branch.is_some());
        assert_eq!(
            repo.find_branch(&bundle_info().branch, git2::BranchType::Local)
                .unwrap()
                .get()
                .target(),
            Some(main_work)
        );
        assert_eq!(head(root), archive_commit(&v2).unwrap());
        assert!(repo.head_detached().unwrap());
        let v3 = bundle_with("v3\n");
        assert_eq!(
            ensure_placed(&home, &who, &v3).unwrap().placement,
            Placement::Updated
        );
        assert_eq!(
            repo.find_branch(&bundle_info().branch, git2::BranchType::Local)
                .unwrap()
                .get()
                .target(),
            Some(main_work)
        );
        assert_eq!(head(root), archive_commit(&v3).unwrap());
        assert!(repo.head_detached().unwrap());
    }

    #[test]
    fn explicit_update_preserves_history_index_working_deletions_and_untracked_files() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let who = Principal::from(7_u64);
        let v1 = bundle_with_files(
            "v1\n",
            &[("deleted.md", "original"), (".gitignore", "cache/\n")],
        );
        let placed = ensure_placed(&home, &who, &v1).unwrap();
        let root = &placed.attachment.root;
        let repo = git2::Repository::open(root).unwrap();
        let original = repo.head().unwrap().peel_to_commit().unwrap().id();
        std::fs::write(&placed.readme_path, "committed\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(&bundle_info().readme)).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("me", "me@example.com").unwrap();
        let committed = repo
            .commit(
                Some("HEAD"),
                &sig,
                &sig,
                "local history",
                &tree,
                &[&repo.find_commit(original).unwrap()],
            )
            .unwrap();
        std::fs::write(&placed.readme_path, "staged\n").unwrap();
        index.add_path(Path::new(&bundle_info().readme)).unwrap();
        index.write().unwrap();
        std::fs::write(&placed.readme_path, "working\n").unwrap();
        std::fs::remove_file(root.join("deleted.md")).unwrap();
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::write(root.join("notes/mine.md"), "untracked").unwrap();
        std::fs::create_dir_all(root.join("cache")).unwrap();
        std::fs::write(root.join("cache/state"), "machine").unwrap();
        repo.remote("origin", "https://example.invalid/private")
            .unwrap();
        let v2 = bundle_with_files("v2\n", &[(".gitignore", "cache/\n")]);
        assert_eq!(
            version(&home, &who, &v2).unwrap().state,
            "behind-and-modified"
        );
        let result = update(&home, &who, &v2).unwrap();
        let branch = result.preservation_branch.unwrap();
        let saved = repo
            .find_branch(&branch, git2::BranchType::Local)
            .unwrap()
            .get()
            .peel_to_commit()
            .unwrap();
        let blob = |tree: &git2::Tree, path: &str| {
            repo.find_blob(tree.get_path(Path::new(path)).unwrap().id())
                .unwrap()
                .content()
                .to_vec()
        };
        assert_eq!(
            blob(&saved.tree().unwrap(), &bundle_info().readme),
            b"working\n"
        );
        assert_eq!(
            blob(
                &saved.parent(0).unwrap().tree().unwrap(),
                &bundle_info().readme
            ),
            b"staged\n"
        );
        assert_eq!(saved.parent(0).unwrap().parent_id(0).unwrap(), committed);
        assert_eq!(blob(&saved.tree().unwrap(), "notes/mine.md"), b"untracked");
        assert!(saved
            .tree()
            .unwrap()
            .get_path(Path::new("deleted.md"))
            .is_err());
        assert_eq!(head(root), archive_commit(&v2).unwrap());
        assert!(changed_paths(&repo).unwrap().is_empty());
        assert_eq!(
            repo.index().unwrap().write_tree().unwrap(),
            repo.head().unwrap().peel_to_commit().unwrap().tree_id()
        );
        assert!(!root.join("notes/mine.md").exists());
        assert_eq!(std::fs::read(root.join("cache/state")).unwrap(), b"machine");
        assert!(repo.find_remote("origin").is_err());
        assert_eq!(result.docs.state, "current");
        assert!(result.changed_paths.contains(&"notes/mine.md".to_string()));
        assert!(update(&home, &who, &v2)
            .unwrap()
            .preservation_branch
            .is_none());
    }

    #[test]
    fn an_ignored_file_removed_from_the_index_blocks_refresh_without_losing_its_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let who = Principal::from(7_u64);
        let path = "getting-started/config/agents/docs-helper/agent.yaml";
        let original = include_str!(
            "../../../codesense-docs/getting-started/config/agents/docs-helper/agent.yaml"
        );
        let placed =
            ensure_placed(&home, &who, &bundle_with_files("v1\n", &[(path, original)])).unwrap();
        let root = &placed.attachment.root;
        let before = head(root);
        let repo = git2::Repository::open(root).unwrap();
        let local = original.replace("name: Docs helper", "name: My helper");
        std::fs::write(root.join(path), &local).unwrap();
        let mut index = repo.index().unwrap();
        index.remove_path(Path::new(path)).unwrap();
        index.write().unwrap();
        std::fs::write(root.join(".gitignore"), format!("{path}\n")).unwrap();
        assert!(repo.status_should_ignore(Path::new(path)).unwrap());
        let result = update(&home, &who, &bundle_with_files("v2\n", &[(path, original)]));
        assert_eq!(
            std::fs::read_to_string(root.join(path)).unwrap(),
            local,
            "ignored local helper bytes must remain available"
        );
        let error = result.expect_err("a collision with ignored local content must refuse refresh");
        assert!(error.contains(path), "{error}");
        assert!(error.contains("preservation branch"), "{error}");
        assert_eq!(head(root), before);
        assert!(repo.index().unwrap().get_path(Path::new(path), 0).is_none());
    }

    #[test]
    fn unindexed_local_only_files_are_saved_or_left_in_place_before_bundle_removes_them() {
        for ignored in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().join("home");
            let who = Principal::from(7_u64);
            let placed = ensure_placed(&home, &who, &bundle_with("v1\n")).unwrap();
            let root = &placed.attachment.root;
            let path = Path::new("local-notes.md");
            let repo = git2::Repository::open(root).unwrap();
            std::fs::write(root.join(path), "committed local notes\n").unwrap();
            let mut index = repo.index().unwrap();
            index.add_path(path).unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            let parent = repo.head().unwrap().peel_to_commit().unwrap();
            let sig = git2::Signature::now("me", "me@example.com").unwrap();
            let before = repo
                .commit(
                    Some("HEAD"),
                    &sig,
                    &sig,
                    "local-only notes",
                    &tree,
                    &[&parent],
                )
                .unwrap();
            let local = "edited local notes that are not in any commit\n";
            std::fs::write(root.join(path), local).unwrap();
            index.remove_path(path).unwrap();
            index.write().unwrap();
            if ignored {
                std::fs::write(root.join(".gitignore"), "local-notes.md\n").unwrap();
            }
            let result = update(&home, &who, &bundle_with("v2\n"));
            if ignored {
                assert_eq!(
                    std::fs::read_to_string(root.join(path))
                        .expect("ignored local notes must not be deleted"),
                    local
                );
                let error = result.expect_err("unsaved ignored bytes must block removal");
                assert!(error.contains("local-notes.md"), "{error}");
                assert!(error.contains("preservation branch"), "{error}");
                assert_eq!(head(root), before.to_string());
                assert!(repo.index().unwrap().get_path(path, 0).is_none());
            } else {
                let branch = result.unwrap().preservation_branch.unwrap();
                let saved = repo
                    .find_branch(&branch, git2::BranchType::Local)
                    .unwrap()
                    .get()
                    .peel_to_tree()
                    .unwrap();
                let blob = repo.find_blob(saved.get_path(path).unwrap().id()).unwrap();
                assert_eq!(blob.content(), local.as_bytes());
                assert!(!root.join(path).exists());
            }
        }
    }

    #[test]
    fn explicit_clean_behind_update_preserves_previous_bundle_and_ignored_collision_refuses() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let who = Principal::from(7_u64);
        let v1 = bundle_with_files("v1\n", &[(".gitignore", "cache/\n")]);
        let placed = ensure_placed(&home, &who, &v1).unwrap();
        let v2 = bundle_with_files("v2\n", &[(".gitignore", "cache/\n")]);
        assert_eq!(version(&home, &who, &v2).unwrap().state, "behind");
        assert!(update(&home, &who, &v2)
            .unwrap()
            .preservation_branch
            .is_some());
        let root = placed.attachment.root;
        std::fs::create_dir_all(root.join("cache")).unwrap();
        std::fs::write(root.join("cache/state"), "machine").unwrap();
        let colliding = bundle_with_files("v3\n", &[("cache/state", "shipped")]);
        let error = update(&home, &who, &colliding).unwrap_err();
        assert!(error.contains("preservation branch"), "{error}");
        assert_eq!(std::fs::read(root.join("cache/state")).unwrap(), b"machine");
        assert_eq!(head(&root), archive_commit(&v2).unwrap());
    }

    #[test]
    fn docs_status_includes_untracked_and_reports_bundle_versions() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let who = Principal::from(7_u64);
        let bundle = bundle_with("v1\n");
        assert_eq!(version(&home, &who, &bundle).unwrap().state, "missing");
        let placed = ensure_placed(&home, &who, &bundle).unwrap();
        assert_eq!(version(&home, &who, &bundle).unwrap().state, "current");
        std::fs::write(placed.attachment.root.join("mine.md"), "mine").unwrap();
        assert_eq!(version(&home, &who, &bundle).unwrap().state, "modified");
        assert!(update(&home, &who, &bundle)
            .unwrap()
            .preservation_branch
            .is_some());
        assert_eq!(version(&home, &who, &bundle).unwrap().state, "current");
        let started = std::time::Instant::now();
        let (_temp, root) = placed_docs();
        let repo = git2::Repository::open(&root).unwrap();
        let status_start = std::time::Instant::now();
        assert!(changed_paths(&repo).unwrap().is_empty());
        let status_elapsed = status_start.elapsed();
        let version_start = std::time::Instant::now();
        let docs = version(root.ancestors().nth(3).unwrap(), &who, real_bundle()).unwrap();
        assert_eq!(docs.state, "current");
        eprintln!(
            "bundled fixture status: {:?}; whole version: {:?}; fixture placement: {:?}",
            status_elapsed,
            version_start.elapsed(),
            started.elapsed()
        );
    }

    #[test]
    fn real_bundle_is_a_loadable_offline_repo_with_the_getting_started_team() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let guest = Principal::local_guest();
        let placed = ensure_placed(&home, &guest, real_bundle()).unwrap();
        let info = bundle_info();
        assert_eq!(placed.placement, Placement::Placed);
        assert_eq!(placed.attachment.slug, info.slug);
        assert!(placed.readme_path.is_file());
        let root = &placed.attachment.root;
        let loaded = crate::domain::load_org(info.slug.clone(), root, None);
        let org = loaded.org.expect("the docs org loads");
        assert!(loaded.file_errors.is_empty(), "{:?}", loaded.file_errors);
        assert_eq!(org.display_name, bundle_field("displayName"));
        assert_eq!(
            org.teams
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            vec![info.team.as_str()]
        );
        let repo = git2::Repository::open(root).unwrap();
        assert!(repo.find_remote("origin").is_err());
        assert_eq!(repo.head().unwrap().shorthand().ok(), Some(info.branch.as_str()));
        let mut options = git2::StatusOptions::new();
        options.include_untracked(true);
        assert!(
            repo.statuses(Some(&mut options)).unwrap().is_empty(),
            "a fresh placement is a clean checkout"
        );
        let team = root.join(&info.team);
        assert!(team
            .join("content/.claude/skills/update-codesense-docs/SKILL.md")
            .is_file());
        // The example skills and dashboards are inert, copyable examples: none is a live
        // skill in the docs team, and no automations ship.
        let examples = team.join("content/examples");
        assert!(examples.join("README.md").is_file());
        for skill in [
            "routine",
            "agent",
            "codesense-setup",
            "worker-exit-contract",
        ] {
            assert!(examples.join(format!("skills/{skill}/SKILL.md")).is_file());
            assert!(!team
                .join(format!("content/.claude/skills/{skill}"))
                .exists());
        }
        for page in [
            "sprint-analysis.html",
            "kickoff-console.html",
            "agent-inbox-console.html",
        ] {
            assert!(examples.join(format!("dashboards/{page}")).is_file());
        }
        assert!(!team.join("content/dashboards/starter").exists());
        assert!(team.join("content/handbook/README.md").is_file());
        // The example routine and the agent it runs are valid definitions.
        let config = team.join("config");
        let routine: crate::routines::model::RoutineDef = serde_yaml::from_slice(
            &std::fs::read(
                config.join("agents/docs-helper/routines/update-codesense-docs/routine.yml"),
            )
            .unwrap(),
        )
        .unwrap();
        // It lives under its agent (SW-AGR-10), so the path names the agent.
        assert!(routine.agent.is_empty());
        let agent: crate::agents::model::Agent = serde_yaml::from_slice(
            &std::fs::read(config.join("agents/docs-helper/agent.yaml")).unwrap(),
        )
        .unwrap();
        agent.validate().unwrap();
        let routine = crate::routines::defs::read_def(
            &config.join("agents/docs-helper/routines/update-codesense-docs"),
        )
        .unwrap()
        .unwrap();
        routine.validate().unwrap();
        assert_eq!(routine.agent.as_str(), agent.id);
        assert!(
            !config.join("routines").exists(),
            "no legacy routines dir ships"
        );
    }

    // The getting-started team's binding and the placed sample clone's origin are the
    // two halves of one identity: every binding lookup compares their keys.
    #[test]
    fn real_bundle_binds_the_sample_repo_under_its_clone_origin_key() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let placed = ensure_placed(&home, &Principal::local_guest(), real_bundle()).unwrap();
        let info = bundle_info();
        let org = crate::domain::load_org(info.slug.clone(), &placed.attachment.root, None)
            .org
            .expect("the docs org loads");
        let team = org.teams.iter().find(|t| t.name == info.team).unwrap();
        let origin = &crate::context_repo::sample_source_repo::bundle_info().remote_url;
        let origin_key = crate::context_repo::canonical_remote_key(origin)
            .expect("the sample origin has a canonical key");
        assert_eq!(
            team.source_repos
                .iter()
                .map(|slug| slug.remote_key())
                .collect::<Vec<_>>(),
            vec![origin_key]
        );
    }

    #[test]
    fn later_launches_keep_the_placed_copy_and_registration() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let who = Principal::from(7_u64);
        let archive = bundle_with("v1\n");
        let first = ensure_placed(&home, &who, &archive).unwrap();
        let again = ensure_placed(&home, &who, &archive).unwrap();
        assert_eq!(again.placement, Placement::Kept);
        assert_eq!(again.attachment, first.attachment);
        assert_eq!(context_attachments::list(&home, &who).unwrap().len(), 1);
    }

    #[test]
    fn a_newer_bundle_replaces_an_untouched_copy_in_place() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let who = Principal::from(7_u64);
        let first = ensure_placed(&home, &who, &bundle_with("v1\n")).unwrap();
        let newer = bundle_with("v2\n");
        let updated = ensure_placed(&home, &who, &newer).unwrap();
        assert_eq!(updated.placement, Placement::Updated);
        assert_eq!(updated.attachment.root, first.attachment.root);
        assert_eq!(
            std::fs::read_to_string(&updated.readme_path).unwrap(),
            "v2\n"
        );
        assert_eq!(
            head(&updated.attachment.root),
            archive_commit(&newer).unwrap()
        );
        // No staging leftovers beside the placed copy.
        let parent = updated.attachment.root.parent().unwrap();
        assert!(!std::fs::read_dir(parent).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".codesense-docs-")));
    }

    #[test]
    fn untracked_work_waits_for_explicit_update_while_ignored_state_survives() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let who = Principal::from(7_u64);
        let placed = ensure_placed(
            &home,
            &who,
            &bundle_with_files("v1\n", &[(".gitignore", "getting-started/install/\n")]),
        )
        .unwrap();
        let root = placed.attachment.root.clone();
        std::fs::create_dir_all(root.join("getting-started/install")).unwrap();
        std::fs::write(root.join("getting-started/install/state.db"), "local").unwrap();
        std::fs::write(root.join("notes.txt"), "mine").unwrap();
        let bundle = bundle_with_files("v2\n", &[(".gitignore", "getting-started/install/\n")]);
        let kept = ensure_placed(&home, &who, &bundle).unwrap();
        assert_eq!(kept.placement, Placement::Kept);
        assert_eq!(std::fs::read_to_string(&kept.readme_path).unwrap(), "v1\n");
        assert_eq!(
            std::fs::read_to_string(root.join("notes.txt")).unwrap(),
            "mine"
        );
        let updated = update(&home, &who, &bundle).unwrap();
        assert!(updated.preservation_branch.is_some());
        assert_eq!(std::fs::read_to_string(&kept.readme_path).unwrap(), "v2\n");
        assert_eq!(
            std::fs::read_to_string(root.join("getting-started/install/state.db")).unwrap(),
            "local"
        );
        assert!(!root.join("notes.txt").exists());
    }

    #[test]
    fn a_crash_between_install_and_the_version_record_heals_on_the_next_launch() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let who = Principal::from(7_u64);
        let archive = bundle_with("v1\n");
        let bundled = archive_commit(&archive).unwrap();
        // The install landed, then the process died before recording the version.
        ensure_placed(&home, &who, &archive).unwrap();
        std::fs::remove_file(marker_path(&home, &who)).unwrap();
        std::fs::write(pending_path(&home, &who), format!("{bundled}\n")).unwrap();
        let healed = ensure_placed(&home, &who, &archive).unwrap();
        assert_eq!(
            std::fs::read_to_string(marker_path(&home, &who))
                .unwrap()
                .trim(),
            bundled
        );
        assert!(!pending_path(&home, &who).exists());
        assert_eq!(
            std::fs::read_to_string(&healed.readme_path).unwrap(),
            "v1\n"
        );
        // From then on, newer bundles update it again.
        let v2 = ensure_placed(&home, &who, &bundle_with("v2\n")).unwrap();
        assert_eq!(v2.placement, Placement::Updated);
    }

    #[test]
    fn a_crash_in_the_middle_of_an_update_is_completed_on_the_next_launch() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let who = Principal::from(7_u64);
        let placed = ensure_placed(&home, &who, &bundle_with("v1\n")).unwrap();
        let v2 = bundle_with("v2\n");
        let v2_commit = archive_commit(&v2).unwrap();
        // Interrupted after the working tree changed, before the refs moved.
        std::fs::write(pending_path(&home, &who), format!("{v2_commit}\n")).unwrap();
        std::fs::write(&placed.readme_path, "v2\n").unwrap();
        let resumed = ensure_placed(&home, &who, &v2).unwrap();
        assert_eq!(resumed.placement, Placement::Updated);
        assert_eq!(head(&resumed.attachment.root), v2_commit);
        let repo = git2::Repository::open(&resumed.attachment.root).unwrap();
        assert!(repo.statuses(None).unwrap().is_empty());
        assert!(!pending_path(&home, &who).exists());
    }

    #[test]
    fn an_update_never_overwrites_an_untracked_file_at_a_path_it_adds() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let who = Principal::from(7_u64);
        let placed = ensure_placed(&home, &who, &bundle_with("v1\n")).unwrap();
        let root = placed.attachment.root.clone();
        let v1_head = head(&root);
        std::fs::write(root.join("notes.md"), "mine").unwrap();
        let v2 = bundle_with_files("v2\n", &[("notes.md", "docs notes")]);
        let kept = ensure_placed(&home, &who, &v2).unwrap();
        assert_eq!(kept.placement, Placement::Kept);
        assert_eq!(
            std::fs::read_to_string(root.join("notes.md")).unwrap(),
            "mine"
        );
        assert_eq!(head(&root), v1_head);
        assert_eq!(
            std::fs::read_to_string(&placed.readme_path).unwrap(),
            "v1\n"
        );
        assert!(!pending_path(&home, &who).exists());
    }

    #[test]
    fn a_checkout_libgit2_cannot_read_is_left_alone_and_stays_connected() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let who = Principal::from(7_u64);
        let placed = ensure_placed(&home, &who, &bundle_with("v1\n")).unwrap();
        let root = placed.attachment.root.clone();
        // Say the user's newer git converted it to a format this libgit2 doesn't know.
        std::fs::write(
            root.join(".git/config"),
            "[core]\n\trepositoryformatversion = 99\n\tbare = false\n",
        )
        .unwrap();
        let kept = ensure_placed(&home, &who, &bundle_with("v2\n")).unwrap();
        assert_eq!(kept.placement, Placement::Kept);
        assert_eq!(kept.attachment.root, root);
        assert_eq!(std::fs::read_to_string(&kept.readme_path).unwrap(), "v1\n");
        let parent = root.parent().unwrap();
        assert!(!std::fs::read_dir(parent).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("damaged")));
    }

    #[test]
    fn an_interrupted_update_the_user_moved_on_from_is_dropped_not_forced() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let who = Principal::from(7_u64);
        let placed = ensure_placed(&home, &who, &bundle_with("v1\n")).unwrap();
        let root = placed.attachment.root.clone();
        let v2 = bundle_with("v2\n");
        std::fs::write(
            pending_path(&home, &who),
            format!("{}\n", archive_commit(&v2).unwrap()),
        )
        .unwrap();
        // After the crash, the user's agent committed (or reset) to something else.
        let repo = git2::Repository::open(&root).unwrap();
        std::fs::write(&placed.readme_path, "theirs\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(&bundle_info().readme)).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("me", "me@example.com").unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        let theirs = repo
            .commit(Some("HEAD"), &sig, &sig, "mine", &tree, &[&parent])
            .unwrap();
        let kept = ensure_placed(&home, &who, &v2).unwrap();
        assert_eq!(kept.placement, Placement::Kept);
        assert_eq!(head(&root), theirs.to_string());
        assert_eq!(
            std::fs::read_to_string(&placed.readme_path).unwrap(),
            "theirs\n"
        );
        assert!(!pending_path(&home, &who).exists());
    }

    #[test]
    fn a_newer_bundle_completes_an_older_interrupted_update() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let who = Principal::from(7_u64);
        let placed = ensure_placed(&home, &who, &bundle_with("v1\n")).unwrap();
        let v2 = archive_commit(&bundle_with("v2\n")).unwrap();
        // An older app crashed while writing v2's tree (HEAD still v1, tree dirty)...
        std::fs::write(pending_path(&home, &who), format!("{v2}\n")).unwrap();
        std::fs::write(&placed.readme_path, "v2\n").unwrap();
        // ...and a newer app with v3 launches.
        let v3 = bundle_with("v3\n");
        let updated = ensure_placed(&home, &who, &v3).unwrap();
        assert_eq!(updated.placement, Placement::Updated);
        assert_eq!(head(&updated.attachment.root), archive_commit(&v3).unwrap());
        assert_eq!(
            std::fs::read_to_string(&updated.readme_path).unwrap(),
            "v3\n"
        );
        let repo = git2::Repository::open(&updated.attachment.root).unwrap();
        assert!(repo.statuses(None).unwrap().is_empty());
        assert!(!pending_path(&home, &who).exists());
        assert_eq!(
            std::fs::read_to_string(marker_path(&home, &who))
                .unwrap()
                .trim(),
            archive_commit(&v3).unwrap()
        );
    }

    #[test]
    fn a_damaged_copy_is_set_aside_and_placed_again() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let who = Principal::from(7_u64);
        let root = docs_root(&home, &who);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("half-written"), "x").unwrap();
        let placed = ensure_placed(&home, &who, &bundle_with("v1\n")).unwrap();
        assert_eq!(placed.placement, Placement::Placed);
        assert_eq!(
            std::fs::read_to_string(&placed.readme_path).unwrap(),
            "v1\n"
        );
        let parent = root.parent().unwrap();
        assert!(std::fs::read_dir(parent).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("codesense-docs.damaged-")));
    }

    #[test]
    fn a_pulled_forward_or_edited_copy_is_never_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let who = Principal::from(7_u64);
        let placed = ensure_placed(&home, &who, &bundle_with("v1\n")).unwrap();
        // A local edit to a tracked file.
        std::fs::write(&placed.readme_path, "mine\n").unwrap();
        let kept = ensure_placed(&home, &who, &bundle_with("v2\n")).unwrap();
        assert_eq!(kept.placement, Placement::Kept);
        assert_eq!(
            std::fs::read_to_string(&kept.readme_path).unwrap(),
            "mine\n"
        );

        // A copy whose HEAD moved past the recorded bundle (the agent pulled).
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let placed = ensure_placed(&home, &who, &bundle_with("v1\n")).unwrap();
        std::fs::write(
            marker_path(&home, &who),
            "0000000000000000000000000000000000000000\n",
        )
        .unwrap();
        let kept = ensure_placed(&home, &who, &bundle_with("v2\n")).unwrap();
        assert_eq!(kept.placement, Placement::Kept);
        assert_eq!(
            std::fs::read_to_string(&placed.readme_path).unwrap(),
            "v1\n"
        );
    }

    /// The real docs, placed under a temp home: (temp, root).
    fn placed_docs() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let placed = ensure_placed(&home, &Principal::from(7_u64), real_bundle()).unwrap();
        let root = placed.attachment.root.clone();
        (temp, root)
    }

    fn assert_refused<E: std::fmt::Display>(result: Result<(), E>) {
        let error = result
            .expect_err("a write into the docs must be refused")
            .to_string();
        assert!(error.contains(READ_ONLY_MESSAGE), "{error}");
    }

    #[test]
    fn the_rule_refuses_tracked_docs_content_and_allows_ignored_state_and_everything_else() {
        let (temp, root) = placed_docs();
        let team = root.join("getting-started");
        assert_refused(refuse_write_at(&team.join("content/README.md")));
        assert_refused(refuse_write_at(&team.join("content/new-note.md")));
        assert_refused(refuse_write_at(&team.join("config/extras/new.yml")));
        assert_refused(refuse_write_at(&root.join("teams.yml")));
        assert_refused(refuse_write_at(&root));
        // `..` can't slip out of the check.
        assert_refused(refuse_write_at(&team.join("content/handbook/../sneaky.md")));
        // Machine-local state CodeSense keeps beside the docs is git-ignored: writable.
        refuse_write_at(&team.join("install/.claude/skills/x/SKILL.md")).unwrap();
        refuse_write_at(&root.join(".git/index")).unwrap();
        // Outside the docs: untouched.
        refuse_write_at(&temp.path().join("elsewhere/a.md")).unwrap();
        // A checkout at the same path shape that is not the docs.
        let lookalike = temp.path().join("other/users/9/codesense-docs");
        std::fs::create_dir_all(&lookalike).unwrap();
        std::fs::write(
            lookalike.join("org.yml"),
            "slug: local/mine\ndisplay_name: Mine\ncreated_at: 2026-10-01T00:00:00Z\n",
        )
        .unwrap();
        refuse_write_at(&lookalike.join("team/content/a.md")).unwrap();
    }

    #[test]
    fn machine_local_state_stays_writable_when_libgit2_cannot_read_the_docs() {
        let (_temp, root) = placed_docs();
        std::fs::write(
            root.join(".git/config"),
            "[core]\n\trepositoryformatversion = 99\n\tbare = false\n",
        )
        .unwrap();
        assert!(git2::Repository::open(&root).is_err());
        let team = root.join("getting-started");
        refuse_write_at(&team.join("install/.claude/skills/x/SKILL.md")).unwrap();
        refuse_write_at(&root.join("_codesense/state.json")).unwrap();
        refuse_write_at(&root.join("deep/cache.sqlite3")).unwrap();
        assert_refused(refuse_write_at(&team.join("content/README.md")));
        assert_refused(refuse_write_at(&team.join("config/extras/new.yml")));
        assert_refused(refuse_write_at(&root.join("teams.yml")));
    }

    #[test]
    fn every_shared_file_writer_refuses_the_docs() {
        let (_temp, root) = placed_docs();
        let target = root.join("getting-started/content/new.md");
        assert_refused(crate::content_engine::write_tmp_fsync_rename(&target, b"x"));
        // Selected publication shares the same guarded writer, including CAS
        // and permission-preserving calls that bypass the ordinary entry point.
        assert_refused(
            crate::content_engine::atomic_io::write_tmp_fsync_rename_expected_mode(
                &target,
                b"x",
                None,
                Some(0o600),
            )
            .map(|_| ()),
        );
        assert_refused(crate::content_engine::write_tmp_rename_no_fsync(
            &target, b"x",
        ));
        assert_refused(crate::shared::fs_atomic::write_bytes_atomic(&target, b"x"));
        assert_refused(crate::shared::yaml_io::write_yaml_atomic(&target, &"x"));
        assert_refused(crate::shared::yaml_io::write_yaml_value_atomic(
            &target, &"x",
        ));
        assert_refused(crate::shared::yaml_io::write_yaml_bytes_atomic(
            &target, b"x",
        ));
        assert!(!target.exists());
        // An ignored path still takes the app's own writes.
        let state = root.join("getting-started/install/state.txt");
        crate::content_engine::write_tmp_fsync_rename(&state, b"ok").unwrap();
        assert_eq!(std::fs::read(&state).unwrap(), b"ok");
    }

    #[test]
    fn example_copy_carries_the_setup_but_not_the_docs_identity() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let placed = ensure_placed(&home, &Principal::local_guest(), real_bundle()).unwrap();
        let dest = temp.path().join("copy");
        copy_example(&placed.attachment.root, &dest).unwrap();
        let team = dest.join(&bundle_info().team);
        assert!(dest.join("teams.yml").is_file());
        assert!(team.join("content/README.md").is_file());
        assert!(team
            .join("content/examples/skills/codesense-setup/SKILL.md")
            .is_file());
        assert!(!dest.join("org.yml").exists());
        assert!(!dest.join(".git").exists());
        assert!(!team
            .join("content/.claude/skills/update-codesense-docs")
            .exists());
        assert!(!team
            .join("config/agents/docs-helper/routines/update-codesense-docs")
            .exists());
        assert!(!team.join("config/agents/docs-helper").exists());
        let readme = std::fs::read_to_string(team.join("content/README.md")).unwrap();
        assert!(readme.contains("yours to edit"), "{readme}");
    }

    /// A failed write stops the tree walk with `Abort`, which git2 reports as an error
    /// since 0.20: the caller still gets the write's own message, naming the file.
    #[test]
    fn example_copy_reports_the_write_that_failed() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let placed = ensure_placed(&home, &Principal::local_guest(), real_bundle()).unwrap();
        let dest = temp.path().join("copy");
        std::fs::write(&dest, "not a directory").unwrap();
        let error = copy_example(&placed.attachment.root, &dest).unwrap_err();
        assert!(
            error.starts_with("Cannot copy the CodeSense example ("),
            "{error}"
        );
    }

    /// SW-ODC-11 AC-1..AC-7: the seeded DEMO epic + 5 stories + sprint registry +
    /// 14 phase snapshots must all survive a round-trip through the bundled docs
    /// placement. The dashboards consume these artifacts via the pipeline scanner
    /// (sprint-analysis, story-board, pipeline-graph) and HTTP routes
    /// (sprint-registry, factory-playbook) — if the files aren't on disk after
    /// placement, every data dashboard renders empty on first launch.
    #[test]
    fn sw_odc_11_seed_data_lands_on_disk_and_is_scanner_readable() {
        use crate::commands::pipeline_scanner::scan_pipeline_tree_impl;
        let (_temp, root) = placed_docs();
        let team = root.join("getting-started");
        let content = team.join("content");

        // AC-9 + AC-10: canonical phases.yml + schema + 14 phase snapshots.
        assert!(content.join("generated/phases.yml").is_file());
        assert!(content
            .join("generated/schemas/factory-phase.schema.json")
            .is_file());
        for i in 1..=14 {
            let path = content.join(format!("generated/factory-phase-{i}.json"));
            assert!(path.is_file(), "missing {}", path.display());
            // Each snapshot parses as JSON.
            let bytes = std::fs::read(&path).unwrap();
            serde_json::from_slice::<serde_json::Value>(&bytes)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        }
        // phases.yml parses as YAML and lists exactly 14 phases.
        let phases_yaml = std::fs::read(content.join("generated/phases.yml")).unwrap();
        let phases_doc: serde_yaml::Value = serde_yaml::from_slice(&phases_yaml).unwrap();
        let phase_count = phases_doc["phases"]
            .as_sequence()
            .map(|s| s.len())
            .unwrap_or(0);
        assert_eq!(phase_count, 14);

        // AC-6: sprint registry under config/ (not content/) with FAKE-S1 active.
        let sprints = team.join("config/sprints.yaml");
        assert!(sprints.is_file());
        let sprints_body = std::fs::read_to_string(&sprints).unwrap();
        assert!(sprints_body.contains("FAKE-S1"));
        assert!(sprints_body.contains("state: active"));

        // AC-5: the 4 seeded inbox thread shapes.
        for name in [
            "01-triage-dispatch.yml",
            "02-decision-escalation.yml",
            "03-review-feedback.yml",
            "04-ship-confirmation.yml",
        ] {
            assert!(content.join(format!("examples/inboxes/{name}")).is_file());
        }

        // AC-1..AC-3 + AC-7: the DEMO epic + 5 stories scan into a pipeline tree.
        let tree = scan_pipeline_tree_impl(&content, "examples/sample-epic", "", "README.md")
            .expect("pipeline scanner reads the seeded DEMO epic");
        // The scan picks up the 5 DEMO stories (reading authoritative `story_id`).
        let story_ids: Vec<String> = tree
            .stories
            .iter()
            .filter_map(|s| s.story_id.clone())
            .collect();
        for id in ["DEMO-1", "DEMO-2", "DEMO-3", "DEMO-4", "DEMO-5"] {
            assert!(
                story_ids.iter().any(|s| s == id),
                "SW-ODC-11 AC-2: pipeline scanner must see {id}; got {:?}",
                story_ids
            );
        }
        // AC-2: dependency graph — all four siblings depend on DEMO-1.
        for id in ["DEMO-2", "DEMO-3", "DEMO-4", "DEMO-5"] {
            let story = tree
                .stories
                .iter()
                .find(|s| s.story_id.as_deref() == Some(id))
                .unwrap_or_else(|| panic!("story {id} not scanned"));
            let deps = story.depends_on.clone().unwrap_or_default();
            assert!(
                deps.iter().any(|d| d == "DEMO-1"),
                "{id} must depend on DEMO-1; got depends_on={:?}",
                deps
            );
        }
        // AC-1 + AC-3: status distribution covers the taxonomy — at least 3 distinct
        // non-empty statuses so sprint-analysis + story-board render non-empty columns.
        let mut statuses = std::collections::HashSet::new();
        for s in &tree.stories {
            if let Some(status) = s.status.as_ref() {
                if !status.is_empty() {
                    statuses.insert(status.clone());
                }
            }
        }
        assert!(
            statuses.len() >= 3,
            "SW-ODC-11 AC-1/AC-3: seed data must span ≥3 statuses to render non-empty; got {:?}",
            statuses
        );
    }

    #[test]
    fn archives_that_escape_the_git_dir_are_refused() {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::default(),
        ));
        let mut header = tar::Header::new_gnu();
        header.set_size(1);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "README.md", &b"x"[..])
            .unwrap();
        let archive = builder.into_inner().unwrap().finish().unwrap();
        assert!(archive_entries(&archive)
            .unwrap_err()
            .contains("Unexpected path"));
    }
}
