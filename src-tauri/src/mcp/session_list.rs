// SPDX-License-Identifier: MIT
//! What `list_sessions` shows: every session the launch registry holds, with its name,
//! who launched it, the role and avatar it was given, where it runs and a coarse state.
//! Listing is a view: every caller sees every session.

use serde::Serialize;

use crate::agent_state::AgentState;
use crate::commands::dashboard_launch::SessionRow;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum SessionState {
    Running,
    Idle,
    NeedsYou,
    Exited,
    /// Its machine is not connected: the session may still run there.
    Offline,
    /// Its owner could not supply a current observation.
    Unknown,
}

/// A session's state from its registry liveness and the last state its dot showed (fed
/// by every dot write: admitted hooks and MCP signals). An agent whose dot was never
/// written reads `running` while it is alive.
pub(crate) fn session_state(alive: bool, last: Option<AgentState>) -> SessionState {
    if !alive {
        return SessionState::Exited;
    }
    match last {
        None | Some(AgentState::Thinking | AgentState::Working) => SessionState::Running,
        Some(AgentState::NeedsPermission | AgentState::Blocked) => SessionState::NeedsYou,
        Some(AgentState::Done | AgentState::Errored | AgentState::Crashed | AgentState::Idle) => {
            SessionState::Idle
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionListRow {
    pub id: String,
    pub team: Option<crate::commands::dashboard_launch::LaunchTeam>,
    pub handle: String,
    pub display_name: String,
    pub state: SessionState,
    pub launched_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub story: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    pub chat_workspace_id: Option<String>,
    pub group_unread: u32,
    pub group_read_id: i64,
    /// The role its launcher assigned, when it assigned one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub queue_role: Option<String>,
    pub role_join_error: Option<String>,
    /// The avatar its launcher assigned, when it assigned one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    /// The saved Agent it runs (its `agent_get` id, e.g. `builtin/claude`), when it was
    /// launched from one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The machine its terminal tab runs on, by its label (`This Mac` here).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    /// That machine's id, for a remote tab: what another tool addresses it by.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    /// The workspace a remote saved-Agent tab runs in, by its handle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_handle: Option<String>,
    /// How many inbox messages addressed to its handle it has not read (what
    /// read_messages would hand it); null when its inbox could not be read. Counting
    /// marks nothing read.
    pub unread: Option<u32>,
    /// When the oldest of those was sent (RFC 3339), or null with none unread.
    pub oldest_unread_at: Option<String>,
    /// Whether its agent was told about that mail by its inbox waiter: `told` when it
    /// was told about all of it, `pending` when newer
    /// mail has not been announced yet; null with none unread. Told is not read: the
    /// agent decides when to read.
    pub mail_notice: Option<MailNotice>,
    /// When its agent was last told about its mail (RFC 3339), or null.
    pub notified_at: Option<String>,
    /// How many subagents its agent runs now, from its host's activity; omitted at none.
    /// `state` stays the agent's own: one idle at its prompt is `idle` while they run.
    #[serde(skip_serializing_if = "is_zero")]
    pub subagents: u32,
    /// Whether new mail reaches its agent at once (an idle agent starts a turn; a running
    /// one hears at its next step), through its CLI's inbox port: a waiter, or its host
    /// submitting the notice. False for an agent CodeSense has no way to tell (its CLI has
    /// no wake path, its host cannot name its process, or its waiter has gone): nothing
    /// tells it about its mail. `"unknown"` for a tab on a machine that is not connected
    /// now.
    pub inbox_waiter: InboxWaiter,
    /// Its latest `signal_progress` ({phase, step?, total?, note?, atMs}), when it
    /// reported any: where in its task it says it is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<crate::dashboard_stream::hub::SessionProgress>,
    /// The launch telemetry for the session's most recent launch, when it
    /// was recorded. Null for sessions launched before launch telemetry existed
    /// or that never completed a telemetry record. See
    /// [`LaunchTelemetry`](crate::commands::launch_telemetry::LaunchTelemetry).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch_telemetry: Option<crate::commands::launch_telemetry::LaunchTelemetry>,
    /// `false` only when the agent CLI release the launch recorded is outside the range
    /// CodeSense is qualified on, so its status, inbox wake and usage may not work. Absent
    /// when the release is qualified or unknown (a launch that recorded no version).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_qualified: Option<bool>,
    /// Why, in the words of the launch warning (the release and the range supported), present
    /// with `release_qualified`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_note: Option<String>,
    /// The enforced epoch-ms deadline for a bounded local launch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline_at_ms: Option<u64>,
    /// Present only while the launch is in its deadline exit grace period.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub expiring: bool,
    /// The current tab session its progress and activity are keyed by; handles are reusable.
    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    pub(crate) session: Option<String>,
    /// Its idle state is an ended turn that errored or crashed, not a clean finish.
    #[serde(skip)]
    pub(crate) failed: bool,
}

fn is_zero(count: &u32) -> bool {
    *count == 0
}

/// The row for the session running under `handle`.
pub(crate) fn row_for_handle<'a>(
    rows: &'a [SessionListRow],
    handle: &str,
) -> Option<&'a SessionListRow> {
    rows.iter().find(|row| row.handle == handle)
}

/// Fill each row's unread mail from `unread` (keyed by handle). A row absent from it
/// has none; one whose inbox could not be read has `unread: null`.
pub(crate) fn with_unread(
    rows: &mut [SessionListRow],
    unread: &crate::commands::agent_inbox::UnreadByHandle,
) {
    for row in rows {
        let found = unread.found.get(&row.handle);
        row.unread = if unread.failed.contains(&row.handle) {
            None
        } else {
            Some(found.map_or(0, |u| u.count))
        };
        let rfc3339 = |ms: i64| {
            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
                .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        };
        row.oldest_unread_at = found.and_then(|u| u.oldest_at_ms).and_then(rfc3339);
        let unread = found.filter(|u| u.count > 0);
        // What this session was told: a mark another session under the handle left tells
        // it nothing.
        let told = unread
            .and_then(|u| u.announced.as_ref())
            .filter(|told| row.session.as_deref() == Some(told.session.as_str()));
        row.mail_notice = unread.and_then(|u| u.newest_id).map(|newest| match told {
            Some(told) if told.through_id >= newest => MailNotice::Told,
            _ => MailNotice::Pending,
        });
        row.notified_at = told.and_then(|told| rfc3339(told.at_ms));
    }
}

/// Whether a session's agent was told about its unread mail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum MailNotice {
    /// Told about all of it.
    Told,
    /// Newer mail has not been announced yet.
    Pending,
}

/// Whether a session's agent has an inbox waiter: `true`, `false`, or `"unknown"` while
/// its machine is not connected.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum InboxWaiter {
    Armed,
    #[default]
    None,
    Unknown,
}

impl From<bool> for InboxWaiter {
    fn from(armed: bool) -> Self {
        if armed {
            Self::Armed
        } else {
            Self::None
        }
    }
}

impl Serialize for InboxWaiter {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Armed => serializer.serialize_bool(true),
            Self::None => serializer.serialize_bool(false),
            Self::Unknown => serializer.serialize_str("unknown"),
        }
    }
}

/// Fill each row's `inboxWaiter` from `armed` (keyed by handle).
pub(crate) fn with_inbox_waiters(rows: &mut [SessionListRow], armed: impl Fn(&str) -> InboxWaiter) {
    for row in rows {
        row.inbox_waiter = armed(&row.handle);
    }
}

/// Fill each row's latest `signal_progress` from `latest` (keyed by the tab session
/// the row runs in). A row with no session, or whose session reported none, has none.
pub(crate) fn with_progress(
    rows: &mut [SessionListRow],
    latest: impl Fn(&str) -> Option<crate::dashboard_stream::hub::SessionProgress>,
) {
    for row in rows {
        row.progress = row.session.as_deref().and_then(&latest);
    }
}

/// Fill each row's `launch_telemetry` from the per-session tracker
/// state. A row whose session was launched before launch telemetry existed (or
/// whose launch wrote no record) stays `None`. Keyed by the tab session (what the
/// launch bound to); falls back to the row's `handle` for callers who pass a raw session id
/// as the handle.
///
/// `#[cfg(test)]`: production enrichment uses the batched variant below so
/// `list_sessions` pays one tracker Mutex acquisition for the whole result.
/// This per-row closure form stays available to tests that want to drive the
/// row-level logic with a lookup function (not a real tracker instance).
#[cfg(test)]
pub(crate) fn with_launch_telemetry(
    rows: &mut [SessionListRow],
    latest: impl Fn(&str) -> Option<crate::commands::launch_telemetry::LaunchTelemetry>,
) {
    for row in rows {
        row.launch_telemetry = row
            .session
            .as_deref()
            .and_then(&latest)
            .or_else(|| latest(&row.handle));
        with_release(row);
    }
}

/// Marks `row` unqualified when its launch recorded an agent CLI release outside the range
/// that CLI is qualified on. The CLI is told by its binary, so nothing here names one.
fn with_release(row: &mut SessionListRow) {
    let note = row.launch_telemetry.as_ref().and_then(|launch| {
        let binary = std::path::Path::new(launch.binary_path.as_deref()?).file_name()?;
        let cli = codesense_pty_core::agent_cli::by_binary(binary.to_str()?)?;
        crate::agents::launch_check::cli_version_warning(cli, launch.binary_version.as_deref())
    });
    row.release_qualified = note.as_ref().map(|_| false);
    row.release_note = note.map(|warning| warning.message);
}

/// Batched variant of [`with_launch_telemetry`] that pays one
/// tracker Mutex acquisition for the whole result, not one per row. Called
/// by `read_session_list_and_unread` so a long `list_sessions` result does
/// not fan out N Mutex locks on the live-data hot path.
pub(crate) fn with_launch_telemetry_batched(
    rows: &mut [SessionListRow],
    tracker: &crate::commands::launch_telemetry::LaunchTelemetryTracker,
) {
    // Collect every lookup key — the row's tab session plus its handle as a
    // fallback — in one pass so the Mutex is acquired exactly once.
    let mut keys: Vec<&str> = Vec::with_capacity(rows.len() * 2);
    for row in rows.iter() {
        if let Some(session) = row.session.as_deref() {
            keys.push(session);
        }
        keys.push(&row.handle);
    }
    let found = tracker.latest_many(&keys);
    for row in rows {
        row.launch_telemetry = row
            .session
            .as_deref()
            .and_then(|k| found.get(k).cloned())
            .or_else(|| found.get(row.handle.as_str()).cloned());
        with_release(row);
    }
}

/// Every session as `list_sessions` reports it now: the launch registry's rows, named,
/// placed and judged by their hosts' live layouts and activity, with their unread mail.
/// `launched_by` (a session id or handle) keeps only the sessions it launched.
pub(crate) async fn read_session_list(
    app: &tauri::AppHandle,
    launched_by: Option<&str>,
) -> Result<Vec<SessionListRow>, String> {
    read_session_list_and_unread(app, launched_by)
        .await
        .map(|(rows, _)| rows)
}

/// [`read_session_list`], with the unread mail behind each row's count (keyed by
/// handle): its team's inbox and newest message, which the `inbox_idle` routine trigger
/// needs.
pub(crate) async fn read_session_list_and_unread(
    app: &tauri::AppHandle,
    launched_by: Option<&str>,
) -> Result<
    (
        Vec<SessionListRow>,
        crate::commands::agent_inbox::UnreadByHandle,
    ),
    String,
> {
    crate::live_hub::read(app, launched_by)
}

/// What a session's terminal tab shows now, read from its host's layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TabView {
    /// The tab's current name (none while its machine is offline).
    pub name: Option<String>,
    pub machine: String,
    /// The machine's id, when the tab runs on a remote machine (`None` on this Mac).
    pub machine_id: Option<String>,
    /// The prepared workspace a remote saved-Agent tab runs in: what hydrate and
    /// reverse-sync name it by.
    pub workspace_handle: Option<String>,
    pub workspace: Option<String>,
    /// The tab's session, for its last agent state.
    pub session: Option<String>,
    /// Whether it runs: `None` while its machine is offline.
    pub alive: Option<bool>,
    /// Authoritative activity, when this host owns a managed invocation.
    pub activity: Option<SessionState>,
    /// That activity is a turn that errored or crashed.
    pub activity_failed: bool,
    /// The subagents its agent runs now, by that activity.
    pub subagents: u32,
    /// The saved agent its tab says it runs (a remote launch's agent-key
    /// tag), for a session whose launch registry recorded none.
    pub agent: Option<String>,
}

/// The `list_sessions` rows, in handle order. `launched_by` (already normalized the way
/// the registry records it) keeps only the sessions that launcher started.
#[cfg(test)]
pub(crate) fn session_list_rows(
    rows: Vec<SessionRow>,
    last_state: impl Fn(&str) -> Option<AgentState>,
    launched_by: Option<&str>,
) -> Vec<SessionListRow> {
    session_list_rows_with_tabs(rows, last_state, launched_by, |_| None)
}

/// [`session_list_rows`], with each session that has a terminal tab named,
/// placed and judged alive by what `tab_of` reads from its host now.
pub(crate) fn session_list_rows_with_tabs(
    rows: Vec<SessionRow>,
    last_state: impl Fn(&str) -> Option<AgentState>,
    launched_by: Option<&str>,
    tab_of: impl Fn(&SessionRow) -> Option<TabView>,
) -> Vec<SessionListRow> {
    let mut listed: Vec<SessionListRow> = rows
        .into_iter()
        .filter(|row| launched_by.is_none_or(|by| row.meta.launched_by.as_deref() == Some(by)))
        .map(|row| {
            let tab = tab_of(&row);
            let session = tab
                .as_ref()
                .and_then(|tab| tab.session.clone())
                .or_else(|| row.session_id.clone());
            let last = session.as_deref().and_then(&last_state);
            let failed_turn = matches!(last, Some(AgentState::Errored | AgentState::Crashed));
            let mut failed = failed_turn;
            let state = match tab.as_ref().map(|tab| tab.alive) {
                Some(None) => SessionState::Offline,
                Some(Some(false)) => SessionState::Exited,
                Some(Some(true)) => {
                    match tab.as_ref().and_then(|tab| tab.activity) {
                        // The owner has seen no turn yet (an agent launched with no
                        // prompt waits at its prompt): what its launch and hooks
                        // recorded stands, when they recorded anything.
                        Some(SessionState::Unknown) if last.is_some() => session_state(true, last),
                        Some(activity) => {
                            failed = tab.as_ref().is_some_and(|tab| tab.activity_failed);
                            activity
                        }
                        None => session_state(true, last),
                    }
                }
                None => session_state(row.alive, last),
            };
            let subagents = tab.as_ref().map_or(0, |tab| tab.subagents);
            let (name, machine, machine_id, workspace_handle, workspace, tab_agent) = match tab {
                Some(tab) => (
                    tab.name,
                    Some(tab.machine),
                    tab.machine_id,
                    tab.workspace_handle,
                    tab.workspace,
                    tab.agent,
                ),
                None => (None, None, None, None, None, None),
            };
            SessionListRow {
                id: row.handle.clone(),
                team: None,
                state,
                display_name: name.or(row.meta.display_name).unwrap_or_else(|| {
                    codesense_pty_core::memorable_name::memorable_name_from_str(&row.handle)
                }),
                chat_workspace_id: row.chat_workspace_id,
                group_unread: row.group_unread,
                group_read_id: row.group_read_id,
                handle: row.handle,
                launched_by: row.meta.launched_by,
                story: row.meta.story,
                workspace: workspace.or(row.workspace),
                role: row.meta.role,
                queue_role: row.meta.queue_role,
                role_join_error: row.meta.role_join_error,
                avatar: row.meta.avatar,
                agent: row
                    .agent_revision
                    .as_deref()
                    .and_then(crate::agents::manifest::AgentRevisionRef::key_of)
                    .map(|key| key.to_string())
                    .or(tab_agent),
                machine,
                machine_id,
                workspace_handle,
                unread: Some(0),
                oldest_unread_at: None,
                mail_notice: None,
                notified_at: None,
                subagents,
                inbox_waiter: Default::default(),
                progress: None,
                launch_telemetry: None,
                release_qualified: None,
                release_note: None,
                deadline_at_ms: row.deadline_at_ms,
                expiring: row.expiring,
                session,
                failed,
            }
        })
        .collect();
    listed.sort_by(|a, b| a.handle.cmp(&b.handle));
    listed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_session_deadline_survives_the_mcp_projection_before_and_during_grace() {
        use crate::commands::dashboard_launch::{session_row, DashboardLaunchState, LaunchRecord};

        let state = DashboardLaunchState::new(1);
        state
            .begin_with_provenance(
                "worker",
                None,
                LaunchRecord {
                    max_runtime_ms: Some(60_000),
                    ..Default::default()
                },
            )
            .unwrap();
        state.bind("worker", "sess-worker", Some("mcp")).unwrap();
        let deadline = state.deadline_snapshot()[0].deadline_ms;
        let wire = || {
            let rows = state
                .snapshot()
                .into_iter()
                .map(|s| session_row(s, true))
                .collect();
            serde_json::to_value(session_list_rows_with_tabs(rows, |_| None, None, |_| None))
                .unwrap()
        };

        let before = wire();
        assert_eq!(before[0]["deadlineAtMs"], deadline);
        assert!(before[0].get("expiring").is_none());

        assert!(state.mark_exit_sent(&state.deadline_snapshot()[0], deadline));
        let during = wire();
        assert_eq!(during[0]["deadlineAtMs"], deadline);
        assert_eq!(during[0]["expiring"], true);
    }

    #[test]
    fn a_session_stuck_on_a_startup_screen_lists_needs_you_with_its_kind() {
        use crate::live_hub::effective_status::join_list_rows;
        let listed = |status: &str, kind: serde_json::Value, state: &str| {
            let mut rows = vec![serde_json::json!({"session":"s","handle":"worker","state":state})];
            let owner =
                [serde_json::json!({"id":"s","effectiveStatus":status,"needsYouKind":kind})];
            join_list_rows(&mut rows, &owner);
            rows.remove(0)
        };
        let stuck = listed("dropped", "startup".into(), "unknown");
        assert_eq!(stuck["state"], "needs-you");
        assert_eq!(stuck["needsYouKind"], "startup");
        assert_eq!(stuck["effectiveStatus"], "dropped");
        // Ready (no kind), or exited: the host's own state stands and no kind is listed.
        let ready = listed("idle", serde_json::Value::Null, "idle");
        assert_eq!(ready["state"], "idle");
        assert!(ready.get("needsYouKind").is_none());
        assert_eq!(
            listed("done", "startup".into(), "exited")["state"],
            "exited"
        );
        // Another kind is listed without rewriting the host's state.
        let failed = listed("failed", "failed".into(), "idle");
        assert_eq!(failed["state"], "idle");
        assert_eq!(failed["needsYouKind"], "failed");
    }

    #[test]
    fn unbounded_session_omits_deadline_fields_from_the_mcp_projection() {
        use crate::commands::dashboard_launch::{session_row, DashboardLaunchState};

        let state = DashboardLaunchState::new(1);
        state.begin("worker").unwrap();
        state.bind("worker", "sess-worker", Some("mcp")).unwrap();
        let rows = state
            .snapshot()
            .into_iter()
            .map(|s| session_row(s, true))
            .collect();
        let wire =
            serde_json::to_value(session_list_rows_with_tabs(rows, |_| None, None, |_| None))
                .unwrap();
        assert_eq!(wire[0]["handle"], "worker");
        assert!(wire[0].get("deadlineAtMs").is_none());
        assert!(wire[0].get("expiring").is_none());
    }

    #[test]
    fn an_agent_at_its_prompt_before_its_first_turn_lists_idle_not_unknown() {
        let state = crate::commands::dashboard_launch::DashboardLaunchState::new(1);
        state
            .begin_with_provenance("SW-1#bubbles", None, Default::default())
            .unwrap();
        state.bind("SW-1#bubbles", "sess-1", Some("mcp")).unwrap();
        let row = crate::commands::dashboard_launch::session_row(state.snapshot().remove(0), true);
        let tab = |activity| {
            Some(TabView {
                name: Some("bubbles".into()),
                machine_id: None,
                workspace_handle: None,
                machine: "This Mac".into(),
                workspace: None,
                session: Some("sess-1".into()),
                alive: Some(true),
                activity,
                activity_failed: false,
                subagents: 0,
                agent: None,
            })
        };
        let list = |last: Option<AgentState>, activity| {
            session_list_rows_with_tabs(vec![row.clone()], |_| last, None, |_| tab(activity))
                .remove(0)
        };
        // Launched with no prompt: the owner saw no turn, the launch recorded it ready.
        let ready = list(Some(AgentState::Done), Some(SessionState::Unknown));
        assert_eq!(ready.state, SessionState::Idle);
        assert_eq!(ready.display_name, "bubbles");
        // Nothing recorded either: still unknown, never invented.
        assert_eq!(
            list(None, Some(SessionState::Unknown)).state,
            SessionState::Unknown
        );
        // An observed owner state wins over the recorded one.
        assert_eq!(
            list(Some(AgentState::Done), Some(SessionState::Running)).state,
            SessionState::Running
        );
    }

    #[test]
    fn a_remote_tabs_agent_key_names_the_agent_when_the_registry_has_none() {
        let state = crate::commands::dashboard_launch::DashboardLaunchState::new(1);
        state
            .begin_with_provenance("SW-1#barb", None, Default::default())
            .unwrap();
        state.bind("SW-1#barb", "sess-1", Some("mcp")).unwrap();
        let row = crate::commands::dashboard_launch::session_row(state.snapshot().remove(0), true);
        let tab = |agent: Option<&str>| TabView {
            name: None,
            machine_id: None,
            workspace_handle: None,
            machine: "build-box".into(),
            workspace: None,
            session: Some("sess-1".into()),
            alive: Some(true),
            activity: Some(SessionState::Idle),
            activity_failed: false,
            subagents: 0,
            agent: agent.map(str::to_string),
        };
        let agent_of = |agent: Option<&str>| {
            session_list_rows_with_tabs(vec![row.clone()], |_| None, None, |_| Some(tab(agent)))
                .remove(0)
                .agent
        };
        assert_eq!(
            agent_of(Some("acme/api/backend/barb")).as_deref(),
            Some("acme/api/backend/barb")
        );
        assert_eq!(agent_of(None), None);
    }

    #[test]
    fn a_session_with_no_recorded_name_falls_back_to_a_memorable_label_not_its_raw_handle() {
        let state = crate::commands::dashboard_launch::DashboardLaunchState::new(1);
        let handle = "term-tab-1791084011927390000-0";
        state.register(handle, "sess-anon", Some("codesense"));
        let row = crate::commands::dashboard_launch::session_row(state.snapshot().remove(0), true);
        let listed = session_list_rows_with_tabs(vec![row], |_| None, None, |_| None).remove(0);
        assert_ne!(listed.display_name, handle);
        let expected = codesense_pty_core::memorable_name::memorable_name_from_str(handle);
        assert_eq!(listed.display_name, expected);
        let words: Vec<&str> = listed.display_name.split(' ').collect();
        assert_eq!(words.len(), 2, "{:?}", listed.display_name);
    }

    #[test]
    fn each_row_carries_its_own_unread_mail_and_none_reads_zero_and_null() {
        use crate::commands::agent_inbox::SessionUnread;
        use crate::commands::dashboard_launch::LaunchTeam;
        let state = crate::commands::dashboard_launch::DashboardLaunchState::new(2);
        state.register("SW-1#mail", "sess-mail", Some("codesense"));
        state.register("SW-1#quiet", "sess-quiet", Some("codesense"));
        state.register("SW-1#lost", "sess-lost", Some("codesense"));
        let rows = state
            .snapshot()
            .into_iter()
            .map(|e| crate::commands::dashboard_launch::session_row(e, true))
            .collect();
        let mut listed = session_list_rows_with_tabs(rows, |_| None, None, |_| None);
        let found = std::collections::HashMap::from([(
            "SW-1#mail".to_string(),
            SessionUnread {
                count: 3,
                oldest_at_ms: Some(1_790_000_000_123),
                newest_id: Some(9),
                team: LaunchTeam {
                    org: "acme".into(),
                    team: "default".into(),
                },
                announced: None,
            },
        )]);
        let unread = crate::commands::agent_inbox::UnreadByHandle {
            found,
            failed: ["SW-1#lost".to_string()].into(),
        };
        with_unread(&mut listed, &unread);
        let json = serde_json::to_value(&listed).unwrap();
        let mail = json
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["handle"] == "SW-1#mail")
            .unwrap();
        assert_eq!(mail["unread"], 3);
        assert_eq!(mail["oldestUnreadAt"], "2026-09-21T14:13:20.123Z");
        let quiet = json
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["handle"] == "SW-1#quiet")
            .unwrap();
        assert_eq!(quiet["unread"], 0);
        assert!(
            quiet["oldestUnreadAt"].is_null(),
            "null, not omitted: {quiet}"
        );
        // An inbox that could not be read is unknown, not empty.
        let lost = json
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["handle"] == "SW-1#lost")
            .unwrap();
        assert!(lost["unread"].is_null(), "{lost}");
    }

    #[test]
    fn each_row_says_whether_its_agent_was_told_about_its_mail_and_can_be_woken() {
        use crate::commands::agent_inbox::Announced;
        use crate::commands::dashboard_launch::LaunchTeam;
        let state = crate::commands::dashboard_launch::DashboardLaunchState::new(3);
        for (handle, session) in [
            ("told", "s-told"),
            ("pending", "s-pending"),
            ("none", "s-none"),
        ] {
            state.register(handle, session, Some("codesense"));
        }
        let rows = state
            .snapshot()
            .into_iter()
            .map(|e| crate::commands::dashboard_launch::session_row(e, true))
            .collect();
        let mut listed = session_list_rows_with_tabs(rows, |_| None, None, |_| None);
        let mail = |newest, through: Option<i64>, session: &str| {
            crate::commands::agent_inbox::SessionUnread {
                count: 1,
                oldest_at_ms: Some(1),
                newest_id: Some(newest),
                team: LaunchTeam {
                    org: "acme".into(),
                    team: "default".into(),
                },
                announced: through.map(|through_id| Announced {
                    through_id,
                    at_ms: 1_790_000_000_123,
                    session: session.to_string(),
                }),
            }
        };
        let unread = crate::commands::agent_inbox::UnreadByHandle {
            found: std::collections::HashMap::from([
                ("told".to_string(), mail(9, Some(9), "s-told")),
                ("pending".to_string(), mail(9, Some(4), "s-pending")),
                // Told in an earlier session under the handle: this one was not.
                ("none".to_string(), mail(9, Some(9), "s-earlier")),
            ]),
            failed: Default::default(),
        };
        with_unread(&mut listed, &unread);
        with_inbox_waiters(&mut listed, |handle| match handle {
            "told" => InboxWaiter::Armed,
            "none" => InboxWaiter::Unknown,
            _ => InboxWaiter::None,
        });
        let json = serde_json::to_value(&listed).unwrap();
        let row = |handle: &str| {
            json.as_array()
                .unwrap()
                .iter()
                .find(|r| r["handle"] == handle)
                .unwrap()
                .clone()
        };
        // Told about everything it has unread, and armed to be woken by the next mail.
        assert_eq!(row("told")["mailNotice"], "told");
        assert_eq!(row("told")["notifiedAt"], "2026-09-21T14:13:20.123Z");
        assert_eq!(row("told")["inboxWaiter"], true);
        // Unread mail newer than what it was told about, and no waiter to wake it.
        assert_eq!(row("pending")["mailNotice"], "pending");
        assert_eq!(row("pending")["inboxWaiter"], false);
        // Mail an earlier session under the handle was told about: this session was not.
        assert_eq!(row("none")["mailNotice"], "pending");
        assert!(row("none")["notifiedAt"].is_null());
        // A tab on a machine that is not connected: neither armed nor not.
        assert_eq!(row("none")["inboxWaiter"], "unknown");
    }

    #[test]
    fn a_self_reported_block_lists_needs_you_without_host_activity() {
        let state = crate::commands::dashboard_launch::DashboardLaunchState::new(1);
        state.register("SW-1#blocked", "sess-blocked", Some("codesense"));
        let row = crate::commands::dashboard_launch::session_row(state.snapshot().remove(0), true);
        let listed =
            session_list_rows_with_tabs(vec![row], |_| Some(AgentState::Blocked), None, |_| None)
                .remove(0);
        assert_eq!(listed.state, SessionState::NeedsYou);
        assert_eq!(serde_json::to_value(&listed).unwrap()["state"], "needs-you");
        assert!(!listed.failed);
    }

    #[test]
    fn an_errored_or_crashed_turn_lists_idle_but_is_marked_failed() {
        let state = crate::commands::dashboard_launch::DashboardLaunchState::new(1);
        state.register("SW-1#a", "sess-a", Some("codesense"));
        let row = crate::commands::dashboard_launch::session_row(state.snapshot().remove(0), true);
        let list = |last: Option<AgentState>| {
            session_list_rows_with_tabs(vec![row.clone()], |_| last, None, |_| None).remove(0)
        };
        for (last, failed) in [
            (AgentState::Done, false),
            (AgentState::Errored, true),
            (AgentState::Crashed, true),
        ] {
            let listed = list(Some(last));
            assert_eq!(listed.state, SessionState::Idle);
            assert_eq!(listed.failed, failed, "{last:?}");
        }
        // The owner's own observation decides when it has one.
        let tab = |activity_failed| TabView {
            name: None,
            machine_id: None,
            workspace_handle: None,
            machine: "This Mac".into(),
            workspace: None,
            session: Some("sess-a".into()),
            alive: Some(true),
            activity: Some(SessionState::Idle),
            activity_failed,
            subagents: 0,
            agent: None,
        };
        let by_owner = |f| {
            session_list_rows_with_tabs(vec![row.clone()], |_| None, None, |_| Some(tab(f)))
                .remove(0)
                .failed
        };
        assert!(by_owner(true));
        assert!(!by_owner(false));
    }

    /// An agent idle at its prompt while its subagents run is `idle` with `subagents: N`;
    /// with none running the key is absent.
    #[test]
    fn subagents_show_beside_the_agents_own_state() {
        let state = crate::commands::dashboard_launch::DashboardLaunchState::new(1);
        state.register("SW-1#a", "sess-a", Some("codesense"));
        let row = crate::commands::dashboard_launch::session_row(state.snapshot().remove(0), true);
        let listed = |subagents| {
            let tab = TabView {
                name: None,
                machine_id: None,
                workspace_handle: None,
                machine: "This Mac".into(),
                workspace: None,
                session: Some("sess-a".into()),
                alive: Some(true),
                activity: Some(SessionState::Idle),
                activity_failed: false,
                subagents,
                agent: None,
            };
            let rows = session_list_rows_with_tabs(
                vec![row.clone()],
                |_| None,
                None,
                |_| Some(tab.clone()),
            );
            serde_json::to_value(&rows[0]).unwrap()
        };
        let busy = listed(2);
        assert_eq!(
            (busy["state"].as_str(), busy["subagents"].as_u64()),
            (Some("idle"), Some(2))
        );
        assert!(listed(0).get("subagents").is_none());
    }

    /// A session's latest `signal_progress` shows on its row (the `list_sessions` row a
    /// dashboard's `sessions()` returns too); a session that reported none has no key.
    #[test]
    fn each_row_carries_its_latest_signal_progress() {
        let state = crate::commands::dashboard_launch::DashboardLaunchState::new(2);
        state.register("SW-1#busy", "sess-busy", Some("codesense"));
        state.register("SW-1#quiet", "sess-quiet", Some("codesense"));
        let rows: Vec<_> = state
            .snapshot()
            .into_iter()
            .map(|r| crate::commands::dashboard_launch::session_row(r, true))
            .collect();
        let mut listed = session_list_rows_with_tabs(rows, |_| None, None, |_| None);
        let hub = crate::dashboard_stream::DashboardStreamHub::new();
        hub.on_session_progress("sess-busy", "dev", Some(1), Some(5), None);
        hub.on_session_progress(
            "sess-busy",
            "review",
            Some(2),
            Some(5),
            Some("tests".into()),
        );
        hub.on_session_progress("sess-quiet", "not-a-phase", None, None, None);
        with_progress(&mut listed, |session| hub.latest_progress(session));

        let wire = serde_json::to_value(&listed).unwrap();
        let busy = wire
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["handle"] == "SW-1#busy")
            .unwrap();
        let progress = &busy["progress"];
        assert_eq!(progress["phase"], "review", "the latest report wins");
        assert_eq!(progress["step"], 2);
        assert_eq!(progress["total"], 5);
        assert_eq!(progress["note"], "tests");
        assert!(progress["atMs"].as_i64().unwrap() > 0);
        assert!(
            busy.get("session").is_none(),
            "the tab session stays internal"
        );
        let quiet = wire
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["handle"] == "SW-1#quiet")
            .unwrap();
        assert!(
            quiet.get("progress").is_none(),
            "an unknown phase is not kept"
        );
    }

    #[test]
    fn state_maps_liveness_then_last_hook_state() {
        use AgentState::*;
        assert_eq!(session_state(false, Some(Working)), SessionState::Exited);
        assert_eq!(session_state(false, None), SessionState::Exited);
        assert_eq!(session_state(true, None), SessionState::Running);
        assert_eq!(session_state(true, Some(Thinking)), SessionState::Running);
        assert_eq!(session_state(true, Some(Working)), SessionState::Running);
        assert_eq!(session_state(true, Some(Blocked)), SessionState::NeedsYou);
        assert_eq!(session_state(false, Some(Blocked)), SessionState::Exited);
        assert_eq!(
            session_state(true, Some(NeedsPermission)),
            SessionState::NeedsYou
        );
        assert_eq!(session_state(true, Some(Done)), SessionState::Idle);
        assert_eq!(session_state(true, Some(Errored)), SessionState::Idle);
        assert_eq!(
            serde_json::to_value(SessionState::NeedsYou).unwrap(),
            serde_json::json!("needs-you")
        );
    }

    /// A row whose session has a finalized launch record surfaces
    /// `launchTelemetry` as part of the JSON; a row without one omits the key.
    #[test]
    fn rows_surface_launch_telemetry_when_the_tracker_has_a_record() {
        use crate::commands::launch_telemetry::{
            LaunchMode, LaunchOutcome, LaunchTelemetryBuilder,
        };
        use std::time::Duration;

        let state = crate::commands::dashboard_launch::DashboardLaunchState::new(2);
        state.register("SW-1#timed", "sess-timed", Some("codesense"));
        state.register("SW-1#quiet", "sess-quiet", Some("codesense"));
        let rows: Vec<_> = state
            .snapshot()
            .into_iter()
            .map(|e| crate::commands::dashboard_launch::session_row(e, true))
            .collect();
        let mut listed = session_list_rows_with_tabs(rows, |_| None, None, |_| None);

        // Build a telemetry record keyed by `sess-timed`. Shell-mode: the
        // prompt renders BEFORE the launch line is injected (echo guard in
        // `observe_first_byte` drops post-inject bytes).
        let mut builder = LaunchTelemetryBuilder::start(
            "sess-timed".into(),
            "builtin/claude".into(),
            LaunchMode::Shell,
        );
        builder.observe_first_byte(builder.started_at() + Duration::from_millis(45));
        builder.observe_shell_injected(builder.started_at() + Duration::from_millis(80));
        builder.observe_agent_live(builder.started_at() + Duration::from_millis(410));
        let record = builder.finish(LaunchOutcome::Succeeded, None);

        with_launch_telemetry(&mut listed, |key| {
            if key == "sess-timed" {
                Some(record.clone())
            } else {
                None
            }
        });

        let json = serde_json::to_value(&listed).unwrap();
        let timed = json
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["handle"] == "SW-1#timed")
            .unwrap();
        assert_eq!(timed["launchTelemetry"]["launchMode"], "shell");
        assert_eq!(timed["launchTelemetry"]["metricsVersion"], 3);
        assert_eq!(timed["launchTelemetry"]["outcome"], "succeeded");
        assert_eq!(timed["launchTelemetry"]["ptyFirstByteMs"], 45);
        assert_eq!(timed["launchTelemetry"]["settleMs"], 80);
        assert_eq!(timed["launchTelemetry"]["agentLiveMs"], 410);

        let quiet = json
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["handle"] == "SW-1#quiet")
            .unwrap();
        assert!(
            quiet.get("launchTelemetry").is_none(),
            "a row without a record omits the key, so pre-F11 sessions read clean"
        );
    }

    /// A row is marked unqualified only when its launch recorded a release of a known CLI
    /// that is outside that CLI's range: a qualified, unknown-binary or version-less launch
    /// leaves the keys out.
    #[test]
    fn rows_say_release_unqualified_only_for_a_known_out_of_range_release() {
        use crate::commands::launch_telemetry::{
            LaunchMode, LaunchOutcome, LaunchTelemetryBuilder,
        };

        let state = crate::commands::dashboard_launch::DashboardLaunchState::new(4);
        let cases = [
            ("old", "/bin/opencode", Some("2.0.1")),
            ("fine", "/bin/opencode", Some("2.1.4")),
            ("odd", "/bin/mystery", Some("9.9.9")),
            ("blank", "/bin/opencode", None),
            // Every listed CLI is told by its binary, Hermes as any other.
            ("hermes-old", "/bin/hermes", Some("0.22.4")),
            ("hermes-fine", "/bin/hermes", Some("0.22.5")),
        ];
        for (name, _, _) in cases {
            state.register(
                &format!("SW-1#{name}"),
                &format!("sess-{name}"),
                Some("codesense"),
            );
        }
        let rows: Vec<_> = state
            .snapshot()
            .into_iter()
            .map(|e| crate::commands::dashboard_launch::session_row(e, true))
            .collect();
        let mut listed = session_list_rows_with_tabs(rows, |_| None, None, |_| None);
        // Through the real tracker and the production join, so dropping `with_release` fails.
        let tracker = crate::commands::launch_telemetry::LaunchTelemetryTracker::new();
        for (name, path, version) in cases {
            let mut builder = LaunchTelemetryBuilder::start(
                format!("sess-{name}"),
                "w4-oc".into(),
                LaunchMode::Argv,
            );
            builder.set_binary(std::path::Path::new(path), version);
            tracker.complete(builder.finish(LaunchOutcome::Succeeded, None));
        }
        with_launch_telemetry_batched(&mut listed, &tracker);
        let json = serde_json::to_value(&listed).unwrap();
        let find = |name: &str| {
            json.as_array()
                .unwrap()
                .iter()
                .find(|r| r["handle"] == format!("SW-1#{name}"))
                .unwrap()
                .clone()
        };
        for (name, release) in [("old", "2.0.1"), ("hermes-old", "0.22.4")] {
            let row = find(name);
            assert_eq!(row["releaseQualified"], false, "{name}: {row}");
            let note = row["releaseNote"].as_str().unwrap();
            assert!(
                note.contains(release) && note.contains("supported:"),
                "{note}"
            );
        }
        for name in ["fine", "odd", "blank", "hermes-fine"] {
            let row = find(name);
            assert!(row.get("releaseQualified").is_none(), "{name}: {row}");
            assert!(row.get("releaseNote").is_none(), "{name}: {row}");
        }
    }

    /// The enrichment keys by the row's tab session, falling back
    /// to its handle. A launch registered under its session id reaches a row
    /// whose session the registry already knows.
    #[test]
    fn launch_telemetry_lookup_prefers_the_tab_session_over_the_handle() {
        use crate::commands::launch_telemetry::{
            LaunchMode, LaunchOutcome, LaunchTelemetryBuilder,
        };

        let state = crate::commands::dashboard_launch::DashboardLaunchState::new(1);
        state.register("SW-1#a", "sess-a", Some("codesense"));
        let row = crate::commands::dashboard_launch::session_row(state.snapshot().remove(0), true);
        let mut listed = session_list_rows_with_tabs(vec![row], |_| None, None, |_| None);

        let session_record = LaunchTelemetryBuilder::start(
            "sess-a".into(),
            "builtin/claude".into(),
            LaunchMode::Argv,
        )
        .finish(LaunchOutcome::Succeeded, None);

        // Only the session id has a record; the handle does not. Row still
        // picks it up because the lookup tries the session first.
        with_launch_telemetry(&mut listed, |key| {
            if key == "sess-a" {
                Some(session_record.clone())
            } else {
                None
            }
        });

        assert_eq!(
            listed[0].launch_telemetry.as_ref().map(|t| t.launch_mode),
            Some(LaunchMode::Argv)
        );
    }
    #[test]
    fn review_trail_live_projection_exposes_the_current_stable_session() {
        use crate::commands::dashboard_launch::{session_row, DashboardLaunchState, LaunchRecord};
        let state=DashboardLaunchState::new(1);state.begin_with_provenance("reusable",None,LaunchRecord::default()).unwrap();
        for expected in [None,Some("current-session")] {
            if let Some(id)=expected {state.bind("reusable",id,Some("mcp")).unwrap();}
            let raw=state.snapshot().into_iter().map(|s|session_row(s,true)).collect();
            let mut rows=session_list_rows_with_tabs(raw, |_|None,None, |_|None);
            rows[0].agent=Some("acme/dev/builder".into());
            let page=serde_json::to_value(&rows).unwrap();
            let mcp=crate::live_hub::LiveHub::default().with_effective_status(&rows);
            let agent=serde_json::to_value(crate::commands::agent_read_model::instances_of(&rows,"acme/dev/builder")).unwrap();
            for value in [&page,&mcp,&agent] {assert_eq!(value[0]["id"],"reusable");assert_eq!(value[0]["sessionId"].as_str(),expected);}
            if expected.is_none(){assert!(page[0].get("sessionId").is_none());assert!(agent[0].get("sessionId").is_none());}
            #[cfg(feature="dashboard-publish")]
            assert!(crate::dashboard_publish::catalog::session_value(&rows[0],&[]).get("sessionId").is_none());
        }
    }

}
