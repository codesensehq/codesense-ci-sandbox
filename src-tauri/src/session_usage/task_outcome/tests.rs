// SPDX-License-Identifier: MIT
use super::*;
use crate::session_usage::record::{current_head, SessionRecord};
use crate::session_usage::task::{TaskTargets, LINK_ARTIFACT, OPEN, SUBTASK_PREFIX};
use crate::session_usage::tests::store;
use crate::session_usage::timing::{Signal, Stamped, TimingState, SOURCE_HOOKS};

fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(dir: &Path, path: &str, text: &str) {
    let file = dir.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, text).unwrap();
}

/// A repository with a source file and a test file, committed.
fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    write(dir.path(), "src/lib.rs", "pub fn a() {}\n");
    write(dir.path(), "tests/a_test.rs", "fn t() {}\n");
    write(dir.path(), ".github/ci.yml", "on: push\n");
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "init"]);
    dir
}

fn started_in(dir: &Path) -> TaskRecord {
    let mut t = TaskRecord::new("t", "goal");
    t.worktree = Some(dir.to_string_lossy().into_owned());
    t.layers.source = current_head(dir);
    t.tamper_globs = vec![
        "**/tests/**".into(),
        "**/*_test.rs".into(),
        ".github/**".into(),
    ];
    t
}

#[test]
fn editing_a_test_file_flags_the_task_and_editing_source_alone_does_not() {
    let dir = repo();
    let task = started_in(dir.path());
    // Nothing changed yet.
    assert_eq!(
        compute_tamper(&task).unwrap(),
        Tamper {
            flag: false,
            files: vec![]
        }
    );
    // A source-only edit, unstaged, does not flag.
    write(dir.path(), "src/lib.rs", "pub fn a() { 1; }\n");
    assert_eq!(
        compute_tamper(&task).unwrap(),
        Tamper {
            flag: false,
            files: vec![]
        }
    );
    // An unstaged edit to a test file does.
    write(dir.path(), "tests/a_test.rs", "fn t() { assert!(true); }\n");
    assert_eq!(
        compute_tamper(&task).unwrap(),
        Tamper {
            flag: true,
            files: vec!["tests/a_test.rs".into()]
        }
    );
}

#[test]
fn a_committed_a_staged_and_a_new_check_file_all_count() {
    let dir = repo();
    let task = started_in(dir.path());
    // Committed after the task started.
    write(dir.path(), ".github/ci.yml", "on: [push, pull_request]\n");
    git(dir.path(), &["commit", "-q", "-am", "ci"]);
    // Staged.
    write(dir.path(), "tests/a_test.rs", "fn t() { 1; }\n");
    git(dir.path(), &["add", "tests/a_test.rs"]);
    // Untracked and new.
    write(dir.path(), "tests/b_test.rs", "fn b() {}\n");
    let tamper = compute_tamper(&task).unwrap();
    assert!(tamper.flag);
    assert_eq!(
        tamper.files,
        [".github/ci.yml", "tests/a_test.rs", "tests/b_test.rs"]
    );
}

#[test]
fn a_tamper_that_cannot_be_worked_out_says_why() {
    let dir = repo();
    let mut task = started_in(dir.path());
    task.layers.source = Some("0000000000000000000000000000000000000000".into());
    assert!(compute_tamper(&task)
        .unwrap_err()
        .contains("start commit may be gone"));
    let mut task = started_in(dir.path());
    task.tamper_globs.clear();
    assert!(compute_tamper(&task)
        .unwrap_err()
        .contains("no tamper_globs"));
    let mut task = started_in(dir.path());
    task.worktree = None;
    assert!(compute_tamper(&task)
        .unwrap_err()
        .contains("no working folder"));
    let mut task = started_in(dir.path());
    task.layers.source = None;
    assert!(compute_tamper(&task).unwrap_err().contains("not recorded"));
}

#[test]
fn closing_a_task_records_the_tamper_fact_and_an_unknown_one_says_why() {
    let dir = repo();
    let (_d, store) = store();
    store
        .insert_task_if_absent(&started_in(dir.path()))
        .unwrap();
    write(dir.path(), "tests/a_test.rs", "fn t() { 2; }\n");
    assert!(finish_task(&store, "t", "success", 10).unwrap());
    let t = store.read_task("t").unwrap().unwrap();
    assert_eq!(
        (t.status.as_str(), t.raw_status.as_deref()),
        ("done", Some("success"))
    );
    assert_eq!(
        t.tamper,
        Some(Tamper {
            flag: true,
            files: vec!["tests/a_test.rs".into()]
        })
    );
    // A task with no recorded start says so instead.
    let mut bare = TaskRecord::new("bare", "goal");
    bare.tamper_globs = vec!["**/tests/**".into()];
    store.insert_task_if_absent(&bare).unwrap();
    finish_task(&store, "bare", "failed", 11).unwrap();
    let t = store.read_task("bare").unwrap().unwrap();
    assert_eq!((t.status.as_str(), t.tamper), ("abandoned", None));
    assert!(t.unavailable["tamper"].contains("no working folder"));
}

#[test]
fn a_session_ending_closes_its_task_and_its_routine_runs_task_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("usage.sqlite");
    let open = || UsageStore::open(&path).unwrap();
    let mut record = SessionRecord::new("tab-1");
    record.run_id = Some("run-1".into());
    open().write_session_record(&record).unwrap();
    open()
        .insert_task_if_absent(&TaskRecord::new("tab-1", "own"))
        .unwrap();
    open()
        .insert_task_if_absent(&TaskRecord::new("run-1", "routine run"))
        .unwrap();
    // An unrelated task and a sub-task of another session stay open.
    open()
        .insert_task_if_absent(&TaskRecord::new("other", "other"))
        .unwrap();

    let closed = close_for_session(&open(), "tab-1", "success", 100).unwrap();
    assert_eq!(closed, ["tab-1", "run-1"]);
    // A duplicate signal_done closes nothing and rewrites nothing.
    assert!(close_for_session(&open(), "tab-1", "failed", 200)
        .unwrap()
        .is_empty());
    let after_restart = open();
    for id in ["tab-1", "run-1"] {
        let t = after_restart.read_task(id).unwrap().unwrap();
        assert_eq!(
            (t.status.as_str(), t.raw_status.as_deref(), t.closed_at_ms),
            ("done", Some("success"), Some(100)),
            "{id}"
        );
    }
    assert_eq!(
        after_restart.read_task("other").unwrap().unwrap().status,
        OPEN
    );
    assert_eq!(status_for("needs-attention"), "abandoned");
    assert_eq!(status_for("failed"), "abandoned");
}

fn hook(session: &str, at_ms: i64, event: &str) -> Stamped {
    Stamped {
        session_id: session.into(),
        at_ms,
        signal: Signal::Activity(crate::agent_state::hook_event(event).unwrap(), SOURCE_HOOKS),
    }
}

#[test]
fn the_outcome_sums_time_and_cost_over_the_sessions_and_sets_targets_beside_them() {
    use crate::session_usage::event::Tokens;
    let (_d, mut store) = store();
    for (session, native) in [("tab-1", "native-1"), ("tab-2", "native-2")] {
        let mut r = SessionRecord::new(session);
        r.native_session_id = Some(native.into());
        store.write_session_record(&r).unwrap();
    }
    let from = crate::session_usage::tests::local_noon(2026, 10, 15);
    let mut state = TimingState::default();
    for e in [
        hook("tab-1", 0, "UserPromptSubmit"),
        hook("tab-1", 60_000, "Stop"),
        hook("tab-1", 120_000, "SessionEnd"),
        hook("tab-2", 0, "UserPromptSubmit"),
        hook("tab-2", 30_000, "Stop"),
    ] {
        let mut e = e;
        e.at_ms += from;
        state.apply(&store, &e).unwrap();
    }
    let tokens = Tokens {
        input: 1_000_000,
        ..Default::default()
    };
    for (id, native) in [("e1", "native-1"), ("e2", "native-2")] {
        store
            .ingest(&[crate::session_usage::tests::event(
                id,
                native,
                "claude-opus-5-5",
                from + 30_000,
                tokens.clone(),
            )])
            .unwrap();
    }
    let mut task = TaskRecord::new("t", "goal");
    task.created_at_ms = from;
    task.closed_at_ms = Some(from + 120_000);
    task.targets = Some(TaskTargets {
        source: "default".into(),
        values: BTreeMap::from([
            ("wall_minutes".into(), json!(90)),
            ("cost_usd".into(), json!(25)),
            ("review_rounds".into(), json!(3)),
            ("tests_green".into(), json!(true)),
        ]),
    });
    task.acceptance_met = Some(true);
    store.insert_task_if_absent(&task).unwrap();
    store.add_task_link("t", LINK_SESSION, "tab-1").unwrap();
    store.add_task_link("t", LINK_SESSION, "tab-2").unwrap();
    // A review task relates to it; an unrelated task and a non-review relation do not count.
    let mut review = TaskRecord::new("rev-1", "review");
    review.task_type = "review".into();
    store.insert_task_if_absent(&review).unwrap();
    store
        .add_task_link("rev-1", "relates_to:reviews", "t")
        .unwrap();
    store
        .add_task_link("rev-1", LINK_ARTIFACT, "design/SV-1/review-r1.md")
        .unwrap();
    let mut other = TaskRecord::new("rev-x", "not a review");
    other.task_type = "feature".into();
    store.insert_task_if_absent(&other).unwrap();
    store
        .add_task_link("rev-x", "relates_to:reviews", "t")
        .unwrap();

    let task = store.read_task("t").unwrap().unwrap();
    let outcome = store.read_outcome(&task).unwrap();
    assert_eq!(outcome.scope, "interval");
    let time = outcome.time.unwrap();
    assert_eq!(
        (time.wall_ms, time.active_ms, time.sessions),
        (120_000 * 2, 60_000 + 30_000, 2)
    );
    let cost = outcome.cost.unwrap();
    assert_eq!(cost.models, ["claude-opus-5-5"]);
    assert_eq!(cost.input_tokens, 2_000_000);
    assert!(cost.cost_usd.unwrap() > 0.0);
    assert_eq!(
        (outcome.review_rounds, outcome.reviews.as_slice()),
        (1, &["rev-1".to_string()][..])
    );
    assert_eq!(outcome.acceptance_met, Some(true));
    let fact = |name: &str| outcome.targets.iter().find(|f| f.name == name).unwrap();
    assert_eq!(fact("wall_minutes").actual, Some(json!(4.0)));
    assert_eq!(fact("review_rounds").actual, Some(json!(1)));
    assert!(fact("cost_usd").actual.is_some());
    assert_eq!(fact("tests_green").actual, None, "nothing measures it");
    assert!(!outcome.unavailable.contains_key("scope"));
    assert!(!outcome.unavailable.values().any(|v| v == "pending SV-CDE-29"));
}

#[test]
fn a_task_with_no_sessions_or_usage_says_what_is_missing() {
    let (_d, store) = store();
    store
        .insert_task_if_absent(&TaskRecord::new("t", "goal"))
        .unwrap();
    let outcome = store
        .read_outcome(&store.read_task("t").unwrap().unwrap())
        .unwrap();
    assert!(outcome.time.is_none() && outcome.cost.is_none());
    assert!(outcome.unavailable["time"].contains("no turn"));
    assert!(outcome.unavailable["cost"].contains("no usage"));
    assert!(outcome.unavailable["acceptanceMet"].contains("task_update"));
}

#[test]
fn a_task_view_nests_the_subtree_with_each_tasks_links_and_the_label() {
    let (_d, store) = store();
    for (id, parent) in [
        ("lead", None),
        ("build", Some("lead")),
        ("review", Some("build")),
    ] {
        let mut t = TaskRecord::new(id, format!("goal {id}"));
        t.parent_id = parent.map(str::to_owned);
        store.insert_task_if_absent(&t).unwrap();
    }
    store
        .add_task_link("review", "relates_to:reviews", "build")
        .unwrap();
    store
        .update_task(
            "build",
            &crate::session_usage::task::TaskPatch {
                label: Some(("good".into(), Some("clean".into()))),
                ..Default::default()
            },
            Some("alice"),
            9,
        )
        .unwrap();
    let view = store.task_view("lead").unwrap().unwrap();
    assert_eq!(view.children.len(), 1);
    let build = &view.children[0];
    assert_eq!(build.task.label.as_ref().unwrap().verdict, "good");
    assert_eq!(build.children[0].task.task_id, "review");
    assert_eq!(build.children[0].links[0].kind, "relates_to:reviews");
    assert!(view.other_skills["unavailable"].as_str().unwrap().contains("no conversation"));
    assert!(store.task_view("missing").unwrap().is_none());
    let wire = serde_json::to_value(&view).unwrap();
    assert!(
        wire["task"]["taskId"].is_string()
            && wire["children"][0]["outcome"]["reviewRounds"].is_number()
    );
}

#[test]
fn a_routine_runs_task_closes_in_the_usage_store_beside_the_coordinator() {
    let home = tempfile::tempdir().unwrap();
    let coordinator = crate::coordinator::default_store_path(home.path());
    let usage = home.path().join("usage.sqlite");
    UsageStore::open(&usage)
        .unwrap()
        .insert_task_if_absent(&TaskRecord::new("run-9", "r"))
        .unwrap();
    close_run_task(&coordinator, "run-9", "succeeded");
    let t = UsageStore::open(&usage)
        .unwrap()
        .read_task("run-9")
        .unwrap()
        .unwrap();
    assert_eq!(
        (t.status.as_str(), t.raw_status.as_deref()),
        ("done", Some("succeeded"))
    );
    // A store that is not a coordinator file closes nothing.
    close_run_task(&home.path().join("elsewhere.sqlite3"), "run-9", "failed");
}

#[test]
fn every_terminal_path_closes_through_the_one_funnel_and_the_first_close_wins() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("usage.sqlite");
    let open = || UsageStore::open(&path).unwrap();
    for id in [
        "clean",
        "failed",
        "signal-then-exit",
        "exit-then-signal",
        "cancelled",
    ] {
        open()
            .insert_task_if_absent(&TaskRecord::new(id, id))
            .unwrap();
    }
    // A clean exit and a failed exit close it as the exit was classified.
    close_for_session_at(&path, "clean", "success", 10).unwrap();
    close_for_session_at(&path, "failed", "failed", 10).unwrap();
    // A signal comes first and the exit that follows changes neither status nor time...
    close_for_session_at(&path, "signal-then-exit", "success", 10).unwrap();
    close_for_session_at(&path, "signal-then-exit", "failed", 20).unwrap();
    // ...and an exit that came first keeps its own verdict against a late signal.
    close_for_session_at(&path, "exit-then-signal", "failed", 10).unwrap();
    close_for_session_at(&path, "exit-then-signal", "success", 20).unwrap();
    // A routine run's cancellation, through the run funnel.
    let coordinator = dir.path().join("orchestration").join("coordinator.sqlite3");
    close_run_task(&coordinator, "cancelled", "cancelled");
    let read = |id: &str| {
        let t = open().read_task(id).unwrap().unwrap();
        (t.status, t.raw_status.unwrap(), t.closed_at_ms.unwrap())
    };
    assert_eq!(read("clean"), ("done".into(), "success".into(), 10));
    assert_eq!(read("failed"), ("abandoned".into(), "failed".into(), 10));
    assert_eq!(
        read("signal-then-exit"),
        ("done".into(), "success".into(), 10)
    );
    assert_eq!(
        read("exit-then-signal"),
        ("abandoned".into(), "failed".into(), 10)
    );
    assert_eq!(
        open()
            .read_task("cancelled")
            .unwrap()
            .unwrap()
            .raw_status
            .as_deref(),
        Some("cancelled")
    );
    // The routine phases map: delivered and ended are done; cancelled, timed out and failed are not.
    for (raw, status) in [
        ("delivered", "done"),
        ("ended", "done"),
        ("succeeded", "done"),
        ("cancelled", "abandoned"),
        ("timed_out", "abandoned"),
    ] {
        assert_eq!(status_for(raw), status, "{raw}");
    }
}

#[test]
fn the_funnel_computes_tamper_whichever_path_closed_the_task() {
    let dir = repo();
    let (_d, store) = store();
    store
        .insert_task_if_absent(&started_in(dir.path()))
        .unwrap();
    write(dir.path(), "tests/a_test.rs", "fn t() { 3; }\n");
    // A manual close (as task_update does) records the same tamper fact as a signal_done does.
    assert!(close_as(&store, "t", "superseded", "superseded", 5).unwrap());
    let t = store.read_task("t").unwrap().unwrap();
    assert_eq!((t.status.as_str(), t.closed_at_ms), ("superseded", Some(5)));
    assert_eq!(
        t.tamper,
        Some(Tamper {
            flag: true,
            files: vec!["tests/a_test.rs".into()]
        })
    );
    // A second close of any kind is a no-op.
    assert!(!close_as(&store, "t", "done", "done", 9).unwrap());
    assert_eq!(store.read_task("t").unwrap().unwrap().status, "superseded");
}

#[test]
fn a_subtask_covers_only_its_own_interval_and_the_session_task_covers_it_all() {
    use crate::session_usage::event::Tokens;
    let (_d, mut store) = store();
    let mut r = SessionRecord::new("tab");
    r.native_session_id = Some("native".into());
    store.write_session_record(&r).unwrap();
    let mut state = TimingState::default();
    for e in [
        // Build: 0-60s. Review subtask: 100-130s. More build: 200-260s.
        hook("tab", 0, "UserPromptSubmit"),
        hook("tab", 60_000, "Stop"),
        hook("tab", 100_000, "UserPromptSubmit"),
        hook("tab", 130_000, "Stop"),
        hook("tab", 200_000, "UserPromptSubmit"),
        hook("tab", 260_000, "Stop"),
    ] {
        state.apply(&store, &e).unwrap();
    }
    let tokens = |n: u64| Tokens {
        input: n,
        ..Default::default()
    };
    // Calls: one in the build, one inside the review, one in the later build.
    store
        .ingest(&[
            crate::session_usage::tests::event(
                "c1",
                "native",
                "claude-opus-5-5",
                50_000,
                tokens(1_000_000),
            ),
            crate::session_usage::tests::event(
                "c2",
                "native",
                "claude-opus-5-5",
                120_000,
                tokens(2_000_000),
            ),
            crate::session_usage::tests::event(
                "c3",
                "native",
                "claude-opus-5-5",
                250_000,
                tokens(4_000_000),
            ),
        ])
        .unwrap();
    let mut session_task = TaskRecord::new("tab", "session");
    session_task.created_at_ms = 0;
    store.insert_task_if_absent(&session_task).unwrap();
    store.add_task_link("tab", LINK_SESSION, "tab").unwrap();
    let mut review = TaskRecord::new(format!("{SUBTASK_PREFIX}r1"), "review pass");
    review.created_at_ms = 100_000;
    store.insert_task_if_absent(&review).unwrap();
    store
        .add_task_link(&review.task_id, LINK_SESSION, "tab")
        .unwrap();
    store
        .close_task(&review.task_id, "done", Some("task_update"), 130_000)
        .unwrap();

    let whole = store
        .read_outcome(&store.read_task("tab").unwrap().unwrap())
        .unwrap();
    assert_eq!(whole.scope, "session");
    assert_eq!(whole.time.unwrap().active_ms, 60_000 + 30_000 + 60_000);
    assert_eq!(
        whole.cost.unwrap().input_tokens,
        7_000_000,
        "a parent includes its children"
    );

    // A wake reuses this session and has a run identity, not a `task-` identity.
    let mut wake = TaskRecord::new("wake-run-1", "wake pass");
    wake.created_at_ms = 100_000;
    store.insert_task_if_absent(&wake).unwrap();
    store.add_task_link(&wake.task_id, LINK_SESSION, "tab").unwrap();
    store.close_task(&wake.task_id, "done", Some("delivered"), 130_000).unwrap();
    let wake = store.task_view(&wake.task_id).unwrap().unwrap();
    assert_eq!(wake.outcome.scope, "interval");
    assert_eq!(wake.outcome.time.unwrap().active_ms, 30_000);
    assert_eq!(wake.outcome.cost.unwrap().input_tokens, 2_000_000);

    let part = store
        .read_outcome(&store.read_task(&review.task_id).unwrap().unwrap())
        .unwrap();
    assert_eq!(part.scope, "interval");
    let time = part.time.unwrap();
    assert_eq!(
        (time.wall_ms, time.active_ms, time.idle_ms),
        (30_000, 30_000, 0)
    );
    let cost = part.cost.unwrap();
    assert_eq!(
        cost.input_tokens, 2_000_000,
        "only the call inside the interval"
    );
    assert_eq!(cost.models, ["claude-opus-5-5"]);

    // A subtask whose interval holds no turn or call says so rather than borrowing the session's.
    let mut quiet = TaskRecord::new(format!("{SUBTASK_PREFIX}r2"), "quiet");
    quiet.created_at_ms = 270_000;
    store.insert_task_if_absent(&quiet).unwrap();
    store
        .add_task_link(&quiet.task_id, LINK_SESSION, "tab")
        .unwrap();
    store
        .close_task(&quiet.task_id, "done", None, 280_000)
        .unwrap();
    let none = store
        .read_outcome(&store.read_task(&quiet.task_id).unwrap().unwrap())
        .unwrap();
    assert!(
        none.cost.is_none()
            && none.unavailable["cost"].contains("inside its start-to-close interval")
    );
    assert_eq!(none.time.unwrap().active_ms, 0);

    // A fresh routine owns its launched session, so its run id is that session's root task.
    let mut fresh_record = SessionRecord::new("fresh-tab");
    fresh_record.run_id = Some("fresh-run".into());
    store.write_session_record(&fresh_record).unwrap();
    let fresh = TaskRecord::new("fresh-run", "fresh routine");
    store.insert_task_if_absent(&fresh).unwrap();
    store.add_task_link(&fresh.task_id, LINK_SESSION, "fresh-tab").unwrap();
    assert_eq!(store.read_outcome(&fresh).unwrap().scope, "session");
}

/// Case rows 11, 12, 13 and 16. Fails on main: there is no `task_of_call`, and an open sub-task's
/// interval runs to now however many later `task_start`s the session made.
#[test]
fn the_task_of_a_call_is_the_innermost_interval_else_the_sessions_root() {
    let (_d, store) = store();
    for session in ["tab", "relaunch"] {
        store.write_session_record(&SessionRecord::new(session)).unwrap();
        let mut root = TaskRecord::new(session, "session");
        root.created_at_ms = 0;
        store.insert_task_if_absent(&root).unwrap();
        store.add_task_link(session, LINK_SESSION, session).unwrap();
    }
    // Two open reviews in one session: the first stops where the second starts (row 11).
    let mut ids = Vec::new();
    for (name, start) in [("r1", 100_000), ("r2", 200_000)] {
        let mut task = TaskRecord::new(format!("{SUBTASK_PREFIX}{name}"), name);
        task.created_at_ms = start;
        store.insert_task_if_absent(&task).unwrap();
        store.add_task_link(&task.task_id, LINK_SESSION, "tab").unwrap();
        ids.push(task.task_id);
    }
    let (r1, r2) = (ids[0].as_str(), ids[1].as_str());
    let first = store.read_task(r1).unwrap().unwrap();
    assert_eq!(
        store.task_interval(&first, &["tab"]).unwrap(),
        Some((100_000, 200_000)),
        "the open review ends at the next start, not at now"
    );
    // A child of the first review opens inside it: it does not end its open parent, only a
    // sibling does (fails on the any-later-task rule: r1 would end at 160_000).
    let mut child = TaskRecord::new(format!("{SUBTASK_PREFIX}c1"), "c1");
    child.created_at_ms = 160_000;
    child.parent_id = Some(r1.to_owned());
    store.insert_task_if_absent(&child).unwrap();
    store.add_task_link(&child.task_id, LINK_SESSION, "tab").unwrap();
    assert_eq!(
        store.task_interval(&first, &["tab"]).unwrap(),
        Some((100_000, 200_000)),
        "a nested child does not end its open parent"
    );
    assert_eq!(
        store.task_of_call("tab", Some(170_000)).unwrap(),
        (child.task_id.clone(), "interval")
    );
    for (ts, expected) in [
        (Some(50_000), ("tab", "session")),
        (Some(150_000), (r1, "interval")),
        (Some(200_000), (r2, "interval")),
        (Some(900_000), (r2, "interval")),
        (None, ("tab", "session")),
    ] {
        let (task, basis) = store.task_of_call("tab", ts).unwrap();
        assert_eq!((task.as_str(), basis), expected, "{ts:?}");
    }
    // A relaunch is a new session with its own root task (row 13).
    assert_eq!(
        store.task_of_call("relaunch", Some(150_000)).unwrap(),
        ("relaunch".to_string(), "session")
    );
    // A closed review holds only up to its close; later calls fall back to the root.
    store.close_task(r1, "done", Some("task_update"), 120_000).unwrap();
    store.close_task(r2, "done", Some("task_update"), 220_000).unwrap();
    assert_eq!(store.task_of_call("tab", Some(150_000)).unwrap().1, "session");
    assert_eq!(store.task_of_call("tab", Some(210_000)).unwrap().0, r2);
    // A routine session answers with its run's task (row 16).
    let mut routine = SessionRecord::new("routine-tab");
    routine.run_id = Some("run-9".into());
    store.write_session_record(&routine).unwrap();
    store.insert_task_if_absent(&TaskRecord::new("run-9", "routine run")).unwrap();
    store.add_task_link("run-9", LINK_SESSION, "routine-tab").unwrap();
    assert_eq!(
        store.task_of_call("routine-tab", Some(5)).unwrap(),
        ("run-9".to_string(), "session")
    );
}

#[test]
fn the_playbook_is_read_from_the_parent_and_pending_until_the_parent_is_captured() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("usage.sqlite");
    let open = || UsageStore::open(&path).unwrap();
    // The child is captured first, naming a parent that has no row yet.
    let mut child = TaskRecord::new("worker", "child");
    child.parent_id = Some("lead".into());
    child.parent_source = Some("launcher".into());
    open().insert_task_if_absent(&child).unwrap();
    assert_eq!(
        open().task_view("worker").unwrap().unwrap().playbook["pending"],
        json!("the parent's task is not captured yet")
    );
    // The launcher's task arrives later, with its main skill and the hash it launched with.
    let mut lead = TaskRecord::new("lead", "parent");
    lead.main_skill = Some("bmad-build".into());
    lead.layers.skills = vec![crate::session_usage::task::SkillVersion {
        name: "bmad-build".into(),
        hash: Some("h-build".into()),
        reason: None,
    }];
    open().insert_task_if_absent(&lead).unwrap();
    // A restart reads the same.
    let view = open().task_view("worker").unwrap().unwrap();
    assert_eq!(
        (
            view.playbook["skill"].as_str(),
            view.playbook["hash"].as_str()
        ),
        (Some("bmad-build"), Some("h-build"))
    );
    // A parent with no main skill, and a root, say why.
    open()
        .insert_task_if_absent(&TaskRecord::new("bare", "no skill"))
        .unwrap();
    let mut other = TaskRecord::new("other", "child");
    other.parent_id = Some("bare".into());
    open().insert_task_if_absent(&other).unwrap();
    assert!(
        open().task_view("other").unwrap().unwrap().playbook["unavailable"]
            .as_str()
            .unwrap()
            .contains("no main skill")
    );
    assert!(
        open().task_view("lead").unwrap().unwrap().playbook["unavailable"]
            .as_str()
            .unwrap()
            .contains("no parent")
    );
}

#[test]
fn an_unhashed_parent_skill_keeps_its_reason_on_the_childs_playbook_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("usage.sqlite");
    let store = UsageStore::open(&path).unwrap();
    let mut parent = TaskRecord::new("parent-unhashed", "Lead the work");
    parent.main_skill = Some("bmad-build".into());
    parent.layers.skills = vec![crate::session_usage::task::SkillVersion {
        name: "bmad-build".into(), hash: None,
        reason: Some("no manifest file backs this skill".into()),
    }];
    store.insert_task_if_absent(&parent).unwrap();
    let mut child = TaskRecord::new("child-unhashed", "Build the part");
    child.parent_id = Some(parent.task_id.clone());
    store.insert_task_if_absent(&child).unwrap();
    drop(store);
    let playbook = UsageStore::open(&path).unwrap()
        .task_view(&child.task_id).unwrap().unwrap().playbook;
    assert_eq!(playbook["skill"], "bmad-build");
    assert!(playbook["hash"].is_null());
    assert_eq!(playbook["unavailable"], "no manifest file backs this skill");
}

#[test]
fn a_hand_started_cli_signal_does_not_create_a_task_without_a_scheduled_capture() {
    use crate::session_usage::record::hand_started;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("usage.sqlite");
    let store = UsageStore::open(&path).unwrap();
    let record = hand_started("hand-started", Some("native-hand-started"), Some("acme"), Some("dev"));
    store.write_session_record(&record).unwrap();
    assert!(close_for_session(&store, "hand-started", "success", 10).unwrap().is_empty());
    drop(store);
    let reopened = UsageStore::open(&path).unwrap();
    assert!(reopened.read_task("hand-started").unwrap().is_none());
    assert!(reopened.task_view("hand-started").unwrap().is_none());
    assert!(reopened.list_tasks(&Default::default()).unwrap().is_empty());
}

#[test]
fn cancelling_a_routine_without_a_captured_task_does_not_promise_future_capture() {
    let dir = tempfile::tempdir().unwrap();
    let coordinator = dir.path().join("orchestration/coordinator.sqlite");
    std::fs::create_dir_all(coordinator.parent().unwrap()).unwrap();
    close_run_task(&coordinator, "never-started-run", "cancelled");
    let store = UsageStore::open(&dir.path().join("usage.sqlite")).unwrap();
    assert!(store.read_task("never-started-run").unwrap().is_none());
    assert!(store.list_tasks(&Default::default()).unwrap().is_empty());
}


#[test]
fn task_read_uses_persisted_skill_invocations_and_limits_a_wake_to_its_interval() {
    use crate::session_usage::conversation::{ConversationMeta, IndexedCall, State};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("usage.sqlite");
    let mut store = UsageStore::open(&path).unwrap();
    let mut record = SessionRecord::new("skill-tab");
    record.native_session_id = Some("native-skills".into());
    store.write_session_record(&record).unwrap();
    let mut root = TaskRecord::new("skill-tab", "build");
    root.main_skill = Some("bmad-build".into());
    store.insert_task_if_absent(&root).unwrap();
    store.add_task_link(&root.task_id, LINK_SESSION, "skill-tab").unwrap();
    let mut wake = TaskRecord::new("skill-wake-run", "review");
    wake.main_skill = Some("bmad-code-review".into());
    wake.created_at_ms = 100_000;
    store.insert_task_if_absent(&wake).unwrap();
    store.add_task_link(&wake.task_id, LINK_SESSION, "skill-tab").unwrap();
    store.close_task(&wake.task_id, "done", Some("delivered"), 130_000).unwrap();
    let meta = ConversationMeta {
        session_id: "skill-tab".into(), state: State::Present,
        source_format: Some("anthropic-session-jsonl-v1".into()), bytes: 100,
        turn_count: 4, truncated: None, captured_at_ms: 260_000,
        unavailable: Default::default(),
    };
    let calls: Vec<_> = [
        (50_000, "before"), (110_000, "bmad-code-review"),
        (120_000, "impeccable"), (250_000, "after"),
    ].into_iter().enumerate().map(|(n, (at, skill))| IndexedCall { aliases: Vec::new(),
        turn_index: n as u32, seq: 0, native_id: Some(format!("call-{n}")),
        name: "Skill".into(), mcp_server: None, is_error: Some(false),
        ts_ms: Some(at), skill: Some(skill.into()),
    }).collect();
    store.write_conversation(&meta, &calls).unwrap();
    drop(store);
    let store = UsageStore::open(&path).unwrap();
    let read = |store: &UsageStore, task: &str| crate::mcp::task_tools::read(store, "skill-tab", crate::mcp::task_tools::TaskReadParams {
        session_id: "skill-tab".into(), task_id: task.into(),
    }).unwrap();
    let root = read(&store, "skill-tab");
    assert_eq!(root.other_skills, json!({"skills": ["after", "before", "bmad-code-review", "impeccable"]}));
    let wake = read(&store, "skill-wake-run");
    assert_eq!(wake.other_skills, json!({"skills": ["impeccable"]}));
    // An available index with no skill invocations is an available empty list.
    let mut store = store;
    store.write_conversation(&meta, &[]).unwrap();
    assert_eq!(read(&store, "skill-tab").other_skills, json!({"skills": []}));
}

#[test]
fn a_reader_sees_pending_or_a_verdict_and_a_racing_close_loser_cannot_recompute_it() {
    use crate::session_usage::task::TAMPER_PENDING;
    let dir = repo();
    let (db, store) = store();
    let path = db.path().join("usage.sqlite");
    store.insert_task_if_absent(&started_in(dir.path())).unwrap();
    // The winner pauses after its terminal CAS, before its Git read has finished.
    assert!(store.close_task("t", DONE, Some("success"), 10).unwrap());
    let pending = store.task_view("t").unwrap().unwrap().task;
    assert_eq!(pending.status, DONE);
    assert!(pending.tamper.is_none());
    assert_eq!(pending.unavailable["tamper"], TAMPER_PENDING);
    write(dir.path(), "tests/a_test.rs", "a later edit\n");
    assert!(!finish_task(&store, "t", "failed", 20).unwrap());
    assert_eq!(store.read_task("t").unwrap().unwrap(), pending,
        "the racing loser must not recompute Git or replace the pending winner");
    let verdict = Tamper { flag: false, files: vec![] };
    store.finish_task_tamper("t", &Ok(verdict.clone())).unwrap();
    store.finish_task_tamper("t", &Ok(Tamper { flag: true, files: vec!["tests/a_test.rs".into()] })).unwrap();
    store.finish_task_tamper("t", &Err("late failure".into())).unwrap();
    let finished = store.read_task("t").unwrap().unwrap();
    assert_eq!(finished.tamper, Some(verdict));
    assert!(!finished.unavailable.contains_key("tamper"));
    assert_eq!(finished.closed_at_ms, Some(10));
    // Reopen the actual database path used by the shared store fixture.
    drop(store);
    let reopened = UsageStore::open(&path).unwrap();
    assert_eq!(reopened.read_task("t").unwrap().unwrap(), finished);
}

#[test]
fn task_read_joins_agent_layer_versions_and_missing_reasons_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("usage.sqlite");
    let store = UsageStore::open(&path).unwrap();
    let mut record = SessionRecord::new("agent-layer-session");
    record.agent = Some("acme/dev/builder@7".into());
    record.manifest_hash = Some("manifest-launch-hash".into());
    store.write_session_record(&record).unwrap();
    let mut missing = SessionRecord::new("missing-layer-session");
    missing.unavailable("agent", "the launch used a built-in CLI");
    missing.unavailable("manifestHash", "the launch had no manifest");
    store.write_session_record(&missing).unwrap();
    let task = TaskRecord::new("agent-layer-task", "inspect launch versions");
    store.insert_task_if_absent(&task).unwrap();
    for session in ["agent-layer-session", "missing-layer-session", "no-record-session"] {
        store.add_task_link(&task.task_id, LINK_SESSION, session).unwrap();
    }
    store.insert_task_if_absent(&TaskRecord::new("no-session-task", "unlinked")).unwrap();
    drop(store);
    let store = UsageStore::open(&path).unwrap();
    let read = |id: &str| {
        let result = crate::mcp::task_tools::read(&store, "agent-layer-session", crate::mcp::task_tools::TaskReadParams {
            session_id: "agent-layer-session".into(), task_id: id.into(),
        }).unwrap();
        serde_json::to_value(result).unwrap()
    };
    let view = read("agent-layer-task");
    let layers = view["agentLayers"]["sessions"].as_array().expect("task_read exposes linked agent layers");
    let known = layers.iter().find(|l| l["sessionId"] == "agent-layer-session").unwrap();
    assert_eq!(known["agent"], "acme/dev/builder@7");
    assert_eq!(known["manifestHash"], "manifest-launch-hash");
    assert_eq!(known["unavailable"], json!({}));
    let missing = layers.iter().find(|l| l["sessionId"] == "missing-layer-session").unwrap();
    assert!(missing["agent"].is_null() && missing["manifestHash"].is_null());
    assert_eq!(missing["unavailable"]["agent"], "the launch used a built-in CLI");
    assert_eq!(missing["unavailable"]["manifestHash"], "the launch had no manifest");
    let absent = layers.iter().find(|l| l["sessionId"] == "no-record-session").unwrap();
    assert!(absent["unavailable"]["agent"].as_str().unwrap().contains("no session record"));
    assert!(absent["unavailable"]["manifestHash"].as_str().unwrap().contains("no session record"));
    assert!(read("no-session-task")["agentLayers"]["unavailable"].as_str().unwrap().contains("no session links"));
}

fn attributed_conversations_after_clear() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("usage.sqlite");
    let store = UsageStore::open(&path).unwrap();
    store.register_launch("clear-launch", "acme", Some("dev"), "acme/dev/builder", "clear-handle", None).unwrap();
    store.write_session_record(&SessionRecord::new("clear-launch")).unwrap();
    store.insert_task_if_absent(&TaskRecord::new("clear-launch", "work across clear")).unwrap();
    // Repeating a supported link does not multiply usage.
    store.add_task_link("clear-launch", LINK_SESSION, "clear-launch").unwrap();
    store.add_task_link("clear-launch", LINK_SESSION, "clear-launch").unwrap();
    drop(store);
    for (request, native, stamp, model, input, cost) in [
        ("before-clear", "native-before", "2026-10-15T12:00:00Z", "claude-opus-5-5", "100", "1"),
        ("after-clear", "native-after", "2026-10-15T12:02:00Z", "claude-sonnet-4-6", "200", "2"),
        ("later", "native-after", "2026-10-15T12:04:00Z", "claude-sonnet-4-6", "400", "4"),
    ] {
        let attr = |key: &str, value: &str| json!({"key": key, "value": {"stringValue": value}});
        let export = json!({"resourceLogs": [{"scopeLogs": [{"logRecords": [{"attributes": [
            attr("event.name", "api_request"), attr("event.timestamp", stamp),
            attr("request_id", request), attr("session.id", native), attr("model", model),
            attr("input_tokens", input), attr("output_tokens", "0"), attr("cost_usd", cost),
        ]}]}]}]});
        assert_eq!(crate::session_usage::ingest_telemetry_for_at(&path, "clear-launch", &export).unwrap(), 1);
    }
    let store = UsageStore::open(&path).unwrap();
    assert_eq!(store.read_session_record("clear-launch").unwrap().unwrap().native_session_id.as_deref(), Some("native-before"));
    assert_eq!(store.report(&Filter { session_id: Some("clear-launch".into()), ..Default::default() }).unwrap().sessions.len(), 2);
    (dir, path)
}

#[test]
fn task_read_sums_all_attributed_native_conversations_after_clear_and_reopen() {
    let (_dir, path) = attributed_conversations_after_clear();
    let store = UsageStore::open(&path).unwrap();
    let view = crate::mcp::task_tools::read(&store, "clear-launch", crate::mcp::task_tools::TaskReadParams {
        session_id: "clear-launch".into(), task_id: "clear-launch".into(),
    }).unwrap();
    let cost = view.outcome.cost.unwrap();
    assert_eq!(cost.cost_usd, Some(7.0));
    assert_eq!(cost.input_tokens, 700);
    assert_eq!(cost.models.len(), 2);
    assert!(cost.models.contains(&"claude-opus-5-5".into()) && cost.models.contains(&"claude-sonnet-4-6".into()));
}

#[test]
fn a_post_clear_subtask_reads_only_its_attributed_interval_after_reopen() {
    let (_dir, path) = attributed_conversations_after_clear();
    let store = UsageStore::open(&path).unwrap();
    let mut task = TaskRecord::new("task-after-clear", "review after clear");
    task.created_at_ms = chrono::DateTime::parse_from_rfc3339("2026-10-15T12:01:00Z").unwrap().timestamp_millis();
    store.insert_task_if_absent(&task).unwrap();
    store.add_task_link(&task.task_id, LINK_SESSION, "clear-launch").unwrap();
    store.close_task(&task.task_id, DONE, Some("success"), task.created_at_ms + 120_000).unwrap();
    drop(store);
    let store = UsageStore::open(&path).unwrap();
    let view = crate::mcp::task_tools::read(&store, "clear-launch", crate::mcp::task_tools::TaskReadParams {
        session_id: "clear-launch".into(), task_id: task.task_id,
    }).unwrap();
    assert_eq!(view.outcome.scope, "interval");
    let cost = view.outcome.cost.expect("post-clear attributed interval has recorded usage");
    assert_eq!(cost.cost_usd, Some(2.0));
    assert_eq!(cost.input_tokens, 200);
    assert_eq!(cost.models, ["claude-sonnet-4-6"]);
}

#[test]
fn task_usage_keeps_native_only_fallback_without_launch_attribution() {
    use crate::session_usage::event::Tokens;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("usage.sqlite");
    let mut store = UsageStore::open(&path).unwrap();
    let mut record = SessionRecord::new("native-only-tab");
    record.native_session_id = Some("native-only-id".into());
    store.write_session_record(&record).unwrap();
    let at = crate::session_usage::tests::local_noon(2026, 10, 15);
    let mut event = crate::session_usage::tests::event("native-only-call", "native-only-id", "claude-opus-5-5", at, Tokens { input: 123, ..Default::default() });
    event.reported_cost_usd = Some(0.5);
    store.ingest(&[event]).unwrap();
    for id in ["native-only-tab", "task-native-only"] {
        let mut task = TaskRecord::new(id, "native fallback");
        task.created_at_ms = at - 1000;
        store.insert_task_if_absent(&task).unwrap();
        store.add_task_link(id, LINK_SESSION, "native-only-tab").unwrap();
        store.close_task(id, DONE, Some("success"), at + 1000).unwrap();
    }
    drop(store);
    let store = UsageStore::open(&path).unwrap();
    for id in ["native-only-tab", "task-native-only"] {
        let cost = store.task_view(id).unwrap().unwrap().outcome.cost.unwrap();
        assert_eq!(cost.input_tokens, 123);
        assert_eq!(cost.cost_usd, Some(0.5));
    }
}
