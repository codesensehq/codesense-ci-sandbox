// SPDX-License-Identifier: MIT
//! Durable, bounded staging around the existing per-file receipt owner.
use super::*;
use codesense_pty_core::context_batch::*;
use codesense_pty_core::remote_files::{
    Effect, EffectOutcome, Expected, FileOperation, FileReply, FileRequest, FileTarget, Observation,
};
use std::collections::HashSet;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    target: FileTarget,
    batch_id: String,
    manifest_digest: [u8; 32],
    manifest: BatchManifest,
    phase: TransferPhase,
    offset: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reservation {
    initial: Record,
    ready: bool,
}
/// An upload that dispatched no effect and has not moved for
/// this long no longer holds its workspace's one active batch.
pub(super) const STAGING_TTL: Duration = Duration::from_secs(60 * 60);
fn expired(staging: &File) -> Result<bool, String> {
    older_than(staging, STAGING_TTL)
}
fn older_than(staging: &File, age: Duration) -> Result<bool, String> {
    let record = open(staging, "record.json", libc::O_RDONLY | libc::O_NONBLOCK)?;
    let modified = io(io(record.metadata())?.modified())?;
    Ok(modified.elapsed().is_ok_and(|elapsed| elapsed > age))
}
fn same_intent(record: &Record, request: &BatchRequest) -> bool {
    record.target == request.target
        && record.batch_id == request.batch_id
        && record.manifest_digest == request.manifest_digest
}
fn save_json(directory: &File, key: &str, bytes: &[u8]) -> Result<(), String> {
    let temporary = format!("{key}.next");
    // A failed rename leaves only an uncommitted checkpoint. The named record
    // remains authoritative; retry may remove this exact owned scratch file.
    if exists(directory, &temporary)? {
        let _ = read(directory, &temporary)?;
        let name = name(&temporary)?;
        // SAFETY: validated owned scratch component under held directory.
        if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err("cannot discard uncommitted batch checkpoint".into());
        }
        io(durable::sync(directory))?;
    }
    stage(directory, &temporary, bytes)?;
    let from = name(&temporary)?;
    let to = name(key)?;
    // SAFETY: validated sibling components under the same held staging directory.
    if unsafe {
        libc::renameat(
            directory.as_raw_fd(),
            from.as_ptr(),
            directory.as_raw_fd(),
            to.as_ptr(),
        )
    } != 0
    {
        return Err("cannot commit batch checkpoint".into());
    }
    io(durable::sync(directory))
}
fn save_reservation(workspace: &File, reservation: &Reservation) -> Result<(), String> {
    save_json(
        workspace,
        "active",
        &serde_json::to_vec(reservation).map_err(|_| "invalid batch reservation")?,
    )
}
fn live(alive: &dyn Fn() -> bool) -> Result<(), String> {
    if alive() {
        Ok(())
    } else {
        Err("selected batch authority retired".into())
    }
}
fn record(directory: &File) -> Result<Record, String> {
    let bytes = read(directory, "record.json")?;
    let record: Record =
        serde_json::from_slice(&bytes).map_err(|_| "selected transfer record invalid")?;
    record.manifest.validate()?;
    if record.manifest.digest()? != record.manifest_digest
        || record.offset > record.manifest.encoded_length
    {
        return Err("selected transfer record identity invalid".into());
    }
    Ok(record)
}
fn save(directory: &File, record: &Record) -> Result<(), String> {
    let bytes = serde_json::to_vec(record).map_err(|_| "cannot encode transfer record")?;
    save_json(directory, "record.json", &bytes)
}

fn payload(directory: &File, name: &str, length: u64, writable: bool) -> Result<File, String> {
    let file = open(
        directory,
        name,
        if writable {
            libc::O_RDWR
        } else {
            libc::O_RDONLY
        } | libc::O_NONBLOCK,
    )?;
    let metadata = io(file.metadata())?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() != length {
        return Err("selected transfer payload missing or corrupt".into());
    }
    Ok(file)
}
fn reserve(
    directory: &File,
    manifest: &BatchManifest,
    alive: &dyn Fn() -> bool,
) -> Result<(), String> {
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: held directory FD and valid writable stat storage.
    if unsafe { libc::fstatvfs(directory.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err("selected transfer cannot check staging headroom".into());
    }
    let stat = unsafe { stat.assume_init() };
    let free = u128::from(stat.f_bavail) * u128::from(stat.f_frsize);
    let mut outstanding = 0;
    for (name, length) in [
        ("encoded", manifest.encoded_length),
        ("decoded", manifest.raw_length),
    ] {
        let allocated = if exists(directory, name)? {
            let file = open(directory, name, libc::O_RDONLY | libc::O_NONBLOCK)?;
            let metadata = io(file.metadata())?;
            if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > length {
                return Err("unsafe incomplete batch reservation".into());
            }
            metadata.len()
        } else {
            0
        };
        outstanding += length - allocated;
    }
    // Already durable allocations are not charged twice during initialization
    // recovery. Keep separate headroom for the existing per-file JSON receipts.
    let required = u128::from(outstanding + manifest.raw_length * 2 + 1024 * 1024);
    if free < required {
        return Err("selected transfer has insufficient staging headroom".into());
    }
    for (name, length) in [
        ("encoded", manifest.encoded_length),
        ("decoded", manifest.raw_length),
    ] {
        let file = if exists(directory, name)? {
            let file = open(directory, name, libc::O_RDWR | libc::O_NONBLOCK)?;
            let metadata = io(file.metadata())?;
            if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > length {
                return Err("unsafe incomplete batch reservation".into());
            }
            if metadata.len() == length {
                continue;
            }
            file
        } else {
            open(directory, name, libc::O_RDWR | libc::O_CREAT | libc::O_EXCL)?
        };
        // Reserve actual disk blocks before accepting upload. Bounded writes also
        // work on hosts whose filesystem does not support fallocate.
        let mut writer = &file;
        let allocated = io(file.metadata())?.len();
        io(writer.seek(SeekFrom::Start(allocated)))?;
        let zeros = [0u8; 64 * 1024];
        let mut remaining = length - allocated;
        while remaining != 0 {
            live(alive)?;
            let count = remaining.min(zeros.len() as u64) as usize;
            #[cfg(test)]
            if FAIL_DURING_RESERVATION.with(|flag| flag.replace(false)) {
                io(writer.write_all(&zeros[..count / 2]))?;
                io(durable::sync(&file))?;
                return Err("injected interrupted payload reservation".into());
            }
            io(writer.write_all(&zeros[..count]))?;
            remaining -= count as u64;
        }
        io(durable::sync(&file))?;
    }
    io(durable::sync(directory))
}
fn initial_status(manifest: &BatchManifest) -> Vec<MemberResult> {
    manifest
        .items
        .iter()
        .map(|item| MemberResult {
            index: item.index,
            operation_id: item.operation_id.clone(),
            outcome: MemberOutcome::Effect {
                outcome: EffectOutcome::NotStarted {
                    operation_id: item.operation_id.clone(),
                },
            },
        })
        .collect()
}
/// The batch journal, the receipt of every write, removal,
/// mode change and directory a batch applied. Append-only segments
/// `journal-NNNNNN.json` in the batch's staging directory, one per commit;
/// a member's outcome is the first one journaled.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Segment {
    outcomes: Vec<Journaled>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journaled {
    operation_id: String,
    outcome: EffectOutcome,
}
#[derive(Default)]
struct Journal {
    outcomes: std::collections::HashMap<String, EffectOutcome>,
    /// The next segment's sequence number (the highest committed plus one).
    next: usize,
}
const JOURNAL_PREFIX: &str = "journal-";
impl Journal {
    /// The committed segments. A staged segment a cut left behind (never
    /// renamed, so never committed) is removed: the caller holds the
    /// workspace lock.
    fn load(directory: &File) -> Result<Self, String> {
        // Sequence numbers are zero-padded: name order is commit order.
        let mut names = files::children(directory)?;
        names.sort();
        let mut journal = Journal::default();
        for name in names {
            if name.starts_with(".journal-") && name.ends_with(".tmp") {
                let component = self::name(&name)?;
                // SAFETY: an owned scratch component under the held staging directory.
                unsafe { libc::unlinkat(directory.as_raw_fd(), component.as_ptr(), 0) };
                continue;
            }
            if !name.starts_with(JOURNAL_PREFIX) || !name.ends_with(".json") {
                continue;
            }
            let sequence: usize = name[JOURNAL_PREFIX.len()..name.len() - ".json".len()]
                .parse()
                .map_err(|_| "batch journal segment name invalid")?;
            let segment: Segment = serde_json::from_slice(&read(directory, &name)?)
                .map_err(|_| "batch journal segment invalid")?;
            for entry in segment.outcomes {
                journal
                    .outcomes
                    .entry(entry.operation_id)
                    .or_insert(entry.outcome);
            }
            journal.next = journal.next.max(sequence + 1);
        }
        Ok(journal)
    }
    fn get(&self, operation_id: &str) -> Option<&EffectOutcome> {
        self.outcomes.get(operation_id)
    }
    /// The entries `commit` would journal: final outcomes not yet journaled.
    fn fresh(
        &self,
        entries: impl IntoIterator<Item = (String, EffectOutcome)>,
    ) -> Vec<(String, EffectOutcome)> {
        entries
            .into_iter()
            .filter(|(id, outcome)| {
                !self.outcomes.contains_key(id)
                    && matches!(
                        outcome,
                        EffectOutcome::Applied { .. } | EffectOutcome::Conflict { .. }
                    )
            })
            .collect()
    }
    /// P3: one durable segment holding `entries` not yet journaled (final
    /// outcomes only: never `Unknown` or `NotStarted`). Staged and synced,
    /// renamed, then the directory synced: on macOS that flush is what makes
    /// every ordered write before it durable.
    fn commit(
        &mut self,
        directory: &File,
        entries: impl IntoIterator<Item = (String, EffectOutcome)>,
    ) -> Result<(), String> {
        let outcomes: Vec<Journaled> = self
            .fresh(entries)
            .into_iter()
            .map(|(operation_id, outcome)| Journaled {
                operation_id,
                outcome,
            })
            .collect::<Vec<_>>();
        if outcomes.is_empty() {
            return Ok(());
        }
        let bytes = serde_json::to_vec(&Segment { outcomes })
            .map_err(|_| "cannot encode batch journal segment")?;
        let key = format!("{JOURNAL_PREFIX}{:06}.json", self.next);
        let staged = format!(".{JOURNAL_PREFIX}{:06}.tmp", self.next);
        stage(directory, &staged, &bytes)?;
        // A crash here leaves a staged, never-committed segment.
        test_hold("journal-staged");
        let from = name(&staged)?;
        let to = name(&key)?;
        // SAFETY: validated sibling components under the held staging directory.
        if unsafe {
            libc::renameat(
                directory.as_raw_fd(),
                from.as_ptr(),
                directory.as_raw_fd(),
                to.as_ptr(),
            )
        } != 0
        {
            discard(directory, &staged);
            return Err("cannot commit batch journal".into());
        }
        // Committed under this number: a retry never renames over it.
        self.next += 1;
        io(durable::sync(directory))?;
        let segment: Segment =
            serde_json::from_slice(&bytes).map_err(|_| "batch journal segment invalid")?;
        for entry in segment.outcomes {
            self.outcomes.insert(entry.operation_id, entry.outcome);
        }
        Ok(())
    }
}
/// Everything a member's status is read from: its journaled outcome;
/// else a receipt an older server left (a batch it began, resumed here);
/// else, while the batch is applying and `check` is set, what is on disk
/// (read-only); else `NotStarted`.
struct View<'a> {
    root: &'a Path,
    owner: u64,
    record: &'a Record,
    alive: &'a dyn Fn() -> bool,
    located: &'a files::Located,
    effects: &'a files::BatchEffects,
    journal: &'a Journal,
    check: bool,
}
impl View<'_> {
    fn checking(&self) -> bool {
        self.check && self.record.phase == TransferPhase::Applying
    }
    fn not_started(&self, operation_id: &str) -> EffectOutcome {
        EffectOutcome::NotStarted {
            operation_id: operation_id.into(),
        }
    }
    fn member(&self, item: &BatchItem) -> Result<EffectOutcome, String> {
        if let Some(outcome) = self.journal.get(&item.operation_id) {
            return Ok(outcome.clone());
        }
        if self.effects.has_receipt(&item.operation_id)? {
            return files::batch_receipt(
                self.root,
                self.owner,
                &self.record.target,
                item,
                self.alive,
                self.located,
            );
        }
        if self.checking() {
            let observation = self
                .effects
                .observe(&item.path)
                .ok()
                .flatten()
                .unwrap_or(Observation::Unsafe);
            if matches!(observation, Observation::Regular { hash, mode, .. }
                if hash == item.hash && mode == item.mode)
            {
                return Ok(EffectOutcome::Applied { observation });
            }
        }
        Ok(self.not_started(&item.operation_id))
    }
    fn operation(
        &self,
        operation_id: &str,
        wanted: &FileOperation,
    ) -> Result<EffectOutcome, String> {
        if let Some(outcome) = self.journal.get(operation_id) {
            return Ok(outcome.clone());
        }
        if self.effects.has_receipt(operation_id)? {
            return files::batch_operation_receipt(
                self.root,
                self.owner,
                &self.record.target,
                operation_id,
                &|retained| retained == wanted,
                self.alive,
                self.located,
            );
        }
        if self.checking() {
            let landed = match wanted {
                FileOperation::EnsureDirectories { path, .. } => {
                    let observation = self
                        .effects
                        .observe(path)
                        .ok()
                        .flatten()
                        .unwrap_or(Observation::Unsafe);
                    (observation == Observation::Directory).then_some(observation)
                }
                FileOperation::Apply {
                    path,
                    expected,
                    effect: Effect::Remove,
                    ..
                } => {
                    let _ = expected;
                    let observation = self
                        .effects
                        .observe(path)
                        .ok()
                        .flatten()
                        .unwrap_or(Observation::Unsafe);
                    (observation == Observation::Absent).then_some(observation)
                }
                FileOperation::Apply {
                    path,
                    expected: Expected::Hash { hash: pre },
                    effect: Effect::SetMode { mode: wanted_mode },
                    ..
                } => {
                    let observation = self
                        .effects
                        .observe(path)
                        .ok()
                        .flatten()
                        .unwrap_or(Observation::Unsafe);
                    matches!(observation, Observation::Regular { hash, mode, .. }
                        if hash == *pre && mode == *wanted_mode)
                    .then_some(observation)
                }
                _ => None,
            };
            if let Some(observation) = landed {
                return Ok(EffectOutcome::Applied { observation });
            }
        }
        Ok(self.not_started(operation_id))
    }
}
fn receipts(view: &View) -> Result<Vec<MemberResult>, String> {
    let mut result = initial_status(&view.record.manifest);
    for (item, member) in result.iter_mut().zip(&view.record.manifest.items) {
        live(view.alive)?;
        item.outcome = MemberOutcome::Effect {
            outcome: view.member(member)?,
        };
    }
    mark_blocked(&view.record.manifest, &mut result);
    Ok(result)
}
fn successful(outcome: &MemberOutcome) -> bool {
    matches!(
        outcome,
        MemberOutcome::Effect {
            outcome: EffectOutcome::Applied { .. }
        }
    )
}
fn mark_blocked(manifest: &BatchManifest, results: &mut [MemberResult]) {
    // `results` is in member order (one per member).
    let succeeded: HashSet<usize> = results
        .iter()
        .filter(|result| successful(&result.outcome))
        .map(|result| result.index)
        .collect();
    for (item, result) in manifest.items.iter().zip(results.iter_mut()) {
        let blocked: Vec<_> = item
            .dependencies
            .iter()
            .copied()
            .filter(|index| !succeeded.contains(index))
            .collect();
        if !blocked.is_empty() && is_not_started(&result.outcome) {
            result.outcome = MemberOutcome::Blocked {
                dependencies: blocked,
            };
        }
    }
}
/// The extension effects of a batch as file operations, in apply
/// order after the writes: directories, then mode changes, then removals.
fn dir_operation(item: &DirItem) -> FileOperation {
    FileOperation::EnsureDirectories {
        operation_id: item.operation_id.clone(),
        path: item.path.clone(),
    }
}
fn chmod_operation(item: &ChmodItem) -> FileOperation {
    FileOperation::Apply {
        operation_id: item.operation_id.clone(),
        path: item.path.clone(),
        expected: Expected::Hash {
            hash: item.expected_pre_hash,
        },
        effect: Effect::SetMode { mode: item.mode },
    }
}
fn remove_operation(item: &RemoveItem) -> FileOperation {
    FileOperation::Apply {
        operation_id: item.operation_id.clone(),
        path: item.path.clone(),
        expected: Expected::Hash {
            hash: item.expected_pre_hash,
        },
        effect: Effect::Remove,
    }
}
fn union(left: &[String], right: &[String]) -> Vec<String> {
    let mut all: Vec<String> = left.iter().chain(right).cloned().collect();
    all.sort();
    all.dedup();
    all
}
/// The exclude block's first write, before any file is renamed into place:
/// everything it listed plus every path this batch adds. Its ID is
/// derived from the block's own, so a retry never allocates a new identity.
fn exclude_add_operation(
    target: &FileTarget,
    block: &ManagedExcludeBlock,
) -> Result<FileOperation, String> {
    let identity =
        serde_json::to_vec(&("selected-batch-exclude-add-v1", target, &block.operation_id))
            .map_err(|_| "invalid exclude intent")?;
    let digest = blake3::hash(&identity);
    let mut id = [0u8; 16];
    id.copy_from_slice(&digest.as_bytes()[..16]);
    Ok(FileOperation::ReplaceWorkspaceExclude {
        operation_id: uuid::Uuid::from_bytes(id).to_string(),
        workspace_id: block.workspace_id.clone(),
        expected: block.expected.clone(),
        paths: union(&block.expected, &block.paths),
        repair: block.repair,
    })
}
/// The block's final write, after the removals: the wanted paths, minus a new
/// path whose write failed (a user's file there must not become ignored),
/// plus a path whose removal failed (the file is still there).
fn exclude_final_paths(
    manifest: &BatchManifest,
    block: &ManagedExcludeBlock,
    items: &[MemberResult],
    operations: &[OperationResult],
) -> Vec<String> {
    let expected: HashSet<&str> = block.expected.iter().map(String::as_str).collect();
    let mut dropped = HashSet::new();
    for (item, result) in manifest.items.iter().zip(items) {
        if !successful(&result.outcome) && !expected.contains(item.path.as_str()) {
            dropped.insert(item.path.as_str());
        }
    }
    let mut paths: Vec<String> = block
        .paths
        .iter()
        .filter(|path| !dropped.contains(path.as_str()))
        .cloned()
        .collect();
    for result in operations {
        if result.kind == OperationKind::Remove && !successful(&result.outcome) {
            let path = &manifest.removes[result.index].path;
            if expected.contains(path.as_str()) {
                paths.push(path.clone());
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}
fn not_started(operation_id: &str) -> MemberOutcome {
    MemberOutcome::Effect {
        outcome: EffectOutcome::NotStarted {
            operation_id: operation_id.into(),
        },
    }
}
fn is_not_started(outcome: &MemberOutcome) -> bool {
    matches!(
        outcome,
        MemberOutcome::Effect {
            outcome: EffectOutcome::NotStarted { .. }
        }
    )
}
fn initial_operations(manifest: &BatchManifest) -> Vec<OperationResult> {
    let mut operations = Vec::new();
    let mut push = |kind, index, operation_id: &str| {
        operations.push(OperationResult {
            kind,
            index,
            operation_id: operation_id.into(),
            outcome: not_started(operation_id),
        })
    };
    for (index, item) in manifest.ensure_dirs.iter().enumerate() {
        push(OperationKind::EnsureDir, index, &item.operation_id);
    }
    for (index, item) in manifest.chmods.iter().enumerate() {
        push(OperationKind::Chmod, index, &item.operation_id);
    }
    for (index, item) in manifest.removes.iter().enumerate() {
        push(OperationKind::Remove, index, &item.operation_id);
    }
    if let Some(block) = &manifest.exclude_block {
        push(OperationKind::ExcludeBlock, 0, &block.operation_id);
    }
    operations
}
/// The retained outcome of the exclude block's first write, if it has one.
fn exclude_add_receipt(
    root: &Path,
    owner: u64,
    record: &Record,
    alive: &dyn Fn() -> bool,
    located: &files::Located,
) -> Result<Option<EffectOutcome>, String> {
    let Some(block) = &record.manifest.exclude_block else {
        return Ok(None);
    };
    let add = exclude_add_operation(&record.target, block)?;
    let FileOperation::ReplaceWorkspaceExclude { operation_id, .. } = &add else {
        unreachable!("the add is a workspace exclude write")
    };
    files::batch_operation_receipt(
        root,
        owner,
        &record.target,
        operation_id,
        &|retained| *retained == add,
        alive,
        located,
    )
    .map(Some)
}
fn operation_receipts(view: &View) -> Result<Vec<OperationResult>, String> {
    let manifest = &view.record.manifest;
    let mut operations = initial_operations(manifest);
    for result in &mut operations {
        live(view.alive)?;
        let outcome = match result.kind {
            OperationKind::EnsureDir => view.operation(
                &result.operation_id,
                &dir_operation(&manifest.ensure_dirs[result.index]),
            )?,
            OperationKind::Chmod => view.operation(
                &result.operation_id,
                &chmod_operation(&manifest.chmods[result.index]),
            )?,
            OperationKind::Remove => view.operation(
                &result.operation_id,
                &remove_operation(&manifest.removes[result.index]),
            )?,
            OperationKind::ExcludeBlock => {
                let block = manifest
                    .exclude_block
                    .as_ref()
                    .ok_or("missing exclude block")?;
                // Its paths depend on the other outcomes; its identity, block
                // and compare-and-swap base do not.
                let base = union(&block.expected, &block.paths);
                files::batch_operation_receipt(
                    view.root,
                    view.owner,
                    &view.record.target,
                    &result.operation_id,
                    &|retained| {
                        matches!(retained, FileOperation::ReplaceWorkspaceExclude {
                            operation_id, workspace_id, expected, ..
                        } if operation_id == &block.operation_id
                            && workspace_id == &block.workspace_id
                            && *expected == base)
                    },
                    view.alive,
                    view.located,
                )?
            }
        };
        result.outcome = MemberOutcome::Effect { outcome };
    }
    Ok(operations)
}
const EXCLUDE_CHANGED: &str =
    "the workspace exclude block changed since the diff; nothing was written";
/// When the exclude block's first write lost its compare-and-swap, nothing
/// else in the batch may run: an unexcluded copy would show as a code change.
fn skip_after_exclude_conflict(
    add: Option<&EffectOutcome>,
    items: &mut [MemberResult],
    operations: &mut [OperationResult],
) {
    if !matches!(add, Some(EffectOutcome::Conflict { .. })) {
        return;
    }
    for item in items.iter_mut() {
        if is_not_started(&item.outcome) || matches!(item.outcome, MemberOutcome::Blocked { .. }) {
            item.outcome = MemberOutcome::Skipped {
                reason: EXCLUDE_CHANGED.into(),
            };
        }
    }
    for operation in operations.iter_mut() {
        if operation.kind == OperationKind::ExcludeBlock {
            operation.outcome = MemberOutcome::Effect {
                outcome: add.cloned().expect("conflict above"),
            };
        } else if operation.kind != OperationKind::EnsureDir && is_not_started(&operation.outcome) {
            operation.outcome = MemberOutcome::Skipped {
                reason: EXCLUDE_CHANGED.into(),
            };
        }
    }
}
/// A batch member's journaled outcome, for a per-operation
/// Reconcile of its ID (batch members have no ledger receipts). Searches the
/// workspace's retained batches. The caller holds the workspace lock.
pub(super) fn journaled(
    ledger: &File,
    owner: u64,
    target: &FileTarget,
    operation_id: &str,
) -> Result<Option<EffectOutcome>, String> {
    if !exists(ledger, "context-batches")? {
        return Ok(None);
    }
    let all = directory(ledger, "context-batches", false)?;
    let workspace_key =
        digest(format!("{owner}:{}:{}", target.origin_key, target.handle).as_bytes());
    if !exists(&all, &workspace_key)? {
        return Ok(None);
    }
    let workspace = directory(&all, &workspace_key, false)?;
    for name in files::children(&workspace)? {
        if uuid::Uuid::parse_str(&name).is_err() {
            continue;
        }
        // An unreadable batch or journal is an error, never a silent "not
        // started".
        let staging = directory(&workspace, &name, false)?;
        let journal = Journal::load(&staging)?;
        if let Some(outcome) = journal.get(operation_id) {
            return Ok(Some(outcome.clone()));
        }
    }
    Ok(None)
}
/// Every member's and extension's retained outcome, as Status reports it.
fn status_in(view: &View) -> Result<(Vec<MemberResult>, Vec<OperationResult>), String> {
    let mut items = receipts(view)?;
    let mut operations = operation_receipts(view)?;
    let add = exclude_add_receipt(view.root, view.owner, view.record, view.alive, view.located)?;
    skip_after_exclude_conflict(add.as_ref(), &mut items, &mut operations);
    Ok((items, operations))
}
/// [`status_in`] for Status, Finish and a superseding Begin. While the batch
/// is applying, an outcome found on disk (a cut landed it before its journal
/// commit) is journaled now, so the proof survives later edits.
fn status(
    root: &Path,
    owner: u64,
    directory: &File,
    record: &Record,
    alive: &dyn Fn() -> bool,
    located: &files::Located,
) -> Result<(Vec<MemberResult>, Vec<OperationResult>), String> {
    let mut effects = files::BatchEffects::open(root, owner, &record.target, located, alive)?;
    let mut journal = Journal::load(directory)?;
    let found = status_in(&View {
        root,
        owner,
        record,
        alive,
        located,
        effects: &effects,
        journal: &journal,
        check: true,
    })?;
    if record.phase == TransferPhase::Applying {
        let fresh = journal.fresh(journal_entries(&found.0, &found.1));
        if !fresh.is_empty() {
            let paths = member_paths(&record.manifest);
            for (operation_id, outcome) in &fresh {
                if let (EffectOutcome::Applied { .. }, Some(path)) =
                    (outcome, paths.get(operation_id.as_str()))
                {
                    effects.settle(path)?;
                }
            }
            effects.seal()?;
            journal.commit(directory, fresh)?;
        }
    }
    Ok(found)
}
/// Every journaled member's and extension's path, by operation ID.
fn member_paths(manifest: &BatchManifest) -> std::collections::HashMap<&str, &str> {
    let items = manifest
        .items
        .iter()
        .map(|item| (item.operation_id.as_str(), item.path.as_str()));
    let dirs = manifest
        .ensure_dirs
        .iter()
        .map(|item| (item.operation_id.as_str(), item.path.as_str()));
    let chmods = manifest
        .chmods
        .iter()
        .map(|item| (item.operation_id.as_str(), item.path.as_str()));
    let removes = manifest
        .removes
        .iter()
        .map(|item| (item.operation_id.as_str(), item.path.as_str()));
    items.chain(dirs).chain(chmods).chain(removes).collect()
}
/// The member and extension outcomes a journal records (never the exclude
/// block's: it keeps its own receipts).
fn journal_entries(
    items: &[MemberResult],
    operations: &[OperationResult],
) -> Vec<(String, EffectOutcome)> {
    let effect = |outcome: &MemberOutcome| match outcome {
        MemberOutcome::Effect { outcome } => Some(outcome.clone()),
        _ => None,
    };
    items
        .iter()
        .filter_map(|item| Some((item.operation_id.clone(), effect(&item.outcome)?)))
        .chain(
            operations
                .iter()
                .filter(|operation| operation.kind != OperationKind::ExcludeBlock)
                .filter_map(|operation| {
                    Some((operation.operation_id.clone(), effect(&operation.outcome)?))
                }),
        )
        .collect()
}
fn is_settled(outcome: &MemberOutcome) -> bool {
    matches!(
        outcome,
        MemberOutcome::Effect {
            outcome: EffectOutcome::Applied { .. } | EffectOutcome::Conflict { .. }
        } | MemberOutcome::Skipped { .. }
    )
}
fn chunk(
    directory: &File,
    record: &mut Record,
    offset: u64,
    bytes: &[u8],
    alive: &dyn Fn() -> bool,
) -> Result<(), String> {
    if record.phase != TransferPhase::Uploading || bytes.is_empty() || bytes.len() > CHUNK_BYTES {
        return Err("invalid selected transfer chunk or phase".into());
    }
    let end = offset
        .checked_add(bytes.len() as u64)
        .ok_or("chunk range overflow")?;
    if end > record.manifest.encoded_length
        || offset > record.offset
        || (offset < record.offset && end > record.offset)
    {
        return Err("selected transfer chunk is gapped or partially overlapping".into());
    }
    let mut file = payload(directory, "encoded", record.manifest.encoded_length, true)?;
    io(file.seek(SeekFrom::Start(offset)))?;
    if offset < record.offset {
        let mut prior = vec![0; bytes.len()];
        io(file.read_exact(&mut prior))?;
        if prior != bytes {
            return Err("duplicate transfer chunk differs".into());
        }
        return Ok(());
    }
    live(alive)?;
    io(file.write_all(bytes))?;
    io(durable::sync(&file))?;
    live(alive)?;
    record.offset = end;
    save(directory, record)
}
fn hash_file(file: &mut File, length: u64, alive: &dyn Fn() -> bool) -> Result<[u8; 32], String> {
    io(file.seek(SeekFrom::Start(0)))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0;
    loop {
        live(alive)?;
        let count = io(file.read(&mut buffer))?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > length {
            return Err("payload exceeds declared length".into());
        }
        hasher.update(&buffer[..count]);
    }
    if total != length {
        return Err("payload is truncated".into());
    }
    Ok(*hasher.finalize().as_bytes())
}
fn seal(directory: &File, record: &mut Record, alive: &dyn Fn() -> bool) -> Result<(), String> {
    if matches!(
        record.phase,
        TransferPhase::Verified | TransferPhase::Applying
    ) {
        return Ok(());
    }
    if record.phase != TransferPhase::Uploading || record.offset != record.manifest.encoded_length {
        return Err("selected transfer is not fully uploaded".into());
    }
    let manifest = &record.manifest;
    let mut encoded = payload(directory, "encoded", manifest.encoded_length, false)?;
    if hash_file(&mut encoded, manifest.encoded_length, alive)? != manifest.encoded_digest {
        return Err("encoded payload digest mismatch".into());
    }
    io(encoded.seek(SeekFrom::Start(0)))?;
    let mut decoded = payload(directory, "decoded", manifest.raw_length, true)?;
    let mut source = std::io::BufReader::new(encoded);
    match manifest.codec {
        Codec::Identity => {
            decode_into(&mut source, &mut decoded, manifest.raw_length, alive)?;
        }
        Codec::Gzip => {
            let mut gzip = flate2::bufread::GzDecoder::new(&mut source);
            decode_into(&mut gzip, &mut decoded, manifest.raw_length, alive)?;
            drop(gzip);
            use std::io::BufRead;
            if !io(source.fill_buf())?.is_empty() {
                return Err("trailing gzip member or data".into());
            }
        }
        Codec::Zstd => {
            zstd_frame(&mut source, manifest.raw_length)?;
            io(source.seek(SeekFrom::Start(0)))?;
            let mut zstd = ruzstd::decoding::StreamingDecoder::new(&mut source)
                .map_err(|_| "invalid zstd frame")?;
            decode_into(&mut zstd, &mut decoded, manifest.raw_length, alive)?;
            drop(zstd);
            use std::io::BufRead;
            if !io(source.fill_buf())?.is_empty() {
                return Err("trailing zstd frame or data".into());
            }
        }
    }
    io(durable::sync(&decoded))?;
    if hash_file(&mut decoded, manifest.raw_length, alive)? != manifest.raw_digest {
        return Err("decoded payload digest mismatch".into());
    }
    for item in &manifest.items {
        live(alive)?;
        io(decoded.seek(SeekFrom::Start(item.offset)))?;
        let mut bytes = vec![0; item.length as usize];
        io(decoded.read_exact(&mut bytes))?;
        if *blake3::hash(&bytes).as_bytes() != item.hash {
            return Err("batch member digest mismatch".into());
        }
    }
    record.phase = TransferPhase::Verified;
    save(directory, record)
}
/// The largest zstd window a batch may ask the host to hold.
const ZSTD_MAX_WINDOW: u64 = 16 * 1024 * 1024;
/// Check a zstd payload's one frame header before decoding anything: a
/// regular frame (never skippable), no dictionary, a content size that is
/// present and equals the manifest's raw length, and a window of at most
/// [`ZSTD_MAX_WINDOW`] (for a single-segment frame, its content size).
fn zstd_frame(source: &mut impl Read, raw_length: u64) -> Result<(), String> {
    // RFC 8878 section 3.1.1: magic, frame header descriptor, then the optional
    // window descriptor, dictionary ID and frame content size fields.
    let mut fixed = [0u8; 5];
    source
        .read_exact(&mut fixed)
        .map_err(|_| "zstd payload is truncated")?;
    if u32::from_le_bytes([fixed[0], fixed[1], fixed[2], fixed[3]]) != 0xFD2F_B528 {
        return Err("zstd payload is not one regular frame".into());
    }
    let descriptor = fixed[4];
    let single_segment = descriptor & 0x20 != 0;
    if descriptor & 0x08 != 0 {
        return Err("invalid zstd frame descriptor".into());
    }
    let window_descriptor = if single_segment {
        None
    } else {
        let mut byte = [0u8; 1];
        source
            .read_exact(&mut byte)
            .map_err(|_| "zstd payload is truncated")?;
        Some(byte[0])
    };
    let dictionary_bytes = [0, 1, 2, 4][usize::from(descriptor & 0x03)];
    let mut dictionary = [0u8; 4];
    source
        .read_exact(&mut dictionary[..dictionary_bytes])
        .map_err(|_| "zstd payload is truncated")?;
    if u32::from_le_bytes(dictionary) != 0 {
        return Err("zstd payload needs a dictionary".into());
    }
    let content_bytes = match descriptor >> 6 {
        0 if single_segment => 1,
        0 => 0,
        1 => 2,
        2 => 4,
        _ => 8,
    };
    if content_bytes == 0 {
        return Err("zstd frame does not record its content size".into());
    }
    let mut content = [0u8; 8];
    source
        .read_exact(&mut content[..content_bytes])
        .map_err(|_| "zstd payload is truncated")?;
    let mut content_size = u64::from_le_bytes(content);
    if content_bytes == 2 {
        content_size += 256;
    }
    if content_size != raw_length {
        return Err("zstd frame content size differs from the manifest".into());
    }
    let window = match window_descriptor {
        None => content_size,
        Some(byte) => {
            let base = 1u64 << (10 + u64::from(byte >> 3));
            base + (base / 8) * u64::from(byte & 0x07)
        }
    };
    if window > ZSTD_MAX_WINDOW {
        return Err("zstd frame window exceeds the host's limit".into());
    }
    Ok(())
}
fn decode_into(
    reader: &mut dyn Read,
    writer: &mut File,
    length: u64,
    alive: &dyn Fn() -> bool,
) -> Result<(), String> {
    // 30 s, plus one second per 8 MiB: the deadline scales with the batch.
    let deadline = Instant::now() + Duration::from_secs(30 + length / (8 * 1024 * 1024));
    let mut total = 0;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        live(alive)?;
        if Instant::now() >= deadline {
            return Err("selected decode deadline exceeded".into());
        }
        let count = io(reader.read(&mut buffer))?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > length || total > STREAMED_MAX_RAW_BYTES {
            return Err("selected decode exceeds declared capacity".into());
        }
        io(writer.write_all(&buffer[..count]))?;
    }
    if total != length {
        return Err("selected decoded payload is truncated".into());
    }
    Ok(())
}
fn apply(
    root: &Path,
    owner: u64,
    directory: &File,
    record: &mut Record,
    alive: &dyn Fn() -> bool,
    located: &files::Located,
) -> Result<(Vec<MemberResult>, Vec<OperationResult>), String> {
    if !matches!(
        record.phase,
        TransferPhase::Verified | TransferPhase::Applying
    ) {
        return Err("selected transfer is not verified or is already closed".into());
    }
    let mut decoded = payload(directory, "decoded", record.manifest.raw_length, false)?;
    if hash_file(&mut decoded, record.manifest.raw_length, alive)? != record.manifest.raw_digest {
        return Err("sealed payload changed".into());
    }
    // P0: dispatched, durably, before any effect (an Applying batch can no
    // longer be abandoned).
    if record.phase != TransferPhase::Applying {
        record.phase = TransferPhase::Applying;
        save(directory, record)?;
    } else {
        // A resumed batch is in use: its staging TTL restarts, as it did when
        // every Apply re-saved the record.
        let file = open(directory, "record.json", libc::O_WRONLY | libc::O_NONBLOCK)?;
        io(file.set_modified(std::time::SystemTime::now()))?;
    }
    let mut journal = Journal::load(directory)?;
    let mut effects = files::BatchEffects::open(root, owner, &record.target, located, alive)?;
    // Idempotent by operation ID: every journaled (or, for a batch an older
    // server began, receipted) outcome is reported as it was; everything else
    // is evaluated now by content, so what already landed is never rewritten.
    let (mut results, mut operations) = status_in(&View {
        root,
        owner,
        record,
        alive,
        located,
        effects: &effects,
        journal: &journal,
        check: false,
    })?;
    let manifest = record.manifest.clone();
    let target = record.target.clone();
    let mut exclude_lost = false;
    let ran = (|| -> Result<(), String> {
        // 1. Directories.
        for operation in operations
            .iter_mut()
            .filter(|operation| operation.kind == OperationKind::EnsureDir)
        {
            if is_not_started(&operation.outcome) {
                live(alive)?;
                let item = &manifest.ensure_dirs[operation.index];
                operation.outcome = MemberOutcome::Effect {
                    outcome: effects.ensure_directory(&item.operation_id, &item.path)?,
                };
            }
        }
        // 2. The exclude block lists every added path before any file lands
        //    (a receipted effect of its own).
        if let Some(block) = &manifest.exclude_block {
            let added = match exclude_add_receipt(root, owner, record, alive, located)? {
                Some(EffectOutcome::NotStarted { .. }) => {
                    live(alive)?;
                    let _leaf = locks::exclude(&located.repo_path, alive)?;
                    files::batch_effect(
                        root,
                        owner,
                        FileRequest {
                            target: target.clone(),
                            operation: exclude_add_operation(&target, block)?,
                        },
                        alive,
                        located,
                    )?
                }
                Some(retained) => retained,
                None => unreachable!("a batch with a block has its first write"),
            };
            match added {
                EffectOutcome::Applied { .. } => {}
                EffectOutcome::Conflict { .. } => {
                    skip_after_exclude_conflict(Some(&added), &mut results, &mut operations);
                    exclude_lost = true;
                    return Ok(());
                }
                _ => return Err("workspace exclude outcome unknown; reconcile with Status".into()),
            }
        }
        // 3. On a case-insensitive filesystem `README.md` and `Readme.md` are
        //    one file: a removal that shares a write's case-folded name runs
        //    first, and is journaled before any write (a redo must never see
        //    the write's bytes as this removal's pre-hash conflict).
        let folded: HashSet<String> = manifest
            .items
            .iter()
            .map(|item| item.path.to_lowercase())
            .collect();
        let writes: HashSet<&str> = manifest
            .items
            .iter()
            .map(|item| item.path.as_str())
            .collect();
        let mut folded_ran = false;
        for operation in operations
            .iter_mut()
            .filter(|operation| operation.kind == OperationKind::Remove)
        {
            let remove = &manifest.removes[operation.index];
            if is_not_started(&operation.outcome)
                && folded.contains(&remove.path.to_lowercase())
                && !writes.contains(remove.path.as_str())
            {
                live(alive)?;
                operation.outcome = MemberOutcome::Effect {
                    outcome: effects.effect(
                        &remove.operation_id,
                        &remove.path,
                        &Expected::Hash {
                            hash: remove.expected_pre_hash,
                        },
                        &Effect::Remove,
                        alive,
                    )?,
                };
                folded_ran = true;
            }
        }
        if folded_ran {
            effects.seal()?;
            journal.commit(directory, journal_entries(&results, &operations))?;
        }
        // 4. The writes, dependencies first.
        let mut succeeded: HashSet<usize> = results
            .iter()
            .filter(|result| successful(&result.outcome))
            .map(|result| result.index)
            .collect();
        for at in manifest.apply_order()? {
            let item = &manifest.items[at];
            if !is_not_started(&results[at].outcome)
                && !matches!(results[at].outcome, MemberOutcome::Blocked { .. })
            {
                continue;
            }
            let blocked: Vec<_> = item
                .dependencies
                .iter()
                .copied()
                .filter(|index| !succeeded.contains(index))
                .collect();
            if !blocked.is_empty() {
                results[at].outcome = MemberOutcome::Blocked {
                    dependencies: blocked,
                };
                continue;
            }
            live(alive)?;
            io(decoded.seek(SeekFrom::Start(item.offset)))?;
            let mut bytes = vec![0; item.length as usize];
            io(decoded.read_exact(&mut bytes))?;
            if *blake3::hash(&bytes).as_bytes() != item.hash {
                return Err("sealed member changed".into());
            }
            let outcome = effects.effect(
                &item.operation_id,
                &item.path,
                &item.expected,
                &Effect::Write {
                    bytes,
                    mode: Some(item.mode),
                },
                alive,
            )?;
            if matches!(outcome, EffectOutcome::Applied { .. }) {
                succeeded.insert(item.index);
            }
            results[at].outcome = MemberOutcome::Effect { outcome };
            test_hold("between-members");
        }
        // 5. Mode changes, then 6. removals, each only while its pre-hash holds.
        for kind in [OperationKind::Chmod, OperationKind::Remove] {
            for operation in operations
                .iter_mut()
                .filter(|operation| operation.kind == kind)
            {
                if !is_not_started(&operation.outcome) {
                    continue;
                }
                live(alive)?;
                let FileOperation::Apply {
                    operation_id,
                    path,
                    expected,
                    effect,
                } = (match kind {
                    OperationKind::Chmod => chmod_operation(&manifest.chmods[operation.index]),
                    _ => remove_operation(&manifest.removes[operation.index]),
                })
                else {
                    unreachable!("chmods and removals are file effects")
                };
                operation.outcome = MemberOutcome::Effect {
                    outcome: effects.effect(&operation_id, &path, &expected, &effect, alive)?,
                };
            }
        }
        Ok(())
    })();
    // P2 then P3, whatever stopped P1: what landed is made durable, then
    // journaled. A failed sync journals nothing.
    test_hold("before-seal");
    let committed = effects.seal().and_then(|()| {
        test_hold("after-seal");
        journal.commit(directory, journal_entries(&results, &operations))
    });
    // The effect error that stopped P1 is the one to report.
    ran?;
    committed?;
    // 7. The block's final write, after removals, from journaled outcomes.
    if let (Some(block), false) = (&manifest.exclude_block, exclude_lost) {
        let position = operations
            .iter()
            .position(|operation| operation.kind == OperationKind::ExcludeBlock)
            .ok_or("missing exclude result")?;
        // Only once every write and removal has a final outcome: a member
        // still unknown would land later outside the block it was left out of.
        let pending = |outcome: &MemberOutcome| {
            matches!(
                outcome,
                MemberOutcome::Effect {
                    outcome: EffectOutcome::Unknown { .. } | EffectOutcome::NotStarted { .. }
                }
            )
        };
        let unsettled = results.iter().any(|result| pending(&result.outcome))
            || operations.iter().any(|operation| {
                operation.kind != OperationKind::ExcludeBlock && pending(&operation.outcome)
            });
        if is_not_started(&operations[position].outcome) && !unsettled {
            live(alive)?;
            let paths = exclude_final_paths(&manifest, block, &results, &operations);
            let _leaf = locks::exclude(&located.repo_path, alive)?;
            let outcome = files::batch_effect(
                root,
                owner,
                FileRequest {
                    target: target.clone(),
                    operation: FileOperation::ReplaceWorkspaceExclude {
                        operation_id: block.operation_id.clone(),
                        workspace_id: block.workspace_id.clone(),
                        expected: union(&block.expected, &block.paths),
                        paths,
                        repair: false,
                    },
                },
                alive,
                located,
            )?;
            operations[position].outcome = MemberOutcome::Effect { outcome };
        }
    }
    Ok((results, operations))
}
fn cleanup(directory: &File) -> Result<(), String> {
    for leaf in ["encoded", "decoded"] {
        if exists(directory, leaf)? {
            let component = name(leaf)?;
            // SAFETY: fixed owned payload name under a held staging directory.
            if unsafe { libc::unlinkat(directory.as_raw_fd(), component.as_ptr(), 0) } != 0 {
                return Err("selected transfer cleanup pending".into());
            }
        }
    }
    io(durable::sync(directory))
}
fn closed(phase: TransferPhase) -> bool {
    matches!(phase, TransferPhase::Finished | TransferPhase::Abandoned)
}
fn finish(
    root: &Path,
    owner: u64,
    directory: &File,
    record: &mut Record,
    disposition: FinishDisposition,
    alive: &dyn Fn() -> bool,
    located: &files::Located,
) -> Result<(), String> {
    let wanted = match disposition {
        FinishDisposition::Settled => TransferPhase::Finished,
        FinishDisposition::Abandon => TransferPhase::Abandoned,
    };
    if closed(record.phase) {
        if record.phase != wanted {
            return Err("transfer already closed with another disposition".into());
        }
    } else {
        match disposition {
            FinishDisposition::Abandon if record.phase == TransferPhase::Applying => {
                return Err("cannot abandon dispatched transfer".into())
            }
            FinishDisposition::Settled => {
                let (results, operations) = status(root, owner, directory, record, alive, located)?;
                if !all_settled(&results, &operations) {
                    return Err("transfer has unresolved effects; settlement refused".into());
                }
            }
            _ => {}
        }
        live(alive)?;
        record.phase = wanted;
        save(directory, record)?; // Tombstone precedes deletion; repeat cleanup after crash.
        if wanted == TransferPhase::Finished {
            settle_receipts(root, owner, record, alive, located);
        }
    }
    cleanup(directory)
}
/// The block's final paths, once its final write applied.
fn final_exclude_paths(
    manifest: &BatchManifest,
    items: &[MemberResult],
    operations: &[OperationResult],
) -> Option<Vec<String>> {
    let block = manifest.exclude_block.as_ref()?;
    let applied = operations.iter().any(|operation| {
        operation.kind == OperationKind::ExcludeBlock && successful(&operation.outcome)
    });
    applied.then(|| exclude_final_paths(manifest, block, items, operations))
}
/// Before a cut batch is purged, its exclude block is put in its final shape
/// from the stored outcomes: paths it never wrote leave the block (a user's
/// file there must not stay ignored), paths it never removed stay.
fn finalize_exclude(
    root: &Path,
    owner: u64,
    record: &Record,
    items: &[MemberResult],
    operations: &[OperationResult],
    alive: &dyn Fn() -> bool,
    located: &files::Located,
) -> Result<(), String> {
    let Some(block) = &record.manifest.exclude_block else {
        return Ok(());
    };
    let added = exclude_add_receipt(root, owner, record, alive, located)?;
    let pending = operations.iter().any(|operation| {
        operation.kind == OperationKind::ExcludeBlock && is_not_started(&operation.outcome)
    });
    if !matches!(added, Some(EffectOutcome::Applied { .. })) || !pending {
        return Ok(());
    }
    let paths = exclude_final_paths(&record.manifest, block, items, operations);
    let _leaf = locks::exclude(&located.repo_path, alive)?;
    files::batch_effect(
        root,
        owner,
        FileRequest {
            target: record.target.clone(),
            operation: FileOperation::ReplaceWorkspaceExclude {
                operation_id: block.operation_id.clone(),
                workspace_id: block.workspace_id.clone(),
                expected: union(&block.expected, &block.paths),
                paths,
                repair: false,
            },
        },
        alive,
        located,
    )
    .map(|_| ())
}
/// Every outcome is final: applied, conflicted, skipped, or blocked only by
/// members that are themselves final.
fn all_settled(results: &[MemberResult], operations: &[OperationResult]) -> bool {
    if !operations
        .iter()
        .all(|operation| is_settled(&operation.outcome))
    {
        return false;
    }
    let mut settled: HashSet<usize> = results
        .iter()
        .filter(|result| is_settled(&result.outcome))
        .map(|result| result.index)
        .collect();
    loop {
        let before = settled.len();
        for result in results {
            if let MemberOutcome::Blocked { dependencies } = &result.outcome {
                if dependencies.iter().all(|index| settled.contains(index)) {
                    settled.insert(result.index);
                }
            }
        }
        if before == settled.len() {
            return settled.len() == results.len();
        }
    }
}
/// Retention: a settled batch keeps its outcomes, not its bytes; old
/// receipts are pruned. Best effort: the settlement already stands.
fn settle_receipts(
    root: &Path,
    owner: u64,
    record: &Record,
    alive: &dyn Fn() -> bool,
    located: &files::Located,
) {
    let ids: Vec<&str> = record
        .manifest
        .items
        .iter()
        .map(|item| item.operation_id.as_str())
        .collect();
    if let Err(error) = files::compact_receipts(root, owner, &record.target, &ids, alive, located) {
        log::warn!("settled batch receipts not compacted: {error}");
    }
}
/// Closed batches' staging records older than the TTL, other than `keep`.
fn prune_batches(workspace: &File, keep: &[&str]) -> Result<(), String> {
    for name in files::children(workspace)? {
        if keep.contains(&name.as_str()) || uuid::Uuid::parse_str(&name).is_err() {
            continue;
        }
        let Ok(staging) = directory(workspace, &name, false) else {
            continue;
        };
        let Ok(old) = record(&staging) else {
            continue;
        };
        // A closed batch's record and journal are its
        // members' receipts, kept as long as per-operation receipts are.
        if !closed(old.phase) || !older_than(&staging, files::RECEIPT_RETENTION)? {
            continue;
        }
        cleanup(&staging)?;
        for leaf in files::children(&staging)? {
            let component = self::name(&leaf)?;
            // SAFETY: a child of the held staging directory; never a directory.
            unsafe { libc::unlinkat(staging.as_raw_fd(), component.as_ptr(), 0) };
        }
        let component = self::name(&name)?;
        // SAFETY: an emptied child directory of the held workspace directory.
        unsafe {
            libc::unlinkat(
                workspace.as_raw_fd(),
                component.as_ptr(),
                libc::AT_REMOVEDIR,
            )
        };
    }
    io(durable::sync(workspace))
}

pub(crate) fn handle(
    root: &Path,
    owner: u64,
    request: BatchRequest,
    alive: impl Fn() -> bool,
) -> Result<BatchReply, String> {
    // Where a batch step's time went (the remote perf harness reads
    // these lines; an operator reads them when a launch is slow).
    let step = match &request.operation {
        BatchOperation::Begin { .. } => "begin",
        BatchOperation::Chunk { .. } => "chunk",
        BatchOperation::Seal => "seal",
        BatchOperation::Apply => "apply",
        BatchOperation::Status => "status",
        BatchOperation::Finish { .. } => "finish",
        BatchOperation::InspectSelected { .. } => "inspect",
    };
    let batch = request.batch_id.clone();
    let started = Instant::now();
    let meter = durable::Meter::start();
    let reply = handle_step(root, owner, request, alive);
    let syncs = meter.read();
    let members = match &reply {
        Ok(BatchReply::Transfer { status }) => status.items.len() + status.operations.len(),
        _ => 0,
    };
    // Chunks and Status polls are frequent: debug.
    let level = if matches!(step, "chunk" | "status") {
        log::Level::Debug
    } else {
        log::Level::Info
    };
    log::log!(
        level,
        "context batch {step} {batch}: {} ms, {members} members, {} fsyncs ({} flush, {} ms){}",
        started.elapsed().as_millis(),
        syncs.total(),
        syncs.flushes,
        syncs.spent.as_millis(),
        if reply.is_ok() { "" } else { ", failed" }
    );
    reply
}
fn handle_step(
    root: &Path,
    owner: u64,
    request: BatchRequest,
    alive: impl Fn() -> bool,
) -> Result<BatchReply, String> {
    if request.version != VERSION || request.control_size()? > STREAMED_MAX_CONTROL_BYTES {
        return Err("unsupported or oversized selected batch control".into());
    }
    let batch_uuid = uuid::Uuid::parse_str(&request.batch_id).map_err(|_| "invalid batch UUID")?;
    if batch_uuid.to_string() != request.batch_id {
        return Err("batch UUID must use canonical spelling".into());
    }
    live(&alive)?;
    if let BatchOperation::InspectSelected { items } = &request.operation {
        validate_paths(items.iter().map(|item| item.path.as_str()))?;
        let mut observations = Vec::new();
        for item in items {
            let reply = files::handle(
                root,
                owner,
                FileRequest {
                    target: request.target.clone(),
                    operation: FileOperation::Inspect {
                        path: item.path.clone(),
                    },
                },
                &alive,
            )?;
            let FileReply::Inspected { observation } = reply else {
                return Err("unexpected selected observation".into());
            };
            // A changed item is reported on its own; the rest of the
            // inspection still answers.
            let mismatch = item.expected_hash.is_some_and(|expected| {
                !matches!(observation, Observation::Regular { hash, .. } if hash == expected)
            });
            observations.push(InspectedItem {
                path: item.path.clone(),
                observation,
                mismatch,
            });
        }
        return Ok(BatchReply::Inspected {
            items: observations,
        });
    }
    // A chunk only appends to its own batch's staging: it takes that batch's
    // staging lock and nothing else, reading the already bound ledger without
    // writes. Every other step takes the workspace lock first (Begin and Seal
    // decide the one active batch; Apply/Status/Finish touch the worktree).
    let chunking = matches!(request.operation, BatchOperation::Chunk { .. });
    let located = files::locate(root, owner, &request.target, &alive)?;
    let _workspace = if chunking {
        None
    } else {
        Some(locks::workspace(&located.worktree_path, &alive)?)
    };
    let staging_key = |batch_id: &str| {
        format!(
            "batch:{}:{}:{}:{batch_id}",
            root.display(),
            owner,
            request.target.handle
        )
    };
    let _staging = locks::staging(&staging_key(&request.batch_id), &alive)?;
    if chunking {
        test_hold("batch-chunk");
    }
    let ledger = if chunking {
        files::bound_ledger(root, &located, &alive)?
            .ok_or("selected transfer has no reservation; repeat Begin")?
    } else {
        files::batch_ledger(root, owner, &request.target, &located, &alive)?
    };
    // A staging directory is synced into its parent when this call
    // created it, and on every Begin (so one a crashed call created is durable
    // before a batch relies on it); never on each Status or Chunk.
    let beginning = matches!(request.operation, BatchOperation::Begin { .. });
    let (all, created) = directory_created(&ledger, "context-batches")?;
    owner_directory(&all)?;
    if created || beginning {
        io(durable::sync(&ledger))?;
    }
    let workspace_key = digest(
        format!(
            "{}:{}:{}",
            owner, request.target.origin_key, request.target.handle
        )
        .as_bytes(),
    );
    let (workspace, created) = directory_created(&all, &workspace_key)?;
    owner_directory(&workspace)?;
    if created || beginning {
        io(durable::sync(&all))?;
    }
    let batch_key = uuid::Uuid::parse_str(&request.batch_id)
        .map_err(|_| "invalid batch UUID")?
        .to_string();
    let reservation: Option<Reservation> = if exists(&workspace, "active")? {
        Some(
            serde_json::from_slice(&read(&workspace, "active")?)
                .map_err(|_| "invalid active batch reservation")?,
        )
    } else {
        None
    };
    let mut current_reservation = reservation;
    if let BatchOperation::Begin { manifest } = &request.operation {
        manifest.validate()?;
        if manifest.digest()? != request.manifest_digest {
            return Err("selected manifest digest mismatch".into());
        }
        if let Some(active) = &current_reservation {
            if active.initial.batch_id != request.batch_id {
                // The superseded batch's own chunks run without the workspace
                // lock: take its staging lock (under ours; Begin is the only
                // step that nests two) so no late chunk writes while it is
                // judged closed and cleaned.
                let _previous_staging =
                    locks::staging(&staging_key(&active.initial.batch_id), &alive)?;
                let previous = directory(&workspace, &active.initial.batch_id, false)?;
                let mut previous_record = record(&previous)?;
                if !closed(previous_record.phase) {
                    let busy = || {
                        format!(
                            "selected transfer busy: recover active transfer {}",
                            active.initial.batch_id
                        )
                    };
                    previous_record.phase = match previous_record.phase {
                        // Nothing was dispatched: it expires after the TTL.
                        TransferPhase::Uploading | TransferPhase::Verified
                            if expired(&previous)? =>
                        {
                            TransferPhase::Abandoned
                        }
                        // We hold the workspace lock, so no Apply of it is
                        // running: it was cut (its desktop died or timed out).
                        // Settled, it is finished; otherwise it is resumed by
                        // its own Begin, or purged after the TTL. Its
                        // outcomes stay in the receipts; Status still answers.
                        TransferPhase::Applying => {
                            let (items, operations) =
                                status(root, owner, &previous, &previous_record, &alive, &located)?;
                            if all_settled(&items, &operations) {
                                TransferPhase::Finished
                            } else if expired(&previous)? {
                                if let Err(error) = finalize_exclude(
                                    root,
                                    owner,
                                    &previous_record,
                                    &items,
                                    &operations,
                                    &alive,
                                    &located,
                                ) {
                                    log::warn!(
                                        "purged batch's exclude block not finalized: {error}"
                                    );
                                }
                                TransferPhase::Abandoned
                            } else {
                                return Err(busy());
                            }
                        }
                        _ => return Err(busy()),
                    };
                    save(&previous, &previous_record)?;
                    if previous_record.phase == TransferPhase::Finished {
                        settle_receipts(root, owner, &previous_record, &alive, &located);
                    }
                }
                cleanup(&previous)?;
            } else if !same_intent(&active.initial, &request)
                || active.initial.manifest != *manifest
            {
                return Err("batch reservation belongs to another immutable intent".into());
            }
        }
        let active_id = current_reservation
            .as_ref()
            .map(|active| active.initial.batch_id.clone())
            .unwrap_or_default();
        if let Err(error) = prune_batches(&workspace, &[&batch_key, &active_id]) {
            log::warn!("old selected batches not pruned: {error}");
        }
        if !exists(&workspace, &batch_key)? {
            if current_reservation
                .as_ref()
                .is_some_and(|active| active.initial.batch_id == request.batch_id && active.ready)
            {
                return Err("initialized selected staging disappeared; outcome unavailable".into());
            }
            let initial = Record {
                target: request.target.clone(),
                batch_id: request.batch_id.clone(),
                manifest_digest: request.manifest_digest,
                manifest: manifest.clone(),
                phase: TransferPhase::Uploading,
                offset: 0,
            };
            let reservation = Reservation {
                initial,
                ready: false,
            };
            // Immutable reservation precedes directory/record/allocation. A cut
            // anywhere before ready can recover only this exact no-effects intent.
            save_reservation(&workspace, &reservation)?;
            current_reservation = Some(reservation);
            #[cfg(test)]
            if FAIL_AFTER_RESERVATION.with(|flag| flag.replace(false)) {
                return Err("injected interruption before transfer record creation".into());
            }
        }
    }
    let initializing = current_reservation
        .as_ref()
        .is_some_and(|active| active.initial.batch_id == request.batch_id && !active.ready);
    let (staging, created) = if initializing {
        directory_created(&workspace, &batch_key)?
    } else {
        (directory(&workspace, &batch_key, false)?, false)
    };
    owner_directory(&staging)?;
    if created || beginning {
        io(durable::sync(&workspace))?;
    }
    let mut current = if exists(&staging, "record.json")? {
        record(&staging)?
    } else if initializing {
        current_reservation
            .as_ref()
            .ok_or("missing reservation")?
            .initial
            .clone()
    } else {
        return Err("selected transfer record missing; outcome unavailable".into());
    };
    if !same_intent(&current, &request) {
        return Err("batch UUID belongs to another immutable intent".into());
    }
    if initializing && !closed(current.phase) {
        if current.phase != TransferPhase::Uploading || current.offset != 0 {
            return Err("inconsistent initializing transfer".into());
        }
        match &request.operation {
            BatchOperation::Begin { .. } => {
                save(&staging, &current)?;
                reserve(&staging, &current.manifest, &alive)?;
                let mut active = current_reservation.take().ok_or("missing reservation")?;
                active.ready = true;
                save_reservation(&workspace, &active)?;
            }
            BatchOperation::Status
            | BatchOperation::Finish {
                disposition: FinishDisposition::Abandon,
            } => {}
            _ => return Err("selected transfer reservation is incomplete; repeat Begin".into()),
        }
    }
    if matches!(
        request.operation,
        BatchOperation::Chunk { .. } | BatchOperation::Seal
    ) && !current_reservation
        .as_ref()
        .is_some_and(|active| active.initial.batch_id == request.batch_id && active.ready)
    {
        // One winner per workspace: only the batch its reservation names may
        // stage or seal. A late chunk or seal of a superseded batch is refused.
        return Err("selected transfer is not this workspace's active batch".into());
    }
    let mut items: Option<(Vec<MemberResult>, Vec<OperationResult>)> = None;
    match request.operation {
        BatchOperation::Begin { manifest } => {
            if manifest != current.manifest {
                return Err("retained batch manifest differs".into());
            }
        }
        BatchOperation::Chunk { offset, bytes } => {
            chunk(&staging, &mut current, offset, &bytes, &alive)?
        }
        BatchOperation::Seal => seal(&staging, &mut current, &alive)?,
        BatchOperation::Apply => {
            items = Some(apply(
                root,
                owner,
                &staging,
                &mut current,
                &alive,
                &located,
            )?);
        }
        BatchOperation::Status => {}
        BatchOperation::Finish { disposition } => finish(
            root,
            owner,
            &staging,
            &mut current,
            disposition,
            &alive,
            &located,
        )?,
        BatchOperation::InspectSelected { .. } => unreachable!("inspection returned above"),
    }
    live(&alive)?;
    let (items, operations) = match items {
        Some(items) => items,
        None if matches!(
            current.phase,
            TransferPhase::Applying | TransferPhase::Finished | TransferPhase::Abandoned
        ) =>
        {
            status(root, owner, &staging, &current, &alive, &located)?
        }
        None => (
            initial_status(&current.manifest),
            initial_operations(&current.manifest),
        ),
    };
    Ok(BatchReply::Transfer {
        status: TransferStatus {
            phase: current.phase,
            offset: current.offset,
            exclude_paths: final_exclude_paths(&current.manifest, &items, &operations),
            items,
            operations,
        },
    })
}

#[cfg(test)]
#[path = "context_batch_tests.rs"]
pub(super) mod tests;

#[cfg(test)]
thread_local! {
    static FAIL_AFTER_RESERVATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_DURING_RESERVATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
#[path = "context_batch_bench.rs"]
mod bench;

#[cfg(test)]
#[path = "context_batch_xs26_tests.rs"]
mod xs26_tests;
