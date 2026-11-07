// SPDX-License-Identifier: MIT
//! An agent's inbox hook and its host: the hook runs the host's helper
//! (`--inbox-wait <cli> <event> <agent pid>`, see
//! [`inbox_hook_command`](crate::agent_cli::inbox_hook_command)), which asks this host's
//! activity ingress, with its CLI's writer token, to be handed the agent's mail notice.
//! The host admits it only from the agent process it named for that invocation
//! ([`ActivityOwner::inbox_admission`]); a helper that asks before the invocation is
//! committed is held for the commit, never ranked. The wait itself is the host inbox's
//! ([`crate::inbox_host::HostInbox::serve`]).

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use codesense_host_proto::inbox_wait::{self as wire, InboxWait};
use serde::{Deserialize, Serialize};

use super::owner::InboxAdmission;
use super::{process, ActivityOwner};
use crate::agent_cli::{AgentCli, HandOver, InboxDelivery};
use crate::inbox_waiters::{WaitArm, REASK_GRACE};

/// What an inbox hook's helper asks its host.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Ask {
    /// The CLI hook event that ran it.
    event: String,
    /// The CLI process that ran the hook (`$PPID`): the session's agent, or refused.
    agent: u32,
    /// The prompt the hook's CLI submitted, when the hook carries one: it settles a
    /// submitted notice whose delivery was uncertain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt: Option<String>,
    /// Native provider receipts; only a confirmation-capable admitted hook may settle them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    receipts: Vec<String>,
    /// The CLI session the hook names, when its input carries one: it tells the agent's
    /// own hooks from a nested CLI's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<String>,
}

/// How often a helper that asked before its invocation was committed is looked at again.
const COMMIT_POLL: Duration = Duration::from_millis(100);
/// How long the host may take to answer before a waiting helper looks whether its agent
/// is still there.
const AGENT_CHECK: Duration = Duration::from_secs(5);
/// The most of a hook's prompt a check carries: its head, where a wake key goes. The
/// host's envelope is bounded, and escaping can grow the prompt sixfold.
const MAX_PROMPT: usize = 2 * 1024;

/// Host side: serves the helper on `stream` (kernel-verified as `peer`), asking with
/// `token` as `ask` says.
pub(super) fn serve(
    owner: &ActivityOwner,
    stream: UnixStream,
    peer: process::Process,
    token: &str,
    ask: Ask,
) {
    let refuse = |stream: &UnixStream| {
        let _ = wire::send_lines(&mut &*stream, &wire::answer_lines(&InboxWait::Replaced, ""));
    };
    // An accepted socket inherits the listener's non-blocking mode on macOS; the wait
    // blocks.
    if stream.set_nonblocking(false).is_err()
        || stream.set_read_timeout(None).is_err()
        || stream.set_write_timeout(Some(AGENT_CHECK)).is_err()
    {
        return;
    }
    // A helper of an invocation not committed yet waits for the commit, then is admitted
    // or refused like any other; it is never weighed against another process.
    let until = Instant::now() + REASK_GRACE;
    let (session, agent, delivery, batch) = loop {
        match owner.inbox_admission(token, peer, ask.agent, ask.session.as_deref()) {
            InboxAdmission::Admitted {
                session,
                agent,
                delivery,
                batch,
            } => break (session, agent, delivery, batch),
            // Only a hook armed at a boot or a turn end, or a host-submitted CLI's check,
            // waits for the commit, a one-shot run's too (the desktop commits its PID just
            // after it starts, and its first look finds the mail already waiting); a
            // re-arm is answered at once.
            InboxAdmission::Pending { delivery }
                if commit_awaited(delivery, &ask.event)
                    && Instant::now() < until
                    && !crate::inbox_host::hung_up(&stream) =>
            {
                std::thread::sleep(COMMIT_POLL);
            }
            InboxAdmission::Pending { .. } | InboxAdmission::Refused => return refuse(&stream),
        }
    };
    let confirms = owner.inbox_confirmation(token, &ask.event);
    // Confirmation hooks only settle a submitted key; they never take a waiter place.
    if delivery.hand_over(&ask.event).is_none() && !confirms {
        return refuse(&stream);
    }
    if confirms {
        if let Some(outlet) = owner.inbox_outlet() {
            if let Some(prompt) = ask.prompt.as_deref() {
                owner.inbox().confirm_prompt(&*outlet, &session, prompt);
            }
            owner
                .inbox()
                .confirm_receipts(&*outlet, &session, &ask.receipts);
        }
        if delivery.hand_over(&ask.event).is_none() {
            return refuse(&stream);
        }
    }
    if delivery.hand_over(&ask.event) == Some(HandOver::Deferred) {
        return refuse(&stream);
    }
    let (arm, check) = match delivery {
        InboxDelivery::Waiter { replace_events, .. } => (
            WaitArm::from_hook(&ask.event, replace_events),
            batch || confirms,
        ),
        // A host-submitted CLI's hooks only look.
        InboxDelivery::HostSubmit { .. } => (WaitArm::IfAbsent, true),
        InboxDelivery::None => return refuse(&stream),
    };
    let Some(outlet) = owner.inbox_outlet() else {
        return refuse(&stream);
    };
    owner.inbox().serve(
        &*outlet,
        &session,
        arm,
        agent,
        check,
        ask.prompt.as_deref(),
        stream,
    );
}

/// What one ask of the host leaves the helper to do.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    /// A quiet hold: ask again.
    Again,
    /// The host could not be reached: ask again after a pause.
    Retry,
    /// A newer waiter took the place, the session ended, or the agent is gone.
    End,
    /// Renew before the CLI's finite hook timer can silently end it.
    Renew,
}

/// How long a waiter pauses between asks.
#[derive(Debug, Clone, Copy)]
struct Pauses {
    /// The first pause before it asks again after its host could not be reached; it
    /// doubles up to `retry_max`.
    retry: Duration,
    retry_max: Duration,
    /// A quiet answer sooner than this after asking did not hold: it pauses `retry_max`
    /// before asking again.
    min_ask: Duration,
}

const PAUSES: Pauses = Pauses {
    retry: Duration::from_secs(1),
    retry_max: Duration::from_secs(30),
    min_ask: Duration::from_secs(5),
};

/// Whether a hook of `delivery` armed at `event` waits for its invocation's commit: a
/// waiter that takes the place, or a host-submitted CLI's check.
fn commit_awaited(delivery: InboxDelivery, event: &str) -> bool {
    match delivery {
        InboxDelivery::Waiter { replace_events, .. } => {
            WaitArm::from_hook(event, replace_events) == WaitArm::Replace
        }
        InboxDelivery::HostSubmit { .. } => delivery.hand_over(event).is_some(),
        InboxDelivery::None => false,
    }
}

/// The event a waiter of `delivery` asks again under once its first ask ended quiet:
/// one that defers to a waiter already waiting (Claude's `PostToolUse`), so a newer
/// waiter armed meanwhile keeps its place.
fn reask_event(delivery: InboxDelivery) -> Option<&'static str> {
    match delivery {
        InboxDelivery::Waiter {
            events,
            replace_events,
            ..
        } => events
            .iter()
            .copied()
            .find(|event| !replace_events.contains(event)),
        InboxDelivery::HostSubmit { .. } | InboxDelivery::None => None,
    }
}

/// Helper side, from the hook's argv (`<cli> <event> <agent pid>`): waits for the
/// agent's mail notice at the host its tab runs on (a host-submitted CLI's hook only
/// looks once), and hands it to the CLI as its inbox port says. Returns the hook's exit
/// status: 2 for a notice handed over as a rewake, 0 for anything else. A CLI with no
/// CodeSense invocation (no activity token, as a run with no terminal) has no waiter.
pub fn wait_from_hook(args: &[std::ffi::OsString]) -> i32 {
    let arg = |at: usize| args.get(at).and_then(|value| value.to_str());
    let (Some(cli), Some(event), Some(agent)) = (
        arg(0).and_then(|cli| cli.parse::<AgentCli>().ok()),
        arg(1),
        match arg(2) {
            Some(pid) => pid.parse::<u32>().ok(),
            None => Some(std::os::unix::process::parent_id()),
        },
    ) else {
        return 0;
    };
    let adapter = crate::agent_cli::adapter(cli);
    let delivery = adapter.inbox();
    let confirms = adapter.confirms_notice(event);
    let Some(hand_over) = delivery
        .hand_over(event)
        .or_else(|| confirms.then_some(HandOver::AdditionalContext))
    else {
        return 0;
    };
    let Some(super::engine::Binding { socket, token, .. }) = super::engine::Binding::from_env()
    else {
        return 0;
    };
    let ask =
        |event: &str, prompt: Option<String>, session: Option<String>, receipts: Vec<String>| Ask {
            event: event.to_string(),
            agent,
            prompt,
            session,
            receipts,
        };
    let parent = std::os::unix::process::parent_id();
    let reask = reask_event(delivery).unwrap_or(event);
    let handed = match delivery {
        InboxDelivery::Waiter { .. } if confirms => {
            let input = adapter.read_check(&mut io::stdin(), MAX_PROMPT);
            if !input.main {
                return 0;
            }
            // A confirmation is not a new turn's inbox delivery.
            let _ = ask_once(
                &socket,
                &token,
                ask(event, input.prompt, input.session, input.receipts),
                parent,
            );
            None
        }
        InboxDelivery::Waiter { .. } => {
            // The CLI writes the hook's input (for a tool call, its whole output) to stdin:
            // it is read to the end, so that write never blocks on a waiter that holds for
            // minutes.
            // A confirming hook's process answers on that stdin instead.
            if hand_over != HandOver::RewakeConfirmed {
                std::thread::spawn(|| io::copy(&mut io::stdin(), &mut io::sink()));
            }
            // It names no session: a waiting CLI runs its hooks itself.
            let renewal = SystemTime::now()
                + Duration::from_secs(crate::agent_cli::INBOX_WAIT_TIMEOUT_SECS - 300);
            match wait_with_deadline(&socket, &token, parent, PAUSES, Some(renewal), |first| {
                ask(if first { event } else { reask }, None, None, Vec::new())
            }) {
                Ok(handed) => handed,
                Err(()) => {
                    eprintln!("{}", crate::agent_cli::WAITER_RENEWAL_LINE);
                    return 2;
                }
            }
        }
        InboxDelivery::HostSubmit { .. } => {
            let input = adapter.read_check(&mut io::stdin(), MAX_PROMPT);
            // A subagent's hook runs in its agent's own process, but the notice is the
            // agent's: handed to the subagent, the agent would never hear it. An input that
            // can't be read may be a subagent's: it is handed nothing, and the notice waits
            // for the next check or for the agent to be idle.
            if !input.main {
                return 0;
            }
            ask_once(
                &socket,
                &token,
                ask(event, input.prompt, input.session, input.receipts),
                parent,
            )
            .ok()
        }
        InboxDelivery::None => return 0,
    };
    handed.map_or(0, |(notice, notice_token, stream)| {
        hand(
            &notice,
            &notice_token,
            stream,
            event,
            hand_over,
            io::stdin(),
            crate::agent_cli::CONFIRMATION_WAIT,
        )
    })
}

/// Prints `notice` as `hand_over` says, tells the host it was printed, and returns the
/// hook's exit status. A confirmed hand-over tells the host only once `confirmation` (the
/// hook process's line on the helper's stdin) says the notice was delivered, within `wait`:
/// else it says nothing and the host keeps the notice for the next waiter.
fn hand(
    notice: &str,
    token: &str,
    mut stream: UnixStream,
    event: &str,
    hand_over: HandOver,
    confirmation: impl Read + Send + 'static,
    wait: Duration,
) -> i32 {
    let (line, on_stderr, status) = handed(notice, event, hand_over);
    let printed = if on_stderr {
        writeln!(io::stderr(), "{line}")
    } else {
        writeln!(io::stdout(), "{line}")
    };
    // A notice the CLI could not be handed is not said printed: the host keeps it.
    if printed.is_err() {
        return 0;
    }
    if hand_over == HandOver::RewakeConfirmed && !confirmed(confirmation, wait) {
        return 0;
    }
    let _ = wire::send_lines(&mut stream, &wire::printed_line(token));
    status
}

/// Whether the hook process confirmed the delivery within `wait`: its first line on
/// `confirmation` is [`WAITER_CONFIRMATION_LINE`](crate::agent_cli::WAITER_CONFIRMATION_LINE).
fn confirmed(confirmation: impl Read + Send + 'static, wait: Duration) -> bool {
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = io::BufRead::read_line(&mut io::BufReader::new(confirmation), &mut line);
        let _ = send.send(line);
    });
    receive
        .recv_timeout(wait)
        .is_ok_and(|line| line.trim() == crate::agent_cli::WAITER_CONFIRMATION_LINE)
}

/// How `notice` is handed to a CLI as `hand_over` says, from its hook armed at `event`:
/// the line, whether it goes to stderr (else stdout), and the hook's exit status.
fn handed(notice: &str, event: &str, hand_over: HandOver) -> (String, bool, i32) {
    match hand_over {
        HandOver::Rewake | HandOver::RewakeConfirmed => (notice.to_string(), true, 2),
        HandOver::Context => (serde_json::json!({"context": notice}).to_string(), false, 0),
        HandOver::Deferred => (String::new(), false, 0),
        HandOver::AdditionalContext => {
            let output = serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": event,
                    "additionalContext": notice,
                }
            });
            (output.to_string(), false, 0)
        }
    }
}

/// Asks until mail arrives: a quiet hold asks again (as `ask(false)` says), an
/// unreachable host after a pause. `None` once the wait ends without mail, or the
/// waiter's agent (its parent `parent`) is gone.
#[cfg(test)]
fn wait(
    socket: &Path,
    token: &str,
    parent: u32,
    pauses: Pauses,
    ask: impl Fn(bool) -> Ask,
) -> Option<(String, String, UnixStream)> {
    wait_with_deadline(socket, token, parent, pauses, None, ask).unwrap()
}

fn wait_with_deadline(
    socket: &Path,
    token: &str,
    parent: u32,
    pauses: Pauses,
    renewal: Option<SystemTime>,
    ask: impl Fn(bool) -> Ask,
) -> Result<Option<(String, String, UnixStream)>, ()> {
    let retrier = retrier_path(socket, token);
    let me = std::process::id().to_string();
    // A waiter that ends gives up its record as the newest retrier.
    let forget = || {
        if std::fs::read_to_string(&retrier).ok().as_deref() == Some(me.as_str()) {
            let _ = std::fs::remove_file(&retrier);
        }
    };
    let mut first = true;
    let mut retry = pauses.retry;
    let mut retrying = false;
    loop {
        if renewal_due(renewal, SystemTime::now()) {
            forget();
            return Err(());
        }
        if std::os::unix::process::parent_id() != parent {
            forget();
            return Ok(None);
        }
        let asked = Instant::now();
        let step = ask_until(socket, token, ask(first), parent, renewal);
        first = false;
        let step = match step {
            Ok(handed) => {
                forget();
                return Ok(Some(handed));
            }
            Err(step) => step,
        };
        if retrying && step != Step::Retry {
            retrying = false;
            retry = pauses.retry;
            forget();
        }
        match step {
            // A quiet answer arrives after a whole hold; an instant one would spin.
            Step::Again if asked.elapsed() < pauses.min_ask => std::thread::sleep(pauses.retry_max),
            Step::Again => {}
            Step::Retry => {
                // While the host cannot be reached nothing replaces an older waiter, and
                // every turn end arms another: only the newest keeps retrying. One that
                // cannot record itself keeps retrying rather than give up.
                retrying = true;
                let recorded = std::fs::write(&retrier, &me).is_ok();
                std::thread::sleep(retry);
                retry = (retry * 2).min(pauses.retry_max);
                if recorded && std::fs::read_to_string(&retrier).ok().as_deref() != Some(&me) {
                    return Ok(None);
                }
            }
            Step::End => return Ok(None),
            Step::Renew => {
                forget();
                return Err(());
            }
        }
    }
}

/// Where the newest of an invocation's waiters that is retrying an unreachable host
/// records itself: beside the host's socket, one file per invocation.
fn retrier_path(socket: &Path, token: &str) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    token.hash(&mut hasher);
    let dir = socket
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    dir.join(format!(".inbox-wait-{:016x}", hasher.finish()))
}

/// One ask of the host: the notice and its token, and the stream to say it was printed
/// on, or what to do next.
fn ask_once(
    socket: &Path,
    token: &str,
    ask: Ask,
    parent: u32,
) -> Result<(String, String, UnixStream), Step> {
    ask_until(socket, token, ask, parent, None)
}

fn renewal_due(deadline: Option<SystemTime>, now: SystemTime) -> bool {
    deadline.is_some_and(|deadline| now >= deadline)
}

fn ask_until(
    socket: &Path,
    token: &str,
    ask: Ask,
    parent: u32,
    renewal: Option<SystemTime>,
) -> Result<(String, String, UnixStream), Step> {
    let mut stream = super::ingress::ask_inbox(socket, token, ask).map_err(|_| Step::Retry)?;
    stream
        .set_read_timeout(Some(AGENT_CHECK))
        .map_err(|_| Step::Retry)?;
    let answer = host_line_until(&mut stream, parent, renewal)?;
    let notice_token = match wire::parse_answer(&answer) {
        Some(Ok(token)) => token.to_string(),
        Some(Err(InboxWait::Quiet)) => return Err(Step::Again),
        Some(Err(_)) | None => return Err(Step::End),
    };
    let notice = host_line_until(&mut stream, parent, renewal)?;
    // An agent that went away is told nothing: the host keeps the notice for the next.
    if std::os::unix::process::parent_id() != parent {
        return Err(Step::End);
    }
    Ok((notice, notice_token, stream))
}

/// The next line the host sends, read as it arrives; `Err(End)` once the waiter's agent
/// (its parent `parent`) is gone, `Err(Retry)` when the host closes the connection.
fn host_line_until(
    stream: &mut UnixStream,
    parent: u32,
    renewal: Option<SystemTime>,
) -> Result<String, Step> {
    let mut line = Vec::new();
    let mut byte = [0];
    loop {
        if renewal_due(renewal, SystemTime::now()) {
            return Err(Step::Renew);
        }
        match stream.read(&mut byte) {
            Ok(1) if byte[0] == b'\n' => return String::from_utf8(line).map_err(|_| Step::End),
            Ok(1) if line.len() < wire::MAX_LINE => line.push(byte[0]),
            Ok(1) => return Err(Step::End),
            Ok(_) => return Err(Step::Retry),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                if std::os::unix::process::parent_id() != parent {
                    return Err(Step::End);
                }
            }
            Err(_) => return Err(Step::Retry),
        }
    }
}

#[cfg(test)]
mod tests;
