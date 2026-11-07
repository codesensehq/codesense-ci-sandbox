// SPDX-License-Identifier: MIT
//! Durable story contributions, shared by agents and pages.
mod agent;
mod cache;
mod commit_rows;
mod git;
mod line_share;
mod planned;
mod ranges;
mod review_rounds;
mod rows;
pub(crate) use commit_rows::{sweep as sweep_commits, StoryCommits, Swept as SweptCommits};
pub(crate) use rows::StoryRows;
pub(crate) use rows::current_cost_reason;
#[cfg(test)]
pub(crate) use rows::resolver_builds;

use super::{contribution, session_read, timing::TimeFacts, Filter, StoryResolver, UsageStore};
use crate::context_repo::caller_scope::CallerScope;
use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::Path,
};

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Params {
    /// Opt in to introduced-line attribution for the current commit page (private reads only).
    #[serde(default)]
    pub include: Vec<Include>,
    /// Story id, for example DEMO-29. Next: session_read, task_read for one session or task.
    pub story_id: String,
    /// Commit page size (default 100, at most 500). Every array pages by limit; tasks are
    /// oldest first. Totals cover every commit (`files` adds landed-diff paths).
    pub limit: Option<usize>,
    /// nextCursor from the previous page with the same story. Stale cursors are refused.
    /// `pages.<array>.cursor` resumes a truncated array.
    pub cursor: Option<String>,
    /// Return only these arrays (plus `totals`) and compute only what they need: `tasks`,
    /// `sessions` and `reviews` never read git. Omit for every array. Without a git array
    /// (`commits`, `prs`, `branches`, `worktrees`, `actual`) `totals.commits`, `files` and `lines`
    /// are null, with `unavailable.commits`, `files` and `lines` saying why, and a cursor is stale.
    pub only: Option<Vec<Only>>,
}

/// An array a read can be narrowed to (`only`).
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[schemars(inline)]
pub enum Only {
    Tasks,
    Sessions,
    Commits,
    Prs,
    Branches,
    Worktrees,
    Actual,
    /// The story's complete review-round markers: round, phase, scope, skill, subjectRev,
    /// high, medium, low, dismissed, converged and reportedAt. From the records, no git.
    Reviews,
}

/// The `only` reason for the totals a git-free read leaves out.
const NOT_READ: &str = "not read: this read has no git array (only)";

impl Params {
    fn asks_for(&self, array: Only) -> bool {
        Option::is_none_or(self.only.as_ref(), |only| <[Only]>::contains(only, &array))
    }

    /// Whether the read needs git: no `only`, or one of the arrays built from it.
    pub(crate) fn needs_git(&self) -> bool {
        let git = [Only::Commits, Only::Prs, Only::Branches, Only::Worktrees, Only::Actual];
        Iterator::any(&mut git.into_iter(), |array| self.asks_for(array))
    }

    /// Whether `reviews` was asked for by name (a read without `only` has none).
    fn asks_for_reviews(&self) -> bool {
        Option::is_some_and(self.only.as_ref(), |only| <[Only]>::contains(only, &Only::Reviews))
    }

    fn check_only(&self) -> Result<(), String> {
        match &self.only {
            Some(only) if Vec::is_empty(only) => {
                Err("invalid: only must name an array (omit it for all)".into())
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
#[schemars(inline)]
pub enum Include {
    Ranges,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoryChanges {
    pub source_repo: Option<String>,
    pub story_id: String,
    pub sessions: Vec<Value>,
    pub tasks: Vec<Value>,
    pub totals: Totals,
    pub branches: Vec<Value>,
    pub prs: Vec<Value>,
    pub worktrees: Vec<Value>,
    /// The story's complete review-round markers (`only: ["reviews"]`); null with
    /// `unavailable.reviews` when they cannot be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviews: Option<Vec<Value>>,
    pub commits: Vec<git::Commit>,
    pub total: usize,
    pub next_cursor: Option<String>,
    pub scan_limit: usize,
    pub planned: Option<Vec<String>>,
    pub planned_base: Option<String>,
    pub actual: Vec<planned::ActualFile>,
    pub planned_untouched: Vec<String>,
    pub unavailable: BTreeMap<String, String>,
    /// Facts that disagree, never a gate: `reviewRounds` when the review tasks and the
    /// review-rounds markers do.
    pub warnings: BTreeMap<&'static str, String>,
    /// The attached checkout's last CodeSense fetch and origin's default branch head; null
    /// when the story's source repo has no attached checkout.
    pub history_as_of: Option<Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    pub sessions: usize,
    pub commits: usize,
    pub files: usize,
    pub cost_usd: Option<f64>,
    pub wall_ms: Option<i64>,
    pub active_ms: Option<i64>,
    pub idle_ms: Option<i64>,
    /// The story's complete `phase: review` markers (`check_records.py` `review_rounds()`), or null
    /// with `unavailable.reviewRounds` when they cannot be read.
    pub review_rounds: Option<usize>,
    /// Distinct review tasks linked `relates_to:reviews` to a task of the story. With none typed,
    /// the tasks the review-rounds markers name (`warnings.reviewTasks` says so); null with
    /// `unavailable.reviewTasks` when rounds were recorded but name no task. Never a 0 that
    /// recorded rounds contradict.
    pub review_tasks: Option<usize>,
    /// AI/human/unattributed share of the lines added by the story's commits (null with
    /// `unavailable.lines` when any cannot be attributed).
    pub lines: Option<Value>,
}

/// The version of what `totals` computes: the last of [`TOTALS_STEPS`].
pub(crate) const TOTALS_VERSION: i64 = TOTALS_STEPS[<[(i64, Bump)]>::len(TOTALS_STEPS) - 1].0;

/// Every version of what `totals` computes, oldest first, each with what it changed. Append a step
/// with any change to the computation, to `Totals`, to the facets or to `GitTotals` (the M4 test
/// pins the published fields). The kind decides what a row computed before the step redoes: a new
/// field is an `Added` step, which never recounts every story.
pub(crate) const TOTALS_STEPS: &[(i64, Bump)] = &[
    // Versions 1 to 8 predate the kinds: a row computed by one of them is recomputed whole.
    (8, Bump::Whole),
    // Lines come from per-commit rows, and `linesInProgress` joins the facets: every story walks
    // once more, which writes its rows and queues the commits to count.
    (9, Bump::Added { half: super::story_totals::Half::Git, fields: &["linesInProgress"], fill: None }),
];

/// What one version step of the totals changed.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Bump {
    /// Adds `fields` (published ones, of `Totals` or the facets, with whatever the git half keeps
    /// for them) and changes no existing field. With `fill`, each stored row gets them from what
    /// it already holds, in place, and no story recomputes; without one, the half that owns them
    /// recomputes and the other half is kept. Either way a row keeps its values until then. A fill
    /// may run again on a row it filled.
    Added {
        half: super::story_totals::Half,
        fields: &'static [&'static str],
        fill: Option<fn(&mut super::story_totals::StoredTotals)>,
    },
    /// Changes what fields of one half mean: that half recomputes for every row, the other half's
    /// kept values stand.
    Changed(super::story_totals::Half),
    /// Changes the row's shape: every row recomputes whole.
    Whole,
}

/// What a row computed at one version redoes to reach the last of the steps, field by field and
/// half by half.
#[derive(Debug)]
pub(crate) struct Redo {
    /// The row recomputes whole: a `Whole` step, or a version the steps do not know.
    pub whole: bool,
    pub session: bool,
    pub git: bool,
    /// The fills of the `Added` steps since, oldest first.
    pub fills: Vec<fn(&mut super::story_totals::StoredTotals)>,
    /// The fields added since, with the half that owns each.
    pub added: Vec<(super::story_totals::Half, &'static str)>,
}

/// What a row computed at `version` (`None`: never stamped) redoes under `steps`.
pub(crate) fn redo_since(steps: &[(i64, Bump)], version: Option<i64>) -> Redo {
    use super::story_totals::Half;
    let mut redo = Redo {
        whole: false,
        session: false,
        git: false,
        fills: Vec::new(),
        added: Vec::new(),
    };
    let (first, last) = (
        Option::map(<[(i64, Bump)]>::first(steps), |s| s.0),
        Option::map(<[(i64, Bump)]>::last(steps), |s| s.0),
    );
    // A row never stamped, older than the kinds, or from a newer build recomputes whole.
    let version = match (version, first, last) {
        (Some(version), Some(first), Some(last)) if first <= version && version <= last => version,
        _ => {
            redo.whole = true;
            return redo;
        }
    };
    for (_, bump) in Iterator::filter(<[(i64, Bump)]>::iter(steps), |step| step.0 > version) {
        let half = match *bump {
            Bump::Whole => {
                redo.whole = true;
                continue;
            }
            Bump::Changed(half) => Some(half),
            Bump::Added { half, fields, fill } => {
                Vec::extend(
                    &mut redo.added,
                    Iterator::map(<[&str]>::iter(fields), |field| (half, *field)),
                );
                match fill {
                    Some(fill) => {
                        Vec::push(&mut redo.fills, fill);
                        None
                    }
                    None => Some(half),
                }
            }
        };
        match half {
            Some(Half::Session) => redo.session = true,
            Some(Half::Git) => redo.git = true,
            None => {}
        }
    }
    redo
}

/// The oldest version whose kept git half `steps` still accept: a row computed before it reads
/// git again.
pub(crate) fn git_kept_since(steps: &[(i64, Bump)]) -> i64 {
    let mut since = 0;
    for (version, bump) in steps {
        let git = match bump {
            Bump::Whole | Bump::Changed(super::story_totals::Half::Git) => true,
            Bump::Added {
                half: super::story_totals::Half::Git,
                fill: None,
                ..
            } => true,
            _ => false,
        };
        if git {
            since = *version;
        }
    }
    since
}

/// The reason a story's lines have while they are being counted and it had none before.
pub(crate) const COUNTING: &str = commit_rows::COUNTING;

/// The git half of a story's totals (commits, files, lines and their reasons), with what it was
/// read from. The totals index keeps it, so a change to a story's sessions or tasks alone is
/// recomputed without reading git.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitTotals {
    /// What the git read depended on: each session's branch, worktree and workspace.
    pub key: String,
    pub commits: usize,
    pub files: usize,
    pub lines: Option<Value>,
    /// Lines on branches that have not merged (`lines` once they do), counted like `lines`.
    #[serde(default)]
    pub lines_in_progress: Option<Value>,
    #[serde(default)]
    pub lines_in_progress_reason: Option<String>,
    /// Some commits are not counted yet: `lines` and `lines_in_progress` are the earlier ones.
    #[serde(default)]
    pub lines_pending: bool,
    pub unavailable: BTreeMap<String, String>,
    /// The git directories it was read from.
    pub repos: BTreeSet<std::path::PathBuf>,
    /// The branches it followed.
    pub branches: BTreeSet<String>,
    /// The newest merged PRs (at most `PRS_SHOWN`), as `{number, merged}` with `merged` the merge
    /// time (epoch ms, or null when unknown).
    #[serde(default)]
    pub prs: Vec<Value>,
    /// How many merged PRs there are in all.
    #[serde(default)]
    pub prs_total: usize,
    /// The latest merge time among them.
    #[serde(default)]
    pub merged_at: Option<i64>,
    /// The earliest and latest authored time among the commits (epoch ms).
    #[serde(default)]
    pub first_commit_at: Option<i64>,
    #[serde(default)]
    pub last_commit_at: Option<i64>,
}

/// Why a story has no activity times.
pub(crate) const NO_ACTIVITY: &str =
    "no session, commit, merge, task or review round is recorded for this story";

/// How many PRs a story's facets list.
pub(crate) const PRS_SHOWN: usize = 5;

/// The merged PRs newest first (an unknown merge time last), the first `PRS_SHOWN` of them as
/// `{number, merged}`, how many there are, and the latest merge time.
fn pr_facets(prs: &[Value]) -> (Vec<Value>, usize, Option<i64>) {
    let mut rows: Vec<(Option<i64>, Value)> = prs
        .iter()
        .map(|pr| {
            let merged = Value::as_i64(&pr["mergedAt"]);
            (merged, json!({"number": pr["number"], "merged": merged}))
        })
        .collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    let latest = Iterator::max(rows.iter().filter_map(|row| row.0));
    let total = Vec::len(&rows);
    (
        rows.into_iter().take(PRS_SHOWN).map(|row| row.1).collect(),
        total,
        latest,
    )
}

/// `reviews` asked for and unreadable: say why.
fn note_reviews(params: &Params, session: &SessionFacts, unavailable: &mut BTreeMap<String, String>) {
    if let (true, Err(reason)) = (params.asks_for_reviews(), &session.review_rows) {
        BTreeMap::insert(unavailable, "reviews".into(), (*reason).into());
    }
}

/// One usage report per scope shared by a batch of story computes: every session tied to a story,
/// read once with one resolver and bucketed by story, so a batch of N computes makes one pass over
/// the usage events instead of two per story. A story marked, or whose sessions a sweep changed,
/// after its scope's report was taken is stale: its compute takes that scope's report again for
/// every story, so K stale stories cost one more pass, not K, and a scope is taken again at most
/// once per `retake_gap_ms` (a stale story waits for the next window).
pub(crate) struct Batch {
    state: std::sync::Mutex<HashMap<String, ScopeReport>>,
    /// The review-rounds records of each team root, read once and shared by the batch's computes
    /// (and read again once they are older than `retake_gap_ms`, so a round recorded meanwhile is
    /// not missed for the rest of the batch).
    rounds: std::sync::Mutex<HashMap<(String, std::path::PathBuf), RoundsRead>>,
    retake_gap_ms: i64,
}

/// One team root's review-rounds records as a batch read them, and when.
struct RoundsRead {
    taken_at: i64,
    records: Option<std::sync::Arc<Vec<crate::records::Record>>>,
}

/// One scope's report in a batch.
struct ScopeReport {
    /// When the report was taken: a story marked since then is stale.
    taken_at: i64,
    /// The sessions by lower-cased story id, newest first as in the report.
    bucket: std::sync::Arc<HashMap<String, Vec<super::SessionUsage>>>,
    /// Stories (lower-cased ids) the report is stale for: marked, or their rows changed by a
    /// sweep, after it was taken.
    stale: BTreeSet<String>,
}

/// The least time between two takes of one scope's report.
const RETAKE_GAP_MS: i64 = 10_000;

impl Batch {
    pub(crate) fn new() -> Self {
        Batch {
            state: std::sync::Mutex::new(HashMap::new()),
            rounds: std::sync::Mutex::new(HashMap::new()),
            retake_gap_ms: RETAKE_GAP_MS,
        }
    }

    /// A batch whose scopes may be taken again this soon (tests).
    #[cfg(test)]
    pub(crate) fn with_retake_gap(retake_gap_ms: i64) -> Self {
        Batch {
            retake_gap_ms,
            ..Self::new()
        }
    }

    /// Move every scope's report `ms` into the past (tests).
    #[cfg(test)]
    pub(crate) fn age(&self, ms: i64) {
        for report in self.state().values_mut() {
            report.taken_at -= ms;
        }
        let mut rounds = Result::unwrap_or_else(
            std::sync::Mutex::lock(&self.rounds),
            std::sync::PoisonError::into_inner,
        );
        for read in HashMap::values_mut(&mut *rounds) {
            read.taken_at -= ms;
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, HashMap<String, ScopeReport>> {
        Result::unwrap_or_else(
            std::sync::Mutex::lock(&self.state),
            std::sync::PoisonError::into_inner,
        )
    }

    /// Whether `story` in scope `scope_key`, last marked at `dirty_at`, may compute at `now`. A
    /// mark since the scope's report was taken makes the story stale; a stale story waits while
    /// the report is younger than the gap; `Err` holds when the next window opens.
    pub(crate) fn admit(
        &self,
        scope_key: &str,
        story: &str,
        dirty_at: Option<i64>,
        now: i64,
    ) -> Result<(), i64> {
        let mut state = self.state();
        let Some(report) = HashMap::get_mut(&mut state, scope_key) else {
            return Ok(());
        };
        let story_key = str::to_ascii_lowercase(str::trim(story));
        if Option::is_some_and(dirty_at, |marked| marked >= report.taken_at) {
            BTreeSet::insert(&mut report.stale, String::clone(&story_key));
        }
        let ready = report.taken_at + self.retake_gap_ms;
        if BTreeSet::contains(&report.stale, &story_key) && now < ready {
            return Err(ready);
        }
        Ok(())
    }

    /// A sweep changed these stories' rows: that makes every report already taken stale for them.
    pub(crate) fn touch(&self, stories: BTreeSet<String>) {
        for report in HashMap::values_mut(&mut self.state()) {
            BTreeSet::extend(
                &mut report.stale,
                Iterator::cloned(BTreeSet::iter(&stories)),
            );
        }
    }

    /// A review record changed: the records read so far are out of date.
    pub(crate) fn forget_rounds(&self) {
        let mut rounds = Result::unwrap_or_else(
            std::sync::Mutex::lock(&self.rounds),
            std::sync::PoisonError::into_inner,
        );
        HashMap::clear(&mut *rounds);
    }

    /// The review-rounds records of team root `root` as `scope` may read them: listed once per
    /// batch, then shared. `None` when the root declares no such collection.
    fn review_records(
        &self,
        root: &Path,
        scope: &CallerScope,
    ) -> Result<Option<std::sync::Arc<Vec<crate::records::Record>>>, String> {
        let key = (ToString::to_string(&CallerScope::cache_key(scope)), Path::to_path_buf(root));
        let now = chrono::DateTime::timestamp_millis(&chrono::Utc::now());
        let held = {
            let rounds = Result::unwrap_or_else(
                std::sync::Mutex::lock(&self.rounds),
                std::sync::PoisonError::into_inner,
            );
            Option::map(
                Option::filter(HashMap::get(&rounds, &key), |read| {
                    now - read.taken_at < self.retake_gap_ms
                }),
                |read| Option::clone(&read.records),
            )
        };
        match held {
            Some(records) => Ok(records),
            None => {
                let read = Option::map(review_rounds::team_records(root, scope)?, std::sync::Arc::new);
                let mut rounds = Result::unwrap_or_else(
                    std::sync::Mutex::lock(&self.rounds),
                    std::sync::PoisonError::into_inner,
                );
                HashMap::insert(
                    &mut rounds,
                    key,
                    RoundsRead { taken_at: now, records: Option::clone(&read) },
                );
                Ok(read)
            }
        }
    }

    /// `story`'s sessions in `scope`, as `story_report` finds them.
    fn sessions(
        &self,
        store: &UsageStore,
        home: &Path,
        story: &str,
        scope: &CallerScope,
    ) -> Result<Vec<super::SessionUsage>, String> {
        let scope_key = ToString::to_string(&CallerScope::cache_key(scope));
        let story_key = str::to_ascii_lowercase(str::trim(story));
        // The scope's stale stories as the pass starts: the pass is current for them.
        let (held, stale) = {
            let state = self.state();
            match HashMap::get(&state, &scope_key) {
                Some(report) if BTreeSet::contains(&report.stale, &story_key) => {
                    (None, report.stale.clone())
                }
                Some(report) => (Some(std::sync::Arc::clone(&report.bucket)), BTreeSet::new()),
                None => (None, BTreeSet::new()),
            }
        };
        let bucket = match held {
            Some(bucket) => bucket,
            None => {
                let taken_at = chrono::DateTime::timestamp_millis(&chrono::Utc::now());
                let every: Filter = Default::default();
                let report = store.report_with(
                    &every,
                    &mut StoryResolver::with_scope(
                        StoryResolver::for_home(home, HashMap::new()),
                        scope.clone(),
                    ),
                )?;
                let mut bucket: HashMap<String, Vec<super::SessionUsage>> = HashMap::new();
                for usage in report.sessions {
                    if let Some(id) = Option::map(Option::as_ref(&usage.story), |s| {
                        str::to_ascii_lowercase(&s.id)
                    }) {
                        Vec::push(
                            std::collections::hash_map::Entry::or_default(HashMap::entry(
                                &mut bucket,
                                id,
                            )),
                            usage,
                        );
                    }
                }
                let bucket = std::sync::Arc::new(bucket);
                let mut state = self.state();
                // A story marked or swept during the pass may be missing from it: it stays stale.
                let mut left = Option::map_or(
                    HashMap::remove(&mut state, &scope_key),
                    BTreeSet::new(),
                    |old| old.stale,
                );
                BTreeSet::retain(&mut left, |story| !BTreeSet::contains(&stale, story));
                HashMap::insert(
                    &mut state,
                    scope_key,
                    ScopeReport {
                        taken_at,
                        bucket: std::sync::Arc::clone(&bucket),
                        stale: left,
                    },
                );
                bucket
            }
        };
        Ok(Option::unwrap_or_default(
            <Option<&Vec<super::SessionUsage>>>::cloned(HashMap::get(&bucket, &story_key)),
        ))
    }
}

/// One story-session discovery path for story contributions and task outcomes.
fn story_report(
    store: &UsageStore,
    home: &Path,
    story: &str,
    scope: &CallerScope,
) -> Result<super::UsageReport, String> {
    store.report_with(
        &Filter {
            story: Some(story.to_owned()),
            ..Default::default()
        },
        &mut StoryResolver::for_home(home, HashMap::new()).with_scope(scope.clone()),
    )
}

/// The branch names a trail read starts from for `story`: its sessions' recorded branches and
/// its live story worktrees' HEAD branches. The why_line walks start from the same names, so
/// every walk links the same merges.
pub(crate) fn story_branch_names(home: &Path, story: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    if let Ok(store) = UsageStore::open(&Path::join(home, "usage.sqlite")) {
        if let Ok(report) = story_report(&store, home, story, &CallerScope::Human) {
            for usage in report.sessions {
                let record = UsageStore::read_session_record(&store, &usage.session_id);
                if let Ok(Some(record)) = record {
                    if let Some(branch) = record.branch {
                        BTreeSet::insert(&mut names, branch);
                    }
                }
            }
        }
    }
    for principal in git::principals(home) {
        let records = crate::install::worktree_index::find_by_story_id(home, &principal, story);
        for record in Result::unwrap_or_default(records) {
            let Ok(repo) = git2::Repository::open(&record.code_repo_path) else {
                continue;
            };
            let Ok(head) = git2::Repository::head(&repo) else {
                continue;
            };
            if git2::Reference::is_branch(&head) {
                if let Ok(name) = git2::Reference::shorthand(&head) {
                    BTreeSet::insert(&mut names, str::to_owned(name));
                }
            }
        }
    }
    names
}

/// Read every matching session, with the same story resolution and session shape as session_usage
/// and session_read. Local refs supply commits; the story supplies planned touchpoints.
pub async fn read(
    home: &Path,
    params: &Params,
    scope: &CallerScope,
) -> Result<StoryChanges, String> {
    read_with(home, params, scope, false, None).await
}

/// [`read`], with `kept` the git half is reused while origin's head and the branch tips of the
/// repos it read are unchanged (SV-ENG-121: hosted reads, which a retry repeats).
async fn read_with(
    home: &Path,
    params: &Params,
    scope: &CallerScope,
    kept: bool,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<StoryChanges, String> {
    let story = params.story_id.trim();
    if str::is_empty(story) {
        return Err("invalid: storyId must not be empty".into());
    }
    params.check_only()?;
    // A cursor pages commits: without the commits array (or any git) it is stale.
    if Option::is_some(&params.cursor) && !params.asks_for(Only::Commits) {
        return Err(git::STALE_CURSOR.into());
    }
    let session = session_facts(home, story, scope, params.asks_for(Only::Tasks)).await?;
    let mut warnings = BTreeMap::new();
    if let Some(note) = session.review_tasks_note {
        BTreeMap::insert(&mut warnings, "reviewTasks", note.to_owned());
    }
    if let Some(mismatch) = session.review_warning.clone() {
        BTreeMap::insert(&mut warnings, "reviewRounds", mismatch);
    }
    let reviews = if params.asks_for_reviews() {
        Result::ok(session.review_rows.clone())
    } else {
        None
    };
    if !params.needs_git() {
        let (totals, mut unavailable) = combine(&session, &Default::default());
        for field in ["commits", "files", "lines"] {
            BTreeMap::insert(&mut unavailable, field.into(), NOT_READ.into());
        }
        note_reviews(params, &session, &mut unavailable);
        return Ok(StoryChanges {
            source_repo: None,
            story_id: str::to_owned(story),
            sessions: session.sessions,
            tasks: session.tasks,
            total: 0,
            totals,
            branches: Vec::new(),
            prs: Vec::new(),
            worktrees: Vec::new(),
            reviews,
            commits: Vec::new(),
            next_cursor: None,
            scan_limit: 0,
            planned: None,
            planned_base: None,
            actual: Vec::new(),
            planned_untouched: Vec::new(),
            unavailable,
            warnings,
            history_as_of: None,
        });
    }
    let refs: Vec<git::SessionRef> = Iterator::collect(Iterator::map(
        <[Value]>::iter(&session.sessions),
        git::SessionRef::of_read,
    ));
    let cache_key = cache::key(home, story, scope, &refs, &session.git_key);
    let kept_half = if kept { cache::get(&cache_key).await } else { None };
    let (git, planned, git_part) = match kept_half {
        Some(half) => half,
        None => {
            if git::cancelled(&cancel) {
                return Err("cancelled".into());
            }
            let (git, planned) = git_facts(home, story, scope, &refs, kept, cancel.clone()).await?;
            if git::cancelled(&cancel) {
                return Err("cancelled".into());
            }
            let git_part = git_totals(
                home,
                &git,
                &planned,
                session.git_key.clone(),
                None,
                cancel.clone(),
            )
            .await?;
            if kept && !git::cancelled(&cancel) {
                cache::put(cache_key, &git, &planned, &git_part).await;
            }
            (git, planned, git_part)
        }
    };
    let (totals, mut unavailable) = combine(&session, &git_part);
    note_reviews(params, &session, &mut unavailable);
    let history_as_of = git
        .attached
        .first()
        .and_then(|checkout| crate::attribution::history::HistorySource::at(checkout))
        .map(|source| source.history_as_of(home));
    let (commits, next_cursor) = ranges::page(home, params, scope, &git).await?;
    if params.include.contains(&Include::Ranges) && totals.commits == 0 {
        if let Some(reason) = BTreeMap::get(&git.unavailable, "commits") {
            BTreeMap::insert(&mut unavailable, "ranges".into(), (*reason).into());
        }
    }
    Ok(StoryChanges {
        source_repo: planned.source_repo,
        story_id: story.to_owned(),
        sessions: session.sessions,
        tasks: session.tasks,
        total: totals.commits,
        totals,
        branches: git.branches,
        prs: git.prs,
        worktrees: git.worktrees,
        reviews,
        commits,
        next_cursor,
        scan_limit: git.scan_limit,
        planned: planned.planned,
        planned_base: planned.planned_base,
        actual: planned.actual,
        planned_untouched: planned.planned_untouched,
        unavailable,
        warnings,
        history_as_of,
    })
}

/// One story's totals and unavailable reasons, as `read` computes them, with their git half.
/// `cached` is a git half computed earlier: it is reused when the sessions it was read for are
/// unchanged, so only the session half is recomputed. The story's lines are never counted here:
/// the walk writes the story's commit rows (counting none that was counted before), the commit
/// sweep counts the rest, and the lines are a fold of the rows. While some are not counted yet the
/// lines of `cached` stand (`lines_pending`); a git half whose reuse was refused (its git changed)
/// still lends its lines as `earlier`. `batch` shares one usage report among the computes of
/// a batch; without one, the compute's rows and git half share one of their own.
pub(crate) async fn totals(
    home: &Path,
    story: &str,
    scope: &CallerScope,
    cached: Option<GitTotals>,
    earlier: Option<GitTotals>,
    batch: Option<std::sync::Arc<Batch>>,
) -> Result<(Totals, BTreeMap<String, String>, Value, GitTotals), String> {
    let batch = Option::unwrap_or_else(batch, || std::sync::Arc::new(Batch::new()));
    let session = indexed_session_facts(home, story, scope, batch.clone()).await?;
    let key = session.git_key.clone();
    let before = Option::or(cached.clone(), earlier);
    let mut git_part = match cached.filter(|cached| cached.key == key) {
        Some(cached) => cached,
        None => {
            // The git walk reads each session's branch, worktree and workspace from the rows the
            // session half folded: no session is read again.
            let (git, planned) = git_facts(home, story, scope, &session.git_refs, false, None).await?;
            let mut part = git_base(&git, &planned, key);
            // What the story showed before stands while its commits are counted (unless its
            // history is unreadable, which makes the lines unavailable whatever it showed).
            if !part.unavailable.contains_key("commits") {
                part.lines = before.as_ref().and_then(|before| before.lines.clone());
                part.lines_in_progress = before.and_then(|before| before.lines_in_progress);
            }
            write_commit_rows(home, story, scope, &git).await?;
            part
        }
    };
    fold_commit_rows(home, story, scope, &mut git_part).await?;
    let (totals, unavailable) = combine(&session, &git_part);
    let facets = facets(&session, &git_part);
    Ok((totals, unavailable, facets, git_part))
}

/// Per home: story git reads (walks and whole-story line counts), walks alone, and commits whose
/// lines were read.
#[cfg(test)]
static GIT_READS: std::sync::Mutex<BTreeMap<std::path::PathBuf, [usize; 3]>> =
    std::sync::Mutex::new(BTreeMap::new());

/// How many times story code read git (`git::collect`, `line_share::totals`) for stories in
/// `home` (tests only). git2 runs in this process, so there is no subprocess to count.
#[cfg(test)]
pub(crate) fn git_reads(home: &Path) -> usize {
    counted(home, 0)
}

/// How many times the story walk (`git::collect_refs`) ran for stories in `home` (tests only).
#[cfg(test)]
pub(crate) fn git_walks(home: &Path) -> usize {
    counted(home, 1)
}

/// How many commits had their lines read (blamed or cache-read) for stories in `home` (tests only).
#[cfg(test)]
pub(crate) fn commit_line_reads(home: &Path) -> usize {
    counted(home, 2)
}

/// `home` as counted: canonical, since some reads take it from the usage store's path, which
/// SQLite resolves (a macOS temp dir under `/var` reads back as `/private/var`).
#[cfg(test)]
fn counter_key(home: &Path) -> std::path::PathBuf {
    home.canonicalize().unwrap_or_else(|_| home.to_owned())
}

#[cfg(test)]
fn counted(home: &Path, at: usize) -> usize {
    BTreeMap::get(&GIT_READS.lock().unwrap(), &counter_key(home)).map_or(0, |counts| counts[at])
}

#[cfg(test)]
fn count_git_read(home: &Path) {
    BTreeMap::entry(&mut GIT_READS.lock().unwrap(), counter_key(home)).or_default()[0] += 1;
}
#[cfg(not(test))]
fn count_git_read(_home: &Path) {}

/// The walk: it is a git read, and counted apart.
#[cfg(test)]
fn count_git_walk(home: &Path) {
    count_git_read(home);
    BTreeMap::entry(&mut GIT_READS.lock().unwrap(), counter_key(home)).or_default()[1] += 1;
}
#[cfg(not(test))]
fn count_git_walk(_home: &Path) {}

#[cfg(test)]
fn count_commit_line_read(home: &Path) {
    BTreeMap::entry(&mut GIT_READS.lock().unwrap(), counter_key(home)).or_default()[2] += 1;
}
#[cfg(not(test))]
fn count_commit_line_read(_home: &Path) {}

/// The session half: the story's sessions (and, for `read`, its task rows), cost, time and
/// review rounds. No git.
struct SessionFacts {
    sessions: Vec<Value>,
    tasks: Vec<Value>,
    count: usize,
    cost: f64,
    wall: Option<i64>,
    timed: bool,
    time: TimeFacts,
    /// Distinct review tasks linked to a task of the story, or the tasks its markers name.
    review_tasks: Option<usize>,
    /// Set when `review_tasks` was associated from the markers rather than typed.
    review_tasks_note: Option<&'static str>,
    /// The story's complete review-round markers, or the reason they cannot be read.
    review_rounds: Result<usize, &'static str>,
    /// Where the review tasks and the markers disagree (a fact, never a gate).
    review_warning: Option<String>,
    /// The story's review-round rows, or the reason the markers cannot be read.
    review_rows: Result<Vec<Value>, &'static str>,
    /// When the story was worked on, over its sessions, tasks and review rounds.
    activity: Activity,
    /// Sessions with observed timing and no end whose last activity is within [`OPEN_WINDOW_MS`]
    /// of the compute.
    open_sessions: usize,
    /// The CLIs that reported the sessions' usage (their ids).
    runtimes: BTreeSet<String>,
    /// What the git half reads from the sessions: each one's branch, worktree and workspace.
    git_key: String,
    /// The same fields, one per session, for the git half.
    git_refs: Vec<git::SessionRef>,
    unavailable: BTreeMap<&'static str, &'static str>,
}

/// The earliest and latest time (epoch ms) something happened on a story.
#[derive(Debug, Clone, Copy)]
struct Activity {
    first: Option<i64>,
    last: Option<i64>,
}

impl Activity {
    fn see(&mut self, at: Option<i64>) {
        if let Some(at) = at {
            self.first = Some(Option::map_or(self.first, at, |v| Ord::min(v, at)));
            self.last = Some(Option::map_or(self.last, at, |v| Ord::max(v, at)));
        }
    }

    /// This story's activity with its commits and merges.
    fn with_git(mut self, git: &GitTotals) -> Self {
        for at in [git.first_commit_at, git.last_commit_at, git.merged_at] {
            self.see(at);
        }
        self
    }
}

/// The fold's open count and activity span, for tests of the rows.
#[cfg(test)]
pub(crate) fn fold_summary(rows: &[contribution::Row], now: i64) -> (usize, Option<i64>, Option<i64>) {
    let fold = rows::fold(rows, now, false);
    (fold.open, fold.activity.first, fold.activity.last)
}

/// A session with no end counts as open while its last activity (usage or turn) is this recent.
const OPEN_WINDOW_MS: i64 = 15 * 60 * 1000;

/// The part of a story's facts that is not its sessions: its task rows and times, and its review
/// rounds. `with_tasks` adds each task's row and outcome, which the totals do not need.
struct TaskHalf {
    tasks: Vec<Value>,
    reviews: BTreeSet<String>,
    rounds: Result<Rounds, &'static str>,
    task_times: Vec<Option<i64>>,
}

async fn task_half(
    home: &Path,
    story: &str,
    scope: &CallerScope,
    with_tasks: bool,
    batch: Option<std::sync::Arc<Batch>>,
) -> Result<TaskHalf, String> {
    let (home_owned, story_owned, scope_owned) = (home.to_owned(), story.to_owned(), scope.clone());
    crate::commands::await_blocking("story_changes", move || {
        let store = UsageStore::open(&home_owned.join("usage.sqlite"))?;
        let mut tasks = Vec::new();
        let mut reviews = BTreeSet::new();
        let records = store.tasks_for_story(&story_owned)?;
        // The teams whose review records may hold the story's markers: its tasks' and its file's.
        let mut teams: BTreeSet<(String, String)> = records
            .iter()
            .filter_map(|task| Some((task.org.clone()?, task.team.clone()?)))
            .collect();
        Extend::extend(
            &mut teams,
            git::story_source(&home_owned, &store, &story_owned, &scope_owned).and_then(|source| {
                review_rounds::team_of_story_file(&home_owned, &source.org, &source.path)
            }),
        );
        let task_times: Vec<Option<i64>> = Iterator::collect(Iterator::flat_map(
            records.iter(),
            |task| [Some(task.created_at_ms), task.closed_at_ms],
        ));
        let evidence = if with_tasks {
            task_evidence(&store, &records)?
        } else {
            TaskEvidence::new()
        };
        for task in records {
            let ids = store.review_tasks_of(&task.task_id)?;
            reviews.extend(ids.iter().cloned());
            if !with_tasks {
                continue;
            }
            let outcome = store.read_outcome_with_evidence(&task, &evidence)?;
            let mut row = serde_json::to_value(task).map_err(|e| e.to_string())?;
            row["reviews"] = json!(ids);
            row["outcome"] = serde_json::to_value(outcome).map_err(|e| e.to_string())?;
            tasks.push(row);
        }
        let rounds =
            review_rounds_of(
                &home_owned,
                &store,
                &teams,
                &story_owned,
                &scope_owned,
                &reviews,
                batch.as_deref(),
            )?;
        Ok(TaskHalf { tasks, reviews, rounds, task_times })
    })
    .await
}

/// The story's sessions as `session_read` reads them, each once (the page's `sessions` and the
/// git half's input), and its session rows folded (the totals).
async fn session_facts(
    home: &Path,
    story: &str,
    scope: &CallerScope,
    with_tasks: bool,
) -> Result<SessionFacts, String> {
    let half = task_half(home, story, scope, with_tasks, None).await?;
    let (home_owned, story_owned, scope_owned) = (home.to_owned(), story.to_owned(), scope.clone());
    let (report, rows) = crate::commands::await_blocking("story_changes.sessions", move || {
        let store = UsageStore::open(&home_owned.join("usage.sqlite"))?;
        let report = story_report(&store, &home_owned, &story_owned, &scope_owned)?;
        let rows = rows::rows_from_sessions(&store, &report.sessions)?;
        Ok((report, rows))
    })
    .await?;
    let sessions = read_sessions(home, scope, report.sessions).await?;
    let rows: Vec<contribution::Row> = Iterator::collect(Iterator::map(IntoIterator::into_iter(rows), |(source, contribution)| {
        contribution::Row { source: (source.kind.to_owned(), source.id), contribution }
    }));
    let redirected = crate::session_usage::managed_telemetry::usage_reason().is_some();
    Ok(SessionFacts::new(half, rows::fold(&rows, chrono::Utc::now().timestamp_millis(), redirected), sessions))
}

/// The story's sessions as `session_read` reads them, each once.
async fn read_sessions(
    home: &Path,
    scope: &CallerScope,
    usages: Vec<super::SessionUsage>,
) -> Result<Vec<Value>, String> {
    let mut sessions = Vec::new();
    let mut seen = BTreeSet::new();
    for usage in usages {
        let basis = Option::unwrap_or(Option::and_then(Option::as_ref(&usage.story), |s| s.basis), "exact");
        let params: session_read::Params =
            serde_json::from_value(json!({"sessionId":usage.session_id}))
                .map_err(|e| e.to_string())?;
        let mut session = session_read::read(
            &home.join("usage.sqlite"),
            &home.join("session-conversations"),
            &params,
            scope,
        )
        .await?;
        let id = serde_json::Value::as_str(&session["session"]["sessionId"])
            .or_else(|| serde_json::Value::as_str(&session["nativeSessionIds"][0]))
            .unwrap_or(&usage.session_id)
            .to_owned();
        if !seen.insert(id) {
            continue;
        }
        session["basis"] = json!(basis);
        sessions.push(session);
    }
    Ok(sessions)
}

/// The session half from the story's stored session rows (no `session_read` per session), built
/// whole from the sessions when the story has none for this version of a row.
async fn indexed_session_facts(
    home: &Path,
    story: &str,
    scope: &CallerScope,
    batch: std::sync::Arc<Batch>,
) -> Result<SessionFacts, String> {
    let half = task_half(home, story, scope, false, Some(std::sync::Arc::clone(&batch))).await?;
    let (home_owned, story_owned, scope_owned) = (home.to_owned(), story.to_owned(), scope.clone());
    let rows = crate::commands::await_blocking("story_changes.rows", move || {
        use contribution::EntityAdapter;
        let store = UsageStore::open(&home_owned.join("usage.sqlite"))?;
        let (scope_key, entity) = (scope_owned.cache_key().to_string(), story_owned.to_lowercase());
        // Sources queued since the last sweep (a session whose usage or timing just changed) are
        // applied first, so the rows this compute folds are current; this story is not marked for
        // what the compute itself picks up.
        let now = chrono::Utc::now().timestamp_millis();
        let computing: (&str, &str) = (&scope_key, &entity);
        match super::story_totals::sweep_sources_except(&store, &home_owned, now, Some(computing)) {
            Ok(touched) => Batch::touch(&batch, touched),
            Err(error) => log::debug!(target: "story_totals", "queued sources: {error}"),
        }
        let version = <rows::StoryRows as EntityAdapter>::VERSION;
        let kind = <rows::StoryRows as EntityAdapter>::KIND;
        if let Some(rows) = contribution::rows_of(&store, &scope_key, kind, &entity, version)? {
            return Ok(rows);
        }
        let sessions = Batch::sessions(&batch, &store, &home_owned, &story_owned, &scope_owned)?;
        let built = rows::rows_from_sessions(&store, &sessions)?;
        let now = chrono::Utc::now().timestamp_millis();
        contribution::replace_entity(&store, &scope_key, kind, &entity, &built, version, now)?;
        Ok(Option::unwrap_or_default(contribution::rows_of(&store, &scope_key, kind, &entity, version)?))
    })
    .await?;
    let redirected = crate::session_usage::managed_telemetry::usage_reason().is_some();
    Ok(SessionFacts::new(half, rows::fold(&rows, chrono::Utc::now().timestamp_millis(), redirected), vec![]))
}

impl SessionFacts {
    /// The session half from its parts: the task half, the sessions' fold, and the sessions'
    /// own JSON (empty when only the totals are wanted).
    fn new(half: TaskHalf, fold: rows::SessionFold, sessions: Vec<Value>) -> Self {
        let TaskHalf {
            tasks,
            reviews,
            rounds,
            task_times,
        } = half;
        let rows::SessionFold {
            count,
            cost,
            wall,
            timed,
            time,
            runtimes,
            mut unavailable,
            mut activity,
            open,
            git_key,
            refs,
        } = fold;
        for at in task_times {
            activity.see(at);
        }
        let review_rows = match &rounds {
            Ok((_, _, _, rows)) => Ok(rows.clone()),
            Err(reason) => Err(*reason),
        };
        if let Ok(rows) = &review_rows {
            for row in rows {
                activity.see(Value::as_i64(&row["reportedAt"]));
            }
        }
        let (review_tasks, review_tasks_note) = review_task_count(&reviews, &rounds);
        if Option::is_none(&review_tasks) {
            let reason = if Result::is_err(&rounds) { REVIEW_TASKS_UNREAD } else { REVIEW_TASKS_UNNAMED };
            BTreeMap::insert(&mut unavailable, "reviewTasks", reason);
        }
        SessionFacts {
            count,
            runtimes,
            sessions,
            tasks,
            cost,
            wall,
            timed,
            time,
            review_tasks,
            review_tasks_note,
            review_warning: Option::and_then(Result::ok(rounds.clone()), |(_, warning, _, _)| warning),
            review_rounds: Result::map(rounds, |(count, _, _, _)| count),
            review_rows,
            activity,
            open_sessions: open,
            git_key,
            git_refs: refs,
            unavailable,
        }
    }
}

/// The story's git facts and planned touchpoints: the part of a story's facts that reads git.
async fn git_facts(
    home: &Path,
    story: &str,
    scope: &CallerScope,
    sessions: &[git::SessionRef],
    stamp: bool,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<(git::GitFacts, planned::PlannedFacts), String> {
    let (home_git, story_git, sessions_git, scope_git) = (
        home.to_owned(),
        story.to_owned(),
        sessions.to_vec(),
        scope.clone(),
    );
    crate::commands::await_blocking("story_changes.git", move || {
        let store = UsageStore::open(&home_git.join("usage.sqlite"))?;
        let collect = || {
            git::collect_refs(
                &home_git,
                &store,
                &story_git,
                &sessions_git,
                std::slice::from_ref(&scope_git),
                stamp,
                cancel.clone(),
            )
        };
        let mut git = collect()?;
        let mut fetch_reason = None;
        if trail_needs_fetch(&git) {
            use crate::attribution::history::{HistorySource, TRAIL_FETCH_BOUND};
            let sources: Vec<_> = git.attached.iter().filter_map(|c| HistorySource::at(c)).collect();
            // One bound for the whole trail open, however many checkouts it has.
            let deadline = std::time::Instant::now() + TRAIL_FETCH_BOUND;
            // Only a checkout whose fetch fully landed may be remembered as a miss.
            let mut complete = Vec::new();
            for source in &sources {
                if source.trail_fetch_spent(&home_git, &story_git) {
                    fetch_reason = fetch_reason.or(Some(crate::attribution::history::FETCH_SPENT));
                    continue;
                }
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    fetch_reason = Some(crate::attribution::history::FETCH_TIMED_OUT);
                    break;
                }
                match source.fetch(&home_git, Some(std::string::String::as_str(&story_git)), left) {
                    Ok(done) => match done.partial {
                        Some(reason) => fetch_reason = Some(reason),
                        None => complete.push(source),
                    },
                    Err(reason) => fetch_reason = Some(reason),
                }
            }
            if !std::vec::Vec::is_empty(&complete) || fetch_reason.is_some() {
                git = collect()?;
            }
            // Still missing at origin's current head: the next open reads locally until it moves.
            if trail_needs_fetch(&git) {
                for source in complete {
                    source.remember_trail_miss(&home_git, &story_git);
                }
            }
        }
        if let Some(reason) = fetch_reason {
            git.unavailable.insert("historyFetch", reason);
        }
        let files = git
            .commits
            .iter()
            .flat_map(|c| c.files.iter().cloned())
            .chain(git.lines.keys().cloned())
            .collect::<BTreeSet<_>>();
        let source = git::story_source(&home_git, &store, &story_git, &scope_git);
        let mut planned = planned::read(
            source
                .as_ref()
                .map(|source| std::path::PathBuf::as_path(&source.path)),
            &files,
        );
        for file in &mut planned.actual {
            file.stats = Some(git.line_stats(&file.path));
        }
        Ok((git, planned))
    })
    .await
}

/// The git half of the totals, from the git facts, with the story's lines counted whole (the read
/// path, which has no rows to fold).
async fn git_totals(
    home: &Path,
    git: &git::GitFacts,
    planned: &planned::PlannedFacts,
    key: String,
    budget: Option<std::time::Duration>,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<GitTotals, String> {
    let mut part = git_base(git, planned, key);
    match line_share::totals_with(home, git, budget, cancel).await {
        Ok(lines) => part.lines = Some(serde_json::to_value(lines).map_err(|e| e.to_string())?),
        Err(reason) => {
            BTreeMap::insert(&mut part.unavailable, "lines".into(), reason.into());
        }
    }
    Ok(part)
}

/// The git half without lines: its commits, files, PRs, branches and the reasons the walk gave.
fn git_base(git: &git::GitFacts, planned: &planned::PlannedFacts, key: String) -> GitTotals {
    let mut unavailable: BTreeMap<String, String> = git
        .unavailable
        .iter()
        .chain(planned.unavailable.iter())
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    // Incomplete history: the lines are unavailable, whatever the rows hold.
    if let Some(reason) = git.unavailable.get("commits") {
        BTreeMap::insert(&mut unavailable, "lines".into(), (*reason).into());
    }
    let (prs, prs_total, merged_at) = pr_facets(&git.prs);
    let times: Vec<i64> = Iterator::collect(Iterator::map(<[git::Commit]>::iter(&git.commits), |commit| commit.authored_at));
    GitTotals {
        first_commit_at: Iterator::min(<[i64]>::iter(&times).copied()),
        last_commit_at: Iterator::max(<[i64]>::iter(&times).copied()),
        prs,
        prs_total,
        merged_at,
        key,
        commits: Vec::len(&git.commits),
        files: Vec::len(&planned.actual),
        lines: None,
        lines_in_progress: None,
        lines_in_progress_reason: None,
        lines_pending: false,
        unavailable,
        repos: git.repos.clone(),
        branches: git
            .branches
            .iter()
            .filter_map(|branch| Value::as_str(&branch["name"]).map(str::to_owned))
            .collect(),
    }
}

/// Write the story's commit rows for a walk (none when the walk could not read the history: its
/// lines are unavailable whatever the commits hold).
async fn write_commit_rows(home: &Path, story: &str, scope: &CallerScope, git: &git::GitFacts) -> Result<(), String> {
    if git.unavailable.contains_key("commits") {
        return Ok(());
    }
    let (home, story, scope_key) = (home.to_owned(), story.to_lowercase(), scope.cache_key().to_string());
    let git = git.clone();
    crate::commands::await_blocking("story_changes.commit_rows", move || {
        let store = UsageStore::open(&home.join("usage.sqlite"))?;
        commit_rows::write(&store, &scope_key, &story, &git, chrono::Utc::now().timestamp_millis())
    })
    .await
}

/// Set the part's lines and the lines on unmerged branches from the story's commit rows. A story
/// whose rows are not written (history unavailable, or never walked in this version) keeps what
/// the part has. Counting still going on keeps the lines the part had, flagged, or says so.
async fn fold_commit_rows(home: &Path, story: &str, scope: &CallerScope, part: &mut GitTotals) -> Result<(), String> {
    if part.unavailable.contains_key("commits") {
        return Ok(());
    }
    let (home, story, scope_key) = (home.to_owned(), story.to_lowercase(), scope.cache_key().to_string());
    let rows = crate::commands::await_blocking("story_changes.commit_fold", move || {
        let store = UsageStore::open(&home.join("usage.sqlite"))?;
        commit_rows::rows(&store, &scope_key, &story)
    })
    .await?;
    let Some(rows) = rows else {
        return Ok(());
    };
    let fold = commit_rows::fold(&rows);
    let value = |lines: &line_share::Lines| serde_json::to_value(lines).map_err(|e| e.to_string());
    part.lines_pending = false;
    BTreeMap::remove(&mut part.unavailable, "lines");
    part.lines_in_progress_reason = None;
    match &fold.lines {
        commit_rows::Share::Lines(lines) => part.lines = Some(value(lines)?),
        commit_rows::Share::Unknown(reason) => {
            part.lines = None;
            BTreeMap::insert(&mut part.unavailable, "lines".into(), reason.clone());
        }
        commit_rows::Share::Pending => {
            part.lines_pending = true;
            if Option::is_none(&part.lines) {
                BTreeMap::insert(&mut part.unavailable, "lines".into(), commit_rows::COUNTING.into());
            }
        }
    }
    match &fold.in_progress {
        commit_rows::Share::Lines(lines) => part.lines_in_progress = Some(value(lines)?),
        commit_rows::Share::Unknown(reason) => {
            part.lines_in_progress = None;
            part.lines_in_progress_reason = Some(reason.clone());
        }
        commit_rows::Share::Pending => {
            part.lines_pending = true;
            if Option::is_none(&part.lines_in_progress) {
                part.lines_in_progress_reason = Some(commit_rows::COUNTING.into());
            }
        }
    }
    Ok(())
}

/// A story's facets beside its totals: its latest merge time (null with `mergedAtReason`), its
/// newest merged PRs with their count, and the CLIs that reported its sessions' usage.
fn facets(session: &SessionFacts, git: &GitTotals) -> Value {
    let mut out = json!({
        "mergedAt": git.merged_at,
        "prs": git.prs,
        "prsTotal": git.prs_total,
        "runtimes": session.runtimes,
    });
    // When the story was worked on, and how many sessions are still running.
    let activity = session.activity.with_git(git);
    out["firstActivityAt"] = json!(activity.first);
    out["lastActivityAt"] = json!(activity.last);
    out["openSessions"] = json!(session.open_sessions);
    out["linesInProgress"] = json!(git.lines_in_progress);
    if let Some(reason) = Option::as_ref(&git.lines_in_progress_reason) {
        out["linesInProgressReason"] = json!(reason);
    }
    if git.lines_pending {
        out["linesRefreshing"] = json!(true);
    }
    if Option::is_none(&activity.first) {
        out["activityReason"] = json!(NO_ACTIVITY);
    }
    if git.merged_at.is_none() {
        out["mergedAtReason"] = json!(if git.prs_total == 0 {
            "no merged PR found locally"
        } else {
            "no merge time for the merged PRs"
        });
    }
    out
}

/// The totals and their reasons from the two halves: the one place a story's totals are made.
fn combine(session: &SessionFacts, git: &GitTotals) -> (Totals, BTreeMap<String, String>) {
    let mut unavailable: BTreeMap<String, String> = session
        .unavailable
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    if let Err(reason) = session.review_rounds {
        BTreeMap::insert(&mut unavailable, "reviewRounds".into(), reason.into());
    }
    Extend::extend(&mut unavailable, git.unavailable.clone());
    let totals = Totals {
        sessions: session.count,
        commits: git.commits,
        files: git.files,
        cost_usd: if BTreeMap::contains_key(&unavailable, "costUsd") {
            None
        } else {
            Some(session.cost)
        },
        // `sessions` holds the page's session JSON, which an indexed (totals-only) read leaves out:
        // the count says whether there are none.
        wall_ms: if session.count == 0 {
            Some(0)
        } else {
            session.wall
        },
        active_ms: session.timed.then_some(session.time.active_ms),
        idle_ms: session.timed.then_some(session.time.idle_ms),
        review_rounds: Result::ok(session.review_rounds),
        review_tasks: session.review_tasks,
        lines: git.lines.clone(),
    };
    (totals, unavailable)
}

/// The story's review rounds: how many markers, how its review tasks disagree with them, the
/// tasks they name, and the rows a page shows.
type Rounds = (usize, Option<String>, BTreeSet<String>, Vec<Value>);

/// The story's review rounds from its markers, and how its review tasks disagree with them.
fn review_rounds_of(
    home: &Path,
    store: &UsageStore,
    teams: &BTreeSet<(String, String)>,
    story: &str,
    scope: &CallerScope,
    review_tasks: &BTreeSet<String>,
    batch: Option<&Batch>,
) -> Result<Result<Rounds, &'static str>, String> {
    let found = match batch {
        Some(batch) => review_rounds::story_markers_from(home, teams, story, &mut |root| {
            batch.review_records(root, scope)
        }),
        None => review_rounds::story_markers(home, teams, story, scope),
    };
    let (markers, rows) = match found {
        Ok(found) => found,
        Err(reason) => return Ok(Err(reason)),
    };
    let mut closed = BTreeSet::new();
    for id in review_tasks {
        if store
            .read_task(id)?
            .is_some_and(|t| t.status != super::task::OPEN)
        {
            closed.insert(id.clone());
        }
    }
    let mut types = BTreeMap::new();
    for task in markers.iter().filter_map(|m| m.task.as_deref()) {
        if let Some(t) = store.read_task(task)? {
            types.insert(task.to_owned(), t.task_type);
        }
    }
    let named = Iterator::collect(Iterator::filter_map(
        <[review_rounds::Marker]>::iter(&markers),
        |m| Option::clone(&m.task),
    ));
    Ok(Ok((
        markers.len(),
        review_rounds::mismatch(&markers, &closed, &types),
        named,
        rows,
    )))
}

const REVIEW_TASKS_UNNAMED: &str =
    "review rounds are recorded but no review task is typed and no marker names a task";
const REVIEW_TASKS_UNREAD: &str =
    "no review task is typed and the review-rounds markers cannot be read to associate one";

/// The story's review task count and, when it is associated, why. Typed review tasks win; with
/// none, the tasks the complete markers name stand in for them (sessions launched before review
/// launches were typed). Recorded rounds that name no task, or markers that cannot be read, leave
/// the count unknown, not 0.
fn review_task_count(
    typed: &BTreeSet<String>,
    rounds: &Result<Rounds, &'static str>,
) -> (Option<usize>, Option<&'static str>) {
    if !BTreeSet::is_empty(typed) {
        return (Some(BTreeSet::len(typed)), None);
    }
    match rounds {
        Ok((_, _, named, _)) if !BTreeSet::is_empty(named) => (
            Some(BTreeSet::len(named)),
            Some("associated from the review-rounds markers: no task is typed as a review task"),
        ),
        Ok((count, _, _, _)) if *count > 0 => (None, None),
        Ok(_) => (Some(0), None),
        Err(reason) if review_rounds::is_absent(reason) => (Some(0), None),
        Err(_) => (None, None),
    }
}

/// Whether `story`'s file, found the way its trail finds it (the caller's scope; durable
/// links first, then the orgs it may read), says it has landed.
pub(crate) fn story_is_done(home: &Path, story: &str, scope: &CallerScope) -> bool {
    Result::ok(UsageStore::open(&home.join("usage.sqlite")))
        .and_then(|store| git::story_source(home, &store, story.trim(), scope))
        .is_some_and(|source| git::source_is_done(&source.path))
}

/// A landed story's trail opened without its history: the merge scan stopped at the shallow
/// boundary, or no merge is on origin's default branch here. Only then does opening it fetch
/// (bounded), once per origin head; a story still in progress reads its branch locally.
fn trail_needs_fetch(git: &git::GitFacts) -> bool {
    use crate::attribution::ranges::{HISTORY_PAST_SHALLOW, HISTORY_PAST_SHALLOW_PARTIAL};
    git.story_done
        && !std::vec::Vec::is_empty(&git.attached)
        && (std::vec::Vec::is_empty(&git.prs)
            || matches!(git.unavailable.get("commits"), Some(&HISTORY_PAST_SHALLOW | &HISTORY_PAST_SHALLOW_PARTIAL)))
}

/// The relative-file disclosure boundary shared with published projections.
pub(crate) fn repo_relative(path: &str) -> bool {
    planned::relative(path)
}

type TaskScope = (Option<String>, Option<String>);
pub(crate) type TaskEvidence = BTreeMap<String, StoryEvidence>;

/// Request-owned facts; planned enrichment is still selected by each task's original scope.
pub(crate) struct StoryEvidence {
    git: git::GitFacts,
    sources: BTreeMap<TaskScope, Option<std::path::PathBuf>>,
}

fn task_scope_key(task: &super::task::TaskRecord) -> TaskScope {
    (task.org.clone(), task.team.clone())
}
fn task_scope(home: &Path, task: &super::task::TaskRecord) -> CallerScope {
    match (task.org.as_deref(), task.team.as_deref()) {
        (Some(org), Some(team)) => git::principals(home)
            .into_iter()
            .find_map(|principal| {
                let root = std::result::Result::ok(crate::context_repo::path_to_org_root(
                    home, &principal, org,
                ))?;
                root.is_dir().then(|| {
                    CallerScope::team(
                        home,
                        &principal,
                        &crate::commands::dashboard_launch::LaunchTeam {
                            org: org.into(),
                            team: team.into(),
                        },
                    )
                })
            })
            .unwrap_or(CallerScope::Unknown),
        _ => CallerScope::Unknown,
    }
}
fn task_links_evidence(
    store: &UsageStore,
    task: &super::task::TaskRecord,
) -> Result<(Vec<Value>, BTreeSet<String>), String> {
    let mut sessions = Vec::new();
    let mut ids = BTreeSet::new();
    for link in store
        .task_links(&task.task_id)?
        .into_iter()
        .filter(|l| l.kind == super::task::LINK_SESSION)
    {
        let identity = session_read::resolve_session(store, None, &link.target)?;
        ids.extend(store.event_session_ids(&link.target)?);
        if let Some(record) = identity.record {
            sessions.push(json!({"session":record}));
        }
    }
    if let Some(path) = &task.worktree {
        sessions.push(json!({"session":{"worktree":path}}));
    }
    Ok((sessions, ids))
}

/// Gather all candidate worktrees before collecting each story once for this request.
pub(crate) fn task_evidence(
    store: &UsageStore,
    tasks: &[super::task::TaskRecord],
) -> Result<TaskEvidence, String> {
    let home = rusqlite::Connection::path(&store.conn)
        .map(Path::new)
        .and_then(Path::parent)
        .ok_or("usage store has no home")?;
    let mut groups: BTreeMap<String, Vec<&super::task::TaskRecord>> =
        std::collections::BTreeMap::new();
    for task in tasks {
        if let Some(story) = &task.story_id {
            groups.entry(story.clone()).or_default().push(task);
        }
    }
    let mut evidence: TaskEvidence = std::collections::BTreeMap::new();
    for (story, tasks) in groups {
        let mut sessions = Vec::new();
        let mut scopes: BTreeMap<TaskScope, CallerScope> = std::collections::BTreeMap::new();
        for task in tasks {
            scopes
                .entry(task_scope_key(task))
                .or_insert_with(|| task_scope(home, task));
            sessions.extend(task_links_evidence(store, task)?.0);
        }
        // Existing story discovery also finds follow-up branches belonging to other tasks.
        for scope in scopes.values() {
            for usage in story_report(store, home, &story, scope)?.sessions {
                if let Some(record) = store.read_session_record(&usage.session_id)? {
                    sessions.push(json!({"session":record}));
                }
            }
        }
        let admitted = scopes.values().cloned().collect::<Vec<_>>();
        let git = git::collect(home, store, &story, &sessions, &admitted)?;
        let sources = scopes
            .into_iter()
            .map(|(key, scope)| {
                (
                    key,
                    git::story_source(home, store, &story, &scope).map(|source| source.path),
                )
            })
            .collect();
        evidence.insert(story, StoryEvidence { git, sources });
    }
    Ok(evidence)
}

/// Select task facts from this request's story evidence, without recursive outcome reads.
pub(crate) fn task_facts(
    store: &UsageStore,
    task: &super::task::TaskRecord,
    evidence: &TaskEvidence,
) -> Result<(planned::PlannedFacts, Value), String> {
    let Some(story) = task.story_id.as_deref() else {
        let mut facts = planned::read(None, &BTreeSet::new());
        facts.unavailable.insert("planned", "task has no story");
        facts.basis = Some("unknown");
        return Ok((
            facts,
            json!({"commits":null,"files":null,"unavailable":"task has no story"}),
        ));
    };
    let story = evidence
        .get(story)
        .ok_or("task story evidence not collected")?;
    let git = &story.git;
    let (_, ids) = task_links_evidence(store, task)?;
    let selected: Vec<_> = git
        .commits
        .iter()
        .filter(|c| {
            if !std::vec::Vec::is_empty(&c.session_ids) {
                c.session_ids.iter().any(|id| ids.contains(id))
            } else {
                c.authored_at >= task.created_at_ms
                    && task.closed_at_ms.is_none_or(|end| c.authored_at <= end)
            }
        })
        .collect();
    let files = selected
        .iter()
        .flat_map(|c| c.files.iter().cloned())
        .collect::<BTreeSet<_>>();
    let source = story
        .sources
        .get(&task_scope_key(task))
        .and_then(std::option::Option::as_deref);
    let mut planned = planned::read(source, &files);
    planned.basis = Some(
        if std::vec::Vec::is_empty(&selected)
            || std::iter::Iterator::any(&mut selected.iter(), |c| c.basis == "unknown")
        {
            "unknown"
        } else if std::iter::Iterator::all(&mut selected.iter(), |c| {
            !std::vec::Vec::is_empty(&c.session_ids)
        }) {
            "exact"
        } else {
            "associated"
        },
    );
    if let Some(reason) = git.unavailable.get("commits") {
        planned.unavailable.insert("actual", reason);
    }
    if let Some(reason) = std::iter::Iterator::find_map(&mut selected.iter(), |c| c.unavailable) {
        planned.unavailable.insert("attribution", reason);
    }
    let first_review = store
        .review_tasks_of(&task.task_id)?
        .iter()
        .map(|id| store.read_task(id))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .filter_map(|r| r.closed_at_ms)
        .min();
    let rework = if let Some(reason) = git.unavailable.get("commits") {
        json!({"commits":null,"files":null,"unavailable":reason})
    } else if let Some(closed) = first_review {
        let rework: Vec<_> = git
            .commits
            .iter()
            .filter(|c| c.authored_at > closed && c.files.iter().any(|f| files.contains(f)))
            .collect();
        let files: BTreeSet<_> = rework
            .iter()
            .flat_map(|c| c.files.iter())
            .filter(|p| files.contains(*p))
            .collect();
        json!({"commits":rework.len(),"files":files})
    } else {
        json!({"commits":null,"files":null,"unavailable":"no closed review task"})
    };
    Ok((planned, rework))
}

pub async fn current(params: &Params, scope: &CallerScope) -> Result<Value, String> {
    current_with(params, scope, false, None).await
}

/// [`current`] for a hosted page: its git half is kept per story until origin's head or a
/// branch tip moves.
pub(crate) async fn current_kept(
    params: &Params,
    scope: &CallerScope,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<Value, String> {
    current_with(params, scope, true, Some(cancel)).await
}

async fn current_with(
    params: &Params,
    scope: &CallerScope,
    kept: bool,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<Value, String> {
    let home = crate::commands::paths::codesense_home_dir().ok_or("no CodeSense home directory")?;
    let mut value = serde_json::to_value(read_with(&home, params, scope, kept, cancel).await?)
        .map_err(|e| e.to_string())?;
    prune(&mut value, params);
    Ok(value)
}

/// The keys each array of `only` owns in an answer.
pub(crate) const OWNED: [(Only, &[&str]); 8] = [
    (Only::Tasks, &["tasks"]),
    (Only::Sessions, &["sessions"]),
    (Only::Commits, &["commits", "total", "nextCursor", "scanLimit"]),
    (Only::Prs, &["prs"]),
    (Only::Branches, &["branches"]),
    (Only::Worktrees, &["worktrees"]),
    (Only::Actual, &["actual", "planned", "plannedBase", "plannedUntouched"]),
    (Only::Reviews, &["reviews"]),
];

/// The answer keys only a git read gives (beside `OWNED`'s git arrays).
pub(crate) const GIT_ONLY: [&str; 2] = ["sourceRepo", "historyAsOf"];

/// An `only` answer holds the arrays asked for, `totals`, `unavailable` and `warnings`: what
/// was not asked for is left out, never answered as empty. Without git the totals that only
/// git gives are null, with their reasons in `unavailable`.
fn prune(value: &mut Value, params: &Params) {
    let Some(answer) = Value::as_object_mut(value) else {
        return;
    };
    if Option::is_none(&params.only) {
        return;
    }
    for (array, keys) in OWNED {
        if !params.asks_for(array) {
            for key in keys {
                serde_json::Map::remove(answer, *key);
            }
        }
    }
    if !params.needs_git() {
        for key in GIT_ONLY {
            serde_json::Map::remove(answer, key);
        }
        if let Some(totals) =
            Option::and_then(serde_json::Map::get_mut(answer, "totals"), Value::as_object_mut)
        {
            for key in ["commits", "files", "lines"] {
                serde_json::Map::insert(totals, key.into(), Value::Null);
            }
        }
    }
}

pub use agent::{agent_current, AgentParams};
#[cfg(test)]
pub(crate) use agent::{agent_projection, agent_read_params};

#[cfg(test)]
mod tests;
