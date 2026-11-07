// SPDX-License-Identifier: MIT
//! Worktree source-file watcher.
//!
//! The in-app code editor is a **second writer** over a story
//! worktree that an agent is also editing live in the terminal. This module is
//! the editor's eyes on that worktree: a lazy, per-open-inset `notify` watcher
//! rooted at `ws.cwd` whose emit sink the host supplies (the app emits a
//! `worktree-file-change` event) so the editor can reload (clean buffer) or warn
//! (dirty buffer) when the agent writes.
//!
//! **Why a NEW watcher, not the context watcher.** The app's context
//! watcher is bound to context/team dirs and feeds a projection-wave
//! `Dispatcher` that bakes `Dispatcher::dispatch` into its notify callback, so
//! reusing it would fan a worktree edit into the context-repo projection engine.
//! This watcher owns its own bare `notify::RecommendedWatcher` and emits
//! directly, sharing only the *idioms* (the 50 ms coalesce window, a relevance
//! filter) — never the dispatch path.
//!
//! **Recursive, root-scoped.** [`attach`] registers ONE recursive watch on the
//! worktree root and filters the stream through `EXCLUDED_DIR_NAMES` — the
//! local editor relies on a change anywhere in the tree reaching it without
//! expanding a directory first.
//!
//! Lifecycle: attach on inset open, detach on close/teardown. One watch per
//! open inset, keyed by `ws_id`; idempotent (re-open won't double-attach),
//! leak-free (detach drops the watcher, stopping the OS watch).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use log::warn;
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher};
use serde::Serialize;

/// FSEvents coalesce window. Mirrors the context watcher's `DEBOUNCE_MS` — macOS
/// double-fires the same path within a few ms; 50 ms collapses the burst to a
/// single emit without adding perceptible reload latency. (An honest local copy
/// of the tuning constant — NOT a shared dispatcher.)
const DEBOUNCE_MS: u64 = 50;

/// Directory names whose subtrees are excluded from the emit stream.
/// A `git checkout` / `npm install` / `cargo build` churns thousands of paths
/// under these and none is a file the user edits in the inset; excluding them
/// here (the FE tree only *de-emphasizes* them) prevents an FSEvents flood that
/// would otherwise spam reload/warn signals and burn CPU.
const EXCLUDED_DIR_NAMES: &[&str] = &[".git", "node_modules", "target", "dist"];

/// Process-wide monotonic attach token. Each attach mints one; detach carries
/// the token of the attach it is cleaning up. This makes the registry safe
/// against out-of-order attach/detach IPC (a StrictMode double-mount or a rapid
/// re-open issues `attach → detach → attach` as independent async calls — a
/// stale detach must not remove a newer attach's watch, or the inset is left
/// silently watcher-dead). See [`detach`].
static WORKTREE_WATCH_SEQ: AtomicU64 = AtomicU64::new(1);

/// The `worktree-file-change` event payload. `camelCase` for the TS listener
/// (`{ wsId, changedPath }`), matching the app's other event payloads.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeFileChange {
    /// The workspace whose inset attached this watch — lets the App listener
    /// route straight to the right inset with no reverse cwd→ws lookup.
    pub ws_id: String,
    /// Path of the changed file **relative to the watched worktree root** (e.g.
    /// `src/main.rs`). Relative — NOT absolute — on purpose: the watcher roots
    /// at the *canonicalized* cwd (notify reports canonicalized paths), but the
    /// FE only knows the raw `ws.cwd`. Emitting the relative suffix lets the FE
    /// re-join it to its own raw cwd and string-compare against the open file,
    /// so a symlinked prefix (`/tmp`→`/private/tmp`, a symlinked home) can't make
    /// the two path-spaces disagree and silently defeat the match.
    pub changed_path: String,
}

/// A live worktree watch: the `notify` watcher (dropping it stops the OS watch)
/// plus the token of the attach that created it (for token-matched detach).
struct WorktreeWatch {
    // Held only to keep the watch alive; dropped on detach / state teardown.
    _watcher: RecommendedWatcher,
    // The best-effort git-ref watcher (commit / branch-switch
    // liveness). `None` when its git dirs couldn't be resolved — the file watch
    // still works. Dropped alongside `_watcher`.
    _git_watcher: Option<RecommendedWatcher>,
    token: u64,
}

/// Registry of active worktree watches, keyed by `ws_id`. One per host (the app
/// holds it as managed state). Keyed so several insets (distinct workspaces) can
/// watch concurrently and detach independently.
#[derive(Default)]
pub struct WorktreeWatchState {
    watches: Mutex<HashMap<String, WorktreeWatch>>,
}

impl WorktreeWatchState {
    /// How many workspaces currently hold a live watch.
    pub fn live_watch_count(&self) -> usize {
        lock_watches(self).len()
    }
}

/// Lock the registry, recovering a poisoned mutex rather than propagating —
/// the only lock holders are the short attach/detach critical sections (the
/// notify callback never touches this lock), so a poison would be spurious and
/// must not permanently strand watchers (leak on detach) or attaches.
fn lock_watches(
    state: &WorktreeWatchState,
) -> std::sync::MutexGuard<'_, HashMap<String, WorktreeWatch>> {
    state.watches.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether `rel` (a path already made relative to the watched root) has any path
/// segment naming an excluded heavy dir.
fn has_excluded_segment(rel: &Path) -> bool {
    rel.components().any(|c| {
        c.as_os_str()
            .to_str()
            .map(|s| EXCLUDED_DIR_NAMES.contains(&s))
            .unwrap_or(false)
    })
}

/// Leading-edge coalesce: `true` for the first event on `path`, or one arriving
/// after the window since its last emit (recording `now` as that emit); `false`
/// for a repeat inside the window (collapses the macOS double-fire).
fn coalesce(debounce: &mut HashMap<PathBuf, Instant>, path: &Path, now: Instant) -> bool {
    if let Some(&last) = debounce.get(path) {
        if now.duration_since(last) < Duration::from_millis(DEBOUNCE_MS) {
            return false;
        }
    }
    debounce.insert(path.to_path_buf(), now);
    true
}

/// Bounded hygiene: drop entries older than the window so a long editing
/// session can't grow the map without limit (mirrors the context watcher's
/// debounce-map eviction). Just-inserted entries (`now`) are inside the window →
/// kept.
fn evict_expired(debounce: &mut HashMap<PathBuf, Instant>, now: Instant) {
    debounce.retain(|_, &mut t| now.duration_since(t) < Duration::from_millis(DEBOUNCE_MS));
}

/// Decide which of `event_paths` should emit, updating the per-path debounce
/// map. Kept a free function (not buried in the notify closure) so the
/// relevance + exclusion + leading-edge coalesce is deterministically
/// unit-testable without live FSEvents — the closure is a thin adapter over it.
fn classify_event(
    event_paths: &[PathBuf],
    root: &Path,
    ws_id: &str,
    debounce: &mut HashMap<PathBuf, Instant>,
    now: Instant,
) -> Vec<WorktreeFileChange> {
    let mut out = Vec::new();
    for path in event_paths {
        // Relevance in one prefix-walk: under the watched root (else skip), not
        // the root dir itself (empty rel — a bare directory event), and not
        // inside an excluded heavy-dir subtree.
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        if rel.as_os_str().is_empty() || has_excluded_segment(rel) {
            continue;
        }
        if !coalesce(debounce, path, now) {
            continue;
        }
        out.push(WorktreeFileChange {
            ws_id: ws_id.to_string(),
            // Relative suffix (see `WorktreeFileChange::changed_path`) — the FE
            // re-joins it to its own raw `ws.cwd`, so canonicalization of the
            // watched root can't desync the two path-spaces.
            changed_path: rel.to_string_lossy().into_owned(),
        });
    }
    evict_expired(debounce, now);
    out
}

/// The two git directories a worktree's ref/HEAD writes land in — resolved from
/// `cwd`. For a LINKED worktree (`git worktree add`), `<cwd>/.git` is a FILE
/// (`gitdir: <path>`) pointing at the per-worktree git dir, and the shared refs
/// live in the COMMON dir (`<git_dir>/commondir`). For a normal checkout,
/// `<cwd>/.git` is a directory that is both. Either way a commit / branch-switch
/// writes here — OUTSIDE the worktree tree for a linked worktree — so the
/// recursive file watcher above can't see it; this is where to watch instead.
struct GitWatchRoots {
    /// Per-worktree git dir — holds this worktree's own `HEAD` (branch switch).
    git_dir: PathBuf,
    /// Common git dir — holds the shared `refs/` + `packed-refs` (commit tips).
    common_dir: PathBuf,
}

fn resolve_git_watch_roots(cwd: &Path) -> Option<GitWatchRoots> {
    let dot_git = cwd.join(".git");
    let git_dir = match std::fs::metadata(&dot_git) {
        Ok(m) if m.is_dir() => dot_git.clone(),
        Ok(_) => {
            // Linked worktree: `.git` is a file "gitdir: <abs-or-rel path>".
            let content = std::fs::read_to_string(&dot_git).ok()?;
            let target = content.lines().next()?.strip_prefix("gitdir:")?.trim();
            let p = PathBuf::from(target);
            if p.is_absolute() {
                p
            } else {
                cwd.join(p)
            }
        }
        Err(_) => return None,
    };
    // `commondir` (present for a linked worktree) points at the shared git dir;
    // for a normal checkout it's absent → the git dir IS the common dir.
    let common_dir = match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(content) => {
            let rel = content.trim();
            let p = PathBuf::from(rel);
            if p.is_absolute() {
                p
            } else {
                git_dir.join(rel)
            }
        }
        Err(_) => git_dir.clone(),
    };
    Some(GitWatchRoots {
        git_dir: git_dir.canonicalize().unwrap_or(git_dir),
        common_dir: common_dir.canonicalize().unwrap_or(common_dir),
    })
}

/// Whether a changed path under a git dir means "the graph moved" — a `HEAD`
/// change (branch switch / detached), a `packed-refs` update, or a loose ref tip
/// under a `refs/` dir. INCLUDE-filter (the inverse of `has_excluded_segment`):
/// everything else in the git dir (objects, index, logs, config) is ignored.
fn is_git_ref_change(path: &Path) -> bool {
    // The reflog (`logs/HEAD`, `logs/refs/…`) mirrors ref names but is a history
    // append, not a ref move — exclude it (and it isn't in our watch roots anyway).
    if path
        .components()
        .any(|c| c.as_os_str().to_str() == Some("logs"))
    {
        return false;
    }
    if matches!(
        path.file_name().and_then(|n| n.to_str()),
        Some("HEAD") | Some("packed-refs")
    ) {
        return true;
    }
    path.components()
        .any(|c| c.as_os_str().to_str() == Some("refs"))
}

/// The git-ref counterpart of `classify_event`: INCLUDE-filtered to ref/HEAD
/// writes (root-agnostic — the git watcher has several roots), same leading-edge
/// coalesce. Its changes go to the same emit sink as file changes, so the host's
/// per-`wsId` refresh also fires on a commit or branch switch.
fn classify_git_event(
    event_paths: &[PathBuf],
    ws_id: &str,
    debounce: &mut HashMap<PathBuf, Instant>,
    now: Instant,
) -> Vec<WorktreeFileChange> {
    let mut out = Vec::new();
    for path in event_paths {
        if !is_git_ref_change(path) || !coalesce(debounce, path, now) {
            continue;
        }
        out.push(WorktreeFileChange {
            ws_id: ws_id.to_string(),
            // A short git-ref label — the FE uses it only for diagnostics (the
            // nonce drives the refetch) and it never matches an open worktree file.
            changed_path: path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("ref")
                .to_string(),
        });
    }
    evict_expired(debounce, now);
    out
}

/// A best-effort second watcher on the worktree's git ref
/// locations. A commit / branch switch on a LINKED worktree writes the common git
/// dir's `refs/` + the per-worktree `HEAD` — OUTSIDE `cwd` — so the recursive file
/// watcher can't see them. This watches just those (tightly filtered, never
/// objects/index), so commits refresh the graph live. Returns `None` (logged) on
/// any failure — the file watch still works; only commit-liveness degrades.
fn attach_git_ref_watch<E>(cwd: &Path, ws_id: String, mut emit: E) -> Option<RecommendedWatcher>
where
    E: FnMut(WorktreeFileChange) + Send + 'static,
{
    let roots = resolve_git_watch_roots(cwd)?;
    let ws_for_closure = ws_id.clone();
    let mut debounce: HashMap<PathBuf, Instant> = HashMap::new();
    let mut watcher = RecommendedWatcher::new(
        move |res: Result<notify::Event, notify::Error>| {
            let Ok(event) = res else { return };
            let now = Instant::now();
            for change in classify_git_event(&event.paths, &ws_for_closure, &mut debounce, now) {
                emit(change);
            }
        },
        Config::default(),
    )
    .map_err(|e| warn!(target: "worktree_watch", "git-ref watcher create failed for {ws_id}: {e}"))
    .ok()?;

    // refs/ subtree (recursive) — loose branch/tag tips written on commit.
    let refs_dir = roots.common_dir.join("refs");
    if let Err(e) = watcher.watch(&refs_dir, RecursiveMode::Recursive) {
        warn!(target: "worktree_watch", "git refs watch failed ({}): {e}", refs_dir.display());
    }
    // Common dir (NON-recursive) — packed-refs + the common HEAD, without
    // recursing into objects/logs/worktrees.
    if let Err(e) = watcher.watch(&roots.common_dir, RecursiveMode::NonRecursive) {
        warn!(target: "worktree_watch", "git common-dir watch failed ({}): {e}", roots.common_dir.display());
    }
    // Per-worktree git dir (NON-recursive) — this worktree's own HEAD (a branch
    // switch here); only when it differs from the common dir (a linked worktree).
    if roots.git_dir != roots.common_dir {
        if let Err(e) = watcher.watch(&roots.git_dir, RecursiveMode::NonRecursive) {
            warn!(target: "worktree_watch", "git worktree-dir watch failed ({}): {e}", roots.git_dir.display());
        }
    }
    Some(watcher)
}

/// Attach a worktree watch rooted at `cwd`, generic over the emit sink (the app
/// passes its event emit; tests substitute a closure). Mints and returns a
/// monotonic **token**; the caller must pass it to [`detach`] so a stale detach
/// (from an out-of-order `attach → detach → attach`) can't remove a newer
/// attach's watch. Attaching again for the same `ws_id` REPLACES the prior watch
/// (dropping it), so the registry always holds the latest attach.
///
/// The canonicalize + recursive kernel-watch registration is heavy on a large
/// worktree: call this off any latency-sensitive thread.
pub fn attach<E>(
    state: &WorktreeWatchState,
    ws_id: String,
    cwd: &Path,
    emit: E,
) -> Result<u64, String>
where
    E: FnMut(WorktreeFileChange) + Send + 'static,
{
    // Canonicalize so event paths (which notify reports canonicalized on macOS)
    // share the `root` prefix `classify_event`'s `strip_prefix` relies on. Do
    // this BEFORE taking the lock (a filesystem syscall shouldn't hold it).
    let root = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let root_for_closure = root.clone();
    let ws_for_closure = ws_id.clone();
    let mut debounce: HashMap<PathBuf, Instant> = HashMap::new();

    // The file watcher AND the git-ref watcher emit through the
    // same sink, so share it behind an `Arc<Mutex>` (each notify callback runs on
    // its own thread; the mutex just serializes the two emit paths).
    let emit = std::sync::Arc::new(Mutex::new(emit));
    let emit_file = move |change: WorktreeFileChange| {
        if let Ok(mut e) = emit.lock() {
            e(change);
        }
    };
    let emit_git = emit_file.clone();

    let mut watcher = RecommendedWatcher::new(
        move |res: Result<notify::Event, notify::Error>| {
            let event = match res {
                Ok(e) => e,
                Err(e) => {
                    warn!(target: "worktree_watch", "notify error event: {e}");
                    return;
                }
            };
            let now = Instant::now();
            for change in classify_event(
                &event.paths,
                &root_for_closure,
                &ws_for_closure,
                &mut debounce,
                now,
            ) {
                emit_file(change);
            }
        },
        Config::default(),
    )
    .map_err(|e| format!("failed to create worktree watcher: {e}"))?;

    watcher
        .watch(&root, RecursiveMode::Recursive)
        .map_err(|e| format!("failed to watch {}: {e}", root.display()))?;

    // Best-effort git-ref watcher for commit / branch-switch liveness;
    // its failure never fails the attach.
    let git_watcher = attach_git_ref_watch(cwd, ws_id.clone(), emit_git);

    // Mint the token WHILE holding the lock so token order and insert order
    // agree — two concurrent attaches for the same ws_id then can't leave an
    // older token as the live watch while the newer attach's token is orphaned.
    let mut watches = lock_watches(state);
    let token = WORKTREE_WATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    // Insert/replace — a prior watch for this ws_id (if any) is dropped here,
    // stopping its OS watch.
    watches.insert(
        ws_id,
        WorktreeWatch {
            _watcher: watcher,
            _git_watcher: git_watcher,
            token,
        },
    );
    Ok(token)
}

/// Detach: drop the watch for `ws_id` **only if** its live token matches
/// `token` (the token returned by the attach this detach is cleaning up). A
/// stale detach whose token no longer matches (a newer attach replaced it) is a
/// no-op — so an out-of-order `attach → detach → attach` never strands the inset
/// watcher-dead. A missing key is also a no-op (double-close / teardown safe).
pub fn detach(state: &WorktreeWatchState, ws_id: &str, token: u64) {
    let mut watches = lock_watches(state);
    if watches.get(ws_id).map(|w| w.token) == Some(token) {
        watches.remove(ws_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn relevant_file_under_root_emits() {
        let root = PathBuf::from("/ws/story");
        let mut db = HashMap::new();
        let out = classify_event(&[root.join("src/main.rs")], &root, "ws-1", &mut db, t0());
        assert_eq!(
            out,
            vec![WorktreeFileChange {
                ws_id: "ws-1".into(),
                // Relative to the watched root (canonicalization-safe routing).
                changed_path: "src/main.rs".into(),
            }]
        );
    }

    #[test]
    fn path_outside_root_is_ignored() {
        let root = PathBuf::from("/ws/story");
        let mut db = HashMap::new();
        let out = classify_event(
            &[PathBuf::from("/elsewhere/x.rs")],
            &root,
            "ws-1",
            &mut db,
            t0(),
        );
        assert!(out.is_empty());
    }

    // The git-ref watcher's INCLUDE-filter — HEAD / packed-refs
    // / a `refs/` tip are "the graph moved"; objects / index / logs are not.
    #[test]
    fn is_git_ref_change_matches_head_packed_refs_and_refs() {
        let common = PathBuf::from("/main/.git");
        assert!(is_git_ref_change(&common.join("HEAD")));
        assert!(is_git_ref_change(&common.join("packed-refs")));
        assert!(is_git_ref_change(&common.join("refs/heads/main")));
        assert!(is_git_ref_change(&common.join("refs/remotes/origin/main")));
        assert!(is_git_ref_change(&common.join("worktrees/foo/HEAD")));
    }

    #[test]
    fn is_git_ref_change_ignores_objects_index_and_logs() {
        let common = PathBuf::from("/main/.git");
        assert!(!is_git_ref_change(&common.join("objects/ab/cdef")));
        assert!(!is_git_ref_change(&common.join("index")));
        assert!(!is_git_ref_change(&common.join("logs/HEAD"))); // reflog, not a ref
        assert!(!is_git_ref_change(&common.join("config")));
        assert!(!is_git_ref_change(&common.join("COMMIT_EDITMSG")));
    }

    #[test]
    fn classify_git_event_emits_ref_writes_and_coalesces() {
        let dir = PathBuf::from("/main/.git");
        let mut db = HashMap::new();
        let now = t0();
        // A loose-ref write emits; an objects write in the same batch does not.
        let out = classify_git_event(
            &[dir.join("refs/heads/main"), dir.join("objects/ab/cd")],
            "ws-1",
            &mut db,
            now,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].changed_path, "main"); // short ref label
                                                 // A repeat within the window coalesces to nothing.
        let out2 = classify_git_event(&[dir.join("refs/heads/main")], "ws-1", &mut db, now);
        assert!(
            out2.is_empty(),
            "double-fire within the window must coalesce"
        );
    }

    #[test]
    fn resolve_git_watch_roots_normal_checkout() {
        // A normal repo: `.git` is a DIR that is both git-dir and common-dir.
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git/refs/heads")).unwrap();
        let roots = resolve_git_watch_roots(dir.path()).unwrap();
        let expected = dir.path().join(".git").canonicalize().unwrap();
        assert_eq!(roots.git_dir, expected);
        assert_eq!(roots.common_dir, expected);
    }

    #[test]
    fn resolve_git_watch_roots_linked_worktree() {
        // A linked worktree: `.git` is a FILE → per-worktree git dir under the
        // common repo, and `commondir` points back to the shared git dir.
        let common = tempdir().unwrap();
        fs::create_dir_all(common.path().join("refs/heads")).unwrap();
        let wt_git = common.path().join("worktrees/foo");
        fs::create_dir_all(&wt_git).unwrap();
        // commondir relative from `worktrees/foo` back up to the common git dir.
        fs::write(wt_git.join("commondir"), "../..\n").unwrap();
        let wt = tempdir().unwrap();
        fs::write(
            wt.path().join(".git"),
            format!("gitdir: {}\n", wt_git.display()),
        )
        .unwrap();

        let roots = resolve_git_watch_roots(wt.path()).unwrap();
        assert_eq!(roots.git_dir, wt_git.canonicalize().unwrap());
        assert_eq!(roots.common_dir, common.path().canonicalize().unwrap());
    }

    #[test]
    fn root_dir_event_empty_relative_is_ignored() {
        // A bare event on the watched root itself (empty relative path) must not
        // emit — it maps to no open file and would just churn the FE signal.
        let root = PathBuf::from("/ws/story");
        let mut db = HashMap::new();
        let out = classify_event(std::slice::from_ref(&root), &root, "ws-1", &mut db, t0());
        assert!(out.is_empty());
    }

    #[test]
    fn heavy_dirs_are_excluded() {
        let root = PathBuf::from("/ws/story");
        let mut db = HashMap::new();
        for heavy in [
            ".git/index",
            "node_modules/x/y.js",
            "target/debug/app",
            "dist/bundle.js",
        ] {
            let out = classify_event(&[root.join(heavy)], &root, "ws-1", &mut db, t0());
            assert!(out.is_empty(), "{heavy} should be excluded");
        }
        // A sibling file whose name merely CONTAINS a heavy token is NOT excluded.
        let out = classify_event(
            &[root.join("src/target_helper.rs")],
            &root,
            "ws-1",
            &mut db,
            t0(),
        );
        assert_eq!(out.len(), 1, "only whole path segments are excluded");
    }

    #[test]
    fn double_fire_within_window_coalesces_to_one() {
        let root = PathBuf::from("/ws/story");
        let mut db = HashMap::new();
        let p = root.join("a.txt");
        let base = t0();
        let first = classify_event(std::slice::from_ref(&p), &root, "ws-1", &mut db, base);
        // Same path 10 ms later — inside the 50 ms window → suppressed.
        let second = classify_event(
            std::slice::from_ref(&p),
            &root,
            "ws-1",
            &mut db,
            base + Duration::from_millis(10),
        );
        assert_eq!(first.len(), 1);
        assert!(
            second.is_empty(),
            "double-fire within the window must coalesce"
        );
    }

    #[test]
    fn same_path_after_window_emits_again() {
        let root = PathBuf::from("/ws/story");
        let mut db = HashMap::new();
        let p = root.join("a.txt");
        let base = t0();
        classify_event(std::slice::from_ref(&p), &root, "ws-1", &mut db, base);
        let later = classify_event(
            std::slice::from_ref(&p),
            &root,
            "ws-1",
            &mut db,
            base + Duration::from_millis(80),
        );
        assert_eq!(later.len(), 1, "a change after the window is a fresh emit");
    }

    #[test]
    fn attach_replaces_and_token_matched_detach_removes() {
        let dir = tempdir().unwrap();
        let state = WorktreeWatchState::default();

        let t1 = attach(&state, "ws-1".into(), dir.path(), |_| {}).unwrap();
        assert_eq!(state.live_watch_count(), 1);

        // Re-attach the same inset → REPLACES (one entry, newer token).
        let t2 = attach(&state, "ws-1".into(), dir.path(), |_| {}).unwrap();
        assert_ne!(t1, t2);
        assert_eq!(state.live_watch_count(), 1);

        // A different workspace attaches independently.
        let other = tempdir().unwrap();
        attach(&state, "ws-2".into(), other.path(), |_| {}).unwrap();
        assert_eq!(state.live_watch_count(), 2);

        // A STALE detach (old token from the replaced attach) must be a no-op —
        // this is the out-of-order `attach → detach → attach` guard.
        detach(&state, "ws-1", t1);
        assert!(lock_watches(&state).contains_key("ws-1"));

        // The matching token detaches.
        detach(&state, "ws-1", t2);
        assert!(!lock_watches(&state).contains_key("ws-1"));
        assert_eq!(state.live_watch_count(), 1);

        // Double-detach / teardown-after-close is a safe no-op.
        detach(&state, "ws-1", t2);
        assert_eq!(state.live_watch_count(), 1);
    }

    #[test]
    fn classify_emits_relative_path_from_a_real_canonical_root() {
        // Guards the attach path's canonicalize + strip_prefix contract on a real
        // FS: a file under the canonical root classifies as relevant and the emit
        // carries the RELATIVE suffix (so the FE can re-join to its raw cwd).
        let dir = tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let file = root.join("sub/hello.txt");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "hi").unwrap();
        let mut db = HashMap::new();
        let out = classify_event(std::slice::from_ref(&file), &root, "ws-1", &mut db, t0());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].changed_path, "sub/hello.txt");
    }
}
