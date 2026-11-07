// SPDX-License-Identifier: MIT
//! Cached live views shared by native surfaces, pages and tools.
pub mod agent_definitions;
pub mod agent_list;
pub mod agent_marks;
pub mod configuration;
pub(crate) mod effective_status;
pub use agent_marks::*;
pub mod files;
pub mod inbox;
pub mod inbox_events;
#[cfg(test)]
mod inbox_offlock_tests;
pub(crate) mod machine_stats;
pub mod pane_sources;
pub mod projection;
pub mod run_review;
#[cfg(test)]
mod spend_offlock_tests;
pub mod stories;
#[cfg(test)]
mod tests;
pub mod view_key;

use crate::commands::{
    agent_inbox::{AppInboxHost, UnreadByHandle},
    dashboard_launch::DashboardLaunchState,
};
use crate::mcp::{
    session_list::{self, SessionListRow},
    terminal_tabs::LiveTabs,
};
use projection::{FieldProviders, Snapshot, SubscriptionReply, TopicRegistry};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{
    atomic::{AtomicU8, Ordering},
    Mutex,
};
use std::time::Duration;
use tauri::{Emitter, Listener, Manager};

fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

pub const COALESCE: Duration = Duration::from_millis(50);
const REBUILD: u8 = 1;
const MACHINE_STATS: u8 = 32;
const DEFINITIONS: u8 = 16;
const FILES: u8 = 64;
const INBOX: u8 = 2;
const HOSTS: u8 = 4;
const FOOTPRINT: u8 = 8;
const DOMAIN_EVENTS: &[&str] = &[
    crate::mcp::agent_dot::TERMINAL_STATUS_EVENT,
    "sessions-waiter-changed",
    "routines:changed",
    "session-state-changed",
    "agent-state-changed",
    "agents-changed",
    "agents:changed",
    "wv-roster-changed",
    "content-changed",
    "codesense:workspace-chat-updated",
    "terminal-layout-changed",
    "terminal-layout-resync",
    "sessions-source-changed",
    "terminal-layout-admission-changed",
    "machines-changed",
    "pty-exit",
    "mcp:agent-progress",
    "mcp:agent-heartbeat",
    "mcp:agent-message",
    "mcp:agent-done",
    "mcp:ask-user",
    "mcp:ask-user-closed",
    "context-index-changed",
    "context-attachments-changed",
    crate::commands::registry_local::ORG_REGISTRY_CHANGED_EVENT,
    crate::commands::scratch_use::SCRATCH_USE_CHANGED_EVENT,
    crate::commands::agent_accounts::LIFECYCLE_EVENT,
    crate::commands::agent_accounts::LOGIN_EVENT,
];
fn definition_membership_event(event: &str) -> bool {
    matches!(
        event,
        "context-index-changed"
            | "context-attachments-changed"
            | crate::commands::registry_local::ORG_REGISTRY_CHANGED_EVENT
            | crate::commands::scratch_use::SCRATCH_USE_CHANGED_EVENT
    )
}
fn event_flags(event: &str) -> u8 {
    REBUILD
        | if matches!(
            event,
            "machines-changed" | "terminal-layout-admission-changed"
        ) {
            MACHINE_STATS
        } else {
            0
        }
        | if matches!(
            event,
            "agents-changed"
                | "agents:changed"
                | "routines:changed"
                | crate::commands::agent_accounts::LIFECYCLE_EVENT
                | crate::commands::agent_accounts::LOGIN_EVENT
        ) || definition_membership_event(event)
        {
            DEFINITIONS
        } else {
            0
        }
        | if matches!(
            event,
            "context-attachments-changed"
                | crate::commands::registry_local::ORG_REGISTRY_CHANGED_EVENT
        ) {
            FOOTPRINT
        } else {
            0
        }
        | if event == "codesense:workspace-chat-updated" {
            INBOX
        } else {
            0
        }
        | if matches!(
            event,
            "terminal-layout-changed"
                | "terminal-layout-resync"
                | "sessions-source-changed"
                | "terminal-layout-admission-changed"
                | "machines-changed"
                | "pty-exit"
                | "session-state-changed"
                | "agent-state-changed"
                | "terminal-status-update"
                | "mcp:agent-progress"
                | "mcp:agent-done"
                | "mcp:ask-user-closed"
        ) {
            HOSTS
        } else {
            0
        }
}
type ActivityRead = Result<Vec<codesense_pty_core::activity::Snapshot>, String>;
#[derive(Default)]
struct Model {
    machine_stats_watches:
        BTreeMap<String, std::sync::Arc<crate::terminal::session_server::SessionServerClient>>,
    rows: Vec<SessionListRow>,
    agent_definitions: agent_definitions::Registry,
    configuration: configuration::Registry,
    processes: BTreeMap<String, String>,
    agent_rows: Vec<agent_list::AgentListRow>,
    spend_rows: Vec<serde_json::Value>,
    agent_views: BTreeSet<String>,
    agent_ready: bool,
    agent_home_team: String,
    agent_team_order: Vec<String>,
    agent_team_names: serde_json::Value,
    agent_pane: serde_json::Value,
    agent_workspace_rollups: serde_json::Value,
    projections: TopicRegistry,
    pane_sources: pane_sources::Registry,
    files: files::Files,
    nav_files: BTreeMap<String, stories::NativeFiles>,
    inbox: std::sync::Arc<inbox::InboxProjection>,
    inbox_pending: BTreeMap<String, (crate::commands::dashboard_launch::LaunchTeam, BTreeSet<i64>)>,
    activity_captured_at: BTreeMap<String, u64>,
    status_request_id: Option<String>,
    stall_deadline: Option<tokio::time::Instant>,
    hosts: BTreeMap<String, (crate::commands::machines::RuntimeLayoutReply, ActivityRead)>,
    reading: HashSet<String>,
    refresh_again: HashSet<String>,
    labels: BTreeMap<String, String>,
}
impl Model {
    fn live_tabs(&self) -> LiveTabs {
        let replies: Vec<_> = self
            .hosts
            .values()
            .map(|(reply, _)| reply.clone())
            .collect();
        let mut live = LiveTabs::of(&replies, self.labels.clone());
        live.activity = self
            .hosts
            .values()
            .filter_map(|(reply, activity)| {
                if let crate::commands::machines::RuntimeLayoutReply::Live { target, .. } = reply {
                    Some((target.clone(), activity.clone()))
                } else {
                    None
                }
            })
            .collect();
        live
    }
    fn begin_refresh(&mut self, key: &str) -> bool {
        if self.reading.insert(key.to_owned()) {
            return true;
        }
        self.refresh_again.insert(key.to_owned());
        false
    }
    fn finish_refresh(&mut self, key: &str) -> bool {
        if self.refresh_again.remove(key) {
            return true;
        }
        self.reading.remove(key);
        false
    }
}
pub struct LiveHub {
    model: Mutex<Model>,
    dirty: AtomicU8,
    wake: std::sync::Arc<tokio::sync::Notify>,
    run_review: std::sync::Arc<Mutex<run_review::Owner>>,
    changes: tokio::sync::watch::Sender<u64>,
    providers: FieldProviders,
}
impl Default for LiveHub {
    fn default() -> Self {
        let mut providers = FieldProviders::new([
            "id",
            "handle",
            "displayName",
            "state",
            "machine",
            "machineId",
            "workspace",
            "team",
            "launchedBy",
            "story",
            "chatWorkspaceId",
            "workspaceHandle",
            "deadlineAtMs",
            "expiring",
            "outputRunId",
            "kind",
            "sessionId",
            "alive",
            "status",
            "failed",
            "backgrounded",
            "startedWith",
            "startedAtMs",
            "launcher",
            "lastOutputMs",
            "provenance",
            "accountProvenance",
            "agentRevision",
            "launchTeamKey",
            "detected",
            "names",
            "rowNames",
            "model",
            "pendingAsk",
        ]);
        providers.reserve(inbox::INBOX_FIELDS.iter().copied());
        crate::agents::session_fields::register(&mut providers);
        agent_list::fields::register(&mut providers);
        effective_status::register(&mut providers);
        Self {
            model: Mutex::new(Model::default()),
            dirty: AtomicU8::new(0),
            wake: std::sync::Arc::new(tokio::sync::Notify::new()),
            run_review: Default::default(),
            changes: tokio::sync::watch::channel(0).0,
            providers,
        }
    }
}
impl LiveHub {
    pub(crate) fn note_process_probe(
        &self,
        probes: &[(String, String, Option<bool>)],
    ) -> Vec<String> {
        let mut missing = Vec::new();
        let mut model = self.model.lock().unwrap();
        let before = model.processes.clone();
        let sessions: BTreeSet<_> = probes.iter().map(|(sid, _, _)| sid).collect();
        for sid in sessions {
            let results: Vec<_> = probes.iter().filter(|(id, _, _)| id == sid).collect();
            if let Some((_, cli, _)) = results.iter().find(|(_, _, alive)| *alive == Some(true)) {
                model.processes.insert(sid.clone(), cli.clone());
            } else if results.iter().all(|(_, _, alive)| *alive == Some(false)) {
                model.processes.remove(sid);
                missing.push(sid.clone());
            }
        }
        let changed = before != model.processes;
        drop(model);
        if changed {
            self.invalidate(REBUILD);
        }
        missing
    }
    pub(crate) fn invalidate_sessions(&self) {
        self.invalidate(REBUILD | HOSTS);
    }
    fn invalidate(&self, flags: u8) {
        self.dirty.fetch_or(flags, Ordering::Release);
        self.wake.notify_one();
    }
    async fn next_batch(&self) -> u8 {
        let deadline = self.model.lock().unwrap().stall_deadline;
        tokio::select! {
            () = self.wake.notified() => {},
            () = async { match deadline { Some(at) => tokio::time::sleep_until(at).await, None => std::future::pending().await } } => {
                self.model.lock().unwrap().stall_deadline = None;
                self.dirty.fetch_or(REBUILD | HOSTS, Ordering::Release);
            }
        }
        tokio::time::sleep(COALESCE).await;
        self.dirty.swap(0, Ordering::AcqRel)
    }
    #[cfg(feature = "bench-support")]
    pub(crate) fn seed_rows(&self, rows: Vec<SessionListRow>) {
        self.model.lock().unwrap().rows = rows;
    }
    pub fn changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changes.subscribe()
    }
    /// One topic's current view, as a page's first paint or a host read sees it.
    pub fn snapshot(&self, key: &str, topic: &str) -> Result<Snapshot, String> {
        let mut model = self.model.lock().unwrap();
        if topic == "agentList" && model.agent_views.insert(key.to_owned()) {
            project_agent_views(&mut model)?;
        }
        model.projections.snapshot(topic, key)
    }
    pub(crate) fn with_effective_status(&self, source: &[SessionListRow]) -> serde_json::Value {
        let owner = self.snapshot("all", "sessions").unwrap().snapshot;
        let mut rows = serde_json::to_value(source).unwrap();
        for (wire, source) in rows.as_array_mut().unwrap().iter_mut().zip(source) {
            wire["sessionId"] = serde_json::json!(source.session);
        }
        effective_status::join_list_rows(rows.as_array_mut().unwrap(), &owner);
        rows
    }
    /// Whether the sessions projection shows `session` waiting on a startup screen.
    pub(crate) fn startup_stuck(&self, session: &str) -> bool {
        self.snapshot("all", "sessions").is_ok_and(|view| {
            view.snapshot
                .iter()
                .any(|row| row["sessionId"] == session && row["needsYouKind"] == "startup")
        })
    }
    pub(crate) fn chat_unread(&self, handle: &str) -> (u32, i64) {
        let inbox = {
            let model = self.model.lock().unwrap();
            std::sync::Arc::clone(&model.inbox)
        };
        let fields = inbox.session_fields.get(handle);
        (
            fields.and_then(|v| v["groupUnread"].as_u64()).unwrap_or(0) as u32,
            fields.and_then(|v| v["groupReadId"].as_i64()).unwrap_or(0),
        )
    }
    pub(crate) fn human_unread_total(&self) -> u32 {
        let inbox = {
            let model = self.model.lock().unwrap();
            std::sync::Arc::clone(&model.inbox)
        };
        inbox.human_unread_total()
    }
    pub(crate) fn inbox_view(
        &self,
        team: &crate::commands::dashboard_launch::LaunchTeam,
    ) -> Result<crate::commands::agent_inbox::InboxView, String> {
        let inbox = {
            let model = self.model.lock().unwrap();
            if !model.inbox.contains_team(team) {
                return Err("inbox projection is loading".into());
            }
            std::sync::Arc::clone(&model.inbox)
        };
        Ok(inbox.view_parts(team).build())
    }

    pub(crate) fn rows(&self, launched_by: Option<&str>) -> (Vec<SessionListRow>, UnreadByHandle) {
        let (mut rows, inbox) = {
            let model = self.model.lock().unwrap();
            let rows: Vec<_> = model
                .rows
                .iter()
                .filter(|r| {
                    launched_by.is_none_or(|parent| r.launched_by.as_deref() == Some(parent))
                })
                .cloned()
                .collect();
            (rows, std::sync::Arc::clone(&model.inbox))
        };
        inbox.join(&mut rows);
        (rows, inbox::InboxProjection::unread(&*inbox))
    }
}
pub(crate) fn read(
    app: &tauri::AppHandle,
    launched_by: Option<&str>,
) -> Result<(Vec<SessionListRow>, UnreadByHandle), String> {
    let key = launched_by
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| app.state::<DashboardLaunchState>().normalize_launched_by(s));
    Ok(app
        .try_state::<LiveHub>()
        .ok_or("live hub unavailable")?
        .rows(key.as_deref()))
}

fn listen_sources<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    for &event in DOMAIN_EVENTS {
        let handle = app.clone();
        app.listen(event, move |change| {
            if definition_membership_event(event) {
                handle
                    .state::<LiveHub>()
                    .model
                    .lock()
                    .unwrap()
                    .agent_definitions
                    .note_membership();
            }
            if matches!(
                event,
                crate::commands::agent_accounts::LIFECYCLE_EVENT
                    | crate::commands::agent_accounts::LOGIN_EVENT
            ) {
                handle
                    .state::<LiveHub>()
                    .model
                    .lock()
                    .unwrap()
                    .agent_definitions
                    .note_account();
            }
            if event == "agents:changed" {
                if let Ok(change) =
                    serde_json::from_str::<crate::agents::events::AgentChanged>(change.payload())
                {
                    handle
                        .state::<LiveHub>()
                        .model
                        .lock()
                        .unwrap()
                        .agent_definitions
                        .note(&change.key, change.revision.is_none());
                }
            }
            if event == "routines:changed" {
                handle
                    .state::<LiveHub>()
                    .model
                    .lock()
                    .unwrap()
                    .agent_definitions
                    .note_routines();
            }
            if matches!(event, "agent-state-changed" | "agents-changed") {
                handle
                    .state::<LiveHub>()
                    .model
                    .lock()
                    .unwrap()
                    .agent_definitions
                    .note_activity();
            }
            let flags = event_flags(event);
            handle.state::<LiveHub>().invalidate(flags);
        });
    }
    let handle = app.clone();
    app.listen(crate::mcp::agent_dot::TERMINAL_STATUS_EVENT, move |event| {
        if let Ok(payload) = serde_json::from_str::<serde_json::Value>(event.payload()) {
            if let (Some(session), Some(status), Some(outcomes)) = (
                payload["tab_session_id"].as_str(),
                payload["status"].as_str(),
                handle.try_state::<crate::mcp::session_status::SessionOutcomes>(),
            ) {
                let authoritative =
                    handle
                        .state::<LiveHub>()
                        .model
                        .lock()
                        .unwrap()
                        .hosts
                        .values()
                        .filter_map(|(_, activity)| activity.as_ref().ok())
                        .flatten()
                        .any(|s| s.session == session && (s.observed || s.exit.is_some()))
                        || crate::commands::pty_backend::in_process_host_handle(&handle)
                            .is_some_and(|host| {
                                host.activity().snapshots().iter().any(|s| {
                                    s.session == session && (s.observed || s.exit.is_some())
                                })
                            });
                if authoritative {
                    outcomes.legacy_status(session, None);
                } else if matches!(
                    status,
                    "running" | "done" | "error" | "needs-permission" | "idle"
                ) {
                    // The existing turn owner already preserves permission across sibling tools.
                    let preserved_permission = payload["hook_event"]
                        .as_str()
                        .is_some_and(|hook| !crate::agent_state::hook_clears_permission(hook))
                        && handle
                            .try_state::<DashboardLaunchState>()
                            .and_then(|launch| launch.turns().last(session))
                            == Some(crate::agent_state::AgentState::NeedsPermission);
                    outcomes.legacy_status(
                        session,
                        if preserved_permission {
                            Some("needs-permission")
                        } else if status == "idle" {
                            None
                        } else {
                            Some(status)
                        },
                    );
                }
                handle.state::<LiveHub>().invalidate(REBUILD);
            }
        }
    });
    let handle = app.clone();
    app.listen("mcp:agent-done", move |event| {
        if let Ok(payload) = serde_json::from_str::<serde_json::Value>(event.payload()) {
            if let (Some(session), Some(status)) =
                (payload["sessionId"].as_str(), payload["status"].as_str())
            {
                let generation = handle
                    .try_state::<DashboardLaunchState>()
                    .and_then(|launch| launch.turns().observation(session))
                    .map_or(0, |p| p.generation);
                handle
                    .state::<crate::mcp::session_status::SessionOutcomes>()
                    .record(
                        session,
                        status,
                        serde_json::from_value(payload["activityCompletion"].clone()).ok(),
                        generation,
                    );
                handle.state::<LiveHub>().invalidate(REBUILD | HOSTS);
            }
        }
    });
    let handle = app.clone();
    app.listen("mcp:ask-user", move |event| {
        if let Ok(payload) = serde_json::from_str::<serde_json::Value>(event.payload()) {
            if let Some(session) = payload["sessionId"].as_str() {
                handle
                    .state::<crate::mcp::session_status::SessionOutcomes>()
                    .ask_opened(session);
                handle.state::<LiveHub>().invalidate(REBUILD);
            }
        }
    });
    let handle = app.clone();
    app.listen("mcp:agent-message", move |event| {
        if let Ok(payload) = serde_json::from_str::<serde_json::Value>(event.payload()) {
            if let (Some(session), Some(outcomes)) = (
                payload["sessionId"].as_str(),
                handle.try_state::<crate::mcp::session_status::SessionOutcomes>(),
            ) {
                outcomes.message_received(session);
                handle.state::<LiveHub>().invalidate(REBUILD);
            }
        }
    });
    let handle = app.clone();
    app.listen("pty-exit", move |event| {
        if let Ok(payload) = serde_json::from_str::<serde_json::Value>(event.payload()) {
            if let (Some(session), Some(outcomes)) = (
                payload["session_id"]
                    .as_str()
                    .or_else(|| payload["sessionId"].as_str()),
                handle.try_state::<crate::mcp::session_status::SessionOutcomes>(),
            ) {
                let generation = handle
                    .try_state::<DashboardLaunchState>()
                    .and_then(|launch| launch.turns().observation(session))
                    .map_or(0, |p| p.generation);
                outcomes.session_died(session, generation);
                handle.state::<LiveHub>().invalidate(REBUILD | HOSTS);
            }
        }
    });
    let handle = app.clone();
    app.listen(inbox_events::INBOX_EVENT, move |event| {
        if let Ok(event) = serde_json::from_str::<inbox_events::InboxEvent>(event.payload()) {
            let hub = handle.state::<LiveHub>();
            let mut model = hub.model.lock().unwrap();
            model
                .inbox_pending
                .entry(inbox::team_key(&event.team))
                .or_insert_with(|| (event.team, BTreeSet::new()))
                .1
                .extend(event.message_ids);
            drop(model);
            hub.invalidate(INBOX);
        }
    });
}

pub fn start(app: &tauri::AppHandle) {
    app.manage(LiveHub::default());
    crate::records::projection::start(app);
    files::start(app);
    app.manage(agent_marks::AgentMarks::default());
    app.manage(crate::mcp::session_status::SessionOutcomes::default());
    let ask_app = app.clone();
    tauri::async_runtime::spawn(async move {
        let registry = ask_app.state::<crate::mcp::state::AskUserRegistry>();
        loop {
            registry.changed().await;
            ask_app.state::<LiveHub>().invalidate(REBUILD);
        }
    });
    app.manage(agent_list::preferences::Preferences::default());
    listen_sources(app);
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let hub = handle.state::<LiveHub>();
        let marks_app = handle.clone();
        if let Err(error) = crate::commands::await_blocking("agent_marks.load", move || {
            marks_app
                .state::<agent_marks::AgentMarks>()
                .load(&agent_marks::path(&marks_app)?)
        })
        .await
        {
            log::warn!("agent marks: {error}");
        }
        hub.invalidate(REBUILD | INBOX | HOSTS | DEFINITIONS);
        loop {
            let flags = next_updates(&handle).await;
            if flags & MACHINE_STATS != 0 {
                machine_stats::reconcile(&handle);
            }
            if flags & FILES != 0 {
                if let Err(e) = files::refresh(&handle).await {
                    log::warn!("files projection: {e}");
                }
            }
            if flags & HOSTS != 0 {
                refresh_hosts(&handle).await;
            }
            if flags & DEFINITIONS != 0 {
                refresh_definitions(&handle).await;
            }
            if flags & REBUILD != 0 {
                if let Err(e) = rebuild(&handle).await {
                    log::warn!("sessions projection: {e}");
                }
            }
            if let Err(e) = refresh_inbox(&handle, flags & FOOTPRINT != 0).await {
                log::warn!("inbox projection: {e}");
            }
        }
    });
}

async fn next_updates<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> u8 {
    let flags = app.state::<LiveHub>().next_batch().await;
    if let Err(error) = run_review::refresh(app) {
        log::warn!("run review projection: {error}");
    }
    flags
}

async fn refresh_hosts(app: &tauri::AppHandle) {
    use crate::commands::machines::{
        RuntimeAdmissionState, RuntimeLayoutReply, RuntimeMemberTarget,
    };
    let Some(boot) = crate::commands::pty_layout::webview_boot() else {
        return;
    };
    if app
        .try_state::<crate::terminal::session_server::SessionServerState>()
        .is_none()
        || app
            .try_state::<crate::commands::host_connection_manager::HostConnectionManager>()
            .is_none()
    {
        return;
    }
    let baseline = crate::commands::pty_layout::terminal_layout_admission(app.clone());
    let labels = crate::commands::machines::machine_labels(app).await;
    let hub = app.state::<LiveHub>();
    let mut model = hub.model.lock().unwrap();
    model.labels = labels;
    let keys: HashSet<_> = baseline
        .rows
        .iter()
        .map(|r| {
            format!(
                "{}:{:?}:{:?}",
                r.member.machine_id, r.member.slot, r.revision
            )
        })
        .collect();
    model.hosts.retain(|key, _| keys.contains(key));
    model
        .activity_captured_at
        .retain(|key, _| keys.contains(key));
    for row in baseline.rows {
        let key = format!(
            "{}:{:?}:{:?}",
            row.member.machine_id, row.member.slot, row.revision
        );
        let namespace = match row.state {
            RuntimeAdmissionState::Live { namespace } => namespace,
            RuntimeAdmissionState::Removed => {
                model.hosts.remove(&key);
                continue;
            }
            RuntimeAdmissionState::Historical { historical } => {
                model.hosts.insert(
                    key,
                    (
                        RuntimeLayoutReply::Historical {
                            private_authority_epoch: row.private_authority_epoch,
                            member: row.member,
                            admission_revision: row.revision,
                            frozen: historical,
                        },
                        Ok(vec![]),
                    ),
                );
                continue;
            }
            RuntimeAdmissionState::Unavailable => {
                model.hosts.insert(
                    key,
                    (
                        RuntimeLayoutReply::Unavailable {
                            member: row.member,
                            admission_revision: row.revision,
                        },
                        Ok(vec![]),
                    ),
                );
                continue;
            }
        };
        model.hosts.entry(key.clone()).or_insert_with(|| {
            (
                RuntimeLayoutReply::Unavailable {
                    member: row.member.clone(),
                    admission_revision: row.revision,
                },
                Ok(vec![]),
            )
        });
        if !model.begin_refresh(&key) {
            continue;
        }
        let target = RuntimeMemberTarget {
            member: row.member,
            admission_revision: row.revision,
            namespace,
        };
        let handle = app.clone();
        let boot = boot.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                let read_app = handle.clone();
                let read_target = target.clone();
                let read_boot = boot.clone();
                let result = crate::commands::await_blocking("live_hub.host", move || {
                    let backend = crate::commands::pty_backend::resolve_member_backend(
                        &read_app,
                        &read_target,
                        true,
                    )?;
                    crate::commands::pty_backend::ResolvedTerminalBackend::revalidate(
                        &backend, &read_app,
                    )?;
                    let snapshot = backend
                        .domain()
                        .layout_snapshot(&crate::commands::pty_layout::app_viewer(), &read_boot)
                        .map_err(|e| e.to_string())?;
                    let activity = backend
                        .domain()
                        .activity_snapshot()
                        .map_err(|e| e.to_string());
                    crate::commands::pty_backend::ResolvedTerminalBackend::revalidate(
                        &backend, &read_app,
                    )?;
                    Ok((snapshot, activity))
                })
                .await;
                let hub = handle.state::<LiveHub>();
                let mut model = hub.model.lock().unwrap();
                let again = model.finish_refresh(&key);
                if let Ok((snapshot, activity)) = result {
                    if let Ok(current) = &activity {
                        for ended in current.iter().filter(|snapshot| snapshot.exit.is_some()) {
                            let already_seen = model
                                .hosts
                                .get(&key)
                                .and_then(|(_, previous)| previous.as_ref().ok())
                                .is_some_and(|previous| {
                                    previous.iter().any(|old| {
                                        old.session == ended.session
                                            && old.identity == ended.identity
                                            && old.revision == ended.revision
                                            && old.exit == ended.exit
                                    })
                                });
                            if !already_seen {
                                model.processes.remove(&ended.session);
                            }
                        }
                    }
                    model.activity_captured_at.insert(key.clone(), now_ms());
                    model.hosts.insert(
                        key.clone(),
                        (
                            RuntimeLayoutReply::Live {
                                private_authority_epoch: row.private_authority_epoch.clone(),
                                target: target.clone(),
                                snapshot: Box::new(snapshot),
                            },
                            activity,
                        ),
                    );
                }
                drop(model);
                hub.invalidate(REBUILD);
                if !again {
                    break;
                }
            }
        });
    }
}

fn cached_registry(
    launch: &DashboardLaunchState,
    live: &LiveTabs,
) -> Result<Vec<crate::commands::dashboard_launch::SessionRow>, String> {
    use crate::commands::dashboard_launch::{
        remote_agent_row, session_row, union_registry_and_live_pty,
    };
    let registry: Vec<_> = launch
        .snapshot()
        .into_iter()
        .filter_map(|record| {
            if let Some(remote) = launch.remote_shell_by_handle(&record.handle) {
                let tab = crate::mcp::terminal_tabs::by_handle(
                    &live.tabs,
                    &record.handle,
                    Some(remote.target.session.as_str()),
                );
                let machine_live = live.live.contains(&remote.machine_id);
                if matches!(remote.status, "opened" | "alreadyLive" | "ended")
                    && machine_live
                    && tab.is_none()
                {
                    return None;
                }
                let alive = tab.map_or(!machine_live, |t| !t.exited);
                let session = tab
                    .map(|t| t.session.clone())
                    .unwrap_or_else(|| remote.target.session.as_str().to_string());
                Some(remote_agent_row(record, alive, Some(session)))
            } else {
                let alive = crate::commands::dashboard_launch::is_live_headless(
                    record.kind,
                    record.session_id.as_deref(),
                ) || record.session_id.as_deref().is_some_and(|id| {
                    live.tabs.iter().find(|t| t.session == id).map_or(
                        !live
                            .live
                            .contains(&crate::commands::machines::LOCAL_MACHINE_ID.to_string()),
                        |t| !t.exited,
                    )
                });
                Some(session_row(record, alive))
            }
        })
        .collect();
    let live_ids = live
        .tabs
        .iter()
        .filter(|t| {
            !t.exited
                && !t
                    .handle
                    .as_ref()
                    .is_some_and(|h| registry.iter().any(|r| &r.handle == h))
        })
        .map(|t| t.session.clone())
        .collect();
    let mut rows = union_registry_and_live_pty(registry, Ok(live_ids))?;
    for row in &mut rows {
        if row.machine_id.is_none() {
            if let Some(tab) = live
                .tabs
                .iter()
                .find(|t| Some(t.session.as_str()) == row.session_id.as_deref())
            {
                if tab.machine_id != crate::commands::machines::LOCAL_MACHINE_ID {
                    row.machine_id = Some(tab.machine_id.clone());
                }
            }
        }
    }
    Ok(rows)
}
fn project_registry(
    launch: &DashboardLaunchState,
    live: &LiveTabs,
    rows: Vec<crate::commands::dashboard_launch::SessionRow>,
) -> Vec<SessionListRow> {
    session_list::session_list_rows_with_tabs(
        rows,
        |id| launch.turns().last(id),
        None,
        |row| {
            let remote = launch.remote_shell_by_handle(&row.handle);
            let location = remote
                .as_ref()
                .map(|r| (r.machine_id.as_str(), r.workspace_id.as_str()))
                .or_else(|| row.machine_id.as_deref().map(|m| (m, "")));
            live.view(&row.handle, row.session_id.as_deref(), location)
                .map(|mut view| {
                    view.workspace_handle = remote
                        .as_ref()
                        .and_then(|r| r.saved_agent_workspace_handle());
                    view
                })
        },
    )
}

fn project_agent_views(model: &mut Model) -> Result<Vec<projection::Delta>, String> {
    let mut deltas = Vec::new();
    let sessions = model.projections.snapshot("sessions", "all")?.snapshot;
    let mut joined = model.rows.clone();
    model.inbox.join(&mut joined);
    let unread = joined
        .iter()
        .map(|row| (row.handle.clone(), row.unread.unwrap_or(0)))
        .collect();
    let meta = serde_json::json!({"spendRows":model.spend_rows,"ready":model.agent_ready,"homeTeam":model.agent_home_team,"teamOrder":model.agent_team_order,"teamNames":model.agent_team_names,"pane":model.agent_pane,"workspaceRollups":model.agent_workspace_rollups});
    for key in model.agent_views.clone() {
        if key.ends_with("|status") {
            deltas.extend(agent_list::replace_status(
                &mut model.projections,
                &key,
                &model.agent_rows,
                &meta,
                &sessions,
                &unread,
            )?);
        } else {
            deltas.extend(agent_list::replace_view(
                &mut model.projections,
                &key,
                &model.agent_rows,
                &meta,
            )?);
        }
    }
    Ok(deltas)
}

pub(crate) fn spend_changed<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    value: &crate::session_usage::spend_caps::Evaluation,
) -> Result<(), String> {
    let Some(hub) = app.try_state::<LiveHub>() else {
        return Ok(());
    };
    let deltas = {
        let mut model = hub.model.lock().unwrap();
        let rows = agent_list::spend::rows(value);
        if rows == model.spend_rows {
            return Ok(());
        }
        model.spend_rows = rows;
        project_agent_views(&mut model)?
    };
    publish(app, deltas);
    Ok(())
}

/// The story totals index recomputed these stories: rows on the `storyTotals` topic, patched like
/// `machine_stats`' row (`patch_view` then `publish`). A row carries no totals: a page that shows the
/// story reads them again. Each row carries the view of the scope it was computed in (a team's
/// `team:<org>/<team>`), so a team page is told of its own team's stories and no other's; a row of
/// no team's scope reaches only unrestricted (`all`) views.
pub(crate) fn story_totals_changed<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    rows: Vec<(Option<String>, serde_json::Value)>,
) {
    let Some(hub) = app.try_state::<LiveHub>() else {
        return;
    };
    let deltas = {
        let mut model = hub.model.lock().unwrap();
        match model.projections.patch_view("storyTotals", rows, vec![], None) {
            Ok(deltas) => deltas,
            Err(error) => {
                log::warn!(target: "live_hub", "storyTotals: {error}");
                return;
            }
        }
    };
    publish(app, deltas);
}

pub(crate) fn refresh_sources(app: &tauri::AppHandle) {
    agent_definitions::membership_changed(app);
    if let Some(hub) = app.try_state::<LiveHub>() {
        hub.invalidate(REBUILD | HOSTS | DEFINITIONS | INBOX | FOOTPRINT);
    }
}
pub(crate) fn inputs_changed(app: &tauri::AppHandle) {
    if let Some(hub) = app.try_state::<LiveHub>() {
        hub.invalidate(REBUILD);
    }
}
async fn refresh_definitions<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let Ok((home, principal)) = crate::commands::route_to_context::resolve_ambient(app).await
    else {
        return;
    };
    let handle = app.clone();
    if let Err(error) = crate::commands::await_blocking("agent_definitions.refresh", move || {
        agent_definitions::refresh(
            &handle,
            &crate::commands::routines::Caller {
                home,
                principal: principal.into(),
            },
        )
    })
    .await
    {
        log::warn!("agent definitions: {error}");
    }
}

async fn rebuild(app: &tauri::AppHandle) -> Result<(), String> {
    use crate::commands::agent_inbox::InboxHost;
    let hub = app.state::<LiveHub>();
    let live = hub.model.lock().unwrap().live_tabs();
    let launch = app.state::<DashboardLaunchState>();
    let mut registry = cached_registry(&launch, &live)?;
    for row in &mut registry {
        row.chat_workspace_id = row
            .session_id
            .as_deref()
            .and_then(|id| launch.workspace_for_session(id));
    }
    let registry_values: Vec<_> = registry
        .iter()
        .map(|row| serde_json::to_value(row).unwrap())
        .collect();
    let mut rows = project_registry(&launch, &live, registry);
    let host = AppInboxHost { app: app.clone() };
    let active = host.active_team().await.ok();
    let teams: HashMap<_, _> = rows
        .iter()
        .map(|r| {
            let team = launch
                .launch_team(&r.handle)
                .or_else(|| {
                    live.tabs
                        .iter()
                        .find(|t| {
                            t.session == r.session.as_deref().unwrap_or(&r.handle)
                                || t.handle.as_deref() == Some(&r.handle)
                        })
                        .and_then(|t| {
                            t.workspace_key
                                .split_once(crate::commands::pty_layout::WORKSPACE_KEY_SEPARATOR)
                        })
                        .and_then(|(key, _)| {
                            crate::commands::dashboard_launch::launch_team_from_key(key)
                        })
                })
                .or_else(|| active.clone());
            (r.handle.clone(), team)
        })
        .collect();
    for row in &mut rows {
        row.team = teams.get(&row.handle).cloned().flatten();
    }
    session_list::with_inbox_waiters(&mut rows, |handle| {
        crate::commands::host_inbox::inbox_target(&launch, handle)
            .map(|(machine, session)| {
                crate::commands::host_inbox::HostInboxes::armed(
                    &launch.host_inboxes(),
                    &machine,
                    &session,
                )
                .map_or(
                    session_list::InboxWaiter::Unknown,
                    session_list::InboxWaiter::from,
                )
            })
            .unwrap_or_default()
    });
    if let Some(stream) = app.try_state::<crate::dashboard_stream::DashboardStreamHub>() {
        session_list::with_progress(&mut rows, |id| stream.latest_progress(id));
    }
    if let Some(tracker) =
        app.try_state::<crate::commands::launch_telemetry::LaunchTelemetryTracker>()
    {
        session_list::with_launch_telemetry_batched(&mut rows, &tracker);
    }
    let mut durable_ready = false;
    let durable = match crate::commands::weft_ledger::wv_list_roster(app.clone()).await {
        Ok(durable) => {
            durable_ready = true;
            let durable: Vec<_> = durable
                .into_iter()
                .map(|row| agent_list::map_roster(&serde_json::to_value(row).unwrap()))
                .collect();
            durable
        }
        Err(error) => {
            log::warn!("agent-list roster: {error}");
            Vec::new()
        }
    };
    let roster = agent_list::union_roster(&durable, &registry_values);
    let viewer = app
        .try_state::<crate::mcp::state::McpViewerState>()
        .map(|state| state.get())
        .unwrap_or_default();
    let identity = agent_list::preferences::Preferences::identity(
        &app.state::<agent_list::preferences::Preferences>(),
    );
    let (definitions, processes, layouts) = {
        let model = hub.model.lock().unwrap();
        let mut layouts = serde_json::json!({});
        for (reply, _) in model.hosts.values() {
            if let crate::commands::machines::RuntimeLayoutReply::Live { snapshot, .. } = reply {
                for (key, layout) in &snapshot.workspaces {
                    layouts[key.as_str()] = serde_json::to_value(layout).unwrap();
                }
            }
        }
        (
            model.agent_definitions.names(),
            model.processes.clone(),
            layouts,
        )
    };
    let home_team = agent_list::home_team(
        &viewer,
        active
            .as_ref()
            .map(|team| format!("{}:{}", team.org, team.team))
            .unwrap_or_default(),
    );
    let stories = app
        .try_state::<crate::install::frontmatter_cache::SharedFrontmatterCache>()
        .map(|cache| crate::install::frontmatter_cache::FrontmatterCache::snapshot(&cache));
    let resolved = agent_list::resolve(
        &viewer,
        roster,
        &live,
        layouts,
        &identity,
        &definitions,
        &processes,
        home_team,
        stories.as_deref(),
        &|sid| {
            app.try_state::<crate::dashboard_stream::DashboardStreamHub>()
                .and_then(|stream| stream.progress_note_for_title(sid))
        },
    );
    let mut resolved = resolved;
    let now = now_ms();
    let headless_activity = crate::commands::pty_backend::in_process_host_handle(app)
        .map(|host| host.activity().snapshots())
        .unwrap_or_default();
    let asks = app
        .state::<crate::mcp::state::AskUserRegistry>()
        .pending_sessions();
    let outcome_owner = app.state::<crate::mcp::session_status::SessionOutcomes>();
    outcome_owner.reconcile_asks(&asks);
    {
        let model = hub.model.lock().unwrap();
        for snapshot in model
            .hosts
            .values()
            .filter_map(|(_, activity)| activity.as_ref().ok())
            .flatten()
            .chain(headless_activity.iter())
        {
            outcome_owner.observe(snapshot);
        }
    }
    let mut launch_deadline = None;
    let runtime: Vec<_> = {
        let model = hub.model.lock().unwrap();
        rows.iter()
            .map(|row| {
                let mut value = serde_json::to_value(row).unwrap();
                value["sessionId"] = serde_json::json!(row.session);
                value["_namesAnAgent"] = launch.names_an_agent(&row.handle).into();
                value["failed"] = row.failed.into();
                value["model"] =
                    viewer["agentSessionModels"][row.session.as_deref().unwrap_or(&row.id)].clone();
                if let Some(session) = row.session.as_deref() {
                    let observation = launch.turns().observation(session);
                    let mut legacy = effective_status::LegacySignals {
                        state: observation.map(|p| p.state.as_wire().into()),
                        observed: observation.is_some(),
                        tool_open: observation.is_some_and(|p| p.open_tools > 0),
                        activity_at: observation.map(|p| p.activity_at),
                        ended: observation.is_some_and(|p| p.exit.is_some()),
                        ..Default::default()
                    };
                    let snapshot = model
                        .hosts
                        .iter()
                        .find_map(|(key, (_, activity))| {
                            activity
                                .as_ref()
                                .ok()?
                                .iter()
                                .find(|s| s.session == session)
                                .map(|snapshot| {
                                    (
                                        snapshot,
                                        model.activity_captured_at.get(key).copied().unwrap_or(now),
                                    )
                                })
                        })
                        .or_else(|| {
                            headless_activity
                                .iter()
                                .find(|s| s.session == session)
                                .map(|s| (s, now))
                        });
                    if let Some((snapshot, _)) =
                        snapshot.filter(|(s, _)| s.observed || s.exit.is_some())
                    {
                        legacy.state = Some(
                            if snapshot.state
                                == codesense_pty_core::activity::State::NeedsPermission
                                && snapshot.reason.as_deref() == Some("blocked")
                            {
                                "blocked".into()
                            } else {
                                serde_json::to_value(snapshot.state)
                                    .unwrap()
                                    .as_str()
                                    .unwrap()
                                    .into()
                            },
                        );
                        legacy.ended = snapshot.exit.is_some();
                        legacy.observed = snapshot.observed;
                        legacy.activity_at = None;
                    }
                    let activity = snapshot.map(|(s, _)| s);
                    if activity.is_some_and(|s| s.observed) {
                        launch.mark_ready(session);
                    }
                    let (bound_at, ready_at) = launch.readiness(session).unwrap_or_default();
                    let startup_stuck =
                        effective_status::stuck_on_startup(activity, ready_at, bound_at, now);
                    if !startup_stuck && ready_at.is_none() {
                        let due = activity
                            .filter(|s| effective_status::awaiting_native_readiness(s))
                            .zip(bound_at)
                            .map(|(s, bound)| bound + effective_status::unready_bound_ms(s));
                        launch_deadline = launch_deadline
                            .into_iter()
                            .chain(due.map(|due| {
                                tokio::time::Instant::now()
                                    + Duration::from_millis(due.saturating_sub(now))
                            }))
                            .min();
                    }
                    let deadline = outcome_owner.reconcile_launch(
                        session,
                        snapshot.map(|(s, _)| s),
                        observation.map_or(0, |p| p.generation),
                        asks.contains(session),
                    );
                    launch_deadline = launch_deadline.into_iter().chain(deadline).min();
                    let retained = outcome_owner.get(session);
                    legacy.new_turn = retained.as_ref().is_some_and(|outcome| {
                        outcome.new_turn_seen
                            || observation.is_some_and(|p| p.generation > outcome.legacy_generation)
                    });
                    let projected_dot = legacy.state.as_deref().and_then(|state| match state {
                        "working" => Some("running"),
                        "thinking" => Some("thinking"),
                        "done" => Some("done"),
                        "needs-permission" | "blocked" => Some("needs-permission"),
                        "errored" => Some("error"),
                        "crashed" => Some("crashed"),
                        _ => None,
                    });
                    let mark = snapshot
                        .and_then(|(s, _)| agent_marks::crash_id(s))
                        .or_else(|| {
                            observation.and_then(|p| agent_marks::legacy_crash_id(session, p))
                        });
                    let dismissed = mark.as_deref().is_some_and(|id| {
                        app.state::<agent_marks::AgentMarks>()
                            .dismissed(session, id)
                    });
                    if let Some(id) = mark {
                        value["crashId"] = id.into();
                    }
                    if let Some(reason) = snapshot.and_then(|(snapshot, _)| {
                        codesense_pty_core::activity::end_reason(snapshot)
                    }) {
                        value["endReason"] = reason.into();
                    }
                    let outcome = retained.as_ref();
                    let outcome = outcome.map(|outcome| outcome.outcome.as_str()).or_else(|| {
                        launch
                            .has_live_headless_session(session)
                            .then_some("running")
                    });
                    let host_tick = snapshot.and_then(|(snapshot, captured)| {
                        snapshot
                            .activity_age_ms
                            .map(|age| captured.saturating_sub(age))
                    });
                    let signals = effective_status::select_live_signals(
                        snapshot.map(|(s, _)| s),
                        &legacy,
                        projected_dot,
                        None,
                        outcome,
                        asks.contains(session),
                        dismissed,
                        registry_values
                            .iter()
                            .find(|row| row["sessionId"] == session)
                            .and_then(|row| row["lastOutputMs"].as_u64()),
                        row.progress.as_ref().map(|p| p.at_ms.max(0) as u64),
                        retained.as_ref().and_then(|o| o.completion.as_ref()),
                        host_tick,
                        startup_stuck,
                        now,
                    );
                    value["pendingAsk"] = viewer["agentPendingAsks"][session].clone();
                    value["_pendingManagedAsk"] = asks.contains(session).into();
                    value["_statusSignals"] = serde_json::to_value(signals).unwrap();
                    if let Some(outcome) = outcome {
                        value["_managedOutcome"] = outcome.into();
                    }
                }
                value
            })
            .collect()
    };
    let mut wire = agent_list::sessions::project(
        &resolved.sessions,
        &runtime,
        &mut resolved.rows,
        &hub.providers,
    );
    let workspace_rollups = agent_list::presentation::workspace_rollups(&resolved.rows, &wire);
    let deltas = {
        let mut model = hub.model.lock().unwrap();
        let previous = model.projections.snapshot("sessions", "all")?.snapshot;
        for (_, row) in &mut wire {
            if let Some(facts) = row["id"]
                .as_str()
                .and_then(|id| viewer["agentSessionFacts"][id].as_object())
            {
                row.as_object_mut().unwrap().extend(facts.clone());
            }
            let since = previous
                .iter()
                .find(|old| {
                    old["id"] == row["id"] && old["effectiveStatus"] == row["effectiveStatus"]
                })
                .and_then(|old| old["statusSinceMs"].as_u64())
                .unwrap_or(now);
            row["statusSinceMs"] = since.into();
        }
        model.stall_deadline = wire
            .iter()
            .filter_map(|(_, row)| {
                let id = row["sessionId"].as_str()?;
                let input = runtime.iter().find(|input| input["sessionId"] == id)?;
                let signals: effective_status::LiveSignals =
                    serde_json::from_value(input["_statusSignals"].clone()).ok()?;
                let status: effective_status::AgentStatus =
                    serde_json::from_value(row["effectiveStatus"].clone()).ok()?;
                if status.rank() != 1 {
                    return None;
                }
                let age = signals.activity_stale_for?;
                (age < effective_status::STALLED_THRESHOLD_MS).then(|| {
                    tokio::time::Instant::now()
                        + Duration::from_millis(effective_status::STALLED_THRESHOLD_MS - age)
                })
            })
            .chain(launch_deadline)
            .min();
        model.agent_definitions.sessions(&rows);
        model.rows = rows;
        model.agent_ready = durable_ready
            && viewer["workspaces"].is_object()
            && viewer["openedTeams"].is_array()
            && launch.local_restore_state() == crate::routines::wake::Restore::Settled;
        model.agent_home_team = resolved.home_team;
        model.agent_team_order = resolved.team_order;
        model.agent_team_names = viewer["agentTeamNames"].clone();
        model.agent_pane = resolved.pane;
        model.agent_workspace_rollups = workspace_rollups;
        model.agent_rows = resolved.rows;
        model.agent_views.insert("all".into());
        let request_id = model.status_request_id.take();
        let mut deltas =
            TopicRegistry::replace(&mut model.projections, "sessions", wire.clone(), request_id)?;
        deltas.extend(project_agent_views(&mut model)?);
        let Model {
            agent_definitions,
            projections,
            ..
        } = &mut *model;
        deltas.extend(agent_definitions.project(projections)?);
        deltas
    };
    // Committed revisions must reach subscribers even when best-effort cleanup fails.
    publish(app, deltas);
    hub.changes.send_modify(|rev| *rev += 1);
    if durable_ready && launch.local_restore_state() == crate::routines::wake::Restore::Settled {
        let sessions: BTreeSet<String> = wire
            .iter()
            .filter_map(|(_, row)| {
                row["sessionId"]
                    .as_str()
                    .or_else(|| row["id"].as_str())
                    .map(str::to_owned)
            })
            .collect();
        app.state::<crate::mcp::session_status::SessionOutcomes>()
            .retain(&sessions);
        let prune_app = app.clone();
        if let Err(error) = crate::commands::await_blocking("agent_marks.prune", move || {
            prune_app
                .state::<agent_marks::AgentMarks>()
                .retain(&agent_marks::path(&prune_app)?, &sessions)
        })
        .await
        {
            log::warn!("agent marks prune: {error}");
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn live_hub_subscribe(
    app: tauri::AppHandle,
    owner_window: tauri::Window,
    topic: String,
    view_key: String,
    since_rev: Option<u64>,
    subscription_id: Option<String>,
    window: Option<[usize; 2]>,
) -> Result<SubscriptionReply, String> {
    if run_review::TOPICS.contains(&topic.as_str()) {
        run_review::prepare(
            &app,
            &topic,
            &view_key,
            subscription_id
                .as_deref()
                .ok_or("run review requires a subscription")?,
        )
        .await?;
    }
    let reply = subscribe(
        &app,
        &topic,
        &view_key,
        since_rev,
        subscription_id.as_deref(),
        window,
    )?;
    if let Some(id) = subscription_id {
        app.state::<LiveHub>()
            .model
            .lock()
            .unwrap()
            .projections
            .set_owner(&id, owner_window.label());
    }
    Ok(reply)
}
pub(crate) fn subscribe<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    topic: &str,
    view_key: &str,
    since_rev: Option<u64>,
    subscription_id: Option<&str>,
    window: Option<[usize; 2]>,
) -> Result<SubscriptionReply, String> {
    let hub = app.state::<LiveHub>();
    let mut fetched = None;
    let (mut model, configuration_deltas) = loop {
        let mut model = hub.model.lock().unwrap();
        let Model {
            configuration,
            projections,
            ..
        } = &mut *model;
        if let Some(fetched) = fetched.take() {
            configuration.deposit(fetched)?;
        }
        match configuration.refresh(projections, topic, view_key)? {
            configuration::Step::Done(deltas) => break (model, deltas),
            // A dirty source is read with the model released, then refreshed again.
            configuration::Step::Read(read) => {
                drop(model);
                fetched = Some(read.fetch());
            }
        }
    };
    if topic == "agentList" {
        model.agent_views.insert(view_key.to_owned());
        project_agent_views(&mut model)?;
    }
    let reply = match subscription_id {
        Some(id) => model
            .projections
            .subscribe_window(&id, &topic, &view_key, since_rev, window),
        None => model.projections.subscribe(&topic, &view_key, since_rev),
    };
    drop(model);
    publish(app, configuration_deltas);
    if topic == "machineStats" {
        hub.invalidate(MACHINE_STATS);
    }
    reply
}
#[tauri::command]
pub fn live_hub_page(
    app: tauri::AppHandle,
    subscription_id: String,
    cursor: String,
) -> Result<Snapshot, String> {
    app.state::<LiveHub>()
        .model
        .lock()
        .unwrap()
        .projections
        .page(&subscription_id, &cursor)
}
#[tauri::command]
pub fn live_hub_unsubscribe(app: tauri::AppHandle, subscription_id: String, paused: Option<bool>) {
    unsubscribe(&app, &subscription_id, paused == Some(true));
}
pub(crate) fn unsubscribe<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    subscription_id: &str,
    paused: bool,
) {
    pane_sources::release(app, subscription_id);
    app.state::<LiveHub>()
        .run_review
        .lock()
        .unwrap()
        .unsubscribe(subscription_id);
    if paused {
        return;
    }
    TopicRegistry::unsubscribe(
        &mut app.state::<LiveHub>().model.lock().unwrap().projections,
        subscription_id,
    );
    app.state::<LiveHub>().invalidate(MACHINE_STATS);
}
pub(crate) fn drop_window<R: tauri::Runtime>(app: &tauri::AppHandle<R>, owner: &str) {
    if let Some(hub) = app.try_state::<LiveHub>() {
        hub.model.lock().unwrap().projections.drop_owner(owner);
        hub.invalidate(MACHINE_STATS);
    }
}
#[tauri::command]
pub fn live_hub_window(
    app: tauri::AppHandle,
    subscription_id: String,
    window: [usize; 2],
) -> Result<projection::Delta, String> {
    app.state::<LiveHub>()
        .model
        .lock()
        .unwrap()
        .projections
        .move_window(&subscription_id, window)
}

pub(crate) fn replace_records<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    key: &str,
    rows: Vec<serde_json::Value>,
    actor: Option<&crate::records::Actor>,
    kind: Option<&str>,
    request_id: Option<&str>,
) -> Result<(), String> {
    let deltas = app
        .state::<LiveHub>()
        .model
        .lock()
        .map_err(|e| e.to_string())?
        .projections
        .replace_records(
            key,
            rows,
            actor.map(|a| serde_json::json!(a)),
            kind.map(str::to_owned),
            request_id.map(str::to_owned),
        )?;
    publish(app, deltas);
    Ok(())
}

fn publish<R: tauri::Runtime>(app: &tauri::AppHandle<R>, deltas: Vec<projection::Delta>) {
    let hub = app.state::<LiveHub>();
    let sent = TopicRegistry::deliver(&mut hub.model.lock().unwrap().projections, &deltas);
    publish_subscriptions(app, sent);
    for delta in deltas {
        if delta.topic != "inbox" {
            let _ = app.emit("codesense:delta", delta);
        }
    }
}

fn publish_subscriptions<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    sent: Vec<(String, projection::Delta)>,
) {
    for (id, delta) in sent {
        let _ = app.emit(
            "codesense:subscription-delta",
            serde_json::json!({"subscriptionId":id,"baseRev":delta.base_rev,"delta":delta}),
        );
    }
}
async fn refresh_inbox(app: &tauri::AppHandle, reconcile_teams: bool) -> Result<(), String> {
    let host = AppInboxHost { app: app.clone() };
    refresh_inbox_with_host(app, &host, reconcile_teams).await
}

async fn refresh_inbox_with_host<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    host: &dyn crate::commands::agent_inbox::InboxHost,
    reconcile_teams: bool,
) -> Result<(), String> {
    use crate::commands::{
        agent_inbox::{KnownSession, KnownSessions},
        agent_read_model,
    };
    let hub = app.state::<LiveHub>();
    let (mut pending, mut rows, inbox) = {
        let mut model = hub.model.lock().unwrap();
        (
            std::mem::take(&mut model.inbox_pending),
            model.rows.clone(),
            std::sync::Arc::clone(&model.inbox),
        )
    };
    let had_pending = !pending.is_empty();
    let active = host.active_team().await.ok();
    // Store resolution is best-effort; only successful discovery can retire a team.
    let allowed = if reconcile_teams {
        host.synced_teams().await.ok().map(|teams| {
            teams
                .iter()
                .chain(active.iter())
                .chain(rows.iter().filter_map(|row| row.team.as_ref()))
                .chain(pending.values().map(|(team, _)| team))
                .map(inbox::team_key)
                .collect::<BTreeSet<_>>()
        })
    } else {
        None
    };
    let mut stores = match agent_read_model::team_stores(host, active, true).await {
        Ok(stores) => stores,
        Err(error) => {
            restore_pending(&hub, pending);
            return Err(error);
        }
    };
    for row in &rows {
        if let Some(team) = &row.team {
            if !stores.iter().any(|(t, _)| t == team) {
                if let Ok(path) = host.team_store(team).await {
                    stores.push((team.clone(), path));
                }
            }
        }
    }
    for (team, _) in pending.values() {
        if !stores.iter().any(|(t, _)| t == team) {
            if let Ok(path) = host.team_store(team).await {
                stores.push((team.clone(), path));
            }
        }
    }
    let sessions = KnownSessions::new(rows.iter().map(KnownSession::from).collect());
    let mut updates = Vec::new();
    let mut removes = Vec::new();
    let mut hydrated = false;
    let mut committed = Vec::new();

    // Prepare the new projection off the hub model lock. Readers still hold the
    // old Arc snapshot, so heavy COW clones and deallocation cannot block them.
    let mut inbox: inbox::InboxProjection = (*inbox).clone();

    for (team, path) in stores {
        let initial = !inbox.contains_team(&team);
        let ids = pending
            .remove(&inbox::team_key(&team))
            .map(|(_, ids)| ids.into_iter().collect::<Vec<_>>());
        if !initial && ids.is_none() {
            continue;
        }
        if !path.exists() {
            inbox.set_announced(&team, BTreeMap::new());
            continue;
        }
        let asked = ids.unwrap_or_default();
        match inbox::read_batch(path, initial, asked.clone()).await {
            Ok(batch) => {
                hydrated |= initial;
                let (changed, deleted) = inbox.apply_batch(host, &sessions, &team, batch, &asked);
                updates.extend(changed);
                removes.extend(deleted);
                if !asked.is_empty() {
                    committed.push((team, asked));
                }
            }
            Err(error) => {
                log::warn!("inbox projection read {}: {error}", inbox::team_key(&team));
                pending.insert(inbox::team_key(&team), (team, asked.into_iter().collect()));
            }
        }
    }
    if !pending.is_empty() {
        restore_pending(&hub, pending);
    }
    updates.extend(inbox.refresh_sessions(host, &rows));
    if let Some(chat) = app
        .try_state::<crate::commands::workspace_chat::WorkspaceChatStore>()
        .filter(|s| crate::commands::workspace_chat::WorkspaceChatStore::is_installed(&s))
    {
        for row in &mut rows {
            if let (Some(workspace), Some(session)) = (&row.chat_workspace_id, &row.session) {
                if let Ok(unread) = chat.unread(workspace, &row.handle, session) {
                    row.group_unread = unread.count;
                    row.group_read_id = unread.read_id;
                }
            }
        }
        if let Err(error) =
            chat.prune_positions(&rows.iter().map(|row| row.handle.clone()).collect())
        {
            log::warn!("pruning chat positions: {error}");
        }
    }
    if let Some(ref allowed) = allowed {
        removes.extend(inbox.retain_teams(allowed));
        updates.retain(|(scope, _)| scope.as_ref().is_some_and(|key| allowed.contains(key)));
    }
    inbox.fill_session_fields(&rows);
    updates.extend(inbox.count_rows());

    // Publish the finished projection briefly under the model lock, then drop
    // the replaced Arc (and any inner maps it frees) after releasing it.
    let (deltas, resets) = {
        let mut model = hub.model.lock().unwrap();
        model.inbox = std::sync::Arc::new(inbox);
        let mut deltas = model
            .projections
            .patch_view("inbox", updates, removes, None)?;
        deltas.extend(project_agent_views(&mut model)?);
        if hydrated {
            // Initial history is loaded through bounded snapshots, never arrival deltas.
            let resets = model.projections.reset_subscriptions("inbox", &deltas)?;
            (
                deltas
                    .into_iter()
                    .filter(|delta| delta.topic != "inbox")
                    .collect(),
                resets,
            )
        } else {
            (deltas, Vec::new())
        }
    };
    let notify_legacy = had_pending || hydrated || !deltas.is_empty();
    if hydrated {
        publish_subscriptions(app, resets);
    }
    publish(app, deltas);
    // Legacy readers must see the committed cache when their refresh starts.
    if committed.is_empty() {
        if notify_legacy {
            host.inbox_changed();
        }
    } else {
        host.inbox_committed(&committed);
    }
    hub.changes.send_modify(|rev| *rev += 1);
    Ok(())
}

fn restore_pending(
    hub: &LiveHub,
    pending: BTreeMap<String, (crate::commands::dashboard_launch::LaunchTeam, BTreeSet<i64>)>,
) {
    let mut model = hub.model.lock().unwrap();
    for (key, (team, ids)) in pending {
        model
            .inbox_pending
            .entry(key)
            .or_insert_with(|| (team, BTreeSet::new()))
            .1
            .extend(ids);
    }
    drop(model);
    hub.invalidate(INBOX);
}

#[tauri::command]
pub fn live_hub_view_key(
    app: tauri::AppHandle,
    origin: String,
    token: String,
) -> Result<String, String> {
    let frames = app.state::<std::sync::Arc<crate::dashboard_stream::frame::FrameStore>>();
    let grant = crate::dashboard_stream::frame::FrameStore::resolve(&frames, &origin, &token)
        .ok_or("denied: frame identity")?;
    view_key::resolve(&grant).ok_or_else(|| "denied: team scope requires a team".into())
}
