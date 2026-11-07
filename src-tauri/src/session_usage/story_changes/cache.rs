// SPDX-License-Identifier: MIT
//! SV-ENG-121: the git half of a hosted story read, kept per story until origin's head or a
//! branch tip of the repos it read moves. A retry of a read that outlived the answer timeout
//! (the walk finishes on its own) is then an index read, not a second walk.
use super::*;

/// The longest a kept half is trusted: the worktree and planned facts in it have no ref to watch.
const KEPT_FOR: std::time::Duration = std::time::Duration::from_secs(300);
/// The most stories kept at once; the oldest goes first.
const KEPT_STORIES: usize = 16;

struct Kept {
    at: std::time::Instant,
    used: std::time::Instant,
    stamp: String,
    git: git::GitFacts,
    planned: planned::PlannedFacts,
    part: GitTotals,
}

/// The kept halves by key (tests keep their own).
struct Store(std::sync::Mutex<std::collections::BTreeMap<String, Kept>>);

static KEPT: Store = Store::new();

impl Store {
    const fn new() -> Self {
        Store(std::sync::Mutex::new(std::collections::BTreeMap::new()))
    }
}

/// What the git half was read from: the home, the story, who asked and the sessions' links.
pub(super) fn key(
    home: &Path,
    story: &str,
    scope: &CallerScope,
    refs: &[git::SessionRef],
    sessions: &str,
) -> String {
    format!(
        "{}\n{}\n{:?}\n{:?}\n{}",
        std::path::Path::display(home),
        str::to_ascii_uppercase(story),
        scope,
        refs,
        sessions
    )
}

/// What a kept half was stamped from: the story and, per repo, the branch names its walk read.
#[derive(Debug, Clone, Default)]
pub struct StampSpec {
    pub story: String,
    pub repos: Vec<(std::path::PathBuf, BTreeSet<String>)>,
}

/// Origin's default head, the names of the story's branches now, and the tips of those and of
/// `names`, the branches the walk read: a branch created or fetched later changes it too.
pub(super) fn repo_stamp(repo: &git2::Repository, story: &str, names: &BTreeSet<String>) -> String {
    let mut out = std::string::String::new();
    if let Some(oid) = crate::attribution::history::default_branch(repo) {
        String::push_str(&mut out, &format!("origin {oid}\n"));
    }
    let mut all = git::story_branch_names(repo, story);
    String::push_str(&mut out, &format!("branches {all:?}\n"));
    all.extend(names.iter().cloned());
    for name in &all {
        for prefix in ["refs/heads/", "refs/remotes/origin/"] {
            let tip = Option::and_then(
                Result::ok(git2::Repository::find_reference(
                    repo,
                    &format!("{prefix}{name}"),
                )),
                |found| git2::Reference::target(&found),
            );
            String::push_str(&mut out, &format!("{prefix}{name} {tip:?}\n"));
        }
    }
    out
}

/// [`repo_stamp`] of each repo of `spec`.
pub(super) fn stamp_of(spec: &StampSpec) -> String {
    let mut out = std::string::String::new();
    for (dir, names) in &spec.repos {
        let Ok(repo) = git2::Repository::open(dir) else {
            continue;
        };
        String::push_str(&mut out, &format!("{}\n", std::path::Path::display(dir)));
        String::push_str(&mut out, &repo_stamp(&repo, &spec.story, names));
    }
    out
}

/// The kept half for `key`, when it is young and no ref it read has moved.
pub(super) async fn get(key: &str) -> Option<(git::GitFacts, planned::PlannedFacts, GitTotals)> {
    KEPT.get_at(key, std::time::Instant::now()).await
}

/// Keep a half just read for `key`, unless a ref it read moved while the walk ran (`git.refs_stamp`
/// was taken before the walk): that answer is served, not kept.
pub(super) async fn put(
    key: String,
    git: &git::GitFacts,
    planned: &planned::PlannedFacts,
    part: &GitTotals,
) {
    KEPT.put_at(key, git, planned, part, std::time::Instant::now())
        .await;
}

impl Store {
    async fn get_at(
        &self,
        key: &str,
        now: std::time::Instant,
    ) -> Option<(git::GitFacts, planned::PlannedFacts, GitTotals)> {
        let (spec, kept_stamp) = {
            let kept = Result::ok(self.0.lock())?;
            let entry = std::collections::BTreeMap::get(&kept, key)?;
            if std::time::Instant::saturating_duration_since(&now, entry.at) > KEPT_FOR {
                return None;
            }
            (entry.git.stamp_spec.clone(), entry.stamp.clone())
        };
        let seen = Result::ok(
            crate::commands::await_blocking("story_changes.cache", move || Ok(stamp_of(&spec)))
                .await,
        )?;
        if seen != kept_stamp {
            return None;
        }
        let mut kept = Result::ok(self.0.lock())?;
        let entry = std::collections::BTreeMap::get_mut(&mut kept, key)?;
        entry.used = now;
        Some((entry.git.clone(), entry.planned.clone(), entry.part.clone()))
    }

    async fn put_at(
        &self,
        key: String,
        git: &git::GitFacts,
        planned: &planned::PlannedFacts,
        part: &GitTotals,
        now: std::time::Instant,
    ) {
        let spec = git.stamp_spec.clone();
        let Ok(seen) =
            crate::commands::await_blocking("story_changes.cache", move || Ok(stamp_of(&spec)))
                .await
        else {
            return;
        };
        if seen != git.refs_stamp {
            return;
        }
        let Ok(mut kept) = self.0.lock() else {
            return;
        };
        if std::collections::BTreeMap::len(&kept) >= KEPT_STORIES
            && !std::collections::BTreeMap::contains_key(&kept, &key)
        {
            let oldest = Option::map(
                Iterator::min_by_key(std::collections::BTreeMap::iter(&kept), |(_, entry)| {
                    entry.used
                }),
                |(oldest, _)| String::clone(oldest),
            );
            if let Some(oldest) = oldest {
                std::collections::BTreeMap::remove(&mut kept, &oldest);
            }
        }
        std::collections::BTreeMap::insert(
            &mut kept,
            key,
            Kept {
                at: now,
                used: now,
                stamp: seen,
                git: git.clone(),
                planned: planned.clone(),
                part: part.clone(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn half(stamp: &str) -> (git::GitFacts, planned::PlannedFacts, GitTotals) {
        let git = git::GitFacts {
            refs_stamp: stamp.to_owned(),
            ..Default::default()
        };
        (git, Default::default(), Default::default())
    }

    #[tokio::test]
    async fn expiry_scope_and_a_mid_walk_ref_move() {
        let store = Store::new();
        let (git, planned, part) = half("");
        let base = std::time::Instant::now();
        store
            .put_at("t-expiry".into(), &git, &planned, &part, base)
            .await;
        assert!(store.get_at("t-expiry", base).await.is_some());
        assert!(store.get_at("t-expiry", base + KEPT_FOR).await.is_some());
        let late = base + KEPT_FOR + std::time::Duration::from_secs(1);
        assert!(
            store.get_at("t-expiry", late).await.is_none(),
            "a kept half is trusted for KEPT_FOR"
        );

        let home = Path::new("/home");
        let human = key(home, "SV-1", &CallerScope::Human, &[], "s");
        let unknown = key(home, "SV-1", &CallerScope::Unknown, &[], "s");
        assert_ne!(human, unknown, "another scope must not share a kept half");
        assert_eq!(human, key(home, "sv-1", &CallerScope::Human, &[], "s"));

        // The refs moved while the walk ran: the stamp taken at its start no longer matches.
        let (moved, planned, part) = half("origin before the walk");
        store
            .put_at("t-moved".into(), &moved, &planned, &part, base)
            .await;
        assert!(
            store.get_at("t-moved", base).await.is_none(),
            "a half read across a ref move was kept"
        );
    }

    #[tokio::test]
    async fn the_least_recently_used_half_goes_first() {
        let store = Store::new();
        let (git, planned, part) = half("");
        let base = std::time::Instant::now();
        let at = |n: u64| base + std::time::Duration::from_millis(n);
        for n in 0..KEPT_STORIES {
            store
                .put_at(format!("t-lru-{n:02}"), &git, &planned, &part, at(n as u64))
                .await;
        }
        // A hit renews the oldest, so the next oldest goes when one more is kept.
        assert!(store.get_at("t-lru-00", at(100)).await.is_some());
        store
            .put_at("t-lru-new".into(), &git, &planned, &part, at(101))
            .await;
        assert!(store.get_at("t-lru-00", at(102)).await.is_some());
        assert!(store.get_at("t-lru-01", at(102)).await.is_none());
        assert!(store.get_at("t-lru-new", at(102)).await.is_some());
    }
}
