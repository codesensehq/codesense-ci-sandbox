// SPDX-License-Identifier: MIT
//! The team's context repo as git sees it: the left nav's status button, its conflict and
//! failure cards, and the History view.
//!
//! CodeSense does not sync the context repo itself; agents do the git. So everything here
//! is read from the repo on disk (branch, upstream, ahead/behind, an operation in progress,
//! conflicted index entries, the last fetch) plus what the syncing agent last reported
//! through `report_context_sync`. The one write is settling a conflicted file from the card
//! (`resolve_context_conflict`): it writes the chosen side into the working tree and stages
//! it, and leaves finishing the merge or rebase to the agent doing the sync.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::git_sync::{
    build_ref_label_map, commit_changed_paths, new_sorted_revwalk, ref_kind, RefKind, RefLabel,
};
use crate::shared::git_util::open_repo;

/// An operation that has not been touched for this long has stopped, not run.
const STALLED_AFTER: Duration = Duration::from_secs(10 * 60);
/// How far back a conflicted file's last writer is looked for.
const WRITER_SEARCH_LIMIT: usize = 300;
/// How many held-back paths a conflict lists.
const HELD_BACK_LIMIT: usize = 20;
/// How deep the main line is walked to tell main commits from branch commits.
const MAIN_LINE_LIMIT: usize = 5_000;
/// How deep each branch is walked back to the main line.
const BRANCH_WALK_LIMIT: usize = 500;
/// How many commits one History page may scan when a path filter skips most of them.
const FILTER_SCAN_LIMIT: usize = 3_000;
/// The largest History page.
const MAX_PAGE: usize = 500;

/// Emitted when the syncing agent reports, so an open nav re-reads the status at once.
pub const CONTEXT_REPO_STATUS_CHANGED: &str = "context-repo-status-changed";

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/// What is happening to the context repo's sync right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ContextSyncState {
    Ok,
    Syncing,
    Failed,
    Signin,
    Retrying,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSync {
    pub state: ContextSyncState,
    /// Unix ms of the last time this clone heard from GitHub (a fetch, or a reported sync).
    pub last_synced_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<String>,
    /// The clone has no remote: nothing can send its changes anywhere.
    pub no_sender: bool,
}

/// One side of a conflicted file: who last changed it on that side, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictSide {
    /// An agent's name, or the commit author's.
    pub who: Option<String>,
    /// That commit's subject.
    pub subject: Option<String>,
    /// Unix ms.
    pub at: Option<i64>,
    /// Absent on this side: it deleted the file.
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextConflict {
    /// Repo-relative path.
    pub path: String,
    /// This Mac's side.
    pub mine: ConflictSide,
    /// GitHub's side.
    pub theirs: ConflictSide,
    /// The agent that last wrote the file on this Mac's side, when one did.
    pub last_writer: Option<String>,
    /// Changes from GitHub staged by the same merge, waiting until it is settled.
    pub held_back: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextRepoStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_commit_at: Option<i64>,
    /// The checked-out branch (during a rebase, the branch being rebuilt); `None` when HEAD
    /// is detached.
    pub branch: Option<String>,
    pub head_sha: String,
    pub has_upstream: bool,
    /// The upstream's short name (`origin/main`).
    pub upstream: Option<String>,
    /// SW-VE-16: the forge provider id for `origin` (`github`/`gitlab`/`bitbucket`) when the
    /// host is recognized via `provider_for_host`. `None` when the repo has no `origin`, the
    /// remote URL does not parse, or the host is not a supported forge. Lets the file-viewer
    /// toolbar render `vs GitHub (origin/main)` without hardcoding GitHub.
    pub remote_provider: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub sync: ContextSync,
    pub conflicts: Vec<ContextConflict>,
}

/// What the syncing agent last said about the sync.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextSyncReport {
    /// syncing | ok | failed | signin | retrying.
    pub state: ContextSyncState,
    /// One plain sentence the human reads.
    #[serde(default)]
    pub reason: Option<String>,
    /// Retrying: this attempt.
    #[serde(default)]
    pub attempt: Option<u32>,
    /// Retrying: how many attempts in all.
    #[serde(default)]
    pub max_attempts: Option<u32>,
    /// Syncing: what and how far ("Pulling 3 of 7").
    #[serde(default)]
    pub progress: Option<String>,
}

struct StoredReport {
    report: ContextSyncReport,
    at_ms: i64,
}

static REPORTS: LazyLock<Mutex<HashMap<PathBuf, StoredReport>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn report_key(repo_path: &Path) -> PathBuf {
    std::fs::canonicalize(repo_path).unwrap_or_else(|_| repo_path.to_path_buf())
}

/// Remember what the syncing agent says about `repo_path`'s sync.
pub fn record_sync_report(repo_path: &Path, report: ContextSyncReport, at_ms: i64) {
    let mut reports = REPORTS.lock().unwrap_or_else(|e| e.into_inner());
    reports.insert(report_key(repo_path), StoredReport { report, at_ms });
}

fn stored_report(repo_path: &Path) -> Option<(ContextSyncReport, i64)> {
    let reports = REPORTS.lock().unwrap_or_else(|e| e.into_inner());
    reports
        .get(&report_key(repo_path))
        .map(|s| (s.report.clone(), s.at_ms))
}

fn now_ms() -> i64 {
    system_ms(SystemTime::now()).unwrap_or(0)
}

fn system_ms(t: SystemTime) -> Option<i64> {
    t.duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as i64)
}

fn mtime_ms(path: &Path) -> Option<i64> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(system_ms)
}

/// The checked-out branch's upstream: its configured one, else `origin/<branch>`.
fn upstream_of(repo: &git2::Repository, branch: &str) -> Option<(String, git2::Oid)> {
    if let Ok(local) = repo.find_branch(branch, git2::BranchType::Local) {
        if let Ok(up) = local.upstream() {
            let name = up.name().ok().flatten().map(str::to_string);
            if let (Some(name), Some(oid)) = (name, up.get().target()) {
                return Some((name, oid));
            }
        }
    }
    let name = format!("origin/{branch}");
    let oid = repo
        .find_reference(&format!("refs/remotes/{name}"))
        .ok()?
        .target()?;
    Some((name, oid))
}

/// The last time this clone heard from GitHub: the newer of the last fetch and the last
/// move of the upstream ref.
fn last_heard_ms(repo: &git2::Repository, upstream: Option<&str>) -> Option<i64> {
    let fetch = mtime_ms(&repo.path().join("FETCH_HEAD"));
    let reflog = upstream.and_then(|name| {
        let log = repo.reflog(&format!("refs/remotes/{name}")).ok()?;
        let seconds = log.get(0).map(|entry| entry.committer().when().seconds());
        seconds.map(|s| s * 1000)
    });
    fetch.max(reflog)
}

/// A git operation under way in the repo, from the files git keeps while it runs.
struct Operation {
    /// "Rebasing 3 of 7", "Merging".
    label: String,
    /// When the operation last moved.
    touched_ms: Option<i64>,
}

fn read_num(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn operation_in_progress(repo: &git2::Repository) -> Option<Operation> {
    let git_dir = repo.path();
    for (dir, step, total) in [
        ("rebase-merge", "msgnum", "end"),
        ("rebase-apply", "next", "last"),
    ] {
        let d = git_dir.join(dir);
        if d.is_dir() {
            let label = match (read_num(&d.join(step)), read_num(&d.join(total))) {
                (Some(n), Some(m)) => format!("Rebasing {n} of {m}"),
                _ => "Rebasing".to_string(),
            };
            let touched_ms = [
                mtime_ms(&d.join(step)),
                mtime_ms(&d),
                mtime_ms(&git_dir.join("index")),
            ]
            .into_iter()
            .flatten()
            .max();
            return Some(Operation { label, touched_ms });
        }
    }
    for (file, label) in [
        ("MERGE_HEAD", "Merging"),
        ("CHERRY_PICK_HEAD", "Picking a commit"),
        ("REVERT_HEAD", "Reverting a commit"),
    ] {
        let p = git_dir.join(file);
        if p.exists() {
            let touched_ms = [mtime_ms(&p), mtime_ms(&git_dir.join("index"))]
                .into_iter()
                .flatten()
                .max();
            return Some(Operation {
                label: label.to_string(),
                touched_ms,
            });
        }
    }
    None
}

/// Which index stage holds this Mac's side of a conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MineStage {
    /// Stage 2: HEAD is this Mac's branch (a merge, a squash merge, a cherry-pick, a revert).
    Ours,
    /// Stage 3: HEAD is the upstream being built on and this Mac's change is the one
    /// replayed (a rebase, `git am`, a stash apply).
    Theirs,
    /// Nothing on disk says which (a `checkout -m`, say): reads fall back to `Theirs`,
    /// and the card does not settle it.
    Unknown,
}

fn mine_stage(repo: &git2::Repository) -> MineStage {
    use git2::RepositoryState as S;
    match repo.state() {
        S::Merge | S::CherryPick | S::CherryPickSequence | S::Revert | S::RevertSequence => {
            MineStage::Ours
        }
        S::Rebase
        | S::RebaseInteractive
        | S::RebaseMerge
        | S::ApplyMailbox
        | S::ApplyMailboxOrRebase => MineStage::Theirs,
        S::Clean => {
            if repo.path().join("SQUASH_MSG").exists() {
                MineStage::Ours
            } else if repo.find_reference("refs/stash").is_ok() {
                MineStage::Theirs
            } else {
                MineStage::Unknown
            }
        }
        S::Bisect => MineStage::Unknown,
    }
}

fn mine_is_ours(repo: &git2::Repository) -> bool {
    mine_stage(repo) == MineStage::Ours
}

/// The commit standing for the side that is not HEAD.
fn incoming_commit(repo: &git2::Repository) -> Option<git2::Oid> {
    for name in [
        "MERGE_HEAD",
        "REBASE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "refs/stash",
    ] {
        if let Ok(r) = repo.find_reference(name) {
            if let Ok(c) = r.peel_to_commit() {
                return Some(c.id());
            }
        }
        let p = repo.path().join(name);
        if let Ok(text) = std::fs::read_to_string(&p) {
            if let Some(oid) = text
                .lines()
                .next()
                .and_then(|l| git2::Oid::from_str(l.trim()).ok())
            {
                return Some(oid);
            }
        }
    }
    None
}

fn blob_at(tree: &git2::Tree, path: &str) -> Option<git2::Oid> {
    tree.get_path(Path::new(path)).ok().map(|e| e.id())
}

/// The newest commit reachable from `tip` (first parents) that changed `path`.
fn last_change<'r>(
    repo: &'r git2::Repository,
    tip: git2::Oid,
    path: &str,
) -> Option<git2::Commit<'r>> {
    let mut commit = repo.find_commit(tip).ok()?;
    for _ in 0..WRITER_SEARCH_LIMIT {
        let here = commit.tree().ok().and_then(|t| blob_at(&t, path));
        let parent = commit.parent(0).ok();
        let before = parent
            .as_ref()
            .and_then(|p| p.tree().ok())
            .and_then(|t| blob_at(&t, path));
        if here != before {
            return Some(commit);
        }
        commit = parent?;
    }
    None
}

fn conflict_side(
    repo: &git2::Repository,
    tip: Option<git2::Oid>,
    path: &str,
    present: bool,
) -> (ConflictSide, Option<String>) {
    let commit = tip.and_then(|t| last_change(repo, t, path));
    let identity = commit.as_ref().map(commit_identity);
    let agent = identity.as_ref().and_then(|i| i.agent.clone());
    let side = ConflictSide {
        who: identity.map(|i| i.agent.map(|a| a.name).unwrap_or(i.author)),
        subject: commit
            .as_ref()
            .map(|c| clean_subject(c).0)
            .filter(|s| !s.is_empty()),
        at: commit.as_ref().map(|c| c.time().seconds() * 1000),
        deleted: !present,
    };
    (side, agent.map(|a| a.name))
}

fn index_path(entry: &git2::IndexEntry) -> String {
    String::from_utf8_lossy(&entry.path).into_owned()
}

fn read_conflicts(repo: &git2::Repository) -> Result<Vec<ContextConflict>, String> {
    let index = repo.index().map_err(|e| format!("index: {e}"))?;
    if !index.has_conflicts() {
        return Ok(Vec::new());
    }
    let mine_ours = mine_is_ours(repo);
    let head = repo.head().ok().and_then(|h| h.target());
    let incoming = incoming_commit(repo);
    let (mine_tip, their_tip) = if mine_ours {
        (head, incoming)
    } else {
        (incoming, head)
    };

    let mut conflicted: Vec<(String, bool, bool)> = Vec::new();
    for c in index.conflicts().map_err(|e| format!("conflicts: {e}"))? {
        let c = c.map_err(|e| format!("conflict: {e}"))?;
        let Some(path) = c
            .our
            .as_ref()
            .or(c.their.as_ref())
            .or(c.ancestor.as_ref())
            .map(index_path)
        else {
            continue;
        };
        let (mine_present, theirs_present) = if mine_ours {
            (c.our.is_some(), c.their.is_some())
        } else {
            (c.their.is_some(), c.our.is_some())
        };
        conflicted.push((path, mine_present, theirs_present));
    }

    let held_back = if repo.state() == git2::RepositoryState::Merge {
        held_back_paths(repo, &conflicted)?
    } else {
        Vec::new()
    };

    Ok(conflicted
        .into_iter()
        .map(|(path, mine_present, theirs_present)| {
            let (mine, last_writer) = conflict_side(repo, mine_tip, &path, mine_present);
            let (theirs, _) = conflict_side(repo, their_tip, &path, theirs_present);
            ContextConflict {
                path,
                mine,
                theirs,
                last_writer,
                held_back: held_back.clone(),
            }
        })
        .collect())
}

/// In a merge, the changes from the other side git staged without trouble: they land when
/// the merge is committed, so until the conflict is settled they are held back.
fn held_back_paths(
    repo: &git2::Repository,
    conflicted: &[(String, bool, bool)],
) -> Result<Vec<String>, String> {
    let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
    let index = repo.index().map_err(|e| format!("index: {e}"))?;
    let diff = repo
        .diff_tree_to_index(head_tree.as_ref(), Some(&index), None)
        .map_err(|e| format!("staged diff: {e}"))?;
    let skip: HashSet<&str> = conflicted.iter().map(|(p, _, _)| p.as_str()).collect();
    let mut out: Vec<String> = diff
        .deltas()
        .filter(|d| d.status() != git2::Delta::Conflicted)
        .filter_map(|d| {
            d.new_file()
                .path()
                .or_else(|| d.old_file().path())
                .map(|p| p.to_string_lossy().into_owned())
        })
        .filter(|p| !skip.contains(p.as_str()))
        .collect();
    out.sort();
    out.dedup();
    out.truncate(HELD_BACK_LIMIT);
    Ok(out)
}

/// The sync state from what git shows, then what the syncing agent last reported. An
/// operation git is running or has stopped partway wins: it is what is on disk now.
fn sync_state(
    repo: &git2::Repository,
    repo_path: &Path,
    upstream: Option<&str>,
    has_conflicts: bool,
    now: i64,
) -> ContextSync {
    let has_remote = repo.remotes().map(|r| !r.is_empty()).unwrap_or(false);
    let heard = last_heard_ms(repo, upstream);
    let report = stored_report(repo_path);
    let last_synced_at = match &report {
        Some((r, at)) if r.state == ContextSyncState::Ok => heard.max(Some(*at)),
        _ => heard,
    };
    let mut sync = ContextSync {
        state: ContextSyncState::Ok,
        last_synced_at,
        reason: None,
        attempt: None,
        max_attempts: None,
        progress: None,
        no_sender: !has_remote,
    };

    let locked = repo.path().join("index.lock").exists();
    if let Some(op) = operation_in_progress(repo) {
        if has_conflicts {
            // The conflicts are the story; the card settles them.
            return sync;
        }
        let idle = op
            .touched_ms
            .map(|t| now.saturating_sub(t) > STALLED_AFTER.as_millis() as i64)
            .unwrap_or(false);
        if idle && !locked {
            sync.state = ContextSyncState::Failed;
            sync.reason = Some(format!("{} stopped partway", op.label));
        } else {
            sync.state = ContextSyncState::Syncing;
            sync.progress = Some(op.label);
        }
        return sync;
    }
    if locked {
        sync.state = ContextSyncState::Syncing;
        return sync;
    }

    if let Some((r, at)) = report {
        // A failure, sign-out or retry stands until the clone hears from GitHub again; a
        // reported sync in flight goes stale on its own.
        let superseded = heard.is_some_and(|h| h > at);
        let live = match r.state {
            ContextSyncState::Ok => false,
            ContextSyncState::Syncing => now.saturating_sub(at) < STALLED_AFTER.as_millis() as i64,
            ContextSyncState::Failed | ContextSyncState::Signin | ContextSyncState::Retrying => {
                !superseded
            }
        };
        if live {
            sync.state = r.state;
            sync.reason = r.reason;
            sync.attempt = r.attempt;
            sync.max_attempts = r.max_attempts;
            sync.progress = r.progress;
        }
    }
    sync
}

/// The branch a rebase in progress is rebuilding, from git's own record of it.
fn rebasing_branch(repo: &git2::Repository) -> Option<String> {
    ["rebase-merge", "rebase-apply"]
        .into_iter()
        .find_map(|dir| {
            let text = std::fs::read_to_string(repo.path().join(dir).join("head-name")).ok()?;
            text.trim()
                .strip_prefix("refs/heads/")
                .filter(|b| !b.is_empty())
                .map(str::to_string)
        })
}

pub fn context_repo_status_impl(repo_path: &Path) -> Result<ContextRepoStatus, String> {
    context_repo_status_impl_for(repo_path, &Default::default())
}

pub(crate) fn context_repo_status_impl_for(
    repo_path: &Path,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<ContextRepoStatus, String> {
    let repo = open_repo(repo_path)?;
    let head = repo.head().ok();
    let head_oid = head.as_ref().and_then(|h| h.target());
    let head_sha = head_oid
        .map(|o| o.to_string().chars().take(7).collect())
        .unwrap_or_default();
    let branch = head
        .as_ref()
        .filter(|h| h.is_branch())
        .and_then(|h| Result::ok(h.shorthand()))
        .map(str::to_string)
        // An unborn branch has no HEAD commit yet but is still on a branch.
        .or_else(|| {
            if head.is_none() {
                repo.find_reference("HEAD")
                    .ok()
                    .and_then(|r| {
                        Option::flatten(Result::ok(r.symbolic_target())).map(str::to_string)
                    })
                    .and_then(|t| t.strip_prefix("refs/heads/").map(str::to_string))
            } else {
                None
            }
        });

    // A rebase detaches HEAD while it runs; the branch it is rebuilding is still the one
    // this clone is on, and until it finishes that branch's tip is what GitHub lacks.
    let rebasing = if branch.is_none() {
        rebasing_branch(&repo)
    } else {
        None
    };
    let local_tip = match &rebasing {
        Some(b) => repo
            .find_reference(&format!("refs/heads/{b}"))
            .ok()
            .and_then(|r| r.target())
            .or(head_oid),
        None => head_oid,
    };
    let branch = branch.or(rebasing);

    let upstream = branch.as_deref().and_then(|b| upstream_of(&repo, b));
    let (ahead, behind) = match (local_tip, &upstream) {
        (Some(local), Some((_, remote))) => repo
            .graph_ahead_behind(local, *remote)
            .map(|(a, b)| (a as u32, b as u32))
            .map_err(|e| format!("ahead/behind: {e}"))?,
        _ => (0, 0),
    };
    let mut conflicts = read_conflicts(&repo)?;
    conflicts.retain(|conflict| scope.allows(&repo_path.join(&conflict.path)));
    for conflict in &mut conflicts {
        conflict
            .held_back
            .retain(|path| scope.allows(&repo_path.join(path)));
    }
    let upstream_name = upstream.as_ref().map(|(n, _)| n.clone());
    let sync = sync_state(
        &repo,
        repo_path,
        upstream_name.as_deref(),
        !conflicts.is_empty(),
        now_ms(),
    );
    let remote_provider = origin_provider(&repo);
    Ok(ContextRepoStatus {
        last_commit_at: if crate::context_repo::scratch_space::is_private_repo(&repo) {
            head.as_ref()
                .and_then(|h| h.peel_to_commit().ok())
                .map(|c| c.time().seconds() * 1000)
        } else {
            None
        },
        branch,
        head_sha,
        has_upstream: upstream.is_some(),
        upstream: upstream_name,
        remote_provider,
        ahead,
        behind,
        sync,
        conflicts,
    })
}

/// SW-VE-16: the forge provider id for a repo's `origin` remote, or `None` when there's no
/// origin, the URL doesn't parse, or the host isn't a supported forge. Parses the URL through
/// the same `normalize_source_repo_url` pipeline the attach + sync paths use so the forge
/// decision is consistent across surfaces (the one surviving caller contract after SW-CR-2).
///
/// **Scoped to `origin` on purpose.** A context repo may configure multiple remotes (push
/// mirror, personal fork, read-only replica); the toolbar's `vs <forge> (origin/<branch>)`
/// line is only ever rendered against `origin`'s baseline, so looking anywhere else would
/// surface a forge the user doesn't actually push to. If a future surface needs the provider
/// of a non-origin remote, add a sibling helper rather than widening this one.
fn origin_provider(repo: &git2::Repository) -> Option<String> {
    let url = Result::ok(repo.find_remote("origin").ok()?.url())?.to_string();
    let normalized = crate::context_repo::normalize_source_repo_url(&url).ok()?;
    let host = normalized.normalized_url.split('/').next()?;
    crate::commands::forge::provider_for_host(host).map(str::to_string)
}

#[tauri::command]
pub async fn context_repo_status(
    app: tauri::AppHandle,
    repo_path: String,
    worktree: Option<String>,
) -> Result<serde_json::Value, String> {
    if let Some(id) = worktree {
        let row = super::git_sync::context_worktree::resolve(&app, &id).await?;
        return super::git_sync::context_worktree::status(&app, row).await;
    }
    let path = PathBuf::from(repo_path);
    tauri::async_runtime::spawn_blocking(move || {
        context_repo_status_impl(&path)
            .and_then(|s| serde_json::to_value(s).map_err(|e| e.to_string()))
    })
    .await
    .map_err(|e| format!("context_repo_status task failed: {e}"))?
}

// ---------------------------------------------------------------------------
// Settling a conflict from the card
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictResolution {
    UseMine,
    UseTheirs,
    KeepBoth,
}

/// Where Keep both puts GitHub's copy: beside the file, named for where it came from.
fn theirs_copy_path(path: &str) -> String {
    let p = Path::new(path);
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = match p.extension() {
        Some(ext) => format!("{stem} (GitHub).{}", ext.to_string_lossy()),
        None => format!("{stem} (GitHub)"),
    };
    match p.parent().filter(|d| !d.as_os_str().is_empty()) {
        Some(dir) => format!("{}/{name}", dir.to_string_lossy()),
        None => name,
    }
}

fn safe_rel(path: &str) -> Result<&Path, String> {
    let p = Path::new(path);
    if p.is_absolute()
        || p.components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!("not a path inside the repo: {path}"));
    }
    Ok(p)
}

/// Where `rel` lands in the working tree, checked: its folder must resolve inside the
/// repo (no folder on the way may be a symlink leading out), and the entry itself is never
/// followed, so an existing symlink there is replaced, not written through.
fn checked_dest(workdir: &Path, rel: &str) -> Result<PathBuf, String> {
    let dest = workdir.join(safe_rel(rel)?);
    let root = workdir
        .canonicalize()
        .map_err(|e| format!("read {}: {e}", workdir.display()))?;
    let dir = dest.parent().unwrap_or(workdir);
    // Walk down from the repo root, refusing any existing folder that is a symlink or not
    // a folder, so nothing is created or written outside the repo.
    let mut at = workdir.to_path_buf();
    for part in dir
        .strip_prefix(workdir)
        .unwrap_or(Path::new(""))
        .components()
    {
        at.push(part);
        match std::fs::symlink_metadata(&at) {
            Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
                return Err(format!(
                    "{rel}: {} is not a folder inside the repo",
                    at.display()
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("read {}: {e}", at.display())),
        }
    }
    if let Ok(real) = dir.canonicalize() {
        if !real.starts_with(&root) {
            return Err(format!("{rel} is not a path inside the repo"));
        }
    }
    Ok(dest)
}

/// Remove whatever is at `dest` without following it: a file or a symlink. A folder there
/// is refused.
fn clear_dest(dest: &Path, rel: &str) -> Result<(), String> {
    match std::fs::symlink_metadata(dest) {
        Ok(m) if m.is_dir() => Err(format!("{rel} is a folder")),
        Ok(_) => std::fs::remove_file(dest).map_err(|e| format!("remove {rel}: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("read {rel}: {e}")),
    }
}

const SYMLINK_MODE: u32 = 0o120000;

fn write_blob(
    repo: &git2::Repository,
    workdir: &Path,
    rel: &str,
    entry: &git2::IndexEntry,
) -> Result<(), String> {
    let blob = repo
        .find_blob(entry.id)
        .map_err(|e| format!("read {rel}: {e}"))?;
    let dest = checked_dest(workdir, rel)?;
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    clear_dest(&dest, rel)?;
    if entry.mode == SYMLINK_MODE {
        // A symlink side stays a symlink: its blob is the link's target.
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let target = std::ffi::OsStr::from_bytes(blob.content());
            return std::os::unix::fs::symlink(target, &dest)
                .map_err(|e| format!("link {rel}: {e}"));
        }
        #[cfg(not(unix))]
        {
            return Err(format!(
                "{rel} is a symlink on one side; ask an agent to settle it"
            ));
        }
    }
    std::fs::write(&dest, blob.content()).map_err(|e| format!("write {rel}: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if entry.mode & 0o111 != 0 {
            0o755
        } else {
            0o644
        };
        let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(mode));
    }
    Ok(())
}

/// Write `side` at `rel` and stage it; a side that deleted the file removes it.
fn keep_side(
    repo: &git2::Repository,
    workdir: &Path,
    index: &mut git2::Index,
    side: &Option<git2::IndexEntry>,
    rel: &str,
) -> Result<(), String> {
    match side {
        Some(entry) => {
            write_blob(repo, workdir, rel, entry)?;
            index
                .add_path(Path::new(rel))
                .map_err(|e| format!("stage {rel}: {e}"))
        }
        None => {
            let dest = checked_dest(workdir, rel)?;
            clear_dest(&dest, rel)?;
            let _ = index.remove_path(Path::new(rel));
            Ok(())
        }
    }
}

/// Settle one conflicted file: write the chosen side into the working tree and stage it.
/// The merge or rebase stays open for the agent doing the sync to finish.
pub fn resolve_context_conflict_impl(
    repo_path: &Path,
    path: &str,
    resolution: ConflictResolution,
) -> Result<(), String> {
    safe_rel(path)?;
    let repo = open_repo(repo_path)?;
    let workdir = repo
        .workdir()
        .ok_or("the context repo has no working tree")?
        .to_path_buf();
    let mut index = repo.index().map_err(|e| format!("index: {e}"))?;
    let conflict = index
        .conflicts()
        .map_err(|e| format!("conflicts: {e}"))?
        .filter_map(Result::ok)
        .find(|c| {
            c.our
                .as_ref()
                .or(c.their.as_ref())
                .or(c.ancestor.as_ref())
                .is_some_and(|e| index_path(e) == path)
        })
        .ok_or_else(|| format!("{path} is not in conflict"))?;
    let (mine, theirs) = match mine_stage(&repo) {
        MineStage::Ours => (conflict.our, conflict.their),
        MineStage::Theirs => (conflict.their, conflict.our),
        MineStage::Unknown => {
            return Err(format!(
                "Can't tell which side of {path} is this Mac's from what git left on disk; \
                 hand it to an agent to settle."
            ))
        }
    };
    // Checked before anything is touched, so a refused path leaves the conflict as it was.
    checked_dest(&workdir, path)?;
    if resolution == ConflictResolution::KeepBoth {
        checked_dest(&workdir, &theirs_copy_path(path))?;
    }

    // Removing the path drops all three conflict stages; the chosen side is added back.
    index
        .remove_path(Path::new(path))
        .map_err(|e| format!("clear conflict: {e}"))?;
    match resolution {
        ConflictResolution::UseMine => keep_side(&repo, &workdir, &mut index, &mine, path)?,
        ConflictResolution::UseTheirs => keep_side(&repo, &workdir, &mut index, &theirs, path)?,
        ConflictResolution::KeepBoth => match (&mine, &theirs) {
            (Some(_), Some(_)) => {
                keep_side(&repo, &workdir, &mut index, &mine, path)?;
                keep_side(
                    &repo,
                    &workdir,
                    &mut index,
                    &theirs,
                    &theirs_copy_path(path),
                )?;
            }
            (None, _) => keep_side(&repo, &workdir, &mut index, &theirs, path)?,
            (_, None) => keep_side(&repo, &workdir, &mut index, &mine, path)?,
        },
    }
    index.write().map_err(|e| format!("write index: {e}"))
}

#[tauri::command]
pub async fn resolve_context_conflict(
    repo_path: String,
    path: String,
    resolution: ConflictResolution,
) -> Result<(), String> {
    let repo = PathBuf::from(repo_path);
    tauri::async_runtime::spawn_blocking(move || {
        resolve_context_conflict_impl(&repo, &path, resolution)
    })
    .await
    .map_err(|e| format!("resolve_context_conflict task failed: {e}"))?
}

// ---------------------------------------------------------------------------
// Asking an agent
// ---------------------------------------------------------------------------

/// The card's ways out of a failed sync. Each asks an agent: CodeSense does not push,
/// pull or sign in to GitHub on the agents' behalf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ContextSyncAction {
    Commit,
    Retry,
    SignIn,
    SetUp,
}

pub fn conflict_hand_off_text(repo_path: &Path, path: &str) -> String {
    format!(
        "{path} in the team's context repo ({}) changed both here and on GitHub. \
         Settle it: merge the two versions so neither side's work is lost, stage it, \
         and finish the sync.",
        repo_path.display()
    )
}

pub fn sync_action_text(
    repo_path: &Path,
    action: ContextSyncAction,
    reason: Option<&str>,
) -> String {
    let repo = repo_path.display();
    let why = reason.map(|r| format!(" ({r})")).unwrap_or_default();
    match action {
        ContextSyncAction::Commit => {
            format!("Commit my private edits in {repo}{why}, locally, never push.")
        }
        ContextSyncAction::Retry => format!(
            "The sync of the team's context repo ({repo}) failed{why}. Find out why, fix it, \
             and sync again; call report_context_sync with how it went."
        ),
        ContextSyncAction::SignIn => format!(
            "The team's context repo ({repo}) can't reach GitHub: the sign-in expired{why}. \
             Sign in again (gh auth login or the git credential helper), then sync and call \
             report_context_sync."
        ),
        ContextSyncAction::SetUp => format!(
            "The team's context repo ({repo}) has changes nothing sends to GitHub: it has no \
             remote. Connect it to the team's GitHub repo and send the changes; call \
             report_context_sync when it is done."
        ),
    }
}

async fn ask_agent(
    app: tauri::AppHandle,
    team: super::agent_inbox::InboxTeam,
    agent_name: String,
    text: String,
    summary: String,
) -> Result<super::agent_read_model::HumanSendResult, String> {
    use super::agent_inbox::AppInboxHost;
    let host = AppInboxHost::port(app.clone());
    let result = super::agent_read_model::send_as_human(
        &host,
        &team.into(),
        &agent_name,
        &text,
        None,
        None,
        None,
        None,
        None,
        Some(&summary),
    )
    .await?;
    if matches!(
        result,
        super::agent_read_model::HumanSendResult::Sent {
            delivery_id: Some(_),
            ..
        }
    ) {
        super::agent_read_model::agent_state_changed(&app);
    }
    Ok(result)
}

/// Hand a conflicted file to an agent through the normal inbox path.
#[tauri::command]
pub async fn hand_context_conflict_to_agent(
    app: tauri::AppHandle,
    team: super::agent_inbox::InboxTeam,
    repo_path: String,
    path: String,
    agent_name: String,
) -> Result<super::agent_read_model::HumanSendResult, String> {
    safe_rel(&path)?;
    let text = conflict_hand_off_text(Path::new(&repo_path), &path);
    let summary = format!("Settle the conflict in {path}");
    ask_agent(app, team, agent_name, text, summary).await
}

/// Ask an agent to retry a failed sync, sign in again, or set up sending.
#[tauri::command]
pub async fn run_context_sync_action(
    app: tauri::AppHandle,
    team: super::agent_inbox::InboxTeam,
    repo_path: String,
    action: ContextSyncAction,
    agent_name: String,
    reason: Option<String>,
) -> Result<super::agent_read_model::HumanSendResult, String> {
    let text = sync_action_text(Path::new(&repo_path), action, reason.as_deref());
    let summary = match action {
        ContextSyncAction::Commit => "Commit your private edits locally; never push",
        ContextSyncAction::Retry => "Retry the context repo's sync",
        ContextSyncAction::SignIn => "Sign the context repo in to GitHub again",
        ContextSyncAction::SetUp => "Connect the context repo to GitHub",
    }
    .to_string();
    ask_agent(app, team, agent_name, text, summary).await
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/// Which commits a History page walks.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase", tag = "kind", content = "name")]
pub enum HistoryBranches {
    /// Every branch, remote branch and HEAD.
    #[default]
    All,
    /// The main line only, first parents.
    Main,
    /// One branch's own commits, then the commit it branched from.
    Branch(String),
}

/// How a commit's lane is drawn: the main line, a merged branch, a branch with an open PR,
/// a local branch GitHub has never seen, or a backup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LaneKind {
    Main,
    Merged,
    Open,
    Local,
    Backup,
}

/// An agent that made a commit, from its trailers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitAgent {
    pub name: String,
    /// The face it wore, as `pack:id`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryCommit {
    pub sha: String,
    pub short_sha: String,
    pub parents: Vec<String>,
    pub refs: Vec<RefLabel>,
    /// The subject without a PR suffix or merge boilerplate.
    pub subject: String,
    pub author: String,
    /// The author is the person at this Mac (their git email).
    pub by_you: bool,
    /// Unix seconds.
    pub committed_at: i64,
    /// The PR this commit merged, from its subject.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<u64>,
    pub lane: LaneKind,
    /// Not on GitHub yet: no remote branch has it.
    pub local_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<CommitAgent>,
    /// The commit a one-branch page ends on: where the branch left main.
    pub fork_point: bool,
    /// The files it changed (repo-relative, vs its first parent), on a page filtered by
    /// path, so a caller can narrow further without asking again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
}

/// A PR GitHub has open, from the PR head refs fetched into this clone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenPr {
    pub number: u64,
    pub sha: String,
    /// The branch whose tip is the PR head, when one is here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPage {
    pub commits: Vec<HistoryCommit>,
    /// Pass back as `cursor` for the next page; `None` at the end.
    pub next_cursor: Option<String>,
    pub open_prs: Vec<OpenPr>,
    /// Every branch a filter can pick, main first.
    pub branches: Vec<String>,
}

pub(crate) struct Identity {
    pub(crate) author: String,
    pub(crate) agent: Option<CommitAgent>,
}

fn trailer_lines(message: &str) -> impl Iterator<Item = (&str, &str)> {
    let body = message.split_once("\n\n").map(|(_, b)| b).unwrap_or("");
    body.lines()
        .filter_map(|l| l.split_once(": "))
        .map(|(k, v)| (k.trim(), v.trim()))
}

/// Mail domains whose co-authors are agents, not people.
const AGENT_DOMAINS: &[&str] = &["anthropic.com", "openai.com", "google.com", "cursor.com"];

fn agent_from_co_author(value: &str) -> Option<String> {
    let (name, email) = value.split_once('<')?;
    let email = email.trim_end_matches('>').trim().to_ascii_lowercase();
    let domain = email.rsplit('@').next()?;
    if !AGENT_DOMAINS
        .iter()
        .any(|d| domain == *d || domain.ends_with(&format!(".{d}")))
    {
        return None;
    }
    let first = name.split_whitespace().next()?;
    Some(first.to_ascii_lowercase())
}

/// Who made a commit: its author, and the agent that wrote it when its trailers say so
/// (`Actor-Name` from a CodeSense write, or an agent's `Co-Authored-By`), or when the
/// author is a bot.
pub(crate) fn commit_identity(commit: &git2::Commit) -> Identity {
    let author = commit.author().name().unwrap_or("").to_string();
    let message = commit.message().unwrap_or("");
    let mut actor_kind = None;
    let mut actor_name = None;
    let mut avatar = None;
    let mut co_author = None;
    for (k, v) in trailer_lines(message) {
        match k {
            "Actor-Kind" => actor_kind = Some(v.to_string()),
            "Actor-Name" => actor_name = Some(v.to_string()),
            "Actor-Avatar" => avatar = Some(v.to_string()),
            k if k.eq_ignore_ascii_case("Co-Authored-By") && co_author.is_none() => {
                co_author = agent_from_co_author(v)
            }
            _ => {}
        }
    }
    let agent_name = match actor_kind.as_deref() {
        Some("human") => None,
        _ => actor_name.or(co_author).or_else(|| {
            author
                .strip_suffix("[bot]")
                .map(|n| n.trim().to_ascii_lowercase())
        }),
    };
    Identity {
        author,
        agent: agent_name
            .filter(|n| !n.is_empty())
            .map(|name| CommitAgent { name, avatar }),
    }
}

/// The subject a person reads, and the PR it merged: "Merge pull request #23 from x/y"
/// reads as the PR's title (the next line), "Title (#23)" as "Title".
pub(crate) fn clean_subject(commit: &git2::Commit) -> (String, Option<u64>) {
    let summary = Option::flatten(Result::ok(commit.summary()))
        .unwrap_or("")
        .trim()
        .to_string();
    if let Some(rest) = summary.strip_prefix("Merge pull request #") {
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = digits.parse::<u64>() {
            let title = Option::flatten(Result::ok(commit.body()))
                .and_then(|b| b.lines().map(str::trim).find(|l| !l.is_empty()))
                .map(str::to_string)
                .unwrap_or_else(|| {
                    rest.split_once(" from ")
                        .map(|(_, b)| b.to_string())
                        .unwrap_or_default()
                });
            return (title, Some(n));
        }
    }
    if let Some(open) = summary.rfind("(#") {
        let tail = &summary[open + 2..];
        if let Some(num) = tail.strip_suffix(')') {
            if let Ok(n) = num.parse::<u64>() {
                return (summary[..open].trim_end().to_string(), Some(n));
            }
        }
    }
    (summary, None)
}

/// The main line's tips: local `main`/`master` and its remote copy.
fn main_tips(repo: &git2::Repository) -> Vec<git2::Oid> {
    let mut out = Vec::new();
    for name in [
        "refs/heads/main",
        "refs/remotes/origin/main",
        "refs/heads/master",
        "refs/remotes/origin/master",
    ] {
        if let Some(oid) = repo.find_reference(name).ok().and_then(|r| r.target()) {
            if !out.contains(&oid) {
                out.push(oid);
            }
        }
    }
    out
}

fn first_parent_chain(
    repo: &git2::Repository,
    tips: &[git2::Oid],
    limit: usize,
) -> HashSet<git2::Oid> {
    let mut set = HashSet::new();
    for tip in tips {
        let mut cur = repo.find_commit(*tip).ok();
        let mut n = 0;
        while let Some(c) = cur {
            if !set.insert(c.id()) || n >= limit {
                break;
            }
            n += 1;
            cur = c.parent(0).ok();
        }
    }
    set
}

/// The PR number a PR head ref carries (`pr/24`, `origin/pr/24`, `refs/pull/24/head`).
fn pr_ref_number(full: &str) -> Option<u64> {
    let rest = full
        .strip_prefix("refs/remotes/pr/")
        .or_else(|| full.strip_prefix("refs/remotes/origin/pr/"))
        .or_else(|| {
            full.strip_prefix("refs/pull/")
                .and_then(|r| r.strip_suffix("/head"))
        })?;
    rest.parse().ok()
}

fn is_main_name(name: &str) -> bool {
    matches!(
        name,
        "main" | "master" | "origin/main" | "origin/master" | "HEAD"
    )
}

struct RefFacts {
    open_prs: Vec<OpenPr>,
    /// Branch tips, each with the lane kind its own commits take.
    branch_tips: Vec<(git2::Oid, LaneKind)>,
    branches: Vec<String>,
}

fn ref_facts(repo: &git2::Repository, main: &HashSet<git2::Oid>) -> Result<RefFacts, String> {
    let main_tip = main_tips(repo);
    let merged_into_main = |oid: git2::Oid| {
        main.contains(&oid)
            || main_tip
                .iter()
                .any(|m| repo.graph_descendant_of(*m, oid).unwrap_or(false))
    };

    let mut open_prs = Vec::new();
    let mut locals: Vec<(String, git2::Oid)> = Vec::new();
    let mut remotes: HashMap<String, git2::Oid> = HashMap::new();
    for r in repo.references().map_err(|e| format!("references: {e}"))? {
        let r = r.map_err(|e| format!("reference: {e}"))?;
        if matches!(r.symbolic_target(), Ok(Some(_))) {
            continue;
        }
        let full = r.name().unwrap_or_default().to_string();
        let Some(oid) = r.peel_to_commit().ok().map(|c| c.id()) else {
            continue;
        };
        if let Some(n) = pr_ref_number(&full) {
            if !merged_into_main(oid) {
                open_prs.push(OpenPr {
                    number: n,
                    sha: oid.to_string(),
                    branch: None,
                });
            }
            continue;
        }
        match ref_kind(&full) {
            Some(RefKind::Local) => locals.push((full["refs/heads/".len()..].to_string(), oid)),
            Some(RefKind::Remote) => {
                remotes.insert(full["refs/remotes/".len()..].to_string(), oid);
            }
            _ => {}
        }
    }
    open_prs.sort_by_key(|p| p.number);
    open_prs.dedup_by_key(|p| p.number);

    let mut branch_tips = Vec::new();
    let mut branches = Vec::new();
    let mut named = |name: &str, oid: git2::Oid, published: bool| {
        if is_main_name(name) {
            return;
        }
        let pr = open_prs.iter_mut().find(|p| p.sha == oid.to_string());
        let kind = if let Some(pr) = pr {
            pr.branch.get_or_insert_with(|| name.to_string());
            LaneKind::Open
        } else if name.starts_with("backup/") || name.contains("/backup/") {
            LaneKind::Backup
        } else if !published {
            LaneKind::Local
        } else {
            LaneKind::Merged
        };
        branch_tips.push((oid, kind));
        if !branches.iter().any(|b| b == name) {
            branches.push(name.to_string());
        }
    };
    for (name, oid) in &locals {
        let published = remotes.contains_key(&format!("origin/{name}"))
            || repo
                .find_branch(name, git2::BranchType::Local)
                .ok()
                .and_then(|b| b.upstream().ok())
                .is_some();
        named(name, *oid, published);
    }
    let mut remote_names: Vec<_> = remotes.iter().collect();
    remote_names.sort();
    for (name, oid) in remote_names {
        let short = name.split_once('/').map(|(_, b)| b).unwrap_or(name);
        if locals.iter().any(|(l, _)| l == short) {
            continue;
        }
        named(short, *oid, true);
    }
    for pr in &open_prs {
        if pr.branch.is_none() {
            let oid = git2::Oid::from_str(&pr.sha).map_err(|e| format!("pr sha: {e}"))?;
            branch_tips.push((oid, LaneKind::Open));
        }
    }
    // Open PRs claim their commits first, then local work, backups, the rest.
    let rank = |k: &LaneKind| match k {
        LaneKind::Open => 0,
        LaneKind::Local => 1,
        LaneKind::Backup => 2,
        _ => 3,
    };
    branch_tips.sort_by_key(|(_, k)| rank(k));
    branches.sort();
    let mut all = vec!["main".to_string()];
    all.extend(branches);
    Ok(RefFacts {
        open_prs,
        branch_tips,
        branches: all,
    })
}

fn lane_kinds(
    repo: &git2::Repository,
    main: &HashSet<git2::Oid>,
    tips: &[(git2::Oid, LaneKind)],
) -> HashMap<git2::Oid, LaneKind> {
    let mut out = HashMap::new();
    for (tip, kind) in tips {
        let mut cur = repo.find_commit(*tip).ok();
        let mut n = 0;
        while let Some(c) = cur {
            if main.contains(&c.id()) || out.contains_key(&c.id()) || n >= BRANCH_WALK_LIMIT {
                break;
            }
            out.insert(c.id(), *kind);
            n += 1;
            cur = c.parent(0).ok();
        }
    }
    out
}

/// Commits no remote branch has: reachable from a local branch or HEAD, hidden by every
/// remote ref.
fn local_only(repo: &git2::Repository) -> Result<HashSet<git2::Oid>, String> {
    let mut walk = repo.revwalk().map_err(|e| format!("revwalk: {e}"))?;
    let mut pushed = false;
    for r in repo.references().map_err(|e| format!("references: {e}"))? {
        let r = r.map_err(|e| format!("reference: {e}"))?;
        let name = r.name().unwrap_or_default();
        let Some(oid) = r.target() else { continue };
        match ref_kind(name) {
            Some(RefKind::Local) => {
                walk.push(oid).map_err(|e| format!("revwalk push: {e}"))?;
                pushed = true;
            }
            Some(RefKind::Remote) => {
                walk.hide(oid).map_err(|e| format!("revwalk hide: {e}"))?;
            }
            _ => {}
        }
    }
    if let Some(oid) = repo.head().ok().and_then(|h| h.target()) {
        walk.push(oid)
            .map_err(|e| format!("revwalk push HEAD: {e}"))?;
        pushed = true;
    }
    if !pushed {
        return Ok(HashSet::new());
    }
    Ok(walk.flatten().take(BRANCH_WALK_LIMIT * 2).collect())
}

fn touches(paths: &[String], filters: &[String]) -> bool {
    paths.iter().any(|p| {
        filters.iter().any(|f| {
            let f = f.trim_end_matches('/');
            f.is_empty() || p == f || p.starts_with(&format!("{f}/"))
        })
    })
}

fn your_email(repo: &git2::Repository) -> Option<String> {
    repo.config()
        .ok()?
        .get_string("user.email")
        .ok()
        .map(|e| e.to_ascii_lowercase())
}

/// One page of the context repo's history, newest first. `paths` keeps only commits that
/// changed a file under one of those repo-relative paths (a team's content, an agent's
/// files). Bounded: a page holds at most `limit` commits and scans at most a few thousand.
pub fn context_repo_history_impl(
    repo_path: &Path,
    branches: &HistoryBranches,
    paths: Option<&[String]>,
    limit: usize,
    cursor: Option<&str>,
) -> Result<HistoryPage, String> {
    context_repo_history_impl_for(
        repo_path,
        branches,
        paths,
        limit,
        cursor,
        &Default::default(),
    )
}

pub(crate) fn context_repo_history_impl_for(
    repo_path: &Path,
    branches: &HistoryBranches,
    paths: Option<&[String]>,
    limit: usize,
    cursor: Option<&str>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<HistoryPage, String> {
    let repo = open_repo(repo_path)?;
    let limit = limit.clamp(1, MAX_PAGE);
    let tips = main_tips(&repo);
    let main = first_parent_chain(&repo, &tips, MAIN_LINE_LIMIT);
    let facts = ref_facts(&repo, &main)?;
    let kinds = lane_kinds(&repo, &main, &facts.branch_tips);
    let local = local_only(&repo)?;
    let mut refs_map = build_ref_label_map(&repo)?;
    // PR heads show as their PR, not as a remote branch.
    for labels in refs_map.values_mut() {
        labels.retain(|l| {
            !(l.kind == RefKind::Remote
                && pr_ref_number(&format!("refs/remotes/{}", l.name)).is_some())
        });
    }
    let you = your_email(&repo);

    let mut walk = new_sorted_revwalk(&repo)?;
    let mut fork: Option<git2::Oid> = None;
    match branches {
        HistoryBranches::All => {
            let mut pushed = false;
            for r in repo.references().map_err(|e| format!("references: {e}"))? {
                let r = r.map_err(|e| format!("reference: {e}"))?;
                let name = r.name().unwrap_or_default().to_string();
                if matches!(ref_kind(&name), Some(RefKind::Local | RefKind::Remote))
                    || pr_ref_number(&name).is_some()
                {
                    if let Some(oid) = r.target() {
                        walk.push(oid).map_err(|e| format!("revwalk push: {e}"))?;
                        pushed = true;
                    }
                }
            }
            if let Some(oid) = repo.head().ok().and_then(|h| h.target()) {
                walk.push(oid)
                    .map_err(|e| format!("revwalk push HEAD: {e}"))?;
                pushed = true;
            }
            if !pushed {
                return Ok(empty_page(facts));
            }
        }
        HistoryBranches::Main => {
            if tips.is_empty() {
                return Ok(empty_page(facts));
            }
            walk.simplify_first_parent()
                .map_err(|e| format!("first parent: {e}"))?;
            for t in &tips {
                walk.push(*t).map_err(|e| format!("revwalk push: {e}"))?;
            }
        }
        HistoryBranches::Branch(name) => {
            let tip = [name.clone(), format!("origin/{name}")]
                .iter()
                .find_map(|n| repo.resolve_reference_from_short_name(n).ok())
                .and_then(|r| r.peel_to_commit().ok())
                .map(|c| c.id())
                .or_else(|| {
                    facts
                        .open_prs
                        .iter()
                        .find(|p| p.branch.as_deref() == Some(name.as_str()))
                        .and_then(|p| git2::Oid::from_str(&p.sha).ok())
                })
                .ok_or_else(|| format!("no branch named {name}"))?;
            walk.push(tip).map_err(|e| format!("revwalk push: {e}"))?;
            for t in &tips {
                walk.hide(*t).map_err(|e| format!("revwalk hide: {e}"))?;
            }
            fork = tips.iter().find_map(|t| repo.merge_base(*t, tip).ok());
        }
    }

    let cursor_oid = cursor.and_then(|c| git2::Oid::from_str(c).ok());
    let mut skipping = cursor_oid.is_some();
    let mut commits = Vec::with_capacity(limit.min(128));
    let mut scanned = 0usize;
    let mut last_seen: Option<git2::Oid> = None;
    let mut exhausted = true;
    let project = |c: &git2::Commit, fork_point: bool| {
        let (subject, pr) = clean_subject(c);
        let identity = commit_identity(c);
        let email = Result::ok(c.author().email()).map(|e| e.to_ascii_lowercase());
        HistoryCommit {
            sha: c.id().to_string(),
            short_sha: c.id().to_string().chars().take(7).collect(),
            parents: if fork_point {
                Vec::new()
            } else {
                c.parent_ids().map(|p| p.to_string()).collect()
            },
            refs: refs_map.get(&c.id()).cloned().unwrap_or_default(),
            subject,
            author: identity.author,
            by_you: email.is_some() && email == you,
            committed_at: c.time().seconds(),
            pr,
            lane: if main.contains(&c.id()) {
                LaneKind::Main
            } else {
                kinds.get(&c.id()).copied().unwrap_or(LaneKind::Merged)
            },
            local_only: local.contains(&c.id()),
            agent: identity.agent,
            fork_point,
            files: None,
        }
    };
    for oid in walk {
        let oid = oid.map_err(|e| format!("revwalk: {e}"))?;
        if skipping {
            if Some(oid) == cursor_oid {
                skipping = false;
            }
            continue;
        }
        if commits.len() >= limit || scanned >= FILTER_SCAN_LIMIT {
            exhausted = false;
            break;
        }
        scanned += 1;
        last_seen = Some(oid);
        let c = repo
            .find_commit(oid)
            .map_err(|e| format!("find commit {oid}: {e}"))?;
        let scoped = !matches!(scope, crate::context_repo::caller_scope::CallerScope::Human);
        let files = if scoped || paths.is_some() {
            let changed: Vec<_> = commit_changed_paths(&repo, &c)?
                .into_iter()
                .filter(|path| scope.allows(&repo_path.join(path)))
                .collect();
            if (scoped && changed.is_empty())
                || paths.is_some_and(|filters| !touches(&changed, filters))
            {
                continue;
            }
            Some(changed)
        } else {
            None
        };
        let mut entry = project(&c, false);
        entry.files = files;
        if matches!(branches, HistoryBranches::Main) {
            entry.parents.truncate(1);
        }
        commits.push(entry);
    }
    if exhausted {
        if let Some(f) = fork.and_then(|f| repo.find_commit(f).ok()) {
            commits.push(project(&f, true));
        }
    }
    Ok(HistoryPage {
        commits,
        next_cursor: if exhausted {
            None
        } else {
            last_seen.map(|o| o.to_string())
        },
        open_prs: facts.open_prs,
        branches: facts.branches,
    })
}

fn empty_page(facts: RefFacts) -> HistoryPage {
    HistoryPage {
        commits: Vec::new(),
        next_cursor: None,
        open_prs: facts.open_prs,
        branches: facts.branches,
    }
}

#[tauri::command]
pub async fn context_repo_history(
    repo_path: String,
    branches: Option<HistoryBranches>,
    paths: Option<Vec<String>>,
    limit: Option<usize>,
    cursor: Option<String>,
) -> Result<HistoryPage, String> {
    let path = PathBuf::from(repo_path);
    tauri::async_runtime::spawn_blocking(move || {
        context_repo_history_impl(
            &path,
            &branches.unwrap_or_default(),
            paths.as_deref(),
            limit.unwrap_or(60),
            cursor.as_deref(),
        )
    })
    .await
    .map_err(|e| format!("context_repo_history task failed: {e}"))?
}

#[cfg(test)]
#[path = "context_repo_git_tests.rs"]
mod tests;
