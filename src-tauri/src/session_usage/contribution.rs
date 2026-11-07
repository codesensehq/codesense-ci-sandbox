// SPDX-License-Identifier: MIT
//! Cached aggregates kept as contribution rows, not as mutable totals.
//!
//! An entity (a story, first) is made of facts that each belong to one source (a session, a task).
//! Each source's facts for an entity are one `contribution` row keyed by the source's own id, and
//! the entity's aggregate is a pure fold of its rows. One event therefore rewrites one row
//! (`apply`), and a replay, a restart or a duplicate event rewrites the same row: nothing is ever
//! added to a stored total, so nothing double counts.
//!
//! The engine is generic: it knows five reducers and nothing about stories. Each kind brings an
//! [`EntityAdapter`] (which sources feed it, how a source maps to entities, how an aggregate reads).
//! There is no query language: a kind's reducers are a const table.
use super::{db_err, UsageStore};
use crate::context_repo::caller_scope::CallerScope;
use rusqlite::{params, OptionalExtension};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS contribution (
    scope_key   TEXT NOT NULL,
    entity_kind TEXT NOT NULL,
    entity_id   TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    source_id   TEXT NOT NULL,
    first_ms    INTEGER,
    last_ms     INTEGER,
    nums        TEXT NOT NULL DEFAULT '{}',
    reasons     TEXT NOT NULL DEFAULT '{}',
    extra       TEXT NOT NULL DEFAULT '{}',
    version     INTEGER NOT NULL,
    updated_ms  INTEGER NOT NULL,
    PRIMARY KEY (scope_key, entity_kind, entity_id, source_kind, source_id)
);
CREATE INDEX IF NOT EXISTS contribution_by_source ON contribution (entity_kind, source_kind, source_id);
CREATE INDEX IF NOT EXISTS contribution_by_entity ON contribution (entity_kind, entity_id, source_kind, source_id);
CREATE TABLE IF NOT EXISTS source_event (
    source_kind TEXT NOT NULL,
    source_id   TEXT NOT NULL,
    enqueued_ms INTEGER NOT NULL,
    attempts      INTEGER NOT NULL DEFAULT 0,
    not_before_ms INTEGER,
    PRIMARY KEY (source_kind, source_id)
);
";

/// The source kind of the row that says an entity's rows were built (and by which version).
const META: &str = "meta";
const BUILT: &str = "built";

/// One source's facts for one entity. A `nums` entry that is `None` is known-unknown, with its
/// reason in `reasons`; a name a row does not carry at all is simply not part of it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Contribution {
    pub first_ms: Option<i64>,
    pub last_ms: Option<i64>,
    pub nums: BTreeMap<String, Option<f64>>,
    pub reasons: BTreeMap<String, String>,
    pub extra: Map<String, Value>,
}

impl Contribution {
    pub(crate) fn new() -> Self {
        Self { first_ms: None, last_ms: None, nums: Default::default(), reasons: Default::default(), extra: Default::default() }
    }

    /// Set a number, or its absence with the reason.
    pub(crate) fn num(&mut self, name: &str, value: Option<f64>, reason: &str) {
        self.nums.insert(name.to_owned(), value);
        if Option::is_none(&value) {
            self.reasons.insert(name.to_owned(), reason.to_owned());
        }
    }
}

/// A queued source and the time it was last queued: `apply` deletes the queue row only if that is
/// still the time, so an event that lands while the source is being resolved is not lost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Queued {
    pub source: Source,
    pub enqueued_ms: i64,
}

/// A source: a session, a task, ... by the id the source's own table uses.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Source {
    pub kind: &'static str,
    pub id: String,
}

/// What a fold reads from a row.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Of {
    /// The row itself (for a count of rows).
    Row,
    /// A number the row carries.
    Num(&'static str),
    /// The row's first or last time.
    First,
    Last,
    /// A string (or list of strings) in the row's `extra`.
    Extra(&'static str),
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Op {
    /// The sum; null with the first reason when any row's number is unknown.
    Sum,
    /// The smallest and largest of the known values (an unknown one is left out).
    Min,
    Max,
    /// How many rows have the value and it is not 0 (for `Row`: every row).
    Count,
    /// The distinct strings.
    Union,
}

/// One output of a kind's aggregate: `op` over `of` in the rows of `source`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Reducer {
    pub out: &'static str,
    pub op: Op,
    pub of: Of,
    pub source: &'static str,
}

/// A kind's rows folded.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Aggregate {
    pub nums: BTreeMap<&'static str, Option<f64>>,
    pub reasons: BTreeMap<&'static str, String>,
    pub sets: BTreeMap<&'static str, BTreeSet<String>>,
}

/// A stored row with the source it came from.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Row {
    pub source: (String, String),
    pub contribution: Contribution,
}

/// What a kind brings to the engine. Nothing else about a kind lives here.
pub(crate) trait EntityAdapter {
    const KIND: &'static str;
    /// Bump it with any change to a row's content: every entity of the kind is rebuilt.
    const VERSION: i64;
    const REDUCERS: &'static [Reducer];
    /// The source kinds this kind is fed by: the queue rows it answers (others are left queued).
    const SOURCES: &'static [&'static str];
}

/// A kind whose queued sources are resolved to rows by `apply` (a kind whose rows are written by
/// its entity's build and only filled by its sweep needs none of this).
pub(crate) trait SourceResolver: EntityAdapter {
    /// The source a queued id names, and the rows it gives now for every entity it belongs to
    /// (none: it belongs to none, and its stored rows go).
    fn contributions(&self, store: &UsageStore, queued: &Source) -> Result<(Source, Vec<(String, Contribution)>), String>;

    /// Every row of one entity, as the sources give them now.
    fn rebuild(&self, store: &UsageStore, scope: &CallerScope, entity: &str) -> Result<Vec<(Source, Contribution)>, String>;
}

/// Fold `rows` with `reducers`.
pub(crate) fn aggregate(reducers: &[Reducer], rows: &[Row]) -> Aggregate {
    let mut out = Aggregate { nums: Default::default(), reasons: Default::default(), sets: Default::default() };
    for reducer in reducers {
        let mine: Vec<&Row> = rows.iter().filter(|row| row.source.0 == reducer.source).collect();
        match reducer.op {
            Op::Union => {
                let mut set: BTreeSet<String> = Default::default();
                for row in &mine {
                    match reducer.of {
                        Of::Extra(key) => match Map::get(&row.contribution.extra, key) {
                            Some(Value::String(s)) => {
                                set.insert(s.clone());
                            }
                            Some(Value::Array(items)) => {
                                for item in items {
                                    if let Some(s) = Value::as_str(item) {
                                        set.insert(s.to_owned());
                                    }
                                }
                            }
                            _ => {}
                        },
                        _ => {}
                    }
                }
                out.sets.insert(reducer.out, set);
            }
            Op::Count => {
                let n = match reducer.of {
                    Of::Row => mine.len(),
                    Of::Num(name) => mine
                        .iter()
                        .filter(|row| matches!(row.contribution.nums.get(name), Some(Some(v)) if *v != 0.0))
                        .count(),
                    _ => 0,
                };
                out.nums.insert(reducer.out, Some(n as f64));
            }
            Op::Sum => {
                let name = match reducer.of {
                    Of::Num(name) => name,
                    _ => continue,
                };
                let mut sum = 0.0;
                let mut unknown = None;
                for row in &mine {
                    match row.contribution.nums.get(name) {
                        Some(Some(v)) => sum += v,
                        Some(None) => {
                            unknown = unknown.or_else(|| row.contribution.reasons.get(name).cloned());
                            unknown = unknown.or(Some(Default::default()));
                        }
                        None => {}
                    }
                }
                match unknown {
                    None => {
                        out.nums.insert(reducer.out, Some(sum));
                    }
                    Some(reason) => {
                        out.nums.insert(reducer.out, None);
                        out.reasons.insert(reducer.out, reason);
                    }
                }
            }
            Op::Min | Op::Max => {
                let values = mine.iter().filter_map(|row| match reducer.of {
                    Of::Num(name) => Option::flatten(row.contribution.nums.get(name).copied()),
                    Of::First => row.contribution.first_ms.map(|v| v as f64),
                    Of::Last => row.contribution.last_ms.map(|v| v as f64),
                    _ => None,
                });
                let folded = if matches!(reducer.op, Op::Min) {
                    values.fold(None, |acc: Option<f64>, v| Some(Option::map_or(acc, v, |a| f64::min(a, v))))
                } else {
                    values.fold(None, |acc: Option<f64>, v| Some(Option::map_or(acc, v, |a| f64::max(a, v))))
                };
                out.nums.insert(reducer.out, folded);
            }
        }
    }
    out
}

/// The rows of an entity's source `kind`, in the order the story totals have always folded
/// them (latest activity first, then id), or `None` when its rows were not built by `version`.
pub(crate) fn rows_of(
    store: &UsageStore,
    scope_key: &str,
    kind: &str,
    entity: &str,
    version: i64,
) -> Result<Option<Vec<Row>>, String> {
    let built: Option<i64> = store
        .conn
        .query_row(
            "SELECT version FROM contribution WHERE scope_key = ?1 AND entity_kind = ?2 AND entity_id = ?3
               AND source_kind = ?4 AND source_id = ?5",
            params![scope_key, kind, entity, META, BUILT],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_err)?;
    if built != Some(version) {
        return Ok(None);
    }
    let mut stmt = store
        .conn
        .prepare_cached(
            "SELECT source_kind, source_id, first_ms, last_ms, nums, reasons, extra FROM contribution
             WHERE scope_key = ?1 AND entity_kind = ?2 AND entity_id = ?3 AND source_kind != ?4
             ORDER BY last_ms IS NULL, last_ms DESC, source_id ASC",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map(params![scope_key, kind, entity, META], |r| {
            let (source_kind, source_id): (String, String) = (r.get(0)?, r.get(1)?);
            Ok((source_kind, source_id, decode_from(r, 2)?))
        })
        .map_err(db_err)?;
    let mut out = vec![];
    for row in rows {
        let (source_kind, source_id, contribution) = row.map_err(db_err)?;
        out.push(Row { source: (source_kind, source_id), contribution: contribution? });
    }
    Ok(Some(out))
}

fn put(conn: &rusqlite::Connection, scope_key: &str, kind: &str, entity: &str, source: (&str, &str), c: &Contribution, version: i64, now: i64) -> Result<(), String> {
    conn.execute(
        "INSERT OR REPLACE INTO contribution
            (scope_key, entity_kind, entity_id, source_kind, source_id, first_ms, last_ms, nums, reasons, extra, version, updated_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            scope_key,
            kind,
            entity,
            source.0,
            source.1,
            c.first_ms,
            c.last_ms,
            serde_json::to_string(&c.nums).map_err(|e| e.to_string())?,
            serde_json::to_string(&c.reasons).map_err(|e| e.to_string())?,
            serde_json::to_string(&c.extra).map_err(|e| e.to_string())?,
            version,
            now
        ],
    )
    .map_err(db_err)?;
    Ok(())
}

/// Store an entity's complete rows, replacing what it had, and mark them built by `version`.
pub(crate) fn replace_entity(
    store: &UsageStore,
    scope_key: &str,
    kind: &str,
    entity: &str,
    rows: &[(Source, Contribution)],
    version: i64,
    now: i64,
) -> Result<(), String> {
    let tx = rusqlite::Transaction::new_unchecked(&store.conn, rusqlite::TransactionBehavior::Immediate).map_err(db_err)?;
    replace_entity_in(&tx, scope_key, kind, entity, rows, version, now)?;
    rusqlite::Transaction::commit(tx).map_err(db_err)
}

/// [`replace_entity`] inside the caller's transaction, so the caller can read and queue in the same one.
pub(crate) fn replace_entity_in(
    conn: &rusqlite::Connection,
    scope_key: &str,
    kind: &str,
    entity: &str,
    rows: &[(Source, Contribution)],
    version: i64,
    now: i64,
) -> Result<(), String> {
    conn.execute(
        "DELETE FROM contribution WHERE scope_key = ?1 AND entity_kind = ?2 AND entity_id = ?3",
        params![scope_key, kind, entity],
    )
    .map_err(db_err)?;
    for (source, c) in rows {
        put(conn, scope_key, kind, entity, (source.kind, &source.id), c, version, now)?;
    }
    put(conn, scope_key, kind, entity, (META, BUILT), &Contribution::new(), version, now)
}

/// The scopes in which each of `entities` has its rows built: one lookup per entity, on the
/// `contribution_by_entity` index (the primary key leads with the scope, so it cannot serve one).
fn built_scopes<'a>(
    conn: &rusqlite::Connection,
    kind: &str,
    version: i64,
    entities: impl Iterator<Item = &'a String>,
) -> Result<BTreeMap<String, Vec<String>>, String> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT scope_key FROM contribution WHERE entity_kind = ?1 AND entity_id = ?2 AND source_kind = ?3
               AND source_id = ?4 AND version = ?5",
        )
        .map_err(db_err)?;
    let mut out: BTreeMap<String, Vec<String>> = Default::default();
    for entity in entities {
        let rows = stmt
            .query_map(params![kind, entity, META, BUILT, version], |r| r.get::<_, String>(0))
            .map_err(db_err)?;
        for scope in rows {
            out.entry(entity.clone()).or_default().push(scope.map_err(db_err)?);
        }
    }
    Ok(out)
}

/// Forget that these entities' rows were built, in every scope: their next compute builds them
/// whole from the sources (for a change the queue cannot see, such as a worktree or a context
/// change that moves a session between entities).
pub(crate) fn unbuild(conn: &rusqlite::Connection, kind: &str, entities: &BTreeSet<String>) -> Result<(), String> {
    let mut stmt = conn
        .prepare_cached(
            "DELETE FROM contribution WHERE entity_kind = ?1 AND entity_id = ?2 AND source_kind = ?3 AND source_id = ?4",
        )
        .map_err(db_err)?;
    for entity in entities {
        stmt.execute(params![kind, entity, META, BUILT]).map_err(db_err)?;
    }
    Ok(())
}

/// [`unbuild`] for every entity of a kind.
#[cfg(test)]
pub(crate) fn unbuild_all(conn: &rusqlite::Connection, kind: &str) -> Result<(), String> {
    conn.execute(
        "DELETE FROM contribution WHERE entity_kind = ?1 AND source_kind = ?2 AND source_id = ?3",
        params![kind, META, BUILT],
    )
    .map_err(db_err)?;
    Ok(())
}

/// What one `apply` did.
#[derive(Debug, Default)]
pub(crate) struct Applied {
    /// The `(scope, entity)` pairs whose rows changed.
    pub changed: Vec<(String, String)>,
    /// Entities the source belongs to whose rows are not built yet (they are built whole when
    /// first computed, so the caller marks them for that).
    pub unbuilt: Vec<String>,
}

/// Apply one source's answer: in one transaction, upsert what changed, delete the rows of the
/// source that no entity has now (a reattribution is exactly that), run `then` (the caller's
/// marks, so a crash cannot keep the rows and lose the mark), and delete the queue row if it is
/// still the one that was read (an event queued since keeps it, and the next sweep resolves the
/// source again). Only entities whose rows are built are kept; the others are built whole when
/// first computed.
pub(crate) fn apply(
    then: &dyn Fn(&rusqlite::Connection, &Applied) -> Result<(), String>,
    store: &UsageStore,
    kind: &str,
    version: i64,
    queued: &Queued,
    source: &Source,
    wanted: &[(String, Contribution)],
    now: i64,
) -> Result<Applied, String> {
    let tx = rusqlite::Transaction::new_unchecked(&store.conn, rusqlite::TransactionBehavior::Immediate).map_err(db_err)?;
    let scopes = built_scopes(&tx, kind, version, wanted.iter().map(|(entity, _)| entity))?;
    let mut existing: BTreeMap<(String, String), Contribution> = Default::default();
    {
        let mut stmt = tx
            .prepare_cached(
                "SELECT scope_key, entity_id, first_ms, last_ms, nums, reasons, extra FROM contribution
                 WHERE entity_kind = ?1 AND source_kind = ?2 AND source_id = ?3",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![kind, source.kind, source.id], |r| {
                let scope: String = r.get(0)?;
                let entity: String = r.get(1)?;
                let shifted = decode_from(r, 2)?;
                Ok((scope, entity, shifted))
            })
            .map_err(db_err)?;
        for row in rows {
            let (scope, entity, decoded) = row.map_err(db_err)?;
            existing.insert((scope, entity), decoded?);
        }
    }
    let mut desired: BTreeMap<(String, String), &Contribution> = Default::default();
    for (entity, c) in wanted {
        if let Some(list) = scopes.get(entity) {
            for scope in list {
                desired.insert((scope.clone(), entity.clone()), c);
            }
        }
    }
    let mut changed = vec![];
    let unbuilt: Vec<String> = wanted
        .iter()
        .filter(|(entity, _)| !scopes.contains_key(entity))
        .map(|(entity, _)| entity.clone())
        .collect();
    for ((scope, entity), c) in &desired {
        if existing.get(&(scope.clone(), entity.clone())) != Some(*c) {
            put(&tx, scope, kind, entity, (source.kind, &source.id), c, version, now)?;
            changed.push((scope.clone(), entity.clone()));
        }
    }
    for ((scope, entity), _) in &existing {
        if !desired.contains_key(&(scope.clone(), entity.clone())) {
            tx.execute(
                "DELETE FROM contribution WHERE scope_key = ?1 AND entity_kind = ?2 AND entity_id = ?3
                   AND source_kind = ?4 AND source_id = ?5",
                params![scope, kind, entity, source.kind, source.id],
            )
            .map_err(db_err)?;
            changed.push((scope.clone(), entity.clone()));
        }
    }
    let applied = Applied { changed, unbuilt };
    then(&tx, &applied)?;
    tx.execute(
        "DELETE FROM source_event WHERE source_kind = ?1 AND source_id = ?2 AND enqueued_ms = ?3",
        params![queued.source.kind, queued.source.id, queued.enqueued_ms],
    )
    .map_err(db_err)?;
    rusqlite::Transaction::commit(tx).map_err(db_err)?;
    Ok(applied)
}

/// A stored row's contribution, read from `r` starting at column `at` (first, last, nums,
/// reasons, extra).
pub(crate) fn decode_from(r: &rusqlite::Row, at: usize) -> rusqlite::Result<Result<Contribution, String>> {
    let first: Option<i64> = r.get(at)?;
    let last: Option<i64> = r.get(at + 1)?;
    let (nums, reasons, extra): (String, String, String) = (r.get(at + 2)?, r.get(at + 3)?, r.get(at + 4)?);
    Ok((|| {
        Ok(Contribution {
            first_ms: first,
            last_ms: last,
            nums: serde_json::from_str(&nums).map_err(|e| e.to_string())?,
            reasons: serde_json::from_str(&reasons).map_err(|e| e.to_string())?,
            extra: serde_json::from_str(&extra).map_err(|e| e.to_string())?,
        })
    })())
}

/// The queued sources of the kinds a kind is fed by, oldest first, at most `limit` (other kinds'
/// rows are left alone and never take a place in the limit).
pub(crate) fn queued(store: &UsageStore, sources: &'static [&'static str], limit: i64) -> Result<Vec<Queued>, String> {
    queued_due(store, sources, limit, i64::MAX, false)
}

/// [`queued`] for the sources due at `now` (a failed source waits out its `not_before_ms`), the
/// newest first when `newest_first`.
pub(crate) fn queued_due(
    store: &UsageStore,
    sources: &'static [&'static str],
    limit: i64,
    now: i64,
    newest_first: bool,
) -> Result<Vec<Queued>, String> {
    let order = if newest_first { "enqueued_ms DESC, rowid DESC" } else { "enqueued_ms, rowid" };
    let mut stmt = store
        .conn
        .prepare_cached(&format!(
            "SELECT source_kind, source_id, enqueued_ms FROM source_event
             WHERE source_kind IN (SELECT value FROM json_each(?1)) AND COALESCE(not_before_ms, 0) <= ?3
             ORDER BY {order} LIMIT ?2"
        ))
        .map_err(db_err)?;
    let kinds = serde_json::to_string(sources).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![kinds, limit, now], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?)))
        .map_err(db_err)?;
    let mut out = vec![];
    for row in rows {
        let (kind, id, enqueued_ms) = row.map_err(db_err)?;
        if let Some(kind) = Iterator::find(&mut sources.iter(), |known| **known == kind) {
            out.push(Queued { source: Source { kind, id }, enqueued_ms });
        }
    }
    Ok(out)
}

/// Queue a source (or queue it again): its attempts and wait start over.
pub(crate) fn enqueue(conn: &rusqlite::Connection, source: &Source, now: i64) -> Result<(), String> {
    conn.execute(
        "INSERT INTO source_event (source_kind, source_id, enqueued_ms) VALUES (?1, ?2, ?3)
         ON CONFLICT (source_kind, source_id) DO UPDATE SET
            enqueued_ms = MAX(excluded.enqueued_ms, enqueued_ms + 1), attempts = 0, not_before_ms = NULL",
        params![source.kind, source.id, now],
    )
    .map_err(db_err)?;
    Ok(())
}

/// The wait after a source's `attempts`-th failure: `base_ms` doubled per earlier failure, capped.
pub(crate) fn backoff_ms(attempts: i64, base_ms: i64, cap_ms: i64) -> i64 {
    let doublings = u32::try_from(Ord::clamp(attempts - 1, 0, 30)).unwrap_or(30);
    Ord::min(base_ms.saturating_mul(1i64 << doublings), cap_ms)
}

/// A failed attempt at a queued source: count it and wait `backoff` (given the new count) before
/// it is due again. Returns the new count; nothing changes when the source was queued again since.
pub(crate) fn fail(
    store: &UsageStore,
    queued: &Queued,
    now: i64,
    backoff: &dyn Fn(i64) -> i64,
) -> Result<Option<i64>, String> {
    let tx = rusqlite::Transaction::new_unchecked(&store.conn, rusqlite::TransactionBehavior::Immediate).map_err(db_err)?;
    let attempts: Option<i64> = tx
        .query_row(
            "SELECT attempts FROM source_event WHERE source_kind = ?1 AND source_id = ?2 AND enqueued_ms = ?3",
            params![queued.source.kind, queued.source.id, queued.enqueued_ms],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_err)?;
    let Some(attempts) = attempts else {
        return Ok(None);
    };
    let attempts = attempts + 1;
    tx.execute(
        "UPDATE source_event SET attempts = ?4, not_before_ms = ?5
         WHERE source_kind = ?1 AND source_id = ?2 AND enqueued_ms = ?3",
        params![queued.source.kind, queued.source.id, queued.enqueued_ms, attempts, now + backoff(attempts)],
    )
    .map_err(db_err)?;
    rusqlite::Transaction::commit(tx).map_err(db_err)?;
    Ok(Some(attempts))
}

/// Fill what a source's existing rows hold, in every scope and entity that has them: `nums` and
/// `reasons` replace the row's, `extra_patch` is merged into its `extra`. It never inserts or
/// deletes a row (a rebuild that dropped the source between the count and the fill makes this a
/// no-op, and a fill cannot bring a row back). In one transaction it runs `then` (the caller's marks)
/// with the `(scope, entity)` pairs filled, and deletes the queue row if it is still the one read.
pub(crate) fn fill(
    then: &dyn Fn(&rusqlite::Connection, &[(String, String)]) -> Result<(), String>,
    store: &UsageStore,
    kind: &str,
    queued: &Queued,
    c: &Contribution,
    now: i64,
) -> Result<Vec<(String, String)>, String> {
    let source = &queued.source;
    let tx = rusqlite::Transaction::new_unchecked(&store.conn, rusqlite::TransactionBehavior::Immediate).map_err(db_err)?;
    let mut filled = vec![];
    {
        let mut stmt = tx
            .prepare_cached(
                "SELECT scope_key, entity_id, extra FROM contribution
                 WHERE entity_kind = ?1 AND source_kind = ?2 AND source_id = ?3",
            )
            .map_err(db_err)?;
        let found = stmt
            .query_map(params![kind, source.kind, source.id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
            .map_err(db_err)?;
        for row in found {
            filled.push(row.map_err(db_err)?);
        }
    }
    let nums = serde_json::to_string(&c.nums).map_err(|e| e.to_string())?;
    let reasons = serde_json::to_string(&c.reasons).map_err(|e| e.to_string())?;
    for (scope, entity, extra) in &filled {
        let mut extra: Map<String, Value> = serde_json::from_str(extra).map_err(|e| e.to_string())?;
        extra.extend(c.extra.clone());
        tx.execute(
            "UPDATE contribution SET nums = ?1, reasons = ?2, extra = ?3, updated_ms = ?4
             WHERE scope_key = ?5 AND entity_kind = ?6 AND entity_id = ?7 AND source_kind = ?8 AND source_id = ?9",
            params![nums, reasons, serde_json::to_string(&extra).map_err(|e| e.to_string())?, now, scope, kind, entity, source.kind, source.id],
        )
        .map_err(db_err)?;
    }
    let pairs: Vec<(String, String)> = filled.into_iter().map(|(scope, entity, _)| (scope, entity)).collect();
    then(&tx, &pairs)?;
    tx.execute(
        "DELETE FROM source_event WHERE source_kind = ?1 AND source_id = ?2 AND enqueued_ms = ?3",
        params![source.kind, source.id, queued.enqueued_ms],
    )
    .map_err(db_err)?;
    rusqlite::Transaction::commit(tx).map_err(db_err)?;
    Ok(pairs)
}

#[cfg(test)]
mod tests;
