// SPDX-License-Identifier: MIT
//! Read committed attribution from a bound local checkout's history, without fetching.
use super::authorship::Authorship;
use super::git_ai::GitAiSource;
use super::ranges::{
    AttributionError, AttributionSource, Basis, LineRange, HUMAN_TOOL, NO_LOCAL_COPY,
    HISTORY_PAST_SHALLOW, OBJECTS_NOT_LOCAL, PATH_NOT_IN_REVISION, REVISION_NOT_LOCAL,
};

use crate::context_repo::caller_scope::CallerScope;
use crate::domain::Principal;
use rmcp::schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

pub(crate) const NOTES_CHANGED: &str = "notes changed during read";

/// The tail a missing-note reason gets when the repo's notes are local-only: another
/// machine's notes never arrive here, so `unknown` is all this copy can say.
const LOCAL_ONLY: &str = "notes for this repo stay on the machine that wrote them (local-only)";

/// Whether the repo's notes stay on the machine that wrote them: no local profile set it to
/// push (the push list is one per Mac).
pub(crate) fn notes_stay_local(home: &Path, repo_key: &str) -> bool {
    !<[String]>::contains(
        &crate::attribution::prefs::push_repos(home),
        &repo_key.to_owned(),
    )
}

/// Whether `commit` has a git-ai note in this copy, in any of the refs readers use.
fn has_note(repo: &Path, commit: &str) -> Result<Option<std::process::Output>, String> {
    GitAiSource::default()
        .raw_note(repo, commit)
        .map_err(|error| error.to_string())
}

/// Why a commit has no note to read here, shared by `why_line` and `story_changes`: `reason`
/// as is, or with the local-only tail when the repo's notes stay where they were written.
pub(crate) fn missing_note_reason(local_only: bool, reason: &'static str) -> Cow<'static, str> {
    if local_only {
        format!("{reason}; {LOCAL_ONLY}").into()
    } else {
        reason.into()
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Params {
    /// Configured source slug; host/owner/repo disambiguates names shared by hosts.
    pub repo: String,
    /// Local commit or ref, honored as given. Omitted: the story's merged SHA, else its branch
    /// head, else origin's default branch; never the checkout's HEAD. Never fetched.
    #[serde(default)]
    pub rev: Option<String>,
    /// Story id (e.g. DEMO-19) whose history an omitted rev follows.
    #[serde(default)]
    pub story: Option<String>,
    /// Committed file path relative to the repository; no leading dash or NUL.
    pub path: String,
    /// First line, 1-based and inclusive.
    #[schemars(range(min = 1))]
    pub start: u32,
    /// Last line, inclusive; end >= start and end - start <= 2000.
    #[schemars(range(min = 1))]
    pub end: u32,
}

/// Transport-neutral result, shared by the MCP and page entry points (T5/T7).
pub(crate) type WhyLine = Value;
pub(crate) type ReadHold = Option<std::sync::Arc<tokio::sync::OwnedSemaphorePermit>>;

pub(crate) async fn resolve(
    home: &Path,
    principal: &Principal,
    params: &Params,
    scope: &CallerScope,
    launch_stories: std::collections::HashMap<String, String>,
) -> Result<WhyLine, String> {
    resolve_holding(home, principal, params, scope, launch_stories, None).await
}

pub(crate) async fn resolve_holding(
    home: &Path,
    principal: &Principal,
    params: &Params,
    scope: &CallerScope,
    launch_stories: HashMap<String, String>,
    hold: ReadHold,
) -> Result<WhyLine, String> {
    let store = crate::session_usage::store_path()?;
    let dir =
        crate::session_conversation::conversations_dir().ok_or("no CodeSense home directory")?;
    resolve_at_holding(
        home,
        principal,
        &store,
        &dir,
        params,
        scope,
        launch_stories,
        hold,
        None,
    )
    .await
}

pub(crate) async fn resolve_at(
    home: &Path,
    principal: &Principal,
    store: &Path,
    dir: &Path,
    params: &Params,
    scope: &CallerScope,
    launch_stories: HashMap<String, String>,
) -> Result<WhyLine, String> {
    resolve_at_holding(
        home,
        principal,
        store,
        dir,
        params,
        scope,
        launch_stories,
        None,
        None,
    )
    .await
}

#[cfg(test)]
pub(crate) async fn resolve_at_observed(
    home: &Path,
    principal: &Principal,
    store: &Path,
    dir: &Path,
    params: &Params,
    scope: &CallerScope,
    stories: HashMap<String, String>,
    observer: impl FnOnce(super::checkpoints::Stats) + Send + 'static,
) -> Result<WhyLine, String> {
    resolve_at_holding(
        home,
        principal,
        store,
        dir,
        params,
        scope,
        stories,
        None,
        Some(Box::new(observer)),
    )
    .await
}

async fn resolve_at_holding(
    home: &Path,
    principal: &Principal,
    store: &Path,
    dir: &Path,
    params: &Params,
    scope: &CallerScope,
    launch_stories: HashMap<String, String>,
    hold: ReadHold,
    observer: Option<Box<dyn FnOnce(super::checkpoints::Stats) + Send>>,
) -> Result<WhyLine, String> {
    let (read_home, read_principal, read_store, read_params, read_scope) = (
        home.to_owned(),
        principal.clone(),
        store.to_owned(),
        params.clone(),
        scope.clone(),
    );
    let blocking_hold = hold.clone();
    let _hold = hold;
    let (mut result, stats) = read_held(blocking_hold, move || {
        let mut result = local_result(resolve_with_read(
            &read_home,
            &read_principal,
            &read_params,
            &read_scope,
            &read_store,
            launch_stories,
            |repo, rev, path| GitAiSource::for_home(&read_home).ranges(repo, rev, path),
        ))?;
        let commits = result["ranges"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|range| super::turns::exact_agent_range(range))
            .filter_map(|range| range["commit"].as_str().map(str::to_owned))
            .collect::<BTreeSet<_>>();
        let stats = if commits.is_empty() {
            std::default::Default::default()
        } else if let Some(repo) = configured_repo(&read_home, &read_principal, &read_params.repo)?
            .filter(|r| {
                r.attachment == crate::mcp::setup_tools::AttachmentStatus::Attached
                    && r.local_path.is_some()
            })
        {
            let mut usage = crate::session_usage::UsageStore::open(&read_store)?;
            let stats = super::checkpoints::harvest(
                Path::new(repo.local_path.as_deref().unwrap()),
                &repo.remote_key,
                &mut usage,
                &commits,
            )?;
            if let Some(ranges) = result["ranges"].as_array_mut() {
                for range in ranges
                    .iter_mut()
                    .filter(|range| super::turns::exact_agent_range(range))
                {
                    if let (Some(cli), Some(id)) =
                        (range["tool"].as_str(), range["nativeSessionId"].as_str())
                    {
                        range["nativeSessionId"] = json!(
                            crate::session_usage::session_read::resolve_session(
                                &usage,
                                Some(cli),
                                id
                            )?
                            .native_session_id
                        );
                    }
                }
            }
            stats
        } else {
            std::default::Default::default()
        };
        Ok((result, stats))
    })
    .await?;
    let natives: BTreeSet<String> = result["ranges"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|range| range["basis"] == "exact" && range["tool"] != HUMAN_TOOL)
        .filter_map(|range| range["nativeSessionId"].as_str().map(str::to_owned))
        .collect();
    let mut native_to_session = HashMap::new();
    let mut sessions = serde_json::Map::new();
    for native in natives {
        let params = crate::session_usage::session_read::Params {
            session_id: native.clone(),
            include: Vec::new(),
            cursor: None,
            limit: None,
            turn: None,
            tool_use_id: None,
        };
        let facts = match crate::session_usage::session_read::read(store, dir, &params, scope).await
        {
            Ok(facts) => facts,
            Err(error) if error == crate::session_usage::session_read::UNKNOWN_SESSION => continue,
            Err(error) => return Err(error),
        };
        if let Some(id) = facts["session"]["sessionId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
        {
            native_to_session.insert(native, id.clone());
            sessions.entry(id).or_insert(facts);
        }
    }
    if let Some(ranges) = result.get_mut("ranges").and_then(Value::as_array_mut) {
        for range in ranges {
            if range["basis"] != "exact" || range["tool"] == HUMAN_TOOL {
                continue;
            }
            if let Some(id) = range["nativeSessionId"]
                .as_str()
                .and_then(|native| native_to_session.get(native))
            {
                range["sessionId"] = json!(id);
                range["link"] = json!("linked");
                range["story"] = sessions[id.as_str()]["story"]["id"].clone();
                range
                    .as_object_mut()
                    .expect("range object")
                    .remove("reason");
            }
        }
        result["sessions"] = Value::Object(sessions);
    }
    let (turn_home, turn_principal, turn_store, turn_name) = (
        home.to_owned(),
        principal.clone(),
        store.to_owned(),
        params.repo.clone(),
    );
    let result = read_held(_hold.clone(), move || {
        super::turns::join(
            &turn_home,
            &turn_principal,
            &turn_store,
            &turn_name,
            &mut result,
            crate::session_conversation::indexed_pointer,
        )?;
        Ok(result)
    })
    .await?;
    if let Some(observer) = observer {
        observer(stats);
    }
    Ok(result)
}

pub(crate) async fn read_held<T, F>(hold: ReadHold, read: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    crate::commands::await_blocking("why_line", move || {
        let _hold = hold;
        read()
    })
    .await
}

fn local_result(result: Result<WhyLine, String>) -> Result<WhyLine, String> {
    match result {
        Err(error) if error == OBJECTS_NOT_LOCAL => {
            Ok(json!({"status": "unavailable", "reason": error}))
        }
        result => result,
    }
}

/// Blame at a shallow boundary changes as local history deepens without changing HEAD, so the
/// cache is not even opened until the checkout has complete history.
fn open_cache(home: &Path, repo: &Path) -> Result<Option<super::why_line_cache::Cache>, String> {
    let complete_history =
        git_text(repo, &["rev-parse", "--is-shallow-repository"])?.as_deref() == Some("false");
    Ok(complete_history
        .then(|| {
            Result::ok(super::why_line_cache::Cache::open(&home.join("caches/why-line.sqlite3")))
        })
        .flatten())
}

/// The cached whole-file ranges, else the adapter's; `true` when the cache answered.
fn cached_raw(
    cache: Option<&super::why_line_cache::Cache>,
    key: &super::why_line_cache::Key<'_>,
    read: impl FnOnce() -> Result<Vec<LineRange>, AttributionError>,
) -> Result<(Vec<LineRange>, bool), String> {
    match cache.and_then(|cache| Option::flatten(Result::ok(cache.get(key)))) {
        Some(ranges) => Ok((ranges, true)),
        None => read().map(|ranges| (ranges, false)).map_err(|e| e.to_string()),
    }
}

fn save_raw(
    cache: Option<&mut super::why_line_cache::Cache>,
    key: &super::why_line_cache::Key<'_>,
    hit: bool,
    raw: &[LineRange],
) {
    if let Some(cache) = cache {
        let _ = if hit { cache.touch(key) } else { cache.put(key, raw) };
    }
}

/// Reads of one file before giving up on moving notes.
const READ_ATTEMPTS: usize = 3;

/// Raw attribution for many (commit, file) reads of one repository, with no session, harvest,
/// turn or story join. The checkout, cache and notes-revisions snapshot are resolved once in
/// `start_reads`, so a warm read is one cache lookup (plus its LRU touch) and a cold read is the one
/// blame that `why_line` itself would pay. If the notes revisions move during a cold read, the
/// snapshot is refreshed and only that file is read again (a compare of the revisions, never a
/// timing retry), at most [`READ_ATTEMPTS`] reads, then `NOTES_CHANGED`.
pub(crate) struct RawReader {
    home: PathBuf,
    remote_key: String,
    checkout: PathBuf,
    notes_ref: Option<String>,
    cache: Option<super::why_line_cache::Cache>,
    /// The note on each commit counted so far, read once however many files it touched; cleared
    /// whenever the notes revisions move.
    own_notes: HashMap<String, Option<Authorship>>,
    #[cfg(test)]
    snapshots: usize,
}

impl RawReader {
    pub(crate) fn start_reads(home: &Path, principal: &Principal, repo: &str) -> Result<Self, String> {
        let local = configured_repo(home, principal, repo)?
            .filter(|r| r.attachment == crate::mcp::setup_tools::AttachmentStatus::Attached)
            .ok_or(NO_LOCAL_COPY)?;
        let checkout = PathBuf::from(local.local_path.ok_or(NO_LOCAL_COPY)?);
        Ok(Self {
            home: home.to_owned(),
            remote_key: local.remote_key,
            cache: open_cache(home, &checkout)?,
            notes_ref: notes_revisions(&checkout)?,
            checkout,
            own_notes: HashMap::new(),
            #[cfg(test)]
            snapshots: 1,
        })
    }

    #[cfg(test)]
    pub(crate) fn raw_ranges_of(&mut self, commit: &str, path: &str) -> Result<Vec<LineRange>, String> {
        let home = self.home.clone();
        self.ranges_with(commit, path, |repo, rev, path| {
            GitAiSource::for_home(&home).ranges(repo, rev, path)
        })
    }

    /// The ranges `raw_ranges_of` returns for the lines `commit` itself added, reading only
    /// `commit`'s own note (once per commit). Every other range is unknown, so only the ranges
    /// whose `commit` is `commit` may be used. Cached apart from `raw_ranges_of`'s whole-file ranges.
    pub(crate) fn own_ranges_of(&mut self, commit: &str, path: &str) -> Result<Vec<LineRange>, String> {
        let source = GitAiSource::for_home(&self.home);
        self.own_ranges_with(&source, commit, path)
    }

    fn own_ranges_with(&mut self, source: &GitAiSource, commit: &str, path: &str) -> Result<Vec<LineRange>, String> {
        self.ranges_keyed(OWN_KEY, commit, path, |repo, rev, path, notes| {
            if !notes.contains_key(rev) {
                notes.insert(rev.to_owned(), source.note(repo, rev)?);
            }
            source.own_ranges(repo, rev, path, &notes[rev])
        })
    }

    fn ranges_with(
        &mut self,
        commit: &str,
        path: &str,
        mut read: impl FnMut(&Path, &str, &str) -> Result<Vec<LineRange>, AttributionError>,
    ) -> Result<Vec<LineRange>, String> {
        self.ranges_keyed("", commit, path, |repo, rev, path, _| read(repo, rev, path))
    }

    fn ranges_keyed(
        &mut self,
        key_suffix: &str,
        commit: &str,
        path: &str,
        mut read: impl FnMut(
            &Path,
            &str,
            &str,
            &mut HashMap<String, Option<Authorship>>,
        ) -> Result<Vec<LineRange>, AttributionError>,
    ) -> Result<Vec<LineRange>, String> {
        let remote_key = format!("{}{key_suffix}", self.remote_key);
        for _ in 0..READ_ATTEMPTS {
            let notes = self.notes_ref.clone();
            let key = raw_key(&remote_key, commit, path, notes.as_deref());
            let (raw, hit) = cached_raw(self.cache.as_ref(), &key, || {
                read(&self.checkout, commit, path, &mut self.own_notes)
            })?;
            // A cold read is only valid if the notes it saw are still the snapshot.
            let now = if hit { notes.clone() } else { notes_revisions(&self.checkout)? };
            #[cfg(test)]
            if !hit {
                self.snapshots += 1;
            }
            if now == notes {
                save_raw(self.cache.as_mut(), &key, hit, &raw);
                return Ok(raw);
            }
            self.notes_ref = now;
            self.own_notes.clear();
        }
        Err(NOTES_CHANGED.into())
    }
}

/// Appended to a repository's cache key for the ranges of a commit's own lines.
const OWN_KEY: &str = "\u{0}own";

fn raw_key<'a>(
    repo: &'a str,
    commit: &'a str,
    path: &'a str,
    notes: Option<&'a str>,
) -> super::why_line_cache::Key<'a> {
    super::why_line_cache::Key { repo, commit, path, notes }
}

#[cfg(test)]
pub(crate) fn resolve_core(
    home: &Path,
    principal: &Principal,
    params: &Params,
    scope: &CallerScope,
) -> Result<WhyLine, String> {
    local_result(resolve_with_read(
        home,
        principal,
        params,
        scope,
        &home.join("usage.sqlite"),
        Default::default(),
        |repo, rev, path| GitAiSource::default().ranges(repo, rev, path),
    ))
}

fn resolve_with_read(
    home: &Path,
    principal: &Principal,
    params: &Params,
    scope: &CallerScope,
    store: &Path,
    launch_stories: HashMap<String, String>,
    mut read: impl FnMut(&Path, &str, &str) -> Result<Vec<LineRange>, AttributionError>,
) -> Result<WhyLine, String> {
    if params.start == 0 || params.start > params.end || params.end - params.start > 2000 {
        return Err(
            "invalid: span must be 1-based, inclusive, ordered, and end - start <= 2000".into(),
        );
    }
    for (name, value) in [
        ("rev", params.rev.as_deref()),
        ("story", params.story.as_deref()),
        ("path", Some(std::string::String::as_str(&params.path))),
    ] {
        let Some(value) = value else { continue };
        if str::is_empty(value) || value.starts_with('-') || value.contains('\0') {
            return Err(format!(
                "invalid: {name} must be nonempty, without a leading dash or NUL"
            ));
        }
    }
    let Some(local) = configured_repo(home, principal, &params.repo)? else {
        return Ok(json!({"status": "unavailable", "reason": NO_LOCAL_COPY}));
    };
    if local.attachment != crate::mcp::setup_tools::AttachmentStatus::Attached {
        return Ok(json!({"status": "unavailable", "reason": NO_LOCAL_COPY}));
    }
    let Some(local_path) = local.local_path.as_deref() else {
        return Ok(json!({"status": "unavailable", "reason": NO_LOCAL_COPY}));
    };
    let repo = Path::new(local_path);
    let Some(history) = super::history::HistorySource::at(repo) else {
        return Ok(json!({"status": "unavailable", "reason": NO_LOCAL_COPY}));
    };
    let unavailable =
        |reason: &str| json!({"status": "unavailable", "reason": reason, "historyAsOf": history.history_as_of(home)});
    let (rev, rev_source) = match params.rev.as_deref() {
        Some(rev) => (rev.to_owned(), "explicit"),
        None => match super::history::HistorySource::default_rev(
            &history,
            home,
            Option::as_deref(&params.story),
        ) {
            Ok(default) => (
                default.oid().to_string(),
                super::history::DefaultRev::source(&default),
            ),
            Err(reason) => return Ok(unavailable(reason)),
        },
    };
    let Some(commit) = git_text(
        repo,
        &["rev-parse", "--verify", &format!("{rev}^{{commit}}")],
    )?
    else {
        return Ok(unavailable(REVISION_NOT_LOCAL));
    };
    let boundary = history.boundary();
    let Some(file_object) = git_text(
        repo,
        &[
            "rev-parse",
            "--verify",
            &format!("{commit}:{}", params.path),
        ],
    )?
    else {
        return Ok(json!({"status": "unavailable", "reason": PATH_NOT_IN_REVISION}));
    };
    // rev-parse can name a promised blob without reading it. Verify it is actually local.
    if !GitAiSource::default()
        .git(repo, &["cat-file", "-e", &file_object])
        .map_err(|error| error.to_string())?
        .status
        .success()
    {
        return Err(AttributionError::ObjectsNotLocal.to_string());
    }
    let mut stories = crate::session_usage::StoryResolver::for_home(home, launch_stories)
        .with_scope(scope.clone());
    let mut candidate_cache = HashMap::new();
    let mut disk_cache = open_cache(home, repo)?;
    let local_only = notes_stay_local(home, &local.remote_key);
    let missing = |reason| missing_note_reason(local_only, reason);
    // Read at most once per call, and only when a noteless commit needs it.
    let mut reflogs: Option<super::unsupported::Reflogs> = None;
    let result = read_notes_snapshot(repo, |notes_ref| {
        // Per snapshot: a retry after the notes moved asks again.
        let mut noted: HashMap<String, bool> = HashMap::new();
        let mut unsupported: HashMap<String, Option<String>> = HashMap::new();
        let key = raw_key(&local.remote_key, &commit, &params.path, notes_ref);
        let (raw, hit) = cached_raw(disk_cache.as_ref(), &key, || read(repo, &commit, &params.path))?;
        let mut ranges: Vec<Value> = raw
            .iter()
            .cloned()
            .filter_map(|mut range| {
                if range.end < params.start || range.start > params.end {
                    return None;
                }
                range.start = range.start.max(params.start);
                range.end = range.end.min(params.end);
                // Only a commit with no note here can be one whose note stayed elsewhere.
                let commit_noted = Option::is_some_and(Option::as_deref(&range.commit), |commit| {
                    *std::collections::hash_map::Entry::or_insert_with(
                        HashMap::entry(&mut noted, commit.to_owned()),
                        || Result::map_or(has_note(repo, commit), true, |note| Option::is_some(&note)),
                    )
                });
                let past_boundary = range.commit.as_ref().is_some_and(|c| boundary.contains(c));
                // A noteless commit an unsupported CLI's session made is that CLI's, never
                // associated or anyone's by default.
                let cli = Option::filter(Option::as_deref(&range.commit), |_| !commit_noted && !past_boundary)
                    .and_then(|commit| {
                        HashMap::entry(&mut unsupported, commit.to_owned())
                            .or_insert_with(|| {
                                let reflogs = Option::get_or_insert_with(&mut reflogs, || {
                                    super::unsupported::Reflogs::read(repo)
                                });
                                super::unsupported::Reflogs::committed_by(reflogs, repo, commit)
                            })
                            .clone()
                    });
                let mut value = output_range(range, notes_ref.is_some(), commit_noted, &missing);
                if past_boundary {
                    // Blame folds every older line into a boundary commit: nothing here is known.
                    for field in ["tool", "nativeSessionId", "model", "traceId"] {
                        value[field] = Value::Null;
                    }
                    value["basis"] = json!("unknown");
                    value["reason"] = json!(HISTORY_PAST_SHALLOW);
                } else if let Some(cli) = cli {
                    value["tool"] = json!(cli);
                    value["reason"] = json!(super::unsupported::REASON);
                }
                Some(value)
            })
            .collect();
        if notes_ref.is_some() {
            for range in &mut ranges {
                if range["basis"] == "unknown"
                    && range["reason"] != HISTORY_PAST_SHALLOW
                    && range["reason"] != super::unsupported::REASON
                {
                    associate(store, &mut stories, &mut candidate_cache, repo, range)?;
                }
            }
        }
        Ok((raw, hit, ranges))
    });
    match result {
        Ok(((raw, hit, ranges), notes_ref)) => {
            let key = raw_key(&local.remote_key, &commit, &params.path, notes_ref.as_deref());
            save_raw(disk_cache.as_mut(), &key, hit, &raw);
            Ok(
                json!({"repo": params.repo, "rev": rev, "revSource": rev_source, "resolvedCommit": commit,
                "path": params.path, "notesRef": notes_ref, "ranges": ranges, "sessions": {},
                "historyAsOf": history.history_as_of(home)}),
            )
        }
        Err(error) if error == NOTES_CHANGED => {
            Ok(json!({"status": "unavailable", "reason": error}))
        }
        Err(error) => Err(error),
    }
}

/// The attached checkout path of the configured repo `name`, if it is attached here.
pub(crate) fn attached_checkout(
    home: &Path,
    principal: &Principal,
    name: &str,
) -> Result<Option<PathBuf>, String> {
    Ok(configured_repo(home, principal, name)?
        .filter(|r| r.attachment == crate::mcp::setup_tools::AttachmentStatus::Attached)
        .and_then(|r| r.local_path)
        .map(PathBuf::from))
}

pub(super) fn configured_repo(
    home: &Path,
    principal: &Principal,
    name: &str,
) -> Result<Option<crate::mcp::setup_tools::SetupRepo>, String> {
    // describe_orgs shares describe_repo + group_attachments_by_remote with list_source_repos.
    // Keep the configured provider/host identity instead of inventing a GitHub slug.
    let mut matching = std::collections::BTreeMap::new();
    for repo in crate::mcp::setup_tools::describe_orgs(home, principal)?
        .into_iter()
        .flat_map(|org| org.teams)
        .flat_map(|team| team.source_repos)
    {
        if repo.slug.eq_ignore_ascii_case(name) || repo.remote_key.eq_ignore_ascii_case(name) {
            matching.insert(repo.remote_key.clone(), repo);
        }
    }
    if matching.len() > 1 {
        return Err("invalid: repo is ambiguous; use its host/owner/repo remote key".into());
    }
    Ok(matching.into_values().next())
}

/// The revisions of every notes ref read ([`NOTES_REFS`](super::git_ai::NOTES_REFS)), as one
/// snapshot id (`sense=<sha> ai=<sha>`, missing refs left out); `None` when there are none.
fn notes_revisions(repo: &Path) -> Result<Option<String>, String> {
    let mut revisions = Vec::new();
    for name in super::git_ai::NOTES_REFS {
        if let Some(sha) = git_text(
            repo,
            &["rev-parse", "--verify", &format!("refs/notes/{name}")],
        )? {
            revisions.push(format!("{name}={sha}"));
        }
    }
    Ok((!std::vec::Vec::is_empty(&revisions)).then(|| revisions.join(" ")))
}

/// One notes-refs snapshot and retry boundary for a line read or a batch of commit notes.
/// The callback must return its candidate facts, rather than publish partial retry results.
pub(crate) fn read_notes_snapshot<T>(
    repo: &Path,
    mut read: impl FnMut(Option<&str>) -> Result<T, String>,
) -> Result<(T, Option<String>), String> {
    for _ in 0..3 {
        let notes_ref = notes_revisions(repo)?;
        let value = read(notes_ref.as_deref())?;
        if notes_ref == notes_revisions(repo)? {
            return Ok((value, notes_ref));
        }
    }
    Err(NOTES_CHANGED.into())
}

fn output_range(
    mut range: LineRange,
    has_notes: bool,
    commit_noted: bool,
    missing: impl Fn(&'static str) -> Cow<'static, str>,
) -> Value {
    range.native_session_id = range.native_session_id.filter(|id| !id.trim().is_empty());
    let reason = if !has_notes {
        missing("no git-ai notes in local copy")
    } else if range.tool.as_deref() == Some(HUMAN_TOOL) {
        "human".into()
    } else if range.basis == Basis::Unknown && commit_noted {
        "no attribution in note".into()
    } else if range.basis == Basis::Unknown {
        missing("no git-ai note on introducing commit")
    } else if range.native_session_id.is_none() {
        "no native session id in note".into()
    } else {
        "no session record (hand-started or older session)".into()
    };
    let mut value = serde_json::to_value(range).expect("LineRange is serializable");
    let fields = value
        .as_object_mut()
        .expect("LineRange serializes as an object");
    if !has_notes {
        fields.insert("basis".into(), json!("unknown"));
    }
    // The live join consumes the internal trace before returning a response.
    fields.extend([
        ("sessionId".into(), Value::Null),
        ("link".into(), json!("unlinked")),
        ("reason".into(), json!(reason)),
        ("story".into(), Value::Null),
        ("turn".into(), Value::Null),
    ]);
    value
}

/// Use only this repository's objects and refs, never ambient Git overrides or the network.
fn git_text(repo: &Path, args: &[&str]) -> Result<Option<String>, String> {
    let output = GitAiSource::default()
        .git(repo, args)
        .map_err(|error| error.to_string())?;
    Ok(output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|s| !s.is_empty()))
}
fn associated_story(subject: &str) -> Option<String> {
    use crate::install::worktree_index::story_id_from_branch;
    if let Some(rest) = subject.strip_prefix("Merge pull request #") {
        let (number, branch) = rest.split_once(" from ")?;
        if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let (_, branch) = branch.split_once('/')?;
        return story_id_from_branch(branch);
    }
    let (id, _) = subject.split_once(':')?;
    if !id.starts_with("SV-") {
        return None;
    }
    story_id_from_branch(id).filter(|parsed| parsed == id)
}

fn associate(
    store: &Path,
    stories: &mut crate::session_usage::StoryResolver,
    candidate_cache: &mut HashMap<String, Vec<String>>,
    repo: &Path,
    value: &mut Value,
) -> Result<(), String> {
    let Some(commit) = value["commit"].as_str() else {
        return Ok(());
    };
    // An existing but unreadable note (or an uncovered line) is never a story association.
    if Option::is_some(&has_note(repo, commit)?) {
        return Ok(());
    }
    let Some(subject) = git_text(repo, &["show", "-s", "--format=%s", commit])? else {
        return Ok(());
    };
    let Some(story) = associated_story(&subject) else {
        return Ok(());
    };
    use crate::session_usage::{Filter, UsageStore};
    let candidates = match candidate_cache.entry(story.clone()) {
        std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
        std::collections::hash_map::Entry::Vacant(entry) => {
            let filter = Filter {
                story: Some(story.clone()),
                limit: Some(20),
                ..Filter::default()
            };
            let ids = UsageStore::open(store)?
                .report_with(&filter, stories)?
                .sessions
                .into_iter()
                .map(|session| session.session_id)
                .collect();
            entry.insert(ids)
        }
    };
    value["basis"] = json!("associated");
    value["story"] = json!(story);
    value["candidateSessions"] = json!(candidates);
    value["reason"] = json!("story association only; no note on introducing commit");
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::attribution::git_ai::tests::{git, rebuild, CLAUDE, CODEX};

    fn params() -> Params {
        Params {
            repo: "acme/api".into(),
            rev: Some("HEAD".into()),
            story: None,
            path: "a.txt".into(),
            start: 2,
            end: 2,
        }
    }

    pub(crate) fn attach(home: &Path, repo: &Path) {
        org(home);
        git(
            repo,
            &["remote", "add", "origin", "https://github.com/acme/api.git"],
        );
        bind(home, repo);
    }

    /// The `acme` org with team `dev` configuring source repo `acme/api`; returns its root.
    pub(crate) fn org(home: &Path) -> PathBuf {
        let org = crate::context_repo::path_to_org_root(home, &Principal::from(1), "acme").unwrap();
        std::fs::create_dir_all(&org).unwrap();
        std::fs::write(
            org.join("org.yml"),
            "slug: acme\ndisplay_name: Acme\ncreated_at: \"2026-01-01T00:00:00Z\"\n",
        )
        .unwrap();
        std::fs::write(org.join("people.yml"), "people: []\n").unwrap();
        std::fs::write(
            org.join("teams.yml"),
            "teams:\n  - name: dev\n    source_repos:\n      - owner: acme\n        repo: api\n",
        )
        .unwrap();
        git2::Repository::init(&org).unwrap();
        org
    }

    /// Record `repo`'s attachment under its current origin (the org must already exist).
    pub(crate) fn bind(home: &Path, repo: &Path) {
        crate::commands::attachments::upsert_attachment_record(
            home,
            1,
            crate::commands::attachments::existing_checkout_attachment(repo).unwrap(),
            None,
            false,
        )
        .unwrap();
    }

    pub(crate) fn answer_default(home: &Path) -> serde_json::Value {
        answer(home, &params())
    }

    fn answer(home: &Path, params: &Params) -> serde_json::Value {
        serde_json::to_value(
            resolve_core(home, &Principal::from(1), params, &CallerScope::Human).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn configured_provider_identity_selects_its_bound_checkout() {
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        let org = crate::context_repo::path_to_org_root(home.path(), &Principal::from(1), "acme")
            .unwrap();
        std::fs::write(org.join("teams.yml"), "teams:\n  - name: dev\n    source_repos:\n      - owner: acme\n        repo: api\n        provider: gitlab\n").unwrap();
        git(
            repo.path(),
            &[
                "remote",
                "set-url",
                "origin",
                "https://gitlab.com/acme/api.git",
            ],
        );
        crate::commands::attachments::upsert_attachment_record(
            home.path(),
            1,
            crate::commands::attachments::existing_checkout_attachment(repo.path()).unwrap(),
            None,
            false,
        )
        .unwrap();
        let result = answer(home.path(), &params());
        assert_eq!(result["ranges"][0]["basis"], "exact");
        let p = Params {
            repo: "gitlab.com/acme/api".into(),
            ..params()
        };
        assert_eq!(answer(home.path(), &p)["ranges"], result["ranges"]);
    }

    #[test]
    fn attached_fixture_resolves_sha_clips_span_and_survives_reopen() {
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        let result = answer(home.path(), &params());
        assert_eq!(
            result["resolvedCommit"],
            git(repo.path(), &["rev-parse", "HEAD"]).trim()
        );
        assert_eq!(
            result["notesRef"],
            format!(
                "ai={}",
                git(repo.path(), &["rev-parse", "refs/notes/ai"]).trim()
            )
        );
        assert_eq!(result["ranges"].as_array().unwrap().len(), 1);
        assert_eq!(result["ranges"][0]["start"], 2);
        assert_eq!(result["ranges"][0]["end"], 2);
        assert_eq!(result["ranges"][0]["basis"], "exact");
        assert_eq!(result["ranges"][0]["sessionId"], serde_json::Value::Null);
        assert!(result.get("unavailable").is_none());
        assert_eq!(result["sessions"], serde_json::json!({}));
        // The internal raw result retains the trace until the shared live join.
        assert!(result["ranges"][0]["traceId"].as_str().is_some());
        assert_eq!(result, answer(home.path(), &params()));
    }

    #[test]
    fn unattached_missing_and_unknown_revision_explain_unavailability() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(answer(home.path(), &params())["reason"], "no local copy");
        let repo = rebuild();
        attach(home.path(), repo.path());
        let mut p = params();
        p.rev = Some("not-a-revision".into());
        assert_eq!(
            answer(home.path(), &p)["reason"],
            "revision not in local copy"
        );
        repo.close().unwrap();
        assert_eq!(answer(home.path(), &params())["reason"], "no local copy");
    }

    #[test]
    fn no_notes_ref_reports_unknown_with_reason() {
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        git(repo.path(), &["update-ref", "-d", "refs/notes/ai"]);
        let result = answer(home.path(), &params());
        assert!(result["notesRef"].is_null());
        assert_eq!(result["ranges"][0]["basis"], "unknown");
        assert_eq!(
            result["ranges"][0]["reason"],
            *missing_note_reason(true, "no git-ai notes in local copy")
        );
    }

    #[test]
    fn validates_options_nul_and_inclusive_span_limits() {
        let home = tempfile::tempdir().unwrap();
        for (rev, path, start, end) in [
            ("--all", "a.txt", 1, 2),
            ("HEAD\0x", "a.txt", 1, 2),
            ("HEAD", "--help", 1, 2),
            ("HEAD", "a\0.txt", 1, 2),
            ("HEAD", "a.txt", 0, 2),
            ("HEAD", "a.txt", 3, 2),
            ("HEAD", "a.txt", 1, 2002),
        ] {
            let p = Params {
                rev: Some(rev.into()),
                path: path.into(),
                start,
                end,
                ..params()
            };
            assert!(
                resolve_core(home.path(), &Principal::from(1), &p, &CallerScope::Human).is_err()
            );
        }
        let p = Params {
            start: 1,
            end: 2001,
            ..params()
        };
        assert!(resolve_core(home.path(), &Principal::from(1), &p, &CallerScope::Human).is_ok());
    }
    #[test]
    fn absent_path_is_unavailable_instead_of_a_git_error() {
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        let p = Params {
            path: "absent.txt".into(),
            ..params()
        };
        let result = answer(home.path(), &p);
        assert_eq!(result["status"], "unavailable");
        assert_eq!(result["reason"], "path not in revision");
    }

    #[test]
    fn ambiguous_names_require_an_explicit_configured_remote_key() {
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        let org = crate::context_repo::path_to_org_root(home.path(), &Principal::from(1), "acme")
            .unwrap();
        std::fs::write(org.join("teams.yml"), "teams:\n  - name: dev\n    source_repos:\n      - owner: acme\n        repo: api\n      - owner: acme\n        repo: api\n        provider: gitlab\n").unwrap();
        let error = resolve_core(
            home.path(),
            &Principal::from(1),
            &params(),
            &CallerScope::Human,
        )
        .unwrap_err();
        assert!(error.contains("ambiguous"));
        let p = Params {
            repo: "github.com/acme/api".into(),
            ..params()
        };
        assert_eq!(answer(home.path(), &p)["ranges"][0]["basis"], "exact");
    }

    #[test]
    fn no_note_subjects_associate_only_recognized_stories_and_cap_candidates() {
        use crate::session_usage::{event::Tokens, StoryLink, UsageStore};
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        let mut store = UsageStore::open(&home.path().join("usage.sqlite")).unwrap();
        for story in ["SV-PBF-200", "SV-CDE-30"] {
            for n in 0..25 {
                let id = format!("{story}-candidate-{n:02}");
                store
                    .ingest(&[crate::session_usage::tests::event(
                        &id,
                        &id,
                        "gpt-6.1-sol",
                        1793131200000,
                        Tokens::default(),
                    )])
                    .unwrap();
                store
                    .link_story(
                        &id,
                        &StoryLink {
                            story_id: story.into(),
                            source: home.path().join("story.md"),
                            org: "acme".into(),
                        },
                    )
                    .unwrap();
            }
        }
        drop(store);
        for (subject, expected) in [
            ("SV-PBF-200: type the inbox projection so the unattended-writer guard resolves apply_batch (#101)", Some("SV-PBF-200")),
            ("Merge pull request #102 from alderco/feat/SV-CDE-30-task-records", Some("SV-CDE-30")),
            ("fix(attribution): keep fresh relaunch flags before prompt operands", None), ("Revert \"SV-CDE-30: task records (#102)\"", None),
            ("fixup! SV-CDE-30: task records (#102)", None),
            ("Revert \"feat(SW-AN-49): ship the Claude notify hook; stop requiring the team to provide it\"", None),
            ("Revert \"feat(SW-DEMO-1): durable mailbox and inbox verbs (#103)\" (#104)", None),
        ] {
            std::fs::write(repo.path().join("fresh.txt"), format!("{subject}\n")).unwrap();
            git(repo.path(), &["add", "fresh.txt"]);
            git(repo.path(), &["commit", "-qm", subject]);
            let p = Params { path: "fresh.txt".into(), start: 1, end: 1, ..params() };
            let result = answer(home.path(), &p);
            let range = &result["ranges"][0];
            if let Some(story) = expected {
                assert_eq!(range["basis"], "associated", "{subject}");
                assert_eq!(range["story"], story);
                let candidates = range["candidateSessions"].as_array().unwrap();
                assert_eq!(candidates.len(), 20);
                assert!(candidates.iter().all(|id| id.as_str().unwrap().starts_with(&format!("{story}-candidate-"))));
                assert!(range["sessionId"].is_null());
                assert_eq!(result, answer(home.path(), &p));
            } else {
                assert_eq!(range["basis"], "unknown", "{subject}");
            }
        }
    }

    #[test]
    fn existing_notes_and_absent_notes_ref_never_use_associated_fallback() {
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        std::fs::write(repo.path().join("fresh.txt"), "one\n").unwrap();
        git(repo.path(), &["add", "fresh.txt"]);
        git(repo.path(), &["commit", "-qm", "SV-CDE-30: fresh (#102)"]);
        let p = Params {
            path: "fresh.txt".into(),
            start: 1,
            end: 1,
            ..params()
        };
        for note in [
            "malformed note",
            "---\n{\"schema_version\":\"authorship/3.0.0\",\"sessions\":{}}",
        ] {
            git(repo.path(), &["notes", "--ref=ai", "add", "-f", "-m", note]);
            assert_eq!(answer(home.path(), &p)["ranges"][0]["basis"], "unknown");
        }
        git(repo.path(), &["update-ref", "-d", "refs/notes/ai"]);
        let result = answer(home.path(), &p);
        assert_eq!(result["ranges"][0]["basis"], "unknown");
        assert_eq!(
            result["ranges"][0]["reason"],
            *missing_note_reason(true, "no git-ai notes in local copy")
        );
    }

    #[test]
    fn subject_patterns_match_real_repository_history() {
        // Captured repository subjects, plus explicit negative story-prefix wrappers.
        for (subject, story) in [
            ("SV-PBF-200: type the inbox projection so the unattended-writer guard resolves apply_batch (#101)", Some("SV-PBF-200")),
            ("Merge pull request #102 from alderco/feat/SV-CDE-30-task-records", Some("SV-CDE-30")),
            ("Merge origin/main into SV-CDE-30 task records", None),
            ("Revert \"SV-CDE-30: task records (#102)\"", None),
            ("fixup! SV-CDE-30: task records (#102)", None),
            ("Revert \"feat(SW-AN-49): ship the Claude notify hook; stop requiring the team to provide it\"", None),
            ("Revert \"feat(SW-DEMO-1): durable mailbox and inbox verbs (#103)\" (#104)", None),
        ] {
            assert_eq!(associated_story(subject).as_deref(), story, "{subject}");
        }
    }
    pub(crate) fn blobless(home: &Path) -> (tempfile::TempDir, std::path::PathBuf) {
        let source = rebuild();
        git(source.path(), &["config", "uploadpack.allowFilter", "true"]);
        let clone = home.join("partial");
        let url = format!("file://{}", source.path().display());
        git(
            home,
            &[
                "clone",
                "--filter=blob:none",
                "--no-checkout",
                &url,
                clone.to_str().unwrap(),
            ],
        );
        // Bind this real partial clone to the configured identity, keeping its promisor remote local.
        git(&clone, &["remote", "remove", "origin"]);
        attach(home, &clone);
        git(&clone, &["remote", "set-url", "origin", &url]);
        git(&clone, &["config", "remote.origin.promisor", "true"]);
        git(
            &clone,
            &["config", "remote.origin.partialclonefilter", "blob:none"],
        );
        (source, clone)
    }

    pub(crate) fn object_count(repo: &Path) -> u64 {
        git(repo, &["count-objects", "-v"])
            .lines()
            .filter_map(|line| {
                let (key, value) = line.split_once(": ")?;
                matches!(key, "count" | "in-pack").then(|| value.parse::<u64>().unwrap())
            })
            .sum()
    }

    #[test]
    fn blobless_clone_does_not_fetch_missing_file_objects() {
        let home = tempfile::tempdir().unwrap();
        let (_source, repo) = blobless(home.path());
        let before = object_count(&repo);
        let result = answer(home.path(), &params());
        assert_eq!(
            object_count(&repo),
            before,
            "attribution must not fetch promised blobs"
        );
        assert_eq!(result["reason"], "objects not in local copy");
    }

    #[test]
    fn blobless_clone_does_not_fetch_or_associate_missing_note_objects() {
        let home = tempfile::tempdir().unwrap();
        let (_source, repo) = blobless(home.path());
        git(
            &repo,
            &[
                "fetch",
                "--filter=blob:none",
                "origin",
                "refs/notes/ai:refs/notes/ai",
            ],
        );
        // Materialize only what blame needs during fixture setup, leaving promised notes missing.
        git(&repo, &["blame", "HEAD", "--", "a.txt"]);
        let before = object_count(&repo);
        let result = answer(home.path(), &params());
        assert_eq!(
            object_count(&repo),
            before,
            "attribution must not fetch promised note blobs"
        );
        assert_eq!(result["reason"], "objects not in local copy");
    }

    fn replace_note(repo: &Path, native: &str) {
        let commit = git(repo, &["rev-parse", "HEAD~1"]);
        let note = format!("a.txt\n  s_1::t_1 1-3\n---\n{{\"schema_version\":\"authorship/3.0.0\",\"sessions\":{{\"s_1\":{{\"agent_id\":{{\"tool\":\"claude\",\"id\":\"{native}\",\"model\":\"m\"}}}}}}}}");
        git(
            repo,
            &["notes", "--ref=ai", "add", "-f", "-m", &note, commit.trim()],
        );
    }

    #[test]
    fn notes_changed_during_read_are_retried_as_one_consistent_snapshot() {
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        let mut reads = 0;
        let result = resolve_with_read(
            home.path(),
            &Principal::from(1),
            &params(),
            &CallerScope::Human,
            &home.path().join("usage.sqlite"),
            Default::default(),
            |repo, rev, path| {
                let ranges = GitAiSource::default().ranges(repo, rev, path)?;
                reads += 1;
                if reads == 1 {
                    replace_note(repo, "after-change");
                }
                Ok(ranges)
            },
        )
        .unwrap();
        assert_eq!(reads, 2);
        assert_eq!(
            result["notesRef"],
            format!(
                "ai={}",
                git(repo.path(), &["rev-parse", "refs/notes/ai"]).trim()
            )
        );
        assert_eq!(result["ranges"][0]["nativeSessionId"], "after-change");
    }

    #[test]
    fn continuously_changing_notes_stop_after_three_attempts() {
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        let mut reads = 0;
        let result = resolve_with_read(
            home.path(),
            &Principal::from(1),
            &params(),
            &CallerScope::Human,
            &home.path().join("usage.sqlite"),
            Default::default(),
            |repo, rev, path| {
                let ranges = GitAiSource::default().ranges(repo, rev, path)?;
                reads += 1;
                replace_note(repo, &format!("change-{reads}"));
                Ok(ranges)
            },
        )
        .unwrap();
        assert_eq!(reads, 3);
        assert_eq!(result["reason"], "notes changed during read");
        let rows: i64 = rusqlite::Connection::open(cache_path(home.path()))
            .unwrap()
            .query_row("SELECT COUNT(*) FROM file_ranges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            rows, 0,
            "unavailable snapshot attempts never populate the cache"
        );
    }

    #[test]
    fn historical_sha_and_branch_read_their_own_ranges_instead_of_head() {
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        let older = git(repo.path(), &["rev-parse", "HEAD~1"]).trim().to_owned();
        git(repo.path(), &["branch", "historical", &older]);
        for rev in [&older, "historical"] {
            let p = Params {
                rev: Some(rev.into()),
                path: "d.txt".into(),
                start: 1,
                end: 3,
                ..params()
            };
            let result = answer(home.path(), &p);
            assert_eq!(result["resolvedCommit"], older);
            assert_eq!(result["ranges"].as_array().unwrap().len(), 1);
            assert_eq!(result["ranges"][0]["end"], 2);
            assert_eq!(result["ranges"][0]["basis"], "unknown");
        }
        let p = Params {
            path: "d.txt".into(),
            start: 1,
            end: 3,
            ..params()
        };
        let result = answer(home.path(), &p);
        assert_ne!(result["resolvedCommit"], older);
        assert_eq!(result["ranges"].as_array().unwrap().len(), 2);
        assert_eq!(result["ranges"][1]["basis"], "exact");
    }

    #[test]
    fn repo_ignore_revs_file_does_not_rewrite_introducing_commits() {
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        std::fs::write(
            repo.path().join("a.txt"),
            "replacement\nclaude two\nclaude three\n",
        )
        .unwrap();
        git(repo.path(), &["add", "a.txt"]);
        git(repo.path(), &["commit", "-qm", "Replace first line"]);
        let introducing = git(repo.path(), &["rev-parse", "HEAD"]).trim().to_owned();
        let ignore = repo.path().join(".git/ignore-revisions");
        std::fs::write(&ignore, format!("{introducing}\n")).unwrap();
        git(
            repo.path(),
            &["config", "blame.ignoreRevsFile", ignore.to_str().unwrap()],
        );
        let p = Params {
            start: 1,
            end: 1,
            ..params()
        };
        assert_eq!(answer(home.path(), &p)["ranges"][0]["commit"], introducing);
    }

    #[test]
    fn shared_git_runner_is_noninteractive_local_and_ignores_user_config() {
        let repo = rebuild();
        let output = GitAiSource::default().git(repo.path(), &[
            "-c", "alias.attribution-env=!printf '%s|%s|%s|%s|%s' \"$GIT_NO_LAZY_FETCH\" \"$GIT_TERMINAL_PROMPT\" \"$GCM_INTERACTIVE\" \"$GIT_CONFIG_NOSYSTEM\" \"$GIT_CONFIG_GLOBAL\"",
            "attribution-env",
        ]).unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "1|0|never|1|/dev/null"
        );
    }
    async fn joined(home: &Path, store: &Path, p: &Params) -> Value {
        resolve_at(
            home,
            &Principal::from(1),
            store,
            &home.join("conversations"),
            p,
            &CallerScope::Human,
            Default::default(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn exact_sessions_join_once_in_both_note_record_orders_and_after_reopen() {
        use crate::session_usage::{record::SessionRecord, UsageStore};
        for records_first in [true, false] {
            let home = tempfile::tempdir().unwrap();
            let repo = rebuild();
            attach(home.path(), repo.path());
            let c1 = git(repo.path(), &["rev-parse", "HEAD~1"]);
            let note = git(repo.path(), &["notes", "--ref=ai", "show", c1.trim()]);
            if records_first {
                git(repo.path(), &["notes", "--ref=ai", "remove", c1.trim()]);
            }
            let store_path = home.path().join("injected-usage.sqlite");
            let store = crate::session_usage::UsageStore::open(&store_path).unwrap();
            if !records_first {
                let missing = joined(home.path(), &store_path, &params()).await;
                assert_eq!(
                    missing["ranges"][0]["reason"],
                    "no session record (hand-started or older session)"
                );
            }
            for (native, id, cli) in [
                (CLAUDE, "tab-claude", "claude"),
                (CODEX, "tab-codex", "codex"),
            ] {
                let mut record = SessionRecord::new(id);
                record.native_session_id = Some(native.into());
                record.cli = Some(cli.into());
                record.story_id = Some("SV-CDE-13".into());
                record.org = Some("acme".into());
                store.write_session_record(&record).unwrap();
            }
            if records_first {
                git(
                    repo.path(),
                    &["notes", "--ref=ai", "add", "-m", &note, c1.trim()],
                );
            }
            drop(store);
            let _reopened = UsageStore::open(&store_path).unwrap();
            for (path, native, id, ranges) in [
                ("a.txt", CLAUDE, "tab-claude", 2),
                ("b.txt", CODEX, "tab-codex", 1),
            ] {
                let p = Params {
                    path: path.into(),
                    start: 1,
                    end: 3,
                    ..params()
                };
                let result = joined(home.path(), &store_path, &p).await;
                assert_eq!(result["ranges"].as_array().unwrap().len(), ranges);
                for range in result["ranges"].as_array().unwrap() {
                    assert_eq!(range["sessionId"], id);
                    assert_eq!(range["link"], "linked");
                    assert_eq!(range["story"], "SV-CDE-13");
                    assert!(range.get("reason").is_none());
                    assert!(range["turn"].is_null());
                }
                assert_eq!(result["sessions"].as_object().unwrap().len(), 1);
                let read = crate::session_usage::session_read::read(
                    &store_path,
                    &home.path().join("conversations"),
                    &serde_json::from_value(json!({"sessionId":native})).unwrap(),
                    &CallerScope::Human,
                )
                .await
                .unwrap();
                assert_eq!(result["sessions"][id], read);
                assert!(result["sessions"][id].get("conversation").is_none());
                assert_eq!(result, joined(home.path(), &store_path, &p).await);
            }
        }
    }

    #[tokio::test]
    async fn human_and_missing_or_blank_native_sessions_remain_unlinked() {
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        let store = home.path().join("injected.sqlite");
        let mut usage_only = crate::session_usage::UsageStore::open(&store).unwrap();
        usage_only
            .ingest(&[crate::session_usage::tests::event(
                "unrecorded",
                CLAUDE,
                "gpt-6-sol",
                1_790_000_000_000,
                Default::default(),
            )])
            .unwrap();
        drop(usage_only);
        let result = joined(home.path(), &store, &params()).await;
        assert_eq!(
            result["ranges"][0]["reason"],
            "no session record (hand-started or older session)"
        );
        let p = Params {
            path: "d.txt".into(),
            start: 3,
            end: 3,
            ..params()
        };
        let human = joined(home.path(), &store, &p).await;
        assert_eq!(human["ranges"][0]["reason"], "human");
        assert_eq!(human["ranges"][0]["link"], "unlinked");
        replace_note(repo.path(), "");
        let missing = joined(home.path(), &store, &params()).await;
        assert!(missing["ranges"][0]["nativeSessionId"].is_null());
        assert_eq!(
            missing["ranges"][0]["reason"],
            "no native session id in note"
        );
        assert!(missing.get("unavailable").is_none());
    }

    #[tokio::test]
    async fn associated_candidates_use_injected_store_and_live_launch_story_hints() {
        use crate::session_usage::{event::Tokens, UsageStore};
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        std::fs::write(repo.path().join("fresh.txt"), "one\ntwo\n").unwrap();
        git(repo.path(), &["add", "fresh.txt"]);
        git(repo.path(), &["commit", "-qm", "SV-CDE-13: first"]);
        std::fs::write(repo.path().join("fresh.txt"), "one\nchanged two\n").unwrap();
        git(repo.path(), &["add", "fresh.txt"]);
        git(repo.path(), &["commit", "-qm", "SV-CDE-13: second"]);
        let path = home.path().join("injected.sqlite");
        let mut store = UsageStore::open(&path).unwrap();
        store
            .ingest(&[crate::session_usage::tests::event(
                "event",
                "native-candidate",
                "gpt-6-sol",
                1_790_000_000_000,
                Tokens {
                    input: 1,
                    ..Default::default()
                },
            )])
            .unwrap();
        store
            .attribute(
                "native-candidate",
                "acme",
                Some("dev"),
                "builder",
                "live-handle",
                None,
            )
            .unwrap();
        drop(store);
        let p = Params {
            path: "fresh.txt".into(),
            start: 1,
            end: 2,
            ..params()
        };
        let result = resolve_at(
            home.path(),
            &Principal::from(1),
            &path,
            &home.path().join("conversations"),
            &p,
            &CallerScope::Human,
            std::collections::HashMap::from([("live-handle".into(), "SV-CDE-13".into())]),
        )
        .await
        .unwrap();
        assert_eq!(result["ranges"].as_array().unwrap().len(), 2);
        for range in result["ranges"].as_array().unwrap() {
            assert_eq!(range["basis"], "associated");
            assert_eq!(range["candidateSessions"], json!(["native-candidate"]));
            assert!(range["sessionId"].is_null());
        }
    }
    #[tokio::test]
    async fn managed_reads_only_await_with_injected_home_and_offline_environment() {
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        let log = home.path().join("await.log");
        crate::attribution::testing::script(&home.path().join("bin/git-ai"), &format!(
            "printf '%s|%s|%s|%s|%s|%s|%s\\n' \"$*\" \"$HOME\" \"$GIT_NO_LAZY_FETCH\" \"$GIT_TERMINAL_PROMPT\" \"$GCM_INTERACTIVE\" \"$GIT_CONFIG_NOSYSTEM\" \"$GIT_CONFIG_GLOBAL\" >> '{}'", log.display()));
        joined(home.path(), &home.path().join("usage.sqlite"), &params()).await;
        let calls = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            calls.trim(),
            format!(
                "await --timeout 5|{}|1|0|never|1|/dev/null",
                home.path().join("git-ai/home").display()
            )
        );
    }
    #[tokio::test]
    async fn shared_resolve_at_harvests_new_checkpoint_rows_after_a_warm_raw_cache() {
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        let store_path = home.path().join("usage.sqlite");
        let dir = home.path().join("conversations");
        let p = params();
        let mut record = crate::session_usage::record::SessionRecord::new("harvest-session");
        record.cli = Some("claude".into());
        record.native_session_id = Some(super::super::git_ai::tests::CLAUDE.into());
        crate::session_usage::UsageStore::open(&store_path)
            .unwrap()
            .write_session_record(&record)
            .unwrap();
        let first = resolve_at(
            home.path(),
            &Principal::from(1),
            &store_path,
            &dir,
            &p,
            &CallerScope::Human,
            Default::default(),
        )
        .await
        .unwrap();
        assert!(cache_path(home.path()).exists());
        let parent = git(repo.path(), &["rev-parse", "HEAD^"]);
        let log = repo
            .path()
            .join(".git/ai/working_logs")
            .join(format!("old-{}", parent.trim()))
            .join("checkpoints.jsonl");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::write(
            log,
            include_str!("testdata/git_ai_1_7_5_checkpoints/live-claude.jsonl"),
        )
        .unwrap();
        let next = resolve_at(
            home.path(),
            &Principal::from(1),
            &store_path,
            &dir,
            &p,
            &CallerScope::Human,
            Default::default(),
        )
        .await
        .unwrap();
        assert_eq!(first["notesRef"], next["notesRef"]);
        let store = crate::session_usage::UsageStore::open(&store_path).unwrap();
        let link = store
            .checkpoint_link(
                "claude",
                "55555555-2222-4333-8444-555555555555",
                "t_03845ac247f947",
            )
            .unwrap();
        assert_eq!(link.unwrap().tool_use_id, "toolu_Q4JOIN");
    }

    fn tracked(home: &Path, p: &Params, reads: &mut usize) -> Value {
        local_result(resolve_with_read(
            home,
            &Principal::from(1),
            p,
            &CallerScope::Human,
            &home.join("usage.sqlite"),
            Default::default(),
            |repo, rev, path| {
                *reads += 1;
                GitAiSource::default().ranges(repo, rev, path)
            },
        ))
        .unwrap()
    }
    fn cache_path(home: &Path) -> std::path::PathBuf {
        home.join("caches/why-line.sqlite3")
    }

    #[test]
    fn shallow_reads_never_open_cache_and_follow_deepened_history() {
        let source = rebuild();
        let parent = tempfile::tempdir().unwrap();
        let repo = parent.path().join("checkout");
        let url = format!("file://{}", source.path().display());
        git(
            parent.path(),
            &["clone", "--depth=1", &url, repo.to_str().unwrap()],
        );
        git(&repo, &["remote", "rename", "origin", "history"]);
        git(&repo, &["fetch", "history", "refs/notes/ai:refs/notes/ai"]);
        let home = tempfile::tempdir().unwrap();
        attach(home.path(), &repo);
        let mut reads = 0;
        let mut previous = None;
        for deepen in [None, Some("--deepen=1"), Some("--unshallow")] {
            if let Some(option) = deepen {
                git(&repo, &["fetch", option, "history", "main"]);
            }
            let shallow = git(&repo, &["rev-parse", "--is-shallow-repository"]).trim() == "true";
            let first = tracked(home.path(), &params(), &mut reads);
            let before_repeat = reads;
            assert_eq!(first, tracked(home.path(), &params(), &mut reads));
            if shallow {
                assert_eq!(reads, before_repeat + 1, "shallow repeat must read live");
                assert!(
                    !cache_path(home.path()).exists(),
                    "shallow reads must not open/write cache"
                );
            }
            let fresh_home = tempfile::tempdir().unwrap();
            // Bind the same checkout without changing its configured remote again.
            let attachment = crate::commands::attachments::existing_checkout_attachment(&repo).unwrap();
            let org =
                crate::context_repo::path_to_org_root(fresh_home.path(), &Principal::from(1), "acme")
                    .unwrap();
            std::fs::create_dir_all(&org).unwrap();
            for name in ["org.yml", "people.yml", "teams.yml"] {
                let old =
                    crate::context_repo::path_to_org_root(home.path(), &Principal::from(1), "acme")
                        .unwrap();
                std::fs::copy(old.join(name), org.join(name)).unwrap();
            }
            git2::Repository::init(&org).unwrap();
            crate::commands::attachments::upsert_attachment_record(
                fresh_home.path(),
                1,
                attachment,
                None,
                false,
            )
            .unwrap();
            assert_eq!(first, tracked(fresh_home.path(), &params(), &mut 0));
            if let Some(old) = previous.take() {
                if deepen == Some("--deepen=1") {
                    assert_ne!(
                        first["ranges"], old,
                        "deeper blame must expose earlier introducing commits"
                    );
                }
            }
            previous = Some(first["ranges"].clone());
        }
        assert!(
            cache_path(home.path()).exists(),
            "complete history may be cached"
        );
    }

    #[test]
    fn replacement_commits_are_ignored_by_live_adapter_and_resolver() {
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        let source = GitAiSource::default();
        let expected = source.ranges(repo.path(), "HEAD", "a.txt").unwrap();
        let mut reads = 0;
        let expected_answer = tracked(home.path(), &params(), &mut reads);
        std::fs::remove_file(cache_path(home.path())).unwrap();
        let head = git(repo.path(), &["rev-parse", "HEAD"]);
        git(repo.path(), &["replace", head.trim(), "HEAD~2"]);
        let actual = source.ranges(repo.path(), "HEAD", "a.txt");
        let answer = tracked(home.path(), &params(), &mut reads);
        git(repo.path(), &["replace", "-d", head.trim()]);
        assert_eq!(
            actual.unwrap(),
            expected,
            "live blame must ignore replacement objects"
        );
        assert_eq!(answer, expected_answer);
        assert_eq!(reads, 2, "resolver also read live after removing cache");
    }

    #[test]
    fn cache_hit_reuses_whole_file_ranges_before_filter_and_reopens() {
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        let mut reads = 0;
        let first = tracked(
            home.path(),
            &Params {
                start: 1,
                end: 1,
                ..params()
            },
            &mut reads,
        );
        assert_eq!(reads, 1);
        let other_span = tracked(
            home.path(),
            &Params {
                start: 3,
                end: 3,
                ..params()
            },
            &mut reads,
        );
        assert_eq!(
            reads, 1,
            "a cache hit must not call the real ranges adapter"
        );
        assert_eq!(other_span["ranges"][0]["start"], 3);
        let reopened = super::super::why_line_cache::Cache::open(&cache_path(home.path())).unwrap();
        let key = super::super::why_line_cache::Key {
            repo: "github.com/acme/api",
            commit: first["resolvedCommit"].as_str().unwrap(),
            path: "a.txt",
            notes: first["notesRef"].as_str(),
        };
        let raw = reopened.get(&key).unwrap().unwrap();
        assert_eq!(raw[0].start, 1);
        assert_eq!(raw.last().unwrap().end, 3);
    }
    #[test]
    fn cache_moved_branch_and_new_notes_miss_in_both_note_population_orders() {
        for notes_first in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let repo = rebuild();
            attach(home.path(), repo.path());
            if notes_first {
                replace_note(repo.path(), "before-cache");
            }
            let mut reads = 0;
            let first = tracked(home.path(), &params(), &mut reads);
            assert_eq!(first, tracked(home.path(), &params(), &mut reads));
            assert_eq!(reads, 1);
            replace_note(repo.path(), "after-cache");
            let changed = tracked(home.path(), &params(), &mut reads);
            assert_eq!(reads, 2);
            assert_ne!(first["notesRef"], changed["notesRef"]);
            assert_eq!(changed["ranges"][0]["nativeSessionId"], "after-cache");
            assert_eq!(changed, tracked(home.path(), &params(), &mut reads));
            assert_eq!(reads, 2);
            git(repo.path(), &["branch", "moving", "HEAD~1"]);
            let p = Params {
                rev: Some("moving".into()),
                path: "d.txt".into(),
                start: 1,
                end: 3,
                ..params()
            };
            let old = tracked(home.path(), &p, &mut reads);
            git(repo.path(), &["branch", "-f", "moving", "HEAD"]);
            let new = tracked(home.path(), &p, &mut reads);
            assert_eq!(reads, 4);
            assert_ne!(old["resolvedCommit"], new["resolvedCommit"]);
            assert_eq!(new["ranges"][1]["basis"], "exact");
        }
    }
    #[test]
    fn cache_corruption_and_deletion_rebuild_identical_answers_and_errors_fall_back() {
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        let mut reads = 0;
        let before = tracked(home.path(), &params(), &mut reads);
        let path = cache_path(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not SQLite").unwrap();
        assert_eq!(before, tracked(home.path(), &params(), &mut reads));
        assert!(crate::shared::sqlite::append_suffix(&path, ".corrupt-0").exists());
        std::fs::remove_file(&path).unwrap();
        assert_eq!(before, tracked(home.path(), &params(), &mut reads));
        assert_eq!(reads, 3);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert_eq!(before, tracked(home.path(), &params(), &mut reads));
        assert_eq!(reads, 4);
    }
    #[test]
    fn cache_abandoned_notes_attempt_never_fills_its_key() {
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        let abandoned = format!(
            "ai={}",
            git(repo.path(), &["rev-parse", "refs/notes/ai"]).trim()
        );
        let mut reads = 0;
        let result = local_result(resolve_with_read(
            home.path(),
            &Principal::from(1),
            &params(),
            &CallerScope::Human,
            &home.path().join("usage.sqlite"),
            Default::default(),
            |repo, rev, path| {
                let ranges = GitAiSource::default().ranges(repo, rev, path)?;
                reads += 1;
                if reads == 1 {
                    replace_note(repo, "after-retry");
                }
                Ok(ranges)
            },
        ))
        .unwrap();
        assert_eq!(reads, 2);
        let cache = super::super::why_line_cache::Cache::open(&cache_path(home.path())).unwrap();
        let key = super::super::why_line_cache::Key {
            repo: "github.com/acme/api",
            commit: result["resolvedCommit"].as_str().unwrap(),
            path: "a.txt",
            notes: Some(&abandoned),
        };
        assert!(cache.get(&key).unwrap().is_none());
        let stable = super::super::why_line_cache::Key {
            notes: result["notesRef"].as_str(),
            ..key
        };
        assert_eq!(
            cache.get(&stable).unwrap().unwrap()[0]
                .native_session_id
                .as_deref(),
            Some("after-retry")
        );
    }
    #[test]
    fn cache_format_mismatch_discards_old_rows_and_rebuilds_from_real_git() {
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        let mut reads = 0;
        let before = tracked(home.path(), &params(), &mut reads);
        let path = cache_path(home.path());
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        drop(conn);
        let cache = super::super::why_line_cache::Cache::open(&path).unwrap();
        let key = super::super::why_line_cache::Key {
            repo: "github.com/acme/api",
            commit: before["resolvedCommit"].as_str().unwrap(),
            path: "a.txt",
            notes: before["notesRef"].as_str(),
        };
        assert!(
            cache.get(&key).unwrap().is_none(),
            "a different format must not reuse old rows"
        );
        drop(cache);
        assert_eq!(tracked(home.path(), &params(), &mut reads), before);
        assert_eq!(reads, 2);
        let version: i64 = rusqlite::Connection::open(&path)
            .unwrap()
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_ne!(version, 1);
    }

    const KEY: &str = "github.com/acme/api";

    /// The notes `git` calls a wrapper around it saw, one per line.
    fn counting_git(dir: &Path) -> (GitAiSource, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let (script, log) = (dir.join("counting-git"), dir.join("git-calls.log"));
        std::fs::write(&script, format!("#!/bin/sh\necho \"$*\" >> \"{}\"\nexec git \"$@\"\n", log.display())).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        (GitAiSource::new(script.to_string_lossy()), log)
    }

    fn notes_calls(log: &Path) -> Vec<String> {
        std::fs::read_to_string(log).unwrap_or_default().lines().filter_map(|l| l.split_once(" notes ").map(|(_, call)| format!("notes {call}"))).collect()
    }

    /// The notes calls counting `file` of `sha` makes on a fresh reader, and on the same reader a
    /// second file of that commit.
    fn counted_notes_calls(home: &Path, sha: &str, files: [&str; 2]) -> (Vec<String>, Vec<String>) {
        let tools = tempfile::tempdir().unwrap();
        let (source, log) = counting_git(tools.path());
        let mut reader = RawReader::start_reads(home, &Principal::from(1), KEY).unwrap();
        reader.own_ranges_with(&source, sha, files[0]).unwrap();
        let first = notes_calls(&log);
        reader.own_ranges_with(&source, sha, files[1]).unwrap();
        (first, notes_calls(&log))
    }

    /// The count path reads the commit's own note and no other, one `git notes show` per notes ref
    /// it tries, in `NOTES_REFS` order until one has the note: a constant (at most two), whatever
    /// the note's ref and however many commits the file's blame names or files the commit touched.
    #[test]
    fn the_count_path_reads_one_note_per_ref_tried_however_many_the_blame_names() {
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        let [managed, ai] = crate::attribution::git_ai::NOTES_REFS;
        let show = |sha: &str, notes_ref: &str| format!("notes --ref={notes_ref} show {sha}");
        let sha_of = |rev: &str| git(repo.path(), &["rev-parse", rev]).trim().to_owned();
        let (head, c1, c0) = (sha_of("HEAD"), sha_of("HEAD~1"), sha_of("HEAD~2"));
        let blamed = |sha: &str, file: &str| {
            let blame = git(repo.path(), &["blame", "-s", sha, "--", file]);
            blame.lines().filter_map(|l| l.split_whitespace().next()).collect::<std::collections::HashSet<_>>().len()
        };
        assert!(blamed(&head, "d.txt") > blamed(&c0, "d.txt"), "the fixture's blame must widen with history");

        // The note is on the `ai` ref only (the fixture): the managed ref is tried first and misses.
        let (first, both) = counted_notes_calls(home.path(), &head, ["d.txt", "a.txt"]);
        assert_eq!(first, vec![show(&head, managed), show(&head, ai)], "ai-only note: one call per ref");
        assert_eq!(both, first, "a second file of the same commit reads no note");

        // The same count on a commit whose blame names fewer commits: it does not grow with the blame.
        let (narrow, _) = counted_notes_calls(home.path(), &c1, ["a.txt", "a.txt"]);
        assert_eq!(narrow, vec![show(&c1, managed), show(&c1, ai)]);

        // No note on either ref: both are tried, once each.
        let (none, none_both) = counted_notes_calls(home.path(), &c0, ["base.txt", "d.txt"]);
        assert_eq!(none, vec![show(&c0, managed), show(&c0, ai)], "no note: one call per ref");
        assert_eq!(none_both, none);

        // A note on the managed ref wins with the first lookup.
        let blob = git(repo.path(), &["notes", &format!("--ref={ai}"), "list", &head]);
        git(repo.path(), &["notes", &format!("--ref={managed}"), "add", "-C", blob.trim(), &head]);
        let (won, won_both) = counted_notes_calls(home.path(), &head, ["d.txt", "a.txt"]);
        assert_eq!(won, vec![show(&head, managed)], "managed-ref note: one call");
        assert_eq!(won_both, won);
    }

    /// Counts are identical by construction: the ranges of the commit's own lines are the same
    /// from either path, for every file of every commit of the fixture history.
    #[test]
    fn own_ranges_equal_the_whole_file_ranges_of_the_commit() {
        let repo = rebuild();
        assert_own_ranges_parity(repo.path());
    }

    /// For every commit and file of `repo`: the ranges of the commit's own lines from the
    /// whole-file read, and from the own-note read.
    pub(crate) fn assert_own_ranges_parity(repo: &Path) {
        let source = GitAiSource::default();
        let commits = git(repo, &["rev-list", "--all"]);
        for sha in commits.lines() {
            let files = git(repo, &["ls-tree", "-r", "--name-only", sha]);
            for path in files.lines() {
                let Ok(whole) = source.ranges(repo, sha, path) else { continue };
                let own = source.own_ranges(repo, sha, path, &source.note(repo, sha).unwrap()).unwrap();
                let of = |ranges: Vec<LineRange>| ranges.into_iter().filter(|r| r.commit.as_deref() == Some(sha)).collect::<Vec<_>>();
                assert_eq!(of(own), of(whole), "{sha} {path}");
            }
        }
    }

    #[test]
    fn a_reader_serves_warm_reads_without_blame_or_any_notes_read() {
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        let sha = git(repo.path(), &["rev-parse", "HEAD"]).trim().to_owned();
        let mut reads = 0;
        let mut reader = RawReader::start_reads(home.path(), &Principal::from(1), KEY).unwrap();
        let mut pass = |reader: &mut RawReader| {
            ["a.txt", "b.txt"]
                .map(|path| {
                    reader
                        .ranges_with(&sha, path, |repo, rev, path| {
                            reads += 1;
                            GitAiSource::default().ranges(repo, rev, path)
                        })
                        .unwrap()
                })
                .to_vec()
        };
        let cold = pass(&mut reader);
        let after_cold = reader.snapshots;
        assert_eq!(after_cold, 3, "one snapshot at open, one verification per cold read");
        let warm = pass(&mut reader);
        assert_eq!(warm, cold, "warm answers equal cold answers");
        assert!(cold[0].iter().any(|r| r.tool.as_deref() == Some("claude")));
        assert_eq!(reader.snapshots, after_cold, "a warm read takes no notes snapshot");
        drop(reader);
        assert_eq!(reads, 2, "cold: one blame per file; warm: none");
        assert!(
            !home.path().join("usage.sqlite").exists(),
            "raw ranges never open the session store"
        );
    }

    /// The retry is a snapshot compare, never a timing retry: a file is read again only when the
    /// notes revisions moved during its read, at most three reads in all. Either notes ref counts.
    #[test]
    fn a_moved_notes_snapshot_rereads_only_that_file_up_to_three_times() {
        // (notes ref that moves, changes during consecutive reads, reads made, answer)
        let rows = [
            ("ai", 0, 1, true),
            ("ai", 1, 2, true),
            ("ai", 2, 3, true),
            ("ai", 3, 3, false),
            ("sense", 1, 2, true),
            ("sense", 3, 3, false),
        ];
        for (moved, changes, expected_reads, ok) in rows {
            let home = tempfile::tempdir().unwrap();
            let repo = rebuild();
            attach(home.path(), repo.path());
            let sha = git(repo.path(), &["rev-parse", "HEAD"]).trim().to_owned();
            let mut reader = RawReader::start_reads(home.path(), &Principal::from(1), KEY).unwrap();
            let mut reads = 0;
            let answer = reader.ranges_with(&sha, "a.txt", |r, v, p| {
                let ranges = GitAiSource::default().ranges(r, v, p)?;
                reads += 1;
                if reads <= changes {
                    if moved == "ai" {
                        replace_note(r, &format!("moved-{reads}"));
                    } else {
                        let head = git(r, &["rev-parse", "HEAD~1"]);
                        git(r, &["notes", "--ref=sense", "add", "-f", "-m", &format!("moved-{reads}"), head.trim()]);
                    }
                }
                Ok(ranges)
            });
            assert_eq!(reads, expected_reads, "{moved} x{changes}");
            if ok {
                assert!(answer.is_ok(), "{moved} x{changes}: {answer:?}");
            } else {
                assert_eq!(answer.unwrap_err(), NOTES_CHANGED, "{moved} x{changes}");
            }
        }
    }

    #[test]
    fn a_reader_without_a_checkout_says_so() {
        let home = tempfile::tempdir().unwrap();
        let error = RawReader::start_reads(home.path(), &Principal::from(1), KEY).err().unwrap();
        assert_eq!(error, super::super::ranges::NO_LOCAL_COPY);
    }
}
