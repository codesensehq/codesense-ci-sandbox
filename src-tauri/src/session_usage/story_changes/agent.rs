// SPDX-License-Identifier: MIT
//! The bounded answer an agent gets from `story_changes` over MCP: a pure projection of the
//! same read the page bridge returns. Sessions and tasks are summaries unless included in full,
//! every array is paged by `limit` with `pages.<array>: {total, truncated, cursor}`, nested file
//! lists are capped, and the answer fits [`AGENT_ANSWER_BUDGET`]. Totals are never touched.
use super::*;
use crate::commands::agent_read_model::AGENT_ANSWER_BUDGET;

/// Every array's page size when the agent names no `limit`.
pub(crate) const AGENT_LIMIT: usize = 20;
/// The most `prs[].files` and `commits[].files` entries an agent answer lists, with
/// `filesTotal` when cut; `actual` pages every file.
pub(crate) const NESTED_FILES: usize = 20;
/// The longest task `goal` a task summary keeps, in chars.
pub(crate) const GOAL_CHARS: usize = 200;
/// Array cursors: `sca:<array>:<key>:<last id>`. Commit cursors keep `nextCursor`'s form.
const CURSOR: &str = "sca:";

/// `story_changes` over MCP: the shared read's arguments plus the agent-only shapes, so the
/// page bridge's arguments stay [`Params`].
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentParams {
    /// `ranges`: the current commit page's introduced added-line ranges (path, resolver
    /// session/native IDs, basis and reasons) with rangesBasis and rangesReason; no-note and
    /// merge commits have empty ranges with a reason; without it these fields are omitted.
    /// `sessions.full`: each session as session_read returns it, not its summary
    /// {sessionId,nativeSessionIds,basis,agent,cli,model,branch,worktree,timing,costUsd,
    /// costReason}. `tasks.full`: each task row with its task_read outcome, not its summary
    /// {taskId,parentId,taskType,status,goal (200 chars, goalTruncated),mainSkill,createdAtMs,
    /// closedAtMs,reviews}.
    #[serde(default)]
    pub include: Vec<AgentInclude>,
    /// Story id, for example DEMO-29.
    pub story_id: String,
    /// Page size of every array (default 20, at most 500). Totals cover the full set.
    pub limit: Option<usize>,
    /// `pages.<array>.cursor` (or `nextCursor` for commits) from the previous answer, same
    /// story: the next page of that array only, the other arrays empty. Cursors over a
    /// changed set are refused.
    pub cursor: Option<String>,
    /// Return only these arrays (plus `totals`); arrays not asked for are absent, not empty.
    /// `tasks`, `sessions` and `reviews` never read git; `commits`, `prs`, `branches`,
    /// `worktrees` and `actual` do. Omit for every array. Without a git array `totals.commits`,
    /// `files` and `lines` are null, with `unavailable.commits`, `files` and `lines` saying why.
    /// `reviews`: the story's complete review rounds, oldest report first, as {id, round, phase,
    /// scope, skill, subjectKind, subjectRef, subjectRev, high, medium, low, dismissed,
    /// converged, reportedAt} (high/medium/low kept findings, dismissed dropped at triage);
    /// unreadable markers give `unavailable.reviews`. A cursor into an array not returned is stale.
    pub only: Option<Vec<Only>>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[schemars(inline)]
pub enum AgentInclude {
    Ranges,
    #[serde(rename = "sessions.full")]
    SessionsFull,
    #[serde(rename = "tasks.full")]
    TasksFull,
}

/// The arrays an agent pages here, in answer order, with each row's id. `commits` pages
/// through the read itself (`nextCursor`).
const ARRAYS: [(&str, fn(&Value) -> Option<String>); 9] = [
    ("sessions", session_id),
    ("tasks", |row| text(&row["taskId"])),
    ("branches", |row| text(&row["name"])),
    ("prs", |row| text(&row["mergedSha"])),
    ("worktrees", |row| text(&row["path"])),
    ("actual", |row| text(&row["path"])),
    ("planned", text),
    ("plannedUntouched", text),
    ("reviews", |row| text(&row["id"])),
];

fn text(value: &Value) -> Option<String> {
    Option::map(Value::as_str(value), str::to_owned)
}

fn session_id(row: &Value) -> Option<String> {
    let id = Option::or_else(text(&row["sessionId"]), || {
        text(&row["session"]["sessionId"])
    });
    Option::or_else(id, || text(&row["nativeSessionIds"][0]))
}

/// `(array, key, last id)` of an agent array cursor; `None` for a commit cursor.
fn array_cursor(cursor: Option<&str>) -> Option<(&str, &str, &str)> {
    let mut parts = str::splitn(str::strip_prefix(cursor?, CURSOR)?, 3, ':');
    Some((
        Iterator::next(&mut parts)?,
        Iterator::next(&mut parts)?,
        Iterator::next(&mut parts)?,
    ))
}

/// Binds an array cursor to the story and that array's whole ordered id list.
fn array_key(story: &str, ids: &[String]) -> String {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(story, &mut hash);
    std::hash::Hash::hash(ids, &mut hash);
    format!("{:x}", std::hash::Hasher::finish(&hash))
}

/// What the agent's read asks the shared backend: pages [`AGENT_LIMIT`] long unless it
/// names a limit, ranges when included, and an array cursor read from the first commit page.
pub(crate) fn agent_read_params(params: &AgentParams) -> Params {
    let ranges = <[AgentInclude]>::contains(&params.include, &AgentInclude::Ranges);
    Params {
        include: if ranges {
            vec![Include::Ranges]
        } else {
            std::vec::Vec::new()
        },
        story_id: std::string::String::clone(&params.story_id),
        limit: Some(usize::clamp(
            Option::unwrap_or(params.limit, AGENT_LIMIT),
            1,
            500,
        )),
        cursor: Option::filter(Option::clone(&params.cursor), |cursor| {
            Option::is_none(&array_cursor(Some(cursor)))
        }),
        only: Option::clone(&params.only),
    }
}

/// The prefix of the read's commit cursors (`sc:<key>:`), for a commit page the budget ends
/// early: from its `nextCursor` or the commit cursor it was asked with, else (the first page
/// holds every commit) the key of that complete set.
fn commit_key(full: &Value, params: &AgentParams) -> String {
    let given = Option::or_else(text(&full["nextCursor"]), || {
        agent_read_params(params).cursor
    });
    let split = Option::and_then(Option::as_deref(&given), |cursor| {
        str::rsplit_once(cursor, ':')
    });
    if let Some((prefix, _)) = split {
        return format!("{prefix}:");
    }
    let rows = Option::unwrap_or_default(Option::map(
        Value::as_array(&full["commits"]),
        std::vec::Vec::as_slice,
    ));
    let shas = Iterator::filter_map(<[Value]>::iter(rows), |c| Value::as_str(&c["sha"]));
    git::commit_cursor_key(&params.story_id, shas)
}

/// The `only` value that returns `array`.
fn only_of(array: &str) -> Option<Only> {
    let owner = Iterator::find(&mut OWNED.iter(), |(_, keys)| <[&str]>::contains(keys, &array));
    owner.map(|(only, _)| *only)
}

/// One paged array of the answer: its cursor key, full length, first listed position and
/// the ids it lists.
struct Page {
    key: String,
    total: usize,
    start: usize,
    ids: Vec<String>,
}

/// The agent's answer from `full`, the bridge read of [`agent_read_params`]`(params)`.
/// A cursor read answers only the array it names (commits for `nextCursor`): the other
/// arrays come back empty and absent from `pages`, and `totals` stay those of the full set.
/// Past [`AGENT_ANSWER_BUDGET`] the heaviest array (commits included) ends its page early,
/// its cursor resuming at the first row left out.
pub(crate) fn agent_projection(mut full: Value, params: &AgentParams) -> Result<Value, String> {
    let story = str::to_lowercase(str::trim(&params.story_id));
    let read = agent_read_params(params);
    let limit = Option::unwrap_or(read.limit, AGENT_LIMIT);
    let cursor = Option::as_deref(&params.cursor);
    let paged = array_cursor(cursor);
    let only = match (cursor, paged) {
        (None, _) => None,
        (Some(_), Some((array, _, _))) => Some(array),
        (Some(_), None) => Some("commits"),
    };
    let known = |only: &str| {
        only == "commits" || Iterator::any(&mut <[_]>::iter(&ARRAYS), |(name, _)| *name == only)
    };
    if Option::is_some_and(only, |only| !known(only)) {
        return Err(std::string::String::from(git::STALE_CURSOR));
    }
    // A cursor into an array this read does not return is stale.
    if Option::is_some_and(only, |array| {
        Option::is_some_and(only_of(array), |owner| !Params::asks_for(&read, owner))
    }) {
        return Err(std::string::String::from(git::STALE_CURSOR));
    }
    let commit_key = commit_key(&full, params);
    let commits_asked = Params::asks_for(&read, Only::Commits);
    let commits_listed = commits_asked && Option::is_none_or(only, |only| only == "commits");
    let answer = Option::ok_or(
        Value::as_object_mut(&mut full),
        "story_changes read is not an object",
    )?;
    if !<[AgentInclude]>::contains(&params.include, &AgentInclude::SessionsFull) {
        summarize(answer, "sessions", session_summary);
    }
    if !<[AgentInclude]>::contains(&params.include, &AgentInclude::TasksFull) {
        summarize(answer, "tasks", task_summary);
    }
    for array in ["prs", "commits"] {
        if let Some(Value::Array(rows)) = serde_json::Map::get_mut(answer, array) {
            for row in rows {
                cap_files(row);
            }
        }
    }
    if commits_asked && !commits_listed {
        serde_json::Map::insert(answer, std::string::String::from("commits"), json!([]));
    }

    let mut listed: BTreeMap<&str, Page> = std::collections::BTreeMap::new();
    for (name, id) in ARRAYS {
        let Some(Value::Array(rows)) = serde_json::Map::get_mut(answer, name) else {
            continue;
        };
        if Option::is_some_and(only, |only| only != name) {
            std::vec::Vec::clear(rows);
            continue;
        }
        let ids: Vec<String> = Iterator::collect(Iterator::map(<[Value]>::iter(rows), |row| {
            Option::unwrap_or_default(id(row))
        }));
        let key = array_key(&story, &ids);
        let start = match Option::filter(paged, |(array, _, _)| *array == name) {
            None => 0,
            Some((_, given, after)) => {
                let at = Iterator::position(&mut <[String]>::iter(&ids), |id| id == after);
                let resumed = Option::map(Option::filter(at, |_| given == key), |n| n + 1);
                Option::ok_or(resumed, git::STALE_CURSOR)?
            }
        };
        let total = std::vec::Vec::len(&ids);
        let end = usize::min(start + limit, total);
        let page: Vec<Value> = Iterator::collect(std::vec::Vec::drain(rows, start..end));
        *rows = page;
        let ids = <[String]>::to_vec(&ids[start..end]);
        std::collections::BTreeMap::insert(
            &mut listed,
            name,
            Page {
                key,
                total,
                start,
                ids,
            },
        );
    }

    // Each page's rows by weight, newest last; the heaviest array gives up its last row
    // until the whole answer, `pages` included, is measured within the budget.
    let mut weights: BTreeMap<&str, Vec<usize>> = std::collections::BTreeMap::new();
    let mut names: Vec<&str> =
        Iterator::collect(Iterator::copied(std::collections::BTreeMap::keys(&listed)));
    if commits_listed {
        std::vec::Vec::push(&mut names, "commits");
    }
    for name in names {
        if let Some(Value::Array(rows)) = serde_json::Map::get(answer, name) {
            let mut sizes = std::vec::Vec::new();
            for row in rows {
                let bytes = Result::map_err(serde_json::to_vec(row), |e| ToString::to_string(&e))?;
                std::vec::Vec::push(&mut sizes, std::vec::Vec::len(&bytes));
            }
            std::collections::BTreeMap::insert(&mut weights, name, sizes);
        }
    }
    // The answer is measured once; each dropped row takes its own bytes and one comma
    // off, a rebuilt `nextCursor` its length difference, and `pages` is measured as built.
    let json_len = |value: &Value| {
        let bytes = Result::map_err(serde_json::to_vec(value), |e| ToString::to_string(&e))?;
        Ok::<_, String>(std::vec::Vec::len(&bytes))
    };
    serde_json::Map::remove(answer, "pages");
    let mut size = json_len(&Value::Object(serde_json::Map::clone(answer)))?;
    loop {
        let measured = pages(answer, &listed, commits_listed);
        // `,"pages":` and the object.
        if size + str::len(r#","pages":"#) + json_len(&measured)? <= AGENT_ANSWER_BUDGET {
            break;
        }
        let candidates =
            Iterator::filter(std::collections::BTreeMap::iter(&weights), |(_, sizes)| {
                std::vec::Vec::len(sizes) > 1
            });
        let heaviest = Iterator::max_by_key(candidates, |(_, sizes)| {
            Iterator::sum::<usize>(<[usize]>::iter(sizes))
        });
        let Some(name) = Option::map(heaviest, |(name, _)| *name) else {
            break;
        };
        if let Some(sizes) = std::collections::BTreeMap::get_mut(&mut weights, name) {
            size -= Option::unwrap_or(std::vec::Vec::pop(sizes), 0) + 1;
        }
        if let Some(Value::Array(rows)) = serde_json::Map::get_mut(answer, name) {
            std::vec::Vec::pop(rows);
        }
        if let Some(page) = std::collections::BTreeMap::get_mut(&mut listed, name) {
            std::vec::Vec::pop(&mut page.ids);
        } else if let Some(last) = last_sha(&answer["commits"]) {
            let next = Value::String(format!("{commit_key}{last}"));
            let before = json_len(&answer["nextCursor"])?;
            size = size + json_len(&next)? - before;
            serde_json::Map::insert(answer, std::string::String::from("nextCursor"), next);
        }
    }
    let measured = pages(answer, &listed, commits_listed);
    serde_json::Map::insert(answer, std::string::String::from("pages"), measured);
    Ok(full)
}

/// The sha of the last listed commit.
fn last_sha(commits: &Value) -> Option<String> {
    let rows = Value::as_array(commits)?;
    text(&<[Value]>::last(rows)?["sha"])
}

/// `pages` for the answer as it stands: each listed array's `{total, truncated, cursor}`,
/// and the commits' from `total` and `nextCursor`.
fn pages(
    answer: &serde_json::Map<String, Value>,
    listed: &BTreeMap<&str, Page>,
    commits: bool,
) -> Value {
    let mut pages = serde_json::Map::new();
    for (name, page) in listed {
        let truncated = page.start + std::vec::Vec::len(&page.ids) < page.total;
        let last = Option::filter(<[String]>::last(&page.ids), |_| truncated);
        let cursor = Option::map(last, |last| format!("{CURSOR}{name}:{}:{last}", page.key));
        let row = json!({"total": page.total, "truncated": truncated, "cursor": cursor});
        serde_json::Map::insert(&mut pages, std::string::String::from(*name), row);
    }
    if commits {
        let field = |key: &str| {
            Option::unwrap_or(
                <Option<&Value>>::cloned(serde_json::Map::get(answer, key)),
                Value::Null,
            )
        };
        let (next, total) = (field("nextCursor"), field("total"));
        let row = json!({"total": total, "truncated": !Value::is_null(&next), "cursor": next});
        serde_json::Map::insert(&mut pages, std::string::String::from("commits"), row);
    }
    Value::Object(pages)
}

fn summarize(
    answer: &mut serde_json::Map<String, Value>,
    array: &str,
    summary: fn(&Value) -> Value,
) {
    if let Some(Value::Array(rows)) = serde_json::Map::get_mut(answer, array) {
        for row in rows {
            *row = summary(row);
        }
    }
}

/// A session row as `session_read`'s summary facts: identity, launch, timing totals and cost.
fn session_summary(row: &Value) -> Value {
    let record = &row["session"];
    let timing = &record["timing"];
    json!({
        "sessionId": session_id(row),
        "nativeSessionIds": row["nativeSessionIds"],
        "basis": row["basis"],
        "agent": record["agent"],
        "cli": record["cli"],
        "model": record["model"],
        "branch": record["branch"],
        "worktree": record["worktree"],
        "timing": if Value::is_object(timing) {
            json!({
                "startedAtMs": timing["startedAtMs"],
                "endedAtMs": timing["endedAtMs"],
                "wallMs": timing["wallMs"],
                "activeMs": timing["activeMs"],
                "idleMs": timing["idleMs"],
            })
        } else {
            Value::Null
        },
        "costUsd": row["cost"]["totalUsd"],
        "costReason": row["cost"]["reason"],
    })
}

/// A task row as its identity, kind, state, review ids and the start of its goal, with
/// `goalTruncated` when the goal was cut.
fn task_summary(row: &Value) -> Value {
    let goal = Value::as_str(&row["goal"]);
    let kept = Option::map(goal, |goal| {
        Iterator::collect::<String>(Iterator::take(str::chars(goal), GOAL_CHARS))
    });
    let mut summary = json!({
        "taskId": row["taskId"],
        "parentId": row["parentId"],
        "taskType": row["taskType"],
        "status": row["status"],
        "goal": kept,
        "mainSkill": row["mainSkill"],
        "createdAtMs": row["createdAtMs"],
        "closedAtMs": row["closedAtMs"],
        "reviews": row["reviews"],
    });
    if Option::is_some_and(goal, |goal| {
        Option::is_some(&Iterator::nth(&mut str::chars(goal), GOAL_CHARS))
    }) {
        summary["goalTruncated"] = Value::Bool(true);
    }
    summary
}

/// Keep the first [`NESTED_FILES`] of a row's `files`, with `filesTotal` when cut.
fn cap_files(row: &mut Value) {
    let Some(Value::Array(files)) = Value::get_mut(row, "files") else {
        return;
    };
    let total = std::vec::Vec::len(files);
    if total > NESTED_FILES {
        std::vec::Vec::truncate(files, NESTED_FILES);
        row["filesTotal"] = Value::from(total);
    }
}

/// The MCP answer: the shared read of [`agent_read_params`], projected for an agent.
pub async fn agent_current(params: &AgentParams, scope: &CallerScope) -> Result<Value, String> {
    agent_projection(current(&agent_read_params(params), scope).await?, params)
}
