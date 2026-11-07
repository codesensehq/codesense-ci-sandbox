// SPDX-License-Identifier: MIT
//! The client side of `SessionWakeV1` (SW-AGR-4): hand a delivery or a timer
//! to the terminal server that runs the session, read its timers, and catch
//! up on the outcomes it decided while this app was away. The app never types
//! a wake itself, local or remote: the session server owns the queue (SPEC
//! §9.6). A server that predates the capability is refused by name, with the
//! live sessions an update would end, and nothing is sent to it.
//!
//! SW-AGR-9's delivery core (`crate::delivery`) is the caller; its own callers
//! (SW-AGR-2 wakes, SW-AGR-3 timers) are what reach the delivery and timer
//! methods outside tests.
#![cfg_attr(not(test), allow(dead_code))]

use codesense_pty_core::domain::TerminalDomain;
use codesense_pty_core::memorable_name::memorable_name_from_str;
use codesense_pty_core::session_wake::{
    DeliverRequest, OutcomeLog, TimerInfo, TimerSpec, WakeOutcome,
};

use super::*;

/// Why a wake call did not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WakeCallError {
    /// The terminal server predates `SessionWakeV1`. Nothing was sent; the
    /// fix is updating it, which ends the sessions it runs.
    UpdateRequired {
        machine: String,
        /// The live sessions an update would end, by their default tab name
        /// and id.
        live_sessions: Vec<String>,
    },
    /// The terminal server is current but its wake queue is not running
    /// (it failed to start, or stopped). Nothing was sent; updating would
    /// not help.
    Unavailable { machine: String },
    /// Refused, or never sent: nothing was queued.
    Refused(String),
    /// Sent, but the reply was lost: it may have been queued. Never resend
    /// blindly (R5); ask again with the same idempotency key.
    Unknown(String),
}

impl std::fmt::Display for WakeCallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UpdateRequired {
                machine,
                live_sessions,
            } => {
                write!(
                    f,
                    "Update the terminal server on {machine} to use wakes and timers."
                )?;
                match live_sessions.len() {
                    0 => f.write_str(" No sessions are running there."),
                    n => write!(
                        f,
                        " Updating ends its {n} live session{}: {}.",
                        if n == 1 { "" } else { "s" },
                        live_sessions.join(", ")
                    ),
                }
            }
            Self::Unavailable { machine } => write!(
                f,
                "Wakes and timers are unavailable on {machine}: its terminal server's wake queue is not running."
            ),
            Self::Refused(message) | Self::Unknown(message) => f.write_str(message),
        }
    }
}

impl From<CallError> for WakeCallError {
    fn from(error: CallError) -> Self {
        match error {
            CallError::NotSent(message) | CallError::Failed(message) => Self::Refused(message),
            CallError::Lost(message) => Self::Unknown(message),
        }
    }
}

/// How far this app has read a server's outcome log, and the deliveries it
/// has already taken (so a re-read after a restart reports none twice).
/// Persisted per server by the delivery core (SW-AGR-9).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct WakeCursor {
    pub boot_id: Option<String>,
    pub last_seq: u64,
    pub seen: std::collections::VecDeque<String>,
}

/// What the refusal of a limited wake to an outdated server says (and how
/// the delivery core knows it).
pub(crate) const WAKE_LIMITS_TOO_OLD: &str = "too old to enforce wake limits";

/// The first proto minor whose servers enforce wake limits (SW-AGR-13).
pub(crate) const WAKE_LIMITS_PROTO_MINOR: u16 = 13;

/// How many delivery ids a cursor remembers: the server's log cap.
const CURSOR_SEEN: usize = codesense_pty_core::session_wake::OUTCOME_LOG_CAP;

/// What [`SessionServerClient::wake_sync`] caught up on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WakeSync {
    pub outcomes: Vec<WakeOutcome>,
    /// Outcomes were dropped from the server's capped log before this read.
    pub overflow: bool,
    /// The server restarted since the cursor's read: deliveries this app
    /// knew as queued there and that are not in `outcomes` were lost with it
    /// (`undelivered(server_restarted)`).
    pub restarted: bool,
}

impl SessionServerClient {
    pub(crate) fn supports_session_wake(&self) -> bool {
        self.hello.supports(&Capability::SessionWakeV1)
    }

    /// Refuse, sending nothing that wakes or types, when the server predates
    /// the capability. Listing its live sessions is a read.
    fn require_session_wake(&self) -> Result<(), WakeCallError> {
        if self.supports_session_wake() {
            return Ok(());
        }
        // A build that knows the capability but was not offered it: the
        // queue failed there, and an update would not fix that.
        if self.hello.proto_minor >= 10 {
            return Err(WakeCallError::Unavailable {
                machine: self.hello.server_name.clone(),
            });
        }
        let mut live: Vec<String> = TerminalDomain::live_session_ids(self)
            .map(|sessions| {
                sessions
                    .into_iter()
                    .map(|id| format!("{} ({id})", memorable_name_from_str(id.as_str())))
                    .collect()
            })
            .unwrap_or_default();
        live.sort();
        Err(WakeCallError::UpdateRequired {
            machine: self.hello.server_name.clone(),
            live_sessions: live,
        })
    }

    /// SW-AGR-13: a server before proto minor 13 would ignore a routine's or
    /// timer's wake limits and type anyway, so nothing limited is sent to
    /// it.
    /// Whether this server enforces a routine's wake limits (SW-AGR-13).
    pub(crate) fn enforces_wake_limits(&self) -> bool {
        self.hello.proto_minor >= WAKE_LIMITS_PROTO_MINOR
    }

    fn require_wake_limits(&self) -> Result<(), WakeCallError> {
        if self.enforces_wake_limits() {
            return Ok(());
        }
        Err(WakeCallError::Refused(format!(
            "The terminal server on {} is {WAKE_LIMITS_TOO_OLD} (max_wakes, max_wakes_per_day, min_gap); update it to wake with limits there.",
            self.hello.server_name
        )))
    }

    /// Queue `request` on the server; its outcome so far (often final at
    /// once). A repeated key answers the stored outcome and writes nothing.
    pub(crate) fn wake_deliver(
        &self,
        request: DeliverRequest,
    ) -> Result<WakeOutcome, WakeCallError> {
        self.require_session_wake()?;
        if request.limits.is_some() {
            self.require_wake_limits()?;
        }
        match self.call(
            Request::WakeDeliver {
                request: Box::new(request),
            },
            REPLY_TIMEOUT,
        )? {
            Reply::WakeDelivery { outcome } => Ok(*outcome),
            other => Err(WakeCallError::Unknown(format!(
                "unexpected wake reply {other:?}"
            ))),
        }
    }

    /// Cancel this desktop's queued delivery `key` (`undelivered(archived)`);
    /// one already being written or final is answered as it stands.
    pub(crate) fn wake_cancel(
        &self,
        session: &PtySessionId,
        key: &str,
    ) -> Result<WakeOutcome, WakeCallError> {
        self.require_session_wake()?;
        match self.call(
            Request::WakeCancel {
                session: session.clone(),
                idempotency_key: key.to_string(),
            },
            REPLY_TIMEOUT,
        )? {
            Reply::WakeDelivery { outcome } => Ok(*outcome),
            other => Err(WakeCallError::Unknown(format!(
                "unexpected cancel reply {other:?}"
            ))),
        }
    }

    pub(crate) fn wake_timer_set(&self, spec: TimerSpec) -> Result<TimerInfo, WakeCallError> {
        self.require_session_wake()?;
        if spec.min_gap_ms.is_some() {
            self.require_wake_limits()?;
        }
        match self.call(
            Request::WakeTimerSet {
                spec: Box::new(spec),
            },
            REPLY_TIMEOUT,
        )? {
            Reply::WakeTimer { timer } => Ok(*timer),
            other => Err(WakeCallError::Unknown(format!(
                "unexpected timer reply {other:?}"
            ))),
        }
    }

    pub(crate) fn wake_timer_list(
        &self,
        session: Option<&PtySessionId>,
    ) -> Result<Vec<TimerInfo>, WakeCallError> {
        self.require_session_wake()?;
        match self.call(
            Request::WakeTimerList {
                session: session.cloned(),
            },
            REPLY_TIMEOUT,
        )? {
            Reply::WakeTimers { timers } => Ok(timers),
            other => Err(WakeCallError::Refused(format!(
                "unexpected timer list reply {other:?}"
            ))),
        }
    }

    /// Whether the timer was active.
    pub(crate) fn wake_timer_cancel(
        &self,
        session: &PtySessionId,
        timer_id: &str,
    ) -> Result<bool, WakeCallError> {
        self.require_session_wake()?;
        match self.call(
            Request::WakeTimerCancel {
                session: session.clone(),
                timer_id: timer_id.to_string(),
            },
            REPLY_TIMEOUT,
        )? {
            Reply::Flag { value } => Ok(value),
            other => Err(WakeCallError::Unknown(format!(
                "unexpected timer cancel reply {other:?}"
            ))),
        }
    }

    /// The outcomes logged after `since`; from now on this connection also
    /// receives each outcome as a `TerminalDomainEvent::Wake`.
    pub(crate) fn wake_outcomes(&self, since: u64) -> Result<OutcomeLog, WakeCallError> {
        self.require_session_wake()?;
        match self.call(Request::WakeOutcomes { since }, REPLY_TIMEOUT)? {
            Reply::WakeOutcomes { log } => Ok(*log),
            other => Err(WakeCallError::Refused(format!(
                "unexpected outcome log reply {other:?}"
            ))),
        }
    }

    /// Catch up from `cursor` on (re)connect and advance it. After a server
    /// restart the log is read from its start, and outcomes already taken
    /// (by delivery id) are left out.
    pub(crate) fn wake_sync(&self, cursor: &mut WakeCursor) -> Result<WakeSync, WakeCallError> {
        catch_up(cursor, |since| self.wake_outcomes(since))
    }
}

/// [`SessionServerClient::wake_sync`] over any outcome-log reader `read`
/// (`since` → the log after it).
pub(crate) fn catch_up(
    cursor: &mut WakeCursor,
    mut read: impl FnMut(u64) -> Result<OutcomeLog, WakeCallError>,
) -> Result<WakeSync, WakeCallError> {
    let mut log = read(cursor.last_seq)?;
    let restarted = cursor
        .boot_id
        .as_ref()
        .is_some_and(|boot| *boot != log.host_boot_id);
    if restarted {
        log = read(0)?;
    }
    let outcomes: Vec<WakeOutcome> = log
        .outcomes
        .into_iter()
        .filter(|outcome| !cursor.seen.contains(&outcome.delivery_id))
        .collect();
    for outcome in &outcomes {
        cursor.seen.push_back(outcome.delivery_id.clone());
        if cursor.seen.len() > CURSOR_SEEN {
            cursor.seen.pop_front();
        }
    }
    cursor.boot_id = Some(log.host_boot_id);
    cursor.last_seq = log.last_seq;
    Ok(WakeSync {
        outcomes,
        overflow: log.overflow,
        restarted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesense_host_proto::{read_frame, read_hello, write_frame, Census, PeerHello};
    use codesense_pty_core::session_wake::{
        DeliveryStatus, Originator, TimerSchedule, UndeliveredReason,
    };
    use codesense_session_server::testing::{
        registration, sh, short_root, wait_until, RecordingSink, TestServer,
    };
    use std::os::unix::net::UnixListener;

    /// A terminal server of an older build at this major: greets with
    /// `hello`, answers what `serve` answers, and returns every call it got.
    fn old_server(
        root: &Path,
        hello: Hello,
        serve: impl Fn(&Request) -> Result<Reply, String> + Send + 'static,
    ) -> std::thread::JoinHandle<Vec<Request>> {
        let listener = UnixListener::bind(socket_path(root)).unwrap();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            write_frame(&mut &stream, &hello.frame().unwrap()).unwrap();
            write_frame(&mut &stream, &Census { live_sessions: 2 }.frame()).unwrap();
            assert!(matches!(
                read_hello(&mut &stream).unwrap(),
                PeerHello::Compatible(_)
            ));
            let mut seen = Vec::new();
            while let Ok(Some(frame)) = read_frame(&mut &stream) {
                let Ok(Message::Call { rid, request }) = frame.decode_json::<Message>() else {
                    continue;
                };
                let result = if matches!(request, Request::Listen) {
                    Ok(Reply::Done)
                } else {
                    serve(&request)
                };
                seen.push(request);
                let reply = Frame::control(&Message::Reply { rid, result }).unwrap();
                write_frame(&mut &stream, &reply).unwrap();
            }
            seen
        })
    }

    fn request(session: &str, key: &str) -> DeliverRequest {
        DeliverRequest {
            session: PtySessionId::new(session),
            text: "check the inbox".into(),
            header: Some("[routine: triage | run 1]".into()),
            marker: "Triage".into(),
            max_wait_ms: 60_000,
            idempotency_key: key.into(),
            originator: Originator::default(),
            limits: None,
        }
    }

    fn connect(root: &Path) -> Arc<SessionServerClient> {
        SessionServerClient::connect(
            root,
            Arc::new(RecordingSink::default()),
            Arc::default(),
            None,
        )
        .unwrap()
    }

    // AC6: an old server is refused by name, naming the live sessions an
    // update would end, and is sent nothing that wakes or types.
    #[test]
    fn an_old_server_reports_the_update_and_is_sent_no_wake() {
        let root = short_root();
        let hello = Hello {
            proto_major: codesense_host_proto::PROTO_MAJOR,
            proto_minor: 9,
            server_name: "codesense-test-1".into(),
            instance_id: instance_id(root.path()),
            capabilities: vec![Capability::Terminals, Capability::ActivityV1],
            pid: 1,
            emulator: Some(codesense_host_proto::EmulatorHello::current()),
            ..Hello::default()
        };
        let server = old_server(root.path(), hello, |request| match request {
            Request::LiveSessionIds => Ok(Reply::Sessions {
                sessions: [PtySessionId::new("s-1"), PtySessionId::new("s-2")]
                    .into_iter()
                    .collect(),
            }),
            other => Err(format!("unexpected {other:?}")),
        });
        let client = connect(root.path());
        assert!(!client.supports_session_wake());
        let error = client.wake_deliver(request("s-1", "k1")).unwrap_err();
        let WakeCallError::UpdateRequired {
            machine,
            live_sessions,
        } = &error
        else {
            panic!("{error:?}")
        };
        assert_eq!(machine, "codesense-test-1");
        assert_eq!(live_sessions.len(), 2);
        let message = error.to_string();
        assert!(
            message.starts_with("Update the terminal server on codesense-test-1"),
            "{message}"
        );
        let named = format!("{} (s-1)", memorable_name_from_str("s-1"));
        assert!(message.contains(&named), "{message}");
        assert!(message.contains("2 live sessions"), "{message}");
        assert!(matches!(
            client.wake_timer_set(TimerSpec {
                session: PtySessionId::new("s-1"),
                schedule: TimerSchedule::After {
                    after_ms: 60_000,
                    every_ms: None
                },
                until_ms: None,
                count: None,
                text: "x".into(),
                name: "t".into(),
                created_by: "human".into(),
                max_wait_ms: 60_000,
                originator: Originator::default(),
                require_observed: false,
                min_gap_ms: None,
            }),
            Err(WakeCallError::UpdateRequired { .. })
        ));
        drop(client);
        let requests = server.join().unwrap();
        assert!(
            requests
                .iter()
                .all(|request| matches!(request, Request::Listen | Request::LiveSessionIds)),
            "{requests:?}"
        );
    }

    // L3: a server of this minor that does not offer the capability failed
    // to start its queue: "unavailable", not "update", and nothing is sent.
    #[test]
    fn a_current_server_without_its_queue_reports_unavailable_and_is_sent_nothing() {
        let root = short_root();
        let hello = Hello {
            proto_major: codesense_host_proto::PROTO_MAJOR,
            proto_minor: 10,
            server_name: "codesense-test-1".into(),
            instance_id: instance_id(root.path()),
            capabilities: vec![Capability::Terminals],
            pid: 1,
            emulator: Some(codesense_host_proto::EmulatorHello::current()),
            ..Hello::default()
        };
        let server = old_server(root.path(), hello, |other| {
            Err(format!("unexpected {other:?}"))
        });
        let client = connect(root.path());
        let error = client.wake_deliver(request("s-1", "k1")).unwrap_err();
        assert_eq!(
            error,
            WakeCallError::Unavailable {
                machine: "codesense-test-1".into()
            }
        );
        drop(client);
        let requests = server.join().unwrap();
        assert!(
            requests.iter().all(|r| matches!(r, Request::Listen)),
            "{requests:?}"
        );
    }

    // SW-AGR-13: a server of minor 12 offers wakes but would ignore a
    // routine's limits: a limited wake (or a timer with a min_gap) is refused
    // before anything is sent; an unlimited one still goes.
    #[test]
    fn a_server_before_wake_limits_is_sent_no_limited_wake() {
        let root = short_root();
        let hello = Hello {
            proto_major: codesense_host_proto::PROTO_MAJOR,
            proto_minor: 12,
            server_name: "codesense-test-1".into(),
            instance_id: instance_id(root.path()),
            capabilities: vec![Capability::Terminals, Capability::SessionWakeV1],
            pid: 1,
            emulator: Some(codesense_host_proto::EmulatorHello::current()),
            ..Hello::default()
        };
        let server = old_server(root.path(), hello, |request| match request {
            Request::WakeDeliver { request } => Ok(Reply::WakeDelivery {
                outcome: Box::new(WakeOutcome {
                    seq: 1,
                    delivery_id: "d1".into(),
                    idempotency_key: request.idempotency_key.clone(),
                    session: request.session.clone(),
                    marker: String::new(),
                    originator: Originator::default(),
                    timer_id: None,
                    status: DeliveryStatus::Delivered { at_ms: 1 },
                    host_boot_id: "b".into(),
                    at_ms: 1,
                    notice: None,
                }),
            }),
            other => Err(format!("unexpected {other:?}")),
        });
        let client = connect(root.path());
        assert!(client.supports_session_wake());
        let mut limited = request("s-1", "k1");
        limited.limits = Some(codesense_pty_core::session_wake::WakeLimits {
            budget: "routine:o/t/r#1".into(),
            max_wakes: Some(3),
            max_wakes_per_day: None,
            min_gap_ms: None,
            gap: Default::default(),
        });
        let error = client.wake_deliver(limited).unwrap_err();
        let WakeCallError::Refused(message) = &error else {
            panic!("{error:?}")
        };
        assert!(
            message.contains("too old to enforce wake limits"),
            "{message}"
        );
        let gapped = TimerSpec {
            session: PtySessionId::new("s-1"),
            schedule: TimerSchedule::After {
                after_ms: 60_000,
                every_ms: Some(60_000),
            },
            until_ms: None,
            count: None,
            text: "x".into(),
            name: "t".into(),
            created_by: "human".into(),
            max_wait_ms: 60_000,
            originator: Originator::default(),
            require_observed: false,
            min_gap_ms: Some(120_000),
        };
        assert!(matches!(
            client.wake_timer_set(gapped),
            Err(WakeCallError::Refused(_))
        ));
        assert!(client.wake_deliver(request("s-1", "k2")).is_ok());
        drop(client);
        let requests = server.join().unwrap();
        let sent: Vec<_> = requests
            .iter()
            .filter(|r| !matches!(r, Request::Listen))
            .collect();
        assert_eq!(sent.len(), 1, "only the unlimited wake: {sent:?}");
    }

    // The client methods against a real server, and the catch-up after a
    // reconnect: a timer that fired while no app was connected.
    #[test]
    fn wake_methods_and_reconnect_sync_against_a_real_server() {
        let server = TestServer::start();
        server.enable_wake();
        let client = connect(server.root());
        assert!(client.supports_session_wake());
        server.track("w1");
        client
            .spawn(sh("cat"), registration("w1", None, false))
            .unwrap();
        let mut cursor = WakeCursor::default();
        let first = client.wake_sync(&mut cursor).unwrap();
        assert!(first.outcomes.is_empty() && !first.restarted);
        let outcome = client.wake_deliver(request("w1", "k1")).unwrap();
        assert_eq!(
            outcome.status,
            DeliveryStatus::Undelivered {
                reason: UndeliveredReason::Unobservable
            }
        );
        assert_eq!(client.wake_deliver(request("w1", "k1")).unwrap(), outcome);
        assert_eq!(
            client.wake_cancel(&PtySessionId::new("w1"), "k1").unwrap(),
            outcome,
            "a final delivery is answered as it stands"
        );
        let timer = client
            .wake_timer_set(TimerSpec {
                session: PtySessionId::new("w1"),
                schedule: TimerSchedule::After {
                    after_ms: 200,
                    every_ms: None,
                },
                until_ms: None,
                count: None,
                text: "ping".into(),
                name: "ping".into(),
                created_by: "human".into(),
                max_wait_ms: 60_000,
                originator: Originator::default(),
                require_observed: false,
                min_gap_ms: None,
            })
            .unwrap();
        assert_eq!(
            client.wake_timer_list(None).unwrap(),
            std::slice::from_ref(&timer)
        );
        drop(client);
        let wake = server.host().wake().unwrap();
        assert!(wait_until(|| wake.timers(None).is_empty()));
        let back = connect(server.root());
        let caught_up = back.wake_sync(&mut cursor).unwrap();
        assert!(!caught_up.restarted && !caught_up.overflow);
        let keys: Vec<_> = caught_up
            .outcomes
            .iter()
            .map(|outcome| outcome.timer_id.clone())
            .collect();
        assert_eq!(keys, [None, Some(timer.timer_id.clone())]);
        assert!(back.wake_sync(&mut cursor).unwrap().outcomes.is_empty());
        assert!(!back
            .wake_timer_cancel(&PtySessionId::new("w1"), &timer.timer_id)
            .unwrap());
        // A cursor from another server lifetime is flagged.
        let mut stale = WakeCursor {
            boot_id: Some("an-older-boot".into()),
            last_seq: 1,
            seen: [caught_up.outcomes[0].delivery_id.clone()].into(),
        };
        let restarted = back.wake_sync(&mut stale).unwrap();
        assert!(restarted.restarted);
        assert_eq!(
            restarted.outcomes.len(),
            1,
            "read from the start, the one already taken left out"
        );
        // A current server whose queue is not running is not "update".
        let error = WakeCallError::Unavailable {
            machine: "m".into(),
        };
        assert!(!error.to_string().contains("Update"));
    }
}
