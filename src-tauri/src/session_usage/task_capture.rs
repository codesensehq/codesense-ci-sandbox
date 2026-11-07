// SPDX-License-Identifier: MIT
//! Making a task record from what a launch already knows: no extra ceremony.
//!
//! A launched session's task is made once, from its session record ([`SessionRecord`]), its
//! brief, its routine (if any), the skills its manifest provides and the team's task policy
//! (`tasks/task-types.yml`). The goal is plain words from the brief's first lines, the file the
//! brief points at, the routine's name and task, or the story's Purpose. The type, main skill,
//! parent and targets are resolved by the rules below, and every fact that cannot be known says
//! why in `unavailable`. Nothing here scores an agent.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use super::record::{current_head, SessionRecord};
use super::task::{
    story_root, valid_tag, SkillVersion, TaskLink, TaskRecord, TaskTargets, LINK_RELATES_PREFIX,
    LINK_SESSION, OPEN, UNCLASSIFIED,
};
use super::UsageStore;
use crate::shared::secret_scan::redact_secrets;

/// The team policy file, under the team's content folder.
pub const POLICY_FILE: &str = "tasks/task-types.yml";
/// `created_by` of a routine run's bare task, until a launch completes it.
pub const ROUTINE_START: &str = "routine";
/// The longest goal kept, in characters.
const GOAL_CAP: usize = 280;
/// The most of a brief's file read for its goal and directives.
const FILE_CAP: u64 = 16 * 1024;
const GOAL_LINES: usize = 3;
/// The most words a brief may have besides the file it points at.
const POINTER_WORDS: usize = 9;

/// A routine's name and task, carried on the run's provenance so a routine's goal is plain
/// words from the routine itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoutineBrief {
    pub name: String,
    pub task: String,
}

/// What a launch path hands the one writer for the task record, beyond the session record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskInputs {
    /// The initial prompt.
    pub brief: Option<String>,
    /// Every skill the launch's manifest provides, each with the hash of its own files.
    pub skills: Vec<SkillVersion>,
    /// The team content folder the launch resolved against: where the policy and the context
    /// commit are read from.
    pub policy_dir: Option<PathBuf>,
    /// The role the launcher gave the session (`reviewer`, ...): a reviewer's task is typed
    /// `review`.
    pub role: Option<String>,
    /// The queue role of the saved agent the session runs.
    pub queue_role: Option<String>,
    /// The task a reviewer launch reviews, as `open_workspace` named it.
    pub reviews: Option<String>,
    /// The session this launch continues: a restore relaunch's predecessor.
    pub continues: Option<String>,
    /// The story of the story worktree the launch runs in, resolved trusted-side from its
    /// working folder.
    pub workspace_story: Option<String>,
}

impl TaskInputs {
    /// The inputs of a prepared agent launch and its brief.
    pub fn from_launch(agent: &crate::agents::launch::AgentLaunch, brief: Option<&str>) -> Self {
        let skills = crate::agents::provides::skill_hashes(agent.runtime, &agent.manifest)
            .into_iter()
            .map(|s| SkillVersion {
                reason: s
                    .hash
                    .is_none()
                    .then(|| "unhashed: no manifest file backs this skill".to_string()),
                name: s.name,
                hash: s.hash,
            })
            .collect();
        let ctx = &agent.startup_context;
        let policy_dir = Some(&ctx.target)
            .filter(|t| !t.org.is_empty() && !t.team.is_empty())
            .and_then(|t| {
                team_policy_dir(
                    &ctx.home,
                    &ctx.principal,
                    &t.org,
                    &t.team,
                    t.context_root.as_ref(),
                )
            });
        Self {
            // What the launch is started with: its own prompt, else the agent's default one.
            brief: brief
                .map(str::trim)
                .filter(|b| !b.is_empty())
                .or(agent.default_prompt.as_deref())
                .map(str::to_owned),
            skills,
            policy_dir,
            ..Default::default()
        }
    }
}

/// The team's task policy: types and their default targets, the skill → type map and the
/// tamper globs. Open by default; every field is optional.
#[derive(Debug, Default, Deserialize)]
pub struct TaskPolicy {
    #[serde(default)]
    pub types: BTreeMap<String, TypePolicy>,
    #[serde(default)]
    pub skill_types: BTreeMap<String, SkillType>,
    #[serde(default)]
    pub tamper_globs: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct TypePolicy {
    #[serde(default)]
    pub targets: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize)]
pub struct SkillType {
    #[serde(rename = "type")]
    pub task_type: String,
    #[serde(default)]
    pub subject: Option<String>,
}

/// The policy in `dir`, or why there is none.
pub fn load_policy(dir: &Path) -> Result<TaskPolicy, String> {
    let path = dir.join(POLICY_FILE);
    let mut text = String::new();
    std::fs::File::open(&path)
        .map_err(|_| format!("the team has no {POLICY_FILE}"))?
        .take(256 * 1024)
        .read_to_string(&mut text)
        .map_err(|e| format!("{POLICY_FILE} cannot be read: {e}"))?;
    serde_yaml::from_str(&text).map_err(|e| format!("{POLICY_FILE} is not valid: {e}"))
}

/// What a brief asks for in lines of its own: `Task-Type:`, `Subject:` and `Targets:`.
#[derive(Debug, Default, PartialEq)]
pub struct Directives {
    pub task_type: Option<String>,
    pub subject: Option<String>,
    pub targets: BTreeMap<String, Value>,
}

fn directive<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let line = line.trim();
    let (head, rest) = line.split_at_checked(key.len())?;
    (head.eq_ignore_ascii_case(key) && rest.starts_with(':')).then(|| rest[1..].trim())
}

fn is_directive(line: &str) -> bool {
    ["task-type", "subject", "targets"]
        .iter()
        .any(|k| directive(line, k).is_some())
}

/// A type tag as the brief wrote it, with the common spellings folded to the standard types.
/// A subject as given: a free lowercase tag. Task-type aliases do not apply to it.
pub fn subject_tag(raw: &str) -> Option<String> {
    let tag = raw.trim().to_lowercase();
    valid_tag(&tag).then_some(tag)
}

pub fn canonical_type(raw: &str) -> Option<String> {
    let tag = raw.trim().to_lowercase().replace([' ', '_'], "-");
    let tag = match tag.as_str() {
        "bug-fix" | "bug" => "bugfix",
        "spike" | "research" | "exploration" => "exploratory",
        other => other,
    };
    valid_tag(tag).then(|| tag.to_string())
}

pub fn directives(text: &str) -> Directives {
    let mut out = Directives::default();
    for line in text.lines() {
        if let Some(v) = directive(line, "task-type") {
            out.task_type = out.task_type.take().or_else(|| canonical_type(v));
        } else if let Some(v) = directive(line, "subject") {
            out.subject = out
                .subject
                .take()
                .or_else(|| Some(v.trim().to_lowercase()).filter(|s| valid_tag(s)));
        } else if let Some(v) = directive(line, "targets") {
            for pair in v.split(|c: char| c == ',' || c.is_whitespace()) {
                if let Some((key, value)) = pair.split_once('=') {
                    let value = value
                        .parse::<f64>()
                        .ok()
                        .filter(|n| n.is_finite())
                        .map_or_else(
                            || Value::String(value.to_string()),
                            |n| serde_json::json!(n),
                        );
                    if valid_tag(key) {
                        out.targets.entry(key.to_string()).or_insert(value);
                    }
                }
            }
        }
    }
    out
}

/// A goal as it is kept: secrets removed, then at most [`GOAL_CAP`] characters. The one rule for
/// a goal found at launch and a goal an agent writes.
pub fn normalize_goal(text: &str) -> String {
    cap(&redact_secrets(text.trim()))
}

fn cap(text: &str) -> String {
    if text.chars().count() <= GOAL_CAP {
        return text.to_string();
    }
    let kept: String = text.chars().take(GOAL_CAP - 1).collect();
    format!("{}…", kept.trim_end())
}

/// The first non-empty lines of `text` that are not directives, joined to at most [`GOAL_CAP`]
/// characters.
pub fn goal_from_text(text: &str) -> Option<String> {
    let mut goal = String::new();
    for line in text
        .lines()
        .map(|l| l.trim().trim_start_matches('#').trim())
        .filter(|l| !l.is_empty() && !is_directive(l))
        .take(GOAL_LINES)
    {
        let next = if goal.is_empty() {
            line.to_string()
        } else {
            format!("{goal} {line}")
        };
        if next.chars().count() > GOAL_CAP {
            if goal.is_empty() {
                goal = next;
            }
            break;
        }
        goal = next;
    }
    (!goal.is_empty()).then(|| cap(&goal))
}

/// The one `.md` file a brief only points at ("Read X and follow it"), if it does.
fn md_pointer(brief: &str, cwd: Option<&Path>) -> Option<PathBuf> {
    let body: Vec<&str> = brief
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !is_directive(l))
        .collect();
    if body.is_empty() || body.len() > 2 {
        return None;
    }
    let trim = |c: char| {
        matches!(
            c,
            '`' | '\'' | '"' | '(' | ')' | ',' | ';' | ':' | '.' | '*'
        )
    };
    let tokens: Vec<&str> = body
        .iter()
        .flat_map(|l| l.split_whitespace())
        .map(|t| t.trim_start_matches(|c| c != '.' && trim(c)).trim_end_matches(trim))
        .collect();
    let mut files = tokens.iter().filter(|t| t.ends_with(".md"));
    let file = *files.next()?;
    // "Read X and follow it": a pointer says little else.
    if files.next().is_some() || tokens.len() > POINTER_WORDS {
        return None;
    }
    let path = match file.strip_prefix("~/") {
        Some(rest) => dirs::home_dir()?.join(rest),
        None => PathBuf::from(file),
    };
    let path = if path.is_absolute() {
        path
    } else {
        cwd?.join(path)
    };
    path.is_file().then_some(path)
}

fn read_capped(path: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(FILE_CAP).read_to_end(&mut bytes))
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The brief and, when the brief only points at one `.md` file, that file's head: the text the
/// goal, the directives and the main skill are read from.
pub fn brief_text(brief: &str, cwd: Option<&Path>) -> (String, bool) {
    match md_pointer(brief, cwd).and_then(|p| read_capped(&p)) {
        Some(file) => (format!("{brief}\n{file}"), true),
        None => (brief.to_string(), false),
    }
}

/// A brief's goal: its first lines, or those of the file it only points at.
pub fn goal_for_brief(brief: &str, cwd: Option<&Path>) -> Option<String> {
    match md_pointer(brief, cwd)
        .and_then(|p| read_capped(&p))
        .and_then(|f| goal_from_text(&f))
    {
        Some(goal) => Some(goal),
        None => goal_from_text(brief),
    }
}

/// A routine's goal: its name, then the first line of its task.
pub fn goal_for_routine(routine: &RoutineBrief) -> String {
    let first = routine.task.lines().map(str::trim).find(|l| !l.is_empty());
    cap(&match first {
        Some(task) => format!("{}: {task}", routine.name.trim()),
        None => routine.name.trim().to_string(),
    })
}

/// A story's goal: its title, then the first sentence of its Purpose.
pub fn goal_for_story(id: &str, title: Option<&str>, purpose: Option<&str>) -> String {
    let title = title.map(str::trim).filter(|t| !t.is_empty()).unwrap_or(id);
    let sentence = purpose.map(|p| {
        let end = p
            .match_indices(". ")
            .next()
            .map_or(p.trim_end_matches('.').len(), |(i, _)| i);
        p[..end].trim().to_string()
    });
    cap(&match sentence.filter(|s| !s.is_empty()) {
        Some(s) => format!("{title}: {s}"),
        None => title.to_string(),
    })
}

/// The first skill `text` names (as `$x`, `/x` or the bare name) that the launch provides.
pub fn main_skill(text: &str, skills: &[SkillVersion]) -> Option<String> {
    let edge = |c: char| !(c.is_alphanumeric() || c == '-' || c == '_' || c == ':');
    text.lines()
        .filter(|l| !is_directive(l))
        .flat_map(|l| l.split_whitespace())
        .map(|t| t.trim_matches(edge))
        .find_map(|t| skills.iter().find(|s| s.name == t).map(|s| s.name.clone()))
}

/// The task type a story's frontmatter `type` stands for.
fn type_of_story_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "bug" => Some("bugfix"),
        "spike" => Some("exploratory"),
        "story" => Some("feature"),
        _ => None,
    }
}

/// A role, queue role or agent name that marks a reviewer: a word that starts with `review`
/// (`reviewer`, `code-reviewer`, `dev-reviewer`), never one that only contains it (`preview`).
fn names_reviewer(label: &str) -> bool {
    str::split(label, |c: char| !char::is_alphanumeric(c))
        .any(|word| str::starts_with(&str::to_ascii_lowercase(word), "review"))
}

/// Why a launch is a reviewer's: its role (`role`), else its saved agent's queue role or name
/// (`agent`). `agent` is the record's `key@revision`; only the key's last segment is read.
pub fn reviewer_source(inputs: &TaskInputs, agent: Option<&str>) -> Option<&'static str> {
    if inputs.role.as_deref().is_some_and(names_reviewer) {
        return Some("role");
    }
    let name = agent
        .map(|a| str::split_once(a, '@').map_or(a, |(key, _)| key))
        .and_then(|key| str::rsplit(key, '/').next());
    (inputs.queue_role.as_deref().is_some_and(names_reviewer) || name.is_some_and(names_reviewer))
        .then_some("agent")
}

/// The type and subject chosen for a task, the rule that chose them, and why when none did.
#[derive(Debug, PartialEq)]
pub struct TypeChoice {
    pub task_type: String,
    pub subject: Option<String>,
    pub source: &'static str,
    pub reason: Option<String>,
}

/// Resolve a task's type: an explicit `Task-Type:` line (or `task_start`), then a routine, then
/// the policy's skill → type map, then the story's `type`, else `unclassified` with a reason.
pub fn resolve_type(
    directives: &Directives,
    routine: bool,
    main_skill: Option<&str>,
    policy: Option<&TaskPolicy>,
    story_kind: Option<&str>,
) -> TypeChoice {
    let chosen = |task_type: &str, subject: Option<String>, source: &'static str| TypeChoice {
        task_type: task_type.to_string(),
        subject: directives.subject.clone().or(subject),
        source,
        reason: None,
    };
    if let Some(t) = &directives.task_type {
        return chosen(t, None, "brief");
    }
    if routine {
        return chosen("routine", None, "routine");
    }
    if let Some(mapped) = main_skill
        .zip(policy)
        .and_then(|(skill, p)| p.skill_types.get(skill))
        .filter(|m| valid_tag(&m.task_type))
    {
        return chosen(&mapped.task_type, mapped.subject.clone(), "skill");
    }
    if let Some(t) = story_kind.and_then(type_of_story_kind) {
        return chosen(t, None, "story");
    }
    TypeChoice {
        task_type: UNCLASSIFIED.to_string(),
        subject: directives.subject.clone(),
        source: UNCLASSIFIED,
        reason: Some(
            "no rule matched: the brief names no Task-Type, it is not a routine, its main skill \
             is not in the policy's skill map and it has no story type"
                .to_string(),
        ),
    }
}

/// Everything one launch's task is made from.
pub struct Capture<'a> {
    pub session_id: &'a str,
    pub record: &'a SessionRecord,
    pub inputs: &'a TaskInputs,
    pub routine: Option<&'a RoutineBrief>,
    /// The launching session's task id, resolved trusted-side from its live handle once at
    /// reservation (a session's task id is its session id; a routine session's is its run id). No
    /// store lookup follows: the launcher's own task may not be captured yet.
    pub launcher_task: Option<&'a str>,
    /// The story file the launch's working folder belongs to.
    pub story_file: Option<&'a Path>,
    pub now_ms: i64,
}

/// Set a task's targets: the policy's defaults for its type, then `own` (from the brief or the
/// caller) over them, recorded as from `own_source`. Also copies the policy's tamper globs.
/// A fact that cannot be known says why.
pub fn apply_targets(
    t: &mut TaskRecord,
    policy: &Result<TaskPolicy, String>,
    own: BTreeMap<String, Value>,
    own_source: &str,
) {
    let mut targets = TaskTargets {
        source: "default".into(),
        values: BTreeMap::new(),
    };
    match policy {
        Ok(p) => {
            if let Some(defaults) = p.types.get(&t.task_type) {
                targets.values.extend(defaults.targets.clone());
            }
            t.tamper_globs = p.tamper_globs.clone();
        }
        Err(why) => {
            t.unavailable.insert("targets".into(), why.clone());
        }
    }
    if !own.is_empty() {
        targets.values.extend(own);
        targets.source = own_source.into();
        t.unavailable.remove("targets");
    }
    if policy.is_ok() && targets.values.is_empty() {
        t.unavailable.insert(
            "targets".into(),
            format!("{POLICY_FILE} sets none for type '{}'", t.task_type),
        );
    } else if !targets.values.is_empty() {
        t.targets = Some(targets);
    }
}

/// Copy the commit the working folder and the team content folder are at, once, as the task
/// starts. A folder outside git says why.
pub fn start_layers(t: &mut TaskRecord, cwd: Option<&Path>, policy_dir: Option<&Path>) {
    t.layers.source = cwd.and_then(current_head);
    t.layers.context = policy_dir.and_then(current_head);
    if t.layers.source.is_none() {
        t.unavailable.insert(
            "source".into(),
            "the working folder is not in a git repository".into(),
        );
    }
    if t.layers.context.is_none() {
        t.unavailable.insert(
            "context".into(),
            "the team content folder is not in a git repository".into(),
        );
    }
}

/// The team content folder of `org/team` for `principal`, if this Mac has it.
pub fn policy_dir_for(
    home: &Path,
    principal: &crate::domain::Principal,
    org: &str,
    team: &str,
) -> Option<PathBuf> {
    team_policy_dir(home, principal, org, team, None)
}

/// The existing own `content/` folder of `org/team`, through the team view.
fn team_policy_dir(
    home: &Path,
    principal: &crate::domain::Principal,
    org: &str,
    team: &str,
    chosen: Option<&crate::agents::manifest_resolve::ContextRoot>,
) -> Option<PathBuf> {
    if crate::content_engine::validate_org_slug(org)
        .or_else(|| crate::content_engine::validate_path_segment(team))
        .is_some()
    {
        return None;
    }
    crate::agents::roots::team_view(home, principal, org, team, chosen)
        .ok()?
        .own_content()
        .own_path()
        .map(Path::to_path_buf)
        .filter(|dir| dir.is_dir())
}

/// The id of the task of `session` as other tasks name it: the session's own, or its routine
/// run's when the session ran one.
pub(crate) fn task_id_of_session(store: &UsageStore, session: &str) -> Result<String, String> {
    let run = store.read_session_record(session)?.and_then(|r| r.run_id);
    match run {
        Some(run) if store.read_task(&run)?.is_some() && store.read_task(session)?.is_none() => {
            Ok(run)
        }
        _ => Ok(session.to_string()),
    }
}

/// The parent, where it came from and why a fallback: the launcher's own task when the launch
/// has one the store knows of, else the story's root task, else none.
fn resolve_parent(
    store: &UsageStore,
    c: &Capture<'_>,
    own_id: &str,
    story: Option<&str>,
    predecessor: Option<&TaskRecord>,
    story_goal: &dyn Fn() -> (String, Option<String>),
) -> Result<(Option<String>, &'static str, Option<String>), String> {
    let mut why =
        "launched by a person, by the app, or by a session that has since exited".to_string();
    if let Some(launcher) = c.launcher_task.filter(|l| !l.is_empty()) {
        if launcher == own_id {
            why = "the launcher is the session itself".into();
        } else {
            return Ok((Some(launcher.to_string()), "launcher", None));
        }
    }
    // A relaunch keeps the parent of the task it continues.
    if let Some(parent) = predecessor
        .and_then(|p| p.parent_id.as_deref())
        .filter(|parent| *parent != own_id)
    {
        return Ok((Some(parent.to_string()), "continues", None));
    }
    let story = story.filter(|_| c.routine.is_none());
    let Some(story) = story else {
        return Ok((
            None,
            "none",
            Some(format!("{why}; the launch belongs to no story")),
        ));
    };
    let (goal, kind) = story_goal();
    let mut root = story_root(story, goal, c.record.org.clone(), c.record.team.clone());
    root.created_at_ms = c.now_ms;
    match kind.as_deref().and_then(type_of_story_kind) {
        Some(t) => {
            root.task_type = t.into();
            root.type_source = Some("story".into());
        }
        None => {
            root.unavailable
                .insert("type".into(), "the story file names no type".into());
        }
    }
    store.insert_task_if_absent(&root)?;
    Ok((Some(root.task_id), "story", Some(why)))
}

/// The task of the session a launch continues, when the store knows it and it is not the
/// launch's own.
fn predecessor_task(
    store: &UsageStore,
    c: &Capture<'_>,
    own_id: &str,
) -> Result<Option<TaskRecord>, String> {
    let Some(session) = c
        .inputs
        .continues
        .as_deref()
        .filter(|s| !str::is_empty(s) && *s != c.session_id)
    else {
        return Ok(None);
    };
    let id = task_id_of_session(store, session)?;
    Ok(store.read_task(&id)?.filter(|t| t.task_id != own_id))
}

/// The `relates_to:reviews` target of the task a launch continues, and how sure that link was
/// (its `fieldSources.reviews`; `associated` when it did not say).
fn inherited_review(
    store: &UsageStore,
    predecessor: Option<&TaskRecord>,
) -> Result<Option<(String, String)>, String> {
    let Some(p) = predecessor else {
        return Ok(None);
    };
    let kind = format!("{LINK_RELATES_PREFIX}reviews");
    let mut links = store.task_links(&p.task_id)?.into_iter();
    let target = Iterator::find(&mut links, |l| l.kind == kind);
    Ok(target.map(|l| {
        let basis = p.field_sources.get("reviews").cloned().unwrap_or_else(|| "associated".into());
        (l.target, basis)
    }))
}

/// A review task's `relates_to:reviews` target and how sure it is: the task the launch named,
/// else the one the task it continues reviewed (keeping that link's basis), each only when it is
/// a task of the same story (and org and team, where both say); else the story's latest open task
/// of that team that is not a review and not its root (`associated`). `Err` says why there is none.
fn reviewed_task(
    store: &UsageStore,
    named: Option<&str>,
    inherited: Option<(String, String)>,
    owner: &TaskRecord,
    story: &str,
    own_id: &str,
) -> Result<Result<(String, String, Option<String>), String>, String> {
    let same = |a: &Option<String>, b: &Option<String>| a.is_none() || b.is_none() || a == b;
    let candidates = named
        .map(str::trim)
        .filter(|n| !str::is_empty(n))
        .map(|n| (n.to_string(), "exact".to_string(), "named"))
        .into_iter()
        .chain(inherited.map(|(target, basis)| (target, basis, "continued")));
    let mut why = None;
    for (target, basis, whence) in candidates {
        let fits = match store.read_task(&target)? {
            None => Err(format!("the {whence} task {target} is not known here")),
            Some(_) if target == own_id => Err(format!("the {whence} task {target} is this launch's own")),
            Some(t) if t.story_id.as_deref() != Some(story) => {
                Err(format!("the {whence} task {target} belongs to another story"))
            }
            Some(t) if !same(&t.org, &owner.org) || !same(&t.team, &owner.team) => {
                Err(format!("the {whence} task {target} belongs to another team"))
            }
            Some(_) => Ok(()),
        };
        match fits {
            Ok(()) => return Ok(Ok((target, basis, why))),
            Err(reason) => why = Some(why.map_or(reason.clone(), |w| format!("{w}; {reason}"))),
        }
    }
    let root = story_root(story, "", None, None).task_id;
    let latest = store
        .tasks_for_story(story)?
        .into_iter()
        .filter(|t| {
            t.status == OPEN && t.task_type != "review" && t.task_id != own_id && t.task_id != root
                && same(&t.org, &owner.org) && same(&t.team, &owner.team)
        })
        .max_by(|a, b| (a.created_at_ms, &a.task_id).cmp(&(b.created_at_ms, &b.task_id)));
    Ok(match latest {
        Some(t) => Ok((t.task_id, "associated".to_string(), why)),
        None => Err(match why {
            Some(why) => format!("{why}, and the story has no other open task"),
            None => "the launch named no task and the story has no open task to review".into(),
        }),
    })
}

/// Make the launch's task, once. A second capture for the same task changes nothing.
fn resolve_launch_task(store: &UsageStore, c: &Capture<'_>) -> Result<TaskRecord, String> {
    resolve_launch_task_with_links(store, c).map(|(t, _)| t)
}

/// The launch's task and the links it is made with (`relates_to:continues` to the task it
/// continues, `relates_to:reviews` to the task it reviews).
fn resolve_launch_task_with_links(
    store: &UsageStore,
    c: &Capture<'_>,
) -> Result<(TaskRecord, Vec<TaskLink>), String> {
    let record = c.record;
    let cwd: Option<PathBuf> = record
        .worktree
        .as_deref()
        .or(record.workspace.as_deref())
        .map(PathBuf::from);
    let own_id = c
        .routine
        .and(record.run_id.as_deref())
        .unwrap_or(c.session_id)
        .to_string();
    let mut t = TaskRecord::new(&own_id, "");
    let mut links = Vec::new();
    t.created_at_ms = c.now_ms;
    t.org = record.org.clone();
    t.team = record.team.clone();
    // The story: the launch's own, else the task it continues, else its story worktree's.
    // Nothing is guessed beyond that.
    let predecessor = predecessor_task(store, c, &own_id)?;
    let routine = c.routine.is_some();
    let (story, story_source) = match (
        record.story_id.clone(),
        predecessor.as_ref().and_then(|p| p.story_id.clone()),
        c.inputs.workspace_story.clone(),
    ) {
        _ if routine => (None, "none"),
        (Some(story), _, _) => (Some(story), "launch"),
        (None, Some(story), _) => (Some(story), "continues"),
        (None, None, Some(story)) => (Some(story), "workspace"),
        (None, None, None) => (None, "none"),
    };
    t.story_id = story;
    if let Some(p) = &predecessor {
        links.push(TaskLink {
            kind: format!("{LINK_RELATES_PREFIX}continues"),
            target: p.task_id.clone(),
        });
    }
    t.worktree = cwd.as_ref().map(|d| d.to_string_lossy().into_owned());
    t.created_by = record
        .launched_by
        .clone()
        .or_else(|| Some("codesense".into()));

    let policy = match c.inputs.policy_dir.as_deref() {
        Some(dir) => load_policy(dir),
        None => Err("the launch resolved no team content folder".to_string()),
    };
    let detail = c.story_file.map(super::story::story_detail);

    // Goal.
    let story_id = t.story_id.clone();
    let story_goal = || {
        let id = story_id.as_deref().unwrap_or_default();
        let d = detail.clone().unwrap_or_default();
        (
            goal_for_story(id, d.title.as_deref(), d.purpose.as_deref()),
            d.kind,
        )
    };
    let brief = c.inputs.brief.as_deref();
    let goal = match (c.routine, brief) {
        (Some(routine), _) => Some(goal_for_routine(routine)),
        (None, Some(b)) => goal_for_brief(b, cwd.as_deref()),
        (None, None) => None,
    };
    let goal_source = if c.routine.is_some() { "routine" }
        else if goal.is_some() { "brief" }
        else if t.story_id.is_some() { "story" }
        else { "unclassified" };
    t.field_sources.insert("goal".into(), goal_source.into());
    t.field_sources.insert("story".into(), story_source.into());
    t.goal = match goal {
        Some(g) => g,
        None if t.story_id.is_some() => {
            t.unavailable.insert(
                "goal".into(),
                "the launch had no brief: the goal is the story's".into(),
            );
            story_goal().0
        }
        None => {
            t.unavailable.insert(
                "goal".into(),
                "the launch had no brief and belongs to no story".into(),
            );
            format!(
                "{} session",
                record.handle.as_deref().unwrap_or(c.session_id)
            )
        }
    };

    // Main skill and type read the brief, and the file it points at.
    let text = match (c.routine, brief) {
        (Some(r), _) => format!("{}\n{}", r.name, r.task),
        (None, Some(b)) => brief_text(b, cwd.as_deref()).0,
        (None, None) => String::new(),
    };
    t.main_skill = main_skill(&text, &c.inputs.skills);
    if t.main_skill.is_none() {
        t.unavailable.insert(
            "mainSkill".into(),
            "the brief names no skill the launch provides".into(),
        );
    }
    let wanted = directives(&text);
    let kind = detail.as_ref().and_then(|d| d.kind.clone());
    let choice = resolve_type(
        &wanted,
        c.routine.is_some(),
        t.main_skill.as_deref(),
        policy.as_ref().ok(),
        kind.as_deref(),
    );
    t.field_sources.insert("subject".into(),
        if wanted.subject.is_some() { "brief" } else if choice.subject.is_some() { "skill" } else { "none" }.into());
    t.task_type = choice.task_type.clone();
    t.subject = choice.subject;
    t.type_source = Some(choice.source.to_string());
    if let Some(reason) = choice.reason {
        t.unavailable.insert("type".into(), reason);
    }
    // A story launch of a reviewer is a review, unless its brief or routine says otherwise.
    let typed_by_rule = matches!(choice.source, "skill" | "story") || choice.source == UNCLASSIFIED;
    if let Some(source) = reviewer_source(c.inputs, record.agent.as_deref())
        .filter(|_| typed_by_rule && t.story_id.is_some())
    {
        t.task_type = "review".into();
        t.type_source = Some(source.into());
        t.unavailable.remove("type");
    }
    if let Some(story) = t.story_id.clone().filter(|_| t.task_type == "review") {
        let inherited = inherited_review(store, predecessor.as_ref())?;
        match reviewed_task(store, c.inputs.reviews.as_deref(), inherited, &t, &story, &own_id)? {
            Ok((target, basis, why)) => {
                links.push(TaskLink {
                    kind: format!("{LINK_RELATES_PREFIX}reviews"),
                    target,
                });
                t.field_sources.insert("reviews".into(), basis);
                if let Some(why) = why {
                    t.unavailable.insert("reviews".into(), why);
                }
            }
            Err(why) => {
                t.unavailable.insert("reviews".into(), why);
            }
        }
    } else if let Some(named) = c.inputs.reviews.as_deref().map(str::trim).filter(|n| !str::is_empty(n)) {
        // A launch that names a task to review but is not a review links nothing; it says so.
        t.unavailable.insert(
            "reviews".into(),
            format!(
                "the launch named task {named} to review, but its task is not a review (no reviewer \
                 role or agent, or its brief or routine typed it), so nothing was linked"
            ),
        );
    }

    // Parent.
    let (parent, source, why) = resolve_parent(
        store,
        c,
        &own_id,
        t.story_id.as_deref(),
        predecessor.as_ref(),
        &story_goal,
    )?;
    t.parent_id = parent;
    t.parent_source = Some(source.to_string());
    if let Some(why) = why {
        t.unavailable.insert("parent".into(), why);
    }

    apply_targets(&mut t, &policy, wanted.targets, "brief");

    // Layer versions, copied once.
    start_layers(&mut t, cwd.as_deref(), c.inputs.policy_dir.as_deref());
    t.layers.skills = c.inputs.skills.clone();

    t.goal = normalize_goal(&t.goal);
    Ok((t, links))
}

/// Capture a launch with the same resolver used when a routine first starts.
pub fn capture_launch_task(store: &UsageStore, c: &Capture<'_>) -> Result<TaskRecord, String> {
    let (t, links) = resolve_launch_task_with_links(store, c)?;
    persist_capture(store, &t, Some(c.session_id), &links)
}

fn persist_capture(store: &UsageStore, t: &TaskRecord, session: Option<&str>, links: &[TaskLink]) -> Result<TaskRecord, String> {
    if !store.insert_task_if_absent(t)? {
        store.complete_launch_task(t)?;
    }
    if let Some(session) = session {
        store.add_task_link(&t.task_id, LINK_SESSION, session)?;
    }
    for link in links {
        store.add_task_link(&t.task_id, &link.kind, &link.target)?;
    }
    if let Some(closed) = store.read_task(&t.task_id)?.filter(|task| task.status != super::task::OPEN) {
        super::task_outcome::close_as(store, &t.task_id, &closed.status,
            closed.raw_status.as_deref().unwrap_or(&closed.status),
            closed.closed_at_ms.unwrap_or(t.created_at_ms))?;
    }
    Ok(store.read_task(&t.task_id)?.unwrap_or_else(|| t.clone()))
}

/// The known facts when any routine run starts. A later launch fills only provisional facts.
pub struct RoutineStart<'a> {
    pub org: &'a str,
    pub team: &'a str,
    pub run_id: &'a str,
    pub routine: &'a RoutineBrief,
    pub policy_dir: Option<&'a Path>,
    pub cwd: Option<&'a Path>,
    pub session_id: Option<&'a str>,
    pub now_ms: i64,
}

pub fn capture_routine_start(store: &UsageStore, r: &RoutineStart<'_>) -> Result<(), String> {
    let session_record = r.session_id.map(|id| store.read_session_record(id)).transpose()?.flatten();
    let launch = r.session_id
        .map(|id| task_id_of_session(store, id))
        .transpose()?
        .map(|id| store.read_task(&id))
        .transpose()?.flatten();
    let mut record = SessionRecord::new(r.session_id.unwrap_or(r.run_id));
    record.org = Some(r.org.into());
    record.team = Some(r.team.into());
    record.run_id = Some(r.run_id.into());
    record.launched_by = Some(ROUTINE_START.into());
    record.worktree = r.cwd.map(|p| p.to_string_lossy().into_owned())
        .or_else(|| session_record.as_ref().and_then(|s| s.worktree.clone().or(s.workspace.clone())))
        .or_else(|| launch.as_ref().and_then(|t| t.worktree.clone()));
    let inputs = TaskInputs {
        policy_dir: r.policy_dir.map(Path::to_path_buf),
        skills: launch.as_ref().map(|t| t.layers.skills.clone()).unwrap_or_default(),
        ..Default::default()
    };
    let mut t = resolve_launch_task(store, &Capture {
        session_id: &record.session_id, record: &record, inputs: &inputs,
        routine: Some(r.routine), launcher_task: None, story_file: None, now_ms: r.now_ms,
    })?;
    if t.layers.skills.is_empty() {
        t.unavailable.insert("skills".into(), "the immutable launch skill snapshot is not captured".into());
    }
    persist_capture(store, &t, r.session_id, &[]).map(|_| ())
}

/// Capture a started routine in the usage store beside its coordinator. Best effort.
pub fn record_routine_start(coordinator: &Path, r: &RoutineStart<'_>) {
    let Some(path) = super::task_outcome::usage_store_beside(coordinator) else {
        return;
    };
    if let Err(e) = UsageStore::open(&path).and_then(|store| capture_routine_start(&store, r)) {
        log::warn!(target: "session_usage", "routine task {} not made: {e}", r.run_id);
    }
}

/// The story of the story worktree holding `cwd`: its id and file.
fn story_link_of(home: &Path, cwd: &str) -> Option<super::StoryLink> {
    super::StoryResolver::for_home(home, Default::default()).worktree_link(cwd)
}

/// Make a launch's task in the real store, off the async runtime. Best effort: a failure is
/// logged and the launch goes on.
pub async fn record_task_async(
    path: Result<PathBuf, String>,
    record: SessionRecord,
    mut inputs: TaskInputs,
    routine: Option<RoutineBrief>,
    launcher_task: Option<String>,
) {
    let result = tokio::task::spawn_blocking(move || {
        let path = path?;
        let cwd = record.worktree.as_deref().or(record.workspace.as_deref());
        let link = path.parent().and_then(|home| cwd.and_then(|cwd| story_link_of(home, cwd)));
        inputs.workspace_story = link.as_ref().map(|link| link.story_id.clone());
        let story_file = link.map(|link| link.source);
        let store = UsageStore::open(&path)?;
        capture_launch_task(
            &store,
            &Capture {
                session_id: &record.session_id,
                record: &record,
                inputs: &inputs,
                routine: routine.as_ref(),
                launcher_task: launcher_task.as_deref(),
                story_file: story_file.as_deref(),
                now_ms: record.recorded_at_ms,
            },
        )
        .map(|_| ())
    })
    .await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => log::warn!(target: "session_usage", "task not written: {e}"),
        Err(e) => log::warn!(target: "session_usage", "task write failed: {e}"),
    }
}

#[cfg(test)]
mod tests;
