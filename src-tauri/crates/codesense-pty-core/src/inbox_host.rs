// SPDX-License-Identifier: MIT
//! A host's inbox notices: every host that runs sessions (this Mac's session server, its
//! in-process fallback, and each machine's) hands its agents their mail notices here.
//!
//! - **The desktop pushes** a session's notice, reserved in its inbox under a token
//!   ([`HostInbox::mail`]); the host holds it for the session's agent.
//! - **A waiter hands it over** ([`HostInbox::serve`]): the host's helper, run by the
//!   agent's own inbox hook and admitted only from the agent process the host named,
//!   prints it and says so; the host then reports it told ([`InboxOutlet::fate`]), and
//!   only then is the mail marked told. A check (one look, never a wait) does the same.
//! - **A host submit** ([`HostInbox::submit`]) hands it to an idle agent with no waiter
//!   through its CLI's [`submit_notice`](AgentCliAdapter::submit_notice), over the engine
//!   the CLI runs on: accepted is told, refused is
//!   released (offered again later), and uncertain is held, never sent again blind, until
//!   a check carrying the agent's prompt shows the notice's wake key.
//! - **Waiter recovery** reuses those token rules for a guarded terminal submit of the
//!   one-line notice. Its prompt hook confirms the wake key before it counts as told.
//! - **Waiter states** are reported as they change ([`HostInbox::report`]), so the
//!   desktop knows whom new mail wakes, and checks a session's inbox whenever it arms.

mod receipts;

use std::collections::{HashMap, VecDeque};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use codesense_host_proto::inbox_wait::{self as wire, InboxWait};

use crate::agent_cli::{AgentCliAdapter, Channel, Notice, SubmitOutcome};
use crate::domain::{InboxWaiterState, PtySessionId, TerminalDomainEvent, TerminalEventSink};
use crate::inbox_waiters::{
    locked as lock, InboxWaiters, Owner, WaitArm, WaiterSignal, INBOX_WAIT_HOLD,
};

/// How long a waiter has to say it printed the notice it was handed.
const PRINT_DEADLINE: Duration = Duration::from_secs(10);
/// How often a waiting waiter looks whether its helper or its agent went away.
const LIVENESS_POLL: Duration = Duration::from_secs(1);
/// How long a notice waits for an agent with no CodeSense stream to open one (it is
/// starting, or its MCP server restarting) before it is handed over anyway.
const NO_STREAM_GRACE: Duration = Duration::from_secs(15);
/// How many tokens each session remembers as handed over (or submitted uncertainly), so
/// a push repeated after its answer was lost is answered again rather than told twice.
const KEPT: usize = 8;
/// How long an agent its host can submit to reads as one new mail reaches while it works:
/// longer than the sweeps that renew it.
const SUBMIT_GRACE: Duration = Duration::from_secs(30);

/// What became of a notice reserved under a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fate {
    /// The agent was handed it: its mail is told.
    Told,
    /// It did not reach the agent and had no effect: it may be offered again.
    Released,
    /// Whether it reached the agent is unknown: it must not be offered again until it is
    /// settled.
    Uncertain,
}

impl Fate {
    /// The event that reports `session`'s notice under `token` met this fate.
    pub fn event(self, session: &str, token: &str) -> TerminalDomainEvent {
        let (session, token) = (PtySessionId::new(session), token.to_string());
        match self {
            Self::Told => TerminalDomainEvent::InboxAck { session, token },
            Self::Released => TerminalDomainEvent::InboxRelease { session, token },
            Self::Uncertain => TerminalDomainEvent::InboxUncertain { session, token },
        }
    }

    /// The fate `event` reports, with its session and token.
    pub fn of(event: TerminalDomainEvent) -> Option<(PtySessionId, String, Self)> {
        match event {
            TerminalDomainEvent::InboxAck { session, token } => Some((session, token, Self::Told)),
            TerminalDomainEvent::InboxRelease { session, token } => {
                Some((session, token, Self::Released))
            }
            TerminalDomainEvent::InboxUncertain { session, token } => {
                Some((session, token, Self::Uncertain))
            }
            _ => None,
        }
    }
}

/// Where a host's inbox reports go.
pub trait InboxOutlet: Send + Sync {
    /// The notice reserved under `token` for `session`'s agent met `fate`.
    fn fate(&self, session: &str, token: &str, fate: Fate);
    /// Whom new mail for `state.session` reaches changed.
    fn waiter(&self, state: InboxWaiterState);
    /// Who hears of `session`'s notices (the desktop it is bound to), when that can change:
    /// a session told to another audience is told again.
    fn audience(&self, session: &str) -> Option<String>;
    /// Whether `session`'s agent has its CodeSense tools, so it can read the mail it is
    /// told of: a notice waits for them. `None`: the agent has no CodeSense stream, and a
    /// notice waits only [`NO_STREAM_GRACE`] for one.
    fn has_tools(&self, session: &str) -> Option<bool>;
    /// Uses the host's existing logger, so waiter deaths are observable without a new file.
    fn waiter_lifecycle(&self, session: &str, event: &str, reason: &str) {
        log::info!(target: "inbox", "session={session} waiter={event} reason={reason}");
    }
    /// A waiter died: attempt the guarded terminal path. Refused means no bytes written.
    fn recover(&self, _session: &str, _notice: &Notice<'_>) -> SubmitOutcome {
        SubmitOutcome::RefusedNoEffect
    }
}

/// The outlet of a host whose events go to its one client (the in-process host): every
/// report goes to its sink, and its agents' CodeSense is always there.
pub struct SinkOutlet(pub Arc<dyn TerminalEventSink>);

impl InboxOutlet for SinkOutlet {
    fn fate(&self, session: &str, token: &str, fate: Fate) {
        let _ = self.0.emit(fate.event(session, token));
    }

    fn waiter(&self, state: InboxWaiterState) {
        let _ = self.0.emit(TerminalDomainEvent::InboxWaiter { state });
    }

    fn audience(&self, _session: &str) -> Option<String> {
        None
    }

    fn has_tools(&self, _session: &str) -> Option<bool> {
        Some(true)
    }
}

/// How a waiter thread is woken: mail held for its session, or its place taken.
#[derive(Default)]
pub struct Signal {
    flags: Mutex<(bool, bool)>,
    changed: Condvar,
}

impl WaiterSignal for Signal {
    fn mail(&self) {
        lock(&self.flags).0 = true;
        self.changed.notify_all();
    }

    fn replaced(&self) {
        lock(&self.flags).1 = true;
        self.changed.notify_all();
    }
}

impl Signal {
    /// Waits up to `timeout` for mail or a replacement; whether it was replaced.
    fn wait(&self, timeout: Duration) -> bool {
        let flags = lock(&self.flags);
        let (mut flags, _) = self
            .changed
            .wait_timeout_while(flags, timeout, |(mail, replaced)| !*mail && !*replaced)
            .unwrap_or_else(|e| e.into_inner());
        flags.0 = false;
        flags.1
    }
}

/// A notice the desktop pushed that has not reached the agent yet.
struct Held {
    notice: String,
    token: String,
    /// A waiter is handing it over, or a submit is under way: no one else takes it.
    taken: bool,
    /// Its CLI refused it with no effect: it is submitted again only once its agent was
    /// seen working and idle again (a check can still take it meanwhile).
    refused: bool,
}

impl Held {
    /// Whether it awaits a submit: no one has it, and its CLI has not refused it since its
    /// agent was last seen working.
    fn awaits_submit(&self) -> bool {
        !self.taken && !self.refused
    }
}

/// What a repeated push of a notice already settled here calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repeat {
    /// It is held for the agent (or already was).
    Held,
    /// It was handed over already (its report was lost): report it told again.
    Told,
    /// Its submit was uncertain: report that again, and never send it again.
    Uncertain,
}

/// The inbox waiters of a host's sessions and the notices held for them.
#[derive(Default)]
pub struct HostInbox {
    wake_holds: Mutex<WakeHolds>,
    waiters: InboxWaiters<Signal>,
    /// The notice the session's desktop pushed and the agent was not handed yet. A newer
    /// push replaces it.
    held: Mutex<HashMap<String, Held>>,
    /// The tokens most recently handed over, by session.
    handed: Tokens,
    /// The tokens whose submit was uncertain, by session, until a prompt settles them.
    uncertain: Tokens,
    /// Provider receipts tied to reservations, including hooks that beat the RPC reply.
    receipts: Mutex<receipts::Receipts>,
    /// The sessions with a submit under way: one at a time, so a CLI is never handed two
    /// notices at once.
    submitting: Mutex<std::collections::HashSet<String>>,
    /// The sessions whose CLI the host submits notices to: whether its agent is idle.
    submitters: Mutex<HashMap<String, bool>>,
    /// What each session's desktop was last told of whom new mail reaches, and when.
    reported: Mutex<HashMap<String, (Reported, Instant)>>,
}

/// A versioned decision for one session. None lifts a hold.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WakeHold {
    pub session: String,
    pub reason: Option<String>,
}

#[derive(Default)]
struct WakeHolds {
    required: bool,
    sessions: HashMap<String, (u64, Option<String>)>,
}

impl WakeHolds {
    fn allows(&self, session: &str) -> bool {
        self.sessions
            .get(session)
            .map_or(!self.required, |(_, reason)| reason.is_none())
    }
}

/// What a session's desktop was told: whether a waiter waits, whether the agent reads
/// armed, and who heard it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reported {
    waiting: bool,
    armed: bool,
    audience: Option<String>,
}

/// The wake key a submitted notice is headed with: the CLI's own prompt hook shows it
/// back, which settles a submit whose outcome was uncertain.
pub fn wake_key(token: &str) -> String {
    format!("[inbox {token}]")
}

/// Each session's most recent tokens of one kind, oldest first.
type Tokens = Mutex<HashMap<String, VecDeque<String>>>;

/// Remembers `token` in `session`'s bounded list.
fn keep(map: &Tokens, session: &str, token: &str) {
    let mut map = lock(map);
    let kept = map.entry(session.to_string()).or_default();
    if kept.len() == KEPT {
        kept.pop_front();
    }
    kept.push_back(token.to_string());
}

impl HostInbox {
    /// Called before the local host accepts inbox work after startup.
    pub fn require_wake_holds(&self) {
        lock(&self.wake_holds).required = true;
    }

    pub fn set_wake_holds(&self, generation: u64, decisions: Vec<WakeHold>) {
        let mut holds = lock(&self.wake_holds);
        for decision in decisions {
            let stale = holds
                .sessions
                .get(&decision.session)
                .is_some_and(|(seen, _)| *seen >= generation);
            if stale {
                continue;
            }
            if decision.reason.is_none() {
                if let Some(held) = lock(&self.held).get_mut(&decision.session) {
                    held.refused = false;
                }
            }
            holds
                .sessions
                .insert(decision.session.clone(), (generation, decision.reason));
            self.waiters.mail_stored(&decision.session);
        }
    }

    pub fn while_wake_allowed<T>(&self, session: &str, submit: impl FnOnce() -> T) -> Option<T> {
        let holds = lock(&self.wake_holds);
        if !holds.allows(session) {
            return None;
        }
        Some(submit())
    }

    pub fn wake_allowed(&self, session: &str) -> bool {
        let holds = lock(&self.wake_holds);
        holds.allows(session)
    }

    /// The desktop pushed `session`'s notice reserved under `token`: held for its agent,
    /// replacing an older one. The same token again changes nothing; one already handed
    /// over or submitted uncertainly is not sent again.
    pub fn mail(&self, session: &str, notice: String, token: String) -> Repeat {
        {
            // Lock order: `held`, then `handed` (a hand-over moves a token across both).
            let mut held = lock(&self.held);
            if lock(&self.handed)
                .get(session)
                .is_some_and(|handed| handed.contains(&token))
            {
                return Repeat::Told;
            }
            if lock(&self.uncertain)
                .get(session)
                .is_some_and(|uncertain| uncertain.contains(&token))
            {
                return Repeat::Uncertain;
            }
            // A newer notice replaces the held one, even one being handed over: that
            // hand-over settles its own token only.
            if held
                .get(session)
                .is_some_and(|current| current.token == token)
            {
                return Repeat::Held;
            }
            held.insert(
                session.to_string(),
                Held {
                    notice,
                    token,
                    taken: false,
                    refused: false,
                },
            );
        }
        self.waiters.mail_stored(session);
        Repeat::Held
    }

    /// `session` ended: its waiter ends without waking it, its notice is dropped, and a
    /// desktop told it was armed hears it is not.
    pub fn forget(&self, session: &str, outlet: &dyn InboxOutlet) {
        lock(&self.wake_holds).sessions.remove(session);
        self.waiters.forget(session);
        lock(&self.held).remove(session);
        lock(&self.handed).remove(session);
        lock(&self.uncertain).remove(session);
        lock(&self.receipts).forget(session);
        lock(&self.submitters).remove(session);
        // Told under the reporters' lock, so a newer report never lands before it.
        let mut reported = lock(&self.reported);
        if reported
            .remove(session)
            .is_some_and(|(told, _)| told.waiting || told.armed)
        {
            outlet.waiter(InboxWaiterState {
                session: PtySessionId::new(session),
                waiting: false,
                grace_ms: None,
            });
        }
    }

    /// Whether new mail reaches `session`'s agent now (see [`InboxWaiterState`]): a waiter
    /// waits or re-arms within its grace, or the host can submit to it (waiting while it
    /// is idle).
    pub(crate) fn waiter_armed(&self, session: &str) -> bool {
        self.waiters.armed(session)
    }

    pub fn state(&self, session: &str) -> InboxWaiterState {
        let submit = lock(&self.submitters).get(session).copied();
        let armed = self.waiters.armed(session);
        let waiting = (armed && self.waiters.is_waiting(session)) || submit == Some(true);
        let millis = |left: Duration| u64::try_from(left.as_millis()).unwrap_or(u64::MAX);
        let grace_ms = if waiting {
            None
        } else if submit.is_some() {
            Some(millis(SUBMIT_GRACE))
        } else if armed {
            self.waiters
                .seen_until(session)
                .map(|until| millis(until.saturating_duration_since(Instant::now())))
        } else {
            None
        };
        InboxWaiterState {
            session: PtySessionId::new(session),
            waiting,
            grace_ms,
        }
    }

    /// Tells `session`'s desktop whom its new mail reaches when that changed since it was
    /// last told, or it is another desktop now. A working agent the host submits to is
    /// told again before its grace runs out.
    pub fn report(&self, outlet: &dyn InboxOutlet, session: &str) {
        // Read and told under one lock, so two reporters never send states out of order.
        let mut reported = lock(&self.reported);
        let state = self.state(session);
        let now = Reported {
            waiting: state.waiting,
            armed: state.waiting || state.grace_ms.is_some(),
            audience: outlet.audience(session),
        };
        let renew = lock(&self.submitters).get(session) == Some(&false);
        // A session never told of anything (one forgotten meanwhile) has nothing to tell.
        if !now.armed && !reported.contains_key(session) {
            return;
        }
        if let Some((told, at)) = reported.get(session) {
            if *told == now && !(renew && at.elapsed() >= SUBMIT_GRACE / 2) {
                return;
            }
        }
        reported.insert(session.to_string(), (now, Instant::now()));
        outlet.waiter(state);
    }

    /// Tells `session`'s desktop whom its new mail reaches, whatever it was told before
    /// (it may be another desktop now).
    pub fn report_again(&self, outlet: &dyn InboxOutlet, session: &str) {
        lock(&self.reported).remove(session);
        self.report(outlet, session);
    }

    /// Reports every session whose state may have changed with no waiter arming or
    /// ending: an agent exited, a grace ran out, an agent went idle.
    pub fn sweep(&self, outlet: &dyn InboxOutlet) {
        let mut sessions: std::collections::BTreeSet<String> =
            self.waiters.sessions().into_iter().collect();
        sessions.extend(lock(&self.reported).keys().cloned());
        sessions.extend(lock(&self.submitters).keys().cloned());
        for session in sessions {
            self.report(outlet, &session);
        }
    }

    /// Whether the host submits notices to `session`'s CLI, and if so whether its agent
    /// is idle now.
    pub fn set_submitter(&self, session: &str, idle: Option<bool>) {
        let mut submitters = lock(&self.submitters);
        match idle {
            Some(idle) => {
                // Seen idle again after working (or after its host could not reach it):
                // what it refused is offered again.
                if submitters.insert(session.to_string(), idle) != Some(true) && idle {
                    if let Some(held) = lock(&self.held).get_mut(session) {
                        held.refused = false;
                    }
                }
            }
            None => {
                submitters.remove(session);
            }
        }
    }

    /// Tracks `session` as one the host submits to, seen idle (it is about to be submitted
    /// to), unless it is tracked already.
    pub(crate) fn track_submitter(&self, session: &str) {
        lock(&self.submitters)
            .entry(session.to_string())
            .or_insert(true);
    }

    /// Whether a notice no one has taken is held for `session`.
    pub fn holds(&self, session: &str) -> bool {
        lock(&self.held)
            .get(session)
            .is_some_and(|held| !held.taken)
    }

    /// Whether a notice held for `session` awaits a submit: no one has taken it, no
    /// submit is under way, and its CLI has not refused it since its agent was last seen
    /// working.
    pub(crate) fn awaits_submit(&self, session: &str) -> bool {
        lock(&self.held)
            .get(session)
            .is_some_and(Held::awaits_submit)
            && !lock(&self.submitting).contains(session)
    }

    /// The sessions with a waiter, a grace, a submitter or a report.
    pub fn sessions(&self) -> Vec<String> {
        let mut sessions: std::collections::BTreeSet<String> =
            self.waiters.sessions().into_iter().collect();
        sessions.extend(lock(&self.submitters).keys().cloned());
        sessions.into_iter().collect()
    }

    /// Hands `session`'s held notice to its CLI through `adapter`'s
    /// [`submit_notice`](AgentCliAdapter::submit_notice), over the channel `connect` opens
    /// to its engine, unless a waiter or another submit has it or its CLI refused it
    /// since its agent was last seen working: accepted is told, refused stays held (for a
    /// check) and is released, and uncertain is kept aside, never sent again, until a
    /// prompt settles it. An engine that can't be reached takes nothing. Blocks while the
    /// CLI takes it; run it on its own thread.
    pub fn submit<'c>(
        &self,
        outlet: &dyn InboxOutlet,
        session: &str,
        adapter: &dyn AgentCliAdapter,
        connect: impl FnOnce() -> Option<Box<dyn Channel + 'c>>,
    ) {
        if !self.awaits_submit(session)
            || !self.wake_allowed(session)
            || !lock(&self.submitting).insert(session.to_string())
        {
            return;
        }
        match connect() {
            Some(mut channel) => {
                // Taken, the agent works on it: a newer notice pushed meanwhile stays held,
                // untaken, for its checks, and is submitted once a sweep sees it idle again.
                let outcome = self.submit_once(outlet, session, adapter, &mut *channel);
                if outcome.is_some_and(|outcome| outcome != SubmitOutcome::RefusedNoEffect) {
                    if let Some(idle) = lock(&self.submitters).get_mut(session) {
                        *idle = false;
                    }
                }
            }
            // Unreachable, it is asked again only as a refusal would be: once its agent was
            // seen working and idle again (or reached again), not every sweep.
            None => {
                if let Some(held) = lock(&self.held).get_mut(session) {
                    held.refused = true;
                }
            }
        }
        lock(&self.submitting).remove(session);
    }

    /// One submit of `session`'s held notice: its outcome, if one was held.
    fn submit_once(
        &self,
        outlet: &dyn InboxOutlet,
        session: &str,
        adapter: &dyn AgentCliAdapter,
        channel: &mut dyn Channel,
    ) -> Option<SubmitOutcome> {
        self.submit_via(outlet, session, true, |notice| {
            let outcome = adapter.submit_notice(channel, notice);
            if outcome == SubmitOutcome::Uncertain {
                if let Some(receipt) = channel.receipt_id() {
                    let early = {
                        let pending = lock(&self.uncertain);
                        pending
                            .get(session)
                            .filter(|tokens| tokens.iter().any(|token| token == notice.token))
                            .and_then(|_| {
                                lock(&self.receipts).register(session, receipt, notice.token)
                            })
                    };
                    if let Some(token) = early {
                        self.confirm_tokens(outlet, session, &[token]);
                    }
                }
            }
            outcome
        })
    }

    /// Submit through the host's guarded recovery path, retaining the same token rules.
    pub fn recover(&self, outlet: &dyn InboxOutlet, session: &str) {
        if !self.awaits_submit(session)
            || !self.wake_allowed(session)
            || !lock(&self.submitting).insert(session.into())
        {
            return;
        }
        if self.submit_via(outlet, session, false, |notice| {
            outlet.recover(session, notice)
        }) == Some(SubmitOutcome::RefusedNoEffect)
        {
            if let Some(held) = lock(&self.held).get_mut(session) {
                held.refused = false;
            }
        }
        lock(&self.submitting).remove(session);
    }

    fn submit_via(
        &self,
        outlet: &dyn InboxOutlet,
        session: &str,
        fence_submit: bool,
        submit: impl FnOnce(&Notice<'_>) -> SubmitOutcome,
    ) -> Option<SubmitOutcome> {
        // Hold updates fence the automatic submit itself, never human terminal input.
        let holds = lock(&self.wake_holds);
        if !holds.allows(session) {
            return None;
        }
        let (text, token) = lock(&self.held).get_mut(session).and_then(|held| {
            held.awaits_submit().then(|| {
                held.taken = true;
                (held.notice.clone(), held.token.clone())
            })
        })?;
        // Recovery takes its own permit at the host claim and final automatic write.
        // Never retain this permit across that re-entrant route.
        let permit = fence_submit.then_some(holds);
        let headed = format!("{}\n{text}", wake_key(&token));
        // The prompt hook can arrive before the write returns: register its key first.
        keep(&self.uncertain, session, &token);
        let outcome = submit(&Notice {
            text: &headed,
            token: &token,
        });
        drop(permit);
        match outcome {
            SubmitOutcome::Accepted => {
                forget_token(&self.uncertain, session, &token);
                self.untake(session, &token, Some(&self.handed));
                outlet.fate(session, &token, Fate::Told);
            }
            SubmitOutcome::RefusedNoEffect => {
                forget_token(&self.uncertain, session, &token);
                self.untake(session, &token, None);
                if let Some(held) = lock(&self.held)
                    .get_mut(session)
                    .filter(|held| held.token == token)
                {
                    held.refused = true;
                }
                outlet.fate(session, &token, Fate::Released);
                // A check that came meanwhile can still hand it over.
                self.waiters.mail_stored(session);
            }
            SubmitOutcome::Uncertain => {
                let mut held = lock(&self.held);
                if held.get(session).is_some_and(|held| held.token == token) {
                    held.remove(session);
                }
                if lock(&self.uncertain)
                    .get(session)
                    .is_some_and(|tokens| tokens.contains(&token))
                {
                    outlet.fate(session, &token, Fate::Uncertain);
                }
            }
        }
        Some(outcome)
    }

    /// `session`'s CLI submitted `prompt`: an uncertain submit whose [`wake_key`] the
    /// prompt holds reached the agent, and is told.
    pub(crate) fn confirm_prompt(&self, outlet: &dyn InboxOutlet, session: &str, prompt: &str) {
        let tokens: Vec<String> = lock(&self.uncertain)
            .get(session)
            .into_iter()
            .flatten()
            .filter(|token| prompt.trim_start().starts_with(&wake_key(token)))
            .cloned()
            .collect();
        self.confirm_tokens(outlet, session, &tokens);
    }

    /// An admitted hook names provider receipts, never reservation tokens or prompt text.
    pub(crate) fn confirm_receipts(&self, outlet: &dyn InboxOutlet, session: &str, ids: &[String]) {
        if ids.is_empty() {
            return;
        }
        let submitting = lock(&self.submitting).contains(session);
        let tokens = lock(&self.receipts).confirm(session, ids, submitting);
        self.confirm_tokens(outlet, session, &tokens);
    }

    fn confirm_tokens(&self, outlet: &dyn InboxOutlet, session: &str, shown: &[String]) {
        let settled = {
            // Same lock order as mail: a repeat sees the reservation held or settled.
            let _held = lock(&self.held);
            let mut uncertain = lock(&self.uncertain);
            let Some(tokens) = uncertain.get_mut(session) else {
                return;
            };
            let settled: Vec<String> = tokens
                .iter()
                .filter(|token| shown.contains(token))
                .cloned()
                .collect();
            tokens.retain(|token| !shown.contains(token));
            for token in &settled {
                keep(&self.handed, session, token);
            }
            settled
        };
        for token in settled {
            outlet.fate(session, &token, Fate::Told);
        }
    }

    /// Takes `session`'s held notice for a hand-over, when no one else has it and `ready`
    /// says its agent can read it.
    fn take(&self, session: &str, ready: impl FnOnce(&str) -> bool) -> Option<(String, String)> {
        if !self.wake_allowed(session) {
            return None;
        }
        let offered = lock(&self.held)
            .get(session)
            .filter(|held| !held.taken)
            .map(|held| held.token.clone())?;
        if !ready(&offered) {
            return None;
        }
        let mut held = lock(&self.held);
        held.get_mut(session)
            .filter(|held| !held.taken && held.token == offered)
            .map(|held| {
                held.taken = true;
                (held.notice.clone(), offered)
            })
    }

    /// Hands the taken notice to the helper on `socket`: told once it says it printed it,
    /// else held again for the next waiter.
    fn hand(
        &self,
        outlet: &dyn InboxOutlet,
        session: &str,
        socket: &UnixStream,
        notice: String,
        token: String,
    ) -> Option<InboxWait> {
        let sent = self.while_wake_allowed(session, || {
            let answer = wire::answer_lines(&InboxWait::Mail(notice.clone()), &token);
            wire::send_lines(&mut &*socket, &answer).map_err(|_| ())
        });
        if sent != Some(Ok(())) {
            self.untake(session, &token, None);
            return None;
        }
        match await_printed(socket, &token) {
            Ok(()) => {
                self.untake(session, &token, Some(&self.handed));
                outlet.fate(session, &token, Fate::Told);
                Some(InboxWait::Mail(notice))
            }
            Err(()) => {
                self.untake(session, &token, None);
                None
            }
        }
    }

    /// Ends the hand-over or submit of `session`'s notice taken under `token`: settled
    /// into `kept` (handed over, or uncertain), it is held no more; unsettled, it is held
    /// again for the next one. Across both at once: a repeated push in between would
    /// otherwise be held and handed over again.
    fn untake(&self, session: &str, token: &str, kept: Option<&Tokens>) {
        let mut held = lock(&self.held);
        // A newer notice pushed meanwhile stays as it is.
        if held.get(session).is_some_and(|held| held.token == token) {
            match kept {
                Some(_) => {
                    held.remove(session);
                }
                None => {
                    if let Some(held) = held.get_mut(session) {
                        held.taken = false;
                    }
                }
            }
        }
        if let Some(kept) = kept {
            keep(kept, session, token);
        }
    }

    /// Serves the waiter whose helper holds `socket`: armed at `arm` by `owner`, the
    /// session's agent process as its host named it. A `check` looks once and never waits
    /// or takes a place: it is handed what is held now; its `prompt` (the hook's own, when
    /// it has one) settles uncertain submits. Blocks until the wait ends; run it on its own
    /// thread.
    #[allow(clippy::too_many_arguments)]
    pub fn serve(
        &self,
        outlet: &dyn InboxOutlet,
        session: &str,
        arm: WaitArm,
        owner: Owner,
        check: bool,
        prompt: Option<&str>,
        socket: UnixStream,
    ) {
        if let Some(prompt) = prompt {
            self.confirm_prompt(outlet, session, prompt);
        }
        let end = if check {
            // An agent known to lack its tools is not told now; any other is.
            // Nothing to hand over ends it: a look never asks again.
            match self.take(session, |_| outlet.has_tools(session) != Some(false)) {
                Some((notice, token)) => self.hand(outlet, session, &socket, notice, token),
                None => Some(InboxWait::Replaced),
            }
        } else {
            self.wait(outlet, session, arm, owner, &socket)
        };
        match end {
            Some(outcome @ (InboxWait::Quiet | InboxWait::Replaced)) => {
                let _ = wire::send_lines(&mut &socket, &wire::answer_lines(&outcome, ""));
            }
            // Handed over already, or the helper went away.
            Some(InboxWait::Mail(_)) | None => {}
        }
    }

    fn wait(
        &self,
        outlet: &dyn InboxOutlet,
        session: &str,
        arm: WaitArm,
        owner: Owner,
        socket: &UnixStream,
    ) -> Option<InboxWait> {
        let signal = Arc::new(Signal::default());
        // A re-arm of the process already waiting (one after each tool call) defers.
        let Some(mut waiter) = self.waiters.register(session, arm, owner, signal.clone()) else {
            outlet.waiter_lifecycle(session, "exit", "replaced");
            return Some(InboxWait::Replaced);
        };
        outlet.waiter_lifecycle(session, "start", "armed");
        self.report(outlet, session);
        let deadline = Instant::now() + INBOX_WAIT_HOLD;
        let mut no_stream: Option<(String, Instant)> = None;
        let (end, reason) = loop {
            if !crate::activity::peer::runs(owner) {
                break (Some(InboxWait::Replaced), "parent-gone");
            }
            let taken = self.take(session, |token| match outlet.has_tools(session) {
                Some(tools) => {
                    no_stream = None;
                    tools
                }
                // The grace runs from when this waiter first saw this notice.
                None => match &no_stream {
                    Some((seen, since)) if seen == token => since.elapsed() >= NO_STREAM_GRACE,
                    _ => {
                        no_stream = Some((token.to_string(), Instant::now()));
                        false
                    }
                },
            });
            if let Some((notice, token)) = taken {
                let handed = self.hand(outlet, session, socket, notice, token);
                let reason = if matches!(handed, Some(InboxWait::Mail(_))) {
                    "mail-delivered"
                } else {
                    "delivery-error"
                };
                break (handed, reason);
            }
            if hung_up(socket) {
                break (None, "helper-disconnected (timeout/killed or renewal)");
            }
            let now = Instant::now();
            if now >= deadline {
                break (Some(InboxWait::Quiet), "quiet-reask");
            }
            if signal.wait(LIVENESS_POLL.min(deadline - now)) {
                break (Some(InboxWait::Replaced), "replaced");
            }
        };
        outlet.waiter_lifecycle(session, "exit", reason);
        waiter.woke = matches!(end, Some(InboxWait::Mail(_)));
        drop(waiter);
        self.report(outlet, session);
        end
    }
}

/// Hands the helper on `socket` the notice reserved under `token`, and waits for it to
/// say it printed it.
fn await_printed(socket: &UnixStream, token: &str) -> Result<(), ()> {
    socket
        .set_read_timeout(Some(PRINT_DEADLINE))
        .map_err(|_| ())?;
    let printed =
        codesense_host_proto::reverse_mcp::read_message(&mut &*socket, wire::MAX_LINE, |_| Ok(()))
            .map_err(|_| ())?;
    (printed == wire::printed_line(token))
        .then_some(())
        .ok_or(())
}

fn forget_token(tokens: &Mutex<HashMap<String, VecDeque<String>>>, session: &str, token: &str) {
    if let Some(tokens) = lock(tokens).get_mut(session) {
        tokens.retain(|held| held != token);
    }
}

/// Whether the helper on `socket` closed its end.
pub(crate) fn hung_up(socket: &UnixStream) -> bool {
    let mut byte = 0u8;
    // SAFETY: a one-byte peek into a local buffer on a socket this thread owns.
    let read = unsafe {
        libc::recv(
            socket.as_raw_fd(),
            (&mut byte as *mut u8).cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    let would_block = || {
        matches!(
            std::io::Error::last_os_error().kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
        )
    };
    // Closed, or failed (reset): either way no helper listens.
    read == 0 || read < 0 && !would_block()
}

#[cfg(test)]
mod tests;
