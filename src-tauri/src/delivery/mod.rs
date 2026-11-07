// SPDX-License-Identifier: MIT
//! SW-AGR-9: the delivery core. One path for every wake typed into a live
//! session, whether a routine (SW-AGR-2) or a session timer (SW-AGR-3) asks
//! for it, and whether the session runs on this Mac or on a connected machine
//! (SPEC §9.3–§9.6, §9.10).
//!
//! The desktop never types a wake itself: the terminal server that runs the
//! session owns the idle-gated queue (`SessionWakeV1`, SW-AGR-4). This module
//! is the desktop's side of it:
//!
//! - **Host.** Resolve the machine's terminal server; an unreachable machine
//!   is `undelivered(machine_offline)`, an unknown one
//!   `undelivered(machine_unknown)`, and nothing is sent.
//! - **Authority** ([`authority`]): the authenticated caller, the
//!   Interactive gate, `allowWake` for sessions without permission checks,
//!   and static-only wake text.
//! - **Outcomes.** A reply, a pushed `TerminalDomainEvent::Wake`, or a
//!   reconnect catch-up all land in one place, de-duplicated by delivery id:
//!   the tab's wake history, the `wake-delivered` / `wake-outcome` events, and
//!   the subscribers that record runs (only for deliveries this desktop asked
//!   for).
//! - **Reconnect sync.** On every (re)connect of a terminal server the
//!   outcome log is read from a cursor persisted per machine; a restarted
//!   server is re-read from the start, and a queued delivery it lost is
//!   `undelivered(server_restarted)`.
//! - **Honest results.** A write that was not confirmed, locally too, is
//!   `unknown` and never re-sent (R5). An outdated terminal server is never
//!   offered as an update from inside a wake: the wake is
//!   `undelivered(unavailable)` with a notice.
//!
//! The core's callers are SW-AGR-2 (routine wakes: [`DeliveryService::deliver`],
//! `cancel`, [`DeliveryService::subscribe`], the queued read model) and
//! SW-AGR-3 (the timer MCP tools and "Remind this agent…", in [`wakeup`]:
//! `timer_*` with an authenticated [`authority::Caller`]). Until SW-AGR-2
//! lands, routine wakes are reached only by the tests.
#![cfg_attr(not(test), allow(dead_code))]

pub(crate) mod app;
pub(crate) mod authority;
pub(crate) mod events;
pub(crate) mod host;
pub(crate) mod read_model;
pub(crate) mod run_status;
pub(crate) mod store;
pub(crate) mod wakeup;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use codesense_pty_core::domain::PtySessionId;
use codesense_pty_core::session_wake::{
    DeliverRequest, DeliveryStatus, Originator, TimerInfo, TimerSchedule, TimerSpec,
    UndeliveredReason, WakeLimits, WakeOutcome,
};

use crate::terminal::session_server::session_wake::WakeCallError;
use authority::{AuthorityError, Caller, SessionPosture, SessionRef};
use events::{
    OutcomeSource, SyncHealth, WakeDeliveredEvent, WakeEventSink, WakeHistoryEntry, WakeOrigin,
    WakeOutcomeEvent,
};
use host::{HostLookup, HostResolver, WakeHost};
use run_status::WakeRunStatus;
use store::{pending_key, HostRecord, PendingDelivery, RecordDir};

/// How long after a backstop note for a session no other is posted.
const BACKSTOP_NOTE_QUIET_MS: u64 = 60 * 60 * 1000;
/// How long after an "update the terminal server" note for a machine no
/// other is posted.
const OUTDATED_NOTE_QUIET_MS: u64 = 24 * 60 * 60 * 1000;

/// A wake to type into one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WakeDelivery {
    pub target: SessionRef,
    /// Rendered with static values only ([`authority::render_wake_text`]).
    pub text: String,
    /// The ASCII run header ([`authority::run_header`]).
    pub header: Option<String>,
    /// What the scrollback annotation and the caption name.
    pub marker: String,
    pub max_wait_ms: u64,
    /// Stable per run (or per human request): a repeat is answered with the
    /// stored outcome and writes nothing.
    pub idempotency_key: String,
    pub origin: WakeOrigin,
    /// From the session's recorded launch ([`authority::posture_from_launch`]).
    pub posture: SessionPosture,
    /// The routine's wake limits (SW-AGR-13), enforced by the terminal
    /// server that runs the session.
    pub limits: Option<WakeLimits>,
}

/// What a delivery request came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeliveryReceipt {
    pub target: SessionRef,
    /// `None` when it never reached a terminal server, or its answer was lost.
    pub delivery_id: Option<String>,
    pub status: WakeRunStatus,
    /// What to tell a person about it (an offline or outdated host).
    pub notice: Option<String>,
}

/// A timer to set on a session (SW-AGR-3 validates the schedule).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TimerRequest {
    pub schedule: TimerSchedule,
    pub until_ms: Option<u64>,
    pub count: Option<u32>,
    pub text: String,
    pub name: String,
    pub created_by: String,
    pub max_wait_ms: u64,
    /// Refuse it now if the session cannot be observed (SW-AGR-3).
    pub require_observed: bool,
    /// SW-AGR-13: the least time between two of its deliveries.
    pub min_gap_ms: Option<u64>,
}

/// Why a request changed nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeliveryError {
    Authority(AuthorityError),
    /// The machine is not reachable now.
    Offline {
        machine: String,
    },
    /// No such machine here.
    UnknownMachine {
        machine: String,
    },
    /// The terminal server predates `SessionWakeV1`. Its message names the
    /// live sessions an update would end. Never produced inside a wake flow.
    UpdateRequired {
        message: String,
    },
    /// The terminal server cannot serve wakes now; updating would not help.
    Unavailable {
        message: String,
    },
    /// Refused, or not sent.
    Refused(String),
    /// Sent, but the answer was lost: it may have taken effect. Not retried.
    Unknown(String),
}

impl std::fmt::Display for DeliveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Authority(error) => write!(f, "{error}"),
            Self::Offline { machine } => write!(
                f,
                "{machine} is offline: wakes and timers there need it connected"
            ),
            Self::UnknownMachine { machine } => {
                write!(f, "{machine} is not a machine this desktop knows")
            }
            Self::UpdateRequired { message }
            | Self::Unavailable { message }
            | Self::Refused(message) => f.write_str(message),
            Self::Unknown(message) => write!(
                f,
                "{message} (it may have taken effect; check before asking again)"
            ),
        }
    }
}

impl From<AuthorityError> for DeliveryError {
    fn from(error: AuthorityError) -> Self {
        Self::Authority(error)
    }
}

/// A delivery outcome as the run recorder sees it (SW-AGR-2 subscribes).
///
/// **Upgrades.** A [`DeliveryService::deliver`] whose answer was lost returns
/// an `unknown` receipt that is *not* routed here: the delivery stays
/// pending, and its real outcome is routed when the server reports it. A
/// recorder that already wrote `unknown` from the receipt must accept that
/// later outcome for the same `idempotency_key` (`unknown` → `delivered`, for
/// instance); it is the one exception to "final statuses never change".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RoutedOutcome {
    pub target: SessionRef,
    pub delivery_id: String,
    pub idempotency_key: String,
    pub origin: WakeOrigin,
    pub status: WakeRunStatus,
    /// This desktop asked for it: only the originator records the run.
    pub ours: bool,
    pub source: OutcomeSource,
}

pub(crate) type OutcomeSubscriber = Arc<dyn Fn(&RoutedOutcome) + Send + Sync>;

/// How an outcome reached the desktop, and whether it is a re-read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Reply,
    Push,
    /// A reconnect catch-up (already de-duplicated by the cursor).
    Sync {
        replayed: bool,
    },
    Local,
}

impl Source {
    fn wire(self) -> OutcomeSource {
        match self {
            Self::Reply => OutcomeSource::Reply,
            Self::Push => OutcomeSource::Push,
            Self::Sync { .. } => OutcomeSource::Sync,
            Self::Local => OutcomeSource::Local,
        }
    }
    fn replayed(self) -> bool {
        matches!(self, Self::Sync { replayed: true })
    }
}

/// The delivery core. One per app (`app::install`), or one per test.
pub(crate) struct DeliveryService {
    resolver: Arc<dyn HostResolver>,
    sink: Arc<dyn WakeEventSink>,
    records: RecordDir,
    /// This desktop's install id (the routine host id): stamped on every
    /// request as the originator's `routine_host`, so outcomes it did not
    /// ask for are shown but never recorded as its runs. Never empty.
    routine_host: String,
    hosts: Mutex<HashMap<String, HostRecord>>,
    sync_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// The connection each machine was last caught up on: a delivery on a
    /// connection not caught up yet catches up first, so it is stamped with
    /// the server lifetime it actually reaches.
    synced_on: Mutex<HashMap<String, usize>>,
    health: Mutex<HashMap<String, SyncHealth>>,
    subscribers: Mutex<Vec<OutcomeSubscriber>>,
    /// SW-AGR-13: when each inbox note was last posted, by what it is about
    /// (a backstop trip per session, an outdated server per machine).
    noted: Mutex<HashMap<String, u64>>,
    /// This desktop's clock (epoch ms); tests move it.
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// One connection's identity, for "caught up on this connection yet?".
fn connection_of(host: &Arc<dyn WakeHost>) -> usize {
    Arc::as_ptr(host) as *const () as usize
}

/// How a terminal server's refusal of a delivery reads as a status: the ones
/// that are facts about the session or the store, not a bad request.
fn refusal_status(message: &str) -> Option<DeliveryStatus> {
    if message.contains("is not running") {
        Some(DeliveryStatus::Undelivered {
            reason: UndeliveredReason::SessionEnded,
        })
    } else if message.contains("has received its limit") {
        // A terminal server before SW-AGR-13: its 500-per-session cap.
        Some(DeliveryStatus::Undelivered {
            reason: UndeliveredReason::LimitReached,
        })
    } else if message.contains("wake store cannot be written")
        || message.contains(crate::terminal::session_server::session_wake::WAKE_LIMITS_TOO_OLD)
    {
        // Nothing was or will be typed: the server undid the queue whole,
        // or (SW-AGR-13) it predates wake limits and would have ignored
        // them, so nothing was sent to it.
        Some(DeliveryStatus::Undelivered {
            reason: UndeliveredReason::Unavailable,
        })
    } else {
        None
    }
}

impl DeliveryService {
    /// `routine_host` must be this install's stable id; an empty one is
    /// replaced by a per-process one so ownership never matches another
    /// desktop's empty stamp.
    pub(crate) fn new(
        resolver: Arc<dyn HostResolver>,
        sink: Arc<dyn WakeEventSink>,
        records: RecordDir,
        routine_host: String,
    ) -> Self {
        let routine_host = if routine_host.trim().is_empty() {
            format!("desktop-{}", uuid::Uuid::new_v4().simple())
        } else {
            routine_host
        };
        let hosts = records
            .load_all()
            .into_iter()
            .map(|record| (record.machine.clone(), record))
            .collect();
        Self {
            resolver,
            sink,
            records,
            routine_host,
            hosts: Mutex::new(hosts),
            sync_locks: Mutex::default(),
            synced_on: Mutex::default(),
            health: Mutex::default(),
            subscribers: Mutex::default(),
            noted: Mutex::default(),
            clock: Arc::new(now_ms),
        }
    }

    /// Ask the sink for an inbox note unless one about `about` went out in
    /// the last `quiet_ms` (SW-AGR-13).
    fn note_once(&self, about: String, quiet_ms: u64, event: events::WakeNoticeEvent) {
        let now = (self.clock)();
        {
            let mut noted = lock(&self.noted);
            if noted
                .get(&about)
                .is_some_and(|at| now.saturating_sub(*at) < quiet_ms)
            {
                return;
            }
            noted.insert(about, now);
        }
        self.sink.notice(&event);
    }

    /// Use `clock` as this desktop's time (tests).
    #[cfg(test)]
    pub(crate) fn with_clock(mut self, clock: Arc<dyn Fn() -> u64 + Send + Sync>) -> Self {
        self.clock = clock;
        self
    }

    /// Tell `subscriber` every outcome (the run recorder of SW-AGR-2).
    ///
    /// A subscriber runs on the thread that landed the outcome, which may be
    /// inside a catch-up holding that machine's (non-reentrant) catch-up
    /// lock: it must not call [`Self::sync`] or [`Self::deliver`] for the
    /// same machine, and should only record (hand anything longer to its own
    /// thread).
    pub(crate) fn subscribe(&self, subscriber: OutcomeSubscriber) {
        lock(&self.subscribers).push(subscriber);
    }

    fn originator(&self) -> Originator {
        Originator {
            // Stamped by the terminal server from the connection's viewer.
            desktop: String::new(),
            routine_host: Some(self.routine_host.clone()),
        }
    }

    /// Run `edit` on `machine`'s record (loaded on first use) and persist it.
    /// A failed write is surfaced in the machine's sync health.
    fn with_record<T>(&self, machine: &str, edit: impl FnOnce(&mut HostRecord) -> T) -> T {
        let (out, saved) = {
            let mut hosts = lock(&self.hosts);
            let record = hosts
                .entry(machine.to_string())
                .or_insert_with(|| self.records.load(machine));
            let out = edit(record);
            (out, self.records.save(machine, record))
        };
        let changed = {
            let mut health = lock(&self.health);
            let entry = health
                .entry(machine.to_string())
                .or_insert_with(|| SyncHealth {
                    machine_id: machine.to_string(),
                    ..SyncHealth::default()
                });
            let error = saved.err();
            let changed = entry.save_error != error;
            entry.save_error = error;
            changed.then(|| entry.clone())
        };
        if let Some(health) = changed {
            if let Some(error) = &health.save_error {
                log::warn!(target: "wake", "wake record for {machine} not saved: {error}");
            }
            self.sink.sync_health(&health);
        }
        out
    }

    /// Hand `delivery` to the terminal server that runs its session.
    pub(crate) fn deliver(
        &self,
        caller: &Caller,
        delivery: WakeDelivery,
    ) -> Result<DeliveryReceipt, DeliveryError> {
        authority::authorize_delivery(caller, &delivery.target, delivery.posture)?;
        // A routine firing is a wake flow: it never offers a terminal-server
        // update (that would end live sessions) and never fails outright.
        let wake_flow = *caller == Caller::RoutineHost;
        let machine = delivery.target.machine.clone();
        let host = match self.resolver.resolve(&machine) {
            HostLookup::Ready(host) => host,
            HostLookup::Offline { detail } => {
                return Ok(self.decided_here(
                    &delivery,
                    DeliveryStatus::Undelivered {
                        reason: UndeliveredReason::MachineOffline,
                    },
                    Some(format!(
                        "{machine} is offline ({detail}); the next fire tries again"
                    )),
                ))
            }
            HostLookup::Unknown => {
                return Ok(self.decided_here(
                    &delivery,
                    DeliveryStatus::Undelivered {
                        reason: UndeliveredReason::MachineUnknown,
                    },
                    Some(format!("{machine} is not a machine this desktop knows")),
                ))
            }
            HostLookup::Unavailable { machine, detail } => {
                let message = format!("Wakes are unavailable on {machine}: {detail}");
                return if wake_flow {
                    Ok(self.decided_here(
                        &delivery,
                        DeliveryStatus::Undelivered {
                            reason: UndeliveredReason::Unavailable,
                        },
                        Some(message),
                    ))
                } else {
                    Err(DeliveryError::Unavailable { message })
                };
            }
        };
        // Catch up on this connection first, so the server lifetime this
        // delivery is stamped with is the one it reaches (and a restart is
        // judged before, never after, it is queued).
        if lock(&self.synced_on).get(&machine) != Some(&connection_of(&host)) {
            if let Err(error) = self.sync_on(&machine, &host) {
                log::debug!(target: "wake", "catch-up before a wake to {machine}: {error}");
            }
        }
        let key = pending_key(&delivery.target.session, &delivery.idempotency_key);
        // Recorded before the call, so a push that beats the reply finds its
        // origin, and a lost reply leaves a trace the next sync settles.
        self.with_record(&machine, |record| {
            let boot = record.cursor.boot_id.clone();
            record
                .pending
                .entry(key.clone())
                .or_insert_with(|| PendingDelivery {
                    delivery_id: None,
                    session: delivery.target.session.clone(),
                    idempotency_key: delivery.idempotency_key.clone(),
                    marker: delivery.marker.clone(),
                    origin: delivery.origin.clone(),
                    deadline_ms: None,
                    host_boot_id: boot,
                    unconfirmed: true,
                    asked_at_ms: (self.clock)(),
                    max_wait_ms: delivery.max_wait_ms,
                });
        });
        let request = DeliverRequest {
            session: PtySessionId::new(&delivery.target.session),
            text: delivery.text.clone(),
            header: delivery.header.clone(),
            marker: delivery.marker.clone(),
            max_wait_ms: delivery.max_wait_ms,
            idempotency_key: delivery.idempotency_key.clone(),
            originator: self.originator(),
            limits: delivery.limits.clone(),
        };
        match host.deliver(request) {
            Ok(outcome) => {
                let receipt = DeliveryReceipt {
                    target: delivery.target.clone(),
                    delivery_id: Some(outcome.delivery_id.clone()).filter(|id| !id.is_empty()),
                    status: WakeRunStatus::of(&outcome.status),
                    // SW-AGR-13: the backstop's note, said once per trip.
                    notice: outcome.notice.clone(),
                };
                self.settle(&machine, outcome, Source::Reply);
                Ok(receipt)
            }
            // Sent, answer lost: it may be queued, or typed. `unknown`, kept
            // pending (unconfirmed) so its real outcome still lands and is
            // routed then (see [`RoutedOutcome`]), and never re-sent.
            Err(WakeCallError::Unknown(detail)) => Ok(DeliveryReceipt {
                target: delivery.target,
                delivery_id: None,
                status: WakeRunStatus::Unknown { detail },
                notice: None,
            }),
            Err(error) => {
                self.with_record(&machine, |record| record.pending.remove(&key));
                match error {
                    WakeCallError::Refused(message) => match refusal_status(&message) {
                        Some(status) => Ok(self.decided_here(&delivery, status, Some(message))),
                        None => Err(DeliveryError::Refused(message)),
                    },
                    WakeCallError::UpdateRequired { machine, .. } if wake_flow => Ok(self
                        .decided_here(
                            &delivery,
                            DeliveryStatus::Undelivered {
                                reason: UndeliveredReason::Unavailable,
                            },
                            Some(format!(
                                "The terminal server on {machine} is too old for wakes"
                            )),
                        )),
                    WakeCallError::Unavailable { .. } if wake_flow => Ok(self.decided_here(
                        &delivery,
                        DeliveryStatus::Undelivered {
                            reason: UndeliveredReason::Unavailable,
                        },
                        Some(error.to_string()),
                    )),
                    other => Err(call_error(other)),
                }
            }
        }
    }

    /// An outcome decided on this desktop: nothing reached a terminal server
    /// (or the server refused it outright).
    fn decided_here(
        &self,
        delivery: &WakeDelivery,
        status: DeliveryStatus,
        notice: Option<String>,
    ) -> DeliveryReceipt {
        // SW-AGR-13: a limited wake an outdated server was never sent is told
        // once per machine per day.
        if let Some(message) = notice.as_deref().filter(|m| {
            m.contains(crate::terminal::session_server::session_wake::WAKE_LIMITS_TOO_OLD)
        }) {
            self.note_once(
                format!("outdated\0{}", delivery.target.machine),
                OUTDATED_NOTE_QUIET_MS,
                events::WakeNoticeEvent {
                    kind: events::WakeNoticeKind::ServerOutdated,
                    machine_id: delivery.target.machine.clone(),
                    session_id: delivery.target.session.clone(),
                    origin: delivery.origin.clone(),
                    text: message.to_string(),
                },
            );
        }
        // Recorded as this desktop's, so the outcome keeps its origin.
        let key = pending_key(&delivery.target.session, &delivery.idempotency_key);
        self.with_record(&delivery.target.machine, |record| {
            record.pending.insert(
                key,
                PendingDelivery {
                    delivery_id: None,
                    session: delivery.target.session.clone(),
                    idempotency_key: delivery.idempotency_key.clone(),
                    marker: delivery.marker.clone(),
                    origin: delivery.origin.clone(),
                    deadline_ms: None,
                    host_boot_id: None,
                    unconfirmed: false,
                    asked_at_ms: (self.clock)(),
                    max_wait_ms: 0,
                },
            );
        });
        self.settle(
            &delivery.target.machine,
            WakeOutcome {
                notice: None,
                seq: 0,
                delivery_id: String::new(),
                idempotency_key: delivery.idempotency_key.clone(),
                session: PtySessionId::new(&delivery.target.session),
                marker: delivery.marker.clone(),
                originator: self.originator(),
                timer_id: None,
                status: status.clone(),
                host_boot_id: String::new(),
                at_ms: (self.clock)(),
            },
            Source::Local,
        );
        DeliveryReceipt {
            target: delivery.target.clone(),
            delivery_id: None,
            status: WakeRunStatus::of(&status),
            notice,
        }
    }

    /// Cancel this desktop's queued delivery (`undelivered(archived)`).
    pub(crate) fn cancel(
        &self,
        caller: &Caller,
        target: &SessionRef,
        idempotency_key: &str,
    ) -> Result<WakeRunStatus, DeliveryError> {
        authority::authorize_cancel(caller, target)?;
        let host = self.ready(&target.machine)?;
        let outcome = host
            .cancel(&PtySessionId::new(&target.session), idempotency_key)
            .map_err(call_error)?;
        let status = WakeRunStatus::of(&outcome.status);
        self.settle(&target.machine, outcome, Source::Reply);
        Ok(status)
    }

    /// Set a timer on `target`, owned by the authenticated caller.
    pub(crate) fn timer_set(
        &self,
        caller: &Caller,
        target: &SessionRef,
        timer: TimerRequest,
    ) -> Result<TimerInfo, DeliveryError> {
        let owner = authority::authorize_timer(caller, target)?;
        let host = self.ready(&owner.machine)?;
        let info = host
            .timer_set(TimerSpec {
                session: PtySessionId::new(&owner.session),
                schedule: timer.schedule,
                until_ms: timer.until_ms,
                count: timer.count,
                text: timer.text,
                name: timer.name,
                created_by: timer.created_by,
                max_wait_ms: timer.max_wait_ms,
                originator: self.originator(),
                require_observed: timer.require_observed,
                min_gap_ms: timer.min_gap_ms,
            })
            .map_err(call_error)?;
        self.timers_changed(&owner);
        Ok(info)
    }

    pub(crate) fn timer_list(
        &self,
        caller: &Caller,
        target: &SessionRef,
    ) -> Result<Vec<TimerInfo>, DeliveryError> {
        let owner = authority::authorize_timer(caller, target)?;
        self.ready(&owner.machine)?
            .timer_list(Some(&PtySessionId::new(&owner.session)))
            .map_err(call_error)
    }

    /// Whether the timer was active.
    pub(crate) fn timer_cancel(
        &self,
        caller: &Caller,
        target: &SessionRef,
        timer_id: &str,
    ) -> Result<bool, DeliveryError> {
        let owner = authority::authorize_timer(caller, target)?;
        let cancelled = self
            .ready(&owner.machine)?
            .timer_cancel(&PtySessionId::new(&owner.session), timer_id)
            .map_err(call_error)?;
        if cancelled {
            self.timers_changed(&owner);
        }
        Ok(cancelled)
    }

    /// Tell the window that `owner`'s timers changed (SW-AGR-6).
    fn timers_changed(&self, owner: &SessionRef) {
        self.sink.timers_changed(&events::SessionTimersChanged {
            machine_id: owner.machine.clone(),
            session_id: owner.session.clone(),
        });
    }

    fn ready(&self, machine: &str) -> Result<Arc<dyn WakeHost>, DeliveryError> {
        match self.resolver.resolve(machine) {
            HostLookup::Ready(host) => Ok(host),
            HostLookup::Offline { .. } => Err(DeliveryError::Offline {
                machine: machine.to_string(),
            }),
            HostLookup::Unknown => Err(DeliveryError::UnknownMachine {
                machine: machine.to_string(),
            }),
            HostLookup::Unavailable { machine, detail } => Err(DeliveryError::Unavailable {
                message: format!("Wakes and timers are unavailable on {machine}: {detail}"),
            }),
        }
    }

    /// A pushed outcome (`TerminalDomainEvent::Wake`) from `machine`.
    pub(crate) fn on_pushed(&self, machine: &str, outcome: WakeOutcome) {
        self.settle(machine, outcome, Source::Push);
    }

    /// Catch up on `machine`'s outcome log from its persisted cursor, and
    /// subscribe the connection to outcome pushes. Call on every (re)connect.
    pub(crate) fn sync(&self, machine: &str) -> Result<(), DeliveryError> {
        let host = self.ready(machine)?;
        self.sync_on(machine, &host)
    }

    /// [`Self::sync`], tried again after each of `backoff` while it fails and
    /// the machine stays reachable (`pause` waits; tests pass a no-op).
    pub(crate) fn sync_with_retry(
        &self,
        machine: &str,
        backoff: &[Duration],
        pause: impl Fn(Duration),
    ) -> Result<(), DeliveryError> {
        let mut result = self.sync(machine);
        for delay in backoff {
            match &result {
                Ok(()) => break,
                // Not reachable: the next connect catches up.
                Err(
                    DeliveryError::Offline { .. }
                    | DeliveryError::UnknownMachine { .. }
                    | DeliveryError::Authority(_),
                ) => break,
                Err(_) => {}
            }
            pause(*delay);
            result = self.sync(machine);
        }
        result
    }

    /// This desktop's catch-up health with `machine`.
    pub(crate) fn sync_health(&self, machine: &str) -> Option<SyncHealth> {
        lock(&self.health).get(machine).cloned()
    }

    fn note_sync(&self, machine: &str, result: &Result<(), DeliveryError>) {
        let health = {
            let mut health = lock(&self.health);
            let entry = health
                .entry(machine.to_string())
                .or_insert_with(|| SyncHealth {
                    machine_id: machine.to_string(),
                    ..SyncHealth::default()
                });
            match result {
                Ok(()) => {
                    entry.ok = true;
                    entry.failures = 0;
                    entry.last_error = None;
                    entry.last_ok_ms = Some(now_ms());
                }
                Err(error) => {
                    entry.ok = false;
                    entry.failures += 1;
                    entry.last_error = Some(error.to_string());
                }
            }
            entry.clone()
        };
        self.sink.sync_health(&health);
    }

    fn sync_on(&self, machine: &str, host: &Arc<dyn WakeHost>) -> Result<(), DeliveryError> {
        let result = self.catch_up(machine, host);
        if result.is_ok() {
            lock(&self.synced_on).insert(machine.to_string(), connection_of(host));
        }
        self.note_sync(machine, &result);
        result
    }

    fn catch_up(&self, machine: &str, host: &Arc<dyn WakeHost>) -> Result<(), DeliveryError> {
        let gate = lock(&self.sync_locks)
            .entry(machine.to_string())
            .or_default()
            .clone();
        let _serial = lock(&gate);
        let (mut cursor, replaying) = self.with_record(machine, |record| {
            // Never read before (a new or lost record): everything in the log
            // is a re-read, shown but not recorded as runs.
            let replaying = record.replaying || record.cursor.boot_id.is_none();
            (record.cursor.clone(), replaying)
        });
        let caught_up = host.sync(&mut cursor).map_err(call_error)?;
        let (fresh, lost) = self.with_record(machine, |record| {
            // Outcomes pushed while the read was in flight are already taken.
            let fresh: Vec<WakeOutcome> = caught_up
                .outcomes
                .into_iter()
                .filter(|outcome| !record.cursor.seen.contains(&outcome.delivery_id))
                .collect();
            let pushed: Vec<String> = record
                .cursor
                .seen
                .iter()
                .filter(|id| !cursor.seen.contains(id))
                .cloned()
                .collect();
            record.cursor = cursor.clone();
            for id in pushed {
                record.mark_seen(&id);
            }
            record.replaying = false;
            let reported: HashSet<String> = fresh
                .iter()
                .map(|o| pending_key(o.session.as_str(), &o.idempotency_key))
                .collect();
            let boot = cursor.boot_id.clone();
            // The newest host-clock time this read saw: a pending wake whose
            // deadline is older than it must be final by now.
            let newest = fresh.iter().map(|o| o.at_ms).max();
            let now = (self.clock)();
            let mut lost = Vec::new();
            for (key, pending) in &record.pending {
                if reported.contains(key) {
                    continue;
                }
                let from_old_lifetime = pending.host_boot_id != boot;
                let status = if caught_up.restarted && from_old_lifetime {
                    // Lost with the restart. Only a wake the old server
                    // confirmed it held is honestly `server_restarted`; one
                    // it never answered may have been typed: `unknown`.
                    if pending.delivery_id.is_some() && !pending.unconfirmed {
                        DeliveryStatus::Undelivered {
                            reason: UndeliveredReason::ServerRestarted,
                        }
                    } else {
                        DeliveryStatus::Unknown {
                            detail: "the terminal server restarted before confirming this wake"
                                .into(),
                        }
                    }
                } else if caught_up.overflow
                    && !from_old_lifetime
                    && pending
                        .deadline_ms
                        .zip(newest)
                        .is_some_and(|(deadline, newest)| deadline < newest)
                {
                    // Its outcome was dropped from the server's capped log.
                    DeliveryStatus::Unknown {
                        detail: "its outcome was dropped from the terminal server's log".into(),
                    }
                } else if pending.expired(now) {
                    // Past its wait and the grace with no outcome: a lost
                    // reply, or an outcome dropped on an earlier catch-up.
                    DeliveryStatus::Unknown {
                        detail: "no outcome arrived within its wait".into(),
                    }
                } else {
                    continue;
                };
                lost.push(WakeOutcome {
                    notice: None,
                    seq: 0,
                    delivery_id: pending.delivery_id.clone().unwrap_or_default(),
                    idempotency_key: pending.idempotency_key.clone(),
                    session: PtySessionId::new(&pending.session),
                    marker: pending.marker.clone(),
                    originator: self.originator(),
                    timer_id: None,
                    status,
                    host_boot_id: boot.clone().unwrap_or_default(),
                    at_ms: now,
                });
            }
            (fresh, lost)
        });
        if caught_up.overflow {
            log::warn!(target: "wake", "{machine}: some wake outcomes were dropped from its log before this desktop read them");
        }
        for outcome in fresh {
            self.settle(
                machine,
                outcome,
                Source::Sync {
                    replayed: replaying,
                },
            );
        }
        for outcome in lost {
            self.settle(machine, outcome, Source::Local);
        }
        Ok(())
    }

    /// Land one outcome: once per delivery id, whatever path it came by.
    fn settle(&self, machine: &str, outcome: WakeOutcome, source: Source) {
        let status = WakeRunStatus::of(&outcome.status);
        let session = outcome.session.as_str().to_string();
        let key = pending_key(&session, &outcome.idempotency_key);
        let replayed = source.replayed();
        let ours_by_stamp = !replayed
            && outcome.originator.routine_host.as_deref() == Some(self.routine_host.as_str());
        let settled = self.with_record(machine, |record| {
            if !status.is_final() {
                if let Some(pending) = record.pending.get_mut(&key) {
                    pending.delivery_id = Some(outcome.delivery_id.clone());
                    pending.unconfirmed = false;
                    if !outcome.host_boot_id.is_empty() {
                        pending.host_boot_id = Some(outcome.host_boot_id.clone());
                    }
                    pending.deadline_ms = match &outcome.status {
                        DeliveryStatus::Queued { deadline_ms } => Some(*deadline_ms),
                        _ => None,
                    };
                }
                let pending = record.pending.get(&key).cloned();
                return Some((pending, false));
            }
            let first = match source {
                // The answer to this request: final for it whatever else
                // happened. An answer without an id (an evicted key's
                // `unknown`) has nothing to be seen twice by; a repeated key
                // answered with an outcome already landed is not new.
                Source::Reply => {
                    let pending = record.pending.remove(&key);
                    let first =
                        record.mark_seen(&outcome.delivery_id) || outcome.delivery_id.is_empty();
                    if !first {
                        return None;
                    }
                    let origin = origin_of(pending.as_ref(), &outcome);
                    record.push_history(&session, history_row(&outcome, origin, &status));
                    return Some((pending, true));
                }
                Source::Push => {
                    record.mark_seen(&outcome.delivery_id) || outcome.delivery_id.is_empty()
                }
                // Already taken by the cursor.
                Source::Sync { .. } => {
                    record.mark_seen(&outcome.delivery_id);
                    true
                }
                Source::Local => true,
            };
            if !first {
                return None;
            }
            let pending = record.pending.remove(&key);
            record.push_history(
                &session,
                history_row(&outcome, origin_of(pending.as_ref(), &outcome), &status),
            );
            Some((pending, true))
        });
        let Some((pending, final_)) = settled else {
            return;
        };
        let origin = origin_of(pending.as_ref(), &outcome);
        // SW-AGR-13: the backstop's note, for any source (a routine, a timer,
        // a person), once per session per hour however often it trips.
        if let (Some(text), false) = (&outcome.notice, replayed) {
            if status
                == (WakeRunStatus::Undelivered {
                    reason: UndeliveredReason::RateLimited,
                })
            {
                self.note_once(
                    format!("backstop\0{machine}\0{session}"),
                    BACKSTOP_NOTE_QUIET_MS,
                    events::WakeNoticeEvent {
                        kind: events::WakeNoticeKind::Backstop,
                        machine_id: machine.to_string(),
                        session_id: session.clone(),
                        origin: origin.clone(),
                        text: text.clone(),
                    },
                );
            }
        }
        // A wake this desktop is waiting on is its own whatever path its
        // outcome takes, a re-read included; otherwise only a live outcome
        // stamped with this desktop counts.
        let ours = pending.is_some() || ours_by_stamp;
        let may_retry = status.may_retry() && !matches!(origin, WakeOrigin::Timer { .. });
        self.sink.outcome(&WakeOutcomeEvent {
            machine_id: machine.to_string(),
            session_id: session.clone(),
            delivery_id: outcome.delivery_id.clone(),
            idempotency_key: outcome.idempotency_key.clone(),
            marker: outcome.marker.clone(),
            origin: origin.clone(),
            status: status.clone(),
            may_retry,
            ours,
            source: source.wire(),
            replayed,
            at_ms: outcome.at_ms,
        });
        if let DeliveryStatus::Delivered { at_ms } = &outcome.status {
            self.sink.delivered(&WakeDeliveredEvent {
                machine_id: machine.to_string(),
                session_id: session.clone(),
                delivery_id: outcome.delivery_id.clone(),
                marker: outcome.marker.clone(),
                caption: events::caption(&outcome.marker),
                origin: origin.clone(),
                delivered_at_ms: *at_ms,
                source: source.wire(),
                replayed,
            });
        }
        // Shown, never recorded: a re-read, or a queued wake nobody here asked for.
        if pending.is_none() && (replayed || !final_) {
            return;
        }
        let routed = RoutedOutcome {
            target: SessionRef::new(machine, &session),
            delivery_id: outcome.delivery_id,
            idempotency_key: outcome.idempotency_key,
            origin,
            status,
            ours,
            source: source.wire(),
        };
        let subscribers = lock(&self.subscribers).clone();
        for subscriber in subscribers {
            subscriber(&routed);
        }
    }

    /// A tab's wake history, oldest first (its timer popover).
    pub(crate) fn history(&self, machine: &str, session: &str) -> Vec<WakeHistoryEntry> {
        let mut hosts = lock(&self.hosts);
        let record = hosts
            .entry(machine.to_string())
            .or_insert_with(|| self.records.load(machine));
        record
            .history
            .get(session)
            .map(|rows| rows.iter().cloned().collect())
            .unwrap_or_default()
    }
}

fn history_row(
    outcome: &WakeOutcome,
    origin: WakeOrigin,
    status: &WakeRunStatus,
) -> WakeHistoryEntry {
    WakeHistoryEntry {
        delivery_id: outcome.delivery_id.clone(),
        marker: outcome.marker.clone(),
        origin,
        status: status.clone(),
        at_ms: outcome.at_ms,
    }
}

/// Record `outcome` on its routine run in `store`, when it is a routine's
/// wake this desktop asked for (R7).
pub(crate) fn record_wake_run_in(
    store: &crate::routines::state::RoutineStateStore,
    outcome: &RoutedOutcome,
) {
    let WakeOrigin::Routine { run_id, .. } = &outcome.origin else {
        return;
    };
    if !outcome.ours {
        return;
    }
    if let Err(error) = crate::routines::finish::apply_wake_status(
        store,
        run_id,
        &outcome.target,
        &outcome.status,
        crate::routines::finish::now_ms(),
    ) {
        log::warn!(target: "wake", "wake run {run_id}: {error}");
    }
}

/// What woke the session: the request's origin when this desktop asked, else
/// what the outcome says.
fn origin_of(pending: Option<&PendingDelivery>, outcome: &WakeOutcome) -> WakeOrigin {
    match (pending, &outcome.timer_id) {
        (Some(pending), _) => pending.origin.clone(),
        (None, Some(timer)) => WakeOrigin::Timer {
            timer_id: Some(timer.clone()),
        },
        (None, None) => WakeOrigin::Other,
    }
}

fn call_error(error: WakeCallError) -> DeliveryError {
    match error {
        WakeCallError::UpdateRequired { .. } => DeliveryError::UpdateRequired {
            message: error.to_string(),
        },
        WakeCallError::Unavailable { .. } => DeliveryError::Unavailable {
            message: error.to_string(),
        },
        WakeCallError::Refused(message) => DeliveryError::Refused(message),
        WakeCallError::Unknown(message) => DeliveryError::Unknown(message),
    }
}
