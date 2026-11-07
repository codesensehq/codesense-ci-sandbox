// SPDX-License-Identifier: MIT
//! The attached source repo's history, read where it already is (its git common dir), at
//! revisions that never come from what the checkout has checked out.
//!
//! The only network use is [`HistorySource::fetch`], at the points callers choose: attach, a
//! story trail whose history is missing, and the `refresh_history` verb. A fetch writes only
//! `refs/remotes/*`, objects and `shallow`; never the working tree, index, `HEAD`, local
//! branches, tags or notes.
use super::ranges::{HISTORY_PAST_SHALLOW, NO_LOCAL_COPY};
use crate::install::worktree_index::story_id_from_branch;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

/// First-parent commits of the default branch a story walk reads.
pub(crate) const SCAN_LIMIT: usize = 2_000;
/// How long a story trail may wait on the fetch it makes when its history is missing.
pub(crate) const TRAIL_FETCH_BOUND: Duration = Duration::from_secs(10);
/// How long the fetch after attaching a checkout may take.
pub(crate) const ATTACH_FETCH_BOUND: Duration = Duration::from_secs(60);
const REFRESH_DEFAULT: Duration = Duration::from_secs(60);
const REFRESH_MAX: Duration = Duration::from_secs(300);

pub(crate) const FETCH_NEEDS_CREDENTIALS: &str = "fetch needs credentials";
pub(crate) const FETCH_REPO_BUSY: &str = "fetch skipped, repo busy";
pub(crate) const FETCH_TIMED_OUT: &str = "fetch timed out";
pub(crate) const FETCH_FAILED: &str = "fetch failed";
/// A trail fetch already ran at origin's current head and still found no merge.
pub(crate) const FETCH_SPENT: &str = "already fetched at this origin head; call refresh_history";
pub(crate) const NO_DEFAULT_BRANCH: &str =
    "origin default branch not known locally; call refresh_history";

/// Remote branches only, mapped under `refs/remotes/origin/`.
const REFSPEC: &str = "+refs/heads/*:refs/remotes/origin/*";
/// Deepen for a cut-off merge range, whose missing depth is not known.
const FIRST_DEEPEN: u32 = 64;
/// Deepens before a copy that still misses the story's history fetches the rest at once.
const MAX_DEEPENS: usize = 4;
const REFRESH_MIN: Duration = Duration::from_secs(1);
/// Fetch-record keys of remembered trail misses: `<prefix><common dir>\n<STORY>` → origin head.
const TRAIL_MISS: &str = "trailMiss:";
/// How long a git that was asked to stop may take before it is killed.
const STOP_GRACE: Duration = Duration::from_millis(300);

/// One attached checkout's history: its objects and refs, through its common dir.
#[derive(Debug, Clone)]
pub(crate) struct HistorySource {
    checkout: PathBuf,
    common_dir: PathBuf,
}

/// What a fetch did beyond the first fetch, which always landed.
#[derive(Debug, Default)]
pub(crate) struct Fetched {
    /// Total `--deepen` depth, or `"unshallow"`.
    pub deepened: Option<Value>,
    /// Why a later deepen failed; the first fetch (and `fetchedAt`) still landed.
    pub partial: Option<&'static str>,
}

/// The revision a trail read uses when the caller names none, in the order it is chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DefaultRev {
    /// The story merged: its newest merge on origin's default branch.
    Merged(git2::Oid),
    /// The story's branch head, local else `refs/remotes/origin/<branch>`.
    Branch(git2::Oid),
    /// Origin's default branch.
    DefaultBranch(git2::Oid),
}

impl DefaultRev {
    pub(crate) fn oid(&self) -> git2::Oid {
        match self {
            Self::Merged(oid) | Self::Branch(oid) | Self::DefaultBranch(oid) => *oid,
        }
    }
    pub(crate) fn source(&self) -> &'static str {
        match self {
            Self::Merged(_) => "mergedSha",
            Self::Branch(_) => "branch",
            Self::DefaultBranch(_) => "defaultBranch",
        }
    }
}

/// A story's merges found on origin's default branch, newest first.
#[derive(Debug, Default)]
pub(crate) struct MergeWalk {
    pub merges: Vec<git2::Oid>,
    /// The walk stopped at the shallow boundary, so older merges may be missing.
    pub past_boundary: bool,
    /// First-parent commits read, the boundary included.
    pub walked: usize,
}

/// A commit whose parents the shallow file cut off: libgit2 shows it as a root.
pub(crate) fn is_boundary(commit: &git2::Commit<'_>) -> bool {
    commit.parent_count() == 0
        && crate::commands::git_sync::commit_recorded_parent_count(commit) > 0
}

/// The PR number and branch of a merge or squash subject that lands `story` (or a branch in
/// `names`): `Merge pull request #N from owner/branch`, or `STORY: title (#N)` (no branch).
pub(crate) fn story_merge_subject<'s>(
    subject: &'s str,
    story: &str,
    names: &BTreeSet<String>,
) -> Option<(u64, Option<&'s str>)> {
    if let Some((number, from)) = subject
        .strip_prefix("Merge pull request #")
        .and_then(|rest| rest.split_once(" from "))
    {
        let (number, (_, branch)) = (
            Result::ok(str::parse::<u64>(number))?,
            from.split_once('/')?,
        );
        let ours = names.contains(branch)
            || story_id_from_branch(branch).is_some_and(|id| id.eq_ignore_ascii_case(story));
        return ours.then_some((number, Some(branch)));
    }
    let (id, _) = subject.split_once(':')?;
    if !id.eq_ignore_ascii_case(story) {
        return None;
    }
    let number = Result::ok(str::parse(subject.rsplit_once(" (#")?.1.strip_suffix(')')?))?;
    Some((number, None))
}

/// The commits a story merge's second parent may be, each with its branch: the live heads
/// (local, else `refs/remotes/origin/`) of the story's own branches (by convention, or named
/// in its recorded tips) and every recorded tip (`install::story_tips`). Never another
/// story's branch or a branch with no story id, such as `develop`.
#[derive(Debug, Default)]
pub(crate) struct StoryTips(std::collections::BTreeMap<git2::Oid, (String, bool)>);

/// Why a merge whose second parent is a story tip is not linked: another story's branch has
/// the same head, so the graph cannot say whose merge it is.
pub(crate) const TIP_SHARED: &str =
    "a merge's second parent is also another story's branch head; not linked by its tip";
/// A merge linked by its second parent whose subject names no PR number and no local PR
/// head ref is at that parent.
pub(crate) const NO_PR_NUMBER: &str =
    "merge subject names no PR number and no local PR head ref matches";
/// More than one local PR head ref is at the merge's second parent.
pub(crate) const AMBIGUOUS_PR_REFS: &str = "ambiguous PR refs";

fn is_story_branch(branch: &str, story: &str) -> bool {
    Option::is_some_and(story_id_from_branch(branch), |id| {
        str::eq_ignore_ascii_case(&id, story)
    })
}

impl StoryTips {
    /// `story`'s tips in `repo`: the live heads of `names` that are the story's branches,
    /// plus its recorded tips in `home`. A live head wins over a recorded tip at the same
    /// commit.
    pub(crate) fn for_story(
        home: &Path,
        repo: &git2::Repository,
        story: &str,
        names: &BTreeSet<String>,
    ) -> Self {
        // Each tip's branch, and whether it was recorded (a sighting) rather than only a
        // live head found by name.
        let mut tips: std::collections::BTreeMap<git2::Oid, (String, bool)> =
            std::collections::BTreeMap::new();
        let mut branches: BTreeSet<String> = BTreeSet::new();
        for tip in crate::install::story_tips::read(home, story) {
            if let Ok(oid) = git2::Oid::from_str(&tip.sha) {
                BTreeSet::insert(&mut branches, std::string::String::clone(&tip.branch));
                std::collections::BTreeMap::insert(&mut tips, oid, (tip.branch, true));
            }
        }
        for name in Iterator::chain(
            BTreeSet::iter(names),
            BTreeSet::iter(&story_branch_names(repo, story)),
        ) {
            BTreeSet::insert(&mut branches, std::string::String::clone(name));
        }
        for name in &branches {
            if !is_story_branch(name, story) {
                continue;
            }
            for reference in [
                format!("refs/heads/{name}"),
                format!("refs/remotes/origin/{name}"),
            ] {
                if let Ok(oid) = git2::Repository::refname_to_id(repo, &reference) {
                    let recorded = Option::is_some_and(
                        std::collections::BTreeMap::get(&tips, &oid),
                        |(_, recorded)| *recorded,
                    );
                    std::collections::BTreeMap::insert(
                        &mut tips,
                        oid,
                        (std::string::String::clone(name), recorded),
                    );
                }
            }
        }
        Self(tips)
    }
}

/// How a first-parent merge on origin's default branch lands a story.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MergeLink {
    /// GitHub's `Merge pull request #N` or a trailing `(#N)` in the subject, else the one
    /// local PR head ref (`refs/remotes/pr/N`, `refs/pull/N/head`) at the second parent.
    pub number: Option<u64>,
    /// Why `number` is null; set whenever it is.
    pub number_reason: Option<&'static str>,
    /// The story branch it merged; `None` for a squash, or a two-parent merge under the
    /// story's squash-style subject whose second parent is no tip.
    pub branch: Option<String>,
    /// The second parent is a live or recorded tip of `branch`.
    pub by_tip: bool,
    /// That tip was recorded for the story (a sighting of its worktree), not only found as a
    /// live head by name.
    pub tip_recorded: bool,
    /// A merge commit (two parents): its commits are `M^1..M^2`. `false` for a squash.
    pub merge: bool,
}

/// The branch a merge subject names, in git's and GitHub's default forms:
/// `Merge pull request #N from <owner>/<branch>`, `Merge branch '<branch>'` and
/// `Merge remote-tracking branch 'origin/<branch>'`.
pub(crate) fn subject_branch(subject: &str) -> Option<&str> {
    if let Some(rest) = str::strip_prefix(subject, "Merge pull request #") {
        let digits = Iterator::count(Iterator::take_while(str::chars(rest), char::is_ascii_digit));
        let from = str::strip_prefix(&rest[digits..], " from ")?;
        let (_, branch) = str::split_once(from, '/')?;
        return Iterator::next(&mut str::split_whitespace(branch));
    }
    let quoted = |prefix: &str| {
        let rest = str::strip_prefix(subject, prefix)?;
        Option::map(str::split_once(rest, '\''), |(branch, _)| branch)
    };
    Option::or_else(quoted("Merge branch '"), || {
        let branch = quoted("Merge remote-tracking branch '")?;
        Some(Option::unwrap_or(
            str::strip_prefix(branch, "origin/"),
            branch,
        ))
    })
}

/// The PR number a subject names: `Merge pull request #N …`, or a trailing `(#N)`.
fn subject_pr_number(subject: &str) -> Option<u64> {
    if let Some(rest) = str::strip_prefix(subject, "Merge pull request #") {
        let digits: String =
            Iterator::collect(Iterator::take_while(str::chars(rest), char::is_ascii_digit));
        return Result::ok(str::parse(&digits));
    }
    let (_, number) = str::rsplit_once(str::trim_end(subject), " (#")?;
    Result::ok(str::parse(str::strip_suffix(number, ')')?))
}

/// The PR whose local head ref is `oid`: the number, or why there is none.
fn local_pr_head(repo: &git2::Repository, oid: git2::Oid) -> Result<u64, &'static str> {
    let mut found: BTreeSet<u64> = BTreeSet::new();
    if let Ok(references) = git2::Repository::references(repo) {
        for reference in Iterator::flatten(references) {
            let Ok(name) = git2::Reference::name(&reference) else {
                continue;
            };
            let number = Option::or_else(str::strip_prefix(name, "refs/remotes/pr/"), || {
                Option::and_then(str::strip_prefix(name, "refs/pull/"), |rest| {
                    str::strip_suffix(rest, "/head")
                })
            });
            let Some(number) = Option::and_then(number, |n| Result::ok(str::parse::<u64>(n)))
            else {
                continue;
            };
            if git2::Reference::target(&reference) == Some(oid) {
                BTreeSet::insert(&mut found, number);
            }
        }
    }
    match BTreeSet::len(&found) {
        0 => Err(NO_PR_NUMBER),
        1 => Ok(Option::unwrap_or_default(<Option<&u64>>::copied(
            BTreeSet::first(&found),
        ))),
        _ => Err(AMBIGUOUS_PR_REFS),
    }
}

/// Another story's branch, local or on origin, whose head is `oid`.
fn other_story_head(repo: &git2::Repository, oid: git2::Oid, story: &str) -> bool {
    let Ok(branches) = git2::Repository::branches(repo, None) else {
        return false;
    };
    Iterator::any(&mut Iterator::flatten(branches), |(branch, kind)| {
        let name = Option::flatten(Result::ok(git2::Branch::name(&branch)));
        let at = git2::Reference::target(git2::Branch::get(&branch)) == Some(oid);
        // Local branches and origin's only: a PR head ref (`refs/remotes/pr/9`) is no branch.
        let name = match kind {
            git2::BranchType::Local => name,
            git2::BranchType::Remote => {
                Option::and_then(name, |name| str::strip_prefix(name, "origin/"))
            }
        };
        at && Option::is_some_and(name, |name| {
            Option::is_some_and(story_id_from_branch(name), |id| {
                !str::eq_ignore_ascii_case(&id, story)
            })
        })
    })
}

/// Whether `merge` lands `story`, the one rule every merge walk uses:
/// - its subject names the story branch (GitHub's default subject), or is the story's
///   squash-style subject (`STORY: title (#N)`): a two-parent one is a merge, `M^1..M^2`;
/// - else, when the subject names no other branch, its second parent is one of `tips`,
///   not already on the first parent's line and not also another story's branch head
///   (then `Err(TIP_SHARED)`: unlinked, with the reason).
pub(crate) fn story_merge(
    repo: &git2::Repository,
    merge: &git2::Commit<'_>,
    story: &str,
    names: &BTreeSet<String>,
    tips: &StoryTips,
) -> Option<Result<MergeLink, &'static str>> {
    let subject = Option::unwrap_or(Option::flatten(Result::ok(git2::Commit::summary(merge))), "");
    let two_parents = git2::Commit::parent_count(merge) >= 2;
    let second = Option::filter(Result::ok(git2::Commit::parent_id(merge, 1)), |_| {
        two_parents
    });
    let found_tip = Option::and_then(second, |oid| std::collections::BTreeMap::get(&tips.0, &oid));
    let tip = Option::map(found_tip, |(branch, _)| branch);
    let tip_recorded = Option::is_some_and(found_tip, |(_, recorded)| *recorded);
    let number = |found: Option<u64>| match found {
        Some(number) => (Some(number), None),
        None => match Option::map(second, |oid| local_pr_head(repo, oid)) {
            Some(Ok(number)) => (Some(number), None),
            Some(Err(reason)) => (None, Some(reason)),
            None => (None, Some(NO_PR_NUMBER)),
        },
    };
    if let Some((found, branch)) = story_merge_subject(subject, story, names) {
        let branch = match branch {
            Some(branch) => Some(str::to_owned(branch)),
            None if two_parents => <Option<&String>>::cloned(tip),
            None => None,
        };
        let by_tip = Option::is_some_and(tip, |tip| Some(tip) == Option::as_ref(&branch));
        let (number, number_reason) = number(Some(found));
        return Some(Ok(MergeLink {
            number,
            number_reason,
            branch,
            by_tip,
            tip_recorded: by_tip && tip_recorded,
            merge: two_parents,
        }));
    }
    // A subject that names another story's (or no story's) branch wins over the tip.
    if let Some(named) = subject_branch(subject) {
        if !BTreeSet::contains(names, named) && !is_story_branch(named, story) {
            return None;
        }
    }
    let (second, tip) = (second?, tip?);
    let first = Result::ok(git2::Commit::parent_id(merge, 0))?;
    let on_first_line = second == first
        || Result::unwrap_or(
            git2::Repository::graph_descendant_of(repo, first, second),
            false,
        );
    if on_first_line {
        return None;
    }
    if other_story_head(repo, second, story) {
        return Some(Err(TIP_SHARED));
    }
    let (number, number_reason) = number(subject_pr_number(subject));
    Some(Ok(MergeLink {
        number,
        number_reason,
        branch: Some(std::string::String::clone(tip)),
        by_tip: true,
        tip_recorded,
        merge: true,
    }))
}

/// Origin's default branch: what `refs/remotes/origin/HEAD` names (a fetch records it when it
/// is missing), else whichever of `origin/main` and `origin/master` exists. Never a name
/// assumed for the repo.
pub(crate) fn default_branch(repo: &git2::Repository) -> Option<git2::Oid> {
    Option::map(default_branch_ref(repo), |(_, oid)| oid)
}

/// [`default_branch`] with the remote-tracking ref it resolved, e.g.
/// `refs/remotes/origin/release` (what `origin/HEAD` names, not `origin/HEAD` itself).
pub(crate) fn default_branch_ref(repo: &git2::Repository) -> Option<(String, git2::Oid)> {
    let names = [
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
        "refs/remotes/origin/master",
    ];
    Iterator::find_map(&mut <[&str]>::iter(&names), |name| {
        let found = Result::ok(git2::Repository::find_reference(repo, name))?;
        let reference = Result::ok(git2::Reference::resolve(&found))?;
        let oid = git2::Commit::id(&Result::ok(git2::Reference::peel_to_commit(&reference))?);
        Some((str::to_owned(Result::ok(git2::Reference::name(&reference))?), oid))
    })
}

/// The branch name of [`default_branch_ref`] on origin, e.g. `release`.
pub(crate) fn default_branch_name(refname: &str) -> &str {
    Option::unwrap_or(str::strip_prefix(refname, "refs/remotes/origin/"), refname)
}

/// The names of the story's branches by convention, local and on origin.
fn story_branch_names(repo: &git2::Repository, story: &str) -> BTreeSet<String> {
    let Ok(branches) = git2::Repository::branches(repo, None) else {
        return BTreeSet::new();
    };
    let mut names = BTreeSet::new();
    for (branch, kind) in Iterator::flatten(branches) {
        let Some(name) = Option::flatten(Result::ok(git2::Branch::name(&branch))) else {
            continue;
        };
        let name = match kind {
            git2::BranchType::Remote => match str::strip_prefix(name, "origin/") {
                Some(name) => name,
                None => continue,
            },
            git2::BranchType::Local => name,
        };
        if is_story_branch(name, story) {
            BTreeSet::insert(&mut names, str::to_owned(name));
        }
    }
    names
}

/// The story's branch heads, local first, then `refs/remotes/origin/`; newest commit first.
fn story_branch_head(repo: &git2::Repository, story: &str) -> Option<git2::Oid> {
    for kind in [git2::BranchType::Local, git2::BranchType::Remote] {
        let mut heads: Vec<_> = Result::ok(repo.branches(Some(kind)))?
            .flatten()
            .filter_map(|(branch, _)| {
                let name = Result::ok(git2::Branch::name(&branch))??;
                let name = if kind == git2::BranchType::Remote {
                    name.strip_prefix("origin/")?
                } else {
                    name
                };
                story_id_from_branch(name)
                    .filter(|id| id.eq_ignore_ascii_case(story))
                    .and_then(|_| Result::ok(branch.get().peel_to_commit()))
            })
            .collect();
        heads.sort_by_key(|commit| std::cmp::Reverse(commit.time().seconds()));
        if let Some(head) = heads.first() {
            return Some(git2::Commit::id(head));
        }
    }
    None
}

impl HistorySource {
    /// The history of the checkout at `checkout`; `None` when it is not a git repo.
    pub(crate) fn at(checkout: &Path) -> Option<Self> {
        let dirs = crate::commands::context_git_watch::GitDirs::resolve(checkout)?;
        Some(Self {
            checkout: checkout.to_owned(),
            common_dir: dirs.common_dir,
        })
    }

    pub(crate) fn open(&self) -> Result<git2::Repository, String> {
        git2::Repository::open(&self.checkout).map_err(|_| NO_LOCAL_COPY.to_owned())
    }

    /// The commits the shallow file names (empty for a complete history).
    pub(crate) fn boundary(&self) -> BTreeSet<String> {
        std::fs::read_to_string(self.common_dir.join("shallow"))
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|line| !str::is_empty(line))
            .map(str::to_owned)
            .collect()
    }

    pub(crate) fn is_shallow(&self) -> bool {
        !std::collections::BTreeSet::is_empty(&self.boundary())
    }

    /// First-parent walk of origin's default branch for `story`'s merges ([`story_merge`],
    /// against [`StoryTips::for_story`], the same tips the trail uses), at most
    /// [`SCAN_LIMIT`] commits; `None` when there is no default branch locally.
    pub(crate) fn story_merges(
        home: &Path,
        repo: &git2::Repository,
        story: &str,
        names: &BTreeSet<String>,
    ) -> Option<MergeWalk> {
        let mut tip = Result::ok(repo.find_commit(default_branch(repo)?))?;
        let mut walk = MergeWalk::default();
        let tips = StoryTips::for_story(home, repo, story, names);
        for _ in 0..SCAN_LIMIT {
            walk.walked += 1;
            if Option::is_some_and(story_merge(repo, &tip, story, names, &tips), |linked| {
                Result::is_ok(&linked)
            }) {
                walk.merges.push(git2::Commit::id(&tip));
            }
            if is_boundary(&tip) {
                walk.past_boundary = true;
                break;
            }
            match tip.parent(0) {
                Ok(parent) => tip = parent,
                Err(_) => break,
            }
        }
        Some(walk)
    }

    /// The revision a read of `story` (or of no story) uses when the caller names none:
    /// mergedSha, else the story's branch head, else origin's default branch; never `HEAD`.
    /// A story with no merge before the shallow boundary and no branch is the shallow reason.
    /// `Err` is a fixed reason.
    pub(crate) fn default_rev(
        &self,
        home: &Path,
        story: Option<&str>,
    ) -> Result<DefaultRev, &'static str> {
        let repo = self.open().map_err(|_| NO_LOCAL_COPY)?;
        if let Some(story) = story {
            let walk = Self::story_merges(
                home,
                &repo,
                story,
                &crate::session_usage::story_changes::story_branch_names(home, story),
            );
            if let Some(merge) = walk.as_ref().and_then(|walk| walk.merges.first()) {
                return Ok(DefaultRev::Merged(*merge));
            }
            if let Some(head) = story_branch_head(&repo, story) {
                return Ok(DefaultRev::Branch(head));
            }
            // A merge past the boundary is unknowable; the default branch would hide it.
            if walk.is_some_and(|walk| walk.past_boundary) {
                return Err(HISTORY_PAST_SHALLOW);
            }
        }
        default_branch(&repo)
            .map(DefaultRev::DefaultBranch)
            .ok_or(NO_DEFAULT_BRANCH)
    }

    /// Whether `story`'s landed history reaches past the shallow boundary.
    pub(crate) fn story_past_boundary(&self, home: &Path, story: &str) -> bool {
        Option::is_some(&Self::deepen_needed(self, home, story))
    }

    /// How far a shallow copy must deepen for `story`'s landed history: the merge scan reaches
    /// the boundary within [`SCAN_LIMIT`] (deepen by what the scan still lacks), or a merge's
    /// branch commits or parents are cut off by it. `None` when nothing is missing.
    fn deepen_needed(&self, home: &Path, story: &str) -> Option<u32> {
        let repo = Result::ok(self.open())?;
        let walk = Self::story_merges(
            home,
            &repo,
            story,
            &crate::session_usage::story_changes::story_branch_names(home, story),
        )?;
        if walk.past_boundary {
            return Some((SCAN_LIMIT + 1).saturating_sub(walk.walked).max(1) as u32);
        }
        let cut = walk.merges.iter().any(|merge| {
            let Ok(merge) = repo.find_commit(*merge) else {
                return true;
            };
            if is_boundary(&merge) {
                return true;
            }
            let parents: Vec<_> = merge.parent_ids().collect();
            if <[git2::Oid]>::is_empty(&parents) {
                return false;
            }
            let Ok(mut revwalk) = repo.revwalk() else {
                return true;
            };
            if parents
                .iter()
                .skip(1)
                .any(|head| revwalk.push(*head).is_err())
                || revwalk.hide(parents[0]).is_err()
            {
                return true;
            }
            parents
                .iter()
                .any(|id| repo.find_commit(*id).map_or(true, |c| is_boundary(&c)))
                || revwalk.any(|id| {
                    Result::ok(id)
                        .and_then(|id| Result::ok(repo.find_commit(id)))
                        .map_or(true, |commit| is_boundary(&commit))
                })
        });
        cut.then_some(FIRST_DEEPEN)
    }

    /// When this copy last fetched through CodeSense, and origin's default branch now.
    pub(crate) fn history_as_of(&self, home: &Path) -> Value {
        json!({"fetchedAt": fetch_record(home).get(&self.record_key()).cloned(), "originHead": self.origin_head()})
    }

    fn origin_head(&self) -> Option<String> {
        Result::ok(self.open())
            .and_then(|repo| default_branch(&repo))
            .map(|oid| oid.to_string())
    }

    fn record_key(&self) -> String {
        self.common_dir.to_string_lossy().into_owned()
    }

    fn miss_key(&self, story: &str) -> String {
        format!(
            "{TRAIL_MISS}{}\n{}",
            self.record_key(),
            story.to_ascii_uppercase()
        )
    }

    /// A trail fetch for `story` already ran at origin's current head and still found no
    /// merge: another one would find the same, until origin's head moves.
    pub(crate) fn trail_fetch_spent(&self, home: &Path, story: &str) -> bool {
        let head = self.origin_head();
        head.is_some()
            && fetch_record(home)
                .get(&self.miss_key(story))
                .and_then(Value::as_str)
                == head.as_deref()
    }

    /// Remember that `story`'s merge is not visible at origin's current head.
    pub(crate) fn remember_trail_miss(&self, home: &Path, story: &str) {
        let mut record = fetch_record(home);
        record.insert(self.miss_key(story), json!(self.origin_head()));
        save_fetch_record(home, &record);
    }

    /// Forget every remembered trail miss of this copy (an explicit refresh or an attach).
    fn clear_trail_misses(&self, home: &Path) {
        let prefix = format!("{TRAIL_MISS}{}\n", self.record_key());
        let mut record = fetch_record(home);
        serde_json::Map::retain(&mut record, |key, _| !key.starts_with(&prefix));
        save_fetch_record(home, &record);
    }

    fn record_fetch(&self, home: &Path) {
        let mut record = fetch_record(home);
        record.insert(self.record_key(), json!(now_ms()));
        save_fetch_record(home, &record);
    }

    /// Fetch origin's branches into `refs/remotes/origin/*`, all within `bound`. With `story`
    /// (a story that has landed), a shallow copy then deepens only while that story's merge
    /// scan still reaches the boundary, then unshallows if that was not enough. A missing
    /// `refs/remotes/origin/HEAD` is set from origin's own `HEAD`. `Err` is a fixed reason
    /// (credentials, busy, time) when not even the first fetch landed; a later deepen that
    /// fails leaves [`Fetched::partial`].
    pub(crate) fn fetch(
        &self,
        home: &Path,
        story: Option<&str>,
        bound: Duration,
    ) -> Result<Fetched, &'static str> {
        let deadline = Instant::now() + bound;
        self.run_fetch(&[], deadline)?;
        self.set_origin_head(deadline);
        self.record_fetch(home);
        let mut fetched = Fetched::default();
        if let Some(story) = story {
            let mut total = 0u32;
            for _ in 0..MAX_DEEPENS {
                let deepen = bool::then(Self::is_shallow(self), || {
                    Self::deepen_needed(self, home, story)
                });
                let Some(depth) = Option::flatten(deepen) else {
                    break;
                };
                let deepen = format!("--deepen={depth}");
                if let Err(reason) =
                    self.run_fetch(&[std::string::String::as_str(&deepen)], deadline)
                {
                    fetched.partial = Some(reason);
                    break;
                }
                total += depth;
                fetched.deepened = Some(Value::from(total));
            }
            if Option::is_none(&fetched.partial)
                && Self::is_shallow(self)
                && Self::story_past_boundary(self, home, story)
            {
                match self.run_fetch(&["--unshallow"], deadline) {
                    Ok(()) => fetched.deepened = Some(Value::from("unshallow")),
                    Err(reason) => fetched.partial = Some(reason),
                }
            }
            self.record_fetch(home);
        }
        Ok(fetched)
    }

    /// A fetch's answer, as `refresh_history` returns it.
    pub(crate) fn fetched_json(&self, home: &Path, fetched: &Fetched) -> Value {
        let mut value = json!({"historyAsOf": self.history_as_of(home), "shallow": self.is_shallow(), "deepened": fetched.deepened});
        if let Some(reason) = fetched.partial {
            value["status"] = json!("partial");
            value["reason"] = json!(reason);
        }
        value
    }

    fn run_fetch(&self, extra: &[&str], deadline: Instant) -> Result<(), &'static str> {
        let mut args = vec![
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-write-fetch-head",
            "--no-recurse-submodules",
            // Only the refspec below: no opportunistic update through a configured refspec.
            "--refmap=",
        ];
        args.extend_from_slice(extra);
        args.extend_from_slice(&["origin", REFSPEC]);
        self.git_bounded(&args, deadline).map(drop)
    }

    /// `refs/remotes/origin/HEAD` from origin's own `HEAD` (`git remote set-head origin -a`,
    /// which writes only that ref), when it is missing. Best effort within the bound.
    fn set_origin_head(&self, deadline: Instant) {
        let missing = self
            .open()
            .is_ok_and(|repo| repo.find_reference("refs/remotes/origin/HEAD").is_err());
        if missing {
            let _ = self.git_bounded(&["remote", "set-head", "origin", "--auto"], deadline);
        }
    }

    /// Git against this checkout that never prompts, runs no hook or maintenance, and is killed
    /// at `deadline`. The user's credential helper still answers non-interactively.
    fn git_bounded(&self, args: &[&str], deadline: Instant) -> Result<String, &'static str> {
        let mut command = super::git_ai::GitAiSource::default().command(&self.checkout);
        for config in [
            "core.hooksPath=/dev/null",
            "credential.interactive=never",
            "gc.auto=0",
            "maintenance.auto=false",
            "fetch.pruneTags=false",
            "remote.origin.pruneTags=false",
            "submodule.recurse=false",
        ] {
            command.args(["-c", config]);
        }
        command.args(args);
        for var in super::git_ai::REPO_OVERRIDES {
            command.env_remove(var);
        }
        command
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "/usr/bin/false")
            .env("SSH_ASKPASS", "/usr/bin/false")
            .env("SSH_ASKPASS_REQUIRE", "never")
            .env("GCM_INTERACTIVE", "never")
            .env("LC_ALL", "C");
        if let Some(ssh) = self.ssh_command() {
            command.env("GIT_SSH_COMMAND", format!("{ssh} -o BatchMode=yes"));
        }
        #[cfg(test)]
        {
            crate::test_support::strip_attribution_env(&mut command);
            command
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1");
        }
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| FETCH_FAILED)?;
        // A process that escaped the group (an ssh control master) may hold a pipe open, so
        // reads are collected through channels with a bound, never joined.
        let read = |pipe: Option<Box<dyn std::io::Read + Send>>| {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                use std::io::Read;
                let mut text = std::string::String::new();
                if let Some(mut pipe) = pipe {
                    let _ = pipe.read_to_string(&mut text);
                }
                let _ = tx.send(text);
            });
            rx
        };
        let stdout = read(child.stdout.take().map(|p| Box::new(p) as _));
        let stderr = read(child.stderr.take().map(|p| Box::new(p) as _));
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => {
                    // The whole group: a remote helper or ssh must not outlive the bound. Ask it
                    // to stop, then kill what is left after a short grace.
                    let group = -(std::process::Child::id(&child) as i32);
                    unsafe { libc::kill(group, libc::SIGTERM) };
                    let grace = Instant::now() + STOP_GRACE;
                    while Instant::now() < grace && matches!(child.try_wait(), Ok(None)) {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    unsafe { libc::kill(group, libc::SIGKILL) };
                    let _ = child.wait();
                    break None;
                }
            }
        };
        let collect = |rx: std::sync::mpsc::Receiver<String>| {
            let wait = deadline.saturating_duration_since(Instant::now()) + STOP_GRACE;
            rx.recv_timeout(wait).unwrap_or_default()
        };
        let (stdout, stderr) = (collect(stdout), collect(stderr));
        match status {
            None => Err(FETCH_TIMED_OUT),
            Some(status) if status.success() => Ok(stdout),
            Some(_) if self.holds_lock() => Err(FETCH_REPO_BUSY),
            Some(_) => Err(fetch_failure(&stderr)),
        }
    }

    /// Whether another git holds a lock this fetch takes: a ref, `packed-refs` or `shallow`.
    fn holds_lock(&self) -> bool {
        ["packed-refs.lock", "shallow.lock"]
            .iter()
            .any(|name| std::path::Path::exists(&self.common_dir.join(name)))
            || walkdir::WalkDir::new(self.common_dir.join("refs"))
                .into_iter()
                .filter_map(Result::ok)
                .any(|entry| {
                    entry.file_type().is_file()
                        && walkdir::DirEntry::path(&entry)
                            .extension()
                            .is_some_and(|e| e == "lock")
                })
    }

    /// The ssh command the user's git would run, to add batch mode to: `GIT_SSH_COMMAND`,
    /// else `core.sshCommand`, else plain ssh. `None` when the user set only `GIT_SSH` (a
    /// program, not a command line): it is left exactly as they chose.
    fn ssh_command(&self) -> Option<String> {
        let set = |name: &str| {
            Result::ok(std::env::var(name)).filter(|value| !str::is_empty(value.trim()))
        };
        set("GIT_SSH_COMMAND")
            .or_else(|| {
                let repo = Result::ok(git2::Repository::open(&self.checkout))?;
                Result::ok(Result::ok(repo.config())?.get_string("core.sshCommand"))
            })
            .or_else(|| set("GIT_SSH").is_none().then(|| "ssh".into()))
    }
}

/// A failed git's stderr as a fixed reason.
fn fetch_failure(stderr: &str) -> &'static str {
    let stderr = stderr.to_ascii_lowercase();
    let any = |needles: &[&str]| needles.iter().any(|needle| stderr.contains(needle));
    if any(&[
        "terminal prompts disabled",
        "could not read username",
        "could not read password",
        "unable to get password",
        "unable to get username",
        "unable to read askpass",
        "authentication failed",
        "authentication required",
        "permission denied (publickey",
        "invalid username or password",
    ]) {
        FETCH_NEEDS_CREDENTIALS
    } else if any(&[
        ".lock': file exists",
        "cannot lock ref",
        "another git process",
        "unable to lock",
    ]) {
        FETCH_REPO_BUSY
    } else {
        FETCH_FAILED
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

fn fetch_record_path(home: &Path) -> PathBuf {
    home.join("caches/history-fetches.json")
}

/// Last CodeSense fetch time (epoch ms) per common dir.
fn fetch_record(home: &Path) -> serde_json::Map<String, Value> {
    Result::ok(std::fs::read(fetch_record_path(home)))
        .and_then(|bytes| Result::ok(serde_json::from_slice(&bytes)))
        .unwrap_or_default()
}

fn save_fetch_record(home: &Path, record: &serde_json::Map<String, Value>) {
    let path = fetch_record_path(home);
    let (Some(dir), Ok(bytes)) = (path.parent(), serde_json::to_vec(record)) else {
        return;
    };
    let _ = std::fs::create_dir_all(dir);
    let _ = crate::records::io::atomic_write(dir, "history-fetches.json", &bytes);
}

/// One fetch after a checkout is attached, off the caller's thread and within
/// [`ATTACH_FETCH_BOUND`]; a failure only leaves the history as it was.
pub(crate) fn fetch_after_attach(home: &Path, checkout: &Path) {
    let (home, checkout) = (home.to_owned(), checkout.to_owned());
    // Tests attach fixtures whose origins name real hosts: only a local origin is fetched there.
    #[cfg(test)]
    if !Result::ok(git2::Repository::open(&checkout))
        .and_then(|repo| {
            Result::ok(Result::ok(repo.find_remote("origin"))?.url())
                .map(|url| url.starts_with("file://"))
        })
        .unwrap_or(false)
    {
        return;
    }
    std::thread::spawn(move || {
        if let Some(source) = HistorySource::at(&checkout) {
            source.clear_trail_misses(&home);
            if let Err(reason) = source.fetch(&home, None, ATTACH_FETCH_BOUND) {
                log::info!(
                    "attach fetch for {}: {reason}",
                    std::path::Path::display(&checkout)
                );
            }
        }
    });
}

#[derive(Debug, Clone, serde::Deserialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RefreshParams {
    /// Configured source slug; host/owner/repo disambiguates names shared by hosts.
    pub repo: String,
    /// For a story that has landed (status done or merged), deepen a shallow copy until its
    /// merge scan is inside the boundary. Another story gets a plain fetch.
    #[serde(default)]
    pub story_id: Option<String>,
    /// Bound on the whole refresh in milliseconds (default 60000, 1000 to 300000).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// `refresh_history`: fetch the configured repo's attached checkout now.
pub(crate) async fn refresh(
    home: &Path,
    principal: &crate::domain::Principal,
    params: &RefreshParams,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<Value, String> {
    if let Some(story) = params.story_id.as_deref() {
        if str::is_empty(story.trim()) || story.contains('\0') {
            return Err("invalid: storyId must be nonempty, without NUL".into());
        }
    }
    let bound = params
        .timeout_ms
        .map_or(REFRESH_DEFAULT, Duration::from_millis)
        .clamp(REFRESH_MIN, REFRESH_MAX);
    let (home, principal, params, scope) = (
        home.to_owned(),
        principal.clone(),
        params.clone(),
        scope.clone(),
    );
    crate::commands::await_blocking("refresh_history", move || {
        let Some(source) = super::why_line::attached_checkout(&home, &principal, &params.repo)?
            .as_deref()
            .and_then(HistorySource::at)
        else {
            return Ok(json!({"status": "unavailable", "reason": NO_LOCAL_COPY}));
        };
        // Only a landed story's merge history is worth deepening for.
        let story = params.story_id.as_deref().map(str::trim).filter(|story| {
            crate::session_usage::story_changes::story_is_done(&home, story, &scope)
        });
        source.clear_trail_misses(&home);
        Ok(match source.fetch(&home, story, bound) {
            Ok(fetched) => {
                let mut value = source.fetched_json(&home, &fetched);
                value["repo"] = json!(params.repo);
                if value.get("reason").is_none()
                    && Option::is_some_and(story, |story| {
                        HistorySource::story_past_boundary(&source, &home, story)
                    })
                {
                    value["reason"] = json!(HISTORY_PAST_SHALLOW);
                }
                value
            }
            Err(reason) => json!({"repo": params.repo, "status": "unavailable", "reason": reason,
                "historyAsOf": source.history_as_of(&home), "shallow": source.is_shallow()}),
        })
    })
    .await
}

/// Whether a story file's `status` says it has landed.
pub(crate) fn status_is_done(status: &str) -> bool {
    ["done", "merged"]
        .iter()
        .any(|done| status.trim().eq_ignore_ascii_case(done))
}

#[cfg(test)]
#[path = "history_tests.rs"]
pub(crate) mod tests;
