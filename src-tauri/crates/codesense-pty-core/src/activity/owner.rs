// SPDX-License-Identifier: MIT
use super::subagents::OpenSubagents;
use super::{
    process, Action, Event, Exit, Identity, Mutation, Outcome, Report, Scope, Snapshot, State,
    Writer,
};
use crate::agent_cli::{AgentCli, AgentCliAdapter, InboxDelivery, Subagents};
use crate::inbox_host::{Fate, HostInbox, InboxOutlet, Repeat};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

const MAX_SESSIONS: usize = 1024;
const RECENT_REPORTS: usize = 128;
const OUTPUT_ACTIVITY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
/// Legacy timeout retained for source compatibility. Silence no longer ends a
/// turn: only provider evidence or process retirement changes its lifecycle.
pub const QUIET_TURN: std::time::Duration = std::time::Duration::from_secs(25);

struct Session {
    pending_native: bool,
    incarnation: String,
    process: Option<process::Process>,
    invocation: Option<Invocation>,
    child: Option<crate::runner::ChildHandle>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WaitKind {
    Permission,
    Question,
}

impl WaitKind {
    fn from_report(report: &Report) -> Self {
        if report.reason.as_deref() == Some("question") {
            Self::Question
        } else {
            Self::Permission
        }
    }
}

struct Invocation {
    engine_loss_pending: bool,
    artifacts: Option<super::launch::OwnedDirectory>,
    inbox_update_required: bool,
    arbitration: super::arbiter::DotState,
    operations: VecDeque<String>,
    asks: HashMap<String, u64>,
    closed_asks: HashSet<String>,
    tools: HashSet<String>,
    waits: HashMap<String, WaitKind>,
    uncorrelated_wait: Option<WaitKind>,
    /// The tool a permission prompt named, when it named one: only that tool's
    /// own progress answers the prompt.
    wait_tool: Option<String>,
    /// A one-shot run (`claude -p`, `codex exec`) draws nothing while it works,
    /// so a quiet terminal says nothing about its turn.
    batch: bool,
    snapshot: Snapshot,
    token: String,
    process: Option<process::Process>,
    reports: VecDeque<String>,
    provider_session: Option<String>,
    retired_provider_sessions: HashSet<String>,
    turn: Option<String>,
    old_turns: VecDeque<String>,
    /// The last provider evidence or explicit mutation.
    evidence_at: Option<Instant>,
    /// The last output the terminal produced, while this invocation runs.
    output_at: Option<Instant>,
    /// Bound output-only activity publications without losing the actual time.
    output_published_at: Option<Instant>,
    /// Its agent's subagents that run now. Its own turn's end leaves them running.
    subagents: OpenSubagents,
    /// The engine its CLI runs on, started under it by its launch helper.
    engine: Option<Engine>,
}

/// An engine a CLI runs on: its process, and the socket its host reaches it through.
/// Its keeper ends it with its CLI. A CLI killed with its keeper (its whole process group)
/// leaves the socket behind, and a keeper killed alone its engine: the host ends both
/// once it forgets the invocation.
#[derive(Debug)]
struct Engine {
    home: Option<PathBuf>,
    process: process::Process,
    keeper: process::Process,
    end_on_loss: bool,
    endpoint: PathBuf,
}

impl Drop for Engine {
    /// Forgotten (its invocation is), it is ended if it still runs, and its socket goes.
    fn drop(&mut self) {
        if process::inspect(self.process.pid) == Some(self.process) {
            // SAFETY: signalling the process just identified as this engine.
            unsafe {
                libc::kill(self.process.pid as i32, libc::SIGTERM);
            }
        }
        super::engine::remove_socket(&self.endpoint);
    }
}

/// How a host reaches an invocation's CLI to hand it a notice: its engine at `endpoint`,
/// connected by `connect`, for the session its hooks last named.
struct Reach {
    cli: AgentCli,
    connect: crate::agent_cli::ChannelConnect,
    endpoint: PathBuf,
    session: Option<String>,
    home: Option<PathBuf>,
}

/// The invocation the writer `token` names, among `sessions`: a writer token is 64
/// characters, so nothing else names one.
fn invocation_mut<'a>(
    sessions: &'a mut HashMap<String, Session>,
    token: &str,
) -> Option<&'a mut Invocation> {
    if token.len() != 64 {
        return None;
    }
    sessions
        .values_mut()
        .filter_map(|entry| entry.invocation.as_mut())
        .find(|invocation| invocation.token == token)
}

impl Invocation {
    /// Whether this invocation's process is named (committed) and has not exited.
    fn running(&self) -> bool {
        self.process.is_some() && self.snapshot.exit.is_none()
    }

    /// How its host hands it notices, its CLI being `adapter`'s: through its engine, for a
    /// running invocation of a host-submitted CLI whose engine still runs, not a one-shot
    /// run, once it is observed (or from its start, for a CLI observed from it). Any other
    /// reads as one new mail does not reach, and its mail waits for its next check. It is
    /// handed one only while seen at its prompt, by lifecycle hooks or verified native
    /// startup evidence (including the enabled observers and their trust).
    fn reach(&self, adapter: &dyn AgentCliAdapter) -> Option<Reach> {
        if !(self.snapshot.observed || adapter.session_policy().observed_from_start) {
            return None;
        }
        self.engine_reach(adapter)
    }

    /// Bound native transport, without claiming that its interactive session is ready.
    fn engine_reach(&self, adapter: &dyn AgentCliAdapter) -> Option<Reach> {
        if !matches!(adapter.inbox(), InboxDelivery::HostSubmit { .. })
            || !self.running()
            || self.batch
        {
            return None;
        }
        let connect = adapter.control().channel?;
        // Its engine still runs, under the CLI the invocation committed.
        let engine = self.engine.as_ref().filter(|engine| {
            process::inspect(engine.process.pid) == Some(engine.process)
                && self
                    .process
                    .is_some_and(|cli| process::descends_from(engine.process, cli))
        })?;
        Some(Reach {
            cli: self.snapshot.provider,
            connect,
            endpoint: engine.endpoint.clone(),
            session: self.provider_session.clone(),
            home: engine.home.clone(),
        })
    }

    /// Host hold facts outlive display writes. Only their own answer/retirement
    /// removes them; each activity continuation uses this projection. Done is
    /// a separate explicit outcome contract and never passes through here.
    fn project_activity(&mut self, continuation: State, reason: Option<String>) {
        let mut kinds = self.waits.values().copied().chain(self.uncorrelated_wait);
        if kinds.any(|kind| kind == WaitKind::Question) {
            self.snapshot.state = State::NeedsPermission;
            self.snapshot.reason = Some("question".into());
        } else if !self.waits.is_empty() || self.uncorrelated_wait.is_some() {
            self.snapshot.state = State::NeedsPermission;
            self.snapshot.reason = None;
        } else if self.arbitration.open_asks > 0 {
            self.snapshot.state = State::NeedsPermission;
            // A newer declared block still owns ask cleanup until admitted
            // activity clears it. Ask IDs independently preserve question UI.
            self.snapshot.reason = reason.filter(|reason| reason == "blocked");
        } else {
            self.snapshot.state = continuation;
            self.snapshot.reason = reason;
        }
    }

    fn capture(&self, now: Instant) -> Snapshot {
        let mut snapshot = self.snapshot.clone();
        // A reconnect/snapshot read must obey the same teardown ordering as pushed
        // events: the final end cause is visible only after replacement cleanup.
        if self.engine_loss_pending {
            snapshot.exit = None;
            snapshot.reason = None;
        }
        // Like its tools, its subagents are a live, observed invocation's.
        snapshot.subagents_open = if snapshot.observed && snapshot.exit.is_none() {
            self.subagents.count()
        } else {
            0
        };
        snapshot.tool_open = snapshot
            .observed
            .then_some(snapshot.exit.is_none() && !self.tools.is_empty());
        snapshot.activity_age_ms = self
            .evidence_at
            .into_iter()
            .chain(self.output_at)
            .max()
            .map(|at| {
                now.saturating_duration_since(at)
                    .as_millis()
                    .min(u64::MAX as u128) as u64
            });
        if self.inbox_update_required && !snapshot.observed && snapshot.exit.is_none() {
            snapshot.reason = Some("inbox-update-required".into());
        }
        snapshot
    }

    /// New provider evidence clears a self-reported block, while independent
    /// permission prompts and open questions keep their own holds.
    fn clear_reported_block(&mut self) -> bool {
        if self.snapshot.reason.as_deref() != Some("blocked") {
            return false;
        }
        self.snapshot.reason = None;
        if self.arbitration.open_asks > 0 {
            // The block no longer owns the display; resolving the surviving
            // question must be allowed to restore the turn's working state.
            self.arbitration.superseded = false;
        }
        if !self.settle_answered() {
            self.snapshot.revision += 1;
        }
        true
    }

    /// Whether progress of `tool_name` answers the open permission prompt that
    /// named no tool use; clears it when it does.
    fn answer_wait(&mut self, tool_name: Option<&str>) -> bool {
        if self.uncorrelated_wait.is_none() {
            return false;
        }
        if let (Some(waiting), Some(name)) = (&self.wait_tool, tool_name) {
            if waiting != name {
                return false;
            }
        }
        self.uncorrelated_wait = None;
        self.wait_tool = None;
        true
    }

    /// Leave a permission state nothing holds any more for the turn's own.
    fn settle_answered(&mut self) -> bool {
        if self.snapshot.state != State::NeedsPermission || self.arbitration.latched {
            return false;
        }
        let continuation = if self.tools.is_empty() {
            State::Thinking
        } else {
            State::Working
        };
        let prior = (self.snapshot.state, self.snapshot.reason.clone());
        let reason = (self.arbitration.open_asks > 0)
            .then(|| self.snapshot.reason.clone())
            .flatten();
        self.project_activity(continuation, reason);
        if prior == (self.snapshot.state, self.snapshot.reason.clone()) {
            return false;
        }
        self.snapshot.revision += 1;
        true
    }
}

// Caller and blocking workers publish through one event seam.
fn publish_activity(sink: Option<&dyn crate::domain::TerminalEventSink>, snapshot: Snapshot) {
    if let Some(sink) = sink {
        let _ = sink.emit(crate::domain::TerminalDomainEvent::Activity { snapshot });
    }
}

/// One owner per terminal host, independent of transport/client lifetimes.
/// The registry retains latest observations only; removing a PTY releases them.
pub struct ActivityOwner {
    boot: String,
    sink: Option<std::sync::Arc<dyn crate::domain::TerminalEventSink>>,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    launch_paths: Mutex<Option<(std::path::PathBuf, std::path::PathBuf)>>,
    /// The inbox notices of this host's sessions, handed to the agents its invocations
    /// name.
    inbox: Arc<HostInbox>,
    /// Where the inbox's reports go: this host's client(s).
    inbox_outlet: Mutex<Option<Arc<dyn InboxOutlet>>>,
    /// When the inbox's states were last swept.
    inbox_swept: Mutex<Option<Instant>>,
    /// The adapter of each CLI its sessions run: [`crate::agent_cli::adapter`], unless a
    /// test runs a CLI of its own in a built-in's place.
    adapters: crate::agent_cli::Adapters,
}

/// How often the inbox's states are compared with what each session's desktop was last
/// told, so a change no waiter reported (an agent exited, went idle, a grace ran out)
/// is told too.
const INBOX_SWEEP: std::time::Duration = std::time::Duration::from_secs(5);

/// Whether an inbox hook's helper may wait for `session`'s agent.
pub(super) enum InboxAdmission {
    /// It runs under the agent process its host named for `session`, whose CLI hears
    /// mail as `delivery`.
    Admitted {
        session: String,
        agent: super::peer::PeerProcess,
        delivery: InboxDelivery,
        /// A one-shot run (`claude -p`): it ends after its answer, so its hooks only look,
        /// never holding the run open.
        batch: bool,
    },
    /// Its invocation is not committed yet: one armed at a boot or a turn end, or a
    /// host-submitted CLI's check, may ask again shortly. Its CLI hears mail as `delivery`.
    Pending { delivery: InboxDelivery },
    /// It is not the session's agent's (a nested, detached or foreign CLI's, or one
    /// whose invocation ended).
    Refused,
}

impl Default for ActivityOwner {
    fn default() -> Self {
        Self::new()
    }
}

impl ActivityOwner {
    pub fn new() -> Self {
        Self {
            boot: uuid::Uuid::new_v4().to_string(),
            sink: None,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            launch_paths: Mutex::new(None),
            inbox: Arc::default(),
            inbox_outlet: Mutex::new(None),
            inbox_swept: Mutex::new(None),
            adapters: crate::agent_cli::adapter,
        }
    }

    /// An owner whose sessions' CLIs are reached through `adapters`: a test's CLI in a
    /// built-in's place.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_adapters(adapters: crate::agent_cli::Adapters) -> Self {
        Self {
            adapters,
            ..Self::new()
        }
    }

    /// `cli`'s adapter.
    fn adapter(&self, cli: AgentCli) -> &'static dyn AgentCliAdapter {
        (self.adapters)(cli)
    }

    pub(crate) fn with_sink(
        sink: std::sync::Arc<dyn crate::domain::TerminalEventSink>,
        adapters: crate::agent_cli::Adapters,
    ) -> Self {
        let outlet: Arc<dyn InboxOutlet> = Arc::new(crate::inbox_host::SinkOutlet(sink.clone()));
        Self {
            sink: Some(sink),
            inbox_outlet: Mutex::new(Some(outlet)),
            adapters,
            ..Self::new()
        }
    }

    /// Send the inbox's reports to `outlet` from now on (a host whose sessions belong to
    /// different clients routes each to its own).
    pub fn set_inbox_outlet(&self, outlet: Arc<dyn InboxOutlet>) {
        *self.inbox_outlet.lock().unwrap_or_else(|p| p.into_inner()) = Some(outlet);
    }

    /// Where the inbox's reports go, once this host has a client.
    pub fn inbox_outlet(&self) -> Option<Arc<dyn InboxOutlet>> {
        self.inbox_outlet
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// This host's inbox notices.
    pub fn inbox(&self) -> &Arc<HostInbox> {
        &self.inbox
    }

    /// Whether the helper `peer`, asking with `token` (its CLI's activity writer token)
    /// for the agent process `agent` (the CLI that ran its hook, naming the CLI session
    /// `hook_session`), may wait for mail: only when `agent` is the invocation `token` names,
    /// committed and still running, or the engine it runs on, or a program under it that
    /// names the invocation's own CLI session; and `peer` runs under it.
    pub(super) fn inbox_admission(
        &self,
        token: &str,
        peer: process::Process,
        agent: u32,
        hook_session: Option<&str>,
    ) -> InboxAdmission {
        let found = {
            let sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            sessions.iter().find_map(|(session, entry)| {
                entry
                    .invocation
                    .as_ref()
                    .filter(|invocation| !token.is_empty() && invocation.token == token)
                    .map(|invocation| {
                        (
                            session.clone(),
                            invocation.process,
                            invocation.snapshot.exit.is_some(),
                            invocation.snapshot.provider,
                            invocation.batch,
                            invocation.engine.as_ref().map(|engine| engine.process),
                            invocation.provider_session.is_some()
                                && invocation.provider_session.as_deref() == hook_session,
                        )
                    })
            })
        };
        // The processes are read with no lock held.
        let Some((session, process, exited, cli, batch, engine, its_own)) = found else {
            return InboxAdmission::Refused;
        };
        let delivery = self.adapter(cli).inbox();
        let Some(process) = process else {
            return InboxAdmission::Pending { delivery };
        };
        let Some(running) = process::inspect(agent).filter(|_| !exited) else {
            return InboxAdmission::Refused;
        };
        // The agent that runs its hooks: the CLI itself, or the engine it runs on, under it;
        // or a program under the CLI that runs in its place (npm's `codex` script runs its
        // binary as its child), which is the agent only by the session its hook names, the
        // agent's own. A nested CLI its tool runs is under it too, but names a session of its
        // own (one resuming the agent's own session is taken for the agent).
        let named = if running.is(process) {
            process
        } else if let Some(engine) =
            engine.filter(|engine| running.is(*engine) && process::descends_from(*engine, process))
        {
            engine
        } else if its_own && process::descends_from(running, process) {
            process
        } else {
            return InboxAdmission::Refused;
        };
        // Under the agent, which is (or is under) `named`.
        if !process::descends_from(peer, running) {
            return InboxAdmission::Refused;
        }
        InboxAdmission::Admitted {
            session,
            agent: super::peer::PeerProcess::of(named),
            delivery,
            batch,
        }
    }

    /// The launch helper `peer` of the invocation `token` names (the CLI it is about to
    /// become) started the engine `pid` under itself, serving on `endpoint`: its host
    /// reaches the CLI through it from now on.
    pub(super) fn bind_engine(
        &self,
        token: &str,
        peer: process::Process,
        pid: u32,
        endpoint: PathBuf,
        home: Option<PathBuf>,
    ) -> Result<(), &'static str> {
        if home.as_ref().is_some_and(|home| !home.is_absolute()) {
            return Err("native home must be absolute");
        }
        let engine = process::inspect(pid).ok_or("activity engine unavailable")?;
        if process::inspect(peer.pid) != Some(peer) || !process::descends_from(engine, peer) {
            return Err("activity engine is not its launch's");
        }
        // Its socket is an engine's, beside the host's own in the host's private root.
        if self
            .launch_root()
            .is_none_or(|root| endpoint.parent() != Some(root.as_path()))
            || !super::engine::names_engine_socket(&endpoint)
        {
            return Err("activity engine endpoint outside the host");
        }
        let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        let invocation =
            invocation_mut(&mut sessions, token).ok_or("activity invocation unavailable")?;
        // Committed or not yet, the invocation is its launch's own process, and it binds
        // one engine.
        if invocation.process.is_some_and(|process| process != peer) || invocation.engine.is_some()
        {
            return Err("activity engine is not its launch's");
        }
        invocation.engine = Some(Engine {
            home,
            end_on_loss: self
                .adapter(invocation.snapshot.provider)
                .end_on_engine_loss(),
            keeper: process::inspect(engine.parent).ok_or("activity keeper unavailable")?,
            process: engine,
            endpoint,
        });
        let adapter = self.adapter(invocation.snapshot.provider);
        drop(sessions);
        if adapter.control().native_readiness {
            self.observe_engine(token.to_owned());
        }
        Ok(())
    }

    /// Initial native evidence is read away from the registry lock. A weak registry
    /// cannot keep a forgotten host (or its engines) alive. Hooks win every race.
    fn observe_engine(&self, token: String) {
        let sessions = Arc::downgrade(&self.sessions);
        let adapters = self.adapters;
        let sink = self.sink.clone();
        std::thread::spawn(move || loop {
            let Some(registry) = sessions.upgrade() else {
                return;
            };
            let reach = {
                let mut entries = registry.lock().unwrap_or_else(|p| p.into_inner());
                let Some(invocation) = invocation_mut(&mut entries, &token) else {
                    return;
                };
                if invocation.snapshot.observed || invocation.snapshot.exit.is_some() {
                    return;
                }
                invocation.engine_reach(adapters(invocation.snapshot.provider))
            };
            drop(registry);
            let ready = reach.and_then(|reach| {
                (reach.connect)(
                    &reach.endpoint,
                    reach.session.as_deref(),
                    reach.home.as_deref(),
                )
                .ok()?
                .ready_session()
            });
            if let Some(session) = ready {
                let Some(registry) = sessions.upgrade() else {
                    return;
                };
                let snapshot = {
                    let mut entries = registry.lock().unwrap_or_else(|p| p.into_inner());
                    let Some(invocation) = invocation_mut(&mut entries, &token) else {
                        return;
                    };
                    if invocation.snapshot.observed || invocation.snapshot.exit.is_some() {
                        return;
                    }
                    if invocation
                        .engine_reach(adapters(invocation.snapshot.provider))
                        .is_none()
                    {
                        return;
                    }
                    invocation.provider_session = Some(session);
                    invocation.snapshot.observed = true;
                    invocation.snapshot.limited = false;
                    invocation.snapshot.revision += 1;
                    invocation.evidence_at = Some(Instant::now());
                    invocation.capture(Instant::now())
                };
                publish_activity(sink.as_deref(), snapshot);
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        });
    }

    /// `session`'s desktop pushed `notice`, reserved under `token`: held for its agent, or
    /// submitted to a CLI the host hands notices to. A repeat of one already settled is
    /// reported again, never sent again.
    pub fn inbox_mail(&self, session: &str, notice: String, token: String) -> Result<(), String> {
        use codesense_host_proto::inbox_wait as wire;
        if !wire::valid_token(&token) || notice.len() >= wire::MAX_LINE {
            return Err("invalid inbox notice".into());
        }
        let outlet = self.inbox_outlet().ok_or("inbox notices unavailable")?;
        if !self
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(session)
        {
            return Err(format!("no session {session} runs on this host"));
        }
        match self.inbox.mail(session, notice, token.clone()) {
            // A repeat of one already settled is only reported again.
            Repeat::Told => {
                outlet.fate(session, &token, Fate::Told);
                return Ok(());
            }
            Repeat::Uncertain => {
                outlet.fate(session, &token, Fate::Uncertain);
                return Ok(());
            }
            Repeat::Held => {}
        }
        // Only an invocation the host has named (committed, running), on an engine, and not
        // seen working is submitted to. A working one keeps the notice held, untaken, for
        // its next check; the sweep submits it once it is idle.
        let reach = self
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(session)
            .and_then(|entry| entry.invocation.as_ref())
            .filter(|invocation| invocation.snapshot.takes_input())
            .and_then(|invocation| invocation.reach(self.adapter(invocation.snapshot.provider)));
        if let Some(reach) = reach {
            self.submit_inbox(&outlet, session, reach)?;
        }
        Ok(())
    }

    pub(super) fn inbox_confirmation(&self, token: &str, event: &str) -> bool {
        let sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        sessions
            .values()
            .filter_map(|entry| entry.invocation.as_ref())
            .find(|invocation| !token.is_empty() && invocation.token == token)
            .is_some_and(|invocation| {
                self.adapter(invocation.snapshot.provider)
                    .confirms_notice(event)
            })
    }

    /// Hands `session`'s held notice to its CLI through its engine, as `reach` says, on a
    /// thread of its own (connecting may wait on the engine).
    fn submit_inbox(
        &self,
        outlet: &Arc<dyn InboxOutlet>,
        session: &str,
        reach: Reach,
    ) -> Result<(), String> {
        if !self.inbox.awaits_submit(session) || outlet.has_tools(session) != Some(true) {
            return Ok(());
        }
        let adapter = self.adapter(reach.cli);
        // Tracked from now on, so a newer notice pushed while this one is submitted waits
        // for the agent to be seen idle again.
        self.inbox.track_submitter(session);
        let (inbox, outlet, session) = (self.inbox.clone(), outlet.clone(), session.to_string());
        std::thread::Builder::new()
            .name("inbox-submit".into())
            .spawn(move || {
                inbox.submit(&*outlet, &session, adapter, || {
                    (reach.connect)(
                        &reach.endpoint,
                        reach.session.as_deref(),
                        reach.home.as_deref(),
                    )
                    .ok()
                });
            })
            .map(drop)
            .map_err(|error| error.to_string())
    }

    /// Whom new mail reaches for each session with a way to be told.
    pub fn inbox_waiters(&self) -> Vec<crate::domain::InboxWaiterState> {
        self.inbox
            .sessions()
            .into_iter()
            .map(|session| self.inbox.state(&session))
            .collect()
    }

    /// Every [`INBOX_SWEEP`]: which sessions the host submits notices to (a running
    /// invocation of a [`InboxDelivery::HostSubmit`] CLI it can reach), whether each is
    /// idle, and every state change reported.
    fn sweep_inbox(&self, now: Instant) {
        {
            let mut swept = self.inbox_swept.lock().unwrap_or_else(|p| p.into_inner());
            if swept.is_some_and(|at| now.saturating_duration_since(at) < INBOX_SWEEP) {
                return;
            }
            *swept = Some(now);
        }
        let Some(outlet) = self.inbox_outlet() else {
            return;
        };
        let submitters: Vec<(String, Option<(bool, Reach)>)> = {
            let sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            sessions
                .iter()
                .filter_map(|(session, entry)| {
                    let invocation = entry.invocation.as_ref()?;
                    let reach = invocation
                        .reach(self.adapter(invocation.snapshot.provider))
                        .filter(|_| outlet.has_tools(session) == Some(true));
                    Some((
                        session.clone(),
                        reach
                            .filter(|_| invocation.snapshot.state != State::NeedsPermission)
                            .map(|reach| (invocation.snapshot.takes_input(), reach)),
                    ))
                })
                .collect()
        };
        for (session, reach) in submitters {
            self.inbox
                .set_submitter(&session, reach.as_ref().map(|(idle, _)| *idle));
            // An idle agent is handed what it was not handed while it worked.
            if let Some((true, reach)) = reach {
                let _ = self.submit_inbox(&outlet, &session, reach);
            }
        }
        let recover: Vec<String> = {
            let sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            sessions
                .iter()
                .filter_map(|(session, entry)| {
                    let invocation = entry.invocation.as_ref()?;
                    (matches!(
                        self.adapter(invocation.snapshot.provider).inbox(),
                        InboxDelivery::Waiter { .. }
                    ) && self
                        .adapter(invocation.snapshot.provider)
                        .allows_pty_recovery()
                        && !invocation.batch
                        && invocation.snapshot.observed
                        && invocation.snapshot.takes_input()
                        && invocation.snapshot.exit.is_none()
                        && !self.inbox.waiter_armed(session)
                        && outlet.has_tools(session) != Some(false)
                        && self.inbox.awaits_submit(session))
                    .then(|| session.clone())
                })
                .collect()
        };
        for session in recover {
            let (inbox, outlet) = (self.inbox.clone(), outlet.clone());
            let _ = std::thread::Builder::new()
                .name("inbox-recover".into())
                .spawn(move || inbox.recover(&*outlet, &session));
        }
        self.inbox.sweep(&*outlet);
    }

    fn publish(&self, snapshot: Snapshot) {
        publish_activity(self.sink.as_deref(), snapshot);
    }

    pub(super) fn configure_ingress(&self, socket: std::path::PathBuf, helper: std::path::PathBuf) {
        *self.launch_paths.lock().unwrap_or_else(|p| p.into_inner()) = Some((socket, helper));
    }

    pub fn prepare_shell_launch(
        &self,
        session: &str,
        provider: AgentCli,
    ) -> Result<super::ShellLaunch, String> {
        let (socket, helper) = self
            .launch_paths
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .ok_or("activity ingress unavailable")?;
        super::launch::prepare_shell_launch(self, session, provider, &socket, &helper)
    }

    /// Reserve native launch identity before spawn. Registration commits its PID.
    pub fn prepare_native(
        &self,
        session: &str,
        provider: AgentCli,
        socket: &str,
    ) -> Result<Writer, &'static str> {
        self.prepare_pending(session, provider, socket, false)
            .map(|(writer, _)| writer)
    }

    /// Reserve a one-shot run with no terminal (`claude -p` launched by the desktop):
    /// its hooks only look for mail, never hold the run open. Registration commits its
    /// PID; the returned incarnation removes it once the run ends.
    pub fn prepare_batch(
        &self,
        session: &str,
        provider: AgentCli,
        socket: &str,
    ) -> Result<(Writer, String), &'static str> {
        self.prepare_pending(session, provider, socket, true)
    }

    fn prepare_pending(
        &self,
        session: &str,
        provider: AgentCli,
        socket: &str,
        batch: bool,
    ) -> Result<(Writer, String), &'static str> {
        let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        if sessions.contains_key(session) {
            return Err("activity session already exists");
        }
        if sessions.len() >= MAX_SESSIONS {
            return Err("activity session limit");
        }
        let incarnation = uuid::Uuid::new_v4().to_string();
        let mut invocation = self.invocation(session, &incarnation, provider);
        invocation.batch = batch;
        let writer = Writer {
            identity: invocation.snapshot.identity.clone(),
            token: invocation.token.clone(),
            socket: socket.into(),
        };
        sessions.insert(
            session.into(),
            Session {
                child: None,
                pending_native: true,
                incarnation: incarnation.clone(),
                process: None,
                invocation: Some(invocation),
            },
        );
        Ok((writer, incarnation))
    }

    fn invocation(&self, session: &str, incarnation: &str, provider: AgentCli) -> Invocation {
        Invocation {
            engine_loss_pending: false,
            artifacts: None,
            inbox_update_required: false,
            arbitration: super::arbiter::DotState::default(),
            operations: VecDeque::new(),
            asks: HashMap::new(),
            closed_asks: HashSet::new(),
            tools: HashSet::new(),
            waits: HashMap::new(),
            uncorrelated_wait: None,
            wait_tool: None,
            batch: false,
            snapshot: Snapshot {
                session: session.into(),
                identity: Identity {
                    host_boot_id: self.boot.clone(),
                    session_incarnation: incarnation.into(),
                    invocation_id: uuid::Uuid::new_v4().to_string(),
                },
                revision: 1,
                turn_generation: 0,
                asks: Vec::new(),
                provider,
                state: State::Idle,
                observed: false,
                limited: true,
                exit: None,
                reason: None,
                tool_open: None,
                activity_age_ms: None,
                subagents_open: 0,
            },
            token: format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ),
            process: None,
            reports: VecDeque::new(),
            provider_session: None,
            retired_provider_sessions: HashSet::new(),
            turn: None,
            old_turns: VecDeque::new(),
            evidence_at: None,
            output_at: None,
            output_published_at: None,
            subagents: OpenSubagents::default(),
            engine: None,
        }
    }

    /// Reserve a fresh invocation inside an existing managed shell. A running
    /// invocation cannot be displaced merely by requesting another launch.
    pub fn prepare_shell(
        &self,
        session: &str,
        provider: AgentCli,
        socket: &str,
    ) -> Result<Writer, &'static str> {
        let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        let entry = sessions
            .get_mut(session)
            .ok_or("activity shell unavailable")?;
        let shell = entry.process.ok_or("activity shell unavailable")?;
        if entry.pending_native || process::inspect(shell.pid) != Some(shell) {
            return Err("activity shell unavailable");
        }
        if let Some(previous) = &entry.invocation {
            if previous
                .process
                .is_some_and(|process| !process::retired(process))
            {
                return Err("activity invocation still running");
            }
        }
        let invocation = self.invocation(session, &entry.incarnation, provider);
        let writer = Writer {
            identity: invocation.snapshot.identity.clone(),
            token: invocation.token.clone(),
            socket: socket.into(),
        };
        let snapshot = invocation.capture(Instant::now());
        entry.invocation = Some(invocation);
        drop(sessions);
        self.publish(snapshot);
        Ok(writer)
    }

    /// A CLI typed by hand claims a fresh invocation of the shell it runs
    /// under. Kernel ancestry from the claiming process to a registered, live
    /// PTY shell of this host is the whole admission: no token, no grant.
    pub(super) fn claim_shell(
        &self,
        peer: process::Process,
        provider: AgentCli,
        socket: &str,
        batch: bool,
    ) -> Result<(String, Writer), &'static str> {
        let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        let shells: HashMap<u32, (&String, process::Process)> = sessions
            .iter()
            .filter(|(_, entry)| !entry.pending_native)
            .filter_map(|(session, entry)| entry.process.map(|shell| (shell.pid, (session, shell))))
            .collect();
        let mut current = peer;
        let mut found = None;
        for _ in 0..128 {
            if current.parent <= 1 || current.parent == current.pid {
                break;
            }
            let Some(parent) = process::inspect(current.parent) else {
                break;
            };
            if let Some((session, shell)) = shells.get(&parent.pid) {
                if *shell == parent {
                    found = Some((*session).clone());
                }
                break;
            }
            current = parent;
        }
        let session = found.ok_or("activity shell unavailable")?;
        let entry = sessions
            .get_mut(&session)
            .ok_or("activity shell unavailable")?;
        if let Some(previous) = &entry.invocation {
            if previous
                .process
                .is_some_and(|process| !process::retired(process))
            {
                return Err("activity invocation still running");
            }
        }
        let mut invocation = self.invocation(&session, &entry.incarnation, provider);
        invocation.process = Some(peer);
        invocation.batch = batch;
        let writer = Writer {
            identity: invocation.snapshot.identity.clone(),
            token: invocation.token.clone(),
            socket: socket.into(),
        };
        let snapshot = invocation.capture(Instant::now());
        entry.invocation = Some(invocation);
        drop(sessions);
        self.publish(snapshot);
        Ok((session, writer))
    }

    /// The host's private activity root, once its ingress listens.
    pub(super) fn launch_root(&self) -> Option<std::path::PathBuf> {
        self.launch_paths
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .and_then(|(socket, _)| socket.parent().map(std::path::Path::to_path_buf))
    }

    /// A private directory for the launch files of the invocation `token` names, removed
    /// with it.
    pub fn launch_directory(&self, token: &str) -> Result<std::path::PathBuf, String> {
        let root = self.launch_root().ok_or("activity ingress unavailable")?;
        let (directory, owned) = super::launch::launch_directory(&root)?;
        self.retain_artifacts(token, owned)?;
        Ok(directory)
    }

    pub(super) fn retain_artifacts(
        &self,
        token: &str,
        artifacts: super::launch::OwnedDirectory,
    ) -> Result<(), &'static str> {
        let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        let invocation = invocation_mut(&mut sessions, token).ok_or("activity writer retired")?;
        invocation.artifacts = Some(artifacts);
        Ok(())
    }

    /// A short-lived exec shim claims exactly its pre-authorized invocation.
    /// Kernel ancestry binds it to the live owning shell before it execs the CLI.
    pub(super) fn bind_shell(
        &self,
        token: &str,
        peer: process::Process,
        deadline: Instant,
        batch: bool,
    ) -> Result<(), &'static str> {
        let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        let entry = sessions
            .values_mut()
            .find(|entry| {
                entry
                    .invocation
                    .as_ref()
                    .is_some_and(|invocation| invocation.token == token && token.len() == 64)
            })
            .ok_or("activity writer unavailable")?;
        let shell = entry.process.ok_or("activity shell unavailable")?;
        if Instant::now() >= deadline
            || entry.pending_native
            || peer == shell
            || process::inspect(shell.pid) != Some(shell)
            || process::inspect(peer.pid) != Some(peer)
            || !process::descends_from(peer, shell)
        {
            return Err("activity launch provenance mismatch");
        }
        let invocation = entry
            .invocation
            .as_mut()
            .ok_or("activity invocation unavailable")?;
        if invocation.process.is_some() {
            return Err("activity launch already committed");
        }
        invocation.process = Some(peer);
        invocation.batch = batch;
        Ok(())
    }

    /// Records an unsupported inbox release without preventing its launch. A host
    /// preparing its own writer passes no PID; a helper must be the committed CLI.
    pub fn require_inbox_update(&self, token: &str, helper_pid: Option<u32>) -> bool {
        let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        let Some(invocation) = invocation_mut(&mut sessions, token) else {
            return false;
        };
        if !matches!(
            self.adapter(invocation.snapshot.provider).inbox(),
            InboxDelivery::HostSubmit { .. }
        ) || invocation.snapshot.exit.is_some()
            || helper_pid.is_some_and(|pid| {
                invocation.process.is_none_or(|process| {
                    process.pid != pid || process::inspect(pid) != Some(process)
                })
            })
        {
            return false;
        }
        invocation.inbox_update_required = true;
        invocation.snapshot.revision += 1;
        let snapshot = invocation.capture(Instant::now());
        drop(sessions);
        self.publish(snapshot);
        true
    }

    /// Commits the PID of `session`'s reserved launch `incarnation` once it started; a
    /// reservation already removed (its run ended first) stays removed.
    pub fn commit(&self, session: &str, incarnation: &str, pid: u32) -> bool {
        let process = process::inspect(pid);
        let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        let Some(entry) = sessions
            .get_mut(session)
            .filter(|entry| entry.pending_native && entry.incarnation == incarnation)
        else {
            return false;
        };
        entry.pending_native = false;
        entry.process = process;
        if let Some(invocation) = &mut entry.invocation {
            invocation.process = process;
        }
        true
    }

    /// Called at actual PTY creation, never at viewer attachment.
    pub fn register(&self, session: &str, pid: Option<u32>) -> Option<String> {
        let process = pid.and_then(process::inspect);
        let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(entry) = sessions
            .get_mut(session)
            .filter(|entry| entry.pending_native)
        {
            entry.pending_native = false;
            entry.process = process;
            if let Some(invocation) = &mut entry.invocation {
                invocation.process = process;
            }
            return Some(entry.incarnation.clone());
        }
        if !sessions.contains_key(session) && sessions.len() >= MAX_SESSIONS {
            return None;
        }
        let incarnation = uuid::Uuid::new_v4().to_string();
        sessions.insert(
            session.into(),
            Session {
                child: None,
                pending_native: false,
                incarnation: incarnation.clone(),
                process,
                invocation: None,
            },
        );
        Some(incarnation)
    }

    /// Attach the process-group identity captured by the runner, never reconstructed
    /// from a PID later. A late registration cannot attach to a replacement incarnation.
    pub(crate) fn attach_child(
        &self,
        session: &str,
        incarnation: &str,
        child: crate::runner::ChildHandle,
    ) {
        if let Some(entry) = self
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_mut(session)
            .filter(|entry| entry.incarnation == incarnation)
        {
            entry.child = Some(child);
        }
    }

    pub(super) fn engine_lost(
        &self,
        token: &str,
        peer: process::Process,
    ) -> Result<(), &'static str> {
        self.end_failed_engine(token, Some(peer))
    }

    fn end_failed_engine(
        &self,
        token: &str,
        peer: Option<process::Process>,
    ) -> Result<(), &'static str> {
        let (session, identity, home, provider, child, snapshot) = {
            let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            let (session, entry) = sessions
                .iter_mut()
                .find(|(_, entry)| {
                    entry
                        .invocation
                        .as_ref()
                        .is_some_and(|invocation| !token.is_empty() && invocation.token == token)
                })
                .ok_or("activity invocation unavailable")?;
            let child = entry.child.clone().ok_or("activity child unavailable")?;
            let invocation = entry
                .invocation
                .as_mut()
                .ok_or("activity invocation unavailable")?;
            let engine = invocation
                .engine
                .as_ref()
                .ok_or("activity engine unavailable")?;
            let root = invocation.process.ok_or("activity launch not committed")?;
            if !engine.end_on_loss
                || !process::retired(engine.process)
                || entry.process != Some(root)
                || peer.is_some_and(|peer| {
                    process::inspect(peer.pid).is_none_or(|current| !current.is(peer))
                        || !peer.is(engine.keeper)
                        || !process::descends_from(peer, root)
                })
            {
                return Err("activity engine loss is not its keeper's");
            }
            let home = engine.home.clone();
            invocation.engine_loss_pending = true;
            invocation.token.clear();
            invocation.snapshot.observed = false;
            invocation.snapshot.state = State::Crashed;
            invocation.snapshot.reason = Some(super::ENGINE_STOPPED.into());
            invocation.snapshot.revision += 1;
            (
                session.clone(),
                invocation.snapshot.identity.clone(),
                home,
                invocation.snapshot.provider,
                child,
                invocation.capture(Instant::now()),
            )
        };
        // Revocation/publication is short; forgetting inbox state can wait for an
        // in-flight submit fence and belongs with the blocking cleanup as well.
        self.publish(snapshot);
        // Reuse the per-operation blocking worker model of inbox-submit/inbox-recover
        // and native readiness observation. No pool exists in this transport-neutral
        // owner, and loss cleanup must not occupy the sole activity accept thread.
        let sessions = self.sessions.clone();
        let inbox = self.inbox.clone();
        let outlet = self.inbox_outlet();
        let sink = self.sink.clone();
        let adapter = self.adapter(provider);
        std::thread::Builder::new()
            .name("engine-cleanup".into())
            .spawn(move || {
                child.terminate();
                child.wait();
                if let Some(outlet) = outlet {
                    inbox.forget(&session, &*outlet);
                }
                if let Some(home) = home {
                    adapter.teardown_home(&home, true);
                }
                let snapshot = {
                    let mut sessions = sessions.lock().unwrap_or_else(|p| p.into_inner());
                    let Some(invocation) = sessions
                        .get_mut(&session)
                        .and_then(|entry| entry.invocation.as_mut())
                        .filter(|invocation| invocation.snapshot.identity == identity)
                    else {
                        return;
                    };
                    invocation.engine_loss_pending = false;
                    invocation.snapshot.exit.get_or_insert(Exit {
                        code: None,
                        signal: None,
                        requested: false,
                    });
                    invocation.snapshot.reason = Some(super::ENGINE_STOPPED.into());
                    invocation.snapshot.state = State::Crashed;
                    invocation.subagents.clear();
                    invocation.snapshot.revision += 1;
                    invocation.capture(Instant::now())
                };
                publish_activity(sink.as_deref(), snapshot);
            })
            .map(drop)
            .map_err(|_| "activity engine cleanup unavailable")
    }

    /// This owner's host lifetime: every snapshot's `host_boot_id`.
    pub fn host_boot_id(&self) -> &str {
        &self.boot
    }

    /// `session`'s current invocation snapshot, if it has one.
    pub fn snapshot(&self, session: &str) -> Option<Snapshot> {
        self.sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(session)
            .and_then(|entry| entry.invocation.as_ref())
            .map(|invocation| invocation.capture(Instant::now()))
    }

    /// Binds `writer`'s committed invocation to an engine serving on `endpoint`, a child
    /// of this process standing in for it (ended with it): for tests of what reaches a CLI
    /// through its engine, with this process as its CLI.
    #[cfg(any(test, feature = "test-support"))]
    pub fn bind_engine_for_test(
        &self,
        writer: &Writer,
        endpoint: PathBuf,
    ) -> Result<(), &'static str> {
        self.bind_engine_with_home_for_test(writer, endpoint, None)
    }

    /// [`Self::bind_engine_for_test`] for an engine that carries a captured `home`, as a
    /// production launch's does.
    #[cfg(any(test, feature = "test-support"))]
    pub fn bind_engine_with_home_for_test(
        &self,
        writer: &Writer,
        endpoint: PathBuf,
        home: Option<PathBuf>,
    ) -> Result<(), &'static str> {
        let process = invocation_mut(
            &mut self.sessions.lock().unwrap_or_else(|p| p.into_inner()),
            &writer.token,
        )
        .and_then(|invocation| invocation.process)
        .ok_or("activity launch not committed")?;
        // A host that serves no ingress (a test's) takes the endpoint's own directory
        // for its root.
        let mut paths = self.launch_paths.lock().unwrap_or_else(|p| p.into_inner());
        if paths.is_none() {
            *paths = Some((endpoint.with_file_name("activity.sock"), PathBuf::new()));
        }
        drop(paths);
        // Outlives any test that binds it; ended once its invocation is forgotten.
        let engine = std::process::Command::new("/bin/sleep")
            .arg("120")
            .spawn()
            .map_err(|_| "activity engine unavailable")?;
        self.bind_engine(&writer.token, process, engine.id(), endpoint, home)
    }

    /// Where a host that serves ingress takes an engine named `name`: its socket in the
    /// host's private root, which an engine's endpoint must be in.
    #[cfg(any(test, feature = "test-support"))]
    pub fn engine_socket_for_test(&self, name: &str) -> Option<PathBuf> {
        self.launch_root()
            .map(|root| root.join(format!("engine-{name}.sock")))
    }

    /// Capture the bounded native record for precisely this invocation and turn.
    pub fn session_output(
        &self,
        session: &str,
        identity: &Identity,
        turn_generation: u64,
    ) -> Option<crate::agent_cli::output::SessionRecord> {
        let capture = || {
            let sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            let invocation = sessions.get(session)?.invocation.as_ref()?;
            let snapshot = &invocation.snapshot;
            if &snapshot.identity != identity
                || snapshot.turn_generation != turn_generation
                || !snapshot.observed
            {
                return None;
            }
            Some((
                snapshot.provider,
                invocation.engine.as_ref()?.home.clone()?,
                invocation.provider_session.clone()?,
            ))
        };
        let selected = capture()?;
        let reader = self.adapter(selected.0).output().session_record?;
        let record = reader(&selected.1, &selected.2)?;
        // Rebinding or replacement during disk I/O cannot attach another thread's
        // record to the completion that requested this one.
        (capture().as_ref() == Some(&selected)).then_some(record)
    }

    /// Fold `report` as the hook process `peer_pid` of `writer`'s invocation
    /// would: the provider evidence path, for tests of what consumes it.
    #[cfg(any(test, feature = "test-support"))]
    pub fn report_for_test(
        &self,
        writer: &Writer,
        peer_pid: u32,
        report: Report,
    ) -> Result<Snapshot, &'static str> {
        let peer = process::inspect(peer_pid).ok_or("activity process unavailable")?;
        self.report(
            &writer.token,
            peer,
            report,
            Instant::now() + std::time::Duration::from_secs(5),
        )
    }

    #[cfg(test)]
    pub(super) fn evidence_at_for_test(&self, session: &str) -> Option<Instant> {
        self.sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(session)
            .and_then(|entry| entry.invocation.as_ref())
            .and_then(|invocation| invocation.evidence_at)
    }

    pub fn snapshots(&self) -> Vec<Snapshot> {
        self.sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .filter_map(|entry| {
                entry
                    .invocation
                    .as_ref()
                    .map(|invocation| invocation.capture(Instant::now()))
            })
            .collect()
    }

    /// The control plane cannot retarget a captured operation onto a newer
    /// invocation/turn. The observational socket never accepts this operation.
    pub fn mutate(&self, mutation: Mutation) -> Result<Snapshot, &'static str> {
        let opaque =
            |id: &str| !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control);
        if !opaque(&mutation.operation_id) {
            return Err("invalid activity operation");
        }
        let action_id = match &mutation.action {
            Action::AskOpened { id } | Action::AskClosed { id, .. } => Some(id.as_str()),
            _ => None,
        };
        if action_id.is_some_and(|id| !opaque(id)) {
            return Err("invalid activity ask");
        }
        let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        let invocation = sessions
            .get_mut(&mutation.session)
            .and_then(|entry| entry.invocation.as_mut())
            .ok_or("activity invocation unavailable")?;
        if invocation.snapshot.identity != mutation.identity || invocation.snapshot.exit.is_some() {
            return Err("retired activity invocation");
        }
        let process = invocation.process.ok_or("activity launch not committed")?;
        if process::inspect(process.pid) != Some(process) {
            return Err("activity process unavailable");
        }
        if invocation.operations.contains(&mutation.operation_id) {
            return Ok(invocation.capture(Instant::now()));
        }
        if mutation.turn_generation < invocation.snapshot.turn_generation
            && matches!(mutation.action, Action::AskClosed { .. })
        {
            // Superseded holds were retired with their turn. A late cleanup is
            // acknowledged without touching even a reused current ask ID.
            return Ok(invocation.capture(Instant::now()));
        }
        if invocation.snapshot.turn_generation != mutation.turn_generation {
            return Err("superseded activity turn");
        }
        // Refusals must not change the reason or freshen activity metadata.
        match &mutation.action {
            Action::AskOpened { id } => {
                if invocation.closed_asks.contains(id) {
                    return Err("activity ask already closed");
                }
                if !invocation.asks.contains_key(id)
                    && invocation.asks.len() + invocation.closed_asks.len() >= 128
                {
                    return Err("activity ask limit");
                }
            }
            Action::AskClosed { id, .. } => {
                let retained = invocation.closed_asks.len() + invocation.asks.len()
                    - usize::from(invocation.asks.contains_key(id));
                if retained >= 128 && !invocation.closed_asks.contains(id) {
                    return Err("activity ask retirement limit");
                }
            }
            _ => {}
        }
        // Closing an ask only owns the display when it restores that ask's
        // state. A superseding block (or other reason) must survive cleanup.
        if !matches!(
            mutation.action,
            Action::AskClosed { .. } | Action::Progress { .. }
        ) {
            invocation.snapshot.reason = None;
        }
        let state = &mut invocation.arbitration;
        match mutation.action {
            Action::Progress { blocked } => {
                // Keep the existing wire state; reason distinguishes an agent's
                // self-report from a permission gate without latching hooks.
                state.declare(false);
                if !blocked && state.open_asks > 0 {
                    // Nonblocked progress relinquishes even an earlier block's
                    // ownership. The surviving ask still owns its restoration.
                    state.superseded = false;
                }
                let reason = if blocked {
                    Some("blocked".into())
                } else if invocation.tools.is_empty() {
                    Some("progress".into())
                } else {
                    None
                };
                let continuation = if blocked {
                    State::NeedsPermission
                } else {
                    State::Working
                };
                invocation.project_activity(continuation, reason);
            }
            Action::Done { outcome } => {
                let display = match outcome {
                    Outcome::Success => State::Done,
                    Outcome::Failed => State::Errored,
                    Outcome::NeedsAttention => State::Done,
                };
                state.declare(display == State::Errored);
                invocation.snapshot.reason =
                    (outcome == Outcome::NeedsAttention).then(|| "needs-attention".into());
                invocation.snapshot.state = display;
            }
            Action::AskOpened { id } => {
                if let std::collections::hash_map::Entry::Vacant(entry) = invocation.asks.entry(id)
                {
                    entry.insert(mutation.turn_generation);
                    state.open_ask();
                    invocation.project_activity(State::NeedsPermission, None);
                }
            }
            Action::AskClosed { id, resolved } => {
                let generation = invocation.asks.get(&id).copied();
                if mutation.turn_generation == invocation.snapshot.turn_generation {
                    invocation.closed_asks.insert(id.clone());
                }
                invocation.asks.remove(&id);
                if generation == Some(invocation.snapshot.turn_generation)
                    && state.close_ask()
                    && resolved
                {
                    invocation.project_activity(State::Working, None);
                }
            }
        }
        invocation.snapshot.asks = invocation
            .asks
            .iter()
            .filter(|(_, generation)| **generation == invocation.snapshot.turn_generation)
            .map(|(id, _)| id.clone())
            .collect();
        invocation.snapshot.asks.sort();
        invocation.evidence_at = Some(Instant::now());
        invocation.snapshot.revision += 1;
        if invocation.operations.len() == RECENT_REPORTS {
            invocation.operations.pop_front();
        }
        invocation.operations.push_back(mutation.operation_id);
        let snapshot = invocation.capture(Instant::now());
        drop(sessions);
        self.publish(snapshot.clone());
        Ok(snapshot)
    }

    /// Both capability and exact process incarnation are validated while folding.
    /// The caller's deadline covers parsing, lock acquisition and this commit.
    pub(super) fn report(
        &self,
        token: &str,
        peer: process::Process,
        report: Report,
        deadline: Instant,
    ) -> Result<Snapshot, &'static str> {
        if !report.valid() || token.len() != 64 {
            return Err("invalid activity report");
        }
        let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        let invocation = invocation_mut(&mut sessions, token).ok_or("retired activity writer")?;
        let root = invocation.process.ok_or("activity launch not committed")?;
        if invocation.snapshot.exit.is_some()
            || process::inspect(root.pid) != Some(root)
            || process::inspect(peer.pid) != Some(peer)
            || !process::descends_from(peer, root)
        {
            return Err("activity process unavailable");
        }
        if report.provider != invocation.snapshot.provider {
            return Err("activity provider mismatch");
        }
        if Instant::now() >= deadline {
            return Err("activity deadline expired");
        }
        if invocation.reports.contains(&report.report_id) {
            return Ok(invocation.capture(Instant::now()));
        }
        let counts_subagents =
            self.adapter(report.provider).session_policy().subagents == Subagents::Reported;
        let own_session = invocation.provider_session.is_some()
            && invocation.provider_session == report.provider_session
            && !report
                .provider_session
                .as_ref()
                .is_some_and(|provider| invocation.retired_provider_sessions.contains(provider));
        if matches!(
            report.event,
            Event::SubagentStarted | Event::SubagentFinished
        ) {
            let id = report.agent.as_deref().ok_or("activity subagent missing")?;
            if !own_session {
                return Err("activity provider session mismatch");
            }
            let now = Instant::now();
            if counts_subagents {
                if report.event == Event::SubagentStarted {
                    invocation.subagents.start(id, now);
                } else {
                    invocation.subagents.finish(id);
                }
            }
            invocation.evidence_at = Some(now);
            // A subagent finished: whatever it asked (a prompt names no subagent) was
            // answered, and its parent moves on. Neither changes the main agent's own
            // state.
            let finished = report.event == Event::SubagentFinished;
            let settled = finished && invocation.answer_wait(None) && invocation.settle_answered();
            let cleared_block = finished && invocation.clear_reported_block();
            // Even unchanged-state provider evidence must reach consumers'
            // activity clocks. A replay still exits above before this revision.
            if !settled && !cleared_block {
                invocation.snapshot.revision += 1;
            }
            if invocation.reports.len() == RECENT_REPORTS {
                invocation.reports.pop_front();
            }
            invocation.reports.push_back(report.report_id);
            let snapshot = invocation.capture(now);
            drop(sessions);
            self.publish(snapshot.clone());
            return Ok(snapshot);
        }
        if report.scope != Scope::Main {
            // A subagent's own tool or prompt: it runs, whatever its parent's turn is doing.
            let subagent_open = match report.agent.as_deref() {
                Some(id)
                    if counts_subagents
                        && own_session
                        && report.scope == Scope::Child
                        && matches!(
                            report.event,
                            Event::ToolStarted | Event::ToolFinished | Event::Waiting
                        ) =>
                {
                    invocation.subagents.heard(id, Instant::now())
                }
                _ => false,
            };
            // A subagent's tools run inside the main turn: once one moves, a
            // permission prompt it raised (which names no subagent) was
            // answered. Nothing else of a child is shown.
            if report.scope == Scope::Child
                && matches!(report.event, Event::ToolStarted | Event::ToolFinished)
                && invocation.provider_session.is_some()
                && invocation.provider_session == report.provider_session
                && invocation.answer_wait(report.tool_name.as_deref())
            {
                invocation.evidence_at = Some(Instant::now());
                let cleared_block = invocation.clear_reported_block();
                let settled = invocation.settle_answered();
                if !settled && !cleared_block {
                    invocation.snapshot.revision += 1;
                }
                if invocation.reports.len() == RECENT_REPORTS {
                    invocation.reports.pop_front();
                }
                invocation.reports.push_back(report.report_id);
                let snapshot = invocation.capture(Instant::now());
                drop(sessions);
                self.publish(snapshot.clone());
                return Ok(snapshot);
            }
            // Child tools can prove liveness of an already active parent, but
            // never own its lifecycle, holds, tools, or turn binding. Keep the
            // existing actual permission-answer operation above separate.
            let matching_turn = invocation.turn == report.turn
                && !report
                    .turn
                    .as_ref()
                    .is_some_and(|turn| invocation.old_turns.contains(turn));
            let active_parent = report.scope == Scope::Child
                && matches!(report.event, Event::ToolStarted | Event::ToolFinished)
                && own_session
                && matching_turn
                && invocation.snapshot.observed
                && invocation.snapshot.exit.is_none()
                && matches!(invocation.snapshot.state, State::Thinking | State::Working);
            if active_parent || subagent_open {
                invocation.evidence_at = Some(Instant::now());
                invocation.snapshot.revision += 1;
                if invocation.reports.len() == RECENT_REPORTS {
                    invocation.reports.pop_front();
                }
                invocation.reports.push_back(report.report_id);
                let snapshot = invocation.capture(Instant::now());
                drop(sessions);
                self.publish(snapshot.clone());
                return Ok(snapshot);
            }
            return Err("activity main scope required");
        }
        // These reports cannot start/rebind a turn, so their capacity is
        // checked before any turn, wait, or freshness state can change.
        if let Some(tool) = &report.tool {
            if report.event == Event::ToolStarted
                && invocation.tools.len() >= RECENT_REPORTS
                && !invocation.tools.contains(tool)
            {
                return Err("activity tool limit");
            }
            if report.event == Event::Waiting
                && invocation.waits.len() >= RECENT_REPORTS
                && !invocation.waits.contains_key(tool)
            {
                return Err("activity wait limit");
            }
        }
        // Some CLIs emit no separate session start when first prompting or resuming.
        // Both start events bind context; Submitted still uses the one turn/state path below.
        let policy = self.adapter(report.provider).session_policy();
        let starts_context = report.event == Event::Started
            || (report.event == Event::Submitted && policy.bind_on_submit);
        let mut provider_rebound = false;
        if invocation.provider_session.is_none() {
            if !starts_context || report.provider_session.is_none() {
                return Err("activity start required");
            }
            invocation.provider_session = report.provider_session.clone();
        } else if invocation.provider_session != report.provider_session {
            let next = report
                .provider_session
                .as_ref()
                .ok_or("activity provider session missing")?;
            let returning = invocation.retired_provider_sessions.contains(next);
            if !starts_context
                || !policy.rebind_on_start
                || (returning && !policy.rebind_to_retired)
                || (!returning && invocation.retired_provider_sessions.len() >= RECENT_REPORTS)
            {
                return Err("activity provider session mismatch");
            }
            invocation.retired_provider_sessions.remove(next);
            // A CLI that rebinds (Claude's /clear) starts a new main provider context
            // inside the same committed process. A main session/turn start is required;
            // IDs alone never rebind.
            if let Some(prior) = invocation.provider_session.replace(next.clone()) {
                invocation.retired_provider_sessions.insert(prior);
            }
            invocation.turn = None;
            invocation.old_turns.clear();
            invocation.arbitration = super::arbiter::DotState::default();
            provider_rebound = true;
        }
        if let Some(turn) = &report.turn {
            if invocation.old_turns.contains(turn) {
                return Err("superseded activity turn");
            }
            if invocation.turn.as_ref() != Some(turn) {
                if !matches!(report.event, Event::Started | Event::Submitted) {
                    return Err("activity turn start required");
                }
                if let Some(old) = invocation.turn.replace(turn.clone()) {
                    if invocation.old_turns.len() == RECENT_REPORTS {
                        invocation.old_turns.pop_front();
                    }
                    invocation.old_turns.push_back(old);
                }
            }
        }
        // A new context, or the session's end: none of its subagents runs on.
        if provider_rebound || report.event == Event::Ended {
            invocation.subagents.clear();
        }
        // At its own turn's end the CLI may list the subagents still running, by the ids
        // its subagent hooks carry; one it does not list stopped unseen.
        if let (Event::Stopped, Some(running)) = (report.event, &report.subagents) {
            invocation.subagents.reconcile(running);
        }
        if report.event == Event::Stopped {
            invocation.arbitration.open_asks = 0;
            invocation.asks.clear();
            invocation.snapshot.asks.clear();
        }
        if provider_rebound || matches!(report.event, Event::Submitted | Event::Ended) {
            invocation.snapshot.turn_generation += 1;
            invocation.arbitration.open_asks = 0;
            invocation.asks.clear();
            invocation.snapshot.asks.clear();
            invocation.operations.clear();
            invocation.closed_asks.clear();
        }
        if provider_rebound
            || matches!(
                report.event,
                Event::Submitted
                    | Event::Stopped
                    | Event::Failed
                    | Event::Interrupted
                    | Event::Ended
            )
        {
            invocation.tools.clear();
            invocation.waits.clear();
            invocation.uncorrelated_wait = None;
            invocation.wait_tool = None;
        }
        // A permission prompt names no tool use. The prompted tool's own
        // progress (or, for a prompt naming no tool, any) answered it; a new
        // prompt or the turn's end does too.
        if matches!(
            report.event,
            Event::ToolStarted | Event::ToolFinished | Event::Compacting
        ) {
            invocation.answer_wait(report.tool_name.as_deref());
        }
        match (&report.event, &report.tool) {
            (Event::ToolStarted, Some(tool)) => {
                invocation.tools.insert(tool.clone());
            }
            (Event::Waiting, Some(tool)) => {
                invocation
                    .waits
                    .insert(tool.clone(), WaitKind::from_report(&report));
            }
            (Event::Waiting, None) => {
                // A notification for the same prompt names no tool: keep the name.
                if report.tool_name.is_some() || invocation.uncorrelated_wait.is_none() {
                    invocation.wait_tool = report.tool_name.clone();
                }
                invocation.uncorrelated_wait = Some(WaitKind::from_report(&report));
            }
            (Event::ToolFinished, Some(tool)) => {
                invocation.tools.remove(tool);
                invocation.waits.remove(tool);
            }
            _ => {}
        }
        let open_turn = matches!(
            invocation.snapshot.state,
            State::Thinking | State::Working | State::NeedsPermission
        );
        if report.event == Event::Prompting {
            if !open_turn {
                // Already settled: the idle reminder adds nothing to show.
                // It is still new provider activity, so publish its revision.
                invocation.evidence_at = Some(Instant::now());
                invocation.snapshot.revision += 1;
                invocation.reports.push_back(report.report_id);
                if invocation.reports.len() > RECENT_REPORTS {
                    invocation.reports.pop_front();
                }
                let snapshot = invocation.capture(Instant::now());
                drop(sessions);
                self.publish(snapshot.clone());
                return Ok(snapshot);
            }
            invocation.tools.clear();
            invocation.waits.clear();
            invocation.uncorrelated_wait = None;
            invocation.wait_tool = None;
        }
        // This is foreground provider evidence, not workflow success.
        let (hook, mut observed_state) = match report.event {
            Event::Started => ("SessionStart", State::Idle),
            Event::Submitted => ("UserPromptSubmit", State::Thinking),
            Event::ToolStarted => ("PreToolUse", State::Working),
            Event::ToolFinished => ("PostToolUse", State::Thinking),
            Event::Waiting => ("PermissionRequest", State::NeedsPermission),
            Event::Stopped => ("Stop", State::Done),
            Event::Prompting => ("Stop", State::Idle),
            Event::Failed => ("StopFailure", State::Errored),
            Event::Interrupted => ("Interrupt", State::Idle),
            Event::Ended => ("SessionEnd", State::Idle),
            Event::Compacting => ("PreCompact", State::Working),
            // A subagent's start or finish returned above.
            Event::SubagentStarted | Event::SubagentFinished => {
                return Err("activity main scope required")
            }
        };
        if matches!(
            report.event,
            Event::ToolStarted | Event::ToolFinished | Event::Compacting
        ) && !invocation.tools.is_empty()
        {
            observed_state = State::Working;
        }
        let ask_superseded = invocation.arbitration.superseded;
        match invocation.arbitration.admit_hook(hook) {
            super::arbiter::HookAdmission::Admit => {
                let cleared_block = invocation.snapshot.reason.as_deref() == Some("blocked");
                if cleared_block {
                    invocation.snapshot.reason = None;
                }
                if invocation.arbitration.open_asks > 0 {
                    // The hook did not replace the visible ask. Keep its prior
                    // restore ownership so a sibling tool cannot strand it.
                    invocation.arbitration.superseded = ask_superseded && !cleared_block;
                }
                invocation.project_activity(observed_state, report.reason.clone());
            }
            super::arbiter::HookAdmission::Suppress => {}
        }
        invocation.snapshot.observed = true;
        invocation.evidence_at = Some(Instant::now());
        invocation.snapshot.revision += 1;
        if invocation.reports.len() == RECENT_REPORTS {
            invocation.reports.pop_front();
        }
        invocation.reports.push_back(report.report_id);
        let snapshot = invocation.capture(Instant::now());
        drop(sessions);
        self.publish(snapshot.clone());
        Ok(snapshot)
    }

    pub fn exit(&self, session: &str, incarnation: &str, exit: Exit) {
        let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        let Some(entry) = sessions
            .get_mut(session)
            .filter(|entry| entry.incarnation == incarnation)
        else {
            return;
        };
        if let Some(invocation) = &mut entry.invocation {
            invocation.token.clear();
            let engine_stopped =
                invocation.snapshot.reason.as_deref() == Some(super::ENGINE_STOPPED);
            invocation.snapshot.state = if engine_stopped
                || (!exit.requested
                    && (exit.code.is_some_and(|code| code != 0)
                        || exit
                            .signal
                            .as_ref()
                            .is_some_and(|signal| !signal.is_empty())))
            {
                State::Crashed
            } else {
                State::Idle
            };
            invocation.snapshot.exit = Some(exit);
            if !engine_stopped {
                invocation.snapshot.reason = None;
            }
            invocation.subagents.clear();
            invocation.snapshot.revision += 1;
            let snapshot = invocation.capture(Instant::now());
            let pending = invocation.engine_loss_pending;
            drop(sessions);
            if !pending {
                self.publish(snapshot);
            }
        } else {
            drop(sessions);
        }
        // Its agent is gone with it: no waiter, and no notice held for it.
        self.forget_inbox(session);
    }

    /// `session`'s agent is gone: its waiter ends, and no notice is held for it.
    fn forget_inbox(&self, session: &str) {
        if let Some(outlet) = self.inbox_outlet() {
            self.inbox.forget(session, &*outlet);
        }
    }

    /// Tells `session`'s desktop whom its new mail reaches, whatever it was told before
    /// (the session is another desktop's now).
    pub fn inbox_report_again(&self, session: &str) {
        if let Some(outlet) = self.inbox_outlet() {
            self.inbox.report_again(&*outlet, session);
        }
    }

    /// A shell command-end marker is useful only after the committed invocation
    /// is positively gone. A live CLI printing an OSC marker cannot retire it.
    pub(crate) fn shell_command_end(&self, session: &str, incarnation: &str, code: Option<i32>) {
        let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        let Some(entry) = sessions
            .get_mut(session)
            .filter(|entry| entry.incarnation == incarnation)
        else {
            return;
        };
        let Some(invocation) = entry.invocation.as_mut() else {
            return;
        };
        let Some(process) = invocation.process else {
            return;
        };
        if entry.process == Some(process)
            || invocation.snapshot.exit.is_some()
            || !process::retired(process)
        {
            return;
        }
        invocation.token.clear();
        let exit = Exit {
            code: code.and_then(|code| code.try_into().ok()),
            signal: None,
            requested: false,
        };
        invocation.snapshot.state = if exit.code.is_some_and(|code| code != 0) {
            State::Crashed
        } else {
            State::Idle
        };
        invocation.snapshot.exit = Some(exit);
        invocation.snapshot.reason = None;
        invocation.snapshot.asks.clear();
        invocation.snapshot.revision += 1;
        let snapshot = invocation.capture(Instant::now());
        drop(sessions);
        // Its agent is gone: no waiter, and no notice held for it.
        self.forget_inbox(session);
        self.publish(snapshot);
    }

    /// Actual PTY output, including when no viewer display is attached. Output
    /// updates freshness but never changes lifecycle, permission, or ask state.
    pub(crate) fn output(&self, session: &str, incarnation: &str) {
        self.output_at(session, incarnation, Instant::now());
    }

    fn output_at(&self, session: &str, incarnation: &str, now: Instant) {
        let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(invocation) = sessions
            .get_mut(session)
            .filter(|entry| entry.incarnation == incarnation)
            .and_then(|entry| entry.invocation.as_mut())
            .filter(|invocation| {
                invocation.snapshot.exit.is_none()
                    && !invocation.engine_loss_pending
                    && invocation.process.is_some()
            })
        {
            invocation.output_at = Some(now);
            if invocation
                .output_published_at
                .is_some_and(|at| now.saturating_duration_since(at) < OUTPUT_ACTIVITY_INTERVAL)
            {
                return;
            }
            invocation.output_published_at = Some(now);
            invocation.snapshot.revision += 1;
            let snapshot = invocation.capture(now);
            drop(sessions);
            self.publish(snapshot);
        }
    }

    /// Retire a CLI started inside a shell when its process is gone, even
    /// without a shell end marker. Silence alone never finishes a turn: the
    /// viewer may label stale activity Stalled while its lifecycle stays open.
    pub fn decay(&self, now: Instant) {
        // The host also detects loss if the keeper's bounded notification was lost.
        // This internal monitor uses the same bound kernel identity and captured child.
        let lost: Vec<String> = self
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .filter_map(|entry| entry.invocation.as_ref())
            .filter(|invocation| {
                invocation.snapshot.exit.is_none()
                    && !invocation.token.is_empty()
                    && invocation.engine.as_ref().is_some_and(|engine| {
                        engine.end_on_loss && process::retired(engine.process)
                    })
            })
            .map(|invocation| invocation.token.clone())
            .collect();
        for token in lost {
            let _ = self.end_failed_engine(&token, None);
        }
        self.sweep_inbox(now);
        let mut settled = Vec::new();
        let mut expired = Vec::new();
        {
            let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            for entry in sessions.values_mut() {
                let shell = entry.process;
                let Some(invocation) = entry.invocation.as_mut() else {
                    continue;
                };
                if invocation.snapshot.exit.is_some() || invocation.engine_loss_pending {
                    continue;
                }
                // The PTY's own process reports its exit itself.
                if let Some(process) = invocation.process.filter(|process| Some(*process) != shell)
                {
                    if process::retired(process) {
                        invocation.token.clear();
                        invocation.snapshot.state = State::Idle;
                        invocation.snapshot.exit = Some(Exit {
                            code: None,
                            signal: None,
                            requested: false,
                        });
                        invocation.snapshot.reason = None;
                        invocation.snapshot.asks.clear();
                        invocation.subagents.clear();
                        invocation.snapshot.revision += 1;
                        settled.push(invocation.capture(Instant::now()));
                        continue;
                    }
                }
                if invocation.subagents.expire(now) {
                    invocation.snapshot.revision += 1;
                    expired.push(invocation.capture(now));
                }
            }
        }
        for snapshot in expired {
            self.publish(snapshot);
        }
        for snapshot in settled {
            // Its agent is gone: no waiter, and no notice held for it.
            self.forget_inbox(&snapshot.session);
            self.publish(snapshot);
        }
    }

    pub fn remove(&self, session: &str, incarnation: &str) {
        let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        if sessions
            .get(session)
            .is_some_and(|entry| entry.incarnation == incarnation)
        {
            sessions.remove(session);
            drop(sessions);
            self.forget_inbox(session);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn without_age(mut snapshot: Snapshot) -> Snapshot {
        snapshot.activity_age_ms = None;
        snapshot
    }

    #[test]
    fn shell_invocation_claim_is_once_only_and_relaunch_changes_only_invocation() {
        let owner = ActivityOwner::new();
        let incarnation = owner.register("shell", Some(std::process::id())).unwrap();
        let first = owner
            .prepare_shell("shell", AgentCli::Claude, "unused")
            .unwrap();
        let shell = process::inspect(std::process::id()).unwrap();
        assert!(owner
            .bind_shell(
                &first.token,
                shell,
                Instant::now() + Duration::from_secs(1),
                false
            )
            .is_err());
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("5")
            .spawn()
            .unwrap();
        let peer = process::inspect(child.id()).unwrap();
        owner
            .bind_shell(
                &first.token,
                peer,
                Instant::now() + Duration::from_secs(1),
                false,
            )
            .unwrap();
        let duplicate = owner.bind_shell(
            &first.token,
            peer,
            Instant::now() + Duration::from_secs(1),
            false,
        );
        let replacement = owner.prepare_shell("shell", AgentCli::Claude, "unused");
        owner.shell_command_end("shell", &incarnation, Some(1));
        let live_snapshot = owner.snapshots();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(duplicate.is_err());
        assert!(replacement.is_err());
        assert!(live_snapshot[0].exit.is_none());
        owner.shell_command_end("shell", &incarnation, Some(0));
        assert_eq!(owner.snapshots()[0].state, State::Idle);
        assert_eq!(owner.snapshots()[0].exit.as_ref().unwrap().code, Some(0));
        let second = owner
            .prepare_shell("shell", AgentCli::Claude, "unused")
            .unwrap();
        assert_eq!(
            first.identity.session_incarnation,
            second.identity.session_incarnation
        );
        assert_eq!(first.identity.host_boot_id, second.identity.host_boot_id);
        assert_ne!(first.identity.invocation_id, second.identity.invocation_id);
        assert_ne!(first.token, second.token);
        assert!(owner
            .bind_shell(
                &first.token,
                shell,
                Instant::now() + Duration::from_secs(1),
                false
            )
            .is_err());
    }

    #[test]
    fn a_native_launch_gets_a_private_directory_that_goes_with_it() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let owner = ActivityOwner::new();
        let writer = owner
            .prepare_native("session", AgentCli::Claude, "unused")
            .unwrap();
        // Without an ingress there is nowhere to put one, which the caller sees.
        assert!(owner.launch_directory(&writer.token).is_err());
        owner.configure_ingress(
            root.path().join("activity.sock"),
            root.path().join("helper"),
        );
        let directory = owner.launch_directory(&writer.token).unwrap();
        assert!(directory.starts_with(root.path()));
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o077,
            0
        );
        assert!(owner.launch_directory("unknown-token").is_err());
    }

    #[test]
    fn ask_cleanup_before_delayed_open_retires_id_without_repainting() {
        let owner = ActivityOwner::new();
        let writer = owner
            .prepare_native("session", AgentCli::Claude, "unused")
            .unwrap();
        owner.register("session", Some(std::process::id()));
        let mutate = |operation: &str, action| {
            owner.mutate(Mutation {
                session: "session".into(),
                identity: writer.identity.clone(),
                turn_generation: 0,
                operation_id: operation.into(),
                action,
            })
        };
        let closed = mutate(
            "close-first",
            Action::AskClosed {
                id: "cancelled".into(),
                resolved: false,
            },
        )
        .unwrap();
        assert!(closed.asks.is_empty());
        assert_eq!(closed.state, State::Idle);
        assert!(mutate(
            "late-open",
            Action::AskOpened {
                id: "cancelled".into()
            }
        )
        .is_err());
        assert!(mutate(
            "close-again",
            Action::AskClosed {
                id: "cancelled".into(),
                resolved: false
            }
        )
        .is_ok());
        mutate(
            "open",
            Action::AskOpened {
                id: "answered".into(),
            },
        )
        .unwrap();
        let settled = mutate(
            "answer",
            Action::AskClosed {
                id: "answered".into(),
                resolved: true,
            },
        )
        .unwrap();
        assert!(settled.asks.is_empty());
        assert_eq!(settled.state, State::Working);
        assert!(mutate(
            "duplicate-late-open",
            Action::AskOpened {
                id: "answered".into()
            }
        )
        .is_err());
        assert_eq!(owner.snapshots()[0].state, State::Working);
    }

    #[test]
    fn ask_retirement_capacity_refusal_is_atomic_and_existing_hold_can_close() {
        let owner = ActivityOwner::new();
        let writer = owner
            .prepare_native("session", AgentCli::Claude, "unused")
            .unwrap();
        owner.register("session", Some(std::process::id()));
        let mutate = |action| {
            owner.mutate(Mutation {
                session: "session".into(),
                identity: writer.identity.clone(),
                turn_generation: 0,
                operation_id: uuid::Uuid::new_v4().to_string(),
                action,
            })
        };
        mutate(Action::AskOpened { id: "held".into() }).unwrap();
        for index in 0..127 {
            mutate(Action::AskClosed {
                id: format!("cancelled-{index}"),
                resolved: false,
            })
            .unwrap();
        }
        let before = owner.snapshots();
        assert!(mutate(Action::AskClosed {
            id: "overflow".into(),
            resolved: false
        })
        .is_err());
        assert_eq!(
            owner
                .snapshots()
                .into_iter()
                .map(without_age)
                .collect::<Vec<_>>(),
            before.into_iter().map(without_age).collect::<Vec<_>>()
        );
        let settled = mutate(Action::AskClosed {
            id: "held".into(),
            resolved: true,
        })
        .unwrap();
        assert!(settled.asks.is_empty());
        assert_eq!(settled.state, State::Working);
        assert!(mutate(Action::AskOpened { id: "held".into() }).is_err());
    }

    #[test]
    fn main_context_start_rebinds_without_admitting_old_context_or_old_ask_cleanup() {
        let owner = ActivityOwner::new();
        let writer = owner
            .prepare_native("session", AgentCli::Claude, "unused")
            .unwrap();
        owner.register("session", Some(std::process::id()));
        let report = |event, provider_session: &str| {
            owner.report(
                &writer.token,
                process::inspect(std::process::id()).unwrap(),
                Report {
                    provider: AgentCli::Claude,
                    event,
                    scope: Scope::Main,
                    report_id: uuid::Uuid::new_v4().to_string(),
                    provider_session: Some(provider_session.into()),
                    turn: None,
                    tool: None,
                    tool_name: None,
                    reason: None,
                    agent: None,
                    subagents: None,
                },
                Instant::now() + Duration::from_millis(250),
            )
        };
        let mutate = |generation, action| {
            owner.mutate(Mutation {
                session: "session".into(),
                identity: writer.identity.clone(),
                turn_generation: generation,
                operation_id: uuid::Uuid::new_v4().to_string(),
                action,
            })
        };
        report(Event::Started, "initial").unwrap();
        mutate(
            0,
            Action::AskOpened {
                id: "reused".into(),
            },
        )
        .unwrap();
        mutate(
            0,
            Action::Done {
                outcome: Outcome::Failed,
            },
        )
        .unwrap();
        assert!(report(Event::Stopped, "cleared").is_err());
        let cleared = report(Event::Started, "cleared").unwrap();
        assert_eq!(cleared.identity, writer.identity);
        assert_eq!(cleared.turn_generation, 1);
        assert_eq!(cleared.state, State::Idle);
        assert!(cleared.asks.is_empty());
        assert!(report(Event::Started, "initial").is_err());
        assert!(report(Event::Stopped, "initial").is_err());
        mutate(
            1,
            Action::AskOpened {
                id: "reused".into(),
            },
        )
        .unwrap();
        let before = owner.snapshots();
        mutate(
            0,
            Action::AskClosed {
                id: "reused".into(),
                resolved: true,
            },
        )
        .unwrap();
        assert_eq!(
            owner
                .snapshots()
                .into_iter()
                .map(without_age)
                .collect::<Vec<_>>(),
            before.into_iter().map(without_age).collect::<Vec<_>>()
        );
        assert!(mutate(0, Action::AskOpened { id: "old".into() }).is_err());
        assert_eq!(
            owner.sessions.lock().unwrap()["session"]
                .invocation
                .as_ref()
                .unwrap()
                .asks
                .len(),
            1
        );
    }

    #[test]
    fn previous_turn_ask_does_not_hold_or_repaint_new_turn() {
        let owner = ActivityOwner::new();
        let writer = owner
            .prepare_native("session", AgentCli::Claude, "unused")
            .unwrap();
        owner.register("session", Some(std::process::id()));
        let action = |generation, operation: &str, action| Mutation {
            session: "session".into(),
            identity: writer.identity.clone(),
            turn_generation: generation,
            operation_id: operation.into(),
            action,
        };
        owner
            .mutate(action(0, "open", Action::AskOpened { id: "ask".into() }))
            .unwrap();
        let peer = process::inspect(std::process::id()).unwrap();
        let report = |event, id: &str| Report {
            provider: AgentCli::Claude,
            event,
            scope: Scope::Main,
            report_id: id.into(),
            provider_session: Some("provider".into()),
            turn: None,
            tool: None,
            tool_name: None,
            reason: None,
            agent: None,
            subagents: None,
        };
        owner
            .report(
                &writer.token,
                peer,
                report(Event::Started, "start"),
                Instant::now() + Duration::from_millis(250),
            )
            .unwrap();
        let submitted = owner
            .report(
                &writer.token,
                peer,
                report(Event::Submitted, "submit"),
                Instant::now() + Duration::from_millis(250),
            )
            .unwrap();
        assert_eq!(submitted.state, State::Thinking);
        assert!(submitted.asks.is_empty());
        let closed = owner
            .mutate(action(
                0,
                "closed",
                Action::AskClosed {
                    id: "ask".into(),
                    resolved: true,
                },
            ))
            .unwrap();
        assert_eq!(closed.state, State::Thinking);
        assert!(closed.asks.is_empty());
        owner
            .mutate(action(
                1,
                "another",
                Action::AskOpened { id: "ask2".into() },
            ))
            .unwrap();
        let ended = owner
            .report(
                &writer.token,
                peer,
                report(Event::Ended, "end"),
                Instant::now() + Duration::from_millis(250),
            )
            .unwrap();
        assert_eq!(ended.state, State::Idle);
        assert!(ended.asks.is_empty());
    }

    #[test]
    fn sibling_tools_preserve_parallel_asks_and_a_failed_finish_latches() {
        let owner = ActivityOwner::new();
        let writer = owner
            .prepare_native("session", AgentCli::Claude, "unused")
            .unwrap();
        owner.register("session", Some(std::process::id()));
        let peer = process::inspect(std::process::id()).unwrap();
        let report = |event, id: &str| {
            owner
                .report(
                    &writer.token,
                    peer,
                    Report {
                        provider: AgentCli::Claude,
                        event,
                        scope: Scope::Main,
                        report_id: id.into(),
                        provider_session: Some("provider".into()),
                        turn: None,
                        tool: None,
                        tool_name: None,
                        reason: None,
                        agent: None,
                        subagents: None,
                    },
                    Instant::now() + Duration::from_millis(250),
                )
                .unwrap()
        };
        let mutate = |action| {
            owner
                .mutate(Mutation {
                    session: "session".into(),
                    identity: writer.identity.clone(),
                    turn_generation: 0,
                    operation_id: uuid::Uuid::new_v4().to_string(),
                    action,
                })
                .unwrap()
        };
        report(Event::Started, "start");
        mutate(Action::AskOpened { id: "first".into() });
        mutate(Action::AskOpened {
            id: "second".into(),
        });
        assert_eq!(
            report(Event::ToolFinished, "parallel-tool").state,
            State::NeedsPermission
        );
        assert_eq!(
            mutate(Action::AskClosed {
                id: "first".into(),
                resolved: true
            })
            .state,
            State::NeedsPermission
        );
        assert_eq!(
            mutate(Action::AskClosed {
                id: "second".into(),
                resolved: true
            })
            .state,
            State::Working
        );
        assert_eq!(
            mutate(Action::Done {
                outcome: Outcome::NeedsAttention,
            })
            .state,
            State::Done
        );
        assert_eq!(report(Event::Stopped, "trailing-stop").state, State::Done);
        report(Event::Submitted, "new-turn");
        assert!(owner
            .mutate(Mutation {
                session: "session".into(),
                identity: writer.identity,
                turn_generation: 0,
                operation_id: "late-done".into(),
                action: Action::Done {
                    outcome: Outcome::Success,
                }
            })
            .is_err());
        assert_eq!(owner.snapshots()[0].state, State::Thinking);
    }

    #[test]
    fn parallel_tool_completion_cannot_clear_another_tools_wait() {
        let owner = ActivityOwner::new();
        let writer = owner
            .prepare_native("session", AgentCli::Claude, "unused")
            .unwrap();
        owner.register("session", Some(std::process::id()));
        let peer = process::inspect(std::process::id()).unwrap();
        let report = |event, tool: Option<&str>| {
            owner
                .report(
                    &writer.token,
                    peer,
                    Report {
                        provider: AgentCli::Claude,
                        event,
                        scope: Scope::Main,
                        report_id: uuid::Uuid::new_v4().to_string(),
                        provider_session: Some("provider".into()),
                        turn: None,
                        tool: tool.map(str::to_owned),
                        tool_name: None,
                        reason: None,
                        agent: None,
                        subagents: None,
                    },
                    Instant::now() + Duration::from_millis(250),
                )
                .unwrap()
        };
        report(Event::Started, None);
        report(Event::ToolStarted, Some("waiting-tool"));
        report(Event::Waiting, Some("waiting-tool"));
        assert_eq!(
            report(Event::ToolStarted, Some("sibling")).state,
            State::NeedsPermission
        );
        assert_eq!(
            report(Event::ToolFinished, Some("sibling")).state,
            State::NeedsPermission
        );
        assert_eq!(
            report(Event::ToolFinished, Some("waiting-tool")).state,
            State::Thinking
        );
        report(Event::ToolStarted, Some("first"));
        report(Event::ToolStarted, Some("second"));
        assert_eq!(
            report(Event::ToolFinished, Some("first")).state,
            State::Working
        );
        assert_eq!(
            report(Event::ToolFinished, Some("second")).state,
            State::Thinking
        );
        // A real permission prompt names no tool: the provider moving on in
        // the turn (the approved tool finishing) answers it.
        report(Event::ToolStarted, Some("asked"));
        assert_eq!(report(Event::Waiting, None).state, State::NeedsPermission);
        assert_eq!(
            report(Event::ToolFinished, Some("asked")).state,
            State::Thinking
        );
        report(Event::Waiting, None);
        assert_eq!(
            report(Event::ToolStarted, Some("next")).state,
            State::Working
        );
        assert_eq!(report(Event::Stopped, None).state, State::Done);
    }

    struct Rig {
        owner: ActivityOwner,
        writer: Writer,
        peer: process::Process,
    }

    impl Rig {
        fn new() -> Self {
            Self::with_owner(ActivityOwner::new())
        }

        fn with_owner(owner: ActivityOwner) -> Self {
            let writer = owner
                .prepare_native("session", AgentCli::Claude, "unused")
                .unwrap();
            owner.register("session", Some(std::process::id()));
            let peer = process::inspect(std::process::id()).unwrap();
            Self {
                owner,
                writer,
                peer,
            }
        }

        fn raw(&self, payload: serde_json::Value) -> Result<Snapshot, &'static str> {
            let report = crate::agent_cli::adapter(AgentCli::Claude)
                .normalize(&serde_json::to_vec(&payload).unwrap(), false)
                .ok_or("not admitted")?;
            self.report(report)
        }

        fn report(&self, report: Report) -> Result<Snapshot, &'static str> {
            self.owner.report(
                &self.writer.token,
                self.peer,
                report,
                Instant::now() + Duration::from_millis(250),
            )
        }

        fn hook(&self, event: &str) -> Snapshot {
            let mut payload =
                serde_json::json!({"hook_event_name": event, "session_id": "provider"});
            // A subagent's start and stop always name it.
            if event.starts_with("Subagent") {
                payload["agent_id"] = "child".into();
            }
            self.raw(payload).unwrap()
        }

        /// Subagent `id`'s own `event` hook.
        fn subagent(&self, event: &str, id: &str) -> Result<Snapshot, &'static str> {
            self.raw(serde_json::json!({"hook_event_name": event, "session_id": "provider", "agent_id": id, "tool_use_id": format!("{id}-tool"), "tool_name": "Bash"}))
        }

        /// A main `Stop` whose `background_tasks` lists `running` subagents.
        fn stop_listing(&self, running: &[&str]) -> Snapshot {
            let tasks: Vec<_> = running
                .iter()
                .map(|id| serde_json::json!({"id": id, "type": "subagent", "status": "running"}))
                .collect();
            self.raw(serde_json::json!({"hook_event_name": "Stop", "session_id": "provider", "background_tasks": tasks}))
                .unwrap()
        }

        fn mutate(&self, action: Action) -> Snapshot {
            self.owner
                .mutate(Mutation {
                    session: "session".into(),
                    identity: self.writer.identity.clone(),
                    turn_generation: self.owner.snapshot("session").unwrap().turn_generation,
                    operation_id: uuid::Uuid::new_v4().to_string(),
                    action,
                })
                .unwrap()
        }

        fn fixture(&self, name: &str) -> Snapshot {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../src/http/routes/fixtures/claude-hooks")
                .join(format!("{name}.json"));
            let mut payload: serde_json::Value =
                serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            payload["session_id"] = "provider".into();
            // The captures come from separate runs: one tool across them.
            if payload.get("tool_use_id").is_some() {
                payload["tool_use_id"] = "tool".into();
            }
            self.raw(payload).unwrap()
        }

        fn notification(&self, kind: &str) -> Result<Snapshot, &'static str> {
            self.raw(serde_json::json!({
                "hook_event_name": "Notification",
                "session_id": "provider",
                "notification_type": kind,
                "message": "SECRET",
            }))
        }
    }

    #[test]
    fn both_providers_publish_open_subagents_after_the_parent_stops() {
        for cli in [AgentCli::Claude, AgentCli::Codex] {
            let owner = ActivityOwner::new();
            let writer = owner.prepare_native("session", cli, "unused").unwrap();
            owner.register("session", Some(std::process::id())).unwrap();
            let hook = |event: &str, agent: Option<&str>| {
                let input = serde_json::to_vec(&serde_json::json!({
                    "hook_event_name":event, "session_id":"provider", "agent_id":agent
                }))
                .unwrap();
                let report = crate::agent_cli::adapter(cli)
                    .normalize(&input, true)
                    .unwrap();
                owner
                    .report_for_test(&writer, std::process::id(), report)
                    .unwrap()
            };
            hook("SessionStart", None);
            hook("UserPromptSubmit", None);
            assert_eq!(
                hook("SubagentStart", Some("child")).subagents_open,
                1,
                "{cli:?}"
            );
            let stopped = hook("Stop", None);
            assert_eq!(
                (stopped.state, stopped.subagents_open),
                (State::Done, 1),
                "{cli:?}"
            );
            assert_eq!(
                hook("SubagentStop", Some("child")).subagents_open,
                0,
                "{cli:?}"
            );
        }
    }

    #[test]
    fn codex_and_claude_distinguish_idle_outcomes_from_permission_waits() {
        for cli in [AgentCli::Claude, AgentCli::Codex] {
            let owner = ActivityOwner::new();
            let writer = owner.prepare_native("status", cli, "unused").unwrap();
            owner.register("status", Some(std::process::id())).unwrap();
            let hook = |event: &str| {
                let input = serde_json::to_vec(&serde_json::json!({
                    "hook_event_name": event, "session_id": "provider"
                }))
                .unwrap();
                let report = crate::agent_cli::adapter(cli)
                    .normalize(&input, true)
                    .unwrap();
                owner
                    .report_for_test(&writer, std::process::id(), report)
                    .unwrap()
            };
            hook("SessionStart");
            hook("UserPromptSubmit");
            let mutate = |action| {
                let snapshot = owner.snapshot("status").unwrap();
                owner
                    .mutate(Mutation {
                        session: "status".into(),
                        identity: snapshot.identity,
                        turn_generation: snapshot.turn_generation,
                        operation_id: uuid::Uuid::new_v4().to_string(),
                        action,
                    })
                    .unwrap()
            };
            let done = mutate(Action::Done {
                outcome: Outcome::NeedsAttention,
            });
            assert_eq!(done.state, State::Done, "{cli:?}");
            assert_eq!(done.reason.as_deref(), Some("needs-attention"));
            assert_eq!(hook("Stop").state, State::Done);
            hook("UserPromptSubmit");
            assert_eq!(hook("PermissionRequest").state, State::NeedsPermission);
            assert!(!owner.snapshot("status").unwrap().takes_input());
            assert_eq!(hook("Stop").state, State::Done);
            hook("UserPromptSubmit");
            assert_eq!(
                mutate(Action::AskOpened {
                    id: "question".into()
                })
                .state,
                State::NeedsPermission
            );
            assert_eq!(hook("Stop").state, State::Done);
            assert_eq!(
                mutate(Action::Progress { blocked: true }).state,
                State::NeedsPermission
            );
        }
    }

    #[test]
    fn completion_reply_retains_its_turn_boundary_after_the_next_turn_starts() {
        for (outcome, expected) in [
            (Outcome::Success, State::Done),
            (Outcome::Failed, State::Errored),
            (Outcome::NeedsAttention, State::Done),
        ] {
            let rig = Rig::new();
            rig.hook("SessionStart");
            let submitted = rig.hook("UserPromptSubmit");
            let completed = rig.mutate(Action::Done { outcome });
            assert_eq!(completed.identity, submitted.identity);
            assert_eq!(completed.turn_generation, submitted.turn_generation);
            assert!(completed.revision > submitted.revision);
            assert_eq!(completed.state, expected);

            let next = rig.hook("UserPromptSubmit");
            assert_eq!(next.identity, completed.identity);
            assert!(next.turn_generation > completed.turn_generation);
            assert!(next.revision > completed.revision);
            assert_eq!(next.state, State::Thinking);
            // A delayed consumer must use the mutation reply, never a later query.
            assert_eq!(completed.turn_generation, submitted.turn_generation);
            assert_eq!(completed.state, expected);
            assert!(rig
                .owner
                .mutate(Mutation {
                    session: "session".into(),
                    identity: completed.identity,
                    turn_generation: completed.turn_generation,
                    operation_id: uuid::Uuid::new_v4().to_string(),
                    action: Action::Done {
                        outcome: Outcome::Success
                    },
                })
                .is_err());
            assert_eq!(
                without_age(rig.owner.snapshot("session").unwrap()),
                without_age(next)
            );
        }
    }

    #[test]
    fn self_reported_block_is_distinct_and_clears_on_the_next_hook() {
        for (hook, expected) in [
            ("SessionStart", State::Idle),
            ("UserPromptSubmit", State::Thinking),
            ("PreToolUse", State::Working),
            ("PostToolUse", State::Thinking),
            ("PostToolUseFailure", State::Thinking),
            ("PreCompact", State::Working),
            ("SubagentStop", State::Thinking),
            ("PermissionRequest", State::NeedsPermission),
            ("Stop", State::Done),
            ("SessionEnd", State::Idle),
        ] {
            let rig = Rig::new();
            rig.hook("SessionStart");
            rig.hook("UserPromptSubmit");
            let blocked = rig.mutate(Action::Progress { blocked: true });
            assert_eq!(blocked.state, State::NeedsPermission);
            assert_eq!(blocked.reason.as_deref(), Some("blocked"));
            let resumed = rig.hook(hook);
            assert_eq!(resumed.state, expected, "{hook}");
            assert_eq!(resumed.reason, None, "{hook}");
            assert!(resumed.revision > blocked.revision, "{hook}");
        }
    }

    #[test]
    fn self_reported_block_clears_on_progress_without_clearing_finish_latches() {
        let rig = Rig::new();
        rig.hook("SessionStart");
        rig.hook("UserPromptSubmit");
        rig.mutate(Action::Progress { blocked: true });
        let resumed = rig.mutate(Action::Progress { blocked: false });
        assert_eq!(resumed.state, State::Working);
        assert_eq!(resumed.reason.as_deref(), Some("progress"));
        rig.raw(serde_json::json!({
            "hook_event_name": "PreToolUse", "session_id": "provider",
            "tool_use_id": "running-tool", "tool_name": "Bash"
        }))
        .unwrap();
        let tool = rig.mutate(Action::Progress { blocked: false });
        assert_eq!(tool.state, State::Working);
        assert_eq!(tool.reason, None);
        rig.hook("UserPromptSubmit");
        for (outcome, expected) in [(Outcome::Failed, State::Errored)] {
            rig.mutate(Action::Done { outcome });
            assert_eq!(rig.hook("PostToolUse").state, expected);
            assert_eq!(rig.hook("Stop").state, expected);
            assert_eq!(rig.hook("SubagentStop").state, expected);
            assert_eq!(rig.hook("UserPromptSubmit").state, State::Thinking);
        }
    }

    #[test]
    fn self_reported_progress_cannot_answer_provider_waits() {
        for correlated in [false, true] {
            for question in [false, true] {
                for blocked in [false, true] {
                    let rig = Rig::new();
                    rig.hook("SessionStart");
                    rig.hook("UserPromptSubmit");
                    let mut waiting = serde_json::json!({
                        "hook_event_name": if question { "Notification" } else { "PermissionRequest" },
                        "notification_type": "elicitation_dialog",
                        "session_id": "provider", "tool_name": "Bash"
                    });
                    if correlated {
                        waiting["tool_use_id"] = "waiting-tool".into();
                    }
                    rig.raw(waiting).unwrap();
                    for progress in [blocked, !blocked] {
                        let held = rig.mutate(Action::Progress { blocked: progress });
                        assert_eq!(held.state, State::NeedsPermission);
                        assert_eq!(held.reason.as_deref(), question.then_some("question"));
                    }
                    let sibling = rig
                        .raw(serde_json::json!({
                            "hook_event_name": "PostToolUse", "session_id": "provider",
                            "tool_use_id": "sibling", "tool_name": "Read"
                        }))
                        .unwrap();
                    assert_eq!(sibling.state, State::NeedsPermission);
                    assert_eq!(sibling.reason.as_deref(), question.then_some("question"));
                    let cleared = rig
                        .raw(serde_json::json!({
                            "hook_event_name": "PostToolUse", "session_id": "provider",
                            "tool_use_id": "waiting-tool", "tool_name": "Bash"
                        }))
                        .unwrap();
                    assert_eq!(cleared.state, State::Thinking);
                    assert_eq!(cleared.reason, None);
                    let progress = rig.mutate(Action::Progress { blocked });
                    assert_eq!(
                        progress.state,
                        if blocked {
                            State::NeedsPermission
                        } else {
                            State::Working
                        }
                    );
                    assert_eq!(
                        progress.reason.as_deref(),
                        Some(if blocked { "blocked" } else { "progress" })
                    );
                }
            }
        }
    }

    #[test]
    fn resolving_one_hold_preserves_the_other_hold_in_both_acquisition_orders() {
        for ask_first in [false, true] {
            for correlated in [false, true] {
                for question in [false, true] {
                    for answer_ask_first in [false, true] {
                        let rig = Rig::new();
                        rig.hook("SessionStart");
                        let turn = rig.hook("UserPromptSubmit");
                        if ask_first {
                            rig.mutate(Action::AskOpened { id: "ask".into() });
                        }
                        let mut wait = serde_json::json!({
                            "hook_event_name": if question { "Notification" } else { "PermissionRequest" },
                            "notification_type": "elicitation_dialog",
                            "session_id": "provider", "tool_name": "Bash"
                        });
                        if correlated {
                            wait["tool_use_id"] = "waiting".into();
                        }
                        let waiting = rig.raw(wait).unwrap();
                        if !ask_first {
                            rig.mutate(Action::AskOpened { id: "ask".into() });
                        }
                        let close_ask = || {
                            rig.mutate(Action::AskClosed {
                                id: "ask".into(),
                                resolved: true,
                            })
                        };
                        let answer_provider = || {
                            rig.raw(serde_json::json!({
                                "hook_event_name": "PostToolUse", "session_id": "provider",
                                "tool_use_id": "waiting", "tool_name": "Bash"
                            }))
                            .unwrap()
                        };
                        let held = if answer_ask_first {
                            close_ask()
                        } else {
                            answer_provider()
                        };
                        assert_eq!(held.state, State::NeedsPermission, "ask_first={ask_first} correlated={correlated} question={question} answer_ask_first={answer_ask_first}");
                        assert_eq!(
                            held.reason.as_deref(),
                            (answer_ask_first && question).then_some("question")
                        );
                        assert_eq!(held.asks.is_empty(), answer_ask_first);
                        assert_eq!(held.turn_generation, turn.turn_generation);
                        assert!(held.revision > waiting.revision);
                        assert_eq!(held.tool_open, Some(false));
                        let resumed = if answer_ask_first {
                            answer_provider()
                        } else {
                            close_ask()
                        };
                        assert_eq!(
                            resumed.state,
                            if answer_ask_first {
                                State::Thinking
                            } else {
                                State::Working
                            }
                        );
                        assert_eq!(resumed.reason, None);
                        assert!(resumed.asks.is_empty());
                    }
                }
            }
        }
    }

    #[test]
    fn provider_wait_kinds_follow_surviving_holds_not_the_latest_report() {
        for question_correlated in [false, true] {
            for question_first in [false, true] {
                for answer_question_first in [false, true] {
                    let rig = Rig::new();
                    rig.hook("SessionStart");
                    rig.hook("UserPromptSubmit");
                    for question in [question_first, !question_first] {
                        let mut wait = serde_json::json!({
                            "hook_event_name": if question { "Notification" } else { "PermissionRequest" },
                            "notification_type": "elicitation_dialog", "session_id": "provider",
                            "tool_name": if question { "Bash" } else { "Read" }
                        });
                        if question == question_correlated {
                            wait["tool_use_id"] =
                                (if question { "question" } else { "permission" }).into();
                        }
                        rig.raw(wait).unwrap();
                    }
                    let both = rig.mutate(Action::Progress { blocked: false });
                    assert_eq!(both.state, State::NeedsPermission);
                    assert_eq!(both.reason.as_deref(), Some("question"));
                    for (index, question) in [answer_question_first, !answer_question_first]
                        .into_iter()
                        .enumerate()
                    {
                        let mut answer = serde_json::json!({
                            "hook_event_name": "PostToolUse", "session_id": "provider",
                            "tool_name": if question { "Bash" } else { "Read" },
                            "tool_use_id": if question { "question" } else { "permission" }
                        });
                        // Child tool progress can answer only the uncorrelated
                        // wait; settlement must reproject the other hold's kind.
                        if question != question_correlated {
                            answer["agent_id"] = "child".into();
                        }
                        let next = rig.raw(answer).unwrap();
                        assert_eq!(
                            next.state,
                            if index == 0 {
                                State::NeedsPermission
                            } else {
                                State::Thinking
                            }
                        );
                        assert_eq!(
                            next.reason.as_deref(),
                            (index == 0 && !question).then_some("question")
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn nonblocked_progress_releases_ask_restore_ownership_even_after_a_block() {
        for previously_blocked in [false, true] {
            for provider_wait in [false, true] {
                let rig = Rig::new();
                rig.hook("SessionStart");
                rig.hook("UserPromptSubmit");
                rig.mutate(Action::AskOpened { id: "ask".into() });
                if provider_wait {
                    rig.raw(serde_json::json!({
                        "hook_event_name": "PermissionRequest", "session_id": "provider",
                        "tool_use_id": "waiting", "tool_name": "Bash"
                    }))
                    .unwrap();
                }
                if previously_blocked {
                    rig.mutate(Action::Progress { blocked: true });
                }
                let progress = rig.mutate(Action::Progress { blocked: false });
                assert_eq!(progress.state, State::NeedsPermission);
                assert_eq!(progress.reason, None);
                let closed = rig.mutate(Action::AskClosed {
                    id: "ask".into(),
                    resolved: true,
                });
                assert_eq!(
                    closed.state,
                    if provider_wait {
                        State::NeedsPermission
                    } else {
                        State::Working
                    }
                );
                assert_eq!(closed.reason, None);
                assert!(closed.asks.is_empty());
                if provider_wait {
                    let answered = rig
                        .raw(serde_json::json!({
                            "hook_event_name": "PostToolUse", "session_id": "provider",
                            "tool_use_id": "waiting", "tool_name": "Bash"
                        }))
                        .unwrap();
                    assert_eq!(answered.state, State::Thinking);
                    assert_eq!(answered.reason, None);
                }
            }
        }
    }

    #[test]
    fn clearing_a_self_reported_block_preserves_independent_permission_and_ask_holds() {
        let rig = Rig::new();
        rig.hook("SessionStart");
        rig.hook("UserPromptSubmit");
        rig.raw(serde_json::json!({
            "hook_event_name": "PermissionRequest", "session_id": "provider",
            "tool_use_id": "waiting-tool", "tool_name": "Bash"
        }))
        .unwrap();
        rig.mutate(Action::Progress { blocked: true });
        let permission = rig.hook("PostToolUse");
        assert_eq!(permission.state, State::NeedsPermission);
        assert_eq!(permission.reason, None);
        for hook in ["PostToolUse", "SubagentStop"] {
            rig.hook("UserPromptSubmit");
            rig.mutate(Action::AskOpened { id: "ask".into() });
            rig.mutate(Action::Progress { blocked: true });
            let asked = rig.hook(hook);
            assert_eq!(asked.state, State::NeedsPermission);
            assert_eq!(asked.reason, None);
            assert_eq!(asked.asks, ["ask"]);
            let answered = rig.mutate(Action::AskClosed {
                id: "ask".into(),
                resolved: true,
            });
            assert_eq!(answered.state, State::Working);
            assert_eq!(answered.reason, None);
        }
    }

    #[test]
    fn closing_an_ask_preserves_a_superseding_block_until_new_activity() {
        for resolved in [false, true] {
            for resume in ["PostToolUse", "SubagentStop", "signal-progress"] {
                let rig = Rig::new();
                rig.hook("SessionStart");
                rig.hook("UserPromptSubmit");
                rig.mutate(Action::AskOpened { id: "ask".into() });
                let blocked = rig.mutate(Action::Progress { blocked: true });
                let closed = rig.mutate(Action::AskClosed {
                    id: "ask".into(),
                    resolved,
                });
                assert_eq!(closed.state, blocked.state, "{resolved} {resume}");
                assert_eq!(closed.reason.as_deref(), Some("blocked"));
                assert!(closed.asks.is_empty());
                assert!(closed.revision > blocked.revision);

                let resumed = if resume == "signal-progress" {
                    rig.mutate(Action::Progress { blocked: false })
                } else {
                    rig.hook(resume)
                };
                assert_eq!(
                    resumed.state,
                    if resume == "signal-progress" {
                        State::Working
                    } else {
                        State::Thinking
                    },
                    "{resolved} {resume}"
                );
                assert_ne!(resumed.reason.as_deref(), Some("blocked"));
                assert!(resumed.asks.is_empty());
            }
        }
    }

    #[test]
    fn recorded_permission_request_without_tool_id_clears_when_the_turn_moves_on() {
        let rig = Rig::new();
        rig.hook("SessionStart");
        assert_eq!(rig.fixture("UserPromptSubmit").state, State::Thinking);
        assert_eq!(rig.fixture("PreToolUse").state, State::Working);
        let asked = rig.fixture("PermissionRequest");
        assert_eq!(asked.state, State::NeedsPermission);
        assert_eq!(rig.fixture("PostToolUse").state, State::Thinking);
        assert_eq!(rig.fixture("Stop").state, State::Done);
        // A new prompt answers a wait too.
        rig.fixture("PermissionRequest");
        assert_eq!(rig.fixture("UserPromptSubmit").state, State::Thinking);
    }

    #[test]
    fn notifications_map_to_permission_question_and_idle() {
        let rig = Rig::new();
        rig.hook("SessionStart");
        rig.hook("UserPromptSubmit");
        let permission = rig.notification("permission_prompt").unwrap();
        assert_eq!(permission.state, State::NeedsPermission);
        assert_eq!(permission.reason, None);
        rig.hook("UserPromptSubmit");
        let question = rig.notification("elicitation_dialog").unwrap();
        assert_eq!(question.state, State::NeedsPermission);
        assert_eq!(question.reason.as_deref(), Some("question"));
        let idle = rig.notification("idle_prompt").unwrap();
        assert_eq!(idle.state, State::Idle);
        assert_eq!(idle.reason, None);
        // A finished turn's idle reminder keeps the finish.
        rig.hook("UserPromptSubmit");
        rig.hook("Stop");
        assert_eq!(rig.notification("idle_prompt").unwrap().state, State::Done);
        assert!(rig.notification("auth_success").is_err());
        assert!(!serde_json::to_string(&rig.owner.snapshots())
            .unwrap()
            .contains("SECRET"));
    }

    #[test]
    fn compaction_subagents_and_interrupts_keep_the_turn_honest() {
        let rig = Rig::new();
        rig.hook("SessionStart");
        rig.hook("UserPromptSubmit");
        assert_eq!(rig.hook("PreCompact").state, State::Working);
        let before = rig.owner.snapshots()[0].revision;
        let progressed = rig
            .raw(serde_json::json!({"hook_event_name": "SubagentStop", "session_id": "provider", "agent_id": "child"}))
            .unwrap();
        assert_eq!(progressed.state, State::Working);
        assert_eq!(progressed.revision, before + 1);
        let interrupted = rig
            .raw(serde_json::json!({"hook_event_name": "PostToolUseFailure", "session_id": "provider", "tool_use_id": "t", "is_interrupt": true}))
            .unwrap();
        assert_eq!(interrupted.state, State::Idle);
    }

    #[test]
    fn stop_failure_carries_its_error_kind_until_the_next_turn() {
        let rig = Rig::new();
        rig.hook("SessionStart");
        rig.hook("UserPromptSubmit");
        let failed = rig.fixture("StopFailure");
        assert_eq!(failed.state, State::Errored);
        assert_eq!(failed.reason.as_deref(), Some("model_not_found"));
        let limited = rig
            .raw(serde_json::json!({"hook_event_name": "StopFailure", "session_id": "provider", "error": "rate_limit"}))
            .unwrap();
        assert_eq!(limited.reason.as_deref(), Some("rate_limit"));
        let unknown = rig
            .raw(serde_json::json!({"hook_event_name": "StopFailure", "session_id": "provider", "error": "SECRET detail"}))
            .unwrap();
        assert_eq!(unknown.reason.as_deref(), Some("unknown"));
        let next = rig.hook("UserPromptSubmit");
        assert_eq!(next.state, State::Thinking);
        assert_eq!(next.reason, None);
    }

    #[test]
    fn silent_turns_stay_open_at_three_and_ten_minutes_until_provider_evidence_ends_them() {
        let rig = Rig::new();
        rig.hook("SessionStart");
        let thinking = rig.hook("UserPromptSubmit");
        for seconds in [3 * 60, 10 * 60, 60 * 60] {
            rig.owner
                .decay(Instant::now() + Duration::from_secs(seconds));
            assert_eq!(
                without_age(rig.owner.snapshots()[0].clone()),
                without_age(thinking.clone())
            );
        }
        // Compaction is Working with no open tool: it too must not be inferred idle.
        let compacting = rig.hook("PreCompact");
        for seconds in [3 * 60, 10 * 60] {
            rig.owner
                .decay(Instant::now() + Duration::from_secs(seconds));
            assert_eq!(
                without_age(rig.owner.snapshots()[0].clone()),
                without_age(compacting.clone())
            );
        }
        rig.hook("UserPromptSubmit");
        let working = rig.raw(serde_json::json!({"hook_event_name": "PreToolUse", "session_id": "provider", "tool_use_id": "long"}))
            .unwrap();
        rig.owner
            .decay(Instant::now() + Duration::from_secs(10 * 60));
        assert_eq!(
            without_age(rig.owner.snapshots()[0].clone()),
            without_age(working.clone())
        );
        rig.hook("UserPromptSubmit");
        let waiting = rig.fixture("PermissionRequest");
        rig.owner
            .decay(Instant::now() + Duration::from_secs(10 * 60));
        assert_eq!(
            without_age(rig.owner.snapshots()[0].clone()),
            without_age(waiting.clone())
        );
        assert_eq!(rig.hook("Stop").state, State::Done);
    }

    #[test]
    fn subagent_stop_publishes_fresh_activity_without_changing_the_parent_state() {
        let sink = std::sync::Arc::new(crate::fixtures::RecordingSink::default());
        let rig = Rig::with_owner(ActivityOwner::with_sink(
            sink.clone(),
            crate::agent_cli::adapter,
        ));
        rig.hook("SessionStart");
        for hook in ["UserPromptSubmit", "PreCompact", "Stop"] {
            let before = rig.hook(hook);
            let event_count = sink.snapshot().len();
            let progressed = rig.subagent("SubagentStop", "internal").unwrap();
            assert_eq!(progressed.state, before.state);
            assert_eq!(progressed.reason, before.reason);
            assert_eq!(progressed.revision, before.revision + 1);
            let events = sink.snapshot();
            assert_eq!(events.len(), event_count + 1);
            assert!(
                matches!(events.last(), Some(crate::domain::TerminalDomainEvent::Activity { snapshot }) if snapshot == &progressed)
            );
        }
    }

    #[test]
    fn background_subagents_outlive_the_main_stop_and_never_move_its_state() {
        let rig = Rig::new();
        rig.hook("SessionStart");
        rig.hook("UserPromptSubmit");
        rig.subagent("SubagentStart", "a1").unwrap();
        rig.subagent("SubagentStart", "a2").unwrap();
        let stopped = rig.stop_listing(&["a1", "a2"]);
        assert_eq!((stopped.state, stopped.subagents_open), (State::Done, 2));
        // Their tools run after the main turn ended: fresh evidence, the parent stays Done.
        for event in ["PreToolUse", "PostToolUse"] {
            let heard = rig.subagent(event, "a1").unwrap();
            assert_eq!((heard.state, heard.subagents_open), (State::Done, 2));
            assert_eq!(heard.tool_open, Some(false));
        }
        assert_eq!(
            rig.subagent("SubagentStop", "a1").unwrap().subagents_open,
            1
        );
        let last = rig.subagent("SubagentStop", "a2").unwrap();
        assert_eq!((last.state, last.subagents_open), (State::Done, 0));
        assert_eq!(
            serde_json::to_value(&last).unwrap().get("subagentsOpen"),
            None
        );
    }

    #[test]
    fn a_resume_a_stray_stop_and_a_late_hook_keep_the_count_honest() {
        let rig = Rig::new();
        rig.hook("SessionStart");
        rig.hook("UserPromptSubmit");
        rig.subagent("SubagentStart", "a1").unwrap();
        // An internal agent's stop matches no start; a resume repeats its start.
        assert_eq!(
            rig.subagent("SubagentStop", "internal")
                .unwrap()
                .subagents_open,
            1
        );
        assert_eq!(
            rig.subagent("SubagentStart", "a1").unwrap().subagents_open,
            1
        );
        rig.hook("Stop");
        assert_eq!(
            rig.subagent("SubagentStop", "a1").unwrap().subagents_open,
            0
        );
        // A late hook of a finished subagent is not admitted and opens nothing.
        assert!(rig.subagent("PostToolUse", "a1").is_err());
        assert_eq!(rig.owner.snapshot("session").unwrap().subagents_open, 0);
        // Resumed by its own background shell, it runs again until it stops again.
        assert_eq!(
            rig.subagent("SubagentStart", "a1").unwrap().subagents_open,
            1
        );
        // A subagent whose start hook failed is opened by its first own hook.
        assert_eq!(rig.subagent("PreToolUse", "b1").unwrap().subagents_open, 2);
        // The main Stop's list drops what it no longer names, and adds nothing.
        let reconciled = rig.stop_listing(&["b1", "unknown"]);
        assert_eq!(reconciled.subagents_open, 1);
        // A Stop with no task list leaves the count alone.
        assert_eq!(rig.hook("Stop").subagents_open, 1);
    }

    #[test]
    fn only_its_own_sessions_named_subagents_and_their_tools_are_counted() {
        let rig = Rig::new();
        rig.hook("SessionStart");
        rig.hook("UserPromptSubmit");
        // A start that names no subagent is not one.
        assert!(rig
            .raw(serde_json::json!({"hook_event_name": "SubagentStart", "session_id": "provider"}))
            .is_err());
        assert!(rig
            .raw(serde_json::json!({"hook_event_name": "SubagentStart", "session_id": "foreign", "agent_id": "x"}))
            .is_err());
        rig.hook("Stop");
        // A subagent's own end of turn opens nothing; only its tools and prompts do.
        assert!(rig.subagent("Stop", "late").is_err());
        assert_eq!(rig.owner.snapshot("session").unwrap().subagents_open, 0);
        // A child Stop's task list never reconciles the main agent's.
        rig.subagent("SubagentStart", "a1").unwrap();
        let child_stop = rig
            .raw(serde_json::json!({"hook_event_name": "Stop", "session_id": "provider", "agent_id": "a1", "background_tasks": []}))
            .unwrap_err();
        assert_eq!(child_stop, "activity main scope required");
        assert_eq!(rig.owner.snapshot("session").unwrap().subagents_open, 1);
    }

    #[test]
    fn an_exit_a_new_context_a_session_end_or_the_bound_clears_the_count() {
        let open = |rig: &Rig| {
            rig.hook("SessionStart");
            rig.hook("UserPromptSubmit");
            rig.subagent("SubagentStart", "a1").unwrap();
            rig.hook("Stop");
            assert_eq!(rig.owner.snapshot("session").unwrap().subagents_open, 1);
        };
        let rig = Rig::new();
        open(&rig);
        rig.owner.exit(
            "session",
            &rig.writer.identity.session_incarnation,
            Exit {
                code: Some(0),
                signal: None,
                requested: false,
            },
        );
        assert_eq!(rig.owner.snapshot("session").unwrap().subagents_open, 0);

        let rig = Rig::new();
        open(&rig);
        assert_eq!(rig.hook("SessionEnd").subagents_open, 0);

        let rig = Rig::new();
        open(&rig);
        let cleared = rig
            .raw(
                serde_json::json!({"hook_event_name": "SessionStart", "session_id": "after-clear"}),
            )
            .unwrap();
        assert_eq!(cleared.subagents_open, 0);

        // A CLI whose main Stop lists its running subagents corrects a lost stop there,
        // so one in a long quiet tool call stays counted however long it is silent.
        let rig = Rig::new();
        open(&rig);
        rig.stop_listing(&["a1"]);
        rig.owner
            .decay(Instant::now() + 2 * super::super::subagents::STALE);
        assert_eq!(rig.owner.snapshot("session").unwrap().subagents_open, 1);
        assert_eq!(rig.stop_listing(&[]).subagents_open, 0);

        // One that never listed them drops a subagent unheard from for the long bound,
        // and says so.
        let sink = std::sync::Arc::new(crate::fixtures::RecordingSink::default());
        let rig = Rig::with_owner(ActivityOwner::with_sink(
            sink.clone(),
            crate::agent_cli::adapter,
        ));
        open(&rig);
        rig.owner
            .decay(Instant::now() + super::super::subagents::STALE / 2);
        assert_eq!(rig.owner.snapshot("session").unwrap().subagents_open, 1);
        let published = sink.snapshot().len();
        rig.owner
            .decay(Instant::now() + super::super::subagents::STALE);
        let expired = rig.owner.snapshot("session").unwrap();
        assert_eq!((expired.state, expired.subagents_open), (State::Done, 0));
        assert_eq!(sink.snapshot().len(), published + 1);
    }

    #[test]
    fn a_cli_that_reports_no_subagents_never_counts_any() {
        struct Silent;
        impl crate::agent_cli::AgentCliAdapter for Silent {
            fn descriptor(&self) -> &'static crate::agent_cli::Descriptor {
                crate::agent_cli::adapter(AgentCli::Claude).descriptor()
            }
            fn probe(&self) -> &'static crate::agent_cli::Probe {
                crate::agent_cli::adapter(AgentCli::Claude).probe()
            }
            fn prepare(
                &self,
                launch: &mut crate::agent_cli::Prepare<'_>,
            ) -> Result<(), &'static str> {
                crate::agent_cli::adapter(AgentCli::Claude).prepare(launch)
            }
            fn inbox(&self) -> InboxDelivery {
                InboxDelivery::None
            }
            fn inbox_hooks(
                &self,
                _: &mut crate::agent_cli::Prepare<'_>,
            ) -> Result<(), &'static str> {
                Ok(())
            }
            fn submit_notice(
                &self,
                _: &mut dyn crate::agent_cli::Channel,
                _: &crate::agent_cli::Notice<'_>,
            ) -> crate::agent_cli::SubmitOutcome {
                crate::agent_cli::SubmitOutcome::RefusedNoEffect
            }
            fn channel(&self, _: &[String]) -> Option<crate::agent_cli::NativeChannel> {
                None
            }
            fn normalize(&self, input: &[u8], qualified: bool) -> Option<Report> {
                crate::agent_cli::adapter(AgentCli::Claude).normalize(input, qualified)
            }
            fn session_policy(&self) -> crate::agent_cli::SessionPolicy {
                crate::agent_cli::SessionPolicy {
                    subagents: Subagents::None,
                    ..crate::agent_cli::adapter(AgentCli::Claude).session_policy()
                }
            }
            fn status_hooks(&self) -> &'static [crate::agent_cli::StatusHook] {
                &[]
            }
            fn control(&self) -> crate::agent_cli::Control {
                crate::agent_cli::adapter(AgentCli::Claude).control()
            }
            fn output(&self) -> crate::agent_cli::Output {
                crate::agent_cli::adapter(AgentCli::Claude).output()
            }
            fn restore_tags(&self) -> crate::agent_cli::RestoreTags {
                crate::agent_cli::adapter(AgentCli::Claude).restore_tags()
            }
        }
        fn adapters(_: AgentCli) -> &'static dyn crate::agent_cli::AgentCliAdapter {
            &Silent
        }
        let rig = Rig::with_owner(ActivityOwner::with_adapters(adapters));
        rig.hook("SessionStart");
        rig.hook("UserPromptSubmit");
        rig.subagent("SubagentStart", "a1").unwrap();
        rig.subagent("PreToolUse", "b1").unwrap();
        assert_eq!(rig.hook("Stop").subagents_open, 0);
    }

    #[test]
    fn activity_metadata_captures_elapsed_age_without_fabricating_initial_evidence() {
        let rig = Rig::new();
        let initial = rig.owner.snapshot("session").unwrap();
        assert_eq!(initial.activity_age_ms, None);
        assert_eq!(initial.tool_open, None);
        rig.hook("SessionStart");
        rig.hook("UserPromptSubmit");
        let now = Instant::now();
        {
            let mut sessions = rig.owner.sessions.lock().unwrap();
            let invocation = sessions
                .get_mut("session")
                .unwrap()
                .invocation
                .as_mut()
                .unwrap();
            invocation.evidence_at = Some(now);
            for seconds in [3 * 60, 10 * 60] {
                let snapshot = invocation.capture(now + Duration::from_secs(seconds));
                assert_eq!(snapshot.state, State::Thinking);
                assert_eq!(snapshot.activity_age_ms, Some(seconds * 1000));
                assert_eq!(snapshot.tool_open, Some(false));
            }
            invocation.output_at = Some(now + Duration::from_secs(9 * 60));
            assert_eq!(
                invocation
                    .capture(now + Duration::from_secs(10 * 60))
                    .activity_age_ms,
                Some(60_000)
            );
            invocation.evidence_at = Some(now + Duration::from_secs(10 * 60));
            assert_eq!(
                invocation
                    .capture(now + Duration::from_secs(10 * 60))
                    .activity_age_ms,
                Some(0)
            );
            // Reconnect and snapshot polling must report the old evidence's age.
            invocation.evidence_at = Some(now - Duration::from_secs(10 * 60));
            invocation.output_at = None;
        }
        let revision = rig.owner.snapshot("session").unwrap().revision;
        for snapshot in [
            rig.owner.snapshot("session").unwrap(),
            rig.owner.snapshots().remove(0),
        ] {
            assert!(snapshot.activity_age_ms.unwrap() >= 600_000);
            assert_eq!(snapshot.revision, revision);
        }
        let fresh = rig.hook("SubagentStop");
        assert!(fresh.activity_age_ms.unwrap() < 1_000);
        assert_eq!(fresh.revision, revision + 1);
    }

    #[test]
    fn activity_metadata_tracks_actual_tools_not_compaction_progress_or_answered_asks() {
        let rig = Rig::new();
        rig.hook("SessionStart");
        assert_eq!(rig.hook("UserPromptSubmit").tool_open, Some(false));
        assert_eq!(rig.hook("PreCompact").tool_open, Some(false));
        rig.mutate(Action::AskOpened {
            id: "question".into(),
        });
        let answered = rig.mutate(Action::AskClosed {
            id: "question".into(),
            resolved: true,
        });
        assert_eq!(answered.state, State::Working);
        assert_eq!(answered.tool_open, Some(false));
        for id in ["first", "second"] {
            let opened = rig.raw(serde_json::json!({"hook_event_name": "PreToolUse", "session_id": "provider", "tool_use_id": id})).unwrap();
            assert_eq!(opened.tool_open, Some(true));
        }
        assert_eq!(
            rig.mutate(Action::Progress { blocked: false }).tool_open,
            Some(true)
        );
        let first = rig.raw(serde_json::json!({"hook_event_name": "PostToolUse", "session_id": "provider", "tool_use_id": "first"})).unwrap();
        assert_eq!(first.tool_open, Some(true));
        let second = rig.raw(serde_json::json!({"hook_event_name": "PostToolUse", "session_id": "provider", "tool_use_id": "second"})).unwrap();
        assert_eq!(second.tool_open, Some(false));
        assert_eq!(rig.hook("Stop").tool_open, Some(false));
    }

    #[test]
    fn output_activity_is_bounded_and_preserves_holds_and_finished_turns() {
        let sink = std::sync::Arc::new(crate::fixtures::RecordingSink::default());
        let rig = Rig::with_owner(ActivityOwner::with_sink(
            sink.clone(),
            crate::agent_cli::adapter,
        ));
        rig.hook("SessionStart");
        rig.hook("UserPromptSubmit");
        let held = rig.mutate(Action::Progress { blocked: true });
        let now = Instant::now();
        let incarnation = &rig.writer.identity.session_incarnation;
        let event_count = sink.snapshot().len();
        rig.owner.output_at("session", incarnation, now);
        let first = rig.owner.snapshot("session").unwrap();
        assert_eq!(first.revision, held.revision + 1);
        assert_eq!(first.state, held.state);
        assert_eq!(first.reason, held.reason);
        for millis in [1, 500, 999] {
            rig.owner
                .output_at("session", incarnation, now + Duration::from_millis(millis));
        }
        assert_eq!(sink.snapshot().len(), event_count + 1);
        {
            let sessions = rig.owner.sessions.lock().unwrap();
            let invocation = sessions["session"].invocation.as_ref().unwrap();
            assert_eq!(
                invocation
                    .capture(now + Duration::from_millis(1000))
                    .activity_age_ms,
                Some(1)
            );
            assert_eq!(invocation.snapshot.revision, first.revision);
        }
        rig.owner
            .output_at("session", incarnation, now + Duration::from_secs(1));
        assert_eq!(sink.snapshot().len(), event_count + 2);
        rig.hook("PermissionRequest");
        let asked = rig.mutate(Action::AskOpened { id: "ask".into() });
        rig.owner
            .output_at("session", incarnation, now + Duration::from_secs(2));
        let still_asked = rig.owner.snapshot("session").unwrap();
        assert_eq!(still_asked.state, State::NeedsPermission);
        assert_eq!(still_asked.asks, asked.asks);
        rig.hook("UserPromptSubmit");
        let stopped = rig.hook("Stop");
        rig.owner
            .output_at("session", incarnation, now + Duration::from_secs(3));
        assert_eq!(rig.owner.snapshot("session").unwrap().state, stopped.state);
    }

    #[test]
    fn wrong_incarnation_and_exited_output_do_not_change_activity() {
        let sink = std::sync::Arc::new(crate::fixtures::RecordingSink::default());
        let rig = Rig::with_owner(ActivityOwner::with_sink(
            sink.clone(),
            crate::agent_cli::adapter,
        ));
        rig.hook("SessionStart");
        let before = rig.owner.snapshot("session").unwrap();
        let count = sink.snapshot().len();
        rig.owner.output_at("session", "retired", Instant::now());
        assert_eq!(sink.snapshot().len(), count);
        assert_eq!(
            without_age(rig.owner.snapshot("session").unwrap()),
            without_age(before)
        );
        assert_eq!(
            rig.owner.sessions.lock().unwrap()["session"]
                .invocation
                .as_ref()
                .unwrap()
                .output_at,
            None
        );
        rig.owner.exit(
            "session",
            &rig.writer.identity.session_incarnation,
            Exit {
                code: Some(0),
                signal: None,
                requested: false,
            },
        );
        let ended = rig.owner.snapshot("session").unwrap();
        assert_eq!(ended.tool_open, Some(false));
        let count = sink.snapshot().len();
        rig.owner
            .output("session", &rig.writer.identity.session_incarnation);
        assert_eq!(sink.snapshot().len(), count);
        assert_eq!(
            without_age(rig.owner.snapshot("session").unwrap()),
            without_age(ended)
        );
        assert_eq!(
            rig.owner.sessions.lock().unwrap()["session"]
                .invocation
                .as_ref()
                .unwrap()
                .output_at,
            None
        );
    }

    #[test]
    fn rejected_and_duplicate_inputs_do_not_freshen_activity_metadata() {
        let rig = Rig::new();
        rig.hook("SessionStart");
        rig.hook("UserPromptSubmit");
        let report = crate::agent_cli::adapter(AgentCli::Claude)
            .normalize(
                br#"{"hook_event_name":"SubagentStop","session_id":"provider","agent_id":"child"}"#,
                false,
            )
            .unwrap();
        rig.report(report.clone()).unwrap();
        let mutation = Mutation {
            session: "session".into(),
            identity: rig.writer.identity.clone(),
            turn_generation: rig.owner.snapshot("session").unwrap().turn_generation,
            operation_id: "once".into(),
            action: Action::Progress { blocked: true },
        };
        rig.owner.mutate(mutation.clone()).unwrap();
        let old = Instant::now() - Duration::from_secs(10 * 60);
        {
            let mut sessions = rig.owner.sessions.lock().unwrap();
            let invocation = sessions
                .get_mut("session")
                .unwrap()
                .invocation
                .as_mut()
                .unwrap();
            invocation.evidence_at = Some(old);
            invocation.closed_asks = (0..127).map(|n| format!("closed-{n}")).collect();
            invocation.closed_asks.insert("closed".into());
            invocation.tools = (0..RECENT_REPORTS).map(|n| format!("tool-{n}")).collect();
            invocation.waits = (0..RECENT_REPORTS)
                .map(|n| (format!("wait-{n}"), WaitKind::Permission))
                .collect();
            invocation.uncorrelated_wait = Some(WaitKind::Permission);
            invocation.wait_tool = Some("Bash".into());
        }
        let before = rig.owner.snapshot("session").unwrap();
        for action in [
            Action::AskOpened {
                id: "closed".into(),
            },
            Action::AskOpened {
                id: "overflow".into(),
            },
            Action::AskClosed {
                id: "overflow".into(),
                resolved: false,
            },
        ] {
            let mut rejected = mutation.clone();
            rejected.operation_id = "rejected".into();
            rejected.action = action;
            assert!(rig.owner.mutate(rejected).is_err());
        }
        for hook in ["PreToolUse", "PermissionRequest"] {
            assert!(rig.raw(serde_json::json!({"hook_event_name": hook, "session_id": "provider", "tool_use_id": "overflow", "tool_name": "Bash"})).is_err());
        }
        for repeated in [
            rig.report(report).unwrap(),
            rig.owner.mutate(mutation).unwrap(),
        ] {
            assert_eq!(without_age(repeated.clone()), without_age(before.clone()));
            assert!(repeated.activity_age_ms.unwrap() >= 600_000);
        }
        let sessions = rig.owner.sessions.lock().unwrap();
        let invocation = sessions["session"].invocation.as_ref().unwrap();
        assert_eq!(invocation.evidence_at, Some(old));
        assert_eq!(invocation.tools.len(), RECENT_REPORTS);
        assert_eq!(invocation.waits.len(), RECENT_REPORTS);
        assert!(invocation.uncorrelated_wait.is_some());
        assert_eq!(invocation.wait_tool.as_deref(), Some("Bash"));
        assert_eq!(
            without_age(invocation.capture(Instant::now())),
            without_age(before)
        );
    }

    #[test]
    fn child_tools_refresh_only_the_active_matching_parent_turn() {
        for working in [false, true] {
            for turn in [None, Some("current")] {
                let sink = std::sync::Arc::new(crate::fixtures::RecordingSink::default());
                let rig = Rig::with_owner(ActivityOwner::with_sink(
                    sink.clone(),
                    crate::agent_cli::adapter,
                ));
                rig.hook("SessionStart");
                rig.raw(serde_json::json!({"hook_event_name":"UserPromptSubmit", "session_id":"provider", "turn_id":turn})).unwrap();
                if working {
                    rig.raw(serde_json::json!({"hook_event_name":"PreToolUse", "session_id":"provider", "turn_id":turn, "tool_use_id":"parent", "tool_name":"Task"})).unwrap();
                }
                {
                    let mut sessions = rig.owner.sessions.lock().unwrap();
                    let invocation = sessions
                        .get_mut("session")
                        .unwrap()
                        .invocation
                        .as_mut()
                        .unwrap();
                    invocation.evidence_at = Some(Instant::now() - Duration::from_secs(601));
                    invocation.output_at = None;
                }
                for event in [Event::ToolStarted, Event::ToolFinished] {
                    let report = Report {
                        provider: AgentCli::Claude,
                        event,
                        scope: Scope::Child,
                        report_id: uuid::Uuid::new_v4().to_string(),
                        provider_session: Some("provider".into()),
                        turn: turn.map(str::to_owned),
                        tool: Some("child-tool".into()),
                        tool_name: Some("Bash".into()),
                        reason: None,
                        agent: None,
                        subagents: None,
                    };
                    let before = rig.owner.snapshot("session").unwrap();
                    let count = sink.snapshot().len();
                    let fresh = rig.report(report.clone()).unwrap();
                    assert_eq!(sink.snapshot().len(), count + 1);
                    assert!(
                        matches!(sink.snapshot().last(), Some(crate::domain::TerminalDomainEvent::Activity { snapshot }) if snapshot == &fresh)
                    );
                    assert!(fresh.activity_age_ms.unwrap() < 1000);
                    let mut expected = before;
                    expected.revision += 1;
                    assert_eq!(without_age(fresh.clone()), without_age(expected));
                    let evidence = rig.owner.sessions.lock().unwrap()["session"]
                        .invocation
                        .as_ref()
                        .unwrap()
                        .evidence_at;
                    let replay = rig.report(report).unwrap();
                    assert_eq!(replay.revision, fresh.revision);
                    assert_eq!(sink.snapshot().len(), count + 1);
                    let sessions = rig.owner.sessions.lock().unwrap();
                    let invocation = sessions["session"].invocation.as_ref().unwrap();
                    assert_eq!(invocation.evidence_at, evidence);
                    assert_eq!(invocation.tools.len(), usize::from(working));
                    assert!(!invocation.tools.contains("child-tool"));
                    assert_eq!(invocation.turn.as_deref(), turn);
                }
            }
        }
    }

    #[test]
    fn child_freshness_rejects_foreign_stale_inactive_and_lifecycle_evidence() {
        for case in [
            "foreign",
            "retired-provider",
            "old-turn",
            "other-turn",
            "missing-turn",
            "unexpected-turn",
            "started",
            "stopped",
            "ended",
            "unknown",
            "ready",
            "done",
            "error",
            "blocked",
            "ask",
            "exited",
            "unobserved",
        ] {
            let rig = Rig::new();
            rig.hook("SessionStart");
            rig.raw(serde_json::json!({"hook_event_name":"UserPromptSubmit", "session_id":"provider", "turn_id":"old"})).unwrap();
            rig.raw(serde_json::json!({"hook_event_name":"UserPromptSubmit", "session_id":"provider", "turn_id":"current"})).unwrap();
            let mut report = Report {
                provider: AgentCli::Claude,
                event: Event::ToolStarted,
                scope: Scope::Child,
                report_id: uuid::Uuid::new_v4().to_string(),
                provider_session: Some("provider".into()),
                turn: Some("current".into()),
                tool: Some("child-tool".into()),
                tool_name: Some("Bash".into()),
                reason: None,
                agent: None,
                subagents: None,
            };
            match case {
                "foreign" => report.provider_session = Some("foreign".into()),
                "old-turn" => report.turn = Some("old".into()),
                "other-turn" => report.turn = Some("other".into()),
                "missing-turn" => report.turn = None,
                "started" => report.event = Event::Started,
                "stopped" => report.event = Event::Stopped,
                "ended" => report.event = Event::Ended,
                "unknown" => report.scope = Scope::Unknown,
                "ready" => {
                    rig.hook("Stop");
                }
                "done" => {
                    rig.mutate(Action::Done {
                        outcome: Outcome::Success,
                    });
                }
                "error" => {
                    rig.hook("StopFailure");
                }
                "blocked" => {
                    rig.mutate(Action::Progress { blocked: true });
                }
                "ask" => {
                    rig.mutate(Action::AskOpened { id: "ask".into() });
                }
                _ => {}
            }
            let old = Instant::now() - Duration::from_secs(601);
            {
                let mut sessions = rig.owner.sessions.lock().unwrap();
                let invocation = sessions
                    .get_mut("session")
                    .unwrap()
                    .invocation
                    .as_mut()
                    .unwrap();
                invocation.evidence_at = Some(old);
                invocation.output_at = None;
                if case == "unexpected-turn" {
                    invocation.turn = None;
                }
                if case == "retired-provider" {
                    invocation
                        .retired_provider_sessions
                        .insert("provider".into());
                }
                if case == "unobserved" {
                    invocation.snapshot.observed = false;
                }
                if case == "exited" {
                    invocation.snapshot.exit = Some(Exit {
                        code: Some(0),
                        signal: None,
                        requested: false,
                    });
                }
            }
            let before = rig.owner.snapshot("session").unwrap();
            assert!(rig.report(report).is_err(), "{case}");
            assert_eq!(
                without_age(rig.owner.snapshot("session").unwrap()),
                without_age(before),
                "{case}"
            );
            assert_eq!(
                rig.owner.sessions.lock().unwrap()["session"]
                    .invocation
                    .as_ref()
                    .unwrap()
                    .evidence_at,
                Some(old),
                "{case}"
            );
        }
    }

    #[test]
    fn a_subagents_permission_prompt_is_answered_by_its_own_progress() {
        let rig = Rig::new();
        rig.hook("SessionStart");
        rig.hook("UserPromptSubmit");
        rig.raw(serde_json::json!({"hook_event_name": "PreToolUse", "session_id": "provider", "tool_use_id": "task", "tool_name": "Task"}))
            .unwrap();
        // The subagent asks; the notification names no subagent.
        assert_eq!(
            rig.notification("permission_prompt").unwrap().state,
            State::NeedsPermission
        );
        // Approved: the subagent's tool runs. Its events are a child's, yet the
        // main turn is no longer waiting.
        let answered = rig
            .raw(serde_json::json!({"hook_event_name": "PostToolUse", "session_id": "provider", "agent_id": "child", "tool_use_id": "c1", "tool_name": "Bash"}))
            .unwrap();
        assert_eq!(answered.state, State::Working);
        // Further child tools refresh age without changing the parent tool/lifecycle.
        let fresh = rig
            .raw(serde_json::json!({"hook_event_name": "PreToolUse", "session_id": "provider", "agent_id": "child", "tool_use_id": "c2", "tool_name": "Bash"}))
            .unwrap();
        assert_eq!(fresh.state, answered.state);
        assert_eq!(fresh.tool_open, answered.tool_open);
        assert_eq!(fresh.revision, answered.revision + 1);
        // A subagent finishing answers one too.
        rig.notification("permission_prompt").unwrap();
        let finished = rig
            .raw(serde_json::json!({"hook_event_name": "SubagentStop", "session_id": "provider", "agent_id": "child"}))
            .unwrap();
        assert_eq!(finished.state, State::Working);
    }

    #[test]
    fn a_named_permission_prompt_waits_for_its_own_tool_not_a_parallel_one() {
        let rig = Rig::new();
        rig.hook("SessionStart");
        rig.hook("UserPromptSubmit");
        let tool = |event: &str, id: &str, name: &str| {
            rig.raw(serde_json::json!({"hook_event_name": event, "session_id": "provider", "tool_use_id": id, "tool_name": name}))
                .unwrap()
        };
        tool("PreToolUse", "fetch", "WebFetch");
        tool("PreToolUse", "read", "Read");
        let asked = rig
            .raw(serde_json::json!({"hook_event_name": "PermissionRequest", "session_id": "provider", "tool_name": "WebFetch"}))
            .unwrap();
        assert_eq!(asked.state, State::NeedsPermission);
        // The same prompt's notification names no tool: the name is kept.
        rig.notification("permission_prompt").unwrap();
        assert_eq!(
            tool("PostToolUse", "read", "Read").state,
            State::NeedsPermission
        );
        assert_eq!(
            tool("PreToolUse", "read2", "Read").state,
            State::NeedsPermission
        );
        assert_eq!(
            tool("PostToolUse", "fetch", "WebFetch").state,
            State::Working
        );
        // A new prompt answers whatever was asked.
        rig.raw(serde_json::json!({"hook_event_name": "PermissionRequest", "session_id": "provider", "tool_name": "WebFetch"}))
            .unwrap();
        assert_eq!(rig.hook("UserPromptSubmit").state, State::Thinking);
    }

    #[test]
    fn a_one_shot_run_does_not_decay_and_a_vanished_cli_is_retired() {
        let owner = ActivityOwner::new();
        owner.register("shell", Some(std::process::id()));
        let mut cli = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let peer = process::inspect(cli.id()).unwrap();
        let (_, writer) = owner
            .claim_shell(peer, AgentCli::Claude, "socket", true)
            .unwrap();
        for (event, id) in [(Event::Started, "s"), (Event::Submitted, "u")] {
            owner
                .report(
                    &writer.token,
                    peer,
                    Report {
                        provider: AgentCli::Claude,
                        event,
                        scope: Scope::Main,
                        report_id: id.into(),
                        provider_session: Some("provider".into()),
                        turn: None,
                        tool: None,
                        tool_name: None,
                        reason: None,
                        agent: None,
                        subagents: None,
                    },
                    Instant::now() + Duration::from_millis(250),
                )
                .unwrap();
        }
        let later = Instant::now() + QUIET_TURN + Duration::from_secs(1);
        owner.decay(later);
        // `claude -p` thinks without drawing: still thinking.
        assert_eq!(owner.snapshots()[0].state, State::Thinking);
        assert!(owner.snapshots()[0].exit.is_none());
        // Killed with no shell marker to say so: retired at the next sweep.
        cli.kill().unwrap();
        cli.wait().unwrap();
        owner.decay(Instant::now());
        let retired = owner.snapshots()[0].clone();
        assert_eq!(retired.state, State::Idle);
        assert!(retired.exit.is_some());
        assert!(owner
            .report(
                &writer.token,
                peer,
                Report {
                    provider: AgentCli::Claude,
                    event: Event::Stopped,
                    scope: Scope::Main,
                    report_id: "late".into(),
                    provider_session: Some("provider".into()),
                    turn: None,
                    tool: None,
                    tool_name: None,
                    reason: None,
                    agent: None,
                    subagents: None,
                },
                Instant::now() + Duration::from_millis(250),
            )
            .is_err());
        // The shell's own process is never retired by the sweep.
        assert_eq!(owner.snapshots()[0].revision, retired.revision);
    }

    #[test]
    fn cx4_output_uses_the_captured_home_invocation_and_rebound_thread() {
        let host = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let owner = ActivityOwner::new();
        owner.configure_ingress(
            host.path().join("activity.sock"),
            host.path().join("activity-helper"),
        );
        let writer = owner
            .prepare_native("output", AgentCli::Codex, "unused")
            .unwrap();
        owner.register("output", Some(std::process::id())).unwrap();
        let mut engine = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let result = owner.bind_engine(
            &writer.token,
            process::inspect(std::process::id()).unwrap(),
            engine.id(),
            host.path().join("engine-test.sock"),
            Some(home.path().into()),
        );
        assert!(result.is_ok());
        let id = "01a15600-0001-7000-8000-000000000002";
        let dir = home.path().join("sessions/2026/10/19");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("rollout-test-{id}.jsonl")),format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\"}}}}\n{{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"own transcript\"}}]}}}}\n")).unwrap();
        let report = |event, session: &str| {
            owner
                .report_for_test(
                    &writer,
                    std::process::id(),
                    Report {
                        provider: AgentCli::Codex,
                        event,
                        scope: Scope::Main,
                        report_id: uuid::Uuid::new_v4().to_string(),
                        provider_session: Some(session.into()),
                        turn: None,
                        tool: None,
                        tool_name: None,
                        reason: None,
                        agent: None,
                        subagents: None,
                    },
                )
                .unwrap()
        };
        let initial = report(Event::Started, id);
        let output = owner.session_output("output", &writer.identity, initial.turn_generation);
        engine.kill().unwrap();
        engine.wait().unwrap();
        let output = output.expect("the matching captured native record");
        assert_eq!(output.session_id, id);
        assert!(output.raw.contains("own transcript"));
        assert!(owner
            .session_output("output", &writer.identity, initial.turn_generation + 1)
            .is_none());
        let mut wrong = writer.identity.clone();
        wrong.invocation_id = "other".into();
        assert!(owner
            .session_output("output", &wrong, initial.turn_generation)
            .is_none());
        // /new changes the thread while retaining its host invocation.
        let next = report(Event::Started, "01a15600-0000-7000-b000-000000000001");
        assert!(
            owner
                .session_output("output", &writer.identity, next.turn_generation)
                .is_none(),
            "cannot fall back to the old thread's existing record"
        );
    }

    /// Only its launch's own process binds an invocation's engine, once: an engine that
    /// descends from it, serving beside the host's socket. Once that engine is gone, its
    /// host reaches the CLI no more.
    #[test]
    fn an_engine_is_bound_only_by_its_launch_and_reaches_only_while_it_runs() {
        let owner = ActivityOwner::new();
        owner.configure_ingress("/host/activity.sock".into(), "/host/activity-helper".into());
        let writer = owner
            .prepare_native("S1", AgentCli::Codex, "unused")
            .unwrap();
        owner.register("S1", Some(std::process::id())).unwrap();
        let launch = process::inspect(std::process::id()).unwrap();
        let mut engine = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let mut other = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let beside = PathBuf::from("/host/engine-1.sock");
        let bind = |token: &str, peer, pid, endpoint: &str| {
            owner.bind_engine(token, peer, pid, endpoint.into(), None)
        };
        let stranger = unsafe { libc::getppid() } as u32;
        // Not its launch's: an engine that does not descend from it, a peer that is not the
        // invocation's process, a token that names none, or a socket outside the host.
        assert!(bind(&writer.token, launch, stranger, "/host/engine-1.sock").is_err());
        let not_launch = process::inspect(other.id()).unwrap();
        assert!(bind(&writer.token, not_launch, other.id(), "/host/engine-1.sock").is_err());
        assert!(bind(&"0".repeat(64), launch, engine.id(), "/host/engine-1.sock").is_err());
        assert!(bind(
            &writer.token,
            launch,
            engine.id(),
            "/elsewhere/engine-1.sock"
        )
        .is_err());
        // Nor a socket of the host's own.
        assert!(bind(&writer.token, launch, engine.id(), "/host/activity.sock").is_err());
        // Its launch's: bound once.
        bind(&writer.token, launch, engine.id(), "/host/engine-1.sock").unwrap();
        assert!(bind(&writer.token, launch, engine.id(), "/host/engine-2.sock").is_err());
        let reach = || {
            let sessions = owner.sessions.lock().unwrap();
            let invocation = sessions["S1"].invocation.as_ref().unwrap();
            invocation
                .reach(owner.adapter(AgentCli::Codex))
                .map(|reach| reach.endpoint)
        };
        // Not observed yet, it is not reached; once it is, it is.
        assert_eq!(reach(), None);
        owner
            .report_for_test(
                &writer,
                std::process::id(),
                Report {
                    provider: AgentCli::Codex,
                    event: Event::Started,
                    scope: Scope::Main,
                    report_id: "start".into(),
                    provider_session: Some("thread-1".into()),
                    turn: None,
                    tool: None,
                    tool_name: None,
                    reason: None,
                    agent: None,
                    subagents: None,
                },
            )
            .unwrap();
        assert_eq!(reach(), Some(beside));
        engine.kill().unwrap();
        engine.wait().unwrap();
        assert_eq!(reach(), None);
        other.kill().unwrap();
        other.wait().unwrap();
    }

    #[test]
    fn a_hand_typed_cli_claims_its_shell_by_ancestry_once_at_a_time() {
        let owner = ActivityOwner::new();
        // This test process stands in for the shell; its child for the CLI.
        owner.register("shell", Some(std::process::id()));
        let mut cli = std::process::Command::new("/bin/sleep")
            .arg("5")
            .spawn()
            .unwrap();
        let peer = process::inspect(cli.id()).unwrap();
        let (session, writer) = owner
            .claim_shell(peer, AgentCli::Claude, "socket", false)
            .unwrap();
        assert_eq!(session, "shell");
        assert_eq!(writer.socket, "socket");
        let started = owner
            .report(
                &writer.token,
                peer,
                Report {
                    provider: AgentCli::Claude,
                    event: Event::Started,
                    scope: Scope::Main,
                    report_id: "start".into(),
                    provider_session: Some("provider".into()),
                    turn: None,
                    tool: None,
                    tool_name: None,
                    reason: None,
                    agent: None,
                    subagents: None,
                },
                Instant::now() + Duration::from_millis(250),
            )
            .unwrap();
        assert!(started.observed);
        // A second CLI while the first runs (a nested agent) claims nothing.
        assert!(owner
            .claim_shell(peer, AgentCli::Claude, "socket", false)
            .is_err());
        cli.kill().unwrap();
        cli.wait().unwrap();
        // The shell itself is no CLI, and a process outside every shell is refused.
        let shell = process::inspect(std::process::id()).unwrap();
        assert!(owner
            .claim_shell(shell, AgentCli::Claude, "socket", false)
            .is_err());
        let stranger = ActivityOwner::new();
        let mut other = std::process::Command::new("/bin/sleep")
            .arg("5")
            .spawn()
            .unwrap();
        let peer = process::inspect(other.id()).unwrap();
        assert!(stranger
            .claim_shell(peer, AgentCli::Codex, "socket", false)
            .is_err());
        let (_, again) = owner
            .claim_shell(peer, AgentCli::Codex, "socket", false)
            .unwrap();
        assert_ne!(again.identity.invocation_id, writer.identity.invocation_id);
        other.kill().unwrap();
        other.wait().unwrap();
    }

    #[test]
    fn readiness_and_submit_connect_use_each_engines_captured_home() {
        use crate::agent_cli::orion;
        struct Outlet;
        impl crate::inbox_host::InboxOutlet for Outlet {
            fn fate(&self, _: &str, _: &str, _: crate::inbox_host::Fate) {}
            fn waiter(&self, _: crate::domain::InboxWaiterState) {}
            fn audience(&self, _: &str) -> Option<String> {
                None
            }
            fn has_tools(&self, _: &str) -> Option<bool> {
                Some(true)
            }
        }
        for index in 0..2 {
            let dir = tempfile::tempdir_in("/tmp").unwrap();
            let home = dir.path().join(format!("home-{index}"));
            std::fs::create_dir(&home).unwrap();
            let socket = dir.path().join("engine-orion.sock");
            let server = orion::serve(&socket);
            server.ready("orion-session");
            let owner = ActivityOwner::with_adapters(orion::adapters);
            owner.set_inbox_outlet(Arc::new(Outlet));
            owner.configure_ingress(dir.path().join("activity.sock"), PathBuf::new());
            let writer = owner
                .prepare_native("session", orion::ORION, "unused")
                .unwrap();
            owner.register("session", Some(std::process::id())).unwrap();
            let mut engine = std::process::Command::new("/bin/sleep")
                .arg("120")
                .spawn()
                .unwrap();
            owner
                .bind_engine(
                    &writer.token,
                    process::inspect(std::process::id()).unwrap(),
                    engine.id(),
                    socket,
                    Some(home.clone()),
                )
                .unwrap();
            // Orion normally waits for hooks; exercise the real readiness worker explicitly.
            owner.observe_engine(writer.token.clone());
            let deadline = Instant::now() + Duration::from_secs(5);
            while !owner.snapshot("session").unwrap().observed {
                assert!(
                    Instant::now() < deadline,
                    "readiness callback never completed"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(server.homes(), [Some(home.clone())], "readiness home");
            owner
                .inbox_mail("session", "mail".into(), "home-token".into())
                .unwrap();
            while server.handed().is_empty() {
                assert!(Instant::now() < deadline, "submit callback never completed");
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(
                server.homes(),
                [Some(home.clone()), Some(home)],
                "submit home"
            );
            assert_eq!(server.sessions(), [None, Some("orion-session".into())]);
            drop(owner);
            engine.wait().unwrap();
        }
    }
}

#[cfg(test)]
#[path = "owner/engine_loss_tests.rs"]
mod engine_loss_tests;
