// SPDX-License-Identifier: MIT
//! Explicit, ignored measurement command. Uses a disposable store/home and the
//! shipped why_line resolver; never opens the real store for writing or drains git-ai.
use super::{authorship::Authorship, turns, why_line};
use crate::context_repo::caller_scope::CallerScope;
use crate::domain::Principal;
use serde::Serialize;
use std::path::Path;

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct Counts {
    agent_commits: usize,
    files_queried: usize,
    measured_agent_ranges: usize,
    log_unavailable: usize,
    trace_missing: usize,
    ambiguous: usize,
    unlinked_s_ranges: usize,
    linked_turn_ranges: usize,
    commit_turn_ranges: usize,
    unavailable_queries: usize,
}
fn add(counts: &mut Counts, answer: &serde_json::Value, rev: &str) {
    if answer["status"] == "unavailable" {
        counts.unavailable_queries += 1;
    }
    for range in answer["ranges"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| turns::exact_agent_range(r) && r["commit"] == rev)
    {
        counts.measured_agent_ranges += 1;
        match range["unavailable"]["turn"].as_str() {
            Some(turns::LOG_UNAVAILABLE) => counts.log_unavailable += 1,
            Some(turns::TRACE_MISSING) => counts.trace_missing += 1,
            Some(turns::AMBIGUOUS) => counts.ambiguous += 1,
            _ => {}
        }
        if range["nativeSessionId"]
            .as_str()
            .is_some_and(|s| s.starts_with("s_"))
            && range["link"] == "unlinked"
        {
            counts.unlinked_s_ranges += 1;
        }
        match serde_json::Value::as_str(&range["turn"]["basis"]) {
            Some(turns::BASIS_COMMIT) => counts.commit_turn_ranges += 1,
            _ if range["turn"].is_object() => counts.linked_turn_ranges += 1,
            _ => {}
        }
    }
}
fn configure(home: &Path, root: &Path) -> String {
    let attachment = crate::commands::attachments::existing_checkout_attachment(root).unwrap();
    let key = crate::commands::attachments::attachment_key(&attachment).unwrap();
    let mut parts = key.rsplit('/');
    let repo = parts.next().unwrap();
    let owner = parts.next().unwrap();
    let principal = Principal::from(1);
    let org = crate::context_repo::path_to_org_root(home, &principal, "measurement").unwrap();
    std::fs::create_dir_all(&org).unwrap();
    std::fs::write(
        org.join("org.yml"),
        "slug: measurement\ndisplay_name: Measurement\ncreated_at: \"2026-01-01T00:00:00Z\"\n",
    )
    .unwrap();
    std::fs::write(org.join("people.yml"), "people: []\n").unwrap();
    std::fs::write(org.join("teams.yml"),format!("teams:\n  - name: measure\n    source_repos:\n      - owner: {owner}\n        repo: {repo}\n")).unwrap();
    git2::Repository::init(&org).unwrap();
    let path = crate::context_repo::path_to_user_attachments_file(home, &principal);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    crate::context_repo::attachments_io::write_attachments_atomic(
        &path,
        &crate::domain::Attachments {
            attachments: vec![attachment],
        },
    )
    .unwrap();
    key
}

#[tokio::test]
#[ignore = "explicit aggregate-only command; requires immutable store snapshot and own linked checkout"]
async fn measure_checkpoint_null_split() {
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name}"));
    let root = Path::new(&var("CDE_MEASURE_REPO")).canonicalize().unwrap();
    assert_ne!(root, dirs::home_dir().unwrap().join("codesense"));
    let observed_ms = var("CDE_MEASURE_UNTIL_MS").parse::<i64>().unwrap();
    let result = measure(Path::new(&var("CDE_MEASURE_SNAPSHOT")), &root, observed_ms).await;
    std::fs::write(
        var("CDE_MEASURE_OUTPUT"),
        format!("{}\n", serde_json::to_string_pretty(&result).unwrap()),
    )
    .unwrap();
}

/// Smoke run on a tiny fixture so the manager's measurement is not the helper's first run.
#[tokio::test]
async fn measure_checkpoint_null_split_fixture_smoke() {
    let repo = super::turns::tests::replay_repo(true);
    super::git_ai::tests::git(
        repo.path(),
        &["remote", "add", "origin", "https://github.com/acme/api.git"],
    );
    let input = tempfile::tempdir().unwrap();
    let snapshot = input.path().join("snapshot.sqlite");
    drop(crate::session_usage::UsageStore::open(&snapshot).unwrap());
    let head_seconds: i64 = super::git_ai::tests::git(repo.path(), &["log", "-1", "--format=%ct"])
        .trim()
        .parse()
        .unwrap();
    let result = measure(&snapshot, repo.path(), head_seconds * 1000).await;
    assert_eq!(result["schemaVersion"], 2);
    assert_eq!(result["repository"], "github.com/acme/api");
    assert!(result["notesStable"].as_bool().unwrap());
    assert!(result["counts"]["agentCommits"].as_u64().unwrap() >= 1);
    assert!(result["counts"]["measuredAgentRanges"].as_u64().unwrap() > 0);
}

async fn measure(snapshot: &Path, root: &Path, observed_ms: i64) -> serde_json::Value {
    let until = observed_ms / 1000;
    let home = tempfile::tempdir().unwrap();
    let store = home.path().join("usage.sqlite");
    std::fs::copy(snapshot, &store).unwrap();
    let key = configure(home.path(), root);
    // No managed binary here: GitAiSource::for_home cannot await a real daemon.
    assert!(!home.path().join("bin/git-ai").exists());
    let conversations = home.path().join("session-conversations");
    let repo = git2::Repository::open(root).unwrap();
    let notes = repo
        .find_reference("refs/notes/ai")
        .ok()
        .and_then(|r| r.target())
        .map(|o| o.to_string());
    let mut walk = repo.revwalk().unwrap();
    walk.push_head().unwrap();
    walk.set_sorting(git2::Sort::TIME).unwrap();
    let mut counts = Counts::default();
    for oid in walk {
        let commit = repo.find_commit(oid.unwrap()).unwrap();
        if commit.time().seconds() < until - 7 * 86400 {
            continue;
        }
        if commit.time().seconds() > until {
            continue;
        }
        let Ok(note) = repo.find_note(Some("refs/notes/ai"), commit.id()) else {
            continue;
        };
        let Ok(note) = Authorship::parse(note.message().unwrap_or("")) else {
            continue;
        };
        if note.session_ids().is_empty() {
            continue;
        }
        counts.agent_commits += 1;
        let tree = commit.tree().unwrap();
        for path in note.attested_files() {
            let Ok(entry) = tree.get_path(Path::new(path)) else {
                continue;
            };
            let Ok(blob) = repo.find_blob(entry.id()) else {
                continue;
            };
            let lines = blob.content().split_inclusive(|b| *b == b'\n').count() as u32;
            let mut intervals: Vec<(u32, u32)> = Vec::new();
            for line in 1..=lines {
                if matches!(
                    note.origin_of_line(path, line),
                    Some(super::authorship::Origin::Agent(..))
                ) {
                    match intervals.last_mut() {
                        Some((_, end)) if *end + 1 == line => *end = line,
                        _ => intervals.push((line, line)),
                    }
                }
            }
            if intervals.is_empty() {
                continue;
            }
            counts.files_queried += 1;
            for (first, last) in intervals {
                for start in (first..=last).step_by(2001) {
                    let params = why_line::Params {
                        repo: key.clone(),
                        rev: Some(commit.id().to_string()),
                        story: None,
                        path: path.into(),
                        start,
                        end: (start + 2000).min(last),
                    };
                    let answer = why_line::resolve_at(
                        home.path(),
                        &Principal::from(1),
                        &store,
                        &conversations,
                        &params,
                        &CallerScope::Human,
                        Default::default(),
                    )
                    .await
                    .unwrap();
                    add(&mut counts, &answer, params.rev.as_deref().unwrap());
                }
            }
        }
        if counts.agent_commits % 10 == 0 {
            eprintln!(
                "MEASURE_PROGRESS agentCommits={} filesQueried={} measuredAgentRanges={}",
                counts.agent_commits, counts.files_queried, counts.measured_agent_ranges
            );
        }
    }
    let final_notes = repo
        .find_reference("refs/notes/ai")
        .ok()
        .and_then(|r| r.target())
        .map(|o| o.to_string());
    serde_json::json!({"schemaVersion":2,"queryScope":"Contiguous attested agent intervals selected by existing Authorship::origin_of_line, clipped to <=2001 lines per existing why_line query; resolver classification unchanged","baselineMode":"existing-store-plus-on-demand","startupCatchUpSimulated":false,"observedAtMs":observed_ms,"baselineLimitation":"Snapshot may predate PR-A rollout. Startup capture and actual capture preferences are not simulated; this does not isolate watcher benefit from startup rollout.","repository":key,"revision":repo.head().unwrap().target().unwrap().to_string(),"untilEpochSeconds":until,"sinceEpochSeconds":until-7*86400,"notesAvailable":notes.is_some(),"notesStable":notes==final_notes,"denominator":"measuredAgentRanges are exact-agent ranges attributed to each sampled commit at that commit, not commit totals; unlinked s_ is an independent range count","conversationText":"not copied; turn linkage uses indexed metadata in snapshot, no conversation text read","counts":counts})
}
