// SPDX-License-Identifier: MIT
//! SW-GT-25 — audit-log compaction (the growth-bound engine).
//!
//! Keeps the [`super::run_store`] append-only JSONL bounded over a long-lived
//! pipeline **without losing resume correctness or audit legibility**. It is a
//! *rewriter over the store's existing read model*, never a second parser: it reads
//! via [`run_store::read_records`], collapses records by class, and writes the result
//! back with the shipped crash-safe [`content_engine::write_tmp_fsync_rename`].
//!
//! **Two retention classes, never one blanket rule (AC-1).**
//! - **State/verdict records** keep the latest per `(story_id, action_identity,
//!   config_version, step_id)` — Kafka keyed compaction on the store's own identity,
//!   the same latest-wins-per-step rule the read fold applies, fused into one keyed pass
//!   ([`keep_newest`]). This 4-tuple is the *finest* key any fold uses, so every read
//!   path re-derives its own latest-per-key winner identically — resume, the config-
//!   scoped and unpinned gate views, AND the unscoped cross-config override snapshot —
//!   and `config_changed` is preserved because every distinct config keeps its verdicts.
//!   Only within-key re-validation churn (a step re-run at the same config) is reclaimed;
//!   distinct step/config history is audit and stays.
//! - **Audit records** — advances/overrides (`Advance`) and config-edits
//!   (`ConfigEdit`) — are history, governed by a bounded **retention window**
//!   (`retain_audit_per_key`, AC-3): keep the newest N per key. N ≥ 1 keeps the
//!   newest advance per `(story, action)`, which is the only advance the fold reads,
//!   so `overridden`/`override_reason` stay byte-identical.
//!
//! **Crash-safe + never in-place (AC-2).** The compacted log is built in memory and
//! written temp→fsync→atomic-rename via [`content_engine::write_tmp_fsync_rename`];
//! a crash mid-pass leaves the original intact (rename is the commit point).
//!
//! **Appends never lost (AC-3).** The whole read→rewrite→rename runs under
//! [`run_store::with_store_lock`], the same lock the append path takes — so no
//! verdict written mid-pass is dropped. Holding across the pass is the simple,
//! provably-safe v1 (a pass is a whole-file rewrite; at KB–MB scale it is brief but
//! not bounded to any fixed budget); segment rotation is the scale-up path, not here.
//!
//! **Off the hot path + never fatal (AC-4/AC-7).** [`should_compact`] gates on a
//! cheap size-stat that skips the dead-ratio fold entirely below the threshold; above
//! it, each check does a full fold to measure the ratio. It all runs on a background
//! `spawn_blocking` worker, never the reactor tick or the webview thread. Every
//! failure is a `warn!` + return — a failed pass leaves the original store fully
//! usable, exactly like the store's append contract.
//!
//! **Disjoint ownership (AC-6).** This module touches only `run-store.jsonl`; it never
//! reads or writes `status`/frontmatter. It cannot change where a story *is*, only how
//! compactly its verdict history is stored.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Instant;

use super::run_store::{self, RunEvent, StoreRecord};

/// The store size (bytes) below which a pass never runs — the cheap hot-path gate
/// (AC-4). At KB scale an un-compacted latest-wins log is never wrong, only
/// marginally slower to fold, so compaction earns its keep only once the file is
/// genuinely large. 1 MiB.
pub const MIN_SIZE_BYTES: u64 = 1 << 20;

/// The dead-record ratio (dead ÷ total) a store must exceed before a pass runs —
/// Kafka's `min.cleanable.dirty.ratio` default (AC-4). Below this the reclaimable
/// space does not justify a rewrite.
pub const DIRTY_RATIO: f64 = 0.5;

/// Retention window: the newest N audit records kept per key (AC-3). Generous by
/// design — audit records are not the growth driver (superseded verdicts are), and
/// N ≥ 1 guarantees the fold-relevant newest advance per `(story, action)` survives.
pub const RETAIN_AUDIT_PER_KEY: usize = 256;

/// The Tauri event a completed pass emits (AC-7) — dashboards subscribe to surface
/// lines/bytes reclaimed. Wired at the edge that owns the `AppHandle`; this module
/// stays handle-free.
pub const EVENT_COMPACTION: &str = "run-store-compaction";

/// Compaction thresholds (AC-4). Constants with a config seam, never magic numbers
/// buried in the loop.
#[derive(Debug, Clone, Copy)]
pub struct CompactionPolicy {
    /// Size gate — a pass never runs below this (the cheap hot-path pre-check).
    pub min_size_bytes: u64,
    /// Dead-ratio gate — a pass runs only once dead ÷ total exceeds this.
    pub dirty_ratio: f64,
    /// Newest audit records kept per key (the AC-3 retention window). Floored at 1 by
    /// the compactor so the fold's latest advance always survives, even if set to 0.
    pub retain_audit_per_key: usize,
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        Self {
            min_size_bytes: MIN_SIZE_BYTES,
            dirty_ratio: DIRTY_RATIO,
            retain_audit_per_key: RETAIN_AUDIT_PER_KEY,
        }
    }
}

/// What a pass reclaimed (AC-7). Emitted for dashboards and logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionMetrics {
    pub lines_before: usize,
    pub lines_after: usize,
    pub bytes_before: u64,
    pub bytes_after: u64,
    /// Fraction of lines removed, in basis points (÷10_000) so the struct stays
    /// `Eq` — the emitted JSON carries the float.
    pub dead_ratio_bps: u32,
    pub duration_ms: u128,
}

impl CompactionMetrics {
    pub fn bytes_reclaimed(&self) -> u64 {
        self.bytes_before.saturating_sub(self.bytes_after)
    }

    pub fn dead_ratio(&self) -> f64 {
        self.dead_ratio_bps as f64 / 10_000.0
    }

    fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "linesBefore": self.lines_before,
            "linesAfter": self.lines_after,
            "bytesBefore": self.bytes_before,
            "bytesAfter": self.bytes_after,
            "bytesReclaimed": self.bytes_reclaimed(),
            "deadRatio": self.dead_ratio(),
            "durationMs": self.duration_ms as u64,
        })
    }
}

/// A pass failure (AC-7) — always non-fatal to the caller: logged and skipped, the
/// original store left fully usable, the next trigger retries.
#[derive(Debug)]
pub enum CompactionError {
    /// A record failed to re-serialize (should never happen — it round-trips the
    /// shipped shape). The original store is untouched.
    Serialize(serde_json::Error),
    /// The temp-write / atomic-rename failed. The original store is untouched
    /// (rename is the commit point).
    Write(crate::content_engine::Error),
}

impl std::fmt::Display for CompactionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompactionError::Serialize(e) => write!(f, "serialize compacted log: {e}"),
            CompactionError::Write(e) => write!(f, "write compacted log: {e}"),
        }
    }
}

impl std::error::Error for CompactionError {}

/// Append order — `(ts, seq)`, the same total order the read fold sorts by.
type Ordinal = (u128, u64);
/// A surviving record's original file index carried with its append order — selection
/// works on indices so the output can be emitted in original file order.
type Timed = (Ordinal, usize);
/// Verdict compaction key: `(story_id, action_identity, config_version, step_id)` —
/// the finest key any read fold uses.
type VerdictKey = (String, String, String, String);

// ---------------------------------------------------------------------------
// Pure transform (AC-1 / AC-5) — no I/O, exhaustively unit- + property-tested.
// ---------------------------------------------------------------------------

/// Compact a record set by class (AC-1). Pure and deterministic: the output for a
/// given input is stable, so `fold(compact_records(x)) == fold(x)` for every live key
/// (AC-5).
///
/// **Verdicts** keep the latest per `(story_id, action_identity, config_version,
/// step_id)` — Kafka keyed compaction on the store's own identity. This 4-tuple is the
/// *finest* key any read fold uses, so a coarser fold re-derives its own latest-per-key
/// winner identically: a per-`(action, config)` fold and the run-scoped per-step read
/// ([`run_store::run_step_verdicts`]) both fold latest-per-{some sub-key}, and a record
/// dropped here is never the latest for either. Only within-4-tuple re-validation churn (a step re-run at the same config) is
/// reclaimed — which is the real growth driver; distinct step/config history is audit
/// and is kept.
///
/// **Audit** (advances/config-edits) keeps the newest `retain_audit_per_key` per key
/// (AC-3 window). Survivors are emitted in **original file order** (a subsequence of the
/// input) — the read fold stable-sorts by `(ts, seq)`, keeping file order for equal
/// ordinals, so preserving input order is what keeps its `.last()` / latest-per-step tie
/// handling byte-identical across a pass.
pub fn compact_records(records: &[StoreRecord], policy: &CompactionPolicy) -> Vec<StoreRecord> {
    // Choose the surviving record INDEX per key/bucket; assemble at the end in original
    // file order.
    let mut verdicts: BTreeMap<VerdictKey, Timed> = BTreeMap::new();
    let mut advances: BTreeMap<(String, Option<String>), Vec<Timed>> = BTreeMap::new();
    let mut config_edits: BTreeMap<(Option<String>, Option<String>), Vec<Timed>> = BTreeMap::new();
    // SW-GT-24 retry-progress: keep the LATEST per `(story, action, config)`. The retry read
    // (`due_retries` → re-admission) only ever reads the latest cursor — `attempt`/`next_run_at`/`exhausted` are absolute on the newest
    // record — so latest-only collapse preserves both folds (AC-5 `fold(compact)==fold(x)`).
    let mut retries: BTreeMap<(String, String, String), Timed> = BTreeMap::new();
    // SW-AU-52 run outcome: keep the LATEST per `(story_id, run_id)`. The read
    // (`run_store::latest_run_outcome`) is latest-wins, so a latest-only collapse preserves the
    // fold (AC-5 `fold(compact)==fold(x)`) exactly like a verdict/retry.
    let mut outcomes: BTreeMap<(String, String), Timed> = BTreeMap::new();
    // An earlier outcome row that names a session also stays: `project_run_outcome` folds the
    // union of a run's sessions, so dropping it would forget a session.
    let mut outcome_sessions: Vec<usize> = Vec::new();
    // SW-AU-96 reported step results ACCUMULATE per node — each event carries distinct
    // artifacts/footprint the `build_run_evidence` fold unions, so collapsing to latest-per-key would
    // drop data and break `fold(compact) == fold(x)` (AC-5). Keep every record whole; volume is
    // bounded per run (a few reports per node), and whole-run reaping bounds it across runs.
    let mut step_results: Vec<usize> = Vec::new();

    for (i, rec) in records.iter().enumerate() {
        let ord = (rec.ts, rec.seq);
        match &rec.event {
            RunEvent::Verdict(v) => {
                let key = (
                    v.story_id.clone(),
                    v.action_identity.clone(),
                    v.config_version.clone(),
                    v.step_id.clone(),
                );
                keep_newest(&mut verdicts, key, ord, i);
            }
            RunEvent::Advance(a) => {
                advances
                    .entry((a.story_id.clone(), a.action_identity.clone()))
                    .or_default()
                    .push((ord, i));
            }
            RunEvent::ConfigEdit(c) => {
                config_edits
                    .entry((c.team.clone(), c.action_identity.clone()))
                    .or_default()
                    .push((ord, i));
            }
            RunEvent::RetryProgress(r) => {
                let key = (
                    r.story_id.clone(),
                    r.action_identity.clone(),
                    r.config_version.clone(),
                );
                keep_newest(&mut retries, key, ord, i);
            }
            RunEvent::Outcome(o) => {
                if !o.session_ids.is_empty() {
                    outcome_sessions.push(i);
                }
                keep_newest(
                    &mut outcomes,
                    (o.story_id.clone(), o.run_id.clone()),
                    ord,
                    i,
                );
            }
            RunEvent::StepResult(_) => step_results.push(i),
        }
    }

    // Union of surviving indices. A `BTreeSet` iterates ascending, so draining it emits
    // records in original file order (preserving the fold's tie handling).
    let mut keep: BTreeSet<usize> = BTreeSet::new();
    keep.extend(verdicts.into_values().map(|(_, i)| i));
    keep.extend(retries.into_values().map(|(_, i)| i));
    keep.extend(outcomes.into_values().map(|(_, i)| i));
    keep.extend(outcome_sessions);
    keep.extend(step_results);
    // The retention window is floored at 1: the read fold reads the single newest
    // advance per `(story, action)`, so dropping it (a `retain_audit_per_key` of 0)
    // would silently erase an `overridden` verdict. Never drop that last advance.
    let retain_audit = policy.retain_audit_per_key.max(1);
    push_newest_n(advances, retain_audit, &mut keep);
    push_newest_n(config_edits, retain_audit, &mut keep);

    keep.into_iter().map(|i| records[i].clone()).collect()
}

/// Keep the entry with the greater `(ts, seq)` at `key` (latest-wins per key) — the
/// one home for the newest-wins verdict upsert. **Last-wins on an exact `(ts, seq)` tie**
/// (`>` not `>=`, so a later-in-file record replaces an equal-ordinal earlier one):
/// this matches the read fold, which stable-sorts by `(ts, seq)` then keeps the last
/// insert per step. Ties are reachable — `seq` resets to 0 on process restart, so two
/// same-millisecond records across runs can collide — and this alignment is what keeps
/// `fold(compacted) == fold(original)` across one (AC-5).
fn keep_newest<K: Ord, V>(map: &mut BTreeMap<K, (Ordinal, V)>, key: K, ord: Ordinal, value: V) {
    match map.get(&key) {
        Some((prev, _)) if *prev > ord => {}
        _ => {
            map.insert(key, (ord, value));
        }
    }
}

/// Keep the newest `n` record indices per bucket (the AC-3 retention window). `n ≥ 1`
/// (the caller floors it) so the fold-relevant newest advance always survives. Ordered
/// by `(ord, Reverse(index))`, so among records tied at the same ordinal the **lower
/// index (earlier in file) sorts last and is retained** — matching the read fold's
/// advance selection (`latest_advance`, a strict `>` that keeps the first-in-file on a
/// tie). Aligning the tie-break here keeps `overridden`/`override_reason` byte-identical
/// across a pass even when the window truncates a tie (AC-5).
fn push_newest_n<K: Ord>(buckets: BTreeMap<K, Vec<Timed>>, n: usize, keep: &mut BTreeSet<usize>) {
    for (_, mut items) in buckets {
        items.sort_by_key(|(ord, idx)| (*ord, std::cmp::Reverse(*idx)));
        let drop_before = items.len().saturating_sub(n);
        keep.extend(items.into_iter().skip(drop_before).map(|(_, i)| i));
    }
}

/// The dead-record ratio (AC-4) — the fraction of lines a pass would remove, i.e.
/// `1 - compacted/total`. Empty ⇒ 0. Runs the pure transform, so it is exact, not
/// an estimate. Gated behind the cheap size-stat in [`should_compact`].
pub fn dead_ratio(records: &[StoreRecord], policy: &CompactionPolicy) -> f64 {
    ratio(records.len(), compact_records(records, policy).len())
}

fn ratio(before: usize, after: usize) -> f64 {
    if before == 0 {
        return 0.0;
    }
    // `after <= before` always (every kept record maps to an original), so this is in
    // `[0, 1]`; clamp defensively so the `* 10_000 as u32` bps cast can never saturate.
    (1.0 - (after as f64 / before as f64)).clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// I/O pass (AC-2 / AC-4 / AC-7) — blocking; callers offload via spawn_blocking.
// ---------------------------------------------------------------------------

/// Whether `path` warrants a pass (AC-4). Cheap-gates on a size-stat first — below
/// `min_size_bytes` it returns without folding, so it is near-free on a background
/// cadence while the store is small. Once the store is past the gate, each call does
/// a full dead-ratio fold (on the blocking worker, off the hot path) to decide.
pub fn should_compact(path: &Path, policy: &CompactionPolicy) -> bool {
    let size = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(_) => return false, // missing / unreadable → nothing to compact
    };
    if size < policy.min_size_bytes {
        return false;
    }
    let (records, _health) = run_store::read_records(path);
    dead_ratio(&records, policy) >= policy.dirty_ratio
}

/// Run one compaction pass over `path` (AC-2). Reads, compacts, and swaps the result
/// in atomically — all under the store-mutation lock so a concurrent append is never
/// lost (AC-3). Returns the reclaim metrics; never edits the store in place. A
/// missing/empty store is a no-op (no file is created).
///
/// Blocking — call from a `spawn_blocking` worker, never the reactor/webview thread.
pub fn compact_store(
    path: &Path,
    policy: &CompactionPolicy,
) -> Result<CompactionMetrics, CompactionError> {
    let start = Instant::now();
    run_store::with_store_lock(|| {
        let bytes_before = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let (records, _health) = run_store::read_records(path);
        let lines_before = records.len();
        if records.is_empty() {
            // Nothing to compact — don't materialize an empty file where none exists.
            return Ok(CompactionMetrics {
                lines_before,
                lines_after: 0,
                bytes_before,
                bytes_after: bytes_before,
                dead_ratio_bps: 0,
                duration_ms: start.elapsed().as_millis(),
            });
        }

        let compacted = compact_records(&records, policy);
        let lines_after = compacted.len();
        let body = serialize_records(&compacted).map_err(CompactionError::Serialize)?;
        let bytes_after = body.len() as u64;

        // COW temp → fsync → atomic rename (Redis-AOF-rewrite): a crash before the
        // rename leaves the original store intact; the orphaned temp is discarded.
        crate::content_engine::write_tmp_fsync_rename(path, body.as_bytes())
            .map_err(CompactionError::Write)?;

        Ok(CompactionMetrics {
            lines_before,
            lines_after,
            bytes_before,
            bytes_after,
            dead_ratio_bps: (ratio(lines_before, lines_after) * 10_000.0).round() as u32,
            duration_ms: start.elapsed().as_millis(),
        })
    })
}

/// Serialize records to the store's on-disk form — one JSON line + `\n` each,
/// byte-for-byte the shape [`run_store::append_to`] writes (so a compacted store
/// round-trips through [`run_store::read_records`] identically).
fn serialize_records(records: &[StoreRecord]) -> Result<String, serde_json::Error> {
    let mut out = String::new();
    for rec in records {
        out.push_str(&serde_json::to_string(rec)?);
        out.push('\n');
    }
    Ok(out)
}

/// Check the trigger and, if warranted, run a pass — emitting metrics on success and
/// logging (never failing) on error (AC-4/AC-7). `emit` is the edge seam that owns
/// the `AppHandle` (production wires `app.emit`; tests pass a recorder).
///
/// Blocking — call from a `spawn_blocking` worker.
pub fn maybe_compact(
    path: &Path,
    policy: &CompactionPolicy,
    emit: impl Fn(&'static str, serde_json::Value),
) {
    if !should_compact(path, policy) {
        return;
    }
    match compact_store(path, policy) {
        Ok(metrics) => {
            log::info!(
                target: "run_store",
                "compaction: {} → {} lines, {} bytes reclaimed in {}ms",
                metrics.lines_before,
                metrics.lines_after,
                metrics.bytes_reclaimed(),
                metrics.duration_ms,
            );
            emit(EVENT_COMPACTION, metrics.to_json());
        }
        Err(e) => {
            // Never fatal: the original store is fully usable; the next trigger retries.
            log::warn!(target: "run_store", "compaction skipped (non-fatal): {e}");
        }
    }
}

/// [`maybe_compact`] against the default store with the default policy — the
/// background poll's entry point. No-ops when the home dir is unresolvable.
///
/// Blocking — call from a `spawn_blocking` worker.
pub fn maybe_compact_default(emit: impl Fn(&'static str, serde_json::Value)) {
    let Some(path) = run_store::store_path() else {
        return;
    };
    maybe_compact(&path, &CompactionPolicy::default(), emit);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::sprint_run::run_store::{
        read_records, run_step_verdicts, step_result_events, AdvanceEvent, ArtifactKind,
        ConfigEditEvent, FootprintChange, FootprintEntry, StepResultEvent, Trigger, VerdictEvent,
        SCHEMA_VERSION,
    };
    use crate::commands::sprint_run::run_store::{StepResult, Via};

    /// The latest-per-step fold scoped to `(story, action, config)`: the steps whose
    /// `(ts, seq)`-latest verdict passed (file order breaks an exact tie, last wins). The
    /// oracle the keyed-compaction tests compare across a pass.
    fn passed_steps(
        records: &[StoreRecord],
        story: &str,
        action: &str,
        cfg: &str,
    ) -> std::collections::BTreeSet<String> {
        let mut latest: std::collections::BTreeMap<&str, (u128, u64, StepResult)> =
            std::collections::BTreeMap::new();
        for rec in records {
            let RunEvent::Verdict(v) = &rec.event else {
                continue;
            };
            if v.story_id != story || v.action_identity != action || v.config_version != cfg {
                continue;
            }
            let newer = latest
                .get(v.step_id.as_str())
                .is_none_or(|(ts, seq, _)| (rec.ts, rec.seq) >= (*ts, *seq));
            if newer {
                latest.insert(v.step_id.as_str(), (rec.ts, rec.seq, v.result));
            }
        }
        latest
            .into_iter()
            .filter(|(_, (_, _, r))| *r == StepResult::Passed)
            .map(|(step, _)| step.to_string())
            .collect()
    }

    // -- builders -----------------------------------------------------------

    fn verdict_rec(
        ts: u128,
        seq: u64,
        story: &str,
        action: &str,
        cfg: &str,
        step: &str,
        result: StepResult,
    ) -> StoreRecord {
        StoreRecord {
            schema_version: SCHEMA_VERSION,
            seq,
            ts,
            event: RunEvent::Verdict(VerdictEvent {
                story_id: story.into(),
                action_identity: action.into(),
                run_id: "run-1".into(),
                step_id: step.into(),
                result,
                at: "2026-08-18T00:00:00Z".into(),
                by: Some("agent:dev".into()),
                via: Via::Evaluator,
                evidence: None,
                config_version: cfg.into(),
            }),
        }
    }

    fn advance_rec(
        ts: u128,
        seq: u64,
        story: &str,
        action: Option<&str>,
        to: &str,
        overridden: bool,
    ) -> StoreRecord {
        StoreRecord {
            schema_version: SCHEMA_VERSION,
            seq,
            ts,
            event: RunEvent::Advance(AdvanceEvent {
                story_id: story.into(),
                from: None,
                to: to.into(),
                trigger: Trigger::AutoRun,
                overridden,
                reason: overridden.then(|| "manual".to_string()),
                steps: Vec::new(),
                action_identity: action.map(str::to_string),
                run_id: Some("run-1".into()),
                by: Some("codesense".into()),
            }),
        }
    }

    fn config_edit_rec(ts: u128, seq: u64, team: &str, action: &str) -> StoreRecord {
        StoreRecord {
            schema_version: SCHEMA_VERSION,
            seq,
            ts,
            event: RunEvent::ConfigEdit(ConfigEditEvent {
                team: Some(team.into()),
                action_identity: Some(action.into()),
                at: "2026-08-18T00:00:00Z".into(),
                by: Some("human:sam".into()),
                config_version: Some("cfgX".into()),
            }),
        }
    }

    fn step_result_rec(ts: u128, seq: u64, node: &str, reference: &str) -> StoreRecord {
        StoreRecord {
            schema_version: SCHEMA_VERSION,
            seq,
            ts,
            event: RunEvent::StepResult(StepResultEvent {
                story_id: "SW-AU-96".into(),
                run_id: "run-1".into(),
                node_id: node.into(),
                summary: None,
                outputs: Vec::new(),
                artifacts: Vec::new(),
                footprint: vec![FootprintEntry {
                    kind: ArtifactKind::File,
                    reference: reference.into(),
                    change: FootprintChange::Modified,
                }],
            }),
        }
    }

    // SW-AU-96 (AC-5): reported step results ACCUMULATE — compaction must keep every one, since each
    // carries distinct footprint the fold unions. `fold(compact) == fold(x)`: the same
    // `step_result_events` before and after compaction, never a latest-per-key collapse.
    #[test]
    fn compaction_keeps_every_step_result() {
        let records = vec![
            step_result_rec(10, 0, "dev", "a.rs"),
            step_result_rec(20, 1, "dev", "b.rs"),
            step_result_rec(30, 2, "dev", "c.rs"),
        ];
        let compacted = compact_records(&records, &CompactionPolicy::default());
        let before = step_result_events(&records, "SW-AU-96", "run-1");
        let after = step_result_events(&compacted, "SW-AU-96", "run-1");
        assert_eq!(after.len(), 3, "no step result dropped by compaction");
        let refs = |evs: &[&StepResultEvent]| -> Vec<String> {
            evs.iter()
                .map(|e| e.footprint[0].reference.clone())
                .collect()
        };
        assert_eq!(refs(&before), refs(&after), "fold(compact) == fold(x)");
    }

    /// One retry-progress record at `(ts, seq)` carrying `attempt`. Same
    /// `(story, action, config)` key across calls so the collapse can be exercised.
    fn retry_rec(ts: u128, seq: u64, attempt: u32) -> StoreRecord {
        use crate::commands::sprint_run::run_store::RetryProgressEvent;
        StoreRecord {
            schema_version: SCHEMA_VERSION,
            seq,
            ts,
            event: RunEvent::RetryProgress(RetryProgressEvent {
                story_id: "SW-A".into(),
                action_identity: "todo->in-progress".into(),
                run_id: "run-1".into(),
                config_version: "c1".into(),
                attempt,
                max: 3,
                next_run_at: Some("2026-08-18T00:30:00Z".into()),
                routed_to: Some("needs-attention".into()),
                exhausted: false,
                dispatch_nonce: None,
            }),
        }
    }

    // -- SW-GT-24: retry-progress collapses to the latest cursor per key ------

    #[test]
    fn retry_progress_collapses_to_latest_per_key_preserving_the_cursor_fold() {
        // Three attempts at one gate → only the newest cursor need survive (the read
        // model + `due_retries` only ever read the latest).
        let records = vec![
            retry_rec(10, 0, 1),
            retry_rec(20, 1, 2),
            retry_rec(30, 2, 3),
        ];
        let out = compact_records(&records, &CompactionPolicy::default());
        assert_eq!(out.len(), 1, "retries collapse to the single latest cursor");
        // The fold is byte-identical across the pass: attempt 3 both before and after.
        let attempt_of = |recs: &[StoreRecord]| -> u32 {
            recs.iter()
                .filter_map(|r| match &r.event {
                    RunEvent::RetryProgress(p) => Some(((r.ts, r.seq), p.attempt)),
                    _ => None,
                })
                .max_by_key(|(ord, _)| *ord)
                .map(|(_, attempt)| attempt)
                .expect("a retry cursor")
        };
        assert_eq!(attempt_of(&records), 3);
        assert_eq!(attempt_of(&out), 3, "fold(compacted) == fold(original)");
    }

    // -- SW-AU-52: run outcome collapses to the latest per (story, run) ------

    fn outcome_rec(
        ts: u128,
        seq: u64,
        run_id: &str,
        status: crate::dashboard_stream::SessionStatus,
    ) -> StoreRecord {
        use crate::commands::sprint_run::run_store::{OutcomeSource, RunOutcomeEvent};
        StoreRecord {
            schema_version: SCHEMA_VERSION,
            seq,
            ts,
            event: RunEvent::Outcome(Box::new(RunOutcomeEvent {
                story_id: "SW-A".into(),
                run_id: run_id.into(),
                automation_id: None,
                trigger_source: None,
                workspace: None,
                scheduled_at: None,
                fired_at: None,
                work_product: None,
                status,
                summary: None,
                exit_code: Some(0),
                started_at: None,
                ended_at: "2026-08-26T00:01:00Z".into(),
                artifact_ref: None,
                validator_step: None,
                structured_declared: None,
                org: None,
                team: None,
                session_ids: Vec::new(),
                source: OutcomeSource::ProcessExit,
            })),
        }
    }

    #[test]
    fn run_outcome_collapses_to_latest_per_run_preserving_the_fold() {
        use crate::commands::sprint_run::run_store::latest_run_outcome;
        use crate::dashboard_stream::SessionStatus;
        // Two outcomes for run-1 (Failed then Success) collapse to the latest; run-2 survives.
        let records = vec![
            outcome_rec(10, 0, "run-1", SessionStatus::Failed),
            outcome_rec(20, 1, "run-1", SessionStatus::Success),
            outcome_rec(30, 2, "run-2", SessionStatus::Success),
        ];
        let out = compact_records(&records, &CompactionPolicy::default());
        assert_eq!(out.len(), 2, "run-1 collapses to its latest; run-2 kept");
        // fold(compact) == fold(original): run-1 resolves to Success both before and after.
        let status_of = |recs: &[StoreRecord], run: &str| {
            latest_run_outcome(recs, "SW-A", run).map(|o| o.status)
        };
        assert_eq!(status_of(&records, "run-1"), Some(SessionStatus::Success));
        assert_eq!(status_of(&out, "run-1"), Some(SessionStatus::Success));
        assert_eq!(status_of(&out, "run-2"), Some(SessionStatus::Success));
    }

    // -- AC-1: verdict class, latest-per-step of the current attempt ---------

    #[test]
    fn current_attempt_collapses_to_latest_per_step() {
        // Two revalidations of step s1 (Failed then Passed) at the same config →
        // only the latest survives.
        let records = vec![
            verdict_rec(1, 0, "S", "a->b", "cfg1", "s1", StepResult::Failed),
            verdict_rec(2, 1, "S", "a->b", "cfg1", "s1", StepResult::Passed),
            verdict_rec(3, 2, "S", "a->b", "cfg1", "s2", StepResult::Passed),
        ];
        let out = compact_records(&records, &CompactionPolicy::default());
        assert_eq!(out.len(), 2, "s1 collapses to its latest; s2 kept");
        // Resume fold unchanged: both steps pass.
        let passed = passed_steps(&out, "S", "a->b", "cfg1");
        assert_eq!(passed.len(), 2);
    }

    // -- AC-1: keyed verdict compaction reclaims re-validation churn ---------

    #[test]
    fn reclaims_revalidation_churn_keeps_distinct_step_and_config_history() {
        // A superseded config (cfg1) is NOT collapsed to a single record: its distinct
        // steps survive (the cross-config override snapshot reads them). Only the
        // re-validation of the SAME (config, step) collapses to its latest.
        let records = vec![
            verdict_rec(1, 0, "S", "a->b", "cfg1", "s1", StepResult::Failed),
            verdict_rec(2, 1, "S", "a->b", "cfg1", "s1", StepResult::Passed), // re-run of s1@cfg1
            verdict_rec(3, 2, "S", "a->b", "cfg1", "s2", StepResult::Passed),
            verdict_rec(4, 3, "S", "a->b", "cfg2", "s1", StepResult::Passed), // cfg2 now current
        ];
        let out = compact_records(&records, &CompactionPolicy::default());
        // s1@cfg1 collapses (2→1); s2@cfg1 and s1@cfg2 survive → 3 records.
        assert_eq!(out.len(), 3);
        assert_eq!(passed_steps(&out, "S", "a->b", "cfg1").len(), 2);
    }

    #[test]
    fn every_distinct_config_keeps_its_verdicts() {
        // Two configs observed: each keeps its verdict across a pass.
        let records = vec![
            verdict_rec(1, 0, "S", "a->b", "cfg1", "s1", StepResult::Passed),
            verdict_rec(2, 1, "S", "a->b", "cfg2", "s1", StepResult::Passed),
        ];
        let out = compact_records(&records, &CompactionPolicy::default());
        assert_eq!(out, records);
    }

    /// Code-review r3 regression (AC-5): the run-scoped per-step read folds across ALL
    /// configs. A superseded config's per-step verdicts must survive a pass or that read
    /// silently loses steps.
    #[test]
    fn run_scoped_read_survives_superseded_configs() {
        let records = vec![
            verdict_rec(1, 0, "S", "A", "c1", "s1", StepResult::Passed),
            verdict_rec(2, 1, "S", "A", "c1", "s2", StepResult::Failed),
            verdict_rec(3, 2, "S", "A", "c2", "s3", StepResult::Passed), // A advances → c1 superseded
        ];
        let out = compact_records(&records, &CompactionPolicy::default());
        assert_eq!(
            run_step_verdicts(&out, "S", "run-1"),
            run_step_verdicts(&records, "S", "run-1"),
            "the cross-config run read must not lose superseded-config steps"
        );
    }

    /// Code-review regression (AC-5): a *settled* attempt of action A carries a config
    /// string equal to action B's *current* config. Collapsing per-`(story, action)`
    /// would drop A@c9's `Failed` step.
    #[test]
    fn cross_action_config_collision_keeps_the_settled_attempt() {
        let records = vec![
            verdict_rec(1, 0, "S", "A", "c9", "s1", StepResult::Failed),
            verdict_rec(2, 1, "S", "A", "c9", "s2", StepResult::Passed),
            verdict_rec(3, 2, "S", "A", "c10", "s1", StepResult::Passed), // A advances to c10
            verdict_rec(4, 3, "S", "B", "c9", "s3", StepResult::Passed), // B current = c9 = story latest
        ];
        let out = compact_records(&records, &CompactionPolicy::default());
        assert_eq!(out, records, "every record is the latest of its own key");
        assert_eq!(
            passed_steps(&out, "S", "A", "c9"),
            passed_steps(&records, "S", "A", "c9"),
        );
    }

    /// Code-review regression (AC-5): two verdicts collide on `(ts, seq)` (reachable —
    /// `seq` resets to 0 on restart, same millisecond). The read fold keeps the last in
    /// file order; `keep_newest` must too (`>` not `>=`), or the live fold diverges.
    #[test]
    fn tie_break_matches_fold_last_wins() {
        let records = vec![
            verdict_rec(5, 0, "S", "A", "c1", "s1", StepResult::Failed),
            verdict_rec(5, 0, "S", "A", "c1", "s1", StepResult::Passed), // same (ts,seq), later
        ];
        let out = compact_records(&records, &CompactionPolicy::default());
        assert_eq!(
            passed_steps(&out, "S", "A", "c1"),
            passed_steps(&records, "S", "A", "c1"),
        );
        assert!(
            passed_steps(&out, "S", "A", "c1").contains("s1"),
            "last-in-file (Passed) wins the tie"
        );
    }

    /// Code-review regression (AC-3): `retain_audit_per_key == 0` must not erase the
    /// newest advance — it is floored to 1 so the fold's `overridden` survives.
    #[test]
    fn retain_audit_zero_is_floored_to_keep_newest_advance() {
        let policy = CompactionPolicy {
            retain_audit_per_key: 0,
            ..CompactionPolicy::default()
        };
        let records = vec![
            verdict_rec(1, 0, "S", "A", "c1", "s1", StepResult::Failed),
            advance_rec(2, 1, "S", Some("A"), "b", true),
        ];
        let out = compact_records(&records, &policy);
        assert!(
            latest_advance(&out, "S").overridden,
            "the override advance must survive a retain=0 misconfiguration"
        );
    }

    #[test]
    fn advance_retention_tie_break_matches_fold_first_wins() {
        let policy = CompactionPolicy {
            retain_audit_per_key: 1,
            ..CompactionPolicy::default()
        };
        let records = vec![
            verdict_rec(1, 0, "S", "A", "c1", "s1", StepResult::Passed),
            advance_rec(5, 9, "S", Some("A"), "b", true), // first-in-file at the tie
            advance_rec(5, 9, "S", Some("A"), "b", false), // same (ts,seq), later
        ];
        let out = compact_records(&records, &policy);
        assert!(
            latest_advance(&out, "S").overridden,
            "the fold-relevant (first-in-file) advance must survive window truncation"
        );
    }

    // -- AC-3: retention window keeps the newest advance ---------------------

    #[test]
    fn retention_keeps_newest_advances_and_drops_oldest() {
        let policy = CompactionPolicy {
            retain_audit_per_key: 2,
            ..CompactionPolicy::default()
        };
        let records = vec![
            advance_rec(1, 0, "S", Some("a->b"), "b", false),
            advance_rec(2, 1, "S", Some("a->b"), "c", false),
            advance_rec(3, 2, "S", Some("a->b"), "d", true),
        ];
        let out = compact_records(&records, &policy);
        let tos: Vec<&str> = out
            .iter()
            .filter_map(|r| match &r.event {
                RunEvent::Advance(a) => Some(a.to.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            tos,
            vec!["c", "d"],
            "oldest advance dropped, newest two kept"
        );
    }

    #[test]
    fn retention_preserves_overridden_fold() {
        // Even with a tight window, the newest advance (the override) survives, so the
        // fold's `overridden` stays true.
        let policy = CompactionPolicy {
            retain_audit_per_key: 1,
            ..CompactionPolicy::default()
        };
        let records = vec![
            verdict_rec(1, 0, "S", "a->b", "cfg1", "s1", StepResult::Failed),
            advance_rec(2, 1, "S", Some("a->b"), "b", false),
            advance_rec(3, 2, "S", Some("a->b"), "b", true),
        ];
        let out = compact_records(&records, &policy);
        assert!(latest_advance(&out, "S").overridden);
    }

    #[test]
    fn config_edits_are_retained_per_key() {
        let policy = CompactionPolicy {
            retain_audit_per_key: 1,
            ..CompactionPolicy::default()
        };
        let records = vec![
            config_edit_rec(1, 0, "team1", "a->b"),
            config_edit_rec(2, 1, "team1", "a->b"),
            config_edit_rec(3, 2, "team2", "a->b"),
        ];
        let out = compact_records(&records, &policy);
        let edits = out
            .iter()
            .filter(|r| matches!(r.event, RunEvent::ConfigEdit(_)))
            .count();
        assert_eq!(
            edits, 2,
            "newest per (team, action) key: 1 each for two teams"
        );
    }

    // -- AC-4: trigger policy ------------------------------------------------

    #[test]
    fn dead_ratio_reflects_removed_lines() {
        // 3 of 4 verdicts are superseded/settled dead weight.
        let records = vec![
            verdict_rec(1, 0, "S", "a->b", "cfg1", "s1", StepResult::Failed),
            verdict_rec(2, 1, "S", "a->b", "cfg1", "s1", StepResult::Passed),
            verdict_rec(3, 2, "S", "a->b", "cfg1", "s1", StepResult::Passed),
            verdict_rec(4, 3, "S", "a->b", "cfg1", "s1", StepResult::Passed),
        ];
        // All four collapse to one → ratio 0.75.
        let r = dead_ratio(&records, &CompactionPolicy::default());
        assert!((r - 0.75).abs() < 1e-9, "ratio was {r}");
    }

    #[test]
    fn should_compact_gates_on_size_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run-store.jsonl");
        // A tiny, fully-dead store: below the size gate, so no pass despite high ratio.
        let body = serialize_records(&[
            verdict_rec(1, 0, "S", "a->b", "cfg1", "s1", StepResult::Failed),
            verdict_rec(2, 1, "S", "a->b", "cfg1", "s1", StepResult::Passed),
        ])
        .unwrap();
        std::fs::write(&path, body).unwrap();
        assert!(
            !should_compact(&path, &CompactionPolicy::default()),
            "cheap size gate short-circuits before the ratio fold"
        );
        // Lower the size gate → the ratio gate now decides (and passes).
        let policy = CompactionPolicy {
            min_size_bytes: 0,
            ..CompactionPolicy::default()
        };
        assert!(should_compact(&path, &policy));
    }

    // -- AC-2/AC-7: the I/O pass --------------------------------------------

    #[test]
    fn compact_store_replaces_atomically_and_reports() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run-store.jsonl");
        let records = vec![
            verdict_rec(1, 0, "S", "a->b", "cfg1", "s1", StepResult::Failed),
            verdict_rec(2, 1, "S", "a->b", "cfg1", "s1", StepResult::Passed),
            verdict_rec(3, 2, "S", "a->b", "cfg1", "s1", StepResult::Passed),
        ];
        std::fs::write(&path, serialize_records(&records).unwrap()).unwrap();

        let policy = CompactionPolicy {
            min_size_bytes: 0,
            ..CompactionPolicy::default()
        };
        let metrics = compact_store(&path, &policy).unwrap();
        assert_eq!(metrics.lines_before, 3);
        assert_eq!(metrics.lines_after, 1);
        assert!(metrics.bytes_reclaimed() > 0);

        // The file on disk now folds identically for the live key.
        let (reread, _h) = read_records(&path);
        assert_eq!(reread.len(), 1);
        assert_eq!(passed_steps(&reread, "S", "a->b", "cfg1").len(), 1);
    }

    #[test]
    fn compact_store_drops_corrupt_lines_and_preserves_fold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run-store.jsonl");
        // A valid record, a torn/garbage line, another valid record.
        let mut body = serialize_records(&[verdict_rec(
            1,
            0,
            "S",
            "a->b",
            "cfg1",
            "s1",
            StepResult::Failed,
        )])
        .unwrap();
        body.push_str("{ this is not json\n");
        body.push_str(
            &serialize_records(&[verdict_rec(
                2,
                1,
                "S",
                "a->b",
                "cfg1",
                "s1",
                StepResult::Passed,
            )])
            .unwrap(),
        );
        std::fs::write(&path, body).unwrap();

        let policy = CompactionPolicy {
            min_size_bytes: 0,
            ..CompactionPolicy::default()
        };
        compact_store(&path, &policy).unwrap();

        // The corrupt line is gone; the store reads clean and the live fold is intact.
        let (reread, health) = read_records(&path);
        assert_eq!(health.skipped_lines, 0, "compaction rewrote a clean store");
        assert_eq!(reread.len(), 1);
        assert_eq!(passed_steps(&reread, "S", "a->b", "cfg1").len(), 1);
    }

    #[test]
    fn compact_store_missing_file_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent.jsonl");
        let metrics = compact_store(&path, &CompactionPolicy::default()).unwrap();
        assert_eq!(metrics.lines_before, 0);
        assert!(
            !path.exists(),
            "no empty file materialized for an absent store"
        );
    }

    #[test]
    fn maybe_compact_emits_on_success() {
        use std::cell::RefCell;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run-store.jsonl");
        std::fs::write(
            &path,
            serialize_records(&[
                verdict_rec(1, 0, "S", "a->b", "cfg1", "s1", StepResult::Failed),
                verdict_rec(2, 1, "S", "a->b", "cfg1", "s1", StepResult::Passed),
            ])
            .unwrap(),
        )
        .unwrap();
        let seen: RefCell<Vec<&'static str>> = RefCell::new(Vec::new());
        let policy = CompactionPolicy {
            min_size_bytes: 0,
            ..CompactionPolicy::default()
        };
        maybe_compact(&path, &policy, |topic, _payload| {
            seen.borrow_mut().push(topic)
        });
        assert_eq!(seen.into_inner(), vec![EVENT_COMPACTION]);
    }

    // -- AC-5: the correctness property (seeded, dependency-free) ------------

    /// A tiny deterministic PRNG (a fixed-seed LCG — Numerical Recipes constants) so
    /// the property runs over randomized histories without adding a `proptest` dep.
    struct Lcg(u64);
    impl Lcg {
        fn next_u64(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next_u64() % n
        }
    }

    /// Generate a plausible random log: several stories × actions × a few configs ×
    /// steps, interleaved verdicts + advances. **`ts` is drawn independent of file
    /// position** (a random value), while `seq` is unique per record — modelling both a
    /// wall-clock regression and the `append_to` stamp-before-lock race, so file order
    /// and `(ts, seq)` order disagree *within a bucket*. It also occasionally reuses the
    /// previous exact ordinal (a same-millisecond `seq`-reset restart collision). This is
    /// the region the earlier monotonic generator could not reach.
    fn gen_log(seed: u64, len: usize) -> Vec<StoreRecord> {
        let mut rng = Lcg(seed);
        let mut records = Vec::with_capacity(len);
        let (mut prev_ts, mut prev_seq) = (0u128, 0u64);
        for i in 0..len {
            let (ts, seq) = if i > 0 && rng.below(6) == 0 {
                (prev_ts, prev_seq) // exact (ts, seq) tie — same-ms restart collision
            } else {
                (rng.below(len as u64) as u128, i as u64) // ts out of file order; seq unique
            };
            prev_ts = ts;
            prev_seq = seq;
            let story = format!("S{}", rng.below(3));
            let action = format!("a{}->b{}", rng.below(2), rng.below(2));
            if rng.below(4) == 0 {
                let over = rng.below(2) == 0;
                records.push(advance_rec(ts, seq, &story, Some(&action), "next", over));
            } else {
                let cfg = format!("cfg{}", rng.below(3));
                let step = format!("s{}", rng.below(3));
                let result = match rng.below(3) {
                    0 => StepResult::Passed,
                    1 => StepResult::Failed,
                    _ => StepResult::Pending,
                };
                records.push(verdict_rec(ts, seq, &story, &action, &cfg, &step, result));
            }
        }
        records
    }

    /// Every distinct `(story, action, config)` present in a log — the universe of
    /// keys to check. The *current* attempt per `(story, action)` is the live key;
    /// the property is asserted for it.
    fn live_keys(records: &[StoreRecord]) -> Vec<(String, String, String)> {
        // Current config per (story, action) = newest verdict's config, last-wins on a
        // `(ts, seq)` tie to match the read fold's `.last()` (the oracle's definition of
        // "current"), so we never pin a config the fold considers settled.
        let mut current: BTreeMap<(String, String), (Ordinal, String)> = BTreeMap::new();
        for r in records {
            if let RunEvent::Verdict(v) = &r.event {
                let key = (v.story_id.clone(), v.action_identity.clone());
                let ord = (r.ts, r.seq);
                match current.get(&key) {
                    Some((prev, _)) if *prev > ord => {}
                    _ => {
                        current.insert(key, (ord, v.config_version.clone()));
                    }
                }
            }
        }
        current
            .into_iter()
            .map(|((s, a), (_, c))| (s, a, c))
            .collect()
    }

    fn stories(records: &[StoreRecord]) -> std::collections::BTreeSet<String> {
        records
            .iter()
            .filter_map(|r| match &r.event {
                RunEvent::Verdict(v) => Some(v.story_id.clone()),
                RunEvent::Advance(a) => Some(a.story_id.clone()),
                RunEvent::RetryProgress(r) => Some(r.story_id.clone()),
                RunEvent::Outcome(o) => Some(o.story_id.clone()),
                RunEvent::ConfigEdit(_) => None,
                RunEvent::StepResult(e) => Some(e.story_id.clone()),
            })
            .collect()
    }

    #[test]
    fn property_fold_is_byte_identical_for_live_keys() {
        let policy = CompactionPolicy::default();
        for seed in 0..256u64 {
            let original = gen_log(seed.wrapping_mul(2654435761).wrapping_add(1), 150);
            let compacted = compact_records(&original, &policy);

            // The compacted log is never longer than the original.
            assert!(compacted.len() <= original.len());

            // (a) The run-scoped cross-config per-step read, per story.
            for story in stories(&original) {
                assert_eq!(
                    run_step_verdicts(&original, &story, "run-1"),
                    run_step_verdicts(&compacted, &story, "run-1"),
                    "run-scoped read diverged for {story} (seed {seed})"
                );
            }

            // (b) Resume view — every live key, pinned to its current attempt.
            for (story, action, cfg) in live_keys(&original) {
                let before = passed_steps(&original, &story, &action, &cfg);
                let after = passed_steps(&compacted, &story, &action, &cfg);
                assert_eq!(
                    before, after,
                    "resume passed-set diverged for {story}/{action}@{cfg} (seed {seed})"
                );
            }
        }
    }

    #[test]
    fn compaction_is_idempotent() {
        let policy = CompactionPolicy::default();
        let original = gen_log(12345, 120);
        let once = compact_records(&original, &policy);
        let twice = compact_records(&once, &policy);
        assert_eq!(once, twice, "a second pass over a compacted log is a no-op");
    }

    // -- helpers ------------------------------------------------------------

    /// The story's latest surviving advance by `(ts, seq)`, first-in-file on a tie.
    fn latest_advance(records: &[StoreRecord], story: &str) -> AdvanceEvent {
        let mut best: Option<((u128, u64), &AdvanceEvent)> = None;
        for r in records {
            if let RunEvent::Advance(a) = &r.event {
                if a.story_id == story && best.is_none_or(|(ord, _)| (r.ts, r.seq) > ord) {
                    best = Some(((r.ts, r.seq), a));
                }
            }
        }
        best.expect("an advance").1.clone()
    }

    // -- AC-6: disjoint-ownership guard -------------------------------------

    /// AC-6 regression guard: compaction touches **only** the run store. It must never grow a path into the
    /// git-synced story frontmatter — no status writer, no metadata editor, no doc
    /// rewrite. Asserted structurally against the module's own source (above the test
    /// module) so a later change can't silently cross the SE-6 line.
    #[test]
    fn module_touches_only_the_run_store() {
        let src = include_str!("compaction.rs");
        let code = src.split("#[cfg(test)]").next().unwrap_or(src);
        for forbidden in [
            "frontmatter_editor",
            "apply_metadata_edit",
            "update_frontmatter_status",
            "rewrite_field",
            "story_linker",
            "pipeline_scanner",
        ] {
            assert!(
                !code.contains(forbidden),
                "compaction must not reach into frontmatter/status (found `{forbidden}`)"
            );
        }
    }

    // A run's earlier outcome rows that name a session survive compaction, so the union of its
    // sessions does too.
    #[test]
    fn compaction_keeps_the_sessions_of_a_runs_earlier_outcomes() {
        use crate::commands::sprint_run::run_store::project_run_outcome;
        use crate::dashboard_stream::SessionStatus;
        let mut first = outcome_rec(10, 0, "run-1", SessionStatus::Failed);
        let mut second = outcome_rec(20, 1, "run-1", SessionStatus::Success);
        for (rec, id) in [(&mut first, "tab-1"), (&mut second, "tab-2")] {
            if let RunEvent::Outcome(o) = &mut rec.event {
                o.session_ids = vec![id.to_string()];
            }
        }
        let records = vec![first, second, outcome_rec(30, 2, "run-2", SessionStatus::Success)];
        let out = compact_records(&records, &CompactionPolicy::default());
        let sessions = |recs: &[StoreRecord]| project_run_outcome(recs, "run-1").unwrap().1.outcome.session_ids;
        assert_eq!(sessions(&out), ["tab-1", "tab-2"]);
        assert_eq!(sessions(&out), sessions(&records));
    }
}
