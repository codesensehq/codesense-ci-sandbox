// SPDX-License-Identifier: MIT
//! Pulls each connected machine's agent usage into this Mac's usage store: once when it connects,
//! then every [`POLL`] while it stays connected, so usage reaches this Mac within about a minute.
//! A pull with nothing new is one small read (an updated remote usually answers it without
//! reading its spool, apart from an hourly prune; an older one reads the spool each time), so the Mac no longer uses agent activity to
//! decide when to pull. That also catches what an
//! activity check misses: a turn that ran between two pulls, a session that opened and closed
//! between them, and usage the CLI exported a few seconds after its turn ended. A machine that
//! disconnects stops being watched, and the next connect starts again. The remote keeps what it has
//! not yet handed over, so a Mac that was away catches up on its next connect.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use codesense_pty_core::protocol::UsageSpoolRead;
use tauri::{AppHandle, Manager};

use super::host_connection_manager::{HostConnectionManager, HostConnectionState, ManagerEvent};
use crate::session_usage::{pull_machine, store_path, UsageStore};
use crate::terminal::session_server::UsageReadError;

/// How often a connected machine is pulled.
const POLL: Duration = Duration::from_secs(60);

/// What a watch reads from its remote.
pub(crate) trait RemoteUsage {
    /// The machine's usage spool past `after`.
    fn read(&self, after: u64, limit: usize) -> Result<UsageSpoolRead, UsageReadError>;
}

/// Watches one machine: pulls now, then once per `wait` while `connected` holds. Returns `Ok` when
/// the machine disconnects, and `Err` when its remote predates usage reads, which no later tick can
/// fix. A failed pull is retried on the next tick; it is logged when it starts failing (or fails
/// another way) and when it recovers, not on every tick.
pub(crate) fn watch(
    machine: &str,
    remote: &dyn RemoteUsage,
    store: &mut UsageStore,
    mut connected: impl FnMut() -> bool,
    mut wait: impl FnMut(),
) -> Result<(), String> {
    let mut failing: Option<String> = None;
    loop {
        match pull(store, machine, remote)? {
            None => {
                if failing.take().is_some() {
                    log::info!(target: "usage_pull", "{machine}: usage pulls work again");
                }
            }
            Some(error) => {
                if failing.as_deref() != Some(error.as_str()) {
                    log::warn!(target: "usage_pull", "{machine}: usage pull failed, retrying on the next tick: {error}");
                }
                failing = Some(error);
            }
        }
        wait();
        if !connected() {
            return Ok(());
        }
    }
}

/// One pull: `Ok(None)` when it worked, `Ok(Some(error))` when it failed and a later tick may work,
/// `Err` when the remote predates usage reads.
fn pull(
    store: &mut UsageStore,
    machine: &str,
    remote: &dyn RemoteUsage,
) -> Result<Option<String>, String> {
    let mut outdated = false;
    let result = pull_machine(store, machine, |after, limit| {
        remote.read(after, limit).map_err(|error| match error {
            UsageReadError::OutdatedServer => {
                outdated = true;
                "this machine's terminal server predates usage reads; update it".to_string()
            }
            UsageReadError::Failed(message) => message,
        })
    });
    match result {
        Ok(stored) => {
            if stored > 0 {
                log::info!(target: "usage_pull", "{machine}: stored {stored} usage events");
            }
            Ok(None)
        }
        Err(error) if outdated => Err(error),
        Err(error) => Ok(Some(error)),
    }
}

/// The machines with a watch running, so a repeated connect does not start a second one.
fn watching() -> &'static Mutex<HashSet<String>> {
    static WATCHING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    WATCHING.get_or_init(Default::default)
}

/// Starts a watch on every machine as it connects.
pub(crate) fn install(app: AppHandle) {
    let watcher_app = app.clone();
    app.state::<HostConnectionManager>()
        .subscribe(Arc::new(move |event: ManagerEvent| {
            if matches!(event.state, HostConnectionState::Connected { .. }) {
                start(watcher_app.clone(), event.machine_id);
            }
        }));
}

fn start(app: AppHandle, machine: String) {
    let fresh = watching()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(machine.clone());
    if !fresh {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name(format!("usage-pull-{machine}"))
        .spawn(move || {
            if let Err(error) = run(&app, &machine) {
                log::warn!(target: "usage_pull", "{machine}: no longer pulling usage: {error}");
            }
            watching()
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&machine);
        });
    if let Err(error) = spawned {
        log::warn!(target: "usage_pull", "could not start a usage watch: {error}");
    }
}

fn run(app: &AppHandle, machine: &str) -> Result<(), String> {
    let remote = Connected {
        app: app.clone(),
        machine: machine.to_string(),
    };
    let mut store = UsageStore::open(&store_path()?)?;
    watch(
        machine,
        &remote,
        &mut store,
        || remote.is_connected(),
        || std::thread::sleep(POLL),
    )
}

/// A machine's remote, reached through the connection manager while the machine stays connected.
struct Connected {
    app: AppHandle,
    machine: String,
}

impl Connected {
    fn client(&self) -> Result<Arc<crate::terminal::session_server::SessionServerClient>, String> {
        self.app
            .state::<HostConnectionManager>()
            .connected_backend(&self.machine)
            .map(|backend| backend.client())
            .map_err(|_| "the machine is not connected".to_string())
    }

    fn is_connected(&self) -> bool {
        self.client().is_ok()
    }
}

impl RemoteUsage for Connected {
    fn read(&self, after: u64, limit: usize) -> Result<UsageSpoolRead, UsageReadError> {
        let client = self.client().map_err(UsageReadError::Failed)?;
        client.usage_read(after, limit)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    use super::*;

    /// How a scripted read answers.
    #[derive(Clone, Copy)]
    enum Answer {
        Empty,
        Fails,
        Outdated,
    }

    /// A remote whose reads answer from a script (then `Empty`), counting them. Its spool holds
    /// nothing.
    #[derive(Default)]
    struct Counting {
        answers: RefCell<VecDeque<Answer>>,
        reads: Cell<u32>,
    }

    impl Counting {
        fn answering(answers: &[Answer]) -> Self {
            Self {
                answers: RefCell::new(answers.iter().copied().collect()),
                ..Default::default()
            }
        }
    }

    impl RemoteUsage for Counting {
        fn read(&self, _after: u64, _limit: usize) -> Result<UsageSpoolRead, UsageReadError> {
            self.reads.set(self.reads.get() + 1);
            match self
                .answers
                .borrow_mut()
                .pop_front()
                .unwrap_or(Answer::Empty)
            {
                Answer::Empty => Ok(UsageSpoolRead {
                    epoch: "e1".into(),
                    oldest_seq: None,
                    last_seq: 0,
                    records: Vec::new(),
                }),
                Answer::Fails => Err(UsageReadError::Failed("connection reset".into())),
                Answer::Outdated => Err(UsageReadError::OutdatedServer),
            }
        }
    }

    /// Runs a watch whose machine stays connected for `ticks` ticks, returning its result and how
    /// many times it waited.
    fn run_for(remote: &Counting, ticks: u32) -> (Result<(), String>, u32) {
        let (_dir, mut store) = store();
        let (checks, waits) = (Cell::new(0), Cell::new(0));
        let result = watch(
            "m1",
            remote,
            &mut store,
            || {
                checks.set(checks.get() + 1);
                checks.get() <= ticks
            },
            || waits.set(waits.get() + 1),
        );
        (result, waits.get())
    }

    fn store() -> (tempfile::TempDir, UsageStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = UsageStore::open(&dir.path().join("usage.sqlite")).unwrap();
        (dir, store)
    }

    #[test]
    fn a_connected_machine_is_pulled_on_connect_and_every_tick_until_it_disconnects() {
        let remote = Counting::default();
        let (result, waits) = run_for(&remote, 3);
        assert!(result.is_ok());
        // On connect, then after each of three waits while connected; the fourth finds it gone.
        assert_eq!((remote.reads.get(), waits), (4, 4));
    }

    #[test]
    fn a_machine_that_disconnects_at_once_is_pulled_once() {
        let remote = Counting::default();
        let (result, waits) = run_for(&remote, 0);
        assert!(result.is_ok());
        assert_eq!((remote.reads.get(), waits), (1, 1));
    }

    #[test]
    fn a_failed_pull_is_retried_on_the_next_tick() {
        let remote = Counting::answering(&[Answer::Fails, Answer::Fails, Answer::Empty]);
        let (result, _) = run_for(&remote, 3);
        assert!(result.is_ok(), "a failure does not end the watch");
        assert_eq!(remote.reads.get(), 4);
    }

    #[test]
    fn an_outdated_remote_stops_the_watch_with_a_message() {
        let remote = Counting::answering(&[Answer::Outdated]);
        let (result, _) = run_for(&remote, 5);
        assert!(result.unwrap_err().contains("predates usage reads"));
        assert_eq!(remote.reads.get(), 1);
        // Found after some pulls worked, too.
        let later = Counting::answering(&[Answer::Empty, Answer::Empty, Answer::Outdated]);
        let (result, _) = run_for(&later, 5);
        assert!(result.unwrap_err().contains("predates usage reads"));
        assert_eq!(later.reads.get(), 3);
    }
}
