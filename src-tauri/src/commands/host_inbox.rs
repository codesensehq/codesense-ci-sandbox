// SPDX-License-Identifier: MIT
//! Inbox notices on the desktop: an agent's notices go through the host its tab runs on,
//! this Mac's terminal server (or its in-process fallback) or a machine's, which hands
//! each one to the agent through its CLI's inbox port (`codesense_pty_core::inbox_host`).
//!
//! - **Mail stored** for a tab ([`mail_stored`]) reserves a notice in the tab's inbox and
//!   pushes it to the tab's host. While the host reports the tab's agent armed, each push
//!   is checked again once its reservation lapses: a confirmed one offers nothing, a lost
//!   one is pushed again under its own token.
//! - **The host reports each notice's fate**: told (the agent has it: only then is the
//!   mail marked told), released (it had no effect: offered again at the next look), or
//!   uncertain (held, never offered again blind, until it is settled told).
//! - **Waiter states** come from the hosts: every arm also checks the tab's inbox, since
//!   mail may have come while no waiter was armed. Each (re)connect of a host reads a full
//!   snapshot and offers every tab there its mail at once, and a dropped link forgets the
//!   machine's states, so its tabs read as unknown, not armed.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use codesense_pty_core::domain::{InboxWaiterState, PtySessionId, TerminalDomainEvent};
pub(crate) use codesense_pty_core::inbox_host::Fate;
use tauri::{AppHandle, Emitter, Manager};

use super::agent_inbox::{self, AppInboxHost, Check, InboxHost};
use super::dashboard_launch::DashboardLaunchState;
use super::host_connection_manager::{HostConnectionManager, HostConnectionState};
use super::machines::LOCAL_MACHINE_ID;

/// Retry delay for a failed handoff before the host accepts the notice (for example,
/// its session is not bound to this app yet, right after a connect).
const RETRY_PUSH: Duration = Duration::from_secs(5);
/// Bounded retries for a failed handoff; held/refused CLI submissions stay on the host.
const PUSH_ATTEMPTS: u32 = 3;
/// How many times a machine's waiters are read after it connects before giving up until
/// it connects again.
const SNAPSHOT_ATTEMPTS: u32 = 3;
/// How long an offer for a tab not bound to a handle yet waits for the launch restore
/// after an app restart to settle.
const RESTORE_WAIT: Duration = Duration::from_secs(120);

/// One tab's inbox waiter as its host last reported it.
#[derive(Debug, Clone, Copy)]
struct Waiter {
    waiting: bool,
    /// Until when (on this Mac's clock) its agent reads as one new mail wakes.
    until: Option<Instant>,
}

impl Waiter {
    fn of(state: &InboxWaiterState) -> Self {
        Self {
            waiting: state.waiting,
            until: state
                .grace_ms
                .and_then(|grace| Instant::now().checked_add(Duration::from_millis(grace))),
        }
    }

    fn armed(&self) -> bool {
        self.waiting || self.until.is_some_and(|until| until > Instant::now())
    }
}

/// One host's waiters, by session there.
type Waiters = HashMap<String, Waiter>;

/// What this app knows of its tabs' inbox waiters, by the machine whose host runs them
/// ([`LOCAL_MACHINE_ID`] for this Mac's).
#[derive(Debug)]
pub(crate) struct HostInboxes {
    /// Each connected host's waiters, by session there, and whether its snapshot was
    /// read. A machine absent here, or not read yet, reads as unknown; this Mac's hosts
    /// are always connected.
    machines: Mutex<HashMap<String, (u64, bool, Waiters)>>,
    /// The sessions with a check of their inbox already scheduled.
    scheduled: Mutex<HashSet<String>>,
    /// Where the hosts' reports go, from the terminal event sinks to the task [`install`]
    /// starts.
    events: Mutex<Option<tokio::sync::mpsc::UnboundedSender<Pushed>>>,
}

impl Default for HostInboxes {
    fn default() -> Self {
        // This Mac's hosts are there from the start: until their first read, none of its
        // tabs has a waiter.
        Self {
            machines: Mutex::new(HashMap::from([(
                LOCAL_MACHINE_ID.to_string(),
                (0, true, HashMap::new()),
            )])),
            scheduled: Mutex::default(),
            events: Mutex::default(),
        }
    }
}

/// An `InboxWaitV1` push from a host.
#[derive(Debug)]
struct Pushed {
    machine: String,
    event: TerminalDomainEvent,
}

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl HostInboxes {
    /// Whether new mail reaches the agent of `session` on `machine` (a waiter waits, or
    /// re-arms within its grace, or its host submits to it); `None` while the machine's
    /// waiters are unknown.
    pub(crate) fn armed(&self, machine: &str, session: &str) -> Option<bool> {
        let machines = locked(&self.machines);
        let (_, read, waiters) = machines.get(machine)?;
        read.then(|| waiters.get(session).is_some_and(Waiter::armed))
    }

    /// `machine` connected: each change pushed from now on is kept, and its waiters are
    /// known once its snapshot is read.
    fn connecting(&self, machine: &str) -> u64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let generation = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        locked(&self.machines).insert(machine.to_string(), (generation, false, HashMap::new()));
        generation
    }

    fn current(&self, machine: &str, generation: u64) -> bool {
        locked(&self.machines)
            .get(machine)
            .is_some_and(|(current, _, _)| *current == generation)
    }

    #[cfg(test)]
    fn snapshot(&self, machine: &str, states: Vec<InboxWaiterState>) {
        let generation = locked(&self.machines)
            .get(machine)
            .map(|(generation, _, _)| *generation);
        if let Some(generation) = generation {
            self.snapshot_for(machine, generation, states);
        }
    }

    /// `machine`'s snapshot: kept for the sessions no push reported meanwhile, and its
    /// waiters known from now on. Dropped if the machine disconnected while it was read.
    fn snapshot_for(&self, machine: &str, generation: u64, states: Vec<InboxWaiterState>) {
        if let Some((_, read, waiters)) = locked(&self.machines)
            .get_mut(machine)
            .filter(|(current, _, _)| *current == generation)
        {
            for state in states {
                waiters
                    .entry(state.session.as_str().to_string())
                    .or_insert_with(|| Waiter::of(&state));
            }
            *read = true;
        }
    }

    fn changed(&self, machine: &str, state: &InboxWaiterState) {
        if let Some((_, _, waiters)) = locked(&self.machines).get_mut(machine) {
            waiters.insert(state.session.as_str().to_string(), Waiter::of(state));
        }
    }

    fn disconnected(&self, machine: &str) {
        locked(&self.machines).remove(machine);
    }
}

/// Follow every host: this Mac's terminal server (re)connecting reads its waiters and
/// offers its tabs their mail, as does each machine's link; a machine's link dropping
/// forgets its waiters. The hosts' reports are taken from here on.
pub(crate) fn install(app: &AppHandle) {
    if let Some(inboxes) = host_inboxes(app) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        *locked(&inboxes.events) = Some(tx);
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            while let Some(Pushed { machine, event }) = rx.recv().await {
                on_report(&app, &machine, event).await;
            }
        });
    }
    if let Some(local) = app.try_state::<crate::terminal::session_server::SessionServerState>() {
        let app = app.clone();
        local.admission.subscribe(std::sync::Arc::new(
            move |row: super::machines::RuntimeAdmissionRow| {
                // Whether it came up or went away, this Mac's waiters are read afresh from
                // the hosts there are now.
                if row.member.slot == super::machines::RuntimeMemberSlot::Sidecar {
                    let app = app.clone();
                    schedule_local(&app);
                }
            },
        ));
    }
    {
        let app = app.clone();
        schedule_local(&app);
    }
    let Some(manager) = app.try_state::<HostConnectionManager>() else {
        return;
    };
    let app = app.clone();
    manager.subscribe(std::sync::Arc::new(move |event| {
        let machine = event.machine_id.clone();
        let Some(inboxes) = host_inboxes(&app) else {
            return;
        };
        if matches!(event.state, HostConnectionState::Connected { .. }) {
            let generation = inboxes.connecting(&machine);
            let app = app.clone();
            tauri::async_runtime::spawn(
                async move { reconnected(&app, &machine, generation).await },
            );
        } else {
            inboxes.disconnected(&machine);
        }
    }));
}

fn host_inboxes(app: &AppHandle) -> Option<std::sync::Arc<HostInboxes>> {
    app.try_state::<DashboardLaunchState>()
        .map(|launch| launch.host_inboxes())
}

/// This Mac's hosts: its terminal server (when it runs) and the in-process fallback.
pub(crate) fn local_hosts(
    app: &AppHandle,
) -> Vec<std::sync::Arc<dyn codesense_pty_core::domain::TerminalDomain>> {
    let mut hosts: Vec<std::sync::Arc<dyn codesense_pty_core::domain::TerminalDomain>> = Vec::new();
    if let Some(client) = app
        .try_state::<crate::terminal::session_server::SessionServerState>()
        .and_then(|state| crate::terminal::session_server::SessionServerState::connected(&state))
        .filter(|client| client.supports_inbox_wait())
    {
        hosts.push(client);
    }
    if let Some(host) = super::pty_backend::in_process_host_handle(app) {
        hosts.push(host);
    }
    hosts
}

fn schedule_local(app: &AppHandle) {
    let Some(inboxes) = host_inboxes(app) else {
        return;
    };
    let generation = inboxes.connecting(LOCAL_MACHINE_ID);
    let app = app.clone();
    tauri::async_runtime::spawn(async move { local_connected(&app, generation).await });
}

/// This Mac's terminal server (re)connected (or the app started): its waiters, read
/// afresh (a restarted server's are gone), and every live local tab offered its mail
/// (what a restarted server held is gone). Reports arrive independently of this read.
async fn local_connected(app: &AppHandle, generation: u64) {
    crate::session_usage::spend_caps::changed();
    crate::mcp::local_tools::refresh(app);
    let Some(inboxes) = host_inboxes(app) else {
        return;
    };
    let mut states = Vec::new();
    let mut read_all = true;
    for host in local_hosts(app) {
        match read_waiters(|| Some(host.clone()), "this Mac").await {
            Some(read) => states.extend(read),
            None => read_all = false,
        }
    }
    if !HostInboxes::current(&inboxes, LOCAL_MACHINE_ID, generation) {
        return;
    }
    if read_all {
        HostInboxes::snapshot_for(&inboxes, LOCAL_MACHINE_ID, generation, states);
    }
    let handle = app.clone();
    let sessions = super::await_blocking("inbox.reconnect.local", move || {
        super::mcp_scope::inventory::restore_live_relays(&handle);
        super::pty_backend::live_session_ids(&handle)
    })
    .await;
    if !HostInboxes::current(&inboxes, LOCAL_MACHINE_ID, generation) {
        return;
    }
    if let Ok(sessions) = sessions {
        for session in sessions {
            tab_restored(app, &session);
        }
    }
}

/// The inbox waiters the host `host()` names (of `machine`, as logs name it) reports,
/// asked again a few times (the host looked up afresh each time); `None` when it could
/// not be read. A host that is gone, or predates `InboxWaitV1`, hands no notices: its
/// tabs have none.
async fn read_waiters(
    host: impl Fn() -> Option<std::sync::Arc<dyn codesense_pty_core::domain::TerminalDomain>>,
    machine: &str,
) -> Option<Vec<InboxWaiterState>> {
    for attempt in 1..=SNAPSHOT_ATTEMPTS {
        let Some(host) = host() else {
            return Some(Vec::new());
        };
        let read = tauri::async_runtime::spawn_blocking(move || host.inbox_waiters()).await;
        match read
            .map_err(|e| e.to_string())
            .and_then(|r| r.map_err(|e| e.0))
        {
            Ok(states) => return Some(states),
            Err(e) => log::warn!(target: "inbox", "inbox waiters of {machine}: {e}"),
        }
        if attempt < SNAPSHOT_ATTEMPTS {
            tokio::time::sleep(RETRY_PUSH).await;
        }
    }
    None
}

/// `machine` connected: its waiters, then every remote tab there offered its mail at
/// once (what its host held for them did not survive a restart).
async fn reconnected(app: &AppHandle, machine: &str, generation: u64) {
    let client = || {
        client_of(app, machine)
            .map(|client| client as std::sync::Arc<dyn codesense_pty_core::domain::TerminalDomain>)
    };
    let states = read_waiters(client, machine).await;
    if let (Some(states), Some(inboxes)) = (states, host_inboxes(app)) {
        HostInboxes::snapshot_for(&inboxes, machine, generation, states);
    }
    if !host_inboxes(app).is_some_and(|inboxes| HostInboxes::current(&inboxes, machine, generation))
    {
        return;
    }
    // After an app restart its tabs come back from their receipts first, so
    // each is offered its mail below without its workspace being opened.
    crate::commands::pty_layout::restore_on_machine(app, machine).await;
    // A snapshot never read leaves the machine's tabs unknown, not unarmed.
    let Some(launch) = app.try_state::<DashboardLaunchState>() else {
        return;
    };
    for session in reconnect_offers(&launch, machine) {
        tab_restored(app, &session);
    }
}

/// The sessions of the remote tabs this app launched on `machine` whose notices go to
/// `machine`, as a push resolves them (a tab without CodeSense's hooks has nowhere to
/// hear them): each is offered its mail again when the machine reconnects.
pub(crate) fn reconnect_offers(launch: &DashboardLaunchState, machine: &str) -> Vec<String> {
    launch
        .remote_shells_on_machine(machine)
        .into_iter()
        .map(|record| record.target.session.as_str().to_string())
        .filter(|session| tab_machine_in(launch, session).as_deref() == Some(machine))
        .collect()
}

/// The connected client of `machine`, when its host hands inbox notices.
fn client_of(
    app: &AppHandle,
    machine: &str,
) -> Option<std::sync::Arc<crate::terminal::session_server::SessionServerClient>> {
    let manager = app.try_state::<HostConnectionManager>()?;
    let client = crate::commands::host_connection_manager::ConnectedBackendSnapshot::client(
        &manager.connected_backend(machine).ok()?,
    );
    client.supports_inbox_wait().then_some(client)
}

/// Where `session`'s notices go: the machine its tab runs on, when this app knows the tab
/// (a remote tab only when this app launched it with CodeSense's hooks).
pub(crate) fn require_mail_spend_holds(app: &AppHandle, session: &str) -> Result<(), String> {
    if tab_machine(app, session).as_deref() != Some(LOCAL_MACHINE_ID) {
        return Ok(());
    }
    if app
        .try_state::<DashboardLaunchState>()
        .is_some_and(|state| state.has_live_headless_session(session))
    {
        return Ok(()); // Headless inboxes live on the hold-capable in-process host.
    }
    let reference = super::machines::RuntimeSessionReference::Legacy(session.to_string());
    let backend = super::pty_backend::resolve_session_reference(app, &reference, false)?;
    if let Some(client) = backend.client() {
        client.require_spend_holds()?;
    }
    Ok(())
}

fn tab_machine(app: &AppHandle, session: &str) -> Option<String> {
    tab_machine_in(&*app.try_state::<DashboardLaunchState>()?, session)
}

/// [`tab_machine`] over the launch registry.
fn tab_machine_in(launch: &DashboardLaunchState, session: &str) -> Option<String> {
    match launch
        .remote_handle_for_session(session)
        .and_then(|handle| launch.remote_shell_by_handle(&handle))
    {
        Some(record) => {
            crate::mcp::remote_authority::hooks_accepted(&record).then(|| record.machine_id.clone())
        }
        None => Some(LOCAL_MACHINE_ID.to_string()),
    }
}

/// A tab was brought back, or its host reconnected: offered its mail at once, an
/// unconfirmed notice included (what its host held for it is gone).
pub(crate) fn tab_restored(app: &AppHandle, session: &str) {
    if tab_machine(app, session).is_none() {
        return;
    }
    let (app, session) = (app.clone(), session.to_string());
    tauri::async_runtime::spawn(async move { offer(&app, &session, Offer::Restored, 0).await });
}

/// Why a tab is offered its mail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Offer {
    /// Mail arrived, a waiter armed, or a reservation lapsed: a notice reserved and not
    /// yet confirmed is left to its reservation.
    New,
    /// The tab was brought back or its host reconnected: its host lost what it held, so
    /// a notice reserved but never confirmed is pushed again.
    Restored,
}

/// Where the notices of the tab under `handle` go: the machine whose host runs it and its
/// session there. A remote tab runs under its session on its machine (`None` when its
/// agent has no CodeSense hooks: nothing there hears its notices); a local tab under the
/// session its handle is bound to, or (opened by hand) the one it is addressed by.
pub(crate) fn inbox_target(
    launch: &DashboardLaunchState,
    handle: &str,
) -> Option<(String, String)> {
    match launch.remote_shell_by_handle(handle) {
        Some(record) => crate::mcp::remote_authority::hooks_accepted(&record).then(|| {
            (
                record.machine_id.clone(),
                record.target.session.as_str().to_string(),
            )
        }),
        None => Some((
            LOCAL_MACHINE_ID.to_string(),
            launch.resolve(handle).unwrap_or_else(|| handle.to_string()),
        )),
    }
}

/// Mail was stored for `handle`: its notice goes to the host its tab runs on, unless
/// that host reports its agent has no way to be told (it is then offered at its next
/// arm). A headless run never arms (its hooks only look), so its notice is pushed for the
/// next look.
pub(crate) fn mail_stored(app: &AppHandle, handle: &str) {
    let Some(launch) = app.try_state::<DashboardLaunchState>() else {
        return;
    };
    let Some((machine, session)) = inbox_target(&launch, handle) else {
        return;
    };
    if machine != LOCAL_MACHINE_ID
        && !headless_place(app, &session)
        && host_inboxes(app).and_then(|inboxes| HostInboxes::armed(&inboxes, &machine, &session))
            == Some(false)
    {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        offer(&app, &session, Offer::New, 0).await;
    });
}

/// Whether `session` is a headless run on this Mac whose place is on the in-process host
/// (one whose CLI has inbox hooks): its notices go there, and its hooks look for them
/// without arming.
fn headless_place(app: &AppHandle, session: &str) -> bool {
    app.try_state::<DashboardLaunchState>()
        .is_some_and(|launch| launch.has_live_headless_session(session))
        && super::pty_backend::in_process_host_handle(app)
            .is_some_and(|host| host.activity().snapshot(session).is_some())
}

/// An `InboxWaitV1` report from this Mac's hosts; any other event is not one.
pub(crate) fn on_local_pushed<R: tauri::Runtime>(app: &AppHandle<R>, event: TerminalDomainEvent) {
    send(app, LOCAL_MACHINE_ID.to_string(), event);
}

/// An `InboxWaitV1` report from the machine whose connection carries `namespace`; any
/// other event is not one.
pub(crate) fn on_pushed<R: tauri::Runtime>(
    app: &AppHandle<R>,
    namespace: &crate::terminal::session_server::EventNamespace,
    event: TerminalDomainEvent,
) {
    if let Some(machine) = app
        .try_state::<HostConnectionManager>()
        .and_then(|manager| HostConnectionManager::target_for_namespace(&manager, namespace))
        .map(|target| target.member.machine_id)
    {
        send(app, machine, event);
    }
}

fn send<R: tauri::Runtime>(app: &AppHandle<R>, machine: String, event: TerminalDomainEvent) {
    let sender = app
        .try_state::<DashboardLaunchState>()
        .and_then(|launch| locked(&launch.host_inboxes().events).clone());
    if let Some(sender) = sender {
        let _ = sender.send(Pushed { machine, event });
    }
}

/// Settles the reservation of `session`'s notice under `token` as its `fate` says.
pub(crate) async fn settle(host: &dyn InboxHost, session: &str, token: &str, fate: Fate) {
    match fate {
        Fate::Told => {
            if let Err(e) = agent_inbox::confirm_notice(host, session, token).await {
                log::warn!(target: "inbox", "confirming a notice for {session} failed: {e}");
            }
        }
        Fate::Released => agent_inbox::release_notice(host, session, token).await,
        Fate::Uncertain => agent_inbox::hold_notice(host, session, token).await,
    }
}

/// A report from `machine`'s host: a notice's fate settles its reservation (only for a
/// session that runs there), a waiter state is kept for `list_sessions`.
async fn on_report(app: &AppHandle, machine: &str, event: TerminalDomainEvent) {
    if let TerminalDomainEvent::InboxWaiter { state } = &event {
        return on_waiter(app, machine, state);
    }
    let Some((session, token, fate)) = Fate::of(event) else {
        return;
    };
    if tab_machine(app, session.as_str()).as_deref() != Some(machine) {
        return;
    }
    // Settled in arrival order: a later fate never overtakes an earlier one.
    let host = AppInboxHost { app: app.clone() };
    settle(&host, session.as_str(), &token, fate).await;
}

/// `machine` reported `state`: kept for `list_sessions`, and an armed waiter's tab is
/// offered the mail it was not told about.
fn on_waiter(app: &AppHandle, machine: &str, state: &InboxWaiterState) {
    if let Some(inboxes) = host_inboxes(app) {
        HostInboxes::changed(&inboxes, machine, state);
    }
    let _ = app.emit("sessions-waiter-changed", ());
    // Only a tab of this app's on that machine is offered its mail.
    if state.waiting && tab_machine(app, state.session.as_str()).as_deref() == Some(machine) {
        // A launched agent whose waiter armed is at its prompt or mid-turn: one that never
        // reported a turn reads idle, not unknown.
        if let Some(launch) = app.try_state::<DashboardLaunchState>() {
            let session = state.session.as_str();
            if DashboardLaunchState::handle_for_session(&launch, session).is_some()
                || launch.remote_handle_for_session(session).is_some()
            {
                launch.turns().record_ready(session);
            }
        }
        let (app, session) = (app.clone(), state.session.as_str().to_string());
        tauri::async_runtime::spawn(async move { offer(&app, &session, Offer::New, 0).await });
    }
}

/// A headless run on this Mac is about to start: the mail already waiting for it is
/// pushed to its host now, for the look its CLI takes at boot.
pub(crate) async fn offer_headless(app: &AppHandle, session: &str) {
    offer(app, session, Offer::New, 0).await;
}

/// Offers `session`'s agent the mail it was not told about: a notice reserved now is
/// pushed to its host and checked again once its reservation lapses (by then confirmed,
/// or pushed again under its own token). A host that is gone, or a tab that is, waits
/// for the next connect or arm instead. A [`Offer::Restored`] offer pushes an unconfirmed
/// notice again at once. `attempt` counts failed handoffs, never host-held submissions.
async fn offer(app: &AppHandle, session: &str, why: Offer, attempt: u32) {
    let host = AppInboxHost { app: app.clone() };
    // After an app restart a launched tab is bound to its handle again only once the
    // launch restore settles, and its mail is addressed to that handle.
    let restoring = Instant::now() + RESTORE_WAIT;
    while host.handle_for_session(session).is_none()
        && !host.launch_restore_settled()
        && Instant::now() < restoring
    {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    if app
        .try_state::<DashboardLaunchState>()
        .is_some_and(|l| l.workspace_for_session(session).is_none())
    {
        super::dashboard_launch::refresh_chat_binding(app, session).await;
    }
    let again = offer_once(&host, session, why, attempt, |notice, token| {
        push(app, session, notice, token)
    })
    .await;
    if let Some((wait, attempt)) = again {
        schedule(app, session, wait, attempt);
    }
}

/// One offer of `session`'s mail through `push`: a notice reserved now is pushed, and a
/// push that did not land releases its reservation, so the notice is offered again at the
/// next look. When to check again (and the refused pushes counted so far), or `None` to
/// wait for the next connect or arm.
pub(crate) async fn offer_once<F, Fut>(
    host: &dyn InboxHost,
    session: &str,
    why: Offer,
    attempt: u32,
    push: F,
) -> Option<(Duration, u32)>
where
    F: FnOnce(String, String) -> Fut,
    Fut: std::future::Future<Output = Result<(), Push>>,
{
    if let Err(reason) = host.admit_automatic_input(session).await {
        log::debug!(target: "inbox", "holding automatic input for {session}: {reason}");
        return Some((RETRY_PUSH, 0));
    }
    let reoffer = why == Offer::Restored;
    match agent_inbox::check_for_mail(host, session, reoffer).await {
        Ok(Check::Offer { notice, token }) => match push(notice, token.clone()).await {
            Ok(()) => Some((ttl(), 0)),
            Err(Push::Unreachable) => {
                agent_inbox::release_notice(host, session, &token).await;
                None
            }
            Err(Push::NotAccepted(e)) => {
                log::warn!(target: "inbox", "pushing a notice for {session} failed: {e}");
                agent_inbox::release_notice(host, session, &token).await;
                (attempt + 1 < PUSH_ATTEMPTS).then_some((RETRY_PUSH, attempt + 1))
            }
        },
        Ok(Check::PendingFor(wait)) => Some((wait, 0)),
        Ok(Check::Nothing | Check::Held) => None,
        Err(()) => (attempt + 1 < PUSH_ATTEMPTS).then_some((RETRY_PUSH, attempt + 1)),
    }
}

/// How long after a push its reservation has lapsed.
pub(crate) fn ttl() -> Duration {
    Duration::from_millis(
        u64::try_from(crate::weft::inbox::RESERVE_TTL_MS).unwrap_or(60_000)
            + agent_inbox::RESERVE_RECHECK_MARGIN_MS,
    )
}

/// Why a notice did not reach its host.
pub(crate) enum Push {
    /// The host is not connected (or hands no notices), or the tab is gone: the next
    /// connect offers it.
    Unreachable,
    /// The handoff failed before acceptance. A host-held notice (including a CLI refusal
    /// or uncertain submission) returns success and is re-offered only by the host.
    NotAccepted(String),
}

/// Pushes `notice` to the host `session` runs on.
async fn push(app: &AppHandle, session: &str, notice: String, token: String) -> Result<(), Push> {
    let machine = tab_machine(app, session).ok_or(Push::Unreachable)?;
    let target = PtySessionId::new(session);
    // A headless run on this Mac has no terminal: its place is on the in-process host, or
    // (its CLI has no inbox hooks) nowhere.
    let headless = app
        .try_state::<DashboardLaunchState>()
        .is_some_and(|launch| launch.has_live_headless_session(session));
    if machine == LOCAL_MACHINE_ID && headless {
        if !headless_place(app, session) {
            return Err(Push::Unreachable);
        }
        let host = super::pty_backend::in_process_host_handle(app).ok_or(Push::Unreachable)?;
        return tauri::async_runtime::spawn_blocking(move || {
            codesense_pty_core::domain::TerminalDomain::inbox_mail(&*host, &target, &notice, &token)
        })
        .await
        .map_err(|e| Push::NotAccepted(e.to_string()))?
        .map_err(|e| Push::NotAccepted(e.0));
    }
    if machine == LOCAL_MACHINE_ID {
        let app = app.clone();
        return tauri::async_runtime::spawn_blocking(move || {
            super::pty_backend::with_session_reference(
                &app,
                &super::machines::RuntimeSessionReference::Legacy(target.as_str().to_string()),
                false,
                |host, session| {
                    host.inbox_mail(session, &notice, &token)
                        .map_err(String::from)
                },
            )
        })
        .await
        .map_err(|e| Push::NotAccepted(e.to_string()))?
        .map_err(Push::NotAccepted);
    }
    let client = client_of(app, &machine).ok_or(Push::Unreachable)?;
    tauri::async_runtime::spawn_blocking(move || {
        codesense_pty_core::domain::TerminalDomain::inbox_mail(&*client, &target, &notice, &token)
    })
    .await
    .map_err(|e| Push::NotAccepted(e.to_string()))?
    .map_err(|e| Push::NotAccepted(e.0))
}

/// Checks `session`'s inbox again after `wait`, once (a check already scheduled for it
/// covers this one), and only while its host still reports its agent armed: a waiter
/// that arms later checks for itself.
fn schedule(app: &AppHandle, session: &str, wait: Duration, attempt: u32) {
    let Some(inboxes) = host_inboxes(app) else {
        return;
    };
    if !locked(&inboxes.scheduled).insert(session.to_string()) {
        return;
    }
    let (app, session) = (app.clone(), session.to_string());
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(wait).await;
        let Some(inboxes) = host_inboxes(&app) else {
            return;
        };
        locked(&inboxes.scheduled).remove(&session);
        // Unknown (its host's waiters not read) is checked too: only a known lack waits.
        // A headless run's hooks look without arming.
        let armed = tab_machine(&app, &session)
            .map(|machine| HostInboxes::armed(&inboxes, &machine, &session));
        if tab_machine(&app, &session).as_deref() == Some(LOCAL_MACHINE_ID)
            || headless_place(&app, &session)
            || armed.is_some_and(|armed| armed != Some(false))
        {
            offer(&app, &session, Offer::New, attempt).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn state(session: &str, waiting: bool, grace_ms: Option<u64>) -> InboxWaiterState {
        InboxWaiterState {
            session: PtySessionId::new(session),
            waiting,
            grace_ms,
        }
    }

    #[test]
    fn a_machines_waiters_are_unknown_until_it_connects_and_after_it_drops() {
        let inboxes = HostInboxes::default();
        assert_eq!(inboxes.armed("m1", "S1"), None);
        inboxes.connecting("m1");
        // Until its snapshot is read, its tabs are unknown.
        assert_eq!(inboxes.armed("m1", "S1"), None);
        // A change pushed while the snapshot is read is kept over the snapshot's.
        inboxes.changed("m1", &state("S1", false, None));
        inboxes.snapshot(
            "m1",
            vec![state("S1", true, None), state("S2", false, Some(60_000))],
        );
        assert_eq!(inboxes.armed("m1", "S1"), Some(false));
        // Within its grace a waiter that ended still reads armed; a tab with none, not.
        assert_eq!(inboxes.armed("m1", "S2"), Some(true));
        assert_eq!(inboxes.armed("m1", "S3"), Some(false));
        inboxes.changed("m1", &state("S1", true, None));
        assert_eq!(inboxes.armed("m1", "S1"), Some(true));
        // A dropped link: unknown, never a stale `true`.
        inboxes.disconnected("m1");
        assert_eq!(inboxes.armed("m1", "S2"), None);
        // A change or a snapshot for a machine that is not connected is not kept.
        inboxes.changed("m1", &state("S1", true, None));
        inboxes.snapshot("m1", vec![state("S1", true, None)]);
        assert_eq!(inboxes.armed("m1", "S1"), None);
    }

    #[test]
    fn this_macs_waiters_are_known_from_the_start_and_read_afresh_on_a_reconnect() {
        let inboxes = HostInboxes::default();
        assert_eq!(inboxes.armed(LOCAL_MACHINE_ID, "S1"), Some(false));
        inboxes.changed(LOCAL_MACHINE_ID, &state("S1", true, None));
        assert_eq!(inboxes.armed(LOCAL_MACHINE_ID, "S1"), Some(true));
        // Its server restarted: what the old one reported is gone.
        inboxes.connecting(LOCAL_MACHINE_ID);
        inboxes.snapshot(LOCAL_MACHINE_ID, vec![state("S2", false, Some(60_000))]);
        assert_eq!(inboxes.armed(LOCAL_MACHINE_ID, "S1"), Some(false));
        assert_eq!(inboxes.armed(LOCAL_MACHINE_ID, "S2"), Some(true));
    }

    #[test]
    fn a_local_tabs_notices_go_to_the_session_its_handle_is_bound_to() {
        let launch = DashboardLaunchState::new(1);
        launch.begin("H1").unwrap();
        launch.bind("H1", "S1", None).unwrap();
        assert_eq!(
            inbox_target(&launch, "H1"),
            Some((LOCAL_MACHINE_ID.to_string(), "S1".to_string()))
        );
        // A tab opened by hand is addressed by its own session.
        assert_eq!(
            inbox_target(&launch, "hand-tab"),
            Some((LOCAL_MACHINE_ID.to_string(), "hand-tab".to_string()))
        );
    }

    #[test]
    fn a_grace_is_counted_on_this_macs_clock_from_when_it_arrived() {
        assert!(!Waiter::of(&state("S1", false, Some(0))).armed());
        assert!(Waiter::of(&state("S1", false, Some(60_000))).armed());
        assert!(Waiter::of(&state("S1", true, None)).armed());
        // A grace past what this clock can count never panics.
        let _ = Waiter::of(&state("S1", false, Some(u64::MAX))).armed();
    }
}

#[cfg(test)]
#[test]
fn reconnect_snapshot_cannot_overwrite_a_newer_connection() {
    let inboxes = HostInboxes::default();
    let old = inboxes.connecting(LOCAL_MACHINE_ID);
    let new = inboxes.connecting(LOCAL_MACHINE_ID);
    inboxes.snapshot_for(LOCAL_MACHINE_ID, new, vec![tests::state("S1", true, None)]);
    inboxes.snapshot_for(LOCAL_MACHINE_ID, old, vec![tests::state("S1", false, None)]);
    assert!(!inboxes.current(LOCAL_MACHINE_ID, old));
    assert_eq!(inboxes.armed(LOCAL_MACHINE_ID, "S1"), Some(true));
}
