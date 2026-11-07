// SPDX-License-Identifier: MIT
use super::*;
use crate::session_usage::{
    event::Tokens,
    record::SessionRecord,
    task::TaskRecord,
    tests::{event, store},
    timing::{Signal, Stamped, TimingState},
    StoryLink, UsageStore,
};
use codesense_pty_core::activity::Event;
use serde_json::json;

mod light_reads;

fn params() -> Params {
    serde_json::from_value(json!({"storyId":"SV-29"})).unwrap()
}

fn seed(s: &mut UsageStore, home: &Path, id: &str, priced: bool, link_first: bool) {
    let link = StoryLink {
        story_id: "SV-29".into(),
        source: home.join("story.md"),
        org: "acme".into(),
    };
    if link_first {
        s.link_story(id, &link).unwrap();
    }
    let mut r = SessionRecord::new(id);
    r.story_id = Some("SV-29".into());
    r.worktree = Some(home.join("worktree").to_string_lossy().into());
    s.write_session_record(&r).unwrap();
    if !link_first {
        s.link_story(id, &link).unwrap();
    }
    s.link_story(id, &link).unwrap();
    let mut e = event(
        id,
        id,
        if priced {
            "gpt-6-sol"
        } else {
            "unpriced-model"
        },
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
async fn get(home: &Path) -> Value {
    serde_json::to_value(read(home, &params(), &CallerScope::Human).await.unwrap()).unwrap()
}

#[tokio::test]
async fn live_merged_and_removed_story_survive_reopen_and_both_link_orders() {
    for status in ["in-progress", "done", "removed"] {
        for first in [false, true] {
            let (dir, mut s) = store();
            std::fs::write(
                dir.path().join("story.md"),
                format!("---\nstatus: {status}\n---\n"),
            )
            .unwrap();
            std::fs::create_dir(dir.path().join("worktree")).unwrap();
            seed(&mut s, dir.path(), "tab", true, first);
            if status == "removed" {
                std::fs::remove_dir(dir.path().join("worktree")).unwrap();
            }
            let before = get(dir.path()).await;
            drop(s);
            let _reopened = UsageStore::open(&dir.path().join("usage.sqlite")).unwrap();
            assert_eq!(before, get(dir.path()).await);
            assert_eq!(before["sessions"].as_array().unwrap().len(), 1);
            assert_eq!(before["sessions"][0]["basis"], "exact");
            assert_eq!(before["sessions"][0]["session"]["sessionId"], "tab");
            assert_eq!(before["totals"]["costUsd"], 2.0);
        }
    }
}

#[tokio::test]
async fn empty_and_unpriced_story_never_reports_a_partial_cost() {
    let (dir, mut s) = store();
    let empty = get(dir.path()).await;
    assert_eq!(empty["sessions"], json!([]));
    assert_eq!(empty["totals"]["sessions"], 0);
    assert!(empty["totals"]["costUsd"].is_null());
    assert!(empty["unavailable"]["costUsd"].is_string());
    for key in ["wallMs", "activeMs", "idleMs"] {
        assert_eq!(empty["totals"][key], 0);
    }
    seed(&mut s, dir.path(), "priced", true, false);
    seed(&mut s, dir.path(), "unknown", false, true);
    let result = get(dir.path()).await;
    assert_eq!(result["totals"]["sessions"], 2);
    assert!(result["totals"]["costUsd"].is_null());
    assert!(result["unavailable"]["costUsd"].is_string());
    for key in ["wallMs", "activeMs", "idleMs"] {
        assert!(result["totals"][key].is_null());
    }
    assert!(result["unavailable"]["time"].is_string());
}

#[tokio::test]
async fn overlapping_time_and_two_review_tasks_are_facts() {
    let (dir, mut s) = store();
    let mut timing = TimingState::default();
    for (id, start, stop, end) in [("a", 100, 200, 300), ("b", 150, 250, 350)] {
        seed(&mut s, dir.path(), id, true, false);
        for (at, event) in [
            (start, Event::Submitted),
            (stop, Event::Stopped),
            (end, Event::Ended),
        ] {
            timing
                .apply(
                    &s,
                    &Stamped {
                        session_id: id.into(),
                        at_ms: at,
                        signal: Signal::Activity(event, "fixture"),
                    },
                )
                .unwrap();
        }
    }
    let mut task = TaskRecord::new("dev", "build");
    task.story_id = Some("SV-29".into());
    s.insert_task_if_absent(&task).unwrap();
    for id in ["review-1", "review-2"] {
        let mut review = TaskRecord::new(id, "review");
        review.story_id = Some("SV-29".into());
        review.task_type = "review".into();
        s.insert_task_if_absent(&review).unwrap();
        s.add_task_link(id, "relates_to:reviews", "dev").unwrap();
    }
    let result = get(dir.path()).await;
    assert_eq!(result["totals"]["wallMs"], 250);
    assert_eq!(result["totals"]["activeMs"], 200);
    assert_eq!(result["totals"]["idleMs"], 200);
    assert_eq!(result["totals"]["reviewTasks"], 2);
    // No team of the story's org is on this Mac, so its markers cannot be read: the
    // rounds say so instead of falling back to the task count.
    assert!(result["totals"]["reviewRounds"].is_null());
    assert!(result["unavailable"]["reviewRounds"].is_string());
    assert_eq!(result["tasks"].as_array().unwrap().len(), 3);
    assert_eq!(
        result["tasks"][0]["reviews"],
        json!(["review-1", "review-2"])
    );
    assert!(result["unavailable"].get("time").is_none());
    seed(&mut s, dir.path(), "untimed", true, false);
    let mixed = get(dir.path()).await;
    assert_eq!(mixed["totals"]["sessions"], 3);
    assert_eq!(mixed["totals"]["wallMs"], 250);
    assert_eq!(mixed["totals"]["activeMs"], 200);
    assert_eq!(mixed["totals"]["idleMs"], 200);
    assert!(mixed["unavailable"]["time"].is_string());
}

#[tokio::test]
async fn all_sessions_and_attributed_native_ids_count_once() {
    let (dir, mut s) = store();
    for index in 0..51 {
        seed(&mut s, dir.path(), &format!("session-{index}"), true, false);
    }
    let mut record = SessionRecord::new("launch");
    record.native_session_id = Some("native-1".into());
    s.write_session_record(&record).unwrap();
    s.register_launch("launch", "acme", Some("dev"), "agent", "handle", None)
        .unwrap();
    let owner = s.launch_owner("launch").unwrap().unwrap();
    for native in ["native-1", "native-2"] {
        s.attribute_if_absent(native, &owner, "launch").unwrap();
        s.link_story(
            native,
            &StoryLink {
                story_id: "SV-29".into(),
                source: dir.path().join("story.md"),
                org: "acme".into(),
            },
        )
        .unwrap();
        let mut e = event(
            native,
            native,
            "gpt-6-sol",
            1_790_000_000_000,
            Tokens {
                input: 10,
                ..Default::default()
            },
        );
        e.reported_cost_usd = Some(1.0);
        s.ingest(&[e]).unwrap();
    }
    let result = get(dir.path()).await;
    assert_eq!(result["totals"]["sessions"], 52);
    assert_eq!(result["totals"]["costUsd"], 104.0);
    assert_eq!(result["sessions"].as_array().unwrap().len(), 52);
}

#[tokio::test]
async fn legacy_native_conversations_without_a_session_record_count_once() {
    let (dir, mut s) = store();
    s.register_launch("old-launch", "acme", Some("dev"), "agent", "handle", None)
        .unwrap();
    let owner = s.launch_owner("old-launch").unwrap().unwrap();
    for native in ["old-1", "old-2"] {
        s.attribute_if_absent(native, &owner, "old-launch").unwrap();
        s.link_story(
            native,
            &StoryLink {
                story_id: "SV-29".into(),
                source: dir.path().join("story.md"),
                org: "acme".into(),
            },
        )
        .unwrap();
        let mut e = event(
            native,
            native,
            "gpt-6-sol",
            1_790_000_000_000,
            Tokens {
                input: 10,
                ..Default::default()
            },
        );
        e.reported_cost_usd = Some(1.0);
        s.ingest(&[e]).unwrap();
    }
    let result = get(dir.path()).await;
    assert_eq!(result["totals"]["sessions"], 1);
    assert_eq!(result["totals"]["costUsd"], 2.0);
    assert!(result["sessions"][0]["session"].is_null());
    assert_eq!(
        result["sessions"][0]["nativeSessionIds"],
        json!(["old-1", "old-2"])
    );
}

struct GitStory {
    dir: tempfile::TempDir,
    repo: git2::Repository,
    root: git2::Oid,
}
impl GitStory {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        repo.remote("origin", "https://github.com/acme/code.git")
            .unwrap();
        let mut fixture = Self {
            dir,
            repo,
            root: git2::Oid::zero(),
        };
        fixture.root = fixture.commit("refs/heads/main", "root", "README", 1000, &[]);
        fixture
            .repo
            .reference("refs/remotes/origin/main", fixture.root, true, "fixture")
            .unwrap();
        fixture
    }
    fn commit(
        &self,
        reference: &str,
        subject: &str,
        file: &str,
        at: i64,
        parents: &[git2::Oid],
    ) -> git2::Oid {
        self.commit_authored(reference, subject, file, at, at, parents)
    }
    fn commit_authored(
        &self,
        reference: &str,
        subject: &str,
        file: &str,
        at: i64,
        authored: i64,
        parents: &[git2::Oid],
    ) -> git2::Oid {
        let mut index = self.repo.index().unwrap();
        if let Some(parent) = parents.first() {
            index
                .read_tree(&self.repo.find_commit(*parent).unwrap().tree().unwrap())
                .unwrap();
        } else {
            index.clear().unwrap();
        }
        let path = self.dir.path().join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("{subject}\n")).unwrap();
        index.add_path(Path::new(file)).unwrap();
        let tree = self.repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::new("fixture", "fixture@example.com", &git2::Time::new(at, 0))
            .unwrap();
        let author = git2::Signature::new("fixture", "fixture@example.com", &git2::Time::new(authored, 0))
            .unwrap();
        let parents: Vec<_> = parents
            .iter()
            .map(|p| self.repo.find_commit(*p).unwrap())
            .collect();
        self.repo
            .commit(
                Some(reference),
                &author,
                &sig,
                subject,
                &tree,
                &parents.iter().collect::<Vec<_>>(),
            )
            .unwrap()
    }
    fn seed(&self, home: &Path, store: &mut UsageStore, branch: &str, removed: bool) {
        seed(store, home, "git-session", true, false);
        if !home.join("story.md").exists() {
            std::fs::write(home.join("story.md"), "---\nsource_repo: acme/code\n---\n").unwrap();
        }
        let mut record = store.read_session_record("git-session").unwrap().unwrap();
        record.branch = Some(branch.into());
        record.workspace = Some(self.dir.path().to_string_lossy().into());
        record.worktree = Some(if removed {
            home.join("removed").to_string_lossy().into()
        } else {
            self.dir.path().to_string_lossy().into()
        });
        store.write_session_record(&record).unwrap();
    }
}
#[tokio::test]
async fn git_live_and_removed_worktree_keep_commits_and_complete_totals() {
    for removed in [false, true] {
        let (home, mut store) = store();
        let fixture = GitStory::new();
        fixture.commit(
            "refs/heads/feat/SV-29-work",
            "work",
            "changed.rs",
            1100,
            &[fixture.root],
        );
        fixture.seed(home.path(), &mut store, "feat/SV-29-work", removed);
        if removed {
            let path = home.path().join("removed");
            std::fs::create_dir(&path).unwrap();
            let principal = crate::domain::Principal::from(1u64);
            let record = crate::install::worktree_index::WorktreeStoryRecord::new(
                path.clone(),
                "acme",
                crate::domain::SourceRepoSlug::new("acme", "code").unwrap(),
                "SV-29",
                home.path().join("story.md"),
            );
            crate::install::worktree_index::write_record(home.path(), &principal, &record).unwrap();
            crate::install::worktree_index::remove_record(home.path(), &principal, &path).unwrap();
            std::fs::remove_dir(&path).unwrap();
            let tip = fixture
                .repo
                .refname_to_id("refs/heads/feat/SV-29-work")
                .unwrap();
            fixture
                .repo
                .reference(
                    "refs/remotes/origin/feat/SV-29-work",
                    tip,
                    true,
                    "remote fixture",
                )
                .unwrap();
            fixture
                .repo
                .find_reference("refs/heads/feat/SV-29-work")
                .unwrap()
                .delete()
                .unwrap();
        }
        let result = get(home.path()).await;
        assert_eq!(result["totals"]["commits"], 1);
        assert_eq!(result["totals"]["files"], 1);
        assert_eq!(result["commits"][0]["files"], json!(["changed.rs"]));
        assert_eq!(result["commits"][0]["basis"], "associated");
        assert_eq!(result["branches"][0]["name"], "feat/SV-29-work");
        assert_eq!(result["branches"][0]["basis"], "exact");
        if removed {
            assert!(result["worktrees"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w["state"] == "removed" && w["basis"] == "exact"));
        }
        drop(store);
        assert_eq!(result, get(home.path()).await);
    }
}
#[tokio::test]
async fn git_merge_deleted_squash_and_unmerged_deleted_are_distinguished() {
    for kind in ["merge", "squash", "missing"] {
        let (home, mut store) = store();
        let f = GitStory::new();
        f.seed(home.path(), &mut store, "feat/SV-29-work", true);
        let tip = f.commit(
            "refs/heads/feat/SV-29-work",
            "work",
            "changed.rs",
            1100,
            &[f.root],
        );
        if kind != "missing" {
            let (subject, parents) = if kind == "merge" {
                (
                    "Merge pull request #42 from acme/feat/SV-29-work",
                    vec![f.root, tip],
                )
            } else {
                ("SV-29: work (#42)", vec![f.root])
            };
            let merged = f.commit("refs/heads/main", subject, "changed.rs", 1200, &parents);
            f.repo
                .reference("refs/remotes/origin/main", merged, true, "merged")
                .unwrap();
        }
        f.repo
            .find_reference("refs/heads/feat/SV-29-work")
            .unwrap()
            .delete()
            .unwrap();
        let result = get(home.path()).await;
        let branch = &result["branches"][0];
        match kind {
            "merge" => {
                assert_eq!(branch["pr"]["number"], 42);
                assert_eq!(result["totals"]["commits"], 1);
            }
            "squash" => {
                assert!(branch["pr"]["number"].is_null());
                assert_eq!(branch["unavailable"], "branch not found locally");
                assert_eq!(result["prs"][0]["number"], 42);
                assert_eq!(result["prs"][0]["basis"], "associated");
                assert!(result["prs"][0]["branch"].is_null());
                assert_eq!(result["prs"][0]["commits"]["unavailable"], "squash-merged");
                assert_eq!(result["prs"][0]["files"], json!(["changed.rs"]));
            }
            _ => assert_eq!(branch["unavailable"], "branch not found locally"),
        }
    }
}
#[tokio::test]
async fn git_paging_150_commits_has_known_total_and_refuses_stale_cursor() {
    let (home, mut store) = store();
    let f = GitStory::new();
    f.seed(home.path(), &mut store, "feat/SV-29-work", false);
    let mut tip = f.root;
    for n in 0..150 {
        tip = f.commit(
            "refs/heads/feat/SV-29-work",
            &format!("work {n}"),
            "file.rs",
            1100 + n,
            &[tip],
        );
    }
    let p: Params = serde_json::from_value(json!({"storyId":"SV-29","limit":100})).unwrap();
    let first =
        serde_json::to_value(read(home.path(), &p, &CallerScope::Human).await.unwrap()).unwrap();
    assert_eq!(first["total"], 150);
    assert_eq!(first["commits"].as_array().unwrap().len(), 100);
    let next: Params =
        serde_json::from_value(json!({"storyId":"SV-29","limit":100,"cursor":first["nextCursor"]}))
            .unwrap();
    let second =
        serde_json::to_value(read(home.path(), &next, &CallerScope::Human).await.unwrap()).unwrap();
    assert_eq!(second["commits"].as_array().unwrap().len(), 50);
    assert!(second["nextCursor"].is_null());
    f.commit(
        "refs/heads/feat/SV-29-work",
        "more",
        "file.rs",
        1400,
        &[tip],
    );
    assert!(read(home.path(), &next, &CallerScope::Human)
        .await
        .unwrap_err()
        .contains("cursor"));
}
#[tokio::test]
async fn planned_touchpoints_decisions_untouched_and_missing_section() {
    let (home, mut store) = store();
    let f = GitStory::new();
    f.seed(home.path(), &mut store, "feat/SV-29-work", false);
    let first = f.commit("refs/heads/feat/SV-29-work", "one", "a.rs", 1100, &[f.root]);
    f.commit(
        "refs/heads/feat/SV-29-work",
        "two",
        "outside.rs",
        1200,
        &[first],
    );
    std::fs::write(
        home.path().join("story.md"),
        "## Touchpoints\n- `a.rs:12` and `untouched/`\n## Decisions\n1. Keep `a.rs` small.\n",
    )
    .unwrap();
    let value = get(home.path()).await;
    assert_eq!(value["planned"], json!(["a.rs", "untouched/"]));
    assert_eq!(value["actual"][0]["inPlanned"], true);
    assert_eq!(value["actual"][0]["decision"], 1);
    assert_eq!(value["actual"][1]["inPlanned"], false);
    assert_eq!(value["plannedUntouched"], json!(["untouched/"]));
    std::fs::write(home.path().join("story.md"), "## Purpose\nhello\n").unwrap();
    assert_eq!(
        get(home.path()).await["unavailable"]["planned"],
        "story has no Touchpoints section"
    );
}
#[tokio::test]
async fn task_outcomes_share_planned_and_rework_facts() {
    let (home, mut store) = store();
    let f = GitStory::new();
    f.seed(home.path(), &mut store, "feat/SV-29-work", false);
    let first = f.commit("refs/heads/feat/SV-29-work", "one", "a.rs", 1100, &[f.root]);
    let fix = f.commit("refs/heads/repair-followup", "fix", "a.rs", 1300, &[first]);
    let unrelated = f.commit(
        "refs/heads/repair-followup",
        "unrelated",
        "b.rs",
        1400,
        &[fix],
    );
    let late = f.commit(
        "refs/heads/repair-followup",
        "task work after review",
        "late.rs",
        1500,
        &[unrelated],
    );
    let sig = git2::Signature::now("test", "test@example.com").unwrap();
    let note = format!(
        "late.rs\n  s_late::t_late 1\n---\n{}",
        json!({"schema_version":"authorship/3.0.0","sessions":{"s_late":{"agent_id":{"tool":"codex","id":"git-session"}}}})
    );
    assert!(crate::attribution::authorship::Authorship::parse(&note).is_ok());
    f.repo
        .note(&sig, &sig, Some("refs/notes/ai"), late, &note, true)
        .unwrap();
    add_repo_session(home.path(), &mut store, "repair-session", &f);
    let mut repair = store
        .read_session_record("repair-session")
        .unwrap()
        .unwrap();
    repair.branch = Some("repair-followup".into());
    store.write_session_record(&repair).unwrap();
    std::fs::write(home.path().join("story.md"), "## Touchpoints\n`a.rs`\n").unwrap();
    let mut task = TaskRecord::new("dev", "develop");
    task.story_id = Some("SV-29".into());
    task.worktree = Some(f.dir.path().to_string_lossy().into());
    task.created_at_ms = 1_050_000;
    task.closed_at_ms = Some(1_150_000);
    store.insert_task_if_absent(&task).unwrap();
    store
        .add_task_link("dev", "session", "git-session")
        .unwrap();
    let mut review = TaskRecord::new("review", "review");
    review.task_type = "review".into();
    review.created_at_ms = 1_160_000;
    review.closed_at_ms = Some(1_200_000);
    store.insert_task_if_absent(&review).unwrap();
    store
        .add_task_link("review", "relates_to:reviews", "dev")
        .unwrap();
    let value = serde_json::to_value(
        crate::mcp::task_tools::read(
            &store,
            "git-session",
            serde_json::from_value(json!({"sessionId":"git-session","taskId":"dev"})).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        value["outcome"]["plannedVsActual"]["actual"][0]["path"],
        "a.rs"
    );
    assert_eq!(
        value["outcome"]["rework"],
        json!({"commits":2,"files":["a.rs","late.rs"]})
    );
    assert_eq!(
        value["outcome"]["plannedVsActual"]["actual"]
            .as_array()
            .unwrap()
            .iter()
            .map(|file| file["path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["a.rs", "late.rs"]
    );
    assert!(value["outcome"]["unavailable"].get("scope").is_none());
    assert!(!value.to_string().contains("pending SV-CDE-29"));
    let listed = serde_json::to_value(
        crate::mcp::task_tools::list(
            &store,
            "git-session",
            serde_json::from_value(json!({"sessionId":"git-session"})).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let dev = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["taskId"] == "dev")
        .unwrap();
    assert_eq!(dev["outcome"], value["outcome"]);
    assert_eq!(
        dev["outcome"]["rework"],
        json!({"commits":2,"files":["a.rs","late.rs"]})
    );
}

#[tokio::test]
async fn task_list_returns_the_same_planned_and_rework_outcome() {
    let (home, mut store) = store();
    let f = GitStory::new();
    f.seed(home.path(), &mut store, "feat/SV-29-work", false);
    f.commit(
        "refs/heads/feat/SV-29-work",
        "work",
        "a.rs",
        1100,
        &[f.root],
    );
    let mut task = TaskRecord::new("dev", "develop");
    task.story_id = Some("SV-29".into());
    task.created_at_ms = 1_000_000;
    task.closed_at_ms = Some(1_200_000);
    store.insert_task_if_absent(&task).unwrap();
    store
        .add_task_link("dev", "session", "git-session")
        .unwrap();
    let rows = crate::mcp::task_tools::list(
        &store,
        "git-session",
        serde_json::from_value(json!({"sessionId":"git-session"})).unwrap(),
    )
    .unwrap();
    let value = serde_json::to_value(rows).unwrap();
    assert!(value[0]["outcome"]["plannedVsActual"].is_object());
    assert!(value[0]["outcome"]["rework"].is_object());
}
/// One backend: the MCP answer for `args` is the agent projection of `bridge`, the page
/// bridge's read of `agent_read_params(args)`, and it leaves totals alone (I3).
fn assert_mcp_projects_the_bridge_read(mcp: rmcp::model::CallToolResult, bridge: &Value, args: &Value) {
    let text = serde_json::to_value(mcp).unwrap();
    let parsed: Value = serde_json::from_str(text["content"][0]["text"].as_str().unwrap()).unwrap();
    let params: AgentParams = serde_json::from_value(args.clone()).unwrap();
    assert_eq!(parsed, agent::agent_projection(bridge.clone(), &params).unwrap());
    assert_eq!(parsed["totals"], bridge["totals"]);
}

#[cfg(feature = "dashboard-publish")]
#[tokio::test]
async fn mcp_tauri_and_published_catalog_support_new_fields_and_page_two() {
    fn assert_published_keys(value: &Value) {
        fn only(value: &Value, allowed: &[&str]) {
            assert!(value.as_object().unwrap().keys().all(|key| allowed.contains(&key.as_str())), "unexpected published keys: {value}");
        }
        only(value, &["storyId", "sessions", "tasks", "totals", "unavailable", "warnings", "commits", "branches", "worktrees", "prs", "total", "nextCursor", "scanLimit", "plannedBase", "planned", "actual", "plannedUntouched", "historyAsOf"]);
        for pr in value["prs"].as_array().unwrap() {
            only(pr, &["basis", "number", "mergedSha", "mergedAt", "source", "commits", "numberReason"]);
            if pr["commits"].is_object() { only(&pr["commits"], &["unavailable"]); }
        }
        for branch in value["branches"].as_array().unwrap() {
            only(branch, &["basis", "pr", "prs", "availability", "mergedSha", "unavailable"]);
            only(&branch["pr"], &["number", "numberReason", "unavailable"]);
            assert!(branch["prs"].as_array().unwrap().iter().all(|number| number.is_u64()));
        }
        for commit in value["commits"].as_array().unwrap() {
            only(commit, &["sha", "authoredAt", "files", "sessionIds", "basis", "unavailable"]);
        }
        for worktree in value["worktrees"].as_array().unwrap() { only(worktree, &["state", "basis"]); }
        for file in value["actual"].as_array().unwrap() { only(file, &["path", "inPlanned", "area", "decision"]); }
        only(&value["unavailable"], &["costUsd", "time", "commits", "files", "lines", "reviewRounds", "planned", "attribution", "historyFetch", "prs"]);
        assert!(value["unavailable"].as_object().unwrap().values().all(Value::is_string));
        fn reasons(value: &Value) {
            match value {
                Value::Object(fields) => for (key, value) in fields {
                    if key == "unavailable" {
                        let text = value.to_string();
                        assert!(!text.contains("://") && !text.contains("PRIVATE") && !text.contains("/private/") && !text.contains("/tmp/"), "private metadata in published reason: {text}");
                    }
                    reasons(value);
                },
                Value::Array(rows) => rows.iter().for_each(reasons),
                _ => {},
            }
        }
        reasons(value);
    }

    let fixture = crate::context_repo::caller_scope::tests::AppFixture::new();
    let home = &fixture.fixture.home;
    let mut store = UsageStore::open(&home.join("usage.sqlite")).unwrap();
    let f = GitStory::new();
    f.seed(home, &mut store, "feat/SV-29-work", false);
    let one = f.commit(
        "refs/heads/feat/SV-29-work",
        "PRIVATE subject",
        "one.rs",
        1100,
        &[f.root],
    );
    let tip = f.commit(
        "refs/heads/feat/SV-29-work",
        "PRIVATE second",
        "two.rs",
        1200,
        &[one],
    );
    let merged = f.commit(
        "refs/heads/main",
        "Merge pull request #5 from acme/feat/SV-29-work",
        "two.rs",
        1300,
        &[f.root, tip],
    );
    f.repo
        .reference("refs/remotes/origin/main", merged, true, "merge fixture")
        .unwrap();
    f.repo.set_head("refs/heads/feat/SV-29-work").unwrap();
    let source = fixture
        .fixture
        .write("child", "content/story.md", "## Touchpoints\n`one.rs`\n");
    store
        .conn
        .execute(
            "UPDATE session_story SET story_path=?1,org='acme/ctx' WHERE session_id='git-session'",
            [source.to_string_lossy().as_ref()],
        )
        .unwrap();
    let index = crate::install::worktree_index::WorktreeStoryRecord::new(
        f.dir.path().to_owned(),
        "acme/ctx",
        crate::domain::SourceRepoSlug::new("acme", "code").unwrap(),
        "SV-29",
        &source,
    );
    crate::install::worktree_index::write_record(
        home,
        &crate::domain::Principal::from(1u64),
        &index,
    )
    .unwrap();
    let folder = fixture.fixture.path("child", "content");
    let app = &fixture.app;
    {
        let args = json!({"storyId":"SV-29","limit":1});
        let mcp = crate::mcp::handler::story_changes_request(
            serde_json::from_value(args.clone()).unwrap(),
            &CallerScope::Human,
        )
        .await
        .unwrap();
        let tauri = crate::commands::data_read_model::story_changes(
            app.handle().clone(),
            serde_json::from_value(args.clone()).unwrap(),
            None,
            None,
        )
        .await
        .unwrap();
        assert_mcp_projects_the_bridge_read(mcp, &tauri, &args);
        assert_eq!(tauri["total"], 2);
        let entry = crate::dashboard_publish::catalog::CATALOG
            .iter()
            .find(|q| q.name == "storyChanges")
            .unwrap();
        let published = (entry.run)(
            Default::default(),
            crate::dashboard_publish::catalog::Call {
                args: args.clone(),
                folder: folder.clone(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_published_keys(&published);
        assert_eq!(published["commits"][0]["files"], json!(["two.rs"]));
        assert_eq!(published["planned"], json!(["one.rs"]));
        assert_eq!(published["branches"][0]["basis"], "exact");
        assert_eq!(tauri["branches"][0]["pr"]["url"], "https://github.com/acme/code/pull/5");
        assert_eq!(published["branches"][0]["pr"], json!({"number":5}));
        assert!(published["prs"][0].get("url").is_none());
        assert!(tauri["prs"][0]["files"].as_array().is_some_and(|files| !files.is_empty()));
        assert!(published["prs"][0].get("files").is_none());
        assert!(published["commits"][0].get("author").is_none());
        assert!(published["actual"][0].get("lines").is_none());
        assert_eq!(published["prs"][0]["basis"], "exact");
        assert_eq!(
            published["worktrees"][0],
            json!({"state":"live","basis":"exact"})
        );
        assert!(published["branches"][0].get("name").is_none());
        assert!(published["prs"][0].get("branch").is_none());
        let out = published.to_string();
        assert!(!out.contains("PRIVATE"));
        assert!(!out.contains(f.dir.path().to_str().unwrap()));
        let range_args = json!({"storyId":"SV-29","limit":1,"include":["ranges"]});
        let mcp = crate::mcp::handler::story_changes_request(
            serde_json::from_value(range_args.clone()).unwrap(),
            &CallerScope::Human,
        )
        .await
        .unwrap();
        let tauri = crate::commands::data_read_model::story_changes(
            app.handle().clone(),
            serde_json::from_value(range_args.clone()).unwrap(),
            None,
            None,
        )
        .await
        .unwrap();
        assert_mcp_projects_the_bridge_read(mcp, &tauri, &range_args);
        assert_eq!(tauri["commits"][0]["ranges"], json!([]));
        assert!((entry.run)(
            Default::default(),
            crate::dashboard_publish::catalog::Call {
                args: range_args,
                folder: folder.clone(),
                ..Default::default()
            }
        )
        .await
        .is_err());
        let second = json!({"storyId":"SV-29","limit":1,"cursor":published["nextCursor"]});
        let page = (entry.run)(
            Default::default(),
            crate::dashboard_publish::catalog::Call {
                args: second.clone(),
                folder: folder.clone(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(page["commits"][0]["files"], json!(["one.rs"]));
        let page_tauri = crate::commands::data_read_model::story_changes(
            app.handle().clone(),
            serde_json::from_value(second.clone()).unwrap(),
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(page["commits"][0]["sha"], page_tauri["commits"][0]["sha"]);
        let mcp = crate::mcp::handler::story_changes_request(
            serde_json::from_value(second.clone()).unwrap(),
            &CallerScope::Human,
        )
        .await
        .unwrap();
        assert_mcp_projects_the_bridge_read(mcp, &page_tauri, &second);
    }
    drop(f.repo);
    f.dir.close().unwrap();
    let missing = crate::commands::data_read_model::story_changes(app.handle().clone(), params(), None, None).await.unwrap();
    assert!(missing["unavailable"]["lines"].is_string());
    assert!(missing["unavailable"]["fileLines"].is_string());
    let entry = crate::dashboard_publish::catalog::CATALOG.iter().find(|entry| entry.name=="storyChanges").unwrap();
    let published = (entry.run)(Default::default(), crate::dashboard_publish::catalog::Call {args:json!({"storyId":"SV-29"}),folder,..Default::default()}).await.unwrap();
    assert_published_keys(&published);
    assert!(published["unavailable"].get("fileLines").is_none());
    assert_eq!(published["unavailable"]["lines"], missing["unavailable"]["lines"]);
    assert!(published["unavailable"]["commits"].is_string());
}

#[tokio::test]
async fn live_index_directory_glob_and_note_session_are_used() {
    let (home, mut store) = store();
    let f = GitStory::new();
    f.seed(home.path(), &mut store, "feat/SV-29-work", false);
    let one = f.commit(
        "refs/heads/feat/SV-29-work",
        "source",
        "src/lib.rs",
        1100,
        &[f.root],
    );
    let two = f.commit(
        "refs/heads/feat/SV-29-work",
        "test",
        "tests/a.rs",
        1200,
        &[one],
    );
    let sig = git2::Signature::now("test", "test@example.com").unwrap();
    f.repo.note(&sig,&sig,Some("refs/notes/ai"),two,"tests/a.rs\n  s_one::t_one 1\n---\n{\"schema_version\":\"authorship/3.0.0\",\"sessions\":{\"s_one\":{\"agent_id\":{\"tool\":\"codex\",\"id\":\"git-session\"}}}}",true).unwrap();
    let story = home.path().join("story.md");
    std::fs::write(
        &story,
        "## Touchpoints\n`src/` and `tests/*.rs`\n## Decisions\n2. Keep `src/lib.rs` focused.\n",
    )
    .unwrap();
    let record = crate::install::worktree_index::WorktreeStoryRecord::new(
        f.dir.path().to_owned(),
        "acme",
        crate::domain::SourceRepoSlug::new("acme", "code").unwrap(),
        "SV-29",
        &story,
    );
    crate::install::worktree_index::write_record(
        home.path(),
        &crate::domain::Principal::from(1u64),
        &record,
    )
    .unwrap();
    let v = get(home.path()).await;
    assert!(v["worktrees"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w["state"] == "live"));
    assert!(v["actual"]
        .as_array()
        .unwrap()
        .iter()
        .all(|f| f["inPlanned"] == true));
    assert_eq!(v["commits"][0]["sessionIds"], json!(["git-session"]));
    assert_eq!(v["commits"][0]["basis"], "exact");
    assert_eq!(v["actual"][0]["decision"], 2);
}

#[tokio::test]
async fn explicit_touchpoint_base_preserves_tokens_and_allows_root_relative_paths() {
    let (home, mut store) = store();
    let f = GitStory::new();
    f.seed(home.path(), &mut store, "feat/SV-29-work", false);
    let one = f.commit(
        "refs/heads/feat/SV-29-work",
        "source",
        "src-tauri/src/session_usage/story_changes.rs",
        1100,
        &[f.root],
    );
    let two = f.commit(
        "refs/heads/feat/SV-29-work",
        "root relative",
        "src-tauri/src/commands/a.rs",
        1200,
        &[one],
    );
    let three = f.commit(
        "refs/heads/feat/SV-29-work",
        "outside",
        "src-tauri/src/elsewhere.rs",
        1300,
        &[two],
    );
    f.commit(
        "refs/heads/feat/SV-29-work",
        "glob",
        "src-tauri/src/tests/a.rs",
        1400,
        &[three],
    );
    std::fs::write(home.path().join("story.md"),"## Touchpoints\nPaths are under `src-tauri/src/`.\n`session_usage/story_changes.rs`, `src-tauri/src/commands/` and `tests/*.rs`\n## Decisions (wave 3 re-groom, 2026-10-27)\n1. **One reader:** `session_usage/story_changes.rs`.\n2. Keep `src-tauri/src/commands/a.rs` focused.\n").unwrap();
    let v = get(home.path()).await;
    assert_eq!(v["plannedBase"], "src-tauri/src/");
    assert_eq!(
        v["planned"],
        json!([
            "session_usage/story_changes.rs",
            "src-tauri/src/commands/",
            "tests/*.rs"
        ])
    );
    let actual = v["actual"].as_array().unwrap();
    assert_eq!(
        actual
            .iter()
            .find(|f| f["path"] == "src-tauri/src/elsewhere.rs")
            .unwrap()["inPlanned"],
        false
    );
    assert_eq!(actual.iter().filter(|f| f["inPlanned"] == true).count(), 3);
    assert_eq!(v["plannedUntouched"], json!([]));
    assert_eq!(
        actual
            .iter()
            .find(|f| f["path"] == "src-tauri/src/session_usage/story_changes.rs")
            .unwrap()["decision"],
        1
    );
    assert_eq!(
        actual
            .iter()
            .find(|f| f["path"] == "src-tauri/src/commands/a.rs")
            .unwrap()["decision"],
        2
    );
    std::fs::write(
        home.path().join("story.md"),
        "## Touchpoints\n`session_usage/story_changes.rs`\n",
    )
    .unwrap();
    let v = get(home.path()).await;
    assert!(v["plannedBase"].is_null());
    assert!(v["actual"]
        .as_array()
        .unwrap()
        .iter()
        .all(|f| f["inPlanned"] == false));
}

#[tokio::test]
async fn context_workspace_cannot_supply_or_hide_source_commits_and_prs() {
    let (home, mut store) = store();
    let source = GitStory::new();
    let context = GitStory::new();
    context
        .repo
        .remote_set_url("origin", "https://github.com/acme/context.git")
        .unwrap();
    source.seed(home.path(), &mut store, "feat/SV-29-work", false);
    context.commit(
        "refs/heads/feat/SV-29-context",
        "context provenance",
        "private.md",
        1100,
        &[context.root],
    );
    let tip = source.commit(
        "refs/heads/feat/SV-29-work",
        "source",
        "code.rs",
        1200,
        &[source.root],
    );
    let merge = source.commit(
        "refs/heads/main",
        "Merge pull request #17 from acme/feat/SV-29-work",
        "code.rs",
        1300,
        &[source.root, tip],
    );
    source
        .repo
        .reference("refs/remotes/origin/main", merge, true, "merge")
        .unwrap();
    source
        .repo
        .find_reference("refs/heads/feat/SV-29-work")
        .unwrap()
        .delete()
        .unwrap();
    let mut record = store.read_session_record("git-session").unwrap().unwrap();
    record.workspace = Some(context.dir.path().to_string_lossy().into());
    store.write_session_record(&record).unwrap();
    let v = get(home.path()).await;
    assert_eq!(v["totals"]["commits"], 1);
    assert_eq!(v["branches"][0]["prs"], json!([17]));
    assert_eq!(v["branches"][0]["availability"], "missing");
    assert_eq!(v["prs"][0]["number"], 17);
    assert_eq!(v["commits"][0]["files"], json!(["code.rs"]));
    assert!(!v["commits"].to_string().contains("private.md"));
}
/// SV-CDE-85: a story whose frontmatter names `work_repo` reads that repo's history, with the
/// session's workspace as its clone; a `work_repo` with no clone here says so. `via_record`:
/// the session records the branch (and the branch is deleted after its merge); otherwise only
/// the `<team>/<epic>-<n>` name finds it.
async fn work_repo_story(work_repo: &str, branch: &str, via_record: bool) -> (tempfile::TempDir, Value) {
    let (home, mut store) = store();
    let context = GitStory::new();
    context
        .repo
        .remote_set_url("origin", "https://github.com/acme/context.git")
        .unwrap();
    std::fs::write(
        home.path().join("story.md"),
        format!("---\nsource_repo: acme/code\nwork_repo: {work_repo}\n---\n"),
    )
    .unwrap();
    // The worktree is gone: only `work_repo` points at the workspace's clone.
    context.seed(home.path(), &mut store, branch, true);
    if !via_record {
        let mut record = store.read_session_record("git-session").unwrap().unwrap();
        record.branch = None;
        store.write_session_record(&record).unwrap();
    }
    let reference = format!("refs/heads/{branch}");
    let tip = context.commit(&reference, "context work", "stories/s.md", 1200, &[context.root]);
    let merge = context.commit(
        "refs/heads/main",
        &format!("Merge pull request #17 from acme/{branch}"),
        "stories/s.md",
        1300,
        &[context.root, tip],
    );
    context.repo.reference("refs/remotes/origin/main", merge, true, "merge").unwrap();
    if via_record {
        context.repo.find_reference(&reference).unwrap().delete().unwrap();
    }
    let v = get(home.path()).await;
    (home, v)
}
#[tokio::test]
async fn work_repo_reads_the_context_repos_prs_and_commits() {
    let (_home, v) = work_repo_story("acme/context", "feat/SV-29-work", true).await;
    assert_eq!(v["prs"][0]["number"], 17);
    assert_eq!(v["totals"]["commits"], 1);
    assert_eq!(v["commits"][0]["files"], json!(["stories/s.md"]));
    assert!(v["unavailable"].get("commits").is_none(), "{}", v["unavailable"]);
}
#[tokio::test]
async fn work_repo_finds_a_lower_case_team_epic_number_branch() {
    // Only the name `riverside/sv-29` ties the branch to SV-29; the session records none.
    let (_home, v) = work_repo_story("acme/context", "riverside/sv-29", false).await;
    assert_eq!(v["prs"][0]["number"], 17);
    assert_eq!(v["commits"][0]["files"], json!(["stories/s.md"]));
}
#[tokio::test]
async fn a_source_repo_story_does_not_read_branches_by_the_lower_case_convention() {
    let (home, mut store) = store();
    let f = GitStory::new();
    f.seed(home.path(), &mut store, "feat/SV-29-gone", false);
    let mut record = store.read_session_record("git-session").unwrap().unwrap();
    record.branch = None;
    store.write_session_record(&record).unwrap();
    let tip = f.commit("refs/heads/riverside/sv-29", "named", "n.rs", 1100, &[f.root]);
    let landed = f.commit("refs/heads/main", "Merge pull request #5 from acme/x", "n.rs", 1200, &[f.root, tip]);
    f.repo.reference("refs/remotes/origin/main", landed, true, "landed").unwrap();
    let v = get(home.path()).await;
    assert!(v["prs"].as_array().unwrap().is_empty(), "{}", v["prs"]);
}
#[tokio::test]
async fn work_repo_with_no_clone_here_is_unavailable_not_zero() {
    let (_home, v) = work_repo_story("acme/elsewhere", "feat/SV-29-work", true).await;
    assert_eq!(v["unavailable"]["commits"], "work repo not attached");
    assert_eq!(v["prs"], json!([]));
}
#[tokio::test]
async fn work_repo_history_is_never_partial_when_the_branch_is_gone_and_nothing_landed() {
    let (home, mut store) = store();
    let context = GitStory::new();
    context.repo.remote_set_url("origin", "https://github.com/acme/context.git").unwrap();
    std::fs::write(home.path().join("story.md"), "---\nsource_repo: acme/code\nwork_repo: acme/context\n---\n").unwrap();
    context.seed(home.path(), &mut store, "feat/SV-29-gone", true);
    let v = get(home.path()).await;
    assert!(v["unavailable"]["commits"].is_string(), "{}", v["unavailable"]);
    assert_eq!(v["prs"], json!([]));
}
#[tokio::test]
async fn work_repo_naming_an_attached_source_repo_reads_its_clone() {
    let (home, mut store) = store();
    let source = GitStory::new();
    let context = GitStory::new();
    source.repo.remote_set_url("origin", "https://git.example.test/acme/code.git").unwrap();
    source.commit("refs/heads/feat/SV-29-work", "code", "code.rs", 1100, &[source.root]);
    context.commit("refs/heads/feat/SV-29-work", "context", "private.md", 1200, &[context.root]);
    source.seed(home.path(), &mut store, "feat/SV-29-work", true);
    let org = home.path().join("users/1/orgs/acme/ctx");
    let content = org.join("child/content");
    std::fs::create_dir_all(&content).unwrap();
    std::fs::write(org.join("org.yml"), "slug: acme/ctx\ndisplay_name: Context\ncreated_at: 2026-01-01T00:00:00Z\n").unwrap();
    std::fs::write(org.join("people.yml"), "people: []\n").unwrap();
    std::fs::write(org.join("teams.yml"), "teams:\n  - name: child\n    source_repos:\n      - owner: acme\n        repo: code\n        provider: gitea\n        host: git.example.test\n").unwrap();
    let story = content.join("story.md");
    std::fs::write(&story, "---\ntype: story\nstory_id: SV-29\nsource_repo: acme/other\nwork_repo: acme/code\n---\n").unwrap();
    store
        .conn
        .execute("UPDATE session_story SET story_path=?1,org='acme/ctx' WHERE session_id='git-session'", [story.to_string_lossy().as_ref()])
        .unwrap();
    let mut record = store.read_session_record("git-session").unwrap().unwrap();
    record.workspace = Some(context.dir.path().to_string_lossy().into());
    store.write_session_record(&record).unwrap();
    let attachment = crate::commands::attachments::existing_checkout_attachment(source.dir.path()).unwrap();
    crate::context_repo::attachments_io::write_attachments_atomic(
        &crate::context_repo::path_to_user_attachments_file(home.path(), &crate::domain::Principal::from(1u64)),
        &crate::domain::Attachments { attachments: vec![attachment] },
    )
    .unwrap();
    let v = get(home.path()).await;
    assert_eq!(v["commits"][0]["files"], json!(["code.rs"]));
    assert!(v["unavailable"].get("commits").is_none(), "{}", v["unavailable"]);
    assert!(!v["commits"].to_string().contains("private.md"));
}
#[tokio::test]
async fn session_basis_is_exact_when_linked_and_associated_when_only_the_worktree_name_ties_it() {
    let (home, mut store) = store();
    seed(&mut store, home.path(), "linked", true, true);
    let org = home.path().join("users/1/orgs/acme");
    std::fs::create_dir_all(org.join("riverside/content")).unwrap();
    std::fs::write(org.join("org.yml"), "").unwrap();
    std::fs::write(
        org.join("riverside/content/SV-29.md"),
        "---\ntype: story\nstatus: todo\nstory_id: SV-29\nsource_repo: acme/code\n---\n",
    )
    .unwrap();
    let mut e = event("named-e", "named", "gpt-6-sol", 1_790_000_000_000, Tokens { input: 5, ..Default::default() });
    e.cwd = Some(home.path().join("users/1/context-worktrees/acme/riverside/sv-29").to_string_lossy().into());
    store.ingest(&[e]).unwrap();
    let v = get(home.path()).await;
    let mut bases: Vec<_> = v["sessions"].as_array().unwrap().iter().map(|s| s["basis"].as_str().unwrap().to_owned()).collect();
    bases.sort();
    assert_eq!(bases, ["associated", "exact"], "{}", v["sessions"]);
}
/// SV-CDE-85: one session tied to SV-29 only by its context worktree's name (`riverside/sv-29`);
/// the story file lives in the org and says `extra` beside its source repo.
async fn named_context_session(extra: &str) -> (tempfile::TempDir, Value) {
    let (home, mut store) = store();
    let context = GitStory::new();
    context.repo.remote_set_url("origin", "https://github.com/acme/context.git").unwrap();
    let org = home.path().join("users/1/orgs/acme");
    std::fs::create_dir_all(org.join("riverside/content")).unwrap();
    std::fs::write(org.join("org.yml"), "").unwrap();
    std::fs::write(
        org.join("riverside/content/SV-29.md"),
        format!("---\ntype: story\nstatus: todo\nstory_id: SV-29\nsource_repo: acme/code\n{extra}---\n"),
    )
    .unwrap();
    let tip = context.commit("refs/heads/riverside/sv-29", "context work", "stories/s.md", 1200, &[context.root]);
    let merge = context.commit(
        "refs/heads/main",
        "Merge pull request #17 from acme/riverside/sv-29",
        "stories/s.md",
        1300,
        &[context.root, tip],
    );
    context.repo.reference("refs/remotes/origin/main", merge, true, "merge").unwrap();
    let mut record = SessionRecord::new("named");
    record.worktree = Some(context.dir.path().to_string_lossy().into());
    record.branch = Some("riverside/sv-29".into());
    store.write_session_record(&record).unwrap();
    let mut e = event("named-e", "named", "gpt-6-sol", 1_790_000_000_000, Tokens { input: 5, ..Default::default() });
    e.cwd = Some(home.path().join("users/1/context-worktrees/acme/riverside/sv-29").to_string_lossy().into());
    store.ingest(&[e]).unwrap();
    let v = get(home.path()).await;
    (home, v)
}
#[tokio::test]
async fn an_associated_context_session_gives_associated_branches_and_prs_when_work_repo_names_it() {
    let (_home, v) = named_context_session("work_repo: acme/context\n").await;
    assert_eq!(v["sessions"][0]["basis"], "associated");
    let branch = v["branches"].as_array().unwrap().iter().find(|b| b["name"] == "riverside/sv-29").unwrap();
    assert_eq!(branch["basis"], "associated", "{branch}");
    assert_eq!(v["prs"][0]["number"], 17);
}
#[tokio::test]
async fn a_source_story_gets_no_context_history_from_a_matching_context_worktree() {
    let (_home, v) = named_context_session("").await;
    assert_eq!(v["prs"], json!([]));
    assert!(!v["commits"].to_string().contains("stories/s.md"), "{}", v["commits"]);
    assert!(v["branches"].as_array().unwrap().iter().all(|b| b["name"] != "riverside/sv-29" || b["availability"] == "missing"));
}
#[cfg(unix)]
#[tokio::test]
async fn work_repo_reads_the_principals_context_clone_when_no_session_points_at_it() {
    let (home, mut store) = store();
    let context = GitStory::new();
    context.repo.remote_set_url("origin", "https://github.com/acme/context.git").unwrap();
    // The managed org root is the context clone; no session workspace or worktree is.
    std::fs::create_dir_all(home.path().join("users/1/orgs")).unwrap();
    std::os::unix::fs::symlink(context.dir.path(), home.path().join("users/1/orgs/acme")).unwrap();
    std::fs::write(home.path().join("story.md"), "---\nsource_repo: acme/code\nwork_repo: acme/context\n---\n").unwrap();
    context.seed(home.path(), &mut store, "feat/SV-29-work", true);
    let mut record = store.read_session_record("git-session").unwrap().unwrap();
    record.workspace = None;
    store.write_session_record(&record).unwrap();
    let tip = context.commit("refs/heads/feat/SV-29-work", "context work", "stories/s.md", 1200, &[context.root]);
    let merge = context.commit(
        "refs/heads/main",
        "Merge pull request #17 from acme/feat/SV-29-work",
        "stories/s.md",
        1300,
        &[context.root, tip],
    );
    context.repo.reference("refs/remotes/origin/main", merge, true, "merge").unwrap();
    context.repo.find_reference("refs/heads/feat/SV-29-work").unwrap().delete().unwrap();
    let v = get(home.path()).await;
    assert_eq!(v["prs"][0]["number"], 17);
    assert!(v["unavailable"].get("commits").is_none(), "{}", v["unavailable"]);
}
#[tokio::test]
async fn historical_squash_does_not_claim_a_later_missing_branch() {
    let (home, mut store) = store();
    let f = GitStory::new();
    f.seed(home.path(), &mut store, "feat/SV-29-later", true);
    let squash = f.commit(
        "refs/heads/main",
        "SV-29: original (#7)",
        "old.rs",
        1100,
        &[f.root],
    );
    f.repo
        .reference("refs/remotes/origin/main", squash, true, "squash")
        .unwrap();
    let v = get(home.path()).await;
    assert_eq!(v["prs"][0]["number"], 7);
    assert_eq!(v["prs"][0]["source"], "squash-subject");
    assert!(v["prs"][0]["branch"].is_null());
    assert_eq!(v["branches"][0]["prs"], json!([]));
    assert!(v["branches"][0]["pr"]["number"].is_null());
    assert_eq!(v["branches"][0]["unavailable"], "branch not found locally");
}
/// SV-PBF-296: a story branch that never merged is reported on its own (`unavailable.branches`)
/// beside a merged PR, and does not make the story's commits unavailable (which nulls its lines);
/// with no merged PR it still does.
#[tokio::test]
async fn an_unmerged_branch_beside_a_merged_pr_does_not_mark_commits_unavailable() {
    for merged in [true, false] {
        let (home, mut store) = store();
        let f = GitStory::new();
        f.seed(home.path(), &mut store, "feat/SV-29-gone", false);
        let one = f.commit("refs/heads/feat/SV-29-one", "one", "one.rs", 1100, &[f.root]);
        if merged {
            let landed = f.commit("refs/heads/main", "Merge pull request #1 from acme/feat/SV-29-one", "one.rs", 1200, &[f.root, one]);
            f.repo.reference("refs/remotes/origin/main", landed, true, "landed").unwrap();
        }
        let v = get(home.path()).await;
        let gone = v["branches"].as_array().unwrap().iter().find(|b| b["name"] == "feat/SV-29-gone").unwrap();
        assert_eq!(gone["unavailable"], "branch not found locally");
        if merged {
            assert!(v["unavailable"].get("commits").is_none(), "{}", v["unavailable"]);
            assert!(v["unavailable"]["branches"].is_string());
        } else {
            assert!(v["unavailable"]["commits"].is_string());
            assert!(v["unavailable"].get("branches").is_none());
        }
    }
}
/// SV-PBF-296: a headed unmerged branch whose commits are missing objects is a side range:
/// it is reported as `unavailable.branches` beside a merged PR, and as `commits` with none.
#[tokio::test]
async fn an_unmerged_branch_with_missing_objects_is_a_side_range() {
    for merged in [true, false] {
        let (home, mut store) = store();
        let f = GitStory::new();
        f.seed(home.path(), &mut store, "feat/SV-29-one", false);
        let one = f.commit("refs/heads/feat/SV-29-one", "one", "one.rs", 1100, &[f.root]);
        if merged {
            let landed = f.commit("refs/heads/main", "Merge pull request #1 from acme/feat/SV-29-one", "one.rs", 1200, &[f.root, one]);
            f.repo.reference("refs/remotes/origin/main", landed, true, "landed").unwrap();
        }
        let tree = f.repo.find_commit(one).unwrap().tree_id();
        let missing = "1".repeat(40);
        let raw = format!("tree {tree}\nparent {missing}\nauthor fixture <fixture@example.com> 1300 +0000\ncommitter fixture <fixture@example.com> 1300 +0000\n\nside\n");
        let side = f.repo.odb().unwrap().write(git2::ObjectType::Commit, raw.as_bytes()).unwrap();
        f.repo.reference("refs/heads/feat/SV-29-side", side, true, "side").unwrap();
        let v = get(home.path()).await;
        let (here, there) = if merged { ("branches", "commits") } else { ("commits", "branches") };
        assert_eq!(v["unavailable"][here], "history objects not in local copy", "{}", v["unavailable"]);
        assert!(v["unavailable"].get(there).is_none(), "{}", v["unavailable"]);
    }
}
#[tokio::test]
async fn repeated_merges_and_new_live_work_keep_all_prs_and_commits() {
    let (home, mut store) = store();
    let f = GitStory::new();
    f.seed(home.path(), &mut store, "feat/SV-29-work", false);
    let one = f.commit(
        "refs/heads/feat/SV-29-work",
        "one",
        "one.rs",
        1100,
        &[f.root],
    );
    let m1 = f.commit(
        "refs/heads/main",
        "Merge pull request #1 from acme/feat/SV-29-work",
        "one.rs",
        1200,
        &[f.root, one],
    );
    let two = f.commit("refs/heads/feat/SV-29-work", "two", "two.rs", 1300, &[one]);
    let m2 = f.commit(
        "refs/heads/main",
        "Merge pull request #2 from acme/feat/SV-29-work",
        "two.rs",
        1400,
        &[m1, two],
    );
    f.repo
        .reference("refs/remotes/origin/main", m2, true, "merged")
        .unwrap();
    f.commit(
        "refs/heads/feat/SV-29-work",
        "three",
        "three.rs",
        1500,
        &[two],
    );
    let v = get(home.path()).await;
    assert_eq!(
        v["prs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["number"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![2, 1]
    );
    assert_eq!(v["branches"][0]["prs"], json!([2, 1]));
    assert_eq!(v["branches"][0]["availability"], "local");
    assert_eq!(v["total"], 3);
    assert_eq!(v["commits"][0]["subject"], "three");
    assert_eq!(v["totals"]["files"], 3);
}

/// A merge of main into the story's branch adds no files to the story (SV-PBF-295).
#[tokio::test]
async fn a_merge_of_main_mid_story_adds_no_files() {
    let (home, mut store) = store();
    let f = GitStory::new();
    f.seed(home.path(), &mut store, "feat/SV-29-work", false);
    let one = f.commit("refs/heads/feat/SV-29-work", "one", "one.rs", 1100, &[f.root]);
    let on_main = f.commit("refs/heads/main", "main work", "main.rs", 1200, &[f.root]);
    f.repo
        .reference("refs/remotes/origin/main", on_main, true, "main moved")
        .unwrap();
    // The fixture writes the subject as the file's content: the same subject as main's commit
    // makes the merge's main.rs equal main's, as a real merge of main is.
    let merge = f.commit("refs/heads/feat/SV-29-work", "main work", "main.rs", 1300, &[one, on_main]);
    f.commit("refs/heads/feat/SV-29-work", "two", "two.rs", 1400, &[merge]);
    let v = get(home.path()).await;
    assert_eq!(v["totals"]["files"], 2, "one.rs and two.rs only: {v}");
}

#[tokio::test]
async fn promised_note_blobs_stay_offline_and_keep_durable_story_facts() {
    use crate::attribution::git_ai::tests::git;
    use crate::attribution::why_line::tests::{blobless, object_count};
    let (home, mut store) = store();
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
    let local = git2::Repository::open(&repo).unwrap();
    let tip = local.head().unwrap().peel_to_commit().unwrap();
    let root = tip.parent(0).unwrap().parent(0).unwrap().id();
    local
        .reference("refs/heads/feat/SV-29-work", tip.id(), true, "fixture")
        .unwrap();
    local
        .reference("refs/remotes/origin/main", root, true, "fixture")
        .unwrap();
    seed(&mut store, home.path(), "git-session", true, false);
    let mut record = store.read_session_record("git-session").unwrap().unwrap();
    record.worktree = Some(repo.to_string_lossy().into());
    record.branch = Some("feat/SV-29-work".into());
    store.write_session_record(&record).unwrap();
    let mut task = TaskRecord::new("dev", "develop");
    task.story_id = Some("SV-29".into());
    task.created_at_ms = 0;
    task.closed_at_ms = Some(i64::MAX);
    store.insert_task_if_absent(&task).unwrap();
    store
        .add_task_link("dev", "session", "git-session")
        .unwrap();
    let before = object_count(&repo);
    let value = get(home.path()).await;
    assert_eq!(
        object_count(&repo),
        before,
        "story reads must not fetch promised note blobs"
    );
    assert_eq!(value["totals"]["sessions"], 1);
    assert_eq!(value["totals"]["costUsd"], 2.0);
    assert_eq!(value["tasks"].as_array().unwrap().len(), 1);
    assert!(value["total"].as_u64().unwrap() > 0);
    assert!(value["commits"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["basis"] == "unknown" && c["unavailable"] == "objects not in local copy"));
    let task = serde_json::to_value(
        crate::mcp::task_tools::read(
            &store,
            "git-session",
            serde_json::from_value(json!({"sessionId":"git-session","taskId":"dev"})).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(!task["outcome"]["plannedVsActual"]["actual"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(task["outcome"]["plannedVsActual"]["basis"], "unknown");
    assert_eq!(
        task["outcome"]["plannedVsActual"]["unavailable"]["attribution"],
        "objects not in local copy"
    );
    assert_eq!(
        value["unavailable"]["attribution"],
        "git-ai note attribution unavailable locally"
    );
    let params: Params =
        serde_json::from_value(json!({"storyId":"SV-29","include":["ranges"]})).unwrap();
    let ranged = serde_json::to_value(
        read(home.path(), &params, &CallerScope::Human)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        object_count(&repo),
        before,
        "range reads must not fetch promised note blobs"
    );
    assert_eq!(ranged["totals"]["costUsd"], 2.0);
    assert!(ranged["commits"]
        .as_array()
        .unwrap()
        .iter()
        .any(|commit| commit["ranges"] == json!([])
            && commit["rangesBasis"] == "unknown"
            && commit["rangesReason"] == "objects not in local copy"));
}
#[tokio::test]
async fn empty_branch_history_skips_managed_drain_and_checkpoint_work_for_story_and_tasks() {
    let (home, mut store) = store();
    let f = GitStory::new();
    f.repo
        .reference("refs/heads/feat/SV-29-work", f.root, true, "fresh branch")
        .unwrap();
    f.seed(home.path(), &mut store, "feat/SV-29-work", false);
    let mut task = TaskRecord::new("empty-history-task", "develop");
    task.story_id = Some("SV-29".into());
    store.insert_task_if_absent(&task).unwrap();
    store
        .add_task_link(&task.task_id, "session", "git-session")
        .unwrap();
    let log = home.path().join("await.log");
    crate::attribution::testing::script(
        &home.path().join("bin/git-ai"),
        &format!(r#"printf '%s\n' "$*" >> '{}'"#, log.display()),
    );
    let probe = crate::attribution::checkpoints::tests::Probe::new(f.dir.path());
    assert_eq!(get(home.path()).await["total"], 0);
    let view = crate::mcp::task_tools::read(
        &store,
        "git-session",
        serde_json::from_value(json!({"sessionId":"git-session","taskId":task.task_id})).unwrap(),
    )
    .unwrap();
    assert_eq!(view.task.task_id, "empty-history-task");
    let rows = crate::mcp::task_tools::list(
        &store,
        "git-session",
        serde_json::from_value(json!({"sessionId":"git-session","story":"SV-29"})).unwrap(),
    )
    .unwrap();
    assert!(rows.iter().any(|task| task.task_id == "empty-history-task"));
    let drains = crate::attribution::testing::logged_calls(&log);
    let checkpoints = probe.take();
    assert!(
        drains.is_empty() && checkpoints.is_empty(),
        "drains={drains:?}, checkpoint git calls={checkpoints:?}"
    );
}

#[tokio::test]
async fn story_notes_use_the_managed_offline_drain_once_per_repository() {
    let (home, mut store) = store();
    let f = GitStory::new();
    f.seed(home.path(), &mut store, "feat/SV-29-work", false);
    let one = f.commit(
        "refs/heads/feat/SV-29-work",
        "one",
        "one.rs",
        1100,
        &[f.root],
    );
    f.commit("refs/heads/feat/SV-29-work", "two", "two.rs", 1200, &[one]);
    let log = home.path().join("await.log");
    crate::attribution::testing::script(
        &home.path().join("bin/git-ai"),
        &format!(
            r#"printf '%s|%s|%s\n' "$*" "$GIT_NO_LAZY_FETCH" "$GIT_TERMINAL_PROMPT" >> '{}'"#,
            log.display()
        ),
    );
    assert_eq!(get(home.path()).await["total"], 2);
    assert_eq!(
        std::fs::read_to_string(log).unwrap().trim(),
        "await --timeout 5|1|0"
    );
}

#[tokio::test]
async fn removed_worktree_uses_the_configured_source_host_and_ignores_a_same_slug_workspace() {
    let (home, mut store) = store();
    let source = GitStory::new();
    let context = GitStory::new();
    source
        .repo
        .remote_set_url("origin", "https://git.example.test/acme/code.git")
        .unwrap();
    source.commit(
        "refs/heads/feat/SV-29-work",
        "code",
        "code.rs",
        1100,
        &[source.root],
    );
    context.commit(
        "refs/heads/feat/SV-29-work",
        "context",
        "private.md",
        1200,
        &[context.root],
    );
    source.seed(home.path(), &mut store, "feat/SV-29-work", true);
    let org = home.path().join("users/1/orgs/acme/ctx");
    let content = org.join("child/content");
    std::fs::create_dir_all(&content).unwrap();
    std::fs::write(
        org.join("org.yml"),
        "slug: acme/ctx\ndisplay_name: Context\ncreated_at: 2026-01-01T00:00:00Z\n",
    )
    .unwrap();
    std::fs::write(org.join("people.yml"), "people: []\n").unwrap();
    std::fs::write(org.join("teams.yml"),"teams:\n  - name: child\n    source_repos:\n      - owner: acme\n        repo: code\n        provider: gitea\n        host: git.example.test\n").unwrap();
    let story = content.join("story.md");
    std::fs::write(&story,"---\ntype: story\nstory_id: SV-29\nsource_repo: acme/code\n---\n## Touchpoints\n`code.rs`\n").unwrap();
    store
        .conn
        .execute(
            "UPDATE session_story SET story_path=?1,org='acme/ctx' WHERE session_id='git-session'",
            [story.to_string_lossy().as_ref()],
        )
        .unwrap();
    let mut record = store.read_session_record("git-session").unwrap().unwrap();
    record.workspace = Some(context.dir.path().to_string_lossy().into());
    store.write_session_record(&record).unwrap();
    let attachment =
        crate::commands::attachments::existing_checkout_attachment(source.dir.path()).unwrap();
    crate::context_repo::attachments_io::write_attachments_atomic(
        &crate::context_repo::path_to_user_attachments_file(
            home.path(),
            &crate::domain::Principal::from(1u64),
        ),
        &crate::domain::Attachments {
            attachments: vec![attachment],
        },
    )
    .unwrap();
    let configured = crate::install::worktree_index::team_source_repos_for_story(
        home.path(),
        &crate::domain::Principal::from(1u64),
        "acme/ctx",
        &story,
    );
    assert_eq!(configured.len(), 1);
    assert_eq!(configured[0].remote_key(), "git.example.test/acme/code");
    let linked: String = store
        .conn
        .query_row(
            "SELECT story_path FROM session_story WHERE session_id='git-session'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(Path::new(&linked), story);
    let v = get(home.path()).await;
    assert_eq!(v["total"], 1);
    assert_eq!(v["commits"][0]["files"], json!(["code.rs"]));
    assert!(!v["commits"].to_string().contains("private.md"));
    assert!(v["worktrees"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w["state"] == "removed" && w["basis"] == "exact"));
}

#[tokio::test]
async fn convention_and_live_index_branch_links_give_prs_their_own_basis() {
    for indexed in [false, true] {
        let (home, mut store) = store();
        let f = GitStory::new();
        f.seed(home.path(), &mut store, "feat/SV-29-work", false);
        let mut record = store.read_session_record("git-session").unwrap().unwrap();
        record.branch = None;
        store.write_session_record(&record).unwrap();
        let tip = f.commit(
            "refs/heads/feat/SV-29-work",
            "work",
            "code.rs",
            1100,
            &[f.root],
        );
        let merged = f.commit(
            "refs/heads/main",
            "Merge pull request #4 from acme/feat/SV-29-work",
            "code.rs",
            1200,
            &[f.root, tip],
        );
        f.repo
            .reference("refs/remotes/origin/main", merged, true, "merge fixture")
            .unwrap();
        f.repo.set_head("refs/heads/feat/SV-29-work").unwrap();
        if indexed {
            let row = crate::install::worktree_index::WorktreeStoryRecord::new(
                f.dir.path().to_owned(),
                "acme",
                crate::domain::SourceRepoSlug::new("acme", "code").unwrap(),
                "SV-29",
                home.path().join("story.md"),
            );
            crate::install::worktree_index::write_record(
                home.path(),
                &crate::domain::Principal::from(1u64),
                &row,
            )
            .unwrap();
        }
        let v = get(home.path()).await;
        let basis = if indexed { "exact" } else { "associated" };
        assert_eq!(v["branches"][0]["basis"], basis);
        assert_eq!(v["prs"][0]["basis"], basis);
        if indexed {
            assert!(v["worktrees"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w["state"] == "live" && w["basis"] == "exact"));
        }
    }
}

#[tokio::test]
async fn unplaced_tasks_read_private_plans_without_assuming_a_team() {
    for private in [true, false] {
        let fixture = crate::context_repo::caller_scope::tests::Fixture::new();
        let home = &fixture.home;
        let mut store = UsageStore::open(&home.join("usage.sqlite")).unwrap();
        let f = GitStory::new();
        f.seed(home, &mut store, "feat/SV-29-work", false);
        f.commit(
            "refs/heads/feat/SV-29-work",
            "work",
            "observed.rs",
            1100,
            &[f.root],
        );
        let source = if private {
            let (space, _) = crate::context_repo::scratch_space::ensure_org_space(
                home,
                &crate::domain::Principal::from(1u64),
                "acme/ctx",
            )
            .unwrap();
            let source = space.content_dir().join("story.md");
            std::fs::create_dir_all(source.parent().unwrap()).unwrap();
            std::fs::write(&source, "## Touchpoints\n`private-plan.rs`\n").unwrap();
            source
        } else {
            fixture.write(
                "parent",
                "content/secret-story.md",
                "## Touchpoints\n`hidden-team-plan.rs`\n",
            )
        };
        store.conn.execute("UPDATE session_story SET story_path=?1,org='acme/ctx' WHERE session_id='git-session'",[source.to_string_lossy().as_ref()]).unwrap();
        let mut task = TaskRecord::new("git-session", "develop");
        task.story_id = Some("SV-29".into());
        task.created_at_ms = 1_000_000;
        store.insert_task_if_absent(&task).unwrap();
        store
            .add_task_link("git-session", "session", "git-session")
            .unwrap();
        let value = serde_json::to_value(
            crate::mcp::task_tools::read(
                &store,
                "git-session",
                serde_json::from_value(json!({"sessionId":"git-session","taskId":"git-session"}))
                    .unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(value["task"]["taskId"], "git-session");
        assert_eq!(value["outcome"]["cost"]["costUsd"], 2.0);
        assert!(value["links"]
            .as_array()
            .unwrap()
            .iter()
            .any(|link| link["kind"] == "session" && link["target"] == "git-session"));
        if private {
            assert_eq!(
                value["outcome"]["plannedVsActual"]["planned"],
                json!(["private-plan.rs"])
            );
        } else {
            assert!(value["outcome"]["plannedVsActual"]["planned"].is_null());
            assert!(!value.to_string().contains("hidden-team-plan.rs"));
        }
    }
}

#[tokio::test]
async fn literal_touchpoints_keep_spaces_parentheses_brace_globs_and_leading_dot() {
    let (home, mut store) = store();
    let f = GitStory::new();
    f.seed(home.path(), &mut store, "feat/SV-29-work", false);
    let mut tip = f.root;
    for (index, path) in [
        "src/My Widget (draft).rs",
        "tests/a.rs",
        "tests/b.rs",
        "src/ranged.rs",
        "src/api/handler.rs",
        "src/cli/main.rs",
        "src/apian/no.rs",
    ]
    .iter()
    .enumerate()
    {
        tip = f.commit(
            "refs/heads/feat/SV-29-work",
            "literal paths",
            path,
            1100 + index as i64,
            &[tip],
        );
    }
    std::fs::write(home.path().join("story.md"),"## Touchpoints\n`./src/My Widget (draft).rs:123 annotation`\n`tests/{a,b}.rs:2-4`\n`src/ranged.rs:3/5`\n`./not-created.rs:1,3-5`\n`session_read::current`\n`src/{api,cli}`\n## Decisions (wave 3 re-groom, 2026-10-27)\n1. Change `./src/My Widget (draft).rs`.\n2. Keep `src/api/` focused.\n").unwrap();
    let v = get(home.path()).await;
    assert_eq!(
        v["planned"],
        json!([
            "./not-created.rs",
            "./src/My Widget (draft).rs",
            "src/ranged.rs",
            "src/{api,cli}",
            "tests/{a,b}.rs"
        ])
    );
    assert_eq!(v["plannedUntouched"], json!(["./not-created.rs"]));
    assert!(v["actual"]
        .as_array()
        .unwrap()
        .iter()
        .all(|file| (file["inPlanned"] == true) == (file["path"] != "src/apian/no.rs")));
    assert_eq!(
        v["actual"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["path"] == "src/api/handler.rs")
            .unwrap()["decision"],
        2
    );
    assert_eq!(
        v["actual"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["path"] == "src/apian/no.rs")
            .unwrap()["decision"],
        Value::Null
    );
    assert_eq!(
        v["actual"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == "src/My Widget (draft).rs")
            .unwrap()["decision"],
        1
    );
}

fn add_repo_session(home: &Path, store: &mut UsageStore, id: &str, f: &GitStory) {
    seed(store, home, id, true, false);
    let mut record = store.read_session_record(id).unwrap().unwrap();
    record.branch = Some("feat/SV-29-work".into());
    record.worktree = Some(f.dir.path().to_string_lossy().into());
    store.write_session_record(&record).unwrap();
}

#[tokio::test]
async fn clone_observations_union_prs_and_keep_best_live_availability() {
    let (home, mut store) = store();
    let a = GitStory::new();
    let b = GitStory::new();
    let (first, second) = if a.dir.path() < b.dir.path() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    for (f, number, at) in [(first, 1, 1200), (second, 2, 1400)] {
        let tip = f.commit(
            "refs/heads/feat/SV-29-work",
            "work",
            &format!("{number}.rs"),
            at - 10,
            &[f.root],
        );
        let merged = f.commit(
            "refs/heads/main",
            &format!("Merge pull request #{number} from acme/feat/SV-29-work"),
            &format!("{number}.rs"),
            at,
            &[f.root, tip],
        );
        f.repo
            .reference("refs/remotes/origin/main", merged, true, "fixture")
            .unwrap();
    }
    first
        .repo
        .find_reference("refs/heads/feat/SV-29-work")
        .unwrap()
        .delete()
        .unwrap();
    add_repo_session(home.path(), &mut store, "first", first);
    add_repo_session(home.path(), &mut store, "second", second);
    let v = get(home.path()).await;
    assert_eq!(v["branches"][0]["availability"], "local");
    assert_eq!(v["branches"][0]["prs"], json!([2, 1]));
    assert_eq!(v["branches"][0]["pr"]["number"], 2);
    assert_eq!(
        v["prs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["number"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![2, 1]
    );
}

#[tokio::test]
async fn task_selection_uses_matching_notes_or_interval_and_list_matches_read() {
    let (home, mut store) = store();
    let a = GitStory::new();
    let b = GitStory::new();
    let (first, second) = if a.dir.path() < b.dir.path() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    for f in [first, second] {
        let mut tip = f.root;
        for (file, at) in [
            ("interval.rs", 1100),
            ("outside.rs", 1500),
            ("matching.rs", 1600),
            ("other.rs", 1150),
        ] {
            tip = f.commit("refs/heads/feat/SV-29-work", file, file, at, &[tip]);
            if f.dir.path() == second.dir.path() && ["matching.rs", "other.rs"].contains(&file) {
                let id = if file == "matching.rs" {
                    "git-session"
                } else {
                    "other-session"
                };
                let sig = git2::Signature::now("test", "test@example.com").unwrap();
                let note = format!(
                    "{file}\n  s_review::t_review 1\n---\n{}",
                    json!({"schema_version":"authorship/3.0.0","sessions":{"s_review":{"agent_id":{"tool":"codex","id":id}}}})
                );
                assert!(crate::attribution::authorship::Authorship::parse(&note).is_ok());
                f.repo
                    .note(&sig, &sig, Some("refs/notes/ai"), tip, &note, true)
                    .unwrap();
            }
        }
    }
    first.seed(home.path(), &mut store, "feat/SV-29-work", false);
    add_repo_session(home.path(), &mut store, "second", second);
    let mut task = TaskRecord::new("selection", "develop");
    task.story_id = Some("SV-29".into());
    task.created_at_ms = 1_050_000;
    task.closed_at_ms = Some(1_200_000);
    store.insert_task_if_absent(&task).unwrap();
    store
        .add_task_link("selection", "session", "git-session")
        .unwrap();
    let v = get(home.path()).await;
    assert_eq!(
        v["commits"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["subject"] == "matching.rs")
            .unwrap()["basis"],
        "exact"
    );
    let read = serde_json::to_value(
        crate::mcp::task_tools::read(
            &store,
            "git-session",
            serde_json::from_value(json!({"sessionId":"git-session","taskId":"selection"}))
                .unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let planned = &read["outcome"]["plannedVsActual"];
    assert_eq!(
        planned["actual"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["interval.rs", "matching.rs"]
    );
    assert_eq!(planned["basis"], "associated");
    let list = serde_json::to_value(
        crate::mcp::task_tools::list(
            &store,
            "git-session",
            serde_json::from_value(json!({"sessionId":"git-session"})).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(list[0]["outcome"], read["outcome"]);
}

#[tokio::test]
async fn shallow_history_keeps_durable_facts_and_explicit_commit_unavailability() {
    use crate::attribution::git_ai::tests::git;
    let (home, mut store) = store();
    let source = GitStory::new();
    let one = source.commit(
        "refs/heads/feat/SV-29-work",
        "one",
        "one.rs",
        1100,
        &[source.root],
    );
    source.commit(
        "refs/heads/feat/SV-29-work",
        "one\nnew",
        "one.rs",
        1200,
        &[one],
    );
    let path = home.path().join("shallow");
    git(
        home.path(),
        &[
            "clone",
            "--depth=1",
            "--branch=feat/SV-29-work",
            &format!("file://{}", source.dir.path().display()),
            path.to_str().unwrap(),
        ],
    );
    let repo = git2::Repository::open(&path).unwrap();
    git(
        &path,
        &[
            "fetch",
            "--depth=1",
            "origin",
            "main:refs/remotes/origin/main",
        ],
    );
    assert!(repo.is_shallow());
    assert!(repo.find_commit(one).is_err());
    // Bind the shallow clone after fetching; re-add the comparison ref removed with origin.
    git(&path, &["remote", "remove", "origin"]);
    crate::attribution::why_line::tests::attach(home.path(), &path);
    repo.reference(
        "refs/remotes/origin/main",
        source.root,
        true,
        "fixture base",
    )
    .unwrap();
    let boundary = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(boundary.parent_count(), 0);
    assert_eq!(
        std::fs::read_to_string(path.join("one.rs")).unwrap(),
        "one\nnew\n"
    );
    let note = "one.rs\n  h_fixture 1-2\n---\n{\"schema_version\":\"authorship/3.0.0\"}";
    assert!(crate::attribution::authorship::Authorship::parse(note).is_ok());
    git(
        &path,
        &[
            "notes",
            "--ref=ai",
            "add",
            "-m",
            note,
            &boundary.id().to_string(),
        ],
    );
    seed(&mut store, home.path(), "git-session", true, false);
    let mut record = store.read_session_record("git-session").unwrap().unwrap();
    record.branch = Some("feat/SV-29-work".into());
    record.worktree = Some(path.to_string_lossy().into());
    store.write_session_record(&record).unwrap();
    let mut task = TaskRecord::new("durable", "develop");
    task.story_id = Some("SV-29".into());
    store.insert_task_if_absent(&task).unwrap();
    let v = get(home.path()).await;
    assert_eq!(v["totals"]["sessions"], 1);
    assert_eq!(v["totals"]["costUsd"], 2.0);
    assert_eq!(v["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(v["unavailable"]["commits"], crate::attribution::ranges::HISTORY_PAST_SHALLOW);
    assert_eq!(v["commits"].as_array().unwrap().len(), 1);
    assert!(v["commits"][0]["files"]
        .as_array()
        .unwrap()
        .contains(&json!("one.rs")));
    assert!(v["commits"][0].get("ranges").is_none());
    let params: Params =
        serde_json::from_value(json!({"storyId":"SV-29","include":["ranges"]})).unwrap();
    let included = serde_json::to_value(
        read(home.path(), &params, &CallerScope::Human)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        included["commits"][0]["ranges"],
        json!([]),
        "a shallow boundary must not claim old unchanged lines"
    );
    assert_eq!(included["commits"][0]["rangesBasis"], "unknown");
    assert_eq!(
        included["commits"][0]["rangesReason"],
        crate::attribution::ranges::HISTORY_PAST_SHALLOW
    );
    let mut durable = included.clone();
    for commit in durable["commits"].as_array_mut().unwrap() {
        for key in ["ranges", "rangesBasis", "rangesReason"] {
            commit.as_object_mut().unwrap().remove(key);
        }
    }
    assert_eq!(durable, v);
    assert_eq!(get(home.path()).await, v);
    // A real root remains distinguishable inside the same shallow repository.
    let root = repo.find_commit(source.root).unwrap();
    assert_eq!(root.parent_count(), 0);
    assert!(crate::commands::git_sync::commit_added_windows_local(&repo, &root, false).is_ok());

    // A shallow merge also hides parents; its durable raw metadata still identifies a merge.
    let merge = source.commit(
        "refs/heads/feat/SV-29-merge",
        "merge",
        "one.rs",
        1300,
        &[source.root, boundary.id()],
    );
    assert_eq!(source.repo.find_commit(merge).unwrap().parent_count(), 2);
    let merge_path = home.path().join("shallow-merge");
    git(
        home.path(),
        &[
            "clone",
            "--depth=1",
            "--branch=feat/SV-29-merge",
            &format!("file://{}", source.dir.path().display()),
            merge_path.to_str().unwrap(),
        ],
    );
    git(
        &merge_path,
        &[
            "fetch",
            "--depth=1",
            "origin",
            "main:refs/remotes/origin/main",
        ],
    );
    let merge_repo = git2::Repository::open(&merge_path).unwrap();
    assert!(merge_repo.is_shallow());
    assert_eq!(merge_repo.find_commit(merge).unwrap().parent_count(), 0);
    record.branch = Some("feat/SV-29-merge".into());
    record.worktree = Some(merge_path.to_string_lossy().into_owned());
    store.write_session_record(&record).unwrap();
    let merge_default = get(home.path()).await;
    let merge_included = serde_json::to_value(
        read(home.path(), &params, &CallerScope::Human)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(merge_included["commits"][0]["sha"], merge.to_string());
    assert_eq!(merge_included["commits"][0]["ranges"], json!([]));
    assert_eq!(merge_included["commits"][0]["rangesReason"], "merge commit");
    assert_eq!(get(home.path()).await, merge_default);
}

#[tokio::test]
async fn exact_note_sessions_are_unioned_across_clones_for_both_tasks() {
    let (home, mut store) = store();
    let a = GitStory::new();
    let b = GitStory::new();
    let mut sha = None;
    for (f, session) in [(&a, "first-session"), (&b, "second-session")] {
        let tip = f.commit(
            "refs/heads/feat/SV-29-work",
            "shared",
            "shared.rs",
            1100,
            &[f.root],
        );
        assert_eq!(*sha.get_or_insert(tip), tip);
        let note = format!(
            "shared.rs\n  s_shared::t_shared 1\n---\n{}",
            json!({"schema_version":"authorship/3.0.0","sessions":{"s_shared":{"agent_id":{"tool":"codex","id":session}}}})
        );
        assert!(crate::attribution::authorship::Authorship::parse(&note).is_ok());
        let sig = git2::Signature::now("test", "test@example.com").unwrap();
        f.repo
            .note(&sig, &sig, Some("refs/notes/ai"), tip, &note, true)
            .unwrap();
        add_repo_session(home.path(), &mut store, session, f);
        let mut task = TaskRecord::new(session, "develop");
        task.story_id = Some("SV-29".into());
        task.created_at_ms = 2_000_000;
        task.closed_at_ms = Some(2_100_000);
        store.insert_task_if_absent(&task).unwrap();
        store.add_task_link(session, "session", session).unwrap();
    }
    let story = get(home.path()).await;
    assert_eq!(
        story["commits"][0]["sessionIds"],
        json!(["first-session", "second-session"])
    );
    assert_eq!(story["commits"][0]["basis"], "exact");
    for session in ["first-session", "second-session"] {
        let task = serde_json::to_value(
            crate::mcp::task_tools::read(
                &store,
                session,
                serde_json::from_value(json!({"sessionId":session,"taskId":session})).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            task["outcome"]["plannedVsActual"]["actual"]
                .as_array()
                .unwrap()
                .iter()
                .map(|file| file["path"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["shared.rs"]
        );
        assert_eq!(task["outcome"]["plannedVsActual"]["basis"], "exact");
    }
}

fn request_outcome_fixture() -> (tempfile::TempDir, UsageStore, GitStory, std::path::PathBuf) {
    let (home, mut store) = store();
    let f = GitStory::new();
    f.seed(home.path(), &mut store, "feat/SV-29-work", false);
    let a = f.commit("refs/heads/feat/SV-29-work", "a", "a.rs", 1100, &[f.root]);
    let b = f.commit("refs/heads/feat/SV-29-work", "b", "b.rs", 1200, &[a]);
    let c = f.commit("refs/heads/feat/SV-29-work", "c", "c.rs", 1300, &[b]);
    let note = format!(
        "c.rs\n  s_batch::t_batch 1\n---\n{}",
        json!({"schema_version":"authorship/3.0.0","sessions":{"s_batch":{"agent_id":{"tool":"codex","id":"git-session"}}}})
    );
    let sig = git2::Signature::now("test", "test@example.com").unwrap();
    f.repo
        .note(&sig, &sig, Some("refs/notes/ai"), c, &note, true)
        .unwrap();
    std::fs::write(
        home.path().join("story.md"),
        "## Touchpoints\n`a.rs`, `b.rs`, `c.rs`, `d.rs`\n",
    )
    .unwrap();
    for (id, from, to) in [
        ("batch-root", 1050, 1150),
        ("batch-one", 1150, 1250),
        ("batch-two", 2000, 2100),
    ] {
        let mut task = TaskRecord::new(id, "develop");
        task.story_id = Some("SV-29".into());
        task.created_at_ms = from * 1000;
        task.closed_at_ms = Some(to * 1000);
        task.worktree = Some(f.dir.path().to_string_lossy().into());
        if id != "batch-root" {
            task.parent_id = Some("batch-root".into());
        }
        store.insert_task_if_absent(&task).unwrap();
        store.add_task_link(id, "session", "git-session").unwrap();
    }
    let log = home.path().join("drains.log");
    crate::attribution::testing::logging_binary(&home.path().join("bin/git-ai"), &log, "");
    (home, store, f, log)
}
fn assert_batch_outcome(task: &str, outcome: &Value, fresh: bool) {
    let expected = match task {
        "batch-root" => vec!["a.rs", "c.rs"],
        "batch-one" => vec!["b.rs", "c.rs"],
        "batch-two" if fresh => vec!["c.rs", "d.rs"],
        "batch-two" => vec!["c.rs"],
        _ => panic!("unexpected task"),
    };
    assert_eq!(
        outcome["plannedVsActual"]["actual"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(
        outcome["plannedVsActual"]["basis"],
        if task == "batch-two" && !fresh {
            "exact"
        } else {
            "associated"
        }
    );
    assert_eq!(
        outcome["plannedVsActual"]["planned"],
        json!(["a.rs", "b.rs", "c.rs", "d.rs"])
    );
}
fn append_fresh_request_commit(f: &GitStory) {
    let tip = f.repo.refname_to_id("refs/heads/feat/SV-29-work").unwrap();
    f.commit(
        "refs/heads/feat/SV-29-work",
        "fresh request",
        "d.rs",
        2050,
        &[tip],
    );
}
#[tokio::test]
async fn task_list_builds_story_evidence_once_and_rebuilds_next_request() {
    let (_home, store, f, log) = request_outcome_fixture();
    for fresh in [false, true] {
        if fresh {
            append_fresh_request_commit(&f);
        }
        let rows = crate::mcp::task_tools::list(
            &store,
            "git-session",
            serde_json::from_value(json!({"sessionId":"git-session","story":"SV-29"})).unwrap(),
        )
        .unwrap();
        assert_eq!(rows.len(), 3);
        for row in rows {
            let value = serde_json::to_value(row).unwrap();
            assert_batch_outcome(value["taskId"].as_str().unwrap(), &value["outcome"], fresh);
        }
        assert_eq!(
            crate::attribution::testing::logged_calls(&log).len(),
            if fresh { 2 } else { 1 },
            "one managed drain/notes batch for the story repository per request"
        );
    }
}
#[tokio::test]
async fn task_read_subtree_builds_story_evidence_once_and_rebuilds_next_request() {
    let (_home, store, f, log) = request_outcome_fixture();
    for fresh in [false, true] {
        if fresh {
            append_fresh_request_commit(&f);
        }
        let view = crate::mcp::task_tools::read(
            &store,
            "git-session",
            serde_json::from_value(json!({"sessionId":"git-session","taskId":"batch-root"}))
                .unwrap(),
        )
        .unwrap();
        let value = serde_json::to_value(view).unwrap();
        assert_batch_outcome("batch-root", &value["outcome"], fresh);
        assert_eq!(value["children"].as_array().unwrap().len(), 2);
        for child in value["children"].as_array().unwrap() {
            assert_batch_outcome(
                child["task"]["taskId"].as_str().unwrap(),
                &child["outcome"],
                fresh,
            );
        }
        assert_eq!(
            crate::attribution::testing::logged_calls(&log).len(),
            if fresh { 2 } else { 1 },
            "one managed drain/notes batch for the subtree repository per request"
        );
    }
}
#[tokio::test]
async fn task_subtree_gathers_later_candidate_worktrees_before_collecting() {
    for indexed in [false, true] {
        let (home, mut store) = store();
        let a = GitStory::new();
        let b = GitStory::new();
        a.seed(home.path(), &mut store, "feat/SV-29-work", false);
        a.commit("refs/heads/feat/SV-29-work", "a", "a.rs", 1100, &[a.root]);
        b.commit("refs/heads/feat/SV-29-work", "b", "b.rs", 1200, &[b.root]);
        if indexed {
            let principal = crate::domain::Principal::from(1u64);
            let record = crate::install::worktree_index::WorktreeStoryRecord::new(
                b.dir.path().to_owned(),
                "acme",
                crate::domain::SourceRepoSlug::new("acme", "code").unwrap(),
                "SV-29",
                home.path().join("story.md"),
            );
            crate::install::worktree_index::write_record(home.path(), &principal, &record).unwrap();
            assert_eq!(
                crate::install::worktree_index::find_by_story_id(home.path(), &principal, "SV-29")
                    .unwrap()
                    .len(),
                1
            );
        }
        for (id, f, from, to) in [("root", &a, 1050, 1150), ("child", &b, 1150, 1250)] {
            let mut task = TaskRecord::new(id, "develop");
            task.story_id = Some("SV-29".into());
            task.worktree = if indexed && id == "child" {
                None
            } else {
                Some(f.dir.path().to_string_lossy().into())
            };
            task.created_at_ms = from * 1000;
            task.closed_at_ms = Some(to * 1000);
            if id == "child" {
                task.parent_id = Some("root".into());
            }
            store.insert_task_if_absent(&task).unwrap();
            store.add_task_link(id, "session", "git-session").unwrap();
        }
        let log = home.path().join("drains.log");
        crate::attribution::testing::logging_binary(&home.path().join("bin/git-ai"), &log, "");
        let view = crate::mcp::task_tools::read(
            &store,
            "git-session",
            serde_json::from_value(json!({"sessionId":"git-session","taskId":"root"})).unwrap(),
        )
        .unwrap();
        let value = serde_json::to_value(view).unwrap();
        assert_eq!(
            value["outcome"]["plannedVsActual"]["actual"][0]["path"],
            "a.rs"
        );
        assert_eq!(
            value["children"][0]["outcome"]["plannedVsActual"]["actual"][0]["path"],
            "b.rs"
        );
        assert_eq!(
            crate::attribution::testing::logged_calls(&log).len(),
            2,
            "each candidate repository is collected once"
        );
    }
}

#[tokio::test]
async fn task_request_keeps_each_original_scope_for_planned_enrichment() {
    for root_visible in [false, true] {
        let fixture = crate::context_repo::caller_scope::tests::Fixture::new();
        let home = &fixture.home;
        let mut store = UsageStore::open(&home.join("usage.sqlite")).unwrap();
        let f = GitStory::new();
        f.seed(home, &mut store, "feat/SV-29-work", false);
        f.commit(
            "refs/heads/feat/SV-29-work",
            "work",
            "observed.rs",
            1100,
            &[f.root],
        );
        let source = fixture.write(
            "parent",
            "content/secret-story.md",
            "## Touchpoints\n`parent-plan.rs`\n",
        );
        store.conn.execute("UPDATE session_story SET story_path=?1,org='acme/ctx' WHERE session_id='git-session'",[source.to_string_lossy().as_ref()]).unwrap();
        for (id, visible) in [("scope-root", root_visible), ("scope-child", !root_visible)] {
            let mut task = TaskRecord::new(id, "develop");
            task.story_id = Some("SV-29".into());
            task.org = Some("acme/ctx".into());
            task.team = Some(if visible { "parent" } else { "child" }.into());
            let scope = CallerScope::team(
                home,
                &crate::domain::Principal::from(1u64),
                &crate::commands::dashboard_launch::LaunchTeam {
                    org: "acme/ctx".into(),
                    team: task.team.clone().unwrap(),
                },
            );
            assert_eq!(
                scope.allows(&source),
                visible,
                "fixture must exercise existing ancestor admission"
            );
            task.created_at_ms = 1_000_000;
            if id == "scope-child" {
                task.parent_id = Some("scope-root".into());
            }
            store.insert_task_if_absent(&task).unwrap();
            store.add_task_link(id, "session", "git-session").unwrap();
        }
        let log = home.join("drains.log");
        crate::attribution::testing::logging_binary(&home.join("bin/git-ai"), &log, "");
        // The generic store tree can contain different scopes; each node retains its own.
        let view = store.task_view("scope-root").unwrap().unwrap();
        let value = serde_json::to_value(view).unwrap();
        for (node, allowed) in [
            (&value, root_visible),
            (&value["children"][0], !root_visible),
        ] {
            assert_eq!(
                node["outcome"]["plannedVsActual"]["actual"][0]["path"],
                "observed.rs"
            );
            if allowed {
                assert_eq!(
                    node["outcome"]["plannedVsActual"]["planned"],
                    json!(["parent-plan.rs"])
                );
            } else {
                assert!(node["outcome"]["plannedVsActual"]["planned"].is_null());
                assert!(!node["outcome"].to_string().contains("parent-plan.rs"));
            }
        }
        assert_eq!(crate::attribution::testing::logged_calls(&log).len(), 1);
    }
}

#[tokio::test]
async fn durable_record_only_story_link_reaches_json_sessions_and_totals() {
    let (home, mut store) = store();
    let mut record = SessionRecord::new("record-only");
    record.story_id = Some("SV-29".into());
    store.write_session_record(&record).unwrap();
    let mut usage = event(
        "record-only-usage",
        "record-only",
        "gpt-6-sol",
        1_790_000_000_000,
        Tokens {
            input: 100,
            ..Default::default()
        },
    );
    usage.reported_cost_usd = Some(2.0);
    store.ingest(&[usage]).unwrap();
    let link_count = || {
        store
            .conn
            .query_row("SELECT count(*) FROM session_story", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
    };
    assert_eq!(
        link_count(),
        0,
        "fixture links the story only through session_record"
    );
    let value = get(home.path()).await;
    assert_eq!(value["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(value["sessions"][0]["session"]["sessionId"], "record-only");
    assert_eq!(value["sessions"][0]["basis"], "exact");
    assert_eq!(value["totals"]["sessions"], 1);
    assert_eq!(value["totals"]["costUsd"], 2.0);
    assert_eq!(
        link_count(),
        0,
        "reading does not backfill a session_story row"
    );
}

#[tokio::test]
async fn story_task_outcomes_keep_subtask_intervals_and_session_totals_separate() {
    let (home, mut store) = store();
    let mut record = SessionRecord::new("tab");
    record.story_id = Some("SV-29".into());
    store.write_session_record(&record).unwrap();
    std::fs::write(home.path().join("story.md"), "A story").unwrap();
    store.link_story("tab", &StoryLink { story_id:"SV-29".into(), source:home.path().join("story.md"), org:"acme".into() }).unwrap();
    let mut timing = TimingState::default();
    for (at, event) in [(1000, Event::Submitted), (4000, Event::Stopped), (4000, Event::Ended)] {
        timing.apply(&store, &Stamped { session_id:"tab".into(), at_ms:at, signal:Signal::Activity(event,"fixture") }).unwrap();
    }
    for (id, at, cost) in [("call-a",1100,1.0),("call-b",2100,2.0),("call-later",3100,4.0)] {
        let mut usage = event(id,"tab","gpt-6-sol",at,Tokens{input:100,..Default::default()});
        usage.reported_cost_usd=Some(cost);store.ingest(&[usage]).unwrap();
    }
    for (id, start, end) in [("task-a",1000,2000),("task-b",2000,3000)] {
        let mut task=TaskRecord::new(id,id);task.story_id=Some("SV-29".into());task.created_at_ms=start;
        store.insert_task_if_absent(&task).unwrap();store.add_task_link(id,"session","tab").unwrap();store.close_task(id,"done",Some("success"),end).unwrap();
    }
    let value=get(home.path()).await;
    assert_eq!(value["totals"]["sessions"],1);
    assert_eq!(value["totals"]["costUsd"],7.0);
    assert_eq!(value["totals"]["wallMs"],3000);
    for (id,cost) in [("task-a",1.0),("task-b",2.0)] {
        let task=store.read_task(id).unwrap().unwrap();
        let outcome=&value["tasks"].as_array().unwrap().iter().find(|row|row["taskId"]==id).unwrap()["outcome"];
        assert_eq!(outcome, &serde_json::to_value(store.read_outcome(&task).unwrap()).unwrap());
        assert_eq!(outcome["scope"],"interval");assert_eq!(outcome["time"]["activeMs"],1000);assert_eq!(outcome["cost"]["costUsd"],cost);
    }
}

#[tokio::test]
async fn story_task_outcomes_without_evidence_keep_the_existing_reasons() {
    let (home, store)=store();let mut task=TaskRecord::new("empty-task","No measurements");task.story_id=Some("SV-29".into());store.insert_task_if_absent(&task).unwrap();
    let value=get(home.path()).await;let outcome=&value["tasks"][0]["outcome"];
    assert!(outcome.is_object());assert!(outcome["time"].is_null());assert!(outcome["cost"].is_null());assert!(outcome["unavailable"]["time"].is_string());assert!(outcome["unavailable"]["cost"].is_string());
}

#[tokio::test]
async fn story_task_outcomes_are_identical_through_mcp_and_page() {
    use tauri::Manager;
    let fixture=crate::context_repo::caller_scope::tests::AppFixture::new();let home=&fixture.fixture.home;let store=UsageStore::open(&home.join("usage.sqlite")).unwrap();
    let mut task=TaskRecord::new("parity-task","Outcome parity");task.story_id=Some("SV-29".into());store.insert_task_if_absent(&task).unwrap();
    // Agents read outcomes with tasks.full; the default task summary has none.
    let args=json!({"storyId":"SV-29","include":["tasks.full"]});
    let mcp=crate::mcp::handler::story_changes_request(serde_json::from_value(args.clone()).unwrap(),&CallerScope::Human).await.unwrap();
    let parsed:Value=serde_json::from_str(&mcp.content[0].as_text().unwrap().text).unwrap();
    let page=crate::commands::data_read_model::story_changes(fixture.app.handle().clone(),agent::agent_read_params(&serde_json::from_value(args.clone()).unwrap()),None,None).await.unwrap();
    assert_eq!(parsed["tasks"][0]["outcome"],page["tasks"][0]["outcome"]);
    assert_mcp_projects_the_bridge_read(mcp,&page,&args);assert_eq!(page["tasks"][0]["outcome"],serde_json::to_value(store.read_outcome(&task).unwrap()).unwrap());
}

#[tokio::test]
async fn contribution_dto_net_lines_and_author_match_mcp_and_page() {
    let fixture=crate::context_repo::caller_scope::tests::AppFixture::new();
    let home=&fixture.fixture.home;let mut store=UsageStore::open(&home.join("usage.sqlite")).unwrap();let f=GitStory::new();
    f.seed(home,&mut store,"feat/SV-29-work",false);
    let one=f.commit("refs/heads/feat/SV-29-work","first","changed.rs",1100,&[f.root]);
    let tip=f.commit("refs/heads/feat/SV-29-work","second","changed.rs",1200,&[one]);
    let merged=f.commit("refs/heads/main","Merge pull request #5 from acme/feat/SV-29-work","changed.rs",1300,&[f.root,tip]);
    f.repo.reference("refs/remotes/origin/main",merged,true,"fixture").unwrap();
    let agent_params=||serde_json::from_value::<AgentParams>(json!({"storyId":"SV-29"})).unwrap();
    let mcp=crate::mcp::handler::story_changes_request(agent_params(),&CallerScope::Human).await.unwrap();
    let page=crate::commands::data_read_model::story_changes(fixture.app.handle().clone(),agent::agent_read_params(&agent_params()),None,None).await.unwrap();
    assert_mcp_projects_the_bridge_read(mcp,&page,&json!({"storyId":"SV-29"}));
    assert_eq!(page["actual"][0]["lines"],json!({"added":1,"removed":0}));
    assert!(page["actual"][0]["linesReason"].is_null());
    assert_eq!(page["commits"][0]["author"],"fixture");
    assert_eq!(page["prs"][0]["mergedAt"],1300000);
    assert_eq!(page["prs"][0]["url"],"https://github.com/acme/code/pull/5");
}

#[tokio::test]
async fn contribution_dto_pr_links_use_only_the_repositories_origin() {
    for (origin, expected, reason) in [
        (Some("https://github.com/Owner/Repo"),Some("https://github.com/Owner/Repo/pull/7"),None),
        (Some("https://github.com/Owner/Repo.git"),Some("https://github.com/Owner/Repo/pull/7"),None),
        (Some("git@github.com:Owner/Repo.git"),Some("https://github.com/Owner/Repo/pull/7"),None),
        (Some("ssh://git@github.com/Owner/Repo.git"),Some("https://github.com/Owner/Repo/pull/7"),None),
        (Some("https://gitlab.com/Owner/Repo.git"),None,Some("origin is not GitHub")),
        (None,None,Some("origin not found locally")),
    ] {
        let (home,mut store)=store();let f=GitStory::new();f.seed(home.path(),&mut store,"feat/SV-29-work",false);
        let tip=f.commit("refs/heads/feat/SV-29-work","change","a.rs",1100,&[f.root]);
        let merged=f.commit("refs/heads/main","Merge pull request #7 from acme/feat/SV-29-work","a.rs",1300,&[f.root,tip]);
        f.repo.reference("refs/remotes/origin/main",merged,true,"fixture").unwrap();
        if let Some(url)=origin {f.repo.remote_set_url("origin",url).unwrap();}else{f.repo.remote_delete("origin").unwrap();f.repo.reference("refs/remotes/origin/main",merged,true,"retained history without a configured remote").unwrap();}
        let value=get(home.path()).await;
        assert_eq!(value["prs"][0]["url"].as_str(),expected,"{origin:?}");
        assert_eq!(value["prs"][0]["urlReason"].as_str(),reason,"{origin:?}");
        assert_eq!(value["prs"][0]["mergedAt"],1300000);
    }
}

#[tokio::test]
async fn contribution_dto_open_pr_has_no_guessed_number_link_or_merge_time() {
    let (home,mut store)=store();let f=GitStory::new();f.seed(home.path(),&mut store,"feat/SV-29-work",false);
    f.commit("refs/heads/feat/SV-29-work","change","a.rs",1100,&[f.root]);
    let value=get(home.path()).await;let pr=&value["branches"][0]["pr"];
    assert!(pr["url"].is_null());assert_eq!(pr["urlReason"],"PR number not available");
    assert!(pr["mergedAt"].is_null());assert_eq!(pr["mergedAtReason"],"not merged");
    assert_eq!(pr["unavailable"],"not merged; open PR lookup needs GitHub");
}

#[tokio::test]
async fn contribution_dto_binary_lines_are_unknown_with_a_reason() {
    let (home,mut store)=store();let f=GitStory::new();f.seed(home.path(),&mut store,"feat/SV-29-work",false);
    let mut index=f.repo.index().unwrap();index.read_tree(&f.repo.find_commit(f.root).unwrap().tree().unwrap()).unwrap();
    std::fs::write(f.dir.path().join("image.bin"),b"\0binary\0content").unwrap();index.add_path(Path::new("image.bin")).unwrap();
    let tree=f.repo.find_tree(index.write_tree().unwrap()).unwrap();let sig=git2::Signature::now("fixture","fixture@example.com").unwrap();
    f.repo.commit(Some("refs/heads/feat/SV-29-work"),&sig,&sig,"binary",&tree,&[&f.repo.find_commit(f.root).unwrap()]).unwrap();
    let value=get(home.path()).await;let file=&value["actual"][0];assert!(file["lines"].is_null());assert_eq!(file["linesReason"],"binary");
}

#[tokio::test]
async fn contribution_dto_stacked_story_refs_count_the_containing_range_once() {
    for (parent_name, child_name) in [("feat/SV-29-a", "feat/SV-29-z"), ("feat/SV-29-z", "feat/SV-29-a")] {
        let (home, mut store) = store();
        let fixture = GitStory::new();
        fixture.seed(home.path(), &mut store, parent_name, false);
        let parent = fixture.commit(&format!("refs/heads/{parent_name}"), "parent", "shared.rs", 1100, &[fixture.root]);
        fixture.commit(&format!("refs/heads/{child_name}"), "child", "shared.rs", 1200, &[parent]);
        let value = get(home.path()).await;
        assert_eq!(value["total"], 2);
        let file = value["actual"].as_array().unwrap().iter().find(|file| file["path"] == "shared.rs").unwrap();
        assert_eq!(file["lines"], json!({"added":1,"removed":0}));
        assert!(file["linesReason"].is_null());
    }
}

#[tokio::test]
async fn contribution_dto_merged_lines_use_the_landed_first_parent_diff() {
    for squash in [false, true] {
        let (home, mut store) = store(); let fixture = GitStory::new();
        fixture.seed(home.path(), &mut store, "feat/SV-29-work", false);
        let tip = fixture.commit("refs/heads/feat/SV-29-work", "branch", "changed.rs", 1100, &[fixture.root]);
        let message = if squash { "SV-29: landed (#42)\n\nresolved content" } else { "Merge pull request #42 from acme/feat/SV-29-work\n\nresolved content" };
        let parents = if squash { vec![fixture.root] } else { vec![fixture.root, tip] };
        let merged = fixture.commit("refs/heads/main", message, "changed.rs", 1200, &parents);
        fixture.repo.reference("refs/remotes/origin/main", merged, true, "landed fixture").unwrap();
        fixture.repo.find_reference("refs/heads/feat/SV-29-work").unwrap().delete().unwrap();
        let value = get(home.path()).await;
        let file = value["actual"].as_array().unwrap().iter().find(|file| file["path"] == "changed.rs").unwrap();
        assert_eq!(file["lines"], json!({"added":3,"removed":0}));
        assert!(file["linesReason"].is_null()); assert_eq!(value["totals"]["files"], 1);
        if squash { assert_eq!(value["prs"][0]["commits"]["unavailable"], "squash-merged"); }
    }
}
#[tokio::test]
async fn contribution_dto_sums_landed_pr_diffs_once_and_ignores_live_ranges() {
    let (home, mut store) = store(); let fixture = GitStory::new();
    fixture.seed(home.path(), &mut store, "feat/SV-29-one", false);
    let one = fixture.commit("refs/heads/feat/SV-29-one", "one", "shared.rs", 1100, &[fixture.root]);
    let landed_one = fixture.commit("refs/heads/main", "Merge pull request #1 from acme/feat/SV-29-one", "shared.rs", 1200, &[fixture.root, one]);
    let two = fixture.commit("refs/heads/feat/SV-29-two", "two", "shared.rs", 1300, &[landed_one]);
    let landed_two = fixture.commit("refs/heads/main", "Merge pull request #2 from acme/feat/SV-29-two\n\nresolution", "shared.rs", 1400, &[landed_one, two]);
    fixture.repo.reference("refs/remotes/origin/main", landed_two, true, "landed fixture").unwrap();
    fixture.commit("refs/heads/feat/SV-29-live", "unlanded", "shared.rs", 1500, &[landed_two]);
    let value = get(home.path()).await;
    assert_eq!(value["prs"].as_array().unwrap().len(), 2);
    let file = value["actual"].as_array().unwrap().iter().find(|file| file["path"] == "shared.rs").unwrap();
    assert_eq!(file["lines"], json!({"added":4,"removed":1}));
}
#[tokio::test]
async fn contribution_dto_unlanded_files_after_a_landed_pr_have_no_invented_counts() {
    let (home, mut store) = store(); let f = GitStory::new();
    f.seed(home.path(), &mut store, "feat/SV-29-one", false);
    let one = f.commit("refs/heads/feat/SV-29-one", "one", "shared.rs", 1100, &[f.root]);
    let landed = f.commit("refs/heads/main", "Merge pull request #1 from acme/feat/SV-29-one", "shared.rs", 1200, &[f.root, one]);
    f.repo.reference("refs/remotes/origin/main", landed, true, "landed fixture").unwrap();
    f.commit("refs/heads/feat/SV-29-live", "unlanded", "new.rs", 1300, &[landed]);
    let value = get(home.path()).await;
    let file = value["actual"].as_array().unwrap().iter().find(|file| file["path"] == "new.rs").unwrap();
    assert!(file["lines"].is_null(), "{file}");
    assert_eq!(file["linesReason"], "not in this story's merged PRs");
}
fn commit_bytes(f: &GitStory, reference: &str, subject: &str, file: &str, bytes: &[u8], parents: &[git2::Oid]) -> git2::Oid {
    let mut index = f.repo.index().unwrap();
    index.read_tree(&f.repo.find_commit(parents[0]).unwrap().tree().unwrap()).unwrap();
    std::fs::write(f.dir.path().join(file), bytes).unwrap();
    index.add_path(Path::new(file)).unwrap();
    let tree = f.repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::new("fixture", "fixture@example.com", &git2::Time::new(1500, 0)).unwrap();
    let parents: Vec<_> = parents.iter().map(|p| f.repo.find_commit(*p).unwrap()).collect();
    f.repo.commit(Some(reference), &sig, &sig, subject, &tree, &parents.iter().collect::<Vec<_>>()).unwrap()
}
#[tokio::test]
async fn contribution_dto_binary_reason_survives_a_later_landed_pr() {
    // Landed PRs are summed in commit-id order; the variants cover both orders.
    for variant in 0..8 {
        let (home, mut store) = store(); let f = GitStory::new();
        f.seed(home.path(), &mut store, "feat/SV-29-one", false);
        let one = commit_bytes(&f, "refs/heads/feat/SV-29-one", "one", "asset.dat", b"\0binary\0", &[f.root]);
        let landed_one = commit_bytes(&f, "refs/heads/main", "Merge pull request #1 from acme/feat/SV-29-one", "asset.dat", b"\0binary\0", &[f.root, one]);
        let text = commit_bytes(&f, "refs/heads/main", "text again", "asset.dat", b"now text\n", &[landed_one]);
        let two = commit_bytes(&f, "refs/heads/feat/SV-29-two", "two", "asset.dat", b"now text\nand more\n", &[text]);
        let landed_two = commit_bytes(&f, "refs/heads/main", &format!("Merge pull request #2 from acme/feat/SV-29-two\n\nvariant {variant}"), "asset.dat", b"now text\nand more\n", &[text, two]);
        f.repo.reference("refs/remotes/origin/main", landed_two, true, "landed fixture").unwrap();
        let value = get(home.path()).await;
        let file = value["actual"].as_array().unwrap().iter().find(|file| file["path"] == "asset.dat").unwrap();
        assert!(file["lines"].is_null(), "variant {variant}: {file}");
        assert_eq!(file["linesReason"], "binary", "variant {variant}");
    }
}
#[tokio::test]
async fn contribution_dto_landed_pr_without_a_local_parent_keeps_lines_unknown() {
    let (home, mut store) = store(); let f = GitStory::new();
    f.seed(home.path(), &mut store, "feat/SV-29-work", false);
    let tip = f.commit("refs/heads/feat/SV-29-work", "change", "changed.rs", 1100, &[f.root]);
    let tree = f.repo.find_commit(tip).unwrap().tree_id();
    let missing = "1".repeat(40);
    let raw = format!("tree {tree}\nparent {missing}\nparent {tip}\nauthor fixture <fixture@example.com> 1300 +0000\ncommitter fixture <fixture@example.com> 1300 +0000\n\nMerge pull request #9 from acme/feat/SV-29-work\n");
    let merged = f.repo.odb().unwrap().write(git2::ObjectType::Commit, raw.as_bytes()).unwrap();
    f.repo.reference("refs/heads/main", merged, true, "fixture").unwrap();
    f.repo.reference("refs/remotes/origin/main", merged, true, "fixture").unwrap();
    let value = get(home.path()).await;
    for file in value["actual"].as_array().unwrap() {
        assert!(file["lines"].is_null(), "{file}");
        assert!(file["linesReason"].is_string(), "{file}");
    }
}
#[tokio::test]
async fn contribution_dto_source_repo_comes_from_story_frontmatter() {
    let (home, mut store) = store(); let f = GitStory::new();
    f.seed(home.path(), &mut store, "feat/SV-29-work", false);
    f.commit("refs/heads/feat/SV-29-work", "change", "changed.rs", 1100, &[f.root]);
    assert_eq!(get(home.path()).await["sourceRepo"], "acme/code");
    std::fs::write(home.path().join("story.md"), "---\ntitle: no repo\n---\n").unwrap();
    assert!(get(home.path()).await["sourceRepo"].is_null());
}
#[tokio::test]
async fn contribution_dto_incomparable_overlapping_branches_keep_lines_unknown() {
    let (home, mut store) = store(); let fixture = GitStory::new();
    fixture.seed(home.path(), &mut store, "feat/SV-29-base", false);
    let shared = fixture.commit("refs/heads/feat/SV-29-base", "shared", "shared.rs", 1100, &[fixture.root]);
    fixture.commit("refs/heads/feat/SV-29-left", "left", "left.rs", 1200, &[shared]);
    fixture.commit("refs/heads/feat/SV-29-right", "right", "right.rs", 1300, &[shared]);
    let value = get(home.path()).await;
    assert_eq!(value["total"], 3); assert_eq!(value["totals"]["files"], 3);
    assert_eq!(value["unavailable"]["fileLines"], "story spans branches with no single final tree");
    for file in value["actual"].as_array().unwrap() {
        assert!(file["lines"].is_null());
        assert_eq!(file["linesReason"], "story spans branches with no single final tree");
    }
}

#[tokio::test]
async fn opt_in_ranges_preserve_default_bytes_and_explain_absent_notes() {
    let (home, mut store) = store();
    let f = GitStory::new();
    f.seed(home.path(), &mut store, "feat/SV-29-work", false);
    f.commit(
        "refs/heads/feat/SV-29-work",
        "work",
        "new.rs",
        1100,
        &[f.root],
    );
    let baseline = get(home.path()).await;
    let expected_commit = json!({
        "sha": f.repo.refname_to_id("refs/heads/feat/SV-29-work").unwrap().to_string(),
        "subject": "work", "author": "fixture", "authoredAt": 1100000, "files": ["new.rs"],
        "sessionIds": [], "basis": "associated"
    });
    // Default private commit shape: opt-in range fields remain absent, not null.
    assert_eq!(baseline["commits"][0], expected_commit);
    let empty: Params = serde_json::from_value(json!({"storyId":"SV-29","include":[]})).unwrap();
    assert_eq!(
        serde_json::to_vec(&baseline).unwrap(),
        serde_json::to_vec(
            &serde_json::to_value(
                read(home.path(), &empty, &CallerScope::Human)
                    .await
                    .unwrap()
            )
            .unwrap()
        )
        .unwrap()
    );
    let included: Params =
        serde_json::from_value(json!({"storyId":"SV-29","include":["ranges"]})).unwrap();
    let value = serde_json::to_value(
        read(home.path(), &included, &CallerScope::Human)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(value["commits"][0]["ranges"], json!([]));
    assert_eq!(value["commits"][0]["rangesBasis"], "unknown");
    assert!(value["commits"][0]["rangesReason"].is_string());
    assert_eq!(
        value["commits"][0]["basis"],
        baseline["commits"][0]["basis"]
    );
}

#[tokio::test]
async fn opt_in_ranges_reuse_real_notes_and_native_sessions() {
    use crate::attribution::git_ai::tests::{git, rebuild};
    let (home, mut store) = store();
    let repo = rebuild();
    crate::attribution::why_line::tests::attach(home.path(), repo.path());
    git(repo.path(), &["branch", "feat/SV-29-work"]);
    git(
        repo.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD~2"],
    );
    seed(&mut store, home.path(), "git-session", true, false);
    let mut record = store.read_session_record("git-session").unwrap().unwrap();
    record.worktree = Some(repo.path().to_string_lossy().into_owned());
    record.branch = Some("feat/SV-29-work".into());
    store.write_session_record(&record).unwrap();
    let mut linked = SessionRecord::new("linked-claude");
    linked.native_session_id = Some(crate::attribution::git_ai::tests::CLAUDE.into());
    linked.org = Some("acme".into());
    store.write_session_record(&linked).unwrap();
    let params: Params =
        serde_json::from_value(json!({"storyId":"SV-29","include":["ranges"]})).unwrap();
    let value = serde_json::to_value(
        read(home.path(), &params, &CallerScope::Human)
            .await
            .unwrap(),
    )
    .unwrap();
    let ranges: Vec<_> = value["commits"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|c| c["ranges"].as_array().unwrap().iter().map(move |r| (c, r)))
        .collect();
    assert!(!ranges.is_empty());
    assert!(ranges.iter().all(|(c, r)| c["sha"] == r["commit"]));
    assert!(ranges
        .iter()
        .any(|(_, r)| r["nativeSessionId"].is_string() && r["basis"] == "exact"));
    assert!(ranges
        .iter()
        .any(|(_, r)| r["sessionId"] == "linked-claude" && r["link"] == "linked"));
    assert!(ranges.iter().any(|(_, r)| r["nativeSessionId"]
        == crate::attribution::git_ai::tests::CODEX
        && r["link"] == "unlinked"
        && r["reason"].is_string()));
    // A configured first principal with no binding, then a stale clone, must not mask a later valid binding.
    crate::commands::attachments::forget_attachment_record(
        home.path(),
        1,
        repo.path().to_str().unwrap(),
    )
    .unwrap();
    let first = crate::context_repo::path_to_org_root(
        home.path(),
        &crate::domain::Principal::from(1),
        "acme",
    )
    .unwrap();
    let second = crate::context_repo::path_to_org_root(
        home.path(),
        &crate::domain::Principal::from(2),
        "acme",
    )
    .unwrap();
    std::fs::create_dir_all(&second).unwrap();
    for name in ["org.yml", "teams.yml", "people.yml"] {
        std::fs::copy(first.join(name), second.join(name)).unwrap();
    }
    git2::Repository::init(&second).unwrap();
    crate::commands::attachments::upsert_attachment_record(
        home.path(),
        2,
        crate::commands::attachments::existing_checkout_attachment(repo.path()).unwrap(),
        None,
        false,
    )
    .unwrap();
    let later = serde_json::to_value(
        read(home.path(), &params, &CallerScope::Human)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(later["commits"], value["commits"]);
    let stale = tempfile::tempdir().unwrap();
    git(stale.path(), &["init", "-q"]);
    git(
        stale.path(),
        &["remote", "add", "origin", "https://github.com/acme/api.git"],
    );
    crate::commands::attachments::upsert_attachment_record(
        home.path(),
        1,
        crate::commands::attachments::existing_checkout_attachment(stale.path()).unwrap(),
        None,
        false,
    )
    .unwrap();
    let later = serde_json::to_value(
        read(home.path(), &params, &CallerScope::Human)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(later["commits"], value["commits"]);
}

#[tokio::test]
async fn included_ranges_retain_sessions_after_worktree_and_clone_removal() {
    use crate::attribution::git_ai::tests::git;
    let (home, mut store) = store();
    let f = GitStory::new();
    f.repo.remote_delete("origin").unwrap();
    crate::attribution::why_line::tests::attach(home.path(), f.dir.path());
    f.repo
        .reference(
            "refs/remotes/origin/main",
            f.root,
            true,
            "restore fixture comparison base",
        )
        .unwrap();
    f.commit(
        "refs/heads/feat/SV-29-work",
        "work",
        "new.rs",
        1100,
        &[f.root],
    );
    f.seed(home.path(), &mut store, "feat/SV-29-work", false);
    std::fs::write(
        home.path().join("story.md"),
        "---\nsource_repo: acme/api\n---\n",
    )
    .unwrap();
    let worktree = home.path().join("temporary-worktree");
    git(
        f.dir.path(),
        &[
            "worktree",
            "add",
            "-q",
            worktree.to_str().unwrap(),
            "feat/SV-29-work",
        ],
    );
    let mut record = store.read_session_record("git-session").unwrap().unwrap();
    record.worktree = Some(worktree.to_string_lossy().into_owned());
    store.write_session_record(&record).unwrap();
    let params: Params =
        serde_json::from_value(json!({"storyId":"SV-29","include":["ranges"]})).unwrap();
    let before = serde_json::to_value(
        read(home.path(), &params, &CallerScope::Human)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(before["totals"]["commits"], 1);
    git(
        f.dir.path(),
        &["worktree", "remove", worktree.to_str().unwrap()],
    );
    let removed = serde_json::to_value(
        read(home.path(), &params, &CallerScope::Human)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(before["commits"], removed["commits"]);
    assert_eq!(removed["totals"]["costUsd"], 2.0);
    drop(f.repo);
    f.dir.close().unwrap();
    let missing = serde_json::to_value(
        read(home.path(), &params, &CallerScope::Human)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        missing["sessions"][0]["session"]["sessionId"],
        "git-session"
    );
    assert_eq!(missing["totals"]["costUsd"], 2.0);
    assert_eq!(missing["commits"], json!([]));
    assert_eq!(
        missing["unavailable"]["ranges"],
        "repository not found locally"
    );
    assert!(get(home.path()).await["unavailable"]
        .get("ranges")
        .is_none());
}

#[tokio::test]
async fn captured_claude_fallback_story_session_ids_and_task_selection_follow_late_mapping() {
    use crate::attribution::git_ai::tests::git;
    use crate::attribution::turns::tests::{replay_log, replay_repo};
    let (home, mut store) = store();
    let repo = replay_repo(true);
    crate::attribution::why_line::tests::attach(home.path(), repo.path());
    git(repo.path(), &["branch", "feat/SV-29-work"]);
    git(
        repo.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD^"],
    );
    seed(&mut store, home.path(), "replay-session", true, false);
    let mut r = store
        .read_session_record("replay-session")
        .unwrap()
        .unwrap();
    r.worktree = Some(repo.path().to_string_lossy().into_owned());
    r.branch = Some("feat/SV-29-work".into());
    r.cli = Some("claude".into());
    r.native_session_id = Some("c1a0de00-0000-4000-a000-000000000101".into());
    store.write_session_record(&r).unwrap();
    let mut task = TaskRecord::new("replay-task", "develop");
    task.story_id = Some("SV-29".into());
    // Outside commit time: only exact identity evidence can select this commit.
    task.created_at_ms = 1;
    task.closed_at_ms = Some(2);
    store.insert_task_if_absent(&task).unwrap();
    store
        .add_task_link("replay-task", "session", "replay-session")
        .unwrap();
    let first = get(home.path()).await;
    assert_eq!(
        first["commits"][0]["sessionIds"],
        json!(["s_5cef34e3292d7d"])
    );
    let log = replay_log(repo.path());
    let linked = get(home.path()).await;
    assert_eq!(
        linked["commits"][0]["sessionIds"],
        json!(["replay-session"])
    );
    let evidence = task_evidence(&store, &[task.clone()]).unwrap();
    let (planned, _) = task_facts(&store, &task, &evidence).unwrap();
    let planned = serde_json::to_value(planned).unwrap();
    assert_eq!(planned["basis"], "exact");
    assert_eq!(planned["actual"].as_array().unwrap().len(), 2);
    drop(store);
    std::fs::remove_file(log).unwrap();
    assert_eq!(
        get(home.path()).await["commits"][0]["sessionIds"],
        json!(["replay-session"])
    );
}

#[tokio::test]
async fn fallback_note_in_earlier_clone_uses_later_clone_checkpoint_on_first_response() {
    let (home, mut store) = store();
    let a = GitStory::new();
    let b = GitStory::new();
    let (first, second) = if a.dir.path() < b.dir.path() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    let mut sha = None;
    for (f, session) in [(first, "mapped-session"), (second, "later-clone")] {
        let tip = f.commit(
            "refs/heads/feat/SV-29-work",
            "shared",
            "shared.rs",
            1100,
            &[f.root],
        );
        assert_eq!(*sha.get_or_insert(tip), tip);
        add_repo_session(home.path(), &mut store, session, f);
    }
    let native = "c1a0de00-0000-4000-a000-000000000101";
    let mut r = store
        .read_session_record("mapped-session")
        .unwrap()
        .unwrap();
    r.native_session_id = Some(native.into());
    r.cli = Some("claude".into());
    store.write_session_record(&r).unwrap();
    let note =
        include_str!("../../attribution/testdata/git_ai_1_7_5_claude_session/native-note.txt")
            .replace(native, "s_5cef34e3292d7d")
            .replace("edited.txt", "shared.rs");
    let sig = git2::Signature::now("test", "test@example.com").unwrap();
    first
        .repo
        .note(&sig, &sig, Some("refs/notes/ai"), sha.unwrap(), &note, true)
        .unwrap();
    let log = second
        .dir
        .path()
        .join(".git/ai/working_logs")
        .join(format!("old-{}", second.root))
        .join("checkpoints.jsonl");
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(
        log,
        include_str!("../../attribution/testdata/git_ai_1_7_5_claude_session/checkpoints.jsonl"),
    )
    .unwrap();
    let result = get(home.path()).await;
    assert_eq!(
        result["commits"][0]["sessionIds"],
        json!(["mapped-session"])
    );
    assert_eq!(result["commits"][0]["basis"], "exact");
}

#[tokio::test]
async fn fallback_checkpoint_secondary_native_alias_links_story_and_why_line_to_launch() {
    use crate::attribution::git_ai::tests::git;
    use crate::attribution::turns::tests::{query, replay_log, replay_repo};
    let (home, mut store) = store();
    let repo = replay_repo(true);
    crate::attribution::why_line::tests::attach(home.path(), repo.path());
    git(repo.path(), &["branch", "feat/SV-29-work"]);
    git(
        repo.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD^"],
    );
    seed(&mut store, home.path(), "alias-launch", true, false);
    let mut r = store.read_session_record("alias-launch").unwrap().unwrap();
    r.worktree = Some(repo.path().to_string_lossy().into_owned());
    r.branch = Some("feat/SV-29-work".into());
    r.native_session_id = Some("primary-native-session".into());
    r.cli = Some("claude".into());
    store.write_session_record(&r).unwrap();
    store
        .register_launch("alias-launch", "acme", Some("dev"), "agent", "handle", None)
        .unwrap();
    let owner = store.launch_owner("alias-launch").unwrap().unwrap();
    store
        .attribute_if_absent("c1a0de00-0000-4000-a000-000000000101", &owner, "alias-launch")
        .unwrap();
    replay_log(repo.path());
    let why = query(home.path(), "edited.txt").await;
    assert_eq!(why["ranges"][0]["sessionId"], "alias-launch");
    assert_eq!(why["ranges"][0]["link"], "linked");
    let result = get(home.path()).await;
    assert_eq!(result["commits"][0]["sessionIds"], json!(["alias-launch"]));
}

#[tokio::test]
async fn identity_forms_agree_across_notes_tasks_events_records_and_aliases() {
    use crate::attribution::git_ai::tests::git;
    use crate::attribution::turns::tests::{query, replay_log, replay_repo};
    const LAUNCH: &str = "identity-launch";
    const PRIMARY: &str = "identity-primary-native";
    const SECONDARY: &str = "c1a0de00-0000-4000-a000-000000000101";
    for (form, input, linked) in [
        ("git-ai fallback", "s_5cef34e3292d7d", true),
        ("primary native", PRIMARY, true),
        ("secondary native", SECONDARY, true),
        ("CodeSense", LAUNCH, true),
        ("unmapped fallback", "s_unmapped", false),
        ("ambiguous fallback", "s_ambiguous", false),
    ] {
        let (home, mut store) = store();
        let repo = replay_repo(true);
        crate::attribution::why_line::tests::attach(home.path(), repo.path());
        git(repo.path(), &["branch", "feat/SV-29-work"]);
        git(
            repo.path(),
            &["update-ref", "refs/remotes/origin/main", "HEAD^"],
        );
        seed(&mut store, home.path(), LAUNCH, true, false);
        let mut record = store.read_session_record(LAUNCH).unwrap().unwrap();
        record.worktree = Some(repo.path().to_string_lossy().into_owned());
        record.branch = Some("feat/SV-29-work".into());
        record.cli = Some("claude".into());
        record.native_session_id = Some(PRIMARY.into());
        store.write_session_record(&record).unwrap();
        store
            .register_launch(LAUNCH, "acme", Some("dev"), "agent", "handle", None)
            .unwrap();
        let owner = store.launch_owner(LAUNCH).unwrap().unwrap();
        store
            .attribute_if_absent(SECONDARY, &owner, LAUNCH)
            .unwrap();
        replay_log(repo.path());
        let sha = git(repo.path(), &["rev-parse", "HEAD"]).trim().to_owned();
        crate::attribution::checkpoints::harvest(
            repo.path(),
            "identity-repo",
            &mut store,
            &[sha].into(),
        )
        .unwrap();
        // Primary trace evidence and two distinct native owners for an ambiguous fallback.
        store.conn.execute_batch("INSERT INTO checkpoint_trace(cli,native_session_id,trace_id,tool_use_id,git_ai_session_id) VALUES ('claude','identity-primary-native','t_4d9f8ddca3e461','toolu_01CDE55Edit','s_primary'),('claude','ambiguous-native-a','t_ambiguous_a','tool-a','s_ambiguous'),('claude','ambiguous-native-b','t_ambiguous_b','tool-b','s_ambiguous');").unwrap();
        let set_note = |id: &str| {
            let note = include_str!(
                "../../attribution/testdata/git_ai_1_7_5_claude_session/native-note.txt"
            )
            .replace(SECONDARY, id);
            git(
                repo.path(),
                &["notes", "--ref=ai", "add", "-f", "-m", &note, "HEAD"],
            );
        };
        let expected = if linked { LAUNCH } else { input };
        for source in [
            "note id",
            "task target",
            "event id",
            "record native",
            "alias",
        ] {
            match source {
                "note id" => {
                    set_note(input);
                    let why = query(home.path(), "edited.txt").await;
                    assert_eq!(
                        why["ranges"][0]["link"],
                        if linked { "linked" } else { "unlinked" },
                        "{form}: {source}"
                    );
                    if linked {
                        assert_eq!(why["ranges"][0]["sessionId"], LAUNCH, "{form}: {source}");
                        assert_eq!(
                            why["ranges"][0]["turn"]["toolUseId"], "toolu_01CDE55Edit",
                            "{form}: turn"
                        );
                        assert_eq!(
                            why["ranges"][0]["nativeSessionId"],
                            if input == SECONDARY || input.starts_with("s_") {
                                SECONDARY
                            } else {
                                PRIMARY
                            },
                            "{form}: observed native"
                        );
                    }
                    let story = get(home.path()).await;
                    assert_eq!(
                        story["commits"][0]["sessionIds"],
                        json!([expected]),
                        "{form}: story note"
                    );
                }
                "task target" => {
                    set_note("s_5cef34e3292d7d");
                    let mut task = TaskRecord::new("identity-task", "develop");
                    task.story_id = Some("SV-29".into());
                    task.created_at_ms = 1;
                    task.closed_at_ms = Some(2);
                    store.insert_task_if_absent(&task).unwrap();
                    store
                        .add_task_link(&task.task_id, "session", input)
                        .unwrap();
                    let evidence = task_evidence(&store, &[task.clone()]).unwrap();
                    let (planned, _) = task_facts(&store, &task, &evidence).unwrap();
                    assert_eq!(
                        planned.actual.len(),
                        if linked { 2 } else { 0 },
                        "{form}: {source}"
                    );
                }
                "event id" => {
                    let ids = store.event_session_ids(input).unwrap();
                    assert!(
                        ids.iter().any(|id| id == input),
                        "{form}: raw event id retained"
                    );
                    assert_eq!(
                        ids.iter().any(|id| id == LAUNCH),
                        linked,
                        "{form}: {source}"
                    );
                    if linked {
                        assert!(
                            ids.iter().any(|id| id == PRIMARY),
                            "{form}: record native retained"
                        );
                        assert!(
                            ids.iter().any(|id| id == SECONDARY),
                            "{form}: secondary event id retained"
                        );
                    }
                }
                "record native" | "alias" => {
                    let params = serde_json::from_value(json!({"sessionId":input})).unwrap();
                    let facts = session_read::read(
                        &home.path().join("usage.sqlite"),
                        &home.path().join("session-conversations"),
                        &params,
                        &CallerScope::Human,
                    )
                    .await;
                    if linked {
                        let facts = facts.unwrap();
                        assert_eq!(facts["session"]["sessionId"], LAUNCH, "{form}: {source}");
                        assert_eq!(
                            facts["session"]["nativeSessionId"], PRIMARY,
                            "{form}: primary record identity"
                        );
                        assert_eq!(
                            facts["cost"]["totalUsd"], 2.0,
                            "{form}: usage sources retained"
                        );
                        assert!(
                            facts["nativeSessionIds"]
                                .as_array()
                                .unwrap()
                                .contains(&json!(SECONDARY)),
                            "{form}: alias retained"
                        );
                    } else {
                        assert_eq!(
                            facts.unwrap_err(),
                            session_read::UNKNOWN_SESSION,
                            "{form}: {source}"
                        );
                    }
                }
                _ => unreachable!(),
            }
        }
    }
}

/// The shared grooming table drives real reader proofs as well as the mounted page.
#[test]
fn pr3_case_table_story_facts() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("../../../assets/dashboards/story-trail/pr3-case-table.json")).unwrap();
    let mut failed = Vec::new();
    for row in cases.iter().filter(|row| row["native"] == "story") {
        let id = row["id"].as_u64().unwrap();
        let result = std::panic::catch_unwind(|| match id {
            1 => {
                tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
                    let (home, mut store) = store(); let f = GitStory::new();
                    f.seed(home.path(), &mut store, "feat/SV-29-work", false);
                    let tip = f.commit("refs/heads/feat/SV-29-work", "change", "changed.rs", 1100, &[f.root]);
                    let merged = f.commit("refs/heads/main", "Merge pull request #5 from acme/feat/SV-29-work", "changed.rs", 1300, &[f.root, tip]);
                    f.repo.reference("refs/remotes/origin/main", merged, true, "fixture").unwrap();
                    let value = get(home.path()).await;
                    assert_eq!(value["prs"][0]["number"], 5);
                    assert!(value["commits"].as_array().unwrap().iter().any(|row| row["sha"] == tip.to_string()));
                    assert_eq!(value["branches"][0]["availability"], "local");
                    assert_eq!(value["actual"][0]["lines"], json!({"added":1,"removed":0}));
                });
            },
            2 => contribution_dto_merged_lines_use_the_landed_first_parent_diff(),
            3 => contribution_dto_sums_landed_pr_diffs_once_and_ignores_live_ranges(),
            4 => contribution_dto_stacked_story_refs_count_the_containing_range_once(),
            5 => contribution_dto_incomparable_overlapping_branches_keep_lines_unknown(),
            6 => { contribution_dto_binary_lines_are_unknown_with_a_reason(); contribution_dto_binary_reason_survives_a_later_landed_pr(); },
            7 | 8 => planned_touchpoints_decisions_untouched_and_missing_section(),
            9 => git_live_and_removed_worktree_keep_commits_and_complete_totals(),
            10 => { contribution_dto_pr_links_use_only_the_repositories_origin(); contribution_dto_open_pr_has_no_guessed_number_link_or_merge_time(); },
            11 => git_paging_150_commits_has_known_total_and_refuses_stale_cursor(),
            19 => contribution_dto_unlanded_files_after_a_landed_pr_have_no_invented_counts(),
            24 => contribution_dto_landed_pr_without_a_local_parent_keeps_lines_unknown(),
            25 => contribution_dto_source_repo_comes_from_story_frontmatter(),
            _ => panic!("Missing native story case {id}"),
        });
        if result.is_err() { failed.push((id, row["name"].clone())); }
    }
    assert!(failed.is_empty(), "PR3 native fact rows failed: {failed:?}");
}

#[tokio::test]
async fn sweep_story_and_why_line_share_canonical_checkpoint_coverage_after_pruning() {
    use crate::attribution::git_ai::tests::git;
    use crate::attribution::turns::tests::{query, replay_log, replay_repo};
    let (home, mut store) = store();
    let repo = replay_repo(true);
    crate::attribution::why_line::tests::attach(home.path(), repo.path());
    git(repo.path(), &["branch", "feat/SV-29-work"]);
    git(
        repo.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD^"],
    );
    seed(&mut store, home.path(), "canonical-session", true, false);
    let mut record = store
        .read_session_record("canonical-session")
        .unwrap()
        .unwrap();
    record.worktree = Some(repo.path().to_string_lossy().into_owned());
    record.branch = Some("feat/SV-29-work".into());
    record.cli = Some("claude".into());
    record.native_session_id = Some("c1a0de00-0000-4000-a000-000000000101".into());
    store.write_session_record(&record).unwrap();
    let log = replay_log(repo.path());
    let story = get(home.path()).await;
    assert_eq!(
        story["commits"][0]["sessionIds"],
        json!(["canonical-session"])
    );
    let base = git(repo.path(), &["rev-parse", "HEAD^"]);
    assert!(store
        .checkpoint_base_seen("github.com/acme/api", base.trim())
        .unwrap());
    assert!(!store
        .checkpoint_base_seen(&repo.path().to_string_lossy(), base.trim())
        .unwrap());
    std::fs::remove_file(log).unwrap();
    let why = query(home.path(), "edited.txt").await;
    assert_eq!(why["ranges"][0]["sessionId"], "canonical-session");
    assert_eq!(why["ranges"][0]["turn"]["toolUseId"], "toolu_01CDE55Edit");
}

#[tokio::test]
async fn review_local_linked_shares_attachment_cursor_identity() {
    review_attachment_cursor_identity(true).await;
}

#[tokio::test]
async fn review_upstream_fork_shares_attachment_cursor_identity() {
    review_attachment_cursor_identity(false).await;
}

async fn review_attachment_cursor_identity(local_linked: bool) {
    use crate::attribution::git_ai::tests::git;
    use crate::attribution::turns::tests::{replay_log, replay_repo};
    use crate::commands::source_repo_add::{add_source_repo_to_team, AddRepoInput, AttachmentEnv};
    use tauri::Manager;
    let f = crate::context_repo::caller_scope::tests::AppFixture::new();
    let home = &f.fixture.home;
    git2::Repository::init(&f.fixture.root).unwrap();
    let repo = replay_repo(true);
    if !local_linked {
        git(
            repo.path(),
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/fork-owner/api.git",
            ],
        );
        git(
            repo.path(),
            &[
                "remote",
                "add",
                "upstream",
                "https://github.com/acme/api.git",
            ],
        );
    }
    git(repo.path(), &["branch", "feat/SV-29-work"]);
    git(
        repo.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD^"],
    );
    let linked_home = tempfile::tempdir().unwrap();
    let linked = linked_home.path().join("local-linked");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            linked.to_str().unwrap(),
            "feat/SV-29-work",
        ],
    );
    let selected = if local_linked {
        linked.as_path()
    } else {
        repo.path()
    };
    let coord = crate::content_engine::sync::WaveCoordinator::default();
    let active = f.app.state::<crate::mcp::state::ActiveTeamState>();
    let added = add_source_repo_to_team(
        &AttachmentEnv {
            app: f.app.handle(),
            coord: &coord,
            active_team: &active,
            home,
            user_id: 1,
        },
        "acme/ctx",
        "child",
        AddRepoInput::LocalPath {
            path: selected.to_string_lossy().into_owned(),
        },
    )
    .await
    .unwrap();
    let principal = crate::domain::Principal::from(1);
    crate::attribution::prefs::set_for_checkout(
        home,
        &principal,
        selected.to_str().unwrap(),
        Some(crate::attribution::prefs::AttributionChoice::On),
        None,
    )
    .unwrap();
    let preference_key = crate::attribution::prefs::repo_key_for_checkout(selected)
        .unwrap()
        .0;
    assert_ne!(preference_key, added.remote_key);
    let db = home.join("usage.sqlite");
    let mut store = UsageStore::open(&db).unwrap();
    seed(&mut store, home, "binding-session", true, false);
    let mut record = store
        .read_session_record("binding-session")
        .unwrap()
        .unwrap();
    record.worktree = Some(linked.to_string_lossy().into_owned());
    record.branch = Some("feat/SV-29-work".into());
    record.cli = Some("claude".into());
    record.native_session_id = Some("c1a0de00-0000-4000-a000-000000000101".into());
    store.write_session_record(&record).unwrap();
    let log = replay_log(repo.path()).canonicalize().unwrap();
    std::fs::write(log.parent().unwrap().join(".archived_at"), "1").unwrap();
    crate::attribution::catch_up::run_at(home, &db);
    let status = crate::attribution::catch_up::capture(home);
    assert_eq!(status.clones_swept, 1, "local linked={local_linked}");
    assert_eq!(status.trace_rows_stored, 2);
    let story = get(home).await;
    assert_eq!(
        story["commits"][0]["sessionIds"],
        json!(["binding-session"])
    );
    assert!(store
        .checkpoint_cursor(&added.remote_key, &log.to_string_lossy())
        .unwrap()
        .is_some());
    assert!(store
        .checkpoint_cursor(&preference_key, &log.to_string_lossy())
        .unwrap()
        .is_none());
    std::fs::remove_file(log).unwrap();
    let why = crate::attribution::why_line::resolve_at(
        home,
        &principal,
        &db,
        &home.join("session-conversations"),
        &crate::attribution::why_line::Params {
            repo: added.remote_key,
            rev: Some("HEAD".into()),
            story: None,
            path: "edited.txt".into(),
            start: 1,
            end: 1,
        },
        &CallerScope::Human,
        Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(why["ranges"][0]["turn"]["toolUseId"], "toolu_01CDE55Edit");
}

/// The real git-ai fixture as a story: commits c1 (claude 3, codex 3) and c2 (human 1).
async fn line_share_story(attached: bool) -> (tempfile::TempDir, tempfile::TempDir) {
    use crate::attribution::git_ai::tests::{git, rebuild};
    let (home, mut store) = store();
    let repo = rebuild();
    if attached {
        crate::attribution::why_line::tests::attach(home.path(), repo.path());
    }
    git(repo.path(), &["branch", "feat/SV-29-work"]);
    git(repo.path(), &["update-ref", "refs/remotes/origin/main", "HEAD~2"]);
    seed(&mut store, home.path(), "git-session", true, false);
    let mut record = store.read_session_record("git-session").unwrap().unwrap();
    record.worktree = Some(repo.path().to_string_lossy().into_owned());
    record.branch = Some("feat/SV-29-work".into());
    store.write_session_record(&record).unwrap();
    (home, repo)
}

#[tokio::test]
async fn line_totals_cover_every_commit_for_any_page_and_survive_a_warm_read() {
    let (home, _repo) = line_share_story(true).await;
    let mut seen = Vec::new();
    for page in [
        json!({"storyId":"SV-29"}),
        json!({"storyId":"SV-29","limit":1}),
        json!({"storyId":"SV-29","limit":1,"include":["ranges"]}),
    ] {
        let params: Params = serde_json::from_value(page).unwrap();
        let v = serde_json::to_value(read(home.path(), &params, &CallerScope::Human).await.unwrap()).unwrap();
        assert_eq!(v["totals"]["lines"], serde_json::to_value(line_share::Lines::of(&[("claude", 3), ("codex", 3)], 1, 0)).unwrap());
        assert!(v["unavailable"].get("lines").is_none());
        let l = &v["totals"]["lines"];
        assert_eq!(
            l["ai"]["total"].as_u64().unwrap() + l["human"].as_u64().unwrap() + l["unattributed"].as_u64().unwrap(),
            l["total"].as_u64().unwrap()
        );
        seen.push(v["totals"]["lines"].clone());
    }
    let first: Params = serde_json::from_value(json!({"storyId":"SV-29","limit":1})).unwrap();
    let first = serde_json::to_value(read(home.path(), &first, &CallerScope::Human).await.unwrap()).unwrap();
    let cursor = first["nextCursor"].as_str().expect("a second page exists").to_owned();
    let second: Params = serde_json::from_value(json!({"storyId":"SV-29","limit":1,"cursor":cursor})).unwrap();
    let second = serde_json::to_value(read(home.path(), &second, &CallerScope::Human).await.unwrap()).unwrap();
    assert_ne!(first["commits"], second["commits"], "a different page");
    seen.push(second["totals"]["lines"].clone());
    assert!(seen.windows(2).all(|w| w[0] == w[1]), "page independent");
}

/// SV-CDE-73: five lines an OpenCode session commits on the story branch, after git-ai's fixup
/// ran, are unattributed, never the person's, and the commit's reason names the CLI.
#[tokio::test]
async fn an_unsupported_cli_commit_counts_unattributed_and_names_the_cli() {
    use crate::attribution::unsupported_tests::{commit_and_fixup, opencode_session_env};
    let (home, repo) = line_share_story(true).await;
    crate::attribution::git_ai::tests::git(repo.path(), &["checkout", "-q", "feat/SV-29-work"]);
    let env = opencode_session_env();
    let sha = commit_and_fixup(repo.path(), &env, "opencode.txt", 5, "SV-29: opencode work");
    let params: Params =
        serde_json::from_value(json!({"storyId":"SV-29","include":["ranges"]})).unwrap();
    let v = serde_json::to_value(read(home.path(), &params, &CallerScope::Human).await.unwrap())
        .unwrap();
    assert_eq!(
        v["totals"]["lines"],
        serde_json::to_value(line_share::Lines::of(&[("claude", 3), ("codex", 3)], 1, 5)).unwrap()
    );
    let commit = v["commits"].as_array().unwrap().iter().find(|c| c["sha"] == sha).unwrap();
    // A no-note commit keeps empty ranges with a reason, which names the CLI.
    assert_eq!(commit["ranges"], json!([]), "{commit}");
    assert_eq!(commit["rangesBasis"], "unknown");
    assert_eq!(commit["rangesReason"], "unsupported_cli: opencode");
}

/// SV-CDE-73: one read of the reflogs serves every noteless commit of a story read.
#[tokio::test]
async fn one_reflog_pass_serves_every_noteless_commit_of_a_story_read() {
    use crate::attribution::unsupported::passes;
    use crate::attribution::unsupported_tests::{commit_and_fixup, opencode_session_env};
    let (home, repo) = line_share_story(true).await;
    crate::attribution::git_ai::tests::git(repo.path(), &["checkout", "-q", "feat/SV-29-work"]);
    let env = opencode_session_env();
    for n in 1..=3 {
        commit_and_fixup(repo.path(), &env, &format!("o{n}.txt"), 1, &format!("SV-29: o{n}"));
    }
    let params: Params =
        serde_json::from_value(json!({"storyId":"SV-29","include":["ranges"]})).unwrap();
    let before = passes::of(repo.path());
    let v = serde_json::to_value(read(home.path(), &params, &CallerScope::Human).await.unwrap())
        .unwrap();
    let named = v["commits"].as_array().unwrap().iter().filter(|c| c["rangesReason"] == "unsupported_cli: opencode").count();
    assert_eq!(named, 3, "{v}");
    assert_eq!(passes::of(repo.path()) - before, 1);
}

#[tokio::test]
async fn line_totals_are_null_with_a_reason_when_no_checkout_is_bound() {
    let (home, _repo) = line_share_story(false).await;
    let v = get(home.path()).await;
    assert!(v["totals"]["lines"].is_null());
    assert_eq!(v["unavailable"]["lines"], "no local copy");
    assert_eq!(v["totals"]["commits"], 2, "the rest of the answer is unaffected");
}

#[tokio::test]
async fn merged_at_is_the_committer_time_of_the_merged_sha_for_both_pr_shapes() {
    let (home, mut store) = store();
    let f = GitStory::new();
    f.seed(home.path(), &mut store, "feat/SV-29-work", false);
    let tip = f.commit("refs/heads/feat/SV-29-work", "work", "w.rs", 1200, &[f.root]);
    let squash = f.commit_authored("refs/heads/main", "SV-29: original (#7)", "old.rs", 1100, 800, &[f.root]);
    let merge = f.commit_authored(
        "refs/heads/main",
        "Merge pull request #17 from acme/feat/SV-29-work",
        "w.rs",
        1300,
        900,
        &[squash, tip],
    );
    f.repo.reference("refs/remotes/origin/main", merge, true, "merge").unwrap();
    let v = get(home.path()).await;
    let at = |n: u64| {
        v["prs"].as_array().unwrap().iter().find(|pr| pr["number"] == n).unwrap()["mergedAt"].clone()
    };
    assert_eq!(at(17), json!(1_300_000), "merge commit time");
    assert_eq!(at(7), json!(1_100_000), "squash commit time");
    assert!(
        v["prs"].as_array().unwrap().iter().all(|pr| pr["mergedAt"].is_i64()),
        "prs[] lists locally walked merged PRs, so mergedAt is always set"
    );
}

/// SV-PBF-287: a story's facets list its merged PRs newest first (an unknown merge time last),
/// at most `PRS_SHOWN`, with their count and the latest merge time.
#[test]
fn pr_facets_list_the_newest_prs_their_count_and_the_latest_merge() {
    let prs: Vec<Value> = (1..=7)
        .map(|n| json!({"number": n, "mergedAt": if n == 4 { Value::Null } else { json!(n * 1000) }}))
        .collect();
    let (shown, total, latest) = pr_facets(&prs);
    assert_eq!((total, latest), (7, Some(7000)));
    assert_eq!(
        shown,
        [7, 6, 5, 3, 2].map(|n| json!({"number": n, "merged": n * 1000})).to_vec()
    );
}

/// Lay the shared review-rounds fixture out as team `riverside` of org `acme`, with every
/// marker of `from` re-stamped as story SV-29. Returns the rounds folder.
fn team_markers(home: &Path, from: &str) -> std::path::PathBuf {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/session_usage/story_changes/fixtures/review-rounds/content");
    let team = home.join("users/1/orgs/acme/riverside/content");
    let rounds = team.join("reviews/rounds");
    std::fs::create_dir_all(team.join("schema")).unwrap();
    std::fs::create_dir_all(&rounds).unwrap();
    std::fs::copy(
        fixture.join("schema/review-rounds.yaml"),
        team.join("schema/review-rounds.yaml"),
    )
    .unwrap();
    for entry in std::fs::read_dir(fixture.join("reviews/rounds")).unwrap() {
        let path = entry.unwrap().path();
        let raw = std::fs::read_to_string(&path).unwrap();
        if raw.contains(&format!("\nstory: {from}\n")) {
            let raw = raw.replace(&format!("\nstory: {from}\n"), "\nstory: SV-29\n");
            std::fs::write(rounds.join(path.file_name().unwrap()), raw).unwrap();
        }
    }
    rounds
}

/// I2, cases 7, 9 and 13: `reviewRounds` counts the story's complete markers (six recorded
/// rounds whose reviewer tasks were typed `build`; the in-flight seventh never
/// counts), `reviewTasks` counts review tasks apart, and their disagreement is a warning.
#[tokio::test]
async fn review_rounds_count_complete_markers_and_disagreement_is_a_warning() {
    let (dir, mut s) = store();
    std::fs::write(
        dir.path().join("story.md"),
        "---\nstatus: in-progress\n---\n",
    )
    .unwrap();
    seed(&mut s, dir.path(), "dev", true, false);
    team_markers(dir.path(), "DEMO-1");
    let mut dev = TaskRecord::new("dev", "build");
    dev.story_id = Some("SV-29".into());
    dev.org = Some("acme".into());
    dev.team = Some("riverside".into());
    s.insert_task_if_absent(&dev).unwrap();

    let agreed = get(dir.path()).await;
    assert_eq!(agreed["totals"]["reviewRounds"], 6);
    // SV-PBF-297: no task is typed as a review, so the tasks the markers name stand in.
    assert!(agreed["totals"]["reviewTasks"].as_u64().unwrap() > 0, "never 0 beside six rounds");
    assert!(agreed["warnings"]["reviewTasks"].as_str().unwrap().starts_with("associated"));
    assert!(agreed["unavailable"].get("reviewTasks").is_none());
    assert!(agreed["unavailable"].get("reviewRounds").is_none());
    assert!(agreed["warnings"].get("reviewRounds").is_none());

    // A closed review task with no marker, and a marker that names the (build) dev task.
    let mut review = TaskRecord::new("review-unrecorded", "review");
    review.story_id = Some("SV-29".into());
    review.task_type = "review".into();
    review.status = "done".into();
    s.insert_task_if_absent(&review).unwrap();
    s.add_task_link("review-unrecorded", "relates_to:reviews", "dev")
        .unwrap();
    let mistyped = dir
        .path()
        .join("users/1/orgs/acme/riverside/content/reviews/rounds/demo1-101-r1.md");
    let raw = std::fs::read_to_string(&mistyped).unwrap();
    std::fs::write(
        &mistyped,
        raw.replace("\ntask: shell-review-a\n", "\ntask: dev\n"),
    )
    .unwrap();

    let warned = get(dir.path()).await;
    assert_eq!(
        warned["totals"]["reviewRounds"], 6,
        "a warning never changes the count"
    );
    assert_eq!(warned["totals"]["reviewTasks"], 1);
    assert!(warned["warnings"].get("reviewTasks").is_none(), "typed review tasks win over markers");
    assert_eq!(
        warned["warnings"]["reviewRounds"],
        "1 closed review task(s) have no complete review-rounds marker; \
         1 complete marker(s) name a task that is not a review task"
    );
}

/// SV-PBF-297: typed review tasks win; markers naming tasks stand in (associated); recorded
/// rounds that name none are unknown, not 0; no rounds at all is a true 0.
#[test]
fn review_task_count_is_never_a_zero_that_recorded_rounds_contradict() {
    let set = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<BTreeSet<_>>();
    let rounds = |count, named: &[&str]| Ok::<_, &'static str>((count, None, set(named), Vec::new()));
    assert_eq!(review_task_count(&set(&["r"]), &rounds(6, &["a"])), (Some(1), None));
    let (associated, note) = review_task_count(&set(&[]), &rounds(6, &["a", "b", "a"]));
    assert_eq!((associated, note.is_some()), (Some(2), true));
    assert_eq!(review_task_count(&set(&[]), &rounds(2, &[])), (None, None));
    assert_eq!(review_task_count(&set(&[]), &rounds(0, &[])), (Some(0), None));
    assert_eq!(review_task_count(&set(&[]), &Err("unreadable")), (None, None));
    // Nothing to read (unknown team, or no collection) is a true 0; only a failed read is unknown.
    for absent in [review_rounds::NO_TEAM, review_rounds::NO_COLLECTION] {
        assert_eq!(review_task_count(&set(&[]), &Err(absent)), (Some(0), None));
    }
}

/// Case 8 through `story_changes`: document and human-lens rounds with no review task count;
/// the story's complete wave and use observations do not.
#[tokio::test]
async fn rounds_without_review_tasks_count_in_story_changes() {
    let (dir, mut s) = store();
    std::fs::write(
        dir.path().join("story.md"),
        "---\nstatus: in-progress\n---\n",
    )
    .unwrap();
    seed(&mut s, dir.path(), "dev", true, false);
    team_markers(dir.path(), "DEMO-2");
    let mut dev = TaskRecord::new("dev", "build");
    dev.story_id = Some("SV-29".into());
    dev.org = Some("acme".into());
    dev.team = Some("riverside".into());
    s.insert_task_if_absent(&dev).unwrap();
    let result = get(dir.path()).await;
    assert_eq!(result["totals"]["reviewRounds"], 2);
    // SV-PBF-297: rounds recorded but no task named anywhere is unknown, never a 0.
    assert!(result["totals"]["reviewTasks"].is_null());
    assert!(result["unavailable"]["reviewTasks"].as_str().unwrap().contains("no marker names a task"));
    assert!(result["warnings"].get("reviewRounds").is_none());
}

/// SV-PBF-296: land the fixture's branch (a merge or a squash) on origin's default branch, with
/// a second, never-merged branch holding a commit of its own. Returns the story's read.
async fn landed_story_with_side_branch(squash: bool) -> Value {
    use crate::attribution::git_ai::tests::git;
    let (home, repo) = line_share_story(true).await;
    let work = git(repo.path(), &["rev-parse", "HEAD"]).trim().to_owned();
    let base = git(repo.path(), &["rev-parse", "HEAD~2"]).trim().to_owned();
    let landed = if squash {
        git(repo.path(), &["commit-tree", "HEAD^{tree}", "-p", &base, "-m", "SV-29: work (#1)"])
    } else {
        git(repo.path(), &["commit-tree", "HEAD^{tree}", "-p", &base, "-p", &work, "-m", "Merge pull request #1 from acme/feat/SV-29-work"])
    };
    git(repo.path(), &["update-ref", "refs/remotes/origin/main", landed.trim()]);
    git(repo.path(), &["checkout", "-q", "-b", "feat/SV-29-side", &base]);
    std::fs::write(repo.path().join("side.txt"), "one\ntwo\n").unwrap();
    git(repo.path(), &["add", "side.txt"]);
    git(repo.path(), &["commit", "-qm", "side"]);
    git(repo.path(), &["checkout", "-q", &work]);
    let params: Params = serde_json::from_value(json!({"storyId":"SV-29"})).unwrap();
    serde_json::to_value(read(home.path(), &params, &CallerScope::Human).await.unwrap()).unwrap()
}

/// SV-PBF-296 end to end: beside a merged PR, a never-merged branch leaves the line totals
/// (non-null, no side lines) and is reported as `unavailable.branches`, not `commits`.
#[tokio::test]
async fn an_unmerged_side_branch_leaves_the_landed_line_totals_alone() {
    let v = landed_story_with_side_branch(false).await;
    assert_eq!(v["totals"]["lines"], serde_json::to_value(line_share::Lines::of(&[("claude", 3), ("codex", 3)], 1, 0)).unwrap());
    assert!(v["unavailable"].get("lines").is_none(), "{}", v["unavailable"]);
}

/// A squash-merged story with a live branch counts the branch's commits (the squash commit is
/// not among them): the lines are the branch's, never a partial 0.
#[tokio::test]
async fn a_squash_merged_story_with_a_live_branch_never_counts_zero_lines() {
    let v = landed_story_with_side_branch(true).await;
    assert!(v["totals"]["lines"]["total"].as_u64().unwrap() > 0, "{}", v["totals"]["lines"]);
}

/// SV-CDE-91: `linesInProgress` is the old full count's own classification. After a squash merge the
/// story's branch ref lives on, so its lines stay in progress until the branch is deleted (the
/// documented limit); beside it a never-merged branch is in progress too. The rows' split matches
/// the full count's: `lines` is the full count, `linesInProgress` the full count of the commits
/// that are not landed.
#[tokio::test]
async fn lines_in_progress_and_landed_lines_are_the_full_counts_split_after_a_squash() {
    use crate::attribution::git_ai::tests::git;
    let (home, repo) = line_share_story(true).await;
    let work = git(repo.path(), &["rev-parse", "HEAD"]).trim().to_owned();
    let base = git(repo.path(), &["rev-parse", "HEAD~2"]).trim().to_owned();
    let squash = git(repo.path(), &["commit-tree", "HEAD^{tree}", "-p", &base, "-m", "SV-29: work (#1)"]);
    git(repo.path(), &["update-ref", "refs/remotes/origin/main", squash.trim()]);
    git(repo.path(), &["checkout", "-q", "-b", "feat/SV-29-side", &base]);
    std::fs::write(repo.path().join("side.txt"), "one\ntwo\n").unwrap();
    git(repo.path(), &["add", "side.txt"]);
    git(repo.path(), &["commit", "-qm", "side"]);
    git(repo.path(), &["checkout", "-q", &work]);

    let scope = CallerScope::Human;
    let store = UsageStore::open(&home.path().join("usage.sqlite")).unwrap();
    let report = story_report(&store, home.path(), "SV-29", &scope).unwrap();
    let full = read_sessions(home.path(), &scope, report.sessions).await.unwrap();
    let facts = git::collect(home.path(), &store, "SV-29", &full, &[scope]).unwrap();
    assert!(facts.commits.iter().any(|c| !c.landed), "the squash-merged branch's ref lives on");

    let store = std::sync::Mutex::new(store);
    commit_rows::write(&store.lock().unwrap(), "scope", "SV-29", &facts, 1).unwrap();
    let nothing: &dyn Fn(&rusqlite::Connection, &[(String, String)]) -> Result<(), String> = &|_, _| Ok(());
    commit_rows::sweep(&store, home.path(), 1, (std::time::Duration::from_secs(600), std::time::Duration::from_secs(60)), nothing).unwrap();
    let rows = commit_rows::rows(&store.lock().unwrap(), "scope", "SV-29").unwrap().unwrap();
    let folded = commit_rows::fold(&rows);

    let mut in_progress = facts.clone();
    in_progress.commits.retain(|c| !c.landed);
    in_progress.landed_lines = false;
    let all = line_share::totals(home.path(), &facts, None).await.unwrap();
    let open = line_share::totals(home.path(), &in_progress, None).await.unwrap();
    assert_eq!(folded.lines, commit_rows::Share::Lines(all));
    assert_eq!(folded.in_progress, commit_rows::Share::Lines(open));
}

mod attached_history {
    //! SV-CDE-70: the trail reads the attached repo's history, whatever the checkout has out.
    use super::*;
    use crate::attribution::git_ai::tests::git;
    use crate::attribution::history::tests::{assert_same_state, blackhole, refresh_story, user_state, Trail, STORY};
    use crate::attribution::ranges::HISTORY_PAST_SHALLOW;

    fn story(home: &Path, status: &str) {
        crate::attribution::history::tests::story_file(home, STORY, status);
    }

    async fn trail(home: &Path) -> Value {
        get(home).await
    }

    fn assert_lines(value: &Value) {
        assert!(value["totals"]["lines"].is_object(), "lines: {}", value["unavailable"]);
        assert_eq!(value["totals"]["lines"]["ai"]["total"], 6, "{}", value["totals"]["lines"]);
        assert!(value["unavailable"].get("lines").is_none(), "{}", value["unavailable"]);
    }

    // AC1 (story_changes): origin's default branch is `trunk`, the checkout is on old-wip.
    #[tokio::test]
    async fn story_changes_lines_come_from_origins_default_branch_not_the_checkout() {
        let home = tempfile::tempdir().unwrap();
        let fixture = Trail::new(home.path(), "trunk", None);
        git(&fixture.clone, &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/trunk"]);
        story(home.path(), "done");
        let value = trail(home.path()).await;
        assert_lines(&value);
        assert_eq!(value["prs"][0]["mergedSha"], fixture.merge.as_str(), "{}", value["prs"]);
        assert!(value["historyAsOf"]["originHead"].is_string(), "{}", value["historyAsOf"]);
    }

    // AC2 (story_changes): shallow; no fetch allowed gives the boundary reason, refresh fixes it.
    #[tokio::test]
    async fn a_shallow_trail_names_the_boundary_then_answers_after_refresh() {
        let home = tempfile::tempdir().unwrap();
        let fixture = Trail::new(home.path(), "main", Some(1));
        story(home.path(), "done");
        // No fetch is allowed: origin cannot answer.
        let hole = blackhole();
        fixture.set_origin(&hole.url);
        let value = trail(home.path()).await;
        assert_eq!(value["unavailable"]["commits"], HISTORY_PAST_SHALLOW, "{}", value["unavailable"]);
        assert_eq!(value["unavailable"]["lines"], HISTORY_PAST_SHALLOW);
        assert!(value["totals"]["lines"].is_null());
        assert_eq!(value["unavailable"]["historyFetch"], crate::attribution::history::FETCH_TIMED_OUT);
        fixture.set_origin(&format!("file://{}", fixture.origin.display()));
        let refreshed = refresh_story(home.path(), Some(STORY)).await;
        assert!(refreshed.get("status").is_none(), "{refreshed}");
        assert_lines(&trail(home.path()).await);
    }

    // AC2/AC5: the trail-open fetch fills a missing merge, leaving the user's state alone (AC3).
    #[tokio::test]
    async fn opening_a_done_trail_without_its_merge_fetches_once_and_writes_only_remote_refs() {
        let home = tempfile::tempdir().unwrap();
        let fixture = Trail::new(home.path(), "main", None);
        story(home.path(), "done");
        // The checkout last fetched before the story merged.
        git(&fixture.clone, &["update-ref", "refs/remotes/origin/main", "refs/remotes/origin/old-wip~1"]);
        let before = user_state(&fixture.clone);
        let value = trail(home.path()).await;
        assert_lines(&value);
        assert_eq!(value["prs"][0]["mergedSha"], fixture.merge.as_str());
        assert!(value["historyAsOf"]["fetchedAt"].is_i64(), "{}", value["historyAsOf"]);
        assert_same_state(&before, &user_state(&fixture.clone), "the trail-open fetch");
        // A read of present history makes no network call.
        let hole = blackhole();
        fixture.set_origin(&hole.url);
        let before = user_state(&fixture.clone);
        assert_lines(&trail(home.path()).await);
        assert_eq!(hole.hits.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_same_state(&before, &user_state(&fixture.clone), "a trail read");
    }
}

mod attached_history_review {
    //! SV-CDE-70 review round 1: partial scans, one fetch per origin head, unmerged stories.
    use super::*;
    use crate::attribution::git_ai::tests::git;
    use crate::attribution::history::tests::{blackhole, branch_commit, refresh_story, story_file, Trail, STORY};
    use crate::attribution::history::{HistorySource, FETCH_TIMED_OUT};
    use crate::attribution::ranges::{HISTORY_PAST_SHALLOW, HISTORY_PAST_SHALLOW_PARTIAL};
    use std::sync::atomic::Ordering;

    async fn story_read(home: &Path, story: &str) -> Value {
        let params: Params = serde_json::from_value(json!({ "storyId": story })).unwrap();
        serde_json::to_value(read(home, &params, &CallerScope::Human).await.unwrap()).unwrap()
    }

    fn fetched_at(home: &Path, trail: &Trail) -> Value {
        trail.source().history_as_of(home)["fetchedAt"].clone()
    }

    // M1: CP2 merged within the boundary, CP1 past it; then M2: the miss is remembered.
    #[tokio::test]
    async fn a_partial_merge_scan_is_named_and_the_failed_fetch_is_not_repeated() {
        let home = tempfile::tempdir().unwrap();
        // Depth 3 holds CP2's merge and its branch, but not CP1's merge further back.
        let trail = Trail::build(home.path(), "main", Some(3), 150, |work| {
            for n in 0..3 {
                std::fs::write(work.join("filler.txt"), format!("{n}\n")).unwrap();
                git(work, &["add", "filler.txt"]);
                git(work, &["commit", "-qm", &format!("filler {n}")]);
            }
            branch_commit(work, "feat/SV-29-cp2", "cp2.txt");
            git(work, &["merge", "--no-ff", "-q", "-m", "Merge pull request #8 from acme/feat/SV-29-cp2", "feat/SV-29-cp2"]);
        });
        // Both PR branches were deleted after merging, as hosts do.
        for branch in ["feat/SV-29-trail", "feat/SV-29-cp2"] {
            git(&trail.clone, &["update-ref", "-d", &format!("refs/remotes/origin/{branch}")]);
            git(&trail.work, &["push", "-q", "origin", &format!(":{branch}")]);
        }
        let hole = blackhole();
        trail.set_origin(&hole.url);
        let first = story_read(home.path(), STORY).await;
        assert_eq!(first["unavailable"]["commits"], HISTORY_PAST_SHALLOW_PARTIAL, "{}", first["unavailable"]);
        assert!(first["totals"]["lines"].is_null());
        assert_eq!(first["unavailable"]["historyFetch"], FETCH_TIMED_OUT);
        let hits = hole.hits.load(Ordering::SeqCst);
        assert!(hits >= 1);
        // A failed fetch is not remembered: the next read tries again.
        assert!(!trail.source().trail_fetch_spent(home.path(), STORY));
        let second = story_read(home.path(), STORY).await;
        assert!(hole.hits.load(Ordering::SeqCst) > hits, "a failed fetch was remembered");
        assert_eq!(second["unavailable"]["commits"], HISTORY_PAST_SHALLOW_PARTIAL);
        trail.set_origin(&format!("file://{}", trail.origin.display()));
        refresh_story(home.path(), Some(STORY)).await;
        let complete = story_read(home.path(), STORY).await;
        assert!(complete["unavailable"].get("commits").is_none(), "{}", complete["unavailable"]);
        assert_eq!(complete["prs"].as_array().unwrap().len(), 2, "{}", complete["prs"]);
        assert!(complete["totals"]["lines"].is_object(), "{}", complete["unavailable"]);
    }

    // M2: a done story with no merge anywhere: two reads, one fetch.
    #[tokio::test]
    async fn two_reads_of_a_done_story_missing_its_merge_make_one_fetch() {
        let home = tempfile::tempdir().unwrap();
        let trail = Trail::build(home.path(), "main", None, 150, |work| {
            branch_commit(work, "feat/SV-32-unlanded", "x.txt");
        });
        story_file(home.path(), "SV-32", "done");
        assert!(fetched_at(home.path(), &trail).is_null());
        story_read(home.path(), "SV-32").await;
        let once = fetched_at(home.path(), &trail);
        assert!(once.is_i64(), "the first read fetched");
        assert!(trail.source().trail_fetch_spent(home.path(), "SV-32"));
        std::thread::sleep(std::time::Duration::from_millis(20));
        let second = story_read(home.path(), "SV-32").await;
        assert_eq!(fetched_at(home.path(), &trail), once, "the second read did not fetch");
        assert_eq!(second["unavailable"]["historyFetch"], crate::attribution::history::FETCH_SPENT);
        // refresh_history forgets the miss, so the next open may look again.
        refresh_story(home.path(), None).await;
        assert!(!trail.source().trail_fetch_spent(home.path(), "SV-32"));
    }

    // M3: a story in progress on a shallow copy: its branch answers, nothing fetches.
    #[tokio::test]
    async fn an_in_progress_story_on_a_shallow_copy_opens_without_a_fetch() {
        let home = tempfile::tempdir().unwrap();
        let trail = Trail::build(home.path(), "main", Some(1), 150, |work| {
            branch_commit(work, "feat/SV-31-open", "open.txt");
        });
        story_file(home.path(), "SV-31", "in-progress");
        let hole = blackhole();
        trail.set_origin(&hole.url);
        let value = story_read(home.path(), "SV-31").await;
        assert_eq!(hole.hits.load(Ordering::SeqCst), 0, "opened with a fetch");
        assert!(value["unavailable"].get("historyFetch").is_none(), "{}", value["unavailable"]);
        assert_eq!(value["unavailable"]["commits"], HISTORY_PAST_SHALLOW, "its boundary commit says so");
    }

    // A trail open that deepens successfully answers in the same read.
    #[tokio::test]
    async fn a_shallow_done_trail_deepens_on_open_and_answers() {
        let home = tempfile::tempdir().unwrap();
        let trail = Trail::new(home.path(), "main", Some(1));
        let value = story_read(home.path(), STORY).await;
        assert!(value["totals"]["lines"].is_object(), "{}", value["unavailable"]);
        assert!(value["historyAsOf"]["fetchedAt"].is_i64());
        assert!(HistorySource::at(&trail.clone).is_some());
    }

    // Review r2: a story merged past the boundary, still marked review, branch deleted.
    #[tokio::test]
    async fn a_review_story_merged_past_the_boundary_without_its_branch_names_it_and_does_not_fetch() {
        let home = tempfile::tempdir().unwrap();
        let trail = Trail::new(home.path(), "main", Some(1));
        story_file(home.path(), STORY, "review");
        git(&trail.clone, &["update-ref", "-d", "refs/remotes/origin/feat/SV-29-trail"]);
        let hole = blackhole();
        trail.set_origin(&hole.url);
        let value = story_read(home.path(), STORY).await;
        assert_eq!(value["unavailable"]["commits"], HISTORY_PAST_SHALLOW, "{}", value["unavailable"]);
        assert_eq!(hole.hits.load(Ordering::SeqCst), 0, "a story not done fetched");
    }

    // Review r2: an in-progress story whose scan saw one PR before the boundary.
    #[tokio::test]
    async fn an_in_progress_partial_multi_pr_scan_names_it_and_does_not_fetch() {
        let home = tempfile::tempdir().unwrap();
        let trail = Trail::build(home.path(), "main", Some(3), 150, |work| {
            for n in 0..3 {
                std::fs::write(work.join("filler.txt"), format!("{n}\n")).unwrap();
                git(work, &["add", "filler.txt"]);
                git(work, &["commit", "-qm", &format!("filler {n}")]);
            }
            branch_commit(work, "feat/SV-29-cp2", "cp2.txt");
            git(work, &["merge", "--no-ff", "-q", "-m", "Merge pull request #8 from acme/feat/SV-29-cp2", "feat/SV-29-cp2"]);
        });
        story_file(home.path(), STORY, "in-progress");
        for branch in ["feat/SV-29-trail", "feat/SV-29-cp2"] {
            git(&trail.clone, &["update-ref", "-d", &format!("refs/remotes/origin/{branch}")]);
        }
        let hole = blackhole();
        trail.set_origin(&hole.url);
        let value = story_read(home.path(), STORY).await;
        assert_eq!(value["unavailable"]["commits"], HISTORY_PAST_SHALLOW_PARTIAL, "{}", value["unavailable"]);
        assert_eq!(hole.hits.load(Ordering::SeqCst), 0, "a story not done fetched");
    }

    struct CanonicalHome(std::path::PathBuf);
    impl CanonicalHome {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    // Review r2: refresh_history reads the story's status the way its trail does.
    #[tokio::test]
    async fn the_same_story_id_in_another_org_does_not_change_whether_it_is_done() {
        // Canonical, so team views and scanned story paths name the same directories.
        let temp = tempfile::tempdir().unwrap();
        let home = CanonicalHome(temp.path().canonicalize().unwrap());
        let trail = Trail::new(home.path(), "main", Some(1));
        assert!(story_is_done(home.path(), "sv-29", &CallerScope::Human), "case-insensitive");
        let other = crate::context_repo::path_to_org_root(home.path(), &crate::domain::Principal::from(1u64), "zeta").unwrap();
        std::fs::create_dir_all(other.join("ops/content")).unwrap();
        std::fs::write(other.join("org.yml"), "slug: zeta\ndisplay_name: Zeta\ncreated_at: \"2026-01-01T00:00:00Z\"\n").unwrap();
        // Team views admit only an org's members.
        let member = "people:\n  - github_user_id: 1\n    github_username: alice\n";
        std::fs::write(other.join("people.yml"), member).unwrap();
        std::fs::write(other.join("teams.yml"), "teams:\n  - name: ops\n").unwrap();
        git2::Repository::init(&other).unwrap();
        let acme_root = crate::context_repo::path_to_org_root(home.path(), &crate::domain::Principal::from(1u64), "acme").unwrap();
        std::fs::write(acme_root.join("people.yml"), member).unwrap();
        std::fs::write(other.join("ops/content/SV-29.md"), "---\ntype: story\nstory_id: SV-29\nsource_repo: zeta/api\nstatus: in-progress\n---\n## Purpose\nOther.\n").unwrap();
        let acme = CallerScope::team(home.path(), &crate::domain::Principal::from(1u64),
            &crate::commands::dashboard_launch::LaunchTeam { org: "acme".into(), team: "dev".into() });
        let zeta = CallerScope::team(home.path(), &crate::domain::Principal::from(1u64),
            &crate::commands::dashboard_launch::LaunchTeam { org: "zeta".into(), team: "ops".into() });
        assert!(story_is_done(home.path(), "sv-29", &acme), "case-insensitive, in the caller's org");
        assert!(!story_is_done(home.path(), STORY, &zeta));
        let params = crate::attribution::history::RefreshParams { repo: "acme/api".into(), story_id: Some(STORY.into()), timeout_ms: Some(60_000) };
        let refreshed = crate::attribution::history::refresh(home.path(), &crate::domain::Principal::from(1u64), &params, &acme).await.unwrap();
        assert!(!refreshed["deepened"].is_null(), "acme's done story deepened: {refreshed}");
        assert!(HistorySource::at(&trail.clone).is_some());
    }
}

use crate::commands::agent_read_model::AGENT_ANSWER_BUDGET;

/// The ids of `answer[array]`, by the same id the agent cursors page on.
fn listed_ids(answer: &Value, array: &str, id: &str) -> Vec<String> {
    answer[array]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row[id].as_str().unwrap().to_owned())
        .collect()
}

/// Page `array` of `full` (the bridge read) to the end with the agent's cursors, each page a
/// cursor read that answers only that array, and return every id it listed.
fn page_through(full: &Value, story: &str, array: &str, id: &str) -> Vec<String> {
    let first: AgentParams = serde_json::from_value(json!({ "storyId": story })).unwrap();
    let mut answer = agent::agent_projection(full.clone(), &first).unwrap();
    let mut ids = listed_ids(&answer, array, id);
    while let Some(cursor) = answer["pages"][array]["cursor"].as_str() {
        let next: AgentParams =
            serde_json::from_value(json!({ "storyId": story, "cursor": cursor })).unwrap();
        answer = agent::agent_projection(full.clone(), &next).unwrap();
        assert!(serde_json::to_vec(&answer).unwrap().len() <= AGENT_ANSWER_BUDGET);
        assert_eq!(answer["totals"], full["totals"], "a cursor read keeps the full totals");
        let pages = answer["pages"].as_object().unwrap();
        assert_eq!(pages.keys().collect::<Vec<_>>(), [array], "only the named array pages");
        for other in ["sessions", "tasks", "commits", "actual", "prs"] {
            if other != array {
                assert_eq!(answer[other], json!([]), "{other} on a {array} page");
            }
        }
        ids.extend(listed_ids(&answer, array, id));
    }
    ids
}

/// SV-CDE-82 case 11 (I3, I4): a story with 200 sessions and one commit touching 2,000
/// files. The agent answer (the MCP projection of the bridge read) fits 64 KB, carries
/// `pages.<array>: {total, truncated, cursor}` for every array, caps `commits[].files` at 20
/// with `filesTotal`, and keeps the unpaged totals; paging sessions and actual with their
/// cursors lists every row once, in order; a cursor over a changed set is refused. Red on
/// the base: no agent projection or array cursors, and the answer carried all 200 full
/// session rows and 2,000 actual files.
#[tokio::test]
async fn agent_answer_for_200_sessions_and_2000_files_fits_with_cursors_and_full_totals() {
    let (home, mut store) = store();
    let f = GitStory::new();
    f.seed(home.path(), &mut store, "feat/SV-29-wide", false);
    let mut index = f.repo.index().unwrap();
    index
        .read_tree(&f.repo.find_commit(f.root).unwrap().tree().unwrap())
        .unwrap();
    for n in 0..2000 {
        let file = format!("src/area_{:02}/module_{n:04}.rs", n % 40);
        let path = f.dir.path().join(&file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("// {n}\n")).unwrap();
        index.add_path(Path::new(&file)).unwrap();
    }
    let tree = f.repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::new("fixture", "f@example.com", &git2::Time::new(1100, 0)).unwrap();
    let root = f.repo.find_commit(f.root).unwrap();
    f.repo
        .commit(Some("refs/heads/feat/SV-29-wide"), &sig, &sig, "wide", &tree, &[&root])
        .unwrap();
    for n in 0..199 {
        seed(&mut store, home.path(), &format!("wide-{n:03}"), true, false);
    }

    let params: AgentParams = serde_json::from_value(json!({"storyId":"SV-29"})).unwrap();
    let read_params = agent::agent_read_params(&params);
    assert_eq!(read_params.limit, Some(agent::AGENT_LIMIT));
    let full = serde_json::to_value(read(home.path(), &read_params, &CallerScope::Human).await.unwrap())
        .unwrap();
    assert!(serde_json::to_vec(&full).unwrap().len() > 2 * AGENT_ANSWER_BUDGET);
    let answer = agent::agent_projection(full.clone(), &params).unwrap();
    assert!(serde_json::to_vec(&answer).unwrap().len() <= AGENT_ANSWER_BUDGET);
    assert_eq!(answer["totals"], full["totals"]);
    assert_eq!(answer["totals"]["sessions"], 200);
    assert_eq!(answer["totals"]["files"], 2000);
    assert_eq!(answer["pages"]["sessions"]["total"], 200);
    assert_eq!(answer["pages"]["sessions"]["truncated"], true);
    assert_eq!(answer["pages"]["actual"]["total"], 2000);
    assert_eq!(answer["pages"]["commits"]["total"], 1);
    assert_eq!(answer["pages"]["commits"]["truncated"], false);
    for name in ["tasks", "branches", "prs", "worktrees", "plannedUntouched"] {
        assert!(answer["pages"][name]["total"].is_u64(), "{name}");
    }
    assert_eq!(answer["commits"][0]["files"].as_array().unwrap().len(), agent::NESTED_FILES);
    assert_eq!(answer["commits"][0]["filesTotal"], 2000);
    let session = &answer["sessions"][0];
    assert!(session["sessionId"].is_string() && session["basis"] == "exact");
    assert!(session.get("tools").is_none(), "a summary, not the session_read row");

    let sessions = page_through(&full, "SV-29", "sessions", "sessionId");
    let expected: Vec<String> = full["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["session"]["sessionId"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(sessions, expected, "every session once, in order");
    let actual = page_through(&full, "SV-29", "actual", "path");
    assert_eq!(actual, listed_ids(&full, "actual", "path"));

    // Through the read: page two of sessions, then a refused cursor once the set changes.
    let cursor = answer["pages"]["sessions"]["cursor"].as_str().unwrap().to_owned();
    let next: AgentParams =
        serde_json::from_value(json!({"storyId":"SV-29","cursor":cursor})).unwrap();
    let second = serde_json::to_value(
        read(home.path(), &agent::agent_read_params(&next), &CallerScope::Human).await.unwrap(),
    )
    .unwrap();
    let page_two = agent::agent_projection(second, &next).unwrap();
    assert_eq!(listed_ids(&page_two, "sessions", "sessionId")[0], expected[answer["sessions"].as_array().unwrap().len()]);
    seed(&mut store, home.path(), "late-session", true, false);
    let changed = serde_json::to_value(
        read(home.path(), &agent::agent_read_params(&next), &CallerScope::Human).await.unwrap(),
    )
    .unwrap();
    assert!(agent::agent_projection(changed, &next).unwrap_err().contains("cursor"));
}

/// A large bridge read: 200 sessions whose full rows weigh about 343 KB
/// and 100 tasks whose rows weigh about 268 KB.
fn eng_108_sized_read() -> Value {
    let sessions: Vec<Value> = (0..200)
        .map(|n| {
            json!({
                "session": {
                    "sessionId": format!("s-{n:03}"),
                    "cli": "claude",
                    "model": "opus",
                    "branch": "feat/DEMO-9-x",
                    "mcpAllow": (0..60).map(|t| format!("mcp__codesense__tool_{t:03}")).collect::<Vec<_>>(),
                    "timing": {"startedAtMs": 1, "endedAtMs": 2, "wallMs": 1, "activeMs": 1, "idleMs": 0, "idleGaps": []},
                },
                "nativeSessionIds": [format!("native-{n:03}")],
                "basis": "exact",
                "cost": {"totalUsd": 1.5, "reason": null},
                "tools": {"used": [], "allowed": {}},
            })
        })
        .collect();
    let tasks: Vec<Value> = (0..100)
        .map(|n| {
            json!({
                "taskId": format!("t-{n:03}"),
                "taskType": "dev",
                "status": "done",
                "goal": "g".repeat(2600),
                "reviews": [],
                "outcome": {"reviewRounds": 0},
            })
        })
        .collect();
    json!({
        "storyId": "DEMO-9",
        "sessions": sessions,
        "tasks": tasks,
        "totals": {"sessions": 200, "commits": 0, "files": 0, "costUsd": 300.0, "reviewRounds": 4, "lines": null},
        "branches": [], "prs": [], "worktrees": [], "commits": [], "total": 0, "nextCursor": null,
        "scanLimit": 2000, "planned": null, "plannedBase": null, "actual": [], "plannedUntouched": [],
        "unavailable": {}, "historyAsOf": null,
    })
}

/// SV-CDE-82 case 14 (lead, ENG-108): sessions of 343 KB and tasks of 268 KB answer under
/// 64 KB by default, as summaries, with totals untouched; with both full shapes included the
/// heaviest array ends its page early and its cursor resumes at the first row it left out.
/// Red on the base: the MCP answer was the whole read (about 658 KB at `limit:1`).
#[test]
fn eng_108_sized_sessions_and_tasks_fit_by_default_and_page_in_full() {
    let full = eng_108_sized_read();
    let size = serde_json::to_vec(&full).unwrap().len();
    assert!(size > 600 * 1024, "{size}");
    let params: AgentParams = serde_json::from_value(json!({"storyId":"DEMO-9"})).unwrap();
    let answer = agent::agent_projection(full.clone(), &params).unwrap();
    assert!(serde_json::to_vec(&answer).unwrap().len() <= AGENT_ANSWER_BUDGET);
    assert_eq!(answer["totals"], full["totals"]);
    assert_eq!(answer["sessions"].as_array().unwrap().len(), agent::AGENT_LIMIT);
    assert_eq!(answer["sessions"][0]["sessionId"], "s-000");
    assert_eq!(answer["sessions"][0]["costUsd"], 1.5);
    assert!(answer["sessions"][0].get("tools").is_none());
    assert_eq!(answer["tasks"][0]["goal"].as_str().unwrap().chars().count(), 200);
    assert!(answer["tasks"][0].get("outcome").is_none());
    assert_eq!(answer["pages"]["tasks"]["total"], 100);

    let params: AgentParams = serde_json::from_value(
        json!({"storyId":"DEMO-9","include":["sessions.full","tasks.full"]}),
    )
    .unwrap();
    let answer = agent::agent_projection(full.clone(), &params).unwrap();
    assert!(serde_json::to_vec(&answer).unwrap().len() <= AGENT_ANSWER_BUDGET);
    assert_eq!(answer["tasks"][0]["outcome"]["reviewRounds"], 0);
    assert!(answer["sessions"][0]["tools"].is_object());
    let listed = answer["tasks"].as_array().unwrap().len();
    assert!(listed < agent::AGENT_LIMIT, "the heaviest array ends early: {listed}");
    let cursor = answer["pages"]["tasks"]["cursor"].as_str().unwrap();
    let next: AgentParams = serde_json::from_value(
        json!({"storyId":"DEMO-9","include":["tasks.full"],"cursor":cursor}),
    )
    .unwrap();
    let page = agent::agent_projection(full, &next).unwrap();
    assert_eq!(page["tasks"][0]["taskId"], format!("t-{listed:03}"));
}

/// SV-CDE-82 / CDE-78 fixture: `feat/SV-29-work` with two commits off `main`'s root, merged
/// on `origin/main` as `subject` (parents root, tip). Returns (merge, tip, first commit).
fn second_parent_merge(f: &GitStory, subject: &str) -> (git2::Oid, git2::Oid, git2::Oid) {
    let one = f.commit("refs/heads/feat/SV-29-work", "first", "one.rs", 1100, &[f.root]);
    let tip = f.commit("refs/heads/feat/SV-29-work", "second", "two.rs", 1200, &[one]);
    let merged = f.commit("refs/heads/main", subject, "two.rs", 1300, &[f.root, tip]);
    f.repo.reference("refs/remotes/origin/main", merged, true, "merge").unwrap();
    (merged, tip, one)
}

fn sorted_shas(value: &Value) -> Vec<String> {
    let mut shas: Vec<String> = value["commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["sha"].as_str().unwrap().to_owned())
        .collect();
    shas.sort();
    shas
}

/// Cases 1, 2 and 4 (I1): a merge links to its branch by subject or by `M^2` = the live
/// branch tip, the two agree, and the PR number is the subject's `#N`, else a local PR head
/// ref at `M^2`, else null with `numberReason`. Red on the base for the custom subjects:
/// only GitHub's default subject linked, so the merged branch read `prs: []` and no commits
/// (its commits are on origin/main now, so not branch-only).
#[tokio::test]
async fn a_merge_links_by_second_parent_whatever_its_subject() {
    for (subject, pr_ref, number) in [
        ("Merge pull request #5 from acme/feat/SV-29-work", false, json!(5)),
        ("Merge pull request #2769: SV-29 launch registry (review r2 APPROVE)", false, json!(2769)),
        ("Land the launch registry", true, json!(9)),
        ("Land the launch registry", false, Value::Null),
    ] {
        let (home, mut store) = store();
        let f = GitStory::new();
        f.seed(home.path(), &mut store, "feat/SV-29-work", false);
        let (merged, tip, one) = second_parent_merge(&f, subject);
        if pr_ref {
            f.repo.reference("refs/remotes/pr/9", tip, true, "pr head").unwrap();
        }
        let value = get(home.path()).await;
        assert_eq!(value["prs"].as_array().unwrap().len(), 1, "{subject}: {}", value["prs"]);
        let pr = &value["prs"][0];
        assert_eq!(pr["mergedSha"], merged.to_string(), "{subject}");
        assert_eq!(pr["branch"], "feat/SV-29-work", "{subject}");
        assert_eq!(pr["basis"], "exact", "{subject}");
        assert_eq!(pr["source"], "merge-commit", "{subject}");
        assert_eq!(pr["number"], number, "{subject}");
        if number.is_null() {
            assert_eq!(pr["numberReason"], crate::attribution::history::NO_PR_NUMBER, "{subject}");
            assert_eq!(value["branches"][0]["prs"], json!([]), "a branch lists only known numbers");
        } else {
            assert!(pr.get("numberReason").is_none(), "{subject}");
        }
        let mut expected = vec![one.to_string(), tip.to_string()];
        expected.sort();
        assert_eq!(sorted_shas(&value), expected, "{subject}: M^1..M^2");
        assert_eq!(value["branches"][0]["mergedSha"], merged.to_string(), "{subject}");
    }
}

/// A linked worktree of `feat/SV-29-work` with a story record (principal 1), the shape a
/// managed story worktree has. Returns its path.
fn story_worktree(f: &GitStory, home: &Path) -> std::path::PathBuf {
    let path = home.join("worktrees").join("sv-29");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let branch = f.repo.find_reference("refs/heads/feat/SV-29-work").unwrap();
    let mut options = git2::WorktreeAddOptions::new();
    options.reference(Some(&branch));
    f.repo.worktree("sv-29", &path, Some(&options)).unwrap();
    let record = crate::install::worktree_index::WorktreeStoryRecord::new(
        path.clone(),
        "acme",
        crate::domain::SourceRepoSlug::new("acme", "code").unwrap(),
        "SV-29",
        home.join("story.md"),
    );
    crate::install::worktree_index::write_record(home, &crate::domain::Principal::from(1u64), &record)
        .unwrap();
    path
}

fn recorded_tip(home: &Path) -> Option<crate::install::story_tips::StoryTip> {
    crate::install::story_tips::read(home, "SV-29").pop()
}

/// Case 3 (I1, review r5 H1): a custom-subject merge whose branch, worktree and worktree
/// record are all gone links by the story's recorded tip, recorded either by an earlier
/// `story_changes` read of the live worktree (b) or by the removal funnel's sighting (c).
/// Red on the base: nothing recorded a tip and the subject names no branch, so the trail
/// read `prs: []` and no commits; with the r4 `lastHead` the tip died with the record.
#[tokio::test]
async fn a_merge_links_by_a_recorded_tip_after_branch_worktree_and_record_are_gone() {
    for stamped_by_read in [true, false] {
        let (home, mut store) = store();
        let f = GitStory::new();
        f.seed(home.path(), &mut store, "feat/SV-29-work", false);
        let one = f.commit("refs/heads/feat/SV-29-work", "first", "one.rs", 1100, &[f.root]);
        let tip = f.commit("refs/heads/feat/SV-29-work", "second", "two.rs", 1200, &[one]);
        let worktree = story_worktree(&f, home.path());
        if stamped_by_read {
            let live = get(home.path()).await;
            assert!(live["worktrees"].as_array().unwrap().iter().any(|w| w["state"] == "live"));
        } else {
            crate::install::story_tips::sight_worktree(home.path(), &worktree);
        }
        let seen = recorded_tip(home.path()).expect("tip recorded");
        assert_eq!((seen.sha.as_str(), seen.branch.as_str()), (tip.to_string().as_str(), "feat/SV-29-work"));

        let merged = f.commit("refs/heads/main", "Merge pull request #2769: SV-29 work", "two.rs", 1300, &[f.root, tip]);
        f.repo.reference("refs/remotes/origin/main", merged, true, "merge").unwrap();
        std::fs::remove_dir_all(&worktree).unwrap();
        f.repo.find_reference("refs/heads/feat/SV-29-work").unwrap().delete().unwrap();
        crate::install::worktree_index::remove_record(home.path(), &crate::domain::Principal::from(1u64), &worktree).unwrap();

        let value = get(home.path()).await;
        let pr = &value["prs"][0];
        assert_eq!(pr["mergedSha"], merged.to_string(), "read stamp {stamped_by_read}: {}", value["prs"]);
        assert_eq!(pr["branch"], "feat/SV-29-work");
        assert_eq!(pr["number"], 2769);
        assert_eq!(pr["basis"], "exact");
        let mut expected = vec![one.to_string(), tip.to_string()];
        expected.sort();
        assert_eq!(sorted_shas(&value), expected);
        assert!(value["worktrees"].as_array().unwrap().iter().all(|w| w["state"] != "live"));
    }
}

/// The read's sighting is write-on-change and best-effort: a second read of an unmoved HEAD
/// writes nothing, and a read whose sighting cannot be written (the tips dir is read-only)
/// still answers in full. Red on the base: no sighting existed.
#[cfg(unix)]
#[tokio::test]
async fn the_read_records_a_moved_tip_once_and_answers_when_it_cannot() {
    use std::os::unix::fs::PermissionsExt;
    let (home, mut store) = store();
    let f = GitStory::new();
    f.seed(home.path(), &mut store, "feat/SV-29-work", false);
    let tip = f.commit("refs/heads/feat/SV-29-work", "first", "one.rs", 1100, &[f.root]);
    let _worktree = story_worktree(&f, home.path());
    let first = get(home.path()).await;
    let seen = recorded_tip(home.path()).unwrap();
    assert_eq!(seen.sha, tip.to_string());
    let again = get(home.path()).await;
    assert_eq!(recorded_tip(home.path()).unwrap(), seen, "unmoved HEAD: no write");
    assert_eq!(again["totals"], first["totals"]);

    let moved = f.commit("refs/heads/feat/SV-29-work", "second", "two.rs", 1200, &[tip]);
    let tips = home.path().join("story-tips");
    let mode = std::fs::metadata(&tips).unwrap().permissions();
    std::fs::set_permissions(&tips, std::fs::Permissions::from_mode(0o500)).unwrap();
    let blocked = read(home.path(), &params(), &CallerScope::Human).await;
    std::fs::set_permissions(&tips, mode).unwrap();
    let blocked = serde_json::to_value(blocked.expect("the read answers")).unwrap();
    assert_eq!(blocked["totals"]["commits"], 2);
    assert_eq!(recorded_tip(home.path()).unwrap().sha, tip.to_string(), "the failed sighting changed nothing");
    get(home.path()).await;
    assert_eq!(recorded_tip(home.path()).unwrap().sha, moved.to_string());
    assert_eq!(crate::install::story_tips::read(home.path(), "SV-29").len(), 2, "the older tip is kept too");
}

/// Case 13 (lead, walk 1): a custom-subject merge linked by its second parent, whose branch
/// commits carry git-ai notes, gives the same non-null `totals.lines` (with its AI share) as
/// the same merge under GitHub's default subject. Red on the base: the custom subject did
/// not link, the branch's commits were on origin/main and no longer branch-only, so the
/// story had no commits and no AI lines.
#[tokio::test]
async fn a_second_parent_merge_keeps_the_ai_line_share_of_the_default_subject() {
    use crate::attribution::git_ai::tests::git;
    let mut lines = Vec::new();
    for subject in [
        "Merge pull request #5 from acme/feat/SV-29-work",
        "Merge pull request #2769: SV-29 work (review r2 APPROVE)",
    ] {
        let (home, repo) = line_share_story(true).await;
        let merge = git(
            repo.path(),
            &["commit-tree", "HEAD^{tree}", "-p", "HEAD~2", "-p", "HEAD", "-m", subject],
        );
        git(repo.path(), &["update-ref", "refs/remotes/origin/main", merge.trim()]);
        let v = get(home.path()).await;
        assert_eq!(v["prs"][0]["mergedSha"], merge.trim(), "{subject}: {}", v["prs"]);
        assert_eq!(v["totals"]["commits"], 2, "{subject}");
        assert!(v["unavailable"].get("lines").is_none(), "{subject}: {}", v["unavailable"]);
        assert_eq!(
            v["totals"]["lines"],
            serde_json::to_value(line_share::Lines::of(&[("claude", 3), ("codex", 3)], 1, 0)).unwrap(),
            "{subject}"
        );
        lines.push(v["totals"]["lines"].clone());
    }
    assert_eq!(lines[0], lines[1], "the same AI share whatever the subject");
}

/// Review r4 L1: the 64 KB budget is measured on the whole answer, `pages` included, and
/// trims commits too. A page of 20 heavy commits (long paths, `include:["ranges"]`) ends
/// early, and the rebuilt `nextCursor` is the read's own cursor after the last kept commit,
/// whether the read had more commits or this page held them all. Red on the base: there was
/// no projection; before this round the budget skipped commits, so the page stayed ~170 KB.
#[test]
fn a_heavy_commit_page_ends_early_with_the_reads_own_cursor() {
    let shas: Vec<String> = (0..40).map(|n| format!("{n:040x}")).collect();
    let heavy = |sha: &String| {
        json!({
            "sha": sha, "subject": "work", "author": "fixture", "authoredAt": 1,
            "files": (0..30).map(|f| format!("{}/{f}.rs", "d".repeat(300))).collect::<Vec<_>>(),
            "sessionIds": [], "basis": "exact",
            "ranges": (0..10).map(|r| json!({"path": "p".repeat(200), "start": r, "end": r})).collect::<Vec<_>>(),
        })
    };
    let all: Vec<git::Commit> = shas
        .iter()
        .map(|sha| git::Commit { sha: sha.clone(), ..Default::default() })
        .collect();
    for listed in [20, 40] {
        let story = if listed == 20 { "SV-29" } else { "SV-30" };
        let set = &all[..listed];
        let read_params: Params = serde_json::from_value(json!({"storyId": story, "limit": 20})).unwrap();
        let (_, next) = git::page(&read_params, set).unwrap();
        let mut full = eng_108_sized_read();
        full["storyId"] = json!(story);
        full["sessions"] = json!([]);
        full["tasks"] = json!([]);
        full["commits"] = json!(set[..20].iter().map(|c| heavy(&c.sha)).collect::<Vec<_>>());
        full["total"] = json!(listed);
        full["nextCursor"] = json!(next);
        let params: AgentParams = serde_json::from_value(json!({"storyId": story, "include": ["ranges"]})).unwrap();
        let answer = agent::agent_projection(full, &params).unwrap();
        assert!(serde_json::to_vec(&answer).unwrap().len() <= AGENT_ANSWER_BUDGET, "{listed}");
        let kept = answer["commits"].as_array().unwrap().len();
        assert!(kept > 0 && kept < 20, "{listed}: {kept}");
        assert_eq!(answer["commits"][0]["files"].as_array().unwrap().len(), agent::NESTED_FILES);
        assert_eq!(answer["commits"][0]["filesTotal"], 30);
        let short: Params = serde_json::from_value(json!({"storyId": story, "limit": kept})).unwrap();
        let (_, expected) = git::page(&short, set).unwrap();
        assert_eq!(answer["nextCursor"], json!(expected), "{listed}: the read's own cursor");
        assert_eq!(answer["pages"]["commits"], json!({"total": listed, "truncated": true, "cursor": expected}));
    }
}

/// Review r8: through the trail read, (a) a two-parent merge under the story's squash-style
/// title whose second parent is no tip is a merge with no branch (`source: merge-commit`,
/// `branch: null`), and its M^1..M^2 commits are read; (b) a custom-subject merge whose tip
/// another story's branch shares is not linked and `unavailable.prs` says why. Red on the
/// base: the titled merge was a squash with no commits; nothing reported a shared tip.
#[tokio::test]
async fn unnamed_merges_read_their_commits_and_shared_tips_are_reported() {
    let (home, mut store) = store();
    let f = GitStory::new();
    f.seed(home.path(), &mut store, "feat/SV-29-work", false);
    let loose = f.commit("refs/heads/loose", "loose work", "loose.rs", 1100, &[f.root]);
    f.repo.find_reference("refs/heads/loose").unwrap().delete().unwrap();
    let titled = f.commit("refs/heads/main", "SV-29: land it (#8)", "loose.rs", 1200, &[f.root, loose]);
    f.repo.reference("refs/remotes/origin/main", titled, true, "merge").unwrap();
    let v = get(home.path()).await;
    let pr = v["prs"].as_array().unwrap().iter().find(|pr| pr["mergedSha"] == titled.to_string()).unwrap();
    assert_eq!((pr["source"].as_str(), pr["branch"].is_null(), pr["number"].as_u64()), (Some("merge-commit"), true, Some(8)));
    assert!(pr.get("commits").is_none(), "a merge, not a squash: {pr}");
    assert!(v["commits"].as_array().unwrap().iter().any(|c| c["sha"] == loose.to_string()), "{}", v["commits"]);

    let one = f.commit("refs/heads/feat/SV-29-work", "work", "one.rs", 1300, &[titled]);
    f.repo.reference("refs/heads/feat/SV-31-stacked", one, true, "stacked").unwrap();
    let merged = f.commit("refs/heads/main", "Land the work", "one.rs", 1400, &[titled, one]);
    f.repo.reference("refs/remotes/origin/main", merged, true, "merge").unwrap();
    let v = get(home.path()).await;
    assert!(v["prs"].as_array().unwrap().iter().all(|pr| pr["mergedSha"] != merged.to_string()), "{}", v["prs"]);
    assert_eq!(v["unavailable"]["prs"], crate::attribution::history::TIP_SHARED);
}

/// Review r6 L6 / r8: why_line's walk starts from the trail's names, so a merge of a
/// session-recorded branch that follows no naming convention is the story's mergedSha in
/// both. Red on the base: `default_rev` walked with no names and fell to the default branch.
#[tokio::test]
async fn why_line_walks_from_the_trails_branch_names() {
    let (home, mut store) = store();
    let f = GitStory::new();
    f.seed(home.path(), &mut store, "work-without-a-story-id", false);
    let tip = f.commit("refs/heads/work-without-a-story-id", "work", "w.rs", 1100, &[f.root]);
    let merged = f.commit("refs/heads/main", "Merge pull request #5 from acme/work-without-a-story-id", "w.rs", 1200, &[f.root, tip]);
    f.repo.reference("refs/remotes/origin/main", merged, true, "merge").unwrap();
    let v = get(home.path()).await;
    assert_eq!(v["prs"][0]["mergedSha"], merged.to_string(), "the trail links it");
    let source = crate::attribution::history::HistorySource::at(f.dir.path()).unwrap();
    let rev = source.default_rev(home.path(), Some("SV-29")).unwrap();
    assert_eq!(rev.oid(), merged, "why_line picks the same merge");
    assert_eq!(rev.source(), "mergedSha");
}

/// SV-CDE-102: with the frontmatter index built, the story file lookups of N story computes (the
/// task half's and the git half's) walk the org no time at all.
#[test]
fn sv_cde_102_story_files_resolve_through_the_index_with_no_org_walk() {
    let (d, s) = store();
    let root = d.path().join("users/1/orgs/acme");
    std::fs::create_dir_all(root.join("riverside/content")).unwrap();
    std::fs::write(root.join("org.yml"), "").unwrap();
    let ids: Vec<String> = (0..12).map(|n| format!("SV-WALK-{n}")).collect();
    for id in &ids {
        std::fs::write(
            root.join(format!("riverside/content/{id}.md")),
            format!("---\ntype: story\nstatus: todo\nstory_id: {id}\nsource_repo: acme/app\n---\n\n# {id}\n"),
        )
        .unwrap();
    }
    crate::install::frontmatter_cache::FrontmatterCache::index_into_global(&root);
    for _ in 0..3 {
        for id in &ids {
            let found = git::story_source(d.path(), &s, id, &CallerScope::Human).expect(id);
            assert!(found.path.ends_with(format!("{id}.md")), "{}", found.path.display());
            assert_eq!(found.org, "acme");
        }
    }
    assert_eq!(crate::session_usage::story::org_scans(&root), 0);
}

/// SV-CDE-103: the session refs the git half reads from the story's rows are the ones the full
/// session reads give, for an exact, an associated and another machine's session, and `collect`
/// reads the same branches, worktrees and repositories from either.
#[tokio::test]
async fn sv_cde_103_row_refs_match_the_full_session_reads() {
    let (home, mut store) = store();
    seed(&mut store, home.path(), "linked", true, true);
    let org = home.path().join("users/1/orgs/acme");
    std::fs::create_dir_all(org.join("riverside/content")).unwrap();
    std::fs::write(org.join("org.yml"), "").unwrap();
    std::fs::write(
        org.join("riverside/content/SV-29.md"),
        "---\ntype: story\nstatus: todo\nstory_id: SV-29\nsource_repo: acme/code\n---\n",
    )
    .unwrap();
    let mut named = event("named-e", "named", "gpt-6-sol", 1_790_000_000_000, Tokens { input: 5, ..Default::default() });
    named.cwd = Some(home.path().join("users/1/context-worktrees/acme/riverside/sv-29").to_string_lossy().into());
    let mut record = SessionRecord::new("named");
    record.branch = Some("feat/SV-29-named".into());
    record.worktree = Some(home.path().join("named-worktree").to_string_lossy().into());
    store.write_session_record(&record).unwrap();
    store.ingest(&[named]).unwrap();
    let mut record = SessionRecord::new("remote");
    record.story_id = Some("SV-29".into());
    record.branch = Some("feat/SV-29-remote".into());
    record.workspace = Some("/elsewhere/workspace".into());
    store.write_session_record(&record).unwrap();
    store
        .ingest(&[event("remote-e", "remote", "gpt-6-sol", 1_790_000_000_500, Tokens { input: 3, ..Default::default() })])
        .unwrap();
    store.conn.execute("UPDATE usage_event SET machine_id = 'm2' WHERE session_id = 'remote'", []).unwrap();

    let scope = CallerScope::Human;
    let report = story_report(&store, home.path(), "SV-29", &scope).unwrap();
    let full = read_sessions(home.path(), &scope, report.sessions.clone()).await.unwrap();
    let mut from_reads: Vec<git::SessionRef> = full.iter().map(git::SessionRef::of_read).collect();
    let rows: Vec<contribution::Row> = rows::rows_from_sessions(&store, &report.sessions)
        .unwrap()
        .into_iter()
        .map(|(source, contribution)| contribution::Row { source: (source.kind.to_owned(), source.id), contribution })
        .collect();
    let mut from_rows: Vec<git::SessionRef> = rows.iter().map(git::SessionRef::of_row).collect();
    from_reads.sort_by_key(|r| format!("{r:?}"));
    from_rows.sort_by_key(|r| format!("{r:?}"));
    assert_eq!(from_rows.len(), 3, "{from_rows:?}");
    assert!(from_rows.iter().any(|r| r.associated), "{from_rows:?}");
    assert_eq!(from_rows, from_reads);
    let facts = |refs: &[git::SessionRef]| {
        git::collect_refs(home.path(), &store, "SV-29", refs, &[CallerScope::Human], false, None).unwrap()
    };
    let (by_rows, by_reads) = (facts(&from_rows), facts(&from_reads));
    assert_eq!(by_rows.branches, by_reads.branches);
    assert_eq!(by_rows.worktrees, by_reads.worktrees);
    assert_eq!(by_rows.repos, by_reads.repos);
    let by_values = git::collect(home.path(), &store, "SV-29", &full, &[CallerScope::Human]).unwrap();
    assert_eq!(by_values.branches, by_reads.branches);
    assert_eq!(by_values.worktrees, by_reads.worktrees);
}

/// SV-CDE-105: a queued session source's contributions, read from that session's usage alone,
/// are the rows a full report gives that session, for an exact, an associated and another
/// machine's session.
#[test]
fn sv_cde_105_per_session_contributions_match_the_full_report() {
    let (home, mut store) = store();
    seed(&mut store, home.path(), "linked", true, true);
    let org = home.path().join("users/1/orgs/acme");
    std::fs::create_dir_all(org.join("riverside/content")).unwrap();
    std::fs::write(org.join("org.yml"), "").unwrap();
    std::fs::write(
        org.join("riverside/content/SV-29.md"),
        "---\ntype: story\nstatus: todo\nstory_id: SV-29\nsource_repo: acme/code\n---\n",
    )
    .unwrap();
    let mut named = event("named-e", "named", "gpt-6-sol", 1_790_000_000_000, Tokens { input: 5, ..Default::default() });
    named.cwd = Some(home.path().join("users/1/context-worktrees/acme/riverside/sv-29").to_string_lossy().into());
    store.ingest(&[named]).unwrap();
    let mut record = SessionRecord::new("remote");
    record.story_id = Some("SV-29".into());
    store.write_session_record(&record).unwrap();
    store
        .ingest(&[event("remote-e", "remote", "gpt-6-sol", 1_790_000_000_500, Tokens { input: 3, ..Default::default() })])
        .unwrap();
    store.conn.execute("UPDATE usage_event SET machine_id = 'm2' WHERE session_id = 'remote'", []).unwrap();

    let adapter = rows::StoryRows::new(home.path().to_owned());
    let full = store.report_with(&Filter::default(), &mut StoryResolver::for_home(home.path(), HashMap::new())).unwrap();
    let mut bases = vec![];
    for session in ["linked", "named", "remote"] {
        let queued = contribution::Source { kind: rows::SESSION, id: session.into() };
        let (source, per_session) = contribution::SourceResolver::contributions(&adapter, &store, &queued).unwrap();
        let mut by_story: BTreeMap<String, Vec<&crate::session_usage::SessionUsage>> = Default::default();
        for usage in full.sessions.iter().filter(|u| u.session_id == session) {
            if let Some(story) = &usage.story {
                by_story.entry(story.id.to_lowercase()).or_default().push(usage);
            }
        }
        let expected: Vec<(String, contribution::Contribution)> = by_story
            .into_iter()
            .map(|(story, usages)| (story, rows::session_row(&store, &source.id, &usages).unwrap()))
            .collect();
        assert_eq!(expected.len(), 1, "{session} belongs to SV-29");
        assert_eq!(per_session, expected, "{session}");
        bases.push(per_session[0].1.extra["basis"].as_str().unwrap().to_owned());
    }
    assert_eq!(bases, ["exact", "associated", "exact"]);
}


/// SV-CDE-106: a batch of story computes lists the team's review-rounds collection once, and
/// each story's markers are those a per-story read finds.
#[tokio::test]
async fn a_batch_lists_the_review_rounds_collection_once() {
    let (dir, _s) = store();
    team_markers(dir.path(), "DEMO-1");
    let name = dir.path().file_name().unwrap().to_owned();
    let teams: BTreeSet<(String, String)> = [("acme".to_owned(), "riverside".to_owned())].into();
    let batch = Batch::new();
    let before = review_rounds::listed(&name);
    for story in ["SV-29", "SV-30", "SV-31", "SV-32"] {
        let alone =
            review_rounds::story_markers(dir.path(), &teams, story, &CallerScope::Human).unwrap();
        let shared = review_rounds::story_markers_from(dir.path(), &teams, story, &mut |root| {
            batch.review_records(root, &CallerScope::Human)
        })
        .unwrap();
        assert_eq!(alone, shared, "{story}: the same markers and rows");
    }
    // Four per-story reads and one batch read.
    assert_eq!(review_rounds::listed(&name) - before, 4 + 1);
}

/// SV-CDE-106: a round recorded after the batch read is counted once the batch forgets its rounds
/// (a review record marked the story), and anyway once the read is older than the retake gap.
#[tokio::test]
async fn a_round_recorded_after_the_batch_read_is_counted_on_the_recompute() {
    let (dir, _s) = store();
    let rounds = team_markers(dir.path(), "DEMO-1");
    let teams: BTreeSet<(String, String)> = [("acme".to_owned(), "riverside".to_owned())].into();
    let batch = Batch::new();
    let count = |batch: &Batch| {
        review_rounds::story_markers_from(dir.path(), &teams, "SV-29", &mut |root| {
            batch.review_records(root, &CallerScope::Human)
        })
        .unwrap()
        .0
        .len()
    };
    let before = count(&batch);
    // Another complete round of SV-29, copied from one.
    let first = rounds.join("demo1-101-r1.md");
    let raw = std::fs::read_to_string(&first).unwrap();
    std::fs::write(rounds.join("late-r1.md"), raw.clone()).unwrap();
    let fresh = review_rounds::story_markers(dir.path(), &teams, "SV-29", &CallerScope::Human)
        .unwrap()
        .0
        .len();
    assert!(fresh > before, "the new round is a marker of its own");
    assert_eq!(count(&batch), before, "within the gap the batch's read stands");
    batch.forget_rounds();
    assert_eq!(count(&batch), fresh, "a review mark makes the next compute read again");

    let again = Batch::new();
    assert_eq!(count(&again), fresh);
    std::fs::write(rounds.join("later-r1.md"), raw).unwrap();
    assert_eq!(count(&again), fresh);
    again.age(RETAKE_GAP_MS + 1);
    assert!(count(&again) > fresh, "a read older than the retake gap is taken again");
}
