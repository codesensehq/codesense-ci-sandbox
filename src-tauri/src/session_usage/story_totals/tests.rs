// SPDX-License-Identifier: MIT
use super::*;
use crate::session_usage::{
    event::Tokens,
    record::SessionRecord,
    task::TaskRecord,
    tests::{event, store},
    StoryLink,
};
use crate::context_query::{index::Snapshot, record::ContextRecord};
use std::sync::Arc;

fn ids(prefix: &str, n: usize) -> Vec<String> {
    (0..n).map(|i| format!("{prefix}-{i}")).collect()
}

/// One session spent on `story`, priced or not, with no timing.
fn seed_session(s: &mut UsageStore, home: &Path, story: &str, session: &str, priced: bool) {
    let source = home.join(format!("{story}.md"));
    std::fs::write(&source, "---\nstatus: in-progress\n---\n").unwrap();
    let link = StoryLink {
        story_id: story.into(),
        source,
        org: "acme".into(),
    };
    s.link_story(session, &link).unwrap();
    let mut record = SessionRecord::new(session);
    record.story_id = Some(story.into());
    s.write_session_record(&record).unwrap();
    let mut e = event(
        &format!("{session}-e"),
        session,
        if priced { "gpt-6-sol" } else { "unpriced-model" },
        1_790_000_000_000,
        Tokens {
            input: 100,
            ..Default::default()
        },
    );
    if priced {
        e.reported_cost_usd = Some(2.0);
    }
    s.ingest(&[e]).unwrap();
}

/// A dev task and `n` review tasks of it.
fn seed_reviews(s: &UsageStore, story: &str, n: usize) {
    let mut dev = TaskRecord::new(format!("{story}-dev"), "build");
    dev.story_id = Some(story.into());
    s.insert_task_if_absent(&dev).unwrap();
    for i in 0..n {
        let id = format!("{story}-review-{i}");
        let mut review = TaskRecord::new(id.clone(), "review");
        review.story_id = Some(story.into());
        review.task_type = "review".into();
        s.insert_task_if_absent(&review).unwrap();
        s.add_task_link(&id, "relates_to:reviews", &format!("{story}-dev"))
            .unwrap();
    }
}

/// Run the worker's compute step until nothing is due, as the worker does.
async fn index_all(home: &Path) {
    let worker = Arc::new(Mutex::new(
        UsageStore::open(&home.join("usage.sqlite")).unwrap(),
    ));
    for _ in 0..100 {
        let jobs = next_jobs(&worker.lock().unwrap(), now_ms(), 1000).unwrap();
        if jobs.is_empty() {
            return;
        }
        for job in jobs {
            refresh(home, &worker, job).await;
        }
    }
    panic!("the index never settled");
}

fn human_read(home: &Path, ids: &[String]) -> Value {
    read_blocking(home, ids, &CallerScope::Human, false).unwrap()
}

/// A story's `unavailable` with each reason index replaced by its text.
fn expand(answer: &Value, story: &Value) -> Value {
    Value::Object(
        story["unavailable"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(field, index)| {
                let text = answer["reasons"][index.as_u64().unwrap() as usize].clone();
                (field.clone(), text)
            })
            .collect(),
    )
}

/// Case 1 (I2): 770 ids in two calls on a warm index, under 5 s, and the read never enters the
/// story code that reads git (`git::collect`, `line_share::totals`; git2 runs in-process, so there
/// is no subprocess to count).
#[tokio::test]
async fn case1_770_ids_read_warm_in_two_calls_under_five_seconds_without_touching_git() {
    let (dir, mut s) = store();
    let home = dir.path();
    let all = ids("SV-WARM", 770);
    seed_session(&mut s, home, &all[0], "warm", true);
    human_read(home, &all[..1]);
    index_all(home).await;
    // Warm the other 769 with that computed row: the values do not matter here, only that a
    // read of indexed stories never computes one.
    for id in &all[1..] {
        s.conn
            .execute(
                "INSERT INTO story_totals (scope_key, story_key, story_id, scope_kind, scope_root,
                    totals, unavailable, git, repos, branches, version, computed_at_ms, asked_at_ms)
                 SELECT scope_key, lower(?1), ?1, scope_kind, scope_root, totals, unavailable, git, repos,
                    branches, version, computed_at_ms, asked_at_ms FROM story_totals WHERE story_key = ?2",
                params![id, all[0].to_lowercase()],
            )
            .unwrap();
    }
    let git_reads = story_changes::git_reads(home);
    let started = std::time::Instant::now();
    let first = human_read(home, &all[..500]);
    let second = human_read(home, &all[500..]);
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    assert_eq!(
        story_changes::git_reads(home),
        git_reads,
        "a storyTotals read read git"
    );
    let mut answered = 0;
    for answer in [&first, &second] {
        assert_eq!(answer["pending"], json!([]));
        assert!(answer.get("rest").is_none(), "{:?}", answer["rest"]);
        assert_eq!(answer["indexing"], json!({"done": 770, "total": 770}));
        assert!(serde_json::to_vec(answer).unwrap().len() < 256 * 1024);
        for story in answer["stories"].as_object().unwrap().values() {
            assert_eq!(story["totals"]["costUsd"], 2.0);
            answered += 1;
        }
    }
    assert_eq!(answered, 770);
}

/// Case 2 (I2): cold, every id is pending (never 0) until the backfill computes it, and
/// `indexing` reports the progress.
#[tokio::test]
async fn case2_a_cold_index_answers_pending_never_zero_and_reports_progress() {
    let (dir, mut s) = store();
    let home = dir.path();
    let all = ids("SV-COLD", 3);
    seed_session(&mut s, home, &all[0], "cold", true);
    let cold = human_read(home, &all);
    assert_eq!(cold["pending"], json!(all));
    for id in &all {
        assert_eq!(cold["stories"][id], json!({"pending": true}), "{id}");
    }
    assert_eq!(cold["indexing"], json!({"done": 0, "total": 3}));

    // One compute step, as the worker takes them.
    let worker = Arc::new(Mutex::new(
        UsageStore::open(&home.join("usage.sqlite")).unwrap(),
    ));
    let job = next_jobs(&worker.lock().unwrap(), now_ms(), 1)
        .unwrap()
        .remove(0);
    refresh(home, &worker, job).await;
    let warming = human_read(home, &all);
    assert_eq!(warming["indexing"], json!({"done": 1, "total": 3}));
    assert_eq!(warming["pending"].as_array().unwrap().len(), 2);
    for (id, story) in warming["stories"].as_object().unwrap() {
        if story.get("pending").is_some() {
            assert!(story.get("totals").is_none(), "{id}: pending carries totals");
        }
    }

    index_all(home).await;
    let warm = human_read(home, &all);
    assert_eq!(warm["pending"], json!([]));
    assert_eq!(warm["indexing"], json!({"done": 3, "total": 3}));
    assert_eq!(warm["stories"][&all[0]]["totals"]["sessions"], 1);
}

/// Case 3 (I2): a task, a session's usage or a commit on a story branch marks only that story
/// changed; the worker recomputes it and no other.
#[tokio::test]
async fn case3_a_change_marks_and_recomputes_only_its_own_story() {
    let (dir, mut s) = store();
    let home = dir.path();
    let (a, b) = ("SV-INC-1".to_string(), "SV-INC-2".to_string());
    seed_session(&mut s, home, &a, "a", true);
    seed_session(&mut s, home, &b, "b", true);
    human_read(home, &[a.clone(), b.clone()]);
    index_all(home).await;
    // The seeding's own writes queued both sessions; this test's changes start from an empty queue.
    s.conn.execute("DELETE FROM source_event", []).unwrap();
    let before = human_read(home, &[a.clone(), b.clone()]);
    assert_eq!(before["stories"][&a]["totals"]["reviewTasks"], 0);
    assert!(before["stories"][&a].get("refreshing").is_none());

    // A task writer marks its story.
    seed_reviews(&s, &a, 2);
    let marked = human_read(home, &[a.clone(), b.clone()]);
    assert_eq!(marked["stories"][&a]["refreshing"], true);
    assert!(marked["stories"][&b].get("refreshing").is_none());
    let due = next_jobs(&s, now_ms() + MIN_REFRESH_MS + SETTLE_MS, 10).unwrap();
    assert_eq!(
        due.iter().map(|j| j.story_id.as_str()).collect::<Vec<_>>(),
        [a.as_str()]
    );
    let worker = Arc::new(Mutex::new(
        UsageStore::open(&home.join("usage.sqlite")).unwrap(),
    ));
    // The mark's clock is SQLite's: start the compute clearly after it.
    std::thread::sleep(Duration::from_millis(5));
    refresh(home, &worker, due[0].clone()).await;
    let after = human_read(home, &[a.clone(), b.clone()]);
    assert_eq!(after["stories"][&a]["totals"]["reviewTasks"], 2);
    assert!(after["stories"][&a].get("refreshing").is_none());
    assert_eq!(after["stories"][&b], before["stories"][&b]);

    // A session's new usage marks the story it was spent on.
    let e = event(
        "b-e2",
        "b",
        "gpt-6-sol",
        1_790_000_000_100,
        Tokens {
            input: 5,
            ..Default::default()
        },
    );
    s.ingest(&[e]).unwrap();
    sweep_sources(&s, home, now_ms()).unwrap();
    let usage = human_read(home, &[a.clone(), b.clone()]);
    assert_eq!(usage["stories"][&b]["refreshing"], true);
    assert!(usage["stories"][&a].get("refreshing").is_none());
}

fn commit(repo: &git2::Repository, refname: &str, parents: &[git2::Oid], message: &str) -> git2::Oid {
    let sig = git2::Signature::now("t", "t@example.com").unwrap();
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    let parents: Vec<_> = parents.iter().map(|p| repo.find_commit(*p).unwrap()).collect();
    let parents: Vec<&git2::Commit> = parents.iter().collect();
    repo.commit(Some(refname), &sig, &sig, message, &tree, &parents)
        .unwrap()
}

fn dirty(s: &UsageStore, story: &str) -> bool {
    s.conn
        .query_row(
            "SELECT dirty_since_ms IS NOT NULL FROM story_totals WHERE story_key = lower(?1)",
            params![story],
            |r| r.get(0),
        )
        .unwrap()
}

/// Case 3 (I2), git: a commit on a story's branch marks that story; a merge landing on the
/// default branch marks the story it names; nothing else is marked. Ref moves made while the app
/// was closed count (the last tips are persisted).
#[test]
fn case3_branch_commits_and_landed_merges_mark_only_their_stories() {
    let (dir, s) = store();
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(repo_dir.path()).unwrap();
    let root = commit(&repo, "refs/remotes/origin/main", &[], "root");
    let one = commit(&repo, "refs/heads/feat/SV-GIT-1-x", &[root], "one");
    let two = commit(&repo, "refs/heads/fix/SV-GIT-2-y", &[root], "two");
    let stories = ids("SV-GIT", 4);
    read_blocking(dir.path(), &stories, &CallerScope::Human, false).unwrap();
    let repos = serde_json::to_string(&[repo.path().to_string_lossy()]).unwrap();
    s.conn
        .execute(
            "UPDATE story_totals SET totals = '{}', unavailable = '{}', repos = ?1, computed_at_ms = 1",
            params![repos],
        )
        .unwrap();
    let first = 10_000_000;
    s.conn
        .execute(
            "UPDATE story_totals SET computed_at_ms = ?1 WHERE story_key = 'sv-git-0'",
            params![first - 1_000],
        )
        .unwrap();
    poll_refs(&s, first).unwrap();
    assert!(
        stories.iter().all(|id| !dirty(&s, id)),
        "the first read of a repository is its baseline: it marks nothing, not even a story computed just before"
    );
    s.conn
        .execute("UPDATE story_totals SET dirty_since_ms = NULL, git_dirty_ms = NULL", [])
        .unwrap();

    commit(&repo, "refs/heads/feat/SV-GIT-1-x", &[one], "more");
    // A new store connection: the tips survived (as across a restart).
    let reopened = UsageStore::open(&dir.path().join("usage.sqlite")).unwrap();
    poll_refs(&reopened, first + 10).unwrap();
    assert!(dirty(&s, "SV-GIT-1"));
    assert!(!dirty(&s, "SV-GIT-2") && !dirty(&s, "SV-GIT-3"));

    commit(
        &repo,
        "refs/remotes/origin/main",
        &[root, two],
        "Merge pull request #7 from acme/fix/SV-GIT-2-y",
    );
    commit(
        &repo,
        "refs/remotes/origin/main",
        &[repo.refname_to_id("refs/remotes/origin/main").unwrap()],
        "SV-GIT-3: squashed (#8)",
    );
    poll_refs(&s, first + 20).unwrap();
    assert!(dirty(&s, "SV-GIT-2") && dirty(&s, "SV-GIT-3"));
    assert!(!dirty(&s, "SV-GIT-0"));
}

/// Case 5 (I1): for a sample of 20 stories (no sessions, priced, unpriced, reviewed), storyTotals
/// equals story_changes' totals and unavailable reasons.
#[tokio::test]
async fn case5_a_sample_of_twenty_equals_story_changes_totals_and_reasons() {
    let (dir, mut s) = store();
    let home = dir.path();
    let stories = ids("SV-EQ", 20);
    for (i, story) in stories.iter().enumerate() {
        match i % 4 {
            0 => {}
            1 => seed_session(&mut s, home, story, &format!("{story}-a"), true),
            2 => {
                seed_session(&mut s, home, story, &format!("{story}-a"), true);
                seed_session(&mut s, home, story, &format!("{story}-b"), false);
            }
            _ => {
                seed_session(&mut s, home, story, &format!("{story}-a"), true);
                seed_reviews(&s, story, i % 3 + 1);
            }
        }
    }
    let cold = human_read(home, &stories);
    assert_eq!(cold["pending"].as_array().unwrap().len(), 20);
    index_all(home).await;
    let answer = human_read(home, &stories);
    assert_eq!(answer["pending"], json!([]));
    for story in &stories {
        let params: story_changes::Params =
            serde_json::from_value(json!({"storyId": story})).unwrap();
        let full = serde_json::to_value(
            story_changes::read(home, &params, &CallerScope::Human)
                .await
                .unwrap(),
        )
        .unwrap();
        let row = &answer["stories"][story];
        assert_eq!(row["totals"], full["totals"], "{story}");
        assert_eq!(expand(&answer, row), full["unavailable"], "{story}");
        assert!(row.get("refreshing").is_none(), "{story}");
        let merged: Vec<Option<i64>> = full["prs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pr| pr["mergedAt"].as_i64())
            .collect();
        assert_eq!(row["prsTotal"], merged.len(), "{story}");
        assert_eq!(row["mergedAt"], json!(merged.iter().flatten().max()), "{story}");
        assert_eq!(row["prs"].as_array().unwrap().len(), merged.len().min(story_changes::PRS_SHOWN));
        let reported = if full["totals"]["sessions"] == 0 {
            json!([])
        } else {
            json!([event("x", "x", "m", 0, Tokens::default()).cli.as_str()])
        };
        assert_eq!(row["runtimes"], reported, "{story}");
    }
}

/// Case 10 (I3): hosted answers keep only the reasons storyChanges publishes and no error text;
/// a story computed in one scope is not answered in another.
#[tokio::test]
async fn case10_published_answers_keep_published_reasons_and_scopes_stay_apart() {
    let (dir, mut s) = store();
    let home = dir.path();
    let (seen, broken) = ("SV-PUB-1".to_string(), "SV-PUB-2".to_string());
    seed_session(&mut s, home, &seen, "pub", true);
    let both = [seen.clone(), broken.clone()];
    human_read(home, &both);
    index_all(home).await;
    s.conn
        .execute(
            "UPDATE story_totals SET totals = NULL, error = '/Users/me/secret: no such repo'
             WHERE story_key = lower(?1)",
            params![broken],
        )
        .unwrap();
    let private = human_read(home, &both);
    assert!(expand(&private, &private["stories"][&seen])
        .as_object()
        .unwrap()
        .contains_key("time"));
    assert_eq!(
        private["stories"][&broken]["error"],
        "/Users/me/secret: no such repo"
    );

    let published = read_blocking(home, &both, &CallerScope::Human, true).unwrap();
    let story = &published["stories"][&seen];
    for field in story["unavailable"].as_object().unwrap().keys() {
        assert!(PUBLISHED_REASONS.contains(&field.as_str()), "{field} published");
    }
    assert_eq!(story["totals"], private["stories"][&seen]["totals"]);
    let referenced: BTreeSet<u64> = story["unavailable"]
        .as_object()
        .unwrap()
        .values()
        .map(|i| i.as_u64().unwrap())
        .collect();
    assert_eq!(referenced.len(), published["reasons"].as_array().unwrap().len());
    assert_eq!(published["stories"][&broken]["error"], "failed");
    for entry in published["stories"].as_object().unwrap().values() {
        for key in entry.as_object().unwrap().keys() {
            assert!(
                [
                    "totals", "unavailable", "computedAtMs", "refreshing", "pending", "error",
                    "mergedAt", "mergedAtReason", "prs", "prsTotal", "runtimes",
                    // The activity times and the open count are published facts (times and a count;
                    // the reason is a fixed sentence).
                    "firstActivityAt", "lastActivityAt", "openSessions", "activityReason",
                    // Lines on unmerged branches: counts, like the totals' lines, and a closed reason.
                    "linesInProgress", "linesInProgressReason",
                ]
                .contains(&key.as_str()),
                "{key}"
            );
        }
    }

    // Outside the scope it was computed in, a story is not answered: it is queued there.
    let elsewhere = read_blocking(home, &both, &CallerScope::Unknown, true).unwrap();
    assert_eq!(elsewhere["pending"], json!(both));
}

/// Case 10 (I3): the hosted entry is declared, pinned like storyChanges (every listed id must
/// pass), required for viewers, and refuses other args.
#[cfg(feature = "dashboard-publish")]
#[tokio::test]
async fn case10_hosted_storytotals_is_declared_pinned_and_strict() {
    use crate::dashboard_publish::{catalog, manifest};
    let def = catalog::find(catalog::CATALOG, "storyTotals").expect("storyTotals is declared");
    assert_eq!(def.args, ["storyIds"]);
    assert!(catalog::effective_fields(def, None).is_empty());
    for args in [json!({"storyIds": ["SV-1"], "limit": 1}), json!({}), json!({"storyIds": "SV-1"})] {
        let got = (def.run)(Default::default(), catalog::Call { args: args.clone(), ..Default::default() }).await;
        assert!(matches!(got, Err(catalog::QueryError::InvalidArgs(_))), "{args}: {got:?}");
    }
    let pins: manifest::LiveArgs =
        serde_json::from_value(json!({"storyTotals": {"storyIds": ["SV-1", "SV-2"]}})).unwrap();
    let allows = |args: Value| manifest::pin_allows(&pins, "storyTotals", &args, |_, _| false);
    assert!(allows(json!({"storyIds": ["SV-2", "SV-1"]})));
    assert!(!allows(json!({"storyIds": ["SV-1", "SV-3"]})));
    assert!(!allows(json!({"storyIds": []})));
    assert!(!allows(json!({"storyIds": "SV-1"})));
    assert!(!allows(json!({})));
    let selectors: manifest::LiveArgs =
        serde_json::from_value(json!({"storyTotals": {"storyIds": [{"epic": "SV-CDE"}]}})).unwrap();
    assert!(manifest::pin_allows(&selectors, "storyTotals", &json!({"storyIds": ["SV-CDE-1", "SV-CDE-2"]}), |_, id| id.starts_with("SV-CDE-")));
    assert!(!manifest::pin_allows(&selectors, "storyTotals", &json!({"storyIds": ["SV-CDE-1", "SV-ADT-2"]}), |_, id| id.starts_with("SV-CDE-")));
    assert_eq!(
        manifest::missing_pin(
            &manifest::LiveQueries::List(vec!["storyTotals".into()]),
            &Default::default()
        ),
        Some(("storyTotals", "storyIds"))
    );
}

/// I3: 500 stories with large totals, every reason and a refresh pending fit one answer; past
/// the budget the rest are listed, never dropped.
#[test]
fn five_hundred_worst_case_stories_fit_one_answer_or_list_the_rest() {
    let (dir, s) = store();
    let all = ids("SV-BIG", 500);
    read_blocking(dir.path(), &all, &CallerScope::Human, false).unwrap();
    let totals = json!({"sessions": 9999, "commits": 9999, "files": 9999, "costUsd": 123456.78901234567_f64,
        "wallMs": 99_999_999_999_i64, "activeMs": 99_999_999_999_i64, "idleMs": 99_999_999_999_i64, "reviewRounds": 999, "reviewTasks": 999,
        "lines": {"ai": {"total": 999_999, "byRuntime": {"claude": 999_999, "codex": 999_999, "opencode": 999_999, "hermes": 999_999, "other": 999_999}},
            "human": 999_999, "unattributed": 999_999, "total": 999_999}});
    let unavailable = json!({
        "costUsd": "one or more sessions have no complete priced usage",
        "time": "one or more sessions have no observed timing; time covers observed sessions only",
        "commits": "history objects not in local copy",
        "fileLines": "story spans branches with no single final tree",
        "planned": "story has no Touchpoints section",
        "attribution": "history objects not in local copy",
        "actual": "history objects not in local copy",
        "lines": "history objects not in local copy"
    });
    let facets = json!({
        "mergedAt": 1_793_318_400_000_i64,
        "prs": (0..story_changes::PRS_SHOWN).map(|i| json!({"number": 99_990 + i, "merged": 1_793_318_400_000_i64})).collect::<Vec<_>>(),
        "prsTotal": 99,
        "runtimes": ["claude", "codex", "hermes", "opencode"],
    });
    s.conn
        .execute(
            "UPDATE story_totals SET totals = ?1, unavailable = ?2, facets = ?3,
                computed_at_ms = 1793318400000, dirty_since_ms = 1793318400000",
            params![totals.to_string(), unavailable.to_string(), facets.to_string()],
        )
        .unwrap();
    let answer = read_blocking(dir.path(), &all, &CallerScope::Human, false).unwrap();
    let size = serde_json::to_vec(&answer).unwrap().len();
    assert!(size < 256 * 1024, "{size}");
    let answered = answer["stories"].as_object().unwrap().len();
    let rest = answer["rest"].as_array().map_or(0, Vec::len);
    assert_eq!(answered + rest, 500);
    assert_eq!(answer["truncated"] == true, rest > 0);
}

#[test]
fn ids_are_one_to_five_hundred_non_blank_and_asked_once() {
    assert!(asked(&[]).unwrap_err().starts_with("invalid:"));
    assert!(asked(&ids("SV", 501)).unwrap_err().starts_with("invalid:"));
    assert!(asked(&["SV-1".into(), " ".into()]).unwrap_err().starts_with("invalid:"));
    assert_eq!(
        asked(&["SV-1".into(), "sv-1".into(), "SV-2".into()]).unwrap(),
        ["SV-1", "SV-2"]
    );
}

#[test]
fn story_tokens_find_ids_in_branches_subjects_and_paths() {
    let has = |text: &str, id: &str| story_tokens(text).contains(&id.to_string());
    assert!(has("refs/heads/fix/SV-PBF-287-pr-b-totals-index", "sv-pbf-287"));
    assert!(has("Merge pull request #2771 from alderco/fix/SV-PBF-267-pr2-single", "sv-pbf-267"));
    assert!(has("SV-PBF-267 PR 2: single-path context file list", "sv-pbf-267"));
    assert!(has("stories/pre-beta-fixes/SV-PBF-274-story-totals-cached-index.md", "sv-pbf-274"));
    assert!(story_tokens("refs/heads/main").is_empty());
    assert!(story_tokens("2026-10-29").is_empty());
}

/// This process's CPU time so far (user + system).
fn cpu() -> Duration {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: getrusage fills the struct it is given.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let time = |t: libc::timeval| {
        Duration::from_secs(t.tv_sec as u64) + Duration::from_micros(t.tv_usec as u64)
    };
    time(usage.ru_utime) + time(usage.ru_stime)
}

/// The PR's measurement, run once at the gate: a Stories-equivalent read of this Mac's stories,
/// before (one storyChanges per story, as the page does today) and after (storyTotals in calls
/// of 500 on a warm index), with wall time and this process's CPU, and the index's steady state
/// (5 stories with active sessions for 2 minutes). It runs under a temporary HOME whose
/// `.codesense` links to the real one's content read-only by convention, with a `VACUUM INTO`
/// snapshot of the usage store and its own empty `caches/`, so the real usage store and caches are
/// never written. STORY_TOTALS_IDS: a file of story ids, one per line. STORY_TOTALS_SAMPLE: how
/// many stories the "before" and the cold compute time (default 20; the totals are scaled from
/// it). STORY_TOTALS_REPORT: where to write the numbers as JSON.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "measures this Mac's own data; run by hand at the gate"]
async fn measure_a_stories_read_before_and_after() {
    let real = crate::commands::paths::codesense_home_dir().unwrap();
    let all: Vec<String> = std::fs::read_to_string(std::env::var("STORY_TOTALS_IDS").unwrap())
        .unwrap()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(String::from)
        .collect();
    let sample: usize = std::env::var("STORY_TOTALS_SAMPLE")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(20)
        .min(all.len());
    let dir = tempfile::tempdir().unwrap();
    let home_dir = dir.path().join(".codesense");
    let home = home_dir.as_path();
    std::fs::create_dir_all(home.join("caches")).unwrap();
    for entry in std::fs::read_dir(&real).unwrap().flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("usage.sqlite") && name != "caches" {
            std::os::unix::fs::symlink(entry.path(), home.join(entry.file_name())).unwrap();
        }
    }
    // Anything that finds the home through HOME finds the temporary one.
    std::env::set_var("HOME", dir.path());
    rusqlite::Connection::open_with_flags(
        real.join("usage.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
    .execute(
        "VACUUM INTO ?1",
        params![home.join("usage.sqlite").to_string_lossy()],
    )
    .unwrap();
    let scope = CallerScope::Human;
    let timed = |wall: std::time::Instant, cpu0: Duration| (wall.elapsed(), cpu() - cpu0);

    // Before: one storyChanges per story.
    let (wall, cpu0) = (std::time::Instant::now(), cpu());
    for id in &all[..sample] {
        let params: story_changes::Params =
            serde_json::from_value(json!({"storyId": id})).unwrap();
        story_changes::read(home, &params, &scope).await.unwrap();
    }
    let (before_wall, before_cpu) = timed(wall, cpu0);
    let scale = all.len() as f64 / sample as f64;

    // The index's own compute of the same stories (the cold backfill's cost per story).
    for chunk in all.chunks(MAX_IDS) {
        read_blocking(home, chunk, &scope, false).unwrap();
    }
    let worker = Arc::new(Mutex::new(
        UsageStore::open(&home.join("usage.sqlite")).unwrap(),
    ));
    let (wall, cpu0) = (std::time::Instant::now(), cpu());
    for id in &all[..sample] {
        let job = Job {
            scope_key: describe(&scope).key,
            story_id: id.trim().to_owned(),
            kind: "human".into(),
            root: None,
            git: None,
            earlier_git: None,
            dirty_at_ms: None,
        };
        refresh(home, &worker, job).await;
    }
    let (compute_wall, compute_cpu) = timed(wall, cpu0);
    // Warm the rest with a computed row: a read's cost does not depend on the values.
    let store = UsageStore::open(&home.join("usage.sqlite")).unwrap();
    store
        .conn
        .execute(
            "UPDATE story_totals SET totals = (SELECT totals FROM story_totals WHERE totals IS NOT NULL LIMIT 1),
                unavailable = '{}', computed_at_ms = 1 WHERE totals IS NULL",
            [],
        )
        .unwrap();

    // After: storyTotals in calls of 500.
    let (wall, cpu0) = (std::time::Instant::now(), cpu());
    let mut answered = 0;
    for chunk in all.chunks(MAX_IDS) {
        let answer = read_blocking(home, chunk, &scope, true).unwrap();
        assert!(serde_json::to_vec(&answer).unwrap().len() < 256 * 1024);
        answered += answer["stories"].as_object().unwrap().len();
    }
    let (after_wall, after_cpu) = timed(wall, cpu0);
    assert_eq!(answered, all.len());

    // Steady state: 5 sampled stories' sessions change every 5 s for 2 minutes; the worker
    // recomputes their session half (the git half is kept), at most every 30 s each.
    let active: BTreeSet<String> = all[..sample.min(5)].iter().map(|id| id.to_lowercase()).collect();
    let (wall, cpu0) = (std::time::Instant::now(), cpu());
    let mut recomputed = 0;
    while wall.elapsed() < Duration::from_secs(120) {
        mark(&store, &active, now_ms(), Half::Session).unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
        let jobs = next_jobs(&store, now_ms(), WORKERS).unwrap();
        for job in jobs {
            recomputed += 1;
            refresh(home, &worker, job).await;
        }
    }
    let (steady_wall, steady_cpu) = timed(wall, cpu0);
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let report = json!({
        "stories": all.len(),
        "sample": sample,
        "before": {"sampleWallMs": ms(before_wall), "sampleCpuMs": ms(before_cpu),
            "allWallMs": ms(before_wall) * scale, "allCpuMs": ms(before_cpu) * scale},
        "indexCompute": {"sampleWallMs": ms(compute_wall), "sampleCpuMs": ms(compute_cpu),
            "allWallMs": ms(compute_wall) * scale, "allCpuMs": ms(compute_cpu) * scale},
        "after": {"calls": all.chunks(MAX_IDS).len(), "wallMs": ms(after_wall), "cpuMs": ms(after_cpu)},
        "steadyState": {"activeStories": active.len(), "wallMs": ms(steady_wall), "cpuMs": ms(steady_cpu),
            "recomputes": recomputed, "cpuShare": ms(steady_cpu) / ms(steady_wall)},
    });
    println!("{report:#}");
    if let Ok(path) = std::env::var("STORY_TOTALS_REPORT") {
        std::fs::write(path, format!("{report:#}")).unwrap();
    }
}

/// Rows that look computed (by this version), without running the computation.
fn fake_computed(home: &Path, ids: &[String]) -> UsageStore {
    read_blocking(home, ids, &CallerScope::Human, false).unwrap();
    let store = UsageStore::open(&home.join("usage.sqlite")).unwrap();
    store
        .conn
        .execute(
            "UPDATE story_totals SET totals = '{}', unavailable = '{}', computed_at_ms = 1, version = ?1",
            params![story_changes::TOTALS_VERSION],
        )
        .unwrap();
    store
}

fn refreshing(home: &Path, id: &str) -> bool {
    human_read(home, &[id.to_owned()])["stories"][id]["refreshing"] == true
}

/// H1: a session's new usage marks its story by every basis `story_changes` resolves it by: a
/// stored link, a story worktree holding its folder, its launch record through a late
/// attribution, and another machine's usage; and a price change marks every story.
#[tokio::test]
async fn h1_every_basis_a_session_resolves_by_marks_its_story() {
    let (dir, mut s) = store();
    let home = dir.path();
    let ids: Vec<String> = ["SV-LINK-1", "SV-CWD-1", "SV-ATT-1", "SV-REM-1", "SV-NONE-1"]
        .map(String::from)
        .to_vec();
    // The worktree that ties a folder to SV-CWD-1, and the launch records.
    let worktree = home.join("wt-cwd");
    std::fs::create_dir_all(&worktree).unwrap();
    let source = home.join("SV-CWD-1.md");
    std::fs::write(&source, "---\nstatus: in-progress\n---\n").unwrap();
    let record = crate::install::worktree_index::WorktreeStoryRecord::new(
        worktree.clone(),
        "acme",
        crate::domain::SourceRepoSlug::new("acme", "code").unwrap(),
        "SV-CWD-1",
        &source,
    );
    crate::install::worktree_index::write_record(home, &crate::domain::Principal::from(1u64), &record)
        .unwrap();
    for (session, story) in [("launch-att", "SV-ATT-1"), ("remote-1", "SV-REM-1")] {
        let mut r = SessionRecord::new(session);
        r.story_id = Some(story.into());
        s.write_session_record(&r).unwrap();
    }
    seed_session(&mut s, home, "SV-LINK-1", "linked", true);
    let store2 = fake_computed(home, &ids);
    sweep_sources(&store2, home, now_ms()).unwrap();
    store2
        .conn
        .execute("UPDATE story_totals SET dirty_since_ms = NULL, dirty_at_ms = NULL", [])
        .unwrap();
    assert!(ids.iter().all(|id| !refreshing(home, id)));

    let usage = |id: &str, session: &str, cwd: Option<&Path>| {
        let mut e = event(id, session, "gpt-6-sol", 1_790_000_000_500, Tokens { input: 1, ..Default::default() });
        e.cwd = cwd.map(|p| p.to_string_lossy().into_owned());
        e
    };
    // A stored link; a folder inside a story worktree; usage first, then its late attribution to
    // a launch whose record names the story; another machine's usage.
    s.ingest(&[usage("e-link", "linked", None), usage("e-cwd", "cwd-only", Some(worktree.join("src").as_path()))]).unwrap();
    s.ingest(&[usage("e-att", "native-att", None)]).unwrap();
    s.conn
        .execute(
            "INSERT INTO session_attribution (session_id, org, agent, handle, attributed_at_ms, launch_id)
             VALUES ('native-att', 'acme', 'dev', 'tab', 1, 'launch-att')",
            [],
        )
        .unwrap();
    s.conn
        .execute(
            "INSERT INTO usage_event (machine_id, cli, event_id, session_id, day, at_ms, model, input, output, cache_read, cache_write)
             SELECT 'remote-mac', cli, 'e-remote', 'remote-1', day, at_ms, model, 1, 0, 0, 0 FROM usage_event WHERE event_id = 'e-link'",
            [],
        )
        .unwrap();
    sweep_sources(&s, home, now_ms()).unwrap();
    for id in ["SV-LINK-1", "SV-CWD-1", "SV-ATT-1", "SV-REM-1"] {
        assert!(refreshing(home, id), "{id} was not marked");
    }
    assert!(!refreshing(home, "SV-NONE-1"));
    // Resolving the folder session keeps its story link (`report_with` writes it), and that write is
    // an event of the session: it is queued again while the sweep applies it, and a sweep deletes
    // only the queue row it read (so the event is not lost). The next sweep finds the stored link,
    // writes nothing, and drains the queue. (Before the rows, the sweep deleted unconditionally.)
    sweep_sources(&s, home, now_ms()).unwrap();
    let queued: i64 = s.conn.query_row("SELECT count(*) FROM source_event", [], |r| r.get(0)).unwrap();
    assert_eq!(queued, 0, "two sweeps drain the queue");

    // A price change marks every story.
    s.conn
        .execute("UPDATE story_totals SET dirty_since_ms = NULL, dirty_at_ms = NULL", [])
        .unwrap();
    s.conn
        .execute(
            "UPDATE model_rate SET input_per_mtok = input_per_mtok WHERE rowid = (SELECT min(rowid) FROM model_rate)",
            [],
        )
        .unwrap();
    assert!(ids.iter().all(|id| refreshing(home, id)));
}

/// M1: a commit on a branch a story followed marks it whatever the branch is named, and a merge
/// whose second parent is that branch's tip marks it whatever its subject says.
#[test]
fn m1_branches_a_story_followed_and_merges_of_their_tips_mark_it() {
    let (dir, s) = store();
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(repo_dir.path()).unwrap();
    let root = commit(&repo, "refs/remotes/origin/main", &[], "root");
    let feature = commit(&repo, "refs/heads/feature-x", &[root], "x");
    let work = commit(&repo, "refs/heads/work-y", &[root], "y");
    let stories = ["SV-BR-1".to_string(), "SV-BR-2".to_string(), "SV-BR-3".to_string()];
    let store = fake_computed(dir.path(), &stories);
    let repos = serde_json::to_string(&[repo.path().to_string_lossy()]).unwrap();
    for (story, branches) in [("sv-br-1", r#"["feature-x"]"#), ("sv-br-2", r#"["work-y"]"#), ("sv-br-3", r#"["other"]"#)] {
        store
            .conn
            .execute(
                "UPDATE story_totals SET repos = ?1, branches = ?2 WHERE story_key = ?3",
                params![repos, branches, story],
            )
            .unwrap();
    }
    let first = 10_000_000;
    poll_refs(&s, first).unwrap();
    assert!(stories.iter().all(|id| !dirty(&s, id)));

    commit(&repo, "refs/heads/feature-x", &[feature], "more x");
    poll_refs(&s, first + 10).unwrap();
    assert!(dirty(&s, "SV-BR-1"));
    assert!(!dirty(&s, "SV-BR-2") && !dirty(&s, "SV-BR-3"));

    commit(&repo, "refs/remotes/origin/main", &[root, work], "Merge pull request #9: Trail facts");
    poll_refs(&s, first + 20).unwrap();
    assert!(dirty(&s, "SV-BR-2"), "the merge of work-y's tip marks the story that followed it");
    assert!(!dirty(&s, "SV-BR-3"));
}

/// M2: a review record marks the story its `story:` names (whatever the file is called), and a
/// capped context change marks every story.
#[test]
fn m2_review_records_mark_their_story_and_capped_changes_wait_for_a_ready_index() {
    let (dir, s) = store();
    let stories = ["SV-REV-1".to_string(), "SV-REV-2".to_string()];
    let store = fake_computed(dir.path(), &stories);
    let rounds = dir.path().join("team/content/reviews/rounds");
    std::fs::create_dir_all(&rounds).unwrap();
    let round = rounds.join("alderco-codesense-2790-r1-abcdef.md");
    std::fs::write(&round, "---\nround: 1\nstory: SV-REV-1\n---\n").unwrap();
    let marks = ContextMarks {
        paths: BTreeSet::from([round.to_string_lossy().into_owned()]),
        capped: false,
    };
    apply_context(&store, Path::new("/home"), marks, 1_000, None).unwrap();
    assert!(dirty(&s, "SV-REV-1"));
    assert!(!dirty(&s, "SV-REV-2"));
    let git_dirty: Option<i64> = s
        .conn
        .query_row("SELECT git_dirty_ms FROM story_totals WHERE story_key = 'sv-rev-1'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(git_dirty, None, "a review record changes the session half only");

    // A capped change with no ready index to compare with waits, and marks nothing.
    apply_context(&store, Path::new("/home"), ContextMarks { paths: BTreeSet::new(), capped: true }, 2_000, None).unwrap();
    assert!(!dirty(&s, "SV-REV-2"));
    assert!(std::mem::take(&mut context_marks().lock().unwrap().capped), "it is kept for the next turn");

    context_changed(r#"{"generation":1,"paths":["/x/stories/SV-REV-2-thing.md"],"orgs":[],"allOrgs":false,"truncated":true}"#);
    let queued = std::mem::take(&mut *context_marks().lock().unwrap());
    assert!(queued.capped);
    assert!(queued.paths.contains("/x/stories/SV-REV-2-thing.md"));
}

/// A ready index snapshot holding one story file per `(path, id, mtime)`, sorted by path.
fn snapshot_at(files: &[(&str, &str, u64)]) -> Snapshot {
    // The index's order: by path components, as `BTreeMap<PathBuf, _>` keeps them.
    let mut files = files.to_vec();
    files.sort_by(|a, b| Path::cmp(Path::new(a.0), Path::new(b.0)));
    let records = files
        .iter()
        .map(|(path, id, mtime)| {
            let file_name = path.rsplit('/').next().unwrap().to_owned();
            Arc::new(ContextRecord {
                headings: Vec::new(),
                path: (*path).to_owned(),
                rel_path: file_name.clone(),
                file_name,
                org: None,
                team: None,
                layer: crate::context_query::model::Layer::Team,
                kind: crate::context_query::model::Kind::Story,
                mtime_ms: Some(*mtime),
                title: None,
                fields: BTreeMap::from([("story_id".to_owned(), json!(id))]),
                is_pipeline_doc: true,
                metadata_truncated: false,
                metadata_valid: true,
                summary: None,
            })
        })
        .collect();
    Snapshot::build(records, Vec::new(), 1, true)
}

/// One story file per `(id, mtime)` under `/ctx/stories`.
fn snapshot_of(files: &[(&str, u64)]) -> Snapshot {
    let paths: Vec<(String, &str, u64)> = files.iter().map(|(id, mtime)| (format!("/ctx/stories/{id}-thing.md"), *id, *mtime)).collect();
    snapshot_at(&paths.iter().map(|(path, id, mtime)| (path.as_str(), *id, *mtime)).collect::<Vec<_>>())
}

fn capped(store: &UsageStore, snapshot: &Snapshot) {
    let marks = ContextMarks { paths: BTreeSet::new(), capped: true };
    apply_context(store, Path::new("/home"), marks, 1_000, Some(snapshot)).unwrap();
}

/// What a finished compute leaves: nothing dirty.
fn settle(s: &UsageStore) {
    s.conn.execute("UPDATE story_totals SET dirty_since_ms = NULL, dirty_at_ms = NULL, git_dirty_ms = NULL", []).unwrap();
}

fn dirty_count(s: &UsageStore) -> i64 {
    s.conn.query_row("SELECT COUNT(*) FROM story_totals WHERE dirty_since_ms IS NOT NULL", [], |r| r.get(0)).unwrap()
}

/// SV-CDE-101: every start replaces the index, so the change is capped; with the same story files
/// as the last start, nothing is marked.
#[test]
fn a_boot_twice_on_an_unchanged_store_marks_no_row() {
    let (dir, s) = store();
    let stories = ["SV-SET-1".to_string(), "SV-SET-2".to_string(), "SV-SET-3".to_string()];
    fake_computed(dir.path(), &stories);
    let files = snapshot_of(&[("sv-set-1", 10), ("sv-set-2", 20), ("sv-set-3", 30)]);
    capped(&s, &files);
    assert_eq!(dirty_count(&s), 3, "the first start has no settled inputs to compare with");
    settle(&s);
    capped(&s, &files);
    assert_eq!(dirty_count(&s), 0);
}

/// SV-CDE-101: a capped change marks the stories whose files changed (or went), and no other.
#[test]
fn a_capped_change_marks_only_the_stories_whose_files_changed() {
    let (dir, s) = store();
    let stories = ["SV-SET-1".to_string(), "SV-SET-2".to_string(), "SV-SET-3".to_string(), "SV-SET-4".to_string()];
    fake_computed(dir.path(), &stories);
    capped(&s, &snapshot_of(&[("sv-set-1", 10), ("sv-set-2", 20), ("sv-set-3", 30), ("sv-set-4", 40)]));
    settle(&s);
    // Story 1 is edited, story 3's file is gone; 2 and 4 are as they were.
    capped(&s, &snapshot_of(&[("sv-set-1", 11), ("sv-set-2", 20), ("sv-set-4", 40)]));
    assert!(dirty(&s, "SV-SET-1"));
    assert!(dirty(&s, "SV-SET-3"));
    assert!(!dirty(&s, "SV-SET-2"));
    assert!(!dirty(&s, "SV-SET-4"));
    // Their session rows are rebuilt whole by the next compute; the others keep theirs.
    let built = |story: &str| -> i64 {
        s.conn
            .query_row(
                "SELECT COUNT(*) FROM contribution WHERE entity_kind = ?1 AND entity_id = ?2 AND source_kind = 'meta'",
                params![<story_changes::StoryRows as EntityAdapter>::KIND, story],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(built("sv-set-1"), 0);
    assert_eq!(built("sv-set-3"), 0);
}

/// SV-CDE-101: a change by paths records the new inputs, so a restart does not mark them again.
#[test]
fn a_change_by_paths_is_settled_for_the_next_start() {
    let (dir, s) = store();
    fake_computed(dir.path(), &["SV-SET-1".to_string(), "SV-SET-2".to_string()]);
    capped(&s, &snapshot_of(&[("sv-set-1", 10), ("sv-set-2", 20)]));
    settle(&s);
    let edited = snapshot_of(&[("sv-set-1", 11), ("sv-set-2", 20)]);
    let marks = ContextMarks { paths: BTreeSet::from(["/ctx/stories/sv-set-1-thing.md".to_owned()]), capped: false };
    apply_context(&s, Path::new("/home"), marks, 2_000, Some(&edited)).unwrap();
    assert_eq!(dirty_count(&s), 1);
    settle(&s);
    capped(&s, &edited);
    assert_eq!(dirty_count(&s), 0);
}

/// SV-CDE-101: a boot's capped event still lists its first paths; they mark nothing for stories
/// whose files are as they were.
#[test]
fn a_capped_event_that_lists_paths_marks_nothing_on_the_second_boot() {
    let (dir, s) = store();
    fake_computed(dir.path(), &["SV-SET-1".to_string(), "SV-SET-2".to_string()]);
    let files = snapshot_of(&[("sv-set-1", 10), ("sv-set-2", 20)]);
    let boot = || ContextMarks {
        paths: BTreeSet::from(["/ctx/stories/sv-set-1-thing.md".to_owned(), "/ctx/stories/sv-set-2-thing.md".to_owned()]),
        capped: true,
    };
    apply_context(&s, Path::new("/home"), boot(), 1_000, Some(&files)).unwrap();
    assert_eq!(dirty_count(&s), 2);
    settle(&s);
    apply_context(&s, Path::new("/home"), boot(), 2_000, Some(&files)).unwrap();
    assert_eq!(dirty_count(&s), 0);
}

/// SV-CDE-101: siblings whose names differ by a `-suffix` sort differently as text and as path
/// components; a path batch finds its file in the index's order, so a restart finds it settled.
#[test]
fn a_path_batch_finds_sibling_names_in_the_indexs_order() {
    let (dir, s) = store();
    fake_computed(dir.path(), &["SV-A-1".to_string()]);
    let files = snapshot_at(&[
        ("/ctx/sv-a-1/s.md", "sv-a-1", 10),
        ("/ctx/sv-a-1-b/s.md", "sv-a-1", 20),
        ("/ctx/sv-a-1.5/s.md", "sv-a-1", 30),
    ]);
    capped(&s, &files);
    settle(&s);
    for path in ["/ctx/sv-a-1/s.md", "/ctx/sv-a-1-b/s.md", "/ctx/sv-a-1.5/s.md"] {
        let marks = ContextMarks { paths: BTreeSet::from([path.to_owned()]), capped: false };
        apply_context(&s, Path::new("/home"), marks, 2_000, Some(&files)).unwrap();
    }
    settle(&s);
    capped(&s, &files);
    assert_eq!(dirty_count(&s), 0, "a path batch must keep its file's row");
}

/// SV-CDE-101: a story file moved to another team's folder, or copied into a context worktree,
/// changes what names the story, and only that story is marked.
#[test]
fn a_team_move_and_a_worktree_copy_mark_their_story() {
    let (dir, s) = store();
    fake_computed(dir.path(), &["SV-SET-1".to_string(), "SV-SET-2".to_string()]);
    let at = |t1: &str, extra: &[(&str, &str, u64)]| {
        let mut files = vec![(t1, "sv-set-1", 10), ("/ctx/a/stories/sv-set-2-thing.md", "sv-set-2", 20)];
        files.extend_from_slice(extra);
        snapshot_at(&files)
    };
    capped(&s, &at("/ctx/a/stories/sv-set-1-thing.md", &[]));
    settle(&s);
    capped(&s, &at("/ctx/b/stories/sv-set-1-thing.md", &[]));
    assert!(dirty(&s, "SV-SET-1"), "the team move");
    assert!(!dirty(&s, "SV-SET-2"));
    settle(&s);
    let copy = [("/wt/sv-set-2/stories/sv-set-2-thing.md", "sv-set-2", 21)];
    capped(&s, &at("/ctx/b/stories/sv-set-1-thing.md", &copy));
    assert!(dirty(&s, "SV-SET-2"), "the worktree copy");
    assert!(!dirty(&s, "SV-SET-1"));
}

/// SV-CDE-101: a batch of paths hashes only the files it names, not the index; and a capped change
/// that arrives before the index is ready is kept whole, paths too, for the next turn.
#[test]
fn a_path_batch_costs_its_paths_and_a_capped_change_before_the_index_is_ready_waits() {
    let (dir, s) = store();
    let ids: Vec<String> = (0..50).map(|n| format!("SV-BIG-{n}")).collect();
    fake_computed(dir.path(), &ids);
    let files: Vec<(String, &str, u64)> = ids.iter().map(|id| (format!("/ctx/stories/{id}-thing.md"), id.as_str(), 10)).collect();
    let borrowed: Vec<(&str, &str, u64)> = files.iter().map(|(p, i, m)| (p.as_str(), *i, *m)).collect();
    let snapshot = snapshot_at(&borrowed);
    capped(&s, &snapshot);
    settle(&s);
    HASHED.with(|n| n.set(0));
    let marks = ContextMarks { paths: BTreeSet::from([files[3].0.clone(), files[7].0.clone()]), capped: false };
    apply_context(&s, Path::new("/home"), marks, 3_000, Some(&snapshot)).unwrap();
    assert_eq!(dirty_count(&s), 2);
    assert!(HASHED.with(|n| n.get()) <= 2, "hashed {} files for 2 paths", HASHED.with(|n| n.get()));
    // Not ready: nothing is marked, and the paths are kept with the capped flag.
    let waiting = ContextMarks { paths: BTreeSet::from([files[9].0.clone()]), capped: true };
    let mut unready = snapshot_at(&borrowed);
    unready.ready = false;
    settle(&s);
    *context_marks().lock().unwrap() = ContextMarks { paths: BTreeSet::new(), capped: false };
    apply_context(&s, Path::new("/home"), waiting, 4_000, Some(&unready)).unwrap();
    assert_eq!(dirty_count(&s), 0);
    let kept = std::mem::take(&mut *context_marks().lock().unwrap());
    assert!(kept.capped && kept.paths.contains(&files[9].0));
}

/// A story file written under `home/ctx/stories`, returning its path.
fn write_story(home: &Path, id: &str, text: &str) -> String {
    let dir = home.join("ctx/stories");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{id}-thing.md"));
    std::fs::write(&path, text).unwrap();
    path.to_str().unwrap().to_owned()
}

/// The index of the story files under `home`, each with its mtime.
fn real_snapshot(home: &Path, files: &[(&str, u64)]) -> Snapshot {
    let paths: Vec<(String, &str, u64)> = files.iter().map(|(id, mtime)| (home.join(format!("ctx/stories/{id}-thing.md")).to_str().unwrap().to_owned(), *id, *mtime)).collect();
    snapshot_at(&paths.iter().map(|(path, id, mtime)| (path.as_str(), *id, *mtime)).collect::<Vec<_>>())
}

fn boot(s: &UsageStore, home: &Path, snapshot: &Snapshot) {
    let marks = ContextMarks { paths: BTreeSet::new(), capped: true };
    apply_context(s, home, marks, 1_000, Some(snapshot)).unwrap();
}

/// SV-CDE-104: a store copied mid-drain (computed, dirty and cold rows) marks no story at a boot
/// with the same files, and none at the next.
#[test]
fn a_mid_drain_store_booted_twice_with_the_same_files_marks_nothing() {
    let (dir, s) = store();
    let files = tempfile::tempdir().unwrap();
    fake_computed(dir.path(), &["SV-MID-1".to_string(), "SV-MID-2".to_string(), "SV-MID-3".to_string()]);
    for (id, text) in [("sv-mid-1", "one"), ("sv-mid-2", "two"), ("sv-mid-3", "three")] {
        write_story(files.path(), id, text);
    }
    let snapshot = real_snapshot(files.path(), &[("sv-mid-1", 10), ("sv-mid-2", 20), ("sv-mid-3", 30)]);
    boot(&s, files.path(), &snapshot);
    settle(&s);
    s.conn.execute("UPDATE story_totals SET dirty_since_ms = 5 WHERE story_key = 'sv-mid-2'", []).unwrap();
    s.conn.execute("UPDATE story_totals SET computed_at_ms = NULL, totals = NULL WHERE story_key = 'sv-mid-3'", []).unwrap();
    for _ in 0..2 {
        boot(&s, files.path(), &snapshot);
        assert_eq!(dirty_count(&s), 1, "only the row that was already dirty");
        assert!(!dirty(&s, "SV-MID-1") && !dirty(&s, "SV-MID-3"));
    }
}

/// SV-CDE-104: a store from before the files were kept by home-relative path (an old files table with
/// rows, and computed totals) marks no story at the first boot, gives every story new rows, and
/// marks none at the next boot.
#[test]
fn a_store_with_the_old_files_table_marks_nothing_and_gets_new_rows() {
    let (dir, s) = store();
    let files = tempfile::tempdir().unwrap();
    let stories = ["SV-OLD-1".to_string(), "SV-OLD-2".to_string()];
    fake_computed(dir.path(), &stories);
    for id in ["sv-old-1", "sv-old-2"] {
        write_story(files.path(), id, id);
    }
    s.conn
        .execute_batch(
            "DROP TABLE story_totals_ctx;
             CREATE TABLE story_totals_ctx (story_key TEXT NOT NULL, path TEXT NOT NULL, hash INTEGER NOT NULL, PRIMARY KEY (story_key, path));
             INSERT INTO story_totals_ctx VALUES ('sv-old-1', '/old/home/sv-old-1-thing.md', 7), ('sv-old-2', '/old/home/sv-old-2-thing.md', 8);",
        )
        .unwrap();
    init(&s.conn).unwrap();
    let snapshot = real_snapshot(files.path(), &[("sv-old-1", 10), ("sv-old-2", 20)]);
    for _ in 0..2 {
        boot(&s, files.path(), &snapshot);
        assert_eq!(dirty_count(&s), 0);
        let rows: i64 = s.conn.query_row("SELECT COUNT(DISTINCT story_key) FROM story_totals_ctx WHERE content IS NOT NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 2, "every story has new rows");
    }
}

/// SV-CDE-104: only a rebuilt files table settles without marking; on a fresh store a story that is
/// computed after the first boot, with no rows, is marked when its file was edited while closed.
#[test]
fn a_fresh_store_marks_a_story_computed_after_a_boot_whose_file_changed() {
    let (dir, s) = store();
    let files = tempfile::tempdir().unwrap();
    fake_computed(dir.path(), &["SV-FLG-1".to_string()]);
    write_story(files.path(), "sv-flg-1", "one");
    write_story(files.path(), "sv-flg-2", "two");
    boot(&s, files.path(), &real_snapshot(files.path(), &[("sv-flg-1", 10), ("sv-flg-2", 20)]));
    settle(&s);
    fake_computed(dir.path(), &["SV-FLG-2".to_string()]);
    settle(&s);
    write_story(files.path(), "sv-flg-2", "edited while closed");
    boot(&s, files.path(), &real_snapshot(files.path(), &[("sv-flg-1", 10), ("sv-flg-2", 25)]));
    assert!(dirty(&s, "SV-FLG-2") && !dirty(&s, "SV-FLG-1"));
}

/// SV-CDE-104: a story whose file appears after a boot is marked at the next boot, once.
#[test]
fn a_story_added_after_a_boot_is_marked_once() {
    let (dir, s) = store();
    let files = tempfile::tempdir().unwrap();
    fake_computed(dir.path(), &["SV-ADD-1".to_string(), "SV-ADD-2".to_string()]);
    write_story(files.path(), "sv-add-1", "one");
    boot(&s, files.path(), &real_snapshot(files.path(), &[("sv-add-1", 10)]));
    settle(&s);
    write_story(files.path(), "sv-add-2", "two");
    let both = real_snapshot(files.path(), &[("sv-add-1", 10), ("sv-add-2", 20)]);
    boot(&s, files.path(), &both);
    assert!(dirty(&s, "SV-ADD-2") && !dirty(&s, "SV-ADD-1"));
    settle(&s);
    boot(&s, files.path(), &both);
    assert_eq!(dirty_count(&s), 0);
}

/// SV-CDE-104: a touched mtime over the same content marks nothing (and is not read again at the
/// next boot), a real change marks only its story, and the same files under another home prefix
/// mark nothing.
#[test]
fn a_touched_mtime_marks_nothing_a_real_change_marks_its_story_and_a_moved_home_marks_nothing() {
    let (dir, s) = store();
    let (home_a, home_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    fake_computed(dir.path(), &["SV-TCH-1".to_string(), "SV-TCH-2".to_string()]);
    for id in ["sv-tch-1", "sv-tch-2"] {
        write_story(home_a.path(), id, id);
    }
    boot(&s, home_a.path(), &real_snapshot(home_a.path(), &[("sv-tch-1", 10), ("sv-tch-2", 20)]));
    settle(&s);
    // A pull rewrites story 1's mtime and nothing else.
    let touched = real_snapshot(home_a.path(), &[("sv-tch-1", 99), ("sv-tch-2", 20)]);
    boot(&s, home_a.path(), &touched);
    assert_eq!(dirty_count(&s), 0, "the same content");
    HASHED.with(|n| n.set(0));
    boot(&s, home_a.path(), &touched);
    assert_eq!(HASHED.with(|n| n.get()), 0, "the new mtime was recorded; the next boot reads nothing");
    // A real edit of story 2.
    write_story(home_a.path(), "sv-tch-2", "edited");
    boot(&s, home_a.path(), &real_snapshot(home_a.path(), &[("sv-tch-1", 99), ("sv-tch-2", 100)]));
    assert!(dirty(&s, "SV-TCH-2") && !dirty(&s, "SV-TCH-1"));
    settle(&s);
    // The same files under another home.
    for id in ["sv-tch-1", "sv-tch-2"] {
        let text = std::fs::read_to_string(home_a.path().join(format!("ctx/stories/{id}-thing.md"))).unwrap();
        write_story(home_b.path(), id, &text);
    }
    boot(&s, home_b.path(), &real_snapshot(home_b.path(), &[("sv-tch-1", 99), ("sv-tch-2", 100)]));
    assert_eq!(dirty_count(&s), 0);
}

/// SV-CDE-104: a batch of rewritten mtimes reads only the files whose mtime differs.
#[test]
fn an_mtime_only_batch_reads_at_most_the_mismatched_files() {
    let (dir, s) = store();
    let files = tempfile::tempdir().unwrap();
    let ids: Vec<String> = (0..20).map(|n| format!("SV-SCAN-{n}")).collect();
    fake_computed(dir.path(), &ids);
    for id in &ids {
        write_story(files.path(), &id.to_lowercase(), id);
    }
    let at = |bumped: u64| -> Snapshot {
        let stamps: Vec<(String, u64)> = ids.iter().enumerate().map(|(n, id)| (id.to_lowercase(), if n < 5 { 10 + bumped } else { 10 })).collect();
        real_snapshot(files.path(), &stamps.iter().map(|(id, m)| (id.as_str(), *m)).collect::<Vec<_>>())
    };
    boot(&s, files.path(), &at(0));
    settle(&s);
    HASHED.with(|n| n.set(0));
    boot(&s, files.path(), &at(50));
    assert_eq!(dirty_count(&s), 0);
    assert_eq!(HASHED.with(|n| n.get()), 5, "five files had a new mtime");
}

/// SV-CDE-101: rows of an older version are recomputed in one pass, and neither the next capped
/// change nor a restart marks them again.
#[test]
fn a_version_bump_settles_in_one_pass_and_stays_settled_across_a_restart() {
    let (dir, s) = store();
    let stories = ["SV-SET-1".to_string(), "SV-SET-2".to_string()];
    fake_computed(dir.path(), &stories);
    let files = snapshot_of(&[("sv-set-1", 10), ("sv-set-2", 20)]);
    capped(&s, &files);
    settle(&s);
    s.conn.execute("UPDATE story_totals SET version = ?1", params![story_changes::TOTALS_VERSION - 1]).unwrap();
    mark_other_versions(&s, 5_000).unwrap();
    assert_eq!(dirty_count(&s), 2);
    // The compute: the rows are the current version and clean.
    s.conn.execute("UPDATE story_totals SET version = ?1", params![story_changes::TOTALS_VERSION]).unwrap();
    settle(&s);
    // The restart: the version check, then the capped change of the new index.
    let reopened = UsageStore::open(&dir.path().join("usage.sqlite")).unwrap();
    mark_other_versions(&reopened, 6_000).unwrap();
    capped(&reopened, &files);
    assert_eq!(dirty_count(&reopened), 0);
}

/// M4: a row computed by another version reads as refreshing and is recomputed whole.
#[test]
fn m4_rows_of_another_version_are_refreshing_and_recomputed_whole() {
    let (dir, s) = store();
    let ids = ["SV-VER-1".to_string()];
    let store = fake_computed(dir.path(), &ids);
    store
        .conn
        .execute("UPDATE story_totals SET version = ?1, git = '{}'", params![story_changes::TOTALS_VERSION - 1])
        .unwrap();
    assert!(refreshing(dir.path(), "SV-VER-1"));
    mark_other_versions(&s, 1_000_000).unwrap();
    let jobs = next_jobs(&s, 1_000_000 + MIN_REFRESH_MS + SETTLE_MS, 10).unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].git, None, "another version's git half is not reused");
}

/// SV-CDE-91: a row computed before activity times existed (an older version) is read at once:
/// its totals, `refreshing`, and the new fields null with `activityReason: "refreshing"`; never
/// pending, and the read computes nothing. The worker's recompute fills them in.
#[tokio::test]
async fn m4_an_older_row_answers_its_totals_with_activity_refreshing_and_no_compute_on_read() {
    let (dir, mut s) = store();
    let home = dir.path();
    let story = "SV-ACT-1".to_string();
    seed_session(&mut s, home, &story, "act", true);
    let ids = [story.clone()];
    let store = fake_computed(home, &ids);
    store
        .conn
        .execute(
            "UPDATE story_totals SET version = ?1, totals = '{\"sessions\":3}', facets = '{\"runtimes\":[\"claude\"]}'",
            params![story_changes::TOTALS_VERSION - 1],
        )
        .unwrap();
    let reads = story_changes::git_reads(home);
    let old = human_read(home, &ids);
    let entry = &old["stories"][&story];
    assert!(entry.get("pending").is_none(), "an older row is never pending: {entry}");
    assert_eq!(entry["totals"], json!({"sessions": 3}), "its old totals answer at once");
    assert_eq!(entry["refreshing"], true);
    assert_eq!(entry["runtimes"], json!(["claude"]));
    for field in ["firstActivityAt", "lastActivityAt", "openSessions"] {
        assert!(entry[field].is_null() && entry.get(field).is_some(), "{field}: {entry}");
    }
    assert_eq!(entry["activityReason"], "refreshing");
    assert_eq!(story_changes::git_reads(home), reads, "the read computed");
    assert_eq!(old["pending"], json!([]));
    // The recompute (the worker's, through the throttled path) answers them for real.
    mark_other_versions(&UsageStore::open(&home.join("usage.sqlite")).unwrap(), 1).unwrap();
    index_all(home).await;
    let fresh = human_read(home, &ids);
    let entry = &fresh["stories"][&story];
    assert!(entry.get("refreshing").is_none(), "{entry}");
    // The session's usage (seeded at 1_790_000_000_000) is the story's only recorded activity.
    assert_eq!(entry["firstActivityAt"], 1_790_000_000_000_i64);
    assert_eq!(entry["lastActivityAt"], 1_790_000_000_000_i64);
    assert!(entry.get("activityReason").is_none(), "{entry}");
    assert_eq!(entry["openSessions"], 0, "a session with no observed timing is not open");
}

/// M4: the computation's version is pinned to `Totals`' fields and the facets. A field added after
/// version 8 is an `Added` step naming it (its kind says what a stored row redoes); any other
/// change to them is a step of another kind.
#[tokio::test]
async fn m4_the_totals_version_is_pinned_to_its_fields() {
    let (dir, _s) = store();
    let (totals, _, facets, _) =
        story_changes::totals(dir.path(), "SV-PIN-1", &CallerScope::Human, None, None, None)
            .await
            .unwrap();
    let mut keys: Vec<String> = facets.as_object().unwrap().keys().cloned().collect();
    keys.extend(serde_json::to_value(&totals).unwrap().as_object().unwrap().keys().cloned());
    keys.sort();
    let mut expected: Vec<String> = [
        "activeMs", "commits", "costUsd", "files", "idleMs", "lines", "reviewRounds", "reviewTasks", "sessions", "wallMs",
        "activityReason", "firstActivityAt", "lastActivityAt", "mergedAt", "mergedAtReason",
        "openSessions", "prs", "prsTotal", "runtimes",
    ]
    .map(String::from)
    .to_vec();
    for (version, bump) in story_changes::TOTALS_STEPS {
        if let (true, story_changes::Bump::Added { fields, .. }) = (*version > 8, bump) {
            expected.extend(fields.iter().map(|field| field.to_string()));
        }
    }
    expected.sort();
    assert_eq!(keys, expected, "a new field needs an Added step");
    assert_eq!(story_changes::TOTALS_STEPS.first().map(|step| step.0), Some(8));
    assert!(
        story_changes::TOTALS_STEPS.windows(2).all(|pair| pair[0].0 < pair[1].0),
        "versions rise one step at a time"
    );
}

/// M5: a session or task change recomputes the session half and keeps the git half (no git
/// read); a git change reads git again.
#[tokio::test]
async fn m5_session_changes_keep_the_git_half_and_git_changes_read_git() {
    let (dir, mut s) = store();
    let home = dir.path();
    let story = "SV-HALF-1".to_string();
    seed_session(&mut s, home, &story, "half", true);
    human_read(home, std::slice::from_ref(&story));
    index_all(home).await;
    let after_first = story_changes::git_reads(home);
    assert!(after_first > 0, "the first compute reads git");

    seed_reviews(&s, &story, 1);
    let jobs = next_jobs(&s, now_ms() + MIN_REFRESH_MS + SETTLE_MS, 10).unwrap();
    assert_eq!(jobs.len(), 1);
    assert!(jobs[0].git.is_some(), "a task change keeps the git half");
    let worker = Arc::new(Mutex::new(UsageStore::open(&home.join("usage.sqlite")).unwrap()));
    std::thread::sleep(Duration::from_millis(5));
    refresh(home, &worker, jobs[0].clone()).await;
    assert_eq!(story_changes::git_reads(home), after_first, "a task change read git");
    assert_eq!(human_read(home, std::slice::from_ref(&story))["stories"][&story]["totals"]["reviewTasks"], 1);

    mark(&s, &BTreeSet::from([story.to_lowercase()]), now_ms(), Half::Git).unwrap();
    let jobs = next_jobs(&s, now_ms() + MIN_REFRESH_MS + SETTLE_MS, 10).unwrap();
    assert_eq!(jobs[0].git, None, "a git change drops the git half");
    std::thread::sleep(Duration::from_millis(5));
    refresh(home, &worker, jobs[0].clone()).await;
    assert!(story_changes::git_reads(home) > after_first, "a git change did not read git");
}

/// Scope add: a story with no merged PR says so beside a null mergedAt, never a guessed time.
#[tokio::test]
async fn facets_of_a_story_with_no_merged_pr_say_why() {
    let (dir, _s) = store();
    let (_, _, facets, _) = story_changes::totals(dir.path(), "SV-NOPR-1", &CallerScope::Human, None, None, None)
        .await
        .unwrap();
    assert_eq!(facets["mergedAt"], Value::Null);
    assert_eq!(facets["mergedAtReason"], "no merged PR found locally");
    assert_eq!((facets["prs"].clone(), facets["prsTotal"].clone()), (json!([]), json!(0)));
    assert_eq!(facets["runtimes"], json!([]));
}

/// Gate red (SV-PBF-287, run 3): a writer's upsert of an already-queued session must not abort.
/// A trigger statement takes the outer statement's conflict policy, so the queue insert must never
/// conflict; with `INSERT OR IGNORE` this second write failed `UNIQUE constraint failed`.
#[test]
fn a_queued_session_never_aborts_its_writers_upsert() {
    let (_dir, s) = store();
    let mut record = SessionRecord::new("twice");
    record.story_id = Some("SV-UP-1".into());
    s.write_session_record(&record).unwrap();
    s.write_session_record(&record).unwrap();
    let queued: i64 = s
        .conn
        .query_row(
            "SELECT count(*) FROM source_event WHERE source_kind = 'session' AND source_id = 'twice'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(queued, 1);
}

/// SV-PBF-294: a cold index finishes when one story hangs and another fails. Each becomes an
/// error row, the rest are computed, the backfill takes one story at a time, and no computed row
/// is left marked changed by its own write.
#[tokio::test]
async fn a_cold_index_finishes_when_one_story_hangs_and_another_fails() {
    let (dir, mut s) = store();
    let home = dir.path();
    let all = ids("SV-BF", 5);
    for id in &all[..2] {
        seed_session(&mut s, home, id, &format!("{id}-session"), true);
    }
    {
        let mut map = injected().lock().unwrap();
        map.insert(all[1].clone(), Inject::Hang);
        map.insert(all[3].clone(), Inject::Fail);
    }
    assert_eq!(human_read(home, &all)["indexing"], json!({"done": 0, "total": 5}));

    let worker = Arc::new(Mutex::new(
        UsageStore::open(&home.join("usage.sqlite")).unwrap(),
    ));
    let mut steps = 0;
    for _ in 0..200 {
        let due = due(&worker.lock().unwrap(), home, now_ms()).unwrap();
        assert!(due.cold.len() <= 1, "the backfill takes one story at a time");
        let Some((job, _)) = pick(due, false) else {
            let cold = select_jobs(&worker.lock().unwrap(), COLD_SQL, now_ms(), 1).unwrap();
            if cold.is_empty() {
                break;
            }
            // Nothing starts while the hung compute still runs.
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        steps += 1;
        assert!(steps <= all.len(), "the backfill must not repeat a story");
        refresh(home, &worker, job).await;
    }
    assert_eq!(steps, all.len());

    let done = human_read(home, &all);
    assert_eq!(done["indexing"], json!({"done": 5, "total": 5}));
    assert_eq!(done["pending"], json!([]));
    let hung = done["stories"][&all[1]]["error"].as_str().unwrap();
    assert!(hung.contains("took longer") || hung.contains("stuck"), "{hung}");
    assert_eq!(done["stories"][&all[3]]["error"], "injected failure");
    for id in [&all[0], &all[2], &all[4]] {
        assert!(done["stories"][id]["totals"].is_object(), "{id}: {done}");
        assert!(!dirty(&worker.lock().unwrap(), id), "{id} was re-marked by its own compute");
    }
    // The failed stories wait out the retry delay instead of being recomputed in a loop.
    assert!(next_jobs(&worker.lock().unwrap(), now_ms(), 10).unwrap().is_empty());
}

/// SV-PBF-294: a moved notes ref marks only the in-flight stories (commits, not merged), each at
/// most once per cooldown; a move inside the cooldown is honoured when it ends, and a recompute
/// never re-arms its own mark.
#[test]
fn a_moved_notes_ref_marks_in_flight_stories_once_per_cooldown() {
    let (dir, s) = store();
    let all = ids("SV-NOTES", 4);
    read_blocking(dir.path(), &all, &CallerScope::Human, false).unwrap();
    let repos = serde_json::to_string(&["/notes-repo/.git"]).unwrap();
    let t = 50_000_000;
    for (id, git, facets, changed) in [
        (&all[0], r#"{"commits":2}"#, r#"{"mergedAt":null}"#, t - 1_000),
        (&all[1], r#"{"commits":2}"#, r#"{"mergedAt":5}"#, t - 1_000),
        (&all[2], r#"{"commits":0}"#, r#"{"mergedAt":null}"#, t - 1_000),
        (&all[3], r#"{"commits":2}"#, r#"{"mergedAt":null}"#, t - NOTES_ACTIVE_MS - 1),
    ] {
        s.conn
            .execute(
                "UPDATE story_totals SET totals = '{}', unavailable = '{}', repos = ?1, git = ?2,
                    facets = ?3, computed_at_ms = 1, changed_ms = ?5 WHERE story_key = lower(?4)",
                params![repos, git, facets, id, changed],
            )
            .unwrap();
    }
    let clear = || {
        s.conn
            .execute("UPDATE story_totals SET dirty_since_ms = NULL, git_dirty_ms = NULL", [])
            .unwrap();
    };
    let marked = || all.iter().filter(|id| dirty(&s, id)).cloned().collect::<Vec<_>>();

    notes_moved(&s, "/notes-repo/.git", t).unwrap();
    assert_eq!(marked(), vec![all[0].clone()], "only the in-flight story that really changed lately");
    clear();

    // Moves inside the cooldown mark nothing now ...
    notes_moved(&s, "/notes-repo/.git", t + 1_000).unwrap();
    notes_flush(&s, t + NOTES_COOLDOWN_MS - 1).unwrap();
    assert!(marked().is_empty(), "inside the cooldown");
    // ... and are not lost: the cooldown's end marks the story.
    notes_flush(&s, t + NOTES_COOLDOWN_MS).unwrap();
    assert_eq!(marked(), vec![all[0].clone()], "the pending move is honoured");
    clear();

    // The flush started a new cooldown, and nothing is pending: no re-mark of its own.
    notes_flush(&s, t + 2 * NOTES_COOLDOWN_MS).unwrap();
    assert!(marked().is_empty(), "a mark never re-arms itself");
}

/// SV-PBF-294, with a real repository: a notes ref that moves marks only the in-flight story,
/// once per cooldown, with the move inside the cooldown honoured when it ends; the first read of
/// the repository and a default-branch move that names no story mark nothing.
#[test]
fn notes_moves_in_a_real_repository_mark_in_flight_stories_once_per_cooldown() {
    let (dir, s) = store();
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(repo_dir.path()).unwrap();
    commit(&repo, "refs/remotes/origin/main", &[], "root");
    let all = ids("SV-RNOTES", 3);
    read_blocking(dir.path(), &all, &CallerScope::Human, false).unwrap();
    let repos = serde_json::to_string(&[repo.path().to_string_lossy()]).unwrap();
    for (id, git, facets) in [
        (&all[0], r#"{"commits":2}"#, r#"{"mergedAt":null}"#),
        (&all[1], r#"{"commits":2}"#, r#"{"mergedAt":5}"#),
        (&all[2], r#"{"commits":0}"#, r#"{"mergedAt":null}"#),
    ] {
        s.conn
            .execute(
                "UPDATE story_totals SET totals = '{}', unavailable = '{}', repos = ?1, git = ?2,
                    facets = ?3, computed_at_ms = ?5, changed_ms = ?6 WHERE story_key = lower(?4)",
                params![repos, git, facets, id, 10_000_000, 60_000_000],
            )
            .unwrap();
    }
    let clear = || {
        s.conn
            .execute("UPDATE story_totals SET dirty_since_ms = NULL, git_dirty_ms = NULL", [])
            .unwrap();
    };
    let marked = || all.iter().filter(|id| dirty(&s, id)).cloned().collect::<Vec<_>>();
    let t = 60_000_000;

    poll_refs(&s, t).unwrap();
    assert!(marked().is_empty(), "first sight is the baseline");

    let notes = commit(&repo, "refs/notes/ai", &[], "note one");
    poll_refs(&s, t + 10).unwrap();
    assert_eq!(marked(), vec![all[0].clone()], "only the in-flight story");
    clear();

    commit(&repo, "refs/notes/ai", &[notes], "note two");
    poll_refs(&s, t + 20).unwrap();
    assert!(marked().is_empty(), "a move inside the cooldown waits");
    poll_refs(&s, t + 10 + NOTES_COOLDOWN_MS).unwrap();
    assert_eq!(marked(), vec![all[0].clone()], "the waiting move is honoured");
    clear();
    poll_refs(&s, t + 20 + 2 * NOTES_COOLDOWN_MS).unwrap();
    assert!(marked().is_empty(), "nothing left waiting, and a mark does not re-arm itself");

    // The default branch jumping to an unrelated commit names no story: it marks no whole repository.
    let unrelated = commit(&repo, "refs/heads/elsewhere", &[], "unrelated");
    repo.reference("refs/remotes/origin/main", unrelated, true, "force").unwrap();
    poll_refs(&s, t + 30 + 2 * NOTES_COOLDOWN_MS).unwrap();
    assert!(marked().is_empty(), "a default-branch move that names no story marks nothing");
}

/// SV-PBF-294: a result whose lines are still being counted (lines unavailable, other totals
/// kept) is stored whole and read back as a ready story, not an error; it schedules no retry of
/// its own (the commit sweep marks the story when it has counted).
#[test]
fn a_counting_result_persists_through_store_result() {
    let (dir, s) = store();
    let all = ids("SV-BUDGET", 1);
    read_blocking(dir.path(), &all, &CallerScope::Human, false).unwrap();
    let job = next_jobs(&s, now_ms(), 1).unwrap().remove(0);
    let reason = story_changes::COUNTING;
    let git = story_changes::GitTotals {
        unavailable: BTreeMap::from([("lines".to_owned(), reason.to_owned())]),
        commits: 3,
        ..Default::default()
    };
    let totals = story_changes::Totals {
        sessions: 0,
        commits: 3,
        files: 0,
        cost_usd: None,
        wall_ms: None,
        active_ms: None,
        idle_ms: None,
        review_rounds: Some(0),
        review_tasks: Some(0),
        lines: None,
    };
    let unavailable = BTreeMap::from([("lines".to_owned(), reason.to_owned())]);
    store_result(&s, &job, now_ms() - 1, now_ms(), Ok((totals, unavailable, json!({}), git)), None).unwrap();
    let answer = human_read(dir.path(), &all);
    let story = &answer["stories"][&all[0]];
    assert!(story.get("error").is_none() && story["totals"].is_object(), "{story}");
    let coded = story["unavailable"]["lines"].as_u64().unwrap() as usize;
    assert_eq!(answer["reasons"][coded], reason);
    let (retry, changed): (Option<i64>, Option<i64>) = s
        .conn
        .query_row("SELECT dirty_since_ms, changed_ms FROM story_totals", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!(retry, None, "no retry is scheduled: the sweep marks the story");
    assert_eq!(changed, None);
}

/// SV-GHI-7 AC7: an indexed story's no-sessions cost reason is read as of now, so installing or
/// removing a managed-settings redirect shows on the next read without the story being folded
/// again; other reasons are as stored.
#[test]
fn a_managed_settings_change_shows_on_the_next_index_read_without_a_fold() {
    use crate::session_usage::managed_telemetry::{Fixture, USAGE_REDIRECTED};
    const NONE: &str = "no sessions with priced usage for this story";
    let (dir, s) = store();
    let all = ids("SV-MANAGED", 1);
    human_read(dir.path(), &all);
    let stored = |cost: &str| {
        let unavailable = json!({"costUsd": cost, "lines": "kept as stored"}).to_string();
        s.conn
            .execute(
                "UPDATE story_totals SET totals = '{}', unavailable = ?2, computed_at_ms = 1 WHERE story_key = lower(?1)",
                params![all[0], unavailable],
            )
            .unwrap();
    };
    let read = || {
        let answer = human_read(dir.path(), &all);
        let story = &answer["stories"][&all[0]];
        assert_eq!(story["computedAtMs"], 1, "the read folded nothing: {story}");
        let unavailable = expand(&answer, story);
        assert_eq!(unavailable["lines"], "kept as stored");
        unavailable["costUsd"].as_str().unwrap().to_owned()
    };
    stored(NONE);
    assert_eq!(read(), NONE);
    let read_from_its_log = Fixture::redirecting();
    assert_eq!(read(), NONE, "Claude's session log is read");
    drop(read_from_its_log);
    let managed = Fixture::redirecting_unread();
    assert_eq!(read(), USAGE_REDIRECTED, "an install shows on the next read");
    stored(USAGE_REDIRECTED);
    assert_eq!(read(), USAGE_REDIRECTED);
    drop(managed);
    assert_eq!(read(), NONE, "a removal shows on the next read");
}

/// SV-PBF-294: notes-marked rows queue behind real changes, and `due` offers nothing while a
/// compute of the home is still running.
#[test]
fn notes_marks_queue_behind_real_changes_and_due_waits_for_a_running_compute() {
    let (dir, s) = store();
    let all = ids("SV-ORDER", 3);
    read_blocking(dir.path(), &all, &CallerScope::Human, false).unwrap();
    // The notes-marked story was asked last (most recent), the real change first.
    for (id, asked, changed, dirty_at) in [
        (&all[0], 1_000, 90_000_000, 90_000_000),
        (&all[1], 9_000, 1_000, 90_000_000),
        (&all[2], 5_000, 90_000_000, 90_000_000),
    ] {
        s.conn
            .execute(
                "UPDATE story_totals SET totals = '{}', unavailable = '{}', computed_at_ms = 1,
                    asked_at_ms = ?2, changed_ms = ?3, dirty_since_ms = ?4, dirty_at_ms = ?4
                 WHERE story_key = lower(?1)",
                params![id, asked, changed, dirty_at],
            )
            .unwrap();
    }
    let now = 100_000_000;
    let order: Vec<String> = select_jobs(&s, CHANGED_SQL, now, 10)
        .unwrap()
        .into_iter()
        .map(|job| job.story_id)
        .collect();
    assert_eq!(order, [all[2].clone(), all[0].clone(), all[1].clone()], "the notes-marked story is last");

    let token = take_slot(dir.path()).unwrap();
    let skipped = due(&s, dir.path(), now).unwrap();
    release(dir.path(), token);
    assert!(skipped.changed.is_empty() && skipped.cold.is_empty(), "nothing is due while a compute runs");
}

/// Due rows run never-computed, then really changed (live work, even when it is the newest asked),
/// then the rows holding the old "too large" reason (the backfill), then the other quiet rows.
#[test]
fn live_changes_run_before_the_too_large_backfill_which_runs_before_other_quiet_rows() {
    let (dir, s) = store();
    let all = ids("SV-BIG", 5);
    read_blocking(dir.path(), &all, &CallerScope::Human, false).unwrap();
    let old = r#"{"unavailable":{"lines":"too large to compute in the background"}}"#;
    // (asked, changed, git): the live row is the oldest asked; the over-budget rows are not really changed.
    for (id, asked, changed, git) in [
        (&all[0], 9_000, 1_000, "{}"),
        (&all[1], 1_000, 1_000, old),
        (&all[2], 8_000, 90_000_000, "{}"),
        (&all[3], 2_000, 1_000, old),
        (&all[4], 500, 90_000_000, "{}"),
    ] {
        s.conn
            .execute(
                "UPDATE story_totals SET totals = '{}', unavailable = '{}', git = ?4, computed_at_ms = 1,
                    asked_at_ms = ?2, changed_ms = ?3, dirty_since_ms = 90_000_000, dirty_at_ms = 90_000_000
                 WHERE story_key = lower(?1)",
                params![id, asked, changed, git],
            )
            .unwrap();
    }
    let order: Vec<String> = select_jobs(&s, CHANGED_SQL, 100_000_000, 10)
        .unwrap()
        .into_iter()
        .map(|job| job.story_id)
        .collect();
    // Really changed: all[2] (asked 8000), all[4] (500). Then over-budget: all[3], all[1]. Then all[0].
    assert_eq!(order, [all[2].clone(), all[4].clone(), all[3].clone(), all[1].clone(), all[0].clone()]);
}

/// SV-PBF-294: an old store gains `changed_ms` and a second `init` changes nothing; a notes mark
/// leaves it alone while a real mark sets it; a late result does not overwrite a newer change.
#[test]
fn changed_ms_migrates_notes_marks_leave_it_and_late_results_respect_it() {
    let (dir, s) = store();
    let triggers: Vec<String> = {
        let mut stmt = s
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'trigger' AND name LIKE 'story_totals_%'")
            .unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    };
    for name in triggers {
        s.conn.execute_batch(&format!("DROP TRIGGER {name};")).unwrap();
    }
    s.conn.execute_batch("ALTER TABLE story_totals DROP COLUMN changed_ms;").unwrap();
    let has = |s: &UsageStore| -> bool {
        s.conn
            .query_row(
                "SELECT count(*) > 0 FROM pragma_table_info('story_totals') WHERE name = 'changed_ms'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert!(!has(&s));
    init(&s.conn).unwrap();
    assert!(has(&s), "an old store gains the column");
    init(&s.conn).unwrap();
    assert!(has(&s), "a second init is a no-op");

    let all = ids("SV-CHG", 1);
    read_blocking(dir.path(), &all, &CallerScope::Human, false).unwrap();
    let (scope_key, key): (String, String) = s
        .conn
        .query_row("SELECT scope_key, story_key FROM story_totals", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    let changed = || -> Option<i64> {
        s.conn.query_row("SELECT changed_ms FROM story_totals", [], |r| r.get(0)).unwrap()
    };
    s.conn.execute("UPDATE story_totals SET changed_ms = 5", []).unwrap();
    mark_story_git(&s, &scope_key, &key, 99).unwrap();
    assert_eq!(changed(), Some(5), "a notes mark leaves changed_ms alone");
    mark(&s, &BTreeSet::from([key.clone()]), 100, Half::Git).unwrap();
    assert_eq!(changed(), Some(100), "a real mark sets it");

    // A late result is not stored over a change made after it started.
    let job = next_jobs(&s, now_ms(), 1).unwrap().remove(0);
    let totals = |commits| story_changes::Totals {
        sessions: 0,
        commits,
        files: 0,
        cost_usd: None,
        wall_ms: None,
        active_ms: None,
        idle_ms: None,
        review_rounds: Some(0),
        review_tasks: Some(0),
        lines: None,
    };
    let result = |commits| Ok((totals(commits), BTreeMap::new(), json!({}), story_changes::GitTotals::default()));
    store_result(&s, &job, i64::MAX, now_ms(), result(1), Some((50, 0, 0))).unwrap();
    let stored: Option<String> =
        s.conn.query_row("SELECT totals FROM story_totals", [], |r| r.get(0)).unwrap();
    assert!(stored.is_none(), "changed at 100 after the late compute started at 50");
    store_result(&s, &job, i64::MAX, now_ms(), result(1), Some((100, 0, 0))).unwrap();
    let stored: Option<String> =
        s.conn.query_row("SELECT totals FROM story_totals", [], |r| r.get(0)).unwrap();
    assert!(stored.is_some(), "no change since it started");
}

/// SV-PBF-294: a blocking compute past its timeout becomes an error row, and nothing else starts
/// until its thread returns; then the next story is computed.
#[tokio::test]
async fn nothing_starts_while_a_blocking_compute_is_still_running() {
    let (dir, s) = store();
    let home = dir.path();
    let all = ids("SV-BLK", 2);
    human_read(home, &all);
    s.conn.execute("UPDATE story_totals SET asked_at_ms = 9_000 WHERE story_key = 'sv-blk-0'", []).unwrap();
    s.conn.execute("UPDATE story_totals SET asked_at_ms = 1_000 WHERE story_key = 'sv-blk-1'", []).unwrap();
    injected().lock().unwrap().insert(all[0].clone(), Inject::Hang);
    let worker = Arc::new(Mutex::new(UsageStore::open(&home.join("usage.sqlite")).unwrap()));

    let first = due(&worker.lock().unwrap(), home, now_ms()).unwrap();
    let (job, _) = pick(first, false).unwrap();
    assert_eq!(job.story_id, all[0]);
    refresh(home, &worker, job).await;
    assert!(human_read(home, &all)["stories"][&all[0]]["error"].as_str().unwrap().contains("took longer"));
    assert!(busy(home), "the timed-out compute's thread is still running");
    let waiting = due(&worker.lock().unwrap(), home, now_ms()).unwrap();
    assert!(waiting.cold.is_empty() && waiting.changed.is_empty(), "nothing starts beside it");

    for _ in 0..100 {
        if !busy(home) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!busy(home), "the hung compute returned");
    let next = due(&worker.lock().unwrap(), home, now_ms()).unwrap();
    let (job, _) = pick(next, false).unwrap();
    assert_eq!(job.story_id, all[1]);
    refresh(home, &worker, job).await;
    assert!(human_read(home, &all)["stories"][&all[1]]["totals"].is_object());
}

/// SV-PBF-294: a version mark, an all-stories mark and a price change leave `changed_ms`, so a
/// notes move after them marks nothing; a default-branch move that cannot be read marks the open
/// stories once, quietly.
#[test]
fn quiet_marks_leave_changed_ms_and_an_unreadable_default_branch_marks_open_stories() {
    let (dir, s) = store();
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(repo_dir.path()).unwrap();
    commit(&repo, "refs/remotes/origin/main", &[], "root");
    let all = ids("SV-QUIET", 3);
    read_blocking(dir.path(), &all, &CallerScope::Human, false).unwrap();
    let repos = serde_json::to_string(&[repo.path().to_string_lossy()]).unwrap();
    for (id, git, facets) in [
        (&all[0], r#"{"commits":2}"#, r#"{"mergedAt":null}"#),
        (&all[1], r#"{"commits":2}"#, r#"{"mergedAt":5}"#),
        (&all[2], r#"{"commits":0}"#, r#"{"mergedAt":null}"#),
    ] {
        s.conn
            .execute(
                "UPDATE story_totals SET totals = '{}', unavailable = '{}', repos = ?1, git = ?2,
                    facets = ?3, computed_at_ms = 1, version = 1 WHERE story_key = lower(?4)",
                params![repos, git, facets, id],
            )
            .unwrap();
    }
    let changed = || -> Vec<Option<i64>> {
        let mut stmt = s.conn.prepare("SELECT changed_ms FROM story_totals ORDER BY story_key").unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    };
    let clear = || {
        s.conn
            .execute("UPDATE story_totals SET dirty_since_ms = NULL, git_dirty_ms = NULL", [])
            .unwrap();
    };
    let marked = || all.iter().filter(|id| dirty(&s, id)).cloned().collect::<Vec<_>>();

    mark_other_versions(&s, 70_000_000).unwrap();
    assert_eq!(marked().len(), 3, "every old-version row recomputes");
    assert_eq!(changed(), vec![None, None, None], "a version mark is not a change");
    mark_all(&s, 70_000_001, Half::Git).unwrap();
    assert_eq!(changed(), vec![None, None, None], "an all-stories mark is not a change");
    clear();
    notes_moved(&s, "/not-this-repo", 70_000_002).unwrap();
    notes_moved(&s, &repo.path().to_string_lossy(), 70_000_003).unwrap();
    assert!(marked().is_empty(), "a notes move marks nothing after quiet marks");

    // The default branch moves from a commit this repository does not have: unreadable.
    poll_refs(&s, 80_000_000).unwrap();
    clear();
    s.conn
        .execute(
            "UPDATE story_totals_ref SET oid = '1111111111111111111111111111111111111111' WHERE name = '@default'",
            [],
        )
        .unwrap();
    poll_refs(&s, 80_000_010).unwrap();
    assert_eq!(marked(), vec![all[0].clone()], "only the open story with commits");
    assert_eq!(changed(), vec![None, None, None], "and without changed_ms");
    clear();
    poll_refs(&s, 80_000_020).unwrap();
    assert!(marked().is_empty(), "once");
}

fn row_of(s: &UsageStore, story: &str) -> (Option<String>, Option<String>, Option<i64>) {
    s.conn
        .query_row(
            "SELECT totals, error, dirty_since_ms FROM story_totals WHERE story_key = lower(?1)",
            params![story],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
}

/// SV-PBF-294: a story whose compute really times out is an error row with a retry mark; when its
/// task finishes late the result is stored and the retry mark cleared, unless the story really
/// changed since the compute started (then the row keeps its mark and is computed again).
#[tokio::test]
async fn a_late_success_clears_the_retry_mark_and_respects_a_real_change() {
    let (dir, mut s) = store();
    let home = dir.path();
    let all = ids("SV-LATE", 3);
    for id in &all {
        seed_session(&mut s, home, id, &format!("{id}-session"), true);
        injected().lock().unwrap().insert(id.clone(), Inject::Slow);
    }
    human_read(home, &all);
    s.conn.execute("UPDATE story_totals SET asked_at_ms = 9_000 WHERE story_key = 'sv-late-0'", []).unwrap();
    s.conn.execute("UPDATE story_totals SET asked_at_ms = 1_000 WHERE story_key = 'sv-late-1'", []).unwrap();
    s.conn.execute("UPDATE story_totals SET asked_at_ms = 500 WHERE story_key = 'sv-late-2'", []).unwrap();
    let worker = Arc::new(Mutex::new(UsageStore::open(&home.join("usage.sqlite")).unwrap()));
    let idle = || async {
        for _ in 0..100 {
            if !busy(home) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the slow compute never returned");
    };

    let (job, _) = pick(due(&worker.lock().unwrap(), home, now_ms()).unwrap(), false).unwrap();
    assert_eq!(job.story_id, all[0]);
    refresh(home, &worker, job).await;
    let (totals, error, retry) = row_of(&s, &all[0]);
    assert!(totals.is_none() && error.unwrap().contains("took longer"));
    assert!(retry.unwrap() > now_ms() + RETRY_MS - 60_000, "the timeout schedules a retry");
    idle().await;
    for _ in 0..100 {
        if row_of(&s, &all[0]).0.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (totals, error, retry) = row_of(&s, &all[0]);
    assert!(totals.is_some() && error.is_none(), "the late result is stored");
    assert_eq!(retry, None, "and the timeout's retry mark is cleared");

    let (job, _) = pick(due(&worker.lock().unwrap(), home, now_ms()).unwrap(), false).unwrap();
    assert_eq!(job.story_id, all[1]);
    refresh(home, &worker, job).await;
    mark(&s, &BTreeSet::from([all[1].to_lowercase()]), now_ms(), Half::Session).unwrap();
    idle().await;
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    let (totals, _, retry) = row_of(&s, &all[1]);
    assert!(totals.is_none(), "a real change after the start: the late result is not stored");
    assert!(retry.is_some(), "and the row stays marked");

    // A note or ref mark that lands while the compute runs is newer than the timeout's own retry
    // mark: the late result is stored, but only the timeout's own mark is cleared.
    let (job, _) = pick(due(&worker.lock().unwrap(), home, now_ms()).unwrap(), false).unwrap();
    assert_eq!(job.story_id, all[2]);
    let (scope_key, story_key) = (job.scope_key.clone(), job.story_id.to_lowercase());
    refresh(home, &worker, job).await;
    let newer = now_ms();
    mark_story_git(&s, &scope_key, &story_key, newer).unwrap();
    idle().await;
    for _ in 0..100 {
        if row_of(&s, &all[2]).0.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (totals, _, retry) = row_of(&s, &all[2]);
    assert!(totals.is_some(), "the late result is stored");
    assert_eq!(retry, Some(newer), "and the newer mark keeps the row due");
}

/// SV-PBF-294: past `ABANDON_AFTER` a stuck compute's slot is freed and its story waits out a
/// long backoff; at `MAX_ABANDONED` stuck computes nothing starts until one returns.
#[tokio::test]
async fn a_stuck_compute_is_abandoned_with_a_long_backoff_and_at_most_two_may_be_abandoned() {
    let (dir, s) = store();
    let home = dir.path();
    let first = take_slot(home).unwrap();
    assert!(busy(home) && take_slot(home).is_none());
    assert!(abandon(home, first) && !busy(home), "an abandoned compute frees the slot");
    let second = take_slot(home).unwrap();
    assert!(abandon(home, second));
    assert!(busy(home) && take_slot(home).is_none(), "two abandoned computes still run: stop");
    release(home, first);
    assert!(!busy(home), "one returned: the index resumes");
    release(home, second);
    assert!(!abandon(home, second), "a returned compute is not abandoned");

    // End to end: a compute stuck past ABANDON_AFTER leaves an error row with a 6 h backoff.
    let all = ids("SV-STUCK", 1);
    human_read(home, &all);
    injected().lock().unwrap().insert(all[0].clone(), Inject::Hang);
    let worker = Arc::new(Mutex::new(UsageStore::open(&home.join("usage.sqlite")).unwrap()));
    let (job, _) = pick(due(&worker.lock().unwrap(), home, now_ms()).unwrap(), false).unwrap();
    refresh(home, &worker, job).await;
    for _ in 0..100 {
        if row_of(&s, &all[0]).1.is_some_and(|e| e.contains("stuck")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (_, error, retry) = row_of(&s, &all[0]);
    assert!(error.unwrap().contains("stuck"));
    assert!(retry.unwrap() > now_ms() + ABANDON_BACKOFF_MS - 60_000, "a long backoff");
    for _ in 0..100 {
        if !busy(home) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// SV-PBF-294: `refreshing` is true when a mark is due, or while the story's lines are still being
/// counted (its facets say `linesRefreshing`).
#[test]
fn refreshing_means_a_due_mark_or_lines_still_counting() {
    let (dir, s) = store();
    let all = ids("SV-DUE", 1);
    read_blocking(dir.path(), &all, &CallerScope::Human, false).unwrap();
    let fake = UsageStore::open(&dir.path().join("usage.sqlite")).unwrap();
    fake.conn
        .execute(
            "UPDATE story_totals SET totals = '{}', unavailable = '{}', computed_at_ms = 1, version = ?1,
                dirty_since_ms = ?2, dirty_at_ms = ?2",
            params![story_changes::TOTALS_VERSION, now_ms() + 3_600_000],
        )
        .unwrap();
    assert!(!refreshing(dir.path(), &all[0]), "a scheduled retry is not a due change");
    fake.conn.execute("UPDATE story_totals SET dirty_since_ms = 1, dirty_at_ms = 1", []).unwrap();
    assert!(refreshing(dir.path(), &all[0]), "a due mark is");

    fake.conn.execute("UPDATE story_totals SET dirty_since_ms = NULL, dirty_at_ms = NULL", []).unwrap();
    assert!(!refreshing(dir.path(), &all[0]));
    fake.conn
        .execute("UPDATE story_totals SET facets = '{\"linesRefreshing\":true}'", [])
        .unwrap();
    assert!(refreshing(dir.path(), &all[0]), "lines still counting");
    let answer = human_read(dir.path(), &all);
    assert!(answer["stories"][&all[0]].get("linesRefreshing").is_none(), "the flag is not published");
    drop(s);
}

/// SV-PBF-296/297: a published story keeps the reasons for a null `reviewTasks` and for an
/// unmerged branch, rather than a bare null.
#[test]
fn published_reasons_include_review_tasks_and_branches() {
    for reason in ["reviewTasks", "branches"] {
        assert!(PUBLISHED_REASONS.contains(&reason), "{reason}");
    }
}

/// SV-CDE-91 B1: the contribution rows under the story totals.
mod b1 {
    use super::*;
    use crate::session_usage::contribution::{self, EntityAdapter, SourceResolver};

    const KIND: &str = <story_changes::StoryRows as EntityAdapter>::KIND;
    const VERSION: i64 = <story_changes::StoryRows as EntityAdapter>::VERSION;

    fn queued_sessions(s: &UsageStore) -> Vec<contribution::Queued> {
        contribution::queued(s, <story_changes::StoryRows as EntityAdapter>::SOURCES, 10).unwrap()
    }

    fn rows(s: &UsageStore, story: &str) -> Vec<contribution::Row> {
        let scope_key = CallerScope::Human.cache_key().to_string();
        contribution::rows_of(s, &scope_key, KIND, &story.to_lowercase(), VERSION).unwrap().unwrap()
    }

    fn updated(s: &UsageStore, story: &str) -> BTreeMap<String, i64> {
        let mut stmt = s
            .conn
            .prepare("SELECT source_id, updated_ms FROM contribution WHERE entity_id = lower(?1) AND source_kind = 'session'")
            .unwrap();
        let rows = stmt.query_map(params![story], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))).unwrap();
        rows.map(|r| r.unwrap()).collect()
    }

    async fn compute(home: &Path, story: &str) -> Value {
        let (totals, unavailable, facets, _) =
            story_changes::totals(home, story, &CallerScope::Human, None, None, None).await.unwrap();
        json!({"totals": totals, "unavailable": unavailable, "facets": facets})
    }

    async fn full(home: &Path, story: &str) -> Value {
        let params: story_changes::Params = serde_json::from_value(json!({"storyId": story})).unwrap();
        let read = story_changes::read(home, &params, &CallerScope::Human).await.unwrap();
        serde_json::to_value(read).unwrap()
    }

    /// The first compute builds the story's rows; the totals are what `story_changes` answers
    /// (the session half, field by field, with its reasons), whatever the sessions are like.
    #[tokio::test]
    async fn the_aggregate_of_the_rows_equals_the_full_read() {
        for (priced, unpriced, untimed) in [(2, 0, 0), (2, 1, 0), (1, 0, 1), (0, 0, 0)] {
            let (dir, mut s) = store();
            let home = dir.path();
            let mut timing = crate::session_usage::timing::TimingState::default();
            for n in 0..priced {
                let id = format!("p{n}");
                seed_session(&mut s, home, "SV-ROW-1", &id, true);
                for (at, event) in [(100 + n * 50, codesense_pty_core::activity::Event::Submitted), (200 + n * 50, codesense_pty_core::activity::Event::Stopped), (300 + n * 50, codesense_pty_core::activity::Event::Ended)] {
                    timing
                        .apply(&s, &crate::session_usage::timing::Stamped { session_id: id.clone(), at_ms: at, signal: crate::session_usage::timing::Signal::Activity(event, "fixture") })
                        .unwrap();
                }
            }
            for n in 0..unpriced {
                seed_session(&mut s, home, "SV-ROW-1", &format!("u{n}"), false);
            }
            for n in 0..untimed {
                seed_session(&mut s, home, "SV-ROW-1", &format!("t{n}"), true);
            }
            seed_reviews(&s, "SV-ROW-1", 1);
            let indexed = compute(home, "SV-ROW-1").await;
            let read = full(home, "SV-ROW-1").await;
            for field in ["sessions", "costUsd", "wallMs", "activeMs", "idleMs", "reviewRounds", "reviewTasks"] {
                assert_eq!(indexed["totals"][field], read["totals"][field], "{field} ({priced},{unpriced},{untimed})");
            }
            for reason in ["costUsd", "time", "reviewRounds", "reviewTasks"] {
                assert_eq!(indexed["unavailable"].get(reason), read["unavailable"].get(reason), "{reason} ({priced},{unpriced},{untimed})");
            }
            // The rows exist now, one per session, and the next compute reads them (same answer).
            assert_eq!(rows(&s, "SV-ROW-1").len(), (priced + unpriced + untimed) as usize);
            assert_eq!(compute(home, "SV-ROW-1").await["totals"], indexed["totals"]);
        }
    }

    /// Output unchanged: the session half of a mixed fixture, pinned.
    #[tokio::test]
    async fn golden_totals_of_a_mixed_story() {
        let (dir, mut s) = store();
        let home = dir.path();
        let mut timing = crate::session_usage::timing::TimingState::default();
        for (id, start, stop, end) in [("a", 100, 200, 300), ("b", 150, 250, 350)] {
            seed_session(&mut s, home, "SV-GOLD-1", id, true);
            for (at, event) in [(start, codesense_pty_core::activity::Event::Submitted), (stop, codesense_pty_core::activity::Event::Stopped), (end, codesense_pty_core::activity::Event::Ended)] {
                timing
                    .apply(&s, &crate::session_usage::timing::Stamped { session_id: id.into(), at_ms: at, signal: crate::session_usage::timing::Signal::Activity(event, "fixture") })
                    .unwrap();
            }
        }
        seed_session(&mut s, home, "SV-GOLD-1", "untimed", true);
        for answer in [compute(home, "SV-GOLD-1").await, compute(home, "SV-GOLD-1").await, full(home, "SV-GOLD-1").await] {
            let t = &answer["totals"];
            assert_eq!(
                json!([t["sessions"], t["costUsd"], t["wallMs"], t["activeMs"], t["idleMs"]]),
                json!([3, 6.0, 250, 200, 200])
            );
            assert!(answer["unavailable"]["time"].is_string() && answer["unavailable"].get("costUsd").is_none());
        }
        let facets = compute(home, "SV-GOLD-1").await["facets"].clone();
        assert!(!facets["runtimes"].as_array().unwrap().is_empty());
        assert_eq!(facets["firstActivityAt"], 1_790_000_000_000_i64, "the usage is the only activity");
    }

    /// One usage event rewrites that session's row and marks that story; the story's other
    /// sessions are not touched, and an event that changes nothing marks nothing.
    #[tokio::test]
    async fn a_session_event_rewrites_only_its_row() {
        let (dir, mut s) = store();
        let home = dir.path();
        let story = "SV-EVT-1";
        seed_session(&mut s, home, story, "one", true);
        seed_session(&mut s, home, story, "two", true);
        human_read(home, &[story.to_owned()]);
        index_all(home).await;
        s.conn.execute("DELETE FROM source_event", []).unwrap();
        let before = updated(&s, story);
        assert_eq!(before.len(), 2);
        std::thread::sleep(Duration::from_millis(5));
        // A usage event of "two" only.
        let mut e = event("two-e2", "two", "gpt-6-sol", 1_790_000_000_500, Tokens { input: 5, ..Default::default() });
        e.reported_cost_usd = Some(1.0);
        s.ingest(&[e]).unwrap();
        sweep_sources(&s, home, now_ms()).unwrap();
        let after = updated(&s, story);
        assert_eq!(after["one"], before["one"], "the other session's row was not rewritten");
        assert!(after["two"] > before["two"]);
        assert!(dirty(&s, story));
        // The same facts again (a duplicate event) change nothing and mark nothing.
        let worker = Arc::new(Mutex::new(UsageStore::open(&home.join("usage.sqlite")).unwrap()));
        let job = next_jobs(&s, now_ms() + MIN_REFRESH_MS + SETTLE_MS, 10).unwrap().remove(0);
        refresh(home, &worker, job).await;
        assert!(!dirty(&s, story));
        s.conn
            .execute("INSERT INTO source_event (source_kind, source_id, enqueued_ms) VALUES ('session', 'two', 1)", [])
            .unwrap();
        sweep_sources(&s, home, now_ms()).unwrap();
        assert!(!dirty(&s, story), "an event that changed no row marked the story");
        assert_eq!(human_read(home, &[story.to_owned()])["stories"][story]["totals"]["costUsd"], 5.0);
    }

    /// A session linked to another story moves its row: the old story loses it, the new one gains it.
    #[tokio::test]
    async fn a_reattributed_session_moves_its_cost_between_stories() {
        let (dir, mut s) = store();
        let home = dir.path();
        let (old, new) = ("SV-MOVE-1", "SV-MOVE-2");
        seed_session(&mut s, home, old, "mover", true);
        seed_session(&mut s, home, new, "stay", true);
        human_read(home, &[old.to_owned(), new.to_owned()]);
        index_all(home).await;
        s.conn.execute("DELETE FROM source_event", []).unwrap();
        // The session's story link now names the other story.
        let link = StoryLink { story_id: new.into(), source: home.join(format!("{new}.md")), org: "acme".into() };
        s.conn.execute("DELETE FROM session_story WHERE session_id = 'mover'", []).unwrap();
        let mut record = SessionRecord::new("mover");
        record.story_id = Some(new.into());
        s.write_session_record(&record).unwrap();
        s.link_story("mover", &link).unwrap();
        sweep_sources(&s, home, now_ms()).unwrap();
        assert_eq!(rows(&s, old).len(), 0, "the old story lost the row");
        assert_eq!(rows(&s, new).len(), 2, "the new story gained it");
        // The cost moved with it: the old story has no session (null cost, the main reason), the
        // new one has both sessions' 2.0 each.
        let (old_cost, new_cost) = (compute(home, old).await, compute(home, new).await);
        assert_eq!(old_cost["totals"]["sessions"], 0);
        assert!(old_cost["totals"]["costUsd"].is_null());
        assert_eq!(old_cost["unavailable"]["costUsd"], "no sessions with priced usage for this story");
        assert_eq!((new_cost["totals"]["sessions"].clone(), new_cost["totals"]["costUsd"].clone()), (json!(2), json!(4.0)));
    }

    /// The upgrade copies the old session queue into `source_event` and drops the old table, in the
    /// transaction `open` runs it in.
    #[test]
    fn the_queue_migration_copies_the_old_queue_and_drops_it() {
        let (_dir, s) = store();
        s.conn
            .execute_batch(
                "CREATE TABLE story_totals_session (session_id TEXT PRIMARY KEY);
                 INSERT INTO story_totals_session VALUES ('old-1'), ('old-2');",
            )
            .unwrap();
        init(&s.conn).unwrap();
        let count = |sql: &str| s.conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(count("SELECT count(*) FROM sqlite_master WHERE name = 'story_totals_session'"), 0);
        assert_eq!(count("SELECT count(*) FROM source_event WHERE source_kind = 'session'"), 2);
        init(&s.conn).unwrap();
        assert_eq!(count("SELECT count(*) FROM source_event"), 2, "a second run changes nothing");
        // Triggers still queue into the new table.
        let mut record = SessionRecord::new("fresh");
        record.story_id = Some("SV-MIG-1".into());
        s.write_session_record(&record).unwrap();
        assert_eq!(count("SELECT count(*) FROM source_event WHERE source_id = 'fresh'"), 1);
    }

    /// An upgrade that is cut short is a rolled-back transaction (the old table stays and nothing of
    /// the new is kept), and one cut after the copy but before the drop finds the copy and finishes
    /// it, without overwriting what was copied or doubling it.
    #[test]
    fn an_interrupted_migration_runs_again_cleanly() {
        let (_dir, s) = store();
        s.conn
            .execute_batch(
                "CREATE TABLE story_totals_session (session_id TEXT PRIMARY KEY);
                 INSERT INTO story_totals_session VALUES ('old-1'), ('old-2');",
            )
            .unwrap();
        let count = |sql: &str| s.conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        // Cut short: the whole upgrade is rolled back.
        s.conn.execute_batch("BEGIN").unwrap();
        init(&s.conn).unwrap();
        s.conn.execute_batch("ROLLBACK").unwrap();
        assert_eq!(count("SELECT count(*) FROM story_totals_session"), 2, "the old queue keeps its 2 rows");
        assert_eq!(count("SELECT count(*) FROM source_event"), 0, "nothing of the copy was kept (open created the table)");
        // The re-run copies cleanly and drops the old table.
        init(&s.conn).unwrap();
        assert_eq!(count("SELECT count(*) FROM sqlite_master WHERE name = 'story_totals_session'"), 0);
        assert_eq!(count("SELECT count(*) FROM source_event WHERE source_kind = 'session'"), 2);
        // Cut after the copy, before the drop: the old table is back with the copy already there
        // (with its own times); the next run finishes without doubling or overwriting it.
        s.conn
            .execute_batch(
                "CREATE TABLE story_totals_session (session_id TEXT PRIMARY KEY);
                 INSERT INTO story_totals_session VALUES ('old-1'), ('old-2');
                 UPDATE source_event SET enqueued_ms = 5 WHERE source_id = 'old-1';",
            )
            .unwrap();
        init(&s.conn).unwrap();
        assert_eq!(count("SELECT count(*) FROM sqlite_master WHERE name = 'story_totals_session'"), 0, "the drop finished");
        assert_eq!(count("SELECT count(*) FROM source_event"), 2, "nothing doubled");
        assert_eq!(count("SELECT enqueued_ms FROM source_event WHERE source_id = 'old-1'"), 5, "the copy is not overwritten");
    }

    /// A burst of task edits is one recompute (SETTLE and MIN_REFRESH hold on the new store).
    #[tokio::test]
    async fn a_burst_of_task_edits_is_one_recompute() {
        let (dir, mut s) = store();
        let home = dir.path();
        let story = "SV-BURST-1";
        seed_session(&mut s, home, story, "burst", true);
        human_read(home, &[story.to_owned()]);
        index_all(home).await;
        for n in 0..6 {
            let mut task = TaskRecord::new(format!("burst-task-{n}"), "build");
            task.story_id = Some(story.into());
            s.insert_task_if_absent(&task).unwrap();
        }
        let later = now_ms() + MIN_REFRESH_MS + SETTLE_MS;
        let jobs = next_jobs(&s, later, 10).unwrap();
        assert_eq!(jobs.len(), 1, "six edits, one job");
        let worker = Arc::new(Mutex::new(UsageStore::open(&home.join("usage.sqlite")).unwrap()));
        std::thread::sleep(Duration::from_millis(5));
        refresh(home, &worker, jobs[0].clone()).await;
        assert!(next_jobs(&s, later, 10).unwrap().is_empty(), "one compute served the whole burst");
    }

    /// A recomputed story is a row of its own view, with an id that no other view's row shares.
    #[test]
    fn a_recomputed_story_is_a_row_of_its_own_view() {
        let team = Some("team:acme/blue".to_owned());
        assert_eq!(
            recomputed_row(team.clone(), "sv-pub-1", 12),
            (team, json!({"id": "team:acme/blue|sv-pub-1", "story": "sv-pub-1", "computedAtMs": 12}))
        );
        assert_eq!(
            recomputed_row(None, "sv-pub-1", 11),
            (None, json!({"id": "all|sv-pub-1", "story": "sv-pub-1", "computedAtMs": 11}))
        );
    }

    /// A team view gets its own team's stories and nothing else; an unrestricted view gets all.
    #[test]
    fn a_team_view_receives_only_its_own_teams_story_rows() {
        use crate::live_hub::projection::TopicRegistry;
        let mut topics = TopicRegistry::default();
        for key in ["team:acme/blue", "team:acme/red", "all"] {
            topics.subscribe("storyTotals", key, None).unwrap();
        }
        let rows = vec![
            recomputed_row(Some("team:acme/blue".into()), "sv-1", 1),
            recomputed_row(Some("team:acme/red".into()), "sv-2", 2),
            recomputed_row(None, "sv-3", 3),
            recomputed_row(Some("team:acme/red".into()), "sv-1", 4),
        ];
        topics.patch_view("storyTotals", rows, Vec::new(), None).unwrap();
        let mut seen = |key: &str| {
            let snapshot = topics.snapshot("storyTotals", key).unwrap();
            let mut stories: Vec<String> = snapshot.snapshot.iter().map(|row| row["story"].as_str().unwrap().to_owned()).collect();
            stories.sort();
            stories
        };
        assert_eq!(seen("team:acme/blue"), ["sv-1"]);
        assert_eq!(seen("team:acme/red"), ["sv-1", "sv-2"], "the same story in two teams is two rows");
        assert_eq!(seen("all"), ["sv-1", "sv-1", "sv-2", "sv-3"]);
        assert_eq!(seen("team:acme/green"), Vec::<String>::new(), "a team with no stories is told of none");
    }

    /// A failing apply or mark keeps the source queued (through `sweep_sources`, not `apply`): the
    /// next sweep redoes it. A queued id no session answers to is applied with no rows and leaves.
    #[tokio::test]
    async fn a_failed_apply_keeps_the_source_queued_and_an_unanswered_one_is_applied_with_no_rows() {
        let (dir, mut s) = store();
        let home = dir.path();
        seed_session(&mut s, home, "SV-KEEP-1", "keep", true);
        human_read(home, &["SV-KEEP-1".to_owned()]);
        index_all(home).await;
        s.conn.execute("DELETE FROM source_event", []).unwrap();
        let mut e = event("keep-e2", "keep", "gpt-6-sol", 1_790_000_000_700, Tokens { input: 5, ..Default::default() });
        e.reported_cost_usd = Some(1.0);
        s.ingest(&[e]).unwrap();
        let queued = || queued_sessions(&s).len();
        assert_eq!(queued(), 1);
        // The mark's table is gone: the apply fails after its rows, and rolls back whole.
        s.conn.execute_batch("DROP TABLE story_totals;").unwrap();
        sweep_sources(&s, home, now_ms()).unwrap();
        assert_eq!(queued(), 1, "a failed apply keeps the queue row");
        init(&s.conn).unwrap();
        sweep_sources(&s, home, now_ms()).unwrap();
        assert_eq!(queued(), 0, "the next sweep did it");
        // A queued id that no session answers to resolves to no rows: applied once, then gone.
        s.conn
            .execute("INSERT INTO source_event (source_kind, source_id, enqueued_ms) VALUES ('session', 'no-such-session', 1)", [])
            .unwrap();
        sweep_sources(&s, home, now_ms()).unwrap();
        assert_eq!(queued(), 0);
    }

    /// An event queued while its source is resolved is not lost: the queue row moves on with every
    /// event, and a sweep deletes only the row it read.
    #[test]
    fn an_event_during_the_sweep_keeps_the_row_for_the_next_one() {
        let (_dir, s) = store();
        let mut record = SessionRecord::new("busy-one");
        record.story_id = Some("SV-LOST-1".into());
        s.write_session_record(&record).unwrap();
        let first = queued_sessions(&s).remove(0);
        // The session writes again after the sweep read it (any trigger event does this).
        record.branch = Some("feat/x".into());
        s.write_session_record(&record).unwrap();
        let moved = queued_sessions(&s).remove(0);
        assert!(moved.enqueued_ms > first.enqueued_ms, "the row moved on with the event");
        let adapter = story_changes::StoryRows::new(_dir.path().to_owned());
        let (resolved, wanted) = SourceResolver::contributions(&adapter, &s, &first.source).unwrap();
        let done = |queued: &contribution::Queued| {
            contribution::apply(&|_, _| Ok(()), &s, KIND, VERSION, queued, &resolved, &wanted, now_ms()).unwrap()
        };
        done(&first);
        assert_eq!(queued_sessions(&s).len(), 1, "the older read cannot delete the newer event");
        done(&moved);
        assert!(queued_sessions(&s).is_empty());
    }

    /// A session whose usage is under two ids (its own and its native one) is one row that folds
    /// every usage entry: the earliest first time, the latest last time and its recent activity.
    #[tokio::test]
    async fn a_session_with_two_usage_ids_is_one_row_over_all_of_them() {
        let (dir, mut s) = store();
        let home = dir.path();
        seed_session(&mut s, home, "SV-MULTI-1", "launch", true);
        let mut record = SessionRecord::new("launch");
        record.story_id = Some("SV-MULTI-1".into());
        record.native_session_id = Some("native-1".into());
        s.write_session_record(&record).unwrap();
        let mut later = event("launch-late", "native-1", "gpt-6-sol", 1_790_000_005_000, Tokens { input: 1, ..Default::default() });
        later.reported_cost_usd = Some(1.0);
        s.ingest(&[later]).unwrap();
        let adapter = story_changes::StoryRows::new(home.to_owned());
        let built = SourceResolver::rebuild(&adapter, &s, &CallerScope::Human, "SV-MULTI-1").unwrap();
        assert_eq!(built.len(), 1, "one session, one row");
        let row = &built[0].1;
        assert_eq!((row.first_ms, row.last_ms), (Some(1_790_000_000_000), Some(1_790_000_005_000)));
        assert_eq!(row.nums["recent"], Some(1_790_000_005_000_f64));
        assert_eq!(row.nums["cost"], Some(3.0), "the cost covers both ids, once");
        // Timed, ended long ago: not open, however recent its usage is read.
        let rows: Vec<contribution::Row> = built
            .into_iter()
            .map(|(source, contribution)| contribution::Row { source: (source.kind.to_owned(), source.id), contribution })
            .collect();
        let (open, first, last) = story_changes::fold_summary(&rows, 1_790_000_005_000 + 60_000);
        assert_eq!(open, 0);
        assert_eq!((first, last), (Some(1_790_000_000_000), Some(1_790_000_005_000)));
    }

    /// The mark is written with the rows: a crash in between cannot keep a row and lose its mark.
    #[tokio::test]
    async fn the_mark_is_written_in_the_apply_transaction() {
        let (dir, mut s) = store();
        let home = dir.path();
        let story = "SV-ATOM-1";
        seed_session(&mut s, home, story, "atom", true);
        human_read(home, &[story.to_owned()]);
        index_all(home).await;
        s.conn.execute("DELETE FROM source_event", []).unwrap();
        s.conn
            .execute("UPDATE story_totals SET dirty_since_ms = NULL, dirty_at_ms = NULL", [])
            .unwrap();
        let mut e = event("atom-e2", "atom", "gpt-6-sol", 1_790_000_000_900, Tokens { input: 5, ..Default::default() });
        e.reported_cost_usd = Some(1.0);
        s.ingest(&[e]).unwrap();
        s.conn.execute("UPDATE story_totals SET dirty_since_ms = NULL, dirty_at_ms = NULL", []).unwrap();
        // The mark step fails after the rows were upserted: nothing is kept, the session stays queued.
        let adapter = story_changes::StoryRows::new(home.to_owned());
        let queued = queued_sessions(&s).remove(0);
        let (resolved, wanted) = SourceResolver::contributions(&adapter, &s, &queued.source).unwrap();
        let failed = contribution::apply(
            &|_, _| Err("crash".into()),
            &s,
            KIND,
            VERSION,
            &queued,
            &resolved,
            &wanted,
            now_ms(),
        );
        assert!(failed.is_err());
        assert!(!dirty(&s, story));
        assert_eq!(queued_sessions(&s).len(), 1, "still queued");
        // The sweep then does it all, together: the row, the mark, the queue delete.
        sweep_sources(&s, home, now_ms()).unwrap();
        assert!(dirty(&s, story), "the mark came with the row");
        assert_eq!(queued_sessions(&s).len(), 0);
    }

    /// The three cost calculations are one: `report_with`, `session_read` and the session row agree
    /// to the bit, for a session with two models, a native id of its own, and an unpriced model.
    #[tokio::test]
    async fn report_with_session_read_and_the_row_agree_on_cost() {
        let (dir, mut s) = store();
        let home = dir.path();
        s.write_session_record(&SessionRecord::new("multi")).unwrap();
        let usage = |id: &str, model: &str, cost: Option<f64>| {
            let mut e = event(id, "multi", model, 1_790_000_000_000, Tokens { input: 1, ..Default::default() });
            e.reported_cost_usd = cost;
            e
        };
        s.ingest(&[usage("e1", "gpt-6-sol", Some(0.1)), usage("e2", "gpt-6-sol", Some(0.2)), usage("e3", "other-model", Some(0.3))]).unwrap();
        let from_rollup = s.session_cost("multi").unwrap().total();
        let params: crate::session_usage::session_read::Params = serde_json::from_value(json!({"sessionId": "multi"})).unwrap();
        let read = crate::session_usage::session_read::read(&home.join("usage.sqlite"), &home.join("conv"), &params, &CallerScope::Human).await.unwrap();
        let report = s
            .report_with(&crate::session_usage::Filter { session_id: Some("multi".into()), ..Default::default() }, &mut crate::session_usage::StoryResolver::for_home(home, HashMap::new()))
            .unwrap();
        assert_eq!(read["cost"]["totalUsd"].as_f64(), from_rollup);
        assert_eq!(report.sessions[0].cost_usd, from_rollup);
        assert!(from_rollup.is_some());
        // One unpriced model makes all three null.
        s.ingest(&[usage("e4", "unpriced-model", None)]).unwrap();
        let read = crate::session_usage::session_read::read(&home.join("usage.sqlite"), &home.join("conv"), &params, &CallerScope::Human).await.unwrap();
        let report = s
            .report_with(&crate::session_usage::Filter { session_id: Some("multi".into()), ..Default::default() }, &mut crate::session_usage::StoryResolver::for_home(home, HashMap::new()))
            .unwrap();
        assert!(s.session_cost("multi").unwrap().total().is_none());
        assert!(read["cost"]["totalUsd"].is_null() && report.sessions[0].cost_usd.is_none());
    }

    /// H1 and M3, pinned to what main answers (literals from main's own tests and code path): an
    /// empty story, two overlapping timed sessions, a priced and an unpriced one, and timed plus
    /// untimed. Indexed (rows) and full reads both give them.
    #[tokio::test]
    async fn main_values_are_pinned_for_empty_unpriced_timed_and_mixed_stories() {
        use crate::session_usage::timing::{Signal, Stamped, TimingState};
        use codesense_pty_core::activity::Event;
        let timed = |s: &UsageStore, id: &str, start: i64, stop: i64, end: i64| {
            let mut timing = TimingState::default();
            for (at, event) in [(start, Event::Submitted), (stop, Event::Stopped), (end, Event::Ended)] {
                timing.apply(s, &Stamped { session_id: id.into(), at_ms: at, signal: Signal::Activity(event, "fixture") }).unwrap();
            }
        };
        let pick = |a: &Value| {
            let t = &a["totals"];
            json!([t["sessions"], t["costUsd"], t["wallMs"], t["activeMs"], t["idleMs"]])
        };
        for case in ["empty", "redirected-empty", "timed", "redirected-timed", "unpriced", "mixed"] {
            // Managed Claude settings that send usage elsewhere: a story with no sessions says so,
            // and a story with sessions is unchanged.
            let _managed = case.starts_with("redirected-").then(crate::session_usage::managed_telemetry::Fixture::redirecting_unread);
            let (dir, mut s) = store();
            let home = dir.path();
            let story = "SV-PIN-1";
            match case {
                "timed" | "redirected-timed" | "mixed" => {
                    seed_session(&mut s, home, story, "a", true);
                    seed_session(&mut s, home, story, "b", true);
                    timed(&s, "a", 100, 200, 300);
                    timed(&s, "b", 150, 250, 350);
                    if case == "mixed" {
                        seed_session(&mut s, home, story, "untimed", true);
                    }
                }
                "unpriced" => {
                    seed_session(&mut s, home, story, "priced", true);
                    seed_session(&mut s, home, story, "unknown", false);
                }
                _ => {}
            }
            let (indexed, read) = (compute(home, story).await, full(home, story).await);
            let (expected, cost_reason, time_reason): (Value, Option<&str>, Option<&str>) = match case {
                "empty" => (json!([0, null, 0, 0, 0]), Some("no sessions with priced usage for this story"), None),
                "redirected-empty" => (
                    json!([0, null, 0, 0, 0]),
                    Some(crate::session_usage::managed_telemetry::USAGE_REDIRECTED),
                    None,
                ),
                "timed" | "redirected-timed" => (json!([2, 4.0, 250, 200, 200]), None, None),
                "unpriced" => (
                    json!([2, null, null, null, null]),
                    Some("one or more sessions have no complete priced usage"),
                    Some("one or more sessions have no observed timing; time covers observed sessions only"),
                ),
                _ => (
                    json!([3, 6.0, 250, 200, 200]),
                    None,
                    Some("one or more sessions have no observed timing; time covers observed sessions only"),
                ),
            };
            for answer in [&indexed, &read] {
                assert_eq!(pick(answer), expected, "{case}");
                assert_eq!(answer["unavailable"].get("costUsd").and_then(Value::as_str), cost_reason, "{case} cost reason");
                assert_eq!(answer["unavailable"].get("time").and_then(Value::as_str), time_reason, "{case} time reason");
            }
            // The facets the index keeps, as main made them.
            let facets = &indexed["facets"];
            let sessions = expected[0].as_i64().unwrap();
            assert_eq!(facets["runtimes"].as_array().unwrap().len(), usize::from(sessions > 0), "{case}");
            assert_eq!(facets["mergedAt"], Value::Null);
            assert_eq!(facets["mergedAtReason"], "no merged PR found locally");
            assert_eq!(facets["prsTotal"], 0);
            assert_eq!(facets["activityReason"].is_string(), sessions == 0, "{case}");
        }
    }

    /// H2: a usage row that changes (a repricing, a late cost) requeues its session, so its row's
    /// cost updates; a row moved to another session requeues both.
    #[tokio::test]
    async fn a_changed_usage_row_requeues_its_session_and_a_moved_one_both() {
        let (dir, mut s) = store();
        let home = dir.path();
        let story = "SV-REPRICE-1";
        seed_session(&mut s, home, story, "one", true);
        seed_session(&mut s, home, story, "two", true);
        human_read(home, &[story.to_owned()]);
        index_all(home).await;
        s.conn.execute("DELETE FROM source_event", []).unwrap();
        // A repricing is an UPDATE of the row's cost (`reprice_covered`).
        s.conn.execute("UPDATE usage_event SET cost_usd = 5.0 WHERE session_id = 'one'", []).unwrap();
        let ids = |s: &UsageStore| {
            let mut ids: Vec<String> = queued_sessions(s).into_iter().map(|q| q.source.id).collect();
            ids.sort();
            ids
        };
        assert_eq!(ids(&s), ["one"]);
        sweep_sources(&s, home, now_ms()).unwrap();
        assert_eq!(compute(home, story).await["totals"]["costUsd"], 7.0, "the row's cost followed the repricing");
        // An event moved to another session (an upsert that changes its session) requeues both.
        s.conn.execute("UPDATE usage_event SET session_id = 'two' WHERE session_id = 'one'", []).unwrap();
        assert_eq!(ids(&s), ["one", "two"]);
    }

    /// M7: a change the queue cannot see (a story file or worktree link that moves a session to another
    /// story) rebuilds the stories it names whole, through the throttled compute: the old story
    /// loses the session's row and the new one gains it.
    #[tokio::test]
    async fn a_context_change_that_moves_a_session_rebuilds_the_stories_it_names() {
        let (dir, mut s) = store();
        let home = dir.path();
        let (old, new) = ("SV-CTX-1", "SV-CTX-2");
        seed_session(&mut s, home, old, "mover", true);
        seed_session(&mut s, home, new, "stay", true);
        human_read(home, &[old.to_owned(), new.to_owned()]);
        index_all(home).await;
        // The attribution changes where no trigger sees it, and nothing is queued.
        let link = StoryLink { story_id: new.into(), source: home.join(format!("{new}.md")), org: "acme".into() };
        s.conn.execute("DELETE FROM session_story WHERE session_id = 'mover'", []).unwrap();
        s.conn.execute("UPDATE session_record SET story_id = ?1 WHERE session_id = 'mover'", params![new]).unwrap();
        s.link_story("mover", &link).unwrap();
        s.conn.execute("DELETE FROM source_event", []).unwrap();
        assert_eq!(rows(&s, old).len(), 1, "still the old row: nothing told the index");
        // The context change names both stories' files.
        let marks = ContextMarks {
            paths: BTreeSet::from(["/ctx/stories/SV-CTX-1-old.md".to_owned(), "/ctx/stories/SV-CTX-2-new.md".to_owned()]),
            capped: false,
        };
        apply_context(&s, Path::new("/home"), marks, now_ms(), None).unwrap();
        let worker = Arc::new(Mutex::new(UsageStore::open(&home.join("usage.sqlite")).unwrap()));
        std::thread::sleep(Duration::from_millis(5));
        for job in next_jobs(&s, now_ms() + MIN_REFRESH_MS + SETTLE_MS, 10).unwrap() {
            refresh(home, &worker, job).await;
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(rows(&s, old).len(), 0, "the old story lost the session");
        assert_eq!(rows(&s, new).len(), 2, "the new story gained it");
        assert_eq!(compute(home, new).await["totals"]["costUsd"], 4.0);
    }

    /// The publish path: a stored recompute reaches the hub topic as a row of its view (injected app).
    #[test]
    fn a_recompute_is_published_to_the_hub_topic_of_its_view() {
        use crate::routines::test_support::{launch_app, TempHome};
        use tauri::Manager;
        let home = TempHome::new();
        let app = launch_app(&home.codesense_home());
        app.manage(crate::live_hub::LiveHub::default());
        let job = Job {
            scope_key: CallerScope::Human.cache_key().to_string(),
            story_id: "SV-HUB-1".into(),
            kind: "human".into(),
            root: None,
            git: None,
            earlier_git: None,
            dirty_at_ms: None,
        };
        publish_to(app.handle(), &job, "sv-hub-1", 42);
        let rows = app.state::<crate::live_hub::LiveHub>().snapshot("all", "storyTotals").unwrap().snapshot;
        assert_eq!(rows, vec![json!({"id": "all|sv-hub-1", "story": "sv-hub-1", "computedAtMs": 42})]);
    }

    /// A never-computed story is not marked by a source that belongs to it: it is the backfill's, and
    /// its first compute builds its rows whole (marking it would take it out of the backfill).
    #[test]
    fn a_source_of_a_never_computed_story_does_not_mark_it() {
        let (dir, mut s) = store();
        let home = dir.path();
        seed_session(&mut s, home, "SV-COLD-1", "cold", true);
        read_blocking(home, &["SV-COLD-1".to_owned()], &CallerScope::Human, false).unwrap();
        sweep_sources(&s, home, now_ms()).unwrap();
        assert!(!dirty(&s, "SV-COLD-1"), "still cold: the backfill computes it");
        assert_eq!(contribution::queued(&s, <story_changes::StoryRows as EntityAdapter>::SOURCES, 10).unwrap().len(), 0);
    }

    /// The compute folds the rows it applies itself, so it does not mark its own (scope, story); the
    /// same story's row in another scope is rewritten by the same apply and must be marked.
    #[tokio::test]
    async fn an_inline_sweep_marks_the_same_story_in_other_scopes() {
        let (dir, mut s) = store();
        let home = dir.path();
        let story = "SV-SCOPES-1";
        seed_session(&mut s, home, story, "scoped", true);
        let ids = [story.to_owned()];
        read_blocking(home, &ids, &CallerScope::Human, false).unwrap();
        read_blocking(home, &ids, &CallerScope::Unknown, false).unwrap();
        // Rows built in both scopes.
        for scope in [CallerScope::Human, CallerScope::Unknown] {
            story_changes::totals(home, story, &scope, None, None, None).await.unwrap();
        }
        s.conn.execute("DELETE FROM source_event", []).unwrap();
        s.conn.execute("UPDATE story_totals SET dirty_since_ms = NULL, dirty_at_ms = NULL", []).unwrap();
        // New usage for the session, queued by its trigger.
        let mut e = event("scoped-e2", "scoped", "gpt-6-sol", 1_790_000_000_900, Tokens { input: 5, ..Default::default() });
        e.reported_cost_usd = Some(1.0);
        s.ingest(&[e]).unwrap();
        s.conn.execute("UPDATE story_totals SET dirty_since_ms = NULL, dirty_at_ms = NULL", []).unwrap();
        // A compute in the human scope applies it inline.
        let totals = story_changes::totals(home, story, &CallerScope::Human, None, None, None).await.unwrap().0;
        assert_eq!(totals.cost_usd, Some(3.0), "the compute folded the new usage");
        let marked = |scope: &CallerScope| -> bool {
            s.conn
                .query_row(
                    "SELECT dirty_since_ms IS NOT NULL FROM story_totals WHERE scope_key = ?1 AND story_key = lower(?2)",
                    params![scope.cache_key().to_string(), story],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert!(!marked(&CallerScope::Human), "the computing scope's row is not marked");
        assert!(marked(&CallerScope::Unknown), "the other scope's row was rewritten, so it is marked");
    }
}

/// The current version and the one before it, with `bump` as the step between them.
fn steps_with(bump: story_changes::Bump) -> [(i64, story_changes::Bump); 2] {
    [(story_changes::TOTALS_VERSION - 1, story_changes::Bump::Whole), (story_changes::TOTALS_VERSION, bump)]
}

/// A fill that adds `fillProbe` to the facets from the commits the kept git half counted.
fn fill_probe(stored: &mut StoredTotals) {
    stored.facets["fillProbe"] = stored.git["commits"].clone();
}

/// SV-CDE-102: a version step that only adds fields, with a fill, marks no row for a recompute:
/// each row gets the new field from what it holds, keeps every value it had, and is current.
#[test]
fn sv_cde_102_an_additive_bump_fills_in_place_and_marks_no_row() {
    let (dir, s) = store();
    let ids = ids("SV-ADD", 4);
    fake_computed(dir.path(), &ids);
    s.conn
        .execute(
            "UPDATE story_totals SET version = ?1, totals = '{\"sessions\":3}', facets = '{\"runtimes\":[\"claude\"]}', git = '{\"commits\":2}'",
            params![story_changes::TOTALS_VERSION - 1],
        )
        .unwrap();
    settle(&s);
    let steps = steps_with(story_changes::Bump::Added {
        half: Half::Git,
        fields: &["fillProbe"],
        fill: Some(fill_probe),
    });
    mark_version_changes(&s, 5_000, &steps).unwrap();
    assert_eq!(dirty_count(&s), 0, "no row is marked for a recompute");
    assert!(next_jobs(&s, now_ms() + 3_600_000, 100).unwrap().is_empty());
    let rows: Vec<(i64, String, String, String)> = s
        .conn
        .prepare("SELECT version, totals, facets, git FROM story_totals")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows.len(), ids.len());
    for (version, totals, facets, git) in rows {
        assert_eq!(version, story_changes::TOTALS_VERSION);
        assert_eq!(serde_json::from_str::<Value>(&totals).unwrap(), json!({"sessions": 3}));
        assert_eq!(serde_json::from_str::<Value>(&facets).unwrap(), json!({"runtimes": ["claude"], "fillProbe": 2}));
        assert_eq!(serde_json::from_str::<Value>(&git).unwrap(), json!({"commits": 2}));
    }
    assert!(!refreshing(dir.path(), &ids[0]), "a filled row is current");
    // The next start finds nothing to redo.
    mark_version_changes(&s, 6_000, &steps).unwrap();
    assert_eq!(dirty_count(&s), 0);
}

/// SV-CDE-102: an added field with no fill recomputes only the half that owns it: the rows are
/// marked for their session half, the kept git half is not dropped, and the values stay.
#[test]
fn sv_cde_102_an_added_session_field_without_a_fill_marks_only_the_session_half() {
    let (dir, s) = store();
    let ids = ids("SV-HALF", 3);
    fake_computed(dir.path(), &ids);
    s.conn
        .execute(
            "UPDATE story_totals SET version = ?1, totals = '{\"sessions\":1}'",
            params![story_changes::TOTALS_VERSION - 1],
        )
        .unwrap();
    settle(&s);
    let steps = steps_with(story_changes::Bump::Added { half: Half::Session, fields: &["probe"], fill: None });
    mark_version_changes(&s, 5_000, &steps).unwrap();
    assert_eq!(dirty_count(&s), 3);
    let git_marked: i64 = s
        .conn
        .query_row("SELECT COUNT(*) FROM story_totals WHERE git_dirty_ms IS NOT NULL", [], |r| r.get(0))
        .unwrap();
    assert_eq!(git_marked, 0, "the git half is kept");
    let kept: i64 = s
        .conn
        .query_row("SELECT COUNT(*) FROM story_totals WHERE totals = '{\"sessions\":1}'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(kept, 3, "the values stay until the recompute");
}

/// SV-CDE-102: a step that changes what a half means recomputes every row once: each is marked,
/// computed, and stamped current, and the next start marks nothing.
#[tokio::test]
async fn sv_cde_102_a_breaking_bump_recomputes_every_row_once() {
    let (dir, mut s) = store();
    let home = dir.path();
    let ids = ids("SV-BRK", 3);
    for (n, id) in ids.iter().enumerate() {
        seed_session(&mut s, home, id, &format!("brk-{n}"), true);
    }
    human_read(home, &ids);
    index_all(home).await;
    s.conn
        .execute("UPDATE story_totals SET version = ?1, computed_at_ms = 1", params![story_changes::TOTALS_VERSION - 1])
        .unwrap();
    settle(&s);
    let steps = steps_with(story_changes::Bump::Changed(Half::Git));
    mark_version_changes(&s, 5_000, &steps).unwrap();
    assert_eq!(dirty_count(&s), 3);
    let git_marked: i64 = s
        .conn
        .query_row("SELECT COUNT(*) FROM story_totals WHERE git_dirty_ms IS NOT NULL", [], |r| r.get(0))
        .unwrap();
    assert_eq!(git_marked, 3, "the git half recomputes");
    index_all(home).await;
    let recomputed: i64 = s
        .conn
        .query_row(
            "SELECT COUNT(*) FROM story_totals WHERE version = ?1 AND computed_at_ms > 1 AND totals IS NOT NULL",
            params![story_changes::TOTALS_VERSION],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(recomputed, 3);
    mark_version_changes(&s, 6_000, &steps).unwrap();
    assert_eq!(dirty_count(&s), 0, "a recomputed row is not marked again");
    let reads = story_changes::git_reads(home);
    index_all(home).await;
    assert_eq!(story_changes::git_reads(home), reads, "nothing computes twice");
}

/// SV-CDE-102: a batch of story computes makes one report over the usage events, shared by every
/// story's rows and git half, and each story's totals are what a compute of its own makes. K
/// stories whose sessions arrive after the report was taken cost one more report, not K, and each
/// counts its new session.
#[tokio::test]
async fn sv_cde_102_a_batch_makes_one_usage_report() {
    let (dir, mut s) = store();
    let home = dir.path();
    let db = home.join("usage.sqlite");
    let ids = ids("SV-BAT", 6);
    for (n, id) in ids.iter().enumerate() {
        seed_session(&mut s, home, id, &format!("bat-{n}"), true);
    }
    human_read(home, &ids);
    let worker = Arc::new(Mutex::new(UsageStore::open(&db).unwrap()));
    // No wait between takes here; the window has its own test.
    let batch = Arc::new(story_changes::Batch::with_retake_gap(0));
    let before = crate::session_usage::full_reports(&db);
    let mut computed = 0;
    let mut late: Vec<String> = Vec::new();
    // Past the settle time, so the late story's mark is due at once.
    loop {
        // The guard is dropped before the compute, which stores through the same store.
        let job = next_jobs(&worker.lock().unwrap(), now_ms() + 60_000, 1).unwrap().pop();
        let Some(job) = job else {
            break;
        };
        refresh_in(home, &worker, job, &batch).await;
        computed += 1;
        if computed == 2 {
            // A new session of each of three stories not computed yet, after the report was taken.
            late = s
                .conn
                .prepare("SELECT story_id FROM story_totals WHERE totals IS NULL LIMIT 3")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            for (n, story) in late.iter().enumerate() {
                seed_session(&mut s, home, story, &format!("bat-late-{n}"), true);
            }
        }
        assert!(computed <= ids.len(), "a story computed twice");
    }
    assert_eq!(computed, ids.len());
    assert_eq!(late.len(), 3);
    let batched = human_read(home, &ids);
    let reports = crate::session_usage::full_reports(&db) - before;
    assert_eq!(reports, 2, "one shared report, and one more for all three late stories");
    for story in &late {
        assert_eq!(batched["stories"][story]["totals"]["sessions"], 2, "{story}: the late session counts");
    }
    // Each story on its own, its rows built again from its own report, reports the same totals.
    s.conn.execute("UPDATE story_totals SET computed_at_ms = 1", []).unwrap();
    s.conn.execute("DELETE FROM contribution", []).unwrap();
    mark(&s, &ids.iter().map(|id| id.to_lowercase()).collect(), 1, Half::Git).unwrap();
    for id in &ids {
        let job = next_jobs(&worker.lock().unwrap(), now_ms(), 100)
            .unwrap()
            .into_iter()
            .find(|job| &job.story_id == id)
            .unwrap();
        refresh(home, &worker, job).await;
    }
    let alone = human_read(home, &ids);
    for id in &ids {
        assert_eq!(batched["stories"][id]["totals"], alone["stories"][id]["totals"], "{id}");
    }
}

/// SV-CDE-102: without the late session, a batch of N computes makes exactly one report.
#[tokio::test]
async fn sv_cde_102_report_with_runs_once_per_batch() {
    let (dir, mut s) = store();
    let home = dir.path();
    let db = home.join("usage.sqlite");
    let ids = ids("SV-ONE", 6);
    for (n, id) in ids.iter().enumerate() {
        seed_session(&mut s, home, id, &format!("one-{n}"), true);
    }
    human_read(home, &ids);
    let worker = Arc::new(Mutex::new(UsageStore::open(&db).unwrap()));
    let batch = Arc::new(story_changes::Batch::new());
    let before = crate::session_usage::full_reports(&db);
    let jobs = next_jobs(&worker.lock().unwrap(), now_ms(), 100).unwrap();
    assert_eq!(jobs.len(), ids.len());
    for job in jobs {
        refresh_in(home, &worker, job, &batch).await;
    }
    assert_eq!(crate::session_usage::full_reports(&db) - before, 1);
    let read = human_read(home, &ids);
    for id in &ids {
        assert_eq!(read["stories"][id]["totals"]["sessions"], 1, "{id}");
    }
}

/// Seed one priced session for each of `ids`, numbered from `tag`.
fn seed_all(s: &mut UsageStore, home: &Path, ids: &[String], tag: &str) {
    for (n, id) in ids.iter().enumerate() {
        seed_session(s, home, id, &format!("{tag}-{n}"), true);
    }
}

/// The ids of the stories not computed yet.
fn uncomputed(s: &UsageStore) -> Vec<String> {
    let mut stmt = s.conn.prepare("SELECT story_id FROM story_totals WHERE totals IS NULL").unwrap();
    let rows = stmt.query_map([], |r| r.get(0)).unwrap();
    rows.map(Result::unwrap).collect()
}

/// SV-CDE-102: computes that alternate between two scopes make one report per scope, not one per
/// switch: a scope's first report leaves the other scope's in place.
#[tokio::test]
async fn sv_cde_102_interleaved_scopes_cost_one_report_each() {
    let (dir, mut s) = store();
    let home = dir.path();
    let db = home.join("usage.sqlite");
    let ids = ids("SV-TWO", 4);
    seed_all(&mut s, home, &ids, "two");
    read_blocking(home, &ids, &CallerScope::Human, false).unwrap();
    read_blocking(home, &ids, &CallerScope::Unknown, false).unwrap();
    let worker = Arc::new(Mutex::new(UsageStore::open(&db).unwrap()));
    let jobs = next_jobs(&worker.lock().unwrap(), now_ms(), 100).unwrap();
    assert_eq!(jobs.len(), 2 * ids.len());
    let human_key = describe(&CallerScope::Human).key;
    let (human, unknown): (Vec<Job>, Vec<Job>) = jobs.into_iter().partition(|job| job.scope_key == human_key);
    assert_eq!(human.len(), unknown.len());
    let batch = Arc::new(story_changes::Batch::new());
    let before = crate::session_usage::full_reports(&db);
    for (a, b) in human.into_iter().zip(unknown) {
        refresh_in(home, &worker, a, &batch).await;
        refresh_in(home, &worker, b, &batch).await;
    }
    assert_eq!(crate::session_usage::full_reports(&db) - before, 2, "one report per scope");
}

/// SV-CDE-102: a scope's report is taken again at most once a window. Stories marked inside it
/// wait, still due and uncomputed, without a report; once the window opens they cost one more.
#[tokio::test]
async fn sv_cde_102_stale_stories_wait_for_the_next_window() {
    let (dir, mut s) = store();
    let home = dir.path();
    let db = home.join("usage.sqlite");
    let ids = ids("SV-WIN", 5);
    seed_all(&mut s, home, &ids, "win");
    human_read(home, &ids);
    let worker = Arc::new(Mutex::new(UsageStore::open(&db).unwrap()));
    let batch = Arc::new(story_changes::Batch::new());
    let before = crate::session_usage::full_reports(&db);
    let first = next_jobs(&worker.lock().unwrap(), now_ms(), 2).unwrap();
    for job in first {
        refresh_in(home, &worker, job, &batch).await;
    }
    let late = uncomputed(&s);
    assert_eq!(late.len(), 3);
    seed_all(&mut s, home, &late, "win-late");
    let waiting = next_jobs(&worker.lock().unwrap(), now_ms() + 60_000, 100).unwrap();
    assert_eq!(waiting.len(), late.len());
    for job in waiting {
        refresh_in(home, &worker, job, &batch).await;
    }
    assert_eq!(crate::session_usage::full_reports(&db) - before, 1, "inside the window nothing takes it again");
    assert_eq!(uncomputed(&s).len(), 3, "the marked stories wait");
    let held: i64 = s
        .conn
        .query_row(
            "SELECT COUNT(*) FROM story_totals WHERE totals IS NULL AND dirty_since_ms > ?1",
            params![now_ms()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(held, 3, "due again when the window opens");
    batch.age(10_000);
    let due = next_jobs(&worker.lock().unwrap(), now_ms() + 60_000, 100).unwrap();
    assert_eq!(due.len(), late.len());
    for job in due {
        refresh_in(home, &worker, job, &batch).await;
    }
    assert_eq!(crate::session_usage::full_reports(&db) - before, 2, "one more report for all three");
    let read = human_read(home, &late);
    for story in &late {
        assert_eq!(read["stories"][story]["totals"]["sessions"], 2, "{story}");
    }
}

/// SV-CDE-103: a batch of N stories with M sessions each reads no session whole: the git half takes
/// each session's branch, worktree and workspace from the rows its session half folded, and the
/// batch makes its one usage report.
#[tokio::test]
async fn sv_cde_103_a_batch_makes_no_session_read() {
    let (dir, mut s) = store();
    let home = dir.path();
    let db = home.join("usage.sqlite");
    let ids = ids("SV-REF", 4);
    for id in &ids {
        for m in 0..3 {
            seed_session(&mut s, home, id, &format!("{id}-ref-{m}"), true);
        }
    }
    human_read(home, &ids);
    let worker = Arc::new(Mutex::new(UsageStore::open(&db).unwrap()));
    let batch = Arc::new(story_changes::Batch::new());
    let (reads, reports) = (crate::session_usage::session_read::reads(&db), crate::session_usage::full_reports(&db));
    let jobs = next_jobs(&worker.lock().unwrap(), now_ms(), 100).unwrap();
    assert_eq!(jobs.len(), ids.len());
    for job in jobs {
        refresh_in(home, &worker, job, &batch).await;
    }
    assert_eq!(crate::session_usage::session_read::reads(&db), reads, "no session is read whole");
    assert_eq!(crate::session_usage::full_reports(&db) - reports, 1);
    let read = human_read(home, &ids);
    for id in &ids {
        assert_eq!(read["stories"][id]["totals"]["sessions"], 3, "{id}");
    }
}

/// SV-CDE-105: K queued session sources cost K reads of their own session's usage and no report
/// over every usage event, and share one story resolver and one read of the stored links.
#[tokio::test]
async fn sv_cde_105_queued_session_sources_read_only_their_session() {
    let (dir, mut s) = store();
    let home = dir.path();
    let db = home.join("usage.sqlite");
    let ids = ids("SV-SRC", 5);
    for (n, id) in ids.iter().enumerate() {
        seed_session(&mut s, home, id, &format!("src-{n}"), true);
    }
    let queued: usize = s
        .conn
        .query_row("SELECT count(*) FROM source_event WHERE source_kind = 'session'", [], |r| r.get(0))
        .unwrap();
    assert!(queued >= ids.len());
    let (full, per_session) = (crate::session_usage::full_reports(&db), crate::session_usage::session_reports(&db));
    let (stored, resolvers) = (crate::session_usage::stored_reads(&db), story_changes::resolver_builds(home));
    sweep_sources(&s, home, now_ms()).unwrap();
    assert_eq!(story_changes::resolver_builds(home) - resolvers, 1, "one resolver for the whole sweep");
    assert_eq!(crate::session_usage::stored_reads(&db) - stored, 1, "one read of the stored links for the whole sweep");
    assert_eq!(crate::session_usage::full_reports(&db), full, "no report over every usage event");
    assert_eq!(crate::session_usage::session_reports(&db) - per_session, queued, "one read per queued source");
}


/// SV-CDE-106: back-to-back computes, each followed by the rest, keep the worker's busy share at
/// or under a third (compute and rest only: the loop's poll and sweep turns are not in it).
#[tokio::test(start_paused = true)]
async fn the_rest_after_a_compute_keeps_the_busy_share_under_a_third() {
    assert_eq!(rest_after(Duration::ZERO), REST_GAP, "a cheap compute still rests the floor");
    assert_eq!(rest_after(Duration::from_millis(1_700)), Duration::from_millis(3_400));
    let wake = tokio::sync::Notify::new();
    let (started, mut busy) = (tokio::time::Instant::now(), Duration::ZERO);
    for millis in [900u64, 1_700, 600, 5_900, 1_200, 870, 3_000] {
        let computed = Duration::from_millis(millis);
        tokio::time::sleep(computed).await;
        busy += computed;
        rest(computed, &wake).await;
    }
    let total = tokio::time::Instant::elapsed(&started);
    assert!(busy.as_secs_f64() / total.as_secs_f64() <= 1.0 / 3.0 + 1e-9, "{busy:?} of {total:?}");
}

/// SV-CDE-106: an ask or a change during the rest is served after the floor, not the whole rest.
#[tokio::test(start_paused = true)]
async fn an_ask_during_the_rest_does_not_wait_it_out() {
    let wake = Arc::new(tokio::sync::Notify::new());
    let started = tokio::time::Instant::now();
    let resting = tokio::spawn({
        let wake = Arc::clone(&wake);
        async move { rest(Duration::from_secs(10), &wake).await }
    });
    tokio::time::sleep(Duration::from_secs(3)).await;
    wake.notify_one();
    resting.await.unwrap();
    assert!(
        tokio::time::Instant::elapsed(&started) <= Duration::from_secs(4),
        "a 20 s rest ended when the ask came"
    );
    // A wake that arrived during the compute cuts the extra rest, but never the floor.
    wake.notify_one();
    let before = tokio::time::Instant::now();
    rest(Duration::from_secs(10), &wake).await;
    assert_eq!(tokio::time::Instant::elapsed(&before), REST_GAP);
}

/// SV-CDE-106: only a read that queues a story (new, or asked again after a day) wakes the worker;
/// a board re-asking stories it already asked, or whose totals were just computed, queues nothing.
#[tokio::test]
async fn a_re_ask_of_a_queued_or_computed_story_queues_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let store = UsageStore::open(&home.join("usage.sqlite")).unwrap();
    let ids = vec!["SV-RE-1".to_owned(), "SV-RE-2".to_owned()];
    let ask = |at: i64| answer(&store, &ids, &CallerScope::Human, at, false).unwrap().1;
    assert!(ask(1_000), "new stories are queued");
    assert!(!ask(2_000), "an immediate re-ask queues nothing");
    index_all(home).await;
    assert!(!ask(3_000), "nor does a re-ask after the totals were computed");
    assert!(ask(1_000 + ASKED_EVERY_MS + 5_000), "an ask a day later queues the story again");
}

/// SV-CDE-91 B2a: the commit sweep's fill marks the story it filled quietly (its lines are folded
/// again; no walk, and not taken for activity).
#[test]
fn a_filled_commit_marks_its_story_without_a_walk() {
    let (dir, s) = store();
    let all = ids("SV-FILL", 1);
    read_blocking(dir.path(), &all, &CallerScope::Human, false).unwrap();
    s.conn
        .execute("UPDATE story_totals SET totals = '{}', computed_at_ms = 1, version = ?1", params![story_changes::TOTALS_VERSION])
        .unwrap();
    let scope_key: String = s.conn.query_row("SELECT scope_key FROM story_totals", [], |r| r.get(0)).unwrap();
    let sha = "a".repeat(40);
    let mut row = contribution::Contribution::new();
    row.extra.insert("repos".into(), json!([]));
    row.extra.insert("counted".into(), json!(false));
    contribution::replace_entity(
        &s,
        &scope_key,
        <story_changes::StoryCommits as EntityAdapter>::KIND,
        "sv-fill-0",
        &[(contribution::Source { kind: "commit", id: sha.clone() }, row)],
        <story_changes::StoryCommits as EntityAdapter>::VERSION,
        1,
    )
    .unwrap();
    contribution::enqueue(&s.conn, &contribution::Source { kind: "commit", id: sha }, 1).unwrap();
    let worker = Mutex::new(UsageStore::open(&dir.path().join("usage.sqlite")).unwrap());
    let swept = sweep_commits(&worker, dir.path(), now_ms(), COMMIT_SPEND).unwrap();
    assert_eq!(swept.counted, 1);
    let (dirty, git_dirty, changed): (Option<i64>, Option<i64>, Option<i64>) = s
        .conn
        .query_row("SELECT dirty_since_ms, git_dirty_ms, changed_ms FROM story_totals", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap();
    assert!(dirty.is_some(), "the story is marked");
    assert_eq!((git_dirty, changed), (None, None), "no walk, and not activity");
    assert_eq!(s.conn.query_row::<i64, _, _>("SELECT count(*) FROM source_event", [], |r| r.get(0)).unwrap(), 0);
}

/// The commit sweep and the computes share one rest budget: each turn of either is followed by the
/// rest of its own length, so the combined busy share stays under a third.
#[tokio::test(start_paused = true)]
async fn the_commit_sweep_and_the_computes_share_one_rest_budget() {
    let wake = tokio::sync::Notify::new();
    let (started, mut busy) = (tokio::time::Instant::now(), Duration::ZERO);
    for _ in 0..4 {
        let gap = sweep_turn(
            async {
                tokio::time::sleep(Duration::from_secs(2)).await;
                Ok(story_changes::SweptCommits { counted: 1, ..Default::default() })
            },
            &wake,
        )
        .await;
        busy += Duration::from_secs(2);
        assert_eq!(gap, Duration::ZERO, "a sweep that worked is due again once it has rested");
        tokio::time::sleep(Duration::from_secs(2)).await;
        busy += Duration::from_secs(2);
        rest(Duration::from_secs(2), &wake).await;
    }
    let total = tokio::time::Instant::elapsed(&started);
    assert!(busy.as_secs_f64() / total.as_secs_f64() <= 1.0 / 3.0 + 1e-9, "{busy:?} of {total:?}");
    let idle = sweep_turn(async { Ok(story_changes::SweptCommits::default()) }, &wake).await;
    assert_eq!(idle, COMMIT_SWEEP);
}

/// A story whose git half is marked changed does not reuse it, but the job still carries the stored
/// half so its lines stand while the new commits are counted.
#[tokio::test]
async fn a_git_dirty_job_carries_its_earlier_git_half() {
    let (dir, mut s) = store();
    let home = dir.path();
    let a = "SV-INC-1".to_string();
    seed_session(&mut s, home, &a, "a", true);
    human_read(home, &[a.clone()]);
    index_all(home).await;
    seed_reviews(&s, &a, 1);
    s.conn
        .execute("UPDATE story_totals SET git = ?1, git_dirty_ms = dirty_since_ms", [r#"{"earlier":true}"#])
        .unwrap();
    let due = next_jobs(&s, now_ms() + MIN_REFRESH_MS + SETTLE_MS, 10).unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].git, None, "a changed git half is not reused");
    assert_eq!(due[0].earlier_git.as_deref(), Some(r#"{"earlier":true}"#));
}

/// The sweep counts at least as long as the last compute ran, so with the shared rest it gets at
/// least half of the busy time, and never less than the floor.
#[test]
fn the_sweep_spends_at_least_the_last_compute() {
    assert_eq!(sweep_spend(Duration::ZERO), COMMIT_SPEND);
    assert_eq!(sweep_spend(Duration::from_millis(500)), COMMIT_SPEND);
    for compute in [Duration::from_secs(3), Duration::from_secs(30), COMPUTE_TIMEOUT] {
        assert_eq!(sweep_spend(compute), compute);
    }
}
