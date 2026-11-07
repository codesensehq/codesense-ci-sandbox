// SPDX-License-Identifier: MIT
//! Completion of the existing detached record write, shared with final capture. A waiter may
//! arrive before launch bind registers its writer. Durable records remain the restart authority.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::{Duration, Instant};

pub(crate) const RECORD_WAIT_TIMEOUT: Duration = Duration::from_secs(2);

type Key = (PathBuf, String);
#[derive(Default)]
struct Completion {
    result: Option<Result<(), ()>>,
    #[cfg(test)]
    waiting: bool,
}
#[derive(Default)]
struct Pending {
    state: Mutex<Completion>,
    changed: Condvar,
}
static WRITES: LazyLock<Mutex<HashMap<Key, Arc<Pending>>>> = LazyLock::new(Default::default);

fn pending(key: &Key) -> Arc<Pending> {
    WRITES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(key.clone())
        .or_default()
        .clone()
}

fn forget_if_idle(key: &Key, slot: &Arc<Pending>) {
    let mut writes = WRITES.lock().unwrap_or_else(|e| e.into_inner());
    if writes
        .get(key)
        .is_some_and(|current| Arc::ptr_eq(current, slot))
        && Arc::strong_count(slot) == 2
    {
        writes.remove(key);
    }
}

pub(crate) struct RecordWriteGuard {
    key: Key,
    slot: Arc<Pending>,
}

/// Register before spawning the writer, including any preceding branch lookup. Dropping an
/// unfinished writer wakes capture with a failure instead of stranding it until the deadline.
pub(crate) fn begin_record_write(path: &Path, session: &str) -> RecordWriteGuard {
    let key = (path.to_owned(), session.to_owned());
    let slot = pending(&key);
    {
        let mut state = slot.state.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(state.result, Some(Err(()))) {
            state.result = None;
        }
    }
    RecordWriteGuard { key, slot }
}

impl RecordWriteGuard {
    pub(crate) fn finish(&self, success: bool) {
        let mut state = self.slot.state.lock().unwrap_or_else(|e| e.into_inner());
        // An outer launch guard can outlive the inner persistence guard. Its drop must not
        // turn an already durable record into a failed write.
        if state.result.is_none() || success {
            state.result = Some(if success { Ok(()) } else { Err(()) });
        }
        self.slot.changed.notify_all();
        let persisted = matches!(state.result, Some(Ok(())));
        drop(state);
        if persisted {
            forget_if_idle(&self.key, &self.slot);
        }
    }
}

impl Drop for RecordWriteGuard {
    fn drop(&mut self) {
        self.finish(false);
    }
}

/// A read-only lookup never initializes schemas or acquires the usage store's writer lock.
/// Its SQLite busy timeout shares the notification deadline.
fn has_record(path: &Path, session: &str, deadline: Instant) -> Result<bool, String> {
    if !path.exists() {
        return Ok(false);
    }
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| "session_record_read_failed: session record store could not be read".to_owned())?;
    conn.busy_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|e| e.to_string())?;
    // The first launch may be initializing a previously empty store. That is still pending,
    // not a failed capture; completion of its real writer is the next observation.
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'session_record')",
        [], |row| row.get(0)).map_err(|e| e.to_string())?;
    if !exists {
        return Ok(false);
    }
    conn.query_row("SELECT EXISTS(SELECT 1 FROM session_record WHERE session_id = ?1 OR native_session_id = ?1)",
        [session], |row| row.get(0)).map_err(|e| e.to_string())
}

/// Blocking only: one absolute deadline covers lookup and notification, with no schema writes,
/// SQLite connection or publication lock held across the condition-variable wait.
pub(crate) fn wait_for_record(path: &Path, session: &str) -> Result<(), String> {
    let deadline = Instant::now() + RECORD_WAIT_TIMEOUT;
    let key = (path.to_owned(), session.to_owned());
    let slot = pending(&key);
    let result = (|| {
        let state = slot.state.lock().unwrap_or_else(|e| e.into_inner());
        if has_record(path, session, deadline)? {
            return Ok(());
        }
        #[cfg(test)]
        let state = {
            let mut state = state;
            state.waiting = true;
            slot.changed.notify_all();
            state
        };
        let (state, _) = slot
            .changed
            .wait_timeout_while(
                state,
                deadline.saturating_duration_since(Instant::now()),
                |state| state.result.is_none(),
            )
            .unwrap_or_else(|e| e.into_inner());
        let completed = state.result;
        drop(state);
        if has_record(path, session, deadline)? {
            return Ok(());
        }
        Err(match completed {
            Some(Err(())) => "session_record_write_failed: session record persistence failed",
            _ => "session_record_timeout: session record was not persisted before the capture deadline",
        }.to_owned())
    })();
    forget_if_idle(&key, &slot);
    result
}

#[cfg(test)]
pub(crate) fn wait_for_record_waiter(path: &Path, session: &str) {
    let slot = pending(&(path.to_owned(), session.to_owned()));
    let state = slot.state.lock().unwrap_or_else(|e| e.into_inner());
    let (state, timeout) = slot
        .changed
        .wait_timeout_while(state, Duration::from_secs(5), |state| !state.waiting)
        .unwrap_or_else(|e| e.into_inner());
    assert!(
        state.waiting && !timeout.timed_out(),
        "capture registered its bounded wait"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pbf210_record_deadline_does_not_acquire_the_usage_store_writer_lock() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("usage.sqlite");
        crate::session_usage::UsageStore::open(&path).unwrap();
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            tx.send(wait_for_record(&path, "pbf210-locked-store"))
                .unwrap();
        });
        // The writer lock stays held beyond the full record deadline. The readiness lookup
        // must still finish; a schema-initializing UsageStore::open would wait ten seconds.
        let result = rx.recv_timeout(RECORD_WAIT_TIMEOUT + Duration::from_secs(1));
        conn.execute_batch("ROLLBACK").unwrap();
        worker.join().unwrap();
        assert!(result
            .expect("readiness respects its deadline")
            .unwrap_err()
            .starts_with("session_record_timeout"));
    }
}
