// SPDX-License-Identifier: MIT
//! The app's [`WakeEnv`] (SW-AGR-2):
//!
//! - **Candidates** come from every terminal host's layout: this Mac's
//!   session server and every connected machine's (a remote session counts
//!   whoever launched it, from the agent key its tab carries), and the last
//!   layout of a machine that went offline. A live tab is probed for its
//!   agent CLI: one whose agent quit back to the shell is not a candidate.
//!   Activity comes from the session list where it has one. Launches the
//!   registry reserved but has not bound yet count as starting.
//! - Delivery and cancel go through the delivery core `delivery::app`
//!   manages; posture is read from the picked session's recorded launch.
//! - A deferred wake is dispatched again on its own task, never in the claim
//!   loop; deferred wakes an app quit on are dispatched again at start.
//! - Inbox posts go to the routine's own team, read with the signed-in
//!   account (no team needs to be open).

use std::collections::HashMap;
use std::time::Duration;

use codesense_pty_core::layout::{TabEntry, Tags};
use tauri::{AppHandle, Emitter, Manager};

use crate::commands::dashboard_launch::{DashboardLaunchState, TAG_LAUNCH_ROUTINE};
use crate::delivery::app::DeliveryState;
use crate::delivery::authority::{Caller, SessionPosture, SessionRef};
use crate::delivery::run_status::WakeRunStatus;
use crate::delivery::{DeliveryError, DeliveryReceipt, WakeDelivery};
use crate::domain::Principal;
use crate::mcp::session_list::SessionState;

use super::admit::RoutinePointer;
use super::wake::{
    Activity, Candidate, LaunchFlights, QuitState, QuitWatch, RemoteLaunch, Restore, RosterFuture,
    TabFuture, TabOp, TabReply, WakeEnv, WakeNotice, WakeNotify, LOCAL,
};

/// The layout tag a remote tab carries its saved agent's key in.
const TAG_REMOTE_AGENT_KEY: &str = "remoteAgentKey";
const TAG_REMOTE_HANDLE: &str = "remoteHandle";

/// Manage the app's wake environment, let it cancel queued wakes on
/// archive, and dispatch again the wakes an app quit on while they waited.
/// Call once the delivery core is installed.
pub(crate) fn install(app: &AppHandle) {
    if app.try_state::<std::sync::Arc<AppWakeEnv>>().is_some() {
        return;
    }
    let env = std::sync::Arc::new(AppWakeEnv::new(app.clone()));
    if let Some(home) = crate::commands::paths::codesense_home_dir() {
        super::wake::install(&home, env.clone());
        if let Ok(store) = super::state::RoutineStateStore::open(&home) {
            for pointer in super::wake::deferred(&store) {
                env.redispatch(pointer, super::wake::DEFER_STEP);
            }
        }
    }
    app.manage(env);
}

pub struct AppWakeEnv {
    app: AppHandle,
    flights: LaunchFlights,
    quits: std::sync::Arc<QuitWatch>,
}

/// One terminal host's tabs, as its layout shows them.
pub(crate) struct HostTabs {
    /// `local` for this Mac's session server.
    pub machine: String,
    /// Connected now; else its last layout (an offline machine).
    pub online: bool,
    pub tabs: Vec<TabEntry>,
}

/// What a live tab's agent probe says: `Some(false)` when its agent CLI is
/// not running (it quit back to the shell), `None` when the host cannot say.
pub(crate) type AgentProbe<'a> = &'a dyn Fn(&str, &str) -> Option<bool>;

/// What this desktop's launch registry recorded about a launch, by handle:
/// when it was last active, whether it is headless.
pub(crate) type Recorded = (i64, bool);

/// What else decides a tab's candidacy: whose desktop this is, and the
/// quit grace.
pub(crate) struct RosterRules<'a> {
    /// This desktop's account (its owner id): a remote tab tagged as launched
    /// under another account is never woken (never typed into a teammate's
    /// session). `None` when it cannot be read: such tags are then trusted.
    pub owner: Option<&'a str>,
    /// `(machine, session, absent)` → whether a tab whose probe found no
    /// agent has been without it for the quit grace.
    pub quit: &'a dyn Fn(&str, &str, bool) -> QuitState,
}

impl RosterRules<'_> {
    /// No owner known; a probe that finds no agent reads as quit at once.
    #[cfg(test)]
    pub fn immediate() -> RosterRules<'static> {
        RosterRules {
            owner: None,
            quit: &|_, _, absent| {
                if absent {
                    QuitState::Quit
                } else {
                    QuitState::Present
                }
            },
        }
    }

    /// The rules of a desktop signed in as `owner`, timing quits with `quits`.
    pub fn of<'a>(
        owner: Option<&'a str>,
        quit: &'a dyn Fn(&str, &str, bool) -> QuitState,
    ) -> RosterRules<'a> {
        RosterRules { owner, quit }
    }
}

/// The layout tag a remote tab carries the account that launched it in.
/// Tabs opened before SW-AGR-2b carry none: they count as this account's,
/// since a machine's private terminal server admits only its enrolled owner
/// (a layout read from it holds that owner's tabs only).
pub(crate) const TAG_REMOTE_OWNER: &str = "remoteOwnerId";
/// The layout tag a remote tab carries its launch request id in.
pub(crate) const TAG_REMOTE_REQUEST: &str = "remoteRequestId";

/// The wake candidates in `hosts`' layouts. `probe(machine, session)` checks a
/// live tab's agent; `activity(session)` is what the session list says it is
/// doing; `registry(handle)` is what this desktop's launch registry recorded
/// (dir, last activity, headless). Tabs that ran no saved agent are left out.
pub(crate) fn roster_from(
    hosts: &[HostTabs],
    probe: AgentProbe<'_>,
    activity: &dyn Fn(&str) -> Option<Activity>,
    registry: &dyn Fn(&str) -> Option<Recorded>,
    rules: &RosterRules<'_>,
) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    for host in hosts {
        for tab in &host.tabs {
            if tab.state.is_exited() {
                continue;
            }
            let Some(agent) = agent_of(&tab.tags) else {
                continue;
            };
            // Never a session another account launched (a shared machine).
            if let (Some(owner), Some(launched_by)) = (rules.owner, tab.tags.get(TAG_REMOTE_OWNER))
            {
                if launched_by != owner {
                    continue;
                }
            }
            let session = tab.session.as_str().to_string();
            let handle = handle_of(&tab.tags).unwrap_or_else(|| session.clone());
            let recorded = registry(&handle);
            let headless = recorded.as_ref().is_some_and(|(_, headless)| *headless);
            let activity = if !host.online {
                // Its agent ran when the machine was last seen: still live (R1).
                Activity::Offline
            } else {
                let absent = probe(&host.machine, &session) == Some(false);
                match (rules.quit)(&host.machine, &session, absent) {
                    QuitState::Quit => Activity::Ended,
                    QuitState::Quitting => Activity::Quitting,
                    QuitState::Present => activity(&session).unwrap_or(Activity::Unknown),
                }
            };
            candidates.push(Candidate {
                machine: host.machine.clone(),
                session,
                handle,
                agent: Some(agent),
                routine_key: tab.tags.get(TAG_LAUNCH_ROUTINE).cloned(),
                activity,
                headless,
                last_active_ms: recorded.map(|(at, _)| at).unwrap_or_default(),
                request_id: tab.tags.get(TAG_REMOTE_REQUEST).cloned(),
            });
        }
    }
    candidates
}

/// The saved agent a tab runs, by key: a local launch's recorded revision,
/// or a remote tab's agent key.
fn agent_of(tags: &Tags) -> Option<String> {
    tags.get(crate::commands::dashboard_launch::TAG_LAUNCH_AGENT_REVISION)
        .and_then(|rev| crate::agents::manifest::AgentRevisionRef::key_of(rev))
        .map(|key| key.to_string())
        .or_else(|| {
            tags.get(TAG_REMOTE_AGENT_KEY)
                .map(|key| key.split('@').next().unwrap_or(key).to_string())
        })
        .filter(|key| !key.is_empty())
}

fn handle_of(tags: &Tags) -> Option<String> {
    tags.get(crate::commands::dashboard_launch::TAG_LAUNCH_HANDLE)
        .or_else(|| tags.get(TAG_REMOTE_HANDLE))
        .cloned()
}

impl AppWakeEnv {
    pub fn new(app: AppHandle) -> Self {
        Self {
            app,
            flights: LaunchFlights::default(),
            quits: Default::default(),
        }
    }

    async fn read_roster(&self) -> Result<super::wake::Roster, String> {
        use crate::commands::machines::{RuntimeLayoutReply, RuntimeSessionReference};
        let app = self.app.clone();
        let replies = crate::commands::await_blocking("routine_wake_layouts", move || {
            crate::commands::pty_layout::member_layout_snapshots_blocking(&app)
                .unwrap_or_else(|| Err("the terminal layouts have not been read yet".into()))
        })
        .await?;
        let mut hosts = Vec::new();
        let mut targets = HashMap::new();
        let mut read = Vec::new();
        for reply in &replies {
            read.push(match reply {
                RuntimeLayoutReply::Live { target, .. } => (
                    target.member.machine_id.clone(),
                    target.member.slot == crate::commands::machines::RuntimeMemberSlot::Sidecar,
                    LayoutRead::Live,
                ),
                RuntimeLayoutReply::Historical { member, .. } => {
                    (member.machine_id.clone(), false, LayoutRead::Historical)
                }
                RuntimeLayoutReply::Unavailable { member, .. } => {
                    (member.machine_id.clone(), false, LayoutRead::Unavailable)
                }
            });
        }
        let unreadable = unreadable_machines(&read, &self.expected_machines());
        for reply in replies {
            match reply {
                RuntimeLayoutReply::Live {
                    target, snapshot, ..
                } => {
                    let machine = target.member.machine_id.clone();
                    targets.insert(machine.clone(), target);
                    hosts.push(HostTabs {
                        machine,
                        online: true,
                        tabs: tabs(&snapshot),
                    });
                }
                RuntimeLayoutReply::Historical { member, frozen, .. } => hosts.push(HostTabs {
                    machine: member.machine_id,
                    online: false,
                    tabs: tabs(&frozen.snapshot),
                }),
                RuntimeLayoutReply::Unavailable { .. } => {}
            }
        }
        // Activity from the session list (its hooks and the hosts' owners).
        let rows = crate::mcp::session_list::read_session_list(&self.app, None)
            .await
            .unwrap_or_default();
        let activity: HashMap<String, Activity> = rows
            .iter()
            .filter_map(|row| {
                let session = row.session.clone()?;
                Some((
                    session,
                    match row.state {
                        SessionState::Idle => Activity::Idle,
                        SessionState::Running => Activity::Busy,
                        SessionState::NeedsYou => Activity::NeedsYou,
                        SessionState::Unknown => Activity::Unknown,
                        SessionState::Offline => Activity::Offline,
                        SessionState::Exited => Activity::Ended,
                    },
                ))
            })
            .collect();
        let launch = self.app.try_state::<DashboardLaunchState>();
        let snapshot = launch.as_ref().map(|l| l.snapshot()).unwrap_or_default();
        let registry: HashMap<String, Recorded> = snapshot
            .iter()
            .map(|s| {
                (
                    s.handle.clone(),
                    (
                        s.last_output_ms.or(s.started_at_ms).unwrap_or_default() as i64,
                        s.kind == crate::commands::dashboard_launch::KIND_HEADLESS,
                    ),
                )
            })
            .collect();
        let owner = self
            .app
            .try_state::<crate::commands::local_profile::LocalProfileState>()
            .and_then(|profile| profile.snapshot().ok())
            .map(|authority| authority.owner_id().to_string());
        let ignorable = crate::commands::machines::unattended_machines(&self.app).await;
        let app = self.app.clone();
        let quits = self.quits.clone();
        let mut candidates = crate::commands::await_blocking("routine_wake_probe", move || {
            let probe = |machine: &str, session: &str| -> Option<bool> {
                let reference = match targets.get(machine) {
                    Some(target) if machine != LOCAL => RuntimeSessionReference::Admitted(
                        Box::new(crate::commands::machines::RuntimeSessionTarget {
                            runtime: target.clone(),
                            session: codesense_pty_core::domain::PtySessionId::new(session),
                        }),
                    ),
                    _ => RuntimeSessionReference::Legacy(session.to_string()),
                };
                crate::commands::pty_backend::with_session_reference(
                    &app,
                    &reference,
                    machine != LOCAL,
                    // Unknown (`None`) whenever the probe cannot answer: only a
                    // probe that worked and found no agent counts as quit.
                    |host, session| {
                        Ok(crate::commands::pty::agent_present(
                            host,
                            session,
                            machine == LOCAL,
                        ))
                    },
                )
                .ok()
                .flatten()
            };
            let now = super::finish::now_ms();
            // A tab that left the layout leaves the quit watch too.
            quits.keep_only(
                &hosts
                    .iter()
                    .filter(|host| host.online)
                    .flat_map(|host| {
                        host.tabs
                            .iter()
                            .map(|tab| (host.machine.clone(), tab.session.as_str().to_string()))
                    })
                    .collect(),
            );
            let quit = |machine: &str, session: &str, absent: bool| {
                quits.observe(machine, session, absent, now)
            };
            Ok(roster_from(
                &hosts,
                &probe,
                &|session| activity.get(session).copied(),
                &|handle| registry.get(handle).cloned(),
                &RosterRules::of(owner.as_deref(), &quit),
            ))
        })
        .await?;
        // Launches reserved but not bound yet: the agent is starting (until
        // the reservation is stale: it then blocks nothing).
        let now = super::finish::now_ms();
        candidates.extend(
            snapshot
                .iter()
                .filter(|s| s.session_id.is_none())
                .filter_map(|entry| {
                    starting(
                        &entry.handle,
                        entry.agent_revision.as_deref(),
                        entry
                            .provenance
                            .as_ref()
                            .and_then(|p| p.routine_key.as_deref()),
                        entry.kind == crate::commands::dashboard_launch::KIND_HEADLESS,
                        entry.started_at_ms,
                        now,
                    )
                }),
        );
        Ok(super::wake::Roster {
            candidates,
            unreadable,
            ignorable,
        })
    }

    /// Every machine this desktop has admitted (this Mac included): each one
    /// is expected to answer a layout read.
    fn expected_machines(&self) -> Vec<String> {
        use crate::commands::machines::RuntimeAdmissionState;
        let mut rows = self
            .app
            .try_state::<crate::terminal::session_server::SessionServerState>()
            .map(|local| local.admission.baseline().rows)
            .unwrap_or_default();
        if let Some(manager) =
            self.app
                .try_state::<crate::commands::host_connection_manager::HostConnectionManager>()
        {
            rows.extend(manager.admission_baseline().rows);
        }
        let mut machines: Vec<String> = rows
            .into_iter()
            .filter(|row| !matches!(row.state, RuntimeAdmissionState::Removed))
            .map(|row| row.member.machine_id)
            .collect();
        machines.push(LOCAL.to_string());
        machines.sort();
        machines.dedup();
        machines
    }

    fn service(&self) -> Result<DeliveryState, DeliveryError> {
        self.app
            .try_state::<DeliveryState>()
            .map(|s| s.inner().clone())
            .ok_or_else(|| DeliveryError::Unavailable {
                message: "wake delivery is not running".into(),
            })
    }
}

/// How a machine's layout read came back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LayoutRead {
    Live,
    /// An offline machine's last layout.
    Historical,
    Unavailable,
}

/// The machines whose sessions could not be read: one that answered
/// `unavailable`, one this desktop expects but that did not answer at all,
/// and this Mac when its session server's layout is not live. `read` is
/// `(machine, is this Mac's session server, how it came back)`.
pub(crate) fn unreadable_machines(
    read: &[(String, bool, LayoutRead)],
    expected: &[String],
) -> Vec<String> {
    let seen = |machine: &str| {
        read.iter().any(|(m, sidecar, how)| {
            m == machine
                && match how {
                    LayoutRead::Live => machine != LOCAL || *sidecar,
                    LayoutRead::Historical => machine != LOCAL,
                    LayoutRead::Unavailable => false,
                }
        })
    };
    let mut unreadable: Vec<String> = expected
        .iter()
        .chain(read.iter().map(|(m, _, _)| m))
        .filter(|machine| !seen(machine))
        .cloned()
        .collect();
    unreadable.sort();
    unreadable.dedup();
    unreadable
}

/// A launch the registry reserved but has not bound yet, as a starting
/// candidate; `None` when it names no saved agent or is older than
/// [`super::wake::IN_FLIGHT_STALE_MS`] (a launch that never settled blocks
/// nothing).
pub(crate) fn starting(
    handle: &str,
    agent_revision: Option<&str>,
    routine_key: Option<&str>,
    headless: bool,
    started_at_ms: Option<u64>,
    now: i64,
) -> Option<Candidate> {
    let agent = agent_revision.and_then(crate::agents::manifest::AgentRevisionRef::key_of)?;
    let started = started_at_ms? as i64;
    if now - started >= super::wake::IN_FLIGHT_STALE_MS {
        return None;
    }
    Some(Candidate {
        machine: LOCAL.to_string(),
        session: String::new(),
        handle: handle.to_string(),
        agent: Some(agent.to_string()),
        routine_key: routine_key.map(str::to_string),
        activity: Activity::Starting,
        headless,
        last_active_ms: started,
        request_id: None,
    })
}

fn tabs(snapshot: &codesense_pty_core::layout::LayoutSnapshot) -> Vec<TabEntry> {
    snapshot
        .workspaces
        .values()
        .flat_map(|w| w.tabs.iter().cloned())
        .collect()
}

/// How long a remote launch still running on its machine is waited for
/// before its answer counts as unknown.
pub(crate) const SETTLE_WAIT: Duration = Duration::from_secs(30);

/// One remote launch's `launch-settled` event, once it is emitted.
struct Settled {
    app: AppHandle,
    id: Option<tauri::EventId>,
    rx: Option<tokio::sync::oneshot::Receiver<()>>,
}

impl Settled {
    /// Resolves once the launch settled (or its listener went away).
    async fn wait(mut self) {
        if let Some(rx) = self.rx.take() {
            let _ = rx.await;
        }
    }
}

/// A remote open answered `first`: when that says the launch is still
/// running on its machine (`pending`), wait for `settled` (at most
/// `at_most`), then ask once more with `ask_again`. What it came to, and
/// whether it asked again.
pub(crate) async fn through_pending<T, Ask, Again>(
    first: T,
    pending: impl Fn(&T) -> bool,
    settled: impl std::future::Future<Output = ()>,
    at_most: Duration,
    ask_again: Ask,
) -> (T, bool)
where
    Ask: FnOnce() -> Again,
    Again: std::future::Future<Output = T>,
{
    if !pending(&first) {
        return (first, false);
    }
    let _ = tokio::time::timeout(at_most, settled).await;
    (ask_again().await, true)
}

impl Drop for Settled {
    fn drop(&mut self) {
        use tauri::Listener;
        if let Some(id) = self.id.take() {
            self.app.unlisten(id);
        }
    }
}

impl AppWakeEnv {
    fn settled_signal(&self, request_id: uuid::Uuid) -> Settled {
        use tauri::Listener;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = std::sync::Mutex::new(Some(tx));
        let wanted = request_id.to_string();
        let id = self.app.listen(
            crate::commands::pty_layout::LAUNCH_SETTLED_EVENT,
            move |event| {
                let ours = serde_json::from_str::<serde_json::Value>(event.payload())
                    .ok()
                    .and_then(|v| v.get("requestId")?.as_str().map(|id| id == wanted))
                    .unwrap_or(false);
                if ours {
                    if let Some(tx) = tx.lock().unwrap_or_else(|e| e.into_inner()).take() {
                        let _ = tx.send(());
                    }
                }
            },
        );
        Settled {
            app: self.app.clone(),
            id: Some(id),
            rx: Some(rx),
        }
    }
}

impl WakeNotify for AppWakeEnv {
    fn post(&self, notice: &WakeNotice) {
        post(&self.app, notice.clone());
    }

    fn post_confirmed(&self, notice: &WakeNotice) -> bool {
        let posted =
            tauri::async_runtime::block_on(post_notice(self.app.clone(), notice.clone())).is_ok();
        if !posted {
            tauri::async_runtime::spawn(async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                crate::session_usage::spend_caps::changed();
            });
        }
        posted
    }

    fn redispatch(&self, pointer: RoutinePointer, after: Duration) {
        let app = self.app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(after).await;
            let Some(home) = crate::commands::paths::codesense_home_dir() else {
                return;
            };
            let run_id = pointer.run_id.clone();
            let launcher = super::dispatch::AppLauncher::new(app);
            if let Err(error) = super::dispatch::execute(home, pointer, &launcher).await {
                log::warn!(target: "routines", "wake run {run_id} not dispatched again: {error}");
            }
        });
    }
}

impl WakeEnv for AppWakeEnv {
    fn restore(&self) -> Restore {
        match self.app.try_state::<DashboardLaunchState>() {
            Some(launch) => launch.local_restore_state(),
            None => Restore::Pending,
        }
    }

    fn roster(&self) -> RosterFuture<'_> {
        Box::pin(self.read_roster())
    }

    fn machine_known(&self, machine: &str) -> bool {
        use crate::commands::host_connection_manager::{
            ConnectedBackendError, HostConnectionManager,
        };
        if machine == LOCAL {
            return true;
        }
        self.app
            .try_state::<HostConnectionManager>()
            .is_some_and(|manager| {
                !matches!(
                    manager.connected_backend(machine),
                    Err(ConnectedBackendError::UnknownMachine)
                )
            })
    }

    fn posture(&self, candidate: &Candidate, principal: &Principal) -> SessionPosture {
        let recorded = self
            .app
            .try_state::<DashboardLaunchState>()
            .is_some_and(|launch| launch.agent_stamp_for_session(&candidate.session).is_some());
        match crate::commands::paths::codesense_home_dir() {
            // A remote tab another desktop launched has no launch record here:
            // judge it by the agent its tab names, as defined now (its
            // permissions, `allowWake`). Fails closed when it cannot be read.
            Some(home) if !recorded && candidate.machine != LOCAL => candidate
                .agent
                .as_deref()
                .and_then(crate::agents::AgentKey::parse)
                .and_then(|key| crate::agents::service::read_current(&home, principal, &key).ok())
                .map(|agent| {
                    SessionPosture::of_agent(Some(&agent), candidate.agent.as_deref().unwrap_or(""))
                })
                .unwrap_or(SessionPosture {
                    bypass: true,
                    allow_wake: false,
                }),
            Some(home) => crate::delivery::app::session_posture(
                &self.app,
                &home,
                principal,
                &candidate.session,
            ),
            // Fails closed: only an agent that opted in can be woken.
            None => SessionPosture {
                bypass: true,
                allow_wake: false,
            },
        }
    }

    fn deliver(&self, delivery: WakeDelivery) -> Result<DeliveryReceipt, DeliveryError> {
        self.service()?.deliver(&Caller::RoutineHost, delivery)
    }

    fn cancel(&self, target: &SessionRef, key: &str) -> Result<WakeRunStatus, DeliveryError> {
        self.service()?.cancel(&Caller::RoutineHost, target, key)
    }

    fn remote_tab<'a>(&'a self, launch: &'a RemoteLaunch, op: TabOp) -> TabFuture<'a> {
        Box::pin(async move {
            use crate::commands::pty_layout::{
                RemoteLaunchIdentity, RemoteShellTabRequest, RemoteTabReference,
            };
            let team_key = crate::commands::pty_layout::team_key(&launch.org, Some(&launch.team));
            let request = match op {
                TabOp::Create => {
                    // Where it runs is resolved on the machine: the workspace it
                    // prepared there, from its durable receipt (SW-AGR-12), never
                    // a path on this Mac nor what the viewer has open.
                    let place = match durable_place(
                        &self.app,
                        &launch.org,
                        &launch.team,
                        &launch.machine,
                        &launch.target,
                    )
                    .await
                    {
                        Ok(place) => place,
                        Err(error) => {
                            return TabReply::NotStarted {
                                reason: error.reason(),
                                detail: error.detail().to_string(),
                            }
                        }
                    };
                    RemoteShellTabRequest::Create {
                        request_id: launch.request_id,
                        machine_id: launch.machine.clone(),
                        team_key,
                        workspace_id: place.workspace_id,
                        cwd: Some(place.cwd),
                        name: None,
                        launch: None,
                        permission_mode: Default::default(),
                        agent_key: Some(launch.agent.clone()),
                        prepared_workspace_handle: Some(place.handle),
                        headed: true,
                        initial_prompt: Some(launch.prompt.clone()),
                        identity: Box::new(RemoteLaunchIdentity {
                            launched_by: Some(format!("routine {}", launch.routine_name)),
                            // SW-AGR-8: a wake's launch-if-absent goes
                            // through the machine's single flight.
                            if_absent: true,
                            // SW-AGR-12: chosen from the receipts; the
                            // receipt gate still checks it exactly.
                            durable_workspace: true,
                            ..Default::default()
                        }),
                    }
                }
                TabOp::Attach => RemoteShellTabRequest::Attach {
                    machine_id: launch.machine.clone(),
                    team_key,
                    reference: RemoteTabReference::RequestId(launch.request_id),
                    headed: false,
                },
            };
            // Listening before asking: a launch that settles in between is
            // not missed.
            let settled = self.settled_signal(launch.request_id);
            let first = crate::commands::pty_layout::remote_shell_tab_bound(
                self.app.clone(),
                request,
                None,
            )
            .await;
            let attach_again = || {
                crate::commands::pty_layout::remote_shell_tab_bound(
                    self.app.clone(),
                    RemoteShellTabRequest::Attach {
                        machine_id: launch.machine.clone(),
                        team_key: crate::commands::pty_layout::team_key(
                            &launch.org,
                            Some(&launch.team),
                        ),
                        reference: RemoteTabReference::RequestId(launch.request_id),
                        headed: false,
                    },
                    None,
                )
            };
            let (outcome, asked_again) = through_pending(
                first,
                |outcome| outcome.status == "pending",
                settled.wait(),
                SETTLE_WAIT,
                attach_again,
            )
            .await;
            log::info!(target: "routines", "routine run {}: {op:?} {} on {} answered {}", launch.run_id, launch.request_id, launch.machine, outcome.status);
            tab_reply(&TabAnswer {
                status: outcome.status,
                code: outcome.code.as_deref(),
                message: outcome.message.as_deref(),
                session: outcome.target.as_ref().map(|t| t.session.as_str()),
                handle: outcome.handle.as_deref(),
                retryable: outcome.retryable,
                attach: op == TabOp::Attach || asked_again,
            })
        })
    }

    fn flights(&self) -> &LaunchFlights {
        &self.flights
    }
}

/// A workspace that lives on a machine, as its preparation receipt records
/// it: where an agent launched there for a routine runs (SW-AGR-2b, SW-AGR-12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostWorkspace {
    pub workspace_id: String,
    /// Its managed handle on the machine (the prepared workspace).
    pub handle: String,
    /// Its worktree on the machine, as it was prepared there.
    pub cwd: String,
}

/// Why no workspace on a machine was found for a routine's launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlaceError {
    /// The machine is not connected (or its terminal server is not known yet,
    /// e.g. right after start): which receipt is live cannot be told. Tried
    /// again at the next fire.
    Offline(String),
    /// Nothing prepared there that the routine can start in.
    NeedsPreparation(String),
    /// Asking the machine failed for another reason (a timeout, a retired
    /// lease, authorization): nothing started, and no other candidate is
    /// tried on this fire.
    Unavailable(String),
}

impl PlaceError {
    pub(crate) fn reason(&self) -> codesense_pty_core::session_wake::UndeliveredReason {
        use codesense_pty_core::session_wake::UndeliveredReason;
        match self {
            Self::Offline(_) => UndeliveredReason::MachineOffline,
            Self::NeedsPreparation(_) => UndeliveredReason::NeedsPreparation,
            Self::Unavailable(_) => UndeliveredReason::Unavailable,
        }
    }

    pub(crate) fn detail(&self) -> &str {
        match self {
            Self::Offline(detail) | Self::NeedsPreparation(detail) | Self::Unavailable(detail) => {
                detail
            }
        }
    }
}

/// SW-AGR-12: the workspace on `machine` a routine of `org`/`team` targeting
/// `target` launches into, from this account's durable preparation receipts:
/// no viewer state, so a workspace closed in the UI, or a fire before the
/// frontend publishes anything, still finds it. Each candidate, in order, is
/// put through the receipt gate ([`resolve_durable`]: owner, team, configured
/// repo, machine, live server, and the machine's own word that it is still
/// prepared there); the first that passes is the one. A stale receipt (its
/// worktree gone) is passed over, never a block.
///
/// [`resolve_durable`]: crate::mcp::remote_workspace_target::resolve_durable
pub(crate) async fn durable_place(
    app: &AppHandle,
    org: &str,
    team: &str,
    machine: &str,
    target: &super::model::RoutineTarget,
) -> Result<HostWorkspace, PlaceError> {
    let ctx = match crate::commands::route_to_context::derive_context_for(app, org, team)
        .await
        .map_err(PlaceError::NeedsPreparation)?
    {
        crate::commands::route_to_context::ContextResolution::Ok(ctx) => ctx,
        _ => {
            return Err(PlaceError::NeedsPreparation(format!(
                "{org}/{team} is not available on this Mac"
            )))
        }
    };
    let server = app
        .try_state::<crate::commands::host_connection_manager::HostConnectionManager>()
        .and_then(|manager| manager.connected_backend(machine).ok())
        .map(|backend| backend.client().namespace().server_id.clone());
    let (home, principal, owner) = (ctx.codesense_home.clone(), ctx.principal(), ctx.user_id);
    let wanted = machine.to_string();
    let places = crate::commands::await_blocking("routine_prepared_places", move || {
        crate::mcp::remote_workspace_target::prepared_places(&home, &principal, owner, &wanted)
    })
    .await
    .map_err(PlaceError::NeedsPreparation)?;
    let team_key = crate::commands::pty_layout::team_key(org, Some(team));
    let configured = |repo: &crate::domain::SourceRepoSlug| {
        crate::mcp::remote_workspace::configured_repo(&ctx, repo).is_ok()
    };
    choose_place(
        &places,
        &team_key,
        machine,
        server.as_deref(),
        &configured,
        target,
        |place| {
            let (app, ctx, machine) = (app.clone(), ctx.clone(), machine.to_string());
            async move {
                let resolved = crate::mcp::remote_workspace_target::resolve_durable(
                    app,
                    ctx,
                    &machine,
                    &place.handle,
                )
                .await?;
                if resolved.cwd != place.cwd || resolved.workspace_id != place.workspace_id {
                    return Err(format!("{RECEIPT_MISMATCH}: another worktree or workspace"));
                }
                Ok(())
            }
        },
    )
    .await
}

/// [`durable_place`] once its inputs are read: the receipts there (`places`),
/// the machine's live terminal server (`None`: not connected), which repos
/// the team configures, and `confirm`, the receipt gate for one candidate.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn choose_place<F, Fut>(
    places: &[crate::mcp::remote_workspace_target::PreparedPlace],
    team_key: &str,
    machine: &str,
    server: Option<&str>,
    configured: &(dyn Fn(&crate::domain::SourceRepoSlug) -> bool + Sync),
    target: &super::model::RoutineTarget,
    mut confirm: F,
) -> Result<HostWorkspace, PlaceError>
where
    F: FnMut(HostWorkspace) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let Some(server) = server else {
        return Err(PlaceError::Offline(format!(
            "{machine} is not connected yet: which of its workspaces is live is known once it is; the next fire tries again"
        )));
    };
    let candidates = host_workspace(places, team_key, machine, server, configured, target)
        .map_err(PlaceError::NeedsPreparation)?;
    let mut failed = Vec::new();
    for candidate in candidates {
        match confirm(candidate.clone()).await {
            Ok(()) => return Ok(candidate),
            // Gone from the machine (or never there): the next candidate.
            Err(error) if is_stale(&error) => failed.push(format!(
                "{} at {}: {error}",
                candidate.workspace_id, candidate.cwd
            )),
            // Anything else says nothing about the receipt: stop here.
            Err(error) => {
                return Err(PlaceError::Unavailable(format!(
                    "{machine} could not confirm the workspace {} at {}: {error}. The next fire tries again",
                    candidate.workspace_id, candidate.cwd
                )))
            }
        }
    }
    Err(PlaceError::NeedsPreparation(format!(
        "{machine} no longer has the workspace this routine starts in ({}). {}",
        failed.join("; "),
        PREPARE_HINT
    )))
}

/// The answer a candidate whose receipt names another worktree or workspace
/// than the one resolved gets.
const RECEIPT_MISMATCH: &str = "its receipt names another place";

/// Whether a receipt gate `error` says the candidate is not (or no longer)
/// prepared where its receipt says: the only errors a candidate is passed
/// over for.
fn is_stale(error: &str) -> bool {
    error.contains("not authoritatively prepared")
        || error.contains("no current prepared workspace matches")
        || error.starts_with(RECEIPT_MISMATCH)
}

/// What to do when a machine has no workspace a routine can start in.
const PREPARE_HINT: &str = "Prepare a repo workspace there: an agent's prepare_remote_workspace MCP tool makes one (a story's workspace, from opening a story on the machine, is that story's and does not count). The next fire starts the agent in it, open in CodeSense or not";

/// Whether `target` (`owner/repo` or `host/owner/repo`) names `slug`: a bare
/// `owner/repo` names it on any host, `host/owner/repo` only on that host.
fn names_repo(target: &str, slug: &crate::domain::SourceRepoSlug) -> bool {
    let wanted = target.trim().to_ascii_lowercase();
    match wanted.split('/').count() {
        3 => wanted == slug.remote_key(),
        2 => wanted == format!("{}/{}", slug.owner, slug.repo).to_ascii_lowercase(),
        _ => false,
    }
}

/// The workspaces on `machine` a routine of `team_key` targeting `target`
/// may launch into, in the order to try them, among the prepared `places`
/// (from the receipts): a repo's own workspace there, never a story's (a
/// story's worktree is that story's work), prepared under the machine's live
/// terminal `server`, for a repo the team configures. `Err`
/// (`needs_preparation`) when the machine has none for it.
pub(crate) fn host_workspace(
    places: &[crate::mcp::remote_workspace_target::PreparedPlace],
    team_key: &str,
    machine: &str,
    server: &str,
    configured: &(dyn Fn(&crate::domain::SourceRepoSlug) -> bool + Sync),
    target: &super::model::RoutineTarget,
) -> Result<Vec<HostWorkspace>, String> {
    use super::model::RoutineTarget;
    let mut there: Vec<&crate::mcp::remote_workspace_target::PreparedPlace> = places
        .iter()
        .filter(|place| place.team_key == team_key && place.machine_id == machine)
        .filter(|place| place.server_id == server && !place.story)
        .filter(|place| configured(&place.source_repo))
        .collect();
    // By id: the same order on every fire.
    there.sort_by(|a, b| (&a.workspace_id, &a.handle).cmp(&(&b.workspace_id, &b.handle)));
    let chosen = |place: &&crate::mcp::remote_workspace_target::PreparedPlace| HostWorkspace {
        workspace_id: place.workspace_id.clone(),
        handle: place.handle.clone(),
        cwd: place.cwd.clone(),
    };
    let prepare = |what: &str| {
        format!("{machine} has no repo workspace prepared for {what} that this routine can start in. {PREPARE_HINT}")
    };
    let found: Vec<HostWorkspace> = match target {
        RoutineTarget::Repo(slug) => there
            .iter()
            .filter(|place| names_repo(slug, &place.source_repo))
            .map(chosen)
            .collect(),
        RoutineTarget::TeamWorkspace => {
            let mut repos: Vec<String> = there
                .iter()
                .map(|place| place.source_repo.remote_key())
                .collect();
            repos.sort();
            repos.dedup();
            if repos.len() > 1 {
                return Err(format!(
                    "{machine} has workspaces for {} repos: a routine starting an agent there names one (where: repo:<owner>/<repo>)",
                    repos.len()
                ));
            }
            there.iter().map(chosen).collect()
        }
    };
    if found.is_empty() {
        return Err(match target {
            RoutineTarget::Repo(slug) => prepare(slug),
            RoutineTarget::TeamWorkspace => prepare("the team"),
        });
    }
    Ok(found)
}

/// What a remote tab open answered.
pub(crate) struct TabAnswer<'a> {
    pub status: &'a str,
    pub code: Option<&'a str>,
    pub message: Option<&'a str>,
    pub session: Option<&'a str>,
    pub handle: Option<&'a str>,
    /// The open may be asked again (its launch is still preparing).
    pub retryable: bool,
    /// It answered an Attach: a refusal of the Attach itself (who may ask,
    /// how it was asked) says nothing about the launch.
    pub attach: bool,
}

/// What a remote tab open answered, as a routine launch reads it:
/// - `opened` / `alreadyLive` (and `attachedElsewhere`) is the agent's tab;
///   `ended`, that tab with its agent gone;
/// - a refusal starts nothing only when it is definitive: final
///   (non-retryable), or a launch still preparing that can never spawn now
///   (`preparationIncomplete`, `newIntentRequired`);
/// - an Attach refused for who asked or how (`accessDenied`, `invalid`) says
///   nothing about the launch: unknown;
/// - `pending` (still running on the machine) is pending;
/// - anything else (`unknown`, another retryable refusal) may have started
///   it: unknown.
pub(crate) fn tab_reply(answer: &TabAnswer<'_>) -> TabReply {
    use codesense_pty_core::session_wake::UndeliveredReason;
    let detail = || match (answer.code, answer.message) {
        (Some(code), Some(message)) => format!("{code}: {message}"),
        (Some(code), None) => code.to_string(),
        (None, Some(message)) => message.to_string(),
        (None, None) => answer.status.to_string(),
    };
    let tab = |ended: bool| match (answer.session, answer.handle) {
        (Some(session), Some(handle)) => {
            let (session, handle) = (session.to_string(), handle.to_string());
            if ended {
                TabReply::Ended { session, handle }
            } else {
                TabReply::Opened { session, handle }
            }
        }
        _ => TabReply::Unknown(format!("{}; it named no tab", detail())),
    };
    match (answer.status, answer.code) {
        ("opened" | "alreadyLive", _) | ("refused", Some("attachedElsewhere")) => tab(false),
        ("ended", _) => tab(true),
        ("pending", _) => TabReply::Pending(detail()),
        ("refused", Some("accessDenied" | "invalid")) if answer.attach => {
            TabReply::Unknown(detail())
        }
        ("refused", Some(codesense_pty_core::routine_arm::AGENT_RUNNING)) => {
            TabReply::Running(detail())
        }
        ("refused", code) => {
            let never_spawns = matches!(code, Some("preparationIncomplete" | "newIntentRequired"));
            if answer.retryable && !never_spawns {
                return TabReply::Unknown(detail());
            }
            TabReply::NotStarted {
                reason: match code {
                    Some(
                        "workspaceUnavailable"
                        | "agentSetupFailed"
                        | "preparationIncomplete"
                        | "newIntentRequired",
                    ) => UndeliveredReason::NeedsPreparation,
                    Some("unavailable") => UndeliveredReason::MachineOffline,
                    _ => UndeliveredReason::Unavailable,
                },
                detail: detail(),
            }
        }
        _ => TabReply::Unknown(detail()),
    }
}

/// The inbox a wake's notice goes to: the routine's own team (a personal
/// routine's, the signed-in account's team in view), for the signed-in
/// account. `None` when there is none.
pub(crate) fn notice_inbox(
    routine_key: &str,
    team_in_view: Option<(String, String)>,
) -> Option<(String, String)> {
    let routine_key = routine_key.split("#routine:").next().unwrap_or(routine_key);
    // A timer's (or a person's) wake names no routine: the team in view.
    let Some(key) = super::model::RoutineKey::parse(routine_key) else {
        return team_in_view;
    };
    if key.is_personal() {
        return team_in_view;
    }
    Some((key.org.clone(), key.team.clone()))
}

/// The team (org, team) of the workspace whose tab runs `session` on
/// `machine`, from the terminal layouts.
fn session_team(app: &AppHandle, machine: &str, session: &str) -> Option<(String, String)> {
    use crate::commands::machines::RuntimeLayoutReply;
    let replies = crate::commands::pty_layout::member_layout_snapshots_blocking(app)?.ok()?;
    replies.iter().find_map(|reply| {
        let (at, snapshot) = match reply {
            RuntimeLayoutReply::Live {
                target, snapshot, ..
            } => (&target.member.machine_id, snapshot.as_ref()),
            RuntimeLayoutReply::Historical { member, frozen, .. } => {
                (&member.machine_id, &frozen.snapshot)
            }
            RuntimeLayoutReply::Unavailable { .. } => return None,
        };
        if at != machine {
            return None;
        }
        snapshot.workspaces.iter().find_map(|(key, layout)| {
            layout
                .tabs
                .iter()
                .any(|tab| tab.session.as_str() == session)
                .then(|| team_of_workspace_key(key.as_str()))
                .flatten()
        })
    })
}

/// The team of a workspace key (`{org}:{team}|{workspace}`); `None` for a
/// workspace of no team.
pub(crate) fn team_of_workspace_key(key: &str) -> Option<(String, String)> {
    let team_key = key
        .split(crate::commands::pty_layout::WORKSPACE_KEY_SEPARATOR)
        .next()?;
    let (org, team) = crate::commands::pty_layout::parse_team_key(team_key);
    Some((org.to_string(), team?.to_string()))
}

/// Post `notice` to the human inbox, `needs-attention` with a Show-tab
/// backlink. Best effort.
pub(crate) fn post(app: &AppHandle, notice: WakeNotice) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = post_notice(app, notice).await {
            log::warn!(target:"routines", "wake notice not posted: {error}");
        }
    });
}

/// Spend notices use the current human inbox, with its existing goto-agent action and cap anchor.
fn store_spend_notice(store: &crate::weft::WeftStore, notice: &WakeNotice) -> Result<i64, String> {
    use crate::weft::inbox::{self, MessageContentType, MessageKind, MessageOnClick, HUMAN};
    let target = (!notice.routine_key.is_empty()).then(|| MessageOnClick::GotoAgent {
        agent_id: format!("{}#cap", notice.routine_key),
    });
    inbox::inbox_insert_with_reply_to(
        store,
        "CodeSense",
        HUMAN,
        &format!(
            "{}\n\n{}\n\n{}",
            notice.title(),
            notice.detail,
            notice.context()
        ),
        None,
        None,
        None,
        target.as_ref(),
        Some(MessageContentType::Plain),
        None,
        Some(&format!("{} — {}", notice.title(), notice.detail)),
        Some(MessageKind::NeedsYou),
    )
    .map_err(|e| e.to_string())
}

async fn post_notice(app: AppHandle, notice: WakeNotice) -> Result<(), String> {
    let Ok((home, user_id)) = crate::commands::route_to_context::resolve_ambient(&app).await else {
        log::warn!(target: "routines", "wake notice for {} not posted: signed out", notice.run_id);
        return Err("no signed-in inbox".into());
    };
    // SW-AGR-13: a note with no routine (a timer's or a person's wake)
    // goes to the team of the session's tab when its layout says, else
    // the team in view.
    let session_team = if notice.routine_key.is_empty() && !notice.session.is_empty() {
        let (layouts, machine, session) =
            (app.clone(), notice.machine.clone(), notice.session.clone());
        crate::commands::await_blocking("wake_notice_team", move || {
            Ok::<_, String>(session_team(&layouts, &machine, &session))
        })
        .await
        .ok()
        .flatten()
    } else {
        None
    };
    let in_view = match session_team {
        Some(team) => Some(team),
        None => crate::commands::route_to_context::derive_active_context(&app)
            .await
            .ok()
            .and_then(|ctx| Some((ctx.org_slug.clone(), ctx.team_name.clone()?))),
    };
    let Some((org, team)) = notice_inbox(&notice.routine_key, in_view) else {
        log::warn!(target: "routines", "wake notice for {} not posted: no team inbox", notice.run_id);
        return Err("no team inbox selected".into());
    };
    let path = match crate::weft::resolve_weft_store_path(
        &home,
        &Principal::from(user_id),
        &org,
        &team,
    ) {
        Ok(path) => path,
        Err(error) => {
            log::warn!(target: "routines", "wake notice for {} not posted: {error}", notice.run_id);
            return Err(error);
        }
    };
    let summary = format!("{} — {}", notice.title(), notice.context());
    if matches!(
        notice.kind,
        super::wake::NoticeKind::SpendWarn
            | super::wake::NoticeKind::SpendReached
            | super::wake::NoticeKind::SpendPending
    ) {
        let id = crate::commands::await_blocking("spend_cap_notice", move || {
            let store = crate::weft::WeftStore::open(path).map_err(|e| e.to_string())?;
            store_spend_notice(&store, &notice)
        })
        .await?;
        use crate::live_hub::inbox_events::{InboxChange, InboxEvent, INBOX_EVENT};
        let team = crate::commands::dashboard_launch::LaunchTeam { org, team };
        let _ = app.emit(
            INBOX_EVENT,
            InboxEvent::new(InboxChange::Stored, &team, [id]),
        );
        let _ = app.emit(crate::commands::agent_inbox::AGENT_INBOX_CHANGED_EVENT, ());
        return Ok(());
    }
    let backlink = notice.backlink();
    let posted = crate::commands::await_blocking("routine_wake_notice", move || {
        let store = crate::weft::WeftStore::open(path).map_err(|e| e.to_string())?;
        crate::weft::wv_upsert_attention(
            &store,
            None,
            Some("needs-you"),
            Some(&summary),
            Some("post"),
            Some(&backlink),
            None,
            Some("needs-attention"),
        )
        .map_err(|e| e.to_string())
    })
    .await;
    posted?;
    let _ = app.emit(crate::weft::WEFT_ATTENTION_CHANGED_EVENT, ());
    Ok(())
}

#[cfg(test)]
mod spend_notice_seams {
    use super::*;
    #[test]
    fn threshold_and_pending_notices_reach_the_human_inbox_with_a_cap_destination() {
        use crate::routines::wake::NoticeKind;
        use crate::weft::inbox::{self, MessageKind, MessageOnClick, HUMAN};
        let dir = tempfile::tempdir().unwrap();
        let store = crate::weft::WeftStore::open(dir.path().join("weft.sqlite")).unwrap();
        for kind in [
            NoticeKind::SpendWarn,
            NoticeKind::SpendReached,
            NoticeKind::SpendPending,
        ] {
            let notice = WakeNotice {
                kind,
                run_id: String::new(),
                routine_key: "acme/api/backend/worker".into(),
                machine: "local".into(),
                session: String::new(),
                detail: "agent worker: spend $50, cap $50".into(),
            };
            let id = store_spend_notice(&store, &notice).unwrap();
            let message = inbox::inbox_get(&store, id).unwrap().unwrap();
            assert_eq!(message.to, HUMAN);
            assert_eq!(message.kind, Some(MessageKind::NeedsYou));
            assert_eq!(
                message.on_click,
                Some(MessageOnClick::GotoAgent {
                    agent_id: "acme/api/backend/worker#cap".into()
                })
            );
        }
    }
    #[test]
    fn a_routine_rate_notice_keeps_the_owner_inbox_and_routine_cap_destination() {
        use crate::weft::inbox::{self, MessageOnClick};
        let target = "acme/api/backend/worker#routine:acme/api/backend/daily";
        assert_eq!(
            notice_inbox(target, None),
            Some(("acme/api".into(), "backend".into()))
        );
        let dir = tempfile::tempdir().unwrap();
        let store = crate::weft::WeftStore::open(dir.path().join("weft.sqlite")).unwrap();
        let notice = WakeNotice {
            kind: crate::routines::wake::NoticeKind::SpendPending,
            run_id: String::new(),
            routine_key: target.into(),
            machine: "local".into(),
            session: String::new(),
            detail: "Model rate unknown-model: input $1.25/M awaits native acceptance".into(),
        };
        let id = store_spend_notice(&store, &notice).unwrap();
        assert_eq!(
            inbox::inbox_get(&store, id).unwrap().unwrap().on_click,
            Some(MessageOnClick::GotoAgent {
                agent_id: format!("{target}#cap")
            })
        );
    }
}
