// SPDX-License-Identifier: MIT
//! The app's [`ArmEnv`] (SW-AGR-8): machines come from the connection
//! manager, a routine's workspace on a machine from the viewer's record of
//! it (as a remote routine launch finds it, SW-AGR-2b), and its agent is
//! prepared there with the SW-XS-7 engine, as a remote launch prepares it.
//!
//! A sync with every machine a routine is armed on (or handing over from)
//! runs on a thread of its own every 30 s, and when a machine (re)connects.
//! It never prepares (SW-XS-25): only [`refresh`], which explicit actions
//! call, does. Sync discovery and imports serialize with ownership changes.
//! Refreshes serialize their workspace writes. Publication rechecks its inputs
//! under the sync guard, then arms and saves its receipt together.

use std::sync::Arc;

use tauri::{AppHandle, Manager};

use crate::commands::host_connection_manager::{
    ConnectedBackendError, HostConnectionManager, HostConnectionState,
};
use crate::domain::Principal;

use super::discover::DiscoveredRoutine;
use super::machine_arm::{
    machines_to_sync, prepare_refresh, publish_refresh, sync_machine, AgentFacts, ArmEnv,
    ArmLookup, ArmPlace, ArmPrepare, SyncInput,
};
use super::state::RoutineStateStore;

pub(crate) struct AppArmEnv {
    app: AppHandle,
    principal: Principal,
    home: std::path::PathBuf,
    /// SW-AGR-12: each routine's place on each machine, chosen once per sync
    /// pass (choosing asks the machine to confirm each candidate).
    places: std::sync::Mutex<std::collections::HashMap<(String, String), Result<ArmPlace, String>>>,
}

/// Lock order: REFRESHING first, then SYNCING; held for the entire refresh.
pub(crate) static REFRESHING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Sync discovery through cursor save, refresh publication, and ownership
/// commit and rewind share one ordering. Slow preparation stays outside.
/// Lock order: REFRESHING before SYNCING; sync and placement never take REFRESHING.
pub(crate) static SYNCING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Sync with every machine that needs it, off the caller's thread. A sync
/// already running makes this a no-op: its next pass picks up the change.
pub(crate) fn sync_in_background(app: &AppHandle) {
    let app = app.clone();
    let _ = std::thread::Builder::new()
        .name("routine-arm-sync".into())
        .spawn(move || {
            let _one = match SYNCING.try_lock() {
                Ok(one) => one,
                Err(std::sync::TryLockError::Poisoned(one)) => one.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => return,
            };
            if let Err(error) = sync_all(&app) {
                log::warn!(target: "routines", "machine routine sync: {error}");
            }
        });
}

/// How long an explicit action waits for its refresh (SW-XS-25). A refresh
/// still running then finishes on its own; the arm stays marked stale until
/// it does.
const REFRESH_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

/// Why an arm is stale while a refresh outlives [`REFRESH_WAIT`].
pub(crate) const STALE_STILL_PREPARING: &str =
    "still preparing on the machine (it took longer than 20 s)";

/// SW-XS-25: prepare routine `key`'s agent on the machine it is armed on and
/// push its arm with the launch recipe, as `principal`. Blocking and
/// unbounded: explicit callers go through [`refresh_bounded`].
fn refresh(
    app: &AppHandle,
    home: &std::path::Path,
    principal: &Principal,
    key: &str,
) -> Result<String, String> {
    let _refresh = REFRESHING.lock().unwrap_or_else(|e| e.into_inner());
    let prepared = with_input(app, home, principal, |env, input| {
        if let Some(machine) = input
            .routines
            .iter()
            .find(|r| r.key.to_string() == key)
            .and_then(|r| r.def.armed_machine())
        {
            // Bound first, so the arm is stamped with this desktop.
            bind_routine_agents(app, machine);
        }
        prepare_refresh(env, env, input, key)
    })??;
    let _one = SYNCING.lock().unwrap_or_else(|e| e.into_inner());
    with_input(app, home, principal, |env, input| {
        publish_refresh(env, input, prepared)
    })?
}

/// [`refresh`] of `key` (armed on `machine`), waiting at most
/// [`REFRESH_WAIT`]. Past it, the arm is marked stale ("still preparing"),
/// this returns, and the refresh finishes on its own thread: the person's
/// explicit action, completing. `Ok` names the machine.
pub(crate) fn refresh_bounded(
    app: &AppHandle,
    home: &std::path::Path,
    principal: &Principal,
    key: &str,
    machine: &str,
) -> Result<String, String> {
    refresh_within(REFRESH_WAIT, home, key, machine, {
        let (app, home, principal, key) = (
            app.clone(),
            home.to_path_buf(),
            principal.clone(),
            key.to_string(),
        );
        move || refresh(&app, &home, &principal, &key)
    })
}

/// Run `work` (a refresh of `key` on `machine`) on its own thread, waiting
/// at most `wait`. Past it, the arm is marked stale with
/// [`STALE_STILL_PREPARING`]; a refresh that then succeeds clears it.
pub(crate) fn refresh_within(
    wait: std::time::Duration,
    home: &std::path::Path,
    key: &str,
    machine: &str,
    work: impl FnOnce() -> Result<String, String> + Send + 'static,
) -> Result<String, String> {
    #[derive(Default)]
    struct Outcome {
        done: Option<Result<String, String>>,
        /// The `since_ms` of the "still preparing" mark the wait left.
        marked_at: Option<i64>,
    }
    let shared = Arc::new((
        std::sync::Mutex::new(Outcome::default()),
        std::sync::Condvar::new(),
    ));
    let worker = shared.clone();
    let (home_w, key_w, machine_w) = (home.to_path_buf(), key.to_string(), machine.to_string());
    std::thread::Builder::new()
        .name("routine-arm-refresh".into())
        .spawn(move || {
            let done = work();
            let (lock, ready) = &*worker;
            let mut outcome = lock.lock().unwrap_or_else(|e| e.into_inner());
            // Marked "still preparing" meanwhile: a refresh that got there
            // clears that mark, and only that one (a later mark stays).
            if let (Some(since), true) = (outcome.marked_at, done.is_ok()) {
                if let Ok(store) = RoutineStateStore::open(&home_w) {
                    let _ = super::machine_arm::clear_stale_if(
                        &store,
                        &key_w,
                        &machine_w,
                        STALE_STILL_PREPARING,
                        since,
                    );
                }
            }
            outcome.done = Some(done);
            ready.notify_all();
        })
        .map_err(|e| format!("the refresh could not start: {e}"))?;
    let (lock, ready) = &*shared;
    let outcome = lock.lock().unwrap_or_else(|e| e.into_inner());
    let (mut outcome, _) = ready
        .wait_timeout_while(outcome, wait, |o| o.done.is_none())
        .unwrap_or_else(|e| e.into_inner());
    if let Some(done) = outcome.done.take() {
        return done;
    }
    if let Ok(store) = RoutineStateStore::open(home) {
        let marked = super::machine_arm::mark_stale(
            &store,
            key,
            machine,
            STALE_STILL_PREPARING,
            chrono::Utc::now().timestamp_millis(),
        );
        // The mark as it stands (an earlier "still preparing" keeps its
        // since): the token the refresh clears by.
        outcome.marked_at = marked
            .ok()
            .and_then(|_| super::machine_arm::stale_arms(&store).ok())
            .and_then(|stale| stale.get(key).cloned())
            .filter(|s| s.machine == machine && s.why == STALE_STILL_PREPARING)
            .map(|s| s.since_ms);
    }
    Err(format!(
        "{STALE_STILL_PREPARING}; it finishes in the background"
    ))
}

/// The machine `key` is armed on, when this Mac manages that arm.
pub(crate) fn managed_machine(
    home: &std::path::Path,
    principal: &Principal,
    key: &str,
) -> Option<String> {
    let parsed = super::model::RoutineKey::parse(key)?;
    let current = super::service::get_current(home, principal, &parsed).ok()?;
    let host_id = super::host::local_host_id(home).ok()?;
    let machine = current.def.armed_machine()?.to_string();
    super::host::manages_machine_arm(&current.def, &host_id).then_some(machine)
}

/// SW-XS-25: "Refresh on machine", as `principal`. `Ok` names the machine.
pub(crate) fn refresh_explicit(
    app: &AppHandle,
    home: &std::path::Path,
    principal: &Principal,
    key: &str,
) -> Result<String, String> {
    let machine = managed_machine(home, principal, key)
        .ok_or_else(|| format!("routine {key} is not armed on a machine this Mac manages"))?;
    refresh_bounded(app, home, principal, key, &machine)
}

/// SW-XS-25: after an explicit arm, edit, resume, restore or handover
/// release of `key`, prepare it on its machine when it is armed on one this
/// Mac manages. The change stands either way: a refresh that cannot run (or
/// is still running after 20 s) leaves the arm marked `content_stale`, and
/// this returns what to tell the person. Blocking.
pub(crate) fn refresh_after_change(
    app: &AppHandle,
    home: &std::path::Path,
    principal: &Principal,
    key: &str,
) -> Option<String> {
    let machine = managed_machine(home, principal, key)?;
    match refresh_bounded(app, home, principal, key, &machine) {
        Ok(_) => {
            log::info!(target: "routines", "routine {key}: prepared and armed on {machine}");
            None
        }
        Err(why) => {
            log::warn!(target: "routines", "routine {key}: not prepared on {machine}: {why}");
            Some(not_prepared(&machine, &why))
        }
    }
}

/// What a person is told when a change of a machine-armed routine was saved
/// but not prepared on its machine.
pub(crate) fn not_prepared(machine: &str, why: &str) -> String {
    format!("Saved, but not prepared on {machine}: {why}. It fires there with what was prepared before; use Refresh on machine.")
}

/// How often the sync runs while the app runs: arms pushed after an edit, a
/// machine's new runs recorded, handovers settled. Each pass rescans the
/// routines on disk, so it runs at twice the scheduler's interval; a machine
/// that (re)connects is synced at once.
const SYNC_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// Sync whenever a machine (re)connects, and every scheduler interval. Call
/// once the connection manager is managed.
pub(crate) fn install(app: &AppHandle) {
    let periodic = app.clone();
    let spawned = std::thread::Builder::new()
        .name("routine-arm-sync-timer".into())
        .spawn(move || loop {
            std::thread::sleep(SYNC_INTERVAL);
            sync_in_background(&periodic);
        });
    if let Err(error) = spawned {
        log::warn!(target: "routines", "machine routine sync timer not started: {error}");
    }
    if let Some(manager) = app.try_state::<HostConnectionManager>() {
        let observed = app.clone();
        manager.subscribe(Arc::new(move |event| {
            if matches!(event.state, HostConnectionState::Connected { .. }) {
                // SW-AGR-11: every machine this app connects to hands back
                // the CodeSense MCP of its agents that lost this app, armed
                // routines or not.
                // Bound first, on the same thread, so the arms the sync sends
                // are stamped with this desktop's identity, not its viewer.
                let (app, machine) = (observed.clone(), event.machine_id.clone());
                let _ = std::thread::Builder::new()
                    .name("routine-agent-bind".into())
                    .spawn(move || {
                        bind_routine_agents(&app, &machine);
                        sync_in_background(&app);
                    });
            }
        }));
    }
}

fn sync_all(app: &AppHandle) -> Result<(), String> {
    let (home, user) =
        tauri::async_runtime::block_on(crate::commands::route_to_context::resolve_ambient(app))?;
    let principal = Principal::from(user);
    with_input(app, &home, &principal, |env, input| {
        let handovers = super::machine_arm::handovers(input.store).map_err(|e| e.to_string())?;
        for machine in machines_to_sync(input.routines, &handovers) {
            bind_routine_agents(app, &machine);
            let report = sync_machine(env, input, &machine);
            if !report.errors.is_empty() || !report.unprepared.is_empty() {
                log::warn!(
                    target: "routines",
                    "machine {machine}: routine sync errors {:?}, unprepared {:?}",
                    report.errors,
                    report.unprepared
                );
            }
            if !report.content_stale.is_empty() {
                log::info!(
                    target: "routines",
                    "machine {machine}: routines {:?} changed since they were prepared there; Refresh on machine prepares them",
                    report.content_stale
                );
            }
            if !report.waiting.is_empty() {
                log::warn!(
                    target: "routines",
                    "machine {machine} is not in the roster; routines {:?} wait for it to confirm it stopped them (release them if it is gone)",
                    report.waiting
                );
            }
            if !report.armed.is_empty() || !report.disarmed.is_empty() || report.recorded > 0 {
                log::info!(
                    target: "routines",
                    "machine {machine}: armed {:?}, disarmed {:?}, handed over {:?}, {} runs recorded",
                    report.armed,
                    report.disarmed,
                    report.handed_over,
                    report.recorded
                );
            }
        }
        Ok(())
    })?
}

/// Run `body` with this app's [`ArmEnv`] and a [`SyncInput`] over the
/// routines on disk now.
fn with_input<R>(
    app: &AppHandle,
    home: &std::path::Path,
    principal: &Principal,
    body: impl FnOnce(&AppArmEnv, &SyncInput<'_>) -> R,
) -> Result<R, String> {
    let (home, principal) = (home.to_path_buf(), principal.clone());
    let store = RoutineStateStore::open(&home).map_err(|e| e.to_string())?;
    let host_id = super::host::local_host_id(&home).map_err(|e| e.to_string())?;
    let routines =
        super::discover::discover_routines(&home, &principal).map_err(|e| e.to_string())?;
    let env = AppArmEnv {
        app: app.clone(),
        principal: principal.clone(),
        home: home.clone(),
        places: Default::default(),
    };
    let mode_of = |routine: &DiscoveredRoutine| {
        super::service::effective_mode(&home, &principal, &routine.key, &routine.def)
    };
    let author_of = |routine: &DiscoveredRoutine| {
        super::defs::read_revision(&routine.dir, routine.def.revision)
            .ok()
            .flatten()
            .map(|record| record.author.name)
            .unwrap_or_default()
    };
    let undiscovered = |key: &str| {
        let key = super::model::RoutineKey::parse(key)?;
        let dir = super::service::routine_dir_of(&home, &principal, &key).ok()?;
        super::defs::read_def(&dir).ok().flatten()
    };
    let agent_of = |routine: &DiscoveredRoutine| agent_facts(&home, &principal, routine);
    let settled = |key: &str, machine: &str, epoch: u64| {
        let Some(key) = super::model::RoutineKey::parse(key) else {
            return;
        };
        if let Err(error) = super::service::settle_handover(&home, &principal, &key, machine, epoch)
        {
            log::warn!(target: "routines", "routine {key}: handover from {machine} not cleared in its file: {error}");
        }
    };
    let notify: std::sync::Arc<dyn super::wake::WakeNotify> =
        match app.try_state::<std::sync::Arc<super::wake_app::AppWakeEnv>>() {
            Some(env) => env.inner().clone(),
            None => std::sync::Arc::new(super::wake::NoNotify),
        };
    let input = SyncInput {
        store: &store,
        principal: &principal,
        host_id: &host_id,
        routines: &routines,
        mode_of: &mode_of,
        author_of: &author_of,
        undiscovered: &undiscovered,
        agent_of: &agent_of,
        settled: &settled,
        notify: notify.as_ref(),
        now: chrono::Utc::now().timestamp_millis(),
    };
    Ok(body(&env, &input))
}

impl ArmEnv for AppArmEnv {
    fn host(&self, machine: &str) -> ArmLookup {
        let Some(manager) = self.app.try_state::<HostConnectionManager>() else {
            return ArmLookup::Offline;
        };
        match manager.connected_backend(machine) {
            Ok(snapshot) => {
                let client = snapshot.client();
                if client.supports_routine_arm() {
                    ArmLookup::Ready(client)
                } else {
                    ArmLookup::Unsupported(format!(
                        "Update the terminal server on {machine} to run routines there without this Mac."
                    ))
                }
            }
            Err(ConnectedBackendError::UnknownMachine) => ArmLookup::Unknown,
            Err(_) => ArmLookup::Offline,
        }
    }

    fn place(&self, routine: &DiscoveredRoutine, machine: &str) -> Result<ArmPlace, String> {
        // SW-AGR-12: from the durable preparation receipts, never the viewer.
        let key = (routine.key.to_string(), machine.to_string());
        if let Some(known) = self
            .places
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            return known.clone();
        }
        let target = routine.def.target.resolve().map_err(|e| e.to_string())?;
        let place = tauri::async_runtime::block_on(super::wake_app::durable_place(
            &self.app,
            &routine.key.org,
            &routine.key.team,
            machine,
            &target,
        ))
        .map(|place| ArmPlace {
            workspace_id: place.workspace_id,
            handle: place.handle,
            cwd: place.cwd,
        })
        .map_err(|error| error.detail().to_string());
        self.places
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, place.clone());
        place
    }
}

/// SW-XS-25: only [`refresh`] hands this to the sync code.
impl ArmPrepare for AppArmEnv {
    fn prepare(
        &self,
        routine: &DiscoveredRoutine,
        machine: &str,
        place: &ArmPlace,
    ) -> Result<codesense_pty_core::routine_arm::ArmLaunch, String> {
        tauri::async_runtime::block_on(prepare_on(self, routine, machine, place))
    }
}

/// Prepare `routine`'s agent into `place` on `machine`: one manifest, one
/// diff, at most one batch, under the destination lock (SW-XS-7).
async fn prepare_on(
    env: &AppArmEnv,
    routine: &DiscoveredRoutine,
    machine: &str,
    place: &ArmPlace,
) -> Result<codesense_pty_core::routine_arm::ArmLaunch, String> {
    let app = &env.app;
    let agent_key = routine
        .agent_key
        .clone()
        .ok_or("the routine has no agent this Mac can see")?;
    let org = routine.key.org.clone();
    let team = routine.key.team.clone();
    let team_key = crate::commands::pty_layout::team_key(&org, Some(&team));
    let ctx = match crate::commands::route_to_context::derive_context_for(app, &org, &team).await? {
        crate::commands::route_to_context::ContextResolution::Ok(ctx) => ctx,
        _ => return Err(format!("{org}/{team} is not available on this Mac")),
    };
    let resolved = crate::mcp::remote_workspace_target::resolve_durable(
        app.clone(),
        ctx,
        machine,
        &place.handle,
    )
    .await?;
    if resolved.cwd != place.cwd || resolved.workspace_id != place.workspace_id {
        return Err("the machine's workspace changed since it was opened".into());
    }
    if !resolved.client.supports_agent_startup() {
        return Err("saved-Agent startup requires an updated remote terminal server".into());
    }
    let client = resolved.client.clone();
    let resolved = crate::mcp::remote_destination::from_resolved(resolved);
    let rctx = crate::commands::content_engine::derive_resolver_context(
        resolved.context.codesense_home.clone(),
        resolved.context.user_id,
        resolved.context.org_slug.clone(),
        resolved.context.team_name.clone(),
    )
    .map_err(|e| e.to_string())?;
    let crate::content_engine::destination::DestinationIdentity::Remote(remote) =
        resolved.io.identity().clone()
    else {
        return Err("remote destination required".into());
    };
    let coordinator = app
        .state::<Arc<crate::content_engine::sync::WaveCoordinator>>()
        .inner()
        .clone();
    let _lock = coordinator
        .acquire_remote_destination_lock(&rctx, &resolved.source_repo, &remote)
        .await;
    let home = env.home.clone();
    let principal = env.principal.clone();
    let handle = place.handle.clone();
    let source = resolved.source_repo.clone();
    let io = resolved.io.clone();
    let agent = crate::commands::await_blocking("routine_arm_prepare", move || {
        let agent = crate::agents::remote_launch::prepare(
            &home, &principal, &agent_key, org, team, handle,
        )?;
        // Before anything is written to the machine.
        client.require_agent_env(!agent.startup.env.is_empty())?;
        crate::agents::remote_launch::prepare_workspace(
            &rctx,
            &source,
            io.as_ref(),
            codesense_host_proto::agent_cli::NativeLaunch::Cli(agent.runtime),
            Some((&agent.manifest, agent.name.as_str())),
            &agent.workspace_files,
            &|_| {},
        )?;
        Ok::<_, String>(agent)
    })
    .await?;
    let (workspace, descriptor) =
        crate::commands::pty_layout::arm_tab_home(app, &team_key, &place.workspace_id, &place.cwd)?;
    Ok(super::machine_arm::launch_recipe(
        &agent,
        place,
        env.principal.as_str(),
        &team_key,
        workspace,
        Some(descriptor),
    ))
}

/// `routine`'s agent as its arm carries it: its manifest and whether it set
/// `allowWake`.
fn agent_facts(
    home: &std::path::Path,
    principal: &Principal,
    routine: &DiscoveredRoutine,
) -> Option<AgentFacts> {
    let key = routine.agent_key.as_ref()?;
    let agent = crate::agents::service::read_current(home, principal, key).ok()?;
    let captured = crate::agents::remote_launch::prepare(
        home,
        principal,
        key,
        routine.key.org.clone(),
        routine.key.team.clone(),
        String::new(),
    )
    .ok()?;
    Some(AgentFacts {
        manifest_hash: captured.manifest.manifest_hash,
        allow_wake: agent.allow_wake,
    })
}

/// The connected terminal server of `machine` that runs routines, or why
/// not.
fn arm_client(
    app: &AppHandle,
    machine: &str,
) -> Result<Arc<crate::terminal::session_server::SessionServerClient>, String> {
    let manager = app
        .try_state::<HostConnectionManager>()
        .ok_or("machines are not available")?;
    match manager.connected_backend(machine) {
        Ok(snapshot) => {
            let client = snapshot.client();
            if client.supports_routine_arm() {
                Ok(client)
            } else {
                Err(format!(
                    "Update the terminal server on {machine} to run routines there without this Mac."
                ))
            }
        }
        Err(ConnectedBackendError::UnknownMachine) => Err(format!(
            "machine_unknown: no machine {machine} in your machines"
        )),
        Err(_) => Err(format!("{machine} is not connected")),
    }
}

/// SW-AGR-8 (SHOULD 10): whether a routine can be armed on `machine`: it is
/// in the roster, and when connected its terminal server runs routines
/// without a desktop. An offline machine is accepted (its arm is pushed when
/// it connects); `Ok(Some(warning))` says so.
/// SW-AGR-13: why a routine with wake limits would fire nowhere on `machine`
/// (its terminal server is connected and too old to enforce them), if it
/// would. A machine not connected now is checked when it connects.
pub(crate) fn wake_limits_note(app: &AppHandle, machine: &str) -> Option<String> {
    let manager = app.try_state::<HostConnectionManager>()?;
    let snapshot = manager.connected_backend(machine).ok()?;
    (!snapshot.client().enforces_wake_limits()).then(|| wake_limits_unenforced(machine))
}

/// The warning for a limited routine armed where its limits can't be
/// enforced.
pub(crate) fn wake_limits_unenforced(machine: &str) -> String {
    format!(
        "{machine}'s terminal server is too old to enforce this routine's wake limits (max_wakes, max_wakes_per_day, min_gap), so it is not armed there and fires nowhere: update the terminal server on {machine}, or remove the limits"
    )
}

/// Whether `machine` can take an arm. `limited`: the routine is a wake
/// with wake limits, which a server before proto minor 13 would drop; it is
/// armed with a warning that it fires nowhere until that server is updated.
pub(crate) fn check_arm_target(
    app: &AppHandle,
    machine: &str,
    limited: bool,
) -> Result<Option<String>, String> {
    let manager = app
        .try_state::<HostConnectionManager>()
        .ok_or("machines are not available")?;
    match manager.connected_backend(machine) {
        Ok(snapshot) => {
            let client = snapshot.client();
            if !client.supports_routine_arm() {
                return Err(format!(
                    "terminal_server_outdated: update the terminal server on {machine} to run routines there without this Mac"
                ));
            }
            let list = client.routine_arms()?;
            match list.unsupported {
                Some(why) => Err(format!("machine_unsupported: {why}")),
                None if limited && !client.enforces_wake_limits() => {
                    Ok(Some(wake_limits_unenforced(machine)))
                }
                None => Ok(None),
            }
        }
        Err(ConnectedBackendError::UnknownMachine) => Err(format!(
            "machine_unknown: {machine} is not one of your machines (list_machines shows their ids)"
        )),
        Err(_) => Ok(Some(format!(
            "{machine} is not connected: the routine is armed there once it connects"
        ))),
    }
}

/// SW-AGR-8 (MUST 2): fire a machine-armed routine by hand, on its machine,
/// through its policy and the machine's single flight; the run is recorded
/// here as the machine reports it.
pub(crate) fn fire_now(
    app: &AppHandle,
    home: &std::path::Path,
    principal: &Principal,
    key: &super::model::RoutineKey,
    variables: std::collections::BTreeMap<String, String>,
) -> Result<super::admit::Admission, String> {
    use codesense_pty_core::routine_arm::{ArmFireAnswer, ArmRunStatus};
    let dir = super::service::routine_dir_of(home, principal, key).map_err(|e| e.to_string())?;
    let def = super::defs::read_def(&dir)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("routine {key} not found"))?;
    let machine = def
        .armed_machine()
        .ok_or_else(|| format!("routine {key} is not armed on a machine"))?
        .to_string();
    if let Some(name) = variables.keys().find(|n| !def.variables.contains_key(*n)) {
        return Err(format!("the routine declares no variable named {name:?}"));
    }
    let client = arm_client(app, &machine)?;
    let task = super::machine_arm::render_declared(&def.task, &def.variables, &variables);
    let run =
        match client.routine_arm_fire_now(&key.to_string(), def.arm_epoch.max(1), Some(task))? {
            ArmFireAnswer::Fired { run } => run,
            ArmFireAnswer::Refused { refusal } => {
                return Err(format!("{machine} did not fire it: {refusal}"));
            }
        };
    let store = RoutineStateStore::open(home).map_err(|e| e.to_string())?;
    super::machine_arm::record_run(
        &store,
        &machine,
        &run,
        chrono::Utc::now().timestamp_millis(),
    )
    .map_err(|e| e.to_string())?;
    use super::admit::Outcome;
    let outcome = match &run.status {
        ArmRunStatus::Coalesced { into } => Outcome::Coalesced {
            active_run_id: into.clone(),
        },
        ArmRunStatus::Skipped { why } => Outcome::Skipped {
            reason: why.clone(),
        },
        ArmRunStatus::Undelivered { reason, .. } => Outcome::Skipped {
            reason: format!(
                "undelivered:{}",
                crate::delivery::run_status::reason_wire(*reason)
            ),
        },
        _ => Outcome::Queued,
    };
    Ok(super::admit::Admission {
        run_id: run.run_id,
        outcome,
    })
}

/// SW-AGR-8 (SHOULD 2): an agent's `routine_done` (or, with no `run_id`, its
/// `signal_done`) that this app relayed, for a run `machine` fired into
/// `session`: finished on the machine (so its open run there ends too) and
/// recorded here. `Ok(None)` when the machine fired no such run there.
pub(crate) fn complete_on_machine(
    app: &AppHandle,
    home: &std::path::Path,
    machine: &str,
    session: &str,
    run_id: Option<&str>,
    outcome: &str,
    summary: &str,
) -> Result<Option<codesense_pty_core::routine_arm::ArmRun>, String> {
    use codesense_pty_core::routine_arm::ArmCompleteAnswer;
    let client = arm_client(app, machine)?;
    match client.routine_arm_complete(session, run_id, outcome, summary)? {
        ArmCompleteAnswer::Done { run } => {
            let store = RoutineStateStore::open(home).map_err(|e| e.to_string())?;
            super::machine_arm::record_run(
                &store,
                machine,
                &run,
                chrono::Utc::now().timestamp_millis(),
            )
            .map_err(|e| e.to_string())?;
            Ok(Some(*run))
        }
        ArmCompleteAnswer::Refused { .. } => Ok(None),
    }
}

/// SW-AGR-8 (MUST 4): what removing or unsharing `machine` would strand:
/// the routines armed on it, and the ones still waiting for it to confirm
/// it stopped them. `Err` names them and what to do first.
pub(crate) fn check_machine_removal(
    home: &std::path::Path,
    principal: &Principal,
    machine: &str,
) -> Result<(), String> {
    let routines =
        super::discover::discover_routines(home, principal).map_err(|e| e.to_string())?;
    let armed: Vec<String> = routines
        .iter()
        .filter(|r| r.def.armed_machine() == Some(machine))
        .map(|r| r.def.name.clone())
        .collect();
    let store = RoutineStateStore::open(home).map_err(|e| e.to_string())?;
    let waiting: Vec<String> = super::machine_arm::handovers(&store)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|h| h.machine == machine)
        .map(|h| h.routine_key)
        .chain(
            routines
                .iter()
                .filter(|r| {
                    r.def
                        .handover_from
                        .as_ref()
                        .is_some_and(|from| from.machine == machine)
                })
                .map(|r| r.key.to_string()),
        )
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if armed.is_empty() && waiting.is_empty() {
        return Ok(());
    }
    let mut why = Vec::new();
    if !armed.is_empty() {
        why.push(format!(
            "{} {} armed on it ({}): move {} to this Mac or another machine first (routine_arm_on), so it stops firing them",
            armed.len(),
            if armed.len() == 1 { "routine is" } else { "routines are" },
            armed.join(", "),
            if armed.len() == 1 { "it" } else { "them" },
        ));
    }
    if !waiting.is_empty() {
        why.push(format!(
            "{} still waiting for it to confirm it stopped firing them ({}): connect it so it can, or release them with routine_handover_release if the machine is gone",
            if waiting.len() == 1 { "a routine is" } else { "routines are" },
            waiting.join(", "),
        ));
    }
    Err(format!(
        "{machine} cannot be removed yet: {}.",
        why.join("; ")
    ))
}

/// SW-AGR-8 (SHOULD 1): accept the CodeSense MCP of the agents `machine`
/// starts for routines while this app is connected (their full tool set,
/// relayed here). Once per connection, where remote MCP is allowed.
pub(crate) fn bind_routine_agents(app: &AppHandle, machine: &str) {
    // One at a time: a connect and a sync binding the same connection at once
    // would send it two secrets.
    static BINDING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = BINDING.lock().unwrap_or_else(|e| e.into_inner());
    let Ok(client) = arm_client(app, machine) else {
        return;
    };
    if client.accepts_routine_bindings() {
        return;
    }
    if tauri::async_runtime::block_on(crate::commands::machines::authorize_remote_mcp_control(
        app, machine,
    ))
    .is_err()
    {
        return;
    }
    let Some(lease) = client.private_lease() else {
        return;
    };
    let secret = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let owner = lease.owner_id();
    let weak = Arc::downgrade(&client);
    let binder_app = app.clone();
    let machine_id = machine.to_string();
    let binder: crate::terminal::session_server::RoutineBinder = Arc::new(move |session: &str| {
        let client = weak.upgrade()?;
        let (app, machine, session) = (binder_app.clone(), machine_id.clone(), session.to_string());
        let serve: Arc<dyn Fn(String, tokio::io::DuplexStream) + Send + Sync> =
            Arc::new(move |_server: String, stream: tokio::io::DuplexStream| {
                let (app, machine, session, client) = (
                    app.clone(),
                    machine.clone(),
                    session.clone(),
                    client.clone(),
                );
                tauri::async_runtime::spawn(async move {
                    use crate::mcp::remote_authority::RemoteAuthority;
                    // SW-AGR-11: a tab this app launched there, handed back
                    // after a disconnect, keeps its launch's authority; a
                    // routine's agent its routine's; one that ended while
                    // this app was away delivers its queued calls only.
                    let live = {
                        let (client, session) = (client.clone(), session.clone());
                        crate::commands::await_blocking("reattached_session_live", move || {
                            Ok::<_, String>(session_live(&client, &session))
                        })
                        .await
                        .unwrap_or(false)
                    };
                    let launched = app
                        .try_state::<crate::commands::dashboard_launch::DashboardLaunchState>()
                        .and_then(|state| state.remote_shell_on_machine(&machine, &session));
                    let mut routine_key = None;
                    let routine = match launched {
                        Some(_) => None,
                        None => {
                            let ran = {
                                let (client, session) = (client.clone(), session.clone());
                                crate::commands::await_blocking("routine_agent_team", move || {
                                    routine_of(&client, &session, live)
                                })
                                .await
                                .ok()
                            };
                            // Only a routine of this app's own, for this
                            // person, armed on this machine.
                            match ran {
                                Some((team, tag, key)) => {
                                    let mine = {
                                        let (app, machine) = (app.clone(), machine.clone());
                                        own_routine(&app, owner, &key, &machine).await
                                    };
                                    if mine {
                                        routine_key = Some(key);
                                        Some((team, tag))
                                    } else {
                                        None
                                    }
                                }
                                None => None,
                            }
                        }
                    };
                    let Some(kind) = reattach_kind(
                        launched.as_ref().map(|record| record.owner_id),
                        owner,
                        live,
                        routine,
                    ) else {
                        return;
                    };
                    let authority = match (kind, launched) {
                        (Reattached::Shell, Some(record)) => {
                            RemoteAuthority::for_reattached_shell(app.clone(), record, &client)
                        }
                        (Reattached::ShellOutbox, Some(record)) => RemoteAuthority::for_outbox(
                            app.clone(),
                            owner,
                            machine,
                            record.team_key.clone(),
                            &client,
                            session,
                            Some(record.handle.clone()),
                        ),
                        (Reattached::Routine(team_key), _) => RemoteAuthority::for_routine(
                            app.clone(),
                            owner,
                            machine,
                            team_key,
                            &client,
                            session,
                            routine_key.expect("admitted routine has a trusted key"),
                        ),
                        (Reattached::RoutineOutbox(team_key), _) => RemoteAuthority::for_outbox(
                            app.clone(),
                            owner,
                            machine,
                            team_key,
                            &client,
                            session,
                            None,
                        ),
                        _ => return,
                    };
                    if authority.validate().await.is_err() {
                        return;
                    }
                    let handler = crate::mcp::handler::CodesenseHandler::new_remote(app, authority);
                    crate::mcp::serve_stream(handler, stream).await;
                });
            });
        Some(serve)
    });
    client.accept_routine_bindings(
        secret.clone(),
        binder,
        tauri::async_runtime::handle().inner().clone(),
    );
    // SW-AGR-11: this install, as this person: the agents it relayed come
    // back to it, not to another Mac of the same person.
    let desktop =
        tauri::async_runtime::block_on(crate::commands::route_to_context::resolve_ambient(app))
            .ok()
            .and_then(|(home, _)| super::host::local_host_id(&home).ok())
            .map(|host| desktop_identity(&host, owner));
    if desktop.is_none() {
        log::warn!(target: "routines", "machine {machine}: no desktop identity (host id unavailable); its agents are handed back by viewer only");
    }
    if let Err(error) = client.routine_arm_bind_secret(&secret, desktop.as_deref()) {
        log::warn!(target: "routines", "machine {machine}: routine agents keep machine-side MCP: {error}");
    }
}

/// SW-AGR-11: this desktop's identity to its machines: this install (its
/// host id) as `owner`, printable and short.
pub(crate) fn desktop_identity(host_id: &str, owner: u64) -> String {
    let clean: String = host_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(64)
        .collect();
    format!("{clean}.{owner}")
}

/// Whether `session` still runs on `client`'s machine.
fn session_live(
    client: &crate::terminal::session_server::SessionServerClient,
    session: &str,
) -> bool {
    use codesense_pty_core::domain::TerminalDomain;
    client
        .live_child_pids(&[codesense_pty_core::domain::PtySessionId::new(session)])
        .ok()
        .and_then(|pids| pids.first().copied().flatten())
        .is_some()
}

/// SW-AGR-11: what a session handed back to this app is served as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Reattached {
    /// A tab this app launched, still running: its launch's authority.
    Shell,
    /// A tab this app launched that ended: its queued calls only.
    ShellOutbox,
    /// A routine's agent, still running, for this team.
    Routine(String),
    /// A routine's agent that ended: its queued calls only.
    RoutineOutbox(String),
}

/// How a session handed back to this app (`owner`) is served: by the tab
/// this app launched there (its owner), else as a routine's agent (its
/// team, and the owner its tab names); `None`: refused.
pub(crate) fn reattach_kind(
    launched_owner: Option<u64>,
    owner: u64,
    live: bool,
    routine: Option<(String, Option<String>)>,
) -> Option<Reattached> {
    match (launched_owner, routine) {
        (Some(launched), _) if launched != owner => None,
        (Some(_), _) if live => Some(Reattached::Shell),
        (Some(_), _) => Some(Reattached::ShellOutbox),
        (None, Some((_, Some(tagged)))) if tagged != owner.to_string() => None,
        (None, Some((team, _))) if live => Some(Reattached::Routine(team)),
        (None, Some((team, _))) => Some(Reattached::RoutineOutbox(team)),
        (None, None) => None,
    }
}

/// SW-AGR-11: whether `key` is one of this app's own routines for `owner`
/// (the person signed in here), armed on `machine`.
async fn own_routine(app: &AppHandle, owner: u64, key: &str, machine: &str) -> bool {
    let Ok((home, user)) = crate::commands::route_to_context::resolve_ambient(app).await else {
        return false;
    };
    if user != owner {
        return false;
    }
    let (key, machine) = (key.to_string(), machine.to_string());
    crate::commands::await_blocking("own_routine", move || {
        let routines = super::discover::discover_routines(&home, &Principal::from(user))
            .map_err(|e| e.to_string())?;
        Ok::<_, String>(owns_routine(
            routines
                .iter()
                .map(|r| (r.key.to_string(), r.def.armed_machine.clone())),
            &key,
            &machine,
        ))
    })
    .await
    .unwrap_or(false)
}

/// Whether `key` is among `routines` (`(key, armed machine)`), armed on
/// `machine`.
pub(crate) fn owns_routine(
    mut routines: impl Iterator<Item = (String, Option<String>)>,
    key: &str,
    machine: &str,
) -> bool {
    routines.any(|(have, armed)| have == key && armed.as_deref() == Some(machine))
}

/// The routine `session` runs (or ran) for: its team (`org:team`), the owner
/// its tab names (while it runs), and its key, from the machine's run log.
fn routine_of(
    client: &crate::terminal::session_server::SessionServerClient,
    session: &str,
    live: bool,
) -> Result<(String, Option<String>, String), String> {
    use codesense_pty_core::domain::TerminalDomain;
    use codesense_pty_core::routine_arm::ArmRunStatus;
    let log = client.routine_arm_runs(0)?;
    let key = log
        .runs
        .iter()
        .rev()
        .find(|run| match &run.status {
            ArmRunStatus::Launched { session: s, .. }
            | ArmRunStatus::Delivered { session: s, .. }
            | ArmRunStatus::Queued { session: s, .. } => s.as_str() == session,
            _ => false,
        })
        .map(|run| run.key.clone())
        .ok_or_else(|| format!("session {session} ran no routine of this machine"))?;
    let parsed = crate::routines::model::RoutineKey::parse(&key)
        .ok_or_else(|| format!("unreadable routine key {key}"))?;
    if !live {
        return Ok((format!("{}:{}", parsed.org, parsed.team), None, key));
    }
    let snapshot = client
        .layout_snapshot(
            &crate::commands::pty_layout::app_viewer(),
            &codesense_pty_core::domain::ClientBootId::new("routine-binding"),
        )
        .map_err(|e| e.to_string())?;
    let tab = snapshot
        .workspaces
        .values()
        .flat_map(|ws| ws.tabs.iter())
        .find(|tab| tab.session.as_str() == session)
        .ok_or_else(|| format!("session {session} is not a routine's agent"))?;
    let team = tab
        .tags
        .get(codesense_pty_core::routine_arm::TAG_ROUTINE_TEAM)
        .cloned()
        .ok_or_else(|| format!("session {session} is not a routine's agent"))?;
    Ok((team, tab.tags.get("remoteOwnerId").cloned(), key))
}

#[cfg(test)]
mod reattach_tests {
    use super::*;

    // SW-AGR-11 (review SHOULD, for_reattached_shell path): a session handed
    // back is served by the tab this app launched (its owner only), else as
    // a routine's agent (its tab's owner checked); ended ones deliver their
    // queued calls only.
    #[test]
    fn a_handed_back_session_is_served_by_who_launched_it() {
        let team = || Some(("acme:core".to_string(), None));
        assert_eq!(
            reattach_kind(Some(41), 41, true, None),
            Some(Reattached::Shell)
        );
        assert_eq!(
            reattach_kind(Some(41), 41, false, None),
            Some(Reattached::ShellOutbox)
        );
        assert_eq!(
            reattach_kind(Some(7), 41, true, team()),
            None,
            "another person's tab"
        );
        assert_eq!(
            reattach_kind(None, 41, true, team()),
            Some(Reattached::Routine("acme:core".into()))
        );
        assert_eq!(
            reattach_kind(None, 41, false, team()),
            Some(Reattached::RoutineOutbox("acme:core".into()))
        );
        assert_eq!(
            reattach_kind(
                None,
                41,
                true,
                Some(("acme:core".into(), Some("41".into())))
            ),
            Some(Reattached::Routine("acme:core".into()))
        );
        assert_eq!(
            reattach_kind(None, 41, true, Some(("acme:core".into(), Some("7".into())))),
            None,
            "a routine tab of another owner"
        );
        assert_eq!(
            reattach_kind(None, 41, true, None),
            None,
            "neither: refused"
        );
    }

    // r2 MUST 2: only this app's own routine, armed on this machine.
    #[test]
    fn only_an_own_routine_armed_here_is_served() {
        let routines = || {
            vec![
                ("acme/core/triage".to_string(), Some("m-1".to_string())),
                ("acme/core/digest".to_string(), None),
            ]
            .into_iter()
        };
        assert!(owns_routine(routines(), "acme/core/triage", "m-1"));
        assert!(
            !owns_routine(routines(), "acme/core/triage", "m-2"),
            "armed elsewhere"
        );
        assert!(
            !owns_routine(routines(), "acme/core/digest", "m-1"),
            "armed on no machine"
        );
        assert!(!owns_routine(routines(), "other/team/x", "m-1"), "not ours");
    }

    #[test]
    fn the_desktop_identity_is_per_install_and_person() {
        assert_eq!(desktop_identity("3f2a-77", 41), "3f2a-77.41");
        assert_ne!(
            desktop_identity("3f2a-77", 41),
            desktop_identity("3f2a-77", 42)
        );
        assert_eq!(desktop_identity("a b\n", 1), "ab.1");
    }
}
