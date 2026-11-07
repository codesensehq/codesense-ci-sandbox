// SPDX-License-Identifier: MIT
//! SW-AGR-19 PR-8: "Update all terminal servers", run by the app. One run at
//! a time updates each reviewed machine in turn: remote machines first, This
//! Mac after them, and the machine an MCP caller's own tab runs on last of
//! all. Each machine is updated only as it was reviewed (`expected`: the same
//! server, the same lifetime, no more sessions than were confirmed); a changed
//! one is skipped, never updated past what was confirmed. A failed machine is
//! reported and the run continues, except that This Mac is not updated after
//! a remote machine was left without a server (or its result is unknown). The
//! run lives here, not in a window: progress is emitted on
//! [`UPDATE_RUN_EVENT`], and a caller is told the outcome when it ends.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use tauri::Manager;

use super::restore::RestoreSummary;
use super::LOCAL_MACHINE_ID;

/// The event every run's progress goes out on (its [`UpdateRun`]).
pub const UPDATE_RUN_EVENT: &str = "terminal-servers-update";

/// What the person (or an agent) reviewed for one machine: the update binds
/// to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerExpectation {
    pub server_id: String,
    pub epoch: String,
    /// How many sessions were confirmed ending (when `session_ids` is given,
    /// its length).
    #[serde(default)]
    pub sessions: u32,
    /// This Mac's server process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// The sessions confirmed ending. A remote machine running any other is
    /// skipped; This Mac's must be exactly these (a changed set is refused
    /// before anything stops).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_ids: Option<Vec<String>>,
}

/// One machine to update, as reviewed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateItem {
    pub machine_id: String,
    pub expected: ServerExpectation,
}

/// What came of one machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum OutcomeKind {
    Updated,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateOutcome {
    pub machine_id: String,
    pub label: String,
    pub kind: OutcomeKind,
    pub message: String,
    /// Its old server is gone and the new one is not up.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub old_stopped: bool,
    /// Asked, but its result is unknown.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub unconfirmed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restored: Option<RestoreSummary>,
}

impl UpdateOutcome {
    fn new(item: &Planned, kind: OutcomeKind, message: impl Into<String>) -> Self {
        Self {
            machine_id: item.machine_id.clone(),
            label: item.label.clone(),
            kind,
            message: message.into(),
            old_stopped: false,
            unconfirmed: false,
            restored: None,
        }
    }

    /// It may have left its machine without a terminal server.
    fn strands(&self) -> bool {
        self.kind == OutcomeKind::Failed && (self.old_stopped || self.unconfirmed)
    }
}

/// One run, as it stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateRun {
    pub op_id: String,
    pub running: bool,
    /// The machines, in the order they are updated.
    pub machines: Vec<String>,
    /// The machine being updated now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    pub outcomes: Vec<UpdateOutcome>,
}

/// A machine in a run, with its label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub machine_id: String,
    pub label: String,
    pub expected: ServerExpectation,
}

/// The order a run updates `items` in: remote machines first (in the order
/// given), then This Mac, then `caller_machine` (an MCP caller's own) last.
pub fn update_order(items: Vec<Planned>, caller_machine: Option<&str>) -> Vec<Planned> {
    let (caller, rest): (Vec<_>, Vec<_>) = items
        .into_iter()
        .partition(|item| Some(item.machine_id.as_str()) == caller_machine);
    let (local, remote): (Vec<_>, Vec<_>) = rest
        .into_iter()
        .partition(|item| item.machine_id == LOCAL_MACHINE_ID);
    remote.into_iter().chain(local).chain(caller).collect()
}

/// Why `item` is not updated in this run, given what came before: This Mac
/// waits after a remote machine was (possibly) left without a server.
pub fn held_back(item: &Planned, before: &[UpdateOutcome]) -> Option<String> {
    if item.machine_id != LOCAL_MACHINE_ID {
        return None;
    }
    before
        .iter()
        .find(|outcome| outcome.strands())
        .map(|stranded| {
            format!(
                "{} may be left without a terminal server; update This Mac after it is fixed",
                stranded.label
            )
        })
}

impl ServerExpectation {
    /// The confirmed session count: the confirmed ids', else the count given.
    pub fn confirmed_count(&self) -> u32 {
        self.session_ids.as_ref().map_or(self.sessions, |ids| {
            u32::try_from(ids.len()).unwrap_or(u32::MAX)
        })
    }
}

/// A remote server as it is now (`sessions`: the ids it runs), against what
/// was reviewed: `None` when it may be updated, else why not. A session not
/// confirmed (one that started since) is never ended unreviewed.
pub fn remote_changed(
    expected: &ServerExpectation,
    server_id: &str,
    epoch: &str,
    sessions: Option<&[String]>,
) -> Option<String> {
    if server_id != expected.server_id || epoch != expected.epoch {
        return Some(
            "its terminal server changed since it was reviewed; review it again".to_string(),
        );
    }
    let Some(now) = sessions else {
        // Not read: the update would not be bound to what was confirmed.
        return Some("its sessions could not be read; update it on its own".to_string());
    };
    match &expected.session_ids {
        Some(confirmed) => {
            let unconfirmed = now.iter().filter(|id| !confirmed.contains(id)).count();
            (unconfirmed > 0).then(|| {
                format!(
                    "{unconfirmed} session(s) started there since it was reviewed; review it again"
                )
            })
        }
        // Never bound to a count alone: a session swapped for another would
        // end unreviewed.
        None => Some("it was not reviewed with its sessions; review it again".to_string()),
    }
}

struct Runs {
    current: Option<String>,
    runs: HashMap<String, UpdateRun>,
    /// Finished runs, oldest first.
    finished: Vec<String>,
}

fn runs() -> &'static Mutex<Runs> {
    static RUNS: OnceLock<Mutex<Runs>> = OnceLock::new();
    RUNS.get_or_init(|| {
        Mutex::new(Runs {
            current: None,
            runs: HashMap::new(),
            finished: Vec::new(),
        })
    })
}

fn lock() -> std::sync::MutexGuard<'static, Runs> {
    runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A run as it stands (`None`: no such run in this app's lifetime).
pub fn run_status(op_id: &str) -> Option<UpdateRun> {
    lock().runs.get(op_id).cloned()
}

/// The run going on now, if any (a finished one is read by its `opId`).
pub fn current_run() -> Option<UpdateRun> {
    let runs = lock();
    runs.current
        .as_ref()
        .and_then(|id| runs.runs.get(id))
        .filter(|run| run.running)
        .cloned()
}

/// How many finished runs are kept to be read by their `opId`.
const KEPT_RUNS: usize = 16;

fn publish(app: &tauri::AppHandle, run: &UpdateRun) {
    use tauri::Emitter;
    remember(run);
    let _ = app.emit(UPDATE_RUN_EVENT, run);
}

/// Keep `run` as it stands; a finished one stops being current, and only the
/// last [`KEPT_RUNS`] finished ones are kept.
fn remember(run: &UpdateRun) {
    let mut runs = lock();
    runs.runs.insert(run.op_id.clone(), run.clone());
    if !run.running {
        if runs.current.as_deref() == Some(run.op_id.as_str()) {
            runs.current = None;
        }
        if !runs.finished.contains(&run.op_id) {
            runs.finished.push(run.op_id.clone());
        }
        while runs.finished.len() > KEPT_RUNS {
            let oldest = runs.finished.remove(0);
            runs.runs.remove(&oldest);
        }
    }
}

/// SW-AGR-19 PR-8 (AC9): an update call ends sessions only when it says so.
pub fn require_confirmation(confirm_end_sessions: bool) -> Result<(), String> {
    if confirm_end_sessions {
        Ok(())
    } else {
        Err("confirmEndSessions must be true: updating ends every terminal and agent each server runs".to_string())
    }
}

/// SW-AGR-19 PR-8: one update at a time — a single machine's update (the
/// person's or an agent's) is refused while an Update all run goes.
pub(crate) fn refuse_while_running() -> Result<(), String> {
    match current_run() {
        Some(_) => Err(
            "terminal servers are being updated (Update all); try again when that run finishes"
                .to_string(),
        ),
        None => Ok(()),
    }
}

/// What the person or agent reviews for one out-of-date machine: its
/// expectation, or why it cannot be updated now.
pub(crate) async fn expectation(
    app: &tauri::AppHandle,
    machine_id: &str,
) -> Result<ServerExpectation, String> {
    if machine_id == LOCAL_MACHINE_ID {
        let update = super::local_update::local_terminal_server_update(app.clone())
            .await?
            .ok_or("This Mac's terminal server is already this app's build")?;
        return Ok(ServerExpectation {
            server_id: update.expected.server_id,
            epoch: update.expected.epoch,
            sessions: u32::try_from(update.expected.session_ids.len()).unwrap_or(u32::MAX),
            pid: Some(update.expected.pid),
            session_ids: Some(update.expected.session_ids),
        });
    }
    let backend = app
        .state::<crate::commands::host_connection_manager::HostConnectionManager>()
        .connected_backend(machine_id)
        .map_err(|_| "connect the machine first".to_string())?;
    let client = backend.client();
    let ids = crate::commands::await_blocking("terminal server sessions", move || {
        Ok(client.live_session_id_list())
    })
    .await?
    .ok_or("its sessions could not be read")?;
    Ok(ServerExpectation {
        server_id: backend.server_id().to_string(),
        epoch: backend.client().epoch().to_string(),
        sessions: u32::try_from(ids.len()).unwrap_or(u32::MAX),
        pid: None,
        session_ids: Some(ids),
    })
}

/// Start a run over `items` (in [`update_order`]). Refused while another runs.
/// `on_done` is told the run as it ended (an MCP caller's inbox).
pub(crate) fn start(
    app: &tauri::AppHandle,
    items: Vec<Planned>,
    caller_machine: Option<&str>,
    on_done: impl FnOnce(UpdateRun) -> futures_util::future::BoxFuture<'static, ()> + Send + 'static,
) -> Result<UpdateRun, String> {
    let order = update_order(items, caller_machine);
    let op_id = uuid::Uuid::new_v4().simple().to_string();
    let run = UpdateRun {
        op_id: op_id.clone(),
        running: true,
        machines: order.iter().map(|item| item.machine_id.clone()).collect(),
        current: order.first().map(|item| item.machine_id.clone()),
        outcomes: Vec::new(),
    };
    {
        let mut runs = lock();
        if let Some(current) = runs.current.as_ref().and_then(|id| runs.runs.get(id)) {
            if current.running {
                return Err(
                    "terminal servers are being updated already; wait for that run to finish"
                        .to_string(),
                );
            }
        }
        runs.current = Some(op_id.clone());
        runs.runs.insert(op_id.clone(), run.clone());
    }
    let app = app.clone();
    let started = run.clone();
    let labels: HashMap<String, String> = order
        .iter()
        .map(|item| (item.machine_id.clone(), item.label.clone()))
        .collect();
    tauri::async_runtime::spawn(async move {
        let worker_app = app.clone();
        let worker_run = started.clone();
        // The run itself, on its own task: should it panic, the run still
        // ends (the machine it was on reads failed, unconfirmed), and never
        // holds every later run.
        let worker = tauri::async_runtime::spawn(async move {
            let mut run = worker_run;
            for item in &order {
                run.current = Some(item.machine_id.clone());
                publish(&worker_app, &run);
                let outcome = match held_back(item, &run.outcomes) {
                    Some(why) => UpdateOutcome::new(item, OutcomeKind::Skipped, why),
                    None => update_one(&worker_app, item).await,
                };
                log::info!(target: "machines", "update all {}: {} {:?}: {}", run.op_id, item.machine_id, outcome.kind, outcome.message);
                run.outcomes.push(outcome);
                publish(&worker_app, &run);
            }
            run
        });
        let mut run = match worker.await {
            Ok(run) => run,
            Err(error) => {
                log::error!(target: "machines", "update all {} stopped: {error}", started.op_id);
                stopped_run(run_status(&started.op_id).unwrap_or(started), &labels)
            }
        };
        run.running = false;
        run.current = None;
        publish(&app, &run);
        on_done(run).await;
    });
    Ok(run)
}

/// A run whose task stopped unexpectedly: the machine it was on reads failed,
/// its result unknown.
pub fn stopped_run(mut run: UpdateRun, labels: &HashMap<String, String>) -> UpdateRun {
    if let Some(machine) = run.current.clone() {
        if !run
            .outcomes
            .iter()
            .any(|outcome| outcome.machine_id == machine)
        {
            run.outcomes.push(UpdateOutcome {
                label: labels
                    .get(&machine)
                    .cloned()
                    .unwrap_or_else(|| machine.clone()),
                machine_id: machine,
                kind: OutcomeKind::Failed,
                message: "the update stopped unexpectedly; check it in Machines".into(),
                old_stopped: false,
                unconfirmed: true,
                restored: None,
            });
        }
    }
    run.running = false;
    run.current = None;
    run
}

/// Update one machine as it was reviewed.
async fn update_one(app: &tauri::AppHandle, item: &Planned) -> UpdateOutcome {
    if item.machine_id == LOCAL_MACHINE_ID {
        return update_local(app, item).await;
    }
    let machine = item.machine_id.clone();
    let backend = match app
        .state::<crate::commands::host_connection_manager::HostConnectionManager>()
        .connected_backend(&machine)
    {
        Ok(backend) => backend,
        Err(_) => {
            return UpdateOutcome::new(
                item,
                OutcomeKind::Skipped,
                "it is not connected; connect it and review it again",
            )
        }
    };
    let client = backend.client();
    let sessions = crate::commands::await_blocking("terminal server sessions", move || {
        Ok(client.live_session_id_list())
    })
    .await
    .ok()
    .flatten();
    if let Some(why) = remote_changed(
        &item.expected,
        backend.server_id(),
        backend.client().epoch(),
        sessions.as_deref(),
    ) {
        return UpdateOutcome::new(item, OutcomeKind::Skipped, why);
    }
    let plan =
        match super::reinstall::machine_terminal_server_update_mode(app.clone(), machine.clone())
            .await
        {
            Ok(plan) => plan,
            Err(why) => return UpdateOutcome::new(item, OutcomeKind::Failed, why),
        };
    use super::reinstall::TerminalServerUpdateMode;
    match plan.mode {
        TerminalServerUpdateMode::Reinstall => reinstall_outcome(
            item,
            super::reinstall::reinstall_now(
                app.clone(),
                machine,
                true,
                Some(item.expected.confirmed_count()),
                item.expected.session_ids.clone(),
            )
            .await,
        ),
        TerminalServerUpdateMode::Replace => {
            use crate::commands::host_connection_manager::{prepare_replacement_now, restart_now};
            let prepared = match prepare_replacement_now(app.clone(), machine.clone()).await {
                Ok(prepared) => prepared,
                Err(why) => return UpdateOutcome::new(item, OutcomeKind::Failed, why),
            };
            // Read again right before the restart: a session that started
            // since the review refuses it (the unused reservation lapses).
            let now = super::reinstall::live_session_ids(app, &machine).await;
            if let Some(why) = remote_changed(
                &item.expected,
                backend.server_id(),
                backend.client().epoch(),
                now.as_deref(),
            ) {
                return UpdateOutcome::new(item, OutcomeKind::Skipped, why);
            }
            // A reservation made while disconnected carries its own count.
            if let Some(why) = prepared_over(&item.expected, prepared.count) {
                return UpdateOutcome::new(item, OutcomeKind::Skipped, why);
            }
            replace_outcome(
                item,
                restart_now(app.clone(), machine, prepared.server_token).await,
            )
        }
    }
}

/// A prepared replacement holding more sessions than confirmed: why it is
/// not restarted.
pub fn prepared_over(expected: &ServerExpectation, prepared: Option<u32>) -> Option<String> {
    let confirmed = expected.confirmed_count();
    prepared.filter(|count| *count > confirmed).map(|count| {
        format!("{count} sessions run there now, not the {confirmed} confirmed; review it again")
    })
}

/// What a reinstall's answer comes to for the run.
pub fn reinstall_outcome(
    item: &Planned,
    result: Result<super::reinstall::ReinstallResult, String>,
) -> UpdateOutcome {
    use super::reinstall::ReinstallResult;
    match result {
        Ok(ReinstallResult::Reinstalled { sessions, restored }) => {
            let mut outcome = UpdateOutcome::new(
                item,
                OutcomeKind::Updated,
                restored
                    .as_ref()
                    .map(|summary| summary.message.clone())
                    .unwrap_or_else(|| format!("{sessions} terminals closed")),
            );
            outcome.restored = restored;
            outcome
        }
        Ok(ReinstallResult::SessionsChanged { sessions }) => UpdateOutcome::new(
            item,
            OutcomeKind::Skipped,
            format!(
                "{sessions} sessions run there now, not the {} confirmed; review it again",
                item.expected.confirmed_count()
            ),
        ),
        Ok(ReinstallResult::Failed {
            old_stopped,
            repair,
        }) => {
            let mut outcome = UpdateOutcome::new(item, OutcomeKind::Failed, repair);
            outcome.old_stopped = old_stopped;
            outcome
        }
        // Refused before anything stopped.
        Err(why) => UpdateOutcome::new(item, OutcomeKind::Failed, why),
    }
}

/// What a replacement's answer comes to for the run.
pub fn replace_outcome(
    item: &Planned,
    result: Result<crate::commands::host_connection_manager::ReplacementResult, String>,
) -> UpdateOutcome {
    use crate::commands::host_connection_manager::ReplacementResult;
    match result {
        Ok(ReplacementResult::Replaced { count, restored }) => {
            let mut outcome = UpdateOutcome::new(
                item,
                OutcomeKind::Updated,
                restored
                    .as_ref()
                    .map(|summary| summary.message.clone())
                    .unwrap_or_else(|| match count {
                        Some(count) => format!("{count} terminals closed"),
                        None => "updated".to_string(),
                    }),
            );
            outcome.restored = restored;
            outcome
        }
        Ok(ReplacementResult::Failed(failed)) => {
            let mut outcome = UpdateOutcome::new(item, OutcomeKind::Failed, failed.repair);
            outcome.old_stopped = failed.old_stopped;
            outcome
        }
        // The token refused (missing, used, expired, stale): nothing stopped.
        Err(why) if why.contains("replacement token") => {
            UpdateOutcome::new(item, OutcomeKind::Failed, why)
        }
        // Asked, but the answer is lost: it may have no server now.
        Err(why) => {
            let mut outcome = UpdateOutcome::new(
                item,
                OutcomeKind::Failed,
                format!("its result could not be confirmed; check it in Machines ({why})"),
            );
            outcome.unconfirmed = true;
            outcome
        }
    }
}

async fn update_local(app: &tauri::AppHandle, item: &Planned) -> UpdateOutcome {
    let (Some(pid), Some(session_ids)) = (item.expected.pid, item.expected.session_ids.clone())
    else {
        return UpdateOutcome::new(
            item,
            OutcomeKind::Skipped,
            "This Mac was not reviewed with its server and sessions; review it again",
        );
    };
    let expected = crate::terminal::session_server::LocalActivityUpgrade {
        server_id: item.expected.server_id.clone(),
        epoch: item.expected.epoch.clone(),
        pid,
        session_ids,
    };
    match super::local_update::update_local_now(app.clone(), expected).await {
        Ok(done) => {
            let mut outcome = UpdateOutcome::new(
                item,
                OutcomeKind::Updated,
                done.restored
                    .as_ref()
                    .map(|summary| summary.message.clone())
                    .unwrap_or_else(|| format!("{} terminals closed", done.sessions)),
            );
            outcome.restored = done.restored;
            outcome
        }
        Err(error) if error.changed => UpdateOutcome::new(
            item,
            OutcomeKind::Skipped,
            "its sessions changed since they were reviewed; update This Mac on its own",
        ),
        Err(error) => {
            let mut outcome = UpdateOutcome::new(
                item,
                OutcomeKind::Failed,
                if error.old_stopped {
                    format!("the old terminal server was signalled, but the new one didn't start; quit and reopen CodeSense to start it ({})", error.message)
                } else {
                    error.message
                },
            );
            outcome.old_stopped = error.old_stopped;
            outcome
        }
    }
}

/// The run's end as one message: "N updated, N skipped, N failed", then each
/// machine.
pub fn run_summary(run: &UpdateRun) -> String {
    let count = |kind| run.outcomes.iter().filter(|o| o.kind == kind).count();
    let mut counts = Vec::new();
    for (kind, word) in [
        (OutcomeKind::Updated, "updated"),
        (OutcomeKind::Failed, "failed"),
        (OutcomeKind::Skipped, "skipped"),
    ] {
        if count(kind) > 0 {
            counts.push(format!("{} {word}", count(kind)));
        }
    }
    let mut text = format!(
        "Terminal server update {}: {}.",
        run.op_id,
        if counts.is_empty() {
            "nothing to update".to_string()
        } else {
            counts.join(", ")
        }
    );
    for outcome in &run.outcomes {
        let kind = match outcome.kind {
            OutcomeKind::Updated => "updated",
            OutcomeKind::Skipped => "skipped",
            OutcomeKind::Failed => "failed",
        };
        let prefix = format!("{}: ", outcome.label);
        let message = outcome
            .message
            .strip_prefix(&prefix)
            .unwrap_or(&outcome.message);
        text.push_str(&format!("\n- {} {kind}: {message}", outcome.label));
    }
    text
}

/// The reviewed expectations by machine, as an MCP call passes them.
pub type Expectations = BTreeMap<String, ServerExpectation>;

/// One live tab an update ends, for the review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerTab {
    /// Its session id (as `expected.sessionIds` names it).
    pub session: String,
    pub workspace: String,
    pub name: String,
    /// The agent it runs, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The calling agent's own tab: it ends with this update.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_caller: bool,
}

/// The live tabs of `snapshot`, marking `caller_session`'s.
pub fn server_tabs(
    snapshot: &codesense_pty_core::layout::LayoutSnapshot,
    caller_session: Option<&str>,
) -> Vec<ServerTab> {
    snapshot
        .workspaces
        .iter()
        .flat_map(|(key, workspace)| {
            let name = if workspace.descriptor.display_name.is_empty() {
                key.as_str()
                    .split_once('|')
                    .map_or(key.as_str(), |(_, form)| form)
                    .to_string()
            } else {
                workspace.descriptor.display_name.clone()
            };
            workspace
                .tabs
                .iter()
                .filter(|tab| tab.state == codesense_pty_core::layout::EntryState::Live)
                .map(move |tab| ServerTab {
                    session: tab.session.as_str().to_string(),
                    workspace: name.clone(),
                    name: tab.name.clone(),
                    agent: super::restore::agent_of(&tab.tags),
                    is_caller: Some(tab.session.as_str()) == caller_session,
                })
        })
        .collect()
}

/// The live tabs `machine_id`'s terminal server runs now.
pub(crate) async fn machine_tabs(
    app: &tauri::AppHandle,
    machine_id: &str,
    caller_session: Option<&str>,
) -> Option<Vec<ServerTab>> {
    let replies =
        match crate::commands::pty_layout::webview_member_layout_snapshots(app.clone()).await {
            Some(Ok(replies)) => replies,
            _ => return None,
        };
    match super::restore::server_layout(&replies, machine_id) {
        Some(super::RuntimeLayoutReply::Live { snapshot, .. }) => {
            Some(server_tabs(&snapshot, caller_session))
        }
        Some(super::RuntimeLayoutReply::Historical { frozen, .. }) => {
            Some(server_tabs(&frozen.snapshot, caller_session))
        }
        _ => None,
    }
}

/// The sessions `expected` names that no listed tab is (a background
/// session, or one the layout no longer shows): they end too.
pub fn untabbed_sessions(expected: Option<&ServerExpectation>, tabs: &[ServerTab]) -> Vec<String> {
    expected
        .and_then(|expected| expected.session_ids.as_ref())
        .map(|ids| {
            ids.iter()
                .filter(|id| !tabs.iter().any(|tab| &tab.session == *id))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a machine's server, as reviewed now (`now`), is still the one
/// `expected` names: the same server and lifetime, This Mac's same process
/// and sessions, and no more sessions than were confirmed.
pub fn still_as_reviewed(expected: &ServerExpectation, now: &ServerExpectation) -> bool {
    let same_server = expected.server_id == now.server_id
        && expected.epoch == now.epoch
        && (expected.pid.is_none() || expected.pid == now.pid);
    let sessions_confirmed = match (&expected.session_ids, &now.session_ids) {
        // This Mac: exactly the confirmed set.
        (Some(confirmed), Some(running)) if expected.pid.is_some() => confirmed == running,
        // A remote machine: no session that was not confirmed.
        (Some(confirmed), Some(running)) => running.iter().all(|id| confirmed.contains(id)),
        // Never bound to a count alone.
        _ => false,
    };
    same_server && sessions_confirmed
}

/// The machines an update call updates, and those it skips (with why).
pub type Selection = (Vec<String>, Vec<(String, String)>);

/// Which machines an update call names: `machine_ids`, or every out-of-date
/// one (`all`). An id that names no machine refuses the call; one whose
/// server is not out of date is skipped, with why. `known` is every machine
/// the caller sees, with whether its server is out of date.
pub fn select(
    known: &[(String, bool)],
    machine_ids: Option<&[String]>,
    all: bool,
) -> Result<Selection, String> {
    let named: Vec<String> = match (machine_ids, all) {
        (Some(ids), false) if !ids.is_empty() => ids.to_vec(),
        (None, true) => known
            .iter()
            .filter(|(_, older)| *older)
            .map(|(id, _)| id.clone())
            .collect(),
        (Some(_), true) => return Err("pass machineIds or all, not both".to_string()),
        _ => return Err("pass machineIds (a non-empty list) or all: true".to_string()),
    };
    let mut chosen = Vec::new();
    let mut skipped = Vec::new();
    for id in named {
        match known.iter().find(|(known, _)| *known == id) {
            None => return Err(format!("unknown machine {id:?}; list_machines lists them")),
            Some((_, true)) => {
                if !chosen.contains(&id) {
                    chosen.push(id)
                }
            }
            Some((_, false)) => {
                skipped.push((id, "its terminal server is not out of date".to_string()))
            }
        }
    }
    Ok((chosen, skipped))
}

/// One out-of-date machine for `list_out_of_date_terminal_servers`: its
/// `list_machines` row, the tabs an update ends, and what to pass back as
/// `expected` (or why it cannot be updated now).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutOfDateServer {
    #[serde(flatten)]
    pub machine: super::MachineSummary,
    pub tabs: Vec<ServerTab>,
    /// Its tabs could not be read: `tabs` is empty, not "nothing ends".
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub tabs_unknown: bool,
    /// Sessions that end with no listed tab (background ones included).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub untabbed_sessions: Vec<String>,
    /// The caller's own tab runs on this machine.
    pub is_caller: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<ServerExpectation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

/// Whether `machine`'s server is out of date (older than this app).
fn out_of_date(machine: &super::MachineSummary) -> bool {
    machine
        .terminal_server
        .as_ref()
        .is_some_and(|server| server.relation == super::terminal_server_info::ServerRelation::Older)
}

/// SW-AGR-19 PR-8: exactly the `list_machines` rows whose terminal server is
/// older than this app, with their tabs and `expected`.
pub(crate) async fn list_out_of_date(
    app: &tauri::AppHandle,
    org_slug: String,
    team: Option<String>,
    caller: Option<&crate::delivery::authority::SessionRef>,
) -> Result<Vec<OutOfDateServer>, String> {
    let machines = super::list_machines(app.clone(), org_slug, team, None).await?;
    let mut listed = Vec::new();
    for machine in machines.into_iter().filter(out_of_date) {
        let caller_here = caller.filter(|caller| caller.machine == machine.machine_id);
        let tabs = machine_tabs(
            app,
            &machine.machine_id,
            caller_here.map(|caller| caller.session.as_str()),
        )
        .await;
        let (expected, unavailable) = match expectation(app, &machine.machine_id).await {
            Ok(expected) => (Some(expected), None),
            Err(why) => (None, Some(why)),
        };
        let tabs_unknown = tabs.is_none();
        let tabs = tabs.unwrap_or_default();
        listed.push(OutOfDateServer {
            is_caller: caller_here.is_some(),
            untabbed_sessions: if tabs_unknown {
                Vec::new()
            } else {
                untabbed_sessions(expected.as_ref(), &tabs)
            },
            tabs_unknown,
            machine,
            tabs,
            expected,
            unavailable,
        });
    }
    Ok(listed)
}

/// SW-AGR-19 PR-8: `update_terminal_servers`. Refused (nothing starts) for an
/// unknown machine, a machine without its `expected`, or one whose server is
/// no longer as listed; one that is not out of date is skipped, with why. The
/// run is asynchronous: its outcome goes to the caller's inbox (else the
/// human Inbox).
pub(crate) async fn update_from_mcp(
    app: &tauri::AppHandle,
    org_slug: String,
    team: Option<String>,
    machine_ids: Option<Vec<String>>,
    all: bool,
    expected: Expectations,
    caller: Option<crate::delivery::authority::SessionRef>,
) -> Result<serde_json::Value, String> {
    let machines = super::list_machines(app.clone(), org_slug, team, None).await?;
    let known: Vec<(String, bool)> = machines
        .iter()
        .map(|machine| (machine.machine_id.clone(), out_of_date(machine)))
        .collect();
    let (chosen, skipped) = select(&known, machine_ids.as_deref(), all)?;
    let skipped_json = skipped
        .iter()
        .map(|(machine_id, reason)| serde_json::json!({"machineId": machine_id, "reason": reason}))
        .collect::<Vec<_>>();
    if chosen.is_empty() {
        return Ok(serde_json::json!({ "opId": null, "machines": [], "skipped": skipped_json }));
    }
    let mut planned = Vec::with_capacity(chosen.len());
    for id in chosen {
        let reviewed = expected.get(&id).ok_or_else(|| {
            format!("no expected for {id:?}: pass each machine's expected from list_out_of_date_terminal_servers")
        })?;
        if reviewed.session_ids.is_none() {
            return Err(format!(
                "{id:?}'s expected has no sessionIds: pass it exactly as list_out_of_date_terminal_servers returned it"
            ));
        }
        let now = expectation(app, &id)
            .await
            .map_err(|why| format!("{id:?} cannot be updated now: {why}"))?;
        if !still_as_reviewed(reviewed, &now) {
            return Err(format!(
                "{id:?} is not as listed (its server or sessions changed): list it again and confirm what ends now"
            ));
        }
        planned.push(Planned {
            label: super::label_of(app, &id).await,
            machine_id: id,
            expected: reviewed.clone(),
        });
    }
    let report_app = app.clone();
    // The caller's inbox address, read now: its own tab may end in the run.
    let caller_session = caller.as_ref().map(|caller| {
        crate::commands::agent_inbox::AppInboxHost::port(app.clone())
            .handle_for_session(&caller.session)
            .unwrap_or_else(|| caller.session.clone())
    });
    let caller_machine = caller.as_ref().map(|caller| caller.machine.clone());
    let has_caller = caller_session.is_some();
    let run = start(
        app,
        planned,
        caller.as_ref().map(|caller| caller.machine.as_str()),
        move |run| {
            Box::pin(async move {
                let text = run_summary(&run);
                if let Some(session) = &caller_session {
                    let host = crate::commands::agent_inbox::AppInboxHost::port(report_app.clone());
                    let _ = crate::commands::agent_inbox::note_to_session(
                        &host,
                        session,
                        &text,
                        "Terminal server update finished",
                    )
                    .await;
                }
                // No caller, or its own machine was updated (its tab, and the
                // session that asked, ended with it): the human Inbox says it.
                let caller_updated = caller_machine.as_ref().is_some_and(|machine| {
                    // Updated, or left without its server: either way its tab
                    // ended.
                    run.outcomes.iter().any(|o| {
                        &o.machine_id == machine && (o.kind == OutcomeKind::Updated || o.strands())
                    })
                });
                if caller_session.is_none() || caller_updated {
                    super::restore::post_summary(&report_app, text).await;
                }
            })
        },
    )?;
    let warning = (!has_caller
        && run.machines.iter().any(|machine| machine != LOCAL_MACHINE_ID))
    .then_some(
        "no sessionId: the outcome goes to the human Inbox, and if your own tab runs on one of these machines it may end before the others are updated",
    );
    let mut reply = serde_json::json!({
        "opId": run.op_id,
        "machines": run.machines,
        "skipped": skipped_json,
    });
    if let Some(warning) = warning {
        reply["warning"] = warning.into();
    }
    Ok(reply)
}

/// SW-AGR-19: start updating the reviewed machines (the app's Update all).
#[tauri::command]
pub async fn start_terminal_servers_update(
    app: tauri::AppHandle,
    items: Vec<UpdateItem>,
) -> Result<UpdateRun, String> {
    let mut planned = Vec::with_capacity(items.len());
    for item in items {
        planned.push(Planned {
            label: super::label_of(&app, &item.machine_id).await,
            machine_id: item.machine_id,
            expected: item.expected,
        });
    }
    start(&app, planned, None, |_| Box::pin(async {}))
}

/// SW-AGR-19: an Update all run as it stands — `opId`'s, else the latest
/// (a window reopening Machines shows where it is).
#[tauri::command]
pub async fn terminal_servers_update_status(
    op_id: Option<String>,
) -> Result<Option<UpdateRun>, String> {
    Ok(match op_id {
        Some(op_id) => run_status(&op_id),
        None => current_run(),
    })
}

/// SW-AGR-19: what updating `machine_id`'s terminal server binds to now, for
/// the confirmation (an error says why it cannot be updated).
#[tauri::command]
pub async fn review_terminal_server_update(
    app: tauri::AppHandle,
    machine_id: String,
) -> Result<ServerExpectation, String> {
    expectation(&app, &machine_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planned(id: &str) -> Planned {
        Planned {
            machine_id: id.into(),
            label: id.into(),
            expected: ServerExpectation {
                server_id: "s".into(),
                epoch: "e".into(),
                sessions: 2,
                pid: None,
                session_ids: None,
            },
        }
    }

    fn outcome(id: &str, kind: OutcomeKind, old_stopped: bool, unconfirmed: bool) -> UpdateOutcome {
        UpdateOutcome {
            machine_id: id.into(),
            label: id.into(),
            kind,
            message: String::new(),
            old_stopped,
            unconfirmed,
            restored: None,
        }
    }

    #[test]
    fn agr19_remote_machines_go_first_this_mac_after_and_the_callers_machine_last() {
        let ids = |order: Vec<Planned>| order.into_iter().map(|p| p.machine_id).collect::<Vec<_>>();
        let items = || {
            vec![
                planned("local"),
                planned("box"),
                planned("gpu"),
                planned("lab"),
            ]
        };
        assert_eq!(
            ids(update_order(items(), None)),
            ["box", "gpu", "lab", "local"]
        );
        assert_eq!(
            ids(update_order(items(), Some("gpu"))),
            ["box", "lab", "local", "gpu"],
            "the caller's own machine last of all"
        );
        assert_eq!(
            ids(update_order(items(), Some("local"))),
            ["box", "gpu", "lab", "local"]
        );
    }

    #[test]
    fn agr19_this_mac_waits_after_a_remote_machine_was_left_without_a_server() {
        let mac = planned("local");
        assert_eq!(
            held_back(&mac, &[outcome("box", OutcomeKind::Updated, false, false)]),
            None
        );
        assert_eq!(
            held_back(&mac, &[outcome("box", OutcomeKind::Failed, false, false)]),
            None
        );
        assert!(
            held_back(&mac, &[outcome("box", OutcomeKind::Failed, true, false)])
                .unwrap()
                .contains("box may be left without a terminal server")
        );
        assert!(held_back(&mac, &[outcome("box", OutcomeKind::Failed, false, true)]).is_some());
        // A remote machine is never held back.
        assert_eq!(
            held_back(
                &planned("gpu"),
                &[outcome("box", OutcomeKind::Failed, true, false)]
            ),
            None
        );
    }

    #[test]
    fn agr19_a_remote_server_is_updated_only_as_it_was_reviewed() {
        let ids = |list: &[&str]| list.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        let expected = ServerExpectation {
            session_ids: Some(ids(&["a", "b"])),
            ..planned("box").expected
        };
        assert_eq!(
            remote_changed(&expected, "s", "e", Some(&ids(&["a", "b"]))),
            None
        );
        assert_eq!(
            remote_changed(&expected, "s", "e", Some(&ids(&["a"]))),
            None,
            "one ended: fine"
        );
        // One ended and a new one started: same count, but never ended unreviewed.
        assert!(
            remote_changed(&expected, "s", "e", Some(&ids(&["a", "new"])))
                .unwrap()
                .contains("started there since it was reviewed")
        );
        assert!(remote_changed(&expected, "other", "e", Some(&[]))
            .unwrap()
            .contains("changed"));
        assert!(remote_changed(&expected, "s", "restarted", Some(&[]))
            .unwrap()
            .contains("changed"));
        assert!(remote_changed(&expected, "s", "e", None)
            .unwrap()
            .contains("could not be read"));
        // Never bound to a count alone: refused even where the count fits.
        let counted = planned("box").expected;
        assert!(remote_changed(&counted, "s", "e", Some(&ids(&["x"])))
            .unwrap()
            .contains("not reviewed with its sessions"));
    }

    #[test]
    fn agr19_the_spec_shape_of_expected_needs_no_count() {
        let expected: ServerExpectation = serde_json::from_value(
            serde_json::json!({"serverId": "s", "epoch": "e", "sessionIds": ["a", "b", "c"]}),
        )
        .unwrap();
        assert_eq!(expected.confirmed_count(), 3);
    }

    #[test]
    fn agr19_a_replacement_holding_more_than_confirmed_is_not_restarted() {
        let expected = planned("box").expected;
        assert_eq!(prepared_over(&expected, None), None);
        assert_eq!(prepared_over(&expected, Some(2)), None);
        assert!(prepared_over(&expected, Some(3))
            .unwrap()
            .contains("3 sessions run there now"));
    }

    #[test]
    fn agr19_each_update_answer_reads_as_the_run_reports_it() {
        use super::super::reinstall::ReinstallResult;
        use crate::commands::host_connection_manager::{ReplacementFailed, ReplacementResult};
        let item = planned("box");
        let reinstalled = reinstall_outcome(
            &item,
            Ok(ReinstallResult::Reinstalled {
                sessions: 2,
                restored: None,
            }),
        );
        assert_eq!(
            (reinstalled.kind, reinstalled.message.as_str()),
            (OutcomeKind::Updated, "2 terminals closed")
        );
        let changed =
            reinstall_outcome(&item, Ok(ReinstallResult::SessionsChanged { sessions: 4 }));
        assert_eq!(changed.kind, OutcomeKind::Skipped);
        let stranded = reinstall_outcome(
            &item,
            Ok(ReinstallResult::Failed {
                old_stopped: true,
                repair: "disk full".into(),
            }),
        );
        assert!(stranded.strands());
        let refused = reinstall_outcome(&item, Err("an update is already running".into()));
        assert_eq!(refused.kind, OutcomeKind::Failed);
        assert!(!refused.strands(), "a refusal stopped nothing");
        let replaced = replace_outcome(
            &item,
            Ok(ReplacementResult::Replaced {
                count: Some(1),
                restored: None,
            }),
        );
        assert_eq!(replaced.kind, OutcomeKind::Updated);
        let failed = replace_outcome(
            &item,
            Ok(ReplacementResult::Failed(ReplacementFailed {
                old_stopped: false,
                count: None,
                repair: "x".into(),
            })),
        );
        assert!(!failed.strands());
        let lost = replace_outcome(&item, Err("connection lost".into()));
        assert!(
            lost.unconfirmed && lost.strands(),
            "an unknown result may have stranded it"
        );
    }

    #[test]
    fn agr19_a_run_that_stopped_unexpectedly_ends_and_frees_the_next() {
        let run = UpdateRun {
            op_id: "panicked".into(),
            running: true,
            machines: vec!["box".into(), "local".into()],
            current: Some("box".into()),
            outcomes: vec![],
        };
        let ended = stopped_run(
            run,
            &HashMap::from([("box".to_string(), "Build box".to_string())]),
        );
        assert!(!ended.running);
        assert_eq!(ended.outcomes.len(), 1);
        assert!(ended.outcomes[0].unconfirmed);
        assert_eq!(ended.outcomes[0].label, "Build box");
        remember(&ended);
        assert_ne!(
            lock().current.as_deref(),
            Some("panicked"),
            "a finished run is never the current one"
        );
        assert_eq!(run_status("panicked"), Some(ended));
        // Only the last few finished runs are kept.
        for n in 0..(KEPT_RUNS + 2) {
            remember(&UpdateRun {
                op_id: format!("old-{n}"),
                running: false,
                machines: vec![],
                current: None,
                outcomes: vec![],
            });
        }
        assert!(run_status("panicked").is_none());
        assert!(run_status(&format!("old-{}", KEPT_RUNS + 1)).is_some());
    }

    #[test]
    fn agr19_a_run_ends_with_one_message_for_its_caller() {
        let run = UpdateRun {
            op_id: "op1".into(),
            running: false,
            machines: vec!["box".into(), "local".into()],
            current: None,
            outcomes: vec![
                UpdateOutcome {
                    message: "box: 2 tabs reopened.".into(),
                    ..outcome("box", OutcomeKind::Updated, false, false)
                },
                UpdateOutcome {
                    message: "already up to date".into(),
                    ..outcome("This Mac", OutcomeKind::Skipped, false, false)
                },
            ],
        };
        assert_eq!(
            run_summary(&run),
            "Terminal server update op1: 1 updated, 1 skipped.\n- box updated: 2 tabs reopened.\n- This Mac skipped: already up to date"
        );
    }

    #[test]
    fn agr19_an_update_call_names_machines_or_all_out_of_date_ones() {
        let known = vec![
            ("local".to_string(), true),
            ("box".to_string(), true),
            ("fresh".to_string(), false),
        ];
        let ids = |list: &[&str]| list.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        assert_eq!(
            select(&known, None, true).unwrap(),
            (ids(&["local", "box"]), vec![])
        );
        assert_eq!(
            select(&known, Some(&ids(&["box", "fresh", "box"])), false).unwrap(),
            (
                ids(&["box"]),
                vec![(
                    "fresh".to_string(),
                    "its terminal server is not out of date".to_string()
                )]
            )
        );
        assert!(select(&known, Some(&ids(&["nope"])), false)
            .unwrap_err()
            .contains("unknown machine"));
        assert!(select(&known, None, false).is_err());
        assert!(select(&known, Some(&ids(&[])), false).is_err());
        assert!(select(&known, Some(&ids(&["box"])), true).is_err());
    }

    #[test]
    fn agr19_an_update_is_refused_once_the_server_is_not_as_reviewed() {
        let reviewed = ServerExpectation {
            server_id: "s".into(),
            epoch: "e".into(),
            sessions: 2,
            pid: Some(7),
            session_ids: Some(vec!["a".into(), "b".into()]),
        };
        assert!(still_as_reviewed(&reviewed, &reviewed));
        let mut now = reviewed.clone();
        now.epoch = "restarted".into();
        assert!(!still_as_reviewed(&reviewed, &now));
        let mut now = reviewed.clone();
        now.session_ids = Some(vec!["a".into(), "c".into()]);
        assert!(
            !still_as_reviewed(&reviewed, &now),
            "This Mac's exact sessions"
        );
        let mut now = reviewed.clone();
        now.pid = Some(8);
        assert!(!still_as_reviewed(&reviewed, &now));
        // A count alone never binds an update (an agent must pass the ids
        // it was listed with).
        let counted = ServerExpectation {
            pid: None,
            session_ids: None,
            ..reviewed.clone()
        };
        let mut fewer = counted.clone();
        fewer.sessions = 1;
        assert!(!still_as_reviewed(&counted, &fewer));
        // A remote machine listed with its session ids: any it runs that was
        // not listed refuses the call, even at the same count.
        let listed = ServerExpectation {
            pid: None,
            session_ids: Some(vec!["a".into(), "b".into()]),
            ..reviewed.clone()
        };
        let mut swapped = listed.clone();
        swapped.session_ids = Some(vec!["a".into(), "new".into()]);
        assert!(!still_as_reviewed(&listed, &swapped));
        let mut ended = listed.clone();
        ended.session_ids = Some(vec!["a".into()]);
        assert!(still_as_reviewed(&listed, &ended));
    }

    #[test]
    fn agr19_the_review_lists_live_tabs_and_marks_the_callers() {
        use codesense_pty_core::domain::{PtySessionId, WorkspaceKey};
        use codesense_pty_core::layout::{
            EntryState, LayoutSnapshot, Placement, TabEntry, WorkspaceDescriptor, WorkspaceLayout,
        };
        let tab = |session: &str, name: &str, tags: &[(&str, &str)], state: EntryState| TabEntry {
            session: PtySessionId::new(session),
            name: name.into(),
            role: None,
            placement: Placement::Tab,
            state,
            tags: tags
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        };
        let mut snapshot: LayoutSnapshot = serde_json::from_value(serde_json::json!({
            "schema": 1, "serverId": "s", "serverName": "box", "epoch": "e", "rev": 1,
            "workspaces": {}, "viewer": {}
        }))
        .unwrap();
        snapshot.workspaces.insert(
            WorkspaceKey::new("acme:default|SW-1"),
            WorkspaceLayout {
                descriptor: WorkspaceDescriptor::default(),
                tabs: vec![
                    tab("s1", "shell", &[], EntryState::Live),
                    tab(
                        "s2",
                        "Barb",
                        &[("remoteAgentKey", "team/barb")],
                        EntryState::Live,
                    ),
                    tab(
                        "s3",
                        "done",
                        &[],
                        EntryState::Exited {
                            exit_code: Some(0),
                            signal: None,
                        },
                    ),
                ],
                ..Default::default()
            },
        );
        let tabs = server_tabs(&snapshot, Some("s2"));
        assert_eq!(
            tabs,
            vec![
                ServerTab {
                    session: "s1".into(),
                    workspace: "SW-1".into(),
                    name: "shell".into(),
                    agent: None,
                    is_caller: false,
                },
                ServerTab {
                    session: "s2".into(),
                    workspace: "SW-1".into(),
                    name: "Barb".into(),
                    agent: Some("team/barb".into()),
                    is_caller: true,
                },
            ]
        );
    }

    #[test]
    fn agr19_sessions_no_listed_tab_shows_are_named_too() {
        let tab = |session: &str| ServerTab {
            session: session.into(),
            workspace: "w".into(),
            name: session.into(),
            agent: None,
            is_caller: false,
        };
        let expected = ServerExpectation {
            session_ids: Some(vec!["a".into(), "bg".into(), "c".into()]),
            ..planned("box").expected
        };
        assert_eq!(
            untabbed_sessions(Some(&expected), &[tab("a"), tab("c")]),
            vec!["bg".to_string()]
        );
        assert!(untabbed_sessions(None, &[]).is_empty());
    }

    #[test]
    fn agr19_a_single_update_waits_while_update_all_runs_and_an_unconfirmed_call_is_refused() {
        assert!(require_confirmation(false)
            .unwrap_err()
            .contains("confirmEndSessions"));
        assert!(require_confirmation(true).is_ok());
        let running = UpdateRun {
            op_id: "single-guard".into(),
            running: true,
            machines: vec!["box".into()],
            current: Some("box".into()),
            outcomes: vec![],
        };
        lock().current = Some(running.op_id.clone());
        remember(&running);
        assert!(refuse_while_running().unwrap_err().contains("Update all"));
        remember(&UpdateRun {
            running: false,
            current: None,
            ..running
        });
        assert!(refuse_while_running().is_ok());
    }

    #[test]
    fn agr19_a_refused_token_stopped_nothing() {
        let item = planned("box");
        let refused = replace_outcome(&item, Err("replacement token expired".into()));
        assert_eq!(refused.kind, OutcomeKind::Failed);
        assert!(!refused.strands(), "This Mac is not held back by it");
    }

    #[test]
    fn agr19_an_expectation_round_trips_as_an_agent_passes_it() {
        let wire = serde_json::json!({"serverId": "s", "epoch": "e", "sessions": 1, "pid": 7, "sessionIds": ["a"]});
        let expected: ServerExpectation = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            expected.session_ids.as_deref(),
            Some(&["a".to_string()][..])
        );
        assert_eq!(serde_json::to_value(&expected).unwrap(), wire);
        let remote: ServerExpectation = serde_json::from_value(
            serde_json::json!({"serverId": "s", "epoch": "e", "sessions": 0}),
        )
        .unwrap();
        assert_eq!(remote.pid, None);
    }
}
