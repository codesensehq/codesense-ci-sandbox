// SPDX-License-Identifier: MIT
//! A task's end and its outcome: the facts next to the targets, never a score.
//!
//! A task closes on `signal_done`, at a routine run's end, or through `task_update`. Time, cost
//! and models are read live from the session record, timing and usage through the task's
//! session links, so there is no second copy. At close the tamper fact is worked out from git:
//! whether files the policy calls tests or checks were edited during the task.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusqlite::params;
use serde::Serialize;
use serde_json::{json, Value};

use super::record::git_lines;
use super::task::{
    Tamper, TaskLink, TaskRecord, ABANDONED, DONE, LINK_RELATES_PREFIX, LINK_SESSION,
    EARLY_CLOSE, CAPTURE_PENDING,
};
use super::{db_err, Filter, UsageStore};

/// The status a verdict closes a task as: a success, or a routine run that ended well
/// (`succeeded`, `delivered`, `ended`), is done; any other verdict is abandoned, and the raw
/// verdict is kept on the task.
pub fn status_for(raw: &str) -> &'static str {
    if matches!(raw, "success" | "succeeded" | "delivered" | "ended") {
        DONE
    } else {
        ABANDONED
    }
}

/// The one close operation every terminal path calls: `signal_done`, a headless process exit, a
/// routine run's end or cancellation, and `task_update`. It closes a task that is still open as
/// `status`, keeping `raw_status`, and then works out its tamper fact. The first close stands for
/// both the status and the time: a later call changes nothing. Returns whether this call closed it.
pub fn close_as(
    store: &UsageStore,
    task_id: &str,
    status: &str,
    raw_status: &str,
    now_ms: i64,
) -> Result<bool, String> {
    let closed = store.close_task(task_id, status, Some(raw_status), now_ms)?;
    if let Some(task) = store.read_task(task_id)? {
        if task.created_by.as_deref() != Some(EARLY_CLOSE)
            && (closed || task.unavailable.get("tamper").is_some_and(|why| why == CAPTURE_PENDING))
        {
            // The close already published a pending reason. Git holds no database lock;
            // the conditional follow-up cannot replace evidence another writer finished.
            store.finish_task_tamper(task_id, &compute_tamper(&task))?;
        }
    }
    Ok(closed)
}

/// [`close_as`] for a verdict (`success`, `failed`, a routine phase, ...): `success` is done and
/// any other verdict is abandoned, with the raw verdict kept.
pub fn finish_task(
    store: &UsageStore,
    task_id: &str,
    raw_status: &str,
    now_ms: i64,
) -> Result<bool, String> {
    close_as(store, task_id, status_for(raw_status), raw_status, now_ms)
}

/// Close the tasks a session ran: its own, and the routine run's when the session ran one.
/// Returns the ids this call closed.
pub fn close_for_session(
    store: &UsageStore,
    session_id: &str,
    raw_status: &str,
    now_ms: i64,
) -> Result<Vec<String>, String> {
    let run = match store.read_session_record(session_id)?.and_then(|r| r.run_id) {
        Some(run) if store.read_task(&run)?.is_some() => Some(run),
        _ => None,
    };
    let mut ids = Vec::new();
    if run.is_none() || store.read_task(session_id)?.is_some() {
        ids.push(session_id.to_string());
    }
    ids.extend(run);
    let mut closed = Vec::new();
    for id in ids {
        if finish_task(store, &id, raw_status, now_ms)? {
            closed.push(id);
        }
    }
    Ok(closed)
}

/// Retain an ending whose caller attests a scheduled capture and supplies its task identity.
/// The existing task row is visibly pending until that capture fills its metadata once.
fn prepare_terminal_task(
    store: &UsageStore,
    task_id: &str,
    session: Option<&str>,
    now_ms: i64,
) -> Result<(), String> {
    let record = session.map(|session| store.read_session_record(session)).transpose()?.flatten();
    let mut task = TaskRecord::new(task_id, "Task capture pending");
    task.created_at_ms = now_ms;
    task.created_by = Some(EARLY_CLOSE.into());
    task.org = record.as_ref().and_then(|r| r.org.clone());
    task.team = record.as_ref().and_then(|r| r.team.clone());
    for field in ["goal", "story", "subject"] {
        task.field_sources.insert(field.into(), "pending".into());
    }
    for key in ["goal", "type", "tamper"] {
        task.unavailable.insert(key.into(), CAPTURE_PENDING.into());
    }
    store.insert_task_if_absent(&task)?;
    if let Some(session) = session {
        store.add_task_link(task_id, LINK_SESSION, session)?;
    }
    Ok(())
}

/// The usage store that sits beside a routine coordinator file (`<home>/orchestration/…`), so
/// a routine's end closes its task in the store of the same CodeSense home.
pub(super) fn usage_store_beside(coordinator: &Path) -> Option<PathBuf> {
    let home = coordinator.parent()?.parent()?;
    (coordinator.parent()?.file_name()? == "orchestration").then(|| home.join("usage.sqlite"))
}

/// A routine run ended: close its task in the home's usage store. Best effort.
pub fn close_run_task(coordinator: &Path, run_id: &str, raw_status: &str) {
    let Some(path) = usage_store_beside(coordinator) else {
        return;
    };
    let result = UsageStore::open(&path).and_then(|store| {
        let now = now_ms();
        // The durable coordinator start attests a scheduled capture, just as the launch
        // registry does for session endings. Queued/unstarted runs cannot leave placeholders.
        if store.read_task(run_id)?.is_none() && coordinator.exists() {
            let runs = crate::routines::state::RoutineStateStore::open_at(coordinator)
                .map_err(|e| e.to_string())?;
            if let Some(run) = runs.run(run_id).map_err(|e| e.to_string())?
                .filter(|run| run.started_at.is_some())
            {
                prepare_terminal_task(&store, run_id, run.session_id.as_deref(), now)?;
            }
        }
        finish_task(&store, run_id, raw_status, now)
    });
    if let Err(e) = result {
        log::warn!(target: "session_usage", "routine task {run_id} not closed: {e}");
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// [`close_for_session`] in the usage store at `path`: the body every close path persists through.
pub fn close_for_session_at(
    path: &Path,
    session_id: &str,
    raw_status: &str,
    now_ms: i64,
) -> Result<Vec<String>, String> {
    close_for_session(&UsageStore::open(path)?, session_id, raw_status, now_ms)
}

/// A session ended with `raw_status`: close its task in the supplied store, off the async
/// runtime. Best effort; both signals and process exits resolve the same store at their seam.
pub async fn close_for_session_async(
    path: Result<PathBuf, String>,
    session_id: &str,
    raw_status: &str,
    task_id: Option<String>,
) {
    let (session, status) = (session_id.to_string(), raw_status.to_string());
    let result = tokio::task::spawn_blocking(move || {
        let path = path?;
        if let Some(task_id) = task_id {
            let store = UsageStore::open(&path)?;
            let now = now_ms();
            prepare_terminal_task(&store, &task_id, Some(&session), now)?;
            finish_task(&store, &task_id, &status, now).map(|_| ())
        } else {
            close_for_session_at(&path, &session, &status, now_ms()).map(|_| ())
        }
    })
    .await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => log::warn!(target: "session_usage", "task not closed: {e}"),
        Err(e) => log::warn!(target: "session_usage", "task close failed: {e}"),
    }
}

/// A wake delivered a routine run's text to `session`: link the run's task to it, in the usage
/// store beside the coordinator. Best effort.
pub fn link_run_session(coordinator: &Path, run_id: &str, session: &str) {
    let Some(path) = usage_store_beside(coordinator) else {
        return;
    };
    let result = UsageStore::open(&path)
        .and_then(|store| store.add_task_link(run_id, LINK_SESSION, session));
    if let Err(e) = result {
        log::warn!(target: "session_usage", "routine task {run_id} not linked to {session}: {e}");
    }
}

/// Whether tests or checks were edited during the task: the files changed since the commit the
/// task started at (committed, staged, unstaged or new) that match the policy's tamper globs.
pub fn compute_tamper(task: &TaskRecord) -> Result<Tamper, String> {
    if task.tamper_globs.is_empty() {
        return Err("the team policy has no tamper_globs".into());
    }
    let dir = task
        .worktree
        .as_deref()
        .ok_or("the task has no working folder")?;
    let start = task
        .layers
        .source
        .as_deref()
        .ok_or("the commit the task started at was not recorded")?;
    let dir = Path::new(dir);
    let mut set = globset::GlobSetBuilder::new();
    for glob in &task.tamper_globs {
        set.add(globset::Glob::new(glob).map_err(|e| format!("tamper glob '{glob}': {e}"))?);
    }
    let set = set.build().map_err(|e| format!("tamper globs: {e}"))?;
    let range = format!("{start}..HEAD");
    let mut changed = std::collections::BTreeSet::new();
    for args in [
        &["diff", "--name-only", range.as_str()][..],
        &["diff", "--name-only", "HEAD"],
        &["ls-files", "--others", "--exclude-standard"],
    ] {
        let mut full = vec!["-c", "core.quotepath=off"];
        full.extend_from_slice(args);
        changed.extend(git_lines(dir, &full).ok_or_else(|| {
            "git could not list the files changed (the start commit may be gone)".to_string()
        })?);
    }
    let files: Vec<String> = changed.into_iter().filter(|f| set.is_match(f)).collect();
    Ok(Tamper {
        flag: !files.is_empty(),
        files,
    })
}

/// One model call (or, for a whole session, its aggregate): what it cost and moved.
struct Call {
    models: Vec<String>,
    cost_usd: Option<f64>,
    input: u64,
    output: u64,
}

/// A target next to what happened, as facts. `actual` is absent when nothing measured it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetFact {
    pub name: String,
    pub target: Value,
    pub actual: Option<Value>,
}

pub use super::timing::TimeFacts;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CostFacts {
    pub cost_usd: Option<f64>,
    pub models: Vec<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// What a task came to, next to what it was to reach. Time and cost are summed over the
/// task's sessions; planned files and rework use the shared story contribution reader.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskOutcome {
    /// What `time` and `cost` cover: `session` (the whole session, subtasks included) or
    /// `interval` (a subtask's own start-to-close span).
    pub scope: String,
    pub planned_vs_actual: Option<Value>,
    pub rework: Option<Value>,
    pub time: Option<TimeFacts>,
    pub cost: Option<CostFacts>,
    /// Review tasks that `relates_to:reviews` this task.
    pub review_rounds: usize,
    pub reviews: Vec<String>,
    pub acceptance_met: Option<bool>,
    pub targets: Vec<TargetFact>,
    pub unavailable: BTreeMap<String, String>,
}

impl UsageStore {
    /// The review tasks of `task_id`: tasks of type `review` that relate to it as `reviews`.
    pub fn review_tasks_of(&self, task_id: &str) -> Result<Vec<String>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT t.task_id FROM task t JOIN task_link l ON l.task_id = t.task_id
                 WHERE t.task_type = 'review' AND l.kind = ?1 AND l.target = ?2
                 ORDER BY t.created_at_ms, t.task_id",
            )
            .map_err(db_err)?;
        let ids = stmt
            .query_map(
                params![format!("{LINK_RELATES_PREFIX}reviews"), task_id],
                |r| r.get(0),
            )
            .map_err(db_err)?
            .collect::<Result<_, _>>()
            .map_err(db_err)?;
        Ok(ids)
    }

    /// Prefer the launch's complete attribution set over its first native conversation. A
    /// native-only record without attribution still reads that recorded conversation.
    fn task_usage_identity(&self, session: &str) -> Result<String, String> {
        let attributed: bool = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM session_attribution WHERE launch_id = ?1)",
            params![session], |r| r.get(0),
        ).map_err(db_err)?;
        if attributed {
            return Ok(session.to_string());
        }
        Ok(self.read_session_record(session)?.and_then(|r| r.native_session_id)
            .unwrap_or_else(|| session.to_string()))
    }

    /// A session's usage as the report prices it: one aggregate per usage session.
    fn session_calls(&self, identity: &str) -> Result<Vec<Call>, String> {
        let report = self.report(&Filter {
            session_id: Some(identity.to_string()),
            ..Default::default()
        })?;
        Ok(report
            .sessions
            .iter()
            .map(|u| Call {
                models: u.models.clone(),
                cost_usd: u.cost_usd,
                input: u.tokens.input,
                output: u.tokens.output,
            })
            .collect())
    }

    /// The model calls of a session whose timestamp falls inside `[from, to]`.
    fn interval_calls(&self, identity: &str, from: i64, to: i64) -> Result<Vec<Call>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT model, cost_usd, input, output FROM usage_event
                 WHERE (session_id = ?1 OR session_id IN
                        (SELECT session_id FROM session_attribution WHERE launch_id = ?1))
                   AND at_ms >= ?2 AND at_ms <= ?3",
            )
            .map_err(db_err)?;
        let calls = stmt
            .query_map(params![identity, from, to], |r| {
                Ok(Call {
                    models: vec![r.get(0)?],
                    cost_usd: r.get(1)?,
                    input: r.get::<_, i64>(2)? as u64,
                    output: r.get::<_, i64>(3)? as u64,
                })
            })
            .map_err(db_err)?
            .collect::<Result<_, _>>()
            .map_err(db_err)?;
        Ok(calls)
    }

    /// A launch's own task owns its session. All other tasks, including wakes that reuse a
    /// session, own only their interval (see `task_interval_end`). The same boundary applies to skills.
    fn task_interval(
        &self,
        task: &TaskRecord,
        sessions: &[&str],
    ) -> Result<Option<(i64, i64)>, String> {
        let mut root = !sessions.is_empty();
        for session in sessions {
            root &= super::task_capture::task_id_of_session(self, session)? == task.task_id;
        }
        if root {
            return Ok(None);
        }
        Ok(Some((
            task.created_at_ms,
            self.task_interval_end(task, sessions)?,
        )))
    }

    /// Where the span `task` owns ends, the one rule for a task's outcome and for the task of a
    /// turn: its close, else the start of the next sibling (same parent) opened after it in the same
    /// session, so a nested child never ends an open parent, else
    /// the end of the session (now while one is still open).
    fn task_interval_end(&self, task: &TaskRecord, sessions: &[&str]) -> Result<i64, String> {
        if let Some(closed) = task.closed_at_ms {
            return Ok(closed);
        }
        let mut next: Option<i64> = None;
        let mut session_end: Option<i64> = None;
        for session in sessions {
            let own = super::task_capture::task_id_of_session(self, session)?;
            let later = self
                .tasks_for_session(session)?
                .into_iter()
                .filter(|t| {
                    t.task_id != own && t.task_id != task.task_id && t.parent_id == task.parent_id
                })
                .map(|t| t.created_at_ms)
                .filter(|&start| start > task.created_at_ms)
                .min();
            next = next.into_iter().chain(later).min();
            match self.read_session_timing(session)?.and_then(|t| t.ended_at_ms) {
                Some(ended) => session_end = Some(session_end.map_or(ended, |e| e.max(ended))),
                None => session_end = Some(now_ms()),
            }
        }
        Ok(next.or(session_end).unwrap_or_else(now_ms))
    }

    /// The task a call at `ts_ms` ran under in `session`: the innermost task of the session whose
    /// interval holds the time (the latest started), else the session's own task. Derived on read,
    /// never stored; `taskBasis` is `interval` for the first and `session` for the fallback.
    pub(crate) fn task_of_call(
        &self,
        session: &str,
        ts_ms: Option<i64>,
    ) -> Result<(String, &'static str), String> {
        let own = super::task_capture::task_id_of_session(self, session)?;
        let Some(at) = ts_ms else {
            return Ok((own, "session"));
        };
        let mut innermost: Option<TaskRecord> = None;
        for task in self.tasks_for_session(session)? {
            if task.task_id == own || task.created_at_ms > at {
                continue;
            }
            if at > self.task_interval_end(&task, &[session])? {
                continue;
            }
            if innermost.as_ref().is_none_or(|t| task.created_at_ms >= t.created_at_ms) {
                innermost = Some(task);
            }
        }
        Ok(match innermost {
            Some(task) => (task.task_id, "interval"),
            None => (own, "session"),
        })
    }

    /// The outcome facts of `task`: time and cost read live through its session links.
    pub fn read_outcome(&self, task: &TaskRecord) -> Result<TaskOutcome, String> {
        let evidence = super::story_changes::task_evidence(self, std::slice::from_ref(task))?;
        self.read_outcome_with_evidence(task, &evidence)
    }

    pub(crate) fn read_outcome_with_evidence(
        &self,
        task: &TaskRecord,
        evidence: &super::story_changes::TaskEvidence,
    ) -> Result<TaskOutcome, String> {
        let links = self.task_links(&task.task_id)?;
        let sessions: Vec<&str> = links
            .iter()
            .filter(|l| l.kind == LINK_SESSION)
            .map(|l| l.target.as_str())
            .collect();
        let mut out = TaskOutcome {
            acceptance_met: task.acceptance_met,
            ..Default::default()
        };
        let interval = self.task_interval(task, &sessions)?;
        out.scope = if interval.is_some() {
            "interval"
        } else {
            "session"
        }
        .into();
        let mut time = TimeFacts::default();
        let mut cost = CostFacts::default();
        let (mut timed, mut costed, mut unpriced) = (0, 0, false);
        for session in &sessions {
            let spans = match interval {
                None => self
                    .read_session_timing(session)?
                    .map(|t| (t.wall_ms, t.active_ms, t.idle_ms)),
                Some((from, to)) => self.interval_time(session, from, to)?,
            };
            if let Some((wall, active, idle)) = spans {
                timed += 1;
                time.add(wall, active, idle);
            }
            let identity = self.task_usage_identity(session)?;
            let calls = match interval {
                None => self.session_calls(&identity)?,
                Some((from, to)) => self.interval_calls(&identity, from, to)?,
            };
            for call in calls {
                costed += 1;
                match call.cost_usd {
                    Some(c) => *cost.cost_usd.get_or_insert(0.0) += c,
                    None => unpriced = true,
                }
                for model in call.models {
                    if !cost.models.contains(&model) {
                        cost.models.push(model);
                    }
                }
                cost.input_tokens += call.input;
                cost.output_tokens += call.output;
            }
        }
        time.sessions = timed;
        if timed > 0 {
            out.time = Some(time);
        } else {
            out.unavailable.insert(
                "time".into(),
                if interval.is_some() {
                    "no turn of the task's sessions falls inside its start-to-close interval"
                } else {
                    "no turn was observed in the task's sessions"
                }
                .into(),
            );
        }
        if costed > 0 {
            if unpriced {
                cost.cost_usd = None;
                out.unavailable.insert(
                    "cost".into(),
                    "some usage has no price: see session_usage".into(),
                );
            }
            out.cost = Some(cost);
        } else {
            out.unavailable.insert(
                "cost".into(),
                if interval.is_some() {
                    "no usage was recorded inside its start-to-close interval"
                } else {
                    "no usage was recorded for the task's sessions"
                }
                .into(),
            );
        }
        out.reviews = self.review_tasks_of(&task.task_id)?;
        out.review_rounds = out.reviews.len();
        if task.acceptance_met.is_none() {
            out.unavailable.insert(
                "acceptanceMet".into(),
                "the lead has not set it with task_update".into(),
            );
        }
        let (planned, rework) = super::story_changes::task_facts(self, task, evidence)?;
        for (key, reason) in &planned.unavailable {
            out.unavailable.insert(
                if *key == "planned" {
                    "plannedVsActual".into()
                } else {
                    (*key).into()
                },
                (*reason).into(),
            );
        }
        if let Some(reason) = serde_json::Value::as_str(&rework["unavailable"]) {
            out.unavailable.insert("rework".into(), reason.into());
        }
        out.planned_vs_actual = Some(serde_json::to_value(planned).map_err(|e| e.to_string())?);
        out.rework = Some(rework);
        if let Some(targets) = &task.targets {
            for (name, target) in &targets.values {
                let minutes = |ms: i64| json!((ms as f64 / 60_000.0 * 10.0).round() / 10.0);
                let actual = match name.as_str() {
                    "wall_minutes" => out.time.as_ref().map(|t| minutes(t.wall_ms)),
                    "active_minutes" => out.time.as_ref().map(|t| minutes(t.active_ms)),
                    "cost_usd" => out.cost.as_ref().and_then(|c| c.cost_usd).map(|c| json!(c)),
                    "review_rounds" => Some(json!(out.review_rounds)),
                    _ => None,
                };
                out.targets.push(TargetFact {
                    name: name.clone(),
                    target: target.clone(),
                    actual,
                });
            }
        }
        Ok(out)
    }
}

/// A task as `task_read` returns it: the record, its links, its subtree and the facts of its
/// outcome. Other skills come from the stored conversation's invocation index.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskView {
    pub task: TaskRecord,
    pub links: Vec<TaskLink>,
    pub children: Vec<TaskView>,
    pub outcome: TaskOutcome,
    /// The parent's main skill and its hash, read from the parent now. `pending` while the
    /// launcher's own task is not captured yet, `unavailable` when it names no skill.
    pub playbook: Value,
    pub other_skills: Value,
    /// Historical agent revisions and manifest hashes joined through the session links.
    pub agent_layers: Value,
}

impl UsageStore {
    /// The task `task_id` with its subtree, each task with its links and outcome.
    pub fn task_view(&self, task_id: &str) -> Result<Option<TaskView>, String> {
        let tasks = self.task_subtree(task_id)?;
        let evidence = super::story_changes::task_evidence(self, &tasks)?;
        let mut by_parent: BTreeMap<String, Vec<TaskRecord>> = BTreeMap::new();
        let mut root = None;
        for t in tasks {
            if t.task_id == task_id {
                root = Some(t);
            } else {
                by_parent
                    .entry(t.parent_id.clone().unwrap_or_default())
                    .or_default()
                    .push(t);
            }
        }
        let Some(root) = root else {
            return Ok(None);
        };
        Ok(Some(self.view_of(root, &mut by_parent, &evidence)?))
    }

    /// The playbook that split `task`: its parent's main skill and the hash of its files, from the
    /// parent's own launch snapshot (immutable once captured, so reading it is not recomputing).
    fn playbook_of(&self, task: &TaskRecord) -> Result<Value, String> {
        let Some(parent_id) = task.parent_id.as_deref() else {
            return Ok(json!({"unavailable": "the task has no parent"}));
        };
        let Some(parent) = self.read_task(parent_id)? else {
            return Ok(json!({"pending": "the parent's task is not captured yet"}));
        };
        let Some(skill) = parent.main_skill else {
            return Ok(json!({"unavailable": "the parent names no main skill"}));
        };
        let version = parent.layers.skills.iter().find(|s| s.name == skill);
        match version.and_then(|version| version.hash.as_ref()) {
            Some(hash) => Ok(json!({"skill": skill, "hash": hash})),
            None => Ok(json!({
                "skill": skill,
                "hash": null,
                "unavailable": version.and_then(|version| version.reason.as_deref())
                    .unwrap_or("the parent's main skill has no launch hash"),
            })),
        }
    }

    fn view_of(
        &self,
        task: TaskRecord,
        by_parent: &mut BTreeMap<String, Vec<TaskRecord>>,
        evidence: &super::story_changes::TaskEvidence,
    ) -> Result<TaskView, String> {
        let kids = by_parent.remove(&task.task_id).unwrap_or_default();
        let children = kids
            .into_iter()
            .map(|k| self.view_of(k, by_parent, evidence))
            .collect::<Result<_, _>>()?;
        let playbook = self.playbook_of(&task)?;
        Ok(TaskView {
            playbook,
            links: self.task_links(&task.task_id)?,
            outcome: self.read_outcome_with_evidence(&task, evidence)?,
            other_skills: self.other_skills_of(&task)?,
            agent_layers: self.agent_layers_of(&task)?,
            children,
            task,
        })
    }

    /// Agent versions live in the immutable launch records, never a second task snapshot.
    fn agent_layers_of(&self, task: &TaskRecord) -> Result<Value, String> {
        let mut sessions = Vec::new();
        for link in self.task_links(&task.task_id)?.into_iter().filter(|l| l.kind == LINK_SESSION) {
            let record = self.read_session_record(&link.target)?;
            let agent = record.as_ref().and_then(|r| r.agent.as_deref());
            let manifest_hash = record.as_ref().and_then(|r| r.manifest_hash.as_deref());
            let mut unavailable = BTreeMap::new();
            for (field, missing) in [("agent", agent.is_none()), ("manifestHash", manifest_hash.is_none())] {
                if missing {
                    let reason = record.as_ref().map(|r| {
                        r.unavailable.get(field).cloned()
                            .unwrap_or_else(|| format!("the launch did not report {field}"))
                    }).unwrap_or_else(|| "no session record was captured for this link".into());
                    unavailable.insert(field, reason);
                }
            }
            sessions.push(json!({
                "sessionId": link.target, "agent": agent, "manifestHash": manifest_hash,
                "unavailable": unavailable,
            }));
        }
        if sessions.is_empty() {
            return Ok(json!({"unavailable": "the task has no session links"}));
        }
        Ok(json!({"sessions": sessions}))
    }

    /// Indexed skill invocations through the task's session links, excluding its main skill.
    fn other_skills_of(&self, task: &TaskRecord) -> Result<Value, String> {
        let links = self.task_links(&task.task_id)?;
        let sessions: Vec<&str> = links.iter().filter(|l| l.kind == LINK_SESSION)
            .map(|l| l.target.as_str()).collect();
        let interval = self.task_interval(task, &sessions)?;
        let mut skills = std::collections::BTreeSet::new();
        let (mut indexed, mut missing, mut untimed) = (false, false, false);
        for session in sessions {
            let calls = self.read_tool_calls(session)?;
            let available = !calls.is_empty() || self.read_conversation_meta(session)?
                .is_some_and(|m| m.state != super::conversation::State::Unsupported);
            indexed |= available;
            missing |= !available;
            for call in calls {
                let Some(skill) = call.skill.filter(|s| Some(s.as_str()) != task.main_skill.as_deref()) else {
                    continue;
                };
                if let Some((from, to)) = interval {
                    let Some(at) = call.ts_ms else {
                        untimed = true;
                        continue;
                    };
                    if at < from || at > to { continue; }
                }
                skills.insert(skill);
            }
        }
        if !indexed {
            return Ok(json!({"unavailable": "no conversation skill index was recorded for the task's sessions"}));
        }
        let mut result = json!({"skills": skills});
        if missing || untimed {
            let reasons: Vec<&str> = [
                missing.then_some("some sessions have no conversation skill index"),
                untimed.then_some("some skill invocations have no timestamp for interval attribution"),
            ].into_iter().flatten().collect();
            result["unavailable"] = json!(reasons.join("; "));
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
