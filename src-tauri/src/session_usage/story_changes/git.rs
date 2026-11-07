// SPDX-License-Identifier: MIT
//! Local git contribution facts; no fetches or GitHub lookup.
use super::*;
use crate::attribution::history::{self, default_branch, is_boundary, story_merge, StoryTips};
use crate::attribution::ranges::{HISTORY_PAST_SHALLOW, HISTORY_PAST_SHALLOW_PARTIAL};
use crate::commands::git_sync::{commit_changed_paths_local, new_sorted_revwalk};
use crate::install::worktree_index::{self, story_id_from_branch};
use std::path::PathBuf;

pub(super) const SCAN_LIMIT: usize = history::SCAN_LIMIT;
const HISTORY_NOT_LOCAL: &str = "history objects not in local copy";
const WORK_REPO_NOT_ATTACHED: &str = "work repo not attached";
const RANGE_NOT_LOCAL: &str = "file range not available locally";
const NOT_IN_MERGED_PR: &str = "not in this story's merged PRs";
pub(super) const STALE_CURSOR: &str = "invalid: stale or mismatched story_changes cursor";
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Commit {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ranges: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ranges_basis: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ranges_reason: Option<String>,
    pub sha: String,
    pub subject: String,
    pub authored_at: i64,
    pub author: Option<String>,
    pub files: Vec<String>,
    pub session_ids: Vec<String>,
    pub basis: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<&'static str>,
    /// Read only through a branch that never merged: not part of what landed when the story has
    /// merged PRs, so the story's lines leave it out then.
    #[serde(skip)]
    pub unlanded: bool,
    /// Read through a merged PR's range (not only through a branch that may still be open).
    #[serde(skip)]
    pub landed: bool,
}
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFacts {
    #[serde(skip)]
    pub repo_paths: BTreeMap<String, Vec<PathBuf>>,
    /// The git directories this story's facts were read from; the totals index watches their refs.
    #[serde(skip)]
    pub repos: BTreeSet<PathBuf>,
    /// Origin's head and the story's branch tips as they were when the walk began (kept reads
    /// only; empty otherwise), with the repos and branch names they were read from.
    #[serde(skip)]
    pub refs_stamp: String,
    #[serde(skip)]
    pub stamp_spec: super::cache::StampSpec,
    pub branches: Vec<Value>,
    pub prs: Vec<Value>,
    pub worktrees: Vec<Value>,
    pub commits: Vec<Commit>,
    pub scan_limit: usize,
    #[serde(skip)]
    pub lines: BTreeMap<String, LineStats>,
    /// Line counts come from merged PRs, so a file outside them has no counted lines.
    #[serde(skip)]
    pub landed_lines: bool,
    pub unavailable: BTreeMap<&'static str, &'static str>,
    /// The attached checkouts of the story's source repo: where a trail fetch would go.
    #[serde(skip)]
    pub attached: Vec<PathBuf>,
    /// The story file says it is done, so a merge is expected on origin's default branch.
    #[serde(skip)]
    pub story_done: bool,
}
impl GitFacts {
    /// One file's line facts; never zero-filled for a file outside the counted diffs.
    pub(super) fn line_stats(&self, path: &str) -> LineStats {
        if let Some(reason) = self.unavailable.get("fileLines") {
            return LineStats::unavailable(reason);
        }
        self.lines.get(path).cloned().unwrap_or_else(|| {
            if self.landed_lines { LineStats::unavailable(NOT_IN_MERGED_PR) } else { LineStats::zero() }
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct LineCounts { pub added: usize, pub removed: usize }
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LineStats { pub lines: Option<LineCounts>, pub lines_reason: Option<&'static str> }
impl LineStats {
    pub(super) fn unavailable(reason: &'static str) -> Self { Self { lines: None, lines_reason: Some(reason) } }
    pub(super) fn zero() -> Self { Self { lines: Some(LineCounts::default()), lines_reason: None } }
}

fn pr_metadata(repo: Option<&git2::Repository>, number: Option<u64>, merged: Option<&git2::Commit<'_>>) -> Value {
    let (url, reason) = if let Some(number) = number {
        match repo.and_then(|repo| std::result::Result::ok(repo.find_remote("origin"))).and_then(|remote| std::result::Result::ok(remote.url()).map(str::to_owned)) {
            None => (None, Some("origin not found locally")),
            Some(raw) => match crate::context_repo::normalize_source_repo_url(&raw) {
                Ok(origin) if origin.normalized_url.starts_with("github.com/") => (Some(format!("https://github.com/{}/{}/pull/{number}",origin.owner,origin.repo)), None),
                _ => (None, Some("origin is not GitHub")),
            }
        }
    } else { (None, Some("PR number not available")) };
    json!({"number":number,"url":url,"urlReason":reason,"mergedAt":merged.map(|commit|commit.time().seconds()*1000),"mergedAtReason":if merged.is_some(){None}else{Some("not merged")}})
}
fn pr_row(repo: &git2::Repository, commit: &git2::Commit<'_>, link: &history::MergeLink, basis: &str) -> Value {
    let source = if link.merge { "merge-commit" } else { "squash-subject" };
    let mut row=pr_metadata(Some(repo),link.number,Some(commit));
    if let Some(reason) = link.number_reason {row["numberReason"]=json!(reason);}
    let branch = Option::as_deref(&link.branch);
    row["mergedSha"]=json!(git2::Commit::id(commit).to_string());row["source"]=json!(source);row["branch"]=json!(branch);row["basis"]=json!(basis);
    if source=="squash-subject" {row["commits"]=json!({"unavailable":"squash-merged"});}
    row
}

// Tree-to-tree counts for a branch range or one landed PR against its first parent.
fn range_lines(repo: &git2::Repository, base: git2::Oid, head: git2::Oid) -> Result<(git2::Oid, BTreeMap<String, LineStats>),git2::Error> {
    let base=repo.merge_base(base,head)?;
    let before=repo.find_commit(base)?.tree()?;let after=repo.find_commit(head)?.tree()?;
    let mut options=git2::DiffOptions::new();options.include_typechange(true);
    let diff=repo.diff_tree_to_tree(Some(&before),Some(&after),Some(&mut options))?;
    let mut rows=std::collections::BTreeMap::new();
    for (index,delta) in diff.deltas().enumerate() {
        let Some(path)=git2::DiffFile::path(&delta.new_file()).or_else(||git2::DiffFile::path(&delta.old_file())) else {continue;};
        let stats=if let Some(patch)=git2::Patch::from_diff(&diff,index)? {if patch.delta().flags().contains(git2::DiffFlags::BINARY){LineStats::unavailable("binary")}else{let (_,added,removed)=patch.line_stats()?;LineStats{lines:Some(LineCounts{added,removed}),lines_reason:None}}}else{LineStats::unavailable("binary")};
        rows.insert(path.to_string_lossy().into_owned(),stats);
    }
    Ok((base,rows))
}
/// Sum per-file counts across ranges; the first reason a count became unknown stays its reason.
fn add_line_rows(totals: &mut BTreeMap<String, LineStats>, rows: BTreeMap<String, LineStats>) {
    for (path, stats) in rows {
        let total = totals.entry(path).or_insert_with(LineStats::zero);
        match (&mut total.lines, stats.lines) {
            (Some(total), Some(lines)) => { total.added += lines.added; total.removed += lines.removed; }
            (None, _) => {}
            (Some(_), None) => *total = LineStats::unavailable(stats.lines_reason.unwrap_or(RANGE_NOT_LOCAL)),
        }
    }
}
/// The first candidate repository that holds the whole range decides its rows.
fn first_range<'r>(candidates: impl IntoIterator<Item = (&'r git2::Repository, git2::Oid)>, head: git2::Oid) -> Result<BTreeMap<String, LineStats>, &'static str> {
    let mut observed = Err(HISTORY_NOT_LOCAL);
    for (repo, base) in candidates {
        match range_lines(repo, base, head) {
            Ok((_, rows)) => return Ok(rows),
            Err(error) => observed = Err(if error.code() == git2::ErrorCode::NotFound { HISTORY_NOT_LOCAL } else { RANGE_NOT_LOCAL }),
        }
    }
    observed
}

pub(super) struct StorySource {
    pub(super) path: PathBuf,
    pub(super) org: String,
}

/// Keep the durable source's org identity with its path; scope applies before file reads.
pub(super) fn story_source(
    home: &Path,
    store: &UsageStore,
    story: &str,
    scope: &CallerScope,
) -> Option<StorySource> {
    let mut stmt=std::result::Result::ok(rusqlite::Connection::prepare(&store.conn,"SELECT DISTINCT story_path,org FROM session_story WHERE lower(story_id)=lower(?1) ORDER BY story_path,org"))?;
    let paths = std::result::Result::ok(stmt.query_map([story], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    }))?;
    for (path, org) in paths.flatten() {
        let path = PathBuf::from(path);
        if scope.allows(&path) && path.is_file() {
            return Some(StorySource { path, org });
        }
    }
    // Story ids are written upper-case; a lower-case ask finds the same file.
    let canonical = story.to_ascii_uppercase();
    // A team caller's story is its own org's: another org's file of the same id is not it,
    // even where the team view would admit it as personal content.
    let caller_org = match scope {
        CallerScope::Team(view) => Some(crate::context_repo::inheritance::TeamView::org(&**view).to_owned()),
        _ => None,
    };
    for root in crate::context_repo::org_enumeration::enumerate_all_org_roots(home) {
        if let Some(caller_org) = caller_org.as_deref() {
            let root_text = root.to_string_lossy();
            let org = crate::context_repo::org_slug_from_org_root(&root_text);
            if org != Some(caller_org) {
                continue;
            }
        }
        // Through the frontmatter index, never an org walk per story.
        let find = |id: &str| crate::session_usage::story::story_file_in_org(&root, id);
        if let Some(path) = Option::or_else(find(story), || {
            Option::flatten(bool::then(canonical != story, || find(&canonical)))
        }) {
            if scope.allows(&path) {
                let org = crate::context_repo::org_slug_from_org_root(&root.to_string_lossy())?
                    .to_owned();
                return Some(StorySource { path, org });
            }
        }
    }
    None
}

pub(super) use crate::context_repo::org_enumeration::principals;

fn status_done(fields: &BTreeMap<String, Value>) -> bool {
    fields
        .get("status")
        .and_then(serde_json::Value::as_str)
        .is_some_and(history::status_is_done)
}

/// Whether the story file at `path` says it has landed.
pub(super) fn source_is_done(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|text| {
        status_done(&crate::commands::frontmatter_editor::frontmatter_json(
            crate::commands::pipeline_format::format_of(path),
            &text,
        ))
    })
}

enum HistoryError {
    ObjectsNotLocal,
    Other(String),
}
impl From<git2::Error> for HistoryError {
    fn from(error: git2::Error) -> Self {
        if git2::Error::code(&error) == git2::ErrorCode::NotFound {
            Self::ObjectsNotLocal
        } else {
            Self::Other(error.to_string())
        }
    }
}

/// The canonical remote key of `path`'s `origin`, when it is a repository with one.
fn origin_key(path: &Path) -> Option<String> {
    let repo = Result::ok(git2::Repository::open(path))?;
    let remote = Result::ok(git2::Repository::find_remote(&repo, "origin"))?;
    Option::and_then(
        Result::ok(git2::Remote::url(&remote)),
        crate::context_repo::canonical_remote_key,
    )
}

/// The principal's context clone for `org` when its origin is the story's `work_repo`.
/// Context worktrees are linked worktrees of it, so they share this history.
fn work_repo_clone(
    home: &Path,
    principal: &crate::domain::Principal,
    org: &str,
    slug: &crate::domain::SourceRepoSlug,
) -> Option<PathBuf> {
    let root = Result::ok(crate::context_repo::path_to_org_root(home, principal, org))?;
    let repo = Result::ok(git2::Repository::open(&root))?;
    let remote = Result::ok(git2::Repository::find_remote(&repo, "origin"))?;
    let key = Option::and_then(
        Result::ok(git2::Remote::url(&remote)),
        crate::context_repo::canonical_remote_key,
    )?;
    bool::then_some(key == slug.remote_key(), root)
}

/// What the git half reads of one session: where it ran, and whether it is tied to the story only
/// by a context worktree's name (`associated`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SessionRef {
    pub branch: Option<String>,
    pub worktree: Option<String>,
    pub workspace: Option<String>,
    pub associated: bool,
}

impl SessionRef {
    /// From a `session_read` answer carrying its `basis`.
    pub(super) fn of_read(session: &Value) -> Self {
        let record = &session["session"];
        let text = |key: &str| Option::map(Value::as_str(&record[key]), str::to_owned);
        SessionRef {
            branch: text("branch"),
            worktree: text("worktree"),
            workspace: text("workspace"),
            associated: Value::as_str(&session["basis"]) == Some("associated"),
        }
    }

    /// From a story session row (`rows::session_row`), which holds the same fields.
    pub(super) fn of_row(row: &crate::session_usage::contribution::Row) -> Self {
        let extra = &row.contribution.extra;
        let text = |key: &str| {
            Option::map(
                Option::and_then(serde_json::Map::get(extra, key), Value::as_str),
                str::to_owned,
            )
        };
        SessionRef {
            branch: text("branch"),
            worktree: text("worktree"),
            workspace: text("workspace"),
            associated: Option::and_then(serde_json::Map::get(extra, "basis"), Value::as_str)
                == Some("associated"),
        }
    }
}

/// [`collect_refs`] over `session_read` answers.
pub(super) fn collect(
    home: &Path,
    store: &UsageStore,
    story: &str,
    sessions: &[Value],
    scopes: &[CallerScope],
) -> Result<GitFacts, String> {
    let refs: Vec<SessionRef> = Iterator::collect(Iterator::map(
        <[Value]>::iter(sessions),
        SessionRef::of_read,
    ));
    collect_refs(home, store, story, &refs, scopes, false, None)
}

/// The error a walk ends with when its cancel flag is set (a hosted read past its deadline).
const CANCELLED: &str = "cancelled";

pub(super) fn cancelled(cancel: &Option<std::sync::Arc<std::sync::atomic::AtomicBool>>) -> bool {
    Option::is_some_and(Option::as_ref(cancel), |flag| {
        std::sync::atomic::AtomicBool::load(flag, std::sync::atomic::Ordering::Relaxed)
    })
}

/// The story's branches in `repo` by name (local and origin's): the names a kept read is
/// stamped with, so a branch created or fetched later invalidates it.
pub(super) fn story_branch_names(repo: &git2::Repository, story: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    if let Ok(branches) = git2::Repository::branches(repo, None) {
        for (branch, kind) in Iterator::flatten(branches) {
            let Ok(Some(name)) = git2::Branch::name(&branch) else {
                continue;
            };
            let name = if kind == git2::BranchType::Remote {
                name.strip_prefix("origin/").unwrap_or(name)
            } else {
                name
            };
            if story_id_from_branch(name).is_some_and(|id| id.eq_ignore_ascii_case(story)) {
                BTreeSet::insert(&mut found, name.to_owned());
            }
        }
    }
    found
}

pub(super) fn collect_refs(
    home: &Path,
    store: &UsageStore,
    story: &str,
    sessions: &[SessionRef],
    scopes: &[CallerScope],
    stamp: bool,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<GitFacts, String> {
    super::count_git_walk(home);
    let mut out = GitFacts {
        scan_limit: SCAN_LIMIT,
        ..Default::default()
    };
    let (mut names, mut paths, mut roots, mut workspaces) = (
        BTreeSet::new(),
        BTreeSet::new(),
        BTreeSet::new(),
        BTreeSet::new(),
    );
    let mut exact_names = BTreeSet::new();
    // Sessions tied to the story only by a context worktree's name: their branch and worktree
    // count only when `work_repo` names the repo they are in, and then as `associated`.
    let mut associated: Vec<(Option<String>, Option<PathBuf>)> = Vec::new();
    for session in sessions {
        if session.associated {
            associated.push((
                session.branch.clone(),
                Option::map(Option::as_deref(&session.worktree), PathBuf::from),
            ));
            if let Some(path) = Option::as_deref(&session.workspace) {
                workspaces.insert(PathBuf::from(path));
            }
            continue;
        }
        if let Some(name) = Option::as_deref(&session.branch) {
            names.insert(name.to_owned());
            exact_names.insert(name.to_owned());
        }
        if let Some(path) = Option::as_deref(&session.workspace) {
            workspaces.insert(PathBuf::from(path));
        }
        if let Some(path) = Option::as_deref(&session.worktree) {
            roots.insert(PathBuf::from(path));
            if !std::path::Path::exists(Path::new(path)) && paths.insert(path.to_owned()) {
                out.worktrees
                    .push(json!({"path":path,"state":"removed","basis":"exact"}));
            }
        }
    }
    let principals = principals(home);
    // Keep the selected attachment remote (including upstream fork bindings), not a
    // launch preference identity, for the same cursor namespace describe_setup exposes.
    let attachments: Vec<_> = principals
        .iter()
        .filter_map(|principal| {
            std::result::Result::ok(crate::context_repo::attachments_io::read_attachments(
                &crate::context_repo::path_to_user_attachments_file(home, principal),
            ))
        })
        .collect();
    let records = Iterator::flat_map(<[crate::domain::Principal]>::iter(&principals), |p| {
        Result::unwrap_or_default(worktree_index::find_by_story_id(home, p, story))
    });
    for record in Iterator::filter(records, |r| {
        Iterator::any(&mut <[CallerScope]>::iter(scopes), |scope| {
            CallerScope::allows(scope, &r.story_source_path)
        })
    }) {
        BTreeSet::insert(&mut roots, std::path::PathBuf::clone(&record.code_repo_path));
        if Path::is_dir(&record.code_repo_path)
            && BTreeSet::insert(&mut paths, std::borrow::Cow::into_owned(Path::to_string_lossy(&record.code_repo_path)))
        {
            std::vec::Vec::push(&mut out.worktrees, json!({"path":record.code_repo_path,"state":"live","basis":"exact"}));
            if let Ok(repo) = git2::Repository::open(&record.code_repo_path) {
                if let Ok(head) = git2::Repository::head(&repo) {
                    if git2::Reference::is_branch(&head) {
                        if let Ok(name) = git2::Reference::shorthand(&head) {
                            BTreeSet::insert(&mut names, str::to_owned(name));
                            BTreeSet::insert(&mut exact_names, str::to_owned(name));
                        }
                    }
                }
                // SV-CDE-82: record the story branch's head in its tips. Best-effort and
                // never waits: the read answers whether or not it lands.
                crate::install::story_tips::sight(
                    home,
                    story,
                    &repo,
                    crate::install::story_tips::Wait::Never,
                );
            }
        }
    }
    // A workspace can be the context repository. Only a source-repo identity match admits
    // it as a fallback after a worktree disappears; context edit provenance is separate.
    let mut work_missing = false;
    let mut work_by_convention = false;
    for scope in scopes {
        if let Some(source) = story_source(home, store, story, scope) {
            if let Ok(text) = std::fs::read_to_string(&source.path) {
                let fields = crate::commands::frontmatter_editor::frontmatter_json(
                    crate::commands::pipeline_format::format_of(&source.path),
                    &text,
                );
                out.story_done |= status_done(&fields);
                // SV-CDE-85: `work_repo` replaces `source_repo` for history reads only.
                let work = Option::filter(
                    Option::map(
                        Option::and_then(fields.get("work_repo"), serde_json::Value::as_str),
                        str::trim,
                    ),
                    |w| !str::is_empty(w),
                );
                let mut work_found = false;
                work_by_convention |= Option::is_some(&work);
                if let Some(raw) = Option::or_else(work, || {
                    Option::and_then(fields.get("source_repo"), serde_json::Value::as_str)
                }) {
                    let configured = principals
                        .iter()
                        .filter(|principal| {
                            crate::context_repo::path_to_org_root(home, principal, &source.org)
                                .is_ok_and(|root| std::path::Path::starts_with(&source.path, root))
                        })
                        .flat_map(|principal| {
                            worktree_index::team_source_repos_for_story(
                                home,
                                principal,
                                &source.org,
                                &source.path,
                            )
                        })
                        .collect::<Vec<_>>();
                    if let Some(slug) =
                        crate::domain::SourceRepoSlug::resolve_against(raw, &configured)
                    {
                        for workspace in &workspaces {
                            if std::result::Result::ok(git2::Repository::open(&workspace))
                                .is_some_and(|r| {
                                    std::result::Result::ok(r.find_remote("origin"))
                                        .and_then(|remote| {
                                            std::result::Result::ok(remote.url())
                                                .and_then(crate::context_repo::canonical_remote_key)
                                        })
                                        .is_some_and(|key| key == slug.remote_key())
                                })
                            {
                                roots.insert(workspace.clone());
                                work_found = true;
                            }
                        }
                        if Option::is_some(&work) {
                            for (branch, tree) in &associated {
                                let Some(tree) = tree else { continue };
                                if Option::is_some_and(origin_key(tree), |key| key == slug.remote_key()) {
                                    BTreeSet::insert(&mut roots, PathBuf::clone(tree));
                                    work_found = true;
                                    if let Some(branch) = branch {
                                        BTreeSet::insert(&mut names, String::clone(branch));
                                    }
                                }
                            }
                        }
                        for principal in &principals {
                            if Option::is_some(&work) {
                                // The context clone, or an attached source repo named as the work repo.
                                if let Some(path) = Option::or_else(
                                    work_repo_clone(home, principal, &source.org, &slug),
                                    || {
                                        Option::flatten(Result::ok(
                                            crate::commands::attachments::attached_clone_path_for_slug(
                                                home, principal, &slug,
                                            ),
                                        ))
                                    },
                                ) {
                                    work_found = true;
                                    if !<[PathBuf]>::contains(&out.attached, &path) {
                                        Vec::push(&mut out.attached, PathBuf::clone(&path));
                                    }
                                    BTreeSet::insert(&mut roots, path);
                                }
                            } else if let Ok(Some(path)) =
                                crate::commands::attachments::attached_clone_path_for_slug(
                                    home, principal, &slug,
                                )
                            {
                                if !out.attached.contains(&path) {
                                    out.attached.push(path.clone());
                                }
                                roots.insert(path);
                            }
                        }
                    }
                    work_missing |= Option::is_some(&work) && !work_found;
                }
            }
        }
    }
    if work_missing {
        std::collections::BTreeMap::insert(&mut out.unavailable, "commits", WORK_REPO_NOT_ATTACHED);
    }
    let mut repositories = std::collections::BTreeMap::new();
    for root in roots {
        if let Ok(repo) = git2::Repository::open(&root) {
            let common = crate::commands::context_git_watch::GitDirs::resolve(&root)
                .map(|dirs| dirs.common_dir)
                .unwrap_or_else(|| git2::Repository::path(&repo).to_owned());
            repositories.entry(common).or_insert(repo);
        }
    }
    out.repos = repositories.keys().cloned().collect();
    let mut commits = std::collections::BTreeMap::new();
    let mut line_ranges = BTreeMap::<(git2::Oid, git2::Oid), Vec<&git2::Repository>>::new();
    let mut landed_ranges = BTreeMap::<git2::Oid, Vec<(&git2::Repository, git2::Oid)>>::new();
    let mut branch_rows = std::collections::BTreeMap::<String, (u8, Value)>::new();
    let mut pr_rows = std::collections::BTreeMap::new();
    let mut repository_notes = Vec::new();
    let mut checkpoint_store = UsageStore::open(&home.join("usage.sqlite"))?;
    for (common, repo) in &repositories {
        let Some(root) = git2::Repository::workdir(repo) else {
            continue;
        };
        for branch in git2::Repository::branches(repo, None)
            .map_err(|e| e.to_string())?
            .flatten()
        {
            if let Ok(Some(name)) = git2::Branch::name(&branch.0) {
                let name = if branch.1 == git2::BranchType::Remote {
                    name.strip_prefix("origin/").unwrap_or(name)
                } else {
                    name
                };
                // A work repo's branches are named `<team>/<epic>-<n>`, lower case.
                let by_convention = work_by_convention
                    && Option::is_some_and(str::rsplit_once(name, '/'), |(_, tail)| {
                        crate::session_usage::story::convention_names(
                            &crate::session_usage::story::convention_tail(tail),
                            story,
                        )
                    });
                if by_convention
                    || story_id_from_branch(name).is_some_and(|id| id.eq_ignore_ascii_case(story))
                {
                    names.insert(name.to_owned());
                }
            }
        }
        // A kept read (`stamp`) notes the refs this walk reads before it starts: origin's default
        // head and the story's own branch tips, so it is kept only if they hold still.
        if stamp {
            out.stamp_spec.story = story.to_owned();
            Vec::push(&mut out.stamp_spec.repos, (common.clone(), names.clone()));
            out.refs_stamp = super::cache::stamp_of(&out.stamp_spec);
        }
        let mut merges = std::collections::BTreeMap::<String, Vec<git2::Oid>>::new();
        // Two-parent merges under the story's squash-style subject whose second parent is no
        // tip: their commits are read, with no branch row.
        let mut unnamed_merges: Vec<git2::Oid> = std::vec::Vec::new();
        // A merge links by its subject or by a second parent that is a story tip.
        let tips = StoryTips::for_story(home, repo, story, &names);
        // Origin's default branch, first parent; libgit2 shows a shallow boundary as a root.
        let (mut found, mut past_boundary) = (false, false);
        // A branch is proven unmerged only by a story with no squash link (a squash lands its
        // commits under a commit of its own) and a scan that covered everything since the
        // branch left origin's default branch, else a merge may be unseen.
        let (mut squash_seen, mut reached_root) = (false, false);
        let mut scanned = BTreeSet::new();
        if let Some(mut tip) = default_branch(repo).and_then(|id| Result::ok(repo.find_commit(id))) {
            for _ in 0..SCAN_LIMIT {
                if cancelled(&cancel) {
                    return Err(CANCELLED.into());
                }
                let id = git2::Commit::id(&tip);
                BTreeSet::insert(&mut scanned, id);
                match story_merge(repo, &tip, story, &names, &tips) {
                    Some(Err(reason)) => {
                        std::collections::BTreeMap::insert(&mut out.unavailable, "prs", reason);
                    }
                    Some(Ok(link)) => {
                    found = true;
                    squash_seen |= !link.merge;
                    let row = match Option::as_deref(&link.branch) {
                        Some(branch) => {
                            BTreeSet::insert(&mut names, str::to_owned(branch));
                            let merged = std::collections::BTreeMap::entry(&mut merges, str::to_owned(branch));
                            std::vec::Vec::push(std::collections::btree_map::Entry::or_default(merged), id);
                            // A recorded tip is exact; a live head found only by naming
                            // convention is as associated as the branch itself.
                            let exact = link.tip_recorded || BTreeSet::contains(&exact_names, branch);
                            pr_row(repo, &tip, &link, if exact { "exact" } else { "associated" })
                        }
                        None => {
                            if link.merge {
                                std::vec::Vec::push(&mut unnamed_merges, id);
                            }
                            pr_row(repo, &tip, &link, "associated")
                        }
                    };
                    pr_rows.entry(id.to_string()).or_insert((git2::Commit::time(&tip).seconds(), row));
                    if let Ok(parent) = tip.parent_id(0) {
                        landed_ranges.entry(id).or_default().push((repo, parent));
                    }
                    }
                    None => {}
                }
                if is_boundary(&tip) {
                    past_boundary = true;
                    break;
                }
                if git2::Commit::parent_count(&tip) == 0 {
                    reached_root = true;
                    break;
                }
                match git2::Commit::parent(&tip, 0) {
                    Ok(parent) => tip = parent,
                    Err(error) if git2::Error::code(&error) == git2::ErrorCode::NotFound => {
                        out.unavailable.insert("commits", HISTORY_NOT_LOCAL);
                        break;
                    }
                    Err(error) => return Err(error.to_string()),
                }
            }
        }
        // The merges seen before the boundary may not be all of them, or none were seen.
        // Only a story still in progress with a branch here reads that branch, whose commits
        // say for themselves; any status gets the reason when merges were seen or no branch is.
        let branch_here = names.iter().any(|name| {
            ["refs/heads/", "refs/remotes/origin/"]
                .iter()
                .any(|prefix| repo.find_reference(&format!("{prefix}{name}")).is_ok())
        });
        if past_boundary && (out.story_done || found || !branch_here) {
            let reason = if found { HISTORY_PAST_SHALLOW_PARTIAL } else { HISTORY_PAST_SHALLOW };
            out.unavailable.entry("commits").or_insert(reason);
        }
        let mut note_commits = BTreeSet::new();
        // Each story branch, then (when there are any) the merges no branch names.
        let unnamed = Option::filter(Some(None), |_: &Option<&String>| !std::vec::Vec::is_empty(&unnamed_merges));
        let entries: Vec<Option<&String>> =
            Iterator::collect(Iterator::chain(Iterator::map(BTreeSet::iter(&names), Some), unnamed));
        for entry in entries {
            if cancelled(&cancel) {
                return Err(CANCELLED.into());
            }
            let mut ranges = std::vec::Vec::new();
            let Some(name) = entry else {
                for sha in &unnamed_merges {
                    let m = Result::map_err(git2::Repository::find_commit(repo, *sha), |e| ToString::to_string(&e))?;
                    if let (Ok(base), Ok(head)) = (git2::Commit::parent_id(&m, 0), git2::Commit::parent_id(&m, 1)) {
                        std::vec::Vec::push(&mut ranges, (base, head));
                    }
                }
                read_ranges(repo, ranges, &mut line_ranges, &mut commits, &mut note_commits, &mut out, false, true)?;
                continue;
            };
            let local = repo
                .find_reference(&format!("refs/heads/{name}"))
                .and_then(|r| r.peel_to_commit());
            let (head, availability) = match local {
                Ok(head) => (Some(head), "local"),
                Err(_) => match repo
                    .find_reference(&format!("refs/remotes/origin/{name}"))
                    .and_then(|r| r.peel_to_commit())
                {
                    Ok(head) => (Some(head), "remote"),
                    Err(_) => (None, "missing"),
                },
            };
            let history = merges.get(name);
            let mut row = json!({"name":name,"basis":if exact_names.contains(name){"exact"}else{"associated"},"prs":[],"availability":availability,"mergedSha":null});
            row["pr"]=pr_metadata(Some(repo),None,None);
            row["pr"]["unavailable"]=json!("not merged; open PR lookup needs GitHub");
            if let Some(history) = history {
                for sha in history {
                    let m = repo.find_commit(*sha).map_err(|e| e.to_string())?;
                    if let (Ok(base), Ok(head)) = (
                        git2::Commit::parent_id(&m, 0),
                        git2::Commit::parent_id(&m, 1),
                    ) {
                        ranges.push((base, head));
                    }
                }
            }
            // The branch's own commits beyond the default branch: when the branch never merged
            // and other PRs of the story did, they are side work, not what landed.
            let mut side_ranges = std::vec::Vec::new();
            let mut side = Option::is_none(&history) && !squash_seen;
            if let Some(head) = head {
                if let Some(base) = default_branch(repo) {
                    if side && !reached_root {
                        let fork = Result::ok(git2::Repository::merge_base(repo, base, git2::Commit::id(&head)));
                        side = Option::is_some_and(fork, |fork| BTreeSet::contains(&scanned, &fork));
                    }
                    std::vec::Vec::push(&mut side_ranges, (base, git2::Commit::id(&head)));
                } else {
                    row["unavailable"] = json!(history::NO_DEFAULT_BRANCH);
                }
            } else if history.is_none() {
                row["unavailable"] = json!("branch not found locally");
            }
            let rank = match availability {
                "local" => 2,
                "remote" => 1,
                _ => 0,
            };
            match branch_rows.entry(name.clone()) {
                std::collections::btree_map::Entry::Vacant(e) => {
                    e.insert((rank, row));
                }
                std::collections::btree_map::Entry::Occupied(mut e) => {
                    if rank > e.get().0 {
                        e.insert((rank, row));
                    }
                }
            }
            read_ranges(repo, ranges, &mut line_ranges, &mut commits, &mut note_commits, &mut out, false, true)?;
            let hit = read_ranges(repo, side_ranges, &mut line_ranges, &mut commits, &mut note_commits, &mut out, side, false)?;
            // The branch's own reason, beside the story-level `unavailable.branches`.
            if let (true, Some(reason), Some((_, row))) = (side, hit, std::collections::BTreeMap::get_mut(&mut branch_rows, name)) {
                if Value::is_null(&row["unavailable"]) {
                    row["unavailable"] = json!(reason);
                }
            }
        }
        if std::collections::BTreeSet::is_empty(&note_commits) {
            continue;
        }
        for sha in &note_commits {
            out.repo_paths
                .entry(sha.clone())
                .or_default()
                .push(root.to_owned());
        }
        crate::attribution::git_ai::GitAiSource::for_home(home).drain(root);
        let bound_key = attachments
            .iter()
            .find_map(|set| {
                std::result::Result::ok(crate::context_repo::attachments_io::resolve_by_repo_path(
                    &set.attachments,
                    &root.to_string_lossy(),
                ))
                .flatten()
                .and_then(|i| crate::commands::attachments::attachment_key(&set.attachments[i]))
            })
            .or_else(|| {
                attachments
                    .iter()
                    .flat_map(|set| &set.attachments)
                    .find_map(|row| {
                        crate::commands::context_git_watch::GitDirs::resolve(Path::new(
                            &row.repo_path,
                        ))
                        .filter(|dirs| &dirs.common_dir == common)
                        .and_then(|_| crate::commands::attachments::attachment_key(row))
                    })
            });
        if let Some(key) = crate::attribution::checkpoints::repository_key(
            root,
            bound_key.as_deref(),
            &mut checkpoint_store,
        )? {
            crate::attribution::checkpoints::harvest(
                root,
                &key,
                &mut checkpoint_store,
                &note_commits,
            )?;
        }
        repository_notes.push((root.to_owned(), note_commits));
    }
    // Resolve notes only after every clone has contributed checkpoint evidence.
    for (root, note_commits) in repository_notes {
        let notes = read_notes(home, &root, &note_commits, &checkpoint_store);
        for (sha, (ids, reason)) in notes {
            if let Some(commit) = commits.get_mut(&sha) {
                // Every clone contributes note evidence, independently of file deduplication.
                // A successful exact observation wins over absent or unavailable notes.
                if !std::vec::Vec::is_empty(&ids) {
                    let mut merged: BTreeSet<String> =
                        std::iter::FromIterator::from_iter(std::mem::take(&mut commit.session_ids));
                    std::iter::Extend::extend(&mut merged, ids);
                    commit.session_ids = std::iter::FromIterator::from_iter(merged);
                    commit.basis = "exact";
                    commit.unavailable = None;
                } else if commit.basis != "exact" {
                    // Successful absence wins over a failed read in another clone.
                    if reason.is_none() || commit.basis == "unknown" {
                        commit.basis = if reason.is_some() {
                            "unknown"
                        } else {
                            "associated"
                        };
                        commit.unavailable = reason;
                    }
                }
            }
        }
    }
    // Landed evidence wins: lines changed by this story's merged PRs, once per SHA.
    // Keep commit-history availability independent from the chosen line-count basis.
    let mut selected = Vec::new();
    let landed = !std::collections::BTreeMap::is_empty(&landed_ranges);
    out.landed_lines = landed;
    if landed {
        for (head, candidates) in landed_ranges {
            let observed = first_range(candidates, head);
            if let (Ok(rows), Some((_, pr))) = (&observed, pr_rows.get_mut(&head.to_string())) {
                pr["files"] = json!(rows.keys().collect::<Vec<_>>());
            }
            selected.push(observed);
        }
    } else {
        let ancestor = |older, newer| older == newer || repositories.values().any(|repo| repo.graph_descendant_of(newer, older).unwrap_or(false));
        let ranges = line_ranges.keys().copied().collect::<Vec<_>>();
        let retained = ranges.iter().filter(|(base, head)| !ranges.iter().any(|(other_base, other_head)| {
            head != other_head && ancestor(*head, *other_head) && ancestor(*other_base, *base)
        })).copied().collect::<Vec<_>>();
        if retained.len() > 1 {
            out.unavailable.insert("fileLines", "story spans branches with no single final tree");
        } else {
            for (base, head) in retained {
                selected.push(first_range(line_ranges[&(base, head)].iter().map(|repo| (*repo, base)), head));
            }
            if let Some(reason) = out.unavailable.get("commits").copied() {
                out.unavailable.insert("fileLines", reason);
            }
        }
    }
    for observation in selected {
        match observation {
            Ok(rows) => add_line_rows(&mut out.lines, rows),
            Err(reason) => { out.unavailable.insert("fileLines", reason); }
        }
    }
    for name in names {
        branch_rows.entry(name.clone()).or_insert((0,json!({"name":name,"basis":if exact_names.contains(&name){"exact"}else{"associated"},"prs":[],"availability":"missing","pr":{"number":null,"url":null,"urlReason":"PR number not available","mergedAt":null,"mergedAtReason":"not merged","unavailable":"not merged; open PR lookup needs GitHub"},"mergedSha":null,"unavailable":"branch not found locally"})));
    }
    if std::collections::BTreeMap::is_empty(&repositories) && !work_missing {
        out.unavailable
            .insert("commits", crate::attribution::ranges::REPO_NOT_FOUND);
    }
    out.branches = branch_rows.into_values().map(|(_, row)| row).collect();
    let mut prs = pr_rows.into_values().collect::<Vec<_>>();
    prs.sort_by(|(at_a, a), (at_b, b)| {
        at_b.cmp(at_a).then_with(|| {
            serde_json::Value::as_str(&a["mergedSha"])
                .cmp(&serde_json::Value::as_str(&b["mergedSha"]))
        })
    });
    out.prs = prs.into_iter().map(|(_, row)| row).collect();
    for branch in &mut out.branches {
        let history = out
            .prs
            .iter()
            .filter(|pr| pr["branch"] == branch["name"])
            .collect::<Vec<_>>();
        let numbers = Iterator::map(<[&Value]>::iter(&history), |pr| &pr["number"]);
        branch["prs"] = json!(Iterator::collect::<Vec<_>>(Iterator::filter(numbers, |number| !Value::is_null(number))));
        if let Some(latest) = history.first() {
            branch["pr"] = json!({"number":latest["number"],"url":latest["url"],"urlReason":latest["urlReason"],"mergedAt":latest["mergedAt"],"mergedAtReason":latest["mergedAtReason"]});
            if Value::is_null(&latest["number"]) {
                branch["pr"]["numberReason"] = Value::clone(&latest["numberReason"]);
            }
            branch["mergedSha"] = latest["mergedSha"].clone();
            if branch["unavailable"] == "branch not found locally" {
                serde_json::Map::remove(
                    serde_json::Value::as_object_mut(branch).unwrap(),
                    "unavailable",
                );
            }
        }
    }
    if out
        .branches
        .iter()
        .any(|row| row["unavailable"].is_string())
    {
        out.unavailable
            .entry("branches")
            .or_insert("one or more story branches have unavailable commits");
    }
    // With merged PRs an unmerged branch is reported on its own and the landed commits still
    // count; with none, every branch is the story.
    if !landed {
        if let Some(reason) = std::collections::BTreeMap::remove(&mut out.unavailable, "branches") {
            std::collections::btree_map::Entry::or_insert(
                std::collections::BTreeMap::entry(&mut out.unavailable, "commits"),
                reason,
            );
        }
    }
    if !landed {
        if let Some(reason) = out.unavailable.get("commits").copied() {
            out.unavailable.entry("fileLines").or_insert(reason);
        }
    }
    out.commits = commits.into_values().collect();
    if std::iter::Iterator::any(&mut out.commits.iter(), |c| c.basis == "unknown") {
        out.unavailable
            .insert("attribution", "git-ai note attribution unavailable locally");
    }
    out.commits.sort_by(|a, b| {
        b.authored_at
            .cmp(&a.authored_at)
            .then_with(|| a.sha.cmp(&b.sha))
    });
    Ok(out)
}

/// Read each `(base, head)` range's commits into `commits` (once per sha) and its line range
/// into `line_ranges`, noting every sha for the notes read.
fn read_ranges<'r>(
    repo: &'r git2::Repository,
    ranges: Vec<(git2::Oid, git2::Oid)>,
    line_ranges: &mut BTreeMap<(git2::Oid, git2::Oid), Vec<&'r git2::Repository>>,
    commits: &mut BTreeMap<String, Commit>,
    note_commits: &mut BTreeSet<String>,
    out: &mut GitFacts,
    side: bool,
    landed: bool,
) -> Result<Option<&'static str>, String> {
    // A side branch's own gaps are reported apart; with nothing landed they are the story's.
    let key = if side { "branches" } else { "commits" };
    let mut hit = None;
    for (base, head) in ranges {
        let from = Result::unwrap_or(git2::Repository::merge_base(repo, base, head), base);
        let entry = std::collections::BTreeMap::entry(line_ranges, (from, head));
        std::vec::Vec::push(std::collections::btree_map::Entry::or_default(entry), repo);
        let mut walk = new_sorted_revwalk(repo)?;
        let mut read_range = || -> Result<(), HistoryError> {
            git2::Revwalk::push(&mut walk, head)?;
            git2::Revwalk::hide(&mut walk, base)?;
            for id in &mut walk {
                let commit = git2::Repository::find_commit(repo, id?)?;
                if is_boundary(&commit) {
                    std::collections::BTreeMap::insert(&mut out.unavailable, key, HISTORY_PAST_SHALLOW);
                    hit = Some(HISTORY_PAST_SHALLOW);
                }
                let sha = ToString::to_string(&git2::Commit::id(&commit));
                BTreeSet::insert(note_commits, std::string::String::clone(&sha));
                if let Some(known) = std::collections::BTreeMap::get_mut(commits, &sha) {
                    known.unlanded &= side;
                    known.landed |= landed;
                    continue;
                }
                // A merge adds no files of its own: diffed against its first parent, a merge of main
                // into the branch would list everything main changed.
                let files = if crate::commands::git_sync::commit_recorded_parent_count(&commit) > 1 {
                    std::vec::Vec::new()
                } else {
                    commit_changed_paths_local(repo, &commit)?
                };
                let author = git2::Commit::author(&commit);
                let row = Commit {
                    ranges: None,
                    ranges_basis: None,
                    ranges_reason: None,
                    sha: std::string::String::clone(&sha),
                    subject: std::string::String::from(Option::unwrap_or(Option::flatten(Result::ok(git2::Commit::summary(&commit))), "")),
                    authored_at: git2::Time::seconds(&git2::Signature::when(&author)) * 1000,
                    author: Option::map(Result::ok(git2::Signature::name(&author)), str::to_owned),
                    files,
                    basis: "unknown",
                    session_ids: std::vec::Vec::new(),
                    unavailable: None,
                    unlanded: side,
                    landed,
                };
                std::collections::BTreeMap::insert(commits, sha, row);
            }
            Ok(())
        };
        if let Err(error) = read_range() {
            match error {
                HistoryError::ObjectsNotLocal => {
                    std::collections::BTreeMap::insert(&mut out.unavailable, key, HISTORY_NOT_LOCAL);
                    hit = Some(HISTORY_NOT_LOCAL);
                }
                HistoryError::Other(error) => return Err(error),
            }
        }
    }
    Ok(hit)
}

/// Reuse the attribution reader's managed drain, local-only runner and one shared retry loop.
fn read_notes(
    home: &Path,
    repo: &Path,
    commits: &BTreeSet<String>,
    store: &UsageStore,
) -> BTreeMap<String, (Vec<String>, Option<&'static str>)> {
    if std::collections::BTreeSet::is_empty(commits) {
        return std::collections::BTreeMap::new();
    }
    let source = crate::attribution::git_ai::GitAiSource::for_home(home);
    let result = crate::attribution::why_line::read_notes_snapshot(repo, |_| {
        Ok(commits
            .iter()
            .map(|sha| {
                let value = match source.note(repo, sha) {
                    Ok(note) => {
                        let ids = note
                            .map(|mut n| {
                                n.normalize_sessions(store)?;
                                Ok::<_, String>(n.session_ids())
                            })
                            .transpose();
                        match ids {
                            Ok(ids) => (ids.unwrap_or_default(), None),
                            Err(_) => (Vec::new(), Some("checkpoint identities unavailable")),
                        }
                    }
                    Err(crate::attribution::ranges::AttributionError::ObjectsNotLocal) => (
                        Vec::new(),
                        Some(crate::attribution::ranges::OBJECTS_NOT_LOCAL),
                    ),
                    Err(_) => (Vec::new(), Some("git-ai note unavailable locally")),
                };
                (sha.clone(), value)
            })
            .collect())
    });
    match result {
        Ok((notes, _)) => notes,
        other => {
            let reason = match other {
                Err(error) if error == crate::attribution::why_line::NOTES_CHANGED => {
                    crate::attribution::why_line::NOTES_CHANGED
                }
                Err(error) if error == crate::attribution::ranges::OBJECTS_NOT_LOCAL => {
                    crate::attribution::ranges::OBJECTS_NOT_LOCAL
                }
                _ => "git-ai note unavailable locally",
            };
            commits
                .iter()
                .map(|sha| (sha.clone(), (Vec::new(), Some(reason))))
                .collect()
        }
    }
}

/// The prefix of every commit cursor for `story` over this complete ordered commit set.
pub(super) fn commit_cursor_key<'s>(story: &str, shas: impl Iterator<Item = &'s str>) -> String {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&str::to_lowercase(str::trim(story)), &mut hash);
    for sha in shas {
        std::hash::Hash::hash(sha, &mut hash);
    }
    format!("sc:{:x}:", std::hash::Hasher::finish(&hash))
}

/// Bind every continuation to both the query and the complete ordered commit set.
pub(super) fn page(
    params: &Params,
    commits: &[Commit],
) -> Result<(Vec<Commit>, Option<String>), String> {
    let key = commit_cursor_key(&params.story_id, Iterator::map(<[Commit]>::iter(commits), |c| std::string::String::as_str(&c.sha)));
    let start = match params.cursor.as_deref() {
        None => 0,
        Some(cursor) => {
            let after = Option::and_then(str::strip_prefix(cursor, std::string::String::as_str(&key)), |sha| {
                Iterator::position(&mut <[Commit]>::iter(commits), |c| c.sha == sha)
            });
            Option::ok_or(Option::map(after, |n| n + 1), STALE_CURSOR)?
        }
    };
    let end = (start + params.limit.unwrap_or(100).clamp(1, 500)).min(commits.len());
    let next = (end < commits.len()).then(|| format!("{key}{}", commits[end - 1].sha));
    Ok((commits[start..end].to_vec(), next))
}

#[cfg(test)]
mod line_tests {
    use super::*;
    #[test]
    fn a_files_first_unknown_reason_survives_later_ranges() {
        let counted = |added| BTreeMap::from([("asset.dat".to_owned(), LineStats { lines: Some(LineCounts { added, removed: 0 }), lines_reason: None })]);
        let binary = || BTreeMap::from([("asset.dat".to_owned(), LineStats::unavailable("binary"))]);
        for order in [[binary(), counted(2)], [counted(2), binary()]] {
            let mut totals = BTreeMap::new();
            for rows in order { add_line_rows(&mut totals, rows); }
            assert_eq!(totals["asset.dat"], LineStats::unavailable("binary"));
        }
        let mut totals = BTreeMap::new();
        add_line_rows(&mut totals, counted(2));
        add_line_rows(&mut totals, counted(3));
        assert_eq!(totals["asset.dat"].lines, Some(LineCounts { added: 5, removed: 0 }));
    }
}
