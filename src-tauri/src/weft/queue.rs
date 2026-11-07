// SPDX-License-Identifier: MIT
//! The `wv_queue` work-item lifecycle + the `wv_key_ledger` re-open-protection
//! record (SW-WV-B2).
//!
//! `wv_queue` is the single machine-checkable record triage writes and the
//! controller consumes. Rows progress through a **four-state** machine —
//! `pending → claimed → dispatched → resolved` (ARCHITECTURE §2.4) — with exactly
//! one owner per mutable row (single-writer CAS, ARCHITECTURE §1.2). On resolve
//! the row is **pruned** (deleted) and a compact `wv_key_ledger` row is written in
//! the **same** IMMEDIATE transaction, so a resolved item can never re-open after
//! its queue row is gone.
//!
//! Every function here runs synchronous SQLite on a caller-supplied
//! [`WeftStore`] connection; the MCP handlers in `mcp/handler.rs` dispatch each
//! through `await_blocking` so the async reactor / UI thread never blocks
//! (ARCHITECTURE §1.6, AC-7). This module is part of the sole sanctioned write
//! path — every `INSERT`/`UPDATE`/`DELETE` against `wv_queue` / `wv_key_ledger`
//! lives here (the `only_weft_sqlite_writes_wv_tables` CI test excludes only
//! `weft/sqlite.rs` + its tests, so the queue writes are routed through the
//! generic [`super::sqlite`] helpers rather than raw SQL in this file).

use rusqlite::{params, OptionalExtension, TransactionBehavior};

use super::sqlite::wv_append;
use super::store::{WeftStore, WeftStoreError, WeftStoreResult};
use super::tripwire::{evaluate_tripwire, TripwireInput, TripwireVerdict};

/// The `wv_queue` table name — a compile-time constant threaded to the sanctioned
/// write-path helpers (never user input).
const QUEUE: &str = "wv_queue";

/// Queue authority comes from verified launch context; the current process is
/// retained separately so a stable controller's successive invocations are auditable.
#[derive(Debug, Clone)]
pub struct QueueCaller {
    pub owner: String,
    pub session: String,
}
impl From<&str> for QueueCaller {
    fn from(session: &str) -> Self {
        Self {
            owner: session.into(),
            session: session.into(),
        }
    }
}
impl From<&String> for QueueCaller {
    fn from(session: &String) -> Self {
        Self::from(session.as_str())
    }
}

/// One non-resolved `wv_queue` row, as returned by [`queue_list`] (AC-9).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueRow {
    pub id: i64,
    pub source: String,
    pub state: String,
    pub claimed_by: Option<String>,
    pub scope: Option<String>,
    pub created: i64,
    pub updated: i64,
    pub refs: Option<String>,
    pub note: Option<String>,
}

/// Typed refusals from the queue verbs — a lost CAS or an ownership/state
/// violation is a *normal* return the caller must distinguish from a storage
/// error, mirroring [`super::store::MutateResult`] semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueError {
    /// A concurrent claimer won the CAS on this row; the loser gets this rather
    /// than a silent no-op (AC-3, single-writer rule).
    ClaimLost,
    /// `wv_resolve_queue` was called by a session that does not own the row
    /// (`claimed_by != caller`) — refused, row unchanged (AC-3).
    OwnerMismatch,
    /// `wv_resolve_queue` was called on a row not in `claimed`/`dispatched`
    /// (e.g. a still-`pending` row) — resolving without claiming is refused
    /// (AC-3, Task 4).
    InvalidState,
    /// SW-WV-C5 (AC-2): `wv_mark_dispatched` was called on a row not in `claimed`
    /// state (or not owned by the caller) — the `claimed → dispatched` transition
    /// only fires from `claimed`. Distinct from [`InvalidState`] so the release path
    /// surfaces a `stateConflict` the operator reads as "claim it first", not the
    /// resolve-path "resolve without claiming".
    StateConflict,
    /// SW-WV-C5 (AC-1 ordering): `wv_mark_dispatched` was called before a dispatch
    /// brief exists in `wv_dispatch_records` for the row's worker handle — the
    /// load-bearing `claim → write brief → spawn → dispatch` order was violated. The
    /// brief must be frozen before release so an observer can read the finish line at
    /// any time.
    BriefRequired,
}

impl std::fmt::Display for QueueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueueError::ClaimLost => write!(f, "claim lost: a concurrent claimer won this row"),
            QueueError::OwnerMismatch => {
                write!(f, "owner mismatch: the caller does not own this queue row")
            }
            QueueError::InvalidState => write!(
                f,
                "invalid state: only a claimed or dispatched row may be resolved"
            ),
            QueueError::StateConflict => write!(
                f,
                "state conflict: only a claimed row you own may be dispatched"
            ),
            QueueError::BriefRequired => write!(
                f,
                "brief must be written before dispatching — call wv_write_dispatch first"
            ),
        }
    }
}

impl std::error::Error for QueueError {}

/// Enqueue a `pending` work item, minting its id live-max inside an IMMEDIATE
/// transaction (AC-2). Returns the row id.
///
/// AC-5 dedupe: when `dedupe_key` is `Some` and a **live** row (state in
/// `pending`/`claimed`/`dispatched`) already carries it, the enqueue is silently
/// ignored and the existing id is returned — "push wakes, pull verifies": triage
/// may call this on every trigger pulse without flooding or re-queuing in-flight
/// work. Once that row resolves (is pruned), a new enqueue with the same key
/// succeeds (its `wv_key_ledger` sibling is a separate table, so the `UNIQUE`
/// index on `wv_queue.dedupe_key` no longer conflicts).
///
/// `scope` is stamped by the enqueuer only (AC-6); the claimer never rewrites it.
///
/// SW-WV-D4 (AC-1): `dispatch_peers` is a nullable JSON array of worker names
/// (already serialized by the caller) naming the peers the claiming worker may read
/// on the bounded peer lane. Triage enqueues with `None` (no peers at triage time);
/// the controller populates it at dispatch time (SW-WV-C1). A `None` writes SQL
/// `NULL` — the lane is closed for that task.
#[allow(clippy::too_many_arguments)]
pub fn enqueue(
    store: &WeftStore,
    source: &str,
    scope: Option<&str>,
    refs: Option<&str>,
    note: Option<&str>,
    dedupe_key: Option<&str>,
    dispatch_peers: Option<&str>,
    now: i64,
) -> WeftStoreResult<i64> {
    let mut conn = store.conn()?;
    // AC-5: short-circuit BEFORE the append when a live row already holds this key.
    // Read on the same connection; the append below opens its own IMMEDIATE tx, and
    // a concurrent racer that slips in between is still caught by the `UNIQUE`
    // index on `dedupe_key` (whose only live rows are non-resolved by construction —
    // resolve prunes the row).
    if let Some(key) = dedupe_key {
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM wv_queue \
                 WHERE dedupe_key=?1 AND state IN ('pending','claimed','dispatched')",
                params![key],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            return Ok(id);
        }
    }

    // The caller-supplied INSERT binds the minted id as ?1 (wv_append prepends it);
    // the remaining columns follow in ?2.. order.
    wv_append(
        &mut conn,
        QUEUE,
        "INSERT INTO wv_queue \
         (id, source, state, scope, created, updated, refs, note, dedupe_key, dispatch_peers) \
         VALUES (?1, ?2, 'pending', ?3, ?4, ?4, ?5, ?6, ?7, ?8)",
        &[
            &source,
            &scope,
            &now,
            &refs,
            &note,
            &dedupe_key,
            &dispatch_peers,
        ],
    )
}

/// CAS-claim one `pending` row into `claimed`, stamping `claimed_by` + `updated`
/// (AC-3). When `scope` is `Some`, only rows with that scope are candidates.
/// Returns:
/// - `Ok(Some(id))` — a row was claimed;
/// - `Ok(None)` — no `pending` row is available (empty queue / all claimed);
/// - `Err(QueueError::ClaimLost)` — a candidate was found every attempt but a
///   concurrent claimer won the guarded UPDATE each time (retry budget exhausted).
///
/// The `scope` is never written here (AC-6) — only `claimed_by` and `updated`.
pub fn claim(
    store: &WeftStore,
    claimed_by: &str,
    scope: Option<&str>,
    now: i64,
) -> Result<Option<i64>, WeftStoreError> {
    let mut conn = store.conn()?;
    // Bounded CAS retry loop (mirrors `claim_ready` in orchestration/sqlite.rs):
    // pick the lowest-id pending candidate, guard the UPDATE on `state='pending'`,
    // and retry if a concurrent claimer flipped it first. A small fixed budget is
    // ample — each loss means another claimer made progress, so the queue drains.
    for _ in 0..16 {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let candidate: Option<i64> = match scope {
            Some(s) => tx
                .query_row(
                    "SELECT id FROM wv_queue \
                     WHERE state='pending' AND scope=?1 ORDER BY id LIMIT 1",
                    params![s],
                    |r| r.get(0),
                )
                .optional()?,
            None => tx
                .query_row(
                    "SELECT id FROM wv_queue WHERE state='pending' ORDER BY id LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .optional()?,
        };
        let Some(id) = candidate else {
            // Nothing to claim — commit the (read-only) tx and report empty.
            tx.commit()?;
            return Ok(None);
        };
        let changed = tx.execute(
            "UPDATE wv_queue SET state='claimed', claimed_by=?2, updated=?3 \
             WHERE id=?1 AND state='pending'",
            params![id, claimed_by, now],
        )?;
        if changed == 1 {
            // Audit the transition in the SAME tx as the ledger mutation (AC-7 /
            // ARCHITECTURE §1.6) before commit.
            super::sqlite::write_audit_public(&tx, QUEUE, "mutate", id)?;
            tx.commit()?;
            return Ok(Some(id));
        }
        // Lost the CAS on this candidate — a concurrent claimer flipped it. Drop
        // the tx (rollback the no-op) and retry with a fresh candidate.
    }
    Err(WeftStoreError::from_queue(QueueError::ClaimLost))
}

/// The outcome of a [`claim_with_tripwire`] call: the claimed row id, the verdict
/// the tripwire returned, and the identity stamped into `claimed_by`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TripwireClaim {
    /// The `wv_queue` row that was claimed.
    pub id: i64,
    /// The keep-vs-delegate verdict, incl. which row fired (auditable — AC-3/AC-5).
    pub verdict: TripwireVerdict,
    /// The verified claimant's session, independent of the tripwire verdict.
    pub claimed_by: String,
}

/// CAS-claim one `pending` row AND run the dispatch tripwire in the SAME IMMEDIATE
/// transaction (SW-WV-C2 Task 3 / AC-2, AC-3, AC-8).
///
/// The caller supplies observed evidence (`input`) and the verified claimant's
/// session (`claimant`). The tripwire is evaluated (pure), and
/// the row is transitioned `pending → claimed` with BOTH `claimed_by` and
/// `tripwire_verdict` set in the one UPDATE:
/// - `Keep`     → `claimed_by = claimant`, `tripwire_verdict = 'keep'`;
/// - `Delegate` → `claimed_by = claimant`, `tripwire_verdict = 'delegate'`.
///
/// No row is ever left `claimed` with `tripwire_verdict = 'pending'` (AC-2), and no
/// row is left `claimed_by = NULL` with a non-`pending` verdict (AC-3) — the two
/// columns move together atomically. The `wv_write_log` audit row is written in the
/// SAME transaction (AC-8, ARCHITECTURE §1.6). The caller drives the actual dispatch
/// (launching the worker) on a `Delegate` verdict AFTER this commits — this function
/// only records the decision on the ledger.
///
/// Returns `Ok(None)` when no `pending` row is available; `Err(QueueError::ClaimLost)`
/// when a concurrent claimer won every retry (same contract as [`claim`]).
pub fn claim_with_tripwire(
    store: &WeftStore,
    input: &TripwireInput,
    claimant: impl Into<QueueCaller>,
    scope: Option<&str>,
    now: i64,
) -> Result<Option<TripwireClaim>, WeftStoreError> {
    let claimant = claimant.into();
    let verdict = evaluate_tripwire(input);
    // The stored verdict string is owned by `TripwireVerdict::verdict_str` (unit-tested
    // to match the column CHECK) — never re-derived inline.
    let verdict_str = verdict.verdict_str();
    let claimed_by = claimant.owner.clone();
    let fired_row = verdict.fired_row;

    let mut conn = store.conn()?;
    // Bounded CAS retry loop — same shape as `claim`, but the UPDATE also stamps the
    // verdict so the decision and the claim can never be written in separate steps.
    for _ in 0..16 {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let candidate: Option<i64> = match scope {
            Some(s) => tx
                .query_row(
                    "SELECT id FROM wv_queue \
                     WHERE state='pending' AND scope=?1 ORDER BY id LIMIT 1",
                    params![s],
                    |r| r.get(0),
                )
                .optional()?,
            None => tx
                .query_row(
                    "SELECT id FROM wv_queue WHERE state='pending' ORDER BY id LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .optional()?,
        };
        let Some(id) = candidate else {
            tx.commit()?;
            return Ok(None);
        };
        let changed = tx.execute(
            "UPDATE wv_queue \
             SET state='claimed', claimed_by=?2, tripwire_verdict=?3, fired_row=?4, updated=?5 \
             WHERE id=?1 AND state='pending'",
            params![id, claimed_by, verdict_str, fired_row, now],
        )?;
        if changed == 1 {
            // Audit the transition (claim + verdict) in the SAME tx (AC-8).
            super::sqlite::write_audit_as(&tx, QUEUE, "mutate", id, Some(&claimant.session))?;
            tx.commit()?;
            return Ok(Some(TripwireClaim {
                id,
                verdict,
                claimed_by,
            }));
        }
        // Lost the CAS — retry with a fresh candidate.
    }
    Err(WeftStoreError::from_queue(QueueError::ClaimLost))
}

/// Increment the verify-and-fix cycle count on a `claimed`/`dispatched` `wv_queue`
/// row and re-evaluate the mid-flight (Row-5-only) tripwire (SW-WV-C2 Task 4 /
/// AC-4, AC-8). Runs entirely in ONE IMMEDIATE transaction:
///
/// 1. read the row's current `verify_fix_cycles` + `state` under the tx;
/// 2. increment the count and, when the NEW count crosses the threshold
///    ([`super::tripwire::cycle_crosses_threshold`]), also set
///    `tripwire_verdict = 'delegate'` — the ONLY path that flips a verdict AFTER a
///    row is claimed (all other verdict writes happen at claim time);
/// 3. write the `wv_write_log` audit row in the SAME tx (AC-8).
///
/// Returns the new cycle count and whether the threshold was crossed on this call.
/// Refuses [`QueueError::InvalidState`] when the row is missing or not in a
/// mid-flight state (`claimed`/`dispatched`) — a `pending`/`resolved` row has no
/// live cycle to report against.
pub fn report_cycle(
    store: &WeftStore,
    id: i64,
    now: i64,
) -> Result<Result<CycleReport, QueueError>, WeftStoreError> {
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    let row: Option<(i64, String)> = tx
        .query_row(
            "SELECT verify_fix_cycles, state FROM wv_queue WHERE id=?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((cycles, state)) = row else {
        return Ok(Err(QueueError::InvalidState));
    };
    if !matches!(state.as_str(), "claimed" | "dispatched") {
        return Ok(Err(QueueError::InvalidState));
    }

    let new_cycles = cycles + 1;
    // Edge-triggered: `crossed` is true ONLY on the call that takes the count from
    // below the threshold to at/above it (2→3), not on any subsequent call. A
    // controller that keeps reporting after the fire gets `ok`, not a repeated
    // `tripwire_fired` — the verdict is already `delegate` and stays put.
    let crossed = !super::tripwire::cycle_crosses_threshold(cycles as u32)
        && super::tripwire::cycle_crosses_threshold(new_cycles as u32);

    // ONE UPDATE. When the increment crosses the Row-5 threshold it ALSO flips the
    // verdict to `delegate` and records `fired_row=5` (the only path that flips a
    // verdict after claim, AC-4); below the threshold those two SET clauses are
    // CASE-guarded to leave the existing verdict/fired_row untouched. Guard on the
    // mid-flight state so a concurrent resolve can't race the bump.
    let crossed_flag = crossed as i64;
    let changed = tx.execute(
        "UPDATE wv_queue \
         SET verify_fix_cycles=?2, \
             tripwire_verdict = CASE WHEN ?3=1 THEN 'delegate' ELSE tripwire_verdict END, \
             fired_row        = CASE WHEN ?3=1 THEN 5 ELSE fired_row END, \
             updated=?4 \
         WHERE id=?1 AND state IN ('claimed','dispatched')",
        params![id, new_cycles, crossed_flag, now],
    )?;
    if changed != 1 {
        return Ok(Err(QueueError::InvalidState));
    }
    super::sqlite::write_audit_public(&tx, QUEUE, "mutate", id)?;
    tx.commit()?;
    Ok(Ok(CycleReport {
        cycles: new_cycles,
        crossed,
    }))
}

/// The result of a [`report_cycle`] increment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CycleReport {
    /// The verify-and-fix cycle count AFTER this increment.
    pub cycles: i64,
    /// The increment crossed the Row-5 threshold on this call — the controller must
    /// stop and dispatch the remainder (`tripwire_verdict` is now `delegate`).
    pub crossed: bool,
}

/// SW-WV-C5 (AC-1, AC-2, AC-3) — the release transition: stamp `state='dispatched'`,
/// `dispatched_at`, and `dispatch_handle` on a `claimed` row owned by `claimed_by`,
/// then RETURN. This is "start-and-release": the controller calls this AFTER a
/// successful `open_workspace` spawn, and the call blocks on nothing — no poll, no
/// wait, no courier loop lives on this path (AC-3).
///
/// The load-bearing ORDER is `claim → write brief → spawn → dispatch`. Before the
/// transition this refuses [`QueueError::BriefRequired`] if no dispatch brief exists
/// in `wv_dispatch_records` for `story_id` (the worker/roster key the brief was
/// written under, C1) — so the finish line is frozen before release and cannot be
/// retro-fitted to whatever the worker did (AC-1). A row not in `claimed` state, or
/// not owned by `claimed_by`, is refused [`QueueError::StateConflict`] (AC-2). Both
/// the guard read and the CAS run inside ONE IMMEDIATE transaction; the
/// `wv_write_log` audit row co-commits in the same tx (ARCHITECTURE §1.6, AC-7).
pub fn mark_dispatched(
    store: &WeftStore,
    id: i64,
    claimed_by: impl Into<QueueCaller>,
    story_id: &str,
    dispatch_handle: &str,
    now: i64,
) -> Result<Result<(), QueueError>, WeftStoreError> {
    let caller = claimed_by.into();
    let claimed_by = caller.owner.as_str();
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    // AC-1 ordering: a FRESH brief must be written before the row is released. The
    // guard requires an `open` `wv_dispatch_records` row for this worker handle —
    // `open` (not merely present) is the discriminator that proves `wv_write_dispatch`
    // (C1) ran for THIS dispatch and the worker has not yet signalled done. `story_id`
    // is a reusable roster key and dispatch records are never pruned (they are only
    // flipped `open → done` by the completion hook), so a bare existence check would
    // let a stale `done` brief from a PRIOR cycle satisfy the guard — exactly the
    // retro-fit this ordering rule forbids. Requiring `open` closes that hole.
    let brief_open: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM wv_dispatch_records WHERE story_id=?1 AND state='open' LIMIT 1",
            params![story_id],
            |r| r.get(0),
        )
        .optional()?;
    if brief_open.is_none() {
        // The tx drops (rollback) on return — nothing was mutated.
        return Ok(Err(QueueError::BriefRequired));
    }

    // AC-2: `claimed → dispatched`, stamping the release-time runtime state. Guarded
    // on `state='claimed' AND claimed_by=?` so it can never jump a `pending` row nor
    // a row owned by someone else.
    let changed = tx.execute(
        "UPDATE wv_queue \
         SET state='dispatched', dispatched_at=?3, dispatch_handle=?4, updated=?3 \
         WHERE id=?1 AND state='claimed' AND claimed_by=?2",
        params![id, claimed_by, now, dispatch_handle],
    )?;
    if changed != 1 {
        return Ok(Err(QueueError::StateConflict));
    }
    super::sqlite::write_audit_as(&tx, QUEUE, "mutate", id, Some(&caller.session))?;
    tx.commit()?;
    Ok(Ok(()))
}

/// Test-seam: stamp `state='dispatched'` on a `claimed` row owned by `claimed_by`
/// WITHOUT the C5 brief-ordering guard (B2's four-state state-machine test predates
/// C1's dispatch records and only needs to exercise the `dispatched` state). C5's
/// [`mark_dispatched`] is the real release transition; this bypass keeps the B2
/// coverage self-contained. Guarded on `state='claimed' AND claimed_by=?`.
#[cfg(test)]
pub fn mark_dispatched_for_test(
    store: &WeftStore,
    id: i64,
    claimed_by: &str,
    dispatch_handle: Option<&str>,
    now: i64,
) -> Result<(), WeftStoreError> {
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute(
        "UPDATE wv_queue SET state='dispatched', dispatch_handle=?4, updated=?3 \
         WHERE id=?1 AND state='claimed' AND claimed_by=?2",
        params![id, claimed_by, now, dispatch_handle],
    )?;
    if changed != 1 {
        return Err(WeftStoreError::from_queue(QueueError::InvalidState));
    }
    super::sqlite::write_audit_public(&tx, QUEUE, "mutate", id)?;
    tx.commit()?;
    Ok(())
}

/// The outcome of a [`resolve`] call — success carries no payload beyond the
/// prune having happened.
pub type ResolveResult = Result<(), QueueError>;

/// The single-snapshot row read at the top of [`resolve`] — the guard columns plus
/// the tripwire columns preserved into the key-ledger `meta` blob (AC-5). A named
/// struct rather than a 6-tuple (keeps the read legible + satisfies clippy's
/// `type_complexity`).
struct ResolveSnapshot {
    claimed_by: Option<String>,
    state: String,
    refs: Option<String>,
    tripwire_verdict: String,
    verify_fix_cycles: i64,
    fired_row: Option<i64>,
}

/// Prune a `claimed`/`dispatched` row on resolve and write its `wv_key_ledger`
/// sibling in the **same** IMMEDIATE transaction (AC-4). Refuses:
/// - [`QueueError::OwnerMismatch`] — `claimed_by != caller` (row unchanged);
/// - [`QueueError::InvalidState`] — the row is not in `claimed`/`dispatched`
///   (resolving a still-`pending` row without claiming it is refused).
///
/// Atomicity: the `DELETE` from `wv_queue` and the `INSERT` into `wv_key_ledger`
/// are one transaction — a crash between them leaves neither change (no
/// half-prune). The `ref` recorded in the ledger is the resolved row's `refs`
/// column (the re-open-protection key: id + ref).
pub fn resolve(
    store: &WeftStore,
    id: i64,
    caller: impl Into<QueueCaller>,
    now: i64,
) -> Result<ResolveResult, WeftStoreError> {
    let caller = caller.into();
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    // Read the row's current owner + state + ref + tripwire columns under the tx so
    // the guard, the prune, and the AC-5 meta-blob all see one consistent snapshot.
    let row: Option<ResolveSnapshot> = tx
        .query_row(
            "SELECT claimed_by, state, refs, tripwire_verdict, verify_fix_cycles, fired_row \
             FROM wv_queue WHERE id=?1",
            params![id],
            |r| {
                Ok(ResolveSnapshot {
                    claimed_by: r.get(0)?,
                    state: r.get(1)?,
                    refs: r.get(2)?,
                    tripwire_verdict: r.get(3)?,
                    verify_fix_cycles: r.get(4)?,
                    fired_row: r.get(5)?,
                })
            },
        )
        .optional()?;
    let Some(ResolveSnapshot {
        claimed_by,
        state,
        refs,
        tripwire_verdict,
        verify_fix_cycles,
        fired_row,
    }) = row
    else {
        // No such row (already resolved/pruned, or never existed) — nothing to
        // prune; a state refusal rather than a storage error.
        return Ok(Err(QueueError::InvalidState));
    };
    // State is checked BEFORE ownership: a still-`pending` row (no owner) is an
    // InvalidState refusal ("resolving without claiming"), not an OwnerMismatch —
    // ownership only becomes meaningful once a row is claimed (AC-3, Task 4).
    if !matches!(state.as_str(), "claimed" | "dispatched") {
        return Ok(Err(QueueError::InvalidState));
    }
    // G4 own-item-only write: ask the named classifier the same question the SQL
    // one-writer rule below enforces, so the guarantee has a machine-checkable guard
    // that must AGREE with the `claimed_by=?` DELETE. The SQL remains the enforcing
    // mechanism; this is a fail-closed parity assertion in front of it (a blank owner
    // or a cross-owner caller is refused here exactly as the DELETE would refuse it).
    let owner = claimed_by.as_deref().unwrap_or("");
    if !crate::weft::admit_own_item_write(&caller.owner, owner).permitted() {
        return Ok(Err(QueueError::OwnerMismatch));
    }

    // Prune + key-ledger, both in this one tx (AC-4). Guard the DELETE on the same
    // owner so a concurrent owner-change can't race the prune.
    let deleted = tx.execute(
        "DELETE FROM wv_queue WHERE id=?1 AND claimed_by=?2",
        params![id, caller.owner],
    )?;
    if deleted != 1 {
        // The owner changed between the read and the delete — refuse rather than
        // commit a partial state. The tx drops (rollback) on return.
        return Ok(Err(QueueError::OwnerMismatch));
    }
    // AC-5: preserve the tripwire decision in the key-ledger `meta` blob so a
    // reviewer can reconstruct keep-vs-delegate after the queue row is pruned. Built
    // from the same-snapshot columns read above — no second read, no schema change on
    // the ledger beyond the `meta` column Task 2 healed on.
    let meta = serde_json::json!({
        "tripwireVerdict": tripwire_verdict,
        "verifyFixCycles": verify_fix_cycles,
        "firedRow": fired_row,
    })
    .to_string();
    tx.execute(
        "INSERT INTO wv_key_ledger (queue_id, ref, resolved_at, meta) VALUES (?1, ?2, ?3, ?4)",
        params![id, refs, now, meta],
    )?;
    // Audit the prune in the same tx (AC-7).
    super::sqlite::write_audit_as(&tx, QUEUE, "prune", id, Some(&caller.session))?;
    tx.commit()?;
    Ok(Ok(()))
}

/// SW-WV-C5 — a single unmet close condition (D5), reported so the operator / the
/// F-sub-epic attention screen sees exactly what is blocking closure — never a vague
/// "not done yet". Serialized to the wire as a stable string via [`Self::as_str`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseCondition {
    /// No `signal_done{status:"success"}` row exists in `wv_signals` for the worker
    /// (the (a) leg of the D5 predicate — SW-WV-B4).
    NotAccepted,
    /// The C3 done record is not `controller_verified` (the (b) leg — SW-WV-C3's
    /// verify-don't-fold has not stamped this dispatch).
    NotControllerVerified,
}

impl CloseCondition {
    /// The stable wire string surfaced in the `missing` array.
    pub fn as_str(self) -> &'static str {
        match self {
            CloseCondition::NotAccepted => "accepted",
            CloseCondition::NotControllerVerified => "controller_verified",
        }
    }
}

/// SW-WV-C5 — the outcome of [`close_task`]. A `Blocked` result is a legitimate
/// "not yet ready", never a thrown error — the controller re-checks after the worker
/// makes progress. `Resolved` means the queue row was pruned and its `wv_key_ledger`
/// re-open-protection sibling was written atomically (delegated to [`resolve`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseOutcome {
    /// Both conditions held → the row was pruned + key-ledger written (via B2's
    /// atomic [`resolve`]).
    Resolved,
    /// At least one condition was unmet → the row was left in place; `missing` names
    /// each failing condition (D5).
    Blocked { missing: Vec<CloseCondition> },
    /// The prune itself was refused by [`resolve`] (e.g. the caller does not own the
    /// row, or it is not in a resolvable state) even though the close predicate held —
    /// carried through so the controller sees the same typed queue refusal it would
    /// from `wv_resolve_queue`.
    ResolveRefused(QueueError),
}

/// SW-WV-C5 (AC-4, AC-5) — CLOSURE: the two-condition check (D5) that gates the
/// atomic prune. This is the controller-side complement to start-and-release: it
/// evaluates BOTH close conditions and, only when both hold, delegates the fold to
/// B2's [`resolve`] (the atomic `DELETE wv_queue` + `INSERT wv_key_ledger` in one
/// IMMEDIATE transaction — C5 adds no new deletion logic).
///
/// The two conditions REUSE C3's verify seam (this story does NOT re-implement it):
/// - **accepted** — [`WeftStore::has_success_signal`] for the dispatch's worker
///   session (read off the `wv_done_record`, not the caller — the controller never
///   calls `signal_done`);
/// - **controller_verified** — [`WeftStore::is_done_verified`] for `dispatch_id`.
///
/// Both are the exact legs [`WeftStore::is_close_predicate_met`] combines; here they
/// are checked GRANULARLY so a `Blocked` result can name which leg is missing (D5's
/// `{status:"closureBlocked", missing:[...]}`). When both hold, [`resolve`] prunes;
/// on any miss no row is touched. Returns a store `Err` only for a genuine storage
/// failure — a "not yet ready" is `Ok(CloseOutcome::Blocked{..})`.
pub fn close_task(
    store: &WeftStore,
    queue_id: i64,
    caller: impl Into<QueueCaller>,
    dispatch_id: &str,
    now: i64,
) -> Result<CloseOutcome, WeftStoreError> {
    // Resolve the worker session from the done record so the `accepted` leg is keyed
    // by the WORKER's signal, not the controller caller. A missing done record means
    // BOTH legs are unmet (there is no verified done-state at all).
    let worker_session = store.done_record_session(dispatch_id)?;
    let mut missing = Vec::new();
    match &worker_session {
        Some(session) if store.has_success_signal(session)? => {}
        _ => missing.push(CloseCondition::NotAccepted),
    }
    if !store.is_done_verified(dispatch_id)? {
        missing.push(CloseCondition::NotControllerVerified);
    }
    if !missing.is_empty() {
        return Ok(CloseOutcome::Blocked { missing });
    }

    // Both conditions hold → delegate the atomic prune + key-ledger to B2 (AC-5). C5
    // adds NO deletion logic; it only gates the call with the D5 predicate above.
    match resolve(store, queue_id, caller, now)? {
        Ok(()) => Ok(CloseOutcome::Resolved),
        Err(refusal) => Ok(CloseOutcome::ResolveRefused(refusal)),
    }
}

/// List all non-resolved rows (`pending`/`claimed`/`dispatched`), optionally
/// filtered by `scope` (AC-9). Read-only — resolved rows are pruned so never
/// appear. Ordered by id for a stable controller view.
pub fn queue_list(store: &WeftStore, scope: Option<&str>) -> WeftStoreResult<Vec<QueueRow>> {
    let conn = store.conn()?;
    let map = |r: &rusqlite::Row| -> rusqlite::Result<QueueRow> {
        Ok(QueueRow {
            id: r.get(0)?,
            source: r.get(1)?,
            state: r.get(2)?,
            claimed_by: r.get(3)?,
            scope: r.get(4)?,
            created: r.get(5)?,
            updated: r.get(6)?,
            refs: r.get(7)?,
            note: r.get(8)?,
        })
    };
    let cols = "id, source, state, claimed_by, scope, created, updated, refs, note";
    let rows: Vec<QueueRow> = match scope {
        Some(s) => {
            let mut stmt = conn.prepare(&format!(
                "SELECT {cols} FROM wv_queue \
                 WHERE state IN ('pending','claimed','dispatched') AND scope=?1 ORDER BY id"
            ))?;
            let it = stmt.query_map(params![s], map)?;
            it.collect::<Result<Vec<_>, _>>()?
        }
        None => {
            let mut stmt = conn.prepare(&format!(
                "SELECT {cols} FROM wv_queue \
                 WHERE state IN ('pending','claimed','dispatched') ORDER BY id"
            ))?;
            let it = stmt.query_map([], map)?;
            it.collect::<Result<Vec<_>, _>>()?
        }
    };
    Ok(rows)
}

/// One-time, idempotent cleanup for `wv_queue` rows that
/// `accept_role_message{kind:"work"}` used to mirror from role-inbox deliveries.
/// The write is now gone (role work lives only in the acceptance receipt + ack),
/// but older stores can still carry `pending` rows with `source='role_inbox'`
/// that nothing will ever resolve. This heal step prunes exactly those rows
/// and lays down the matching `wv_key_ledger` sibling — the same terminal
/// transition [`resolve`] uses for a resolved row — so an audit trail
/// survives the delete.
///
/// Scope: only `source='role_inbox'` AND `state='pending'`. A row in `claimed`
/// or `dispatched` already has a controller owner and resolves through the
/// normal [`resolve`] path, so it is left untouched (spec §Out of scope).
///
/// Atomicity: one IMMEDIATE transaction. For each matched row we DELETE the
/// `wv_queue` row, INSERT a `wv_key_ledger` sibling under the row's
/// `dedupe_key` (the `role_delivery:<delivery_id>` tag the old enqueue wrote),
/// and write a `wv_write_log` `prune` audit — the same three-step shape as
/// [`resolve`]. A crash mid-tx leaves none of it, so a retry on next open
/// sees the same rows and does the same work.
///
/// Idempotence: a second call finds no matching rows and performs no writes;
/// `WeftStore::open` can call this on every open without cost after the first.
///
/// Returns the number of rows pruned.
pub(crate) fn prune_orphan_role_inbox_rows(
    conn: &mut rusqlite::Connection,
    now: i64,
) -> WeftStoreResult<usize> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Read id + dedupe_key under the tx so a concurrent writer can't slip a
    // row in between the SELECT and the per-row DELETE. The write path for
    // this source is gone, so this is defence-in-depth, not a hot race.
    let rows: Vec<(i64, Option<String>)> = {
        let mut stmt = tx.prepare(
            "SELECT id, dedupe_key FROM wv_queue \
             WHERE source='role_inbox' AND state='pending' ORDER BY id",
        )?;
        let it = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        it.collect::<Result<Vec<_>, _>>()?
    };
    let mut pruned: usize = 0;
    for (id, dedupe_key) in rows {
        // Mirror `resolve` (`:583-610`): DELETE + ledger INSERT + audit `prune`,
        // all in this one tx. The ledger `ref` carries the dedupe key tag so a
        // reviewer can map the pruned row back to the role delivery it mirrored.
        let deleted = tx.execute("DELETE FROM wv_queue WHERE id=?1", params![id])?;
        if deleted != 1 {
            // The row vanished between SELECT and DELETE (should not happen
            // under IMMEDIATE, but we refuse to write a bogus ledger entry and
            // surface the anomaly so a real race is not invisible).
            log::warn!(
                target: "weft::queue",
                "prune_orphan_role_inbox_rows: wv_queue row {id} vanished between SELECT and DELETE under IMMEDIATE; skipping ledger write"
            );
            continue;
        }
        let meta = r#"{"prunedBy":"role-inbox-orphan-cleanup"}"#;
        tx.execute(
            "INSERT INTO wv_key_ledger (queue_id, ref, resolved_at, meta) \
             VALUES (?1, ?2, ?3, ?4)",
            params![id, dedupe_key, now, meta],
        )?;
        super::sqlite::write_audit_public(&tx, QUEUE, "prune", id)?;
        pruned += 1;
    }
    tx.commit()?;
    Ok(pruned)
}

/// Read the highest minted `wv_queue` id (test helper for the live-max contract).
#[cfg(test)]
pub fn max_id(store: &WeftStore) -> WeftStoreResult<i64> {
    current_max_id(store)
}

/// The highest `wv_queue` id currently present (`MAX(id)`, 0 when empty). Tests snapshot it
/// before [`enqueue`] to tell a fresh insert (live-max+1) from a dedup short-circuit (an
/// existing id at or below the snapshot).
#[cfg(test)]
pub fn current_max_id(store: &WeftStore) -> WeftStoreResult<i64> {
    let conn = store.conn()?;
    let n: i64 = conn.query_row("SELECT COALESCE(MAX(id), 0) FROM wv_queue", [], |r| {
        r.get(0)
    })?;
    Ok(n)
}

#[cfg(test)]
mod tests;
