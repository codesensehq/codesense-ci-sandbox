// SPDX-License-Identifier: MIT
//! Each story's branch tips as CodeSense saw them, kept apart from worktree records so they
//! outlive a removed worktree or a pruned record. A merge on origin's default branch whose
//! second parent is one of them lands the story (SV-CDE-82). One file per story,
//! `{home}/story-tips/<STORY>.json`: `[{branch, sha, atMs}]`, oldest first, the newest
//! [`KEEP`] with a sha kept once. This module is its only writer.
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::worktree_index::{self, story_id_from_branch};

const DIR: &str = "story-tips";
/// The most tips a story keeps: enough for every PR of a long-lived branch.
pub(crate) const KEEP: usize = 50;
/// How long a removal or close waits for the writer: its sighting is the last chance.
const LAST_CHANCE: Duration = Duration::from_secs(2);

/// One sighting of a story branch's head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoryTip {
    pub branch: String,
    pub sha: String,
    /// Unix-epoch milliseconds of the sighting.
    pub at_ms: u64,
}

/// Whether a sighting may wait for the store's writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// A read or a launch: skip the sighting rather than wait.
    Never,
    /// A removal or a close: wait up to [`LAST_CHANCE`].
    LastChance,
}

fn now_ms() -> u64 {
    Result::unwrap_or(
        u64::try_from(chrono::DateTime::timestamp_millis(&chrono::Utc::now())),
        0,
    )
}

fn writer() -> &'static std::sync::Mutex<()> {
    static WRITER: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    std::sync::OnceLock::get_or_init(&WRITER, std::sync::Mutex::default)
}

/// The store file of `story`; `None` for an id that is not a plain story id.
fn path(home: &Path, story: &str) -> Option<PathBuf> {
    let plain = !str::is_empty(story)
        && Iterator::all(&mut str::chars(story), |c| {
            char::is_ascii_alphanumeric(&c) || c == '-' || c == '_'
        });
    bool::then(plain, || {
        Path::join(
            &Path::join(home, DIR),
            format!("{}.json", str::to_ascii_uppercase(story)),
        )
    })
}

/// Every recorded tip of `story`, oldest first; empty when none is recorded or unreadable.
pub fn read(home: &Path, story: &str) -> Vec<StoryTip> {
    Option::unwrap_or_default(load(home, story))
}

/// The recorded tips of `story`: empty when none is recorded (no file), `None` when the file
/// is there but cannot be read or parsed, so a writer never replaces what it could not read.
fn load(home: &Path, story: &str) -> Option<Vec<StoryTip>> {
    let path = path(home, story)?;
    match std::fs::read(path) {
        Ok(bytes) => Result::ok(serde_json::from_slice(&bytes)),
        Err(error) if std::io::Error::kind(&error) == std::io::ErrorKind::NotFound => {
            Some(std::vec::Vec::new())
        }
        Err(_) => None,
    }
}

/// Record `repo`'s HEAD for `story` when it is on one of the story's branches by convention
/// (never a detached HEAD or another story's branch) and differs from the newest sighting.
/// Best-effort: a busy writer (with [`Wait::Never`]) or a failed write leaves the store as it
/// was. `true` when it wrote.
pub fn sight(home: &Path, story: &str, repo: &git2::Repository, wait: Wait) -> bool {
    sight_with(writer(), home, story, repo, wait)
}

/// [`sight`] serialised by `writer` (the store's one writer lock; tests pass their own).
fn sight_with(
    writer: &std::sync::Mutex<()>,
    home: &Path,
    story: &str,
    repo: &git2::Repository,
    wait: Wait,
) -> bool {
    let Ok(head) = git2::Repository::head(repo) else {
        return false;
    };
    let (Some(branch), Some(oid)) = (
        Result::ok(git2::Reference::shorthand(&head)),
        git2::Reference::target(&head),
    ) else {
        return false;
    };
    let ours = git2::Reference::is_branch(&head)
        && Option::is_some_and(story_id_from_branch(branch), |id| {
            str::eq_ignore_ascii_case(&id, story)
        });
    if !ours {
        return false;
    }
    let tip = StoryTip {
        branch: str::to_owned(branch),
        sha: ToString::to_string(&oid),
        at_ms: now_ms(),
    };
    let Some(_guard) = lock(writer, wait) else {
        return false;
    };
    let Some(mut tips) = load(home, story) else {
        return false;
    };
    if Option::is_some_and(<[StoryTip]>::last(&tips), |newest| {
        newest.sha == tip.sha && newest.branch == tip.branch
    }) {
        return false;
    }
    std::vec::Vec::retain(&mut tips, |seen| seen.sha != tip.sha);
    std::vec::Vec::push(&mut tips, tip);
    let excess = usize::saturating_sub(std::vec::Vec::len(&tips), KEEP);
    write(home, story, &std::vec::Vec::split_off(&mut tips, excess))
}

/// [`sight`] the checkout at `root` (found with `Repository::discover`).
pub fn sight_root(home: &Path, story: &str, root: &Path, wait: Wait) -> bool {
    Result::is_ok_and(git2::Repository::discover(root), |repo| {
        sight(home, story, &repo, wait)
    })
}

/// Before `root` is removed: [`sight`] it for every story a worktree record of it names.
pub fn sight_worktree(home: &Path, root: &Path) {
    for principal in crate::context_repo::org_enumeration::principals(home) {
        if let Ok(Some(record)) = worktree_index::read_record(home, &principal, root) {
            sight_root(home, &record.story_id, root, Wait::LastChance);
        }
    }
}

/// When `story`'s workspace closes: [`sight`] each of its live worktrees.
pub fn sight_story_worktrees(home: &Path, story: &str) {
    for principal in crate::context_repo::org_enumeration::principals(home) {
        let records = worktree_index::find_by_story_id(home, &principal, story);
        for record in Result::unwrap_or_default(records) {
            sight_root(home, story, &record.code_repo_path, Wait::LastChance);
        }
    }
}

fn lock(writer: &std::sync::Mutex<()>, wait: Wait) -> Option<std::sync::MutexGuard<'_, ()>> {
    let deadline = Instant::now() + LAST_CHANCE;
    loop {
        match std::sync::Mutex::try_lock(writer) {
            Ok(guard) => return Some(guard),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                return Some(std::sync::PoisonError::into_inner(poisoned))
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                if wait == Wait::Never || Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

fn write(home: &Path, story: &str, tips: &[StoryTip]) -> bool {
    let Some(path) = path(home, story) else {
        return false;
    };
    let Ok(bytes) = serde_json::to_vec_pretty(tips) else {
        return false;
    };
    Option::is_some_and(Path::parent(&path), |dir| {
        Result::is_ok(&std::fs::create_dir_all(dir))
    }) && Result::is_ok(&crate::content_engine::write_tmp_fsync_rename(
        &path, &bytes,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_on(branch: &str) -> (tempfile::TempDir, git2::Repository, git2::Oid) {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let sig = git2::Signature::new("t", "t@example.com", &git2::Time::new(1, 0)).unwrap();
        let tree = repo
            .find_tree(repo.treebuilder(None).unwrap().write().unwrap())
            .unwrap();
        let reference = format!("refs/heads/{branch}");
        let oid = repo
            .commit(Some(&reference), &sig, &sig, "c", &tree, &[])
            .unwrap();
        repo.set_head(&reference).unwrap();
        drop(tree);
        (dir, repo, oid)
    }

    /// A story's own branch is recorded once per new head, kept across many PRs up to
    /// [`KEEP`]; another story's branch and a detached HEAD are never recorded, and a busy
    /// writer skips a read's sighting rather than wait.
    #[test]
    fn only_the_storys_branch_heads_are_kept_once_each_newest_last() {
        let home = tempfile::tempdir().unwrap();
        let (_dir, repo, first) = repo_on("feat/SV-29-work");
        assert!(sight(home.path(), "SV-29", &repo, Wait::Never));
        assert!(
            !sight(home.path(), "SV-29", &repo, Wait::Never),
            "unmoved: no write"
        );
        assert!(
            !sight(home.path(), "SV-30", &repo, Wait::Never),
            "another story's branch"
        );
        let tips = read(home.path(), "sv-29");
        assert_eq!(tips.len(), 1);
        assert_eq!(
            (tips[0].branch.as_str(), tips[0].sha.clone()),
            ("feat/SV-29-work", first.to_string())
        );

        let sig = git2::Signature::new("t", "t@example.com", &git2::Time::new(2, 0)).unwrap();
        let mut parent = repo.find_commit(first).unwrap();
        for n in 0..(KEEP + 5) {
            let tree = parent.tree().unwrap();
            let oid = repo
                .commit(
                    Some("HEAD"),
                    &sig,
                    &sig,
                    &format!("c{n}"),
                    &tree,
                    &[&parent],
                )
                .unwrap();
            parent = repo.find_commit(oid).unwrap();
            assert!(sight(home.path(), "SV-29", &repo, Wait::LastChance));
        }
        let tips = read(home.path(), "SV-29");
        assert_eq!(tips.len(), KEEP);
        assert_eq!(tips.last().unwrap().sha, parent.id().to_string());

        repo.set_head_detached(first).unwrap();
        assert!(
            !sight(home.path(), "SV-29", &repo, Wait::LastChance),
            "detached HEAD"
        );
        assert_eq!(
            read(home.path(), "SV-29").last().unwrap().sha,
            parent.id().to_string()
        );

        let own = std::sync::Mutex::new(());
        let held = own.lock().unwrap();
        repo.set_head("refs/heads/feat/SV-29-work").unwrap();
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            "late",
            &parent.tree().unwrap(),
            &[&parent],
        )
        .unwrap();
        assert!(
            !sight_with(&own, home.path(), "SV-29", &repo, Wait::Never),
            "a busy writer is skipped"
        );
        drop(held);
        // A store that is there but unreadable is never replaced.
        let file = path(home.path(), "SV-29").unwrap();
        std::fs::write(&file, b"not json").unwrap();
        assert!(!sight(home.path(), "SV-29", &repo, Wait::Never));
        assert_eq!(std::fs::read(&file).unwrap(), b"not json");
        assert!(path(home.path(), "../escape").is_none());
    }

    /// The close and removal sightings find a story's worktrees through their records: the
    /// close (`sight_story_worktrees`, what `close_workspace` calls with the workspace's
    /// story) records each live worktree's story branch head; a non-story form records
    /// nothing. Red on the base: a close recorded no tip.
    #[test]
    fn a_close_records_each_story_worktrees_branch_head() {
        let home = tempfile::tempdir().unwrap();
        let (dir, _repo, tip) = repo_on("feat/SV-31-close");
        let record = worktree_index::WorktreeStoryRecord::new(
            dir.path(),
            "acme",
            crate::domain::SourceRepoSlug::new("acme", "api").unwrap(),
            "SV-31",
            home.path().join("story.md"),
        );
        let principal = crate::domain::Principal::from(7u64);
        worktree_index::write_record(home.path(), &principal, &record).unwrap();
        sight_story_worktrees(home.path(), "generic:/tmp/somewhere");
        assert!(read(home.path(), "SV-31").is_empty());
        sight_story_worktrees(home.path(), "SV-31");
        let tips = read(home.path(), "SV-31");
        assert_eq!(tips.len(), 1, "{tips:?}");
        assert_eq!(tips[0].sha, tip.to_string());
        // The removal sighting reads the same record by path.
        std::fs::remove_file(path(home.path(), "SV-31").unwrap()).unwrap();
        sight_worktree(home.path(), dir.path());
        assert_eq!(read(home.path(), "SV-31")[0].sha, tip.to_string());
    }
}
