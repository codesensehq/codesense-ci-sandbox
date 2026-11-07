// SPDX-License-Identifier: MIT
//! The story-commits kind: a story's lines are a fold of per-commit rows, and a commit is read once.
use super::*;
use crate::attribution::git_ai::tests::git;
use crate::attribution::testing::{logged_calls, logging_binary};
use crate::session_usage::story_changes::line_share::tests::{add, annotate, cases, facts, fixture, head};
use crate::session_usage::story_changes::{commit_line_reads, git_walks};

const SCOPE: &str = "scope";
const SPEND: Duration = Duration::from_secs(600);
const CEILING: Duration = Duration::from_secs(60);
const NOTHING: &dyn Fn(&rusqlite::Connection, &[(String, String)]) -> Result<(), String> = &|_, _| Ok(());

fn open(home: &Path) -> Mutex<UsageStore> {
    Mutex::new(UsageStore::open(&home.join("usage.sqlite")).unwrap())
}

fn walk(store: &Mutex<UsageStore>, story: &str, facts: &git::GitFacts) {
    write(&lock(store), SCOPE, story, facts, 1).unwrap();
}

fn drain(home: &Path, store: &Mutex<UsageStore>) -> Swept {
    sweep(store, home, 1, (SPEND, CEILING), NOTHING).unwrap()
}

fn folded(store: &Mutex<UsageStore>, story: &str) -> Fold {
    fold(&rows(&lock(store), SCOPE, story).unwrap().unwrap())
}

fn queued_count(store: &Mutex<UsageStore>) -> i64 {
    lock(store).conn.query_row("SELECT count(*) FROM source_event", [], |r| r.get(0)).unwrap()
}

fn rows_of_sha(store: &Mutex<UsageStore>, sha: &str) -> i64 {
    lock(store)
        .conn
        .query_row("SELECT count(*) FROM contribution WHERE source_kind = 'commit' AND source_id = ?1", [sha], |r| r.get(0))
        .unwrap()
}

async fn whole(home: &Path, facts: &git::GitFacts) -> Result<Lines, &'static str> {
    line_share::totals(home, facts, None).await
}

/// Parity by construction, checked on every way a commit's lines can land: the fold of the rows
/// is the whole-story count with no budget, field for field.
#[tokio::test]
async fn the_fold_of_the_rows_is_the_whole_story_count_for_every_case() {
    for (name, build, _) in cases() {
        let (home, repo) = fixture();
        let facts = facts(repo.path(), &build(repo.path()));
        let store = open(home.path());
        walk(&store, "s", &facts);
        drain(home.path(), &store);
        let expected = whole(home.path(), &facts).await.unwrap_or_else(|reason| panic!("{name}: {reason}"));
        assert_eq!(folded(&store, "s").lines, Share::Lines(expected), "{name}");
    }
}

/// K new commits cost K reads, and the walk that finds them reads none; K more reads are not made
/// for the ones counted already.
#[tokio::test]
async fn k_new_commits_cost_k_reads_and_the_walk_costs_none() {
    let (home, repo) = fixture();
    let mut shas: Vec<String> = (0..3)
        .map(|n| add(repo.path(), &format!("f{n}.txt"), 2 + n, &format!("c{n}"), Some(("claude", 2))))
        .collect();
    let store = open(home.path());
    walk(&store, "s", &facts(repo.path(), &shas));
    assert_eq!(folded(&store, "s").lines, Share::Pending, "not counted yet is not 0");
    let first = drain(home.path(), &store);
    assert_eq!((first.counted, first.reused), (3, 0));
    assert_eq!(commit_line_reads(home.path()), 3);

    for n in 3..5 {
        shas.push(add(repo.path(), &format!("f{n}.txt"), 2, &format!("c{n}"), Some(("codex", 2))));
    }
    let all = facts(repo.path(), &shas);
    walk(&store, "s", &all);
    assert_eq!(queued_count(&store), 2, "only the new commits are queued");
    let second = drain(home.path(), &store);
    assert_eq!(second.counted, 2);
    assert_eq!(commit_line_reads(home.path()), 5, "K new commits are K more reads");
    assert_eq!(git_walks(home.path()), 0);
    // The same walk again costs nothing.
    walk(&store, "s", &all);
    assert_eq!(queued_count(&store), 0);
    assert_eq!(drain(home.path(), &store), Swept::default());
    assert_eq!(commit_line_reads(home.path()), 5);
    assert_eq!(folded(&store, "s").lines, Share::Lines(whole(home.path(), &all).await.unwrap()));
}

/// Rewritten history (a rebase, a squash, a force-push) costs one read per new sha; the rows of
/// the shas no longer reached are gone.
#[tokio::test]
async fn a_rewrite_reads_only_the_new_shas_and_drops_the_old_rows() {
    let (home, repo) = fixture();
    let shas: Vec<String> = (0..5).map(|n| add(repo.path(), &format!("f{n}.txt"), 2, &format!("c{n}"), Some(("claude", 2)))).collect();
    let store = open(home.path());
    walk(&store, "s", &facts(repo.path(), &shas));
    drain(home.path(), &store);
    assert_eq!(commit_line_reads(home.path()), 5);

    git(repo.path(), &["reset", "-q", "--hard", &shas[2]]);
    let mut rewritten = shas[..3].to_vec();
    for n in 0..2 {
        rewritten.push(add(repo.path(), &format!("g{n}.txt"), 3, &format!("again{n}"), Some(("claude", 3))));
    }
    let all = facts(repo.path(), &rewritten);
    walk(&store, "s", &all);
    drain(home.path(), &store);
    assert_eq!(commit_line_reads(home.path()), 7, "two new shas, two reads");
    for gone in &shas[3..] {
        assert_eq!(rows_of_sha(&store, gone), 0, "{gone}");
    }
    assert_eq!(folded(&store, "s").lines, Share::Lines(whole(home.path(), &all).await.unwrap()));
}

/// A merge turns a branch's commits into landed ones without a recount; lines on unmerged
/// branches (`in_progress`) move into `lines`.
#[tokio::test]
async fn a_merge_changes_state_not_counts() {
    let (home, repo) = fixture();
    let shas: Vec<String> = (0..2).map(|n| add(repo.path(), &format!("f{n}.txt"), 3, &format!("c{n}"), Some(("claude", 3)))).collect();
    let store = open(home.path());
    let mut story = facts(repo.path(), &shas);
    walk(&store, "s", &story);
    drain(home.path(), &store);
    let open_branch = folded(&store, "s");
    assert_eq!(open_branch.lines, open_branch.in_progress, "everything is still on the branch");
    let before = commit_line_reads(home.path());

    for commit in &mut story.commits {
        commit.landed = true;
    }
    walk(&store, "s", &story);
    assert_eq!(queued_count(&store), 0, "no sha changed, so nothing is queued");
    let merged = folded(&store, "s");
    assert_eq!(merged.lines, open_branch.lines);
    assert_eq!(merged.in_progress, Share::Lines(Lines::default()));
    assert_eq!(commit_line_reads(home.path()), before);
}

/// With merged PRs, a side branch's commits are not the story's lines, but they are in progress.
#[tokio::test]
async fn side_commits_are_in_progress_and_leave_lines_only_when_prs_landed() {
    let (home, repo) = fixture();
    let landed = add(repo.path(), "a.txt", 2, "c", Some(("claude", 2)));
    let side = add(repo.path(), "b.txt", 3, "d", Some(("codex", 3)));
    let store = open(home.path());
    let mut story = facts(repo.path(), &[landed, side]);
    story.commits[0].landed = true;
    story.commits[1].unlanded = true;
    story.landed_lines = true;
    walk(&store, "s", &story);
    drain(home.path(), &store);
    let got = folded(&store, "s");
    assert_eq!(got.in_progress, Share::Lines(Lines::of(&[("codex", 3)], 0, 0)));
    // With nothing landed every commit is the story.
    story.landed_lines = false;
    story.commits[0].landed = false;
    walk(&store, "t", &story);
    drain(home.path(), &store);
    assert_eq!(commit_line_reads(home.path()), 2, "the second story reused both counts");
    assert_eq!(folded(&store, "t").lines, Share::Lines(whole(home.path(), &story).await.unwrap()));
    story.landed_lines = true;
    story.commits[0].landed = true;
    assert_eq!(got.lines, Share::Lines(whole(home.path(), &story).await.unwrap()));
}

/// An unreadable commit makes the story's lines unavailable with its reason, as the whole count does.
#[tokio::test]
async fn an_unreadable_commit_nulls_the_lines_with_its_reason() {
    let (home, repo) = fixture();
    let good = add(repo.path(), "a.txt", 2, "c", Some(("claude", 2)));
    let missing = "1".repeat(40);
    let mut story = facts(repo.path(), &[good, missing.clone()]);
    story.repo_paths.insert(missing, vec![repo.path().to_owned()]);
    let store = open(home.path());
    walk(&store, "s", &story);
    drain(home.path(), &store);
    let expected = whole(home.path(), &story).await.unwrap_err();
    assert_eq!(folded(&store, "s").lines, Share::Unknown(expected.to_owned()));
}

/// A reason from where the commit was looked for (a worktree that is gone) is not the commit's
/// answer: a later walk that finds the repository counts it, and the sweep reads every repo any
/// story's walk found it in.
#[tokio::test]
async fn an_environment_reason_is_retried_by_a_later_walk() {
    let (home, repo) = fixture();
    let sha = add(repo.path(), "a.txt", 2, "c", Some(("claude", 2)));
    let good = facts(repo.path(), &[sha.clone()]);
    let mut gone = good.clone();
    gone.repo_paths.insert(sha.clone(), vec![home.path().join("removed-worktree")]);
    let store = open(home.path());
    walk(&store, "s", &gone);
    drain(home.path(), &store);
    assert_eq!(folded(&store, "s").lines, Share::Unknown(crate::attribution::ranges::REPO_NOT_FOUND.to_owned()));
    walk(&store, "s", &good);
    assert_eq!(queued_count(&store), 1, "the walk that finds the repository queues the commit again");
    drain(home.path(), &store);
    assert_eq!(folded(&store, "s").lines, Share::Lines(Lines::of(&[("claude", 2)], 0, 0)));

    // Another story's stale path does not decide the outcome for the sha.
    let (home, repo) = fixture();
    let sha = add(repo.path(), "a.txt", 2, "c", Some(("claude", 2)));
    let good = facts(repo.path(), &[sha.clone()]);
    let mut gone = good.clone();
    gone.repo_paths.insert(sha.clone(), vec![home.path().join("removed-worktree")]);
    let store = open(home.path());
    walk(&store, "stale", &gone);
    walk(&store, "fresh", &good);
    drain(home.path(), &store);
    assert_eq!(folded(&store, "stale").lines, Share::Lines(Lines::of(&[("claude", 2)], 0, 0)));
}

/// A note that lands after the commit was counted changes the commit's key: it is read once more.
#[tokio::test]
async fn a_late_note_recounts_that_commit_once() {
    let (home, repo) = fixture();
    let sha = add(repo.path(), "a.txt", 3, "c", None);
    let story = facts(repo.path(), &[sha]);
    let store = open(home.path());
    walk(&store, "s", &story);
    drain(home.path(), &store);
    assert_eq!(folded(&store, "s").lines, Share::Lines(Lines::of(&[], 0, 3)));
    annotate(repo.path(), "a.txt", 3, "claude");
    walk(&store, "s", &story);
    assert_eq!(queued_count(&store), 1);
    drain(home.path(), &store);
    assert_eq!(folded(&store, "s").lines, Share::Lines(Lines::of(&[("claude", 3)], 0, 0)));
    assert_eq!(commit_line_reads(home.path()), 2);
}

/// One sha in two stories is read once.
#[tokio::test]
async fn a_sha_shared_by_two_stories_is_counted_once() {
    let (home, repo) = fixture();
    let sha = add(repo.path(), "a.txt", 2, "c", Some(("claude", 2)));
    let story = facts(repo.path(), &[sha]);
    let store = open(home.path());
    walk(&store, "one", &story);
    walk(&store, "two", &story);
    assert_eq!(queued_count(&store), 1, "one queue row per sha");
    drain(home.path(), &store);
    assert_eq!(commit_line_reads(home.path()), 1);
    assert_eq!(folded(&store, "one").lines, folded(&store, "two").lines);
    assert_eq!(folded(&store, "one").lines, Share::Lines(Lines::of(&[("claude", 2)], 0, 0)));
}

/// The backfill: a slow git-ai (one that would have outrun the old 15 s story budget) is counted
/// a little per pass, across restarts; counted shas are never read again, and no row ever says the
/// story was too large.
#[tokio::test]
async fn a_backfill_drains_across_restarts_without_recounting() {
    let (home, repo) = fixture();
    let shas: Vec<String> = (0..4).map(|n| add(repo.path(), &format!("f{n}.txt"), 2, &format!("c{n}"), Some(("claude", 2)))).collect();
    let log = home.path().join("adapter-await.log");
    logging_binary(&home.path().join("bin/git-ai"), &log, "sleep 0.2");
    let story = facts(repo.path(), &shas);
    walk(&open(home.path()), "s", &story);
    let mut passes = 0;
    loop {
        // A restart: the store is opened again.
        let store = open(home.path());
        let swept = sweep(&store, home.path(), 1, (Duration::from_millis(1), CEILING), NOTHING).unwrap();
        passes += 1;
        if !swept.left {
            break;
        }
        assert_eq!(folded(&store, "s").lines, Share::Pending);
        assert!(passes < 20);
    }
    assert!(passes > 1, "the work was spread over passes");
    let store = open(home.path());
    assert_eq!(folded(&store, "s").lines, Share::Lines(Lines::of(&[("claude", 8)], 0, 0)));
    assert_eq!(commit_line_reads(home.path()), 4);
    assert_eq!(logged_calls(&log).len(), 4, "one blame per commit, however many passes");
    let stored: i64 = lock(&store)
        .conn
        .query_row("SELECT count(*) FROM contribution WHERE nums LIKE '%background%' OR reasons LIKE '%too large%'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(stored, 0);
}

/// A commit whose read keeps failing is tried five times, with a wait that doubles, and then its
/// lines are unavailable; the queue is empty and nothing loops hot.
#[test]
fn a_failing_commit_backs_off_five_times_then_gives_up() {
    let (home, repo) = fixture();
    let sha = add(repo.path(), "a.txt", 2, "c", Some(("claude", 2)));
    let store = open(home.path());
    walk(&store, "s", &facts(repo.path(), &[sha]));
    let mut calls = vec![];
    for minute in 0..(7 * 60) {
        let now = minute * 60_000;
        let mut count = |_: &str, _: &[PathBuf]| {
            calls.push(now);
            Err(line_share::UNREADABLE)
        };
        sweep_with(&store, now, SPEND, NOTHING, &mut count).unwrap();
    }
    assert_eq!(calls.len(), 5);
    let waits: Vec<i64> = calls.windows(2).map(|pair| pair[1] - pair[0]).collect();
    assert_eq!(waits, [300_000, 600_000, 1_200_000, 2_400_000]);
    assert_eq!(queued_count(&store), 0);
    assert_eq!(folded(&store, "s").lines, Share::Unknown(line_share::UNREADABLE.into()));
}

/// A commit that gave up on a transient failure is counted again by the next walk once the
/// attribution recovers; its failed row is not reused.
#[test]
fn a_later_walk_retries_a_commit_that_gave_up() {
    let (home, repo) = fixture();
    let sha = add(repo.path(), "a.txt", 2, "c", Some(("claude", 2)));
    let store = open(home.path());
    let story = facts(repo.path(), &[sha]);
    walk(&store, "s", &story);
    for minute in 0..(7 * 60) {
        let mut fail = |_: &str, _: &[PathBuf]| Err(line_share::UNREADABLE);
        sweep_with(&store, minute * 60_000, SPEND, NOTHING, &mut fail).unwrap();
    }
    assert_eq!(folded(&store, "s").lines, Share::Unknown(line_share::UNREADABLE.into()));
    // Attribution is back; an ordinary new walk of the story queues the commit again.
    walk(&store, "s", &story);
    assert_eq!(queued_count(&store), 1);
    let now = 8 * 60 * 60_000;
    let mut ok = |_: &str, _: &[PathBuf]| Ok(Lines::of(&[("claude", 2)], 0, 0));
    sweep_with(&store, now, SPEND, NOTHING, &mut ok).unwrap();
    assert_eq!(folded(&store, "s").lines, Share::Lines(Lines::of(&[("claude", 2)], 0, 0)));
}

/// A walk that drops a sha between the count and the fill leaves nothing behind for it.
#[test]
fn a_fill_never_resurrects_a_row_the_walk_dropped() {
    let (home, repo) = fixture();
    let (keep, drop_me) = (
        add(repo.path(), "a.txt", 2, "c", Some(("claude", 2))),
        add(repo.path(), "b.txt", 2, "d", Some(("claude", 2))),
    );
    let store = open(home.path());
    walk(&store, "s", &facts(repo.path(), &[drop_me.clone(), keep.clone()]));
    let survivor = facts(repo.path(), &[keep]);
    let mut counted = vec![];
    let mut count = |sha: &str, _: &[PathBuf]| {
        counted.push(sha.to_owned());
        // The walk runs while the sha is being read.
        write(&lock(&store), SCOPE, "s", &survivor, 2).unwrap();
        Ok(Lines::default())
    };
    // Oldest queued first: `drop_me` was queued first.
    sweep_with(&store, 1, SPEND, NOTHING, &mut count).unwrap();
    assert_eq!(counted[0], drop_me);
    assert_eq!(rows_of_sha(&store, &drop_me), 0);
    let waiting: i64 = lock(&store)
        .conn
        .query_row("SELECT count(*) FROM source_event WHERE source_id = ?1", [drop_me.as_str()], |r| r.get(0))
        .unwrap();
    assert_eq!(waiting, 0, "its queue row went with the fill");
}

/// The engine is not about stories: a second kind on it brings a kind and a reducer, no schema.
#[test]
fn the_kind_is_its_own_built_marker_on_the_shared_tables() {
    let (home, repo) = fixture();
    let sha = add(repo.path(), "a.txt", 2, "c", Some(("claude", 2)));
    let store = open(home.path());
    walk(&store, "s", &facts(repo.path(), &[sha]));
    let kinds: Vec<String> = {
        let guard = lock(&store);
        let mut stmt = guard.conn.prepare("SELECT DISTINCT entity_kind FROM contribution").unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    };
    assert_eq!(kinds, [StoryCommits::KIND]);
    let rows = rows(&lock(&store), SCOPE, "s").unwrap().unwrap();
    let agg = contribution::aggregate(StoryCommits::REDUCERS, &rows);
    assert_eq!(agg.nums["commits"], Some(1.0));
    assert!(head(repo.path()).len() == 40);
}

/// Counts are identical by construction: on every fixture case the ranges of the commit's own
/// lines are the same whether the other commits' notes are read or not.
#[test]
fn the_own_note_path_gives_the_whole_file_ranges_of_the_commit_for_every_case() {
    for (_, build, _) in cases() {
        let (_home, repo) = fixture();
        build(repo.path());
        crate::attribution::why_line::tests::assert_own_ranges_parity(repo.path());
    }
}

/// A story is marked for a recompute once, when its last commit is counted; the commits before
/// it change nothing a recompute could show.
#[test]
fn a_story_recomputes_once_when_its_last_row_fills() {
    let (home, repo) = fixture();
    let shas: Vec<String> = (0..3).map(|n| add(repo.path(), &format!("f{n}.txt"), 2, &format!("c{n}"), Some(("claude", 2)))).collect();
    let store = open(home.path());
    walk(&store, "s", &facts(repo.path(), &shas));
    let marks = std::cell::RefCell::new(vec![]);
    let mark = |_: &rusqlite::Connection, filled: &[(String, String)]| {
        marks.borrow_mut().extend(filled.iter().cloned());
        Ok(())
    };
    let mut count = |_: &str, _: &[PathBuf]| Ok(Lines::of(&[("claude", 2)], 0, 0));
    sweep_with(&store, 1, SPEND, &mark, &mut count).unwrap();
    assert_eq!(*marks.borrow(), [(SCOPE.to_owned(), "s".to_owned())]);
    assert_eq!(folded(&store, "s").lines, Share::Lines(Lines::of(&[("claude", 6)], 0, 0)));
}

/// The story with the fewest commits left is finished first, and queued order breaks ties.
#[test]
fn the_story_with_the_fewest_commits_left_is_counted_first() {
    let (home, repo) = fixture();
    let mut sha = |n: usize| add(repo.path(), &format!("f{n}.txt"), 2, &format!("c{n}"), None);
    let (big, small, mid): (Vec<String>, Vec<String>, Vec<String>) =
        ((0..3).map(&mut sha).collect(), (3..4).map(&mut sha).collect(), (4..6).map(&mut sha).collect());
    let store = open(home.path());
    walk(&store, "big", &facts(repo.path(), &big));
    walk(&store, "small", &facts(repo.path(), &small));
    walk(&store, "mid", &facts(repo.path(), &mid));
    let mut order = vec![];
    let mut count = |sha: &str, _: &[PathBuf]| {
        order.push(sha.to_owned());
        Ok(Lines::default())
    };
    sweep_with(&store, 1, SPEND, NOTHING, &mut count).unwrap();
    let expected: Vec<String> = [small, mid, big].concat();
    assert_eq!(order, expected);
}

/// A story that really changed lately is counted before every quiet backfill story, whatever the
/// backfill's size or age; the production selector runs over the production tables.
#[test]
fn a_live_story_is_counted_before_every_backfill_story() {
    const NOW: i64 = 10_000_000;
    let (home, repo) = fixture();
    let mut sha = |n: usize| add(repo.path(), &format!("f{n}.txt"), 2, &format!("c{n}"), None);
    let backfill: Vec<Vec<String>> = (0..5).map(|n| vec![sha(n)]).collect();
    let live: Vec<String> = (5..7).map(&mut sha).collect();
    let store = open(home.path());
    for (n, shas) in backfill.iter().enumerate() {
        walk(&store, &format!("old{n}"), &facts(repo.path(), shas));
    }
    walk(&store, "live", &facts(repo.path(), &live));
    {
        let store = lock(&store);
        let mut stmt = store
            .conn
            .prepare(
                "INSERT INTO story_totals (scope_key, story_key, story_id, scope_kind, asked_at_ms, changed_ms, dirty_since_ms, dirty_at_ms)
                 VALUES (?1, ?2, ?2, 'user', 1, ?3, ?3, ?3)",
            )
            .unwrap();
        stmt.execute(rusqlite::params![SCOPE, "live", NOW - 1000]).unwrap();
        // A notes-only mark leaves `changed_ms` old: that story is backfill.
        stmt.execute(rusqlite::params![SCOPE, "old0", 1]).unwrap();
    }
    let mut order = vec![];
    let mut count = |sha: &str, _: &[PathBuf]| {
        order.push(sha.to_owned());
        Ok(Lines::default())
    };
    sweep_with(&store, NOW, SPEND, NOTHING, &mut count).unwrap();
    let taken: Vec<&String> = order.iter().take(live.len()).collect();
    assert_eq!(taken, live.iter().collect::<Vec<_>>(), "every live commit before any backfill commit: {order:?}");
    assert_eq!(order.len(), 7);
}

/// Constant live enqueues cannot starve the quiet backfill: it gets every `LIVE_RUN + 1`th pick, so
/// it completes in bounded picks and a live commit never waits behind two backfill picks in a row.
/// The production selector runs over the production tables, one pick per sweep.
#[test]
fn constant_live_activity_cannot_starve_the_backfill() {
    const NOW: i64 = 10_000_000;
    const QUIET: usize = 3;
    let (home, repo) = fixture();
    let mut sha = |n: usize| add(repo.path(), &format!("f{n}.txt"), 2, &format!("c{n}"), None);
    let quiet: Vec<String> = (0..QUIET).map(&mut sha).collect();
    let live: Vec<String> = (QUIET..QUIET + 40).map(&mut sha).collect();
    let store = open(home.path());
    for (n, one) in quiet.iter().enumerate() {
        walk(&store, &format!("old{n}"), &facts(repo.path(), std::slice::from_ref(one)));
    }
    lock(&store)
        .conn
        .execute(
            "INSERT INTO story_totals (scope_key, story_key, story_id, scope_kind, asked_at_ms, changed_ms, dirty_since_ms, dirty_at_ms)
             VALUES (?1, 'live', 'live', 'user', 1, ?2, ?2, ?2)",
            rusqlite::params![SCOPE, NOW - 1000],
        )
        .unwrap();
    let streak = AtomicUsize::new(0);
    let clock = std::cell::Cell::new(Duration::ZERO);
    let mut picks: Vec<bool> = vec![];
    for round in 0..4 * QUIET {
        // New live work arrives every round, so a live commit is always waiting.
        walk(&store, "live", &facts(repo.path(), &live[..3 + round]));
        clock.set(Duration::ZERO);
        let mut count = |sha: &str, _: &[PathBuf]| {
            clock.set(clock.get() + Duration::from_secs(1));
            picks.push(quiet.iter().any(|q| q == sha));
            Ok(Lines::default())
        };
        sweep_timed(&store, NOW, Duration::from_secs(1), &|| clock.get(), &streak, NOTHING, &mut count).unwrap();
    }
    assert_eq!(picks.len(), 4 * QUIET, "one pick per sweep: {picks:?}");
    assert_eq!(picks.iter().filter(|quiet| **quiet).count(), QUIET, "the whole backfill completes within {} picks: {picks:?}", 4 * QUIET);
    assert!(picks.windows(2).all(|pair| !(pair[0] && pair[1])), "a live commit is never behind two backfill picks: {picks:?}");
}

/// With walks queueing new stories all the time, counting at the sweep's spend finishes stories
/// one at a time: at most one story is ever half counted, every story completes within a bounded
/// number of cycles of its walk, and the queue stays bounded. The sweep's share of the busy time
/// is at least half.
#[test]
fn stories_complete_in_bounded_cycles_while_walks_keep_enqueuing() {
    const PER_STORY: usize = 2;
    const CYCLES: usize = 30;
    let compute = Duration::from_secs(3);
    let cost = Duration::from_millis(1_000);
    let (home, repo) = fixture();
    let stories: Vec<(String, Vec<String>)> = (0..(8 + CYCLES))
        .map(|n| {
            let shas = (0..PER_STORY).map(|k| add(repo.path(), &format!("s{n}k{k}.txt"), 1, &format!("c{n}-{k}"), None)).collect();
            (format!("story{n}"), shas)
        })
        .collect();
    let store = open(home.path());
    // A backlog, then one new story per cycle.
    for (story, shas) in &stories[..8] {
        walk(&store, story, &facts(repo.path(), shas));
    }
    let clock = std::cell::Cell::new(Duration::ZERO);
    let (mut sweeping, mut busy) = (Duration::ZERO, Duration::ZERO);
    let (mut worst_queue, mut half_counted_max) = (0i64, 0);
    let mut done_at: Vec<Option<usize>> = vec![None; stories.len()];
    for cycle in 0..CYCLES {
        let (story, shas) = &stories[8 + cycle];
        walk(&store, story, &facts(repo.path(), shas));
        worst_queue = worst_queue.max(queued_count(&store));
        let spend = Ord::max(Duration::from_secs(2), compute);
        clock.set(Duration::ZERO);
        let mut count = |_: &str, _: &[PathBuf]| {
            clock.set(clock.get() + cost);
            Ok(Lines::default())
        };
        let swept = sweep_timed(&store, 1, spend, &|| clock.get(), &AtomicUsize::new(0), NOTHING, &mut count).unwrap();
        // A sweep that found nothing left to do is idle, not short of its share.
        if swept.left {
            sweeping += clock.get();
            busy += clock.get() + compute;
        }
        let mut half = 0;
        for (n, (story, _)) in stories.iter().enumerate() {
            let Some(rows) = rows(&lock(&store), SCOPE, story).unwrap() else { continue };
            match fold(&rows).lines {
                Share::Pending if rows.iter().any(|row| counted(&row.contribution)) => half += 1,
                Share::Pending => {}
                _ => done_at[n] = done_at[n].or(Some(cycle)),
            }
        }
        half_counted_max = half_counted_max.max(half);
    }
    assert!(half_counted_max <= 1, "stories finish one at a time: {half_counted_max} half counted");
    assert!(busy > Duration::ZERO && sweeping * 2 >= busy, "the sweep gets at least half: {sweeping:?} of {busy:?}");
    assert!(worst_queue <= i64::try_from(9 * PER_STORY).unwrap(), "the queue stays bounded: {worst_queue}");
    // The backlog's stories all finished, each within its place in line.
    for (n, done) in done_at[..8].iter().enumerate() {
        assert!(done.is_some_and(|cycle| cycle <= 2 * n + 2), "story {n} completed at {done:?}");
    }
}
