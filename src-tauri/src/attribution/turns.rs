// SPDX-License-Identifier: MIT
//! Live indexed annotations. Checkpoint and conversation state never enter the raw Git cache.
use super::ranges::HUMAN_TOOL;
use crate::domain::Principal;
use crate::session_conversation::{Availability, IndexedPointer};
use crate::session_usage::UsageStore;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
pub(crate) const LOG_UNAVAILABLE: &str =
    "checkpoint log not available (pruned, another machine, or before capture)";
pub(crate) const TRACE_MISSING: &str = "edit not recorded by git-ai";
pub(crate) const AMBIGUOUS: &str = "ambiguous turn link";
pub(crate) const CONVERSATION_NOT_CAPTURED: &str = "conversation not captured";
pub(crate) const CONVERSATION_EXPIRED: &str = "conversation expired";
pub(crate) const CONVERSATION_TEXT_TRUNCATED: &str = "conversation text truncated";
/// `turn.basis` of a pointer that comes from the recorded edit.
pub(crate) const BASIS_EDIT: &str = "edit";
/// `turn.basis` of the turn the commit was made in, when the edit turn is unknown.
pub(crate) const BASIS_COMMIT: &str = "commit";
/// Tool names of a call that can run `git commit`, as the indexed conversations name them.
const SHELL_TOOLS: [&str; 4] = ["Bash", "exec", "exec_command", "shell"];
/// A commit runs inside its shell call, so a call older than this is another actor's commit
/// (a human, or another session) rather than this turn's; long test-then-commit calls still fit.
const COMMIT_TURN_MAX_GAP_MS: i64 = 600_000;
pub(super) fn exact_agent_range(range: &Value) -> bool {
    range["basis"] == "exact"
        && range["tool"]
            .as_str()
            .is_some_and(|tool| tool != HUMAN_TOOL)
}
fn eligible(range: &Value) -> bool {
    exact_agent_range(range) && range["link"] == "linked"
}
pub(crate) fn join<F>(
    home: &Path,
    principal: &Principal,
    store_path: &Path,
    name: &str,
    value: &mut Value,
    mut lookup: F,
) -> Result<(), String>
where
    F: FnMut(&UsageStore, &str, &str) -> Result<IndexedPointer, String>,
{
    let Some(ranges) = value.get_mut("ranges").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    if !ranges.iter().any(eligible) {
        for range in ranges {
            range
                .as_object_mut()
                .expect("range object")
                .remove("traceId");
        }
        return Ok(());
    }
    let context = super::why_line::configured_repo(home, principal, name)?.filter(|r| {
        r.attachment == crate::mcp::setup_tools::AttachmentStatus::Attached
            && r.local_path.is_some()
    });
    let store = UsageStore::open(store_path)?;
    let mut pointers = HashMap::<(String, String), Value>::new();
    let mut coverage = HashMap::<String, bool>::new();
    let mut commit_turns = HashMap::<(String, String), Option<Value>>::new();
    for range in ranges {
        let trace = range
            .as_object_mut()
            .expect("range object")
            .remove("traceId")
            .and_then(|v| v.as_str().map(str::to_owned));
        if !eligible(range) {
            continue;
        }
        let cli = range["tool"].as_str().unwrap();
        let native = crate::session_usage::session_read::resolve_session(
            &store,
            Some(cli),
            range["nativeSessionId"].as_str().unwrap_or(""),
        )?
        .native_session_id;
        let link = match trace.as_deref() {
            Some(trace) => store.checkpoint_link(cli, &native, trace)?,
            None => None,
        };
        let ambiguous = link.as_ref().is_some_and(|link| link.ambiguous);
        if let Some(link) = link.filter(|link| !link.ambiguous) {
            let session = range["sessionId"].as_str().unwrap().to_owned();
            let key = (session.clone(), link.tool_use_id.clone());
            if !pointers.contains_key(&key) {
                let value =
                    turn_object(&store, &mut lookup, &session, &link.tool_use_id, BASIS_EDIT)?;
                pointers.insert(key.clone(), value);
            }
            range["turn"] = pointers[&key].clone();
        } else {
            let commit = Value::as_str(&range["commit"]).unwrap_or("").to_owned();
            let reason = if ambiguous {
                AMBIGUOUS
            } else {
                if !coverage.contains_key(&commit) {
                    let covered = if let Some(repo) = &context {
                        super::checkpoints::covered(
                            Path::new(repo.local_path.as_deref().unwrap()),
                            &repo.remote_key,
                            &commit,
                            &store,
                        )?
                    } else {
                        false
                    };
                    coverage.insert(commit.clone(), covered);
                }
                if coverage[&commit] {
                    TRACE_MISSING
                } else {
                    LOG_UNAVAILABLE
                }
            };
            // The edit turn is unknown; where a log was seen the commit's own turn may still be
            // told, labelled as such and only on evidence (see `commit_turn`).
            range["turn"] = Value::Null;
            if reason == TRACE_MISSING {
                let session = Value::as_str(&range["sessionId"]).unwrap().to_owned();
                let key = (session, commit);
                if !commit_turns.contains_key(&key) {
                    let turn = match &context {
                        Some(repo) => commit_turn(
                            &store,
                            &mut lookup,
                            &key.0,
                            Path::new(repo.local_path.as_deref().unwrap()),
                            &key.1,
                        )?,
                        None => None,
                    };
                    commit_turns.insert(key.clone(), turn);
                }
                if let Some(turn) = &commit_turns[&key] {
                    range["turn"] = turn.clone();
                }
            }
            range["unavailable"] = json!({"turn":reason});
        }
    }
    Ok(())
}
/// The `turn` object for the call `tool_use_id` of `session`, resolved through the conversation
/// index: its zero-based index and the reason it is null or only partly known, with the call's task.
fn turn_object<F>(
    store: &UsageStore,
    lookup: &mut F,
    session: &str,
    tool_use_id: &str,
    basis: &str,
) -> Result<Value, String>
where
    F: FnMut(&UsageStore, &str, &str) -> Result<IndexedPointer, String>,
{
    let pointer = lookup(store, session, tool_use_id)?;
    let index = match pointer.state {
        Availability::Present | Availability::Truncated | Availability::Expired => {
            pointer.turn_index
        }
        _ => None,
    };
    let reason = if index.is_none() {
        Some(CONVERSATION_NOT_CAPTURED)
    } else if pointer.state == Availability::Expired {
        Some(CONVERSATION_EXPIRED)
    } else if pointer.text_truncated {
        Some(CONVERSATION_TEXT_TRUNCATED)
    } else {
        None
    };
    let (task_id, task_basis) = store.task_of_call(session, pointer.ts_ms)?;
    let mut value = json!({"sessionId":pointer.session_id,"toolUseId":tool_use_id,"turn":index,"basis":basis,"taskId":task_id,"taskBasis":task_basis});
    if let Some(reason) = reason {
        value["reason"] = json!(reason);
    }
    Ok(value)
}
/// The committer time of `commit` in `repo`, read in-process, as the last millisecond of its second
/// (Git records whole seconds, and a call that ran in the same second preceded the commit).
fn committer_ms(repo: &Path, commit: &str) -> Option<i64> {
    let oid = Result::ok(git2::Oid::from_str(commit))?;
    let repository = Result::ok(git2::Repository::open(repo))?;
    let found = Result::ok(repository.find_commit(oid))?;
    let seconds = found.committer().when().seconds();
    Some(seconds * 1000 + 999)
}
/// The turn `commit` was made in, for a line whose edit turn is unknown: the last indexed
/// shell-type call of the linked session at or before the committer time, and only when that time
/// lies inside the session's observed span. Any other case (another session committed the work,
/// no shell call before it, no timing or conversation index) is `None`, never a guess.
fn commit_turn<F>(
    store: &UsageStore,
    lookup: &mut F,
    session: &str,
    repo: &Path,
    commit: &str,
) -> Result<Option<Value>, String>
where
    F: FnMut(&UsageStore, &str, &str) -> Result<IndexedPointer, String>,
{
    let Some(at_ms) = committer_ms(repo, commit) else {
        return Ok(None);
    };
    let Some(timing) = store.read_session_timing(session)? else {
        return Ok(None);
    };
    if at_ms < timing.started_at_ms || timing.ended_at_ms.is_some_and(|end| at_ms > end) {
        return Ok(None);
    }
    let call = store
        .read_tool_calls(session)?
        .into_iter()
        .filter(|call| {
            SHELL_TOOLS.iter().any(|tool| call.name == *tool)
                && call
                    .ts_ms
                    .is_some_and(|ts| ts <= at_ms && at_ms - ts <= COMMIT_TURN_MAX_GAP_MS)
        })
        .max_by_key(|call| (call.turn_index, call.seq));
    let Some(tool_use_id) = call.and_then(|call| call.native_id) else {
        return Ok(None);
    };
    turn_object(store, lookup, session, &tool_use_id, BASIS_COMMIT).map(Some)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::attribution::git_ai::{
        tests::{git, rebuild},
        GitAiSource,
    };
    use crate::attribution::ranges::AttributionSource;
    use crate::attribution::why_line::{self, tests::attach};
    use crate::context_repo::caller_scope::CallerScope;
    use crate::domain::Principal;
    use crate::session_usage::{record::SessionRecord, UsageStore};
    use serde_json::{json, Value};
    use std::path::{Path, PathBuf};
    pub(crate) fn params(path: &str) -> why_line::Params {
        why_line::Params {
            repo: "acme/api".into(),
            rev: Some("HEAD".into()),
            story: None,
            path: path.into(),
            start: 1,
            end: 3,
        }
    }
    pub(crate) fn native(repo: &Path, path: &str) -> (String, String) {
        let r = GitAiSource::default()
            .ranges(repo, "HEAD", path)
            .unwrap()
            .remove(0);
        (r.tool.unwrap(), r.native_session_id.unwrap())
    }
    pub(crate) fn record(home: &Path, repo: &Path, path: &str, session: &str) {
        let (cli, native) = native(repo, path);
        let store = UsageStore::open(&home.join("usage.sqlite")).unwrap();
        let mut r = store
            .read_session_record(session)
            .unwrap()
            .unwrap_or_else(|| SessionRecord::new(session));
        r.cli = Some(cli);
        r.native_session_id = Some(native);
        store.write_session_record(&r).unwrap();
    }
    /// The committer time of HEAD in epoch milliseconds (a whole second).
    fn head_commit_ms(repo: &Path) -> i64 {
        git(repo, &["log", "-1", "--format=%ct"])
            .trim()
            .parse::<i64>()
            .unwrap()
            * 1000
    }
    /// Give `session` an observed span and an indexed conversation around HEAD's commit time:
    /// `span` is the start and end in seconds relative to it, each call is
    /// (turn, tool name, native id, seconds relative to it).
    fn seed_commit_session(
        home: &Path,
        repo: &Path,
        session: &str,
        (from, to): (i64, i64),
        calls: &[(u32, &str, &str, i64)],
    ) {
        use crate::session_usage::{
            conversation::{ConversationMeta, IndexedCall, State},
            timing::{Signal, Stamped, TimingState},
        };
        use codesense_pty_core::activity::Event;
        let committed = head_commit_ms(repo);
        let mut store = UsageStore::open(&home.join("usage.sqlite")).unwrap();
        let mut timing = TimingState::default();
        for (at, event) in [(from, Event::Submitted), (to, Event::Ended)] {
            let stamp = Stamped {
                session_id: session.into(),
                at_ms: committed + at * 1000,
                signal: Signal::Activity(event, "test"),
            };
            timing.apply(&store, &stamp).unwrap();
        }
        let indexed: Vec<IndexedCall> = calls
            .iter()
            .map(|(turn, tool, id, at)| IndexedCall {
                turn_index: *turn,
                seq: 0,
                native_id: Some((*id).into()),
                name: (*tool).into(),
                mcp_server: None,
                is_error: None,
                ts_ms: Some(committed + at * 1000),
                skill: None,
                aliases: Vec::new(),
            })
            .collect();
        let meta = ConversationMeta {
            session_id: session.into(),
            state: State::Present,
            source_format: None,
            bytes: 0,
            turn_count: 8,
            truncated: None,
            captured_at_ms: 1,
            unavailable: Default::default(),
        };
        store.write_conversation(&meta, &indexed).unwrap();
    }
    /// Adapt identities to pair existing engine notes with native conversation fixtures.
    /// The imported scripted-real-engine evidence files themselves remain unchanged.
    pub(crate) fn checkpoint_log(repo: &Path, path: &str, tools: &[&str]) -> PathBuf {
        let ranges = GitAiSource::default().ranges(repo, "HEAD", path).unwrap();
        let mut text = String::new();
        for (i, r) in ranges.iter().enumerate() {
            let source = if r.tool.as_deref() == Some("claude") {
                include_str!("testdata/git_ai_1_7_5_checkpoints/live-claude.jsonl")
            } else {
                include_str!("testdata/git_ai_1_7_5_checkpoints/archived-codex.jsonl")
            };
            let mut v: Value = serde_json::from_str(source.lines().next().unwrap()).unwrap();
            v["agent_id"]["id"] = json!(r.native_session_id);
            v["trace_id"] = json!(r.trace_id);
            v["agent_metadata"]["tool_use_id"] = json!(tools[i.min(tools.len() - 1)]);
            text.push_str(&v.to_string());
            text.push('\n');
        }
        let label = git(repo, &["rev-parse", "HEAD^"]);
        let p = repo
            .join(".git/ai/working_logs")
            .join(format!("old-{}", label.trim()))
            .join("checkpoints.jsonl");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        {
            use std::io::Write;
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&p)
                .unwrap()
                .write_all(text.as_bytes())
                .unwrap();
        }
        p
    }
    pub(crate) fn capture(
        home: &Path,
        dir: &Path,
        session: &str,
        body: Option<&str>,
        policy: &crate::session_conversation::Policy,
    ) {
        use crate::session_conversation as conv;
        let store_path = home.join("usage.sqlite");
        let store = UsageStore::open(&store_path).unwrap();
        let mut r = store.read_session_record(session).unwrap().unwrap();
        let cli = r.cli.as_deref().unwrap();
        let native = r.native_session_id.as_deref().unwrap();
        let account = home.join("turn-fixture").join(session);
        let (file, text) = if cli == "claude" {
            (
                account
                    .join("projects")
                    .join(
                        r.worktree
                            .as_deref()
                            .map(|p| p.replace('/', "-"))
                            .unwrap_or_else(|| "-work-wt".into()),
                    )
                    .join(format!("{native}.jsonl")),
                body.unwrap_or(conv::CLAUDE_FIXTURE)
                    .replace(conv::CLAUDE_ID, native),
            )
        } else {
            (
                account
                    .join("sessions/2026/10/27")
                    .join(format!("rollout-2026-10-27T10-00-00-{native}.jsonl")),
                body.unwrap_or(conv::CODEX_FIXTURE)
                    .replace(conv::CODEX_ID, native),
            )
        };
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
        r.config_home = Some(account.to_string_lossy().into_owned());
        store.write_session_record(&r).unwrap();
        drop(store);
        let env = |key: &str| (key == "HOME").then(|| home.to_owned().into_os_string());
        assert!(matches!(
            conv::capture_in(&store_path, dir, &env, session, 1000, policy).unwrap(),
            conv::Captured::Stored(_)
        ));
    }
    pub(crate) async fn query(home: &Path, path: &str) -> Value {
        why_line::resolve_at(
            home,
            &Principal::from(1),
            &home.join("usage.sqlite"),
            &home.join("session-conversations"),
            &params(path),
            &CallerScope::Human,
            Default::default(),
        )
        .await
        .unwrap()
    }
    const REPLAY_NATIVE: &str = "c1a0de00-0000-4000-a000-000000000101";
    const REPLAY_KEY: &str = "s_5cef34e3292d7d";
    pub(crate) fn replay_repo(fallback: bool) -> tempfile::TempDir {
        let repo = rebuild();
        std::fs::write(repo.path().join("edited.txt"), "after\n").unwrap();
        std::fs::write(
            repo.path().join("written.txt"),
            "written by Claude fixture\n",
        )
        .unwrap();
        git(repo.path(), &["add", "edited.txt", "written.txt"]);
        git(repo.path(), &["commit", "-m", "Claude replay"]);
        let mut note =
            include_str!("testdata/git_ai_1_7_5_claude_session/native-note.txt").to_owned();
        if fallback {
            note = note.replace(REPLAY_NATIVE, REPLAY_KEY);
        }
        git(
            repo.path(),
            &["notes", "--ref=ai", "add", "-m", &note, "HEAD"],
        );
        repo
    }
    pub(crate) fn replay_log(repo: &Path) -> PathBuf {
        let label = git(repo, &["rev-parse", "HEAD^"]);
        let path = repo
            .join(".git/ai/working_logs")
            .join(format!("old-{}", label.trim()))
            .join("checkpoints.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            include_str!("testdata/git_ai_1_7_5_claude_session/checkpoints.jsonl"),
        )
        .unwrap();
        path
    }
    #[tokio::test]
    async fn claude_fallback_links_edit_write_model_cost_and_turns_before_or_after_cached_query() {
        for late in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let repo = replay_repo(true);
            attach(home.path(), repo.path());
            let mut store = UsageStore::open(&home.path().join("usage.sqlite")).unwrap();
            let mut r = SessionRecord::new("replay-session");
            r.cli = Some("claude".into());
            r.native_session_id = Some(REPLAY_NATIVE.into());
            store.write_session_record(&r).unwrap();
            let mut e = crate::session_usage::tests::event(
                "replay-usage",
                "replay-session",
                "claude-sonnet-5-5",
                1_790_000_000_000,
                crate::session_usage::event::Tokens {
                    input: 100,
                    ..Default::default()
                },
            );
            e.reported_cost_usd = Some(2.0);
            store.ingest(&[e]).unwrap();
            if late {
                let missing = query(home.path(), "edited.txt").await;
                assert_eq!(missing["ranges"][0]["link"], "unlinked");
                assert_eq!(missing["ranges"][0]["nativeSessionId"], REPLAY_KEY);
            }
            let log = replay_log(repo.path());
            for (path, tool) in [
                ("edited.txt", "toolu_01CDE55Edit"),
                ("written.txt", "toolu_01CDE55Write"),
            ] {
                let answer = query(home.path(), path).await;
                assert_eq!(answer["ranges"][0]["basis"], "exact");
                assert_eq!(answer["ranges"][0]["link"], "linked");
                assert_eq!(answer["ranges"][0]["sessionId"], "replay-session");
                assert_eq!(answer["ranges"][0]["nativeSessionId"], REPLAY_NATIVE);
                assert_eq!(answer["ranges"][0]["turn"]["toolUseId"], tool);
                assert_eq!(
                    answer["sessions"]["replay-session"]["models"][0]["model"],
                    "claude-sonnet-5-5"
                );
                assert_eq!(
                    answer["sessions"]["replay-session"]["cost"]["totalUsd"],
                    2.0
                );
            }
            drop(store);
            std::fs::remove_file(log).unwrap();
            assert_eq!(
                query(home.path(), "edited.txt").await["ranges"][0]["sessionId"],
                "replay-session"
            );
        }
    }
    #[tokio::test]
    async fn original_pruned_claude_fallback_stays_unlinked_and_codex_still_links() {
        let home = tempfile::tempdir().unwrap();
        let repo = replay_repo(false);
        attach(home.path(), repo.path());
        let note = include_str!("testdata/git_ai_1_7_5_claude_session/original-failing-note.txt")
            .replace(
                "src-tauri/src/commands/dashboard_launch/persistence.rs",
                "edited.txt",
            )
            .replace("303-340", "1");
        git(
            repo.path(),
            &["notes", "--ref=ai", "add", "-f", "-m", &note, "HEAD"],
        );
        let answer = query(home.path(), "edited.txt").await;
        assert_eq!(answer["ranges"][0]["basis"], "exact");
        assert_eq!(answer["ranges"][0]["link"], "unlinked");
        assert_eq!(answer["ranges"][0]["nativeSessionId"], "s_c68e0980509078");
        record(home.path(), repo.path(), "b.txt", "codex-session");
        assert_eq!(
            query(home.path(), "b.txt").await["ranges"][0]["sessionId"],
            "codex-session"
        );
    }
    #[tokio::test]
    async fn captured_native_claude_note_links_without_checkpoint_mapping() {
        let home = tempfile::tempdir().unwrap();
        let repo = replay_repo(false);
        attach(home.path(), repo.path());
        record(home.path(), repo.path(), "edited.txt", "native-session");
        assert_eq!(
            query(home.path(), "edited.txt").await["ranges"][0]["sessionId"],
            "native-session"
        );
    }

    #[tokio::test]
    async fn real_claude_codex_pointers_in_both_record_orders_survive_log_loss_and_expiry() {
        for (path, tool) in [("a.txt", "toolu_01CCC"), ("b.txt", "exec-0001")] {
            for record_first in [false, true] {
                let home = tempfile::tempdir().unwrap();
                let repo = rebuild();
                attach(home.path(), repo.path());
                if record_first {
                    record(home.path(), repo.path(), path, "turn-session");
                }
                let warm = query(home.path(), path).await;
                assert!(home.path().join("caches/why-line.sqlite3").exists());
                let log = checkpoint_log(repo.path(), path, &[tool]);
                if !record_first {
                    record(home.path(), repo.path(), path, "turn-session");
                }
                let pending = query(home.path(), path).await;
                assert_eq!(pending["ranges"][0]["turn"]["toolUseId"], tool);
                assert_eq!(pending["ranges"][0]["turn"]["turn"], Value::Null);
                assert_eq!(
                    pending["ranges"][0]["turn"]["reason"],
                    CONVERSATION_NOT_CAPTURED
                );
                capture(
                    home.path(),
                    &home.path().join("session-conversations"),
                    "turn-session",
                    None,
                    &Default::default(),
                );
                let store = UsageStore::open(&home.path().join("usage.sqlite")).unwrap();
                let index = store
                    .find_tool_call("turn-session", tool)
                    .unwrap()
                    .unwrap()
                    .turn_index;
                let linked = query(home.path(), path).await;
                let pointer = &linked["ranges"][0]["turn"];
                assert_eq!(
                    (
                        &pointer["sessionId"],
                        &pointer["toolUseId"],
                        &pointer["turn"],
                        &pointer["basis"]
                    ),
                    (
                        &json!("turn-session"),
                        &json!(tool),
                        &json!(index),
                        &json!("edit")
                    )
                );
                assert!(pointer.get("reason").is_none());
                // Row 12: no sub-task, so the session's own task answers.
                assert_eq!(
                    (&pointer["taskId"], &pointer["taskBasis"]),
                    (&json!("turn-session"), &json!("session"))
                );
                assert_eq!(linked["notesRef"], warm["notesRef"]);
                assert!(linked["ranges"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|r| r.get("traceId").is_none()));
                std::fs::remove_file(log).unwrap();
                assert_eq!(
                    query(home.path(), path).await["ranges"][0]["turn"],
                    *pointer
                );
                store.mark_conversation_expired("turn-session").unwrap();
                drop(store);
                let expired = query(home.path(), path).await;
                assert_eq!(expired["ranges"][0]["turn"]["turn"], index);
                assert_eq!(expired["ranges"][0]["turn"]["reason"], CONVERSATION_EXPIRED);
            }
        }
    }
    #[tokio::test]
    async fn missing_unrelated_missing_trace_and_ambiguous_are_distinct() {
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        record(home.path(), repo.path(), "a.txt", "turn-session");
        assert_eq!(
            query(home.path(), "a.txt").await["ranges"][0]["unavailable"]["turn"],
            LOG_UNAVAILABLE
        );
        let unrelated = repo
            .path()
            .join(".git/ai/working_logs")
            .join(git(repo.path(), &["rev-parse", "HEAD"]).trim())
            .join("checkpoints.jsonl");
        std::fs::create_dir_all(unrelated.parent().unwrap()).unwrap();
        std::fs::write(unrelated, "").unwrap();
        assert_eq!(
            query(home.path(), "a.txt").await["ranges"][0]["unavailable"]["turn"],
            LOG_UNAVAILABLE
        );
        let log = checkpoint_log(repo.path(), "a.txt", &["toolu_01CCC"]);
        std::fs::write(&log, "").unwrap();
        assert_eq!(
            query(home.path(), "a.txt").await["ranges"][0]["unavailable"]["turn"],
            TRACE_MISSING
        );
        checkpoint_log(repo.path(), "a.txt", &["toolu_01CCC"]);
        query(home.path(), "a.txt").await;
        checkpoint_log(repo.path(), "a.txt", &["toolu_conflicting"]);
        let ambiguous = query(home.path(), "a.txt").await;
        assert_eq!(ambiguous["ranges"][0]["turn"], Value::Null);
        assert_eq!(ambiguous["ranges"][0]["unavailable"]["turn"], AMBIGUOUS);
    }
    /// Case rows 2, 5, 6, 7, 8, 17. Fails on main: a miss never carries a turn, so rows 2 and 5 stay
    /// `turn: null` however close the session's shell call to the commit.
    #[tokio::test]
    async fn commit_turn_is_labelled_guarded_and_only_where_a_log_was_seen() {
        // (name, what was seen for the commit's base: "none", an archive "marker" with no log, or a
        // "log" that holds no record of the edit; session span relative to the commit in seconds,
        // calls as (turn, tool, seconds), expected turn)
        type Case = (
            &'static str,
            &'static str,
            (i64, i64),
            &'static [(u32, &'static str, i64)],
            Option<u32>,
        );
        let cases: [Case; 8] = [
            (
                "row 2: last shell call before the commit (marker-only archive)",
                "marker",
                (-60, 60),
                &[(3, "Read", -50), (5, "Bash", -5), (7, "Bash", 30)],
                Some(5),
            ),
            (
                "row 5: the log exists and holds no record of the edit",
                "log",
                (-60, 60),
                &[(3, "Read", -50), (5, "Bash", -5)],
                Some(5),
            ),
            (
                "row 6: no directory ever seen",
                "none",
                (-60, 60),
                &[(5, "Bash", -5)],
                None,
            ),
            (
                "row 7: another session committed it",
                "marker",
                (-60, -10),
                &[(5, "Bash", -20)],
                None,
            ),
            (
                "row 8: no shell call at or before the commit",
                "marker",
                (-60, 60),
                &[(3, "Read", -5), (7, "Bash", 30)],
                None,
            ),
            (
                "codex shell name",
                "marker",
                (-60, 60),
                &[(2, "exec", -5)],
                Some(2),
            ),
            (
                "codex exec_command name",
                "marker",
                (-60, 60),
                &[(4, "exec_command", -5)],
                Some(4),
            ),
            (
                "row 17: the last shell call is 30 minutes before the commit",
                "marker",
                (-3600, 3600),
                &[(5, "Bash", -1800)],
                None,
            ),
        ];
        for (name, seen, (from, to), calls, expected) in cases {
            let home = tempfile::tempdir().unwrap();
            let repo = rebuild();
            attach(home.path(), repo.path());
            record(home.path(), repo.path(), "a.txt", "commit-session");
            if seen != "none" {
                let dir = repo.path().join(".git/ai/working_logs").join(format!(
                    "old-{}",
                    git(repo.path(), &["rev-parse", "HEAD^"]).trim()
                ));
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join(".archived_at"), "1").unwrap();
                if seen == "log" {
                    std::fs::write(dir.join("checkpoints.jsonl"), "").unwrap();
                }
            }
            let ids: Vec<String> = calls
                .iter()
                .map(|(turn, tool, _)| format!("{tool}-{turn}"))
                .collect();
            let spec: Vec<(u32, &str, &str, i64)> = calls
                .iter()
                .zip(&ids)
                .map(|((turn, tool, at), id)| (*turn, *tool, id.as_str(), *at))
                .collect();
            seed_commit_session(
                home.path(),
                repo.path(),
                "commit-session",
                (from, to),
                &spec,
            );
            let answer = query(home.path(), "a.txt").await;
            let range = &answer["ranges"][0];
            let reason = if seen != "none" {
                TRACE_MISSING
            } else {
                LOG_UNAVAILABLE
            };
            assert_eq!(range["unavailable"]["turn"], reason, "{name}");
            match expected {
                Some(turn) => {
                    assert_eq!(range["turn"]["basis"], BASIS_COMMIT, "{name}");
                    assert_eq!(range["turn"]["turn"], turn, "{name}");
                    assert_eq!(range["turn"]["sessionId"], "commit-session", "{name}");
                }
                None => assert_eq!(range["turn"], Value::Null, "{name}"),
            }
        }
    }
    /// Case row 14. Fails on main: a miss never carries a turn, so the second range is `null`.
    /// One answer holds an edit turn (its trace is in the log) and a commit turn (its trace is not).
    #[tokio::test]
    async fn one_answer_mixes_an_edit_turn_and_a_commit_turn_across_ranges() {
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        record(home.path(), repo.path(), "a.txt", "mix-session");
        let log = checkpoint_log(repo.path(), "a.txt", &["toolu_edit"]);
        // Keep the first range's record only: the other ranges' edits are not recorded.
        let first = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .to_owned();
        std::fs::write(&log, format!("{first}\n")).unwrap();
        seed_commit_session(
            home.path(),
            repo.path(),
            "mix-session",
            (-60, 60),
            &[(1, "Edit", "toolu_edit", -30), (5, "Bash", "Bash-5", -5)],
        );
        let answer = query(home.path(), "a.txt").await;
        let ranges = answer["ranges"].as_array().unwrap();
        assert!(ranges.len() >= 2, "{answer}");
        assert_eq!(ranges[0]["turn"]["basis"], BASIS_EDIT);
        assert_eq!(ranges[0]["turn"]["toolUseId"], "toolu_edit");
        assert_eq!(ranges[0]["turn"]["turn"], 1);
        assert_eq!(ranges[0]["unavailable"]["turn"], Value::Null);
        assert_eq!(ranges[1]["turn"]["basis"], BASIS_COMMIT);
        assert_eq!(ranges[1]["turn"]["turn"], 5);
        assert_eq!(ranges[1]["unavailable"]["turn"], TRACE_MISSING);
    }
    /// Rows 11 and 12 at the entry. Fails on main: every pointer names the session's own task, so
    /// the interval sub-task the edit ran in is never reported.
    #[tokio::test]
    async fn a_pointer_names_the_interval_sub_task_its_call_ran_in() {
        use crate::session_usage::task::{TaskRecord, LINK_SESSION};
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        record(home.path(), repo.path(), "a.txt", "task-session");
        checkpoint_log(repo.path(), "a.txt", &["toolu_01CCC"]);
        seed_commit_session(
            home.path(),
            repo.path(),
            "task-session",
            (-60, 60),
            &[(2, "Edit", "toolu_01CCC", -30)],
        );
        let store = UsageStore::open(&home.path().join("usage.sqlite")).unwrap();
        let mut sub = TaskRecord::new("sub-review", "review");
        sub.created_at_ms = head_commit_ms(repo.path()) - 40_000;
        store.insert_task_if_absent(&sub).unwrap();
        store
            .add_task_link("sub-review", LINK_SESSION, "task-session")
            .unwrap();
        drop(store);
        let turn = query(home.path(), "a.txt").await["ranges"][0]["turn"].clone();
        assert_eq!(turn["basis"], BASIS_EDIT);
        assert_eq!(turn["taskId"], "sub-review");
        assert_eq!(turn["taskBasis"], "interval");
    }
    /// Case row 3. Fails on main: the trace's `exec-…` id names no indexed call (only `call_id`
    /// is indexed), so the pointer keeps `turn: null` / "conversation not captured".
    #[tokio::test]
    async fn codex_file_change_item_id_resolves_to_the_enclosing_call_turn() {
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        record(home.path(), repo.path(), "b.txt", "codex-turn");
        checkpoint_log(repo.path(), "b.txt", &["exec-alias-1"]);
        let rollout = [
            r#"{"type":"session_meta","payload":{"id":"01a17e00-0000-7000-b000-000000000003"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"edit b"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"custom_tool_call","call_id":"call_enclosing","name":"exec","input":"apply_patch"}}"#,
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"FileChange","id":"exec-alias-1"}}}"#,
            r#"{"type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"call_enclosing","output":"ok"}}"#,
        ]
        .join("\n");
        capture(
            home.path(),
            &home.path().join("session-conversations"),
            "codex-turn",
            Some(&rollout),
            &Default::default(),
        );
        let store = UsageStore::open(&home.path().join("usage.sqlite")).unwrap();
        let enclosing = store
            .find_tool_call("codex-turn", "call_enclosing")
            .unwrap()
            .unwrap();
        let answer = query(home.path(), "b.txt").await;
        let turn = &answer["ranges"][0]["turn"];
        assert_eq!(turn["toolUseId"], "exec-alias-1");
        assert_eq!(turn["turn"], enclosing.turn_index);
        assert_eq!(turn["basis"], BASIS_EDIT);
        assert!(turn.get("reason").is_none());
        // `session_read {toolUseId: exec-…}` jumps through the same lookup.
        let pointer =
            crate::session_conversation::indexed_pointer(&store, "codex-turn", "exec-alias-1")
                .unwrap();
        assert_eq!(pointer.turn_index, Some(enclosing.turn_index));
    }
    /// Case rows 5 and 6 (reason half). Fails on main: a seen archive with no `checkpoints.jsonl`
    /// never stores its base, so the miss says "checkpoint log not available".
    #[tokio::test]
    async fn a_seen_directory_without_a_log_is_not_log_unavailable() {
        for (seen, expected) in [(false, LOG_UNAVAILABLE), (true, TRACE_MISSING)] {
            let home = tempfile::tempdir().unwrap();
            let repo = rebuild();
            attach(home.path(), repo.path());
            record(home.path(), repo.path(), "a.txt", "turn-session");
            if seen {
                let dir = repo.path().join(".git/ai/working_logs").join(format!(
                    "old-{}",
                    git(repo.path(), &["rev-parse", "HEAD^"]).trim()
                ));
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join(".archived_at"), "1").unwrap();
            }
            let answer = query(home.path(), "a.txt").await;
            assert_eq!(answer["ranges"][0]["turn"], Value::Null, "seen={seen}");
            assert_eq!(
                answer["ranges"][0]["unavailable"]["turn"], expected,
                "seen={seen}"
            );
        }
    }
    #[tokio::test]
    async fn truncated_reason_applies_only_to_the_dropped_pointer_text() {
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        record(home.path(), repo.path(), "a.txt", "turn-session");
        checkpoint_log(repo.path(), "a.txt", &["toolu_2", "toolu_199"]);
        let log = crate::session_conversation::tests::long_claude_log(200);
        capture(
            home.path(),
            &home.path().join("session-conversations"),
            "turn-session",
            Some(&log),
            &crate::session_conversation::Policy {
                max_bytes: 60 * 1024,
                ..Default::default()
            },
        );
        let answer = query(home.path(), "a.txt").await;
        assert!(answer["ranges"][0]["turn"]["turn"].is_number());
        assert_eq!(
            answer["ranges"][0]["turn"]["reason"],
            CONVERSATION_TEXT_TRUNCATED
        );
        assert!(answer["ranges"][1]["turn"]["turn"].is_number());
        assert!(answer["ranges"][1]["turn"].get("reason").is_none());
    }
    #[test]
    fn many_traces_for_one_tool_use_one_real_index_lookup() {
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        record(home.path(), repo.path(), "a.txt", "memo");
        checkpoint_log(repo.path(), "a.txt", &["toolu_01CCC"]);
        capture(
            home.path(),
            &home.path().join("session-conversations"),
            "memo",
            None,
            &Default::default(),
        );
        let mut ranges = serde_json::to_value(
            GitAiSource::default()
                .ranges(repo.path(), "HEAD", "a.txt")
                .unwrap(),
        )
        .unwrap();
        for r in ranges.as_array_mut().unwrap() {
            r["link"] = json!("linked");
            r["sessionId"] = json!("memo");
        }
        let commits = ranges
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["commit"].as_str().map(str::to_owned))
            .collect();
        let mut store = UsageStore::open(&home.path().join("usage.sqlite")).unwrap();
        super::super::checkpoints::harvest(
            repo.path(),
            "github.com/acme/api",
            &mut store,
            &commits,
        )
        .unwrap();
        let mut value = json!({"ranges":ranges});
        let mut calls = 0;
        join(
            home.path(),
            &Principal::from(1),
            &home.path().join("usage.sqlite"),
            "acme/api",
            &mut value,
            |store, session, tool| {
                calls += 1;
                crate::session_conversation::indexed_pointer(store, session, tool)
            },
        )
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(value["ranges"][0]["turn"], value["ranges"][1]["turn"]);
    }
    #[tokio::test]
    async fn scoped_queries_read_zero_unchanged_bytes_and_resume_partial_appends() {
        async fn observed(home: &Path) -> crate::attribution::checkpoints::Stats {
            let (send, recv) = std::sync::mpsc::channel();
            why_line::resolve_at_observed(
                home,
                &Principal::from(1),
                &home.join("usage.sqlite"),
                &home.join("session-conversations"),
                &params("a.txt"),
                &CallerScope::Human,
                Default::default(),
                move |stats| {
                    send.send(stats).unwrap();
                },
            )
            .await
            .unwrap();
            recv.recv().unwrap()
        }
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        record(home.path(), repo.path(), "a.txt", "turn-session");
        let log = checkpoint_log(repo.path(), "a.txt", &["toolu_01CCC"]);
        let unrelated = repo
            .path()
            .join(".git/ai/working_logs")
            .join(git(repo.path(), &["rev-parse", "HEAD"]).trim())
            .join("checkpoints.jsonl");
        std::fs::create_dir_all(unrelated.parent().unwrap()).unwrap();
        std::fs::write(
            unrelated,
            include_str!("testdata/git_ai_1_7_5_checkpoints/archived-codex.jsonl"),
        )
        .unwrap();
        assert_eq!(
            observed(home.path()).await.bytes_read as u64,
            log.metadata().unwrap().len()
        );
        assert_eq!(observed(home.path()).await.bytes_read, 0);
        let mut extra: Value = serde_json::from_str(
            std::fs::read_to_string(&log)
                .unwrap()
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        extra["trace_id"] = json!("t_partial_append");
        let text = extra.to_string();
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&log)
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
        assert_eq!(observed(home.path()).await.partial, 1);
        assert_eq!(observed(home.path()).await.bytes_read, 0);
        std::fs::OpenOptions::new()
            .append(true)
            .open(log)
            .unwrap()
            .write_all(b"\n")
            .unwrap();
        assert_eq!(observed(home.path()).await.bytes_read, text.len() + 1);
    }
    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "descriptor census requires an isolated test process"]
    async fn fifty_sequential_queries_release_checkpoint_and_sqlite_descriptors() {
        use std::io::Write;
        fn descriptors() -> usize {
            std::fs::read_dir("/dev/fd").unwrap().count()
        }
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        attach(home.path(), repo.path());
        record(home.path(), repo.path(), "a.txt", "fd-session");
        let log = checkpoint_log(repo.path(), "a.txt", &["toolu_01CCC"]);
        let records = std::fs::read(&log).unwrap();
        capture(
            home.path(),
            &home.path().join("session-conversations"),
            "fd-session",
            None,
            &crate::session_conversation::Policy::default(),
        );
        // Prime the runtime, raw cache, conversation index and SQLite connections.
        for _ in 0..5 {
            assert_eq!(
                query(home.path(), "a.txt").await["ranges"][0]["turn"]["turn"],
                2
            );
        }
        let before = descriptors();
        let mut peak = before;
        for iteration in 0..50 {
            // Every query must reopen/read the log, not only skip an unchanged cursor.
            std::fs::OpenOptions::new()
                .append(true)
                .open(&log)
                .unwrap()
                .write_all(&records)
                .unwrap();
            let value = query(home.path(), "a.txt").await;
            assert_eq!(value["ranges"][0]["turn"]["toolUseId"], "toolu_01CCC");
            assert_eq!(value["ranges"][0]["turn"]["turn"], 2);
            let after = descriptors();
            peak = peak.max(after);
            assert!(
                after <= before,
                "query {iteration}: descriptors grew from {before} to {after}"
            );
        }
        eprintln!("50 sequential resolve_at calls with appended checkpoint logs: fd before={before}, peak={peak}, after={}", descriptors());
    }
}
