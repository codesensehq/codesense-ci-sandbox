// SPDX-License-Identifier: MIT
//! Who may wake a session, and what a wake may say (SPEC §9.5).
//!
//! - **Caller binding.** A timer's owner, and the session a `routine_done`
//!   speaks for, is the session the *transport* authenticated: the reverse-MCP
//!   connection of a remote terminal, or, on this Mac, the kernel-verified
//!   process on the other end of the MCP socket, which must run inside the
//!   session it names. A `sessionId` parameter is only a claim checked against
//!   that; a forged one is refused.
//! - **Gate.** Every wake delivery and every timer goes through the
//!   Interactive tier gate, the same gate as `send_to_session`.
//! - **Bypass.** A wake into a session that skips permission checks (bypass
//!   or unattended) needs its agent to opt in with `allowWake: true`.
//! - **Static text only.** A wake carries the routine's task with `date` and
//!   `timestamp` filled in. Nothing another run or an event produced
//!   (`{{last_run_summary}}`, `{{trigger.*}}`, …) is ever rendered into text
//!   typed into a live session: those stay literal and are reported.

use std::collections::BTreeMap;

use codesense_pty_core::activity::peer::{self, PeerProcess};
use serde::{Deserialize, Serialize};

use crate::commands::interactive_gate::{GateRefusal, InteractiveGate};

/// The Interactive-tier verb every wake delivery is admitted under.
pub(crate) const WAKE_DELIVER_VERB: &str = "wake_deliver";
/// The Interactive-tier verb every timer change is admitted under.
pub(crate) const SCHEDULE_WAKEUP_VERB: &str = "schedule_wakeup";

/// The template values a wake may render.
pub(crate) const WAKE_STATIC_VARIABLES: [&str; 2] = ["date", "timestamp"];

/// One session, on the machine that runs it (`local` for this Mac).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionRef {
    pub machine: String,
    pub session: String,
}

impl SessionRef {
    pub(crate) fn new(machine: impl Into<String>, session: impl Into<String>) -> Self {
        Self {
            machine: machine.into(),
            session: session.into(),
        }
    }
}

/// Who asks for a delivery or a timer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Caller {
    /// An agent session the transport authenticated (see [`bind_caller`]).
    Session(SessionRef),
    /// The person at this desktop. Only a command from the app window can
    /// produce the [`HumanGesture`](crate::commands::wake::HumanGesture) it
    /// carries: nothing an agent or a dashboard reaches can act as the person.
    Human(crate::commands::wake::HumanGesture),
    /// This desktop's routine host, firing a routine run (SW-AGR-2).
    RoutineHost,
}

/// What the transport knows about an MCP caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TransportIdentity {
    /// A remote terminal's reverse-MCP connection: the tab it runs in, if any
    /// (a remote task has none), or the remote agent task it serves (SW-AGR-8:
    /// a task authenticates as its own task handle).
    Reverse {
        machine: String,
        session: Option<String>,
        task: Option<String>,
    },
    /// The local MCP socket's peer process (the agent's bridge).
    LocalPeer(PeerProcess),
    /// Nothing the transport can vouch for.
    Anonymous,
}

/// Why a wake or a timer was refused. Nothing was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AuthorityError {
    /// The named session is not the caller's own.
    Forged { claimed: String },
    /// The transport cannot say which session is calling.
    Unauthenticated,
    /// The claimed session is not running here, or its processes cannot be
    /// read, so the caller cannot be checked against it.
    Unverifiable { claimed: String },
    /// The Interactive tier refused.
    Gate(GateRefusal),
    /// The session skips permission checks and its agent did not set
    /// `allowWake: true`.
    BypassWithoutAllowWake { session: String },
    /// A timer only ever wakes its own session (R6); a routine host sets none.
    NotOwnSession,
}

impl std::fmt::Display for AuthorityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Forged { claimed } => write!(
                f,
                "denied: sessionId {claimed} is not the calling session; pass your own $CODESENSE_TAB_SESSION_ID"
            ),
            Self::Unauthenticated => f.write_str(
                "denied: CodeSense cannot tell which session is calling; this tool works only from a CodeSense terminal",
            ),
            Self::Unverifiable { claimed } => write!(
                f,
                "denied: CodeSense cannot verify that you run in session {claimed} (it is not running on this Mac, or its processes cannot be read); retry from that session's terminal"
            ),
            Self::Gate(refusal) => write!(f, "{refusal}"),
            Self::BypassWithoutAllowWake { session } => write!(
                f,
                "denied: session {session} runs without permission checks; its agent must set allowWake: true to be woken"
            ),
            Self::NotOwnSession => {
                f.write_str("denied: a timer can only wake the session that owns it")
            }
        }
    }
}

/// Bind `claimed` (a tool's `sessionId`) to the session the transport
/// authenticated. On this Mac the peer process must run inside `claimed`:
/// `root_of` answers the process the session runs (its shell or agent, or a
/// headless run's process), `None` when it cannot say.
pub(crate) fn bind_caller(
    identity: &TransportIdentity,
    claimed: &str,
    root_of: impl FnOnce(&str) -> Option<PeerProcess>,
) -> Result<SessionRef, AuthorityError> {
    let forged = || AuthorityError::Forged {
        claimed: claimed.to_string(),
    };
    match identity {
        TransportIdentity::Reverse {
            machine,
            session,
            task,
        } => match (session, task) {
            (Some(own), _) if own == claimed => Ok(SessionRef::new(machine, own)),
            (Some(_), _) => Err(forged()),
            // SW-AGR-8: a remote task's endpoint was minted for that task
            // alone, so it speaks for its task handle and nothing else.
            (None, Some(own)) if own == claimed => Ok(SessionRef::new(machine, own)),
            (None, Some(_)) => Err(forged()),
            (None, None) => Err(AuthorityError::Unauthenticated),
        },
        TransportIdentity::LocalPeer(peer) => match root_of(claimed) {
            Some(root) if peer::descends_from(*peer, root) => Ok(SessionRef::new(
                crate::commands::machines::LOCAL_MACHINE_ID,
                claimed,
            )),
            Some(_) => Err(forged()),
            None => Err(AuthorityError::Unverifiable {
                claimed: claimed.to_string(),
            }),
        },
        TransportIdentity::Anonymous => Err(AuthorityError::Unauthenticated),
    }
}

/// The process a local session runs: its terminal's shell or agent (from the
/// terminal host that runs it), else a headless run's process. `None` when
/// neither can be found or read.
pub(crate) fn session_root(
    domain: Option<&dyn codesense_pty_core::domain::TerminalDomain>,
    headless_pid: Option<u32>,
    session: &str,
) -> Option<PeerProcess> {
    let terminal = domain
        .and_then(|domain| {
            domain
                .live_child_pids(&[codesense_pty_core::domain::PtySessionId::new(session)])
                .ok()
        })
        .and_then(|pids| pids.into_iter().next().flatten());
    terminal.or(headless_pid).and_then(peer::inspect)
}

/// Whether a session can be typed into by a wake it did not ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionPosture {
    /// It runs without permission checks (bypass or unattended).
    pub bypass: bool,
    /// Its agent set `allowWake: true`.
    pub allow_wake: bool,
}

/// The shipped starters that skip permission checks. A user's agent is never
/// judged by its name: only by what its recorded launch ran.
const UNATTENDED_STARTERS: [&str; 2] = ["builtin/claude-unattended", "builtin/codex-unattended"];

impl SessionPosture {
    /// The posture of a session launched as `agent` (the definition at the
    /// revision its launch recorded) under the agent key `agent_key`. Only
    /// the shipped `*-unattended` starters count as unattended by key; being
    /// read-only, they can never opt in.
    pub(crate) fn of_agent(agent: Option<&crate::agents::model::Agent>, agent_key: &str) -> Self {
        let key = agent_key.split('@').next().unwrap_or_default();
        let unattended = UNATTENDED_STARTERS.contains(&key);
        Self {
            bypass: unattended || agent.is_some_and(|agent| agent.is_escalating()),
            allow_wake: !unattended && agent.is_some_and(|agent| agent.allow_wake),
        }
    }
}

/// The posture of a session from its recorded launch: `stamp` is the
/// `<key>@<revision>` the launch registry recorded, `pinned` loads that
/// revision of the agent (never the current definition), and
/// `launched_bypass` is whether the launch itself skipped permission checks
/// (a raw `permissionMode: bypass` launch).
///
/// It fails **closed**: a session with no recorded launch, or whose launched
/// revision cannot be loaded, counts as skipping checks, so a routine can
/// wake it only through an agent that opted in.
pub(crate) fn posture_from_launch(
    stamp: Option<&str>,
    launched_bypass: bool,
    pinned: impl FnOnce(&crate::agents::AgentKey, u32) -> Option<crate::agents::model::Agent>,
) -> SessionPosture {
    let unknown = SessionPosture {
        bypass: true,
        allow_wake: false,
    };
    let Some(stamp) = stamp else {
        return unknown;
    };
    let key = stamp.split('@').next().unwrap_or_default();
    if UNATTENDED_STARTERS.contains(&key) {
        return unknown;
    }
    let Some(agent) = stamp.rsplit_once('@').and_then(|(key, revision)| {
        let key = crate::agents::AgentKey::parse(key)?;
        pinned(&key, revision.parse().ok()?)
    }) else {
        return unknown;
    };
    let mut posture = SessionPosture::of_agent(Some(&agent), stamp);
    posture.bypass |= launched_bypass;
    posture
}

type Gate<'a> = &'a dyn Fn(&str) -> Result<(), GateRefusal>;

fn interactive_gate(verb: &str) -> Result<(), GateRefusal> {
    InteractiveGate::admit(verb)
}

/// Admit a delivery into `target` from `caller`.
pub(crate) fn authorize_delivery(
    caller: &Caller,
    target: &SessionRef,
    posture: SessionPosture,
) -> Result<(), AuthorityError> {
    authorize_delivery_through(&interactive_gate, caller, target, posture)
}

pub(crate) fn authorize_delivery_through(
    gate: Gate<'_>,
    caller: &Caller,
    target: &SessionRef,
    posture: SessionPosture,
) -> Result<(), AuthorityError> {
    gate(WAKE_DELIVER_VERB).map_err(AuthorityError::Gate)?;
    let own = match caller {
        Caller::Session(own) if own != target => return Err(AuthorityError::NotOwnSession),
        Caller::Session(_) => true,
        Caller::Human(_) | Caller::RoutineHost => false,
    };
    // A session's own timer is its own request, and a person may type into
    // their own terminal; a routine's wake into a session nobody supervises
    // needs the agent's consent.
    if !own && !matches!(caller, Caller::Human(_)) && posture.bypass && !posture.allow_wake {
        return Err(AuthorityError::BypassWithoutAllowWake {
            session: target.session.clone(),
        });
    }
    Ok(())
}

/// Admit cancelling a queued wake on `target`.
pub(crate) fn authorize_cancel(caller: &Caller, target: &SessionRef) -> Result<(), AuthorityError> {
    interactive_gate(WAKE_DELIVER_VERB).map_err(AuthorityError::Gate)?;
    match caller {
        Caller::Session(own) if own != target => Err(AuthorityError::NotOwnSession),
        _ => Ok(()),
    }
}

/// Admit a timer change on `target`; the owner it is bound to.
pub(crate) fn authorize_timer(
    caller: &Caller,
    target: &SessionRef,
) -> Result<SessionRef, AuthorityError> {
    authorize_timer_through(&interactive_gate, caller, target)
}

pub(crate) fn authorize_timer_through(
    gate: Gate<'_>,
    caller: &Caller,
    target: &SessionRef,
) -> Result<SessionRef, AuthorityError> {
    gate(SCHEDULE_WAKEUP_VERB).map_err(AuthorityError::Gate)?;
    match caller {
        Caller::Session(own) if own == target => Ok(own.clone()),
        Caller::Session(_) => Err(AuthorityError::Forged {
            claimed: target.session.clone(),
        }),
        Caller::Human(_) => Ok(target.clone()),
        Caller::RoutineHost => Err(AuthorityError::NotOwnSession),
    }
}

/// A routine task as a wake types it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WakeText {
    pub text: String,
    /// Tokens left literal: dynamic values never rendered into a wake, and
    /// anything with no value.
    pub withheld: Vec<String>,
}

/// Render `task` for a wake: only [`WAKE_STATIC_VARIABLES`] are filled from
/// `values`; every other token stays as written.
pub(crate) fn render_wake_text(task: &str, values: &BTreeMap<String, String>) -> WakeText {
    let statics: BTreeMap<String, String> = values
        .iter()
        .filter(|(name, _)| WAKE_STATIC_VARIABLES.contains(&name.as_str()))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let rendered = crate::routines::template::render(task, &statics);
    WakeText {
        text: rendered.text,
        withheld: rendered.unresolved,
    }
}

/// The ASCII run header a routine wake starts with:
/// `[routine: <name> | run <id>]`. Anything but printable ASCII becomes `?`,
/// and a `]` or `|` in the name cannot end the header early.
pub(crate) fn run_header(routine_name: &str, run_id: &str) -> String {
    format!(
        "[routine: {} | run {}]",
        header_ascii(routine_name),
        header_ascii(run_id)
    )
}

fn header_ascii(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            ']' | '|' | '[' => '?',
            c if c.is_ascii() && !c.is_ascii_control() => c,
            _ => '?',
        })
        .collect::<String>()
        .trim()
        .to_string()
}
