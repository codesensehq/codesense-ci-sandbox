// SPDX-License-Identifier: MIT
use std::collections::HashMap;
use std::process::Command;
use std::sync::mpsc;

use super::*;
use crate::content_engine::WatchSetup;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_EDITOR", "true")
        .output()
        .expect("run git");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write(dir: &Path, rel: &str, text: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A repo with one commit on `main`.
fn repo() -> tempfile::TempDir {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    git(root, &["init", "-q", "-b", "main"]);
    write(root, "a.md", "one\n");
    git(root, &["add", "a.md"]);
    git(root, &["commit", "-q", "-m", "first"]);
    tmp
}

type Snapshot = HashMap<PathBuf, (u64, i128, u64)>;

/// Every entry under `dir` (files and dirs) with what tells a write: inode, mtime, size.
fn snapshot(dir: &Path, out: &mut Snapshot) {
    use std::os::unix::fs::MetadataExt;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let mtime = i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec());
        out.insert(path.clone(), (meta.ino(), mtime, meta.len()));
        if meta.is_dir() {
            snapshot(&path, out);
        }
    }
}

/// The git state of a temp repo wired to the production handlers and coalescer, with the
/// kernel's part played by replaying what a git command changed: every path under the git
/// dir it created, rewrote or removed (and the dirs those live in), dispatched as one burst.
/// That keeps these tests exact on a machine where `fseventsd` lags; the kernel wiring has its
/// own test below.
struct Probe {
    dispatcher: Arc<Dispatcher>,
    rx: mpsc::Receiver<(Instant, ContextGitMoved)>,
    root: PathBuf,
    dirs: GitDirs,
}

fn coalescer_into(tx: mpsc::Sender<(Instant, ContextGitMoved)>) -> Arc<MoveCoalescer> {
    let tx = Mutex::new(tx);
    Arc::new(MoveCoalescer::new(
        QUIET,
        MAX_GATHER,
        Arc::new(move |moved| {
            let _ = tx.lock().unwrap().send((Instant::now(), moved));
        }),
    ))
}

fn probe(root: &Path) -> Probe {
    let dispatcher = Arc::new(Dispatcher::new(50, 1_000, 60_000, 500));
    let (tx, rx) = mpsc::channel();
    let dirs = GitDirs::resolve(root).expect("a git repo");
    register_git_handlers(
        &dispatcher,
        &root.to_string_lossy(),
        &dirs,
        &coalescer_into(tx),
    );
    Probe {
        dispatcher,
        rx,
        root: root.to_path_buf(),
        dirs,
    }
}

impl Probe {
    fn snapshot(&self) -> Snapshot {
        let mut out = Snapshot::new();
        snapshot(&self.dirs.git_dir, &mut out);
        snapshot(&self.dirs.common_dir, &mut out);
        out
    }

    /// Run `act` and dispatch every path it changed under the git dirs; returns them.
    fn replay(&self, act: impl FnOnce()) -> Vec<PathBuf> {
        let before = self.snapshot();
        act();
        let after = self.snapshot();
        let mut changed: Vec<PathBuf> = after
            .iter()
            .filter(|(path, facts)| before.get(*path) != Some(facts))
            .map(|(path, _)| path.clone())
            .chain(before.keys().filter(|p| !after.contains_key(*p)).cloned())
            .collect();
        changed.sort();
        for path in &changed {
            self.dispatcher.dispatch(path);
        }
        changed
    }

    /// Run `act`, then expect exactly one event for its burst. Returns the event's kinds.
    fn one_event(&self, act: impl FnOnce()) -> Vec<GitMoveKind> {
        let changed = self.replay(act);
        let (_, moved) = self
            .rx
            .recv_timeout(Duration::from_secs(3))
            .unwrap_or_else(|_| panic!("one event for the burst of {changed:?}"));
        assert_eq!(moved.repo_path, self.root.to_string_lossy());
        let extra = self
            .rx
            .recv_timeout(MAX_GATHER + Duration::from_millis(300));
        assert!(
            extra.is_err(),
            "one event per burst, got a second: {extra:?}"
        );
        moved.kinds
    }

    /// Run `act`, then expect no event. Returns what it changed under the git dirs.
    fn no_event(&self, act: impl FnOnce()) -> Vec<PathBuf> {
        let changed = self.replay(act);
        let got = self
            .rx
            .recv_timeout(MAX_GATHER + Duration::from_millis(300));
        assert!(
            got.is_err(),
            "expected no event for {changed:?}, got {got:?}"
        );
        changed
    }
}

#[test]
fn classify_names_only_git_state() {
    let dirs = GitDirs {
        git_dir: PathBuf::from("/r/.git"),
        common_dir: PathBuf::from("/r/.git"),
    };
    let k = |p: &str| dirs.classify(Path::new(p));
    assert_eq!(k("/r/.git/HEAD"), Some(GitMoveKind::Head));
    assert_eq!(k("/r/.git/ORIG_HEAD"), Some(GitMoveKind::Head));
    assert_eq!(k("/r/.git/index"), Some(GitMoveKind::Index));
    assert_eq!(k("/r/.git/index.lock"), Some(GitMoveKind::Index));
    assert_eq!(k("/r/.git/packed-refs"), Some(GitMoveKind::Refs));
    assert_eq!(k("/r/.git/refs/heads/main"), Some(GitMoveKind::Refs));
    assert_eq!(
        k("/r/.git/refs/remotes/origin/feature/x"),
        Some(GitMoveKind::Refs)
    );
    assert_eq!(k("/r/.git/refs/tags/v1"), Some(GitMoveKind::Refs));
    assert_eq!(k("/r/.git/FETCH_HEAD"), Some(GitMoveKind::Fetch));
    assert_eq!(k("/r/.git/MERGE_HEAD"), Some(GitMoveKind::MergeState));
    assert_eq!(k("/r/.git/CHERRY_PICK_HEAD"), Some(GitMoveKind::MergeState));
    assert_eq!(k("/r/.git/rebase-merge"), Some(GitMoveKind::MergeState));
    assert_eq!(k("/r/.git/rebase-apply"), Some(GitMoveKind::MergeState));
    for noise in [
        "/r/.git/HEAD.lock",
        "/r/.git/refs/heads/main.lock",
        "/r/.git/objects/ab/cdef",
        "/r/.git/objects/pack/pack-1.pack",
        "/r/.git/logs/HEAD",
        "/r/.git/COMMIT_EDITMSG",
        "/r/.git/config",
        "/r/.git/rebase-merge/msgnum",
        "/r/notes.md",
        "/elsewhere/.git/HEAD",
    ] {
        assert_eq!(k(noise), None, "{noise}");
    }
}

#[test]
fn a_linked_worktree_reads_its_own_git_dir_and_the_shared_refs() {
    let tmp = repo();
    let main = tmp.path();
    let linked = main.parent().unwrap().join(format!(
        "{}-linked",
        main.file_name().unwrap().to_string_lossy()
    ));
    git(
        main,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "side",
            &linked.to_string_lossy(),
        ],
    );
    let dirs = GitDirs::resolve(&linked).unwrap();
    let common = std::fs::canonicalize(main.join(".git")).unwrap();
    assert_eq!(dirs.common_dir, common);
    assert_eq!(
        dirs.git_dir.parent(),
        Some(common.join("worktrees").as_path())
    );
    assert!(dirs.git_dir.join("commondir").is_file());
    assert_eq!(
        dirs.classify(&dirs.git_dir.join("HEAD")),
        Some(GitMoveKind::Head)
    );
    assert_eq!(
        dirs.classify(&common.join("refs/heads/side")),
        Some(GitMoveKind::Refs)
    );
    // The main worktree's own index is not this worktree's.
    assert_eq!(dirs.classify(&common.join("worktrees/other/index")), None);
    let watched: Vec<_> = dirs
        .watch_dirs()
        .into_iter()
        .map(|d| (d.path, d.recursive))
        .collect();
    assert_eq!(
        watched,
        vec![
            (dirs.git_dir.clone(), false),
            (common.clone(), false),
            (common.join("refs"), true),
        ]
    );
    let _ = std::fs::remove_dir_all(&linked);
}

#[test]
fn a_dot_git_file_pointing_elsewhere_resolves_to_the_real_git_dir() {
    let tmp = tempfile::TempDir::new().unwrap();
    let work = tmp.path().join("work");
    let store = tmp.path().join("store.git");
    std::fs::create_dir_all(&work).unwrap();
    git(
        tmp.path(),
        &[
            "init",
            "-q",
            "--separate-git-dir",
            &store.to_string_lossy(),
            &work.to_string_lossy(),
        ],
    );
    let dirs = GitDirs::resolve(&work).unwrap();
    assert_eq!(dirs.git_dir, std::fs::canonicalize(&store).unwrap());
    assert_eq!(dirs.common_dir, dirs.git_dir);
}

#[test]
fn a_commit_is_one_event() {
    let tmp = repo();
    let p = probe(tmp.path());
    write(&p.root, "b.md", "two\n");
    let kinds = p.one_event(|| {
        git(&p.root, &["add", "b.md"]);
        git(&p.root, &["commit", "-q", "-m", "second"]);
    });
    assert!(kinds.contains(&GitMoveKind::Refs), "{kinds:?}");
    assert!(kinds.contains(&GitMoveKind::Index), "{kinds:?}");
}

#[test]
fn a_push_moving_the_remote_ref_is_one_event() {
    let tmp = repo();
    let head = git(tmp.path(), &["rev-parse", "HEAD"]);
    let p = probe(tmp.path());
    let kinds = p.one_event(|| {
        git(&p.root, &["update-ref", "refs/remotes/origin/main", &head]);
    });
    assert_eq!(kinds, vec![GitMoveKind::Refs]);
}

#[test]
fn a_fetch_writing_fetch_head_is_one_event() {
    let tmp = repo();
    let p = probe(tmp.path());
    let kinds = p.one_event(|| {
        write(&p.root, ".git/FETCH_HEAD", "abc\t\tbranch 'main' of x\n");
    });
    assert_eq!(kinds, vec![GitMoveKind::Fetch]);
}

#[test]
fn a_checkout_is_one_event() {
    let tmp = repo();
    let p = probe(tmp.path());
    let kinds = p.one_event(|| {
        git(&p.root, &["checkout", "-q", "-b", "feature"]);
    });
    assert!(kinds.contains(&GitMoveKind::Head), "{kinds:?}");
    assert!(kinds.contains(&GitMoveKind::Refs), "{kinds:?}");
}

#[test]
fn staging_a_file_is_one_event() {
    let tmp = repo();
    write(tmp.path(), "a.md", "changed\n");
    let p = probe(tmp.path());
    let kinds = p.one_event(|| {
        git(&p.root, &["add", "a.md"]);
    });
    assert_eq!(kinds, vec![GitMoveKind::Index]);
}

#[test]
fn a_rebase_stopping_and_its_abort_are_one_event_each() {
    let tmp = repo();
    let root = tmp.path();
    git(root, &["checkout", "-q", "-b", "feature"]);
    write(root, "a.md", "feature\n");
    git(root, &["commit", "-qam", "feature"]);
    git(root, &["checkout", "-q", "main"]);
    write(root, "a.md", "main\n");
    git(root, &["commit", "-qam", "main"]);
    git(root, &["checkout", "-q", "feature"]);

    let p = probe(root);
    let kinds = p.one_event(|| {
        git(&p.root, &["rebase", "main"]);
    });
    assert!(
        root.join(".git/rebase-merge").exists(),
        "the rebase stopped on its conflict"
    );
    assert!(kinds.contains(&GitMoveKind::MergeState), "{kinds:?}");
    assert!(kinds.contains(&GitMoveKind::Head), "{kinds:?}");

    let kinds = p.one_event(|| {
        git(&p.root, &["rebase", "--abort"]);
    });
    assert!(!root.join(".git/rebase-merge").exists());
    assert!(kinds.contains(&GitMoveKind::MergeState), "{kinds:?}");
    assert!(kinds.contains(&GitMoveKind::Head), "{kinds:?}");
}

#[test]
fn objects_and_logs_writes_are_not_events() {
    let tmp = repo();
    let p = probe(tmp.path());
    let changed = p.no_event(|| {
        write(&p.root, "loose.md", "loose\n");
        git(&p.root, &["hash-object", "-w", "loose.md"]);
        write(&p.root, ".git/logs/refs/heads/scratch", "x\n");
        write(&p.root, ".git/COMMIT_EDITMSG", "msg\n");
    });
    assert!(
        changed
            .iter()
            .any(|p| p.to_string_lossy().contains("/objects/")),
        "the act wrote an object: {changed:?}"
    );
}

#[test]
fn the_apps_own_reads_write_nothing_under_git() {
    let tmp = repo();
    // A file changed since the index last saw it: what makes `git status` rewrite the index.
    write(tmp.path(), "a.md", "edit\n");
    let p = probe(tmp.path());
    let changed = p.no_event(|| {
        let root = p.root.clone();
        crate::commands::context_repo_git::context_repo_status_impl(&root).unwrap();
        crate::commands::git_sync::publish_set_impl(&root, None, None).unwrap();
        crate::commands::context_repo_git::context_repo_history_impl(
            &root,
            &crate::commands::context_repo_git::HistoryBranches::All,
            None,
            60,
            None,
        )
        .unwrap();
    });
    assert!(changed.is_empty(), "a read wrote under .git: {changed:?}");
}

/// The kernel wiring: a live watcher on the real dirs reports a git write. `fseventsd` can lag
/// or drop on a loaded machine, so the write is repeated until one comes through.
#[test]
fn the_live_watch_reports_a_git_write() {
    let tmp = repo();
    let root = tmp.path();
    let dispatcher = Arc::new(Dispatcher::new(50, 1_000, 60_000, 500));
    let watcher = Arc::new(
        ContentWatcher::new(WatchSetup { dirs: vec![] }, Arc::clone(&dispatcher)).unwrap(),
    );
    let (tx, rx) = mpsc::channel();
    let dirs = GitDirs::resolve(root).unwrap();
    attach_git_watch(
        &watcher,
        &dispatcher,
        &root.to_string_lossy(),
        &dirs,
        &coalescer_into(tx),
    );
    std::thread::sleep(Duration::from_millis(200));
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut n = 0;
    let moved = loop {
        n += 1;
        let wrote = Instant::now();
        write(root, ".git/FETCH_HEAD", &format!("{n}\n"));
        if let Ok((at, moved)) = rx.recv_timeout(Duration::from_secs(3)) {
            if moved.kinds.contains(&GitMoveKind::Fetch) {
                eprintln!("FETCH_HEAD write -> context-git-moved: {:?}", at - wrote);
                break moved;
            }
        }
        assert!(
            Instant::now() < deadline,
            "no git write came through the live watch"
        );
    };
    assert_eq!(moved.repo_path, root.to_string_lossy());
}

#[test]
fn bursts_that_never_go_quiet_still_emit_within_the_max_wait() {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let coalescer = MoveCoalescer::new(
        Duration::from_millis(100),
        Duration::from_millis(400),
        Arc::new(move |moved| {
            let _ = tx.lock().unwrap().send((Instant::now(), moved));
        }),
    );
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(600) {
        coalescer.note("/r", &[GitMoveKind::Index]);
        std::thread::sleep(Duration::from_millis(20));
    }
    coalescer.note("/r", &[GitMoveKind::Refs]);
    let (first_at, first) = rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(first_at.duration_since(start) < Duration::from_millis(550));
    assert_eq!(first.kinds, vec![GitMoveKind::Index]);
    let (_, second) = rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(second.kinds, vec![GitMoveKind::Refs, GitMoveKind::Index]);
}

#[test]
fn a_lone_change_is_emitted_one_quiet_window_later() {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let coalescer = MoveCoalescer::new(
        QUIET,
        MAX_GATHER,
        Arc::new(move |moved| {
            let _ = tx.lock().unwrap().send((Instant::now(), moved));
        }),
    );
    let start = Instant::now();
    coalescer.note("/r", &[GitMoveKind::Head]);
    let (at, moved) = rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let latency = at.duration_since(start);
    assert_eq!(moved.kinds, vec![GitMoveKind::Head]);
    assert!(
        latency >= QUIET && latency < QUIET + Duration::from_millis(200),
        "{latency:?}"
    );
}
