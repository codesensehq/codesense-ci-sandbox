// SPDX-License-Identifier: MIT
//! Task records over MCP: `task_start`, `task_update`, `task_list` and `task_read`.
//!
//! The same primitive the launches capture (see `session_usage::task`), made readable and
//! refinable by an agent without source access. Every verb speaks as the caller's own session
//! (`sessionId`, checked against the verified connection by the handler). A task is visible to
//! sessions of its own org and team; nothing here scores an agent.

use std::collections::BTreeMap;
use std::path::Path;

use rmcp::schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::session_usage::record::SessionRecord;
use crate::session_usage::task::{
    TaskFilter, TaskPatch, TaskRecord, Visibility, ABANDONED, DONE, LINK_SESSION, SUBTASK_PREFIX,
    SUPERSEDED, UNCLASSIFIED,
};
use crate::session_usage::task_capture::{
    apply_targets, canonical_type, load_policy, normalize_goal, start_layers, subject_tag,
    task_id_of_session,
};
use crate::session_usage::task_outcome::{close_as, TaskView};
use crate::session_usage::UsageStore;

/// The most tasks one `task_list` returns when the caller does not say.
pub(crate) const DEFAULT_LIMIT: usize = 50;

/// A link a task is given: `kind` is `session`, `artifact` or `relates_to:<verb>`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TaskLinkInput {
    pub kind: String,
    pub target: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskStartParams {
    /// Your own `$CODESENSE_TAB_SESSION_ID`; another session's id is refused.
    #[serde(alias = "session_id")]
    pub session_id: String,
    /// What the task is to achieve, in plain words (at most 280 characters are kept).
    pub goal: String,
    /// The task type tag, for example `exploratory`, `feature`, `bugfix`, `review`, `routine`
    /// or `ops`. Omitted: `unclassified`, with the reason.
    #[serde(default, rename = "type")]
    pub task_type: Option<String>,
    /// A free tag refining the type, for example `code`, `design`, `doc`, `security`.
    #[serde(default)]
    pub subject: Option<String>,
    /// The task this one sits under. Omitted: your own session's task.
    #[serde(default)]
    pub parent: Option<String>,
    /// The story this task works. Omitted: its parent's current story, then your session's.
    #[serde(default)]
    pub story: Option<String>,
    /// The main skill this task runs, by name.
    #[serde(default)]
    pub skill: Option<String>,
    /// Targets for this task, over the type's defaults, for example `{"wall_minutes": 60}`.
    #[serde(default)]
    pub targets: Option<BTreeMap<String, Value>>,
    /// Links to add, for example `{"kind":"relates_to:reviews","target":"<task id>"}`.
    #[serde(default)]
    pub links: Option<Vec<TaskLinkInput>>,
}

/// A human's verdict on a task.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TaskLabelInput {
    /// `good` or `bad`.
    pub verdict: String,
    /// Why, in plain words.
    #[serde(default)]
    pub why: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskUpdateParams {
    /// Your own `$CODESENSE_TAB_SESSION_ID`; another session's id is refused.
    #[serde(alias = "session_id")]
    pub session_id: String,
    /// The task to change, from `task_list`.
    #[serde(alias = "task_id")]
    pub task_id: String,
    #[serde(default)]
    pub goal: Option<String>,
    #[serde(default, rename = "type")]
    pub task_type: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    /// Link the task to a story. A task with no parent then goes under that story's root task.
    #[serde(default)]
    pub story: Option<String>,
    /// Re-attach the task under another task, never one inside its own subtree.
    #[serde(default)]
    pub parent: Option<String>,
    /// Targets to set over the current ones. They are recorded as set by `task_update`.
    #[serde(default)]
    pub targets: Option<BTreeMap<String, Value>>,
    /// Whether the task's acceptance criteria were met.
    #[serde(default, alias = "acceptance_met")]
    pub acceptance_met: Option<bool>,
    /// A verdict to keep with the task, so the Steward can be calibrated against it.
    #[serde(default)]
    pub label: Option<TaskLabelInput>,
    /// Close the task: `done`, `abandoned` or `superseded`.
    #[serde(default)]
    pub status: Option<String>,
    /// Links to add.
    #[serde(default)]
    pub links: Option<Vec<TaskLinkInput>>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskListParams {
    /// Your own `$CODESENSE_TAB_SESSION_ID`; another session's id is refused.
    #[serde(alias = "session_id")]
    pub session_id: String,
    /// Only tasks of the named story.
    #[serde(default)]
    pub story: Option<String>,
    /// Only tasks of this type.
    #[serde(default, rename = "type")]
    pub task_type: Option<String>,
    /// Only tasks in this status: `open`, `done`, `abandoned` or `superseded`.
    #[serde(default)]
    pub status: Option<String>,
    /// Only tasks run by this saved agent (`key` or `key@revision`).
    #[serde(default)]
    pub agent: Option<String>,
    /// Only the children of this task.
    #[serde(default)]
    pub parent: Option<String>,
    /// At most this many tasks, newest first (default 50, max 200).
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskReadParams {
    /// Your own `$CODESENSE_TAB_SESSION_ID`; another session's id is refused.
    #[serde(alias = "session_id")]
    pub session_id: String,
    /// The task to read, from `task_list` (a session's task is that session's id; a story's
    /// root task is `story:<id>`).
    #[serde(alias = "task_id")]
    pub task_id: String,
}

/// Who is asking: the org and team of the caller's session record.
struct Caller {
    session: String,
    visibility: Visibility,
    story: Option<String>,
    worktree: Option<String>,
}

fn caller_of(store: &UsageStore, session: &str) -> Result<Caller, String> {
    let record: SessionRecord = store
        .read_session_record(session)?
        .ok_or_else(|| format!("unavailable: session '{session}' has no record here"))?;
    Ok(Caller {
        session: record.session_id,
        visibility: Visibility {
            org: record.org,
            team: record.team,
        },
        story: record.story_id,
        worktree: record.worktree.or(record.workspace),
    })
}

fn visible(store: &UsageStore, caller: &Caller, id: &str) -> Result<TaskRecord, String> {
    store
        .read_task(id)?
        .filter(|t| caller.visibility.sees(t))
        .ok_or_else(|| format!("invalid: no task '{id}' is visible to your session"))
}

fn type_tag(value: &str) -> Result<String, String> {
    canonical_type(value).ok_or_else(|| {
        format!("invalid: type '{value}' is not a tag (lowercase letters, digits, - and _)")
    })
}

/// A subject is a free tag: the type aliases (`bug`, `spike`, ...) do not apply to it.
fn subject_of(value: &str) -> Result<String, String> {
    subject_tag(value).ok_or_else(|| {
        format!("invalid: subject '{value}' is not a tag (lowercase letters, digits, - and _)")
    })
}

fn add_links(store: &UsageStore, task: &str, links: &[TaskLinkInput]) -> Result<(), String> {
    for link in links {
        store
            .add_task_link(task, &link.kind, &link.target)
            .map_err(|e| format!("invalid: {e}"))?;
    }
    Ok(())
}

/// `task_start`: a task an agent made under its own, with its own goal.
pub fn start(
    store: &UsageStore,
    session: &str,
    p: TaskStartParams,
    policy_dir: Option<&Path>,
    now_ms: i64,
) -> Result<TaskRecord, String> {
    let caller = caller_of(store, session)?;
    let goal = normalize_goal(&p.goal);
    if goal.is_empty() {
        return Err("invalid: a task needs a goal".into());
    }
    let mut t = TaskRecord::new(format!("{SUBTASK_PREFIX}{}", uuid::Uuid::new_v4()), goal);
    t.created_at_ms = now_ms;
    t.org = caller.visibility.org.clone();
    t.team = caller.visibility.team.clone();
    t.worktree = caller.worktree.clone();
    t.created_by = Some(caller.session.clone());
    t.main_skill = p.skill;
    // The skill's hash is the one the session's own launch recorded, not a fresh reading of the
    // files; a skill that snapshot does not hold says so.
    if let Some(skill) = &t.main_skill {
        let launch = store.read_task(&task_id_of_session(store, &caller.session)?)?;
        match launch.and_then(|l| l.layers.skills.into_iter().find(|s| &s.name == skill)) {
            Some(version) => t.layers.skills = vec![version],
            None => {
                t.unavailable.insert(
                    "skill".into(),
                    format!("the session's launch snapshot has no skill '{skill}'"),
                );
            }
        }
    }
    match p.task_type.as_deref() {
        Some(raw) => {
            t.task_type = type_tag(raw)?;
            t.type_source = Some("task_start".into());
        }
        None => {
            t.type_source = Some(UNCLASSIFIED.into());
            t.unavailable
                .insert("type".into(), "task_start named no type".into());
        }
    }
    if let Some(subject) = p.subject.as_deref() {
        t.subject = Some(subject_of(subject)?);
    }
    match p.parent {
        Some(parent) => {
            t.parent_id = Some(visible(store, &caller, &parent)?.task_id);
            t.parent_source = Some("task_start".into());
        }
        None => {
            t.parent_id = Some(task_id_of_session(store, &caller.session)?);
            t.parent_source = Some("launcher".into());
        }
    }
    t.story_id = match p.story {
        Some(story) => Some(story),
        None => t.parent_id
            .as_deref()
            .map(|parent| store.read_task(parent))
            .transpose()?
            .flatten()
            .and_then(|parent| parent.story_id)
            .or(caller.story),
    };
    for (field, known) in [("goal", true), ("story", t.story_id.is_some()), ("subject", t.subject.is_some())] {
        t.field_sources.insert(field.into(), if known { "task_start" } else { "none" }.into());
    }
    let policy = match policy_dir {
        Some(dir) => load_policy(dir),
        None => Err("the caller's team has no content folder on this Mac".to_string()),
    };
    apply_targets(&mut t, &policy, p.targets.unwrap_or_default(), "task_start");
    start_layers(
        &mut t,
        caller.worktree.as_deref().map(Path::new),
        policy_dir,
    );
    store.insert_task_if_absent(&t)?;
    store.add_task_link(&t.task_id, LINK_SESSION, &caller.session)?;
    add_links(store, &t.task_id, &p.links.unwrap_or_default())?;
    store
        .read_task(&t.task_id)?
        .ok_or_else(|| "the task was not kept".into())
}

/// `task_update`: refine a task, set its verdict, or close it.
pub fn update(
    store: &UsageStore,
    session: &str,
    p: TaskUpdateParams,
    now_ms: i64,
) -> Result<TaskRecord, String> {
    let caller = caller_of(store, session)?;
    visible(store, &caller, &p.task_id)?;
    if let Some(parent) = &p.parent {
        visible(store, &caller, parent)?;
    }
    if let Some(status) = &p.status {
        if ![DONE, ABANDONED, SUPERSEDED].contains(&status.as_str()) {
            return Err("invalid: status is `done`, `abandoned` or `superseded`".into());
        }
    }
    let patch = TaskPatch {
        goal: p
            .goal
            .as_deref()
            .map(normalize_goal)
            .filter(|g| !g.is_empty()),
        task_type: p.task_type.as_deref().map(type_tag).transpose()?,
        subject: p.subject.as_deref().map(subject_of).transpose()?,
        story: p.story,
        parent: p.parent,
        targets: p.targets,
        acceptance_met: p.acceptance_met,
        label: p.label.map(|l| (l.verdict, l.why)),
    };
    store
        .update_task(&p.task_id, &patch, Some(&caller.session), now_ms)
        .map_err(|e| {
            if e.starts_with("invalid") {
                e
            } else {
                format!("invalid: {e}")
            }
        })?;
    add_links(store, &p.task_id, &p.links.unwrap_or_default())?;
    // Closing goes through the one close operation: it computes the tamper fact, and the first
    // close wins for the status and the time.
    if let Some(status) = &p.status {
        close_as(store, &p.task_id, status, status, now_ms)?;
    }
    store
        .read_task(&p.task_id)?
        .ok_or_else(|| "the task was not kept".into())
}

/// `task_list`: the tasks the caller can see, newest first. The caller's org and team and the
/// limit are applied together in SQL, so other teams' newer tasks never take its page.
pub fn list(
    store: &UsageStore,
    session: &str,
    p: TaskListParams,
) -> Result<Vec<TaskRecord>, String> {
    let caller = caller_of(store, session)?;
    let limit = (p.limit.map_or(DEFAULT_LIMIT, |l| l as usize)).clamp(1, 200);
    let mut rows = store.list_tasks(&TaskFilter {
        visible_to: Some(caller.visibility),
        story: p.story,
        task_type: p.task_type,
        status: p.status,
        agent: p.agent,
        parent: p.parent,
        limit: Some(limit),
    })?;
    let evidence = crate::session_usage::story_changes::task_evidence(store, &rows)?;
    for row in &mut rows {
        row.outcome = Some(serde_json::to_value(store.read_outcome_with_evidence(row, &evidence)?)
            .map_err(|e| e.to_string())?);
    }
    Ok(rows)
}

/// `task_read`: a task with its links, its subtree and its outcome facts.
pub fn read(store: &UsageStore, session: &str, p: TaskReadParams) -> Result<TaskView, String> {
    let caller = caller_of(store, session)?;
    visible(store, &caller, &p.task_id)?;
    let mut view = store
        .task_view(&p.task_id)?
        .ok_or_else(|| format!("invalid: no task '{}'", p.task_id))?;
    hide_unseen(&mut view, &caller.visibility);
    Ok(view)
}

/// Drop the subtasks the caller may not see.
fn hide_unseen(view: &mut TaskView, visibility: &Visibility) {
    view.children.retain(|c| visibility.sees(&c.task));
    for child in &mut view.children {
        hide_unseen(child, visibility);
    }
}

#[cfg(test)]
mod tests;
