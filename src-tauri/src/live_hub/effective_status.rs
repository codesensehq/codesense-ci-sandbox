// SPDX-License-Identifier: MIT
//! The sessions projection's status vocabulary and pure display precedence.
use super::projection::{FieldProvider, FieldProviders};
pub(crate) use crate::mcp::activity::CompletionBoundary;
use codesense_pty_core::activity::{Snapshot, State};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const STALLED_THRESHOLD_MS: u64 = 600_000;
/// How long a bound session may stay unobserved, when its CLI waits on a native
/// readiness signal, before it reads as stuck on a startup screen. An adapter may
/// override it through `Control::unready_bound_ms`.
pub const UNREADY_BOUND_MS: u64 = 180_000;

/// Whether `snapshot` is a live session whose CLI reports readiness natively and
/// has not yet been observed: the capability decides, never a CLI name.
pub(crate) fn awaiting_native_readiness(snapshot: &Snapshot) -> bool {
    snapshot.exit.is_none()
        && !snapshot.observed
        && codesense_pty_core::agent_cli::adapter(snapshot.provider)
            .control()
            .native_readiness
}

/// The bound after which an unready `snapshot`'s session needs you.
pub(crate) fn unready_bound_ms(snapshot: &Snapshot) -> u64 {
    codesense_pty_core::agent_cli::adapter(snapshot.provider)
        .control()
        .unready_bound_ms
        .unwrap_or(UNREADY_BOUND_MS)
}

/// A session is stuck on a startup screen when it is awaiting native readiness,
/// was never observed ready, and was bound at least its bound ago.
pub(crate) fn stuck_on_startup(
    snapshot: Option<&Snapshot>,
    ready_at_ms: Option<u64>,
    bound_at_ms: Option<u64>,
    now: u64,
) -> bool {
    let Some((snapshot, bound_at)) = snapshot.zip(bound_at_ms) else {
        return false;
    };
    ready_at_ms.is_none()
        && awaiting_native_readiness(snapshot)
        && now.saturating_sub(bound_at) >= unready_bound_ms(snapshot)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentStatus {
    Running,
    Thinking,
    ToolRunning,
    Paused,
    Blocked,
    AwaitingAnswer,
    Idle,
    Stalled,
    Unknown,
    Completed,
    Dropped,
    Failed,
    Crashed,
    Done,
    Errored,
}
impl AgentStatus {
    pub(crate) fn rank(self) -> u8 {
        match self {
            Self::AwaitingAnswer
            | Self::Paused
            | Self::Blocked
            | Self::Dropped
            | Self::Failed
            | Self::Crashed
            | Self::Errored => 0,
            Self::Running | Self::Thinking | Self::ToolRunning => 1,
            Self::Idle | Self::Stalled | Self::Unknown | Self::Completed | Self::Done => 2,
        }
    }
}
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LiveSignals {
    pub needs_you: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_state: Option<String>,
    pub agent_ended: bool,
    pub new_turn_after_completion: bool,
    pub tool_open: bool,
    pub subagents: u32,
    pub unobserved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity_tick: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity_stale_for: Option<u64>,
}

pub(crate) fn effective_status(
    row: &Value,
    outcome: Option<&str>,
    live: Option<&LiveSignals>,
) -> AgentStatus {
    use AgentStatus::*;
    let base = row
        .get("status")
        .filter(|v| !v.is_null())
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_else(|| {
            if row["sessionId"].as_str().is_none() {
                Errored
            } else if row["alive"] == true {
                Running
            } else {
                Done
            }
        });
    if matches!(base, Paused | Errored) {
        return base;
    }
    let historical = matches!(outcome, Some("done" | "failed" | "needs-attention"));
    let display_outcome = if historical && live.is_some_and(|l| l.new_turn_after_completion) {
        None
    } else {
        outcome
    };
    if let Some(live) = live {
        if live.agent_ended {
            return match live.needs_you.as_deref() {
                Some("question") => AwaitingAnswer,
                Some("failed") => Failed,
                Some("attention") => Dropped,
                _ if display_outcome == Some("needs-attention") => Dropped,
                Some("crashed") => Crashed,
                _ => Done,
            };
        }
        if live.needs_you.as_deref() == Some("question") {
            return AwaitingAnswer;
        }
    }
    let resolved = match display_outcome {
        None => base,
        Some("done") => Completed,
        Some("failed") => Failed,
        Some("needs-attention") => Dropped,
        Some("awaiting-answer") => AwaitingAnswer,
        _ => Running,
    };
    let Some(live) = live.filter(|_| resolved == Running) else {
        return resolved;
    };
    match live.needs_you.as_deref() {
        Some("question") => return AwaitingAnswer,
        Some("permission") => return Paused,
        Some("blocked") => return Blocked,
        Some("failed") => return Failed,
        Some("attention" | "startup") => return Dropped,
        Some("crashed") => return Crashed,
        _ => {}
    }
    if live.unobserved {
        return Unknown;
    }
    if live.subagents == 0
        && (live.dot.as_deref() == Some("done")
            || matches!(live.agent_state.as_deref(), Some("done" | "idle")))
    {
        return Idle;
    }
    if live.subagents == 0
        && live
            .activity_stale_for
            .is_some_and(|age| age >= STALLED_THRESHOLD_MS)
    {
        return Stalled;
    }
    if matches!(live.agent_state.as_deref(), Some("working" | "thinking")) {
        return if live.tool_open {
            ToolRunning
        } else {
            Thinking
        };
    }
    if live.tool_open {
        return ToolRunning;
    }
    if matches!(live.dot.as_deref(), Some("thinking" | "running")) {
        return Thinking;
    }
    Running
}

impl CompletionBoundary {
    pub(crate) fn followed_by(&self, snapshot: &Snapshot) -> bool {
        snapshot.observed
            && snapshot.exit.is_none()
            && matches!(
                snapshot.state,
                State::Thinking | State::Working | State::NeedsPermission
            )
            && snapshot.identity == self.identity
            && snapshot.turn_generation > self.turn_generation
            && snapshot.revision > self.revision
    }
}

/// Legacy inputs come from the existing turn owner, not a second activity reducer.
#[derive(Default)]
pub(crate) struct LegacySignals {
    pub state: Option<String>,
    pub observed: bool,
    pub tool_open: bool,
    pub ended: bool,
    pub activity_at: Option<u64>,
    pub new_turn: bool,
}

pub(crate) fn select_ai_status(projected: Option<&str>, stored: Option<&str>) -> Option<String> {
    if stored == Some("needs-permission") && matches!(projected, Some("running" | "thinking")) {
        stored.map(str::to_owned)
    } else {
        projected.or(stored).map(str::to_owned)
    }
}

pub(crate) fn select_live_signals(
    snapshot: Option<&Snapshot>,
    legacy: &LegacySignals,
    projected_dot: Option<&str>,
    stored_dot: Option<&str>,
    outcome: Option<&str>,
    pending_ask: bool,
    dismissed: bool,
    output_tick: Option<u64>,
    progress_tick: Option<u64>,
    completion: Option<&CompletionBoundary>,
    host_activity_tick: Option<u64>,
    startup_stuck: bool,
    now: u64,
) -> LiveSignals {
    let new_turn =
        legacy.new_turn || completion.is_some_and(|c| snapshot.is_some_and(|s| c.followed_by(s)));
    let outcome_for_needs = if new_turn { None } else { outcome };
    let dot = select_ai_status(projected_dot, stored_dot);
    let terminal_outcome = matches!(
        outcome_for_needs,
        Some("done" | "failed" | "needs-attention")
    );
    let host_question = !terminal_outcome
        && snapshot.is_some_and(|s| {
            s.observed
                && s.exit.is_none()
                && s.state == State::NeedsPermission
                && (s.reason.as_deref() == Some("question") || !s.asks.is_empty())
        });
    let needs = if pending_ask || host_question {
        Some("question")
    } else if dot.as_deref() == Some("crashed") {
        if dismissed {
            None
        } else {
            Some("crashed")
        }
    } else if dismissed && legacy.ended {
        None
    } else if startup_stuck && !legacy.ended {
        Some("startup")
    } else if outcome_for_needs == Some("failed") {
        Some("failed")
    } else if outcome_for_needs == Some("needs-attention") {
        Some("attention")
    } else if dot.as_deref() == Some("error") {
        Some("failed")
    } else if dot.as_deref() == Some("needs-permission") {
        Some(if legacy.state.as_deref() == Some("blocked") {
            "blocked"
        } else {
            "permission"
        })
    } else {
        None
    };
    let activity_tick = [
        legacy.activity_at,
        output_tick,
        progress_tick,
        host_activity_tick,
    ]
    .into_iter()
    .flatten()
    .max();
    LiveSignals {
        needs_you: needs.map(str::to_owned),
        dot,
        agent_state: if snapshot.is_some_and(|s| !s.observed) && !legacy.observed {
            None
        } else {
            legacy.state.clone()
        },
        agent_ended: legacy.ended,
        new_turn_after_completion: new_turn,
        tool_open: snapshot
            .filter(|s| s.observed && s.exit.is_none())
            .and_then(|s| s.tool_open)
            .unwrap_or(legacy.tool_open),
        subagents: snapshot.map_or(0, |s| s.subagents_open),
        unobserved: !(snapshot.is_some_and(|s| s.observed)
            || legacy.observed
            || stored_dot.is_some_and(|s| s != "running")
            || progress_tick.is_some()
            || outcome.is_some_and(|s| s != "running")
            || (outcome == Some("running") && output_tick.is_some())
            || pending_ask),
        activity_tick,
        activity_stale_for: activity_tick.map(|tick| now.saturating_sub(tick)),
    }
}

/// MCP rows reference the sessions owner's display fields, including the mark key to dismiss.
pub(crate) fn join_list_rows(rows: &mut [Value], owner: &[Value]) {
    for row in rows {
        let session = row["sessionId"]
            .as_str()
            .or_else(|| row["session"].as_str());
        let source = owner.iter().find(|source| match session {
            Some(id) => source["id"] == id,
            None => source["handle"] == row["handle"],
        });
        if let Some(source) = source {
            for key in ["effectiveStatus", "crashId", "endReason"] {
                if let Some(value) = source.get(key) {
                    row[key] = value.clone();
                }
            }
            // Why the row needs you; a live session stuck on a startup screen lists as
            // `needs-you` though its host has seen no turn.
            if let Some(kind) = source.get("needsYouKind").filter(|kind| !kind.is_null()) {
                row["needsYouKind"] = kind.clone();
                if kind == "startup" && row["state"] != "exited" {
                    row["state"] = "needs-you".into();
                }
            }
        }
    }
}

struct EffectiveStatusFields;
impl FieldProvider for EffectiveStatusFields {
    fn fields(&self) -> &'static [&'static str] {
        &[
            "effectiveStatus",
            "hasAgent",
            "crashId",
            "endReason",
            "activityAtMs",
            "managedState",
            "needsYouKind",
        ]
    }
    fn contribute(&self, source: &Map<String, Value>, row: &mut Map<String, Value>) {
        let live: Option<LiveSignals> = source
            .get("_statusSignals")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        let outcome = source.get("_managedOutcome").and_then(Value::as_str);
        let row_source = Value::Object(source.clone());
        // Architecture: process-row existence is not agent evidence. Publish the
        // existing observation/launch distinction once for terminal consumers.
        let has_agent = live.as_ref().is_some_and(|signals| !signals.unobserved)
            || outcome.is_some()
            || source.get("detected").is_some_and(Value::is_object)
            || source.get("_namesAnAgent").and_then(Value::as_bool) == Some(true);
        row.insert("hasAgent".into(), has_agent.into());
        row.insert(
            "effectiveStatus".into(),
            serde_json::to_value(effective_status(&row_source, outcome, live.as_ref())).unwrap(),
        );
        row.insert(
            "needsYouKind".into(),
            live.as_ref()
                .and_then(|signals| signals.needs_you.as_ref())
                .map_or(Value::Null, |kind| kind.clone().into()),
        );
        if source.get("_pendingManagedAsk").and_then(Value::as_bool) == Some(true) {
            row.insert("managedState".into(), "awaiting-answer".into());
        } else if let Some(outcome) = outcome {
            row.insert("managedState".into(), outcome.into());
        }
        if let Some(tick) = live.as_ref().and_then(|signals| signals.activity_tick) {
            row.insert("activityAtMs".into(), tick.into());
        }
        if let Some(id) = source.get("crashId") {
            row.insert("crashId".into(), id.clone());
        }
        if let Some(reason) = source.get("endReason") {
            row.insert("endReason".into(), reason.clone());
        }
    }
}
pub(crate) fn register(providers: &mut FieldProviders) {
    providers
        .register(Box::new(EffectiveStatusFields))
        .expect("unique effective status owner");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_parents_with_open_subagents_keep_a_working_status_for_both_providers() {
        for provider in ["claude", "codex"] {
            for (state, tool_open, expected) in [
                ("thinking", false, AgentStatus::Thinking),
                ("working", true, AgentStatus::ToolRunning),
                ("done", false, AgentStatus::Running),
                ("idle", false, AgentStatus::Running),
            ] {
                for age in [
                    STALLED_THRESHOLD_MS - 1,
                    STALLED_THRESHOLD_MS,
                    STALLED_THRESHOLD_MS + 1,
                ] {
                    let snapshot: Snapshot = serde_json::from_value(serde_json::json!({
                        "session":"s", "identity":{"hostBootId":"boot", "sessionIncarnation":"pty", "invocationId":"run"},
                        "revision":1, "turnGeneration":1, "provider":provider,
                        "state":state, "observed":true, "limited":false, "exit":null,
                        "asks":[], "toolOpen":tool_open, "subagentsOpen":2
                    })).unwrap();
                    let legacy = LegacySignals {
                        state: Some(state.into()),
                        observed: true,
                        activity_at: Some(1000),
                        ..Default::default()
                    };
                    let live = select_live_signals(
                        Some(&snapshot),
                        &legacy,
                        None,
                        None,
                        None,
                        false,
                        false,
                        None,
                        None,
                        None,
                        None,
                        false,
                        1000 + age,
                    );
                    assert_eq!(live.subagents, 2, "{provider}");
                    let row =
                        serde_json::json!({"status":"running", "sessionId":"s", "alive":true});
                    assert_eq!(
                        effective_status(&row, None, Some(&live)),
                        expected,
                        "{provider} {state} {age}"
                    );
                }
            }
        }
    }

    #[test]
    fn stale_parent_without_subagents_still_stalls() {
        let row = serde_json::json!({"status":"running", "sessionId":"s", "alive":true});
        for age in [STALLED_THRESHOLD_MS, STALLED_THRESHOLD_MS + 1] {
            let live = LiveSignals {
                agent_state: Some("working".into()),
                tool_open: true,
                activity_stale_for: Some(age),
                ..Default::default()
            };
            assert_eq!(
                effective_status(&row, None, Some(&live)),
                AgentStatus::Stalled
            );
        }
    }

    #[test]
    fn a_plain_shell_is_not_an_agent_but_observed_and_launched_sessions_are() {
        use crate::commands::dashboard_launch::DashboardLaunchState;
        let launch = DashboardLaunchState::new(4);
        launch.register("workspace", "shell", Some("codesense"));
        launch.begin("launched").unwrap();
        launch.register("named", "named-session", Some("codesense"));
        launch.set_agent_revision_if_absent("named", "worker@1".into());
        let mut providers = FieldProviders::new([] as [&str; 0]);
        register(&mut providers);
        let source = serde_json::json!({"sessionId":"shell","handle":"workspace","alive":true,"kind":"registered",
            "agentRevision":null,"_namesAnAgent":launch.names_an_agent("workspace"),
            "_statusSignals":LiveSignals { unobserved:true, ..Default::default() }});
        let project = |source: &Value| {
            super::super::agent_list::sessions::project(&[source.clone()], &[], &mut [], &providers)
                [0]
            .1
            .clone()
        };
        assert_eq!(project(&source)["effectiveStatus"], "unknown");
        assert_eq!(project(&source)["hasAgent"], false);
        assert!(project(&source).get("_namesAnAgent").is_none());
        let mut live_pty = source.clone();
        live_pty["kind"] = "live-pty".into();
        assert_eq!(project(&live_pty)["hasAgent"], false);
        let mut observed = source.clone();
        observed["_statusSignals"]["unobserved"] = false.into();
        assert_eq!(project(&observed)["hasAgent"], true);
        for handle in ["launched", "named"] {
            let mut launched = source.clone();
            launched["_namesAnAgent"] = launch.names_an_agent(handle).into();
            assert_eq!(project(&launched)["hasAgent"], true, "{handle}");
        }
        let mut managed = source.clone();
        managed["_managedOutcome"] = "running".into();
        assert_eq!(project(&managed)["hasAgent"], true);
        let mut detected = source.clone();
        detected["detected"] = serde_json::json!({"cli":"claude"});
        assert_eq!(project(&detected)["hasAgent"], true);
    }
    #[test]
    fn shared_derive_table() {
        let fixture: Value =
            serde_json::from_str(include_str!("effective_status/parity.json")).unwrap();
        for (i, case) in fixture["derive"].as_array().unwrap().iter().enumerate() {
            let args = &case["args"];
            let mut live: Option<LiveSignals> = args.get(2).filter(|v| !v.is_null()).map(|v| {
                let mut value = serde_json::to_value(LiveSignals::default()).unwrap();
                value
                    .as_object_mut()
                    .unwrap()
                    .extend(v.as_object().unwrap().clone());
                serde_json::from_value(value).unwrap()
            });
            if let Some(live) = live.as_mut() {
                live.activity_tick = None;
            }
            assert_eq!(
                serde_json::to_value(effective_status(
                    &args[0],
                    args.get(1).and_then(Value::as_str),
                    live.as_ref()
                ))
                .unwrap(),
                case["expected"],
                "derive case {i}"
            );
        }
    }
    fn legacy_from_record(rec: &Value, completion: Option<u64>) -> LegacySignals {
        let ended = rec["t2"]["signal"]["kind"] == "exited"
            && (rec["t1"].is_null() || rec["t2"]["seq"].as_u64() >= rec["t1"]["seq"].as_u64());
        LegacySignals {
            state: rec["state"].as_str().map(str::to_owned),
            observed: !rec["t1"].is_null(),
            ended,
            tool_open: rec["state"] == "working"
                && rec["openTools"].as_u64().map_or(
                    rec["t1"]["signal"]["kind"] == "claude-hook"
                        && rec["t1"]["signal"]["event"] == "PreToolUse",
                    |n| n > 0,
                ),
            activity_at: rec["activityAt"].as_u64(),
            new_turn: completion
                .zip(rec["turnStartedSeq"].as_u64())
                .is_some_and(|(c, t)| t > c),
        }
    }
    #[test]
    fn shared_live_signal_table() {
        let fixture: Value =
            serde_json::from_str(include_str!("effective_status/parity.json")).unwrap();
        for (i, case) in fixture["live"].as_array().unwrap().iter().enumerate() {
            let src = &case["args"][0];
            let id = case["args"][1].as_str().unwrap();
            let rec = &src["agent"]["bySession"][id];
            let legacy =
                legacy_from_record(rec, src["agent"]["completionSeqBySession"][id].as_u64());
            let snapshot: Option<Snapshot> =
                serde_json::from_value(src["agent"]["activityBySession"][id]["snapshot"].clone())
                    .ok();
            let pending = src["pendingAsks"]
                .as_object()
                .is_some_and(|asks| asks.values().any(|a| a["sessionId"] == id));
            let dismissed = src["agent"]["dismissedCrashBySession"][id]
                .as_u64()
                .is_some_and(|seq| {
                    legacy.ended
                        && rec["state"] == "crashed"
                        && rec["t2"]["seq"].as_u64() == Some(seq)
                });
            let mut signals = select_live_signals(
                snapshot.as_ref(),
                &legacy,
                src["agentDots"][id].as_str(),
                src["aiStatusBySession"][id].as_str(),
                src["outcomes"][id].as_str(),
                pending,
                dismissed,
                src["agent"]["outputAtBySession"][id].as_u64(),
                src["progressTickBySession"][id].as_u64(),
                None,
                None,
                src["startupStuck"].as_bool().unwrap_or(false),
                0,
            );
            signals.activity_stale_for = None;
            assert_eq!(
                serde_json::to_value(signals).unwrap(),
                case["expected"],
                "live case {i}"
            );
        }
    }
    fn codex_snapshot(provider: &str, observed: bool, exited: bool) -> Snapshot {
        serde_json::from_value(serde_json::json!({
            "session":"s", "identity":{"hostBootId":"boot", "sessionIncarnation":"pty", "invocationId":"run"},
            "revision":1, "turnGeneration":0, "provider":provider,
            "state":"idle", "observed":observed, "limited":false,
            "exit": if exited { serde_json::json!({"code":0,"signal":null,"requested":false}) } else { Value::Null },
            "asks":[]
        })).unwrap()
    }

    fn signals_with(
        snapshot: Option<&Snapshot>,
        legacy: &LegacySignals,
        dot: Option<&str>,
        outcome: Option<&str>,
        pending_ask: bool,
        startup_stuck: bool,
        now: u64,
    ) -> LiveSignals {
        select_live_signals(
            snapshot,
            legacy,
            dot,
            None,
            outcome,
            pending_ask,
            false,
            None,
            None,
            None,
            None,
            startup_stuck,
            now,
        )
    }

    #[test]
    fn an_unready_native_readiness_session_needs_you_once_past_the_bound() {
        let waiting = codex_snapshot("codex", false, false);
        let bound = unready_bound_ms(&waiting);
        assert_eq!(bound, UNREADY_BOUND_MS);
        assert_eq!(UNREADY_BOUND_MS, 180_000);
        let row = serde_json::json!({"status":"running", "sessionId":"s", "alive":true});
        for (elapsed, stuck) in [(bound - 1, false), (bound, true), (bound * 20, true)] {
            assert_eq!(
                stuck_on_startup(Some(&waiting), None, Some(1_000), 1_000 + elapsed),
                stuck,
                "{elapsed}"
            );
            let live = signals_with(
                Some(&waiting),
                &LegacySignals::default(),
                None,
                None,
                false,
                stuck,
                1_000 + elapsed,
            );
            assert_eq!(live.needs_you.as_deref(), stuck.then_some("startup"));
            assert_eq!(
                effective_status(&row, None, Some(&live)),
                if stuck {
                    AgentStatus::Dropped
                } else {
                    AgentStatus::Unknown
                }
            );
        }
    }

    #[test]
    fn a_ready_exited_or_unbound_session_is_never_stuck_on_startup() {
        let waiting = codex_snapshot("codex", false, false);
        let late = 1_000 + UNREADY_BOUND_MS * 10;
        // Ready: the registry stamped it, or the host observes it.
        assert!(!stuck_on_startup(
            Some(&waiting),
            Some(2_000),
            Some(1_000),
            late
        ));
        let observed = codex_snapshot("codex", true, false);
        assert!(!stuck_on_startup(Some(&observed), None, Some(1_000), late));
        // Exited.
        let exited = codex_snapshot("codex", false, true);
        assert!(!stuck_on_startup(Some(&exited), None, Some(1_000), late));
        // Not bound by a launch, or no host snapshot yet.
        assert!(!stuck_on_startup(Some(&waiting), None, None, late));
        assert!(!stuck_on_startup(None, None, Some(1_000), late));
        // A stuck input never marks an ended session.
        let ended = LegacySignals {
            ended: true,
            ..Default::default()
        };
        let live = signals_with(Some(&exited), &ended, None, None, false, true, late);
        assert_eq!(live.needs_you, None);
    }

    #[test]
    fn a_cli_without_native_readiness_is_never_stuck_on_startup() {
        let claude = codex_snapshot("claude", false, false);
        assert!(!awaiting_native_readiness(&claude));
        for elapsed in [UNREADY_BOUND_MS, UNREADY_BOUND_MS * 1_000] {
            assert!(!stuck_on_startup(
                Some(&claude),
                None,
                Some(1_000),
                1_000 + elapsed
            ));
        }
        assert!(awaiting_native_readiness(&codex_snapshot(
            "codex", false, false
        )));
    }

    #[test]
    fn a_question_or_crash_outranks_a_startup_screen_and_it_outranks_attention() {
        let legacy = LegacySignals::default();
        let kind =
            |ask, dot, outcome| signals_with(None, &legacy, dot, outcome, ask, true, 0).needs_you;
        assert_eq!(kind(true, None, None).as_deref(), Some("question"));
        assert_eq!(
            kind(false, Some("crashed"), None).as_deref(),
            Some("crashed")
        );
        assert_eq!(kind(false, None, None).as_deref(), Some("startup"));
        assert_eq!(
            kind(false, None, Some("needs-attention")).as_deref(),
            Some("startup")
        );
    }

    #[test]
    fn live_selection_is_observation_driven() {
        let signals = select_live_signals(
            None,
            &LegacySignals::default(),
            None,
            Some("running"),
            Some("running"),
            false,
            false,
            None,
            None,
            None,
            None,
            false,
            1000,
        );
        assert!(signals.unobserved);
    }
}

#[cfg(test)]
#[path = "effective_status/seam.rs"]
mod seam;
