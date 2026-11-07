// SPDX-License-Identifier: MIT
//! The story-commits kind of the contribution store: one `commit` row per sha of a story, holding
//! the lines that commit added (counted once per sha, however many stories or scopes share it).
//! The story walk owns which commits a story has and in what state; the commit sweep owns only the
//! counts, filling rows the walk wrote. A story's lines are a fold of its rows, so a change to one
//! commit costs one commit's read and a story's walk costs none.
use super::line_share::{self, Counter, Lines, Stop};
use super::*;
use crate::session_usage::contribution::{self, Contribution, EntityAdapter, Of, Op, Queued, Reducer, Row, Source};
use serde_json::Map;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub(super) const COMMIT: &str = "commit";

/// The reason while a story's lines are still being counted and it has none from before.
pub(crate) const COUNTING: &str = "counting in the background";

/// A commit that fails this many times is given up on: its lines are unavailable.
const MAX_ATTEMPTS: i64 = 5;
/// The wait after the first failure, doubled by each further one up to the cap.
const BACKOFF_MS: i64 = 300_000;
const BACKOFF_CAP_MS: i64 = 21_600_000;

/// How a story holds a commit: through a merged PR, through a story branch that may still be open,
/// or only through a branch proven never to merge while other PRs of the story did.
const LANDED: &str = "landed";
const BRANCH: &str = "branch";
const SIDE: &str = "side";

/// The story-commits kind.
pub(crate) struct StoryCommits;

impl EntityAdapter for StoryCommits {
    const KIND: &'static str = "story_commits";
    /// Bump it with any change to what a commit row holds.
    const VERSION: i64 = 1;
    const SOURCES: &'static [&'static str] = &[COMMIT];
    const REDUCERS: &'static [Reducer] = &[Reducer { out: "commits", op: Op::Count, of: Of::Row, source: COMMIT }];
}

/// The note blobs a commit's attribution depends on, one per notes ref that has one: a commit's
/// lines change only with its diff and these, so it is the reuse key (never the ref's tip, which
/// moves with every commit anywhere in the repository).
fn note_key(opened: &mut OpenRepos, repos: &[PathBuf], sha: &str) -> Option<String> {
    let oid = git2::Oid::from_str(sha).ok()?;
    for path in repos {
        let Some(repo) = opened.get(path) else {
            continue;
        };
        let parts: Vec<String> = crate::attribution::git_ai::NOTES_REFS
            .iter()
            .filter_map(|name| {
                let note = git2::Repository::find_note(repo, Some(&format!("refs/notes/{name}")), oid).ok()?;
                Some(format!("{name}={}", git2::Note::id(&note)))
            })
            .collect();
        if !Vec::is_empty(&parts) {
            return Some(parts.join(";"));
        }
    }
    None
}

/// The repositories a pass has opened, each once (a story's commits share a few).
struct OpenRepos(Vec<(PathBuf, Option<git2::Repository>)>);

impl OpenRepos {
    fn get(&mut self, path: &Path) -> Option<&git2::Repository> {
        let at = match Iterator::position(&mut self.0.iter(), |(opened, _)| opened == path) {
            Some(at) => at,
            None => {
                self.0.push((path.to_owned(), Result::ok(git2::Repository::open(path))));
                self.0.len() - 1
            }
        };
        Option::as_ref(&self.0[at].1)
    }
}

/// A story's commit rows from its walk: every commit uncounted, with its state and where to read it.
fn walked_rows(facts: &git::GitFacts) -> Vec<(Source, Contribution)> {
    facts
        .commits
        .iter()
        .map(|commit| {
            let repos: Vec<String> = facts
                .repo_paths
                .get(&commit.sha)
                .map(|paths| paths.iter().map(|p| p.to_string_lossy().into_owned()).collect())
                .unwrap_or_default();
            let mut row = Contribution::new();
            row.first_ms = Some(commit.authored_at);
            row.last_ms = Some(commit.authored_at);
            let state = if commit.landed {
                LANDED
            } else if commit.unlanded {
                SIDE
            } else {
                BRANCH
            };
            row.extra.insert("state".into(), json!(state));
            row.extra.insert("landedLines".into(), json!(facts.landed_lines));
            row.extra.insert("files".into(), json!(commit.files));
            row.extra.insert("repos".into(), json!(repos));
            row.extra.insert("noteKey".into(), Value::Null);
            row.extra.insert("counted".into(), json!(false));
            (Source { kind: COMMIT, id: commit.sha.clone() }, row)
        })
        .collect()
}

fn counted(row: &Contribution) -> bool {
    row.extra.get("counted") == Some(&json!(true))
}

/// A counted row for `sha` made under the same notes (`key`), in any story or scope. A row that
/// gave up on a transient failure, or that holds a reason from where it was looked for rather than
/// from the commit, is not one: a later walk counts the commit again.
fn reusable(conn: &rusqlite::Connection, sha: &str, key: &Option<String>) -> Result<Option<Contribution>, String> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT first_ms, last_ms, nums, reasons, extra FROM contribution
             WHERE entity_kind = ?1 AND source_kind = ?2 AND source_id = ?3",
        )
        .map_err(crate::session_usage::db_err)?;
    let found = stmt
        .query_map(rusqlite::params![StoryCommits::KIND, COMMIT, sha], |r| contribution::decode_from(r, 0))
        .map_err(crate::session_usage::db_err)?;
    for row in found {
        let row = row.map_err(crate::session_usage::db_err)??;
        let gave_up = Lines::load(&row).is_err_and(|reason| line_share::is_transient(&reason) || line_share::is_environmental(&reason));
        if counted(&row) && !gave_up && row.extra.get("noteKey") == Some(&json!(key)) {
            return Ok(Some(row));
        }
    }
    Ok(None)
}

/// Write a story's commit rows for the walk it just made, in one transaction: every commit keeps
/// its count when the same sha has been counted under the same notes (here or in any story), and
/// each other commit is queued for the sweep. Rows of commits the walk no longer reaches go.
pub(super) fn write(store: &UsageStore, scope_key: &str, story: &str, facts: &git::GitFacts, now: i64) -> Result<(), String> {
    let tx = rusqlite::Transaction::new_unchecked(&store.conn, rusqlite::TransactionBehavior::Immediate)
        .map_err(crate::session_usage::db_err)?;
    let mut rows = walked_rows(facts);
    let mut queue = vec![];
    let mut opened = OpenRepos(vec![]);
    for (source, row) in &mut rows {
        let repos: Vec<PathBuf> = facts.repo_paths.get(&source.id).cloned().unwrap_or_default();
        let key = note_key(&mut opened, &repos, &source.id);
        match reusable(&tx, &source.id, &key)? {
            Some(done) => {
                row.nums = done.nums;
                row.reasons = done.reasons;
                row.extra.insert("noteKey".into(), json!(key));
                row.extra.insert("counted".into(), json!(true));
            }
            None => queue.push(source.clone()),
        }
    }
    contribution::replace_entity_in(&tx, scope_key, StoryCommits::KIND, story, &rows, StoryCommits::VERSION, now)?;
    for source in &queue {
        contribution::enqueue(&tx, source, now)?;
    }
    rusqlite::Transaction::commit(tx).map_err(crate::session_usage::db_err)
}

/// What a story's lines are, from its commit rows.
#[derive(Debug, PartialEq)]
pub(super) enum Share {
    Lines(Lines),
    /// Some commit cannot be read: the reason.
    Unknown(String),
    /// Some commit is not counted yet.
    Pending,
}

/// The story's lines and the lines still on unmerged branches, from its commit rows.
#[derive(Debug, PartialEq)]
pub(super) struct Fold {
    pub lines: Share,
    pub in_progress: Share,
}

fn share<'a>(rows: impl Iterator<Item = &'a Row>) -> Share {
    let mut sum = Lines::ZERO;
    let (mut unknown, mut pending) = (None, false);
    for row in rows {
        if !counted(&row.contribution) {
            pending = true;
            continue;
        }
        match Lines::load(&row.contribution) {
            Ok(lines) => sum.add(&lines),
            Err(reason) => unknown = unknown.or(Some(reason)),
        }
    }
    match (unknown, pending) {
        (Some(reason), _) => Share::Unknown(reason),
        (None, true) => Share::Pending,
        (None, false) => Share::Lines(sum),
    }
}

pub(super) fn fold(rows: &[Row]) -> Fold {
    let state = |row: &Row| row.contribution.extra.get("state").and_then(Value::as_str).unwrap_or(LANDED).to_owned();
    // The old whole-story count leaves a side branch's commits out when merged PRs exist.
    let landed_lines = rows.iter().any(|row| row.contribution.extra.get("landedLines") == Some(&json!(true)));
    Fold {
        lines: share(rows.iter().filter(|row| state(row) != SIDE || !landed_lines)),
        in_progress: share(rows.iter().filter(|row| state(row) != LANDED)),
    }
}

/// The story's commit rows, when the walk has written them for this version.
pub(super) fn rows(store: &UsageStore, scope_key: &str, story: &str) -> Result<Option<Vec<Row>>, String> {
    contribution::rows_of(store, scope_key, StoryCommits::KIND, story, StoryCommits::VERSION)
}

/// What a sweep did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Swept {
    pub counted: usize,
    pub reused: usize,
    pub failed: usize,
    /// Commits still queued and due.
    pub left: bool,
}

/// Live commits counted in a row before one backfill commit gets its turn.
const LIVE_RUN: usize = 3;

/// The live picks since the last backfill pick, kept across the sweeps of one run.
static LIVE_STREAK: AtomicUsize = AtomicUsize::new(0);

type CountFn<'a> = &'a mut dyn FnMut(&str, &[PathBuf]) -> Result<Lines, &'static str>;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Count the due commits until `spend` is used up, finishing the story with the fewest commits
/// left first ([`next_due`]). Each commit is read without the store locked, so the index is not
/// held up by a blame; `ceiling` bounds one commit's read. `mark` runs in the fill's transaction
/// with the `(scope, story)` pairs whose last commit it filled.
pub(crate) fn sweep(
    store: &Mutex<UsageStore>,
    home: &Path,
    now: i64,
    (spend, ceiling): (Duration, Duration),
    mark: &dyn Fn(&rusqlite::Connection, &[(String, String)]) -> Result<(), String>,
) -> Result<Swept, String> {
    let mut counter: Option<Counter> = None;
    let mut count = |sha: &str, repos: &[PathBuf]| {
        let counter = counter.get_or_insert_with(|| Counter::new(home));
        let commit = git::Commit { sha: sha.to_owned(), ..Default::default() };
        let candidates = Iterator::collect(std::iter::once((sha.to_owned(), repos.to_vec())));
        let stop = Stop { deadline: Some(Instant::now() + ceiling), cancel: None };
        counter.commit_lines_of(home, &commit, &candidates, &stop)
    };
    let started = Instant::now();
    sweep_timed(store, now, spend, &|| Instant::elapsed(&started), &LIVE_STREAK, mark, &mut count)
}

pub(super) fn sweep_with(
    store: &Mutex<UsageStore>,
    now: i64,
    spend: Duration,
    mark: &dyn Fn(&rusqlite::Connection, &[(String, String)]) -> Result<(), String>,
    count: CountFn,
) -> Result<Swept, String> {
    let started = Instant::now();
    sweep_timed(store, now, spend, &|| Instant::elapsed(&started), &AtomicUsize::new(0), mark, count)
}

/// [`sweep_with`] with the time spent read from `spent`, so a test can run it on a mock clock.
pub(super) fn sweep_timed(
    store: &Mutex<UsageStore>,
    now: i64,
    spend: Duration,
    spent: &dyn Fn() -> Duration,
    live_streak: &AtomicUsize,
    mark: &dyn Fn(&rusqlite::Connection, &[(String, String)]) -> Result<(), String>,
    count: CountFn,
) -> Result<Swept, String> {
    let mut swept = Swept::default();
    loop {
        let next = {
            let store = lock(store);
            next_due(&store, now, live_streak)?
        };
        let Some((entry, quiet)) = next else {
            return Ok(swept);
        };
        if spent() >= spend {
            swept.left = true;
            return Ok(swept);
        }
        // Only a commit this sweep takes counts toward the live run.
        live_streak.store(if quiet { 0 } else { live_streak.load(Ordering::Relaxed) + 1 }, Ordering::Relaxed);
        let sha = entry.source.id.clone();
        let stored = {
            let store = lock(store);
            stored_repos(&store, &sha)?
        };
        // A commit no walk keeps any more has nothing to fill; its queue row just goes.
        let Some(repos) = stored else {
            fill(store, &entry, &Contribution::new(), now, mark)?;
            continue;
        };
        let key = note_key(&mut OpenRepos(vec![]), &repos, &sha);
        let reused = {
            let store = lock(store);
            reusable(&store.conn, &sha, &key)?
        };
        let mut row = Contribution::new();
        if let Some(done) = reused {
            row.nums = done.nums;
            row.reasons = done.reasons;
            swept.reused += 1;
        } else {
            match count(&sha, &repos) {
                Ok(lines) => Lines::store(&Ok(lines), &mut row),
                Err(reason) if line_share::is_transient(reason) => {
                    let store = lock(store);
                    let attempts = contribution::fail(&store, &entry, now, &|n| {
                        contribution::backoff_ms(n, BACKOFF_MS, BACKOFF_CAP_MS)
                    })?;
                    swept.failed += 1;
                    if Option::is_none(&attempts) || attempts < Some(MAX_ATTEMPTS) {
                        continue;
                    }
                    drop(store);
                    Lines::store(&Err(line_share::UNREADABLE), &mut row);
                }
                Err(reason) => Lines::store(&Err(reason), &mut row),
            }
            swept.counted += 1;
        }
        row.extra.insert("noteKey".into(), json!(key));
        row.extra.insert("counted".into(), json!(true));
        fill(store, &entry, &row, now, mark)?;
    }
}

/// The queued commit to count next: of the commits due, one of a story that really changed lately
/// (live work, as the story totals' own recompute order has it) before any quiet backfill, then the
/// one in the story with the fewest commits still uncounted (oldest queued first among equals), so
/// stories finish one after another instead of every story staying nearly done. A queued commit
/// no story keeps comes last, to have its queue row cleared.
///
/// Live work cannot starve the backfill: after `LIVE_RUN` live picks in a row (`live_streak`, which
/// outlives one sweep) the next pick is the quiet backfill's best, then live goes first again, so a
/// live story waits for at most one backfill commit.
fn next_due(store: &UsageStore, now: i64, live_streak: &AtomicUsize) -> Result<Option<(Queued, bool)>, String> {
    let backfill_turn = i64::from(live_streak.load(Ordering::Relaxed) >= LIVE_RUN);
    let mut stmt = store
        .conn
        .prepare_cached(
            "SELECT e.source_kind, e.source_id, e.enqueued_ms, COALESCE(t.changed_ms, 0) < ?4 FROM source_event e
             JOIN contribution c ON c.entity_kind = ?1 AND c.source_kind = e.source_kind AND c.source_id = e.source_id
             JOIN (SELECT scope_key, entity_id, COUNT(*) AS remaining FROM contribution
                   WHERE entity_kind = ?1 AND source_kind = ?2 AND COALESCE(json_extract(extra, '$.counted'), 0) != 1
                   GROUP BY scope_key, entity_id) r ON r.scope_key = c.scope_key AND r.entity_id = c.entity_id
             LEFT JOIN story_totals t ON t.scope_key = c.scope_key AND t.story_key = c.entity_id
             WHERE e.source_kind = ?2 AND COALESCE(e.not_before_ms, 0) <= ?3
             ORDER BY ABS(?5 - (COALESCE(t.changed_ms, 0) < ?4)), r.remaining, e.enqueued_ms, e.rowid LIMIT 1",
        )
        .map_err(crate::session_usage::db_err)?;
    let kept = rusqlite::OptionalExtension::optional(stmt.query_row(rusqlite::params![StoryCommits::KIND, COMMIT, now, now - crate::session_usage::story_totals::NOTES_ACTIVE_MS, backfill_turn], |r| {
        Ok((Queued { source: Source { kind: COMMIT, id: r.get(1)? }, enqueued_ms: r.get(2)? }, r.get::<_, bool>(3)?))
    }))
    .map_err(crate::session_usage::db_err)?;
    match kept {
        Some(found) => Ok(Some(found)),
        None => Ok(contribution::queued_due(store, StoryCommits::SOURCES, 1, now, false)?.into_iter().next().map(|entry| (entry, true))),
    }
}

/// How many commits of a story are not counted yet.
fn uncounted(conn: &rusqlite::Connection, scope: &str, story: &str) -> Result<i64, String> {
    conn.prepare_cached(
        "SELECT COUNT(*) FROM contribution WHERE scope_key = ?1 AND entity_kind = ?2 AND entity_id = ?3
         AND source_kind = ?4 AND COALESCE(json_extract(extra, '$.counted'), 0) != 1",
    )
    .map_err(crate::session_usage::db_err)?
    .query_row(rusqlite::params![scope, StoryCommits::KIND, story, COMMIT], |r| r.get(0))
    .map_err(crate::session_usage::db_err)
}

/// Fill a commit's rows; `mark` hears only of the stories this fill completed: a story with
/// commits left has nothing new to show, and a recompute then would only repeat "counting".
fn fill(
    store: &Mutex<UsageStore>,
    entry: &Queued,
    row: &Contribution,
    now: i64,
    mark: &dyn Fn(&rusqlite::Connection, &[(String, String)]) -> Result<(), String>,
) -> Result<(), String> {
    let store = lock(store);
    let finished = |conn: &rusqlite::Connection, filled: &[(String, String)]| {
        let mut done = vec![];
        for pair in filled {
            if uncounted(conn, &pair.0, &pair.1)? == 0 {
                done.push(pair.clone());
            }
        }
        if Vec::is_empty(&done) { Ok(()) } else { mark(conn, &done) }
    };
    contribution::fill(&finished, &store, StoryCommits::KIND, entry, row, now).map(|_| ())
}

/// Where to read a stored commit: the git directories every story's walk found it in; none when no story has it.
fn stored_repos(store: &UsageStore, sha: &str) -> Result<Option<Vec<PathBuf>>, String> {
    let mut stmt = store
        .conn
        .prepare_cached("SELECT extra FROM contribution WHERE entity_kind = ?1 AND source_kind = ?2 AND source_id = ?3")
        .map_err(crate::session_usage::db_err)?;
    let extras = stmt
        .query_map(rusqlite::params![StoryCommits::KIND, COMMIT, sha], |r| r.get::<_, String>(0))
        .map_err(crate::session_usage::db_err)?;
    let mut found = false;
    let mut repos: Vec<PathBuf> = vec![];
    for extra in extras {
        found = true;
        let extra: Map<String, Value> = serde_json::from_str(&extra.map_err(crate::session_usage::db_err)?).map_err(|e| e.to_string())?;
        for path in extra.get("repos").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
            let path = PathBuf::from(path);
            if !repos.contains(&path) {
                repos.push(path);
            }
        }
    }
    Ok(found.then_some(repos))
}

#[cfg(test)]
mod tests;
