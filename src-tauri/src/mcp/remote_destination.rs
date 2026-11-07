// SPDX-License-Identifier: MIT
//! Exact W1 workspace handles adapted to the existing hydration engine's I/O port.
use super::remote_authority::RemoteAuthority;
use crate::commands::route_to_context::ActiveContext;
use crate::content_engine::destination::*;
use crate::terminal::session_server::SessionServerClient;
use codesense_pty_core::context_batch as batch;
use codesense_pty_core::remote_files as wire;
use std::sync::Arc;
use uuid::Uuid;

pub(crate) struct ResolvedRemoteDestination {
    pub(crate) context: ActiveContext,
    pub(crate) io: Arc<dyn DestinationIo>,
    pub(crate) source_repo: crate::domain::SourceRepoSlug,
    pub(crate) team_key: String,
    pub(crate) workspace_id: String,
}

pub(crate) async fn resolve(
    app: tauri::AppHandle,
    ctx: ActiveContext,
    machine_id: &str,
    workspace_handle: &str,
    caller: Option<RemoteAuthority>,
) -> Result<ResolvedRemoteDestination, String> {
    let resolved = super::remote_workspace_target::resolve(
        app,
        ctx,
        Some(machine_id),
        workspace_handle,
        caller,
    )
    .await?;
    Ok(from_resolved(resolved))
}

pub(crate) async fn resolve_selected(
    app: tauri::AppHandle,
    ctx: ActiveContext,
    machine_id: &str,
    workspace_handle: &str,
    caller: Option<RemoteAuthority>,
    receipt_only: bool,
) -> Result<ResolvedRemoteDestination, String> {
    Ok(from_resolved(
        super::remote_workspace_target::resolve_selected(
            app,
            ctx,
            Some(machine_id),
            workspace_handle,
            caller,
            receipt_only,
        )
        .await?,
    ))
}

pub(crate) fn from_resolved(
    resolved: super::remote_workspace_target::ResolvedWorkspace,
) -> ResolvedRemoteDestination {
    let journal = resolved
        .context
        .codesense_home
        .join("remote-batch-journal")
        .join(resolved.context.user_id.to_string());
    ResolvedRemoteDestination {
        context: resolved.context,
        source_repo: resolved.source_repo,
        team_key: resolved.team_key,
        workspace_id: resolved.workspace_id,
        io: Arc::new(Adapter {
            #[cfg(test)]
            lose_ack: None,
            #[cfg(test)]
            held_receipt: None,
            identity: DestinationIdentity::Remote(RemoteDestinationIdentity {
                owner_id: resolved.target.owner_id,
                machine_id: resolved.machine_id,
                server_id: resolved.server_id,
                workspace_handle: resolved.target.handle.clone(),
            }),
            target: resolved.target,
            client: resolved.client,
            validate: resolved.validate,
            journal: Some(journal),
        }),
    }
}

struct Adapter {
    #[cfg(test)]
    lose_ack: Option<Arc<std::sync::atomic::AtomicBool>>,
    #[cfg(test)]
    held_receipt: Option<Arc<std::sync::Mutex<Option<Uuid>>>>,
    identity: DestinationIdentity,
    target: wire::FileTarget,
    client: Arc<SessionServerClient>,
    validate: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    /// Where each destination's in-flight batch is journaled.
    journal: Option<std::path::PathBuf>,
}
impl Adapter {
    fn validate(&self) -> Result<(), String> {
        (self.validate)()
    }
    fn call(&self, operation: wire::FileOperation) -> Result<wire::FileReply, String> {
        self.validate()?;
        let result = self.client.workspace_file(wire::FileRequest {
            target: self.target.clone(),
            operation,
        });
        self.validate()?;
        result
    }
    fn effect(
        &self,
        id: Uuid,
        operation: wire::FileOperation,
    ) -> Result<DestinationEffectOutcome, String> {
        #[cfg(test)]
        let can_lose = matches!(operation, wire::FileOperation::Apply { .. });
        #[cfg(test)]
        let is_reconcile = matches!(operation, wire::FileOperation::Reconcile { .. });
        match self.call(operation) {
            Ok(wire::FileReply::Effect { outcome }) => {
                #[cfg(test)]
                if can_lose
                    && matches!(outcome, wire::EffectOutcome::Applied { .. })
                    && self
                        .lose_ack
                        .as_ref()
                        .is_some_and(|flag| flag.swap(false, std::sync::atomic::Ordering::SeqCst))
                {
                    if let Some(held) = &self.held_receipt {
                        *held.lock().expect("test receipt gate") = Some(id);
                    }
                    return Ok(DestinationEffectOutcome::Unknown { operation_id: id });
                }
                #[cfg(test)]
                if is_reconcile
                    && self
                        .held_receipt
                        .as_ref()
                        .is_some_and(|held| *held.lock().expect("test receipt gate") == Some(id))
                {
                    return Ok(DestinationEffectOutcome::Unknown { operation_id: id });
                }
                outcome_from(outcome, id)
            }
            _ => Ok(DestinationEffectOutcome::Unknown { operation_id: id }),
        }
    }
}
/// One in-flight batch, journaled before its Begin: enough to find and
/// settle it again (Status / Apply / Finish need only its id and digest),
/// never its bytes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct PendingBatch {
    batch_id: Uuid,
    digest: [u8; 32],
}
/// An Apply's deadline grows with its batch: 65 s, plus 20 ms per
/// operation, plus a second per MiB.
pub(crate) fn apply_wait(operations: usize, bytes: u64) -> std::time::Duration {
    crate::terminal::session_server::BATCH_CALL_TIMEOUT
        + std::time::Duration::from_millis(20 * operations as u64)
        + std::time::Duration::from_secs(bytes / (1024 * 1024))
}
impl Adapter {
    /// Journal the batch, then Begin, its chunks (from the wire bytes) and
    /// Seal. Progress counts wire bytes; a `false` from it abandons.
    fn upload(
        &self,
        id: Uuid,
        digest: [u8; 32],
        wire: &WireBatch<'_>,
        progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<batch::TransferStatus, DestinationBatchError> {
        self.journal(&PendingBatch {
            batch_id: id,
            digest,
        })?;
        let total = wire.wire_length();
        let mut status = self.batch_call(
            id,
            digest,
            batch::BatchOperation::Begin {
                manifest: wire.manifest.clone(),
            },
        )?;
        if status.phase == batch::TransferPhase::Uploading {
            let mut offset = status.offset;
            if !progress(offset, total) {
                return Err(self.abandon(id, digest));
            }
            while offset < total {
                let end = (offset + batch::CHUNK_BYTES as u64).min(total);
                status = self.batch_call(
                    id,
                    digest,
                    batch::BatchOperation::Chunk {
                        offset,
                        bytes: wire.chunk(offset, end),
                    },
                )?;
                offset = status.offset;
                if !progress(offset, total) {
                    return Err(self.abandon(id, digest));
                }
            }
            status = self.batch_call(id, digest, batch::BatchOperation::Seal)?;
        }
        Ok(status)
    }
    fn machine_id(&self) -> String {
        match &self.identity {
            DestinationIdentity::Remote(remote) => remote.machine_id.clone(),
            _ => String::new(),
        }
    }
    fn outdated(&self) -> DestinationBatchError {
        DestinationBatchError::ServerOutdated {
            machine_id: self.machine_id(),
        }
    }
    fn journal_path(&self) -> Option<std::path::PathBuf> {
        let directory = self.journal.as_ref()?;
        let DestinationIdentity::Remote(remote) = &self.identity else {
            return None;
        };
        let key = serde_json::to_vec(&(
            remote.owner_id,
            &remote.machine_id,
            &remote.server_id,
            &remote.workspace_handle,
        ))
        .ok()?;
        Some(directory.join(format!("{}.json", blake3::hash(&key).to_hex())))
    }
    fn journaled(&self) -> Option<PendingBatch> {
        let bytes = std::fs::read(self.journal_path()?).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
    fn journal(&self, pending: &PendingBatch) -> Result<(), DestinationBatchError> {
        let Some(path) = self.journal_path() else {
            return Ok(());
        };
        let failed = |error: std::io::Error| {
            DestinationBatchError::Failed(format!("cannot journal the context batch: {error}"))
        };
        let directory = path.parent().expect("journal file has a directory");
        std::fs::create_dir_all(directory).map_err(failed)?;
        let temporary = path.with_extension("next");
        let bytes = serde_json::to_vec(pending).expect("pending batch encodes");
        {
            use std::io::Write as _;
            let mut file = std::fs::File::create(&temporary).map_err(failed)?;
            file.write_all(&bytes).map_err(failed)?;
            file.sync_all().map_err(failed)?;
        }
        std::fs::rename(&temporary, &path).map_err(failed)
    }
    fn settled_journal(&self, batch_id: Uuid) {
        if self
            .journaled()
            .is_some_and(|pending| pending.batch_id == batch_id)
        {
            if let Some(path) = self.journal_path() {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    fn batch_call_within(
        &self,
        batch_id: Uuid,
        digest: [u8; 32],
        operation: batch::BatchOperation,
        wait: std::time::Duration,
    ) -> Result<batch::TransferStatus, String> {
        self.validate()?;
        let reply = self.client.workspace_context_batch_within(
            batch::BatchRequest {
                version: batch::VERSION,
                target: self.target.clone(),
                batch_id: batch_id.to_string(),
                manifest_digest: digest,
                operation,
            },
            wait,
        );
        self.validate()?;
        match reply? {
            batch::BatchReply::Transfer { status } => Ok(status),
            _ => Err("unexpected batch reply".into()),
        }
    }
    fn batch_call(
        &self,
        batch_id: Uuid,
        digest: [u8; 32],
        operation: batch::BatchOperation,
    ) -> Result<batch::TransferStatus, DestinationBatchError> {
        self.batch_call_within(
            batch_id,
            digest,
            operation,
            crate::terminal::session_server::BATCH_CALL_TIMEOUT,
        )
        .map_err(DestinationBatchError::Failed)
    }
    /// Apply until it is answered. A lost reply means the Apply may still be
    /// running (it holds the workspace lock) or was cut: asking again waits
    /// behind it, then resumes or replays the stored outcomes (Apply is
    /// idempotent by operation id). No single call outlives `wait`; it is asked
    /// again for up to eight times that.
    fn apply_until_answered(
        &self,
        batch_id: Uuid,
        digest: [u8; 32],
        wait: std::time::Duration,
    ) -> Result<batch::TransferStatus, DestinationBatchError> {
        #[allow(unused_mut)]
        let (mut wait, mut budget) = (wait, wait * 8);
        #[cfg(test)]
        if let Some(forced) = crate::terminal::session_server::APPLY_WAIT_OVERRIDE.with(|f| f.get())
        {
            (wait, budget) = forced;
        }
        let deadline = std::time::Instant::now() + budget;
        loop {
            match self.batch_call_within(batch_id, digest, batch::BatchOperation::Apply, wait) {
                Ok(status) => return Ok(status),
                Err(error)
                    if crate::terminal::session_server::is_reply_lost(&error)
                        && std::time::Instant::now() < deadline => {}
                Err(error) => return Err(DestinationBatchError::Failed(error)),
            }
        }
    }
    /// Settle a journaled batch an earlier call left behind: resume and
    /// finish one that was applying, abandon one that never applied.
    fn recover(
        &self,
        pending: &PendingBatch,
    ) -> Result<Option<RecoveredBatch>, DestinationBatchError> {
        let unrecovered = |reason: String| DestinationBatchError::Unrecovered {
            batch_id: pending.batch_id,
            reason,
        };
        let status = match self.batch_call(
            pending.batch_id,
            pending.digest,
            batch::BatchOperation::Status,
        ) {
            Ok(status) => status,
            // The machine never accepted it (or already forgot it).
            Err(DestinationBatchError::Failed(error))
                if error.contains("outcome unavailable")
                    || error.contains("reservation is incomplete")
                    || error.contains("remote path is missing") =>
            {
                self.settled_journal(pending.batch_id);
                return Ok(None);
            }
            Err(error) => return Err(unrecovered(error.to_string())),
        };
        let finish = |disposition| {
            self.batch_call(
                pending.batch_id,
                pending.digest,
                batch::BatchOperation::Finish { disposition },
            )
            .map_err(|error| unrecovered(error.to_string()))
        };
        let status = match status.phase {
            batch::TransferPhase::Uploading | batch::TransferPhase::Verified => {
                finish(batch::FinishDisposition::Abandon)?
            }
            batch::TransferPhase::Applying => {
                self.apply_until_answered(pending.batch_id, pending.digest, apply_wait(0, 0))
                    .map_err(|error| unrecovered(error.to_string()))?;
                finish(batch::FinishDisposition::Settled)?
            }
            _ => status,
        };
        self.settled_journal(pending.batch_id);
        Ok(Some(RecoveredBatch {
            batch_id: pending.batch_id,
            phase: status.phase,
            writes: status.items,
            operations: status.operations,
            exclude_paths: status.exclude_paths,
        }))
    }
}
/// A destination batch on the wire: its manifest, and the writes it carries
/// (in payload order, with each one's position in `writes`). A write over the
/// member limit is left out and reported `TooLarge`. The payload is never
/// copied whole: chunks are cut from the writes' own bytes.
struct WireBatch<'a> {
    manifest: batch::BatchManifest,
    carried: Vec<&'a BatchWrite>,
    starts: Vec<u64>,
    too_large: Vec<usize>,
    /// The zstd frame on the wire, when the batch is compressed.
    /// Chunk offsets are then in its bytes.
    encoded: Option<Vec<u8>>,
}
/// The zstd level the batch payload is compressed with
/// (measured: 4.9x on a real team context at ~400 MB/s).
const ZSTD_LEVEL: i32 = 3;
/// The window the encoder may use; the host refuses frames over 16 MiB.
const ZSTD_WINDOW_LOG: u32 = 22;
impl WireBatch<'_> {
    /// The payload's total length on the wire.
    fn wire_length(&self) -> u64 {
        self.manifest.encoded_length
    }
    fn chunk(&self, offset: u64, end: u64) -> Vec<u8> {
        if let Some(encoded) = &self.encoded {
            return encoded[offset as usize..end as usize].to_vec();
        }
        let mut bytes = Vec::with_capacity((end - offset) as usize);
        let mut at = self
            .starts
            .partition_point(|start| *start <= offset)
            .saturating_sub(1);
        let mut position = offset;
        while position < end && at < self.carried.len() {
            let write = &self.carried[at].bytes;
            let local = (position - self.starts[at]) as usize;
            let take = ((end - position) as usize).min(write.len() - local);
            bytes.extend_from_slice(&write[local..local + take]);
            position += take as u64;
            at += 1;
        }
        bytes
    }
}
/// One zstd frame of `writes` (in payload order) recording its content size,
/// or `None` when it would not be smaller.
fn zstd_payload(writes: &[&BatchWrite], raw_length: u64) -> Result<Option<Vec<u8>>, String> {
    use std::io::Write as _;
    let failed = |error: std::io::Error| format!("cannot compress the batch: {error}");
    // Streamed from the writes' own bytes: the only extra copy is the frame.
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), ZSTD_LEVEL).map_err(failed)?;
    encoder
        .set_pledged_src_size(Some(raw_length))
        .map_err(failed)?;
    encoder.include_contentsize(true).map_err(failed)?;
    encoder.include_checksum(false).map_err(failed)?;
    encoder.window_log(ZSTD_WINDOW_LOG).map_err(failed)?;
    for write in writes {
        encoder.write_all(&write.bytes).map_err(failed)?;
    }
    let encoded = encoder.finish().map_err(failed)?;
    Ok(((encoded.len() as u64) < raw_length).then_some(encoded))
}
fn wire_batch(destination: &DestinationBatch, zstd: bool) -> Result<WireBatch<'_>, String> {
    let limit = wire::MAX_FILE_BYTES;
    let mut carried = Vec::new();
    let mut starts = Vec::new();
    let mut too_large = Vec::new();
    let mut items = Vec::new();
    let mut hasher = blake3::Hasher::new();
    let mut length = 0u64;
    for (index, write) in destination.writes.iter().enumerate() {
        if write.bytes.len() > limit {
            too_large.push(index);
            continue;
        }
        items.push(batch::BatchItem {
            index,
            operation_id: write.operation_id.to_string(),
            path: write.path.as_str().into(),
            offset: length,
            length: write.bytes.len() as u64,
            hash: *blake3::hash(&write.bytes).as_bytes(),
            mode: write.mode,
            expected: expected_to(&write.expected),
            dependencies: Vec::new(),
        });
        hasher.update(&write.bytes);
        starts.push(length);
        carried.push(write);
        length += write.bytes.len() as u64;
    }
    let digest = *hasher.finalize().as_bytes();
    let manifest = batch::BatchManifest {
        request_id: destination.batch_id.to_string(),
        generation: 1,
        codec: batch::Codec::Identity,
        raw_length: length,
        encoded_length: length,
        raw_digest: digest,
        encoded_digest: digest,
        items,
        removes: destination
            .removes
            .iter()
            .map(|remove| batch::RemoveItem {
                operation_id: remove.operation_id.to_string(),
                path: remove.path.as_str().into(),
                expected_pre_hash: remove.expected_pre_hash,
            })
            .collect(),
        chmods: destination
            .chmods
            .iter()
            .map(|chmod| batch::ChmodItem {
                operation_id: chmod.operation_id.to_string(),
                path: chmod.path.as_str().into(),
                mode: chmod.mode,
                expected_pre_hash: chmod.expected_pre_hash,
            })
            .collect(),
        ensure_dirs: destination
            .ensure_dirs
            .iter()
            .map(|dir| batch::DirItem {
                operation_id: dir.operation_id.to_string(),
                path: dir.path.as_str().into(),
            })
            .collect(),
        exclude_block: destination.exclude.as_ref().map(|block| {
            // A write never sent (too large) must not be excluded unless it
            // already was: a file the user has there would stay hidden.
            let unsent: std::collections::HashSet<&str> = too_large
                .iter()
                .map(|index| destination.writes[*index].path.as_str())
                .collect();
            Box::new(batch::ManagedExcludeBlock {
                operation_id: block.operation_id.to_string(),
                workspace_id: block.workspace_id.clone(),
                expected: block.expected.clone(),
                paths: block
                    .paths
                    .iter()
                    .filter(|path| !unsent.contains(path.as_str()) || block.expected.contains(path))
                    .cloned()
                    .collect(),
                repair: block.repair,
            })
        }),
    };
    let mut manifest = manifest;
    // An encoder failure is not the batch's: it goes as identity.
    let encoded = if zstd && length > 0 {
        zstd_payload(&carried, length).unwrap_or_else(|error| {
            log::warn!("context batch sent uncompressed: {error}");
            None
        })
    } else {
        None
    };
    if let Some(encoded) = &encoded {
        manifest.codec = batch::Codec::Zstd;
        manifest.encoded_length = encoded.len() as u64;
        manifest.encoded_digest = *blake3::hash(encoded).as_bytes();
    }
    if manifest.operation_count() > 0 {
        manifest.validate()?;
    }
    Ok(WireBatch {
        manifest,
        carried,
        starts,
        too_large,
        encoded,
    })
}
/// Every write's outcome in `writes` order: `TooLarge` for the ones never
/// sent, the machine's answer for the rest.
fn write_outcomes(
    destination: &DestinationBatch,
    too_large: &[usize],
    answered: Vec<batch::MemberResult>,
) -> Vec<batch::MemberResult> {
    let mut outcomes: Vec<Option<batch::MemberResult>> = vec![None; destination.writes.len()];
    for &index in too_large {
        let write = &destination.writes[index];
        outcomes[index] = Some(batch::MemberResult {
            index,
            operation_id: write.operation_id.to_string(),
            outcome: batch::MemberOutcome::TooLarge {
                size: write.bytes.len() as u64,
                limit: wire::MAX_FILE_BYTES as u64,
            },
        });
    }
    for result in answered {
        if let Some(slot) = outcomes.get_mut(result.index) {
            *slot = Some(result);
        }
    }
    outcomes.into_iter().flatten().collect()
}
fn observation_from(value: wire::Observation) -> DestinationObservation {
    value.into()
}
fn expected_to(value: &ExpectedDestination) -> wire::Expected {
    value.into()
}
fn outcome_from(value: wire::EffectOutcome, id: Uuid) -> Result<DestinationEffectOutcome, String> {
    DestinationEffectOutcome::from_wire(value, id)
}
impl Adapter {
    fn manifest_call(
        &self,
        prefixes: &[String],
        limit: usize,
        read: &[String],
        progress: &mut dyn FnMut(&codesense_pty_core::remote_manifest::ManifestProgress),
    ) -> Result<(CompleteManifest, ReadFiles), DestinationBatchError> {
        if !self.client.supports_context_manifest() {
            return Err(self.outdated());
        }
        // What a cut batch applied must be known before anything is diffed.
        let recovered = self.reconcile_pending()?;
        self.manifest_walk(prefixes, limit, read, progress, recovered, 2)
    }

    /// One `WorkspaceManifest` call (asked again up to `busy_retries` times
    /// when the machine answers busy). Settles nothing: the caller decided
    /// what `recovered` is.
    fn manifest_walk(
        &self,
        prefixes: &[String],
        limit: usize,
        read: &[String],
        progress: &mut dyn FnMut(&codesense_pty_core::remote_manifest::ManifestProgress),
        recovered: Option<RecoveredBatch>,
        busy_retries: usize,
    ) -> Result<(CompleteManifest, ReadFiles), DestinationBatchError> {
        use crate::terminal::session_server::ManifestCallError;
        use codesense_pty_core::remote_manifest::EntryKind;
        let mut attempt = 0;
        loop {
            self.validate().map_err(DestinationBatchError::Failed)?;
            let result = self.client.workspace_manifest(
                codesense_pty_core::remote_manifest::ManifestRequest {
                    target: self.target.clone(),
                    prefixes: prefixes.to_vec(),
                    limit,
                    // Only a host that advertises it gets the field: an
                    // older ContextManifestV1 host rejects unknown fields,
                    // and Step 2 then falls back to its own record.
                    workspace_id: match &self.identity {
                        DestinationIdentity::Remote(remote)
                            if self.client.supports_manifest_workspace_exclude() =>
                        {
                            Some(remote.workspace_handle.clone())
                        }
                        _ => None,
                    },
                    read: read.to_vec(),
                },
                progress,
            );
            self.validate().map_err(DestinationBatchError::Failed)?;
            match result {
                Ok((reply, _, _)) if !reply.complete => {
                    return Err(DestinationBatchError::Incomplete {
                        reason: reply
                            .incomplete_reason
                            .unwrap_or_else(|| "the walk stopped early".into()),
                    })
                }
                Ok((reply, mut entries, mut contents)) => {
                    // A name that is not a contained relative
                    // path is never used (a launch skips it; a publish
                    // refuses the manifest), and only bytes asked for count.
                    sanitize_machine_paths(&mut entries, &mut contents.files, read);
                    let (files, skipped): (Vec<_>, Vec<_>) = entries
                        .into_iter()
                        .partition(|entry| entry.kind == EntryKind::File);
                    // Every byte read back must be what the manifest hashed.
                    let mut read_files = ReadFiles::default();
                    for (path, bytes) in contents.files {
                        let hashed = files
                            .iter()
                            .find(|entry| entry.path == path)
                            .and_then(|entry| entry.hash);
                        if hashed == Some(*blake3::hash(&bytes).as_bytes()) {
                            read_files.files.insert(path, bytes);
                        } else {
                            read_files
                                .unread
                                .push((path, "its bytes did not match its hash".into()));
                        }
                    }
                    read_files.unread.extend(
                        reply
                            .unread
                            .into_iter()
                            .map(|unread| (unread.path, unread.reason)),
                    );
                    return Ok((
                        CompleteManifest {
                            prefixes: reply.prefixes,
                            files,
                            skipped,
                            progress: reply.progress,
                            recovered,
                            workspace_exclude: reply.workspace_exclude,
                        },
                        read_files,
                    ));
                }
                Err(ManifestCallError::OutdatedServer) => return Err(self.outdated()),
                Err(ManifestCallError::Failed(error))
                    if attempt < busy_retries && error.contains(wire::WORKSPACE_BUSY) =>
                {
                    attempt += 1;
                }
                Err(ManifestCallError::Failed(error)) => {
                    return Err(DestinationBatchError::Failed(error))
                }
            }
        }
    }

    /// A cancelled batch is abandoned on the machine before its
    /// Apply; nothing in it applies. The journal entry goes once the machine
    /// confirms, else the next call settles it.
    fn abandon(&self, id: Uuid, digest: [u8; 32]) -> DestinationBatchError {
        if let Ok(status) = self.batch_call(
            id,
            digest,
            batch::BatchOperation::Finish {
                disposition: batch::FinishDisposition::Abandon,
            },
        ) {
            if matches!(
                status.phase,
                batch::TransferPhase::Abandoned | batch::TransferPhase::Finished
            ) {
                self.settled_journal(id);
            }
        }
        DestinationBatchError::Cancelled
    }
}

impl DestinationIo for Adapter {
    /// ONE `WorkspaceManifest` frame, whatever the file count. A read the
    /// machine answered busy (a write overlapped the whole walk) changed
    /// nothing and is asked again.
    fn manifest(
        &self,
        prefixes: &[String],
        limit: usize,
        progress: &mut dyn FnMut(&codesense_pty_core::remote_manifest::ManifestProgress),
    ) -> Result<CompleteManifest, DestinationBatchError> {
        self.manifest_call(prefixes, limit, &[], progress)
            .map(|(manifest, _)| manifest)
    }

    /// ONE `WorkspaceManifest` frame for a read-only count. A
    /// batch an earlier call left behind is never resumed here (that applies
    /// writes on the machine): with one journaled, the count is refused. A
    /// busy machine is not asked again.
    fn manifest_observe(
        &self,
        prefixes: &[String],
        limit: usize,
        progress: &mut dyn FnMut(&codesense_pty_core::remote_manifest::ManifestProgress),
    ) -> Result<CompleteManifest, DestinationBatchError> {
        if !self.client.supports_context_manifest() {
            return Err(self.outdated());
        }
        if DestinationIo::has_pending(self) {
            return Err(DestinationBatchError::Failed(
                "an earlier copy to this workspace has not finished".into(),
            ));
        }
        self.manifest_walk(prefixes, limit, &[], progress, None, 0)
            .map(|(manifest, _)| manifest)
    }

    /// A journal file that can't be read or parsed is still a batch left
    /// behind (never read as "nothing pending").
    fn has_pending(&self) -> bool {
        self.journal_path().is_some_and(|path| path.exists())
    }

    /// The manifest and the bytes of `read`, in that ONE frame.
    fn manifest_reading(
        &self,
        prefixes: &[String],
        limit: usize,
        read: &[String],
        progress: &mut dyn FnMut(&codesense_pty_core::remote_manifest::ManifestProgress),
    ) -> Result<(CompleteManifest, ReadFiles), DestinationBatchError> {
        if !read.is_empty() && !self.client.supports_manifest_read() {
            return Err(self.outdated());
        }
        self.manifest_call(prefixes, limit, read, progress)
    }

    /// Begin, the chunks, Seal, ONE Apply, Finish — the batch journaled first
    /// and any batch an earlier call left behind settled before it. The
    /// version gate applies only when the batch needs `ContextManifestV1`: a
    /// writes-only batch inside the original caps still goes to an older host.
    fn apply_batch(
        &self,
        destination_batch: &DestinationBatch,
        progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<DestinationBatchOutcome, DestinationBatchError> {
        // Compressed only for a host that decodes zstd (and only
        // through the streamed batch; the legacy path stays identity).
        let wire = wire_batch(
            destination_batch,
            self.client.supports_context_manifest() && self.client.supports_context_batch_zstd(),
        )
        .map_err(DestinationBatchError::Failed)?;
        if wire.manifest.operation_count() == 0 {
            // Only writes too large to send: nothing reaches the machine.
            return Ok(DestinationBatchOutcome {
                phase: batch::TransferPhase::Finished,
                writes: write_outcomes(destination_batch, &wire.too_large, Vec::new()),
                operations: Vec::new(),
                exclude_paths: None,
                recovered: self.reconcile_pending()?,
            });
        }
        let digest = wire
            .manifest
            .digest()
            .map_err(DestinationBatchError::Failed)?;
        let id = destination_batch.batch_id;
        let begin = batch::BatchOperation::Begin {
            manifest: wire.manifest.clone(),
        };
        if !self.client.supports_context_manifest() {
            let legacy = self.client.supports_selected_context_batch()
                && wire.manifest.fits_legacy()
                && self
                    .selected_batch_control_size(id, digest, &begin)
                    .map_err(DestinationBatchError::Failed)?
                    .is_some_and(|size| size <= batch::MAX_CONTROL_BYTES);
            if !legacy {
                return Err(self.outdated());
            }
        }
        let recovered = match self.journaled() {
            Some(pending) if pending.batch_id != id => self.recover(&pending)?,
            _ => None,
        };
        let (mut wire, mut id, mut digest) = (wire, id, digest);
        let mut status = match self.upload(id, digest, &wire, progress) {
            // A host that refuses the zstd frame at Seal (nothing
            // was applied) gets the same batch once more, uncompressed, under
            // a new batch ID (the refused one is abandoned).
            Err(DestinationBatchError::Failed(error))
                if wire.manifest.codec == batch::Codec::Zstd && error.contains("zstd") =>
            {
                log::warn!("context batch refused compressed ({error}); sending it uncompressed");
                // Its journal entry goes once the machine confirms the abandon.
                let _ = self.abandon(id, digest);
                wire =
                    wire_batch(destination_batch, false).map_err(DestinationBatchError::Failed)?;
                digest = wire
                    .manifest
                    .digest()
                    .map_err(DestinationBatchError::Failed)?;
                id = Uuid::new_v4();
                self.upload(id, digest, &wire, progress)?
            }
            other => other?,
        };
        if matches!(
            status.phase,
            batch::TransferPhase::Verified | batch::TransferPhase::Applying
        ) {
            let wait = apply_wait(wire.manifest.operation_count(), wire.manifest.raw_length);
            status = self.apply_until_answered(id, digest, wait)?;
            // Settlement refuses while an outcome is unknown; the stored
            // outcomes are reported either way, and the journal keeps the
            // batch for the next call to settle.
            if let Ok(settled) = self.batch_call(
                id,
                digest,
                batch::BatchOperation::Finish {
                    disposition: batch::FinishDisposition::Settled,
                },
            ) {
                status.phase = settled.phase;
            }
        }
        if matches!(
            status.phase,
            batch::TransferPhase::Finished | batch::TransferPhase::Abandoned
        ) {
            self.settled_journal(id);
        }
        Ok(DestinationBatchOutcome {
            phase: status.phase,
            writes: write_outcomes(destination_batch, &wire.too_large, status.items),
            operations: status.operations,
            exclude_paths: status.exclude_paths,
            recovered,
        })
    }

    fn reconcile_pending(&self) -> Result<Option<RecoveredBatch>, DestinationBatchError> {
        match self.journaled() {
            Some(pending) => self.recover(&pending),
            None => Ok(None),
        }
    }

    fn selected_batch_target(&self) -> Result<Option<wire::FileTarget>, String> {
        self.validate()?;
        Ok(self
            .client
            .supports_selected_context_batch()
            .then(|| self.target.clone()))
    }

    fn selected_batch_control_size(
        &self,
        batch_id: Uuid,
        manifest_digest: [u8; 32],
        operation: &batch::BatchOperation,
    ) -> Result<Option<usize>, String> {
        self.validate()?;
        if !self.client.supports_selected_context_batch() {
            return Ok(None);
        }
        let request = batch::BatchRequest {
            version: batch::VERSION,
            target: self.target.clone(),
            batch_id: batch_id.to_string(),
            manifest_digest,
            operation: operation.clone(),
        };
        Ok(Some(request.control_size()?))
    }
    fn selected_batch(
        &self,
        batch_id: Uuid,
        manifest_digest: [u8; 32],
        operation: batch::BatchOperation,
    ) -> Result<batch::BatchReply, String> {
        self.validate()?;
        #[cfg(test)]
        let can_lose = matches!(operation, batch::BatchOperation::Apply);
        let result = self.client.workspace_context_batch(batch::BatchRequest {
            version: batch::VERSION,
            target: self.target.clone(),
            batch_id: batch_id.to_string(),
            manifest_digest,
            operation,
        });
        self.validate()?;
        #[cfg(test)]
        let result = result.map(|mut reply| {
            if let batch::BatchReply::Transfer { status } = &mut reply {
                for member in &mut status.items {
                    let batch::MemberOutcome::Effect { outcome } = &mut member.outcome else {
                        continue;
                    };
                    let id = Uuid::parse_str(&member.operation_id).expect("test batch effect UUID");
                    if can_lose
                        && matches!(outcome, wire::EffectOutcome::Applied { .. })
                        && self.lose_ack.as_ref().is_some_and(|flag| {
                            flag.swap(false, std::sync::atomic::Ordering::SeqCst)
                        })
                    {
                        if let Some(held) = &self.held_receipt {
                            *held.lock().expect("test receipt gate") = Some(id);
                        }
                    }
                    if self
                        .held_receipt
                        .as_ref()
                        .is_some_and(|held| *held.lock().expect("test receipt gate") == Some(id))
                    {
                        *outcome = wire::EffectOutcome::Unknown {
                            operation_id: member.operation_id.clone(),
                        };
                    }
                }
            }
            reply
        });
        result
    }

    fn validate_authority(&self) -> Result<(), String> {
        self.validate()
    }
    fn identity(&self) -> &DestinationIdentity {
        &self.identity
    }
    fn inspect(&self, path: &DestinationRelativePath) -> Result<DestinationObservation, String> {
        match self.call(wire::FileOperation::Inspect {
            path: path.as_str().into(),
        })? {
            wire::FileReply::Inspected { observation } => Ok(observation_from(observation)),
            _ => Err("unexpected inspection reply".into()),
        }
    }
    fn read(
        &self,
        path: &DestinationRelativePath,
        expected_hash: Option<[u8; 32]>,
        limit: usize,
    ) -> Result<(DestinationObservation, Vec<u8>), String> {
        match self.call(wire::FileOperation::Read {
            path: path.as_str().into(),
            expected_hash,
            limit,
        })? {
            wire::FileReply::Read { observation, bytes } => {
                Ok((observation_from(observation), bytes))
            }
            _ => Err("unexpected read reply".into()),
        }
    }
    fn enumerate(
        &self,
        prefix: Option<&DestinationRelativePath>,
        limit: usize,
    ) -> Result<Vec<DestinationRelativePath>, String> {
        match self.call(wire::FileOperation::Enumerate {
            prefix: prefix.map(|p| p.as_str().into()),
            limit,
        })? {
            wire::FileReply::Paths { paths } => paths
                .into_iter()
                .map(DestinationRelativePath::try_from)
                .collect(),
            _ => Err("unexpected enumeration reply".into()),
        }
    }
    fn apply(
        &self,
        operation_id: Uuid,
        path: &DestinationRelativePath,
        expected: &ExpectedDestination,
        effect: &DestinationEffect,
    ) -> Result<DestinationEffectOutcome, String> {
        let effect = match effect {
            DestinationEffect::Write { bytes, mode } => wire::Effect::Write {
                bytes: bytes.clone(),
                mode: *mode,
            },
            DestinationEffect::Remove => wire::Effect::Remove,
            DestinationEffect::SetMode { mode } => wire::Effect::SetMode { mode: *mode },
        };
        self.effect(
            operation_id,
            wire::FileOperation::Apply {
                operation_id: operation_id.to_string(),
                path: path.as_str().into(),
                expected: expected_to(expected),
                effect,
            },
        )
    }
    fn reconcile(&self, operation_id: Uuid) -> Result<DestinationEffectOutcome, String> {
        self.effect(
            operation_id,
            wire::FileOperation::Reconcile {
                operation_id: operation_id.to_string(),
            },
        )
    }
    fn tracked_paths(
        &self,
        prefix: Option<&DestinationRelativePath>,
        limit: usize,
    ) -> Result<Vec<DestinationRelativePath>, String> {
        match self.call(wire::FileOperation::TrackedPaths {
            prefix: prefix.map(|p| p.as_str().into()),
            limit,
        })? {
            wire::FileReply::Paths { paths } => paths
                .into_iter()
                .map(DestinationRelativePath::try_from)
                .collect(),
            _ => Err("unexpected tracked paths reply".into()),
        }
    }
    fn ensure_directories(
        &self,
        operation_id: Uuid,
        path: &DestinationRelativePath,
    ) -> Result<DestinationEffectOutcome, String> {
        self.effect(
            operation_id,
            wire::FileOperation::EnsureDirectories {
                operation_id: operation_id.to_string(),
                path: path.as_str().into(),
            },
        )
    }
    fn read_managed_exclude(&self) -> Result<ManagedExcludeState, String> {
        match self.call(wire::FileOperation::ReadManagedExclude)? {
            wire::FileReply::ManagedExclude { expected, lines } => Ok(ManagedExcludeState {
                expected: match expected {
                    wire::Expected::Absent => ExpectedDestination::Absent,
                    wire::Expected::Hash { hash } => ExpectedDestination::Hash(hash),
                },
                managed_lines: lines,
            }),
            _ => Err("unexpected exclude reply".into()),
        }
    }
    fn replace_managed_exclude(
        &self,
        operation_id: Uuid,
        expected: &ExpectedDestination,
        lines: &[String],
    ) -> Result<DestinationEffectOutcome, String> {
        self.effect(
            operation_id,
            wire::FileOperation::ReplaceManagedExclude {
                operation_id: operation_id.to_string(),
                expected: expected_to(expected),
                lines: lines.to_vec(),
            },
        )
    }
}

/// Fixture construction uses the exact production transport implementation with
/// a real admitted manager guard; only Wry context resolution is supplied externally.
#[cfg(test)]
pub(crate) fn from_admitted_for_test(
    identity: RemoteDestinationIdentity,
    target: wire::FileTarget,
    client: Arc<SessionServerClient>,
    validate: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    lose_ack: Arc<std::sync::atomic::AtomicBool>,
    held_receipt: Arc<std::sync::Mutex<Option<Uuid>>>,
) -> Arc<dyn DestinationIo> {
    from_admitted_with_journal_for_test(
        identity,
        target,
        client,
        validate,
        lose_ack,
        held_receipt,
        None,
    )
}

/// What a machine names is untrusted. An entry whose path is
/// not a contained relative path (`docs/../../.ssh/…`, absolute, `.git`) is
/// turned into an `Unsafe` skipped entry (never written, never read; the
/// explicit publish refuses a manifest holding one), and bytes for a path
/// that was not asked for are dropped.
pub(crate) fn sanitize_machine_paths(
    entries: &mut [codesense_pty_core::remote_manifest::ManifestEntry],
    returned: &mut std::collections::BTreeMap<String, Vec<u8>>,
    asked: &[String],
) {
    use codesense_pty_core::remote_manifest::EntryKind;
    for entry in entries.iter_mut() {
        if DestinationRelativePath::try_from(entry.path.clone()).is_err() {
            entry.kind = EntryKind::Unsafe;
            entry.hash = None;
            entry.reason = Some("the machine reported an unsafe path name; never used".into());
        }
    }
    let asked: std::collections::HashSet<&str> = asked.iter().map(String::as_str).collect();
    returned.retain(|path, _| {
        asked.contains(path.as_str()) && DestinationRelativePath::try_from(path.clone()).is_ok()
    });
}

/// [`from_admitted_for_test`] with a batch journal directory.
#[cfg(test)]
pub(crate) fn from_admitted_with_journal_for_test(
    identity: RemoteDestinationIdentity,
    target: wire::FileTarget,
    client: Arc<SessionServerClient>,
    validate: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    lose_ack: Arc<std::sync::atomic::AtomicBool>,
    held_receipt: Arc<std::sync::Mutex<Option<Uuid>>>,
    journal: Option<std::path::PathBuf>,
) -> Arc<dyn DestinationIo> {
    Arc::new(Adapter {
        lose_ack: Some(lose_ack),
        held_receipt: Some(held_receipt),
        identity: DestinationIdentity::Remote(identity),
        target,
        client,
        validate,
        journal,
    })
}
