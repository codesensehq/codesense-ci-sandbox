// SPDX-License-Identifier: MIT
//! Contained effects only. Projection, merge and publication policy stay in the desktop engine.
use super::*;
use codesense_pty_core::remote_files::{
    Effect, EffectOutcome, Expected, FileOperation, FileReply, FileRequest, FileTarget, ListEntry,
    Observation, MAX_FILE_BYTES, MAX_PATHS,
};

#[derive(Clone, Serialize, Deserialize)]
struct FileReceipt {
    request: FileRequest,
    outcome: Option<EffectOutcome>,
    /// Retention: a settled write keeps the digest of its bytes, not
    /// the bytes (`request`'s write bytes are then empty).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    written: Option<WrittenDigest>,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct WrittenDigest {
    length: u64,
    hash: [u8; 32],
}
impl FileReceipt {
    fn new(request: FileRequest) -> Self {
        Self {
            request,
            outcome: None,
            written: None,
        }
    }
    /// The length and hash of the bytes a retained write wrote.
    fn write_identity(&self) -> Option<(u64, [u8; 32])> {
        match &self.request.operation {
            FileOperation::Apply {
                effect: Effect::Write { bytes, .. },
                ..
            } => Some(match self.written {
                Some(digest) => (digest.length, digest.hash),
                None => (bytes.len() as u64, *blake3::hash(bytes).as_bytes()),
            }),
            _ => None,
        }
    }
    /// Whether `operation` is this receipt's exact intent (a compacted write
    /// is compared by its bytes' digest).
    fn names(&self, operation: &FileOperation) -> bool {
        match (&self.written, &self.request.operation, operation) {
            (
                Some(digest),
                FileOperation::Apply {
                    operation_id,
                    path,
                    expected,
                    effect: Effect::Write { mode, .. },
                },
                FileOperation::Apply {
                    operation_id: wanted_id,
                    path: wanted_path,
                    expected: wanted_expected,
                    effect:
                        Effect::Write {
                            bytes,
                            mode: wanted_mode,
                        },
                },
            ) => {
                operation_id == wanted_id
                    && path == wanted_path
                    && expected == wanted_expected
                    && mode == wanted_mode
                    && digest.length == bytes.len() as u64
                    && digest.hash == *blake3::hash(bytes).as_bytes()
            }
            _ => self.request.operation == *operation,
        }
    }
    /// Drop a settled write's bytes, keeping their digest.
    fn compact(&mut self) -> bool {
        if self.outcome.is_none() || self.written.is_some() {
            return false;
        }
        let Some((length, hash)) = self.write_identity() else {
            return false;
        };
        if let FileOperation::Apply {
            effect: Effect::Write { bytes, .. },
            ..
        } = &mut self.request.operation
        {
            bytes.clear();
        }
        self.written = Some(WrittenDigest { length, hash });
        true
    }
}
struct Workspace {
    root: File,
    ledger: File,
    repo_path: String,
    worktree_path: String,
}
/// Where a target's worktree and clone are, for lock keys. Lock-free: it
/// only reads the receipt and checks authority; whoever locks re-resolves.
#[derive(Clone)]
pub(super) struct Located {
    pub(super) worktree_path: String,
    pub(super) repo_path: String,
    receipts: std::sync::Arc<File>,
    key: String,
    receipt: Receipt,
}
/// Authority and completed-worktree identity, checked once (two Git
/// commands); every later step reuses this instead of checking again.
pub(super) fn locate(
    root: &Path,
    owner: u64,
    target: &FileTarget,
    alive: &dyn Fn() -> bool,
) -> Result<Located, String> {
    let (receipts, key, receipt) = resolve_completed_target(root, owner, target, alive)?;
    Ok(Located {
        worktree_path: receipt.intent.worktree_path.clone(),
        repo_path: receipt.intent.repo_path.clone(),
        receipts: std::sync::Arc::new(receipts),
        key,
        receipt,
    })
}
/// A read's view of a completed workspace: authority checked, worktree held,
/// nothing written (no namespace binding; reads need no ledger).
pub(super) fn reader(
    root: &Path,
    owner: u64,
    target: &FileTarget,
    alive: &dyn Fn() -> bool,
) -> Result<(File, Located), String> {
    resolve_reader(root, owner, target, alive)
}
fn resolve_reader(
    root: &Path,
    owner: u64,
    target: &FileTarget,
    alive: &dyn Fn() -> bool,
) -> Result<(File, Located), String> {
    let located = locate(root, owner, target, alive)?;
    let worktree = open_directory(&located.worktree_path, false)?;
    owner_directory(&worktree)?;
    Ok((worktree, located))
}
/// The ledger of a workspace whose namespace is already bound, without any
/// lock or write; `None` when binding is still needed (that takes the lock).
pub(super) fn bound_ledger(
    root: &Path,
    located: &Located,
    alive: &dyn Fn() -> bool,
) -> Result<Option<File>, String> {
    let Some(expected) = located.receipt.file_operation_generation.clone() else {
        return Ok(None);
    };
    let private = io(File::open(root))?;
    if !exists(&private, "remote-file-operations")? {
        return Err("remote file operation namespace is missing; outcome unknown".into());
    }
    let ledger = directory(&private, "remote-file-operations", false)?;
    owner_directory(&ledger)?;
    if !exists(&ledger, "identity")?
        || String::from_utf8(read(&ledger, "identity")?).ok().as_ref() != Some(&expected)
    {
        return Err("remote file operation namespace replaced; outcome unknown".into());
    }
    if !alive() {
        return Err("remote file authority retired".into());
    }
    Ok(Some(ledger))
}
fn relative(path: &str) -> Result<Vec<&str>, String> {
    let parts: Vec<_> = path.split('/').collect();
    if path.is_empty()
        || path.len() > 4096
        || parts.len() > 64
        || path.contains(['\\', '\0'])
        || parts.iter().any(|part| {
            part.is_empty() || matches!(*part, "." | "..") || part.eq_ignore_ascii_case(".git")
        })
    {
        return Err("remote file path must be relative, contained and outside Git metadata".into());
    }
    Ok(parts)
}
pub(super) fn relative_path(path: &str) -> Result<(), String> {
    relative(path).map(|_| ())
}
/// A contained, owner-checked read of one worktree file (see `observation_at`).
pub(super) fn observe_path(root: &File, path: &str) -> Result<(Observation, Vec<u8>), String> {
    observe(root, path)
}
/// `observe_path` for a read-only view: a path under a component that is no
/// longer a directory (a file or symlink now stands where its parent was)
/// cannot exist, so it reads as absent instead of failing.
pub(super) fn observe_path_for_view(
    root: &File,
    path: &str,
) -> Result<(Observation, Vec<u8>), String> {
    let parts = relative(path)?;
    let mut parent = io(root.try_clone())?;
    for part in &parts[..parts.len() - 1] {
        if !is_directory_entry(&parent, part)? {
            return Ok((Observation::Absent, Vec::new()));
        }
        parent = directory(&parent, part, false)?;
        owner_directory(&parent)?;
    }
    observation_at(&parent, parts[parts.len() - 1])
}
/// Whether `component` of `parent` is a directory itself (never a symlink to
/// one); `false` when absent.
fn is_directory_entry(parent: &File, component: &str) -> Result<bool, String> {
    let c = name(component)?;
    let mut meta = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: valid directory FD, NUL-terminated name, and a stat buffer the
    // call fills on success.
    let rc = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            c.as_ptr(),
            meta.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc != 0 {
        return if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
            Ok(false)
        } else {
            Err("cannot inspect remote path".into())
        };
    }
    // SAFETY: fstatat succeeded, so the buffer is initialized.
    let meta = unsafe { meta.assume_init() };
    Ok(meta.st_mode & libc::S_IFMT == libc::S_IFDIR)
}
/// `(size, mtime_ns)` of a contained regular file without reading it; `None`
/// when absent, unsafe or not a regular file.
pub(super) fn stat_path(root: &File, path: &str) -> Option<(u64, i64)> {
    relative(path).ok()?;
    let (parent, leaf) = parent_at(root, path).ok()??;
    let leaf = std::ffi::CString::new(leaf).ok()?;
    // A stat relative to the contained parent, never following a final
    // symlink: no open (so no FIFO, device or lock side effects) per tick.
    let mut meta = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: valid directory FD, NUL-terminated name, and a stat buffer the
    // call fills on success.
    let result = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            leaf.as_ptr(),
            meta.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result != 0 {
        return None;
    }
    // SAFETY: fstatat succeeded, so the buffer is initialized.
    let meta = unsafe { meta.assume_init() };
    (meta.st_mode & libc::S_IFMT == libc::S_IFREG).then(|| {
        (
            meta.st_size.max(0) as u64,
            meta.st_mtime
                .saturating_mul(1_000_000_000)
                .saturating_add(meta.st_mtime_nsec),
        )
    })
}
fn operation_id(operation: &FileOperation) -> Option<&str> {
    match operation {
        FileOperation::Apply { operation_id, .. }
        | FileOperation::Reconcile { operation_id }
        | FileOperation::EnsureDirectories { operation_id, .. }
        | FileOperation::ReplaceManagedExclude { operation_id, .. }
        | FileOperation::ReplaceWorkspaceExclude { operation_id, .. } => Some(operation_id),
        _ => None,
    }
}
fn observation_at(parent: &File, leaf: &str) -> Result<(Observation, Vec<u8>), String> {
    if !exists(parent, leaf)? {
        return Ok((Observation::Absent, Vec::new()));
    }
    let file = match open(parent, leaf, libc::O_RDONLY | libc::O_NONBLOCK) {
        Ok(file) => file,
        Err(_) => return Ok((Observation::Unsafe, Vec::new())),
    };
    let before = io(file.metadata())?;
    // SAFETY: geteuid takes no pointers.
    if before.uid() != unsafe { libc::geteuid() } {
        return Ok((Observation::Unsafe, Vec::new()));
    }
    if before.is_dir() {
        return Ok((Observation::Directory, Vec::new()));
    }
    if !before.is_file() || before.nlink() != 1 {
        return Ok((Observation::Unsafe, Vec::new()));
    }
    if before.len() > MAX_FILE_BYTES as u64 {
        return Err("remote file exceeds bounded read limit".into());
    }
    let mut bytes = Vec::new();
    io((&file)
        .take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes))?;
    let after = io(file.metadata())?;
    if bytes.len() > MAX_FILE_BYTES
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err("remote file changed during read".into());
    }
    let mtime_ns = (after.mtime().max(0) as u128)
        .saturating_mul(1_000_000_000)
        .saturating_add(after.mtime_nsec().max(0) as u128);
    Ok((
        Observation::Regular {
            hash: *blake3::hash(&bytes).as_bytes(),
            size: after.len(),
            mtime_ns: u64::try_from(mtime_ns).map_err(|_| "remote mtime exceeds wire range")?,
            mode: after.mode() & 0o777,
        },
        bytes,
    ))
}
fn parent_at(root: &File, path: &str) -> Result<Option<(File, String)>, String> {
    let parts = relative(path)?;
    let mut parent = io(root.try_clone())?;
    for part in &parts[..parts.len() - 1] {
        if !exists(&parent, part)? {
            return Ok(None);
        }
        parent = directory(&parent, part, false)?;
        owner_directory(&parent)?;
    }
    let leaf = resolve_leaf(&parent, parts[parts.len() - 1])?;
    Ok(Some((parent, leaf)))
}
/// The leaf as spelled on disk. Paths are compared in NFC;
/// on a filesystem that compares names byte for byte, a file whose name is
/// stored decomposed is found by NFC equivalence and acted on under its own
/// spelling, so a remove or replace never misses it (and never reports it
/// absent).
fn resolve_leaf(parent: &File, leaf: &str) -> Result<String, String> {
    if exists(parent, leaf)? {
        return Ok(leaf.into());
    }
    let wanted = codesense_pty_core::remote_manifest::nfc(leaf);
    Ok(nfc_equivalent(&children(parent)?, &wanted)?.unwrap_or_else(|| leaf.into()))
}
/// The name in `names` whose NFC form is `wanted`. Two such names are an
/// error, never a guess (and never a false "absent").
pub(super) fn nfc_equivalent(names: &[String], wanted: &str) -> Result<Option<String>, String> {
    let mut found = names
        .iter()
        .filter(|name| codesense_pty_core::remote_manifest::nfc(name) == wanted);
    let Some(first) = found.next() else {
        return Ok(None);
    };
    if found.next().is_some() {
        return Err("names here differ only in Unicode normalization; resolve them by hand".into());
    }
    Ok(Some(first.clone()))
}
/// A workspace-relative path as spelled on disk (each leaf resolved by NFC
/// equivalence where the parent exists), for exclude patterns that must
/// match the stored bytes.
fn disk_spelling(root: &File, path: &str) -> Result<String, String> {
    Ok(match parent_at(root, path)? {
        Some((_, leaf)) => match path.rsplit_once('/') {
            Some((parent, _)) => format!("{parent}/{leaf}"),
            None => leaf,
        },
        None => path.into(),
    })
}
fn nfc_sorted(paths: &[String]) -> Vec<String> {
    let mut paths: Vec<String> = paths
        .iter()
        .map(|path| codesense_pty_core::remote_manifest::nfc(path))
        .collect();
    paths.sort();
    paths.dedup();
    paths
}
fn observe(root: &File, path: &str) -> Result<(Observation, Vec<u8>), String> {
    match parent_at(root, path)? {
        Some((parent, leaf)) => observation_at(&parent, &leaf),
        None => Ok((Observation::Absent, Vec::new())),
    }
}
fn expected_matches(expected: &Expected, observation: &Observation) -> bool {
    matches!(
        (expected, observation),
        (Expected::Absent, Observation::Absent)
    ) || matches!((expected, observation), (Expected::Hash { hash: expected }, Observation::Regular { hash, .. }) if expected == hash)
}
fn expected_for(observation: &Observation) -> Result<Expected, String> {
    match observation {
        Observation::Absent => Ok(Expected::Absent),
        Observation::Regular { hash, .. } => Ok(Expected::Hash { hash: *hash }),
        _ => Err("remote destination is unsafe".into()),
    }
}
fn read_record(root: &File, key: &str) -> Result<Vec<u8>, String> {
    let file = open(root, key, libc::O_RDONLY | libc::O_NONBLOCK)?;
    let meta = io(file.metadata())?;
    if !meta.is_file() || meta.nlink() != 1 || meta.len() > (MAX_FILE_BYTES * 2) as u64 {
        return Err("remote operation receipt is unsafe".into());
    }
    let mut bytes = Vec::new();
    io(file
        .take((MAX_FILE_BYTES * 2 + 1) as u64)
        .read_to_end(&mut bytes))?;
    if bytes.len() > MAX_FILE_BYTES * 2 {
        return Err("remote operation receipt exceeds limit".into());
    }
    Ok(bytes)
}
/// The full mutating view, binding the operation namespace on first use.
/// The caller holds the target worktree's lock.
#[cfg(test)]
fn resolve(
    root: &Path,
    owner: u64,
    target: &FileTarget,
    alive: &dyn Fn() -> bool,
) -> Result<Workspace, String> {
    resolve_at(
        root,
        owner,
        target,
        locate(root, owner, target, alive)?,
        alive,
    )
}
/// [`resolve`] from an already checked [`Located`]: the receipt is re-read
/// (under the caller's lock, it is now final) but the Git identity checks are
/// not repeated.
fn resolve_at(
    root: &Path,
    owner: u64,
    target: &FileTarget,
    located: Located,
    alive: &dyn Fn() -> bool,
) -> Result<Workspace, String> {
    let Located {
        receipts,
        key,
        receipt: checked,
        ..
    } = located;
    let mut receipt: Receipt = serde_json::from_slice(&read(&receipts, &key)?)
        .map_err(|_| "workspace receipt unavailable")?;
    if receipt.handle != checked.handle || receipt.intent != checked.intent || !alive() {
        return Err("workspace handle does not belong to this captured origin".into());
    }
    let private = io(File::open(root))?;
    let worktree = open_directory(&receipt.intent.worktree_path, false)?;
    owner_directory(&worktree)?;
    // W1 request IDs locate setup evidence; the handle is the durable worktree
    // identity. An equivalent later setup receipt must inherit any established
    // journal generation, or lost-ledger absence could become NotStarted again.
    let needs_binding = receipt.file_operation_generation.is_none();
    if needs_binding {
        for candidate in children(&receipts)? {
            if candidate.len() != 69
                || !candidate.ends_with(".json")
                || !candidate.as_bytes()[..64]
                    .iter()
                    .all(|byte| byte.is_ascii_hexdigit())
            {
                continue;
            }
            let previous: Receipt = serde_json::from_slice(&read(&receipts, &candidate)?)
                .map_err(|_| "workspace receipt unavailable during generation binding")?;
            if previous.handle != target.handle
                || previous.intent.owner_id != owner
                || previous.intent.origin_key != target.origin_key
            {
                continue;
            }
            if let Some(generation) = previous.file_operation_generation {
                if receipt
                    .file_operation_generation
                    .as_ref()
                    .is_some_and(|known| known != &generation)
                {
                    return Err(
                        "equivalent workspace receipts disagree on operation namespace".into(),
                    );
                }
                receipt.file_operation_generation = Some(generation);
            }
        }
    }
    // Bind the operation ledger into the already durable W1 receipt before any file
    // effects. If the ledger later disappears or is replaced, absence is Unknown.
    if receipt.file_operation_generation.is_some() && !exists(&private, "remote-file-operations")? {
        return Err("remote file operation namespace is missing; outcome unknown".into());
    }
    let ledger = directory(
        &private,
        "remote-file-operations",
        receipt.file_operation_generation.is_none(),
    )?;
    owner_directory(&ledger)?;
    let generation = if exists(&ledger, "identity")? {
        String::from_utf8(read(&ledger, "identity")?)
            .map_err(|_| "file operation namespace invalid")?
    } else if receipt.file_operation_generation.is_none() {
        // The namespace is shared by every workspace on this host, whose first
        // bindings may race under their own locks: one identity is created and
        // every other binder adopts it.
        let value = uuid::Uuid::new_v4().to_string();
        if publish_new(&ledger, "identity", value.as_bytes())? {
            value
        } else {
            String::from_utf8(read(&ledger, "identity")?)
                .map_err(|_| "file operation namespace invalid")?
        }
    } else {
        return Err("remote file operation namespace is incomplete; outcome unknown".into());
    };
    uuid::Uuid::parse_str(&generation).map_err(|_| "file operation namespace invalid")?;
    if let Some(expected) = &receipt.file_operation_generation {
        if expected != &generation {
            return Err("remote file operation namespace replaced; outcome unknown".into());
        }
    } else {
        receipt.file_operation_generation = Some(generation);
    }
    if needs_binding {
        save(&receipts, &key, &receipt, false)?;
    }
    if !alive() {
        return Err("remote file authority retired".into());
    }
    Ok(Workspace {
        root: worktree,
        ledger,
        repo_path: receipt.intent.repo_path,
        worktree_path: receipt.intent.worktree_path,
    })
}
fn ledger_key(target: &FileTarget, operation_id: &str) -> Result<String, String> {
    uuid::Uuid::parse_str(operation_id).map_err(|_| "file operation ID must be a UUID")?;
    Ok(format!(
        "{}.json",
        digest(format!("{}:{operation_id}", target.handle).as_bytes())
    ))
}
fn save_receipt(
    ledger: &File,
    key: &str,
    receipt: &FileReceipt,
    first: bool,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(receipt).map_err(|_| "file operation intent invalid")?;
    if first {
        write_new(ledger, key, &bytes)
    } else {
        replace(ledger, key, &bytes)
    }
}
fn applied(observation: Observation) -> EffectOutcome {
    EffectOutcome::Applied { observation }
}
fn unknown(operation_id: &str) -> EffectOutcome {
    EffectOutcome::Unknown {
        operation_id: operation_id.into(),
    }
}
fn mode(value: u32) -> Result<u32, String> {
    if value & !0o777 != 0 {
        return Err("unsupported remote file mode".into());
    }
    Ok(value)
}
/// How a conditional effect makes itself durable.
pub(super) enum Durability<'a> {
    /// On its own: the parent directory is synced before the effect reports.
    Now,
    /// Batch Apply: data is ordered before the rename, and what the
    /// effect did is reported for one directory sync at the end of the call.
    /// A staged temporary of this operation left by a cut Apply is redone.
    Later(&'a mut Landed),
}
/// What a deferred effect did.
#[derive(Default)]
pub(super) struct Landed {
    /// The parent directory's entries changed (a rename or an unlink).
    pub(super) entries: bool,
    /// The file itself was ordered (a mode change).
    pub(super) ordered: bool,
}
fn conditional_file(
    parent: &File,
    leaf: &str,
    expected: &Expected,
    effect: &Effect,
    operation_id: &str,
    alive: &dyn Fn() -> bool,
    mut durability: Durability,
) -> Result<EffectOutcome, String> {
    let (current, _) = observation_at(parent, leaf)?;
    // A remove whose target is already gone, and a write whose
    // target already holds exactly these bytes and mode (two launches raced),
    // are success whatever pre-hash they carried.
    if matches!(effect, Effect::Remove) && current == Observation::Absent {
        return Ok(applied(Observation::Absent));
    }
    if !expected_matches(expected, &current) {
        if matches!(effect, Effect::Write { .. }) && effect_matches(effect, expected, &current) {
            return Ok(applied(current));
        }
        return Ok(EffectOutcome::Conflict {
            observation: current,
        });
    }
    if !alive() {
        return Ok(unknown(operation_id));
    }
    match effect {
        Effect::Write {
            bytes,
            mode: requested_mode,
        } => {
            if bytes.len() > MAX_FILE_BYTES {
                return Err("remote file exceeds limit".into());
            }
            let target_mode = mode(requested_mode.unwrap_or(match current {
                Observation::Regular { mode, .. } => mode,
                _ => 0o644,
            }))?;
            let temporary = format!(".codesense-file-{operation_id}.tmp");
            if exists(parent, &temporary)? {
                match durability {
                    Durability::Now => return Ok(unknown(operation_id)),
                    // Ours alone (the workspace lock is held and the name is
                    // this operation's): a cut Apply's leftover.
                    Durability::Later(_) => unlink_component(parent, &temporary)?,
                }
            }
            // Until the commit below, the staged file is ours alone: every exit
            // before it removes the file, so no error leaves a stray temporary.
            let staging = (|| -> Result<Option<EffectOutcome>, String> {
                let mut staged = open(
                    parent,
                    &temporary,
                    libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                )?;
                io(staged.write_all(bytes))?;
                // SAFETY: the held descriptor belongs to our own newly staged regular file.
                if unsafe { libc::fchmod(staged.as_raw_fd(), target_mode as libc::mode_t) } != 0 {
                    return Ok(Some(unknown(operation_id)));
                }
                // Data before name: the rename below never exposes torn bytes.
                match durability {
                    Durability::Now => io(durable::sync(&staged))?,
                    Durability::Later(_) => io(durable::order(&staged))?,
                }
                test_hold("member-write");
                // Same optimistic boundary as local materialization: check the latest
                // hash immediately before atomic replacement, never overwrite a known change.
                let (latest, _) = observation_at(parent, leaf)?;
                if !expected_matches(expected, &latest) {
                    return Ok(Some(EffectOutcome::Conflict {
                        observation: latest,
                    }));
                }
                if !alive() {
                    return Ok(Some(unknown(operation_id)));
                }
                Ok(None)
            })();
            match staging {
                Ok(None) => {}
                Ok(Some(outcome)) => {
                    discard(parent, &temporary);
                    return Ok(outcome);
                }
                Err(error) => {
                    discard(parent, &temporary);
                    return Err(error);
                }
            }
            // Per-entry durability: the staged file was synced above;
            // the parent directory is synced after the rename, now or once
            // per touched directory at the end of a batch Apply call.
            let committed = if matches!(expected, Expected::Absent) {
                // A new file: a no-replace rename keeps one link throughout, so
                // a lock-free reader never sees it doubly linked.
                match rename_no_replace(parent, &temporary, leaf) {
                    Ok(NoReplace::Renamed) => true,
                    Ok(NoReplace::Exists) => {
                        discard(parent, &temporary);
                        let (latest, _) = observation_at(parent, leaf)?;
                        return Ok(EffectOutcome::Conflict {
                            observation: latest,
                        });
                    }
                    Ok(NoReplace::Unsupported) => {
                        let src = name(&temporary)?;
                        let dst = name(leaf)?;
                        // SAFETY: two validated components under the same held directory.
                        let linked = unsafe {
                            libc::linkat(
                                parent.as_raw_fd(),
                                src.as_ptr(),
                                parent.as_raw_fd(),
                                dst.as_ptr(),
                                0,
                            )
                        } == 0;
                        discard(parent, &temporary);
                        linked
                    }
                    Err(_) => {
                        discard(parent, &temporary);
                        false
                    }
                }
            } else {
                let src = name(&temporary)?;
                let dst = name(leaf)?;
                // SAFETY: two validated components under the same held directory.
                let renamed = unsafe {
                    libc::renameat(
                        parent.as_raw_fd(),
                        src.as_ptr(),
                        parent.as_raw_fd(),
                        dst.as_ptr(),
                    )
                } == 0;
                if !renamed {
                    discard(parent, &temporary);
                }
                renamed
            };
            if !committed {
                return Ok(unknown(operation_id));
            }
            match &mut durability {
                Durability::Now => io(durable::sync(parent))?,
                Durability::Later(landed) => landed.entries = true,
            }
        }
        Effect::Remove => {
            if matches!(current, Observation::Absent) {
                return Ok(applied(Observation::Absent));
            }
            let (latest, _) = observation_at(parent, leaf)?;
            if !expected_matches(expected, &latest) {
                return Ok(EffectOutcome::Conflict {
                    observation: latest,
                });
            }
            if !alive() {
                return Ok(unknown(operation_id));
            }
            match &mut durability {
                Durability::Now => unlink_owned(parent, leaf)?,
                Durability::Later(landed) => {
                    unlink_component(parent, leaf)?;
                    landed.entries = true;
                }
            }
        }
        Effect::SetMode {
            mode: requested_mode,
        } => {
            let mode = mode(*requested_mode)?;
            let file = open(parent, leaf, libc::O_RDONLY | libc::O_NONBLOCK)?;
            let (latest, _) = observation_at(parent, leaf)?;
            if !expected_matches(expected, &latest) {
                return Ok(EffectOutcome::Conflict {
                    observation: latest,
                });
            }
            if !alive() {
                return Ok(unknown(operation_id));
            }
            // SAFETY: descriptor is a regular destination checked immediately above.
            if unsafe { libc::fchmod(file.as_raw_fd(), mode as libc::mode_t) } != 0 {
                return Ok(unknown(operation_id));
            }
            match &mut durability {
                Durability::Now => io(durable::sync(&file))?,
                Durability::Later(landed) => {
                    io(durable::order(&file))?;
                    landed.ordered = true;
                }
            }
        }
    }
    let (observation, _) = observation_at(parent, leaf)?;
    if effect_matches(effect, expected, &observation) {
        Ok(applied(observation))
    } else {
        Ok(unknown(operation_id))
    }
}
fn unlink_owned(parent: &File, leaf: &str) -> Result<(), String> {
    unlink_component(parent, leaf)?;
    io(durable::sync(parent))
}
fn unlink_component(parent: &File, leaf: &str) -> Result<(), String> {
    let component = name(leaf)?;
    // SAFETY: validated child of the held directory; unlink never follows a symlink.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), component.as_ptr(), 0) } != 0 {
        return Err("remote unlink outcome unknown".into());
    }
    Ok(())
}
fn effect_matches(effect: &Effect, expected: &Expected, observation: &Observation) -> bool {
    match (effect, observation) {
        (
            Effect::Write { bytes, mode },
            Observation::Regular {
                hash, mode: actual, ..
            },
        ) => *hash == *blake3::hash(bytes).as_bytes() && mode.is_none_or(|mode| mode == *actual),
        (Effect::Remove, Observation::Absent) => true,
        (Effect::SetMode { mode }, Observation::Regular { mode: actual, .. }) => {
            mode == actual && expected_matches(expected, observation)
        }
        _ => false,
    }
}
fn directory_effect(
    root: &File,
    path: &str,
    operation_id: &str,
    alive: &dyn Fn() -> bool,
) -> Result<EffectOutcome, String> {
    let mut current = io(root.try_clone())?;
    for part in relative(path)? {
        let (observation, _) = observation_at(&current, part)?;
        if !matches!(observation, Observation::Absent | Observation::Directory) {
            return Ok(EffectOutcome::Conflict { observation });
        }
        if !alive() {
            return Ok(unknown(operation_id));
        }
        let next = directory(&current, part, true)?;
        owner_directory(&next)?;
        io(durable::sync(&current))?;
        current = next;
    }
    Ok(applied(Observation::Directory))
}

const LIST_BYTES: usize = 8 * 1024 * 1024;
// readdir reports EOF and errors with the same null return. Inspect thread-local
// errno so a failed scan cannot authorize a partial absence result.
pub(super) fn directory_errno() -> *mut libc::c_int {
    #[cfg(target_os = "macos")]
    unsafe {
        libc::__error()
    }
    #[cfg(target_os = "linux")]
    unsafe {
        libc::__errno_location()
    }
}
pub(super) fn children(directory: &File) -> Result<Vec<String>, String> {
    children_of(directory, false)
}
/// The names in `directory`. `skip_unreadable` drops a non-UTF-8 name instead
/// of failing: only for the editor's one-level listing, where such a name can't
/// be opened anyway. Enumeration that must be exact keeps failing on one.
fn children_of(directory: &File, skip_unreadable: bool) -> Result<Vec<String>, String> {
    // A new open-file description avoids sharing the directory stream offset.
    // SAFETY: fixed dot component reopens this held directory with a fresh offset.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c".".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err("directory reopen failed".into());
    }
    // SAFETY: fd is an owned directory descriptor transferred to fdopendir.
    let stream = unsafe { libc::fdopendir(fd) };
    if stream.is_null() {
        // SAFETY: fdopendir did not take ownership on failure.
        unsafe {
            libc::close(fd);
        }
        return Err("remote directory cannot be enumerated".into());
    }
    struct Stream(*mut libc::DIR);
    impl Drop for Stream {
        fn drop(&mut self) {
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let stream = Stream(stream);
    let mut names = Vec::new();
    let mut size = 0usize;
    loop {
        // SAFETY: stream remains owned for this call; name is copied before next call.
        let entry = unsafe {
            *directory_errno() = 0;
            libc::readdir(stream.0)
        };
        if entry.is_null() {
            if unsafe { *directory_errno() } != 0 {
                return Err("remote directory scan failed; result is incomplete".into());
            }
            break;
        }
        let raw = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
        let name = match raw.to_str() {
            Ok(name) => name,
            Err(_) if skip_unreadable => continue,
            Err(_) => return Err("non-UTF8 remote path".into()),
        };
        if matches!(name, "." | "..") || name.eq_ignore_ascii_case(".git") {
            continue;
        }
        size = size.saturating_add(name.len());
        if names.len() >= MAX_PATHS || size > LIST_BYTES {
            return Err("remote path listing exceeds limit".into());
        }
        names.push(name.to_owned());
    }
    names.sort();
    Ok(names)
}
fn enumerate(
    root: &File,
    prefix: Option<&str>,
    limit: usize,
    alive: &dyn Fn() -> bool,
) -> Result<Vec<String>, String> {
    if limit == 0 || limit > MAX_PATHS {
        return Err("invalid remote enumeration limit".into());
    }
    let start = if let Some(prefix) = prefix {
        let mut dir = io(root.try_clone())?;
        for part in relative(prefix)? {
            if !exists(&dir, part)? {
                return Ok(Vec::new());
            }
            dir = directory(&dir, part, false)?;
            owner_directory(&dir)?;
        }
        dir
    } else {
        io(root.try_clone())?
    };
    let mut pending = vec![(start, prefix.unwrap_or_default().to_string())];
    let mut result = Vec::new();
    let mut bytes = 0usize;
    while let Some((dir, prefix)) = pending.pop() {
        if !alive() {
            return Err("remote file authority retired".into());
        }
        for leaf in children(&dir)? {
            let path = if prefix.is_empty() {
                leaf.clone()
            } else {
                format!("{prefix}/{leaf}")
            };
            relative(&path)?;
            bytes = bytes.saturating_add(path.len());
            if result.len() >= limit || bytes > LIST_BYTES {
                return Err("remote enumeration exceeds limit; result is incomplete".into());
            }
            // No-follow directory open; unsafe entries are reported but never traversed.
            let name = name(&leaf)?;
            let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
            // SAFETY: initialized output and validated child under the held directory.
            if unsafe {
                libc::fstatat(
                    dir.as_raw_fd(),
                    name.as_ptr(),
                    &mut metadata,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } != 0
            {
                return Err("remote entry changed during enumeration; result is incomplete".into());
            }
            if metadata.st_mode & libc::S_IFMT == libc::S_IFDIR {
                let child = directory(&dir, &leaf, false)?;
                owner_directory(&child)?;
                pending.push((child, path.clone()));
            }
            result.push(path);
        }
    }
    result.sort();
    Ok(result)
}
/// One directory level for an editor tree. Descends only along `path` itself,
/// so the cost is one `readdir` however large the sibling directories are.
fn list(root: &File, path: Option<&str>) -> Result<Vec<ListEntry>, String> {
    let mut dir = io(root.try_clone())?;
    if let Some(path) = path.filter(|p| !p.is_empty()) {
        for part in relative(path)? {
            if !exists(&dir, part)? {
                return Err("remote directory not found".into());
            }
            dir = directory(&dir, part, false)?;
            owner_directory(&dir)?;
        }
    }
    let mut entries = Vec::new();
    for leaf in children_of(&dir, true)? {
        let component = name(&leaf)?;
        let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: initialized output and a validated child of the held directory;
        // NOFOLLOW so a symlink reports itself, never its target.
        if unsafe {
            libc::fstatat(
                dir.as_raw_fd(),
                component.as_ptr(),
                &mut metadata,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            // Removed between readdir and stat: the listing simply omits it.
            continue;
        }
        let kind = metadata.st_mode & libc::S_IFMT;
        entries.push(ListEntry {
            name: leaf,
            is_dir: kind == libc::S_IFDIR,
            size: (kind == libc::S_IFREG).then_some(metadata.st_size.max(0) as u64),
        });
    }
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(entries)
}
/// The committed bytes of `path`, or `None` when `HEAD` holds no blob there
/// (or there is no commit yet, as on the desktop). `ls-tree` answers absence
/// without an error, so only a real Git failure surfaces as one.
fn read_head(root: &File, path: &str, alive: &dyn Fn() -> bool) -> Result<Option<Vec<u8>>, String> {
    relative(path)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let listing = match git_output(
        root,
        &["--literal-pathspecs", "ls-tree", "-z", "HEAD", "--", path],
        alive,
        deadline,
        64 * 1024,
    ) {
        Ok(listing) => listing,
        Err(error) => {
            // An unborn branch: a repository whose HEAD names no commit yet
            // has no baseline, which is not a failure.
            let in_repo = git_output(
                root,
                &["rev-parse", "--is-inside-work-tree"],
                alive,
                deadline,
                64,
            )
            .is_ok();
            let verify_failed = in_repo
                && git_output(
                    root,
                    &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
                    alive,
                    deadline,
                    128,
                )
                .is_err();
            // A check cut short by the deadline (or a retired authority) says
            // nothing about HEAD: that is the original failure, not "unborn".
            let unborn = verify_failed && alive() && Instant::now() < deadline;
            return if unborn { Ok(None) } else { Err(error) };
        }
    };
    // The entry for exactly `path` (ls-tree also answers a directory's name).
    let Some(meta) = listing
        .split(|byte| *byte == 0)
        .filter_map(|r| std::str::from_utf8(r).ok())
        .filter_map(|r| r.split_once('\t'))
        .find(|(_, entry)| *entry == path)
        .map(|(meta, _)| meta)
    else {
        return Ok(None);
    };
    let mut fields = meta.split(' ');
    let (mode, kind, object) = (fields.next(), fields.next(), fields.next());
    // A symlink's blob is its target, not file content to diff against.
    if mode == Some("120000") {
        return Ok(None);
    }
    let (Some("blob"), Some(object)) = (kind, object) else {
        return Ok(None);
    };
    if !object.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid Git object name".into());
    }
    // Size first: a committed blob past what one reply carries has no
    // baseline (as on the local side), rather than failing the request.
    let size = git_output(root, &["cat-file", "-s", object], alive, deadline, 64)?;
    let size: u64 = std::str::from_utf8(&size)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .ok_or_else(|| "unreadable Git object size".to_string())?;
    if size > MAX_FILE_BYTES as u64 {
        return Ok(None);
    }
    git_output(
        root,
        &["cat-file", "blob", object],
        alive,
        deadline,
        MAX_FILE_BYTES,
    )
    .map(Some)
}
/// Tracked plus untracked-but-not-ignored paths, truncated at `limit`.
fn list_files(
    root: &File,
    limit: usize,
    alive: &dyn Fn() -> bool,
) -> Result<(Vec<String>, bool), String> {
    if limit == 0 || limit > MAX_PATHS {
        return Err("invalid file list limit".into());
    }
    let bytes = git_output(
        root,
        &[
            "--literal-pathspecs",
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
        alive,
        Instant::now() + Duration::from_secs(30),
        LIST_BYTES,
    )?;
    let mut paths = Vec::new();
    for path in bytes.split(|byte| *byte == 0).filter(|p| !p.is_empty()) {
        // A non-UTF8 name cannot be opened through this protocol; skip it
        // rather than failing the whole list.
        let Ok(path) = std::str::from_utf8(path) else {
            continue;
        };
        if relative(path).is_ok() {
            paths.push(path.to_string());
        }
    }
    // Tracked files deleted from the working tree are still in the index;
    // opening one could only fail, so the editor isn't offered them.
    if let Ok(deleted) = git_output(
        root,
        &["--literal-pathspecs", "ls-files", "-z", "--deleted"],
        alive,
        Instant::now() + Duration::from_secs(30),
        LIST_BYTES,
    ) {
        let deleted: std::collections::HashSet<&[u8]> = deleted
            .split(|byte| *byte == 0)
            .filter(|p| !p.is_empty())
            .collect();
        if !deleted.is_empty() {
            paths.retain(|p| !deleted.contains(p.as_bytes()));
        }
    }
    paths.sort();
    paths.dedup();
    let truncated = paths.len() > limit;
    paths.truncate(limit);
    Ok((paths, truncated))
}
fn tracked(
    root: &File,
    prefix: Option<&str>,
    limit: usize,
    alive: &dyn Fn() -> bool,
) -> Result<Vec<String>, String> {
    if limit == 0 || limit > MAX_PATHS {
        return Err("invalid tracked path limit".into());
    }
    let mut args = vec!["--literal-pathspecs", "ls-files", "-z", "--cached", "--"];
    if let Some(prefix) = prefix {
        relative(prefix)?;
        args.push(prefix);
    }
    let bytes = git_output(
        root,
        &args,
        alive,
        Instant::now() + Duration::from_secs(30),
        LIST_BYTES,
    )?;
    let mut paths = Vec::new();
    for path in bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = std::str::from_utf8(path).map_err(|_| "non-UTF8 tracked path")?;
        relative(path)?;
        if paths.len() >= limit {
            return Err("tracked paths exceed limit; result is incomplete".into());
        }
        paths.push(path.to_string());
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}
const EXCLUDE_START: &str = "# >>> codesense managed";
const EXCLUDE_END: &str = "# <<< codesense managed";
fn exclude_parent(repo_path: &str, create: bool) -> Result<Option<File>, String> {
    let repo = open_directory(repo_path, false)?;
    owner_directory(&repo)?;
    let git = directory(&repo, ".git", false)?;
    owner_directory(&git)?;
    if !exists(&git, "info")? && !create {
        return Ok(None);
    }
    let info = directory(&git, "info", create)?;
    owner_directory(&info)?;
    if create {
        io(durable::sync(&git))?;
    }
    Ok(Some(info))
}
fn exclude_read(repo_path: &str) -> Result<(Observation, Vec<u8>), String> {
    match exclude_parent(repo_path, false)? {
        Some(info) => observation_at(&info, "exclude"),
        None => Ok((Observation::Absent, Vec::new())),
    }
}
fn exclude_block(bytes: &[u8]) -> Result<(String, String, Vec<String>), String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "managed exclude is not UTF8")?;
    let mut start = None;
    let mut end = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let content = line.trim();
        if content == EXCLUDE_START {
            if start.is_some() || end.is_some() {
                return Err("managed exclude markers are ambiguous".into());
            }
            start = Some((offset, offset + line.len()));
        }
        if content == EXCLUDE_END {
            if end.is_some() || start.is_none() {
                return Err("managed exclude markers are ambiguous".into());
            }
            end = Some((offset, offset + line.len()));
        }
        offset += line.len();
    }
    match (start, end) {
        (None, None) => Ok((text.to_string(), String::new(), Vec::new())),
        (Some((before, content)), Some((end, after))) if content <= end => Ok((
            text[..before].into(),
            text[after..].into(),
            text[content..end]
                .lines()
                .map(str::trim_end)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect(),
        )),
        _ => Err("managed exclude markers are incomplete".into()),
    }
}
fn exclude_bytes(bytes: &[u8], lines: &[String]) -> Result<Vec<u8>, String> {
    if lines.len() > MAX_PATHS
        || lines.iter().any(|line| {
            line.is_empty()
                || line.contains(['\n', '\r', '\0'])
                || line == EXCLUDE_START
                || line == EXCLUDE_END
        })
    {
        return Err("invalid managed exclude lines".into());
    }
    let (mut before, after, _) = exclude_block(bytes)?;
    if !before.is_empty() && !before.ends_with('\n') {
        before.push('\n');
    }
    if !lines.is_empty() {
        before.push_str(EXCLUDE_START);
        before.push('\n');
        for line in lines {
            before.push_str(line);
            before.push('\n');
        }
        before.push_str(EXCLUDE_END);
        before.push('\n');
    }
    before.push_str(&after);
    if before.len() > MAX_FILE_BYTES {
        return Err("managed exclude exceeds limit".into());
    }
    Ok(before.into_bytes())
}
/// One marker block per workspace in the clone's shared
/// exclude file. Every byte outside this workspace's block is preserved.
fn workspace_markers(workspace_id: &str) -> (String, String) {
    (
        format!("{EXCLUDE_START} workspace {workspace_id}"),
        format!("{EXCLUDE_END} workspace {workspace_id}"),
    )
}
/// A workspace-relative file path as an anchored exclude pattern.
pub(super) fn exclude_pattern(path: &str) -> String {
    let mut pattern = String::with_capacity(path.len() + 2);
    pattern.push('/');
    for character in path.chars() {
        if matches!(character, '\\' | '*' | '?' | '[' | ']') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    if pattern.ends_with(' ') {
        let trimmed = pattern.trim_end_matches(' ').len();
        let spaces = pattern.len() - trimmed;
        pattern.truncate(trimmed);
        for _ in 0..spaces {
            pattern.push_str("\\ ");
        }
    }
    pattern
}
fn pattern_path(line: &str) -> Option<String> {
    let body = line.strip_prefix('/')?;
    let mut path = String::with_capacity(body.len());
    let mut escaped = false;
    for character in body.chars() {
        if escaped {
            path.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            path.push(character);
        }
    }
    Some(path)
}
/// What this workspace's block in the clone's shared exclude file
/// lists, read without any lock (a manifest reads; the desktop's later write
/// compares-and-swaps against what it saw).
pub(super) fn workspace_exclude_state(
    repo_path: &str,
    workspace_id: &str,
) -> codesense_pty_core::remote_manifest::WorkspaceExclude {
    use codesense_pty_core::remote_manifest::WorkspaceExclude;
    let edited = |reason: String| WorkspaceExclude::HandEdited { reason };
    let bytes = match exclude_read(repo_path) {
        Ok((_, bytes)) => bytes,
        Err(error) => return edited(error),
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return edited("the exclude file is not UTF-8".into());
    };
    match workspace_block(text, workspace_id) {
        Ok(block) if block.foreign == 0 => WorkspaceExclude::Paths {
            paths: sorted_paths(&block.paths),
        },
        Ok(block) => edited(format!(
            "{} line(s) in this workspace's block were not written by CodeSense",
            block.foreign
        )),
        Err(error) => edited(error),
    }
}
/// One workspace's block in the shared exclude file, and the text around it.
pub(super) struct WorkspaceBlock {
    before: String,
    after: String,
    pub(super) paths: Vec<String>,
    /// Lines inside the block this code never writes (a hand edit).
    pub(super) foreign: usize,
}
/// This workspace's block. `Err` only when its markers are broken (doubled,
/// unbalanced); a write then conflicts, and only an explicit repair replaces
/// it.
pub(super) fn workspace_block(text: &str, workspace_id: &str) -> Result<WorkspaceBlock, String> {
    let (start_marker, end_marker) = workspace_markers(workspace_id);
    let mut start = None;
    let mut end = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let content = line.trim_end_matches(['\n', '\r']);
        if content == start_marker {
            if start.is_some() || end.is_some() {
                return Err("workspace exclude markers are ambiguous".into());
            }
            start = Some((offset, offset + line.len()));
        } else if content == end_marker {
            if end.is_some() || start.is_none() {
                return Err("workspace exclude markers are ambiguous".into());
            }
            end = Some((offset, offset + line.len()));
        }
        offset += line.len();
    }
    match (start, end) {
        (None, None) => Ok(WorkspaceBlock {
            before: text.to_string(),
            after: String::new(),
            paths: Vec::new(),
            foreign: 0,
        }),
        (Some((before, content)), Some((end, after))) if content <= end => {
            let mut paths = Vec::new();
            let mut foreign = 0;
            for line in text[content..end].lines() {
                let line = line.trim_end_matches('\r');
                if line.is_empty() {
                    continue;
                }
                match pattern_path(line) {
                    Some(path) => paths.push(path),
                    None => foreign += 1,
                }
            }
            Ok(WorkspaceBlock {
                before: text[..before].into(),
                after: text[after..].into(),
                paths,
                foreign,
            })
        }
        _ => Err("workspace exclude markers are incomplete".into()),
    }
}
/// For a repair: the text with every line of this workspace's markers and
/// everything between its first start and last end marker removed.
fn without_workspace_block(text: &str, workspace_id: &str) -> WorkspaceBlock {
    let (start_marker, end_marker) = workspace_markers(workspace_id);
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let content = |line: &str| line.trim_end_matches(['\n', '\r']).to_string();
    let first = lines.iter().position(|line| content(line) == start_marker);
    // The block is its first start marker through the first end marker after
    // it: nothing beyond this workspace's own block is ever removed.
    let last = first.and_then(|first| {
        lines[first..]
            .iter()
            .position(|line| content(line) == end_marker)
            .map(|offset| first + offset)
    });
    let (before, after) = match (first, last) {
        (Some(first), Some(last)) if first <= last => {
            (lines[..first].concat(), lines[last + 1..].concat())
        }
        _ => (
            lines
                .iter()
                .filter(|line| {
                    let line = content(line);
                    line != start_marker && line != end_marker
                })
                .copied()
                .collect::<String>(),
            String::new(),
        ),
    };
    WorkspaceBlock {
        before,
        after,
        paths: Vec::new(),
        foreign: 0,
    }
}
fn sorted_paths(paths: &[String]) -> Vec<String> {
    let mut paths = paths.to_vec();
    paths.sort();
    paths.dedup();
    paths
}
pub(super) fn workspace_exclude_bytes(
    block: WorkspaceBlock,
    workspace_id: &str,
    paths: &[String],
    spelling: &dyn Fn(&str) -> Result<String, String>,
) -> Result<Vec<u8>, String> {
    let WorkspaceBlock {
        mut before, after, ..
    } = block;
    if paths.len() > MAX_PATHS {
        return Err("invalid workspace exclude paths".into());
    }
    if !paths.is_empty() {
        if !before.is_empty() && !before.ends_with('\n') {
            before.push('\n');
        }
        let (start, end) = workspace_markers(workspace_id);
        before.push_str(&start);
        before.push('\n');
        for path in sorted_paths(paths) {
            relative(&path)?;
            before.push_str(&exclude_pattern(&spelling(&path)?));
            before.push('\n');
        }
        before.push_str(&end);
        before.push('\n');
    }
    before.push_str(&after);
    if before.len() > MAX_FILE_BYTES {
        return Err("managed exclude exceeds limit".into());
    }
    Ok(before.into_bytes())
}
fn execute(
    workspace: &Workspace,
    request: &FileRequest,
    alive: &dyn Fn() -> bool,
) -> Result<EffectOutcome, String> {
    match &request.operation {
        FileOperation::Apply {
            operation_id,
            path,
            expected,
            effect,
        } => match parent_at(&workspace.root, path)? {
            Some((parent, leaf)) => conditional_file(
                &parent,
                &leaf,
                expected,
                effect,
                operation_id,
                alive,
                Durability::Now,
            ),
            // No parent directory: the file to remove is already gone.
            None if matches!(effect, Effect::Remove) => Ok(applied(Observation::Absent)),
            None => Ok(EffectOutcome::Conflict {
                observation: Observation::Absent,
            }),
        },
        FileOperation::EnsureDirectories { operation_id, path } => {
            directory_effect(&workspace.root, path, operation_id, alive)
        }
        FileOperation::ReplaceManagedExclude {
            operation_id,
            expected,
            lines,
        } => {
            let (current, bytes) = exclude_read(&workspace.repo_path)?;
            if !expected_matches(expected, &current) {
                return Ok(EffectOutcome::Conflict {
                    observation: current,
                });
            }
            let bytes = exclude_bytes(&bytes, lines)?;
            if !alive() {
                return Ok(unknown(operation_id));
            }
            let info = exclude_parent(&workspace.repo_path, true)?
                .ok_or("Git exclude directory unavailable")?;
            conditional_file(
                &info,
                "exclude",
                expected,
                &Effect::Write { bytes, mode: None },
                operation_id,
                alive,
                Durability::Now,
            )
        }
        FileOperation::ReplaceWorkspaceExclude {
            operation_id,
            workspace_id,
            expected,
            paths,
            repair,
        } => {
            // The caller holds the clone's exclude leaf lock: nothing else
            // writes the file between this read and the conditional write.
            let (current, bytes) = exclude_read(&workspace.repo_path)?;
            let conflict = || {
                Ok(EffectOutcome::Conflict {
                    observation: current.clone(),
                })
            };
            if !matches!(current, Observation::Absent | Observation::Regular { .. }) {
                return conflict();
            }
            let Ok(text) = std::str::from_utf8(&bytes) else {
                return conflict();
            };
            // A hand-edited block (a foreign line, broken markers) is a
            // conflict, never an error; an explicit repair replaces it.
            let block = if *repair {
                without_workspace_block(text, workspace_id)
            } else {
                match workspace_block(text, workspace_id) {
                    Ok(block)
                        if block.foreign == 0
                            && nfc_sorted(&block.paths) == nfc_sorted(expected) =>
                    {
                        block
                    }
                    _ => return conflict(),
                }
            };
            let next = workspace_exclude_bytes(block, workspace_id, paths, &|path| {
                disk_spelling(&workspace.root, path)
            })?;
            if next == bytes && current != Observation::Absent {
                return Ok(applied(current));
            }
            if !alive() {
                return Ok(unknown(operation_id));
            }
            let info = exclude_parent(&workspace.repo_path, true)?
                .ok_or("Git exclude directory unavailable")?;
            conditional_file(
                &info,
                "exclude",
                &expected_for(&current)?,
                &Effect::Write {
                    bytes: next,
                    mode: None,
                },
                operation_id,
                alive,
                Durability::Now,
            )
        }
        _ => Err("operation is not a file mutation".into()),
    }
}
fn recover(workspace: &Workspace, receipt: &FileReceipt) -> Result<EffectOutcome, String> {
    let id = operation_id(&receipt.request.operation).ok_or("missing operation ID")?;
    if let Some(outcome) = &receipt.outcome {
        return Ok(outcome.clone());
    }
    let observed = match &receipt.request.operation {
        FileOperation::Apply {
            path,
            expected,
            effect,
            ..
        } => {
            let (observed, _) = observe(&workspace.root, path)?;
            if !effect_matches(effect, expected, &observed) {
                return Ok(unknown(id));
            }
            if let Some((parent, leaf)) = parent_at(&workspace.root, path)? {
                if !matches!(observed, Observation::Absent) {
                    io(durable::sync(&open(
                        &parent,
                        &leaf,
                        libc::O_RDONLY | libc::O_NONBLOCK,
                    )?))?;
                }
                io(durable::sync(&parent))?;
            }
            observed
        }
        FileOperation::EnsureDirectories { path, .. } => {
            let (observed, _) = observe(&workspace.root, path)?;
            if observed != Observation::Directory {
                return Ok(unknown(id));
            }
            let mut dir = io(workspace.root.try_clone())?;
            for part in relative(path)? {
                io(durable::sync(&dir))?;
                dir = directory(&dir, part, false)?;
            }
            io(durable::sync(&dir))?;
            observed
        }
        FileOperation::ReplaceManagedExclude { lines, .. } => {
            let (observed, bytes) = exclude_read(&workspace.repo_path)?;
            let (_, _, actual) = exclude_block(&bytes)?;
            if &actual != lines {
                return Ok(unknown(id));
            }
            let Some(info) = exclude_parent(&workspace.repo_path, false)? else {
                return Ok(unknown(id));
            };
            io(durable::sync(&open(
                &info,
                "exclude",
                libc::O_RDONLY | libc::O_NONBLOCK,
            )?))?;
            io(durable::sync(&info))?;
            observed
        }
        FileOperation::ReplaceWorkspaceExclude {
            workspace_id,
            paths,
            ..
        } => {
            let (observed, bytes) = exclude_read(&workspace.repo_path)?;
            let text = std::str::from_utf8(&bytes).map_err(|_| "managed exclude is not UTF8")?;
            let Ok(actual) = workspace_block(text, workspace_id) else {
                return Ok(unknown(id));
            };
            if actual.foreign != 0 || nfc_sorted(&actual.paths) != nfc_sorted(paths) {
                return Ok(unknown(id));
            }
            if let Some(info) = exclude_parent(&workspace.repo_path, false)? {
                if observed != Observation::Absent {
                    io(durable::sync(&open(
                        &info,
                        "exclude",
                        libc::O_RDONLY | libc::O_NONBLOCK,
                    )?))?;
                }
                io(durable::sync(&info))?;
            }
            observed
        }
        _ => return Err("invalid pending file operation".into()),
    };
    Ok(applied(observed))
}

pub(crate) fn handle(
    root: &Path,
    owner: u64,
    request: FileRequest,
    alive: impl Fn() -> bool,
) -> Result<FileReply, String> {
    handle_inner(root, owner, request, &alive)
}

fn batch_directory_id(
    target: &FileTarget,
    operation_id: &str,
    parent: &str,
) -> Result<String, String> {
    let identity =
        serde_json::to_vec(&("selected-batch-directory-v1", target, operation_id, parent))
            .map_err(|_| "invalid directory intent")?;
    let digest = blake3::hash(&identity);
    let mut id = [0u8; 16];
    id.copy_from_slice(&digest.as_bytes()[..16]);
    Ok(uuid::Uuid::from_bytes(id).to_string())
}

/// Share the ordinary file owner's durable reconciliation boundary with batch
/// status. Recovered proof must survive later legitimate workspace edits; a
/// receipt-save failure is not permission to report a successful outcome.
fn reconcile_receipt(
    workspace: &Workspace,
    key: &str,
    mut receipt: FileReceipt,
    operation_id: &str,
    alive: &dyn Fn() -> bool,
) -> Result<EffectOutcome, String> {
    if !alive() {
        return Ok(unknown(operation_id));
    }
    let outcome = recover(workspace, &receipt).unwrap_or_else(|_| unknown(operation_id));
    if !matches!(outcome, EffectOutcome::Unknown { .. }) && receipt.outcome.is_none() {
        receipt.outcome = Some(outcome.clone());
        save_receipt(&workspace.ledger, key, &receipt, false)?;
    }
    Ok(if alive() {
        outcome
    } else {
        unknown(operation_id)
    })
}

/// Status does not dispatch; a reused UUID must still name this exact member,
/// not merely any previously successful file operation in the workspace.
pub(super) fn batch_receipt(
    root: &Path,
    owner: u64,
    target: &FileTarget,
    item: &codesense_pty_core::context_batch::BatchItem,
    alive: &dyn Fn() -> bool,
    located: &Located,
) -> Result<EffectOutcome, String> {
    // The caller holds the workspace lock.
    let workspace = resolve_at(root, owner, target, located.clone(), alive)?;
    let key = ledger_key(target, &item.operation_id)?;
    if exists(&workspace.ledger, &key)? {
        let receipt: FileReceipt = serde_json::from_slice(&read_record(&workspace.ledger, &key)?)
            .map_err(|_| "invalid member receipt")?;
        let identity = receipt.write_identity();
        let matches = match &receipt.request.operation {
            FileOperation::Apply {
                operation_id,
                path,
                expected,
                effect: Effect::Write { mode, .. },
            } => {
                operation_id == &item.operation_id
                    && path == &item.path
                    && expected == &item.expected
                    && *mode == Some(item.mode)
                    && identity == Some((item.length, item.hash))
            }
            _ => false,
        };
        if !matches
            || receipt.request.target.owner_id != target.owner_id
            || receipt.request.target.origin_key != target.origin_key
            || receipt.request.target.handle != target.handle
        {
            return Err("batch member UUID belongs to different immutable file intent".into());
        }
        return reconcile_receipt(&workspace, &key, receipt, &item.operation_id, alive);
    }
    if let Some((parent, _)) = item.path.rsplit_once('/') {
        let directory_id = batch_directory_id(target, &item.operation_id, parent)?;
        let key = ledger_key(target, &directory_id)?;
        if exists(&workspace.ledger, &key)? {
            let receipt: FileReceipt =
                serde_json::from_slice(&read_record(&workspace.ledger, &key)?)
                    .map_err(|_| "invalid directory receipt")?;
            if receipt.request.target != *target
                || receipt.request.operation
                    != (FileOperation::EnsureDirectories {
                        operation_id: directory_id.clone(),
                        path: parent.into(),
                    })
            {
                return Err("batch directory receipt intent differs".into());
            }
            match reconcile_receipt(&workspace, &key, receipt, &directory_id, alive)? {
                EffectOutcome::Applied { .. } => {}
                EffectOutcome::Conflict { observation } => {
                    return Ok(EffectOutcome::Conflict { observation })
                }
                _ => return Ok(unknown(&item.operation_id)),
            }
        }
    }
    Ok(if alive() {
        EffectOutcome::NotStarted {
            operation_id: item.operation_id.clone(),
        }
    } else {
        unknown(&item.operation_id)
    })
}

/// The conditional effects of one batch Apply call (or the checks
/// of one Status) against one resolved workspace. Effects order their data
/// before their rename and mark the directories whose entries changed;
/// [`BatchEffects::seal`] syncs each of those once. Nothing here writes a
/// per-operation receipt: the caller journals the outcomes. The caller holds
/// the workspace lock.
pub(super) struct BatchEffects {
    workspace: Workspace,
    target: FileTarget,
    /// Directories opened this call, by workspace-relative path ("" is the
    /// worktree itself); at most [`DIRECTORY_CACHE`] besides the worktree.
    directories: std::collections::HashMap<String, File>,
    /// Directories whose entries changed this call, synced once at seal.
    touched: std::collections::BTreeSet<String>,
}
/// Open directory handles kept per call (macOS's soft descriptor limit is 256).
const DIRECTORY_CACHE: usize = 64;
enum Parent {
    Ready(String),
    Missing,
    Conflict(Observation),
}
impl BatchEffects {
    pub(super) fn open(
        root: &Path,
        owner: u64,
        target: &FileTarget,
        located: &Located,
        alive: &dyn Fn() -> bool,
    ) -> Result<Self, String> {
        let workspace = resolve_at(root, owner, target, located.clone(), alive)?;
        let mut directories = std::collections::HashMap::new();
        directories.insert(String::new(), io(workspace.root.try_clone())?);
        Ok(Self {
            workspace,
            target: target.clone(),
            directories,
            touched: Default::default(),
        })
    }
    /// The directory at `path` (workspace-relative, "" for the worktree),
    /// creating missing components when `create`. A component that is not a
    /// directory of ours is a conflict, never followed.
    fn directory(&mut self, path: &str, create: bool) -> Result<Parent, String> {
        if self.directories.contains_key(path) {
            return Ok(Parent::Ready(path.into()));
        }
        if self.directories.len() > DIRECTORY_CACHE {
            self.directories.retain(|path, _| path.is_empty());
        }
        let mut at = String::new();
        let mut current = io(self.workspace.root.try_clone())?;
        for part in relative(path)? {
            let next = if at.is_empty() {
                part.to_string()
            } else {
                format!("{at}/{part}")
            };
            current = match self.directories.get(&next) {
                Some(cached) => io(cached.try_clone())?,
                None => {
                    let (observation, _) = observation_at(&current, part)?;
                    let created = match observation {
                        Observation::Directory => false,
                        Observation::Absent if create => true,
                        Observation::Absent => return Ok(Parent::Missing),
                        other => return Ok(Parent::Conflict(other)),
                    };
                    let opened = directory(&current, part, created)?;
                    if owner_directory(&opened).is_err() {
                        return Ok(Parent::Conflict(Observation::Unsafe));
                    }
                    if created {
                        self.touched.insert(at.clone());
                    }
                    opened
                }
            };
            at = next;
        }
        if !self.directories.contains_key(&at) {
            self.directories.insert(at.clone(), current);
        }
        Ok(Parent::Ready(at))
    }
    /// One write, removal or mode change. An error stops the Apply call (it
    /// still syncs and journals what landed; the next Apply evaluates this
    /// member again).
    pub(super) fn effect(
        &mut self,
        operation_id: &str,
        path: &str,
        expected: &Expected,
        effect: &Effect,
        alive: &dyn Fn() -> bool,
    ) -> Result<EffectOutcome, String> {
        let parts = relative(path)?;
        let parent_path = parts[..parts.len() - 1].join("/");
        let create = matches!(effect, Effect::Write { .. });
        let parent = match self.directory(&parent_path, create)? {
            Parent::Ready(parent) => parent,
            // No parent: the file to remove is already gone, and there is no
            // file whose mode could change.
            Parent::Missing if matches!(effect, Effect::Remove) => {
                return Ok(applied(Observation::Absent))
            }
            Parent::Missing => {
                return Ok(EffectOutcome::Conflict {
                    observation: Observation::Absent,
                })
            }
            Parent::Conflict(observation) => return Ok(EffectOutcome::Conflict { observation }),
        };
        let directory = &self.directories[&parent];
        let leaf = resolve_leaf(directory, parts[parts.len() - 1])?;
        let mut did = Landed::default();
        let outcome = {
            let _writing = locks::writing(&self.workspace.worktree_path);
            conditional_file(
                directory,
                &leaf,
                expected,
                effect,
                operation_id,
                alive,
                Durability::Later(&mut did),
            )?
        };
        let landed = matches!(outcome, EffectOutcome::Applied { .. });
        let touched = did.entries;
        if !did.entries && !did.ordered && landed {
            // Applied with no effect of ours (it landed before a cut, or two
            // launches raced): make it durable before it is journaled.
            self.settle_existing(directory_clone(directory)?, &leaf)?;
        }
        if touched || (landed && !did.ordered) {
            self.touched.insert(parent);
        }
        Ok(outcome)
    }
    /// Order `leaf` (when present) so a rename an earlier process made is
    /// durable once its directory is synced at seal.
    fn settle_existing(&self, directory: File, leaf: &str) -> Result<(), String> {
        if exists(&directory, leaf)? {
            if let Ok(file) = open(&directory, leaf, libc::O_RDONLY | libc::O_NONBLOCK) {
                io(durable::order(&file))?;
            }
        }
        Ok(())
    }
    /// An `ensure_dirs` member: the directory and every missing parent.
    pub(super) fn ensure_directory(
        &mut self,
        operation_id: &str,
        path: &str,
    ) -> Result<EffectOutcome, String> {
        Ok(match self.directory(path, true)? {
            Parent::Ready(_) => {
                // An existing directory from a cut call: its own entry in its
                // parent is made durable too.
                if let Some((parent, _)) = path.rsplit_once('/') {
                    self.touched.insert(parent.into());
                } else {
                    self.touched.insert(String::new());
                }
                applied(Observation::Directory)
            }
            Parent::Conflict(observation) => EffectOutcome::Conflict { observation },
            Parent::Missing => unknown(operation_id),
        })
    }
    /// Make an outcome Status found on disk durable before it is
    /// journaled: the file ordered, its directory synced at seal.
    pub(super) fn settle(&mut self, path: &str) -> Result<(), String> {
        let parts = relative(path)?;
        let parent_path = parts[..parts.len() - 1].join("/");
        if let Parent::Ready(parent) = self.directory(&parent_path, false)? {
            let directory = directory_clone(&self.directories[&parent])?;
            self.settle_existing(directory, parts[parts.len() - 1])?;
            self.touched.insert(parent);
        }
        Ok(())
    }
    /// P2: each directory whose entries changed, synced once. Not gated on
    /// liveness: what landed is made durable before anything is journaled.
    pub(super) fn seal(&mut self) -> Result<(), String> {
        // A directory leaves the set only once synced: after a failed sync the
        // next seal tries it again, and nothing under it is journaled first.
        for path in self.touched.clone() {
            let directory = match self.directories.get(&path) {
                Some(directory) => Some(directory_clone(directory)?),
                None => match self.directory(&path, false)? {
                    Parent::Ready(path) => Some(directory_clone(&self.directories[&path])?),
                    // Gone since (a later effect removed it): nothing to sync.
                    _ => None,
                },
            };
            if let Some(directory) = directory {
                io(durable::order(&directory))?;
            }
            self.touched.remove(&path);
        }
        Ok(())
    }
    /// What is at `path` now (read-only; a path under a non-directory reads
    /// as absent).
    ///
    /// Resolved exactly as Apply resolves it: a parent component that is not
    /// a directory of ours is `None` (Apply would report a conflict, so
    /// nothing there has landed), a missing parent reads `Absent`, and the
    /// leaf is found by its NFC spelling.
    pub(super) fn observe(&self, path: &str) -> Result<Option<Observation>, String> {
        let parts = relative(path)?;
        let mut current = io(self.workspace.root.try_clone())?;
        for part in &parts[..parts.len() - 1] {
            match observation_at(&current, part)?.0 {
                Observation::Directory => {}
                Observation::Absent => return Ok(Some(Observation::Absent)),
                _ => return Ok(None),
            }
            let next = directory(&current, part, false)?;
            if owner_directory(&next).is_err() {
                return Ok(None);
            }
            current = next;
        }
        let leaf = resolve_leaf(&current, parts[parts.len() - 1])?;
        Ok(Some(observation_at(&current, &leaf)?.0))
    }
    /// Whether an older server left a per-operation receipt for this ID (a
    /// batch it began, resumed here).
    pub(super) fn has_receipt(&self, operation_id: &str) -> Result<bool, String> {
        exists(
            &self.workspace.ledger,
            &ledger_key(&self.target, operation_id)?,
        )
    }
}
fn directory_clone(directory: &File) -> Result<File, String> {
    io(directory.try_clone())
}

/// Revalidate live workspace authority under the caller's workspace lock,
/// then lend a held private ledger descriptor to staging (never the workspace
/// root).
pub(super) fn batch_ledger(
    root: &Path,
    owner: u64,
    target: &FileTarget,
    located: &Located,
    alive: &dyn Fn() -> bool,
) -> Result<File, String> {
    Ok(resolve_at(root, owner, target, located.clone(), alive)?.ledger)
}

/// How long a settled receipt is kept for Reconcile / Status by its ID.
pub(super) const RECEIPT_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Retention: drop the bytes of these settled writes (keeping their
/// digest), then, at most once an hour, prune settled receipts older than
/// [`RECEIPT_RETENTION`] from the shared ledger. The caller holds the
/// workspace lock.
pub(super) fn compact_receipts(
    root: &Path,
    owner: u64,
    target: &FileTarget,
    operation_ids: &[&str],
    alive: &dyn Fn() -> bool,
    located: &Located,
) -> Result<(), String> {
    let workspace = resolve_at(root, owner, target, located.clone(), alive)?;
    for id in operation_ids {
        let key = ledger_key(target, id)?;
        if !exists(&workspace.ledger, &key)? {
            continue;
        }
        let mut receipt: FileReceipt =
            serde_json::from_slice(&read_record(&workspace.ledger, &key)?)
                .map_err(|_| "invalid member receipt")?;
        if receipt.compact() {
            save_receipt(&workspace.ledger, &key, &receipt, false)?;
        }
    }
    prune_ledger(&workspace.ledger, target)
}
/// Settled receipts of `target`'s workspace older than the retention, at most
/// once an hour per workspace.
fn prune_ledger(ledger: &File, target: &FileTarget) -> Result<(), String> {
    type Throttle = std::sync::Mutex<std::collections::HashMap<String, Instant>>;
    static LAST: std::sync::OnceLock<Throttle> = std::sync::OnceLock::new();
    {
        let mut last = LAST
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let key = format!(
            "{}:{}:{}",
            target.owner_id, target.origin_key, target.handle
        );
        if last
            .get(&key)
            .is_some_and(|at| at.elapsed() < Duration::from_secs(60 * 60))
        {
            return Ok(());
        }
        last.insert(key, Instant::now());
    }
    for name in children(ledger)? {
        if name.len() != 69 || !name.ends_with(".json") {
            continue;
        }
        let Ok(file) = open(ledger, &name, libc::O_RDONLY | libc::O_NONBLOCK) else {
            continue;
        };
        let old = file
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > RECEIPT_RETENTION);
        if !old {
            continue;
        }
        let settled = read_record(ledger, &name)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<FileReceipt>(&bytes).ok())
            .is_some_and(|receipt| {
                receipt.outcome.is_some()
                    && receipt.request.target.owner_id == target.owner_id
                    && receipt.request.target.origin_key == target.origin_key
                    && receipt.request.target.handle == target.handle
            });
        if settled {
            let component = self::name(&name)?;
            // SAFETY: a validated receipt file directly under the held ledger.
            unsafe { libc::unlinkat(ledger.as_raw_fd(), component.as_ptr(), 0) };
        }
    }
    io(durable::sync(ledger))
}

/// One extension effect of a selected batch (a removal, a mode
/// change, a directory or the workspace exclude block), through the same
/// durable receipt owner as every file effect. The caller holds the workspace
/// lock and, for the exclude block, the clone's exclude leaf lock.
pub(super) fn batch_effect(
    root: &Path,
    owner: u64,
    request: FileRequest,
    alive: &dyn Fn() -> bool,
    located: &Located,
) -> Result<EffectOutcome, String> {
    let id = operation_id(&request.operation)
        .ok_or("batch operation is not a file effect")?
        .to_string();
    match effect_locked(root, owner, request, alive, &id, located)? {
        FileReply::Effect { outcome } => Ok(outcome),
        _ => Err("unexpected batch effect response".into()),
    }
}

/// The durable outcome of one extension effect, without dispatching it:
/// `NotStarted` when its ID was never accepted. `same` decides whether a
/// retained intent is this exact operation; a reused UUID naming anything else
/// is refused.
pub(super) fn batch_operation_receipt(
    root: &Path,
    owner: u64,
    target: &FileTarget,
    operation_id: &str,
    same: &dyn Fn(&FileOperation) -> bool,
    alive: &dyn Fn() -> bool,
    located: &Located,
) -> Result<EffectOutcome, String> {
    let workspace = resolve_at(root, owner, target, located.clone(), alive)?;
    let key = ledger_key(target, operation_id)?;
    if !exists(&workspace.ledger, &key)? {
        return Ok(if alive() {
            EffectOutcome::NotStarted {
                operation_id: operation_id.into(),
            }
        } else {
            unknown(operation_id)
        });
    }
    let receipt: FileReceipt = serde_json::from_slice(&read_record(&workspace.ledger, &key)?)
        .map_err(|_| "invalid batch operation receipt")?;
    if receipt.request.target != *target || !same(&receipt.request.operation) {
        return Err("batch operation UUID belongs to different immutable file intent".into());
    }
    reconcile_receipt(&workspace, &key, receipt, operation_id, alive)
}

fn handle_inner(
    root: &Path,
    owner: u64,
    request: FileRequest,
    alive: &dyn Fn() -> bool,
) -> Result<FileReply, String> {
    if let Some(id) = operation_id(&request.operation).map(str::to_string) {
        let located = match locate(root, owner, &request.target, alive) {
            Ok(located) => located,
            Err(_) => {
                return Ok(FileReply::Effect {
                    outcome: unknown(&id),
                })
            }
        };
        let Ok(_workspace) = locks::workspace(&located.worktree_path, alive) else {
            return Ok(FileReply::Effect {
                outcome: unknown(&id),
            });
        };
        // The exclude block is one file in the clone, shared by every worktree
        // of the repository: its write takes the clone's short exclude lock (a
        // leaf), never the repository lock a long prepare holds.
        let _exclude = match request.operation {
            FileOperation::ReplaceManagedExclude { .. }
            | FileOperation::ReplaceWorkspaceExclude { .. } => {
                match locks::exclude(&located.repo_path, alive) {
                    Ok(held) => Some(held),
                    Err(_) => {
                        return Ok(FileReply::Effect {
                            outcome: unknown(&id),
                        })
                    }
                }
            }
            _ => None,
        };
        return effect_locked(root, owner, request, alive, &id, &located);
    }
    let (worktree, located) = resolve_reader(root, owner, &request.target, alive)?;
    locks::consistent(&located.worktree_path, || {
        // The diff's prep fork is the base the workspace stands on now: a base
        // update fast-forwards it past the commit it was prepared from.
        let prep_fork = located
            .receipt
            .base_commit
            .as_deref()
            .or(located.receipt.commit.as_deref());
        read_reply(
            &worktree,
            &located.repo_path,
            prep_fork,
            &request.operation,
            alive,
        )
    })
}

/// One file effect. The caller holds the target worktree's lock (and, for the
/// exclude block, the clone's exclude leaf lock).
fn effect_locked(
    root: &Path,
    owner: u64,
    request: FileRequest,
    alive: &dyn Fn() -> bool,
    operation_id: &str,
    located: &Located,
) -> Result<FileReply, String> {
    let id = &operation_id.to_string();
    let workspace = match resolve_at(root, owner, &request.target, located.clone(), &alive) {
        Ok(workspace) => workspace,
        Err(_) => {
            return Ok(FileReply::Effect {
                outcome: unknown(id),
            })
        }
    };
    {
        let key = ledger_key(&request.target, id)?;
        let prior = if exists(&workspace.ledger, &key)? {
            Some(
                serde_json::from_slice::<FileReceipt>(&read_record(&workspace.ledger, &key)?)
                    .map_err(|_| "file receipt corrupt; outcome unknown")?,
            )
        } else {
            None
        };
        let mut receipt = match prior {
            Some(receipt) => {
                if receipt.request.target.owner_id != request.target.owner_id
                    || receipt.request.target.origin_key != request.target.origin_key
                    || receipt.request.target.handle != request.target.handle
                    || (!matches!(request.operation, FileOperation::Reconcile { .. })
                        && !receipt.names(&request.operation))
                {
                    return Err("operation ID belongs to different immutable file intent".into());
                }
                return Ok(FileReply::Effect {
                    outcome: reconcile_receipt(&workspace, &key, receipt, id, alive)?,
                });
            }
            None if matches!(request.operation, FileOperation::Reconcile { .. }) => {
                // A batch member's receipt is its batch journal.
                if let Some(outcome) =
                    super::context_batch::journaled(&workspace.ledger, owner, &request.target, id)?
                {
                    return Ok(FileReply::Effect {
                        outcome: if alive() { outcome } else { unknown(id) },
                    });
                }
                return Ok(FileReply::Effect {
                    outcome: if alive() {
                        EffectOutcome::NotStarted {
                            operation_id: id.clone(),
                        }
                    } else {
                        unknown(id)
                    },
                });
            }
            None => FileReceipt::new(request.clone()),
        };
        if !alive() {
            return Ok(FileReply::Effect {
                outcome: unknown(id),
            });
        }
        save_receipt(&workspace.ledger, &key, &receipt, true)?;
        let outcome = {
            let _writing = locks::writing(&workspace.worktree_path);
            execute(&workspace, &request, &alive).unwrap_or_else(|_| unknown(id))
        };
        if !matches!(outcome, EffectOutcome::Unknown { .. }) {
            receipt.outcome = Some(outcome.clone());
            if save_receipt(&workspace.ledger, &key, &receipt, false).is_err() {
                return Ok(FileReply::Effect {
                    outcome: unknown(id),
                });
            }
        }
        Ok(FileReply::Effect {
            outcome: if alive() { outcome } else { unknown(id) },
        })
    }
}

/// A read-only operation against a held worktree; takes no lock.
fn read_reply(
    worktree: &File,
    repo_path: &str,
    // The base the worktree stands on (its prep fork): the commit it was
    // prepared from, or the one a base update moved it to.
    base_commit: Option<&str>,
    operation: &FileOperation,
    alive: &dyn Fn() -> bool,
) -> Result<FileReply, String> {
    let reply = match operation {
        FileOperation::Inspect { path } => FileReply::Inspected {
            observation: observe(worktree, path)?.0,
        },
        FileOperation::Read {
            path,
            expected_hash,
            limit,
        } => {
            if *limit > MAX_FILE_BYTES {
                return Err("remote read limit exceeds maximum".into());
            }
            let (observation, bytes) = observe(worktree, path)?;
            if !matches!(observation, Observation::Regular { .. }) || bytes.len() > *limit || expected_hash.is_some_and(|expected| !matches!(observation, Observation::Regular { hash, .. } if hash == expected)) { return Err("remote read changed, unsafe or exceeds limit".into()); }
            FileReply::Read { observation, bytes }
        }
        FileOperation::Enumerate { prefix, limit } => FileReply::Paths {
            paths: enumerate(worktree, prefix.as_deref(), *limit, &alive)?,
        },
        FileOperation::List { path } => FileReply::Listing {
            entries: list(worktree, path.as_deref())?,
        },
        FileOperation::ReadHead { path } => FileReply::Head {
            bytes: read_head(worktree, path, &alive)?,
        },
        FileOperation::ListFiles { limit } => {
            let (paths, truncated) = list_files(worktree, *limit, &alive)?;
            FileReply::FileList { paths, truncated }
        }
        FileOperation::TrackedPaths { prefix, limit } => FileReply::Paths {
            paths: tracked(worktree, prefix.as_deref(), *limit, &alive)?,
        },
        FileOperation::Changes { base_ref } => {
            super::diff::changes(worktree, base_commit, base_ref.as_deref(), &alive)?
        }
        FileOperation::Redline {
            path,
            old_path,
            base_ref,
        } => super::diff::redline(
            worktree,
            base_commit,
            path,
            old_path.as_deref(),
            base_ref.as_deref(),
            &alive,
        )?,
        FileOperation::ReadManagedExclude => {
            let (observation, bytes) = exclude_read(repo_path)?;
            let expected = expected_for(&observation)?;
            let (_, _, lines) = exclude_block(&bytes)?;
            FileReply::ManagedExclude { expected, lines }
        }
        _ => return Err("invalid remote file operation".into()),
    };
    if !alive() {
        return Err("remote file authority retired".into());
    }
    Ok(reply)
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
