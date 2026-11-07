// SPDX-License-Identifier: MIT
//! Watch the context repo's git state, so a commit, push, fetch, checkout, merge or rebase
//! done by anyone (an agent in or outside CodeSense, a human in a terminal) reaches the nav's
//! status button, Changes and History within a moment, with no poll.
//!
//! Only git's small state files are watched: the git dir itself non-recursively (`HEAD`,
//! `index`, `FETCH_HEAD`, `ORIG_HEAD`, `MERGE_HEAD`, `packed-refs`, the `rebase-*` dirs
//! appearing and going) and `refs/` recursively. `objects/` and `logs/` are never watched.
//! The dirs come from git itself (`repo.path()` and its `commondir`), so a `.git` file that
//! points elsewhere, a linked worktree and `core.worktree` all resolve to the real ones.
//!
//! Each changed path is classified into a [`GitMoveKind`]; a [`MoveCoalescer`] gathers a
//! burst per repo (a rebase rewrites refs and the index many times) and emits one
//! [`CONTEXT_GIT_MOVED_EVENT`] carrying every kind seen once the burst has been quiet for
//! [`QUIET`] (or has gathered for [`MAX_GATHER`]).
//!
//! The app's own reads (status, the changed-files diff, History) are libgit2 reads that never
//! write the index, so reading after an event cannot cause another.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::content_engine::{DispatchHandler, Dispatcher, WatchDir, Watcher as ContentWatcher};

/// Emitted once per settled burst of git activity in the watched context repo.
pub const CONTEXT_GIT_MOVED_EVENT: &str = "context-git-moved";

/// How long a repo's git activity must be quiet before its event is emitted.
pub const QUIET: Duration = Duration::from_millis(300);

/// The longest a burst that never goes quiet is held before it is emitted.
pub const MAX_GATHER: Duration = Duration::from_millis(1_500);

/// What moved in the repo's git state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum GitMoveKind {
    /// `HEAD` (a commit on a detached HEAD, a checkout, a reset) or `ORIG_HEAD`.
    Head,
    /// A branch, remote-tracking branch or tag (`refs/…`, `packed-refs`): a commit, a push,
    /// a fetch that brought something.
    Refs,
    /// The index: staging, a commit, a checkout, a merge.
    Index,
    /// `FETCH_HEAD`: a fetch ran, even one that brought nothing.
    Fetch,
    /// A merge, cherry-pick, revert or rebase started or ended.
    MergeState,
}

const ALL_KINDS: [GitMoveKind; 5] = [
    GitMoveKind::Head,
    GitMoveKind::Refs,
    GitMoveKind::Index,
    GitMoveKind::Fetch,
    GitMoveKind::MergeState,
];

/// The payload of [`CONTEXT_GIT_MOVED_EVENT`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextGitMoved {
    /// The repo's working dir, spelled as it was registered (the org root).
    pub repo_path: String,
    /// Every kind seen in the burst, sorted.
    pub kinds: Vec<GitMoveKind>,
}

/// Where a repo keeps its git state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitDirs {
    /// The working tree's own git dir (`.git`, or `.git/worktrees/<name>` for a linked
    /// worktree): `HEAD`, `index`, merge and rebase state.
    pub git_dir: PathBuf,
    /// The dir shared by every worktree: `refs/`, `packed-refs`. Equal to `git_dir` for a
    /// plain clone.
    pub common_dir: PathBuf,
}

impl GitDirs {
    /// Ask git where `repo_root` keeps its state. `None` when it is not a git repo.
    pub fn resolve(repo_root: &Path) -> Option<Self> {
        let repo = git2::Repository::open(repo_root).ok()?;
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let git_dir = canon(repo.path());
        // A linked worktree's git dir names the shared one in its `commondir` file, relative
        // to itself or absolute; a plain clone has none.
        let common_dir = std::fs::read_to_string(git_dir.join("commondir"))
            .ok()
            .map(|text| canon(&git_dir.join(text.trim())))
            .unwrap_or_else(|| git_dir.clone());
        Some(Self {
            git_dir,
            common_dir,
        })
    }

    /// The dirs to watch: each git dir non-recursively, `refs/` recursively.
    pub fn watch_dirs(&self) -> Vec<WatchDir> {
        let mut dirs = vec![WatchDir {
            path: self.git_dir.clone(),
            recursive: false,
        }];
        if self.common_dir != self.git_dir {
            dirs.push(WatchDir {
                path: self.common_dir.clone(),
                recursive: false,
            });
        }
        dirs.push(WatchDir {
            path: self.common_dir.join("refs"),
            recursive: true,
        });
        dirs
    }

    /// The dirs the dispatcher routes to the handler (`refs/` resolves to its parent).
    fn handler_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = vec![self.git_dir.clone()];
        if self.common_dir != self.git_dir {
            dirs.push(self.common_dir.clone());
        }
        dirs
    }

    /// What a change at `path` means, if anything. The per-worktree git dir is tried first
    /// (it may sit inside the common dir, under `worktrees/`).
    pub fn classify(&self, path: &Path) -> Option<GitMoveKind> {
        [&self.git_dir, &self.common_dir]
            .into_iter()
            .find_map(|dir| relative_to(path, dir))
            .and_then(|rel| classify_rel(&rel))
    }
}

/// `path` relative to `dir`, trying the path as given and with its parent canonicalized (the
/// watcher may report `/var/…` or `/private/var/…` for the same file).
fn relative_to(path: &Path, dir: &Path) -> Option<PathBuf> {
    if let Ok(rel) = path.strip_prefix(dir) {
        return Some(rel.to_path_buf());
    }
    let canonical = crate::content_engine::dispatcher::stable_key(path);
    canonical.strip_prefix(dir).ok().map(Path::to_path_buf)
}

/// What a change at `rel` (relative to a git dir) means. Lock files are git's way of writing
/// the real file and are followed by it, except `index.lock`, whose coming and going is what
/// tells a sync is under way.
fn classify_rel(rel: &Path) -> Option<GitMoveKind> {
    let parts = super::scratch_watch::segments(rel)?;
    match parts.as_slice() {
        [name] => match name.as_str() {
            "HEAD" | "ORIG_HEAD" => Some(GitMoveKind::Head),
            "index" | "index.lock" => Some(GitMoveKind::Index),
            "packed-refs" => Some(GitMoveKind::Refs),
            "FETCH_HEAD" => Some(GitMoveKind::Fetch),
            "MERGE_HEAD" | "CHERRY_PICK_HEAD" | "REVERT_HEAD" | "rebase-merge" | "rebase-apply" => {
                Some(GitMoveKind::MergeState)
            }
            _ => None,
        },
        [first, .., last] if first == "refs" && !last.ends_with(".lock") => Some(GitMoveKind::Refs),
        _ => None,
    }
}

#[derive(Debug)]
struct Burst {
    kinds: BTreeSet<GitMoveKind>,
    first: Instant,
    last: Instant,
}

type MoveSink = Arc<dyn Fn(ContextGitMoved) + Send + Sync>;

/// Gathers a repo's git moves and hands them to `sink` once per burst. Never blocks the
/// caller: each burst waits on its own short-lived thread.
pub struct MoveCoalescer {
    quiet: Duration,
    max: Duration,
    pending: Arc<Mutex<HashMap<String, Burst>>>,
    sink: MoveSink,
}

impl MoveCoalescer {
    pub fn new(quiet: Duration, max: Duration, sink: MoveSink) -> Self {
        Self {
            quiet,
            max,
            pending: Arc::new(Mutex::new(HashMap::new())),
            sink,
        }
    }

    pub fn note(&self, repo_path: &str, kinds: &[GitMoveKind]) {
        let now = Instant::now();
        {
            let Ok(mut pending) = self.pending.lock() else {
                return;
            };
            if let Some(burst) = pending.get_mut(repo_path) {
                burst.kinds.extend(kinds.iter().copied());
                burst.last = now;
                return;
            }
            pending.insert(
                repo_path.to_string(),
                Burst {
                    kinds: kinds.iter().copied().collect(),
                    first: now,
                    last: now,
                },
            );
        }
        let (pending, sink) = (Arc::clone(&self.pending), Arc::clone(&self.sink));
        let (quiet, max) = (self.quiet, self.max);
        let repo_path = repo_path.to_string();
        std::thread::spawn(move || loop {
            let wait = {
                let Ok(mut locked) = pending.lock() else {
                    return;
                };
                let Some(burst) = locked.get(&repo_path) else {
                    return;
                };
                let due = (burst.last + quiet).min(burst.first + max);
                let wait = due.saturating_duration_since(Instant::now());
                if wait.is_zero() {
                    let Some(burst) = locked.remove(&repo_path) else {
                        return;
                    };
                    drop(locked);
                    sink(ContextGitMoved {
                        repo_path,
                        kinds: burst.kinds.into_iter().collect(),
                    });
                    return;
                }
                wait
            };
            std::thread::sleep(wait);
        });
    }
}

/// Register the handlers for `dirs` and attach their watches.
pub(crate) fn attach_git_watch(
    watcher: &ContentWatcher,
    dispatcher: &Dispatcher,
    repo_path: &str,
    dirs: &GitDirs,
    coalescer: &Arc<MoveCoalescer>,
) {
    register_git_handlers(dispatcher, repo_path, dirs, coalescer);
    for dir in dirs.watch_dirs() {
        if let Err(e) = watcher.watch(dir.clone()) {
            log::warn!(
                target: "watch",
                "context git watch: watch({}) failed: {e}",
                dir.path.display()
            );
        }
    }
}

/// Route changes under `dirs` to `coalescer` for `repo_path`: a change is classified and
/// noted; a dropped-events signal on these dirs notes every kind.
fn register_git_handlers(
    dispatcher: &Dispatcher,
    repo_path: &str,
    dirs: &GitDirs,
    coalescer: &Arc<MoveCoalescer>,
) {
    let handler: DispatchHandler = {
        let (dirs, coalescer, repo) = (dirs.clone(), Arc::clone(coalescer), repo_path.to_string());
        Arc::new(move |path: &Path| {
            if let Some(kind) = dirs.classify(path) {
                coalescer.note(&repo, &[kind]);
            }
        })
    };
    let rescan: DispatchHandler = {
        let (coalescer, repo) = (Arc::clone(coalescer), repo_path.to_string());
        Arc::new(move |_path: &Path| coalescer.note(&repo, &ALL_KINDS))
    };
    for dir in dirs.handler_dirs() {
        dispatcher.register_handler(&dir, Arc::clone(&handler));
        dispatcher.register_rescan_handler(&dir, Arc::clone(&rescan));
    }
}

fn detach_git_watch(watcher: &ContentWatcher, dispatcher: &Dispatcher, dirs: &GitDirs) {
    for dir in dirs.handler_dirs() {
        dispatcher.unregister_handler(&dir);
    }
    for dir in dirs.watch_dirs() {
        let _ = watcher.unwatch(&dir.path);
    }
}

/// The context repo currently watched (the active team's org) and its git dirs.
static WATCHED: Mutex<Option<(PathBuf, GitDirs)>> = Mutex::new(None);

/// The app's one coalescer, emitting [`CONTEXT_GIT_MOVED_EVENT`] on `app`.
pub(crate) fn app_coalescer<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Arc<MoveCoalescer> {
    let build = || {
        let app = app.clone();
        Arc::new(MoveCoalescer::new(
            QUIET,
            MAX_GATHER,
            Arc::new(move |moved: ContextGitMoved| {
                use tauri::Emitter;
                if let Err(e) = app.emit(CONTEXT_GIT_MOVED_EVENT, &moved) {
                    log::warn!(target: "watch", "emit {CONTEXT_GIT_MOVED_EVENT} failed: {e}");
                }
            }),
        ))
    };
    // Tests build many mock apps: a process-wide coalescer would keep emitting on the first.
    #[cfg(test)]
    {
        build()
    }
    #[cfg(not(test))]
    {
        static COALESCER: std::sync::OnceLock<Arc<MoveCoalescer>> = std::sync::OnceLock::new();
        Arc::clone(COALESCER.get_or_init(build))
    }
}

/// Watch the git state of the context repo at `org_root` (`dirs` from [`GitDirs::resolve`],
/// `None` when it is not a git repo). Idempotent; activating another org moves the watch to
/// it. A no-op until the app's watcher has started (the next team activation re-attempts).
pub fn register_context_git_watch<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &super::watch::WatcherState,
    org_root: &Path,
    dirs: Option<GitDirs>,
) {
    let Some(watcher) = state.watcher_handle() else {
        log::debug!(target: "watch", "context git watch: watcher not started yet; deferring");
        return;
    };
    let Ok(mut watched) = WATCHED.lock() else {
        return;
    };
    if let Some((previous_root, previous_dirs)) = watched.take() {
        if previous_root != org_root || dirs.as_ref() != Some(&previous_dirs) {
            detach_git_watch(&watcher, &state.dispatcher, &previous_dirs);
        }
    }
    let Some(dirs) = dirs else {
        return;
    };
    attach_git_watch(
        &watcher,
        &state.dispatcher,
        &org_root.to_string_lossy(),
        &dirs,
        &app_coalescer(app),
    );
    *watched = Some((org_root.to_path_buf(), dirs));
}

/// Scratch has its own watch lifetime: selecting it never detaches the clone's watch.
pub fn register_private_git_watch<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &super::watch::WatcherState,
    root: &Path,
) {
    static PRIVATE: Mutex<Option<(PathBuf, GitDirs)>> = Mutex::new(None);
    let Some(watcher) = state.watcher_handle() else {
        return;
    };
    let Some(dirs) = GitDirs::resolve(root) else {
        return;
    };
    let mut watched = PRIVATE.lock().unwrap_or_else(|e| e.into_inner());
    if watched
        .as_ref()
        .is_some_and(|(previous, old)| previous == root && old == &dirs)
    {
        return;
    }
    if let Some((_, previous)) = watched.take() {
        detach_git_watch(&watcher, &state.dispatcher, &previous);
    }
    attach_git_watch(
        &watcher,
        &state.dispatcher,
        &root.to_string_lossy(),
        &dirs,
        &app_coalescer(app),
    );
    *watched = Some((root.to_path_buf(), dirs));
}

#[cfg(test)]
#[path = "context_git_watch_tests.rs"]
mod tests;
