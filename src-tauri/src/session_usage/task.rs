// SPDX-License-Identifier: MIT
//! Task records: what an agent was asked to do, judged by task rather than by run.
//!
//! One additive `task` table plus a `task_link` table in the usage store, created like
//! `session_story` and `session_record` (no schema version bump). A task's id is an existing
//! identity wherever one exists: a launched session's task is its CodeSense session id, a
//! story's root task is `story:<id>`, a routine run's task is its run id, and only an
//! agent-created sub-task gets a new `task-<uuid>`. The table keeps only what nothing else
//! holds; time, cost and models are read live from the session record, timing and usage through
//! the session links (see `task_outcome`). The parent is a plain id with no foreign key, so a
//! launcher's task and its child's can be written in either order.
//!
//! Free text passes through `redact_secrets`; nothing here scores an agent.

use std::collections::BTreeMap;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{db_err, UsageStore};
use crate::shared::secret_scan::redact_secrets;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS task (
    task_id        TEXT PRIMARY KEY,
    org            TEXT,
    team           TEXT,
    story_id       TEXT,
    goal           TEXT NOT NULL,
    task_type      TEXT NOT NULL,
    subject        TEXT,
    parent_id      TEXT,
    parent_source  TEXT,
    type_source    TEXT,
    field_sources  TEXT NOT NULL DEFAULT '{}',
    main_skill     TEXT,
    targets        TEXT,
    layers         TEXT NOT NULL DEFAULT '{}',
    unavailable    TEXT NOT NULL DEFAULT '{}',
    status         TEXT NOT NULL DEFAULT 'open',
    raw_status     TEXT,
    closed_at_ms   INTEGER,
    acceptance_met INTEGER,
    label          TEXT,
    tamper         TEXT,
    tamper_globs   TEXT NOT NULL DEFAULT '[]',
    worktree       TEXT,
    created_by     TEXT,
    created_at_ms  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS task_by_parent ON task (parent_id) WHERE parent_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS task_by_story ON task (story_id) WHERE story_id IS NOT NULL;
CREATE TABLE IF NOT EXISTS task_link (
    task_id TEXT NOT NULL,
    kind    TEXT NOT NULL,
    target  TEXT NOT NULL,
    PRIMARY KEY (task_id, kind, target)
);
CREATE INDEX IF NOT EXISTS task_link_by_target ON task_link (kind, target);
";

pub(super) fn init(conn: &rusqlite::Connection) -> Result<(), String> {
    conn.execute_batch(SCHEMA).map_err(db_err)?;
    if !super::has_column(conn, "task", "field_sources")? {
        conn.execute_batch("ALTER TABLE task ADD COLUMN field_sources TEXT NOT NULL DEFAULT '{}';")
            .map_err(db_err)?;
    }
    Ok(())
}

/// Statuses a task is in. Everything but `open` is closed.
pub const OPEN: &str = "open";
pub const DONE: &str = "done";
pub const ABANDONED: &str = "abandoned";
pub const SUPERSEDED: &str = "superseded";

/// The link kinds: the session a task ran in, an artifact it produced, and `relates_to:<verb>`
/// (a review task `relates_to:reviews` the task it reviewed).
pub const LINK_SESSION: &str = "session";
pub const LINK_ARTIFACT: &str = "artifact";
pub const LINK_RELATES_PREFIX: &str = "relates_to:";

/// The id prefix of a task an agent made with `task_start`.
pub const SUBTASK_PREFIX: &str = "task-";

/// The type of a task nothing classified.
pub const UNCLASSIFIED: &str = "unclassified";
/// A genuine terminal event arrived before the launch metadata.
pub(crate) const EARLY_CLOSE: &str = "codesense:pending-task-capture";
pub(crate) const CAPTURE_PENDING: &str = "pending launch capture";
pub(crate) const TAMPER_PENDING: &str = "pending tamper capture";

/// The id of a story's root task.
fn story_task_id(story_id: &str) -> String {
    format!("story:{story_id}")
}

/// A story's root task: the top of its tree, with the goal the caller found. Org and team are
/// the story's, as the caller knows them.
pub fn story_root(
    story_id: &str,
    goal: impl Into<String>,
    org: Option<String>,
    team: Option<String>,
) -> TaskRecord {
    let mut root = TaskRecord::new(story_task_id(story_id), goal);
    root.story_id = Some(story_id.to_string());
    root.org = org;
    root.team = team;
    root.created_by = Some("codesense".into());
    root
}

/// What a task was told to reach, copied onto the task when it was made so a later policy edit
/// never rewrites history. `source` is `default`, `brief` or `task_update`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskTargets {
    pub source: String,
    pub values: BTreeMap<String, Value>,
}

/// One skill's version at launch: its name and the hash of its own files, or why there is none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillVersion {
    pub name: String,
    pub hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The playbook that split a task: the parent's main skill and its hash. Read from the parent
/// when the task is read; never stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybookVersion {
    pub skill: String,
    pub hash: Option<String>,
}

/// Which version of each layer a task ran with, copied once at launch and never recomputed. The
/// agent layer is read through the session link, not copied here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskLayers {
    pub skills: Vec<SkillVersion>,
    /// The source worktree's HEAD commit.
    pub source: Option<String>,
    /// The context checkout's HEAD commit the launch hydrated from.
    pub context: Option<String>,
}

/// A human's verdict on a task, kept to calibrate the Steward. Never a score.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskLabel {
    /// `good` or `bad`.
    pub verdict: String,
    pub why: Option<String>,
    pub by: Option<String>,
    pub at_ms: i64,
}

/// Whether tests or checks were edited during a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tamper {
    pub flag: bool,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRecord {
    pub task_id: String,
    /// Live facts on task_list; never persisted with the task record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Value>,
    pub org: Option<String>,
    pub team: Option<String>,
    pub story_id: Option<String>,
    pub goal: String,
    /// `unclassified` when no rule resolved it, with a reason in `unavailable["type"]`.
    pub task_type: String,
    pub subject: Option<String>,
    pub parent_id: Option<String>,
    /// Where the parent came from: `launcher` (the launching session's task), `story` (the
    /// story's root task) or `none`. A fallback says why in `unavailable["parent"]`.
    pub parent_source: Option<String>,
    /// The rule that chose the type: `brief`, `routine`, `skill`, `story` or `unclassified`.
    pub type_source: Option<String>,
    /// Sources of goal, story and subject; pending/default facts may be enriched, refinements stand.
    #[serde(default)]
    pub field_sources: BTreeMap<String, String>,
    pub main_skill: Option<String>,
    pub targets: Option<TaskTargets>,
    pub layers: TaskLayers,
    /// `{fact: reason}` for each fact that could not be known.
    pub unavailable: BTreeMap<String, String>,
    pub status: String,
    /// The raw `signal_done` status a close came from.
    pub raw_status: Option<String>,
    pub closed_at_ms: Option<i64>,
    pub acceptance_met: Option<bool>,
    pub label: Option<TaskLabel>,
    pub tamper: Option<Tamper>,
    /// The policy's tamper globs, copied at launch with the targets.
    pub tamper_globs: Vec<String>,
    pub worktree: Option<String>,
    pub created_by: Option<String>,
    pub created_at_ms: i64,
}

impl TaskRecord {
    pub fn new(task_id: impl Into<String>, goal: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            outcome: None,
            org: None,
            team: None,
            story_id: None,
            goal: goal.into(),
            task_type: UNCLASSIFIED.to_string(),
            subject: None,
            parent_id: None,
            parent_source: None,
            type_source: None,
            field_sources: BTreeMap::new(),
            main_skill: None,
            targets: None,
            layers: TaskLayers::default(),
            unavailable: BTreeMap::new(),
            status: OPEN.to_string(),
            raw_status: None,
            closed_at_ms: None,
            acceptance_met: None,
            label: None,
            tamper: None,
            tamper_globs: Vec::new(),
            worktree: None,
            created_by: None,
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        }
    }
}

/// What a `task_update` may change. Absent fields are left alone.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TaskPatch {
    pub goal: Option<String>,
    pub task_type: Option<String>,
    pub subject: Option<String>,
    /// Link the task to a story after it was made (a session in a context worktree launches
    /// with none). A task with no parent then goes under that story's root task.
    pub story: Option<String>,
    /// Re-attach the task under another task, never one inside its own subtree.
    pub parent: Option<String>,
    pub targets: Option<BTreeMap<String, Value>>,
    pub acceptance_met: Option<bool>,
    pub label: Option<(String, Option<String>)>,
}

/// One `(kind, target)` link of a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskLink {
    pub kind: String,
    pub target: String,
}

/// Which tasks a session can see: those of its own org and team (a missing org or team only sees
/// tasks that have none). The one rule for lists and reads, in SQL and in code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Visibility {
    pub org: Option<String>,
    pub team: Option<String>,
}

impl Visibility {
    pub fn sees(&self, t: &TaskRecord) -> bool {
        t.org == self.org && t.team == self.team
    }
}

#[derive(Debug, Clone, Default)]
pub struct TaskFilter {
    /// Only the tasks this org and team can see.
    pub visible_to: Option<Visibility>,
    pub story: Option<String>,
    pub task_type: Option<String>,
    pub status: Option<String>,
    pub agent: Option<String>,
    pub parent: Option<String>,
    pub limit: Option<usize>,
}

const COLS: &str = "task_id, org, team, story_id, goal, task_type, subject, parent_id, main_skill, targets, layers,
    unavailable, status, raw_status, closed_at_ms, acceptance_met, label, tamper, worktree, created_by, created_at_ms,
    parent_source, type_source, tamper_globs, field_sources";

/// A type or subject tag: policy owns the list, so any short lowercase tag is accepted.
pub fn valid_tag(tag: &str) -> bool {
    let mut chars = tag.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && tag.len() <= 40
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

fn valid_story_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn valid_link_kind(kind: &str) -> bool {
    kind == LINK_SESSION
        || kind == LINK_ARTIFACT
        || kind
            .strip_prefix(LINK_RELATES_PREFIX)
            .is_some_and(valid_tag)
}

fn json_or<T: Serialize>(value: &T, default: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| default.to_string())
}

fn from_json<T: for<'de> Deserialize<'de>>(text: Option<String>) -> Option<T> {
    text.and_then(|t| serde_json::from_str(&t).ok())
}

fn clean_layers(layers: &TaskLayers) -> TaskLayers {
    let mut layers = layers.clone();
    for skill in &mut layers.skills {
        skill.reason = skill.reason.as_deref().map(redact_secrets);
    }
    layers
}

fn row_task(r: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRecord> {
    let layers: String = r.get(10)?;
    let unavailable: String = r.get(11)?;
    Ok(TaskRecord {
        task_id: r.get(0)?,
        outcome: None,
        org: r.get(1)?,
        team: r.get(2)?,
        story_id: r.get(3)?,
        goal: r.get(4)?,
        task_type: r.get(5)?,
        subject: r.get(6)?,
        parent_id: r.get(7)?,
        main_skill: r.get(8)?,
        targets: from_json(r.get(9)?),
        layers: serde_json::from_str(&layers).unwrap_or_default(),
        unavailable: serde_json::from_str(&unavailable).unwrap_or_default(),
        status: r.get(12)?,
        raw_status: r.get(13)?,
        closed_at_ms: r.get(14)?,
        acceptance_met: r.get::<_, Option<i64>>(15)?.map(|v| v != 0),
        label: from_json(r.get(16)?),
        tamper: from_json(r.get(17)?),
        worktree: r.get(18)?,
        created_by: r.get(19)?,
        created_at_ms: r.get(20)?,
        parent_source: r.get(21)?,
        type_source: r.get(22)?,
        tamper_globs: serde_json::from_str(&r.get::<_, String>(23)?).unwrap_or_default(),
        field_sources: from_json(r.get(24)?).unwrap_or_default(),
    })
}

fn clean(value: &Option<String>) -> Option<String> {
    value.as_deref().map(redact_secrets)
}

impl UsageStore {
    /// Write a task when it has none; a second write for the same id changes nothing, so a
    /// launch written twice never reopens or rewrites a task. Returns whether it wrote.
    pub fn insert_task_if_absent(&self, t: &TaskRecord) -> Result<bool, String> {
        if !valid_tag(&t.task_type) {
            return Err(format!("'{}' is not a task type tag", t.task_type));
        }
        if t.subject.as_deref().is_some_and(|s| !valid_tag(s)) {
            return Err("the subject is not a tag".into());
        }
        let unavailable: BTreeMap<_, _> = t
            .unavailable
            .iter()
            .map(|(k, v)| (k.clone(), redact_secrets(v)))
            .collect();
        let written = self
            .conn
            .execute(
                &format!(
                    "INSERT OR IGNORE INTO task ({COLS})
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)"
                ),
                params![
                    t.task_id,
                    clean(&t.org),
                    clean(&t.team),
                    clean(&t.story_id),
                    redact_secrets(&t.goal),
                    t.task_type,
                    t.subject,
                    t.parent_id,
                    clean(&t.main_skill),
                    t.targets.as_ref().map(|v| json_or(v, "null")),
                    json_or(&clean_layers(&t.layers), "{}"),
                    json_or(&unavailable, "{}"),
                    t.status,
                    t.raw_status,
                    t.closed_at_ms,
                    t.acceptance_met.map(i64::from),
                    t.label.as_ref().map(|v| json_or(v, "null")),
                    t.tamper.as_ref().map(|v| json_or(v, "null")),
                    clean(&t.worktree),
                    clean(&t.created_by),
                    t.created_at_ms,
                    t.parent_source,
                    t.type_source,
                    json_or(&t.tamper_globs, "[]"),
                    json_or(&t.field_sources, "{}"),
                ],
            )
            .map_err(db_err)?;
        Ok(written > 0)
    }

    pub fn read_task(&self, task_id: &str) -> Result<Option<TaskRecord>, String> {
        self.conn
            .query_row(
                &format!("SELECT {COLS} FROM task WHERE task_id = ?1"),
                params![task_id],
                row_task,
            )
            .optional()
            .map_err(db_err)
    }

    /// Apply a patch to a task. A patch never changes a task's status: closing goes through the one
    /// close operation (`task_outcome::close_as`).
    pub fn update_task(
        &self,
        task_id: &str,
        patch: &TaskPatch,
        caller: Option<&str>,
        now_ms: i64,
    ) -> Result<bool, String> {
        for tag in [&patch.task_type, &patch.subject].into_iter().flatten() {
            if !valid_tag(tag) {
                return Err(format!(
                    "'{tag}' is not a tag: use lowercase letters, digits, - and _"
                ));
            }
        }
        if let Some((verdict, _)) = &patch.label {
            if verdict != "good" && verdict != "bad" {
                return Err("a label's verdict is `good` or `bad`".into());
            }
        }
        let Some(t) = self.read_task(task_id)? else {
            return Ok(false);
        };
        #[cfg(test)]
        tests::after_update_read();
        let mut story_parent = None;
        if let Some(story) = &patch.story {
            if !valid_story_id(story) {
                return Err(format!("'{story}' is not a story id"));
            }
            if t.parent_id.is_none() && patch.parent.is_none() {
                let root = story_root(
                    story,
                    format!("Story {story}"),
                    t.org.clone(),
                    t.team.clone(),
                );
                self.insert_task_if_absent(&root)?;
                story_parent = Some(root.task_id);
            }
        }
        if let Some(parent) = &patch.parent {
            if self
                .task_subtree(task_id)?
                .iter()
                .any(|d| &d.task_id == parent)
            {
                return Err(format!(
                    "'{parent}' is inside '{task_id}': a task cannot sit below itself"
                ));
            }
        }
        let label = patch.label.as_ref().map(|(verdict, why)| TaskLabel {
            verdict: verdict.clone(),
            why: why.as_deref().map(redact_secrets),
            by: caller.map(redact_secrets),
            at_ms: now_ms,
        });
        let field_sources: BTreeMap<&str, &str> = [
            ("goal", patch.goal.is_some()),
            ("story", patch.story.is_some()),
            ("subject", patch.subject.is_some()),
        ].into_iter().filter(|(_, supplied)| *supplied)
            .map(|(field, _)| (field, "task_update")).collect();
        // Patch the current row, never the read snapshot. Concurrent metadata and terminal
        // evidence survive; target keys replace whole values like BTreeMap::extend.
        self.conn
            .execute(
                "UPDATE task SET goal = COALESCE(?2, goal), task_type = COALESCE(?3, task_type),
                    subject = COALESCE(?4, subject),
                    targets = CASE WHEN ?5 IS NULL THEN targets ELSE json_object(
                        'source', 'task_update', 'values', (
                            SELECT json_group_object(key, json(value)) FROM (
                                SELECT key, json_extract(targets, '$.values') -> fullkey AS value
                                FROM json_each(json_extract(targets, '$.values'))
                                WHERE key NOT IN (SELECT key FROM json_each(?5))
                                UNION ALL SELECT key, ?5 -> fullkey AS value FROM json_each(?5)
                            )
                        )) END,
                    unavailable = (SELECT json_group_object(key, value) FROM json_each(unavailable)
                        WHERE NOT ((key = 'goal' AND ?2 IS NOT NULL)
                            OR (key = 'type' AND ?3 IS NOT NULL)
                            OR (key = 'subject' AND ?4 IS NOT NULL)
                            OR (key = 'targets' AND ?5 IS NOT NULL)
                            OR (key = 'story' AND ?8 IS NOT NULL)
                            OR (key = 'parent' AND (?9 IS NOT NULL OR
                                (parent_id IS NULL AND ?10 IS NOT NULL))))),
                    acceptance_met = COALESCE(?6, acceptance_met), label = COALESCE(?7, label),
                    story_id = COALESCE(?8, story_id),
                    parent_id = COALESCE(?9, CASE WHEN parent_id IS NULL THEN ?10 END, parent_id),
                    parent_source = CASE WHEN ?9 IS NOT NULL THEN 'task_update'
                        WHEN parent_id IS NULL AND ?10 IS NOT NULL THEN 'story' ELSE parent_source END,
                    type_source = CASE WHEN ?3 IS NOT NULL THEN 'task_update' ELSE type_source END,
                    field_sources = json_patch(field_sources, ?11)
                 WHERE task_id = ?1",
                params![
                    task_id,
                    clean(&patch.goal),
                    patch.task_type,
                    patch.subject,
                    patch.targets.as_ref().map(|v| json_or(v, "{}")),
                    patch.acceptance_met.map(i64::from),
                    label.as_ref().map(|v| json_or(v, "null")),
                    clean(&patch.story),
                    patch.parent,
                    story_parent,
                    json_or(&field_sources, "{}"),
                ],
            )
            .map_err(db_err)?;
        Ok(true)
    }

    /// Complete a routine start or early-close placeholder with its launch facts once. First
    /// terminal status and time are preserved; the close funnel finishes deferred tamper next.
    pub fn complete_launch_task(&self, incoming: &TaskRecord) -> Result<(), String> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)
            .map_err(db_err)?;
        let Some(old) = self.read_task(&incoming.task_id)? else { return Ok(()); };
        let early = old.created_by.as_deref() == Some(EARLY_CLOSE);
        if !early && old.created_by.as_deref() != Some(super::task_capture::ROUTINE_START) {
            return Ok(());
        }
        let mut t = incoming.clone();
        if !early {
            t.created_at_ms = old.created_at_ms;
        }
        t.org = old.org.clone().or(t.org);
        t.team = old.team.clone().or(t.team);
        // Every placeholder uses the same per-field rule: final sources stand; only
        // pending, absent or default facts take later capture facts.
        let final_source = |source: Option<&str>| !matches!(source,
            None | Some("pending" | "none" | "unclassified" | "default"));
        for field in ["goal", "story", "subject"] {
            if final_source(old.field_sources.get(field).map(String::as_str)) {
                match field {
                    "goal" => t.goal = old.goal.clone(),
                    "story" => t.story_id = old.story_id.clone(),
                    "subject" => t.subject = old.subject.clone(),
                    _ => unreachable!(),
                }
                t.field_sources.insert(field.into(), old.field_sources[field].clone());
            }
        }
        if final_source(old.parent_source.as_deref()) {
            t.parent_id = old.parent_id.clone();
            t.parent_source = old.parent_source.clone();
        }
        if final_source(old.type_source.as_deref()) && old.type_source.as_deref() != Some("routine") {
            t.task_type = old.task_type.clone();
            t.type_source = old.type_source.clone();
        }
        if let Some(targets) = &old.targets {
            if targets.source != "default" || t.targets.as_ref().is_none_or(|v| v.source == "default") {
                t.targets = Some(targets.clone());
            }
        }
        t.main_skill = old.main_skill.clone().or(t.main_skill);
        t.worktree = old.worktree.clone().or(t.worktree);
        t.layers.source = old.layers.source.clone().or(t.layers.source);
        t.layers.context = old.layers.context.clone().or(t.layers.context);
        if !old.layers.skills.is_empty() { t.layers.skills = old.layers.skills.clone(); }
        if !old.tamper_globs.is_empty() { t.tamper_globs = old.tamper_globs.clone(); }
        for (key, why) in &old.unavailable {
            if why != CAPTURE_PENDING {
                t.unavailable.entry(key.clone()).or_insert_with(|| why.clone());
            }
        }
        for (key, known) in [
            ("goal", t.field_sources.get("goal").is_some_and(|s| s == "task_update")),
            ("story", t.story_id.is_some()),
            ("subject", t.subject.is_some()),
            ("parent", t.parent_source.as_deref() == Some("task_update")
                || t.field_sources.get("story").is_some_and(|s| s == "task_update")),
            ("type", t.task_type != UNCLASSIFIED),
            ("mainSkill", t.main_skill.is_some()),
            ("targets", t.targets.is_some()),
            ("source", t.layers.source.is_some()),
            ("context", t.layers.context.is_some()),
            ("skills", !t.layers.skills.is_empty()),
        ] {
            if known { t.unavailable.remove(key); }
        }
        if old.status != OPEN && old.tamper.is_none() {
            t.unavailable.insert("tamper".into(), CAPTURE_PENDING.into());
        }
        tx.execute(
            "UPDATE task SET main_skill = ?2, layers = ?3, worktree = ?4,
                targets = ?5, tamper_globs = ?6, unavailable = ?7, created_by = ?8,
                org = ?9, team = ?10, story_id = ?11, goal = ?12, task_type = ?13,
                subject = ?14, parent_id = ?15, parent_source = ?16, type_source = ?17,
                created_at_ms = ?18, field_sources = ?19 WHERE task_id = ?1",
            params![t.task_id, clean(&t.main_skill), json_or(&clean_layers(&t.layers), "{}"),
                clean(&t.worktree), t.targets.as_ref().map(|v| json_or(v, "null")),
                json_or(&t.tamper_globs, "[]"), json_or(&t.unavailable, "{}"), clean(&t.created_by),
                clean(&t.org), clean(&t.team), clean(&t.story_id), redact_secrets(&t.goal),
                t.task_type, t.subject, t.parent_id, t.parent_source, t.type_source, t.created_at_ms,
                json_or(&t.field_sources, "{}")],
        ).map_err(db_err)?;
        tx.commit().map_err(db_err)
    }

    /// Close a task that is still open. The first close stands: a duplicate `signal_done`
    /// changes nothing. Returns whether this call closed it.
    pub fn close_task(
        &self,
        task_id: &str,
        status: &str,
        raw_status: Option<&str>,
        now_ms: i64,
    ) -> Result<bool, String> {
        let closed = self
            .conn
            .execute(
                "UPDATE task SET status = ?2, raw_status = ?3, closed_at_ms = ?4,
                    unavailable = json_set(unavailable, '$.tamper', ?5)
                 WHERE task_id = ?1 AND status = 'open'",
                params![task_id, status, raw_status.map(redact_secrets), now_ms, TAMPER_PENDING],
            )
            .map_err(db_err)?;
        Ok(closed > 0)
    }

    /// Finish only pending tamper evidence. First verdict wins, including an unavailable reason.
    pub fn finish_task_tamper(&self, task_id: &str, evidence: &Result<Tamper, String>) -> Result<(), String> {
        let tamper = evidence.as_ref().ok().map(|t| json_or(t, "null"));
        let reason = evidence.as_ref().err().map(|why| redact_secrets(why));
        self.conn.execute(
            "UPDATE task SET tamper = ?2,
                unavailable = CASE WHEN ?3 IS NULL THEN json_remove(unavailable, '$.tamper')
                    ELSE json_set(unavailable, '$.tamper', ?3) END
             WHERE task_id = ?1 AND tamper IS NULL
                AND json_extract(unavailable, '$.tamper') IN (?4, ?5)",
            params![task_id, tamper, reason, TAMPER_PENDING, CAPTURE_PENDING],
        ).map(|_| ()).map_err(db_err)
    }

    pub fn add_task_link(&self, task_id: &str, kind: &str, target: &str) -> Result<(), String> {
        if !valid_link_kind(kind) {
            return Err(format!(
                "'{kind}' is not a link kind: use `session`, `artifact` or `relates_to:<verb>`"
            ));
        }
        self.conn
            .execute(
                "INSERT OR IGNORE INTO task_link (task_id, kind, target) VALUES (?1, ?2, ?3)",
                params![task_id, kind, redact_secrets(target)],
            )
            .map(|_| ())
            .map_err(db_err)
    }

    pub fn task_links(&self, task_id: &str) -> Result<Vec<TaskLink>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT kind, target FROM task_link WHERE task_id = ?1 ORDER BY kind, target")
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![task_id], |r| {
                Ok(TaskLink {
                    kind: r.get(0)?,
                    target: r.get(1)?,
                })
            })
            .map_err(db_err)?
            .collect::<Result<_, _>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    /// The task and every task below it, parents before children.
    pub fn task_subtree(&self, task_id: &str) -> Result<Vec<TaskRecord>, String> {
        self.query_tasks(
            &format!(
                "WITH RECURSIVE sub(task_id, depth) AS (
                    SELECT task_id, 0 FROM task WHERE task_id = ?1
                    UNION
                    SELECT t.task_id, sub.depth + 1 FROM task t JOIN sub ON t.parent_id = sub.task_id
                    WHERE sub.depth < 32
                 )
                 SELECT {COLS} FROM task JOIN sub USING (task_id)
                 GROUP BY task_id ORDER BY MIN(sub.depth), created_at_ms, task_id"
            ),
            params![task_id],
        )
    }

    fn query_tasks(
        &self,
        sql: &str,
        args: impl rusqlite::Params,
    ) -> Result<Vec<TaskRecord>, String> {
        let mut stmt = self.conn.prepare(sql).map_err(db_err)?;
        let rows = stmt
            .query_map(args, row_task)
            .map_err(db_err)?
            .collect::<Result<_, _>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    pub fn list_tasks(&self, f: &TaskFilter) -> Result<Vec<TaskRecord>, String> {
        let limit = f.limit.unwrap_or(200).clamp(1, 1000) as i64;
        let (scoped, org, team) = match &f.visible_to {
            Some(v) => (1, v.org.clone(), v.team.clone()),
            None => (0, None, None),
        };
        self.query_tasks(
            &format!(
                "SELECT {COLS} FROM task
                 WHERE (?1 IS NULL OR story_id = ?1)
                   AND (?2 IS NULL OR task_type = ?2)
                   AND (?3 IS NULL OR status = ?3)
                   AND (?4 IS NULL OR parent_id = ?4)
                   AND (?5 IS NULL OR task_id IN (
                        SELECT l.task_id FROM task_link l JOIN session_record r ON r.session_id = l.target
                        WHERE l.kind = 'session' AND (r.agent = ?5 OR r.agent LIKE ?5 || '@%')))
                   AND (?6 = 0 OR (org IS ?7 AND team IS ?8))
                 ORDER BY created_at_ms DESC, task_id LIMIT ?9"
            ),
            params![f.story, f.task_type, f.status, f.parent, f.agent, scoped, org, team, limit],
        )
    }

    /// The tasks a session ran in: the one named by its id, plus any linked to it. A CLI's own
    /// session id finds its CodeSense session's tasks.
    pub fn tasks_for_session(&self, session_id: &str) -> Result<Vec<TaskRecord>, String> {
        self.query_tasks(
            &format!(
                "SELECT {COLS} FROM task
                 WHERE task_id IN (?1, (SELECT session_id FROM session_record WHERE native_session_id = ?1))
                    OR task_id IN (
                        SELECT task_id FROM task_link WHERE kind = 'session'
                          AND target IN (?1, (SELECT session_id FROM session_record WHERE native_session_id = ?1)))
                 ORDER BY created_at_ms, task_id"
            ),
            params![session_id],
        )
    }

    pub fn tasks_for_story(&self, story_id: &str) -> Result<Vec<TaskRecord>, String> {
        self.query_tasks(
            &format!("SELECT {COLS} FROM task WHERE story_id = ?1 ORDER BY created_at_ms, task_id"),
            params![story_id],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_usage::tests::store;

    thread_local! {
        static AFTER_UPDATE_READ: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = Default::default();
    }

    pub(super) fn after_update_read() {
        let hook = AFTER_UPDATE_READ.with(|slot| slot.borrow_mut().take());
        if let Some(hook) = hook { hook(); }
    }

    #[test]
    fn metadata_update_preserves_a_concurrent_close_and_its_pending_tamper_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        let store = UsageStore::open(&path).unwrap();
        store.insert_task_if_absent(&task("worker", None)).unwrap();
        let other_path = path.clone();
        AFTER_UPDATE_READ.with(|slot| *slot.borrow_mut() = Some(Box::new(move || {
            let other = UsageStore::open(&other_path).unwrap();
            assert!(other.close_task("worker", DONE, Some("success"), 20).unwrap());
            assert_eq!(other.read_task("worker").unwrap().unwrap().unavailable["tamper"], TAMPER_PENDING);
            other.update_task("worker", &TaskPatch {
                label: Some(("good".into(), Some("reviewed".into()))),
                ..Default::default()
            }, Some("lead"), 21).unwrap();
        })));
        store.update_task("worker", &TaskPatch {
            acceptance_met: Some(true), ..Default::default()
        }, Some("lead"), 19).unwrap();
        let tamper = Tamper { flag: true, files: vec!["tests/worker.rs".into()] };
        store.finish_task_tamper("worker", &Ok(tamper.clone())).unwrap();
        drop(store);
        let back = UsageStore::open(&path).unwrap().read_task("worker").unwrap().unwrap();
        assert_eq!(back.tamper, Some(tamper));
        assert!(!back.unavailable.contains_key("tamper"));
        assert_eq!(back.acceptance_met, Some(true));
        assert_eq!(back.label.unwrap().verdict, "good");
        assert_eq!((back.status.as_str(), back.raw_status.as_deref(), back.closed_at_ms),
            (DONE, Some("success"), Some(20)));
    }

    #[test]
    fn concurrent_metadata_updates_preserve_each_others_fields_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        let store = UsageStore::open(&path).unwrap();
        let mut worker = task("worker", None);
        worker.targets.as_mut().unwrap().values.insert(
            "custom".into(), serde_json::json!({"obsolete": true, "checks": [false]}));
        store.insert_task_if_absent(&worker).unwrap();
        let other_path = path.clone();
        AFTER_UPDATE_READ.with(|slot| *slot.borrow_mut() = Some(Box::new(move || {
            UsageStore::open(&other_path).unwrap().update_task("worker", &TaskPatch {
                goal: Some("Refined goal".into()),
                subject: Some("research".into()),
                label: Some(("good".into(), None)),
                targets: Some(BTreeMap::from([
                    ("cost_usd".into(), serde_json::json!(5)),
                    ("tests_green".into(), serde_json::json!(true)),
                    ("quality".into(), serde_json::json!("reviewed")),
                    ("custom".into(), serde_json::json!({"checks": [true, false, "green"]})),
                    ("nullable".into(), Value::Null),
                ])),
                ..Default::default()
            }, Some("reviewer"), 11).unwrap();
        })));
        store.update_task("worker", &TaskPatch {
            acceptance_met: Some(true),
            task_type: Some("review".into()),
            targets: Some(BTreeMap::from([("wall_minutes".into(), serde_json::json!(30))])),
            ..Default::default()
        }, Some("lead"), 10).unwrap();
        drop(store);
        let back = UsageStore::open(&path).unwrap().read_task("worker").unwrap().unwrap();
        assert_eq!(back.goal, "Refined goal");
        assert_eq!(back.subject.as_deref(), Some("research"));
        assert_eq!(back.task_type, "review");
        assert_eq!(back.acceptance_met, Some(true));
        assert_eq!(back.label.unwrap().by.as_deref(), Some("reviewer"));
        let targets = back.targets.unwrap();
        assert_eq!(targets.source, "task_update");
        assert_eq!(targets.values["wall_minutes"], 30);
        assert_eq!(targets.values["cost_usd"], 5);
        assert_eq!(targets.values["tests_green"], true);
        assert_eq!(targets.values["quality"], "reviewed");
        assert_eq!(targets.values["custom"], serde_json::json!({"checks": [true, false, "green"]}));
        assert_eq!(targets.values["nullable"], Value::Null);
    }

    fn task(id: &str, parent: Option<&str>) -> TaskRecord {
        let mut t = TaskRecord::new(id, format!("goal of {id}"));
        t.org = Some("acme".into());
        t.team = Some("riverside".into());
        t.story_id = Some("SV-1".into());
        t.task_type = "feature".into();
        t.subject = Some("code".into());
        t.parent_id = parent.map(str::to_owned);
        t.parent_source = parent.map(|_| "launcher".to_string());
        t.type_source = Some("brief".into());
        t.tamper_globs = vec!["**/tests/**".into()];
        t.main_skill = Some("bmad-build".into());
        t.targets = Some(TaskTargets {
            source: "default".into(),
            values: BTreeMap::from([("wall_minutes".into(), serde_json::json!(90))]),
        });
        t.layers = TaskLayers {
            skills: vec![SkillVersion {
                name: "bmad-build".into(),
                hash: Some("h1".into()),
                reason: None,
            }],
            source: Some("abc".into()),
            context: None,
        };
        t.unavailable
            .insert("context".into(), "no context checkout".into());
        t.worktree = Some("/work/wt".into());
        t.created_by = Some("lead".into());
        t
    }

    #[test]
    fn the_task_tables_are_added_without_changing_the_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        {
            let store = UsageStore::open(&path).unwrap();
            store
                .conn
                .execute_batch("DROP TABLE task; DROP TABLE task_link;")
                .unwrap();
        }
        let store = UsageStore::open(&path).unwrap();
        assert!(crate::session_usage::has_column(&store.conn, "task", "goal").unwrap());
        assert!(crate::session_usage::has_column(&store.conn, "task_link", "kind").unwrap());
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 5, "an older build must still open this store");
    }

    #[test]
    fn a_task_survives_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        let t = task("tab-1", None);
        UsageStore::open(&path)
            .unwrap()
            .insert_task_if_absent(&t)
            .unwrap();
        let back = UsageStore::open(&path).unwrap().read_task("tab-1").unwrap();
        assert_eq!(back, Some(t));
    }

    #[test]
    fn a_second_write_changes_nothing() {
        let (_d, store) = store();
        let mut t = task("tab-1", None);
        assert!(store.insert_task_if_absent(&t).unwrap());
        store
            .close_task("tab-1", DONE, Some("success"), 10)
            .unwrap();
        t.goal = "rewritten".into();
        assert!(!store.insert_task_if_absent(&t).unwrap());
        let back = store.read_task("tab-1").unwrap().unwrap();
        assert_eq!(back.goal, "goal of tab-1");
        assert_eq!(back.status, DONE);
    }

    #[test]
    fn the_tree_reads_the_same_whichever_of_parent_and_child_is_written_first() {
        for child_first in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("usage.sqlite");
            let write = |t: &TaskRecord| {
                UsageStore::open(&path)
                    .unwrap()
                    .insert_task_if_absent(t)
                    .unwrap();
            };
            let parent = task("lead-tab", None);
            let child = task("worker-tab", Some("lead-tab"));
            if child_first {
                write(&child);
                write(&parent);
            } else {
                write(&parent);
                write(&child);
            }
            // A restart keeps the tree.
            let store = UsageStore::open(&path).unwrap();
            let ids: Vec<String> = store
                .task_subtree("lead-tab")
                .unwrap()
                .into_iter()
                .map(|t| t.task_id)
                .collect();
            assert_eq!(
                ids,
                ["lead-tab", "worker-tab"],
                "child first: {child_first}"
            );
            assert_eq!(
                store
                    .read_task("worker-tab")
                    .unwrap()
                    .unwrap()
                    .parent_id
                    .as_deref(),
                Some("lead-tab")
            );
        }
    }

    #[test]
    fn a_subtree_holds_every_depth_and_no_sibling_branch() {
        let (_d, store) = store();
        for (id, parent) in [
            ("story:SV-1", None),
            ("lead", Some("story:SV-1")),
            ("build", Some("lead")),
            ("review", Some("build")),
            ("other-lead", Some("story:SV-1")),
        ] {
            store.insert_task_if_absent(&task(id, parent)).unwrap();
        }
        let ids = |root: &str| -> Vec<String> {
            store
                .task_subtree(root)
                .unwrap()
                .into_iter()
                .map(|t| t.task_id)
                .collect()
        };
        assert_eq!(ids("lead"), ["lead", "build", "review"]);
        assert_eq!(ids("story:SV-1").len(), 5);
        assert_eq!(ids("missing"), Vec::<String>::new());
    }

    #[test]
    fn a_duplicate_close_keeps_the_first() {
        let (_d, store) = store();
        store.insert_task_if_absent(&task("t", None)).unwrap();
        assert!(store.close_task("t", DONE, Some("success"), 100).unwrap());
        assert!(!store
            .close_task("t", ABANDONED, Some("failed"), 200)
            .unwrap());
        let back = store.read_task("t").unwrap().unwrap();
        assert_eq!(back.status, DONE);
        assert_eq!(back.raw_status.as_deref(), Some("success"));
        assert_eq!(back.closed_at_ms, Some(100));
    }

    #[test]
    fn links_are_generic_and_deduplicated() {
        let (_d, store) = store();
        store.insert_task_if_absent(&task("review", None)).unwrap();
        store
            .add_task_link("review", LINK_SESSION, "tab-r")
            .unwrap();
        store
            .add_task_link("review", "relates_to:reviews", "build")
            .unwrap();
        store
            .add_task_link("review", LINK_ARTIFACT, "design/SV-1/review-r1.md")
            .unwrap();
        store
            .add_task_link("review", LINK_SESSION, "tab-r")
            .unwrap();
        let links = store.task_links("review").unwrap();
        assert_eq!(links.len(), 3);
        assert!(links.contains(&TaskLink {
            kind: "relates_to:reviews".into(),
            target: "build".into()
        }));
        assert!(store.add_task_link("review", "bogus", "x").is_err());
    }

    #[test]
    fn a_patch_sets_the_label_acceptance_and_targets_with_their_source() {
        let (_d, store) = store();
        store.insert_task_if_absent(&task("t", None)).unwrap();
        let patch = TaskPatch {
            acceptance_met: Some(true),
            label: Some(("good".into(), Some("clean diff".into()))),
            targets: Some(BTreeMap::from([(
                "wall_minutes".into(),
                serde_json::json!(240),
            )])),
            ..Default::default()
        };
        assert!(store.update_task("t", &patch, Some("alice"), 77).unwrap());
        let back = store.read_task("t").unwrap().unwrap();
        assert_eq!(back.acceptance_met, Some(true));
        let label = back.label.unwrap();
        assert_eq!(
            (label.verdict.as_str(), label.by.as_deref(), label.at_ms),
            ("good", Some("alice"), 77)
        );
        let targets = back.targets.unwrap();
        assert_eq!(targets.source, "task_update");
        assert_eq!(targets.values["wall_minutes"], 240);
        assert!(store
            .update_task(
                "t",
                &TaskPatch {
                    label: Some(("meh".into(), None)),
                    ..Default::default()
                },
                None,
                1
            )
            .is_err());
        assert!(!store.update_task("missing", &patch, None, 1).unwrap());
    }

    #[test]
    fn a_type_is_a_free_tag_whose_source_follows_the_change_and_a_patch_never_moves_the_status() {
        let (_d, store) = store();
        store.insert_task_if_absent(&task("t", None)).unwrap();
        store.close_task("t", ABANDONED, Some("failed"), 3).unwrap();
        let patch = TaskPatch {
            task_type: Some("design-review".into()),
            goal: Some("a later goal".into()),
            ..Default::default()
        };
        store.update_task("t", &patch, None, 5).unwrap();
        let back = store.read_task("t").unwrap().unwrap();
        assert_eq!(back.task_type, "design-review");
        assert_eq!(back.type_source.as_deref(), Some("task_update"));
        assert_eq!(
            (
                back.status.as_str(),
                back.raw_status.as_deref(),
                back.closed_at_ms
            ),
            (ABANDONED, Some("failed"), Some(3)),
            "a late update leaves the first close alone"
        );
        assert!(store
            .update_task(
                "t",
                &TaskPatch {
                    task_type: Some("Not A Tag!".into()),
                    ..Default::default()
                },
                None,
                6
            )
            .is_err());
    }

    #[test]
    fn the_filters_pick_by_story_type_status_parent_and_session() {
        let (_d, store) = store();
        let mut other = task("b", Some("a"));
        other.story_id = Some("SV-2".into());
        other.task_type = "review".into();
        store.insert_task_if_absent(&task("a", None)).unwrap();
        store.insert_task_if_absent(&other).unwrap();
        store.close_task("b", DONE, None, 1).unwrap();
        let ids = |f: TaskFilter| -> Vec<String> {
            store
                .list_tasks(&f)
                .unwrap()
                .into_iter()
                .map(|t| t.task_id)
                .collect()
        };
        assert_eq!(
            ids(TaskFilter {
                story: Some("SV-2".into()),
                ..Default::default()
            }),
            ["b"]
        );
        assert_eq!(
            ids(TaskFilter {
                task_type: Some("review".into()),
                ..Default::default()
            }),
            ["b"]
        );
        assert_eq!(
            ids(TaskFilter {
                status: Some(OPEN.into()),
                ..Default::default()
            }),
            ["a"]
        );
        assert_eq!(
            ids(TaskFilter {
                parent: Some("a".into()),
                ..Default::default()
            }),
            ["b"]
        );
        assert_eq!(store.tasks_for_story("SV-1").unwrap().len(), 1);
        // The visibility rule applies in SQL, before the limit.
        let mine = Visibility {
            org: Some("acme".into()),
            team: Some("riverside".into()),
        };
        let theirs = Visibility {
            org: Some("acme".into()),
            team: Some("ops".into()),
        };
        let scoped = |v: &Visibility, limit| -> Vec<String> {
            store
                .list_tasks(&TaskFilter {
                    visible_to: Some(v.clone()),
                    limit: Some(limit),
                    ..Default::default()
                })
                .unwrap()
                .into_iter()
                .map(|t| t.task_id)
                .collect()
        };
        assert_eq!(scoped(&mine, 1).len(), 1);
        assert!(scoped(&theirs, 5).is_empty());
        assert!(mine.sees(&store.read_task("a").unwrap().unwrap()));
        store.add_task_link("a", LINK_SESSION, "tab-x").unwrap();
        assert_eq!(store.tasks_for_session("tab-x").unwrap()[0].task_id, "a");
        assert_eq!(store.tasks_for_session("b").unwrap()[0].task_id, "b");
    }

    #[test]
    fn a_story_root_is_made_once() {
        let (_d, store) = store();
        let root = story_root("SV-1", "ship it", None, None);
        assert_eq!(root.task_id, "story:SV-1");
        assert!(store.insert_task_if_absent(&root).unwrap());
        assert!(!store.insert_task_if_absent(&root).unwrap());
    }

    #[test]
    fn a_credential_never_reaches_a_task() {
        let (_d, store) = store();
        let secret = format!("ghp_{}", "a1B2c3D4e5".repeat(4));
        let mut t = task("t", None);
        t.goal = format!("deploy with {secret}");
        t.unavailable
            .insert("source".into(), format!("failed {secret}"));
        store.insert_task_if_absent(&t).unwrap();
        store
            .update_task(
                "t",
                &TaskPatch {
                    label: Some(("bad".into(), Some(format!("leaked {secret}")))),
                    ..Default::default()
                },
                None,
                1,
            )
            .unwrap();
        let dump: String = store
            .conn
            .query_row(
                "SELECT json_object('g', goal, 'u', unavailable, 'l', label) FROM task",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!dump.contains(&secret), "{dump}");
    }

    #[test]
    fn a_story_set_after_create_finds_the_task_and_moves_it_under_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        let mut t = task("ctx-tab", None);
        t.story_id = None;
        {
            let store = UsageStore::open(&path).unwrap();
            store.insert_task_if_absent(&t).unwrap();
            assert!(store.tasks_for_story("SV-9").unwrap().is_empty());
            let patch = TaskPatch {
                story: Some("SV-9".into()),
                ..Default::default()
            };
            assert!(store.update_task("ctx-tab", &patch, None, 1).unwrap());
        }
        let store = UsageStore::open(&path).unwrap();
        let by_story = TaskFilter {
            story: Some("SV-9".into()),
            ..Default::default()
        };
        let listed: Vec<String> = store
            .list_tasks(&by_story)
            .unwrap()
            .into_iter()
            .map(|t| t.task_id)
            .collect();
        assert!(listed.contains(&"ctx-tab".to_string()), "{listed:?}");
        let for_story: Vec<String> = store
            .tasks_for_story("SV-9")
            .unwrap()
            .into_iter()
            .map(|t| t.task_id)
            .collect();
        assert!(for_story.contains(&"ctx-tab".to_string()), "{for_story:?}");
        let back = store.read_task("ctx-tab").unwrap().unwrap();
        assert_eq!(back.story_id.as_deref(), Some("SV-9"));
        assert_eq!(back.parent_id.as_deref(), Some("story:SV-9"));
        assert!(
            store.read_task("story:SV-9").unwrap().is_some(),
            "the root is made on first use"
        );
    }

    #[test]
    fn a_task_with_a_launcher_parent_keeps_it_when_a_story_is_set() {
        let (_d, store) = store();
        store.insert_task_if_absent(&task("lead", None)).unwrap();
        let mut child = task("worker", Some("lead"));
        child.story_id = None;
        store.insert_task_if_absent(&child).unwrap();
        store
            .update_task(
                "worker",
                &TaskPatch {
                    story: Some("SV-9".into()),
                    ..Default::default()
                },
                None,
                1,
            )
            .unwrap();
        let back = store.read_task("worker").unwrap().unwrap();
        assert_eq!(back.parent_id.as_deref(), Some("lead"));
        assert_eq!(back.story_id.as_deref(), Some("SV-9"));
    }

    #[test]
    fn a_parent_can_be_changed_but_never_into_its_own_subtree() {
        let (_d, store) = store();
        for (id, parent) in [("a", None), ("b", Some("a")), ("c", Some("b")), ("x", None)] {
            store.insert_task_if_absent(&task(id, parent)).unwrap();
        }
        let set = |id: &str, parent: &str| {
            store.update_task(
                id,
                &TaskPatch {
                    parent: Some(parent.into()),
                    ..Default::default()
                },
                None,
                1,
            )
        };
        assert!(set("a", "c").is_err(), "a is above c");
        assert!(set("a", "a").is_err());
        set("c", "x").unwrap();
        assert_eq!(
            store.read_task("c").unwrap().unwrap().parent_id.as_deref(),
            Some("x")
        );
    }
}
