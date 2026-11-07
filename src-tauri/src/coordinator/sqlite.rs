// SPDX-License-Identifier: MIT
//! `SqliteOrchestrationStore` — the first concrete [`OrchestrationStore`] impl.
//!
//! One embedded SQLite database (WAL mode, busy-timeout) is the sole coordinator.
//! Every correctness-critical mutation that spans more than one row commits inside
//! a single `IMMEDIATE` transaction, so a crash between writes leaves no
//! half-state (AC-3/AC-6). Each operation opens its **own** connection to the
//! shared DB file — a concurrent claim race therefore resolves inside SQLite's
//! compare-and-set, never a Rust mutex (AC-2).
//!
//! The store persists **only** coordination state (claims, run events, deliveries,
//! run-request pointers, routine runtime state). It mirrors no metadata value; metadata stays
//! authoritative in YAML/frontmatter (AD-11, AC-9).

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::shared::secret_scan::redact_secrets;

use super::fault::{self, FaultPoint};
use super::store::{OrchestrationStore, StoreError, StoreResult};
#[cfg(test)]
use super::types::RunState;
use super::types::{DeliveryRef, RecoveryReport, RunEvent, RunnableClaim, WorkerId};

/// The coordination schema. Column names are deliberately coordination-only — no
/// `status`, `rule`, `definition`, `config`, or subject-metadata column exists, so
/// the AC-9 column scan holds by construction. `state` here is *run coordination*
/// state (runnable/claimed/done), not a user-facing status value.
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS runs (
    run_id       TEXT PRIMARY KEY,
    state        TEXT NOT NULL,
    claimed_by   TEXT,
    lease_expiry INTEGER
);

CREATE TABLE IF NOT EXISTS run_events (
    seq     INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id  TEXT NOT NULL,
    kind    TEXT NOT NULL,
    payload TEXT NOT NULL
);

-- A delivery is an external coordination identity (adapter_id/delivery_key) until it
-- is consumed by dispatch; `run_id` then links it to the runnable run it fired. The
-- FK is DEFERRABLE INITIALLY DEFERRED so the consume + run-row write commit together in
-- one transaction without a spurious mid-statement violation; it is checked at commit,
-- rejecting a delivery linked to a non-existent run. `run_id` stays NULL for a pending
-- (not-yet-consumed) delivery.
CREATE TABLE IF NOT EXISTS deliveries (
    adapter_id   TEXT NOT NULL,
    delivery_key TEXT NOT NULL,
    state        TEXT NOT NULL DEFAULT 'pending',
    run_id       TEXT REFERENCES runs(run_id) DEFERRABLE INITIALLY DEFERRED,
    PRIMARY KEY (adapter_id, delivery_key)
);

-- The durable run-request pointer: enough dispatch intent (a serialized, scrubbed
-- RunRequest) to rebuild an executable request after restart, keyed by run_id. A
-- coordination/dispatch fact — distinct from the run-identity event stream, joined
-- only by run_id (ARCH-INV-24). The FK ties a pointer to its runnable run and commits
-- in the same transaction as the run row (SW-AU-80D).
CREATE TABLE IF NOT EXISTS run_requests (
    run_id  TEXT PRIMARY KEY REFERENCES runs(run_id) DEFERRABLE INITIALLY DEFERRED,
    request TEXT NOT NULL
);
"#;

/// Routine runtime state, owned by `crate::routines::state`. Kept beside the
/// coordination tables so one machine-local file holds all runtime state, while
/// the routine definitions themselves stay in git.
///
/// - `routine_state`: one row per routine key (`<org>/<team>/<id>`). `enabled`
///   is this Mac's switch for the routine (a pause, or "Run on my Mac") and
///   `enabled_mode` the `runs_on` it was flipped under (`NULL`: a shared job,
///   or no switch at all). A missing row means the mode's default with no
///   history. The switch never touches the definition.
/// - `routine_triggers`: the scheduler's per-trigger cursor. `next_run_at` is
///   the compare-and-set claim column; `spec_hash` detects an edited trigger.
///   `owner` is the account whose app seeded the cursor: the file is shared by
///   every account on the machine, and only that account may move or drop it.
/// - `routine_runs`: every fire, including skipped and coalesced ones with their
///   `reason`. `occurrence_key` is unique so a re-delivered occurrence cannot
///   admit twice. `phase` and `result_text` carry what a user-facing model would
///   call status and summary (the schema column-name guard forbids those words).
///   `request` holds a held run's dispatch request until the run is released to
///   the claim loop. `max_runtime_ms` is the admitted revision's runtime bound.
///   `causation` is an event-fired run's chain of routine runs, for the loop
///   guard of the events its own agent raises.
/// - `routine_arm_handover` (SW-AGR-8): a routine moved off a machine whose
///   disarm that machine has not acknowledged yet. Nothing fires it elsewhere
///   until it does, so a move never doubles a fire.
/// - `routine_arm_cursors` (SW-AGR-8): how far this Mac has read each
///   machine's run log of machine-armed routines, and when that log last
///   dropped runs before this Mac read them.
/// - `routine_arm_floor` (SW-AGR-8): the last occurrence a machine fired a
///   routine at, as its handover acknowledgement reported it: this Mac's
///   schedule for it starts after it.
/// - `routine_arm_stale` (SW-XS-25): a machine arm whose prepared content is
///   behind the routine and its agent. Only an explicit refresh prepares it;
///   the machine fires with what was last prepared meanwhile.
/// - `routine_arm_prepared` (SW-XS-25): the launch recipe this Mac last
///   prepared for a routine on a machine, and the agent manifest it was
///   prepared from. A definition-only push (an edit synced without a
///   preparation) carries it, so the machine still starts the agent.
///
/// Times are Unix epoch milliseconds.
const ROUTINE_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS routine_result_runs (
    run_id TEXT PRIMARY KEY REFERENCES routine_runs(run_id),
    principal TEXT NOT NULL,
    schema_json TEXT NOT NULL,
    candidate TEXT
);
CREATE INDEX IF NOT EXISTS idx_routine_result_principal_run ON routine_result_runs(principal, run_id);
CREATE TABLE IF NOT EXISTS routine_results (
    principal TEXT NOT NULL, routine_key TEXT NOT NULL,
    run_id TEXT NOT NULL, revision INTEGER NOT NULL,
    schema_json TEXT NOT NULL, payload TEXT NOT NULL, succeeded_at INTEGER NOT NULL,
    PRIMARY KEY(principal, routine_key)
);
CREATE TABLE IF NOT EXISTS routine_state (
    routine_key      TEXT PRIMARY KEY,
    enabled          INTEGER NOT NULL DEFAULT 1,
    enabled_mode     TEXT,
    last_run_id      TEXT,
    last_run_at      INTEGER,
    last_result_text TEXT,
    missed_count     INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS routine_triggers (
    trigger_id  TEXT PRIMARY KEY,
    routine_key TEXT NOT NULL,
    next_run_at INTEGER,
    spec_hash   TEXT NOT NULL,
    owner       TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_routine_triggers_key ON routine_triggers(routine_key);

CREATE TABLE IF NOT EXISTS routine_runs (
    run_id          TEXT PRIMARY KEY,
    routine_key     TEXT NOT NULL,
    revision        INTEGER NOT NULL,
    source          TEXT NOT NULL,
    occurrence_key  TEXT UNIQUE,
    phase           TEXT NOT NULL,
    reason          TEXT,
    linked_run_id   TEXT,
    session_id      TEXT,
    created_at      INTEGER NOT NULL,
    started_at      INTEGER,
    ended_at        INTEGER,
    result_text     TEXT,
    request         TEXT,
    max_runtime_ms  INTEGER,
    causation       TEXT,
    agent_ref       TEXT,
    delivered_at    INTEGER,
    wake_machine    TEXT,
    wake_notified   INTEGER,
    wake_retries    INTEGER,
    launch_machine  TEXT,
    launch_request  TEXT,
    action          TEXT NOT NULL DEFAULT 'agent',
    command_details TEXT,
    detail          TEXT
);
CREATE INDEX IF NOT EXISTS idx_routine_runs_key ON routine_runs(routine_key, created_at);
CREATE INDEX IF NOT EXISTS idx_routine_runs_session ON routine_runs(session_id);

CREATE TABLE IF NOT EXISTS routine_arm_handover (
    routine_key TEXT NOT NULL,
    machine     TEXT NOT NULL,
    epoch       INTEGER NOT NULL,
    since_ms    INTEGER NOT NULL,
    PRIMARY KEY(routine_key, machine)
);
CREATE TABLE IF NOT EXISTS routine_arm_cursors (
    machine    TEXT PRIMARY KEY,
    last_seq   INTEGER NOT NULL,
    boot_id    TEXT NOT NULL DEFAULT '',
    lost_at_ms INTEGER
);
CREATE TABLE IF NOT EXISTS routine_arm_floor (
    routine_key   TEXT PRIMARY KEY,
    last_fired_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS routine_arm_stale (
    routine_key TEXT NOT NULL,
    machine     TEXT NOT NULL,
    since_ms    INTEGER NOT NULL,
    why         TEXT NOT NULL,
    PRIMARY KEY(routine_key, machine)
);
CREATE TABLE IF NOT EXISTS routine_arm_prepared (
    routine_key   TEXT NOT NULL,
    machine       TEXT NOT NULL,
    workspace_id  TEXT NOT NULL,
    agent_key     TEXT NOT NULL DEFAULT '',
    manifest_hash TEXT NOT NULL,
    launch_json   TEXT NOT NULL,
    prepared_ms   INTEGER NOT NULL,
    PRIMARY KEY(routine_key, machine)
);
"#;

/// The coordinator. Cheap to `clone` (just the path); a fresh connection is
/// opened per operation, so a cloned handle shared across worker threads is
/// exactly the intended concurrency model.
#[derive(Debug, Clone)]
pub struct SqliteOrchestrationStore {
    path: PathBuf,
}

impl SqliteOrchestrationStore {
    /// Open (creating + migrating on first use) a store at `path`. The path holds
    /// runtime coordination state and belongs under `codesense_home` — never the
    /// context repo (ARCH-INV-11). Tests pass a temp path.
    pub fn open(path: impl Into<PathBuf>) -> StoreResult<Self> {
        let path = path.into();
        // Parent-dir create + WAL set/assert are the shared open plumbing. The
        // helper hands back a live connection and owns no schema, so this store
        // keeps full ownership of its `execute_batch(SCHEMA)` + `ensure_column`
        // reconciliation below — an upgraded coordinator DB still heals here.
        let conn = crate::shared::sqlite::open_wal_db(&path)?;
        conn.execute_batch(SCHEMA)?;
        // Routine tables are created by the idempotent batch; a column added to
        // one since is reconciled after it.
        conn.execute_batch(ROUTINE_SCHEMA)?;
        crate::shared::sqlite::ensure_column(&conn, "routine_runs", "request", "TEXT")?;
        // SW-XS-25: the agent a kept launch recipe was prepared for. A row
        // from before it has none, and is never used for any agent.
        crate::shared::sqlite::ensure_column(
            &conn,
            "routine_arm_prepared",
            "agent_key",
            "TEXT NOT NULL DEFAULT ''",
        )?;
        crate::shared::sqlite::ensure_column(
            &conn,
            "routine_runs",
            "action",
            "TEXT NOT NULL DEFAULT 'agent'",
        )?;
        crate::shared::sqlite::ensure_column(&conn, "routine_runs", "command_details", "TEXT")?;
        crate::shared::sqlite::ensure_column(&conn, "routine_runs", "max_runtime_ms", "INTEGER")?;
        crate::shared::sqlite::ensure_column(&conn, "routine_runs", "causation", "TEXT")?;
        crate::shared::sqlite::ensure_column(&conn, "routine_runs", "agent_ref", "TEXT")?;
        // SW-AGR-9: when a wake run's task was typed into its session.
        crate::shared::sqlite::ensure_column(&conn, "routine_runs", "delivered_at", "INTEGER")?;
        // SW-AGR-2: the machine of the session a wake run woke, and whether
        // its `unknown` was posted to the human inbox (once per run).
        crate::shared::sqlite::ensure_column(&conn, "routine_runs", "wake_machine", "TEXT")?;
        crate::shared::sqlite::ensure_column(&conn, "routine_runs", "wake_notified", "INTEGER")?;
        crate::shared::sqlite::ensure_column(&conn, "routine_runs", "wake_retries", "INTEGER")?;
        // SW-AGR-2b: the machine a run started its agent on (not this Mac),
        // and the remote launch request it was made under.
        crate::shared::sqlite::ensure_column(&conn, "routine_runs", "launch_machine", "TEXT")?;
        crate::shared::sqlite::ensure_column(&conn, "routine_runs", "launch_request", "TEXT")?;
        // CodeSense's explanation of why a run ended, apart from what its agent produced.
        crate::shared::sqlite::ensure_column(&conn, "routine_runs", "detail", "TEXT")?;
        crate::shared::sqlite::ensure_column(
            &conn,
            "routine_triggers",
            "owner",
            "TEXT NOT NULL DEFAULT ''",
        )?;
        crate::shared::sqlite::ensure_column(&conn, "routine_state", "enabled_mode", "TEXT")?;
        // SW-AGR-13: the routine's wake budget generation, bumped on every
        // resume so a routine paused at its `max_wakes` starts a new count.
        crate::shared::sqlite::ensure_column(
            &conn,
            "routine_state",
            "wake_budget",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        crate::shared::sqlite::ensure_column(&conn, "routine_state", "wake_budget_at", "INTEGER")?;
        // Reconcile a coordinator file created by an earlier build: SW-AU-80A first
        // shipped this DB (opened at the default path by the run-evidence reader), and
        // `CREATE TABLE IF NOT EXISTS` above will NOT add a column to a pre-existing
        // `runs` table. Add `lease_expiry` idempotently so a claim/reclaim on an
        // upgraded DB does not hit "no such column". New tables (`deliveries`) are
        // created by the batch above.
        crate::shared::sqlite::ensure_column(&conn, "runs", "lease_expiry", "INTEGER")?;
        // SW-AU-80D links a consumed delivery to the run it fired. A `deliveries` table
        // created by SW-AU-80B has no `run_id`; add it idempotently. SQLite permits a
        // REFERENCES clause on an added column only when its default is NULL (it is), so
        // the fired-run FK applies to upgraded DBs too. `run_requests` is a whole new
        // table, created by the `CREATE TABLE IF NOT EXISTS` batch above.
        crate::shared::sqlite::ensure_column(
            &conn,
            "deliveries",
            "run_id",
            "TEXT REFERENCES runs(run_id) DEFERRABLE INITIALLY DEFERRED",
        )?;
        Ok(Self { path })
    }

    /// Open a fresh connection to the shared DB file via the shared per-connection
    /// plumbing (busy-timeout + `synchronous=NORMAL` + `foreign_keys=ON`). Safe on
    /// the blocking pool: the busy-timeout wait never blocks the async runtime. WAL
    /// journal mode is a durable file property set once at [`Self::open`], not
    /// re-issued here.
    fn conn(&self) -> StoreResult<Connection> {
        Ok(crate::shared::sqlite::open_conn(&self.path)?)
    }

    /// Read back the durable run-request pointer for `run_id`, or `None` when no pointer was
    /// enqueued. The rehydration source a claim-driven dispatcher (SW-AU-80E) rebuilds an
    /// executable request from after restart.
    pub fn run_request_json(&self, run_id: &str) -> StoreResult<Option<String>> {
        let conn = self.conn()?;
        let request: Option<String> = conn
            .query_row(
                "SELECT request FROM run_requests WHERE run_id=?1",
                params![run_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(request)
    }

    /// SW-AU-80E: retire a run the claim-driven dispatcher finished — the terminal CAS the
    /// loop calls after `run_dispatch_tick` returns. Transitions `claimed → done` guarded on both
    /// the current state **and** `claimed_by = worker`, so it retires **only** a run this exact
    /// worker still holds. Returns `Ok(false)` when the run is not `claimed` by `worker` (already
    /// `done`, reclaimed to `runnable` after a lease expiry, or re-claimed by a **different** worker)
    /// — so a stale worker whose run was reclaimed and re-claimed elsewhere can never clobber the new
    /// owner's live claim, and there is never a silent resurrection. Mirrors the no-clobber
    /// `UPDATE … WHERE …` idiom of [`enqueue_runnable_stmt`]; no new transaction primitive (a single
    /// guarded statement is atomic on its own connection).
    pub fn finish_run(&self, run_id: &str, worker: &WorkerId) -> StoreResult<bool> {
        let conn = self.conn()?;
        let changed = conn.execute(
            "UPDATE runs SET state='done', claimed_by=NULL, lease_expiry=NULL
             WHERE run_id=?1 AND state='claimed' AND claimed_by=?2",
            params![run_id, worker.as_str()],
        )?;
        Ok(changed == 1)
    }

    /// Numbered committed events let live readers merge a cold read with concurrent appends.
    pub(crate) fn run_events_numbered(&self, run_id: &str) -> StoreResult<Vec<(i64, RunEvent)>> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT seq, kind, payload FROM run_events WHERE run_id=?1 ORDER BY seq")?;
        let rows = stmt.query_map(params![run_id], |r| {
            Ok((
                r.get(0)?,
                RunEvent {
                    run_id: run_id.into(),
                    kind: r.get(1)?,
                    payload: r.get(2)?,
                },
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Replay one run's durable event stream in append (`seq`) order — the read-back
    /// the in-memory Run projection rebuilds from (SW-AU-82B, ARCH-INV-9). Payloads are
    /// already scrubbed at persist, so what a projection replays can never carry an
    /// unredacted secret. This is an inherent read helper, not an `OrchestrationStore`
    /// trait method: the projection depends on the event slice, not the port.
    pub fn run_events_for(&self, run_id: &str) -> StoreResult<Vec<RunEvent>> {
        self.run_events_numbered(run_id)
            .map(|events| events.into_iter().map(|(_, event)| event).collect())
    }
}

/// Record a matched, accepted fire as durable runnable work, inside the caller's transaction and needs the enqueue to commit or roll back with its own
/// writes. Same contract: the runnable row (no-clobber CAS), the scrubbed pointer for a run this
/// call freshly owns, and the consume + link of `delivery`. Opens no transaction of its own.
pub fn enqueue_on_accept_in(
    conn: &Connection,
    run_id: &str,
    request: &str,
    delivery: Option<&DeliveryRef>,
) -> StoreResult<bool> {
    // (a) Runnable row — the no-clobber CAS. `false` means the run is occupied
    // (`claimed`/`done`): it already exists (so step (c)'s FK link still holds),
    // but we write no new pointer and report the duplicate.
    let freshly_runnable = enqueue_runnable_stmt(conn, run_id)?;
    // Injected crash point: the runnable row is written but the pointer/delivery are not.
    // The caller's transaction, dropped on this error, rolls the runnable row back too (AC-1).
    if fault::should_abort(FaultPoint::EnqueuePostRunnablePrePointer) {
        return Err(StoreError::InjectedFault(
            FaultPoint::EnqueuePostRunnablePrePointer,
        ));
    }
    // (b) Durable run-request pointer, scrubbed at persist by the one redaction owner — only
    // for a run we freshly own. An occupied run keeps the pointer written when it was first
    // enqueued; a replay of an already-runnable run re-writes it as a no-op (`DO NOTHING`).
    if freshly_runnable {
        conn.execute(
            "INSERT INTO run_requests (run_id, request) VALUES (?1, ?2)
             ON CONFLICT(run_id) DO NOTHING",
            params![run_id, redact_secrets(request)],
        )?;
    }
    // (c) Consume + link the delivery that fired this run. The run row exists either way
    // (fresh or occupied), so the deferred FK is satisfied at commit. A dedup-neutral fire
    // (empty key) never recorded a delivery, so the caller passes `None`. The `state='pending'`
    // guard makes the consume a one-way election: an already-`consumed` delivery is never
    // re-linked to a second run, and an idempotent replay finds nothing to re-consume.
    if let Some(d) = delivery {
        conn.execute(
            "UPDATE deliveries SET state='consumed', run_id=?3
             WHERE adapter_id=?1 AND delivery_key=?2 AND state='pending'",
            params![d.adapter_id, d.delivery_key, run_id],
        )?;
    }
    Ok(freshly_runnable)
}

/// The runnable-upsert CAS — one owner of the no-clobber guard SQL, shared by
/// [`SqliteOrchestrationStore::enqueue_runnable`] (its own connection) and the
/// [`SqliteOrchestrationStore::enqueue_on_accept`] transaction (`&Transaction` derefs to
/// `&Connection`). A fresh run inserts; an already-`runnable` run re-arms idempotently; a
/// `claimed`/`done` run matches no row and returns `Ok(false)` — never a silent
/// reset (SW-AU-80B AC-2).
fn enqueue_runnable_stmt(conn: &Connection, run_id: &str) -> StoreResult<bool> {
    let changed = conn.execute(
        "INSERT INTO runs (run_id, state, lease_expiry) VALUES (?1, 'runnable', NULL)
         ON CONFLICT(run_id) DO UPDATE SET state='runnable', claimed_by=NULL, lease_expiry=NULL
             WHERE runs.state='runnable'",
        params![run_id],
    )?;
    Ok(changed > 0)
}

/// Append an event, scrubbing its payload at persist.
fn insert_run_event(
    conn: &Connection,
    run_id: &str,
    kind: &str,
    payload: &str,
) -> StoreResult<i64> {
    conn.execute(
        "INSERT INTO run_events (run_id, kind, payload) VALUES (?1, ?2, ?3)",
        params![run_id, kind, redact_secrets(payload)],
    )?;
    Ok(conn.last_insert_rowid())
}

impl OrchestrationStore for SqliteOrchestrationStore {
    fn append(&self, ev: RunEvent) -> StoreResult<i64> {
        let conn = self.conn()?;
        let seq = insert_run_event(&conn, &ev.run_id, &ev.kind, &ev.payload)?;
        let mut event = ev;
        event.payload = redact_secrets(&event.payload);
        crate::live_hub::run_review::written(
            &self.path,
            crate::live_hub::run_review::Written::Durable { seq, event },
        );
        Ok(seq)
    }

    fn claim_ready(
        &self,
        worker: &WorkerId,
        lease_expiry: i64,
    ) -> StoreResult<Option<RunnableClaim>> {
        let conn = self.conn()?;
        // Pick a runnable candidate, then CAS it by id. If a concurrent worker won
        // that candidate first (`changes()==0`), try the next — the loop drains the
        // queue and the `WHERE state='runnable'` guard guarantees a single winner.
        // The winner stamps `lease_expiry` on the claim so a later `recover` can tell
        // a live claim from an abandoned one.
        loop {
            let candidate: Option<String> = conn
                .query_row(
                    "SELECT run_id FROM runs WHERE state='runnable' LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(run_id) = candidate else {
                return Ok(None);
            };
            let changed = conn.execute(
                "UPDATE runs SET state='claimed', claimed_by=?2, lease_expiry=?3
                 WHERE run_id=?1 AND state='runnable'",
                params![run_id, worker.as_str(), lease_expiry],
            )?;
            if changed == 1 {
                return Ok(Some(RunnableClaim {
                    run_id,
                    worker: worker.clone(),
                    lease_expiry,
                }));
            }
        }
    }

    fn recover(&self, now_cutoff: i64) -> StoreResult<RecoveryReport> {
        let mut conn = self.conn()?;
        // One IMMEDIATE transaction so the reclaim and the scan are a consistent
        // snapshot: no concurrent claim can slip between reclaiming an expired lease
        // and reporting the runnable set.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Reclaim abandoned claims: a `claimed` run whose caller-stamped lease is past
        // the caller-supplied cutoff returns to `runnable`. Integer compare only — the
        // store reads no clock (AC-3).
        let reclaimed_claims = tx.execute(
            "UPDATE runs SET state='runnable', claimed_by=NULL, lease_expiry=NULL
             WHERE state='claimed' AND lease_expiry IS NOT NULL AND lease_expiry < ?1",
            params![now_cutoff],
        )? as u64;
        // Injected crash point: the reclaim ran but nothing is committed. Dropping
        // `tx` rolls it back; a retry reclaims the same rows idempotently (AC-4).
        if fault::should_abort(FaultPoint::RecoverPostReclaimPreScan) {
            return Err(StoreError::InjectedFault(
                FaultPoint::RecoverPostReclaimPreScan,
            ));
        }
        let runnable_runs = collect_run_ids(&tx, "runnable")?;
        let pending_deliveries = {
            let mut stmt = tx
                .prepare("SELECT adapter_id, delivery_key FROM deliveries WHERE state='pending'")?;
            let rows = stmt.query_map([], |r| {
                Ok(DeliveryRef {
                    adapter_id: r.get(0)?,
                    delivery_key: r.get(1)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.commit()?;
        Ok(RecoveryReport {
            reclaimed_claims,
            runnable_runs,
            pending_deliveries,
        })
    }
}

/// Collect the `run_id`s currently in `state` — the durable set `recover` re-offers.
fn collect_run_ids(conn: &Connection, state: &str) -> StoreResult<Vec<String>> {
    let mut stmt = conn.prepare("SELECT run_id FROM runs WHERE state=?1")?;
    let rows = stmt.query_map(params![state], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Test-only enqueue, counting and schema reads, plus seams that force otherwise-unreachable states so the CAS guards, the FK,
/// and `recover`'s scans can be proven without a full runtime (there is no terminal
/// `done` transition or delivery-consume path in this story — those are SW-AU-80D).
#[cfg(test)]
impl SqliteOrchestrationStore {
    /// Enqueue a run as `runnable`. A fresh run is inserted; an already-`runnable`
    /// run is an idempotent no-op re-arm. A `claimed`/`done` run is **never**
    /// clobbered back to runnable — the guarded conflict update matches no row, so the
    /// return is `Ok(false)` ("occupied, not enqueued"), never a silent reset
    /// (SW-AU-80B AC-2). Returns `Ok(true)` when the run is now runnable (fresh or
    /// already-runnable).
    pub fn enqueue_runnable(&self, run_id: &str) -> StoreResult<bool> {
        enqueue_runnable_stmt(&self.conn()?, run_id)
    }

    /// SW-AU-80D AC-1: record a matched, accepted event as durable runnable work in one
    /// `IMMEDIATE` transaction — the producer half of decoupling firing from execution.
    /// Atomically: (a) enqueue the runnable `runs` row (the no-clobber CAS above); (b)
    /// persist the durable run-request pointer (`request`, the rehydration source) scrubbed
    /// through the one redaction owner; and (c) when a real `delivery` fired this run,
    /// transition its `deliveries` row to `consumed` and link it to `run_id`. A crash
    /// between (a) and (b)/(c) rolls back all three (AC-1); the `deliveries → runs` FK is
    /// satisfied because the run row is written first in the same transaction (AC-4).
    ///
    /// Returns `Ok(true)` when the run is now runnable (fresh, or an idempotent replay of an
    /// already-runnable row — the pointer insert is a no-op on replay). Returns `Ok(false)`
    /// when the run is **occupied** (`claimed`/`done`): no new runnable row, no
    /// pointer overwritten — the caller treats this as a duplicate, not an error. In **either**
    /// case the delivery (if any) is consumed and linked to the run, so a fresh delivery whose
    /// deterministic `run_id` maps onto an already-in-flight run is never stranded `pending` and
    /// re-offered by `recover` forever.
    pub fn enqueue_on_accept(
        &self,
        run_id: &str,
        request: &str,
        delivery: Option<&DeliveryRef>,
    ) -> StoreResult<bool> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let freshly_runnable = enqueue_on_accept_in(&tx, run_id, request, delivery)?;
        tx.commit()?;
        Ok(freshly_runnable)
    }

    /// Count runs in `state` — the durable scan a scheduler performs on restart
    /// (AC-7).
    pub fn count_runs_in_state(&self, state: RunState) -> StoreResult<i64> {
        let conn = self.conn()?;
        let n = conn.query_row(
            "SELECT COUNT(*) FROM runs WHERE state=?1",
            params![state.as_db()],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// Number of `run_events` rows whose `payload` contains `needle` — AC-11
    /// evidence that a secret never lands unscrubbed in a DB row. Uses `instr`
    /// (plain substring) so a `%`/`_` in the needle can't skew the count the way a
    /// `LIKE` pattern would.
    pub fn count_event_payloads_containing(&self, needle: &str) -> StoreResult<i64> {
        self.count_containing(
            "SELECT COUNT(*) FROM run_events WHERE instr(payload, ?1) > 0",
            needle,
        )
    }

    /// Shared substring-count query for the redaction probe above.
    fn count_containing(&self, sql: &str, needle: &str) -> StoreResult<i64> {
        let conn = self.conn()?;
        let n = conn.query_row(sql, params![needle], |r| r.get(0))?;
        Ok(n)
    }

    /// The concatenated `CREATE` statements for every table — the AC-9 column-scan
    /// test greps this and fails if any column would mirror a metadata fact.
    pub fn schema_sql(&self) -> StoreResult<String> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT sql FROM sqlite_master WHERE type='table' AND sql IS NOT NULL ORDER BY name",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = String::new();
        for sql in rows {
            out.push_str(&sql?);
            out.push('\n');
        }
        Ok(out)
    }
    /// Force `run_id` into `state` (e.g. `done`) so a guard-rejection path is reachable.
    pub fn set_state_for_test(&self, run_id: &str, state: &str) -> StoreResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE runs SET state=?2 WHERE run_id=?1",
            params![run_id, state],
        )?;
        Ok(())
    }

    /// Attempt to insert a delivery already linked to a `run_id` that has no `runs` row, so
    /// the deferred `deliveries → runs` FK's rejection of a dangling link can be asserted
    /// (SW-AU-80D AC-4). The insert is autocommit, so the deferred FK is checked at the
    /// statement's implicit commit — which is what rejects the dangling reference.
    pub fn try_link_delivery_to_missing_run_for_test(
        &self,
        adapter_id: &str,
        delivery_key: &str,
        run_id: &str,
    ) -> StoreResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO deliveries (adapter_id, delivery_key, state, run_id)
             VALUES (?1, ?2, 'consumed', ?3)",
            params![adapter_id, delivery_key, run_id],
        )?;
        Ok(())
    }

    /// Seed a delivery row directly in `state` so `recover`'s pending-only scan can be
    /// proven to exclude a `consumed` delivery.
    pub fn seed_delivery_for_test(
        &self,
        adapter_id: &str,
        delivery_key: &str,
        state: &str,
    ) -> StoreResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO deliveries (adapter_id, delivery_key, state) VALUES (?1, ?2, ?3)",
            params![adapter_id, delivery_key, state],
        )?;
        Ok(())
    }
}

/// The default durable location for the coordinator DB:
/// `{codesense_home}/orchestration/coordinator.sqlite3`. Runtime coordination
/// state only — never the context repo (ARCH-INV-11). Returns `None` when the
/// home dir cannot be resolved (headless/no-`$HOME`), same benign degrade as the
/// run-artifact store.
pub fn default_store_path(codesense_home: &Path) -> PathBuf {
    codesense_home
        .join("orchestration")
        .join("coordinator.sqlite3")
}
