// SPDX-License-Identifier: MIT
//! Optional introduced-line attribution; durable contribution facts never depend on this read.
use super::*;
use crate::attribution::ranges::{NO_LOCAL_COPY, REPO_NOT_FOUND, REVISION_NOT_LOCAL};
use crate::attribution::why_line;
use crate::domain::Principal;
use std::future::Future;
use std::path::PathBuf;

type Candidates = BTreeMap<String, Vec<PathBuf>>;

pub(super) async fn page(
    home: &Path,
    params: &Params,
    scope: &CallerScope,
    facts: &git::GitFacts,
) -> Result<(Vec<git::Commit>, Option<String>), String> {
    page_with(home, params, facts, |principal, params| async move {
        why_line::resolve_at(
            home,
            &principal,
            &home.join("usage.sqlite"),
            &home.join("session-conversations"),
            &params,
            scope,
            HashMap::new(),
        )
        .await
    })
    .await
}

/// Page before optional enrichment; the production reader and call-count tests share this order.
async fn page_with<F, Fut>(
    home: &Path,
    params: &Params,
    facts: &git::GitFacts,
    resolve: F,
) -> Result<(Vec<git::Commit>, Option<String>), String>
where
    F: FnMut(Principal, why_line::Params) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    let (mut commits, cursor) = git::page(params, &facts.commits)?;
    if params.include.contains(&Include::Ranges) {
        enrich_with(home, &mut commits, &facts.repo_paths, resolve).await;
    }
    Ok((commits, cursor))
}

/// The caller boundary is injectable so tests count resolver invocations independently of caches.
async fn enrich_with<F, Fut>(
    home: &Path,
    commits: &mut [git::Commit],
    candidates: &Candidates,
    mut resolve: F,
) where
    F: FnMut(Principal, why_line::Params) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    let (read_home, read_candidates, read_commits) =
        (home.to_owned(), candidates.clone(), commits.to_vec());
    let prepared = crate::commands::await_blocking("story_changes.ranges", move || {
        Ok(prepare_page(&read_home, &read_commits, &read_candidates, false))
    })
    .await
    .unwrap_or_else(|error| vec![Err(error); commits.len()]);
    for (commit, prepared) in commits.iter_mut().zip(prepared) {
        commit.ranges = Some(Vec::new());
        commit.ranges_basis = Some("unknown".into());
        let bindings = match prepared {
            Ok(value) => value,
            Err(reason) => {
                commit.ranges_reason = Some(reason);
                continue;
            }
        };
        let windows = bindings[0].2.clone();
        let had_added = !std::collections::BTreeMap::is_empty(&windows);
        let mut evidence = Vec::new();
        let mut unavailable: BTreeSet<String> = BTreeSet::new();
        for (path, windows) in windows {
            // Bound calls by the added-line span, never by the full file's length.
            let mut start = windows[0].0;
            let end = windows[windows.len() - 1].1;
            while start <= end {
                let stop = start.saturating_add(1999).min(end);
                let mut resolved = Err("range attribution unavailable".to_owned());
                // A local copy can pass preparation yet lack older notes needed by blame.
                // Retry only this window, stopping at the first successful resolver result.
                for (principal, remote, _) in &bindings {
                    let params = why_line::Params {
                        repo: remote.clone(),
                        rev: Some(commit.sha.clone()),
                        story: None,
                        path: path.clone(),
                        start,
                        end: stop,
                    };
                    resolved = match resolve(principal.clone(), params).await {
                        Ok(value) if value["status"] == "unavailable" => Err(
                            value["reason"]
                                .as_str()
                                .unwrap_or("range attribution unavailable")
                                .to_owned(),
                        ),
                        result => result,
                    };
                    if std::result::Result::is_ok(&resolved) {
                        break;
                    }
                }
                match resolved {
                    Ok(value) => {
                        for range in value["ranges"].as_array().into_iter().flatten() {
                            if range["commit"] != commit.sha {
                                continue;
                            }
                            let (Some(from), Some(to)) =
                                (range["start"].as_u64(), range["end"].as_u64())
                            else {
                                continue;
                            };
                            for &(added_start, added_end) in &windows {
                                let from = from.max(u64::from(added_start)).max(u64::from(start));
                                let to = to.min(u64::from(added_end)).min(u64::from(stop));
                                if from <= to {
                                    let mut clipped = range.clone();
                                    clipped["path"] = json!(path);
                                    clipped["start"] = json!(from);
                                    clipped["end"] = json!(to);
                                    evidence.push(clipped);
                                }
                            }
                        }
                    }
                    Err(error) => {
                        unavailable.insert(error);
                    }
                }
                if stop == end {
                    break;
                }
                start = stop + 1;
            }
        }
        let basis = if std::vec::Vec::is_empty(&evidence)
            || !std::collections::BTreeSet::is_empty(&unavailable)
            || evidence.iter().any(|r| r["basis"] == "unknown")
        {
            "unknown"
        } else if std::iter::Iterator::all(&mut evidence.iter(), |r| r["basis"] == "exact") {
            "exact"
        } else {
            "associated"
        };
        commit.ranges_basis = Some(basis.into());
        commit.ranges_reason = if !std::collections::BTreeSet::is_empty(&unavailable) {
            Some(unavailable.into_iter().collect::<Vec<_>>().join("; "))
        } else if std::vec::Vec::is_empty(&evidence) {
            Some(
                if had_added {
                    "no ranges introduced by this commit"
                } else {
                    "no added lines"
                }
                .into(),
            )
        } else {
            None
        };
        commit.ranges = Some(evidence);
    }
}

pub(super) const MERGE_COMMIT: &str = "merge commit";

pub(super) type Binding = (Principal, String, AddedWindows);
pub(super) type Prepared = Result<Vec<Binding>, String>;
/// One checkout lookup per commit. Line-level evidence needs a note on the commit; line totals
/// (`for_totals`) do not (a noteless commit's lines are unattributed) and follow renames.
pub(super) fn prepare_page(
    home: &Path,
    commits: &[git::Commit],
    candidates: &Candidates,
    for_totals: bool,
) -> Vec<Prepared> {
    let mut preparer = Preparer::new(home);
    commits
        .iter()
        .map(|commit| preparer.one(home, commit, candidates, for_totals))
        .collect()
}

/// Prepares commits one at a time (the totals loop checks its budget between them): the
/// configured checkouts are listed once, each checkout's reflogs are read at most once.
pub(super) struct Preparer {
    configured: Vec<(Principal, String, PathBuf)>,
    reflogs: ReflogsByCheckout,
}

impl Preparer {
    pub(super) fn new(home: &Path) -> Self {
        let configured: Vec<_> = git::principals(home)
            .into_iter()
            .flat_map(|principal| {
                crate::mcp::setup_tools::describe_orgs(home, &principal)
                    .unwrap_or_default()
                    .into_iter()
                    .flat_map(|org| org.teams)
                    .flat_map(|team| team.source_repos)
                    .filter(|repo| {
                        repo.attachment == crate::mcp::setup_tools::AttachmentStatus::Attached
                    })
                    .filter_map(move |repo| {
                        repo.local_path
                            .map(|path| (principal.clone(), repo.remote_key, PathBuf::from(path)))
                    })
            })
            .collect();
        Self { configured, reflogs: std::collections::BTreeMap::new() }
    }

    pub(super) fn one(
        &mut self,
        home: &Path,
        commit: &git::Commit,
        candidates: &Candidates,
        for_totals: bool,
    ) -> Prepared {
        let mut reason = REPO_NOT_FOUND.to_owned();
        let mut bindings = Vec::new();
        let mut seen = BTreeSet::new();
        if let Some(paths) = candidates.get(&commit.sha) {
            for path in paths {
                if crate::commands::context_git_watch::GitDirs::resolve(path)
                    .is_some_and(|dirs| std::collections::BTreeSet::contains(&seen, &dirs.common_dir))
                {
                    continue;
                }
                match prepare(home, path, &commit.sha, &self.configured, &mut seen, &mut self.reflogs, for_totals) {
                    Ok(values) => std::iter::Extend::extend(&mut bindings, values),
                    Err(error) => reason = error,
                }
            }
        }
        if !std::vec::Vec::is_empty(&bindings) {
            return Ok(bindings);
        }
        Err(reason)
    }
}

pub(super) type AddedWindows = BTreeMap<String, Vec<(u32, u32)>>;
/// The reflogs read so far, by checkout.
type ReflogsByCheckout = BTreeMap<PathBuf, crate::attribution::unsupported::Reflogs>;
fn prepare(
    home: &Path,
    path: &Path,
    sha: &str,
    configured: &[(Principal, String, PathBuf)],
    seen: &mut BTreeSet<PathBuf>,
    reflogs: &mut ReflogsByCheckout,
    for_totals: bool,
) -> Prepared {
    let repo = git2::Repository::open(path).map_err(|_| REPO_NOT_FOUND)?;
    let id = git2::Oid::from_str(sha).map_err(|e| e.to_string())?;
    let commit =
        git2::Repository::find_commit(&repo, id).map_err(|_| REVISION_NOT_LOCAL)?;
    if crate::commands::git_sync::commit_recorded_parent_count(&commit) > 1 {
        return Err(MERGE_COMMIT.into());
    }
    let common =
        crate::commands::context_git_watch::GitDirs::resolve(path).map(|dirs| dirs.common_dir);
    let remote = std::result::Result::ok(git2::Repository::find_remote(&repo, "origin"))
        .and_then(|r| {
            std::result::Result::ok(r.url()).and_then(crate::context_repo::canonical_remote_key)
        });
    let mut reason = NO_LOCAL_COPY.to_owned();
    let mut bindings = Vec::new();
    // A bound checkout supplies its configured identity even with an offline/promisor remote.
    for same_repository in [true, false] {
        for (principal, key, local) in configured {
            let bound_common = crate::commands::context_git_watch::GitDirs::resolve(local)
                .map(|dirs| dirs.common_dir);
            let same = common.is_some() && common == bound_common;
            if same != same_repository
                || (!same
                    && !remote
                        .as_deref()
                        .is_some_and(|remote| key.eq_ignore_ascii_case(remote)))
            {
                continue;
            }
            let Some(bound_common) = bound_common else { continue };
            if !std::collections::BTreeSet::insert(seen, bound_common) {
                continue;
            }
            match prepare_bound(home, key, local, sha, id, reflogs, for_totals) {
                Ok(windows) => std::vec::Vec::push(&mut bindings, (principal.clone(), key.clone(), windows)),
                Err(error) => reason = error,
            }
        }
    }
    if std::vec::Vec::is_empty(&bindings) { Err(reason) } else { Ok(bindings) }
}

fn prepare_bound(
    home: &Path,
    key: &str,
    path: &Path,
    sha: &str,
    id: git2::Oid,
    reflogs: &mut ReflogsByCheckout,
    for_totals: bool,
) -> Result<AddedWindows, String> {
    let repo = git2::Repository::open(path).map_err(|_| NO_LOCAL_COPY)?;
    let commit =
        git2::Repository::find_commit(&repo, id).map_err(|_| REVISION_NOT_LOCAL)?;
    if !for_totals {
        match crate::attribution::git_ai::GitAiSource::for_home(home).note(path, sha) {
            Ok(Some(_)) => {}
            Ok(None) => {
                // An unsupported CLI's commit has no note by design: say whose it is.
                let read = std::collections::BTreeMap::entry(reflogs, PathBuf::from(path))
                    .or_insert_with(|| crate::attribution::unsupported::Reflogs::read(path));
                if let Some(cli) = crate::attribution::unsupported::Reflogs::committed_by(read, path, sha) {
                    return Err(crate::attribution::unsupported::reason(&cli));
                }
                let local_only = why_line::notes_stay_local(home, key);
                return Err(
                    why_line::missing_note_reason(local_only, "no git-ai note on commit").into_owned(),
                );
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    use crate::attribution::ranges::{HISTORY_PAST_SHALLOW, OBJECTS_NOT_LOCAL};
    crate::commands::git_sync::commit_added_windows_local(&repo, &commit, for_totals).map_err(|error| {
        if git2::Error::message(&error) == HISTORY_PAST_SHALLOW { HISTORY_PAST_SHALLOW } else { OBJECTS_NOT_LOCAL }.into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attribution::git_ai::tests::git;
    use std::cell::RefCell;
    pub(super) fn note(repo: &Path, sha: &str) {
        let text = "large.txt\n  h_fixture 1\n---\n{\"schema_version\":\"authorship/3.0.0\",\"git_ai_version\":\"1.7.5\",\"base_commit_sha\":\"0000000000000000000000000000000000000000\",\"prompts\":{},\"humans\":{\"h_fixture\":{\"author\":\"Fixture <fixture@example.com>\"}}}";
        assert!(crate::attribution::authorship::Authorship::parse(text).is_ok());
        git(repo, &["notes", "--ref=ai", "add", "-m", text, sha]);
    }

    #[tokio::test]
    async fn production_ranges_page_carries_shared_turn_pointers() {
        let home=tempfile::tempdir().unwrap();let repo=crate::attribution::git_ai::tests::rebuild();
        crate::attribution::why_line::tests::attach(home.path(),repo.path());
        crate::attribution::turns::tests::record(home.path(),repo.path(),"a.txt","story-turn");
        crate::attribution::turns::tests::checkpoint_log(repo.path(),"a.txt",&["toolu_01CCC"]);
        crate::attribution::turns::tests::capture(home.path(),&home.path().join("session-conversations"),"story-turn",None,&Default::default());
        let sha=git(repo.path(),&["rev-parse","HEAD^"]).trim().to_owned();
        let facts=git::GitFacts {commits:vec![git::Commit {sha:sha.clone(),subject:"fixture".into(),authored_at:1000, author: None,files:vec!["a.txt".into()],session_ids:vec!["story-turn".into()],basis:"exact",unavailable:None,ranges:None,ranges_basis:None,ranges_reason:None,unlanded:false,landed:false}],repo_paths:BTreeMap::from([(sha,vec![repo.path().to_owned()])]),..Default::default()};
        let params=serde_json::from_value(json!({"storyId":"SV-29","include":["ranges"]})).unwrap();
        let result=page(home.path(),&params,&CallerScope::Human,&facts).await.unwrap();
        let turn=&result.0[0].ranges.as_ref().unwrap()[0]["turn"];
        assert_eq!(*turn,json!({"sessionId":"story-turn","toolUseId":"toolu_01CCC","turn":2,"basis":"edit","taskId":"story-turn","taskBasis":"session"}));
        // Rows 12 and 14: the range carries exactly what `why_line` answers for the same line.
        assert_eq!(*turn,crate::attribution::turns::tests::query(home.path(),"a.txt").await["ranges"][0]["turn"]);
    }

    #[tokio::test]
    async fn cache_reuses_second_included_page_without_another_adapter_read() {
        use crate::attribution::testing::{logged_calls, logging_binary};
        let (home, _) = crate::session_usage::tests::store();
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q", "-b", "main"]);
        crate::attribution::why_line::tests::attach(home.path(), repo.path());
        std::fs::write(repo.path().join("large.txt"), "new\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-qm", "noted root"]);
        let sha = git(repo.path(), &["rev-parse", "HEAD"]).trim().to_owned();
        note(repo.path(), &sha);
        let facts = git::GitFacts {
            commits: vec![git::Commit {
                sha: sha.clone(), subject: "noted root".into(), authored_at: 1000, author: None,
                files: vec!["large.txt".into()], session_ids: Vec::new(), basis: "exact",
                unavailable: None, ranges: None, ranges_basis: None, ranges_reason: None, unlanded: false, landed: false,
            }],
            repo_paths: BTreeMap::from([(sha, vec![repo.path().to_owned()])]),
            ..Default::default()
        };
        let params: Params = serde_json::from_value(json!({"storyId":"SV-29","include":["ranges"]})).unwrap();
        let log = home.path().join("adapter-await.log");
        logging_binary(&home.path().join("bin/git-ai"), &log, "exit 0");
        let first = page(home.path(), &params, &CallerScope::Human, &facts).await.unwrap();
        assert_eq!(first.0[0].ranges_basis.as_deref(), Some("exact"));
        assert_eq!(logged_calls(&log).len(), 1, "cold page must read the real attribution adapter");
        let cache = home.path().join("caches/why-line.sqlite3");
        let rows: i64 = rusqlite::Connection::open(&cache).unwrap()
            .query_row("SELECT count(*) FROM file_ranges", [], |row| row.get(0)).unwrap();
        assert_eq!(rows, 1);
        let second = page(home.path(), &params, &CallerScope::Human, &facts).await.unwrap();
        assert_eq!(serde_json::to_value(&second).unwrap(), serde_json::to_value(&first).unwrap());
        assert_eq!(logged_calls(&log).len(), 1, "same-page cache hit must skip another real adapter read");
    }

    struct RetryFixture {
        home: tempfile::TempDir,
        _source: tempfile::TempDir,
        partial: PathBuf,
        complete: PathBuf,
        facts: git::GitFacts,
        params: Params,
    }

    fn retry_fixture() -> RetryFixture {
        let (home, _) = crate::session_usage::tests::store();
        let source = tempfile::tempdir().unwrap();
        git(source.path(), &["init", "-q", "-b", "main"]);
        for (text, subject) in [("old\n", "old"), ("old\nnew\n", "queried")] {
            std::fs::write(source.path().join("a.txt"), text).unwrap();
            git(source.path(), &["add", "."]);
            git(source.path(), &["commit", "-qm", subject]);
            let note = format!("a.txt\n  h_fixture 1-{}\n---\n{{\"schema_version\":\"authorship/3.0.0\",\"humans\":{{\"h_fixture\":{{\"author\":\"Fixture <fixture@example.com>\"}}}}}}", text.lines().count());
            assert!(crate::attribution::authorship::Authorship::parse(&note).is_ok());
            git(source.path(), &["notes", "--ref=ai", "add", "-m", &note]);
        }
        let sha = git(source.path(), &["rev-parse", "HEAD"]).trim().to_owned();
        git(source.path(), &["config", "uploadpack.allowFilter", "true"]);
        let partial = home.path().join("partial");
        let url = format!("file://{}", source.path().display());
        git(home.path(), &["clone", "--filter=blob:none", "--no-checkout", &url, partial.to_str().unwrap()]);
        git(&partial, &["fetch", "--filter=blob:none", "origin", "refs/notes/ai:refs/notes/ai"]);
        // Materialize parent/current file blobs and ONLY the queried commit's note.
        // The older note stays promised and fails the resolver's full-file attribution.
        git(&partial, &["blame", "HEAD", "--", "a.txt"]);
        git(&partial, &["notes", "--ref=ai", "show", &sha]);
        git(&partial, &["remote", "remove", "origin"]);
        crate::attribution::why_line::tests::attach(home.path(), &partial);
        git(&partial, &["remote", "set-url", "origin", &url]);
        git(&partial, &["config", "remote.origin.promisor", "true"]);
        git(&partial, &["config", "remote.origin.partialclonefilter", "blob:none"]);
        git(source.path(), &["remote", "add", "origin", "https://github.com/acme/api.git"]);
        // Principal 2 has a complete clone; principal 3 is another binding of clone 1.
        let first = crate::context_repo::path_to_org_root(home.path(), &Principal::from(1), "acme").unwrap();
        for (id, path) in [(2, source.path()), (3, partial.as_path())] {
            let org = crate::context_repo::path_to_org_root(home.path(), &Principal::from(id), "acme").unwrap();
            std::fs::create_dir_all(&org).unwrap();
            for name in ["org.yml", "people.yml", "teams.yml"] {
                std::fs::copy(first.join(name), org.join(name)).unwrap();
            }
            git2::Repository::init(&org).unwrap();
            // Identity remains the configured acme/api even for the offline promisor URL.
            if id == 3 {
                git(&partial, &["remote", "set-url", "origin", "https://github.com/acme/api.git"]);
            }
            let attachment = crate::commands::attachments::existing_checkout_attachment(path).unwrap();
            if id == 3 { git(&partial, &["remote", "set-url", "origin", &url]); }
            crate::commands::attachments::upsert_attachment_record(home.path(), id, attachment, None, false).unwrap();
        }
        let commit = git::Commit {
            sha: sha.clone(), subject: "queried".into(), authored_at: 2000, author: None,
            files: vec!["a.txt".into()], session_ids: vec!["durable-session".into()],
            basis: "exact", unavailable: None, ranges: None, ranges_basis: None, ranges_reason: None, unlanded: false, landed: false,
        };
        let mut older = commit.clone();
        older.sha = git(source.path(), &["rev-parse", "HEAD~1"]).trim().to_owned();
        older.authored_at = 1000;
        let complete = source.path().to_owned();
        let facts = git::GitFacts {
            commits: vec![commit, older],
            repo_paths: BTreeMap::from([(sha, vec![partial.clone(), partial.clone(), complete.clone()])]),
            ..Default::default()
        };
        let params = serde_json::from_value(json!({"storyId":"SV-29","limit":1,"include":["ranges"]})).unwrap();
        RetryFixture { home, _source: source, partial, complete, facts, params }
    }

    #[tokio::test]
    async fn review_retry_resolver_time_promised_note_uses_complete_clone_without_fetch() {
        use crate::attribution::why_line::tests::object_count;
        let fixture = retry_fixture();
        let home = fixture.home.path();
        let sha = &fixture.facts.commits[0].sha;
        let before = (object_count(&fixture.partial), object_count(&fixture.complete));
        assert_eq!(prepare_bound(home, "github.com/acme/api", &fixture.partial, sha, git2::Oid::from_str(sha).unwrap(), &mut BTreeMap::new(), false).unwrap()["a.txt"], vec![(2,2)]);
        let query = why_line::Params { repo: "acme/api".into(), rev: Some(sha.clone()), story: None, path: "a.txt".into(), start: 2, end: 2 };
        let first = why_line::resolve_at(home, &Principal::from(1), &home.join("usage.sqlite"), &home.join("session-conversations"), &query, &CallerScope::Human, HashMap::new()).await.unwrap();
        assert_eq!(first["status"], "unavailable");
        assert_eq!(first["reason"], "objects not in local copy");
        let calls = RefCell::new(Vec::new());
        let (page, _) = page_with(home, &fixture.params, &fixture.facts, |principal, params| {
            calls.borrow_mut().push((principal.clone(),params.start,params.end));
            async move { why_line::resolve_at(home, &principal, &home.join("usage.sqlite"), &home.join("session-conversations"), &params, &CallerScope::Human, HashMap::new()).await }
        }).await.unwrap();
        assert_eq!(page[0].sha, *sha, "pagination must select the queried commit");
        assert_eq!(page[0].ranges_basis.as_deref(), Some("exact"));
        assert_eq!(page[0].ranges_reason, None);
        assert_eq!(page[0].ranges.as_ref().unwrap()[0]["start"], 2);
        assert_eq!(page[0].ranges.as_ref().unwrap()[0]["end"], 2);
        assert_eq!(page[0].ranges.as_ref().unwrap()[0]["commit"], *sha);
        assert_eq!(*calls.borrow(), vec![(Principal::from(1),2,2),(Principal::from(2),2,2)]);
        assert_eq!((object_count(&fixture.partial), object_count(&fixture.complete)), before, "neither clone may fetch during preparation or attribution");
    }

    #[tokio::test]
    async fn review_retry_exhaustion_keeps_only_last_reason_and_success_stops() {
        let fixture = retry_fixture();
        for first_is_error in [false, true] {
        let calls = RefCell::new(Vec::new());
        let (page, _) = page_with(fixture.home.path(), &fixture.params, &fixture.facts, |principal, _| {
            calls.borrow_mut().push(principal.clone());
            async move {
                if principal == Principal::from(1) {
                    if first_is_error { Err("first failure".into()) }
                    else { Ok(json!({"status":"unavailable","reason":"first failure"})) }
                }
                else { Err("last failure".into()) }
            }
        }).await.unwrap();
        assert_eq!(*calls.borrow(), vec![Principal::from(1),Principal::from(2)]);
        assert_eq!(page[0].ranges_basis.as_deref(), Some("unknown"));
        assert_eq!(page[0].ranges_reason.as_deref(), Some("last failure"));
        assert!(page[0].ranges.as_ref().unwrap().is_empty());
        }
        let calls = RefCell::new(Vec::new());
        let (page, _) = page_with(fixture.home.path(), &fixture.params, &fixture.facts, |principal, params| {
            calls.borrow_mut().push(principal);
            async move { Ok(json!({"ranges":[{"start":2,"end":2,"commit":params.rev,"basis":"exact"}]})) }
        }).await.unwrap();
        assert_eq!(*calls.borrow(), vec![Principal::from(1)]);
        assert_eq!(page[0].ranges_basis.as_deref(), Some("exact"));
        assert_eq!(page[0].ranges_reason, None);
    }

    async fn partial_unavailable_retains_durable_facts(error: bool) {
        let (home, _) = crate::session_usage::tests::store();
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q", "-b", "main"]);
        crate::attribution::why_line::tests::attach(home.path(), repo.path());
        for path in ["a.txt", "b.txt"] {
            std::fs::write(repo.path().join(path), "new\n").unwrap();
        }
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-qm", "two files"]);
        let sha = git(repo.path(), &["rev-parse", "HEAD"]).trim().to_owned();
        let text = "a.txt\n  h_fixture 1\nb.txt\n  h_fixture 1\n---\n{\"schema_version\":\"authorship/3.0.0\"}";
        assert!(crate::attribution::authorship::Authorship::parse(text).is_ok());
        git(repo.path(), &["notes", "--ref=ai", "add", "-m", text, &sha]);
        let commit = git::Commit {
            sha: sha.clone(),
            subject: "two files".into(),
            authored_at: 1000, author: None,
            files: vec!["a.txt".into(), "b.txt".into()],
            session_ids: vec!["durable-session".into()],
            basis: "exact",
            unavailable: None,
            ranges: None,
            ranges_basis: None,
            ranges_reason: None, unlanded: false, landed: false,
        };
        let original = serde_json::to_value(&commit).unwrap();
        let facts = git::GitFacts {
            commits: vec![commit],
            repo_paths: BTreeMap::from([(sha, vec![repo.path().to_owned()])]),
            ..Default::default()
        };
        let params: Params =
            serde_json::from_value(json!({"storyId":"SV-29","include":["ranges"]})).unwrap();
        let reason = if error {
            "objects not in local copy"
        } else {
            "path not in revision"
        };
        let (page,_) = page_with(home.path(),&params,&facts,|_,params|async move {
            if params.path == "a.txt" { Ok(json!({"ranges":[{"start":1,"end":1,"commit":params.rev,"basis":"exact","reason":"human","tool":"human"}]})) }
            else if error { Err(reason.into()) }
            else { Ok(json!({"status":"unavailable","reason":reason})) }
        }).await.unwrap();
        let mut actual = serde_json::to_value(&page[0]).unwrap();
        assert_eq!(actual["ranges"].as_array().unwrap().len(), 1);
        assert_eq!(actual["ranges"][0]["path"], "a.txt");
        assert_eq!(actual["ranges"][0]["basis"], "exact");
        assert_eq!(actual["ranges"][0]["reason"], "human");
        assert_eq!(actual["rangesBasis"], "unknown");
        assert_eq!(actual["rangesReason"], reason);
        for key in ["ranges", "rangesBasis", "rangesReason"] {
            actual.as_object_mut().unwrap().remove(key);
        }
        assert_eq!(actual, original);
    }

    #[tokio::test]
    async fn review_partial_status_unavailable_preserves_success_and_durable_facts() {
        partial_unavailable_retains_durable_facts(false).await;
    }
    #[tokio::test]
    async fn review_partial_error_preserves_success_and_durable_facts() {
        partial_unavailable_retains_durable_facts(true).await;
    }

    #[tokio::test]
    async fn page_files_use_added_bounding_spans_and_exclude_unchanged_lines() {
        let (home, mut store) = crate::session_usage::tests::store();
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q", "-b", "main"]);
        crate::attribution::why_line::tests::attach(home.path(), repo.path());
        let original: Vec<_> = (1..=10000).map(|n| format!("line {n}")).collect();
        std::fs::write(repo.path().join("large.txt"), original.join("\n") + "\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-qm", "base"]);
        git(
            repo.path(),
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
        git(repo.path(), &["checkout", "-qb", "feat/SV-29-work"]);
        let mut lines = original.clone();
        for index in [9, 19] {
            lines[index] = format!("changed {index}");
        }
        std::fs::write(repo.path().join("large.txt"), lines.join("\n") + "\n").unwrap();
        std::fs::write(repo.path().join("small.txt"), "new\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-qam", "first"]);
        let first = git(repo.path(), &["rev-parse", "HEAD"]).trim().to_owned();
        note(repo.path(), &first);
        for index in [99, 4099] {
            lines[index] = format!("second {index}");
        }
        std::fs::write(repo.path().join("large.txt"), lines.join("\n") + "\n").unwrap();
        git(repo.path(), &["commit", "-qam", "second"]);
        let second = git(repo.path(), &["rev-parse", "HEAD"]).trim().to_owned();
        note(repo.path(), &second);
        let mut record = crate::session_usage::record::SessionRecord::new("page-session");
        record.worktree = Some(repo.path().to_string_lossy().into_owned());
        record.branch = Some("feat/SV-29-work".into());
        store.write_session_record(&record).unwrap();
        let facts = git::collect(
            home.path(),
            &store,
            "SV-29",
            &[json!({"session":record})],
            &[CallerScope::Human],
        )
        .unwrap();
        // Both default spellings bypass the same resolver boundary used by the real reader.
        for args in [
            json!({"storyId":"SV-29","limit":1}),
            json!({"storyId":"SV-29","limit":1,"include":[]}),
        ] {
            let params: Params = serde_json::from_value(args).unwrap();
            let expected = git::page(&params, &facts.commits).unwrap();
            let actual = page_with(home.path(), &params, &facts, |_, _| async {
                panic!("default read must not resolve")
            })
            .await
            .unwrap();
            assert_eq!(
                serde_json::to_vec(&actual).unwrap(),
                serde_json::to_vec(&expected).unwrap()
            );
        }
        let calls = RefCell::new(Vec::new());
        let p: Params =
            serde_json::from_value(json!({"storyId":"SV-29","limit":1,"include":["ranges"]}))
                .unwrap();
        let (page, cursor) = page_with(home.path(), &p, &facts, |_, p| {
            calls.borrow_mut().push((p.rev.clone().unwrap(),p.path.clone(),p.start,p.end));
            async move { Ok(json!({"ranges":[{"start":p.start,"end":p.end,"commit":p.rev,"basis":"exact","nativeSessionId":"native"}]})) }
        }).await.unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].ranges_basis.as_deref(), Some("exact"));
        let page_sha = page[0].sha.clone();
        let calls = calls.into_inner();
        assert!(calls.iter().all(|c| c.0 == page_sha));
        assert!(calls.iter().all(|c| c.3 - c.2 < 2000));
        if page_sha == second {
            assert_eq!(
                calls
                    .iter()
                    .filter(|c| c.1 == "large.txt")
                    .map(|c| (c.2, c.3))
                    .collect::<Vec<_>>(),
                vec![(100, 2099), (2100, 4099), (4100, 4100)]
            );
            assert_eq!(
                page[0]
                    .ranges
                    .as_ref()
                    .unwrap()
                    .iter()
                    .filter(|r| r["path"] == "large.txt")
                    .map(|r| (r["start"].as_u64().unwrap(), r["end"].as_u64().unwrap()))
                    .collect::<Vec<_>>(),
                vec![(100, 100), (4100, 4100)]
            );
        } else {
            assert_eq!(
                calls
                    .iter()
                    .filter(|c| c.1 == "large.txt")
                    .map(|c| (c.2, c.3))
                    .collect::<Vec<_>>(),
                vec![(10, 20)]
            );
            assert_eq!(
                page[0]
                    .ranges
                    .as_ref()
                    .unwrap()
                    .iter()
                    .filter(|r| r["path"] == "large.txt")
                    .map(|r| (r["start"].as_u64().unwrap(), r["end"].as_u64().unwrap()))
                    .collect::<Vec<_>>(),
                vec![(10, 10), (20, 20)]
            );
        }
        assert_eq!(
            calls.iter().filter(|c| c.1 == "small.txt").count(),
            usize::from(page_sha == first)
        );
        let next: Params = serde_json::from_value(
            json!({"storyId":"SV-29","limit":1,"cursor":cursor,"include":["ranges"]}),
        )
        .unwrap();
        let calls = RefCell::new(Vec::new());
        let (page, _) = page_with(home.path(), &next, &facts, |_, p| {
            calls.borrow_mut().push((p.rev.clone().unwrap(),p.start,p.end));
            async move { Ok(json!({"ranges":[{"start":p.start,"end":p.end,"commit":"other-commit","basis":"exact"}]})) }
        }).await.unwrap();
        assert!(calls.borrow().iter().all(|c| c.0 != page_sha));
        assert!(page[0].ranges.as_ref().unwrap().is_empty());
    }
}

#[cfg(test)]
mod edge_tests {
    use super::*;
    use crate::attribution::git_ai::tests::git;

    #[tokio::test]
    async fn root_deletion_merge_and_missing_clone_retain_durable_facts() {
        let (home, _) = crate::session_usage::tests::store();
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]);
        crate::attribution::why_line::tests::attach(home.path(), dir.path());
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-qm", "root"]);
        let repo = git2::Repository::open(dir.path()).unwrap();
        let root = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(
            crate::commands::git_sync::commit_added_windows_local(&repo, &root, false).unwrap()["a.txt"],
            vec![(1, 3)]
        );
        std::fs::write(dir.path().join("a.txt"), "one\nthree\n").unwrap();
        git(dir.path(), &["commit", "-qam", "delete"]);
        let deletion = repo.head().unwrap().peel_to_commit().unwrap();
        assert!(
            crate::commands::git_sync::commit_added_windows_local(&repo, &deletion, false)
                .unwrap()
                .is_empty()
        );
        // A real two-parent commit is excluded regardless of available note evidence.
        let sig = git2::Signature::now("fixture", "fixture@example.com").unwrap();
        let sha = repo
            .commit(
                None,
                &sig,
                &sig,
                "merge",
                &deletion.tree().unwrap(),
                &[&deletion, &root],
            )
            .unwrap()
            .to_string();
        let mut commits = vec![git::Commit {
            sha: sha.clone(),
            subject: "merge".into(),
            authored_at: 0, author: None,
            files: vec!["a.txt".into()],
            session_ids: vec!["durable".into()],
            basis: "exact",
            unavailable: None,
            ranges: None,
            ranges_basis: None,
            ranges_reason: None, unlanded: false, landed: false,
        }];
        let root_sha = root.id().to_string();
        commits[0].sha = root_sha.clone();
        let root_candidates = BTreeMap::from([(root_sha.clone(), vec![dir.path().to_owned()])]);
        enrich_with(home.path(), &mut commits, &root_candidates, |_, _| async {
            panic!("no note must not resolve")
        })
        .await;
        assert_eq!(
            commits[0].ranges_reason.as_deref(),
            Some(&*why_line::missing_note_reason(true, "no git-ai note on commit"))
        );
        super::tests::note(dir.path(), &root_sha);
        enrich_with(home.path(),&mut commits,&root_candidates,|_,p| async move {
            assert_eq!((p.start,p.end),(1,3));
            Ok(json!({"ranges":[{"start":1,"end":3,"commit":p.rev,"basis":"unknown","reason":"retained resolver reason"}]}))
        }).await;
        assert_eq!(
            commits[0].ranges.as_ref().unwrap()[0]["reason"],
            "retained resolver reason"
        );
        commits[0].sha = deletion.id().to_string();
        super::tests::note(dir.path(), &commits[0].sha);
        let deletion_candidates =
            BTreeMap::from([(commits[0].sha.clone(), vec![dir.path().to_owned()])]);
        enrich_with(
            home.path(),
            &mut commits,
            &deletion_candidates,
            |_, _| async { panic!("removed-only lines must not resolve") },
        )
        .await;
        assert_eq!(commits[0].ranges_reason.as_deref(), Some("no added lines"));
        commits[0].sha = sha.clone();
        let candidates = BTreeMap::from([(sha.clone(), vec![dir.path().to_owned()])]);
        enrich_with(home.path(), &mut commits, &candidates, |_, _| async {
            panic!("merge must not resolve")
        })
        .await;
        assert_eq!(commits[0].ranges_reason.as_deref(), Some("merge commit"));
        assert_eq!(commits[0].basis, "exact");
        assert_eq!(commits[0].session_ids, vec!["durable"]);
        drop(root);
        drop(deletion);
        drop(repo);
        dir.close().unwrap();
        enrich_with(home.path(), &mut commits, &candidates, |_, _| async {
            panic!("missing clone must not resolve")
        })
        .await;
        assert_eq!(
            commits[0].ranges_reason.as_deref(),
            Some("repository not found locally")
        );
        assert_eq!(commits[0].ranges.as_ref().unwrap(), &Vec::<Value>::new());
    }
}
