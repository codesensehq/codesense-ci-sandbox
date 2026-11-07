// SPDX-License-Identifier: MIT
//! Event-driven checkpoint reconciliation. The frontend watcher is active-team scoped;
//! this app-lifetime owner reuses its core watcher/state for all enabled attachments.
use crate::commands::watch::WatcherState;
use crate::content_engine::{DispatchHandler, Dispatcher, WatchDir, WatchSetup, Watcher};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::{Duration, Instant};
use tauri::Listener;

const DEBOUNCE_MS: u64 = 2000;
type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;
static OWNERS: LazyLock<Mutex<BTreeMap<PathBuf, Arc<CheckpointWatcher>>>> =
    LazyLock::new(Default::default);
#[derive(Default)]
struct Slot {
    due: Option<u64>,
    running: bool,
    pending: bool,
}
#[derive(Default)]
struct Queue {
    stopped: bool,
    refresh: bool,
    slots: BTreeMap<PathBuf, Slot>,
}

pub(crate) struct CheckpointWatcher {
    home: PathBuf,
    path: Option<OsString>,
    watches: WatcherState,
    owned: Mutex<BTreeSet<PathBuf>>,
    queue: Mutex<Queue>,
    wake: Condvar,
    clock: Clock,
    native: bool,
    errors: Mutex<usize>,
    /// The last config re-apply failure, logged once until it changes or clears.
    reapply_error: Mutex<Option<String>>,
    lifecycle: Mutex<()>,
    #[cfg(test)]
    denied: Mutex<BTreeSet<PathBuf>>,
    #[cfg(test)]
    metadata_denied: Mutex<BTreeSet<PathBuf>>,
    #[cfg(test)]
    reconciles: Mutex<usize>,
    #[cfg(test)]
    passes: Mutex<BTreeMap<PathBuf, usize>>,
}
impl CheckpointWatcher {
    fn new(home: PathBuf, path: Option<OsString>, clock: Clock, native: bool) -> Arc<Self> {
        let mut watches = WatcherState::default();
        // Debounce belongs to the clone queue, not to individual kernel event paths.
        watches.dispatcher = Arc::new(Dispatcher::new(0, 1000, 60_000, 500));
        Arc::new(Self {
            home,
            path,
            watches,
            owned: Mutex::new(BTreeSet::new()),
            queue: Mutex::new(Queue {
                refresh: true,
                ..Default::default()
            }),
            wake: Condvar::new(),
            clock,
            native,
            errors: Mutex::new(0),
            reapply_error: Mutex::new(None),
            lifecycle: Mutex::new(()),
            #[cfg(test)]
            denied: Mutex::new(BTreeSet::new()),
            #[cfg(test)]
            metadata_denied: Mutex::new(BTreeSet::new()),
            #[cfg(test)]
            reconciles: Mutex::new(0),
            #[cfg(test)]
            passes: Mutex::new(BTreeMap::new()),
        })
    }
    /// Lifecycle and sentinel events: re-register watches (which queues every clone when a
    /// new directory is attached).
    fn schedule_refresh(&self) {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        if q.stopped {
            return;
        }
        q.refresh = true;
        self.wake.notify_one();
    }
    /// Archive events: only debounce that clone's pass; one in flight plus one pending.
    fn queue_clone(&self, clone: &Path) {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        if q.stopped {
            return;
        }
        let s = q.slots.entry(clone.to_owned()).or_default();
        if s.running {
            s.pending = true;
        } else {
            s.due.get_or_insert((self.clock)() + DEBOUNCE_MS);
        }
        self.wake.notify_one();
    }
    fn record_watch_error(&self, error: impl std::fmt::Display) {
        log::warn!(target:"attribution", "checkpoint watcher: {error}");
        *self.errors.lock().unwrap_or_else(|e| e.into_inner()) += 1;
    }
    fn attach_checkpoint_dir(
        self: &Arc<Self>,
        dir: &Path,
        clones: &[PathBuf],
        children: Option<&'static [&'static str]>,
        plan: &mut BTreeMap<PathBuf, bool>,
    ) {
        if plan.contains_key(dir) {
            return;
        }
        plan.insert(dir.to_owned(), false);
        if !self.checkpoint_directory(dir) {
            return;
        }
        let dir = dir.to_owned();
        let paths = clones.to_vec();
        let weak = Arc::downgrade(self);
        let base = dir.clone();
        let handler: DispatchHandler = Arc::new(move |event| {
            // Pure path filtering and queue bookkeeping only; no IO on notify thread.
            // Dispatcher canonicalizes lookup, but hands the original event spelling
            // to this closure. Recover the watched anchor across /var-style aliases
            // using path components only; no filesystem work on the notify thread.
            let relative = std::result::Result::ok(event.strip_prefix(&base)).or_else(|| {
                std::iter::Iterator::find(&mut event.ancestors(), |p| {
                    p.file_name() == base.file_name()
                })
                .and_then(|anchor| std::result::Result::ok(event.strip_prefix(anchor)))
            });
            let child = relative
                .and_then(|path| path.components().next())
                .and_then(|c| c.as_os_str().to_str());
            let accepted = match (children, child) {
                (_, None) => true,
                (Some(names), Some(name)) => <[&str]>::is_empty(names) || names.contains(&name),
                (None, Some(name)) => name.starts_with("old-"),
            };
            if let Some(owner) = weak.upgrade().filter(|_| accepted) {
                // `children: None` marks a working_logs root; every other watch is a sentinel.
                if children.is_some() {
                    owner.schedule_refresh();
                } else {
                    for clone in &paths {
                        owner.queue_clone(clone);
                    }
                }
            }
        });
        self.watches
            .dispatcher
            .register_handler(&dir, handler.clone());
        self.watches
            .dispatcher
            .register_rescan_handler(&dir, handler);
        let result = self.watch_checkpoint_dir(&dir);
        if let Err(error) = result {
            self.watches.dispatcher.unregister_handler(&dir);
            self.record_watch_error(error);
        } else {
            plan.insert(dir, true);
        }
    }
    fn watch_checkpoint_dir(&self, dir: &Path) -> Result<(), String> {
        #[cfg(test)]
        if self.denied.lock().unwrap().contains(dir) {
            return Err("watch permission denied".into());
        }
        if !self.native {
            return Ok(());
        }
        let mut watcher = self
            .watches
            .watcher
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if watcher.is_none() {
            *watcher = Some(Arc::new(
                Watcher::new(WatchSetup { dirs: vec![] }, self.watches.dispatcher.clone())
                    .map_err(|e| e.to_string())?,
            ));
        }
        let _outcome = watcher
            .as_ref()
            .unwrap()
            .watch(WatchDir {
                path: dir.to_owned(),
                recursive: false,
            })
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    fn checkpoint_directory(&self, dir: &Path) -> bool {
        let metadata = std::fs::metadata(dir);
        #[cfg(test)]
        let metadata = if self.metadata_denied.lock().unwrap().contains(dir) {
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        } else {
            metadata
        };
        match metadata {
            Ok(meta) => meta.is_dir(),
            Err(error) if std::io::Error::kind(&error) == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                self.record_watch_error(format!(
                    "watch directory {}: {error}",
                    std::path::Path::display(dir)
                ));
                false
            }
        }
    }
    fn child_directories(&self, dir: &Path) -> Vec<PathBuf> {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if std::io::Error::kind(&error) == std::io::ErrorKind::NotFound => {
                return Vec::new()
            }
            Err(error) => {
                self.record_watch_error(format!(
                    "watch children {}: {error}",
                    std::path::Path::display(dir)
                ));
                return Vec::new();
            }
        };
        let mut children = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) => {
                    let path = std::fs::DirEntry::path(&entry);
                    if self.checkpoint_directory(&path) {
                        children.push(path);
                    }
                }
                Err(error) => self.record_watch_error(format!(
                    "watch child {}: {error}",
                    std::path::Path::display(dir)
                )),
            }
        }
        children
    }
    fn reconcile_checkpoint_watches(self: &Arc<Self>) {
        let _lifecycle = self.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
        if self.queue.lock().unwrap_or_else(|e| e.into_inner()).stopped {
            return;
        }
        #[cfg(test)]
        {
            *self.reconciles.lock().unwrap() += 1;
        }
        *self.errors.lock().unwrap_or_else(|e| e.into_inner()) = 0;
        let (mut clones, catalog_error) = super::catch_up::eligible_clones(&self.home);
        if super::detect::find_binary(&self.home, self.path.as_deref()).is_none() {
            std::vec::Vec::clear(&mut clones);
        }
        super::catch_up::retain_clone_errors(&self.home, &clones, catalog_error);
        let mut plan = BTreeMap::new();
        let mut common: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
        for (repo, _) in &clones {
            if let Some(dirs) = crate::commands::context_git_watch::GitDirs::resolve(repo) {
                common
                    .entry(dirs.common_dir)
                    .or_default()
                    .push(repo.clone());
            }
        }
        // The watch set is a deliberate superset of the log roots: git-ai creates them
        // lazily, so each container and ancestor is a sentinel watched before it is listed,
        // and a directory created mid-enumeration queues another reconciliation.
        for (git, clones) in common {
            let roots = {
                let mut list = |container: &Path| {
                    if container == git.join("worktrees") {
                        self.attach_checkpoint_dir(
                            &git,
                            &clones,
                            Some(&["ai", "worktrees"]),
                            &mut plan,
                        );
                    } else if let Some(ai) = container.parent() {
                        if let Some(admin) = ai.parent() {
                            self.attach_checkpoint_dir(
                                admin,
                                &clones,
                                Some(&["ai", "worktrees"]),
                                &mut plan,
                            );
                        }
                        self.attach_checkpoint_dir(
                            ai,
                            &clones,
                            Some(&["working_logs", "worktrees"]),
                            &mut plan,
                        );
                    }
                    self.attach_checkpoint_dir(container, &clones, Some(&[]), &mut plan);
                    self.child_directories(container)
                };
                super::checkpoints::working_log_roots(&git, &mut list)
            };
            for root in roots {
                if let Some(namespace) = root
                    .parent()
                    .filter(|parent| parent.file_name() != Some(std::ffi::OsStr::new("ai")))
                {
                    self.attach_checkpoint_dir(
                        namespace,
                        &clones,
                        Some(&["working_logs"]),
                        &mut plan,
                    );
                }
                self.attach_checkpoint_dir(&root, &clones, None, &mut plan);
            }
        }
        let wanted: BTreeSet<PathBuf> = plan
            .into_iter()
            .filter_map(|(dir, attached)| attached.then_some(dir))
            .collect();
        let mut owned = self.owned.lock().unwrap_or_else(|e| e.into_inner());
        let newly_attached = wanted.iter().any(|p| !owned.contains(p));
        for stale in owned.difference(&wanted) {
            self.watches.dispatcher.unregister_handler(stale);
            if let Some(watcher) = self.watches.watcher_handle() {
                if let Err(e) = watcher.unwatch(stale) {
                    self.record_watch_error(e);
                }
            }
        }
        *owned = wanted;
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        std::collections::BTreeMap::retain(&mut q.slots, |repo, _| {
            clones.iter().any(|(p, _)| p == repo)
        });
        if newly_attached && !q.stopped {
            // Capture after attachment closes the initial enumeration/first-archive gap.
            for (repo, _) in clones {
                q.slots
                    .entry(repo)
                    .or_default()
                    .due
                    .get_or_insert((self.clock)() + DEBOUNCE_MS);
            }
        }
        let stopped = q.stopped;
        drop(q);
        drop(owned);
        if stopped {
            self.detach_checkpoint_watches();
        }
        if !stopped {
            let errors = *self.errors.lock().unwrap_or_else(|e| e.into_inner());
            super::catch_up::record_watch_errors(&self.home, errors);
        }
    }
    fn reapply_managed_config(&self) {
        let error = match super::launch::reapply_config(&self.home) {
            super::launch::Reapplied::Failed { message }
            | super::launch::Reapplied::Unsupported { message } => Some(message),
            super::launch::Reapplied::Now | super::launch::Reapplied::Pending => None,
        };
        let mut last = Result::unwrap_or_else(Mutex::lock(&self.reapply_error), |e| {
            std::sync::PoisonError::into_inner(e)
        });
        if let Some(message) = &error {
            if Option::as_ref(&last) != Some(message) {
                log::warn!(target: "attribution", "managed git-ai config not re-applied: {message}");
            }
        }
        *last = error;
    }
    fn begin_capture(&self) -> Option<PathBuf> {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        if q.stopped {
            return None;
        }
        let now = (self.clock)();
        let (repo, slot) = std::iter::Iterator::find(&mut q.slots.iter_mut(), |(_, s)| {
            !s.running && s.due.is_some_and(|d| d <= now)
        })?;
        slot.running = true;
        slot.due = None;
        Some(repo.clone())
    }
    fn finish_capture(&self, repo: &Path) {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        let stopped = q.stopped;
        if let Some(slot) = q.slots.get_mut(repo) {
            slot.running = false;
            if slot.pending && !stopped {
                slot.due = Some((self.clock)() + DEBOUNCE_MS);
            }
            slot.pending = false;
        }
    }
    fn capture_clone(&self, repo: &Path) {
        // Re-read choice and binding immediately before import, including prefs flips.
        let (eligible, error) = super::catch_up::eligible_clones(&self.home);
        let clones: Vec<_> = eligible
            .iter()
            .filter(|(p, _)| p == repo)
            .cloned()
            .collect();
        #[cfg(test)]
        {
            *self
                .passes
                .lock()
                .unwrap()
                .entry(repo.to_owned())
                .or_default() += 1;
        }
        if std::vec::Vec::is_empty(&clones) {
            super::catch_up::retain_clone_errors(&self.home, &eligible, error);
        } else {
            super::catch_up::run_clones(
                &self.home,
                &self.home.join("usage.sqlite"),
                &clones,
                "watcher",
                error,
            );
        }
    }
    fn drive_capture(self: &Arc<Self>) -> bool {
        let refresh = {
            let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
            if q.stopped {
                return false;
            }
            std::mem::take(&mut q.refresh)
        };
        if refresh {
            // Attachments changed (or the app started): git-ai's daemon tracks only the
            // attached clones, and reloads the list without a restart.
            self.reapply_managed_config();
            self.reconcile_checkpoint_watches();
        }
        if let Some(repo) = self.begin_capture() {
            self.capture_clone(&repo);
            self.finish_capture(&repo);
            return true;
        }
        false
    }
    fn stop_capture(&self) {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.stopped = true;
        q.refresh = false;
        for slot in q.slots.values_mut() {
            slot.due = None;
            slot.pending = false;
        }
        self.wake.notify_all();
        drop(q);
        if let Ok(_lifecycle) = self.lifecycle.try_lock() {
            self.detach_checkpoint_watches();
        }
        // A registration already running sees stopped under the lifecycle guard
        // and detaches before it exits; shutdown never waits on filesystem/Git IO.
    }
    fn detach_checkpoint_watches(&self) {
        self.watches
            .watcher
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        for dir in std::mem::take(&mut *self.owned.lock().unwrap_or_else(|e| e.into_inner())) {
            self.watches.dispatcher.unregister_handler(&dir);
        }
        // Already-running import may finish its once-selected file list; per-file
        // SQLite transactions remain atomic and startup replay remains idempotent.
        // Neither stop nor app exit joins the worker or queues another pass.
    }
    fn serve_capture(self: Arc<Self>) {
        if self.queue.lock().unwrap_or_else(|e| e.into_inner()).stopped {
            return;
        }
        super::catch_up::run_at(&self.home, &self.home.join("usage.sqlite"));
        loop {
            if self.drive_capture() {
                continue;
            }
            let q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
            if q.stopped {
                break;
            }
            if q.refresh {
                continue;
            }
            let next = q.slots.values().filter_map(|s| s.due).min();
            match next {
                Some(due) => {
                    let _guard = self
                        .wake
                        .wait_timeout(q, Duration::from_millis(due.saturating_sub((self.clock)())))
                        .unwrap_or_else(|e| e.into_inner());
                }
                None => {
                    let _guard = self.wake.wait(q).unwrap_or_else(|e| e.into_inner());
                }
            }
        }
    }
}

pub(crate) fn refresh(home: &Path) {
    if let Some(owner) = OWNERS.lock().unwrap_or_else(|e| e.into_inner()).get(home) {
        owner.schedule_refresh();
    }
}
pub(crate) fn shutdown() {
    for (_, owner) in std::mem::take(&mut *OWNERS.lock().unwrap_or_else(|e| e.into_inner())) {
        owner.stop_capture();
    }
}
pub(crate) fn start<R: tauri::Runtime>(app: &tauri::AppHandle<R>, home: PathBuf) {
    let since = Instant::now();
    let owner = CheckpointWatcher::new(
        home.clone(),
        std::env::var_os("PATH"),
        Arc::new(move || since.elapsed().as_millis() as u64),
        true,
    );
    if let Some(old) = OWNERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(home, owner.clone())
    {
        old.stop_capture();
    }
    bind(app, &owner);
    tauri::async_runtime::spawn_blocking(move || owner.serve_capture());
}
fn bind<R: tauri::Runtime>(app: &tauri::AppHandle<R>, owner: &Arc<CheckpointWatcher>) {
    for topic in [
        crate::commands::worktree::WORKTREE_CHANGED_EVENT,
        crate::commands::attachments::SOURCE_ATTACHMENTS_CHANGED_EVENT,
    ] {
        let owner = Arc::downgrade(owner);
        app.listen(topic, move |_| {
            if let Some(owner) = owner.upgrade() {
                owner.schedule_refresh();
            }
        });
    }
}
#[cfg(test)]
pub(crate) mod tests;
