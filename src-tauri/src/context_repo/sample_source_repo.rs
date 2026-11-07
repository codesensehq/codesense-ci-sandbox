// SPDX-License-Identifier: MIT
//! The bundled sample source repo (SW-ODC-11).
//!
//! Every install ships a tiny real git clone of a fictitious codebase as a Tauri
//! resource (`resources/sample-source-repo.tar.gz`, built from source by
//! `scripts/build-sample-source-repo.mjs`). On launch [`ensure_placed`] puts it
//! under the profile's CodeSense home. The sample-source-repo pairs with the
//! sample DEMO epic that lives in the shipped docs org — the epic's
//! `source_repo: acme/sample-source-repo` reference now resolves to a real
//! on-disk clone, so every shipped data dashboard (`agent-dag.html`,
//! `pipeline-graph.html`, `story-board.html`, …) renders non-empty on first launch.
//!
//! Mirrors [`super::codesense_docs::ensure_placed`] (SW-OC-16) 1:1. Journaled,
//! crash-safe, idempotent, non-destructive. The one meaningful difference: a
//! target directory that is non-empty with a different repo origin (or has no
//! `.git/HEAD` to prove it is a clone) is kept untouched and the function returns
//! [`Placement::Refused`] — the dashboards fall back to seed-data-only mode
//! instead of overwriting the user's work (AC-11).

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::Deserialize;

/// The folder (below the profile's CodeSense home) the sample source repo is placed in.
const PLACED_DIR: &str = "sample-source-repo";
/// Records the bundled commit last placed, so a newer bundle replaces only an
/// untouched copy, never one that has been pulled forward or edited.
const PLACED_MARKER: &str = "sample-source-repo.bundle";
/// Journal of an install or update in progress: the bundle commit being placed.
/// Present at launch only if a crash interrupted one, which the next launch
/// completes (mirrors SW-OC-16 posture).
const PENDING_MARKER: &str = "sample-source-repo.pending";

/// `codesense-docs/sample-source-repo/bundle.json`: the identity the generator
/// and the app share.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)] // slug + display_name mirror codesense_docs::BundleInfo for parity;
                    // future dashboards (SW-ODC-9) read them.
pub struct BundleInfo {
    pub slug: String,
    pub display_name: String,
    pub branch: String,
    pub remote_url: String,
}

pub fn bundle_info() -> &'static BundleInfo {
    static INFO: OnceLock<BundleInfo> = OnceLock::new();
    INFO.get_or_init(|| {
        serde_json::from_str(include_str!(
            "../../../codesense-docs/sample-source-repo/bundle.json"
        ))
        .expect("codesense-docs/sample-source-repo/bundle.json is valid (checked by tests)")
    })
}

/// Where the sample source repo lives for `principal`. Per profile, like
/// [`super::codesense_docs::docs_root`].
pub fn sample_repo_root(home: &Path, principal: &crate::domain::Principal) -> PathBuf {
    super::path_to_user_home(home, principal).join(PLACED_DIR)
}

fn marker_path(home: &Path, principal: &crate::domain::Principal) -> PathBuf {
    super::path_to_user_home(home, principal).join(PLACED_MARKER)
}

fn pending_path(home: &Path, principal: &crate::domain::Principal) -> PathBuf {
    super::path_to_user_home(home, principal).join(PENDING_MARKER)
}

/// What placement did on this launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// First placement on this profile.
    Placed,
    /// An untouched older copy was replaced by this app's newer bundle.
    Updated,
    /// The placed copy was kept (current, pulled forward, or locally changed).
    Kept,
    /// The target directory exists with a different repo origin (or no `.git/HEAD`),
    /// so placement was refused — AC-11. The caller falls back to seed-data-only
    /// mode without a bound live repo.
    Refused,
}

#[derive(Debug)]
pub struct PlacedSampleRepo {
    pub root: PathBuf,
    pub bundled_commit: String,
    pub placement: Placement,
    pub refused_reason: Option<String>,
}

impl PlacedSampleRepo {
    /// `true` unless placement was refused (AC-11). The Tauri command surface
    /// already exposes this through `SampleSourceRepoResult.available`; keeping
    /// the predicate here makes AC-11 testable in isolation.
    #[allow(dead_code)]
    pub fn is_available(&self) -> bool {
        !matches!(self.placement, Placement::Refused)
    }
}

/// Places (or refreshes) the bundled sample source repo for `principal`.
/// Idempotent; serialized process-wide. Non-destructive: a non-matching existing
/// directory is kept and the function returns [`Placement::Refused`] with the
/// reason — see AC-11.
pub fn ensure_placed(
    home: &Path,
    principal: &crate::domain::Principal,
    archive: &[u8],
) -> Result<PlacedSampleRepo, String> {
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let bundled = archive_commit(archive)?;
    let root = sample_repo_root(home, principal);
    let journal = Journal {
        bundled: bundled.clone(),
        marker: marker_path(home, principal),
        pending: pending_path(home, principal),
    };
    match place(&root, archive, &journal)? {
        PlaceOutcome::Done(placement) => Ok(PlacedSampleRepo {
            root,
            bundled_commit: bundled,
            placement,
            refused_reason: None,
        }),
        PlaceOutcome::Refused(reason) => Ok(PlacedSampleRepo {
            root,
            bundled_commit: bundled,
            placement: Placement::Refused,
            refused_reason: Some(reason),
        }),
    }
}

/// The placement bookkeeping beside the sample repo (never inside it).
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

    fn begin(&self) -> Result<(), String> {
        crate::shared::fs_atomic::write_bytes_atomic(
            &self.pending,
            format!("{}\n", self.bundled).as_bytes(),
        )
        .map_err(|e| format!("Cannot record the sample source repo update: {e}"))
    }

    fn finish(&self) -> Result<(), String> {
        crate::shared::fs_atomic::write_bytes_atomic(
            &self.marker,
            format!("{}\n", self.bundled).as_bytes(),
        )
        .map_err(|e| format!("Cannot record the sample source repo version: {e}"))?;
        self.abandon();
        Ok(())
    }

    fn abandon(&self) {
        let _ = std::fs::remove_file(&self.pending);
    }
}

enum PlaceOutcome {
    Done(Placement),
    Refused(String),
}

fn place(root: &Path, archive: &[u8], journal: &Journal) -> Result<PlaceOutcome, String> {
    // AC-11: an existing non-empty directory with no .git/HEAD is a user artifact,
    // kept untouched. An empty existing directory is treated as greenfield (install).
    let exists = root.exists();
    let is_empty_dir = exists
        && std::fs::read_dir(root)
            .map(|mut it| it.next().is_none())
            .unwrap_or(false);
    if exists && !is_empty_dir && clearly_not_a_repo(root) {
        return Ok(PlaceOutcome::Refused(format!(
            "{} is non-empty and has no .git/HEAD — placement refused to avoid overwriting unrelated work.",
            root.display()
        )));
    }
    if let Some(pending) = journal.interrupted() {
        if let Some(placement) = complete_interrupted(root, archive, journal, &pending)? {
            return Ok(PlaceOutcome::Done(placement));
        }
    }
    if !exists || is_empty_dir {
        journal.begin()?;
        install(archive, root)?;
        journal.finish()?;
        return Ok(PlaceOutcome::Done(Placement::Placed));
    }
    let repo = match git2::Repository::open(root) {
        Ok(repo) => repo,
        Err(e) => {
            log::warn!(
                "Sample source repo at {} can't be read by this version ({e}); leaving it alone",
                root.display()
            );
            return Ok(PlaceOutcome::Done(Placement::Kept));
        }
    };
    // AC-11: a readable repo whose origin remote doesn't match the bundle is a
    // DIFFERENT project the user set up at this path. Keep it; report refusal.
    if let Some(reason) = different_origin(&repo) {
        return Ok(PlaceOutcome::Refused(reason));
    }
    if !should_update(&repo, journal) {
        return Ok(PlaceOutcome::Done(Placement::Kept));
    }
    journal.begin()?;
    match update_in_place(archive, root, &journal.bundled, Overwrite::Nothing)? {
        UpdateOutcome::Applied => {
            journal.finish()?;
            Ok(PlaceOutcome::Done(Placement::Updated))
        }
        UpdateOutcome::Blocked(path) => {
            log::warn!(
                "Not updating the sample source repo: {} isn't tracked and the update would overwrite it",
                path.display()
            );
            journal.abandon();
            Ok(PlaceOutcome::Done(Placement::Kept))
        }
    }
}

/// A readable git repo whose `origin` URL doesn't match this bundle — the user
/// (or an agent) put a different project at this path. Return a human-readable
/// refusal reason, else `None`.
fn different_origin(repo: &git2::Repository) -> Option<String> {
    let expected = &bundle_info().remote_url;
    let origin = repo.find_remote("origin").ok()?;
    let url = origin.url().ok()?.to_string();
    if url == *expected {
        return None;
    }
    Some(format!(
        "sample-source-repo path already holds a different repo (origin = {url}); kept to avoid overwriting."
    ))
}

/// A crash interrupted placing bundle `pending`. Same posture as SW-OC-16:
/// - HEAD at this app's bundle: record it.
/// - HEAD at `pending`: record it, carry on with a normal update.
/// - HEAD at the recorded commit: our own write was cut short; place over it.
/// - HEAD elsewhere: the user's agent moved on; drop the journal, keep.
fn complete_interrupted(
    root: &Path,
    archive: &[u8],
    journal: &Journal,
    pending: &str,
) -> Result<Option<Placement>, String> {
    if !root.exists() || clearly_not_a_repo(root) {
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
        .map_err(|e| format!("Cannot record the sample source repo version: {e}"))?;
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
        "Sample source repo at {} moved on to {head} after an interrupted update; leaving it alone",
        root.display()
    );
    journal.abandon();
    Ok(Some(Placement::Kept))
}

fn clearly_not_a_repo(root: &Path) -> bool {
    !root.join(".git").join("HEAD").is_file()
}

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
    options.include_untracked(false).include_ignored(false);
    repo.statuses(Some(&mut options))
        .map(|statuses| statuses.is_empty())
        .unwrap_or(false)
}

fn install(archive: &[u8], root: &Path) -> Result<(), String> {
    let parent = root.parent().ok_or("Invalid sample source repo location")?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("Cannot create {}: {e}", parent.display()))?;
    let staging = tempfile::Builder::new()
        .prefix(".sample-source-repo-")
        .tempdir_in(parent)
        .map_err(|e| format!("Cannot stage the sample source repo: {e}"))?;
    let staged = staging.path().join(PLACED_DIR);
    materialize(archive, &staged)?;
    // The target may exist (empty — see `place`); rename refuses to clobber a
    // directory on some filesystems, so remove it first when empty.
    if root.exists() {
        let _ = std::fs::remove_dir(root);
    }
    std::fs::rename(&staged, root).map_err(|e| {
        format!(
            "Cannot place the sample source repo at {}: {e}",
            root.display()
        )
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Overwrite {
    Nothing,
    OwnInterruptedWork,
}

enum UpdateOutcome {
    Applied,
    Blocked(PathBuf),
}

/// Same crash-safe ordering as `codesense_docs::update_in_place`: objects first,
/// then the working tree, then the refs. An interrupted update is completed by
/// the next launch via the journal.
fn update_in_place(
    archive: &[u8],
    root: &Path,
    bundled: &str,
    overwrite: Overwrite,
) -> Result<UpdateOutcome, String> {
    let fail = |e: &dyn std::fmt::Display| format!("Cannot update the sample source repo: {e}");
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
    if overwrite == Overwrite::Nothing {
        let current = repo.head().and_then(|h| h.peel_to_tree()).ok();
        let mut blocked: Option<PathBuf> = None;
        tree.walk(git2::TreeWalkMode::PreOrder, |dir, entry| {
            if entry.kind() != Some(git2::ObjectType::Blob) {
                return git2::TreeWalkResult::Ok;
            }
            let path = PathBuf::from(format!("{dir}{}", entry.name().unwrap_or_default()));
            let tracked = current
                .as_ref()
                .is_some_and(|tree| tree.get_path(&path).is_ok());
            if !tracked && root.join(&path).symlink_metadata().is_ok() {
                blocked = Some(path);
                return git2::TreeWalkResult::Abort;
            }
            git2::TreeWalkResult::Ok
        })
        .ok();
        if let Some(path) = blocked {
            return Ok(UpdateOutcome::Blocked(path));
        }
    }
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.force();
    repo.checkout_tree(tree.as_object(), Some(&mut checkout))
        .map_err(|e| fail(&e))?;
    let branch = &bundle_info().branch;
    let message = "sample source repo update";
    repo.reference(&format!("refs/remotes/origin/{branch}"), oid, true, message)
        .map_err(|e| fail(&e))?;
    repo.reference(&format!("refs/heads/{branch}"), oid, true, message)
        .map_err(|e| fail(&e))?;
    Ok(UpdateOutcome::Applied)
}

fn materialize(archive: &[u8], dir: &Path) -> Result<(), String> {
    for (relative, bytes) in archive_entries(archive)? {
        let target = dir.join(&relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&target, bytes).map_err(|e| e.to_string())?;
    }
    let repo = git2::Repository::open(dir)
        .map_err(|e| format!("The bundled sample source repo is not a git repository: {e}"))?;
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.force();
    repo.checkout_head(Some(&mut checkout))
        .map_err(|e| format!("Cannot check out the bundled sample source repo: {e}"))
}

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
                "Unexpected path in the sample source repo bundle: {}",
                path.display()
            ));
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        out.push((path, bytes));
    }
    Ok(out)
}

fn archive_commit(archive: &[u8]) -> Result<String, String> {
    let branch = PathBuf::from(format!(".git/refs/heads/{}", bundle_info().branch));
    archive_entries(archive)?
        .into_iter()
        .find(|(path, _)| path == &branch)
        .map(|(_, bytes)| String::from_utf8_lossy(&bytes).trim().to_string())
        .ok_or_else(|| "The sample source repo bundle has no branch".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Principal;

    /// The archive this build bundles (generated by build.rs).
    fn real_bundle() -> &'static [u8] {
        include_bytes!("../../resources/sample-source-repo.tar.gz")
    }

    /// A one-commit bundle built with libgit2, for the test suite — same shape
    /// as `codesense_docs::tests::bundle_with`.
    fn bundle_with(readme: &str) -> Vec<u8> {
        bundle_with_origin(readme, &bundle_info().remote_url)
    }

    fn bundle_with_origin(readme: &str, origin_url: &str) -> Vec<u8> {
        bundle_of(&[("README.md", readme)], origin_url)
    }

    fn bundle_of(files: &[(&str, &str)], origin_url: &str) -> Vec<u8> {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let info = bundle_info();
        let repo = git2::Repository::init_opts(
            root,
            git2::RepositoryInitOptions::new().initial_head(&info.branch),
        )
        .unwrap();
        repo.remote("origin", origin_url).unwrap();
        let mut index = repo.index().unwrap();
        for (path, body) in files {
            std::fs::write(root.join(path), body).unwrap();
            index.add_path(Path::new(path)).unwrap();
        }
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig =
            git2::Signature::new("sample", "sample@codesense.ai", &git2::Time::new(0, 0)).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "sample", &tree, &[])
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
    fn real_bundle_is_a_loadable_clone_with_five_commits() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let placed = ensure_placed(&home, &Principal::local_guest(), real_bundle()).unwrap();
        assert_eq!(placed.placement, Placement::Placed);
        let repo = git2::Repository::open(&placed.root).unwrap();
        // The sample ships five commits (one per DEMO story).
        let mut walk = repo.revwalk().unwrap();
        walk.push_head().unwrap();
        let n = walk.count();
        assert_eq!(n, 5, "sample-source-repo should ship 5 commits, got {n}");
        assert_eq!(
            repo.find_remote("origin").unwrap().url(),
            Ok(bundle_info().remote_url.as_str())
        );
        assert_eq!(
            repo.head().unwrap().shorthand(),
            Ok(bundle_info().branch.as_str())
        );
        // Every DEMO story is represented on disk.
        assert!(placed.root.join("README.md").is_file());
        assert!(placed.root.join("CHANGELOG.md").is_file());
        assert!(placed.root.join("src-tauri/src/kill_agent.rs").is_file());
        assert!(placed.root.join("src-tauri/src/mcp_public.rs").is_file());
        assert!(placed.root.join("src-tauri/src/kill_hold.rs").is_file());
        assert!(placed.root.join("src/components/DagView.tsx").is_file());
        assert!(placed
            .root
            .join("src/components/ConfirmModal.tsx")
            .is_file());
    }

    #[test]
    fn later_launches_keep_the_placed_copy() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let who = Principal::from(7_u64);
        let archive = bundle_with("v1\n");
        let first = ensure_placed(&home, &who, &archive).unwrap();
        assert_eq!(first.placement, Placement::Placed);
        let again = ensure_placed(&home, &who, &archive).unwrap();
        assert_eq!(again.placement, Placement::Kept);
        assert_eq!(again.root, first.root);
    }

    #[test]
    fn a_newer_bundle_replaces_an_untouched_copy() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let who = Principal::from(7_u64);
        let first = ensure_placed(&home, &who, &bundle_with("v1\n")).unwrap();
        let newer = bundle_with("v2\n");
        let updated = ensure_placed(&home, &who, &newer).unwrap();
        assert_eq!(updated.placement, Placement::Updated);
        assert_eq!(updated.root, first.root);
        assert_eq!(head(&updated.root), archive_commit(&newer).unwrap());
    }

    /// The walk that finds an untracked file in the way stops with `Abort`, which git2
    /// reports as an error since 0.20: the update is still skipped, not failed.
    #[test]
    fn an_untracked_file_in_the_way_of_an_update_keeps_the_placed_copy() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let who = Principal::from(7_u64);
        let first = ensure_placed(&home, &who, &bundle_with("v1\n")).unwrap();
        let placed_head = head(&first.root);
        std::fs::write(first.root.join("NOTES.md"), "mine\n").unwrap();
        let newer = bundle_of(
            &[("README.md", "v2\n"), ("NOTES.md", "theirs\n")],
            &bundle_info().remote_url,
        );
        let again = ensure_placed(&home, &who, &newer).unwrap();
        assert_eq!(again.placement, Placement::Kept);
        assert_eq!(head(&again.root), placed_head);
        assert_eq!(
            std::fs::read_to_string(first.root.join("NOTES.md")).unwrap(),
            "mine\n"
        );
    }

    #[test]
    fn ac11_a_non_empty_directory_without_dot_git_is_refused_not_overwritten() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let who = Principal::from(7_u64);
        let root = sample_repo_root(&home, &who);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("their-file.txt"), "theirs").unwrap();
        let placed = ensure_placed(&home, &who, &bundle_with("v1\n")).unwrap();
        assert_eq!(placed.placement, Placement::Refused);
        assert!(placed.refused_reason.is_some());
        // The user's file is untouched.
        assert_eq!(
            std::fs::read_to_string(root.join("their-file.txt")).unwrap(),
            "theirs"
        );
        // No .git appeared.
        assert!(!root.join(".git").exists());
    }

    #[test]
    fn ac11_a_repo_with_a_different_origin_is_refused_not_overwritten() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let who = Principal::from(7_u64);
        let foreign =
            bundle_with_origin("not ours\n", "https://github.com/someone-else/project.git");
        let root = sample_repo_root(&home, &who);
        std::fs::create_dir_all(root.parent().unwrap()).unwrap();
        {
            let staged = tempfile::tempdir().unwrap();
            super::materialize(&foreign, staged.path()).unwrap();
            // The TempDir's drop will remove the moved folder's old entry;
            // guard against that by consuming the handle.
            let staged_path = staged.keep();
            std::fs::rename(&staged_path, &root).unwrap();
        }
        let foreign_head = head(&root);
        let ours = bundle_with("ours\n");
        let placed = ensure_placed(&home, &who, &ours).unwrap();
        assert_eq!(placed.placement, Placement::Refused);
        // The user's repo is untouched.
        assert_eq!(head(&root), foreign_head);
    }

    #[test]
    fn an_empty_target_directory_is_treated_as_greenfield() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let who = Principal::from(7_u64);
        let root = sample_repo_root(&home, &who);
        std::fs::create_dir_all(&root).unwrap();
        let placed = ensure_placed(&home, &who, &bundle_with("v1\n")).unwrap();
        assert_eq!(placed.placement, Placement::Placed);
    }

    #[test]
    fn a_crash_between_install_and_the_version_record_heals_on_the_next_launch() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let who = Principal::from(7_u64);
        let archive = bundle_with("v1\n");
        let bundled = archive_commit(&archive).unwrap();
        ensure_placed(&home, &who, &archive).unwrap();
        std::fs::remove_file(marker_path(&home, &who)).unwrap();
        std::fs::write(pending_path(&home, &who), format!("{bundled}\n")).unwrap();
        let healed = ensure_placed(&home, &who, &archive).unwrap();
        assert!(matches!(
            healed.placement,
            Placement::Updated | Placement::Kept
        ));
        assert!(!pending_path(&home, &who).exists());
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
