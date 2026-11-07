// SPDX-License-Identifier: MIT
//! SW-XS-7: Step 2 on a machine — ONE `WorkspaceManifest` call to diff (it
//! also reports this workspace's exclude block), then, only when a file or
//! the block must change, ONE batch: Begin, the chunks, Seal, one Apply
//! carrying every write, mode change, removal and the exclude block, Finish
//! (SPEC §4, X2). The Apply checks each entry's pre-hash on the machine, so
//! there is no separate verification pass. The caller holds the destination
//! lock across all of it (the XS-6 contract).
use std::collections::HashMap;

use codesense_pty_core::context_batch::{MemberOutcome, OperationKind};
use codesense_pty_core::remote_files::{EffectOutcome, MAX_FILE_BYTES};
use codesense_pty_core::remote_manifest::{EntryKind, WorkspaceExclude, MAX_ENTRIES};

use super::diff::{Observation, Observed, Step};
use super::record::{ManagedRecord, PendingBatch};
use super::{PreparePhase, Run};
use crate::content_engine::destination::{
    BatchChmod, BatchExclude, BatchRemove, BatchWrite, CompleteManifest, DestinationBatch,
    DestinationBatchError, DestinationIo, DestinationRelativePath, ExpectedDestination,
    RecoveredBatch,
};

fn applied(outcome: &MemberOutcome) -> bool {
    matches!(
        outcome,
        MemberOutcome::Effect {
            outcome: EffectOutcome::Applied { .. }
        }
    )
}

fn too_large(bytes: u64) -> String {
    format!(
        "too large for a machine ({:.1} MB; a machine takes files up to {} MB)",
        bytes as f64 / (1024.0 * 1024.0),
        MAX_FILE_BYTES / (1024 * 1024)
    )
}

fn why(outcome: &MemberOutcome) -> String {
    match outcome {
        MemberOutcome::Effect { outcome } => match outcome {
            EffectOutcome::Conflict { .. } => {
                "it changed on the machine since the comparison".into()
            }
            EffectOutcome::NotStarted { .. } => "the machine didn't start it".into(),
            EffectOutcome::Unknown { .. } => "the machine didn't confirm it".into(),
            EffectOutcome::Applied { .. } => "applied".into(),
        },
        MemberOutcome::Blocked { .. } => "it waited on an entry that failed".into(),
        MemberOutcome::Skipped { reason } => reason.clone(),
        MemberOutcome::TooLarge { size, .. } => too_large(*size),
    }
}

/// What a batch an earlier run left behind did, folded into the record
/// before anything is diffed (§11.2). Pending batches with no matching
/// recovery stay pending until one arrives (from a manifest, or from a later
/// `apply_batch`'s own recovery).
fn settle(run: &mut Run<'_, '_>, recovered: Option<RecoveredBatch>) {
    settle_record(&mut run.record, recovered);
}

/// Fold what a cut batch applied (its `recovered` report) into `record`:
/// the pending batch it matches is dropped and exactly its applied writes,
/// removes and exclude block are recorded. SW-XS-8: the explicit publish
/// settles through this too, so a recovery reported to it is never dropped.
pub(super) fn settle_record(record: &mut ManagedRecord, recovered: Option<RecoveredBatch>) {
    let Some(recovered) = recovered else {
        return;
    };
    let Some(at) = record
        .pending
        .iter()
        .position(|pending| pending.batch_id == recovered.batch_id)
    else {
        return;
    };
    let pending = record.pending.remove(at);
    for result in &recovered.writes {
        if applied(&result.outcome) {
            if let Some((path, entry)) = pending.writes.get(result.index) {
                record.entries.insert(path.clone(), entry.clone());
            }
        }
    }
    for result in &recovered.operations {
        if result.kind == OperationKind::Remove && applied(&result.outcome) {
            if let Some(path) = pending.removes.get(result.index) {
                record.entries.remove(path);
            }
        }
    }
    if let Some(paths) = recovered.exclude_paths {
        record.exclude = Some(paths);
    }
}

/// Clear the legacy engine's leftover effects on this destination and admit
/// it to explicit context (§11.11): a workspace with pending legacy effects
/// still launches. [`super::prepare`] holds the selected lease (no in-process
/// legacy writer runs) and the destination lock.
fn admit(run: &Run<'_, '_>, io: &dyn DestinationIo) -> Result<(), String> {
    let store = crate::content_engine::sync_state_store::SyncStateStore::open(
        crate::content_engine::sync_state_store::default_store_path(run.request.home),
    )
    .map_err(|e| e.to_string())?;
    let cleared = store
        .migrate_to_explicit(io.identity())
        .map_err(|e| e.to_string())?;
    if cleared > 0 {
        log::warn!(
            target: "agents",
            "cleared {cleared} unsettled legacy sync effect(s) on {}: Step 2 compares the machine's files directly",
            run.request.workspace
        );
    }
    Ok(())
}

fn manifest(
    run: &Run<'_, '_>,
    io: &dyn DestinationIo,
    prefixes: &[String],
) -> Result<CompleteManifest, String> {
    io.manifest(prefixes, MAX_ENTRIES, &mut |progress| {
        run.emit(
            PreparePhase::Diff,
            progress.scanned,
            progress.hashed_bytes,
            run.want().files.len() as u64,
        )
    })
    .map_err(|error| error.to_string())
}

/// The block as the machine reported it: `Ok(paths)`, or `Err(reason)` for
/// a hand-edited one. An older reply without it falls back to the record.
fn reported_block(
    run: &Run<'_, '_>,
    reported: Option<WorkspaceExclude>,
) -> Result<Vec<String>, String> {
    match reported {
        Some(WorkspaceExclude::Paths { mut paths }) => {
            paths.sort();
            paths.dedup();
            Ok(paths)
        }
        Some(WorkspaceExclude::HandEdited { reason }) => Err(reason),
        None => Ok(run.record.exclude.clone().unwrap_or_default()),
    }
}

pub(super) fn run(run: &mut Run<'_, '_>, io: &dyn DestinationIo) -> Result<(), String> {
    io.validate_authority()?;
    admit(run, io)?;
    run.hash_wants()?;
    let mut prefixes = run.want().prefixes.clone();
    if run.request.prune {
        // A prune must see every managed file it may remove.
        prefixes.extend(run.record.entries.keys().cloned());
        prefixes =
            codesense_pty_core::remote_manifest::normalize_prefixes(&prefixes).or_else(|_| {
                let mut tops: Vec<String> = prefixes
                    .iter()
                    .map(|p| p.split('/').next().unwrap_or(p).to_string())
                    .collect();
                tops.sort();
                tops.dedup();
                codesense_pty_core::remote_manifest::normalize_prefixes(&tops)
            })?;
    }
    if prefixes.is_empty() {
        // An empty want set asks the machine for no manifest (§11.5), but a
        // batch an earlier run left behind is still settled.
        let recovered = io.reconcile_pending().map_err(|e| e.to_string())?;
        settle(run, recovered);
        return Ok(());
    }
    let manifest = manifest(run, io, &prefixes)?;
    run.hashed += manifest.progress.hashed as usize;
    settle(run, manifest.recovered);
    let mut observed: HashMap<String, Observation> = HashMap::new();
    for entry in manifest.files {
        let state = match entry.hash {
            Some(hash) if entry.kind == EntryKind::File => Observed::File {
                hash,
                mode: entry.mode,
            },
            _ if entry.oversize => Observed::Other {
                reason: too_large(entry.size),
            },
            _ => Observed::Other {
                reason: entry
                    .reason
                    .clone()
                    .unwrap_or_else(|| "the machine couldn't hash it".into()),
            },
        };
        observed.insert(
            entry.path.clone(),
            Observation {
                state,
                tracked: entry.tracked,
            },
        );
    }
    for entry in manifest.skipped {
        observed.insert(
            entry.path.clone(),
            Observation {
                state: Observed::Other {
                    reason: entry
                        .reason
                        .unwrap_or_else(|| format!("{:?} on the machine", entry.kind)),
                },
                tracked: entry.tracked,
            },
        );
    }
    run.classify(&|dest| {
        observed.get(dest).cloned().unwrap_or(Observation {
            state: Observed::Absent,
            tracked: false,
        })
    });
    // MUST 1: a file over the per-member limit is never sent; it is a
    // per-entry skip, never a failure of the launch.
    let mut kept_steps = Vec::with_capacity(run.plan.steps.len());
    for (index, step) in std::mem::take(&mut run.plan.steps) {
        let file = &run.request.want.files[index];
        if matches!(step, Step::Add | Step::Replace { .. }) && file.bytes > MAX_FILE_BYTES as u64 {
            run.plan
                .skipped_other
                .push((file.dest.clone(), too_large(file.bytes)));
        } else {
            kept_steps.push((index, step));
        }
    }
    run.plan.steps = kept_steps;
    let actual = match reported_block(run, manifest.workspace_exclude) {
        Ok(actual) => actual,
        Err(reason) => {
            run.exclude_unusable(&reason);
            return Ok(());
        }
    };
    let desired = run.exclude_desired(&actual);
    if !run.changes_files() && actual == desired {
        run.record.exclude = Some(desired);
        return Ok(());
    }
    io.validate_authority()?;
    if run.cancelled() {
        run.cancelled_run = true;
        return Ok(());
    }
    apply(run, io, &prefixes, actual, desired, true)
}

fn apply(
    run: &mut Run<'_, '_>,
    io: &dyn DestinationIo,
    prefixes: &[String],
    expected: Vec<String>,
    paths: Vec<String>,
    may_retry: bool,
) -> Result<(), String> {
    let mut batch = DestinationBatch {
        batch_id: uuid::Uuid::new_v4(),
        writes: Vec::new(),
        removes: Vec::new(),
        chmods: Vec::new(),
        ensure_dirs: Vec::new(),
        exclude: None,
    };
    let mut pending = PendingBatch {
        batch_id: batch.batch_id,
        writes: Vec::new(),
        removes: Vec::new(),
    };
    let mut write_index: Vec<usize> = Vec::new();
    let mut chmod_index: Vec<usize> = Vec::new();
    let mut unread: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (index, step) in run.plan.steps.clone() {
        let file = &run.request.want.files[index];
        let path = DestinationRelativePath::try_from(file.dest.clone())?;
        match step {
            Step::Add | Step::Replace { .. } => {
                let bytes = match std::fs::read(&file.source) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        unread.insert(file.dest.clone());
                        run.plan.skipped_other.push((
                            file.dest.clone(),
                            format!("its team file is unavailable: {error}"),
                        ));
                        continue;
                    }
                };
                // Hashed as read: a source that changed since the diff is
                // never sent (§11.7).
                if *blake3::hash(&bytes).as_bytes() != run.hash_of(index) {
                    run.failed.push((
                        file.dest.clone(),
                        "the team file changed while it was read; launch again".into(),
                    ));
                    unread.insert(file.dest.clone());
                    continue;
                }
                let expected = match step {
                    Step::Replace { expected } => ExpectedDestination::Hash(expected),
                    _ => ExpectedDestination::Absent,
                };
                pending
                    .writes
                    .push((file.dest.clone(), run.entry_for(index)));
                write_index.push(index);
                batch.writes.push(BatchWrite {
                    operation_id: uuid::Uuid::new_v4(),
                    path,
                    bytes,
                    mode: file.mode,
                    expected,
                });
            }
            Step::Chmod { hash } => {
                chmod_index.push(index);
                batch.chmods.push(BatchChmod {
                    operation_id: uuid::Uuid::new_v4(),
                    path,
                    mode: file.mode,
                    expected_pre_hash: hash,
                });
            }
            Step::Unchanged => {}
        }
    }
    for (path, expected) in &run.plan.removes {
        pending.removes.push(path.clone());
        batch.removes.push(BatchRemove {
            operation_id: uuid::Uuid::new_v4(),
            path: DestinationRelativePath::try_from(path.clone())?,
            expected_pre_hash: *expected,
        });
    }
    let block_paths: Vec<String> = paths
        .iter()
        .filter(|path| {
            expected.contains(path)
                || run.record.entries.contains_key(*path)
                || !unread.contains(*path)
        })
        .cloned()
        .collect();
    batch.exclude = Some(BatchExclude {
        operation_id: uuid::Uuid::new_v4(),
        workspace_id: super::workspace_id(&run.identity),
        expected: expected.clone(),
        paths: block_paths.clone(),
        repair: false,
    });
    let total: u64 = batch.writes.iter().map(|w| w.bytes.len() as u64).sum();
    // This batch is journaled in the record first, beside any earlier one
    // still unsettled; a failed call keeps them all for a later recovery.
    let batch_id = pending.batch_id;
    run.record.pending.push(pending);
    run.record
        .save(run.request.home, run.request.principal, &run.identity)?;
    run.emit(PreparePhase::Transfer, 0, 0, total);
    let mut announced_apply = false;
    let outcome = io.apply_batch(&batch, &mut |sent, total| {
        if run.cancelled() {
            return false;
        }
        run.emit(PreparePhase::Transfer, 0, sent, total);
        if sent >= total && !announced_apply {
            announced_apply = true;
            run.emit(PreparePhase::Apply, 0, 0, total);
        }
        true
    });
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(DestinationBatchError::Cancelled) => {
            // SW-XS-8: abandoned before its Apply: nothing in it landed, and
            // without an Apply it never can.
            run.record
                .pending
                .retain(|pending| pending.batch_id != batch_id);
            run.cancelled_run = true;
            return Ok(());
        }
        Err(DestinationBatchError::Unrecovered { batch_id, reason }) => {
            return Err(format!(
                "an earlier context sync ({batch_id}) to this machine is unsettled: {reason}"
            ))
        }
        Err(error) => return Err(error.to_string()),
    };
    if !announced_apply {
        run.emit(PreparePhase::Apply, 0, 0, total);
    }
    // This batch's outcome is known now; an earlier one learns from the
    // recovery apply_batch ran before it.
    run.record
        .pending
        .retain(|pending| pending.batch_id != batch_id);
    settle(run, outcome.recovered.clone());
    if !run.record.pending.is_empty() {
        log::warn!(target: "agents", "an earlier context batch to {} is still unsettled", run.request.workspace);
    }
    let exclude_conflict = outcome.operations.iter().any(|op| {
        op.kind == OperationKind::ExcludeBlock
            && matches!(
                op.outcome,
                MemberOutcome::Effect {
                    outcome: EffectOutcome::Conflict { .. }
                }
            )
    });
    if exclude_conflict {
        // MUST 4: the block changed since the manifest (nothing in this batch
        // was written). Read it again and merge — never drop a path the
        // record doesn't know; a hand-edited block needs an explicit Repair.
        let reread = manifest(run, io, prefixes)?;
        let current = match reported_block(run, reread.workspace_exclude) {
            Ok(current) => current,
            Err(reason) => {
                run.exclude_unusable(&reason);
                return Ok(());
            }
        };
        if !may_retry {
            run.failed.push((
                ".git/info/exclude".into(),
                "the exclude block kept changing on the machine; launch again".into(),
            ));
            return Ok(());
        }
        let mut merged: Vec<String> = current.iter().chain(&paths).cloned().collect();
        merged.sort();
        merged.dedup();
        return apply(run, io, prefixes, current, merged, false);
    }
    for result in &outcome.writes {
        let Some(&index) = write_index.get(result.index) else {
            continue;
        };
        let file = &run.request.want.files[index];
        if applied(&result.outcome) {
            run.written.push(file.dest.clone());
            run.bytes_written += file.bytes;
        } else if matches!(result.outcome, MemberOutcome::TooLarge { .. }) {
            run.plan
                .skipped_other
                .push((file.dest.clone(), why(&result.outcome)));
        } else {
            run.failed.push((file.dest.clone(), why(&result.outcome)));
        }
    }
    for op in &outcome.operations {
        match op.kind {
            OperationKind::Chmod => {
                let file = &run.request.want.files[chmod_index[op.index]];
                if applied(&op.outcome) {
                    run.chmodded.push(file.dest.clone());
                } else {
                    run.failed.push((file.dest.clone(), why(&op.outcome)));
                }
            }
            OperationKind::Remove => {
                let path = run.plan.removes[op.index].0.clone();
                if applied(&op.outcome) {
                    run.removed.push(path);
                } else {
                    run.failed.push((path, why(&op.outcome)));
                }
            }
            OperationKind::ExcludeBlock => {
                if !applied(&op.outcome) {
                    run.failed
                        .push((".git/info/exclude".into(), why(&op.outcome)));
                }
            }
            OperationKind::EnsureDir => {}
        }
    }
    run.record.exclude = outcome.exclude_paths.or(Some(block_paths));
    Ok(())
}
