// SPDX-License-Identifier: MIT
//! update_workspace_base on the machine, with real Git.
use super::super::tests::fixture;
use super::super::{arm_test_hold, bundle_status, digest, prepare, reconcile, stage_chunk};
use super::*;
use codesense_host_proto::test_support::strip_attribution_env;
use codesense_pty_core::remote_files::FileTarget;
use codesense_pty_core::remote_workspace::{
    chunk_range, GitBundle, PrepareWorkspace, StagedBundleChunk,
};
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = strip_attribution_env(&mut Command::new("git"))
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Stage `bytes` as `request`'s pipelined upload (unsealed).
pub(crate) fn stage_pipelined(root: &Path, request: &PrepareWorkspace, bytes: &[u8]) {
    let status = bundle_status(root, 41, request.clone(), || true).unwrap();
    for index in status.received.missing() {
        let (offset, len) = chunk_range(bytes.len() as u64, index).unwrap();
        let chunk = bytes[offset as usize..offset as usize + len].to_vec();
        stage_chunk(
            root,
            41,
            StagedBundleChunk {
                request: request.clone(),
                index,
                sha256: digest(&chunk),
                bytes: chunk,
            },
            || true,
        )
        .unwrap();
    }
}

/// Send `bytes` as `request`'s pipelined upload and seal it.
pub(crate) fn upload_pipelined(root: &Path, request: &PrepareWorkspace, bytes: &[u8]) {
    stage_pipelined(root, request, bytes);
    seal_bundle(root, 41, request.clone(), || true).unwrap();
}

struct Prepared {
    temp: tempfile::TempDir,
    request: PrepareWorkspace,
    target: FileTarget,
    base: String,
}

impl Prepared {
    fn work(&self) -> &Path {
        Path::new(&self.request.worktree_path)
    }
    fn exclude(&self) -> std::path::PathBuf {
        Path::new(&self.request.repo_path).join(".git/info/exclude")
    }
    /// Place a managed copy the way Step 2 does: the file, plus its line in
    /// this workspace's exclude block (written by the block writer).
    fn place_managed(&self, path: &str, body: &str) -> ManagedCopy {
        let full = self.work().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, body).unwrap();
        std::fs::set_permissions(&full, std::fs::Permissions::from_mode(0o640)).unwrap();
        std::fs::create_dir_all(self.exclude().parent().unwrap()).unwrap();
        let text = std::fs::read_to_string(self.exclude()).unwrap_or_default();
        let mut listed = workspace_block_paths(&text, &self.target.handle).unwrap();
        listed.push(path.into());
        std::fs::write(
            self.exclude(),
            with_workspace_block(&text, &self.target.handle, &listed).unwrap(),
        )
        .unwrap();
        ManagedCopy {
            path: path.into(),
            blake3: blake3::hash(body.as_bytes()).to_hex().to_string(),
        }
    }
}

fn prepared_workspace() -> Prepared {
    let (temp, request) = fixture();
    let bytes = std::fs::read(temp.path().join("input.bundle")).unwrap();
    upload_pipelined(temp.path(), &request, &bytes);
    let result = prepare(temp.path(), 41, request.clone(), || true);
    assert_eq!(result.status, "prepared", "{result:?}");
    let target = FileTarget {
        owner_id: 41,
        origin_key: request.origin_key.clone(),
        request_id: request.request_id.clone(),
        handle: result.handle.unwrap(),
    };
    let base = request.bundle.commit.clone();
    Prepared {
        temp,
        request,
        target,
        base,
    }
}

/// Three new base commits in the (offline) source, and the delta bundle the
/// desktop builds for them: `^<prepared base>`, so only new objects travel.
fn newer_base(prepared: &Prepared, id: u8, files: &[(&str, &str)]) -> (PrepareWorkspace, Vec<u8>) {
    let source = prepared.temp.path().join("source-offline");
    for (n, (path, body)) in files.iter().enumerate() {
        let full = source.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, body).unwrap();
        git_in(&source, &["add", path]);
        git_in(&source, &["commit", "-m", &format!("base {n}")]);
    }
    while git_in(&source, &["rev-list", "--count", "HEAD"])
        .parse::<u32>()
        .unwrap()
        < 4
    {
        git_in(&source, &["commit", "--allow-empty", "-m", "more base"]);
    }
    git_in(&source, &["branch", "-f", "codesense-prepared", "HEAD"]);
    let path = prepared.temp.path().join(format!("delta-{id}.bundle"));
    git_in(
        &source,
        &[
            "bundle",
            "create",
            path.to_str().unwrap(),
            "refs/heads/codesense-prepared",
            &format!("^{}", prepared.base),
        ],
    );
    let bytes = std::fs::read(&path).unwrap();
    let mut carrier = prepared.request.clone();
    carrier.request_id = format!("00000000-0000-4000-8000-0000000000{id:02x}");
    carrier.bundle = GitBundle {
        size: bytes.len() as u64,
        digest: digest(&bytes),
        commit: git_in(&source, &["rev-parse", "HEAD"]),
    };
    (carrier, bytes)
}

fn update_with(
    prepared: &Prepared,
    carrier: &PrepareWorkspace,
    precheck: bool,
    remove: bool,
    managed: &[ManagedCopy],
) -> Result<WorkspaceBaseUpdate, String> {
    update_base(
        prepared.temp.path(),
        41,
        UpdateWorkspaceBase {
            target: prepared.target.clone(),
            upload: carrier.clone(),
            precheck,
            remove_managed_copies: remove,
            managed: managed.to_vec(),
            discard_uploads: Vec::new(),
        },
        || true,
    )
}

fn update(
    prepared: &Prepared,
    carrier: &PrepareWorkspace,
    precheck: bool,
    remove: bool,
) -> WorkspaceBaseUpdate {
    update_with(prepared, carrier, precheck, remove, &[]).unwrap()
}

fn head(prepared: &Prepared) -> String {
    git_in(prepared.work(), &["rev-parse", "HEAD"])
}

fn staging_files(prepared: &Prepared) -> Vec<String> {
    let receipts = prepared.temp.path().join("remote-workspaces");
    let mut names: Vec<String> = std::fs::read_dir(receipts)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("bundle-") || name.starts_with("sealed-"))
        .collect();
    names.sort();
    names
}

#[test]
fn xs9_update_base_on_a_clean_worktree_sends_the_delta_and_fast_forwards() {
    let prepared = prepared_workspace();
    let (carrier, bytes) = newer_base(
        &prepared,
        0xc1,
        &[("a.txt", "one"), ("dir/b.txt", "two"), ("c.txt", "three")],
    );
    // Delta only: the bundle names the prepared base as its prerequisite.
    assert!(bytes
        .windows(41)
        .any(|w| w == format!("-{}", prepared.base).as_bytes()));
    let ready = update(&prepared, &carrier, true, false);
    assert_eq!(ready.status, "ready", "{ready:?}");
    assert_eq!(head(&prepared), prepared.base, "a precheck moves nothing");
    upload_pipelined(prepared.temp.path(), &carrier, &bytes);
    let done = update(&prepared, &carrier, false, false);
    assert_eq!(done.status, "updated", "{done:?}");
    assert_eq!(
        done.previous_commit.as_deref(),
        Some(prepared.base.as_str())
    );
    assert_eq!(done.commit.as_deref(), Some(carrier.bundle.commit.as_str()));
    assert_eq!(head(&prepared), carrier.bundle.commit, "HEAD advanced");
    assert_eq!(
        std::fs::read_to_string(prepared.work().join("dir/b.txt")).unwrap(),
        "two"
    );
    // Every byte of the carrier upload is reclaimed (bytes, records, seal
    // claim, receipt); the workspace still reconciles, now on its new base.
    assert!(
        staging_files(&prepared).is_empty(),
        "{:?}",
        staging_files(&prepared)
    );
    let carrier_key = receipt_key(&carrier.origin_key, &carrier.request_id);
    assert!(!prepared
        .temp
        .path()
        .join("remote-workspaces")
        .join(&carrier_key)
        .exists());
    let settled = reconcile(prepared.temp.path(), 41, prepared.request.clone(), || true)
        .unwrap()
        .unwrap();
    assert_eq!(settled.status, "alreadyPrepared");
    assert_eq!(
        settled.resolved_commit.as_deref(),
        Some(carrier.bundle.commit.as_str()),
        "the machine remembers the base it moved to"
    );
    // A retry of the same, finished update answers instead of failing.
    assert_eq!(update(&prepared, &carrier, false, false).status, "upToDate");
    assert!(staging_files(&prepared).is_empty());
}

/// The diff's prep fork follows a base update: the new base's commits are
/// not the workspace's work, and work committed on top of it is.
#[test]
fn the_remote_diff_forks_from_the_updated_base_not_the_preparation_commit() {
    use codesense_pty_core::remote_files::{FileOperation, FileReply, FileRequest};
    let prepared = prepared_workspace();
    let (carrier, bytes) = newer_base(&prepared, 0xc7, &[("a.txt", "one")]);
    upload_pipelined(prepared.temp.path(), &carrier, &bytes);
    assert_eq!(update(&prepared, &carrier, false, false).status, "updated");
    let listed = || match crate::remote_workspace::files::handle(
        prepared.temp.path(),
        41,
        FileRequest {
            target: prepared.target.clone(),
            operation: FileOperation::Changes { base_ref: None },
        },
        || true,
    )
    .unwrap()
    {
        FileReply::Changes { files, .. } => files.into_iter().map(|f| f.path).collect::<Vec<_>>(),
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(listed(), Vec::<String>::new(), "the new base is not work");
    std::fs::write(prepared.work().join("mine.txt"), "mine\n").unwrap();
    git_in(prepared.work(), &["add", "mine.txt"]);
    git_in(prepared.work(), &["commit", "-q", "-m", "work"]);
    assert_eq!(listed(), vec!["mine.txt".to_string()]);
}

#[test]
fn xs9_update_base_on_a_dirty_worktree_refuses_and_changes_nothing() {
    let prepared = prepared_workspace();
    let (carrier, bytes) = newer_base(&prepared, 0xc1, &[("a.txt", "one")]);
    std::fs::write(prepared.work().join("notes.txt"), "agent work in progress").unwrap();
    let refused = update(&prepared, &carrier, true, false);
    assert_eq!(refused.status, "refusedDirty", "{refused:?}");
    assert!(refused.dirty.iter().any(|line| line.contains("notes.txt")));
    // Even with the delta already on the machine, the worktree stays put.
    upload_pipelined(prepared.temp.path(), &carrier, &bytes);
    let refused = update(&prepared, &carrier, false, false);
    assert_eq!(refused.status, "refusedDirty");
    assert_eq!(head(&prepared), prepared.base);
    assert!(!prepared.work().join("a.txt").exists());
    assert_eq!(
        std::fs::read_to_string(prepared.work().join("notes.txt")).unwrap(),
        "agent work in progress"
    );
    assert!(
        staging_files(&prepared).is_empty(),
        "a final answer reclaims the upload"
    );
}

/// An edited managed copy is invisible to `git status` (it is
/// ignored) but still makes the workspace dirty.
#[test]
fn xs9_an_edited_managed_copy_makes_the_workspace_dirty() {
    let prepared = prepared_workspace();
    let (carrier, _) = newer_base(&prepared, 0xc1, &[("a.txt", "one")]);
    let record = prepared.place_managed(".claude/skills/x/SKILL.md", "as CodeSense wrote it");
    assert_eq!(
        git_in(prepared.work(), &["status", "--porcelain"]),
        "",
        "ignored"
    );
    assert_eq!(
        update_with(
            &prepared,
            &carrier,
            true,
            false,
            std::slice::from_ref(&record)
        )
        .unwrap()
        .status,
        "ready"
    );
    std::fs::write(
        prepared.work().join(".claude/skills/x/SKILL.md"),
        "an agent's edit",
    )
    .unwrap();
    let refused = update_with(&prepared, &carrier, true, false, &[record]).unwrap();
    assert_eq!(refused.status, "refusedDirty", "{refused:?}");
    assert_eq!(
        refused.dirty,
        vec!["M! .claude/skills/x/SKILL.md (managed copy edited)".to_string()]
    );
    assert!(refused.message.unwrap().contains("reverse_sync_worktree"));
}

#[test]
fn xs9_update_base_refuses_or_removes_managed_copies_the_new_base_tracks() {
    let prepared = prepared_workspace();
    let skill = ".claude/skills/review/SKILL.md";
    let (carrier, bytes) = newer_base(
        &prepared,
        0xc1,
        &[(skill, "tracked by the repo now"), ("src/lib.rs", "code")],
    );
    let record = prepared.place_managed(skill, "managed copy");
    let other = prepared.place_managed(".codesense/notes.md", "stays managed");
    let managed = vec![record, other];
    upload_pipelined(prepared.temp.path(), &carrier, &bytes);
    let refused = update_with(&prepared, &carrier, false, false, &managed).unwrap();
    assert_eq!(refused.status, "refusedManagedOverlap", "{refused:?}");
    assert_eq!(refused.managed_overlap, vec![skill.to_string()]);
    assert!(refused.message.unwrap().contains("removeManagedCopies"));
    assert_eq!(head(&prepared), prepared.base, "nothing moved");
    assert_eq!(
        std::fs::read_to_string(prepared.work().join(skill)).unwrap(),
        "managed copy"
    );
    // The refusal was final: the upload is reclaimed, so the retry re-sends.
    upload_pipelined(prepared.temp.path(), &carrier, &bytes);
    let done = update_with(&prepared, &carrier, false, true, &managed).unwrap();
    assert_eq!(done.status, "updated", "{done:?}");
    assert_eq!(done.managed_overlap, vec![skill.to_string()]);
    assert_eq!(head(&prepared), carrier.bundle.commit);
    assert_eq!(
        std::fs::read_to_string(prepared.work().join(skill)).unwrap(),
        "tracked by the repo now"
    );
    // The block drops exactly the copy the repo now tracks.
    let text = std::fs::read_to_string(prepared.exclude()).unwrap();
    let listed = workspace_block_paths(&text, &prepared.target.handle).unwrap();
    assert_eq!(listed, vec![".codesense/notes.md".to_string()]);
    assert_eq!(git_in(prepared.work(), &["status", "--porcelain"]), "");
}

/// A managed copy with no record to compare, and an ignored
/// file CodeSense never placed, are never removed or overwritten.
#[test]
fn xs9_update_base_never_overwrites_unrecorded_or_unmanaged_ignored_files() {
    let prepared = prepared_workspace();
    let (carrier, bytes) = newer_base(
        &prepared,
        0xc1,
        &[("managed.md", "repo"), ("build/out.txt", "repo")],
    );
    prepared.place_managed("managed.md", "no record of this one");
    std::fs::create_dir_all(prepared.work().join("build")).unwrap();
    std::fs::write(prepared.work().join("build/out.txt"), "the user's").unwrap();
    // The user's own ignore rule, outside any workspace block.
    let mut text = std::fs::read_to_string(prepared.exclude()).unwrap();
    text.insert_str(0, "build/\n");
    std::fs::write(prepared.exclude(), text).unwrap();
    upload_pipelined(prepared.temp.path(), &carrier, &bytes);
    let refused = update_with(&prepared, &carrier, false, true, &[]).unwrap();
    assert_eq!(refused.status, "refusedManagedOverlap", "{refused:?}");
    assert_eq!(refused.modified_managed, vec!["managed.md".to_string()]);
    assert_eq!(refused.unmanaged_overlap, vec!["build/out.txt".to_string()]);
    assert_eq!(head(&prepared), prepared.base);
    assert_eq!(
        std::fs::read_to_string(prepared.work().join("build/out.txt")).unwrap(),
        "the user's"
    );
}

/// Removal and the merge are one step; a merge that fails puts
/// the removed copies back (bytes and mode) and leaves HEAD and the block.
#[test]
fn xs9_a_failed_merge_restores_the_removed_managed_copies() {
    let prepared = prepared_workspace();
    let skill = ".claude/skills/review/SKILL.md";
    let (carrier, bytes) = newer_base(
        &prepared,
        0xc1,
        &[(skill, "tracked by the repo now"), ("src/lib.rs", "code")],
    );
    let record = prepared.place_managed(skill, "managed copy");
    let block_before = std::fs::read_to_string(prepared.exclude()).unwrap();
    upload_pipelined(prepared.temp.path(), &carrier, &bytes);
    // Just before the merge, an untracked file appears where the new base
    // adds one: `git merge --ff-only` refuses.
    let work = prepared.work().to_path_buf();
    arm_test_hold("before-merge", move || {
        std::fs::create_dir_all(work.join("src")).unwrap();
        std::fs::write(work.join("src/lib.rs"), "in the way").unwrap();
    });
    let error = update_with(&prepared, &carrier, false, true, &[record]).unwrap_err();
    assert!(error.contains("left as it was"), "{error}");
    assert_eq!(head(&prepared), prepared.base);
    let restored = prepared.work().join(skill);
    assert_eq!(std::fs::read_to_string(&restored).unwrap(), "managed copy");
    assert_eq!(
        std::fs::metadata(&restored).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert_eq!(
        std::fs::read_to_string(prepared.exclude()).unwrap(),
        block_before
    );
}

/// One sealed upload per worktree at a time. A holder
/// in use (its staging lock held: verifying, preparing or updating) keeps the
/// claim; once it is idle, a later upload reclaims it.
#[test]
fn xs9_only_one_upload_of_a_workspace_can_be_sealed() {
    let prepared = prepared_workspace();
    let (first, first_bytes) = newer_base(&prepared, 0xc1, &[("a.txt", "one")]);
    let mut second = first.clone();
    second.request_id = "00000000-0000-4000-8000-0000000000c2".into();
    stage_pipelined(prepared.temp.path(), &first, &first_bytes);
    stage_pipelined(prepared.temp.path(), &second, &first_bytes);
    seal_bundle(prepared.temp.path(), 41, first.clone(), || true).unwrap();
    let first_key = receipt_key(&first.origin_key, &first.request_id);
    let in_use = locks::staging(
        &super::super::bundle_lock(prepared.temp.path(), &first_key),
        &|| true,
    )
    .unwrap();
    let refused = seal_bundle(prepared.temp.path(), 41, second.clone(), || true).unwrap_err();
    assert!(refused.contains("another upload"), "{refused}");
    drop(in_use);
    seal_bundle(prepared.temp.path(), 41, second.clone(), || true).unwrap();
    assert!(
        !carrier_receipt_exists(&prepared, &first),
        "the idle holder was reclaimed"
    );
    assert_eq!(update(&prepared, &second, false, false).status, "updated");
}

#[test]
fn xs9_update_base_refuses_a_carrier_for_another_workspace() {
    let prepared = prepared_workspace();
    let (mut carrier, _) = newer_base(&prepared, 0xc1, &[("a.txt", "one")]);
    carrier.repository_key = "github:someone/else".into();
    assert!(update_with(&prepared, &carrier, true, false, &[]).is_err());
    let (mut carrier, _) = newer_base(&prepared, 0xc2, &[]);
    carrier.branch = "someone-else".into();
    assert_ne!(update(&prepared, &carrier, true, false).status, "ready");
}

/// A detached HEAD is a refusal, not an error.
#[test]
fn xs9_a_detached_workspace_is_refused() {
    let prepared = prepared_workspace();
    let (carrier, _) = newer_base(&prepared, 0xc1, &[("a.txt", "one")]);
    git_in(prepared.work(), &["checkout", "--detach"]);
    let refused = update(&prepared, &carrier, true, false);
    assert_eq!(refused.status, "refused");
    assert!(refused.message.unwrap().contains("detached"));
}

/// The block is read and written by #1975's own `files.rs` code: anchored,
/// escaped paths between per-workspace markers; other lines are kept; a line
/// that writer never writes makes the block uncertain, so it is refused.
#[test]
fn xs9_the_workspace_block_matches_the_per_workspace_format() {
    let text = "# user\nbuild/\n";
    let written = with_workspace_block(
        text,
        "remote-workspace-ab",
        &["b.md".into(), "a [1]*.md".into(), "trail ".into()],
    )
    .unwrap();
    assert_eq!(
        written,
        "# user\nbuild/\n# >>> codesense managed workspace remote-workspace-ab\n/a \\[1\\]\\*.md\n/b.md\n/trail\\ \n# <<< codesense managed workspace remote-workspace-ab\n"
    );
    assert_eq!(
        workspace_block_paths(&written, "remote-workspace-ab").unwrap(),
        vec!["a [1]*.md", "b.md", "trail "]
    );
    assert!(workspace_block_paths(&written, "other").unwrap().is_empty());
    let unanchored =
        "# >>> codesense managed workspace w\nnotes.md\n# <<< codesense managed workspace w\n";
    assert!(workspace_block_paths(unanchored, "w").is_err());
    // An unescaped glob is read as the literal name it spells, never as a
    // pattern: it names no other file.
    let globbed =
        "# >>> codesense managed workspace w\n/*.md\n# <<< codesense managed workspace w\n";
    assert_eq!(workspace_block_paths(globbed, "w").unwrap(), vec!["*.md"]);
    assert_eq!(
        with_workspace_block(&written, "remote-workspace-ab", &[]).unwrap(),
        text
    );
}

fn carrier_receipt_exists(prepared: &Prepared, carrier: &PrepareWorkspace) -> bool {
    prepared
        .temp
        .path()
        .join("remote-workspaces")
        .join(receipt_key(&carrier.origin_key, &carrier.request_id))
        .exists()
}

/// Re-review A: an update sealed, then lost (unknown); the base moves on and a
/// new update seals: the abandoned holder is reclaimed, never a wedge.
#[test]
fn xs9_an_abandoned_sealed_update_never_wedges_the_next_one() {
    let prepared = prepared_workspace();
    let (abandoned, abandoned_bytes) = newer_base(&prepared, 0xc1, &[("a.txt", "one")]);
    upload_pipelined(prepared.temp.path(), &abandoned, &abandoned_bytes);
    assert!(!staging_files(&prepared).is_empty());
    let (next, next_bytes) = newer_base(&prepared, 0xc2, &[("b.txt", "two")]);
    upload_pipelined(prepared.temp.path(), &next, &next_bytes);
    let done = update(&prepared, &next, false, false);
    assert_eq!(done.status, "updated", "{done:?}");
    assert_eq!(head(&prepared), next.bundle.commit);
    assert!(
        staging_files(&prepared).is_empty(),
        "the abandoned staging is gone too: {:?}",
        staging_files(&prepared)
    );
    assert!(!carrier_receipt_exists(&prepared, &abandoned));
}

/// Re-review A: the desktop names an abandoned update id and the machine
/// reclaims what it staged, even before any new seal.
#[test]
fn xs9_discarding_an_abandoned_update_reclaims_its_staging() {
    let prepared = prepared_workspace();
    let (abandoned, bytes) = newer_base(&prepared, 0xc1, &[("a.txt", "one")]);
    upload_pipelined(prepared.temp.path(), &abandoned, &bytes);
    let mut next = abandoned.clone();
    next.request_id = "00000000-0000-4000-8000-0000000000c2".into();
    let ready = update_base(
        prepared.temp.path(),
        41,
        UpdateWorkspaceBase {
            target: prepared.target.clone(),
            upload: next,
            precheck: true,
            remove_managed_copies: false,
            managed: Vec::new(),
            discard_uploads: vec![abandoned.request_id.clone()],
        },
        || true,
    )
    .unwrap();
    assert_eq!(ready.status, "ready");
    assert!(
        staging_files(&prepared).is_empty(),
        "{:?}",
        staging_files(&prepared)
    );
    assert!(!carrier_receipt_exists(&prepared, &abandoned));
    // The prepared workspace's own receipt is untouched.
    assert_eq!(
        reconcile(prepared.temp.path(), 41, prepared.request.clone(), || true)
            .unwrap()
            .unwrap()
            .status,
        "alreadyPrepared"
    );
}

/// Re-review B: after an update replaced managed copies with tracked files,
/// the desktop's old records of those paths never make the workspace dirty.
#[test]
fn xs9_records_of_paths_the_repo_now_tracks_never_make_it_dirty() {
    let prepared = prepared_workspace();
    let skill = ".claude/skills/review/SKILL.md";
    let (carrier, bytes) = newer_base(&prepared, 0xc1, &[(skill, "tracked by the repo now")]);
    let record = prepared.place_managed(skill, "managed copy");
    upload_pipelined(prepared.temp.path(), &carrier, &bytes);
    let stale = vec![record];
    let first = update_with(&prepared, &carrier, false, true, &stale).unwrap();
    assert_eq!(first.status, "updated", "{first:?}");
    // The record still names the old copy's hash; the file is the repo's now.
    let (later, later_bytes) = newer_base(&prepared, 0xc2, &[("later.txt", "more")]);
    let ready = update_with(&prepared, &later, true, false, &stale).unwrap();
    assert_eq!(ready.status, "ready", "{ready:?}");
    upload_pipelined(prepared.temp.path(), &later, &later_bytes);
    let done = update_with(&prepared, &later, false, false, &stale).unwrap();
    assert_eq!(done.status, "updated", "{done:?}");
}

/// Re-review: an update that moved the worktree but crashed before recording
/// it is recorded on the retry's upToDate answer.
#[test]
fn xs9_an_up_to_date_answer_records_the_base() {
    let prepared = prepared_workspace();
    let (carrier, _) = newer_base(&prepared, 0xc1, &[("a.txt", "one")]);
    let source = prepared.temp.path().join("source-offline");
    // The fast-forward happened; the receipt was never saved.
    git_in(
        prepared.work(),
        &[
            "fetch",
            "-q",
            source.to_str().unwrap(),
            "codesense-prepared",
        ],
    );
    git_in(
        prepared.work(),
        &["merge", "-q", "--ff-only", &carrier.bundle.commit],
    );
    let mut retry = carrier.clone();
    retry.bundle.size = 0;
    retry.bundle.digest = digest(&[]);
    assert_eq!(update(&prepared, &retry, false, false).status, "upToDate");
    assert_eq!(
        reconcile(prepared.temp.path(), 41, prepared.request.clone(), || true)
            .unwrap()
            .unwrap()
            .resolved_commit
            .as_deref(),
        Some(carrier.bundle.commit.as_str())
    );
}
