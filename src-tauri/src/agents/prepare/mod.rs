// SPDX-License-Identifier: MIT
//! SW-XS-7: Step 2 — the one agent-preparation engine (SPEC §4, §11.1–§11.7).
//!
//! Every agent launch puts what its definition declares into its workspace
//! here, locally and on a machine alike:
//!
//! 1. **Want set** ([`WantSet`]): the agent-context resolver's answer
//!    (context ∪ startup + repo override − excludes), the captured startup
//!    manifest pinned first.
//! 2. **Diff** ([`diff::classify`]): locally one stat-cached look at each
//!    wanted path (the managed record is the stat cache, so an unchanged file
//!    is never read); on a machine ONE `WorkspaceManifest` call.
//! 3. **Transfer**: locally a copy-on-write clone (APFS `clonefile`, a byte
//!    copy elsewhere) hashed as it lands, then a rename; on a machine ONE
//!    batch (Begin, chunks, Seal, one Apply, Finish) carrying every write,
//!    mode change and the exclude block.
//!
//! A launch adds missing files and replaces stale unmodified managed ones. It
//! never removes, and never touches a modified, unmanaged or git-tracked file
//! (§11.1, §11.3). Only an explicit prune removes. The engine holds a
//! Step-2-scoped lock per destination for the whole run, never the wave
//! coordinator's repo lock (§11.7).
pub mod diff;
pub mod edits;
mod local;
pub mod progress;
pub mod publish;
pub mod record;
mod remote;
pub mod step3;
pub mod want;

#[cfg(test)]
pub(crate) mod tests;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use crate::content_engine::destination::{DestinationIdentity, DestinationIo};
use crate::domain::Principal;

pub use progress::{PreparePhase, PrepareProgress, PREPARE_PROGRESS_EVENT};
pub use record::{ManagedEntry, ManagedRecord, StatKey};
pub use want::{WantFile, WantSet};

use diff::{Plan, Step};
use record::Hash32;

/// Legacy lockfile rows: `(workspace-relative path, applied hash, layer)`.
pub type LegacyRows = Vec<(String, [u8; 32], String)>;

/// Where Step 2 prepares.
#[derive(Clone, Copy)]
pub enum Destination<'a> {
    /// A directory on this Mac: a git worktree, or a team-workspace dir
    /// (no `.git`: no exclude block, §11.4).
    Local { root: &'a Path },
    /// A workspace on a machine.
    Remote { io: &'a dyn DestinationIo },
}

/// SW-XS-8: what an explicit Step 3 hydrate adds to a run. A launch uses
/// the default: no overwrite, no cancel, no operation id.
#[derive(Default, Clone, Copy)]
pub struct Explicit<'a> {
    /// `hydrate_worktree {overwriteModified:true}` / "Restore pinned files":
    /// a wanted file the workspace holds differently (modified or unmanaged,
    /// never tracked) is replaced by the team's version.
    pub overwrite_modified: bool,
    /// Polled between files (locally) and between chunks (on a machine);
    /// `true` stops the run before anything more lands. A cancelled run
    /// leaves no partial file and records exactly what it applied.
    pub cancel: Option<&'a dyn Fn() -> bool>,
    /// The Step 3 operation this run belongs to, carried on every progress
    /// event.
    pub operation: Option<&'a str>,
    /// SW-XS-8 (review r1 M2): a Step 3 prune reaches only managed paths
    /// under these prefixes (the call's own scope). `None`: all of them.
    pub prune_under: Option<&'a [String]>,
}

/// One Step 2 run.
pub struct PrepareRequest<'a> {
    pub home: &'a Path,
    pub principal: &'a Principal,
    pub destination: Destination<'a>,
    pub want: &'a WantSet,
    /// `step2:<agentId>` (a launch) or `step3:<operationId>`.
    pub origin: String,
    /// For progress: the workspace and the agent being prepared.
    pub workspace: String,
    pub agent: String,
    /// The legacy lockfile's `(path, applied hash, layer)` rows: read once,
    /// on the destination's first run, to seed the managed record (§11.2).
    /// `Err` (the store is busy or unreadable) leaves the record unseeded,
    /// so the next run seeds it.
    pub legacy: &'a dyn Fn() -> Result<LegacyRows, String>,
    /// Remove unwanted unmodified managed files. Never on a launch (§11.1):
    /// only an explicit `prune` / Clean up.
    pub prune: bool,
    pub progress: &'a dyn Fn(&PrepareProgress),
    /// Re-checks the caller's authority over the destination; called right
    /// before the first write (and never between a check and its write
    /// longer than the engine needs).
    pub validate: &'a dyn Fn() -> Result<(), String>,
    /// SW-XS-8: Step 3 options (default for a launch).
    pub explicit: Explicit<'a>,
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PrepareStatus {
    /// Every wanted file is in place, or kept / skipped for a stated reason.
    Ready,
    /// At least one entry failed (§11.7): never `Ready`.
    Failed,
    /// Nothing to do here (the destination is a content root).
    Skipped,
    /// SW-XS-8: an explicit operation was cancelled; `written` lists exactly
    /// what landed before it stopped (nothing partial).
    Cancelled,
}

/// What a run did: the startup note (§8.5) and the MCP structured result.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareReport {
    pub status: PrepareStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<String>,
    /// Files the want set names.
    pub wanted: usize,
    pub wanted_bytes: u64,
    /// Files written (added or replaced).
    pub written: Vec<String>,
    pub bytes_written: u64,
    pub chmodded: Vec<String>,
    pub removed: Vec<String>,
    pub unchanged: usize,
    /// Files whose bytes were read to compare (stat-cache misses), source
    /// and destination together.
    pub hashed: usize,
    pub kept_modified: Vec<String>,
    pub kept_unmanaged: Vec<String>,
    pub skipped_tracked: Vec<String>,
    pub skipped_other: Vec<(String, String)>,
    pub failed: Vec<(String, String)>,
    /// Pinned startup files the workspace holds differently from the agent
    /// definition (edited, unmanaged, tracked, a symlink…). They never block
    /// a launch, so an agent editing its own CLAUDE.md is not locked out;
    /// they are reported and restorable explicitly.
    pub pinned_drift: Vec<String>,
    /// Unwanted managed files the old projection wrote (a prune removes them).
    pub leftovers: usize,
    pub elapsed_ms: u64,
}

impl PrepareReport {
    fn skipped(reason: String, wanted: &WantSet) -> Self {
        Self {
            status: PrepareStatus::Skipped,
            skipped_reason: Some(reason),
            wanted: wanted.files.len(),
            wanted_bytes: wanted.total_bytes(),
            written: Vec::new(),
            bytes_written: 0,
            chmodded: Vec::new(),
            removed: Vec::new(),
            unchanged: 0,
            hashed: 0,
            kept_modified: Vec::new(),
            kept_unmanaged: Vec::new(),
            skipped_tracked: Vec::new(),
            skipped_other: Vec::new(),
            failed: Vec::new(),
            pinned_drift: Vec::new(),
            leftovers: 0,
            elapsed_ms: 0,
        }
    }

    pub fn is_ready(&self) -> bool {
        matches!(self.status, PrepareStatus::Ready | PrepareStatus::Skipped)
    }

    /// The one line Step 2 hands the agent (SPEC §8.5).
    pub fn startup_note(&self, from: &str) -> String {
        progress::startup_note(self, from)
    }

    /// The error a launch reports when the run is not `Ready`.
    pub fn failure(&self) -> Option<String> {
        (self.status == PrepareStatus::Failed).then(|| {
            let shown: Vec<String> = self
                .failed
                .iter()
                .take(5)
                .map(|(path, why)| format!("{path} ({why})"))
                .collect();
            format!(
                "Couldn't prepare {} of {} agent context files: {}{}",
                self.failed.len(),
                self.wanted,
                shown.join(", "),
                if self.failed.len() > shown.len() {
                    ", …"
                } else {
                    ""
                }
            )
        })
    }
}

/// The Step-2-scoped lock of one destination: held for a whole run, so two
/// launches into one workspace never interleave. It is not the wave
/// coordinator's repo lock (§11.7).
pub(crate) fn step_lock(identity: &DestinationIdentity) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<DestinationIdentity, Arc<Mutex<()>>>>> = OnceLock::new();
    LOCKS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(identity.clone())
        .or_default()
        .clone()
}

/// The workspace id naming this destination's exclude block.
pub(crate) fn workspace_id(identity: &DestinationIdentity) -> String {
    match identity {
        DestinationIdentity::Remote(remote) => remote.workspace_handle.clone(),
        DestinationIdentity::Local { root } => {
            let hash = blake3::hash(root.to_string_lossy().as_bytes()).to_hex();
            format!("local-{}", &hash[..16])
        }
    }
}

/// How long a run waits for an in-process legacy writer or a Step 3
/// operation on the same destination to drain before giving up.
pub const ADMISSION_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Run Step 2. `Err` only when no entry could be judged at all (an
/// incomplete or refused manifest, an outdated terminal server, a captured
/// startup file that changed since the launch was prepared, an unreadable Git
/// index); a failed entry makes the report `Failed`.
///
/// The run emits `diff` / `transfer` / `apply` progress (and `failed` on an
/// `Err`); the caller emits the terminal event with [`progress::finish`]
/// once its own post-checks pass, so `ready` never precedes them.
pub fn prepare(request: PrepareRequest<'_>) -> Result<PrepareReport, String> {
    let started = Instant::now();
    let identity = match request.destination {
        Destination::Local { root } => {
            crate::content_engine::context_lifecycle::local_identity(root)
        }
        Destination::Remote { io } => io.identity().clone(),
    };
    if let Destination::Local { root } = request.destination {
        if let Some(reason) = inside_content_root(root, &request.want.source_roots) {
            return Ok(PrepareReport::skipped(reason, request.want));
        }
    }
    let lock = step_lock(&identity);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    // SW-XS-8 (review r1): an interim merged-file record XS-14 left for this
    // workspace is folded under the lock, before the record is loaded.
    if let Destination::Local { root } = request.destination {
        record::fold_generated_workspace_files(request.home, request.principal, root);
    }
    let mut run = Run::new(&request, identity);
    run.emit(PreparePhase::Diff, 0, 0, request.want.files.len() as u64);
    let result = (|| {
        // Drain any in-process legacy writer and exclude a concurrent Step 3
        // selected operation on this destination for the whole run.
        let _lease = crate::content_engine::context_lifecycle::reserve_selected_within(
            request.home,
            &run.identity,
            ADMISSION_WAIT,
        )?;
        match request.destination {
            Destination::Local { root } => local::run(&mut run, root)?,
            Destination::Remote { io } => remote::run(&mut run, io)?,
        }
        run.finish()
    })();
    match result {
        Ok(mut report) => {
            // This workspace's own block now holds what XS-14 kept in the
            // clone's shared block: drop those shared entries.
            if let (Destination::Local { root }, PrepareStatus::Ready) =
                (request.destination, report.status)
            {
                let mut record =
                    ManagedRecord::load(request.home, request.principal, &run.identity);
                if record::release_shared_excludes(&mut record, root) {
                    if let Err(error) = record.save(request.home, request.principal, &run.identity)
                    {
                        log::warn!(target: "agents", "{error}");
                    }
                }
            }
            report.elapsed_ms = started.elapsed().as_millis() as u64;
            Ok(report)
        }
        Err(error) => {
            run.emit_failed(&error);
            Err(error)
        }
    }
}

/// Register files emitted by the shared config owner under the same destination lock as Step 2.
pub(crate) fn record_generated_files(
    home: &Path,
    principal: &Principal,
    root: &Path,
    paths: &[String],
) -> Result<(), String> {
    let identity = crate::content_engine::context_lifecycle::local_identity(root);
    let lock = step_lock(&identity);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut record = ManagedRecord::load(home, principal, &identity);
    for path in paths {
        let dest = root.join(path);
        let hash = crate::content_engine::atomic_io::hash_file(&dest).map_err(|e| e.to_string())?;
        let metadata = std::fs::metadata(&dest).map_err(|e| e.to_string())?;
        record.entries.insert(
            path.clone(),
            ManagedEntry {
                source_context: None,
                hash: Hash32(hash),
                origin: "generated-config".into(),
                source_layer: want::GENERATED_LAYER.into(),
                mode: 0o644,
                dest_stat: Some(StatKey::of(&metadata)),
                source_stat: None,
                source: None,
            },
        );
    }
    record.save(home, principal, &identity)
}

/// Whether the repo at `root` tracks `path` (a destination whose index can't
/// be read reports `true`: fail closed, as Step 2 does).
pub(crate) fn tracked_in(root: &Path, path: &str) -> bool {
    local::Tracked::open(root)
        .map(|tracked| tracked.contains(path))
        .unwrap_or(true)
}

fn inside_content_root(root: &Path, source_roots: &[PathBuf]) -> Option<String> {
    let root = root.canonicalize().ok()?;
    source_roots.iter().find_map(|source| {
        let source = source.canonicalize().ok()?;
        (root == source || root.starts_with(&source)).then(|| {
            format!(
                "{} is team content itself; agents running there read it in place",
                root.display()
            )
        })
    })
}

/// The state one run carries from diff to record.
pub(crate) struct Run<'r, 'a> {
    request: &'r PrepareRequest<'a>,
    identity: DestinationIdentity,
    record: ManagedRecord,
    /// The record as loaded, to skip saving an unchanged one.
    loaded: ManagedRecord,
    /// The want set's hashes, by want index (`None`: its source is gone or
    /// not a regular file; reported per entry, never compared).
    hashes: Vec<Option<[u8; 32]>>,
    source_stats: Vec<Option<StatKey>>,
    plan: Plan,
    hashed: usize,
    /// What execution did.
    written: Vec<String>,
    bytes_written: u64,
    chmodded: Vec<String>,
    removed: Vec<String>,
    failed: Vec<(String, String)>,
    /// Local destination stats after a write or as observed.
    dest_stats: HashMap<String, StatKey>,
    /// SW-XS-8: the run stopped on the caller's cancel.
    cancelled_run: bool,
}

impl<'r, 'a> Run<'r, 'a> {
    fn new(request: &'r PrepareRequest<'a>, identity: DestinationIdentity) -> Self {
        let mut record = ManagedRecord::load(request.home, request.principal, &identity);
        // The snapshot is taken before seeding: a seed is a change to save.
        let loaded = record.clone();
        if !record.seeded {
            match (request.legacy)() {
                Ok(rows) => {
                    let seeded = record.seed(rows);
                    if seeded > 0 {
                        log::info!(
                            target: "agents",
                            "managed record for {} seeded with {seeded} file(s) from the legacy projection",
                            request.workspace
                        );
                    }
                }
                Err(error) => log::warn!(
                    target: "agents",
                    "couldn't read {}'s legacy lockfile ({error}); seeding on a later launch",
                    request.workspace
                ),
            }
        }
        Self {
            request,
            identity,
            loaded,
            record,
            hashes: Vec::new(),
            source_stats: Vec::new(),
            plan: Plan::default(),
            hashed: 0,
            written: Vec::new(),
            bytes_written: 0,
            chmodded: Vec::new(),
            removed: Vec::new(),
            failed: Vec::new(),
            dest_stats: HashMap::new(),
            cancelled_run: false,
        }
    }

    fn want(&self) -> &WantSet {
        self.request.want
    }

    fn emit(&self, phase: PreparePhase, files: u64, bytes: u64, total: u64) {
        (self.request.progress)(&PrepareProgress {
            workspace: self.request.workspace.clone(),
            agent: self.request.agent.clone(),
            phase,
            files,
            bytes,
            total,
            note: None,
            summary: None,
            operation_id: self.request.explicit.operation.map(str::to_string),
            operation_kind: None,
        });
    }

    fn emit_failed(&self, error: &str) {
        (self.request.progress)(&PrepareProgress {
            workspace: self.request.workspace.clone(),
            agent: self.request.agent.clone(),
            phase: PreparePhase::Failed,
            files: 0,
            bytes: 0,
            total: self.want().files.len() as u64,
            note: Some(error.to_string()),
            summary: None,
            operation_id: self.request.explicit.operation.map(str::to_string),
            operation_kind: None,
        });
    }

    /// SW-XS-8: whether the caller cancelled this explicit run.
    fn cancelled(&self) -> bool {
        self.request.explicit.cancel.is_some_and(|cancel| cancel())
    }

    /// Every wanted file's hash, reading only sources whose stat changed
    /// since the engine last copied them (and only trusting a stat older than
    /// the record's save by a tick, §11.6). A source that vanished or is not
    /// a regular file is skipped for that entry alone. A pinned startup file
    /// whose source changed since the launch was prepared fails the run.
    fn hash_wants(&mut self) -> Result<(), String> {
        let mut hashes = Vec::with_capacity(self.want().files.len());
        let mut stats = Vec::with_capacity(self.want().files.len());
        for file in &self.request.want.files {
            let meta = match std::fs::symlink_metadata(&file.source) {
                Ok(meta) if meta.file_type().is_file() => meta,
                Ok(_) => {
                    self.plan.skipped_other.push((
                        file.dest.clone(),
                        "its team file is not a regular file".into(),
                    ));
                    hashes.push(None);
                    stats.push(None);
                    continue;
                }
                Err(error) => {
                    self.plan.skipped_other.push((
                        file.dest.clone(),
                        format!("its team file is unavailable: {error}"),
                    ));
                    hashes.push(None);
                    stats.push(None);
                    continue;
                }
            };
            let key = StatKey::of(&meta);
            let cached = self
                .record
                .entries
                .get(&file.dest)
                .filter(|entry| {
                    entry.source.as_deref() == Some(file.source.as_path())
                        && key.trusted(entry.source_stat, self.record.saved_at_ns)
                })
                .map(|entry| entry.hash.0);
            let hash = match cached {
                Some(hash) => hash,
                None => {
                    self.hashed += 1;
                    match crate::content_engine::atomic_io::hash_file(&file.source) {
                        Ok(hash) => hash,
                        Err(error) => {
                            self.plan.skipped_other.push((
                                file.dest.clone(),
                                format!("its team file is unreadable: {error}"),
                            ));
                            hashes.push(None);
                            stats.push(None);
                            continue;
                        }
                    }
                }
            };
            if let Some(pinned) = file.pinned {
                if pinned != hash {
                    return Err(format!(
                        "newIntentRequired: startup file {} changed after the launch was prepared; start a new launch",
                        file.dest
                    ));
                }
            }
            hashes.push(Some(hash));
            stats.push(Some(key));
        }
        self.hashes = hashes;
        self.source_stats = stats;
        Ok(())
    }

    fn wanted(&self) -> Vec<diff::Wanted<'_>> {
        self.request
            .want
            .files
            .iter()
            .zip(&self.hashes)
            .enumerate()
            .filter_map(|(index, (file, hash))| {
                Some(diff::Wanted {
                    index,
                    dest: &file.dest,
                    hash: (*hash)?,
                    mode: file.mode,
                })
            })
            .collect()
    }

    /// Classify, keeping the per-entry source skips found while hashing.
    fn classify(&mut self, observe: &dyn Fn(&str) -> diff::Observation) {
        let skipped = std::mem::take(&mut self.plan.skipped_other);
        let managed = self.record.entries.clone();
        let wanted = self.wanted();
        let mut plan = diff::classify(
            &wanted,
            observe,
            &managed,
            diff::Rules {
                prune: self.request.prune,
                prune_under: self.request.explicit.prune_under,
                overwrite_modified: self.request.explicit.overwrite_modified,
            },
        );
        drop(wanted);
        let mut all = skipped;
        all.append(&mut plan.skipped_other);
        plan.skipped_other = all;
        self.plan = plan;
    }

    fn hash_of(&self, index: usize) -> [u8; 32] {
        self.hashes[index].expect("a planned entry has a hash")
    }

    /// The managed entry a successful write or adoption of want `index` leaves.
    fn entry_for(&self, index: usize) -> ManagedEntry {
        let file = &self.request.want.files[index];
        ManagedEntry {
            source_context: file.source_context.clone(),
            hash: Hash32(self.hash_of(index)),
            origin: self.request.origin.clone(),
            source_layer: file.layer.clone(),
            mode: file.mode,
            dest_stat: self.dest_stats.get(&file.dest).copied(),
            source_stat: self.source_stats[index],
            source: Some(file.source.clone()),
        }
    }

    /// The exclude block this run wants: what the block already lists
    /// (`actual`: a path the record doesn't know is never dropped), every
    /// managed path, and every planned write — minus planned removals and
    /// forgotten entries. Identical files CodeSense did not write are not
    /// added: they are not managed.
    fn exclude_desired(&self, actual: &[String]) -> Vec<String> {
        let mut paths: Vec<String> = self.record.entries.keys().cloned().collect();
        paths.extend(actual.iter().cloned());
        for (index, step) in &self.plan.steps {
            if matches!(step, Step::Add | Step::Replace { .. }) {
                paths.push(self.request.want.files[*index].dest.clone());
            }
        }
        let gone: std::collections::HashSet<&str> = self
            .plan
            .removes
            .iter()
            .map(|(path, _)| path.as_str())
            .chain(self.plan.forget.iter().map(String::as_str))
            .collect();
        paths.retain(|path| !gone.contains(path.as_str()));
        paths.sort();
        paths.dedup();
        paths
    }

    /// Whether the plan changes any file.
    fn changes_files(&self) -> bool {
        self.plan
            .steps
            .iter()
            .any(|(_, step)| !matches!(step, Step::Unchanged))
            || !self.plan.removes.is_empty()
    }

    /// The exclude block can't be maintained (hand-edited, §11.4): with file
    /// changes pending nothing is written (an unexcluded copy would show as
    /// a code change) and the run fails with the Repair action; with none,
    /// it is a warning.
    fn exclude_unusable(&mut self, reason: &str) {
        let message = format!(
            "this workspace's .git/info/exclude block was edited by hand ({reason}); Repair it (remove the block between its `codesense managed workspace` markers) and launch again"
        );
        if self.changes_files() {
            self.failed.push((".git/info/exclude".into(), message));
            for (index, step) in self.plan.steps.iter_mut() {
                let _ = index;
                *step = Step::Unchanged;
            }
            self.plan.removes.clear();
        } else {
            self.plan
                .skipped_other
                .push((".git/info/exclude".into(), message));
        }
    }

    /// Fold what ran into the managed record, save it (only when it
    /// changed), and report.
    fn finish(&mut self) -> Result<PrepareReport, String> {
        let written: std::collections::HashSet<String> = self.written.iter().cloned().collect();
        let failed: std::collections::HashSet<String> =
            self.failed.iter().map(|(path, _)| path.clone()).collect();
        let chmodded: std::collections::HashSet<String> = self.chmodded.iter().cloned().collect();
        let mut unchanged = 0;
        for (index, step) in self.plan.steps.clone() {
            let dest = self.request.want.files[index].dest.clone();
            if failed.contains(&dest) {
                continue;
            }
            match step {
                Step::Add | Step::Replace { .. } if written.contains(&dest) => {
                    let entry = self.entry_for(index);
                    self.record.entries.insert(dest, entry);
                }
                Step::Add | Step::Replace { .. } => {}
                Step::Chmod { .. } | Step::Unchanged => {
                    if step == Step::Unchanged {
                        unchanged += 1;
                    }
                    // Accept the selected source for an already managed file,
                    // even when no byte write is needed. Its origin stays the
                    // operation that first managed it; unmanaged files are
                    // never adopted.
                    let stat = self.dest_stats.get(&dest).copied();
                    let source = self.source_stats[index];
                    let file = &self.request.want.files[index];
                    let hash = self.hash_of(index);
                    if let Some(entry) = self.record.entries.get_mut(&dest) {
                        entry.hash = Hash32(hash);
                        entry.source = Some(file.source.clone());
                        entry.source_layer = file.layer.clone();
                        entry.source_context = file.source_context.clone();
                        if stat.is_some() {
                            entry.dest_stat = stat;
                        }
                        entry.source_stat = source;
                        if chmodded.contains(&dest) {
                            entry.mode = file.mode;
                        }
                    }
                }
            }
        }
        for path in &self.removed {
            self.record.entries.remove(path);
        }
        for path in &self.plan.forget {
            self.record.entries.remove(path);
        }
        let status = if self.cancelled_run {
            PrepareStatus::Cancelled
        } else if self.failed.is_empty() {
            PrepareStatus::Ready
        } else {
            PrepareStatus::Failed
        };
        // The want set's prefixes: where a later publish looks for new files
        // (SPEC §11.11).
        self.record
            .prefixes
            .extend(self.request.want.prefixes.iter().cloned());
        // An explicit operation never changes whether a launch's Step 2
        // ended ready (X5): only a launch sets it.
        if !self.request.origin.starts_with("step3:") {
            self.record.last_status = Some(
                match status {
                    PrepareStatus::Failed => "failed",
                    _ => "ready",
                }
                .into(),
            );
        }
        let mut compare = self.record.clone();
        compare.saved_at_ns = self.loaded.saved_at_ns;
        compare.version = self.loaded.version;
        if compare != self.loaded {
            self.settle_racy_window();
            self.record
                .save(self.request.home, self.request.principal, &self.identity)?;
        }
        let drift_paths: std::collections::HashSet<&str> = self
            .plan
            .kept_modified
            .iter()
            .chain(&self.plan.kept_unmanaged)
            .chain(&self.plan.skipped_tracked)
            .map(String::as_str)
            .chain(
                self.plan
                    .skipped_other
                    .iter()
                    .map(|(path, _)| path.as_str()),
            )
            .collect();
        let pinned_drift: Vec<String> = self
            .want()
            .files
            .iter()
            .filter(|file| file.pinned.is_some() && drift_paths.contains(file.dest.as_str()))
            .map(|file| file.dest.clone())
            .collect();
        Ok(PrepareReport {
            status,
            skipped_reason: None,
            wanted: self.want().files.len(),
            wanted_bytes: self.want().total_bytes(),
            written: self.written.clone(),
            bytes_written: self.bytes_written,
            chmodded: self.chmodded.clone(),
            removed: self.removed.clone(),
            unchanged,
            hashed: self.hashed,
            kept_modified: self.plan.kept_modified.clone(),
            kept_unmanaged: self.plan.kept_unmanaged.clone(),
            skipped_tracked: self.plan.skipped_tracked.clone(),
            skipped_other: self.plan.skipped_other.clone(),
            failed: self.failed.clone(),
            pinned_drift,
            leftovers: self.plan.leftovers.len(),
            elapsed_ms: 0,
        })
    }

    /// Racy-clean (§11.6): a stat is trusted only when the file is a tick
    /// older than the record's save. Wait out the tick of the newest stat
    /// this run records, so the next run can trust what this one wrote.
    fn settle_racy_window(&self) {
        let newest = self
            .record
            .entries
            .values()
            .flat_map(|entry| [entry.dest_stat, entry.source_stat])
            .flatten()
            .map(|stat| stat.mtime_ns.saturating_add(record::tick_ns(stat.mtime_ns)))
            .max();
        if let Some(until) = newest {
            let wait = until.saturating_sub(record::now_ns());
            if wait > 0 && wait <= 1_100_000_000 {
                std::thread::sleep(std::time::Duration::from_nanos(wait as u64 + 1_000));
            }
        }
    }
}

/// Tear down a local destination (SHOULD 11): run `remove` (the worktree
/// removal), and only when it succeeded drop this workspace's exclude block
/// from the clone's shared `info/exclude` and every user's managed record of
/// it. The exclude path and workspace id are resolved first, while `.git`
/// still resolves. Clearing the block before the removal would make the
/// prepared files untracked and fail a non-forced `git worktree remove`; a
/// failed removal leaves block and records intact.
pub fn teardown_local_destination(
    home: &Path,
    root: &Path,
    remove: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let identity = crate::content_engine::context_lifecycle::local_identity(root);
    let exclude = root
        .join(".git")
        .exists()
        .then(|| crate::content_engine::git_exclude::resolve_info_exclude_path(root).ok())
        .flatten();
    remove()?;
    if let Some(exclude) = exclude {
        if let Err(error) = crate::content_engine::git_exclude::replace_workspace_block_in(
            &exclude,
            &workspace_id(&identity),
            None,
            &[],
        ) {
            log::warn!(target: "agents", "couldn't remove {}'s exclude block: {error}", root.display());
        }
    }
    let Some(name) = record::record_path(home, &Principal::from(0u64), &identity)
        .file_name()
        .map(|name| name.to_os_string())
    else {
        return Ok(());
    };
    if let Ok(users) = std::fs::read_dir(home.join("users")) {
        for user in users.flatten() {
            let _ = std::fs::remove_file(user.path().join("worktrees/managed-v1").join(&name));
        }
    }
    Ok(())
}

/// The legacy lockfile's rows for a local destination: `(workspace-relative
/// path, applied hash, layer)`, for seeding the managed record (§11.2).
pub fn legacy_rows_local(
    ctx: &crate::content_engine::ResolverContext,
    root: &Path,
) -> Result<LegacyRows, String> {
    use crate::content_engine::sync_state_store::{default_store_path, SyncStateStore};
    let store =
        SyncStateStore::open(default_store_path(&ctx.codesense_home)).map_err(|e| e.to_string())?;
    legacy_rows_local_in(&store, ctx, root)
}

/// SW-XS-27: [`legacy_rows_local`] for a read-only caller: the install store
/// is opened only if it exists, never created, migrated or quarantined.
pub fn legacy_rows_local_read_only(
    ctx: &crate::content_engine::ResolverContext,
    root: &Path,
) -> Result<LegacyRows, String> {
    use crate::content_engine::sync_state_store::{default_store_path, SyncStateStore};
    match SyncStateStore::open_existing(default_store_path(&ctx.codesense_home)) {
        Some(store) => legacy_rows_local_in(&store, ctx, root),
        None => Ok(Vec::new()),
    }
}

fn legacy_rows_local_in(
    store: &crate::content_engine::sync_state_store::SyncStateStore,
    ctx: &crate::content_engine::ResolverContext,
    root: &Path,
) -> Result<LegacyRows, String> {
    use crate::content_engine::sync_state_store::TargetKey;
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let lock = match store
        .read(&TargetKey::from_ctx(ctx, root))
        .map_err(|e| e.to_string())?
    {
        Some(lock) => lock,
        None => match store
            .read(&TargetKey::from_ctx(ctx, &canonical))
            .map_err(|e| e.to_string())?
        {
            Some(lock) => lock,
            None => return Ok(Vec::new()),
        },
    };
    Ok(lock
        .entries
        .into_iter()
        .filter_map(|entry| {
            let relative = entry
                .dest_path
                .strip_prefix(root)
                .or_else(|_| entry.dest_path.strip_prefix(&canonical))
                .ok()?
                .to_str()?
                .replace('\\', "/");
            Some((
                relative,
                entry.last_materialized_hash,
                format!("legacy:{:?}", entry.source_layer),
            ))
        })
        .collect())
}

/// [`legacy_rows_local`] for a machine's workspace.
pub fn legacy_rows_remote(
    ctx: &crate::content_engine::ResolverContext,
    slug: &crate::domain::SourceRepoSlug,
    identity: &crate::content_engine::destination::RemoteDestinationIdentity,
) -> Result<LegacyRows, String> {
    use crate::content_engine::sync_state_store::{default_store_path, SyncStateStore};
    let store =
        SyncStateStore::open(default_store_path(&ctx.codesense_home)).map_err(|e| e.to_string())?;
    legacy_rows_remote_in(&store, ctx, slug, identity)
}

/// SW-XS-27: [`legacy_rows_remote`] for a read-only caller (the install store
/// is opened only if it exists, never created, migrated or quarantined).
pub fn legacy_rows_remote_read_only(
    ctx: &crate::content_engine::ResolverContext,
    slug: &crate::domain::SourceRepoSlug,
    identity: &crate::content_engine::destination::RemoteDestinationIdentity,
) -> Result<LegacyRows, String> {
    use crate::content_engine::sync_state_store::{default_store_path, SyncStateStore};
    match SyncStateStore::open_existing(default_store_path(&ctx.codesense_home)) {
        Some(store) => legacy_rows_remote_in(&store, ctx, slug, identity),
        None => Ok(Vec::new()),
    }
}

fn legacy_rows_remote_in(
    store: &crate::content_engine::sync_state_store::SyncStateStore,
    ctx: &crate::content_engine::ResolverContext,
    slug: &crate::domain::SourceRepoSlug,
    identity: &crate::content_engine::destination::RemoteDestinationIdentity,
) -> Result<LegacyRows, String> {
    use crate::content_engine::destination::DestinationReference;
    let Some(lock) = store
        .read_remote_target(ctx, slug, identity)
        .map_err(|e| e.to_string())?
    else {
        return Ok(Vec::new());
    };
    Ok(lock
        .entries
        .into_iter()
        .filter_map(|entry| match entry.dest_path {
            DestinationReference::Remote { relative, .. } => Some((
                relative.as_str().to_string(),
                entry.last_materialized_hash,
                format!("legacy:{:?}", entry.source_layer),
            )),
            _ => None,
        })
        .collect())
}
