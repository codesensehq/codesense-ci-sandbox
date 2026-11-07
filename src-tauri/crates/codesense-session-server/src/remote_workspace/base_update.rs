// SPDX-License-Identifier: MIT
//! Move a prepared workspace to a newer base commit, explicitly.
//!
//! The delta arrives as an ordinary pipelined bundle upload under its own
//! request id (the carrier). The worktree moves only by fast-forward, only
//! when it is clean (no tracked or untracked change, and no managed copy
//! edited since CodeSense wrote it) and still on its branch.
//!
//! Git overwrites ignored files on a fast-forward without asking, so every
//! path the new base adds that already exists in the worktree is checked
//! first:
//! - a managed copy of this workspace, unedited: removed when the caller asks
//!   (`removeManagedCopies`), else the update is refused with the list;
//! - a managed copy that was edited: never removed (publish or discard it
//!   first; `reverse_sync_worktree` previews it);
//! - any other ignored file: never overwritten.
//!
//! Removal and the fast-forward are one step: the removed copies come back if
//! the merge fails, and this workspace's exclude block loses exactly the
//! paths the repo now tracks.
use super::{
    claim_seal, directory, exists, git, git_output, import_bundle, io, locks, open, open_directory,
    owner_directory, read, receipt_key, replace, resolve_completed_target, save, seal_bundle,
    settle_upload, test_hold, validate, write_new,
};
use codesense_pty_core::remote_workspace::{
    ManagedCopy, PrepareWorkspace, UpdateWorkspaceBase, WorkspaceBaseUpdate,
};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::{Duration, Instant};

const LISTED: usize = 50;
const STATUS_BYTES: usize = 1024 * 1024;
const EXCLUDE_START: &str = "# >>> codesense managed";
const EXCLUDE_END: &str = "# <<< codesense managed";
/// The most managed records one update checks.
const MAX_MANAGED: usize = 100_000;
/// A managed copy larger than this is never held in memory for a restore.
const MAX_RESTORE_BYTES: u64 = 64 * 1024 * 1024;

fn answer(status: &str) -> WorkspaceBaseUpdate {
    WorkspaceBaseUpdate {
        status: status.into(),
        ..Default::default()
    }
}

/// Reclaim the carrier upload (staging, records, seal claim, receipt) once an
/// update reached a final answer. Unknown outcomes keep it, so a retry
/// resumes the same upload.
fn settle_carrier(receipts: &File, carrier: &PrepareWorkspace) {
    let key = receipt_key(&carrier.origin_key, &carrier.request_id);
    let _ = settle_upload(receipts, &key, carrier);
    if exists(receipts, &key).unwrap_or(false) {
        super::discard(receipts, &key);
    }
}

pub(crate) fn update_base(
    root: &Path,
    owner: u64,
    request: UpdateWorkspaceBase,
    alive: impl Fn() -> bool,
) -> Result<WorkspaceBaseUpdate, String> {
    let carrier = &request.upload;
    validate(carrier)?;
    if owner != carrier.owner_id
        || owner != request.target.owner_id
        || request.managed.len() > MAX_MANAGED
    {
        return Err("private workspace update refused".into());
    }
    // The carrier's whole-bundle hash runs before the live workspace is
    // locked: only the seal claim takes that lock, briefly.
    // A retry of an update that already finished finds nothing staged; its
    // seal error only matters if the worktree has not moved yet.
    let sealed = if !request.precheck && carrier.bundle.size > 0 {
        seal_bundle(root, owner, carrier.clone(), &alive)
    } else {
        Ok(())
    };
    let _repository = locks::repository(&carrier.repo_path, &alive)?;
    let _workspace = locks::workspace(&carrier.worktree_path, &alive)?;
    let deadline = Instant::now() + Duration::from_secs(30 * 60);
    // On another branch or a detached HEAD the workspace no longer resolves
    // as prepared; that is an answer, not an error.
    {
        let worktree = open_directory(&carrier.worktree_path, false)?;
        match git(
            &worktree,
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
            &alive,
            deadline,
        ) {
            Ok(branch) if branch == carrier.branch => {}
            _ => {
                if !request.precheck {
                    let receipts = directory(&io(File::open(root))?, "remote-workspaces", true)?;
                    settle_carrier(&receipts, carrier);
                }
                return Ok(WorkspaceBaseUpdate {
                    previous_commit: git(&worktree, &["rev-parse", "HEAD"], &alive, deadline).ok(),
                    message: Some(format!(
                        "The workspace is not on its branch {} (another branch or a detached HEAD); check it out, then update the base again",
                        carrier.branch
                    )),
                    ..answer("refused")
                });
            }
        }
    }
    // Abandoned updates of this workspace: reclaim what they staged, unless
    // one is still in use.
    {
        let receipts = directory(&io(File::open(root))?, "remote-workspaces", true)?;
        for id in &request.discard_uploads {
            if uuid::Uuid::parse_str(id).is_err() || *id == carrier.request_id {
                continue;
            }
            let key = receipt_key(&carrier.origin_key, id);
            if super::reclaim_upload(root, &receipts, &key)? {
                let mut abandoned = carrier.clone();
                abandoned.request_id = id.clone();
                super::release_seal(&receipts, &key, &abandoned);
            }
        }
    }
    let (receipts, prepared_key, mut receipt) =
        resolve_completed_target(root, owner, &request.target, &alive)?;
    let prepared = receipt.intent.clone();
    if carrier.request_id == prepared.request_id
        || carrier.origin_key != prepared.origin_key
        || carrier.repository_key != prepared.repository_key
        || carrier.clone_url != prepared.clone_url
        || carrier.repo_path != prepared.repo_path
        || carrier.worktree_path != prepared.worktree_path
        || carrier.branch != prepared.branch
    {
        return Err("the update does not belong to this workspace".into());
    }
    let finish = |result: WorkspaceBaseUpdate| -> Result<WorkspaceBaseUpdate, String> {
        if !request.precheck {
            settle_carrier(&receipts, carrier);
        }
        Ok(result)
    };
    let worktree = open_directory(&prepared.worktree_path, false)?;
    owner_directory(&worktree)?;
    let previous = git(&worktree, &["rev-parse", "HEAD"], &alive, deadline)?;

    let mut dirty = match git_output(
        &worktree,
        &["status", "--porcelain=v1"],
        &alive,
        deadline,
        STATUS_BYTES,
    ) {
        Ok(bytes) => String::from_utf8_lossy(&bytes)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>(),
        Err(error) if error.contains("exceeds") => {
            return finish(WorkspaceBaseUpdate {
                previous_commit: Some(previous),
                message: Some(
                    "The workspace has too many uncommitted changes to list; commit or discard them, then update the base again"
                        .into(),
                ),
                ..answer("refusedDirty")
            });
        }
        Err(error) => return Err(error),
    };
    // A managed copy is ignored by Git, so `git status` can't see an edit to
    // it; the desktop's record of what it wrote can.
    // A record whose path the repo tracks now (it was a managed copy before
    // an update made it a tracked file) is history, not a managed copy.
    let tracked: std::collections::HashSet<String> = if request.managed.is_empty() {
        Default::default()
    } else {
        String::from_utf8(git_output(
            &worktree,
            &["ls-files", "-z"],
            &alive,
            deadline,
            64 * 1024 * 1024,
        )?)
        .map_err(|_| "invalid Git output")?
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect()
    };
    let managed: BTreeMap<&str, &str> = request
        .managed
        .iter()
        .filter(|record| !tracked.contains(&record.path))
        .map(|ManagedCopy { path, blake3 }| (path.as_str(), blake3.as_str()))
        .collect();
    for (path, recorded) in &managed {
        if !relative_path(path) {
            return Err("invalid managed record path".into());
        }
        if let Some(actual) = hash_in(&worktree, path)? {
            if actual != *recorded {
                dirty.push(format!("M! {path} (managed copy edited)"));
            }
        }
    }
    if !dirty.is_empty() {
        let edited = dirty.iter().any(|line| line.starts_with("M! "));
        dirty.truncate(LISTED);
        return finish(WorkspaceBaseUpdate {
            previous_commit: Some(previous),
            dirty,
            message: Some(if edited {
                "The workspace has uncommitted changes, including edited CodeSense copies (preview those with reverse_sync_worktree); commit, publish or discard them, then update the base again".into()
            } else {
                "The workspace has uncommitted changes; commit or discard them, then update the base again".into()
            }),
            ..answer("refusedDirty")
        });
    }
    if request.precheck {
        return Ok(WorkspaceBaseUpdate {
            previous_commit: Some(previous),
            ..answer("ready")
        });
    }
    let commit = carrier.bundle.commit.clone();
    if previous == commit {
        // A crash between the merge and the save leaves the worktree moved
        // but unrecorded; record it now.
        if receipt.base_commit.as_deref() != Some(commit.as_str()) {
            receipt.base_commit = Some(commit.clone());
            save(&receipts, &prepared_key, &receipt, false)?;
        }
        return finish(WorkspaceBaseUpdate {
            previous_commit: Some(previous),
            commit: Some(commit),
            ..answer("upToDate")
        });
    }

    sealed?;
    let carrier_key = receipt_key(&carrier.origin_key, &carrier.request_id);
    let repo = open_directory(&prepared.repo_path, false)?;
    owner_directory(&repo)?;
    {
        let _staging = locks::staging(&super::bundle_lock(root, &carrier_key), &alive)?;
        if carrier.bundle.size > 0 {
            claim_seal(root, &receipts, &carrier_key, carrier)?;
        }
        import_bundle(
            root,
            &receipts,
            &carrier_key,
            carrier,
            &repo,
            &alive,
            deadline,
        )?;
    }
    let is_ancestor = |older: &str, newer: &str| {
        git(
            &worktree,
            &["merge-base", "--is-ancestor", older, newer],
            &alive,
            deadline,
        )
        .is_ok()
    };
    if is_ancestor(&commit, &previous) {
        return finish(WorkspaceBaseUpdate {
            previous_commit: Some(previous.clone()),
            commit: Some(previous),
            ..answer("upToDate")
        });
    }
    if !is_ancestor(&previous, &commit) {
        return finish(WorkspaceBaseUpdate {
            previous_commit: Some(previous),
            commit: Some(commit),
            message: Some(
                "The workspace's branch has commits the new base lacks; it can't fast-forward"
                    .into(),
            ),
            ..answer("refusedDiverged")
        });
    }

    let added = String::from_utf8(git_output(
        &worktree,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "--diff-filter=A",
            "-z",
            &previous,
            &commit,
        ],
        &alive,
        deadline,
        16 * 1024 * 1024,
    )?)
    .map_err(|_| "invalid Git output")?;
    let workspace_id = request.target.handle.as_str();
    let block = match exclude_text(&prepared.repo_path)
        .and_then(|text| workspace_block_paths(&text, workspace_id))
    {
        Ok(paths) => paths,
        Err(error) => {
            return finish(WorkspaceBaseUpdate {
                previous_commit: Some(previous),
                commit: Some(commit),
                message: Some(format!(
                    "This workspace's managed exclude block can't be read ({error}); nothing was changed"
                )),
                ..answer("refused")
            });
        }
    };
    let legacy = exclude_text(&prepared.repo_path)
        .map(|text| legacy_entries(&text))
        .unwrap_or_default();
    let mut legacy_copies = 0;
    let (mut removable, mut modified, mut unmanaged) = (Vec::new(), Vec::new(), Vec::new());
    for path in added.split('\0').filter(|path| !path.is_empty()) {
        if !present(&worktree, path) {
            continue;
        }
        if !block.iter().any(|entry| entry == path) {
            if legacy.iter().any(|entry| legacy_covers(entry, path)) {
                legacy_copies += 1;
            }
            unmanaged.push(path.to_string());
        } else if managed.get(path).is_some_and(|recorded| {
            hash_in(&worktree, path).ok().flatten().as_deref() == Some(*recorded)
        }) {
            removable.push(path.to_string());
        } else {
            modified.push(path.to_string());
        }
    }
    if !modified.is_empty() || !unmanaged.is_empty() {
        let message = if !modified.is_empty() {
            format!(
                "The new base tracks {} CodeSense copy(ies) that were edited in this workspace; publish them (preview with reverse_sync_worktree) or discard the edits, then update the base again",
                modified.len()
            )
        } else if legacy_copies > 0 {
            // Copies listed only in the older shared block (from before
            // per-workspace blocks) can't be told apart from
            // another workspace's, so they are never removed automatically.
            format!(
                "The new base tracks {} file(s) CodeSense copied here before copies were tracked per workspace; CodeSense can't remove those safely yet. Move them aside (or delete them if unedited), then update the base again",
                unmanaged.len()
            )
        } else {
            format!(
                "The new base tracks {} ignored file(s) CodeSense did not place; move them aside, then update the base again",
                unmanaged.len()
            )
        };
        return finish(WorkspaceBaseUpdate {
            previous_commit: Some(previous),
            commit: Some(commit),
            managed_overlap: removable,
            modified_managed: modified,
            unmanaged_overlap: unmanaged,
            message: Some(message),
            ..answer("refusedManagedOverlap")
        });
    }
    if !removable.is_empty() && !request.remove_managed_copies {
        return finish(WorkspaceBaseUpdate {
            previous_commit: Some(previous),
            commit: Some(commit),
            message: Some(format!(
                "The new base tracks {} file(s) CodeSense copied into this workspace; update with removeManagedCopies to replace those copies with the repo's",
                removable.len()
            )),
            managed_overlap: removable,
            ..answer("refusedManagedOverlap")
        });
    }

    // Removal and fast-forward are one write: readers wait it out, and a
    // failed merge puts every removed copy back.
    {
        let _writing = locks::writing(&prepared.worktree_path);
        let mut removed: Vec<(String, Vec<u8>, u32)> = Vec::new();
        let moved = (|| -> Result<(), String> {
            for path in &removable {
                let (bytes, mode) = take_in(&worktree, path)?;
                removed.push((path.clone(), bytes, mode));
            }
            test_hold("before-merge");
            let _refs = locks::refs(&prepared.repo_path, &alive)?;
            git(
                &worktree,
                &["merge", "--ff-only", "--no-edit", &commit],
                &alive,
                deadline,
            )?;
            Ok(())
        })();
        if let Err(error) = moved {
            let restored = removed
                .iter()
                .all(|(path, bytes, mode)| restore_in(&worktree, path, bytes, *mode).is_ok());
            return Err(if restored {
                format!("{error}; the workspace was left as it was")
            } else {
                format!("{error}; some managed copies could not be put back")
            });
        }
    }
    if git(&worktree, &["rev-parse", "HEAD"], &alive, deadline)? != commit {
        return Err("the workspace did not move to the new base".into());
    }
    let mut message = None;
    if !removable.is_empty() {
        if let Err(error) = drop_from_block(&prepared.repo_path, workspace_id, &removable, &alive) {
            message = Some(format!(
                "Updated; the managed exclude block still lists the removed copies ({error})"
            ));
        }
    }
    // The machine remembers the base it is on, so a later fetch knows
    // these commits already.
    receipt.base_commit = Some(commit.clone());
    save(&receipts, &prepared_key, &receipt, false)?;
    finish(WorkspaceBaseUpdate {
        previous_commit: Some(previous),
        commit: Some(commit),
        managed_overlap: removable,
        message,
        ..answer("updated")
    })
}

/// The lines of the older, shared (unlabeled) CodeSense block.
fn legacy_entries(text: &str) -> Vec<String> {
    let mut inside = false;
    let mut entries = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line == EXCLUDE_START {
            inside = true;
        } else if line == EXCLUDE_END {
            inside = false;
        } else if inside && !line.is_empty() && !line.starts_with('#') {
            entries.push(line.to_string());
        }
    }
    entries
}

/// Whether a shared-block line names `path` (only for the refusal message).
fn legacy_covers(entry: &str, path: &str) -> bool {
    let entry = entry.trim_start_matches('/').trim_end_matches('/');
    !entry.is_empty() && (path == entry || path.starts_with(&format!("{entry}/")))
}

/// A contained, normalized workspace-relative file path.
fn relative_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.contains('\0')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// The clone's shared `.git/info/exclude`, or empty when it has none.
fn exclude_text(repo_path: &str) -> Result<String, String> {
    let repo = open_directory(repo_path, false)?;
    let git_dir = directory(&repo, ".git", false)?;
    if !exists(&git_dir, "info")? {
        return Ok(String::new());
    }
    let info = directory(&git_dir, "info", false)?;
    if !exists(&info, "exclude")? {
        return Ok(String::new());
    }
    String::from_utf8(read(&info, "exclude")?).map_err(|_| "managed exclude is not UTF8".into())
}

/// This workspace's paths in its block, read by the writer's own parser
/// (`files.rs`). A line that writer never writes (a hand edit, an
/// unanchored name) makes the block uncertain: refuse rather than guess.
pub(crate) fn workspace_block_paths(text: &str, workspace_id: &str) -> Result<Vec<String>, String> {
    let block = super::files::workspace_block(text, workspace_id)?;
    if block.foreign > 0 {
        return Err("this workspace's exclude block holds lines CodeSense never writes".into());
    }
    Ok(block.paths)
}

/// `text` with this workspace's block listing exactly `paths` (none: no
/// block), written by the same writer Step 2 uses.
pub(crate) fn with_workspace_block(
    text: &str,
    workspace_id: &str,
    paths: &[String],
) -> Result<String, String> {
    let block = super::files::workspace_block(text, workspace_id)?;
    let bytes =
        super::files::workspace_exclude_bytes(block, workspace_id, paths, &|path| Ok(path.into()))?;
    String::from_utf8(bytes).map_err(|_| "managed exclude is not UTF8".into())
}

/// Remove `gone` from this workspace's block, under the clone's exclude lock,
/// re-reading the file so a concurrent writer's lines are kept.
fn drop_from_block(
    repo_path: &str,
    workspace_id: &str,
    gone: &[String],
    alive: &dyn Fn() -> bool,
) -> Result<(), String> {
    let _exclude = locks::exclude(repo_path, alive)?;
    let text = exclude_text(repo_path)?;
    let listed = workspace_block_paths(&text, workspace_id)?;
    let kept: Vec<String> = listed.into_iter().filter(|p| !gone.contains(p)).collect();
    let next = with_workspace_block(&text, workspace_id, &kept)?;
    let repo = open_directory(repo_path, false)?;
    let git_dir = directory(&repo, ".git", false)?;
    let info = directory(&git_dir, "info", false)?;
    replace(&info, "exclude", next.as_bytes())
}

/// Walk to `path`'s parent under the held worktree, never following a link.
fn parent_of<'a>(worktree: &File, path: &'a str) -> Option<(File, &'a str)> {
    let (dirs, leaf) = match path.rsplit_once('/') {
        Some((dirs, leaf)) => (Some(dirs), leaf),
        None => (None, path),
    };
    let mut fd = worktree.try_clone().ok()?;
    for component in dirs.into_iter().flat_map(|d| d.split('/')) {
        fd = directory(&fd, component, false).ok()?;
    }
    Some((fd, leaf))
}

fn present(worktree: &File, path: &str) -> bool {
    parent_of(worktree, path).is_some_and(|(parent, leaf)| exists(&parent, leaf).unwrap_or(false))
}

/// BLAKE3 (hex) of the regular file at `path`, or `None` when absent.
fn hash_in(worktree: &File, path: &str) -> Result<Option<String>, String> {
    let Some((parent, leaf)) = parent_of(worktree, path) else {
        return Ok(None);
    };
    if !exists(&parent, leaf)? {
        return Ok(None);
    }
    let mut file = open(&parent, leaf, libc::O_RDONLY | libc::O_NONBLOCK)?;
    if !io(file.metadata())?.is_file() {
        return Ok(Some(String::new()));
    }
    let mut hash = blake3::Hasher::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = io(file.read(&mut buffer))?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(Some(hash.finalize().to_hex().to_string()))
}

/// Remove the managed copy at `path`, returning its bytes and mode so it can
/// be put back.
fn take_in(worktree: &File, path: &str) -> Result<(Vec<u8>, u32), String> {
    let (parent, leaf) = parent_of(worktree, path).ok_or("managed copy is unreachable")?;
    let mut file = open(&parent, leaf, libc::O_RDONLY | libc::O_NONBLOCK)?;
    let meta = io(file.metadata())?;
    if !meta.is_file() || meta.len() > MAX_RESTORE_BYTES {
        return Err(format!("the managed copy {path} can't be removed safely"));
    }
    let mut bytes = Vec::new();
    io(file.read_to_end(&mut bytes))?;
    let name = std::ffi::CString::new(leaf).map_err(|_| "invalid path component")?;
    // SAFETY: held parent descriptor and a validated single component.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(format!("could not remove the managed copy {path}"));
    }
    io(super::durable::sync(&parent))?;
    Ok((bytes, meta.mode() & 0o7777))
}

fn restore_in(worktree: &File, path: &str, bytes: &[u8], mode: u32) -> Result<(), String> {
    let (parent, leaf) = parent_of(worktree, path).ok_or("managed copy is unreachable")?;
    if exists(&parent, leaf)? {
        return Err(format!("{path} was replaced before it could be put back"));
    }
    write_new(&parent, leaf, bytes)?;
    let file = open(&parent, leaf, libc::O_RDONLY | libc::O_NONBLOCK)?;
    // SAFETY: a descriptor this function just opened; fchmod takes no pointers.
    if unsafe { libc::fchmod(file.as_raw_fd(), mode as libc::mode_t) } != 0 {
        return Err(format!("could not restore the mode of {path}"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "base_update_tests.rs"]
mod tests;
