// SPDX-License-Identifier: MIT
//! Machine-local routine runtime state in the coordinator SQLite.
//!
//! The tables are created by the coordinator's schema
//! (`coordinator::sqlite`); opening this store goes through
//! [`SqliteOrchestrationStore::open`] so an older coordinator file gains them
//! before the first query. Nothing here is ever written to git.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::{params, OptionalExtension};

use crate::coordinator::{default_store_path, SqliteOrchestrationStore, StoreError, StoreResult};
use crate::domain::Principal;

use super::host::starts_on_here;
use super::model::{RoutineDef, RoutineKey, RunsOn};
use super::runs::{self, RoutineRun};

/// This Mac's own on/off switch for one routine, and the mode it was flipped
/// under. A pause of a shared job and "Run on my Mac" for a recipe each Mac
/// runs are both this switch; the mode keeps one from standing in for the
/// other when a routine changes mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Switch {
    pub enabled: bool,
    pub mode: RunsOn,
}

impl Switch {
    /// The switch a `routine_state` row holds. A row with no mode is a shared
    /// job's: one written by a pause from before modes existed, or one that
    /// only counts missed runs. A mode this build cannot read is no switch.
    fn from_row(enabled: i64, mode: Option<String>) -> Option<Self> {
        let mode = match mode.as_deref() {
            None => RunsOn::OneMac,
            Some(raw) => RunsOn::parse(raw).ok()?,
        };
        Some(Self {
            enabled: enabled != 0,
            mode,
        })
    }
}

/// Whether `def` fires from this Mac (`host`), given this Mac's `switch` for
/// it: the switch when it was flipped under the routine's current mode, else
/// where the switch starts (`host::starts_on_here`). So a routine that changes
/// mode starts from its new mode's default here, never from a choice made for
/// the other one, with one exception: a shared job paused (or saved as a
/// draft) on its Mac stays off there when it becomes a recipe each Mac runs,
/// so a mode change never starts a routine someone stopped.
pub fn enabled_here(switch: Option<Switch>, def: &RoutineDef, host: &str) -> bool {
    let Ok(mode) = def.runs_on() else {
        return false;
    };
    match switch {
        Some(switch) if switch.mode == mode => switch.enabled,
        Some(Switch {
            enabled: false,
            mode: RunsOn::OneMac,
        }) if mode == RunsOn::EachMac => false,
        _ => starts_on_here(def, host),
    }
}

/// This Mac's switch for `key`, read on `conn` (inside a caller's
/// transaction).
pub fn switch_in(conn: &rusqlite::Connection, key: &str) -> rusqlite::Result<Option<Switch>> {
    Ok(conn
        .query_row(
            "SELECT enabled, enabled_mode FROM routine_state WHERE routine_key = ?1",
            params![key],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .optional()?
        .and_then(|(enabled, mode)| Switch::from_row(enabled, mode)))
}

/// One `routine_state` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineState {
    pub routine_key: String,
    /// The raw switch; read it through [`enabled_here`], which knows the mode
    /// it was flipped under.
    pub enabled: bool,
    pub last_run_id: Option<String>,
    /// Unix epoch milliseconds.
    pub last_run_at: Option<i64>,
    pub last_result_text: Option<String>,
    pub missed_count: u32,
}

/// One `routine_triggers` row: the cursor of one schedule trigger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerRow {
    pub trigger_id: String,
    pub routine_key: String,
    /// Unix epoch milliseconds of the next fire; `None` when the schedule never
    /// fires again.
    pub next_run_at: Option<i64>,
    /// Identifies the cron + zone the cursor was computed from.
    pub spec_hash: String,
    /// The account whose app seeded the cursor and fires it.
    pub owner: String,
}

/// Coordinator files whose schema this process has already brought up to date.
static READY: std::sync::Mutex<std::collections::BTreeSet<PathBuf>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// Handle on the routine tables. Cheap to clone; each call opens its own
/// connection, matching the coordinator's concurrency model.
#[derive(Debug, Clone)]
pub struct RoutineStateStore {
    path: PathBuf,
}

impl RoutineStateStore {
    /// The coordinator file this store reads.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The store in this install's coordinator file.
    pub fn open(codesense_home: &Path) -> StoreResult<Self> {
        Self::open_at(default_store_path(codesense_home))
    }

    /// The store at an explicit coordinator path (tests use a temp file).
    /// Bringing the schema up to date runs once per file per process, so the
    /// completion and dispatch paths that open the store per call stay cheap.
    pub fn open_at(path: impl Into<PathBuf>) -> StoreResult<Self> {
        let path = path.into();
        let mut ready = READY.lock().unwrap_or_else(|e| e.into_inner());
        if !ready.contains(&path) || !path.exists() {
            SqliteOrchestrationStore::open(path.clone())?;
            ready.insert(path.clone());
        }
        Ok(Self { path })
    }

    fn conn(&self) -> StoreResult<rusqlite::Connection> {
        crate::shared::sqlite::open_conn(&self.path).map_err(StoreError::from)
    }

    /// The state row for `key`, or `None` when the routine has never been
    /// paused or run on this machine.
    pub fn state(&self, key: &RoutineKey) -> StoreResult<Option<RoutineState>> {
        let conn = self.conn()?;
        let row = conn
            .query_row(
                "SELECT routine_key, enabled, last_run_id, last_run_at, last_result_text, missed_count
                 FROM routine_state WHERE routine_key = ?1",
                params![key.to_string()],
                |r| {
                    Ok(RoutineState {
                        routine_key: r.get(0)?,
                        enabled: r.get::<_, i64>(1)? != 0,
                        last_run_id: r.get(2)?,
                        last_run_at: r.get(3)?,
                        last_result_text: r.get(4)?,
                        missed_count: r.get::<_, i64>(5)?.try_into().unwrap_or(u32::MAX),
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Whether `key`, defined as `def`, fires from this Mac (`host`); see
    /// [`enabled_here`].
    pub fn is_enabled(&self, key: &RoutineKey, def: &RoutineDef, host: &str) -> StoreResult<bool> {
        let switch = switch_in(&self.conn()?, &key.to_string())?;
        Ok(enabled_here(switch, def, host))
    }

    /// Every switch flipped on this Mac, by rendered key.
    pub fn switches(&self) -> StoreResult<HashMap<String, Switch>> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT routine_key, enabled, enabled_mode FROM routine_state")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (key, enabled, mode) = row?;
            if let Some(switch) = Switch::from_row(enabled, mode) {
                out.insert(key, switch);
            }
        }
        Ok(out)
    }

    /// Settle every switch flipped under a mode other than the one each of
    /// `routines` (rendered key, definition) has now: it is rewritten to what
    /// it reads as under the current mode (see [`enabled_here`]), tagged with
    /// that mode. A routine that changes mode and later changes back then
    /// starts from where it stood in between, never from a choice made before.
    /// The run state on the same row is kept, and a wake that finds no stale
    /// switch writes nothing. Returns how many switches were settled.
    pub fn settle_switches(
        &self,
        routines: &[(String, &RoutineDef)],
        host: &str,
    ) -> StoreResult<usize> {
        let stale = self.stale_switches(routines, host)?;
        if stale.is_empty() {
            return Ok(0);
        }
        self.write_settled(&stale)
    }

    /// Each switch of `routines` flipped under another mode than its current
    /// one: (rendered key, the switch as read, what it settles to).
    fn stale_switches(
        &self,
        routines: &[(String, &RoutineDef)],
        host: &str,
    ) -> StoreResult<Vec<(String, Switch, Switch)>> {
        let switches = self.switches()?;
        Ok(routines
            .iter()
            .filter_map(|(key, def)| {
                let mode = def.runs_on().ok()?;
                let switch = *switches.get(key)?;
                (switch.mode != mode).then(|| {
                    let settled = Switch {
                        enabled: enabled_here(Some(switch), def, host),
                        mode,
                    };
                    (key.clone(), switch, settled)
                })
            })
            .collect())
    }

    /// Write each settled switch, but only while its row still holds the
    /// switch it was read as, so a flip made here in between (say "Run on my
    /// Mac") is never overwritten by what the stale one settled to.
    fn write_settled(&self, stale: &[(String, Switch, Switch)]) -> StoreResult<usize> {
        self.transaction(|tx| {
            let mut stmt = tx.prepare(
                "UPDATE routine_state SET enabled = ?2, enabled_mode = ?3
                 WHERE routine_key = ?1 AND enabled = ?4
                   AND COALESCE(enabled_mode, 'one_mac') = ?5",
            )?;
            let mut written = 0;
            for (key, was, settled) in stale {
                written += stmt.execute(params![
                    key,
                    i64::from(settled.enabled),
                    settled.mode.as_str(),
                    i64::from(was.enabled),
                    was.mode.as_str(),
                ])?;
            }
            Ok(written)
        })
    }

    /// Every trigger cursor on this machine.
    pub fn triggers(&self) -> StoreResult<Vec<TriggerRow>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT trigger_id, routine_key, next_run_at, spec_hash, owner FROM routine_triggers",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(TriggerRow {
                trigger_id: r.get(0)?,
                routine_key: r.get(1)?,
                next_run_at: r.get(2)?,
                spec_hash: r.get(3)?,
                owner: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Point a trigger at `next_run_at` for the spec `spec_hash`, recording
    /// `owner` as the personal space that seeded it, creating the row if
    /// needed. A row already on the same spec is left alone, whoever seeded it,
    /// so two racing seeds cannot move a cursor another tick has claimed and a
    /// switch of personal space on this Mac carries the schedule on.
    pub fn seed_trigger(
        &self,
        trigger_id: &str,
        key: &RoutineKey,
        next_run_at: Option<i64>,
        spec_hash: &str,
        owner: &Principal,
    ) -> StoreResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO routine_triggers (trigger_id, routine_key, next_run_at, spec_hash, owner)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(trigger_id) DO UPDATE SET
                 routine_key = excluded.routine_key,
                 next_run_at = excluded.next_run_at,
                 spec_hash = excluded.spec_hash,
                 owner = excluded.owner
             WHERE routine_triggers.spec_hash != excluded.spec_hash",
            params![
                trigger_id,
                key.to_string(),
                next_run_at,
                spec_hash,
                owner.as_str()
            ],
        )?;
        Ok(())
    }

    /// Drop the cursors of `routines` (rendered keys) whose trigger id is not
    /// in `live`, whoever seeded them: a cursor is this Mac's. Cursors of
    /// routines outside `routines` are left alone.
    pub fn prune_triggers(
        &self,
        routines: &HashSet<String>,
        live: &HashSet<String>,
    ) -> StoreResult<usize> {
        let conn = self.conn()?;
        let stale: Vec<String> = {
            let mut stmt = conn.prepare("SELECT trigger_id, routine_key FROM routine_triggers")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            rows.filter_map(Result::ok)
                .filter(|(id, key)| routines.contains(key) && !live.contains(id))
                .map(|(id, _)| id)
                .collect()
        };
        for id in &stale {
            conn.execute(
                "DELETE FROM routine_triggers WHERE trigger_id = ?1",
                params![id],
            )?;
        }
        Ok(stale.len())
    }

    /// Advance a trigger from `old` to `next` and run `admit` in the same
    /// transaction. Compare-and-swap: when another tick already moved the
    /// cursor, nothing is written and this returns `false`. Either both the
    /// advance and whatever `admit` writes commit, or neither does.
    pub fn claim<F>(
        &self,
        trigger_id: &str,
        old: i64,
        next: Option<i64>,
        admit: F,
    ) -> StoreResult<bool>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> StoreResult<()>,
    {
        self.transaction(|tx| {
            let changed = tx.execute(
                "UPDATE routine_triggers SET next_run_at = ?1
                 WHERE trigger_id = ?2 AND next_run_at = ?3",
                params![next, trigger_id, old],
            )?;
            if changed == 0 {
                return Ok(false);
            }
            admit(tx)?;
            Ok(true)
        })
    }

    /// Consistent cached reads never reserve the coordinator writer lock.
    pub(super) fn read_transaction<T>(
        &self,
        f: impl FnOnce(&rusqlite::Connection) -> StoreResult<T>,
    ) -> StoreResult<T> {
        let mut conn = self.conn()?;
        conn.pragma_update(None, "query_only", true)?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
        let result = f(&tx)?;
        tx.commit()?;
        Ok(result)
    }

    /// Run `f` in one `IMMEDIATE` transaction, committed when it returns `Ok`.
    pub fn transaction<T>(
        &self,
        f: impl FnOnce(&rusqlite::Transaction<'_>) -> StoreResult<T>,
    ) -> StoreResult<T> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// One run row.
    pub fn run(&self, run_id: &str) -> StoreResult<Option<RoutineRun>> {
        Ok(runs::run_in(&self.conn()?, run_id)?)
    }

    /// Every run of `key`, oldest first.
    pub fn runs(&self, key: &RoutineKey) -> StoreResult<Vec<RoutineRun>> {
        self.select_runs("routine_key = ?1", params![key.to_string()])
    }

    /// Every `running` run on this machine, oldest first.
    pub fn running_runs(&self) -> StoreResult<Vec<RoutineRun>> {
        self.select_runs("phase = 'running'", [])
    }

    /// The keys of every routine with a run queued or running on this machine.
    pub fn active_routine_keys(&self) -> StoreResult<std::collections::HashSet<String>> {
        Ok(self
            .select_runs("phase IN ('queued', 'running')", [])?
            .into_iter()
            .map(|run| run.routine_key)
            .collect())
    }

    fn select_runs(
        &self,
        filter: &str,
        args: impl rusqlite::Params,
    ) -> StoreResult<Vec<RoutineRun>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM routine_runs WHERE {filter} ORDER BY created_at, rowid",
            runs::RUN_COLUMNS
        ))?;
        let rows = stmt.query_map(args, runs::read_run)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// `key`'s runs newest first: at most `limit`, older than the run
    /// `before` when given. `None` when `before` names no run.
    pub fn runs_newest_first(
        &self,
        key: &RoutineKey,
        before: Option<&str>,
        limit: usize,
    ) -> StoreResult<Option<Vec<RoutineRun>>> {
        let conn = self.conn()?;
        let cursor: Option<(i64, i64)> = match before {
            Some(run_id) => {
                let found = conn
                    .query_row(
                        "SELECT created_at, rowid FROM routine_runs
                         WHERE run_id = ?1 AND routine_key = ?2",
                        params![run_id, key.to_string()],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                if found.is_none() {
                    return Ok(None);
                }
                found
            }
            None => None,
        };
        let (created_at, rowid) = cursor.unwrap_or((i64::MAX, i64::MAX));
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM routine_runs
             WHERE routine_key = ?1 AND (created_at, rowid) < (?2, ?3)
             ORDER BY created_at DESC, rowid DESC LIMIT ?4",
            runs::RUN_COLUMNS
        ))?;
        let rows = stmt.query_map(
            params![
                key.to_string(),
                created_at,
                rowid,
                i64::try_from(limit).unwrap_or(i64::MAX)
            ],
            runs::read_run,
        )?;
        Ok(Some(rows.collect::<Result<Vec<_>, _>>()?))
    }

    /// The runs of `keys` that happened at or after `since_ms` (ended, else
    /// started, else created), newest first: at most `per_routine` of each
    /// routine's runs and, counted apart so they never crowd those out, at
    /// most `per_routine` of its merged and skipped fires.
    /// A run lasts at most a day (`max_runtime`), so one created more than a
    /// day before `since_ms` cannot qualify and the read never scans it.
    pub fn runs_since(
        &self,
        keys: &[String],
        since_ms: i64,
        per_routine: usize,
    ) -> StoreResult<Vec<RoutineRun>> {
        const MAX_RUN_MS: i64 = 24 * 60 * 60 * 1000;
        let conn = self.conn()?;
        let marks = vec!["?"; keys.len()].join(", ");
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM (
                 SELECT *, rowid AS row_order, ROW_NUMBER() OVER (
                     PARTITION BY routine_key, phase IN ('coalesced', 'skipped')
                     ORDER BY created_at DESC, rowid DESC) AS n
                 FROM routine_runs
                 WHERE routine_key IN ({marks})
                   AND created_at >= ?
                   AND COALESCE(ended_at, started_at, created_at) >= ?) AS routine_runs
             WHERE n <= ?
             ORDER BY created_at DESC, row_order DESC",
            runs::RUN_COLUMNS
        ))?;
        let mut values: Vec<rusqlite::types::Value> = keys
            .iter()
            .map(|k| rusqlite::types::Value::Text(k.clone()))
            .collect();
        values.push(rusqlite::types::Value::Integer(
            since_ms.saturating_sub(MAX_RUN_MS),
        ));
        values.push(rusqlite::types::Value::Integer(since_ms));
        values.push(rusqlite::types::Value::Integer(
            i64::try_from(per_routine).unwrap_or(i64::MAX),
        ));
        let rows = stmt.query_map(rusqlite::params_from_iter(values), runs::read_run)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Each routine's latest run that started: running, or ended after it
    /// started. Skips, merges, runs still waiting and runs that ended before
    /// they started (stopped while queued, refused at dispatch) are only in
    /// its history, as `finish` never makes them the routine's last run. A
    /// run that ended `undelivered` or `unknown` without ever starting (a wake
    /// straight from queued, whichever path ended it) is a last run too: its
    /// outcome is the news.
    pub fn last_runs(&self) -> StoreResult<HashMap<String, RoutineRun>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM (
                 SELECT *, ROW_NUMBER() OVER (
                     PARTITION BY routine_key ORDER BY created_at DESC, rowid DESC) AS n
                 FROM routine_runs
                 WHERE (started_at IS NOT NULL OR phase IN ('undelivered', 'unknown'))
                   AND (phase = 'running' OR {})) AS routine_runs
             WHERE n = 1",
            runs::RUN_COLUMNS,
            runs::TERMINAL
        ))?;
        let rows = stmt.query_map([], runs::read_run)?;
        let mut out = HashMap::new();
        for run in rows {
            let run = run?;
            out.insert(run.routine_key.clone(), run);
        }
        Ok(out)
    }

    /// Every routine whose latest run that started and ended with an outcome,
    /// ended `needs_you`, with that run. A run that simply `ended` said
    /// nothing, so it neither raises nor answers a needs-you.
    pub fn needing_you(&self) -> StoreResult<HashMap<String, RoutineRun>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM (
                 SELECT *, ROW_NUMBER() OVER (
                     PARTITION BY routine_key ORDER BY ended_at DESC, rowid DESC) AS n
                 FROM routine_runs
                 WHERE started_at IS NOT NULL AND {} AND phase <> 'ended') AS routine_runs
             WHERE n = 1 AND phase = 'needs_you'",
            runs::RUN_COLUMNS,
            runs::TERMINAL
        ))?;
        let rows = stmt.query_map([], runs::read_run)?;
        let mut out = HashMap::new();
        for run in rows {
            let run = run?;
            out.insert(run.routine_key.clone(), run);
        }
        Ok(out)
    }

    /// `key`'s latest succeeded run that ended by `at`: the run whose result a
    /// task rendered at `at` saw as its `{{last_run_summary}}`.
    pub fn last_success_by(&self, key: &RoutineKey, at: i64) -> StoreResult<Option<RoutineRun>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {} FROM routine_runs
                     WHERE routine_key = ?1 AND phase = 'succeeded' AND ended_at <= ?2
                     ORDER BY ended_at DESC, rowid DESC LIMIT 1",
                    runs::RUN_COLUMNS
                ),
                params![key.to_string(), at],
                runs::read_run,
            )
            .optional()?)
    }

    /// Payloads carried by occurrences merged into this run, in observation order.
    pub fn coalesced_payloads(&self, run_id: &str) -> StoreResult<Vec<serde_json::Value>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT request FROM routine_runs WHERE linked_run_id = ?1 AND phase = 'coalesced' ORDER BY created_at, rowid")?;
        let requests = stmt.query_map([run_id], |row| row.get::<_, Option<String>>(0))?;
        let mut payloads = Vec::new();
        for request in requests {
            if let Some(Ok(pointer)) = request?.as_deref().and_then(super::admit::parse_pointer) {
                if let Some(payload) = pointer.trigger_payload {
                    payloads.push(payload);
                }
            }
        }
        Ok(payloads)
    }

    /// The run whose agent is `session_id`, latest first.
    pub fn run_by_session(&self, session_id: &str) -> StoreResult<Option<RoutineRun>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {} FROM routine_runs WHERE session_id = ?1
                     ORDER BY created_at DESC, rowid DESC LIMIT 1",
                    runs::RUN_COLUMNS
                ),
                params![session_id],
                runs::read_run,
            )
            .optional()?)
    }

    /// Occurrences of `keys` missed since they were last acknowledged.
    pub fn missed_count(&self, keys: &HashSet<String>) -> StoreResult<u64> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT routine_key, missed_count FROM routine_state WHERE missed_count > 0",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        let mut total = 0u64;
        for row in rows {
            let (key, count) = row?;
            if keys.contains(&key) {
                total = total.saturating_add(u64::try_from(count).unwrap_or(0));
            }
        }
        Ok(total)
    }

    /// Clear the missed count of each of `keys`. Returns the keys that had one.
    pub fn acknowledge_missed(&self, keys: &HashSet<String>) -> StoreResult<Vec<String>> {
        self.transaction(|tx| {
            let mut cleared = Vec::new();
            for key in keys {
                let changed = tx.execute(
                    "UPDATE routine_state SET missed_count = 0
                     WHERE routine_key = ?1 AND missed_count > 0",
                    params![key],
                )?;
                if changed > 0 {
                    cleared.push(key.clone());
                }
            }
            Ok(cleared)
        })
    }

    /// Flip `key`'s switch on this machine only, under `mode`: pause or
    /// resume a shared job, or turn a recipe on or off for this Mac. A
    /// resume (off to on) starts a new wake budget (SW-AGR-13).
    pub fn set_enabled(&self, key: &RoutineKey, mode: RunsOn, enabled: bool) -> StoreResult<()> {
        self.write_switch(key, i64::from(enabled), Some(mode.as_str()))
    }

    /// SW-AGR-13: the routine reached its `max_wakes` in a run fired at
    /// `fired_at` (epoch ms): turn this Mac's switch off under the mode it is
    /// on in (no switch yet: a shared job, on by default). True when this
    /// paused it; false when it was already off, or when the run was fired
    /// before the routine's last resume (a stale result never pauses the new
    /// count).
    pub fn pause_for_wake_limit(&self, key: &str, fired_at: i64) -> StoreResult<bool> {
        self.transaction(|tx| {
            let switch = switch_in(tx, key)?;
            if switch.is_some_and(|s| !s.enabled) {
                return Ok(false);
            }
            let resumed_at: Option<i64> = tx
                .query_row(
                    "SELECT wake_budget_at FROM routine_state WHERE routine_key = ?1",
                    params![key],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            if resumed_at.is_some_and(|at| fired_at < at) {
                return Ok(false);
            }
            let mode = switch.map(|s| s.mode).unwrap_or(RunsOn::OneMac);
            tx.execute(
                "INSERT INTO routine_state (routine_key, enabled, enabled_mode) VALUES (?1, 0, ?2)
                 ON CONFLICT(routine_key) DO UPDATE SET
                     enabled = 0,
                     enabled_mode = excluded.enabled_mode",
                params![key, mode.as_str()],
            )?;
            Ok(true)
        })
    }

    /// `key`'s wake budget generation (SW-AGR-13): 0 until it is first
    /// resumed from a pause.
    pub fn wake_budget(&self, key: &str) -> StoreResult<u64> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT wake_budget FROM routine_state WHERE routine_key = ?1",
                params![key],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
            .max(0) as u64)
    }

    /// Whether this Mac's raw switch for `key` is on (no row: on).
    pub fn switch_on(&self, key: &str) -> StoreResult<bool> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT enabled FROM routine_state WHERE routine_key = ?1",
                params![key],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .is_none_or(|enabled| enabled != 0))
    }

    /// Forget `key`'s switch, so it reads as its mode's default again.
    pub fn clear_switch(&self, key: &RoutineKey) -> StoreResult<()> {
        self.write_switch(key, 1, None)
    }

    /// Write the switch. Turning a paused routine on (a resume, or a cleared
    /// switch) starts a new wake budget and stamps when (SW-AGR-13).
    fn write_switch(&self, key: &RoutineKey, enabled: i64, mode: Option<&str>) -> StoreResult<()> {
        let now = chrono::Utc::now().timestamp_millis();
        self.transaction(|tx| {
            let was_off = tx
                .query_row(
                    "SELECT enabled FROM routine_state WHERE routine_key = ?1",
                    params![key.to_string()],
                    |r| r.get::<_, i64>(0),
                )
                .optional()?
                == Some(0);
            tx.execute(
                "INSERT INTO routine_state (routine_key, enabled, enabled_mode) VALUES (?1, ?2, ?3)
                 ON CONFLICT(routine_key) DO UPDATE SET
                     enabled = excluded.enabled,
                     enabled_mode = excluded.enabled_mode",
                params![key.to_string(), enabled, mode],
            )?;
            if enabled != 0 && was_off {
                tx.execute(
                    "UPDATE routine_state SET wake_budget = wake_budget + 1, wake_budget_at = ?2
                      WHERE routine_key = ?1",
                    params![key.to_string(), now],
                )?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The coordinator's column-name guard, applied to whatever schema a file
    /// ends up with. Mirrors `schema_persists_no_metadata_mirroring_column`.
    fn assert_no_metadata_columns(sql: &str) {
        let sql = sql.to_lowercase();
        for forbidden in [
            "status",
            "rule",
            "definition",
            "config",
            "assignee",
            "title",
            "summary",
            "priority",
            "metadata",
        ] {
            assert!(!sql.contains(forbidden), "`{forbidden}` in schema:\n{sql}");
        }
    }

    fn table_names(path: &Path) -> Vec<String> {
        let conn = rusqlite::Connection::open(path).unwrap();
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
        rows.collect::<Result<Vec<_>, _>>().unwrap()
    }

    /// A routine as stored, armed on `armed_host` and run `runs_on`.
    fn def(armed_host: &str, runs_on: &str) -> RoutineDef {
        let mut def: RoutineDef = serde_yaml::from_str(
            "id: triage\nname: T\nowner: '1'\nagent: a\ntask: t\nwhere: {kind: team_workspace}\npolicy: {max_runtime: 30m}\nrevision: 1\n",
        )
        .unwrap();
        def.armed_host = armed_host.into();
        def.runs_on = runs_on.into();
        def
    }

    #[test]
    fn missing_row_is_enabled_and_pause_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = RoutineStateStore::open(dir.path()).unwrap();
        let key = RoutineKey::new("acme/api", "backend", "triage");
        let job = def("here", "");
        assert_eq!(store.state(&key).unwrap(), None);
        assert!(store.is_enabled(&key, &job, "here").unwrap());

        store.set_enabled(&key, RunsOn::OneMac, false).unwrap();
        assert!(!store.is_enabled(&key, &job, "here").unwrap());
        let row = store.state(&key).unwrap().unwrap();
        assert_eq!(row.routine_key, "acme/api/backend/triage");
        assert_eq!(row.missed_count, 0);
        assert_eq!(row.last_run_id, None);

        store.set_enabled(&key, RunsOn::OneMac, true).unwrap();
        assert!(store.is_enabled(&key, &job, "here").unwrap());
        // Other routines are unaffected.
        assert!(store
            .is_enabled(
                &RoutineKey::new("acme/api", "backend", "other"),
                &job,
                "here"
            )
            .unwrap());
        // The default path is the coordinator file.
        assert!(dir
            .path()
            .join("orchestration/coordinator.sqlite3")
            .exists());
    }

    #[test]
    fn a_switch_counts_only_under_the_mode_it_was_flipped_in() {
        let dir = tempfile::tempdir().unwrap();
        let store = RoutineStateStore::open(dir.path()).unwrap();
        let key = RoutineKey::new("acme/api", "backend", "triage");
        let rendered = key.to_string();
        let on = |def: &RoutineDef, host: &str| store.is_enabled(&key, def, host).unwrap();
        let switch = || store.switches().unwrap().get(&rendered).copied();

        // A recipe each Mac runs is off here by default, unless this is the
        // Mac it ran on as a shared job.
        assert!(!on(&def("", "each_mac"), "here"));
        assert!(!on(&def("other", "each_mac"), "here"));
        assert!(on(&def("here", "each_mac"), "here"));

        // A recipe turned on here does not arm the shared job here, nor does
        // a recipe turned off here pause it.
        store.set_enabled(&key, RunsOn::EachMac, true).unwrap();
        assert!(on(&def("other", "each_mac"), "here"));
        store.set_enabled(&key, RunsOn::EachMac, false).unwrap();
        assert!(!on(&def("here", "each_mac"), "here"), "turned off here");
        assert!(on(&def("here", ""), "here"));

        // A paused (or draft) shared job stays off on its Mac when it becomes
        // a recipe; anywhere else a recipe starts off anyway.
        store.set_enabled(&key, RunsOn::OneMac, false).unwrap();
        assert!(!on(&def("here", ""), "here"));
        assert!(!on(&def("here", "each_mac"), "here"));
        assert!(!on(&def("other", "each_mac"), "here"));

        // Settling pins what a switch reads as under the current mode, so the
        // pause survives as the recipe's switch, and once settled there is
        // nothing more to write.
        let recipe = def("here", "each_mac");
        let seen = [(rendered.clone(), &recipe)];
        assert_eq!(store.settle_switches(&seen, "here").unwrap(), 1);
        assert_eq!(store.settle_switches(&seen, "here").unwrap(), 0);
        assert_eq!(
            switch(),
            Some(Switch {
                enabled: false,
                mode: RunsOn::EachMac
            })
        );
        assert!(!on(&recipe, "here"));

        // Seen as a shared job again, the recipe's "off" is settled into the
        // job's default (on), so it neither pauses the job nor comes back as
        // "off" if the routine becomes a recipe again.
        let job = def("here", "");
        assert_eq!(
            store
                .settle_switches(&[(rendered.clone(), &job)], "here")
                .unwrap(),
            1
        );
        assert_eq!(
            switch(),
            Some(Switch {
                enabled: true,
                mode: RunsOn::OneMac
            })
        );
        assert!(on(&job, "here"));
        assert!(!on(&def("other", "each_mac"), "here"));
        // A cleared switch reads as the default under either mode.
        store.clear_switch(&key).unwrap();
        assert!(on(&job, "here"));
        assert!(!on(&def("other", "each_mac"), "here"));
        // A mode this build cannot read never fires, and is never settled.
        let unknown = def("here", "every_mac");
        assert!(!on(&unknown, "here"));
        assert_eq!(
            store
                .settle_switches(&[(rendered.clone(), &unknown)], "here")
                .unwrap(),
            0
        );
    }

    #[test]
    fn settling_never_overwrites_a_switch_flipped_after_it_was_read() {
        let dir = tempfile::tempdir().unwrap();
        let store = RoutineStateStore::open(dir.path()).unwrap();
        let key = RoutineKey::new("acme/api", "backend", "triage");
        let rendered = key.to_string();
        store.set_enabled(&key, RunsOn::OneMac, true).unwrap();
        let recipe = def("other", "each_mac");
        let seen = [(rendered.clone(), &recipe)];
        // The settle reads the stale shared-job switch (it settles to off
        // here), then the user turns the recipe on for this Mac before the
        // settle writes.
        let stale = store.stale_switches(&seen, "here").unwrap();
        assert_eq!(stale.len(), 1);
        assert!(!stale[0].2.enabled);
        store.set_enabled(&key, RunsOn::EachMac, true).unwrap();
        assert_eq!(store.write_settled(&stale).unwrap(), 0);
        assert!(store.is_enabled(&key, &recipe, "here").unwrap());
        assert_eq!(store.settle_switches(&seen, "here").unwrap(), 0);
    }

    #[test]
    fn an_older_routine_state_table_gains_the_switch_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("coordinator.sqlite3");
        RoutineStateStore::open_at(&path).unwrap();
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "DROP TABLE routine_state;
                 CREATE TABLE routine_state (
                     routine_key TEXT PRIMARY KEY, enabled INTEGER NOT NULL DEFAULT 1,
                     last_run_id TEXT, last_run_at INTEGER, last_result_text TEXT,
                     missed_count INTEGER NOT NULL DEFAULT 0);
                 INSERT INTO routine_state (routine_key, enabled) VALUES ('acme/api/backend/triage', 0);",
            )
            .unwrap();
        }
        SqliteOrchestrationStore::open(&path).unwrap();
        let store = RoutineStateStore::open_at(&path).unwrap();
        let key = RoutineKey::new("acme/api", "backend", "triage");
        // The pause from before modes is the shared job's, and a routine
        // someone paused stays off if it becomes a recipe.
        assert!(!store.is_enabled(&key, &def("here", ""), "here").unwrap());
        assert!(!store
            .is_enabled(&key, &def("here", "each_mac"), "here")
            .unwrap());
        store.set_enabled(&key, RunsOn::OneMac, true).unwrap();
        assert!(store
            .is_enabled(&key, &def("here", "each_mac"), "here")
            .unwrap());
    }

    #[test]
    fn older_coordinator_file_gains_routine_tables_and_keeps_the_guard() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("coordinator.sqlite3");
        // A coordinator file written by the previous build: the coordination
        // tables, no routine tables, and an existing coordination row.
        let store = SqliteOrchestrationStore::open(&path).unwrap();
        store.enqueue_runnable("run-before-upgrade").unwrap();
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "DROP TABLE routine_results; DROP TABLE routine_result_runs;
                 DROP TABLE routine_state; DROP TABLE routine_triggers; DROP TABLE routine_runs;
                 DROP TABLE routine_arm_handover; DROP TABLE routine_arm_cursors;
                 DROP TABLE routine_arm_floor; DROP TABLE routine_arm_stale;
                 DROP TABLE routine_arm_prepared;",
            )
            .unwrap();
        }
        assert!(!table_names(&path).iter().any(|t| t.starts_with("routine_")));

        let routines = RoutineStateStore::open_at(&path).unwrap();
        let tables = table_names(&path);
        for t in [
            "routine_state",
            "routine_triggers",
            "routine_runs",
            "routine_result_runs",
            "routine_results",
            "routine_arm_handover",
            "routine_arm_cursors",
            "routine_arm_floor",
            "routine_arm_stale",
            "routine_arm_prepared",
        ] {
            assert!(tables.contains(&t.to_string()), "{t} missing: {tables:?}");
        }
        let key = RoutineKey::new("acme/api", "backend", "triage");
        let job = def("here", "");
        routines.set_enabled(&key, RunsOn::OneMac, false).unwrap();
        assert!(!routines.is_enabled(&key, &job, "here").unwrap());

        // Re-opening is idempotent and the pre-upgrade coordination row survives.
        let reopened = SqliteOrchestrationStore::open(&path).unwrap();
        assert_eq!(
            reopened
                .count_runs_in_state(crate::coordinator::RunState::Runnable)
                .unwrap(),
            1
        );
        assert!(!RoutineStateStore::open_at(&path)
            .unwrap()
            .is_enabled(&key, &job, "here")
            .unwrap());
        assert_no_metadata_columns(&reopened.schema_sql().unwrap());
    }

    #[test]
    fn a_cursor_is_this_macs_whichever_personal_space_seeded_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = RoutineStateStore::open(dir.path()).unwrap();
        let key = RoutineKey::new("acme/api", "backend", "triage");
        let (alice, bob) = (Principal::from(1u64), Principal::from(2u64));
        let id = format!("{key}#0");
        let row = || store.triggers().unwrap().pop().unwrap();
        let routines = HashSet::from([key.to_string()]);

        store
            .seed_trigger(&id, &key, Some(100), "h", &alice)
            .unwrap();
        // A re-seed on the same spec keeps its claimed position, whoever
        // re-seeds it, so a switch of personal space carries the schedule on.
        store
            .seed_trigger(&id, &key, Some(500), "h", &alice)
            .unwrap();
        store.seed_trigger(&id, &key, Some(900), "h", &bob).unwrap();
        assert_eq!((row().next_run_at, row().owner), (Some(100), "1".into()));
        // A changed spec re-seeds it.
        store
            .seed_trigger(&id, &key, Some(900), "h2", &bob)
            .unwrap();
        assert_eq!((row().next_run_at, row().owner), (Some(900), "2".into()));
        // A live trigger is kept, and a routine outside the set is left alone.
        assert_eq!(
            store
                .prune_triggers(&routines, &HashSet::from([id.clone()]))
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .prune_triggers(&HashSet::new(), &HashSet::new())
                .unwrap(),
            0
        );
        assert_eq!(store.prune_triggers(&routines, &HashSet::new()).unwrap(), 1);
        assert!(store.triggers().unwrap().is_empty());
    }

    // SW-AGR-13: a resume (off to on) starts a new wake budget; a pause, or
    // an on that was already on, does not.
    #[test]
    fn a_resume_starts_a_new_wake_budget() {
        let dir = tempfile::tempdir().unwrap();
        let store = RoutineStateStore::open_at(dir.path().join("c.sqlite3")).unwrap();
        let key = RoutineKey::parse("acme/core/heavy").unwrap();
        let k = key.to_string();
        assert_eq!(store.wake_budget(&k).unwrap(), 0);
        store.set_enabled(&key, RunsOn::OneMac, true).unwrap();
        assert_eq!(store.wake_budget(&k).unwrap(), 0, "on while on");
        store.set_enabled(&key, RunsOn::OneMac, false).unwrap();
        assert!(!store.switch_on(&k).unwrap());
        assert_eq!(store.wake_budget(&k).unwrap(), 0, "a pause");
        store.set_enabled(&key, RunsOn::OneMac, true).unwrap();
        assert_eq!(store.wake_budget(&k).unwrap(), 1, "a resume");
        store.set_enabled(&key, RunsOn::OneMac, true).unwrap();
        assert_eq!(store.wake_budget(&k).unwrap(), 1);
        assert!(store.switch_on("acme/core/never-switched").unwrap());
        // r1: clearing a paused switch is a resume too.
        store.set_enabled(&key, RunsOn::OneMac, false).unwrap();
        store.clear_switch(&key).unwrap();
        assert_eq!(store.wake_budget(&k).unwrap(), 2);
    }

    // r1 LOW: a `max_wakes` result from before the last resume never pauses
    // the new count; one from after it does, once.
    #[test]
    fn a_stale_max_wakes_result_never_pauses_a_resumed_routine() {
        let dir = tempfile::tempdir().unwrap();
        let store = RoutineStateStore::open_at(dir.path().join("c.sqlite3")).unwrap();
        let key = RoutineKey::parse("acme/core/heavy").unwrap();
        let k = key.to_string();
        let before = chrono::Utc::now().timestamp_millis() - 60_000;
        assert!(
            store.pause_for_wake_limit(&k, before).unwrap(),
            "first pause"
        );
        store.set_enabled(&key, RunsOn::OneMac, true).unwrap();
        assert!(!store.pause_for_wake_limit(&k, before).unwrap(), "stale");
        assert!(store.switch_on(&k).unwrap());
        let after = chrono::Utc::now().timestamp_millis() + 1;
        assert!(store.pause_for_wake_limit(&k, after).unwrap());
        assert!(!store.pause_for_wake_limit(&k, after).unwrap(), "once");
    }

    #[test]
    fn occurrence_key_is_unique() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.sqlite3");
        RoutineStateStore::open_at(&path).unwrap();
        let conn = rusqlite::Connection::open(&path).unwrap();
        let insert = |run_id: &str| {
            conn.execute(
                "INSERT INTO routine_runs (run_id, routine_key, revision, source, occurrence_key, phase, created_at)
                 VALUES (?1, 'a/b/c/d', 1, 'schedule', 'a/b/c/d@1', 'queued', 0)",
                params![run_id],
            )
        };
        insert("r1").unwrap();
        assert!(
            insert("r2").is_err(),
            "a re-delivered occurrence must not admit twice"
        );
    }
}
