// SPDX-License-Identifier: MIT
//! The story totals index: each story's `story_changes` totals, cached in the usage store and read
//! many at once (`storyTotals`). A read never walks git; the background worker computes a story
//! the first time it is asked for, and again when something it is made of changes.
//!
//! What marks a story changed:
//! - its sessions: every write to a session's usage, timing, record, story link or attribution
//!   queues the session, and the worker resolves its story with the same resolution
//!   `story_changes` uses (`report_with`), so a session tied by its working folder, its launch
//!   record or another machine counts too;
//! - its tasks and task links, and any price change (`model_rate`);
//! - git: a moved branch it followed or that names it, and a merge or squash that lands it on the
//!   default branch (by subject, or by a merge's second parent being one of its branch tips);
//! - context: a change to its story file, and a review record that names it.
//!
//! A session or task change recomputes only the session half; the git half is kept (with what it
//! was read from) and read again only after a git or context change, or when the sessions' branches,
//! worktrees or workspaces change.
use super::contribution::{self, EntityAdapter, SourceResolver};
use crate::context_query::index::Snapshot;
use crate::context_query::record::ContextRecord;
use super::{db_err, story_changes, UsageStore};
use crate::context_repo::caller_scope::CallerScope;
use rmcp::schemars::JsonSchema;
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Params {
    /// Story ids, for example DEMO-29; 1 to 500.
    pub story_ids: Vec<String>,
}

/// The most ids one read takes (MCP and the bridge; a hosted ask is bounded by its 2 KB args).
pub const MAX_IDS: usize = 500;
/// The stories and reasons one answer holds, as JSON bytes; the rest of the ids are listed in
/// `rest`, so a hosted answer stays under its 256 KB.
const ANSWER_BUDGET: usize = 240 * 1024;
/// A changed story waits this long without a further change before it is recomputed ...
const SETTLE_MS: i64 = 3_000;
/// ... and is recomputed at most this often, however often it changes.
const MIN_REFRESH_MS: i64 = 30_000;
/// A failed compute is tried again after this.
const RETRY_MS: i64 = 300_000;
/// Changed stories offered per turn; the worker computes one story at a time.
const WORKERS: usize = 1;
/// The least the worker rests after each story it computes, so the index never queues behind
/// itself ...
const REST_GAP: Duration = Duration::from_secs(1);
/// ... and the rest is at least this many times the compute it follows, so a backlog keeps the
/// worker at or under 1 / (1 + REST_FACTOR) of a core, however long computes take.
const REST_FACTOR: u32 = 2;
/// The most computes that share one usage report (`story_changes::Batch`), and the longest it is
/// kept.
const BATCH: usize = 16;
const BATCH_AGE: Duration = Duration::from_secs(120);
/// A timed-out compute that still runs this long after its timeout is given up on: its slot is
/// freed and its story waits out `ABANDON_BACKOFF_MS`.
const ABANDON_AFTER: Duration = Duration::from_secs(600);
const ABANDON_BACKOFF_MS: i64 = 21_600_000;
/// Abandoned computes (still running) per home beyond which nothing starts.
const MAX_ABANDONED: usize = 2;
/// How long one story's whole compute may take; past it the story is an error row, retried later.
const COMPUTE_TIMEOUT: Duration = Duration::from_secs(90);
/// How often the watched repositories' refs are read.
const REF_POLL: Duration = Duration::from_secs(15);
/// How often the changed sessions are resolved to their stories.
const SESSION_SWEEP: Duration = Duration::from_secs(5);
/// The most changed sessions one sweep resolves; the rest wait for the next.
const SESSION_SWEEP_LIMIT: i64 = 200;
/// The least counting one commit sweep does before it stops; it counts as long as the last
/// compute ran if that was longer (`sweep_spend`). Its rest is the compute worker's (`rest`), so
/// counting and computing share one budget of a third of a core.
const COMMIT_SPEND: Duration = Duration::from_secs(2);
/// The longest the sweep waits to look again when it found nothing due.
const COMMIT_SWEEP: Duration = Duration::from_secs(5);
/// The worker's idle tick, for changes that come in with no wake.
const IDLE_TICK: Duration = Duration::from_secs(2);
/// A story is marked for a moved notes ref at most this often; a move inside it is kept pending.
const NOTES_COOLDOWN_MS: i64 = 300_000;
/// A moved notes ref marks only stories that really changed (a commit, session or task) this recently.
pub(super) const NOTES_ACTIVE_MS: i64 = 1_800_000;
/// The most first-parent commits read for one move of the default branch; past it the move is
/// logged and marks the repository's open stories, once.
const LANDED_SCAN: usize = 2_000;
/// A row nobody has asked for in this long is dropped (it is computed again if asked).
const FORGET_MS: i64 = 30 * 86_400_000;
/// A read refreshes a row's last-asked time at most this often.
const ASKED_EVERY_MS: i64 = 86_400_000;
/// The pseudo-ref that holds the default branch's tip (`origin/main`).
const DEFAULT_REF: &str = "@default";
/// Unix epoch milliseconds, in SQL.
const NOW_SQL: &str = "CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)";

/// An additive table (no schema version), like `session_story`.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS story_totals (
    scope_key      TEXT NOT NULL,
    story_key      TEXT NOT NULL,
    story_id       TEXT NOT NULL,
    scope_kind     TEXT NOT NULL,
    scope_root     TEXT,
    totals         TEXT,
    unavailable    TEXT,
    error          TEXT,
    git            TEXT,
    facets         TEXT,
    repos          TEXT NOT NULL DEFAULT '[]',
    branches       TEXT NOT NULL DEFAULT '[]',
    version        INTEGER,
    computed_at_ms INTEGER,
    dirty_since_ms INTEGER,
    dirty_at_ms    INTEGER,
    git_dirty_ms   INTEGER,
    changed_ms     INTEGER,
    asked_at_ms    INTEGER NOT NULL,
    PRIMARY KEY (scope_key, story_key)
);
CREATE INDEX IF NOT EXISTS story_totals_by_story ON story_totals (story_key);
-- The indexed files that name each story, as the totals last settled on them (see `story_files`):
-- the file's home-relative path, its mtime, and a hash of its content (NULL when it could not be
-- read), so a touched mtime over the same content is not a change.
CREATE TABLE IF NOT EXISTS story_totals_ctx (
    story_key TEXT NOT NULL,
    path      TEXT NOT NULL,
    mtime_ms  INTEGER NOT NULL,
    content   INTEGER,
    PRIMARY KEY (story_key, path)
);
CREATE INDEX IF NOT EXISTS story_totals_ctx_by_path ON story_totals_ctx (path);
-- One-shot markers. `ctx_rebuild` is set when the files table above is rebuilt from an older form,
-- and cleared by the first settle of the files that follows.
CREATE TABLE IF NOT EXISTS story_totals_flag (name TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS story_totals_ref (
    repo TEXT NOT NULL,
    name TEXT NOT NULL,
    oid  TEXT NOT NULL,
    PRIMARY KEY (repo, name)
);
";

/// The `SET` part that marks a story changed at `$NOW`; the caller adds which stories. `changed_ms`
/// is when it last really changed (a session, task, commit or merge), which a notes mark leaves.
const MARK_SQL: &str = "UPDATE story_totals SET dirty_since_ms = MIN(COALESCE(dirty_since_ms, $NOW), $NOW), dirty_at_ms = $NOW, changed_ms = $NOW";
/// A mark for a story that did not change itself (a new version, a price change, a capped change):
/// it recomputes, but `changed_ms` stays, so it is not taken for recent activity.
const QUIET_MARK_SQL: &str = "UPDATE story_totals SET dirty_since_ms = MIN(COALESCE(dirty_since_ms, $NOW), $NOW), dirty_at_ms = $NOW";

/// Queues a session for the worker to resolve to its stories. It never conflicts (`NOT EXISTS`, not
/// `OR IGNORE`): a trigger statement takes the outer statement's conflict policy, so under a writer's
/// upsert an `OR IGNORE` insert of a queued session would abort that writer.
/// An event for a session already queued moves its time forward (by at least 1 ms), so a sweep that
/// read the row earlier cannot delete it: the event is seen by the next sweep.
const QUEUE_SQL: &str = "UPDATE source_event SET enqueued_ms = MAX($NOW, enqueued_ms + 1) WHERE source_kind = 'session' AND source_id = $ID; \
     INSERT INTO source_event (source_kind, source_id, enqueued_ms) SELECT 'session', $ID, $NOW \
     WHERE $ID IS NOT NULL AND NOT EXISTS (SELECT 1 FROM source_event WHERE source_kind = 'session' AND source_id = $ID);";

/// The writers whose rows a story's totals are made of, each with what its trigger does: mark
/// stories it names (`mark`), queue sessions to resolve (`queue`), or mark every story (`all`).
enum Effect {
    Mark(&'static str),
    Queue(&'static str),
    MarkAndQueue(&'static str, &'static str),
    All,
}

const TRIGGERS: &[(&str, &str, Effect)] = &[
    ("task_insert", "AFTER INSERT ON task", Effect::Mark("lower(NEW.story_id)")),
    ("task_update", "AFTER UPDATE ON task", Effect::Mark("lower(NEW.story_id), lower(OLD.story_id)")),
    (
        "task_link_insert",
        "AFTER INSERT ON task_link",
        Effect::Mark("SELECT lower(story_id) FROM task WHERE task_id IN (NEW.task_id, NEW.target) AND story_id IS NOT NULL"),
    ),
    ("session_story_insert", "AFTER INSERT ON session_story", Effect::MarkAndQueue("lower(NEW.story_id)", "NEW.session_id")),
    (
        "session_story_update",
        "AFTER UPDATE ON session_story",
        Effect::MarkAndQueue("lower(NEW.story_id), lower(OLD.story_id)", "NEW.session_id"),
    ),
    ("session_record_insert", "AFTER INSERT ON session_record", Effect::MarkAndQueue("lower(NEW.story_id)", "NEW.session_id")),
    (
        "session_record_update",
        "AFTER UPDATE ON session_record",
        Effect::MarkAndQueue("lower(NEW.story_id), lower(OLD.story_id)", "NEW.session_id"),
    ),
    ("usage_event_insert", "AFTER INSERT ON usage_event", Effect::Queue("NEW.session_id")),
    // A usage row that changes (a repricing, a late cost, an upsert that moves it to another
    // session): both the session it was and the one it is queue, so each rewrites its row.
    ("usage_event_update", "AFTER UPDATE ON usage_event", Effect::Queue("NEW.session_id")),
    ("usage_event_update_old", "AFTER UPDATE ON usage_event", Effect::Queue("OLD.session_id")),
    ("session_turn_insert", "AFTER INSERT ON session_turn", Effect::Queue("NEW.session_id")),
    ("session_turn_update", "AFTER UPDATE ON session_turn", Effect::Queue("NEW.session_id")),
    ("session_end_insert", "AFTER INSERT ON session_end", Effect::Queue("NEW.session_id")),
    ("session_end_update", "AFTER UPDATE ON session_end", Effect::Queue("NEW.session_id")),
    ("session_attribution_insert", "AFTER INSERT ON session_attribution", Effect::Queue("NEW.session_id")),
    ("session_attribution_update", "AFTER UPDATE ON session_attribution", Effect::Queue("NEW.session_id")),
    ("session_attribution_launch_insert", "AFTER INSERT ON session_attribution", Effect::Queue("NEW.launch_id")),
    ("session_attribution_launch_update", "AFTER UPDATE ON session_attribution", Effect::Queue("NEW.launch_id")),
    ("model_rate_insert", "AFTER INSERT ON model_rate", Effect::All),
    ("model_rate_update", "AFTER UPDATE ON model_rate", Effect::All),
    ("model_rate_delete", "AFTER DELETE ON model_rate", Effect::All),
];

fn trigger_body(effect: &Effect) -> String {
    let mark = |stories: &str| {
        format!("{} WHERE story_key IN ({stories});", str::replace(MARK_SQL, "$NOW", NOW_SQL))
    };
    let queue = |id: &str| str::replace(&str::replace(QUEUE_SQL, "$NOW", NOW_SQL), "$ID", id);
    match effect {
        Effect::Mark(stories) => mark(stories),
        Effect::Queue(id) => queue(id),
        Effect::MarkAndQueue(stories, id) => format!("{} {}", mark(stories), queue(id)),
        Effect::All => format!("{};", str::replace(QUIET_MARK_SQL, "$NOW", NOW_SQL)),
    }
}

pub(super) fn init(conn: &rusqlite::Connection) -> Result<(), String> {
    // The files table once held a hash of the absolute path and mtime; it is rebuilt by the next
    // start, which settles every story once.
    let old_ctx: bool = db(conn.query_row(
        "SELECT count(*) > 0 FROM pragma_table_info('story_totals_ctx') WHERE name = 'hash'",
        [],
        |r| rusqlite::Row::get(r, 0),
    ))?;
    if old_ctx {
        db(conn.execute_batch("DROP TABLE story_totals_ctx;"))?;
    }
    conn.execute_batch(SCHEMA).map_err(db_err)?;
    if old_ctx {
        db(conn.execute_batch("INSERT OR IGNORE INTO story_totals_flag (name) VALUES ('ctx_rebuild');"))?;
    }
    conn.execute_batch(contribution::SCHEMA).map_err(db_err)?;
    // The queue gained its retry columns after it was first made.
    for (column, definition) in [("attempts", "INTEGER NOT NULL DEFAULT 0"), ("not_before_ms", "INTEGER")] {
        let present: bool = db(conn.query_row(
            "SELECT count(*) > 0 FROM pragma_table_info('source_event') WHERE name = ?1",
            params![column],
            |r| rusqlite::Row::get(r, 0),
        ))?;
        if !present {
            db(conn.execute_batch(&format!("ALTER TABLE source_event ADD COLUMN {column} {definition};")))?;
        }
    }
    // The session queue was `story_totals_session`; it is `source_event` now. Copy, then drop: run
    // in one transaction by `open`, and safe to run again (the copy ignores what is there, the
    // drop ignores what is gone), so an interrupted upgrade finishes on the next open.
    let old_queue: bool = db(conn.query_row(
        "SELECT count(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = 'story_totals_session'",
        [],
        |r| rusqlite::Row::get(r, 0),
    ))?;
    if old_queue {
        db(conn.execute_batch(&format!(
            "INSERT OR IGNORE INTO source_event (source_kind, source_id, enqueued_ms)
                 SELECT 'session', session_id, {NOW_SQL} FROM story_totals_session;
             DROP TABLE IF EXISTS story_totals_session;"
        )))?;
    }
    let has_changed: bool = db(conn.query_row(
        "SELECT count(*) > 0 FROM pragma_table_info('story_totals') WHERE name = 'changed_ms'",
        [],
        |r| rusqlite::Row::get(r, 0),
    ))?;
    if !has_changed {
        db(conn.execute_batch("ALTER TABLE story_totals ADD COLUMN changed_ms INTEGER;"))?;
    }
    // Replaced, never kept: a store keeps no trigger from an earlier build. One already as this
    // build writes it is left alone, so opening the store to read it changes nothing.
    for (name, event, effect) in TRIGGERS {
        let create = format!("CREATE TRIGGER story_totals_{name} {event} BEGIN {} END", trigger_body(effect));
        let stored: Option<String> = db(OptionalExtension::optional(conn.query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = ?1",
            params![format!("story_totals_{name}")],
            |r| rusqlite::Row::get(r, 0),
        )))?;
        if Option::as_deref(&stored) != Some(String::as_str(&create)) {
            db(conn.execute_batch(&format!("DROP TRIGGER IF EXISTS story_totals_{name}; {create};")))?;
        }
    }
    Ok(())
}

/// The app, for publishing recomputed stories on the live hub (unset in tests and headless runs).
static APP: OnceLock<tauri::AppHandle> = OnceLock::new();

/// The live-hub view a scope's rows belong to: its team's (`team:<org>/<team>`, as team pages
/// subscribe), or none for a scope that is no team's (only unrestricted views receive those).
fn view_scope(scope: &CallerScope) -> Option<String> {
    match scope {
        CallerScope::Team(view) => Some(crate::live_hub::view_key::team_key(crate::context_repo::inheritance::TeamView::org(view), view.reader_team())),
        CallerScope::Human | CallerScope::Unknown => None,
    }
}

/// A recomputed story as a `storyTotals` row with its view: `id` is unique per view
/// (`<view>|<story>`, so two teams' rows for one story never replace each other), `story` is the
/// story key and `computedAtMs` when it was computed.
fn recomputed_row(view: Option<String>, story: &str, computed_at: i64) -> (Option<String>, Value) {
    let id = format!("{}|{story}", Option::unwrap_or(view.as_deref(), "all"));
    (view, json!({"id": id, "story": story, "computedAtMs": computed_at}))
}

/// Publish a recomputed story on the `storyTotals` live-hub topic, to the views of the scope it was
/// computed in. The worker computes one story at a time with a rest between, so there is no burst
/// to coalesce here.
fn publish_recomputed(job: &Job, story: &str, computed_at: i64) {
    if let Some(app) = APP.get() {
        publish_to(app, job, story, computed_at);
    }
}

/// [`publish_recomputed`] to a given app (the worker's is the one `install` was given).
fn publish_to<R: tauri::Runtime>(app: &tauri::AppHandle<R>, job: &Job, story: &str, computed_at: i64) {
    if let Some(scope) = scope_for(&job.scope_key, &job.kind, job.root.as_deref()) {
        crate::live_hub::story_totals_changed(app, vec![recomputed_row(view_scope(&scope), story, computed_at)]);
    }
}

/// A mutex's guard, whether or not a holder panicked.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// A rusqlite result as this module's string error.
fn db<T>(result: rusqlite::Result<T>) -> Result<T, String> {
    Result::map_err(result, db_err)
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// What a row records of the caller's scope, so the worker can compute in it again.
struct ScopeRow {
    key: String,
    kind: &'static str,
    root: Option<String>,
}

fn describe(scope: &CallerScope) -> ScopeRow {
    let key = scope.cache_key().to_string();
    let (kind, root) = match scope {
        CallerScope::Human => ("human", None),
        CallerScope::Unknown => ("unknown", None),
        CallerScope::Team(view) => (
            "team",
            view.own_content()
                .own_path()
                .map(|path| path.to_string_lossy().into_owned()),
        ),
    };
    ScopeRow { key, kind, root }
}

/// The scopes asked in this run, by key: the worker computes in the caller's own scope.
fn scopes() -> &'static Mutex<HashMap<String, CallerScope>> {
    static SCOPES: OnceLock<Mutex<HashMap<String, CallerScope>>> = OnceLock::new();
    SCOPES.get_or_init(Default::default)
}

/// The scope a row was asked in: as asked in this run, else rebuilt from its team content root
/// (after a restart). `None` when that scope is gone.
fn scope_for(key: &str, kind: &str, root: Option<&str>) -> Option<CallerScope> {
    if let Some(scope) = HashMap::get(&scopes().lock().unwrap_or_else(|e| e.into_inner()), key) {
        return Some(scope.clone());
    }
    let scope = match kind {
        "human" => CallerScope::Human,
        "unknown" => CallerScope::Unknown,
        _ => crate::context_repo::caller_scope::for_content(Path::new(root?))?,
    };
    (scope.cache_key().to_string() == key).then_some(scope)
}

fn waker() -> &'static tokio::sync::Notify {
    static WAKE: OnceLock<tokio::sync::Notify> = OnceLock::new();
    WAKE.get_or_init(tokio::sync::Notify::new)
}

/// Tell an idle worker there may be work (something changed).
fn wake() {
    waker().notify_one();
}

fn ask_waker() -> &'static tokio::sync::Notify {
    static ASKED: OnceLock<tokio::sync::Notify> = OnceLock::new();
    ASKED.get_or_init(tokio::sync::Notify::new)
}

/// A read queued a story: wake an idle worker, and cut the extra rest of a resting one.
fn wake_ask() {
    wake();
    ask_waker().notify_one();
}

/// The ids asked for, once each (case-insensitively), in order.
fn asked(ids: &[String]) -> Result<Vec<String>, String> {
    if <[String]>::is_empty(ids) || <[String]>::len(ids) > MAX_IDS {
        return Err(format!("invalid: storyIds takes 1 to {MAX_IDS} ids"));
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for id in ids {
        if str::is_empty(str::trim(id)) {
            return Err("invalid: storyIds must not hold a blank id".into());
        }
        if BTreeSet::insert(&mut seen, id.trim().to_lowercase()) {
            out.push(id.clone());
        }
    }
    Ok(out)
}

/// The batch read for MCP and the bridge.
pub async fn read(home: &Path, params: &Params, scope: &CallerScope) -> Result<Value, String> {
    let (home, ids, scope) = (home.to_owned(), params.story_ids.clone(), scope.clone());
    crate::commands::await_blocking("story_totals", move || read_blocking(&home, &ids, &scope, false))
        .await
}

pub async fn current(params: &Params, scope: &CallerScope) -> Result<Value, String> {
    let home = crate::commands::paths::codesense_home_dir().ok_or("no CodeSense home directory")?;
    read(&home, params, scope).await
}

/// The read itself, on a blocking thread. `published` keeps only what a hosted viewer may see.
pub(crate) fn read_blocking(
    home: &Path,
    ids: &[String],
    scope: &CallerScope,
    published: bool,
) -> Result<Value, String> {
    let ids = asked(ids)?;
    let store = UsageStore::open(&home.join("usage.sqlite"))?;
    let (answer, queued) = answer(&store, &ids, scope, now_ms(), published)?;
    // A board re-asking stories it already asked changes no row, so it wakes nothing.
    if queued {
        wake_ask();
    }
    Ok(answer)
}

/// The `activityReason` of a story whose row was computed before activity times were kept.
const ACTIVITY_REFRESHING: &str = "refreshing";

/// The `unavailable` reasons `storyChanges` publishes; the others stay on the Mac.
pub(crate) const PUBLISHED_REASONS: &[&str] = &[
    "actual",
    "attribution",
    "branches",
    "commits",
    "costUsd",
    "files",
    "historyFetch",
    "lines",
    "planned",
    // SV-CDE-82: a merge whose tip another story's branch shares.
    "prs",
    "ranges",
    "reviewRounds",
    "reviews",
    "reviewTasks",
];

fn answer(
    store: &UsageStore,
    ids: &[String],
    scope: &CallerScope,
    now: i64,
    published: bool,
) -> Result<(Value, bool), String> {
    let row = describe(scope);
    HashMap::insert(
        &mut scopes().lock().unwrap_or_else(|e| e.into_inner()),
        row.key.clone(),
        scope.clone(),
    );
    // Deferred: a read writes only a story not indexed yet, or one last asked a day ago.
    let tx = rusqlite::Transaction::new_unchecked(
        &store.conn,
        rusqlite::TransactionBehavior::Deferred,
    )
    .map_err(db_err)?;
    // Whether the read added a story to the queue or asked an older one again.
    let mut queued = false;
    {
        let mut queue = tx
            .prepare_cached(
                "INSERT INTO story_totals (scope_key, story_key, story_id, scope_kind, scope_root, asked_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (scope_key, story_key) DO UPDATE SET asked_at_ms = excluded.asked_at_ms
                 WHERE asked_at_ms < excluded.asked_at_ms - ?7",
            )
            .map_err(db_err)?;
        for id in ids {
            let changed = rusqlite::Statement::execute(
                &mut queue,
                params![
                    row.key,
                    id.trim().to_lowercase(),
                    id.trim(),
                    row.kind,
                    row.root,
                    now,
                    ASKED_EVERY_MS
                ],
            )
            .map_err(db_err)?;
            queued |= changed > 0;
        }
    }
    rusqlite::Transaction::commit(tx).map_err(db_err)?;
    let mut select = store
        .conn
        .prepare_cached(
            "SELECT totals, unavailable, error, computed_at_ms,
                    (dirty_since_ms IS NOT NULL AND dirty_since_ms <= ?4) OR version IS NOT ?3, facets
             FROM story_totals WHERE scope_key = ?1 AND story_key = ?2",
        )
        .map_err(db_err)?;
    let mut stories = Map::new();
    let mut pending = Vec::new();
    let mut rest = Vec::new();
    let mut reasons: Vec<String> = Vec::new();
    let mut reason_index = HashMap::<String, usize>::new();
    let mut used = 0;
    // A managed-settings install or removal shows on this read, not on the story's next fold.
    let redirected = crate::session_usage::managed_telemetry::usage_reason().is_some();
    for id in ids {
        if !Vec::is_empty(&rest) {
            rest.push(id.clone());
            continue;
        }
        let found = select
            .query_row(
                params![row.key, id.trim().to_lowercase(), story_changes::TOTALS_VERSION, now],
                |r| {
                    Ok((
                        rusqlite::Row::get::<_, Option<String>>(r, 0)?,
                        rusqlite::Row::get::<_, Option<String>>(r, 1)?,
                        rusqlite::Row::get::<_, Option<String>>(r, 2)?,
                        rusqlite::Row::get::<_, Option<i64>>(r, 3)?,
                        rusqlite::Row::get::<_, bool>(r, 4)?,
                        rusqlite::Row::get::<_, Option<String>>(r, 5)?,
                    ))
                },
            )
            .optional()
            .map_err(db_err)?;
        let entry = match found {
            Some((Some(totals), unavailable, _, computed, refreshing, facets)) => {
                let totals: Value = serde_json::from_str(&totals).map_err(|e| e.to_string())?;
                let unavailable: Map<String, Value> =
                    serde_json::from_str(unavailable.as_deref().unwrap_or("{}"))
                        .map_err(|e| e.to_string())?;
                let mut coded = Map::new();
                let mut added = Vec::new();
                for (field, reason) in unavailable {
                    let Some(reason) = Value::as_str(&reason) else {
                        continue;
                    };
                    let reason = if field == "costUsd" {
                        story_changes::current_cost_reason(reason, redirected)
                    } else {
                        reason
                    };
                    if published && !<[&str]>::contains(PUBLISHED_REASONS, &&*field) {
                        continue;
                    }
                    let index = *reason_index.entry(reason.to_owned()).or_insert_with(|| {
                        reasons.push(reason.to_owned());
                        added.push(str::len(reason) + 3);
                        Vec::len(&reasons) - 1
                    });
                    Map::insert(&mut coded, field, json!(index));
                }
                let mut entry =
                    json!({"totals": totals, "unavailable": coded, "computedAtMs": computed});
                if refreshing {
                    entry["refreshing"] = json!(true);
                }
                // mergedAt (or mergedAtReason), prs, prsTotal, runtimes and the activity times
                // (firstActivityAt, lastActivityAt, openSessions): published facts.
                let mut facets: Map<String, Value> =
                    serde_json::from_str(facets.as_deref().unwrap_or("{}"))
                        .map_err(|e| e.to_string())?;
                // A row computed before the activity times existed keeps its totals and says
                // they are coming; the recompute is the worker's, never this read's.
                if !Map::contains_key(&facets, "openSessions") {
                    for field in ["firstActivityAt", "lastActivityAt", "openSessions"] {
                        Map::insert(&mut facets, field.into(), Value::Null);
                    }
                    Map::insert(&mut facets, "activityReason".into(), json!(ACTIVITY_REFRESHING));
                }
                // Lines still being counted: the totals show the earlier ones, refreshing.
                if Map::remove(&mut facets, "linesRefreshing") == Some(json!(true)) {
                    entry["refreshing"] = json!(true);
                }
                for (key, value) in facets {
                    entry[key] = value;
                }
                used += added.iter().sum::<usize>();
                entry
            }
            Some((None, _, Some(error), computed, _, _)) => {
                let error = if published { "failed" } else { &*error };
                json!({"error": error, "computedAtMs": computed})
            }
            _ => json!({"pending": true}),
        };
        used += str::len(id) + 4 + serde_json::to_vec(&entry).map_or(0, |bytes| Vec::len(&bytes));
        if used > ANSWER_BUDGET && !Map::is_empty(&stories) {
            // Its new reasons stay listed: they are short, and nothing refers to them.
            rest.push(id.clone());
            continue;
        }
        if Value::get(&entry, "pending").is_some() {
            pending.push(id.clone());
        }
        Map::insert(&mut stories, id.clone(), entry);
    }
    let (total, done): (i64, i64) = store
        .conn
        .query_row(
            "SELECT count(*), coalesce(sum(totals IS NOT NULL OR error IS NOT NULL), 0)
             FROM story_totals WHERE scope_key = ?1",
            params![row.key],
            |r| Ok((rusqlite::Row::get(r, 0)?, rusqlite::Row::get(r, 1)?)),
        )
        .map_err(db_err)?;
    let mut answer = json!({
        "stories": stories,
        "reasons": reasons,
        "pending": pending,
        "indexing": {"done": done, "total": total},
    });
    if !Vec::is_empty(&rest) {
        answer["truncated"] = json!(true);
        answer["rest"] = json!(rest);
    }
    Ok((answer, queued))
}

/// One story to compute, in the scope it was asked in, with its kept git half when that is still
/// good (no git change since, and computed by this version).
#[derive(Debug, Clone, PartialEq)]
struct Job {
    scope_key: String,
    story_id: String,
    kind: String,
    root: Option<String>,
    git: Option<String>,
    /// The stored git half whether or not it may be reused: its lines stand while a changed git
    /// half is counted.
    earlier_git: Option<String>,
    /// When the story was last marked changed.
    dirty_at_ms: Option<i64>,
}

/// The reason an earlier build stored for a story whose lines it gave up on. Such a row is walked
/// first by the backfill that counts every story's commits; once recomputed it no longer has it.
const UNCOUNTED_FIRST: &str = "too large to compute in the background";

/// A never-computed story (the backfill).
const COLD_SQL: &str = "totals IS NULL AND error IS NULL AND dirty_since_ms IS NULL";
/// A story with a settled change, due again.
const CHANGED_SQL: &str = "dirty_since_ms IS NOT NULL AND dirty_since_ms <= ?1 AND (computed_at_ms IS NULL OR computed_at_ms <= ?2)";

/// The stories matching `due`: never-computed ones first, then really changed ones (live work never
/// waits behind the backfill), then those holding the old "too large" reason (the backfill counts
/// them before the other quiet rows), then most recently asked first.
fn select_jobs(store: &UsageStore, due: &str, now: i64, limit: usize) -> Result<Vec<Job>, String> {
    let mut stmt = store
        .conn
        .prepare_cached(&format!(
            "SELECT scope_key, story_id, scope_kind, scope_root,
                    CASE WHEN git_dirty_ms IS NULL AND version BETWEEN ?5 AND ?4 THEN git END, dirty_at_ms, git
             FROM story_totals
             WHERE {due}
             ORDER BY totals IS NOT NULL, (changed_ms IS NULL OR changed_ms < dirty_at_ms), instr(COALESCE(git, ''), '{UNCOUNTED_FIRST}') = 0, asked_at_ms DESC
             LIMIT ?3"
        ))
        .map_err(db_err)?;
    let rows = stmt
        .query_map(
            params![
                now - SETTLE_MS,
                now - MIN_REFRESH_MS,
                limit as i64,
                story_changes::TOTALS_VERSION,
                story_changes::git_kept_since(story_changes::TOTALS_STEPS)
            ],
            |r| {
                Ok(Job {
                    scope_key: rusqlite::Row::get(r, 0)?,
                    story_id: rusqlite::Row::get(r, 1)?,
                    kind: rusqlite::Row::get(r, 2)?,
                    root: rusqlite::Row::get(r, 3)?,
                    git: rusqlite::Row::get(r, 4)?,
                    dirty_at_ms: rusqlite::Row::get(r, 5)?,
                    earlier_git: rusqlite::Row::get(r, 6)?,
                })
            },
        )
        .map_err(db_err)?;
    rows.collect::<Result<_, _>>().map_err(db_err)
}

/// Everything due, never-computed ones first.
#[cfg(test)]
fn next_jobs(store: &UsageStore, now: i64, limit: usize) -> Result<Vec<Job>, String> {
    select_jobs(store, &format!("({COLD_SQL}) OR ({CHANGED_SQL})"), now, limit)
}

/// What is due: a never-computed story and a changed one; the worker takes one at a time.
struct Due {
    cold: Vec<Job>,
    changed: Vec<Job>,
}

/// Nothing is due while a compute of this home is still running (a timed-out one included): the
/// worker never has more than one compute going, so the CPU bound holds.
fn due(store: &UsageStore, home: &Path, now: i64) -> Result<Due, String> {
    if busy(home) {
        return Ok(Due { cold: Vec::new(), changed: Vec::new() });
    }
    Ok(Due {
        cold: select_jobs(store, COLD_SQL, now, 1)?,
        changed: select_jobs(store, CHANGED_SQL, now, WORKERS)?,
    })
}

type Computed = Result<
    (
        story_changes::Totals,
        BTreeMap<String, String>,
        Value,
        story_changes::GitTotals,
    ),
    String,
>;

/// A late result: `(started, own_mark_a, own_mark_b)`. It is not stored over a real change made after
/// `started`, and it clears only the marks the timeout itself set (the retry mark and the abandon
/// backoff): a newer mark from a note or ref move keeps the row due.
type Late = (i64, i64, i64);

/// Store one compute. A change marked after `started` keeps the row changed.
fn store_result(
    store: &UsageStore,
    job: &Job,
    started: i64,
    now: i64,
    result: Computed,
    late: Option<Late>,
) -> Result<(), String> {
    let key = job.story_id.to_lowercase();
    match result {
        Ok((totals, unavailable, facets, git)) => {
            let paths: Vec<String> = git
                .repos
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            let stored = store
                .conn
                .execute(
                    "UPDATE story_totals SET totals = ?1, unavailable = ?2, git = ?3, repos = ?4,
                        branches = ?5, version = ?6, error = NULL, computed_at_ms = ?7,
                        dirty_since_ms = CASE
                            WHEN ?12 IS NOT NULL THEN CASE WHEN dirty_since_ms IN (?13, ?14) THEN NULL ELSE dirty_since_ms END
                            WHEN dirty_at_ms > ?8 THEN dirty_at_ms ELSE NULL END,
                        dirty_at_ms = CASE WHEN ?12 IS NOT NULL AND dirty_at_ms IN (?13, ?14) THEN NULL ELSE dirty_at_ms END,
                        git_dirty_ms = CASE WHEN git_dirty_ms > ?8 THEN git_dirty_ms ELSE NULL END,
                        facets = ?11
                     WHERE scope_key = ?9 AND story_key = ?10
                       AND (?12 IS NULL OR changed_ms IS NULL OR changed_ms <= ?12)",
                    params![
                        to_json(&totals)?,
                        to_json(&unavailable)?,
                        to_json(&git)?,
                        to_json(&paths)?,
                        to_json(&git.branches)?,
                        story_changes::TOTALS_VERSION,
                        now,
                        started,
                        job.scope_key,
                        key,
                        to_json(&facets)?,
                        Option::map(late, |late| late.0),
                        Option::map(late, |late| late.1),
                        Option::map(late, |late| late.2)
                    ],
                )
                .map_err(db_err)?;
            // Only a compute that was stored is news: a late result a real change superseded is not.
            if stored > 0 {
                publish_recomputed(job, &key, now);
            }
        }
        Err(error) => {
            log::info!(target: "story_totals", "{}: {error}", job.story_id);
            // Earlier totals stay (shown as refreshing); a story never computed shows the error.
            store
                .conn
                .execute(
                    "UPDATE story_totals SET error = CASE WHEN totals IS NULL THEN ?1 ELSE NULL END,
                        computed_at_ms = ?2, dirty_since_ms = ?3, dirty_at_ms = ?3
                     WHERE scope_key = ?4 AND story_key = ?5",
                    params![error, now, now + RETRY_MS, job.scope_key, key],
                )
                .map_err(db_err)?;
        }
    }
    Ok(())
}

/// JSON text of a value, for one column.
fn to_json<T: serde::Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|e| e.to_string())
}

/// The compute slots. One compute runs per home. A timed-out compute is not stopped, so it keeps
/// its slot until its task returns; if that takes past `ABANDON_AFTER` the slot is released
/// (the task is then "gone", still running), and at `MAX_ABANDONED` gone tasks nothing starts.
#[derive(Default)]
struct Slots {
    running: HashMap<PathBuf, u64>,
    gone: HashMap<u64, PathBuf>,
    next: u64,
}

fn slots() -> &'static Mutex<Slots> {
    static SLOTS: OnceLock<Mutex<Slots>> = OnceLock::new();
    SLOTS.get_or_init(Default::default)
}

fn gone_in(slots: &Slots, home: &Path) -> usize {
    Iterator::count(Iterator::filter(HashMap::values(&slots.gone), |gone| PathBuf::as_path(gone) == home))
}

/// True while a compute of this home holds its slot, or too many abandoned ones still run.
fn busy(home: &Path) -> bool {
    let slots = lock(slots());
    HashMap::contains_key(&slots.running, home) || gone_in(&slots, home) >= MAX_ABANDONED
}

/// Take the home's slot; None when it is busy.
fn take_slot(home: &Path) -> Option<u64> {
    let mut slots = lock(slots());
    if HashMap::contains_key(&slots.running, home) || gone_in(&slots, home) >= MAX_ABANDONED {
        return None;
    }
    slots.next += 1;
    let token = slots.next;
    HashMap::insert(&mut slots.running, PathBuf::from(home), token);
    Some(token)
}

fn release(home: &Path, token: u64) {
    let mut slots = lock(slots());
    if HashMap::get(&slots.running, home) == Some(&token) {
        HashMap::remove(&mut slots.running, home);
    } else {
        HashMap::remove(&mut slots.gone, &token);
    }
}

/// Give up waiting for a compute that is still running: free the home's slot. False when it had
/// already returned.
fn abandon(home: &Path, token: u64) -> bool {
    let mut slots = lock(slots());
    if HashMap::get(&slots.running, home) != Some(&token) {
        return false;
    }
    HashMap::remove(&mut slots.running, home);
    HashMap::insert(&mut slots.gone, token, PathBuf::from(home));
    if gone_in(&slots, home) >= MAX_ABANDONED {
        log::warn!(target: "story_totals", "{} computes are stuck: the index stops until one returns", MAX_ABANDONED);
    }
    true
}

/// Releases a compute's slot when its task ends.
struct InFlight(PathBuf, u64);

impl Drop for InFlight {
    fn drop(&mut self) {
        release(&self.0, self.1);
    }
}

/// One story's compute, bounded by a timeout, so a story too slow to compute is an error row,
/// never a stall. None: the story's
/// compute is still running, so nothing starts. Past the timeout the task keeps running:
/// its handle comes back with the error, so a late result can still be stored.
async fn compute(
    home: &Path,
    job: &Job,
    scope: CallerScope,
    cached: Option<story_changes::GitTotals>,
    earlier: Option<story_changes::GitTotals>,
    batch: Option<std::sync::Arc<story_changes::Batch>>,
) -> Option<(Computed, Option<(tokio::task::JoinHandle<Computed>, u64)>)> {
    #[cfg(test)]
    let timeout = timeout_for(&job.story_id);
    #[cfg(not(test))]
    let timeout = COMPUTE_TIMEOUT;
    let token = take_slot(home)?;
    let guard = InFlight(PathBuf::from(home), token);
    let (home, story) = (PathBuf::from(home), String::clone(&job.story_id));
    let mut task = tokio::spawn(async move {
        let _guard = guard;
        #[cfg(test)]
        inject(&story).await?;
        story_changes::totals(&home, &story, &scope, cached, earlier, batch).await
    });
    Some(match tokio::time::timeout(timeout, &mut task).await {
        Ok(Ok(computed)) => (computed, None),
        Ok(Err(error)) => (Err(format!("the compute task failed: {error}")), None),
        Err(_) => (
            Err(format!("computing took longer than {}s", Duration::as_secs(&timeout))),
            Some((task, token)),
        ),
    })
}

/// `refresh_in` with a batch of its own.
#[cfg(test)]
async fn refresh(home: &Path, store: &std::sync::Arc<Mutex<UsageStore>>, job: Job) {
    refresh_in(
        home,
        store,
        job,
        &std::sync::Arc::new(story_changes::Batch::new()),
    )
    .await
}

/// Compute one story in its scope and store it. A row whose scope is gone is dropped. The story's
/// sessions come from the batch's shared usage report; a story marked since that report was taken
/// takes it again (the mark may stand for sessions it lacks), once for every story marked so far,
/// or waits for the next window when the report is too recent to take again.
async fn refresh_in(
    home: &Path,
    store: &std::sync::Arc<Mutex<UsageStore>>,
    job: Job,
    batch: &std::sync::Arc<story_changes::Batch>,
) {
    let started = now_ms();
    if let Err(ready) = story_changes::Batch::admit(
        batch,
        &job.scope_key,
        &job.story_id,
        job.dirty_at_ms,
        started,
    ) {
        // Its scope's report was taken too recently to take again: the story waits, still due,
        // for the next window.
        let (store, job) = (std::sync::Arc::clone(store), job);
        let waited = crate::commands::await_blocking("story_totals.wait", move || {
            let store = lock(&store);
            Result::map_err(
                rusqlite::Connection::execute(
                    &store.conn,
                    "UPDATE story_totals SET dirty_since_ms = MAX(COALESCE(dirty_since_ms, ?1), ?1)
                     WHERE scope_key = ?2 AND story_key = ?3",
                    params![ready, job.scope_key, str::to_lowercase(&job.story_id)],
                ),
                db_err,
            )
            .map(|_| ())
        })
        .await;
        if let Err(error) = waited {
            log::warn!(target: "story_totals", "wait: {error}");
        }
        return;
    }
    let batch = Some(std::sync::Arc::clone(batch));
    let scope = scope_for(&job.scope_key, &job.kind, job.root.as_deref());
    let parse = |git: &Option<String>| {
        git.as_deref()
            .and_then(|git| serde_json::from_str::<story_changes::GitTotals>(git).ok())
    };
    let (cached, earlier) = (parse(&job.git), parse(&job.earlier_git));
    let job = std::sync::Arc::new(job);
    let (result, late) = match scope {
        Some(scope) => match compute(home, &job, scope, cached, earlier, batch).await {
            Some((result, late)) => (Some(result), late),
            None => return,
        },
        None => (None, None),
    };
    let stored_at = now_ms();
    let late_store = std::sync::Arc::clone(store);
    let late_job = std::sync::Arc::clone(&job);
    let store = std::sync::Arc::clone(store);
    let stored = crate::commands::await_blocking("story_totals.store", move || {
        let store = store.lock().unwrap_or_else(|e| e.into_inner());
        match result {
            Some(result) => store_result(&store, &job, started, stored_at, result, None),
            None => store
                .conn
                .execute(
                    "DELETE FROM story_totals WHERE scope_key = ?1 AND story_key = ?2",
                    params![job.scope_key, job.story_id.to_lowercase()],
                )
                .map(|_| ())
                .map_err(db_err),
        }
    })
    .await;
    if let Err(error) = stored {
        log::warn!(target: "story_totals", "store: {error}");
    }
    // After the timeout error is stored, so a quick late result cannot be overwritten by it.
    if let Some((task, token)) = late {
        tokio::spawn(store_late(
            late_store,
            PathBuf::from(home),
            late_job,
            (started, stored_at + RETRY_MS),
            (task, token),
        ));
    }
}

/// A compute that outlived its timeout: past `ABANDON_AFTER` its slot is freed and its story waits
/// out a long backoff; when it does return, its result is stored unless the story really changed
/// since it started (then it is already marked), and the timeout's own retry mark is cleared, so a
/// slow story that finishes is not recomputed for ever.
async fn store_late(
    store: std::sync::Arc<Mutex<UsageStore>>,
    home: PathBuf,
    job: std::sync::Arc<Job>,
    (started, retry_mark): (i64, i64),
    (mut task, token): (tokio::task::JoinHandle<Computed>, u64),
) {
    #[cfg(test)]
    let abandon_after = abandon_after();
    #[cfg(not(test))]
    let abandon_after = ABANDON_AFTER;
    let mut backoff_mark = retry_mark;
    let joined = tokio::select! {
        joined = &mut task => joined,
        _ = tokio::time::sleep(abandon_after) => {
            if abandon(&home, token) {
                backoff_mark = now_ms() + ABANDON_BACKOFF_MS;
                log::warn!(target: "story_totals", "{}: the compute is stuck; its slot is freed and it is tried again in 6 h", job.story_id);
                let (held, held_job) = (std::sync::Arc::clone(&store), std::sync::Arc::clone(&job));
                let backed = crate::commands::await_blocking("story_totals.backoff", move || {
                    let store = lock(&held);
                    Result::map_err(
                        store.conn.execute(
                            "UPDATE story_totals SET
                                error = CASE WHEN totals IS NULL THEN ?1 ELSE error END,
                                dirty_since_ms = CASE WHEN dirty_since_ms = ?6 THEN ?3 ELSE dirty_since_ms END,
                                dirty_at_ms = CASE WHEN dirty_at_ms = ?6 THEN ?3 ELSE dirty_at_ms END
                             WHERE scope_key = ?4 AND story_key = ?5",
                            params![
                                "computing is stuck; tried again in 6 hours",
                                started,
                                backoff_mark,
                                held_job.scope_key,
                                str::to_lowercase(&held_job.story_id),
                                retry_mark
                            ],
                        ),
                        db_err,
                    )
                    .map(|_| ())
                })
                .await;
                if let Err(error) = backed {
                    log::warn!(target: "story_totals", "backoff: {error}");
                }
            }
            task.await
        }
    };
    let Ok(Ok(computed)) = joined else {
        return;
    };
    let stored = crate::commands::await_blocking("story_totals.late", move || {
        let store = lock(&store);
        // Only the timeout's own marks (retry, backoff) are cleared; a real change after `started`
        // is checked in the UPDATE itself (atomic) and keeps the row as it is.
        store_result(&store, &job, started, now_ms(), Ok(computed), Some((started, retry_mark, backoff_mark)))
    })
    .await;
    if let Err(error) = stored {
        log::warn!(target: "story_totals", "late store: {error}");
    }
}

/// Every candidate story id in `text`: each run of dash-joined words from one that starts with a
/// letter up to a number (`fix/SV-PBF-287-pr-b` gives `sv-pbf-287`). Lower-cased.
fn story_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in str::split(text, |c: char| !(c.is_ascii_alphanumeric() || c == '-')) {
        let parts: Vec<&str> = str::split(token, '-').collect();
        if !parts[0].starts_with(|c: char| c.is_ascii_alphabetic()) {
            continue;
        }
        for (end, part) in Iterator::enumerate(parts.iter()).skip(1) {
            if !str::is_empty(part) && Iterator::all(&mut str::bytes(part), |b| b.is_ascii_digit()) {
                out.push(parts[..=end].join("-").to_lowercase());
            }
        }
    }
    out
}

/// What a mark covers: its session half only, or its git half too.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Half {
    Session,
    Git,
}

/// The `SET` part of a mark at `?1`; a git mark also drops the kept git half.
fn mark_set(half: Half) -> String {
    marked_with(MARK_SQL, half)
}

/// `mark_set` for a story that did not change itself (`QUIET_MARK_SQL`).
fn quiet_mark_set(half: Half) -> String {
    marked_with(QUIET_MARK_SQL, half)
}

fn marked_with(sql: &str, half: Half) -> String {
    let set = str::replace(sql, "$NOW", "?1");
    match half {
        Half::Session => set,
        Half::Git => format!("{set}, git_dirty_ms = ?1"),
    }
}

/// Mark these stories (lower-cased ids) changed in every scope.
fn mark(store: &UsageStore, keys: &BTreeSet<String>, now: i64, half: Half) -> Result<(), String> {
    mark_on(&store.conn, keys, now, half)
}

/// [`mark`] on a connection (or a transaction on it).
fn mark_on(conn: &rusqlite::Connection, keys: &BTreeSet<String>, now: i64, half: Half) -> Result<(), String> {
    let mut stmt = conn
        .prepare_cached(&format!("{} WHERE story_key = ?2", mark_set(half)))
        .map_err(db_err)?;
    for key in keys {
        stmt.execute(params![now, key]).map_err(db_err)?;
    }
    Ok(())
}

/// [`mark_scoped_on`] for a story that did not change itself (`QUIET_MARK_SQL`): lines were counted.
fn quiet_mark_scoped_on(conn: &rusqlite::Connection, scope_key: &str, story_key: &str, now: i64) -> Result<(), String> {
    conn.prepare_cached(&format!("{} WHERE story_key = ?2 AND scope_key = ?3", quiet_mark_set(Half::Session)))
        .map_err(db_err)?
        .execute(params![now, story_key, scope_key])
        .map_err(db_err)?;
    Ok(())
}

/// Count the due commits (see `story_changes::sweep_commits`), marking each story whose rows were
/// filled so its lines are folded again; the git half is kept (no walk). Returns what it did.
fn sweep_commits(
    store: &Mutex<UsageStore>,
    home: &Path,
    now: i64,
    spend: Duration,
) -> Result<story_changes::SweptCommits, String> {
    story_changes::sweep_commits(store, home, now, (spend, COMPUTE_TIMEOUT), &|conn, filled| {
        for (scope, story) in filled {
            quiet_mark_scoped_on(conn, scope, story, now)?;
        }
        Ok(())
    })
}

/// How long one commit sweep counts: as long as the last compute ran (so the sweep gets at least
/// half of the shared budget, the computes the other half), never under `COMMIT_SPEND`.
fn sweep_spend(last_compute: Duration) -> Duration {
    Ord::max(COMMIT_SPEND, last_compute)
}

/// Mark one story in one scope (`?3` is its scope key): the rows of the same story in other scopes
/// are not touched.
fn mark_scoped_on(
    conn: &rusqlite::Connection,
    scope_key: &str,
    story_key: &str,
    now: i64,
    half: Half,
) -> Result<(), String> {
    conn.prepare_cached(&format!("{} WHERE story_key = ?2 AND scope_key = ?3", mark_set(half)))
        .map_err(db_err)?
        .execute(params![now, story_key, scope_key])
        .map_err(db_err)?;
    Ok(())
}

/// Mark these stories where they have totals already. A story with none is still waiting for its
/// first compute (the backfill's), which builds everything whole, so it is left as it is.
fn mark_computed_on(conn: &rusqlite::Connection, keys: &BTreeSet<String>, now: i64, half: Half) -> Result<(), String> {
    let mut stmt = conn
        .prepare_cached(&format!("{} WHERE story_key = ?2 AND totals IS NOT NULL", mark_set(half)))
        .map_err(db_err)?;
    for key in keys {
        stmt.execute(params![now, key]).map_err(db_err)?;
    }
    Ok(())
}

/// Mark the stories that followed one of these branches (git half).
fn mark_branches(store: &UsageStore, names: &BTreeSet<String>, now: i64) -> Result<(), String> {
    let mut stmt = store
        .conn
        .prepare_cached(&format!("{} WHERE instr(branches, ?2) > 0", mark_set(Half::Git)))
        .map_err(db_err)?;
    for name in names {
        let quoted = serde_json::to_string(name).map_err(|e| e.to_string())?;
        stmt.execute(params![now, quoted]).map_err(db_err)?;
    }
    Ok(())
}

/// Mark every story.
#[cfg(test)]
fn mark_all(store: &UsageStore, now: i64, half: Half) -> Result<(), String> {
    store
        .conn
        .execute(&quiet_mark_set(half), params![now])
        .map_err(db_err)?;
    Ok(())
}

/// The tips of a repository's branches, remote branches and notes, plus its default branch.
fn ref_tips(repo: &git2::Repository) -> BTreeMap<String, String> {
    let mut tips = BTreeMap::new();
    if let Ok(refs) = repo.references() {
        for reference in refs.flatten() {
            let (Ok(name), Some(oid)) =
                (git2::Reference::name(&reference), git2::Reference::target(&reference))
            else {
                continue;
            };
            if ["refs/heads/", "refs/remotes/", "refs/notes/"]
                .iter()
                .any(|prefix| name.starts_with(prefix))
            {
                BTreeMap::insert(&mut tips, name.to_owned(), oid.to_string());
            }
        }
    }
    if let Some(oid) = crate::attribution::history::default_branch(repo) {
        std::collections::BTreeMap::insert(&mut tips, DEFAULT_REF.into(), ToString::to_string(&oid));
    }
    tips
}

/// A branch ref's branch name (`refs/heads/x` and `refs/remotes/origin/x` are both `x`).
fn branch_name(reference: &str) -> Option<&str> {
    reference
        .strip_prefix("refs/heads/")
        .or_else(|| reference.strip_prefix("refs/remotes/origin/"))
}

/// What moved between two reads of a repository's refs.
#[derive(Debug, Default, PartialEq)]
struct Moved {
    /// Stories (lower-cased ids) named by a moved branch or by a commit that landed.
    stories: BTreeSet<String>,
    /// Moved branches, and branches whose tip a landed merge took as its second parent.
    branches: BTreeSet<String>,
    /// The default branch moved in a way that could not be read (its open stories are marked).
    all: bool,
    /// A notes ref moved.
    notes: bool,
}

fn moved(
    repo: &git2::Repository,
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> Moved {
    let mut out = Moved::default();
    let names: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    for name in names {
        if BTreeMap::get(before, name) == BTreeMap::get(after, name) {
            continue;
        }
        if name.starts_with("refs/notes/") {
            out.notes = true;
        } else if name == DEFAULT_REF {
            let mut merged = BTreeSet::new();
            let read = match (BTreeMap::get(before, name), BTreeMap::get(after, name)) {
                (Some(old), Some(new)) => landed(repo, old, new, &mut out.stories, &mut merged),
                _ => false,
            };
            out.all |= !read;
            // A merge's second parent is the tip a branch had: that branch's stories landed.
            for (reference, oid) in before.iter().chain(after.iter()) {
                if BTreeSet::contains(&merged, oid) {
                    if let Some(branch) = branch_name(reference) {
                        BTreeSet::insert(&mut out.branches, branch.to_owned());
                        Extend::extend(&mut out.stories, story_tokens(branch));
                    }
                }
            }
        } else if name != "refs/remotes/origin/main" {
            // The default branch is read through DEFAULT_REF, by what landed on it.
            if let Some(branch) = branch_name(name) {
                BTreeSet::insert(&mut out.branches, branch.to_owned());
            }
            out.stories.extend(story_tokens(name));
        }
    }
    out
}

/// The stories named by the first-parent commits from `old` to `new` (merge branches and
/// subjects), and each merge's second parent. False when they cannot all be read.
fn landed(
    repo: &git2::Repository,
    old: &str,
    new: &str,
    stories: &mut BTreeSet<String>,
    merged: &mut BTreeSet<String>,
) -> bool {
    let (Ok(old), Ok(new)) = (git2::Oid::from_str(old), git2::Oid::from_str(new)) else {
        return false;
    };
    let Ok(mut walk) = repo.revwalk() else {
        return false;
    };
    if walk.push(new).is_err() || walk.hide(old).is_err() || walk.simplify_first_parent().is_err() {
        return false;
    }
    for (read, id) in Iterator::enumerate(walk) {
        if read >= LANDED_SCAN {
            return false;
        }
        let Ok(commit) = id.and_then(|id| repo.find_commit(id)) else {
            return false;
        };
        stories.extend(story_tokens(
            Option::flatten(Result::ok(commit.summary())).unwrap_or(""),
        ));
        if let Ok(second) = commit.parent_id(1) {
            BTreeSet::insert(merged, second.to_string());
        }
    }
    true
}

/// Read the refs of every repository a story was computed from, mark what moved since the last
/// read (persisted, so moves while the app was closed count), and keep the new tips.
fn poll_refs(store: &UsageStore, now: i64) -> Result<(), String> {
    let mut repos = BTreeSet::new();
    {
        let mut stmt = store
            .conn
            .prepare_cached("SELECT DISTINCT repos FROM story_totals WHERE repos != '[]'")
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |r| rusqlite::Row::get::<_, String>(r, 0))
            .map_err(db_err)?;
        for list in rows {
            let list: Vec<String> =
                serde_json::from_str(&list.map_err(db_err)?).unwrap_or_default();
            repos.extend(list);
        }
    }
    for path in repos {
        let Ok(repo) = git2::Repository::open(&path) else {
            continue;
        };
        let after = ref_tips(&repo);
        let before: BTreeMap<String, String> = {
            let mut stmt = store
                .conn
                .prepare_cached("SELECT name, oid FROM story_totals_ref WHERE repo = ?1")
                .map_err(db_err)?;
            let rows = stmt
                .query_map(params![path], |r| {
                    Ok((rusqlite::Row::get(r, 0)?, rusqlite::Row::get(r, 1)?))
                })
                .map_err(db_err)?;
            rows.collect::<Result<_, _>>().map_err(db_err)?
        };
        if before == after {
            continue;
        }
        // First sight of a repository is its baseline: the rows read from it were computed a moment
        // ago, and marking them would recompute what just finished.
        if !BTreeMap::is_empty(&before) {
            let moved = moved(&repo, &before, &after);
            mark(store, &moved.stories, now, Half::Git)?;
            mark_branches(store, &moved.branches, now)?;
            if moved.all {
                log::warn!(target: "story_totals", "{path}: the default branch moved in a way that could not be read; marking its open stories");
                mark_open_stories(store, &path, now)?;
            }
            if moved.notes {
                notes_moved(store, &path, now)?;
            }
        }
        // Only what moved is written.
        let tx = rusqlite::Transaction::new_unchecked(
            &store.conn,
            rusqlite::TransactionBehavior::Immediate,
        )
        .map_err(db_err)?;
        for (name, oid) in &after {
            if BTreeMap::get(&before, name) != Some(oid) {
                tx.execute(
                    "INSERT OR REPLACE INTO story_totals_ref (repo, name, oid) VALUES (?1, ?2, ?3)",
                    params![path, name, oid],
                )
                .map_err(db_err)?;
            }
        }
        for name in before.keys() {
            if !BTreeMap::contains_key(&after, name) {
                tx.execute(
                    "DELETE FROM story_totals_ref WHERE repo = ?1 AND name = ?2",
                    params![path, name],
                )
                .map_err(db_err)?;
            }
        }
        rusqlite::Transaction::commit(tx).map_err(db_err)?;
    }
    notes_flush(store, now)?;
    store
        .conn
        .execute(
            "DELETE FROM story_totals WHERE asked_at_ms < ?1",
            params![now - FORGET_MS],
        )
        .map_err(db_err)?;
    Ok(())
}

/// When a story was last marked for a moved notes ref, and whether a move since is waiting out the
/// cooldown. Kept in memory: a restart only allows one early mark.
#[derive(Default)]
struct NotesCooldown {
    marked_ms: i64,
    pending: bool,
}

fn notes_cooldowns() -> &'static Mutex<HashMap<(String, String), NotesCooldown>> {
    static COOLDOWNS: OnceLock<Mutex<HashMap<(String, String), NotesCooldown>>> = OnceLock::new();
    COOLDOWNS.get_or_init(Default::default)
}

/// A notes mark: the git half is stale, but the story did not change itself, so `changed_ms` stays.
const NOTES_MARK_SQL: &str = "UPDATE story_totals SET dirty_since_ms = MIN(COALESCE(dirty_since_ms, ?1), ?1), dirty_at_ms = ?1, git_dirty_ms = ?1 WHERE scope_key = ?2 AND story_key = ?3";

fn mark_story_git(store: &UsageStore, scope_key: &str, story_key: &str, now: i64) -> Result<(), String> {
    db(store.conn.execute(NOTES_MARK_SQL, params![now, scope_key, story_key]))?;
    Ok(())
}

/// The default branch of `repo` moved in a way that could not be read: mark, once and quietly, the
/// stories that are still open (computed with commits, not merged).
fn mark_open_stories(store: &UsageStore, repo: &str, now: i64) -> Result<(), String> {
    let repo = to_json(&repo)?;
    db(store.conn.execute(
        "UPDATE story_totals SET dirty_since_ms = MIN(COALESCE(dirty_since_ms, ?1), ?1),
            dirty_at_ms = ?1, git_dirty_ms = ?1
         WHERE instr(repos, ?2) > 0 AND totals IS NOT NULL
           AND json_extract(git, '$.commits') > 0
           AND json_extract(facets, '$.mergedAt') IS NULL",
        params![now, repo],
    ))?;
    Ok(())
}

/// A notes ref of `repo` moved: mark the stories that can still gain notes (computed with commits,
/// not merged, and really changed in the last half hour), each at most once per cooldown. A move inside the cooldown is kept pending and
/// honoured by `notes_flush`, so none is lost. A recompute never re-arms its own mark.
fn notes_moved(store: &UsageStore, repo: &str, now: i64) -> Result<(), String> {
    let repo = to_json(&repo)?;
    let rows: Vec<(String, String)> = {
        let mut stmt = db(store.conn.prepare_cached(
            "SELECT scope_key, story_key FROM story_totals
             WHERE instr(repos, ?1) > 0 AND totals IS NOT NULL
               AND json_extract(git, '$.commits') > 0
               AND json_extract(facets, '$.mergedAt') IS NULL
               AND changed_ms >= ?2",
        ))?;
        let found = db(stmt.query_map(params![repo, now - NOTES_ACTIVE_MS], |r| {
            Ok((rusqlite::Row::get(r, 0)?, rusqlite::Row::get(r, 1)?))
        }))?;
        db(Iterator::collect::<Result<_, _>>(found))?
    };
    let mut cooldowns = lock(notes_cooldowns());
    for key in rows {
        let state = HashMap::entry(&mut cooldowns, (String::clone(&key.0), String::clone(&key.1))).or_default();
        if now - state.marked_ms >= NOTES_COOLDOWN_MS {
            mark_story_git(store, &key.0, &key.1, now)?;
            state.marked_ms = now;
            state.pending = false;
        } else {
            state.pending = true;
        }
    }
    Ok(())
}

/// Mark the stories whose notes move waited out its cooldown.
fn notes_flush(store: &UsageStore, now: i64) -> Result<(), String> {
    let mut cooldowns = lock(notes_cooldowns());
    for (key, state) in HashMap::iter_mut(&mut cooldowns) {
        if state.pending && now - state.marked_ms >= NOTES_COOLDOWN_MS {
            mark_story_git(store, &key.0, &key.1, now)?;
            state.marked_ms = now;
            state.pending = false;
        }
    }
    Ok(())
}

/// `sweep_sources_except` with no computing story.
#[cfg(test)]
fn sweep_sources(store: &UsageStore, home: &Path, now: i64) -> Result<(), String> {
    sweep_sources_except(store, home, now, None).map(|_| ())
}

/// Apply the queued sources to the contribution rows, the way `story_changes` resolves them
/// (`report_with`): a session tied by its working folder, its launch record or another machine is
/// found here, where a trigger could not. Each source rewrites only its own rows; the stories whose
/// rows changed (and those not built yet, which are built whole when computed) are marked. A
/// compute of one story in one scope (`except`) folds the rows it applies itself and so does not
/// mark that `(scope key, story)` pair; the same story in another scope, and every other story the
/// sources touch, are marked as usual. Returns the stories (lower-cased ids) whose rows the sweep
/// changed or found unbuilt.
pub(super) fn sweep_sources_except(
    store: &UsageStore,
    home: &Path,
    now: i64,
    except: Option<(&str, &str)>,
) -> Result<BTreeSet<String>, String> {
    const KIND: &str = <story_changes::StoryRows as EntityAdapter>::KIND;
    const VERSION: i64 = <story_changes::StoryRows as EntityAdapter>::VERSION;
    let queued = contribution::queued(store, <story_changes::StoryRows as EntityAdapter>::SOURCES, SESSION_SWEEP_LIMIT)?;
    let mut touched = BTreeSet::new();
    if matches!(queued.len(), 0) {
        return Ok(touched);
    }
    let adapter = story_changes::StoryRows::new(home.to_owned());
    for entry in &queued {
        let source = &entry.source;
        // The marks are written inside the apply's transaction, with the rows and the queue delete.
        let mark_changed = |conn: &rusqlite::Connection, applied: &contribution::Applied| {
            // Each (scope, story) whose rows changed is marked in its own scope, except the one that
            // is computing (it folds what it applied).
            for (scope, entity) in &applied.changed {
                let own = Option::is_some_and(except, |(s, story)| *s == scope[..] && *story == entity[..]);
                if !own {
                    mark_scoped_on(conn, scope, entity, now, Half::Session)?;
                }
            }
            // A story with no built rows has not computed in this version: it builds them whole when
            // it does, so only one that already has totals is marked (never the computing story).
            let unbuilt: BTreeSet<String> = applied
                .unbuilt
                .iter()
                .filter(|entity| !Option::is_some_and(except, |(_, story)| *story == entity[..]))
                .cloned()
                .collect();
            mark_computed_on(conn, &unbuilt, now, Half::Session)
        };
        let (resolved, wanted) = match SourceResolver::contributions(&adapter, store, source) {
            Ok(answer) => answer,
            Err(error) => {
                // Kept queued: the next sweep asks again (a session no one answers to resolves
                // to no rows, and is applied and dequeued below like any other).
                log::debug!(target: "story_totals", "{} {}: {error}", source.kind, source.id);
                continue;
            }
        };
        // A failed apply rolled back whole (rows, marks and the queue row), so the source stays
        // queued and the next sweep redoes it.
        match contribution::apply(
            &mark_changed,
            store,
            KIND,
            VERSION,
            entry,
            &resolved,
            &wanted,
            now,
        ) {
            Ok(applied) => {
                BTreeSet::extend(
                    &mut touched,
                    Iterator::map(IntoIterator::into_iter(applied.changed), |(_, entity)| {
                        entity
                    }),
                );
                BTreeSet::extend(&mut touched, applied.unbuilt);
            }
            Err(error) => {
                log::warn!(target: "story_totals", "{} {}: {error}", source.kind, source.id)
            }
        }
    }
    Ok(touched)
}

/// Context changes for the worker to mark on its next turn.
#[derive(Debug, Default)]
struct ContextMarks {
    paths: BTreeSet<String>,
    /// The change was capped or replaced the index (every app start), so `paths` is not all of it:
    /// the stories whose context inputs differ from what the index settled on are marked.
    capped: bool,
}

fn context_marks() -> &'static Mutex<ContextMarks> {
    static MARKS: OnceLock<Mutex<ContextMarks>> = OnceLock::new();
    MARKS.get_or_init(Default::default)
}

/// Queue a context change (`context-index-changed`) for the worker.
fn context_changed(payload: &str) {
    let Ok(change) = serde_json::from_str::<Value>(payload) else {
        return;
    };
    let paths = Value::get(&change, "paths").and_then(Value::as_array);
    let mut marks = context_marks().lock().unwrap_or_else(|e| e.into_inner());
    marks.capped |= Value::get(&change, "truncated").and_then(Value::as_bool) == Some(true);
    Extend::extend(
        &mut marks.paths,
        paths
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned),
    );
    drop(marks);
    wake();
}

/// The story a review record names (`story:` in its frontmatter), for a path under
/// `reviews/rounds`, `reviews/findings` or `reviews/dispositions`.
fn review_record_story(path: &str) -> Option<String> {
    if !["/reviews/rounds/", "/reviews/findings/", "/reviews/dispositions/"]
        .iter()
        .any(|dir| str::contains(path, dir))
    {
        return None;
    }
    let path = Path::new(path);
    let text = std::fs::read_to_string(path).ok()?;
    let fields = crate::commands::frontmatter_editor::frontmatter_json(
        crate::commands::pipeline_format::format_of(path),
        &text,
    );
    BTreeMap::get(&fields, "story")
        .and_then(Value::as_str)
        .map(str::to_lowercase)
}

/// A hash of a file's content, or None when it cannot be read.
fn content_hash(path: &str) -> Option<i64> {
    #[cfg(test)]
    HASHED.with(|n| n.set(n.get() + 1));
    let bytes = fs::read(path).ok()?;
    let hash = Iterator::fold(IntoIterator::into_iter(bytes), 0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        u64::wrapping_mul(hash ^ u64::from(byte), 0x0100_0000_01b3)
    });
    // Held in 63 bits, so SQLite keeps it as it is.
    Some((hash >> 1) as i64)
}

/// How many files `content_hash` has read on this thread.
#[cfg(test)]
thread_local! {
    static HASHED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// An indexed file's key in `story_totals_ctx`: its path under the CodeSense home, so a moved home
/// (or a copy of it) keeps the keys; a path outside the home is kept as it is.
fn file_key(home: &Path, path: &str) -> String {
    let rel = Result::ok(Path::strip_prefix(Path::new(path), home));
    Option::unwrap_or(Option::and_then(rel, Path::to_str), path).to_owned()
}

/// What the totals settled on for one file: its mtime and, when it was read, its content.
#[derive(Clone, Copy, PartialEq)]
struct Settled {
    mtime: i64,
    content: Option<i64>,
}

/// A story's files by key.
type Files = BTreeMap<String, Settled>;

/// The stories an indexed file names: by its path, its `story_id` and (a review record) its `story`.
fn stories_named_by(record: &ContextRecord) -> Vec<String> {
    let mut stories = story_tokens(&record.path);
    for key in ["story_id", "story"] {
        if let Some(id) = ContextRecord::str_field(record, key) {
            Vec::push(&mut stories, str::to_lowercase(id));
        }
    }
    <[_]>::sort(&mut stories);
    Vec::dedup(&mut stories);
    stories
}

/// Every indexed file that names one of the stories `known`, by story: key to mtime and absolute
/// path. One pass over the index, for a start or a capped change; no file is read.
fn story_files(
    home: &Path,
    snapshot: &Snapshot,
    known: &BTreeSet<String>,
) -> BTreeMap<String, BTreeMap<String, (i64, String)>> {
    let mut files: BTreeMap<String, BTreeMap<String, (i64, String)>> = BTreeMap::new();
    for record in &snapshot.records {
        for story in stories_named_by(record) {
            if BTreeSet::contains(known, &story) {
                let mtime = Option::unwrap_or(record.mtime_ms, 0) as i64;
                BTreeMap::insert(
                    BTreeMap::entry(&mut files, story).or_default(),
                    file_key(home, &record.path),
                    (mtime, record.path.clone()),
                );
            }
        }
    }
    files
}

/// What [`changed_stories`] found.
struct Diff {
    /// The stories whose files differ (a file added, gone, moved or really changed).
    changed: BTreeSet<String>,
    /// The files to record for those stories, and for the ones that were only touched (a newer
    /// mtime over the same content), so the next start finds them settled.
    settle: BTreeMap<String, Files>,
}

/// Compare the files naming each story with the ones the totals last settled on. A file whose mtime
/// differs is read once and counts as changed only if its content differs too; a git pull or a
/// checkout that rewrites mtimes marks nothing. The caller marks the changed stories, then
/// [`settle_files`] records the files. One pass over the index and two queries, and a read per
/// file that is new or whose mtime differs, whatever the number of stories.
fn changed_stories(store: &UsageStore, home: &Path, snapshot: &Snapshot) -> Result<Diff, String> {
    let mut stmt = store.conn.prepare_cached("SELECT DISTINCT story_key FROM story_totals").map_err(db_err)?;
    let known: BTreeSet<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(db_err)?
        .collect::<Result<_, _>>()
        .map_err(db_err)?;
    let mut stmt = store
        .conn
        .prepare_cached("SELECT DISTINCT story_key FROM story_totals WHERE computed_at_ms IS NOT NULL")
        .map_err(db_err)?;
    let computed: BTreeSet<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(db_err)?
        .collect::<Result<_, _>>()
        .map_err(db_err)?;
    let mut stmt = store.conn.prepare_cached("SELECT story_key, path, mtime_ms, content FROM story_totals_ctx").map_err(db_err)?;
    let mut stored: BTreeMap<String, Files> = BTreeMap::new();
    for row in stmt
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, Settled { mtime: r.get(2)?, content: r.get(3)? }))
        })
        .map_err(db_err)?
    {
        let (story, path, settled) = row.map_err(db_err)?;
        BTreeMap::insert(BTreeMap::entry(&mut stored, story).or_default(), path, settled);
    }
    let mut current = story_files(home, snapshot, &known);
    // A story that nothing names and nothing recorded is unchanged; a recorded one that nothing
    // names now (or whose row is gone) has changed to nothing.
    for story in stored.keys() {
        BTreeMap::entry(&mut current, story.clone()).or_default();
    }
    // While the files table is being rebuilt from an older form, the stories already computed record
    // their files without being marked: nothing is known to have changed.
    let nothing_recorded: bool = db(store.conn.query_row(
        "SELECT count(*) > 0 FROM story_totals_flag WHERE name = 'ctx_rebuild'",
        [],
        |r| rusqlite::Row::get(r, 0),
    ))?;
    let mut read: BTreeMap<String, Option<i64>> = BTreeMap::new();
    let mut diff = Diff { changed: BTreeSet::new(), settle: BTreeMap::new() };
    for (story, files) in current {
        let before = BTreeMap::get(&stored, &story);
        if before.is_none() && files.is_empty() {
            continue;
        }
        let mut same = Option::map_or(before, false, |before| before.len() == files.len());
        let mut touched = false;
        let mut settled = Files::new();
        for (key, (mtime, path)) in files {
            let was = <Option<&Settled>>::copied(Option::and_then(before, |before| {
                BTreeMap::get(before, &key)
            }));
            let entry = match was {
                Some(was) if was.mtime == mtime => was,
                _ => {
                    let content = *BTreeMap::entry(&mut read, path.clone()).or_insert_with(|| content_hash(&path));
                    match was {
                        Some(was) if content.is_some() && was.content == content => touched = true,
                        _ => same = false,
                    }
                    Settled { mtime, content }
                }
            };
            BTreeMap::insert(&mut settled, key, entry);
        }
        if !same && !(nothing_recorded && BTreeSet::contains(&computed, &story)) {
            BTreeSet::insert(&mut diff.changed, story.clone());
        }
        if !same || touched {
            BTreeMap::insert(&mut diff.settle, story, settled);
        }
    }
    Ok(diff)
}

/// Record the files [`changed_stories`] found, in one transaction, which also ends the rebuild of
/// the files table (see `init`).
fn settle_files(store: &UsageStore, settle: &BTreeMap<String, Files>) -> Result<(), String> {
    let tx = db(rusqlite::Transaction::new_unchecked(&store.conn, rusqlite::TransactionBehavior::Immediate))?;
    for (story, files) in settle {
        tx.execute("DELETE FROM story_totals_ctx WHERE story_key = ?1", params![story]).map_err(db_err)?;
        for (path, settled) in files {
            tx.execute(
                "INSERT INTO story_totals_ctx (story_key, path, mtime_ms, content) VALUES (?1, ?2, ?3, ?4)",
                params![story, path, settled.mtime, settled.content],
            )
            .map_err(db_err)?;
        }
    }
    tx.execute("DELETE FROM story_totals_flag WHERE name = 'ctx_rebuild'", []).map_err(db_err)?;
    db(rusqlite::Transaction::commit(tx))
}

/// Record the files of the paths a change named, in the work of those paths alone: each path's old
/// rows go, and its current file (found in the sorted index) is recorded for the stories with
/// totals that it names.
fn settle_paths(store: &UsageStore, home: &Path, snapshot: &Snapshot, paths: &BTreeSet<String>) -> Result<(), String> {
    for path in paths {
        let key = file_key(home, path);
        store.conn.execute("DELETE FROM story_totals_ctx WHERE path = ?1", params![key]).map_err(db_err)?;
        // The index is ordered by path components (`BTreeMap<PathBuf, _>`), not by the path's text.
        let found = snapshot
            .records
            .binary_search_by(|record| Path::cmp(Path::new(&record.path), Path::new(path)));
        let Ok(at) = found else {
            continue;
        };
        let record = &snapshot.records[at];
        let mtime = Option::unwrap_or(record.mtime_ms, 0) as i64;
        let content = content_hash(&record.path);
        for story in stories_named_by(record) {
            store
                .conn
                .execute(
                    "INSERT OR REPLACE INTO story_totals_ctx (story_key, path, mtime_ms, content)
                     SELECT ?1, ?2, ?3, ?4 WHERE EXISTS (SELECT 1 FROM story_totals WHERE story_key = ?1)",
                    params![story, key, mtime, content],
                )
                .map_err(db_err)?;
        }
    }
    Ok(())
}

/// Mark what queued context changes name. A review record marks its story's session half (its
/// rounds); any other path that names a story (its file: source repo, touchpoints) marks the git
/// half. Only the stories the paths name are looked at.
///
/// A capped change (and the index's first build at every start) lists only some of its paths, so
/// the stories are found by what they are made of: each one whose indexed files differ from the
/// ones last settled is marked. A story whose files are the same is left as it is. With no ready
/// index to compare with, the whole change (its paths too) waits for the next turn.
///
/// A change the queue cannot see (a story file, a worktree link) may move a session between
/// stories, so the stories it names (or whose files differ) have their session rows rebuilt
/// whole by their next compute, not only marked.
fn apply_context(
    store: &UsageStore,
    home: &Path,
    marks: ContextMarks,
    now: i64,
    snapshot: Option<&Snapshot>,
) -> Result<bool, String> {
    let kind = <story_changes::StoryRows as EntityAdapter>::KIND;
    if marks.capped {
        let Some(snapshot) = snapshot.filter(|snapshot| snapshot.ready) else {
            let mut kept = context_marks().lock().unwrap_or_else(|e| e.into_inner());
            kept.capped = true;
            Extend::extend(&mut kept.paths, marks.paths);
            return Ok(false);
        };
        let diff = changed_stories(store, home, snapshot)?;
        contribution::unbuild(&store.conn, kind, &diff.changed)?;
        mark(store, &diff.changed, now, Half::Git)?;
        settle_files(store, &diff.settle)?;
        // The diff covers what the listed paths name: a path of an unchanged story marks nothing.
        // Which review records changed is not known, so the batch reads them again.
        return Ok(true);
    }
    let (mut reviewed, mut named) = (BTreeSet::new(), BTreeSet::new());
    for path in &marks.paths {
        match review_record_story(path) {
            Some(story) => {
                BTreeSet::insert(&mut reviewed, story);
            }
            None => Extend::extend(&mut named, story_tokens(path)),
        }
    }
    mark(store, &reviewed, now, Half::Session)?;
    contribution::unbuild(&store.conn, kind, &named)?;
    mark(store, &named, now, Half::Git)?;
    // Record the files the paths name as settled, so a restart finds nothing more to mark.
    if let Some(snapshot) = snapshot {
        settle_paths(store, home, snapshot, &marks.paths)?;
    }
    Ok(!BTreeSet::is_empty(&reviewed))
}

/// Rows computed by another version of the computation redo what the steps since changed.
fn mark_other_versions(store: &UsageStore, now: i64) -> Result<(), String> {
    mark_version_changes(store, now, story_changes::TOTALS_STEPS)
}

/// A stored row's parts, as a version step's fill reads and writes them.
#[derive(Debug)]
pub(crate) struct StoredTotals {
    pub totals: Value,
    pub facets: Value,
    /// The kept git half (`GitTotals`); null when the row has none.
    pub git: Value,
}

/// Each row computed by another version than the last of `steps` redoes only what the steps since
/// its version changed: the `Added` fills run on it in place, a half that changed recomputes (the
/// other half's kept values stand), and a `Whole` step recomputes it whole, as every step did
/// before the kinds. A row that only needed fills is stamped current and marked for nothing.
fn mark_version_changes(
    store: &UsageStore,
    now: i64,
    steps: &[(i64, story_changes::Bump)],
) -> Result<(), String> {
    let current = Option::map_or(<[(i64, story_changes::Bump)]>::last(steps), 0, |step| {
        step.0
    });
    let versions: Vec<Option<i64>> = {
        let mut stmt = db(rusqlite::Connection::prepare(&store.conn,
            "SELECT DISTINCT version FROM story_totals WHERE totals IS NOT NULL AND version IS NOT ?1",
        ))?;
        let rows = db(rusqlite::Statement::query_map(
            &mut stmt,
            params![current],
            |r| rusqlite::Row::get(r, 0),
        ))?;
        db(Iterator::collect(rows))?
    };
    for version in versions {
        let redo = story_changes::redo_since(steps, version);
        if !Vec::is_empty(&redo.added) {
            log::info!(target: "story_totals", "rows of version {version:?}: fields added since {:?}", redo.added);
        }
        let half = if redo.whole || redo.git {
            Some(Half::Git)
        } else if redo.session {
            Some(Half::Session)
        } else {
            None
        };
        if !redo.whole {
            // The git half a half mark keeps needs its fills too; only a row with nothing left
            // to recompute is stamped current.
            fill_rows(
                store,
                version,
                &redo.fills,
                bool::then_some(Option::is_none(&half), current),
            )?;
        }
        if let Some(half) = half {
            db(rusqlite::Connection::execute(
                &store.conn,
                &format!(
                    "{} WHERE totals IS NOT NULL AND version IS ?2",
                    quiet_mark_set(half)
                ),
                params![now, version],
            ))?;
        }
    }
    Ok(())
}

/// Run `fills` on every row computed at `version`, in one transaction (a read and a write of each
/// row's JSON, no git or usage read), and stamp the rows `stamp` when given.
fn fill_rows(
    store: &UsageStore,
    version: Option<i64>,
    fills: &[fn(&mut StoredTotals)],
    stamp: Option<i64>,
) -> Result<(), String> {
    if <[fn(&mut StoredTotals)]>::is_empty(fills) && Option::is_none(&stamp) {
        return Ok(());
    }
    let parse = |text: Option<String>, empty: Value| -> Result<Value, String> {
        match text {
            Some(text) => Result::map_err(serde_json::from_str(&text), |e| ToString::to_string(&e)),
            None => Ok(empty),
        }
    };
    let tx = db(rusqlite::Transaction::new_unchecked(
        &store.conn,
        rusqlite::TransactionBehavior::Immediate,
    ))?;
    let rows: Vec<(String, String, String, Option<String>, Option<String>)> = {
        let mut stmt = db(rusqlite::Connection::prepare(
            &tx,
            "SELECT scope_key, story_key, totals, facets, git FROM story_totals
             WHERE totals IS NOT NULL AND version IS ?1",
        ))?;
        let rows = db(rusqlite::Statement::query_map(
            &mut stmt,
            params![version],
            |r| {
                Ok((
                    rusqlite::Row::get(r, 0)?,
                    rusqlite::Row::get(r, 1)?,
                    rusqlite::Row::get(r, 2)?,
                    rusqlite::Row::get(r, 3)?,
                    rusqlite::Row::get(r, 4)?,
                ))
            },
        ))?;
        db(Iterator::collect(rows))?
    };
    for (scope_key, story_key, totals, facets, git) in rows {
        let mut stored = StoredTotals {
            totals: parse(Some(totals), Value::Null)?,
            facets: parse(facets, json!({}))?,
            git: parse(git, Value::Null)?,
        };
        for fill in fills {
            fill(&mut stored);
        }
        let git = if Value::is_null(&stored.git) {
            None
        } else {
            Some(to_json(&stored.git)?)
        };
        db(rusqlite::Connection::execute(&tx,
            "UPDATE story_totals SET totals = ?1, facets = ?2, git = ?3, version = COALESCE(?4, version)
             WHERE scope_key = ?5 AND story_key = ?6",
            params![to_json(&stored.totals)?, to_json(&stored.facets)?, git, stamp, scope_key, story_key],
        ))?;
    }
    db(rusqlite::Transaction::commit(tx))
}

/// Start the index worker: it fills the index one story at a time with a rest between, recomputes changed stories beside it, resolves
/// changed sessions to their stories, and reads the watched repositories' refs.
pub fn install(app: tauri::AppHandle) {
    use tauri::Listener;
    let Some(home) = crate::commands::paths::codesense_home_dir() else {
        return;
    };
    let _ = APP.set(app.clone());
    app.listen(crate::context_query::service::CHANGE_EVENT, |event| {
        context_changed(tauri::Event::payload(&event));
    });
    tauri::async_runtime::spawn(run(home));
}

async fn run(home: PathBuf) {
    let opened = home.clone();
    let store = match crate::commands::await_blocking("story_totals.open", move || {
        let store = UsageStore::open(&opened.join("usage.sqlite"))?;
        mark_other_versions(&store, now_ms())?;
        Ok(store)
    })
    .await
    {
        Ok(store) => std::sync::Arc::new(Mutex::new(store)),
        Err(error) => {
            log::warn!(target: "story_totals", "open: {error}");
            return;
        }
    };
    let (mut polled, mut swept): (Option<tokio::time::Instant>, Option<tokio::time::Instant>) =
        (None, None);
    let mut changed_turn = false;
    let mut commits_at: Option<tokio::time::Instant> = None;
    let mut last_compute = Duration::ZERO;
    let (mut batch, mut batched, mut batch_at) = (
        std::sync::Arc::new(story_changes::Batch::new()),
        0usize,
        tokio::time::Instant::now(),
    );
    loop {
        if batched >= BATCH || tokio::time::Instant::elapsed(&batch_at) >= BATCH_AGE {
            (batch, batched, batch_at) = (
                std::sync::Arc::new(story_changes::Batch::new()),
                0,
                tokio::time::Instant::now(),
            );
        }
        if polled.is_none_or(|at| at.elapsed() >= REF_POLL) {
            polled = Some(tokio::time::Instant::now());
            let store = store.clone();
            if let Err(error) = crate::commands::await_blocking("story_totals.refs", move || {
                poll_refs(&store.lock().unwrap_or_else(|e| e.into_inner()), now_ms())
            })
            .await
            {
                log::warn!(target: "story_totals", "refs: {error}");
            }
        }
        if commits_at.is_none_or(|at| tokio::time::Instant::now() >= at) {
            let (store, home, spend) = (store.clone(), home.clone(), sweep_spend(last_compute));
            let gap = sweep_turn(
                crate::commands::await_blocking("story_totals.commits", move || {
                    sweep_commits(&store, &home, now_ms(), spend)
                }),
                ask_waker(),
            )
            .await;
            commits_at = Some(tokio::time::Instant::now() + gap);
        }
        if swept.is_none_or(|at| at.elapsed() >= SESSION_SWEEP) {
            swept = Some(tokio::time::Instant::now());
            let (store, home, swept_for) =
                (store.clone(), home.clone(), std::sync::Arc::clone(&batch));
            if let Err(error) =
                crate::commands::await_blocking("story_totals.sessions", move || {
                    let store =
                        Result::unwrap_or_else(store.lock(), std::sync::PoisonError::into_inner);
                    // The batch's report lacks what the sweep just dequeued: those stories read
                    // their own.
                    let touched = sweep_sources_except(&store, &home, now_ms(), None)?;
                    story_changes::Batch::touch(&swept_for, touched);
                    Ok(())
                })
                .await
            {
                log::warn!(target: "story_totals", "sessions: {error}");
            }
        }
        let next = store.clone();
        let due_home = PathBuf::clone(&home);
        let marks = std::mem::take(&mut *context_marks().lock().unwrap_or_else(|e| e.into_inner()));
        let due = crate::commands::await_blocking("story_totals.next", move || {
            let store = next.lock().unwrap_or_else(|e| e.into_inner());
            let now = now_ms();
            let snapshot = APP
                .get()
                .and_then(|app| tauri::Manager::try_state::<crate::context_query::ContextIndexService>(app))
                .map(|index| index.snapshot());
            let reviewed = apply_context(&store, &due_home, marks, now, snapshot.as_deref())?;
            Ok((due(&store, &due_home, now)?, reviewed))
        })
        .await
        .unwrap_or_else(|error| {
            log::warn!(target: "story_totals", "next: {error}");
            (Due { cold: Vec::new(), changed: Vec::new() }, false)
        });
        // A review record changed: the rounds the batch read are out of date.
        if due.1 {
            story_changes::Batch::forget_rounds(&batch);
        }
        let due = due.0;
        let Some((job, was_changed)) = pick(due, changed_turn) else {
            // An idle spell clears the compute the next sweep would match.
            last_compute = Duration::ZERO;
            tokio::select! {
                _ = waker().notified() => {}
                _ = tokio::time::sleep(IDLE_TICK) => {}
            }
            continue;
        };
        // An ask that came while the worker was idle is served by this compute.
        let _ = tokio::time::timeout(Duration::ZERO, ask_waker().notified()).await;
        let computed_at = tokio::time::Instant::now();
        refresh_in(&home, &store, job, &batch).await;
        batched += 1;
        changed_turn = !was_changed;
        last_compute = tokio::time::Instant::elapsed(&computed_at);
        rest(last_compute, ask_waker()).await;
    }
}

/// One commit-sweep turn. A sweep that worked rests like a compute of the same length, so the
/// sweep and the computes share one budget; the result is how long until the next turn.
async fn sweep_turn(
    sweep: impl std::future::Future<Output = Result<story_changes::SweptCommits, String>>,
    wake: &tokio::sync::Notify,
) -> Duration {
    let started = tokio::time::Instant::now();
    let swept = sweep.await;
    match swept {
        Ok(done) if done.counted + done.reused + done.failed > 0 => {
            rest(tokio::time::Instant::elapsed(&started), wake).await;
            Duration::ZERO
        }
        Ok(_) => COMMIT_SWEEP,
        Err(error) => {
            log::warn!(target: "story_totals", "commits: {error}");
            COMMIT_SWEEP
        }
    }
}

/// How long the worker rests after a compute that took `computed`.
fn rest_after(computed: Duration) -> Duration {
    Ord::max(REST_GAP, computed * REST_FACTOR)
}

/// The rest after a compute: `REST_GAP`, then the rest of `rest_after` unless `wake` is notified
/// (a read queued a story, or asked an older one again), so an ask waits one `REST_GAP`, not the
/// whole rest. A board re-asking stories already queued, a change and the backlog itself never
/// notify it, so they keep the cap.
async fn rest(computed: Duration, wake: &tokio::sync::Notify) {
    tokio::time::sleep(REST_GAP).await;
    let extra = Duration::saturating_sub(rest_after(computed), REST_GAP);
    if !Duration::is_zero(&extra) {
        tokio::select! {
            _ = wake.notified() => {}
            _ = tokio::time::sleep(extra) => {}
        }
    }
}

/// The next job: the never-computed story, but every other turn a changed one so changes are not
/// starved by a long fill. True when it is a changed one.
fn pick(due: Due, changed_turn: bool) -> Option<(Job, bool)> {
    let (mut cold, mut changed) = (IntoIterator::into_iter(due.cold), IntoIterator::into_iter(due.changed));
    if changed_turn {
        if let Some(job) = Iterator::next(&mut changed) {
            return Some((job, true));
        }
    }
    if let Some(job) = Iterator::next(&mut cold) {
        return Some((job, false));
    }
    Option::map(Iterator::next(&mut changed), |job| (job, true))
}

/// What a test makes a story's compute do instead of its work.
#[cfg(test)]
#[derive(Clone, Copy)]
enum Inject {
    Hang,
    /// Blocks past its timeout, then computes for real.
    Slow,
    Fail,
}

#[cfg(test)]
fn injected() -> &'static Mutex<BTreeMap<String, Inject>> {
    static INJECTED: OnceLock<Mutex<BTreeMap<String, Inject>>> = OnceLock::new();
    INJECTED.get_or_init(Default::default)
}

#[cfg(test)]
async fn inject(story: &str) -> Result<(), String> {
    let found = injected().lock().unwrap().get(story).copied();
    match found {
        Some(Inject::Fail) => Err("injected failure".into()),
        // A blocking compute that outlives its timeout, as a long git walk does.
        Some(Inject::Hang) => {
            let _ = tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_millis(700))).await;
            Err("the hung compute ended".into())
        }
        Some(Inject::Slow) => {
            let _ = tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_millis(700))).await;
            Ok(())
        }
        None => Ok(()),
    }
}

/// How long a test waits before it gives up on a timed-out compute.
#[cfg(test)]
fn abandon_after() -> Duration {
    Duration::from_millis(300)
}

/// A hanging story times out at once in a test; every other story keeps the real timeout.
#[cfg(test)]
fn timeout_for(story: &str) -> Duration {
    match injected().lock().unwrap().get(story) {
        Some(Inject::Hang | Inject::Slow) => Duration::from_millis(100),
        _ => COMPUTE_TIMEOUT,
    }
}

#[cfg(test)]
mod tests;
