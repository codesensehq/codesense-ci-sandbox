// SPDX-License-Identifier: MIT
//! The session server's remote-workspace locks. There is no
//! process-wide operations lock. Each lock names exactly what it protects:
//!
//! | Lock | Key | Held by |
//! |---|---|---|
//! | repository | realpath of the managed clone | prepare (clone, fetch, import refs, `worktree add`) and repository inspection |
//! | workspace | realpath of the worktree | prepare and reconcile of that worktree, every mutating file effect, and a selected batch's Begin / Seal / Apply / Status / Finish |
//! | staging | one upload (a bundle's request key, a batch's id, an export's key) | that upload's chunk writes, its seal / prepare, and an export's build and reads |
//! | exclude (leaf) | realpath of the clone's common Git dir | one write of the managed `.git/info/exclude` block, which every worktree of the clone shares |
//! | refs (leaf) | realpath of the clone's common Git dir | each Git command that writes refs in the shared clone (prepare's fetch / `update-ref` / `worktree add`, an export's `update-ref`) |
//!
//! Order is always repository → workspace → staging, and a leaf lock is taken
//! last and held only for its one short write, so no two operations can wait
//! on each other in a cycle. In particular a 30-minute prepare never blocks
//! another worktree's exclude write: it holds the refs lock only per command.
//! Every wait gives up when the requesting connection retires. Reads take no lock: `WorkspaceHead`,
//! `WorkspaceExport`'s HEAD checks and Inspect / Read / Enumerate /
//! TrackedPaths / ReadManagedExclude see each file before or after a write
//! (writes are a staged file plus an atomic rename), and they re-read when the
//! workspace's write generation moved under them. A PTY spawn takes none of
//! these.
//!
//! Keys are realpaths, not handles, so two handles (or two spellings) that
//! reach one worktree share its lock. A path not created yet is keyed by the
//! realpath of its deepest existing ancestor plus the rest.
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

struct Table {
    held: Mutex<HashSet<String>>,
    freed: Condvar,
}

fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| Table {
        held: Mutex::new(HashSet::new()),
        freed: Condvar::new(),
    })
}

/// One held key; released on drop.
#[must_use = "the lock is released as soon as this is dropped"]
pub(crate) struct Held {
    key: String,
}

impl Drop for Held {
    fn drop(&mut self) {
        let table = table();
        table
            .held
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&self.key);
        table.freed.notify_all();
    }
}

/// How often a waiter rechecks that its connection is still alive.
const WAIT_SLICE: Duration = Duration::from_millis(100);

fn acquire(key: String, alive: &dyn Fn() -> bool) -> Result<Held, String> {
    let table = table();
    let mut held = table
        .held
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    while held.contains(&key) {
        if !alive() {
            return Err("remote workspace authority retired while waiting".into());
        }
        held = table
            .freed
            .wait_timeout(held, WAIT_SLICE)
            .unwrap_or_else(|poison| poison.into_inner())
            .0;
    }
    held.insert(key.clone());
    Ok(Held { key })
}

#[cfg(test)]
fn is_held(key: &str) -> bool {
    table()
        .held
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .contains(key)
}

/// The realpath `path` names, following its deepest existing ancestor.
pub(crate) fn canonical(path: &str) -> String {
    let mut existing = PathBuf::from(path);
    let mut rest = Vec::new();
    loop {
        if let Ok(mut real) = std::fs::canonicalize(&existing) {
            for part in rest.iter().rev() {
                real.push(part);
            }
            return real.to_string_lossy().into_owned();
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(leaf)) => {
                rest.push(leaf.to_os_string());
                existing = parent.to_path_buf();
            }
            _ => return path.to_string(),
        }
    }
}

/// The managed clone at `repo_path`.
pub(crate) fn repository(repo_path: &str, alive: &dyn Fn() -> bool) -> Result<Held, String> {
    acquire(format!("repository:{}", canonical(repo_path)), alive)
}

/// The worktree at `worktree_path`.
pub(crate) fn workspace(worktree_path: &str, alive: &dyn Fn() -> bool) -> Result<Held, String> {
    acquire(format!("workspace:{}", canonical(worktree_path)), alive)
}

/// One upload's staging, named by its own id.
pub(crate) fn staging(upload: &str, alive: &dyn Fn() -> bool) -> Result<Held, String> {
    acquire(format!("staging:{upload}"), alive)
}

/// One upload's staging if nobody holds it now; `None` while it is in use.
pub(crate) fn try_staging(upload: &str) -> Option<Held> {
    let key = format!("staging:{upload}");
    let mut held = table()
        .held
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if held.contains(&key) {
        return None;
    }
    held.insert(key.clone());
    Some(Held { key })
}

fn common_dir(repo_path: &str) -> String {
    canonical(&format!("{repo_path}/.git"))
}

/// Leaf: one write of the clone's shared exclude block.
pub(crate) fn exclude(repo_path: &str, alive: &dyn Fn() -> bool) -> Result<Held, String> {
    acquire(format!("exclude:{}", common_dir(repo_path)), alive)
}

/// Leaf: one ref-writing Git command in the shared clone.
pub(crate) fn refs(repo_path: &str, alive: &dyn Fn() -> bool) -> Result<Held, String> {
    acquire(format!("refs:{}", common_dir(repo_path)), alive)
}

fn generation(worktree_path: &str) -> Arc<AtomicU64> {
    type Generations = Mutex<HashMap<String, std::sync::Weak<AtomicU64>>>;
    static GENERATIONS: OnceLock<Generations> = OnceLock::new();
    // Resolve the key before taking the table: canonicalize does file I/O.
    let key = canonical(worktree_path);
    let mut table = GENERATIONS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if let Some(live) = table.get(&key).and_then(std::sync::Weak::upgrade) {
        return live;
    }
    // A counter lives while any reader or writer holds it, so dropping the
    // unheld ones never lets a reader and a writer use different counters.
    table.retain(|_, counter| counter.strong_count() > 0);
    let counter = Arc::new(AtomicU64::new(0));
    table.insert(key, Arc::downgrade(&counter));
    counter
}

/// A write in progress in one worktree: the generation is odd until drop.
/// Taken only under that worktree's lock, so there is one writer at a time.
pub(crate) struct Writing(Arc<AtomicU64>);

impl Drop for Writing {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Release);
    }
}

pub(crate) fn writing(worktree_path: &str) -> Writing {
    let generation = generation(worktree_path);
    generation.fetch_add(1, Ordering::AcqRel);
    Writing(generation)
}

/// How long a lock-free read keeps retrying behind writes.
const READ_DEADLINE: Duration = Duration::from_secs(5);
/// The answer when it gives up; the desktop retries on it.
pub(crate) const BUSY: &str = codesense_pty_core::remote_files::WORKSPACE_BUSY;

/// Run a lock-free `read` of the worktree so it never observes a write
/// half-done: it waits out an odd generation and re-reads when the generation
/// moved during the read, backing off (1 ms doubling to 50 ms) until
/// [`READ_DEADLINE`].
pub(crate) fn consistent<T>(
    worktree_path: &str,
    mut read: impl FnMut() -> Result<T, String>,
) -> Result<T, String> {
    let generation = generation(worktree_path);
    let deadline = std::time::Instant::now() + READ_DEADLINE;
    let mut backoff = Duration::from_millis(1);
    loop {
        let before = generation.load(Ordering::Acquire);
        if before.is_multiple_of(2) {
            let value = read();
            if generation.load(Ordering::Acquire) == before {
                return value;
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(BUSY.into());
        }
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_spellings_of_one_worktree_share_one_lock_key() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        // Not created yet: keyed through its deepest existing ancestor.
        let through_link = format!("{}/work/tree", link.display());
        let direct = format!("{}/work/tree", real.display());
        assert_eq!(canonical(&through_link), canonical(&direct));
        let held = workspace(&through_link, &|| true).unwrap();
        assert!(is_held(&format!("workspace:{}", canonical(&direct))));
        drop(held);
        assert!(!is_held(&format!("workspace:{}", canonical(&direct))));
        assert_ne!(
            canonical(&direct),
            canonical(&format!("{}/other", real.display()))
        );
    }

    #[test]
    fn a_lock_waits_only_for_its_own_key() {
        let temp = tempfile::tempdir().unwrap();
        let one = format!("{}/one", temp.path().display());
        let two = format!("{}/two", temp.path().display());
        let held = workspace(&one, &|| true).unwrap();
        // Another key is free while `one` is held.
        drop(workspace(&two, &|| true).unwrap());
        let (sent, got) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let _again = workspace(&one, &|| true).unwrap();
            sent.send(()).unwrap();
        });
        assert!(got.try_recv().is_err(), "the same key waits");
        drop(held);
        got.recv().unwrap();
        waiter.join().unwrap();
    }

    #[test]
    fn a_wait_gives_up_when_its_connection_retires() {
        let temp = tempfile::tempdir().unwrap();
        let path = format!("{}/tree", temp.path().display());
        let _held = workspace(&path, &|| true).unwrap();
        assert!(workspace(&path, &|| false).is_err());
    }

    #[test]
    fn an_unheld_generation_is_pruned_and_a_held_one_is_shared() {
        let temp = tempfile::tempdir().unwrap();
        let path = format!("{}/tree", temp.path().display());
        let reader = generation(&path);
        drop(writing(&path));
        assert_eq!(reader.load(Ordering::Acquire), 2, "one counter while held");
        drop(reader);
        assert_eq!(generation(&path).load(Ordering::Acquire), 0, "pruned");
    }

    #[test]
    fn a_read_that_overlaps_a_write_reads_again() {
        let temp = tempfile::tempdir().unwrap();
        let path = format!("{}/tree", temp.path().display());
        let mut reads = 0;
        let value = consistent(&path, || {
            reads += 1;
            if reads == 1 {
                // A whole write lands during the first read.
                drop(writing(&path));
                Ok("torn")
            } else {
                Ok("settled")
            }
        });
        assert_eq!(value, Ok("settled"));
        assert_eq!(reads, 2);
    }

    #[test]
    fn a_read_waits_out_a_write_in_progress() {
        let temp = tempfile::tempdir().unwrap();
        let path = format!("{}/tree", temp.path().display());
        let write = writing(&path);
        let (started, reading) = std::sync::mpsc::channel();
        let (finished, done) = std::sync::mpsc::channel();
        let probe = path.clone();
        let reader = std::thread::spawn(move || {
            started.send(()).unwrap();
            let value = consistent(&probe, || Ok(1));
            finished.send(value).unwrap();
        });
        reading.recv().unwrap();
        assert!(done.try_recv().is_err(), "no read while the write is open");
        drop(write);
        assert_eq!(done.recv().unwrap(), Ok(1));
        reader.join().unwrap();
    }
}
