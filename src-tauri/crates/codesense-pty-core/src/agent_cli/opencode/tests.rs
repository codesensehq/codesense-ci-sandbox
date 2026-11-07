// SPDX-License-Identifier: MIT
use super::*;
use crate::activity::{ActivityOwner, Scope, State, Writer};

/// The plugin's hook input for `event` in provider session `session`.
fn hook(event: &str, session: &str, fields: serde_json::Value) -> Vec<u8> {
    let mut input = serde_json::json!({"hook_event_name": event, "session_id": session});
    input
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    serde_json::to_vec(&input).unwrap()
}

fn report(event: &str, fields: serde_json::Value) -> Report {
    OpenCode
        .normalize(&hook(event, "main", fields), true)
        .unwrap_or_else(|| panic!("{event} did not normalize"))
}

#[test]
fn each_event_the_plugin_reports_maps_to_the_shared_activity_event() {
    for (name, event) in [
        ("SessionStart", Event::Started),
        ("UserPromptSubmit", Event::Submitted),
        ("PreToolUse", Event::ToolStarted),
        ("PostToolUse", Event::ToolFinished),
        ("PermissionRequest", Event::Waiting),
        ("Stop", Event::Stopped),
        ("Interrupt", Event::Interrupted),
    ] {
        let report = report(name, serde_json::json!({"tool_use_id": "c1"}));
        assert_eq!(
            (report.provider, report.event, report.scope),
            (AgentCli::OpenCode, event, Scope::Main),
            "{name}"
        );
        assert_eq!(report.provider_session.as_deref(), Some("main"));
    }
    let failed = report("StopFailure", serde_json::json!({"error": "rate_limit"}));
    assert_eq!(
        (failed.event, failed.reason.as_deref()),
        (Event::Failed, Some("rate_limit"))
    );
    let unmapped = report(
        "StopFailure",
        serde_json::json!({"error": "ProviderBlewUp"}),
    );
    assert_eq!(unmapped.reason.as_deref(), Some("unknown"));
}

#[test]
fn a_hook_naming_a_subagent_is_the_childs_and_its_ids_are_kept() {
    for (name, event) in [
        ("SubagentStart", Event::SubagentStarted),
        ("SubagentStop", Event::SubagentFinished),
        ("PreToolUse", Event::ToolStarted),
    ] {
        let report = report(
            name,
            serde_json::json!({"agent_id": "kid", "tool_use_id": "k1"}),
        );
        assert_eq!(
            (report.event, report.scope),
            (event, Scope::Child),
            "{name}"
        );
        assert_eq!(report.agent.as_deref(), Some("kid"));
    }
    let tool = report(
        "PostToolUse",
        serde_json::json!({"tool_name": "bash", "tool_use_id": "c1"}),
    );
    assert_eq!(tool.tool_name.as_deref(), Some("bash"));
    assert_eq!(tool.tool.as_deref(), Some("c1"));
}

#[test]
fn an_unqualified_release_scopes_its_hooks_unknown_and_unknown_events_are_dropped() {
    let input = hook("Stop", "main", serde_json::json!({}));
    assert_eq!(
        OpenCode.normalize(&input, false).unwrap().scope,
        Scope::Unknown
    );
    assert!(OpenCode
        .normalize(&hook("Notification", "main", serde_json::json!({})), true)
        .is_none());
    assert!(OpenCode.normalize(b"not json", true).is_none());
    assert!(OpenCode.normalize(b"{}", true).is_none());
}

#[test]
fn a_qualified_release_scopes_its_hooks_main() {
    let input = hook("Stop", "main", serde_json::json!({}));
    assert_eq!(OpenCode.normalize(&input, true).unwrap().scope, Scope::Main);
}

#[test]
fn its_policy_says_what_the_plugin_proves() {
    let policy = OpenCode.session_policy();
    assert!(policy.rebind_on_start);
    assert!(!policy.observed_from_start);
    assert_eq!(policy.subagents, Subagents::Reported);
    assert_eq!(
        OpenCode.descriptor().attention,
        Some(AttentionSignal::Hooks)
    );
    assert!(OpenCode.status_hooks().is_empty());
    assert!(matches!(OpenCode.probe().activity, ActivityProbe::Version));
}

/// A host running one committed OpenCode tab, `S1`, whose plugin reports through the
/// helper's normalizer as the activity ingress does.
struct Host {
    owner: ActivityOwner,
    writer: Writer,
}

fn rig() -> Host {
    let owner = ActivityOwner::with_adapters(adapter);
    let writer = owner
        .prepare_native("S1", AgentCli::OpenCode, "unused")
        .unwrap();
    owner.register("S1", Some(std::process::id())).unwrap();
    Host { owner, writer }
}

impl Host {
    fn try_report(
        &self,
        event: &str,
        session: &str,
        fields: serde_json::Value,
    ) -> Result<crate::activity::Snapshot, &'static str> {
        let report = OpenCode
            .normalize(&hook(event, session, fields), true)
            .unwrap();
        self.owner
            .report_for_test(&self.writer, std::process::id(), report)
    }

    fn send(&self, event: &str, fields: serde_json::Value) -> crate::activity::Snapshot {
        self.try_report(event, "main", fields).unwrap()
    }

    fn state(&self) -> State {
        self.owner.snapshot("S1").unwrap().state
    }

    fn started() -> Self {
        let host = rig();
        assert_eq!(
            host.send("SessionStart", serde_json::json!({})).state,
            State::Idle
        );
        host
    }
}

#[test]
fn a_turn_reads_working_then_done() {
    let host = Host::started();
    assert_eq!(
        host.send("UserPromptSubmit", serde_json::json!({})).state,
        State::Thinking
    );
    let tool = serde_json::json!({"tool_name": "bash", "tool_use_id": "c1"});
    assert_eq!(host.send("PreToolUse", tool.clone()).state, State::Working);
    assert_eq!(host.send("PostToolUse", tool).state, State::Thinking);
    assert_eq!(host.send("Stop", serde_json::json!({})).state, State::Done);
    // The idle event reaches the plugin twice in `run` mode: the second changes nothing.
    assert_eq!(host.send("Stop", serde_json::json!({})).state, State::Done);
}

#[test]
fn a_permission_ask_needs_the_user_until_its_reply_moves_the_tool() {
    let host = Host::started();
    host.send("UserPromptSubmit", serde_json::json!({}));
    host.send(
        "PreToolUse",
        serde_json::json!({"tool_name": "bash", "tool_use_id": "c1"}),
    );
    assert_eq!(
        host.send("PermissionRequest", serde_json::json!({})).state,
        State::NeedsPermission
    );
    // Allowed: the tool goes on.
    assert_eq!(
        host.send("PreToolUse", serde_json::json!({"tool_use_id": "c1"}))
            .state,
        State::Working
    );
    host.send(
        "PostToolUse",
        serde_json::json!({"tool_name": "bash", "tool_use_id": "c1"}),
    );
    // Rejected: the prompt clears the same way.
    host.send(
        "PreToolUse",
        serde_json::json!({"tool_name": "edit", "tool_use_id": "c2"}),
    );
    assert_eq!(
        host.send("PermissionRequest", serde_json::json!({})).state,
        State::NeedsPermission
    );
    assert_eq!(
        host.send("PostToolUse", serde_json::json!({"tool_use_id": "c2"}))
            .state,
        State::Thinking
    );
    // An ask the turn ends on is cleared by the turn.
    host.send("PermissionRequest", serde_json::json!({}));
    assert_eq!(host.send("Stop", serde_json::json!({})).state, State::Done);
}

#[test]
fn two_child_sessions_count_as_open_subagents_until_each_goes_idle() {
    let host = Host::started();
    host.send("UserPromptSubmit", serde_json::json!({}));
    host.send("SubagentStart", serde_json::json!({"agent_id": "kid-1"}));
    let snapshot = host.send("SubagentStart", serde_json::json!({"agent_id": "kid-2"}));
    assert_eq!(snapshot.subagents_open, 2);
    // A child's own tool is the child's, not the main turn's.
    host.send(
        "PreToolUse",
        serde_json::json!({"agent_id": "kid-1", "tool_use_id": "k1"}),
    );
    assert_eq!(host.owner.snapshot("S1").unwrap().subagents_open, 2);
    // Child idles come before the parent's. A repeated one changes nothing.
    assert_eq!(
        host.send("SubagentStop", serde_json::json!({"agent_id": "kid-1"}))
            .subagents_open,
        1
    );
    assert_eq!(
        host.send("SubagentStop", serde_json::json!({"agent_id": "kid-1"}))
            .subagents_open,
        1
    );
    assert_eq!(
        host.send("SubagentStop", serde_json::json!({"agent_id": "kid-2"}))
            .subagents_open,
        0
    );
    assert_eq!(host.send("Stop", serde_json::json!({})).state, State::Done);
}

#[test]
fn a_session_error_fails_the_turn_with_its_reason() {
    let host = Host::started();
    host.send("UserPromptSubmit", serde_json::json!({}));
    let snapshot = host.send(
        "StopFailure",
        serde_json::json!({"error": "authentication_failed"}),
    );
    assert_eq!(snapshot.state, State::Errored);
    assert_eq!(snapshot.reason.as_deref(), Some("authentication_failed"));
}

#[test]
fn a_new_main_session_rebinds_the_tab_and_the_old_one_is_retired() {
    let host = Host::started();
    host.send("UserPromptSubmit", serde_json::json!({}));
    host.send("SubagentStart", serde_json::json!({"agent_id": "kid"}));
    let two = host
        .try_report("SessionStart", "two", serde_json::json!({}))
        .unwrap();
    assert_eq!((two.state, two.subagents_open), (State::Idle, 0));
    assert!(host
        .try_report("UserPromptSubmit", "main", serde_json::json!({}))
        .is_err());
    assert_eq!(
        host.try_report("UserPromptSubmit", "two", serde_json::json!({}))
            .unwrap()
            .state,
        State::Thinking
    );
}

#[test]
fn a_first_report_that_is_not_a_start_is_refused() {
    let host = rig();
    assert!(host
        .try_report("UserPromptSubmit", "main", serde_json::json!({}))
        .is_err());
}

#[test]
fn a_question_is_a_wait_for_the_user_cleared_by_its_own_call() {
    let host = Host::started();
    host.send("UserPromptSubmit", serde_json::json!({}));
    host.send(
        "PreToolUse",
        serde_json::json!({"tool_name": "question", "tool_use_id": "q1"}),
    );
    let asked = report("Question", serde_json::json!({"tool_use_id": "q1"}));
    assert_eq!(
        (asked.event, asked.reason.as_deref()),
        (Event::Waiting, Some("question"))
    );
    let waiting = host.send("Question", serde_json::json!({"tool_use_id": "q1"}));
    assert_eq!(
        (waiting.state, waiting.reason.as_deref()),
        (State::NeedsPermission, Some("question"))
    );
    assert_eq!(
        host.send("PostToolUse", serde_json::json!({"tool_use_id": "q1"}))
            .state,
        State::Thinking
    );
}

#[test]
fn answering_one_of_two_parallel_permission_asks_leaves_the_other_waiting() {
    let host = Host::started();
    host.send("UserPromptSubmit", serde_json::json!({}));
    for call in ["c1", "c2"] {
        host.send(
            "PreToolUse",
            serde_json::json!({"tool_name": "bash", "tool_use_id": call}),
        );
    }
    for call in ["c1", "c2"] {
        host.send(
            "PermissionRequest",
            serde_json::json!({"tool_use_id": call}),
        );
    }
    // The first ask is allowed: its call ends the wait and goes on running.
    host.send("PostToolUse", serde_json::json!({"tool_use_id": "c1"}));
    let allowed = host.send("PreToolUse", serde_json::json!({"tool_use_id": "c1"}));
    assert_eq!(allowed.state, State::NeedsPermission);
    // The second is rejected.
    assert_ne!(
        host.send("PostToolUse", serde_json::json!({"tool_use_id": "c2"}))
            .state,
        State::NeedsPermission
    );
}

#[test]
fn work_with_no_prompt_is_working_until_the_session_stops() {
    let host = Host::started();
    host.send("UserPromptSubmit", serde_json::json!({}));
    assert_eq!(host.send("Stop", serde_json::json!({})).state, State::Done);
    assert_eq!(
        report("PreCompact", serde_json::json!({})).event,
        Event::Compacting
    );
    assert_eq!(
        host.send("PreCompact", serde_json::json!({})).state,
        State::Working
    );
    assert_eq!(
        host.send("PreCompact", serde_json::json!({})).state,
        State::Working
    );
    assert_eq!(host.send("Stop", serde_json::json!({})).state, State::Done);
}

#[test]
fn a_resumed_subagent_counts_again() {
    let host = Host::started();
    host.send("UserPromptSubmit", serde_json::json!({}));
    let kid = serde_json::json!({"agent_id": "kid"});
    assert_eq!(host.send("SubagentStart", kid.clone()).subagents_open, 1);
    assert_eq!(host.send("SubagentStop", kid.clone()).subagents_open, 0);
    assert_eq!(host.send("SubagentStart", kid.clone()).subagents_open, 1);
    assert_eq!(host.send("SubagentStop", kid).subagents_open, 0);
}

#[test]
fn returning_to_an_earlier_session_rebinds_the_tab_for_opencode_only() {
    for cli in [AgentCli::Claude, AgentCli::Codex, AgentCli::OpenCode] {
        let owner = ActivityOwner::with_adapters(adapter);
        let writer = owner.prepare_native("S1", cli, "unused").unwrap();
        owner.register("S1", Some(std::process::id())).unwrap();
        let send = |event: &str, session: &str| {
            let report = adapter(cli)
                .normalize(&hook(event, session, serde_json::json!({})), true)
                .unwrap();
            owner.report_for_test(&writer, std::process::id(), report)
        };
        send("SessionStart", "A").unwrap();
        send("UserPromptSubmit", "A").unwrap();
        send("SessionStart", "B").unwrap();
        // Back in A: only a CLI that returns to its sessions rebinds to it.
        let back = send("SessionStart", "A");
        if cli == AgentCli::OpenCode {
            assert_eq!(back.unwrap().state, State::Idle);
            assert_eq!(
                send("UserPromptSubmit", "A").unwrap().state,
                State::Thinking
            );
            // B is retired now, and its late reports are refused.
            assert!(send("Stop", "B").is_err());
        } else {
            assert!(back.is_err(), "{cli:?} rebound to a retired session");
            assert!(send("UserPromptSubmit", "A").is_err(), "{cli:?}");
        }
    }
}

#[test]
fn a_compaction_in_a_session_returned_to_runs_on_the_tab_once_it_is_started_again() {
    let host = Host::started();
    host.send("UserPromptSubmit", serde_json::json!({}));
    host.send("Stop", serde_json::json!({}));
    host.try_report("SessionStart", "other", serde_json::json!({}))
        .unwrap();
    // Without the return start, the tab is on the other session and refuses A's compaction.
    assert!(host
        .try_report("PreCompact", "main", serde_json::json!({}))
        .is_err());
    host.try_report("SessionStart", "main", serde_json::json!({}))
        .unwrap();
    let working = host
        .try_report("PreCompact", "main", serde_json::json!({}))
        .unwrap();
    assert_eq!(working.state, State::Working);
    assert_eq!(host.send("Stop", serde_json::json!({})).state, State::Done);
}

#[test]
fn a_task_still_running_when_its_session_is_returned_to_is_counted_once_announced_again() {
    let host = Host::started();
    host.send("UserPromptSubmit", serde_json::json!({}));
    let kid = serde_json::json!({"agent_id": "kid"});
    assert_eq!(host.send("SubagentStart", kid.clone()).subagents_open, 1);
    // The owner forgets a session's subagents when the tab moves to another one.
    let away = host
        .try_report("SessionStart", "other", serde_json::json!({}))
        .unwrap();
    assert_eq!(away.subagents_open, 0);
    let back = host
        .try_report("SessionStart", "main", serde_json::json!({}))
        .unwrap();
    assert_eq!(back.subagents_open, 0);
    assert_eq!(host.send("SubagentStart", kid.clone()).subagents_open, 1);
    assert_eq!(host.send("SubagentStop", kid).subagents_open, 0);
}
