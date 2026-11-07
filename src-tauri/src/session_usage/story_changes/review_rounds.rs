// SPDX-License-Identifier: MIT
//! A story's review rounds, from its team's `review-rounds` markers (SV-CDE-83 I2).
//!
//! The rule is `check_records.py` `review_rounds()`: a review round is a complete marker
//! (`load_snapshot` keeps those in `rounds` and never counts `inflight`) whose `phase` is
//! `review`. Wave and use markers (round 0) are observations, not review rounds. A marker is
//! complete when every stamp key is present or named in `stampNote` (`missing_stamps`).
//! [`STAMP_KEYS`], that test and the phase rule are ported from the script; the shared fixture
//! under `fixtures/review-rounds/` pins the two to the same answers.

use crate::context_repo::caller_scope::CallerScope;
use crate::records::{Record, Scope, Store};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The ADT-35 collection a team declares in `content/schema/review-rounds.yaml`.
pub(super) const COLLECTION: &str = "review-rounds";

/// `check_records.py` `STAMP_KEYS`, in its order.
pub(super) const STAMP_KEYS: [&str; 17] = [
    "subjectRev",
    "reviewerRevision",
    "reviewerModel",
    "reviewerRuntime",
    "author",
    "authorRevision",
    "authorModel",
    "authorRuntime",
    "sourceRepo",
    "story",
    "storyType",
    "storyPoints",
    "epic",
    "teamKey",
    "taskType",
    "size",
    "sizeUnit",
];

/// One complete marker of the story: its record id and the reviewer task it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Marker {
    pub id: String,
    pub task: Option<String>,
}

/// A field as `check_records.py` `parse_text` reads it: its text, `""` when absent.
fn text(fields: &BTreeMap<String, Value>, key: &str) -> String {
    match fields.get(key) {
        None | Some(Value::Null) => "".into(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => ToString::to_string(other),
    }
}

/// `re.search(rf'\b{key}\b', note)`, with Python's Unicode word characters.
fn names(note: &str, key: &str) -> bool {
    let word = |c: Option<char>| c.is_some_and(|c| char::is_alphanumeric(c) || c == '_');
    str::match_indices(note, key).any(|(at, _)| {
        !word(note[..at].chars().next_back()) && !word(note[at + key.len()..].chars().next())
    })
}

/// `missing_stamps`: the stamp keys neither present nor named in `stampNote`.
pub(super) fn missing_stamps(fields: &BTreeMap<String, Value>) -> Vec<&'static str> {
    let note = text(fields, "stampNote");
    STAMP_KEYS
        .into_iter()
        .filter(|key| str::is_empty(str::trim(&text(fields, key))) && !names(&note, key))
        .collect()
}

/// `story_key(a) == story_key(b)`: a story id compared case-insensitively when it is ASCII.
fn same_story(a: &str, b: &str) -> bool {
    let (a, b) = (str::trim(a), str::trim(b));
    if str::is_ascii(a) && str::is_ascii(b) {
        str::eq_ignore_ascii_case(a, b)
    } else {
        a == b
    }
}

/// `is_review_round`: a marker's `phase` is `review`.
pub(super) fn is_review_round(fields: &BTreeMap<String, Value>) -> bool {
    text(fields, "phase") == "review"
}

/// `review_rounds(snapshot, story)`: the complete `phase: review` markers among `records`
/// whose `story` is `story`.
fn rounds_of_story<'a>(records: &'a [Record], story: &'a str) -> impl Iterator<Item = &'a Record> {
    records
        .iter()
        .filter(move |r| same_story(&text(&r.fields, "story"), story))
        .filter(|r| missing_stamps(&r.fields).is_empty() && is_review_round(&r.fields))
}

/// `review_rounds(snapshot, story)`: the story's markers by id.
pub(super) fn review_rounds_for_story(records: &[Record], story: &str) -> Vec<Marker> {
    let mut out: Vec<Marker> = rounds_of_story(records, story)
        .map(|r| Marker {
            id: r.id.clone(),
            task: Some(text(&r.fields, "task"))
                .map(|t| str::trim(&t).to_owned())
                .filter(|t| !str::is_empty(t)),
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// A number field of a record: a number, or text that holds one.
fn number(fields: &BTreeMap<String, Value>, key: &str) -> Option<i64> {
    match fields.get(key)? {
        Value::Number(n) => serde_json::Number::as_i64(n)
            .or_else(|| serde_json::Number::as_f64(n).map(|f| f as i64)),
        Value::String(s) => Result::ok(str::parse::<i64>(str::trim(s))),
        _ => None,
    }
}

/// A `yes-no` field: true for `yes` or true.
fn yes(fields: &BTreeMap<String, Value>, key: &str) -> Option<bool> {
    match fields.get(key)? {
        Value::Bool(b) => Some(*b),
        Value::String(s) => match str::trim(s) {
            "yes" | "true" => Some(true),
            "no" | "false" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// One round as a page shows it: what the marker records of the round and its kept and
/// dismissed findings, each only when the record holds it. No reviewer, session or task
/// identity, and no finding ids.
fn round_row(record: &Record) -> Value {
    let fields = &record.fields;
    let mut row = serde_json::Map::new();
    serde_json::Map::insert(&mut row, "id".into(), Value::String(record.id.clone()));
    for key in ["phase", "scope", "skill", "subjectKind", "subjectRef", "subjectRev"] {
        let value = text(fields, key);
        if !str::is_empty(&value) {
            serde_json::Map::insert(&mut row, key.into(), Value::String(value));
        }
    }
    for key in ["round", "high", "medium", "low", "dismissed", "reportedAt"] {
        if let Some(n) = number(fields, key) {
            serde_json::Map::insert(&mut row, key.into(), Value::from(n));
        }
    }
    if let Some(converged) = yes(fields, "converged") {
        serde_json::Map::insert(&mut row, "converged".into(), Value::Bool(converged));
    }
    Value::Object(row)
}

/// The rows of `story`'s complete review rounds.
pub(super) fn round_rows_for_story(records: &[Record], story: &str) -> Vec<Value> {
    Iterator::collect(Iterator::map(rounds_of_story(records, story), round_row))
}

#[cfg(test)]
static LISTED: std::sync::Mutex<BTreeMap<PathBuf, usize>> = std::sync::Mutex::new(BTreeMap::new());

#[cfg(test)]
fn count_listing(root: &Path) {
    *BTreeMap::entry(&mut LISTED.lock().unwrap(), root.to_owned()).or_default() += 1;
}
#[cfg(not(test))]
fn count_listing(_root: &Path) {}

/// How many times a review-rounds collection under a folder named `home_name` was listed (tests
/// only).
#[cfg(test)]
pub(super) fn listed(home_name: &std::ffi::OsStr) -> usize {
    Iterator::sum(Iterator::filter_map(
        BTreeMap::iter(&LISTED.lock().unwrap()),
        |(root, count)| {
            Iterator::any(&mut Path::components(root), |c| c.as_os_str() == home_name)
                .then_some(*count)
        },
    ))
}

/// The markers of one team root (a folder holding `content/schema/review-rounds.yaml`), as
/// `caller` may read them. `None` when the root declares no such collection.
pub(super) fn team_records(
    root: &Path,
    caller: &CallerScope,
) -> Result<Option<Vec<Record>>, String> {
    count_listing(root);
    let collections = crate::records::schema::load_collections_for(root, Scope::Team, caller)?;
    let mut all = collections.into_iter();
    let Some(collection) = Iterator::find(&mut all, |c| c.name == COLLECTION) else {
        return Ok(None);
    };
    let store = Store::new(root, collection, None)?.with_caller(caller.clone());
    store.list_records().map(Some)
}

/// The team roots of `teams` (`(org, team)`) on this Mac that declare the collection, for every
/// local profile. Only the story's own teams are read: never a parent's or a sibling's.
fn team_roots(home: &Path, teams: &BTreeSet<(String, String)>) -> Vec<PathBuf> {
    let mut roots = BTreeSet::new();
    for principal in crate::context_repo::org_enumeration::principals(home) {
        for (org, team) in teams {
            let Ok(org_root) = crate::context_repo::path_to_org_root(home, &principal, org) else {
                continue;
            };
            let content = crate::context_repo::path_to_team_content_dir(&org_root, team);
            let declared = content.join("schema").join(format!("{COLLECTION}.yaml"));
            if let Some(root) = Path::parent(&content).filter(|_| Path::is_file(&declared)) {
                roots.insert(root.to_path_buf());
            }
        }
    }
    roots.into_iter().collect()
}

/// The team (`(org, team)`) whose content folder holds `story_file` under `org`'s root.
pub(super) fn team_of_story_file(
    home: &Path,
    org: &str,
    story_file: &Path,
) -> Option<(String, String)> {
    crate::context_repo::org_enumeration::principals(home)
        .into_iter()
        .find_map(|principal| {
            let org_root = crate::context_repo::path_to_org_root(home, &principal, org).ok()?;
            let relative = story_file.strip_prefix(&org_root).ok()?;
            let team = relative
                .components()
                .next()?
                .as_os_str()
                .to_str()?
                .to_owned();
            Some((org.to_owned(), team))
        })
}

pub(super) const NO_TEAM: &str = "the story's team is not known on this Mac, so its review-rounds markers cannot be read";
pub(super) const NO_COLLECTION: &str = "the story's team declares no review-rounds collection on this Mac";

/// Whether `reason` says there is nothing to read (no known team, or none declares the
/// collection), as opposed to markers that exist and could not be read.
pub(super) fn is_absent(reason: &str) -> bool {
    reason == NO_TEAM || reason == NO_COLLECTION
}

/// The story's complete markers across its own teams, with the rows a page shows (oldest report
/// first). Marker ids are unique per team folder; a marker seen under two profiles' roots counts
/// once.
pub(super) fn story_markers(
    home: &Path,
    teams: &BTreeSet<(String, String)>,
    story: &str,
    caller: &CallerScope,
) -> Result<(Vec<Marker>, Vec<Value>), &'static str> {
    story_markers_from(home, teams, story, &mut |root| {
        team_records(root, caller).map(|found| Option::map(found, std::sync::Arc::new))
    })
}

/// `story_markers` with the team root's records read by `read`, so a batch reads each root once
/// and hands every story its records.
pub(super) fn story_markers_from(
    home: &Path,
    teams: &BTreeSet<(String, String)>,
    story: &str,
    read: &mut dyn FnMut(&Path) -> Result<Option<std::sync::Arc<Vec<Record>>>, String>,
) -> Result<(Vec<Marker>, Vec<Value>), &'static str> {
    if BTreeSet::is_empty(teams) {
        return Err(NO_TEAM);
    }
    let mut declared = false;
    let mut markers = BTreeMap::new();
    let mut rows = BTreeMap::new();
    for root in team_roots(home, teams) {
        match read(&root) {
            Ok(Some(records)) => {
                declared = true;
                for marker in review_rounds_for_story(&records, story) {
                    markers.entry(marker.id.clone()).or_insert(marker);
                }
                for row in round_rows_for_story(&records, story) {
                    let id = str::to_owned(Option::unwrap_or_default(Value::as_str(&row["id"])));
                    if !BTreeMap::contains_key(&rows, &id) {
                        BTreeMap::insert(&mut rows, id, row);
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                log::warn!(target: "session_usage", "review-rounds under {}: {e}", root.display());
                return Err("the story team's review-rounds markers could not be read");
            }
        }
    }
    if !declared {
        return Err(NO_COLLECTION);
    }
    let mut rows: Vec<Value> = Iterator::collect(BTreeMap::into_values(rows));
    // Oldest report first; a round with no report time last.
    <[Value]>::sort_by_key(&mut rows, |row| {
        let at = Value::as_i64(&row["reportedAt"]);
        (Option::is_none(&at), at, Value::as_str(&row["id"]).map(str::to_owned))
    });
    Ok((Iterator::collect(BTreeMap::into_values(markers)), rows))
}

/// Why the story's review tasks and its markers disagree, or `None` when they agree. Never a
/// gate: a closed review task with no marker is a round that was not recorded; a marker whose
/// task is known here and is not a review task is a mistyped launch.
pub(super) fn mismatch(
    markers: &[Marker],
    closed_review_tasks: &BTreeSet<String>,
    marker_task_types: &BTreeMap<String, String>,
) -> Option<String> {
    let recorded: BTreeSet<&str> = markers.iter().filter_map(|m| m.task.as_deref()).collect();
    let unrecorded = closed_review_tasks
        .iter()
        .filter(|t| !recorded.contains(t.as_str()))
        .count();
    let mistyped = markers
        .iter()
        .filter(|m| {
            m.task
                .as_deref()
                .and_then(|t| marker_task_types.get(t))
                .is_some_and(|kind| kind != "review")
        })
        .count();
    let mut parts = Vec::new();
    if unrecorded > 0 {
        parts.push(format!(
            "{unrecorded} closed review task(s) have no complete review-rounds marker"
        ));
    }
    if mistyped > 0 {
        parts.push(format!(
            "{mistyped} complete marker(s) name a task that is not a review task"
        ));
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

#[cfg(test)]
mod tests;
