// SPDX-License-Identifier: MIT
//! `SessionWakeV1`: the wire of idle-gated deliveries into a session and of
//! the session timers that produce them, both owned by the host that runs the
//! session.
//!
//! A delivery is queued on the host and written into the session's PTY only
//! when the host observes the agent idle, as one bracketed paste followed by
//! one submit. Its terminal outcome is reported as an event to the clients
//! that asked for them and kept in a capped log for the ones that were away.
//! Every time on this wire is a duration or a host-clock instant the host
//! reported itself; a client's wall clock never crosses it.

use serde::{Deserialize, Serialize};

use crate::domain::PtySessionId;

/// The largest delivered payload (run header plus text), in bytes, after
/// sanitizing.
pub const MAX_TEXT_BYTES: usize = 8 * 1024;
/// The longest a delivery may wait for its session to become idle.
pub const MAX_WAIT_LIMIT_MS: u64 = 24 * 60 * 60 * 1000;
/// The default wait (`policy.max_wait`).
pub const DEFAULT_MAX_WAIT_MS: u64 = 30 * 60 * 1000;
/// The longest idempotency key, marker or name.
pub const MAX_LABEL_LEN: usize = 256;
/// The safety backstop: how many deliveries one session may be
/// handed in [`SESSION_RATE_WINDOW_MS`], every source combined. Generous: it
/// only catches loops. Not configurable by agents; it resumes on its own.
pub const SESSION_RATE_LIMIT: usize = 120;
/// The backstop's rolling window.
pub const SESSION_RATE_WINDOW_MS: u64 = 60 * 60 * 1000;
/// The rolling window of `max_wakes_per_day`.
pub const WAKE_DAY_MS: u64 = 24 * 60 * 60 * 1000;
/// The longest `min_gap` (a week).
pub const MAX_MIN_GAP_MS: u64 = 7 * 24 * 60 * 60 * 1000;
/// How long a routine's wake budget is kept after its last use (a fire
/// checked against it counts): a year, so a monthly routine keeps its count.
pub const BUDGET_RETENTION_MS: u64 = 366 * 24 * 60 * 60 * 1000;
/// How many active timers one host may hold.
pub const MACHINE_TIMER_CAP: usize = 200;
/// How many active timers one session may hold.
pub const SESSION_TIMER_CAP: usize = 10;
/// The shortest repeat interval of a timer.
pub const MIN_TIMER_INTERVAL_MS: u64 = 60 * 1000;
/// How many outcomes the host keeps for clients that were away.
pub const OUTCOME_LOG_CAP: usize = 1000;
/// How long a delivery's outcome answers a repeated key, beyond its wait.
pub const IDEMPOTENCY_GRACE_MS: u64 = 24 * 60 * 60 * 1000;
/// How long no viewer input must have reached a session before a delivery
/// may type into it.
pub const QUIET_INPUT_MS: u64 = 3000;
/// How long a delivery waits for a fresh agent's first activity evidence
/// before it is `unobservable` (within its own wait).
pub const OBSERVE_GRACE_MS: u64 = 120_000;
/// The least time between the paste and its Enter: agent TUIs fold an Enter
/// that arrives with a burst of text into it as a newline (the repo's agent
/// submit timing, `AGENT_ENTER_GAP_MIN_MS`).
pub const ENTER_GAP_MIN_MS: u64 = 200;
/// The most time spent waiting for the paste's echo to go quiet.
pub const ENTER_GAP_MAX_MS: u64 = 1000;
/// How long the agent has to show the submit took (a new turn, or busy).
pub const SUBMIT_CONFIRM_MS: u64 = 10_000;

/// Who asked for a delivery or a timer. `desktop` is the viewer of the client
/// connection that asked, stamped by the host from the handshake (never taken
/// from the request); `routine_host` names the routine host that owns the run,
/// when one does. Only the originator records the run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Originator {
    pub desktop: String,
    pub routine_host: Option<String>,
}

/// One delivery into a running session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliverRequest {
    pub session: PtySessionId,
    /// The message, delivered after `header` on its own line.
    pub text: String,
    /// The ASCII run header, e.g. `[routine: triage | run 42]`.
    #[serde(default)]
    pub header: Option<String>,
    /// What the desktop's scrollback annotation names (routine or timer, and
    /// its author). Travels on the outcome; never written to the PTY.
    #[serde(default)]
    pub marker: String,
    /// How long the delivery may wait for the session to be idle.
    pub max_wait_ms: u64,
    pub idempotency_key: String,
    #[serde(default)]
    pub originator: Originator,
    /// The routine's own wake limits, enforced here, on the host
    /// that runs the session. A server before proto minor 13 ignores them,
    /// so a client sends them only to a newer one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<WakeLimits>,
}

/// What a fire inside a routine's `min_gap` is recorded as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapPolicy {
    /// `coalesced` into the delivery that opened the gap.
    #[default]
    Coalesce,
    /// `undelivered(min_gap)`.
    Skip,
}

/// A routine's wake limits, set by whoever wrote the routine. Each
/// is optional; none set means no limit but the host's backstop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WakeLimits {
    /// Whose budget this is: the routine's key and its budget generation
    /// (a resume starts a new one).
    pub budget: String,
    /// Total deliveries under this budget. Reached: `undelivered(max_wakes)`,
    /// and the routine pauses itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wakes: Option<u32>,
    /// Deliveries in any rolling 24 h. Over it: `undelivered(limit_reached)`;
    /// it resumes on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wakes_per_day: Option<u32>,
    /// The least time between two deliveries of this budget (this routine),
    /// into any session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_gap_ms: Option<u64>,
    /// What a fire inside the gap is.
    #[serde(default)]
    pub gap: GapPolicy,
}

impl WakeLimits {
    /// The limits' own validity, before anything is queued.
    pub fn validate(&self) -> Result<(), String> {
        if !valid_label(&self.budget) {
            return Err("a wake budget must be 1-256 bytes without controls".into());
        }
        if self.max_wakes == Some(0) || self.max_wakes_per_day == Some(0) {
            return Err("max_wakes and max_wakes_per_day must be at least 1".into());
        }
        if self
            .min_gap_ms
            .is_some_and(|gap| gap == 0 || gap > MAX_MIN_GAP_MS)
        {
            return Err("min_gap must be between 1 ms and 7 days".into());
        }
        Ok(())
    }
}

/// Why a delivery was not written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UndeliveredReason {
    BusyTimeout,
    NeedsPreparation,
    MachineOffline,
    MachineUnknown,
    SessionEnded,
    Unobservable,
    AgentFailed,
    Archived,
    ServerRestarted,
    /// Over the routine's `max_wakes_per_day`; resumes on its own.
    LimitReached,
    /// The wait ran out while a human draft sat in the agent's prompt.
    DraftPending,
    /// The routine made its `max_wakes` deliveries: it pauses.
    MaxWakes,
    /// Inside the routine's (or timer's) `min_gap` with `overlap: skip`.
    MinGap,
    /// The host's per-session safety backstop: too many deliveries in the
    /// last hour, every source combined. Resumes on its own.
    RateLimited,
    /// The host's wake queue stopped (it kept failing). A reason a newer
    /// peer sends that this build does not know reads as this.
    #[serde(other)]
    Unavailable,
}

/// Where a delivery stands. Every status but `queued` is final.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DeliveryStatus {
    Queued {
        deadline_ms: u64,
    },
    /// Written: the header and text as one bracketed paste, then one submit.
    Delivered {
        at_ms: u64,
    },
    /// A timer fire that collapsed into the delivery still queued for it.
    Coalesced {
        into: String,
    },
    Undelivered {
        reason: UndeliveredReason,
    },
    /// A write began but was not confirmed: it may have reached the session.
    /// Never written again (R5).
    Unknown {
        detail: String,
    },
}

impl DeliveryStatus {
    pub fn is_final(&self) -> bool {
        !matches!(self, Self::Queued { .. })
    }
}

/// A delivery's outcome, as the host reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WakeOutcome {
    /// The outcome log position; 0 for a reply that is not a log entry
    /// (a queued delivery, or an answer for an evicted key).
    pub seq: u64,
    pub delivery_id: String,
    pub idempotency_key: String,
    pub session: PtySessionId,
    pub marker: String,
    pub originator: Originator,
    /// The timer that produced the delivery, if one did.
    pub timer_id: Option<String>,
    pub status: DeliveryStatus,
    /// The host lifetime that decided this status.
    pub host_boot_id: String,
    /// Host clock, milliseconds since the Unix epoch.
    pub at_ms: u64,
    /// Something to tell a person once (the backstop tripped, and
    /// the source that woke the session most). Old peers ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

/// When a timer fires. Durations count from when the host accepted the timer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TimerSchedule {
    /// First after `after_ms`, then every `every_ms` if given.
    After {
        after_ms: u64,
        #[serde(default)]
        every_ms: Option<u64>,
    },
    /// A 5-field cron in an IANA time zone, evaluated by the host.
    Cron { cron: String, tz: String },
}

/// A timer to set on a session the caller owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimerSpec {
    pub session: PtySessionId,
    pub schedule: TimerSchedule,
    /// Stop after this long (from acceptance); must be after the first fire.
    #[serde(default)]
    pub until_ms: Option<u64>,
    /// Stop after this many fires (at least 1).
    #[serde(default)]
    pub count: Option<u32>,
    pub text: String,
    pub name: String,
    /// The agent or human that set it, for the marker.
    pub created_by: String,
    /// How long each fire may wait for the session to be idle.
    pub max_wait_ms: u64,
    #[serde(default)]
    pub originator: Originator,
    /// Refuse the timer now if the session cannot be observed (no activity
    /// snapshot, or an agent whose activity is unqualified, like Codex's
    /// `Scope::Unknown`): its fires could only end `unobservable`.
    /// A fresh Claude that has not reported yet is accepted. Old servers
    /// ignore it.
    #[serde(default)]
    pub require_observed: bool,
    /// The least time between two of its deliveries: a fire
    /// inside it is coalesced into the last one. Old servers ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_gap_ms: Option<u64>,
}

/// An active timer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimerInfo {
    pub timer_id: String,
    pub session: PtySessionId,
    pub name: String,
    pub created_by: String,
    pub originator: Originator,
    pub schedule: TimerSchedule,
    /// What each fire delivers (the timer popover shows it).
    /// Empty from a server that predates it.
    #[serde(default)]
    pub text: String,
    /// Host clock of the next fire.
    pub next_at_ms: u64,
    pub fires: u32,
    pub count: Option<u32>,
    pub until_at_ms: Option<u64>,
    pub created_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_gap_ms: Option<u64>,
}

/// The outcomes a client missed: those after `since`. `overflow` is set when
/// some of them were dropped from the capped log before being read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutcomeLog {
    pub host_boot_id: String,
    pub outcomes: Vec<WakeOutcome>,
    pub overflow: bool,
    /// The newest log position; pass it as the next `since`.
    pub last_seq: u64,
}

/// The ASCII run header of a timer's `fire`th delivery:
/// `[timer: <name> | fire <n>]`.
pub fn timer_header(name: &str, fire: u32) -> String {
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_ascii() && !c.is_ascii_control() && c != ']' {
                c
            } else {
                '?'
            }
        })
        .collect();
    format!("[timer: {name} | fire {fire}]")
}

/// Strip what could steer a terminal: every C0 and C1 control (and DEL)
/// except newline and tab. ESC is gone, so a bracketed paste can never be
/// closed early from inside the text.
pub fn sanitize(text: &str) -> String {
    text.chars()
        .filter(|c| matches!(c, '\n' | '\t') || !c.is_control())
        .collect()
}

/// The bytes a delivery types: the sanitized header line and text, inside one
/// bracketed paste. The submit key follows as a write of its own.
pub fn paste_payload(header: Option<&str>, text: &str) -> Result<String, String> {
    let body = compose(header, text)?;
    Ok(format!("\x1b[200~{body}\x1b[201~"))
}

/// The submit key that follows a paste.
pub const SUBMIT_KEY: &str = "\r";

/// The header line and text, validated and sanitized, within
/// [`MAX_TEXT_BYTES`].
pub fn compose(header: Option<&str>, text: &str) -> Result<String, String> {
    let text = sanitize(text);
    if text.trim().is_empty() {
        return Err("wake text is empty".into());
    }
    let body = match header {
        Some(header) => {
            if !header
                .chars()
                .all(|c| c.is_ascii() && !c.is_ascii_control())
            {
                return Err("the run header must be one line of printable ASCII".into());
            }
            format!("{header}\n{text}")
        }
        None => text,
    };
    if body.len() > MAX_TEXT_BYTES {
        return Err(format!(
            "wake text is {} bytes; the limit is {MAX_TEXT_BYTES}",
            body.len()
        ));
    }
    Ok(body)
}

/// The refusal of a line typed into an agent whose prompt never became ready
/// for input: nothing was typed.
pub const AGENT_INPUT_NOT_READY: &str = "agentInputNotReady";

/// Whether viewer input `bytes` can leave text in an agent's prompt: anything
/// but focus reports, SGR mouse reports, a lone ESC or Ctrl-C. Arrow keys
/// count (they recall history into the prompt).
pub fn may_draft(bytes: &[u8]) -> bool {
    let mut rest = bytes;
    while !rest.is_empty() {
        if rest.starts_with(b"\x1b[I") || rest.starts_with(b"\x1b[O") {
            rest = &rest[3..];
        } else if rest.starts_with(b"\x1b[<") {
            match rest.iter().position(|b| *b == b'M' || *b == b'm') {
                Some(end) => rest = &rest[end + 1..],
                None => return true,
            }
        } else if rest == b"\x1b" || rest[0] == 0x03 {
            rest = &rest[1..];
        } else {
            return true;
        }
    }
    false
}

/// A non-empty opaque label of at most [`MAX_LABEL_LEN`] bytes without
/// controls.
pub fn valid_label(label: &str) -> bool {
    !label.is_empty() && label.len() <= MAX_LABEL_LEN && !label.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_escape_and_c1_but_keeps_newline_and_tab() {
        let hostile = "ok\x1b[201~\x1b]0;x\x07\r\n\tnext\u{9b}31m\u{85}\x7f!";
        assert_eq!(sanitize(hostile), "ok[201~]0;x\n\tnext31m!");
    }

    #[test]
    fn paste_payload_wraps_header_and_text_in_one_bracketed_paste() {
        let payload = paste_payload(Some("[routine: triage | run 7]"), "look\x1b[201~").unwrap();
        assert_eq!(
            payload,
            "\x1b[200~[routine: triage | run 7]\nlook[201~\x1b[201~"
        );
        assert_eq!(payload.matches("\x1b[201~").count(), 1);
    }

    #[test]
    fn compose_refuses_non_ascii_header_empty_text_and_oversize() {
        assert!(compose(Some("[routine: a · run 1]"), "x").is_err());
        assert!(compose(None, "\x1b\x07").is_err());
        assert!(compose(None, &"a".repeat(MAX_TEXT_BYTES)).is_ok());
        assert!(compose(None, &"a".repeat(MAX_TEXT_BYTES + 1)).is_err());
        assert!(compose(Some("[h]"), &"a".repeat(MAX_TEXT_BYTES - 3)).is_err());
    }

    #[test]
    fn only_input_that_can_leave_text_is_a_draft() {
        for quiet in [
            &b"\x1b[I"[..],
            b"\x1b[O\x1b[I",
            b"\x1b[<0;10;5M\x1b[<0;10;5m",
            b"\x03",
            b"\x1b",
        ] {
            assert!(!may_draft(quiet), "{quiet:?}");
        }
        for draft in [&b"h"[..], b"/", b"\x1b[A", b"\x7f", b"\x1b[Ihi"] {
            assert!(may_draft(draft), "{draft:?}");
        }
    }

    // `TimerInfo.text` is new. A server that predates it sends
    // none (read as empty); a client that predates it ignores it.
    #[test]
    fn timer_info_text_is_optional_both_ways() {
        let info = TimerInfo {
            timer_id: "t-1".into(),
            session: PtySessionId::new("s-1"),
            name: "ping".into(),
            created_by: "agent:Barb".into(),
            originator: Originator::default(),
            schedule: TimerSchedule::After {
                after_ms: 60_000,
                every_ms: None,
            },
            text: "check the build".into(),
            next_at_ms: 1,
            fires: 0,
            count: None,
            until_at_ms: None,
            created_at_ms: 0,
            min_gap_ms: None,
        };
        let mut json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["text"], "check the build");
        assert_eq!(
            serde_json::from_value::<TimerInfo>(json.clone()).unwrap(),
            info
        );
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        #[allow(dead_code)]
        struct OldTimerInfo {
            timer_id: String,
            name: String,
            next_at_ms: u64,
        }
        let old: OldTimerInfo = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(old.timer_id, "t-1");
        json.as_object_mut().unwrap().remove("text");
        let from_old = serde_json::from_value::<TimerInfo>(json).unwrap();
        assert_eq!(from_old.text, "");
        let spec: TimerSpec = serde_json::from_value(serde_json::json!({
            "session": "s-1",
            "schedule": {"kind": "after", "after_ms": 60000},
            "text": "x", "name": "n", "createdBy": "a", "maxWaitMs": 1
        }))
        .unwrap();
        assert!(!spec.require_observed, "an old client asks nothing more");
    }

    // Limits travel on the delivery and the timer; a peer that
    // predates them sends none (read as none) and ignores them when sent.
    #[test]
    fn wake_limits_are_optional_both_ways_and_validated() {
        let old: DeliverRequest = serde_json::from_value(serde_json::json!({
            "session": "s-1", "text": "x", "maxWaitMs": 1, "idempotencyKey": "k"
        }))
        .unwrap();
        assert_eq!(old.limits, None);
        assert!(!serde_json::to_string(&old).unwrap().contains("limits"));
        let limits = WakeLimits {
            budget: "routine:o/t/heavy#1".into(),
            max_wakes: Some(3),
            max_wakes_per_day: Some(2),
            min_gap_ms: Some(2 * 60 * 60 * 1000),
            gap: GapPolicy::Skip,
        };
        let json = serde_json::to_value(&limits).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "budget": "routine:o/t/heavy#1", "maxWakes": 3, "maxWakesPerDay": 2,
                "minGapMs": 7_200_000u64, "gap": "skip"
            })
        );
        assert_eq!(serde_json::from_value::<WakeLimits>(json).unwrap(), limits);
        assert!(limits.validate().is_ok());
        for bad in [
            WakeLimits {
                max_wakes: Some(0),
                ..limits.clone()
            },
            WakeLimits {
                max_wakes_per_day: Some(0),
                ..limits.clone()
            },
            WakeLimits {
                min_gap_ms: Some(MAX_MIN_GAP_MS + 1),
                ..limits.clone()
            },
            WakeLimits {
                budget: String::new(),
                ..limits.clone()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        // The new reasons have wire names, and an old reader takes them as
        // `unavailable`.
        for (reason, wire) in [
            (UndeliveredReason::MaxWakes, "max_wakes"),
            (UndeliveredReason::MinGap, "min_gap"),
            (UndeliveredReason::RateLimited, "rate_limited"),
        ] {
            assert_eq!(serde_json::to_value(reason).unwrap(), wire);
        }
        #[derive(Deserialize, Debug, PartialEq)]
        #[serde(rename_all = "snake_case")]
        enum OldReason {
            BusyTimeout,
            #[serde(other)]
            Unavailable,
        }
        assert_eq!(
            serde_json::from_value::<OldReason>(serde_json::json!("rate_limited")).unwrap(),
            OldReason::Unavailable
        );
    }

    #[test]
    fn timer_header_is_one_line_of_ascii() {
        assert_eq!(
            timer_header("ci · check]", 3),
            "[timer: ci ? check? | fire 3]"
        );
    }

    #[test]
    fn statuses_round_trip_with_their_reason() {
        let status = DeliveryStatus::Undelivered {
            reason: UndeliveredReason::ServerRestarted,
        };
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(
            json,
            r#"{"status":"undelivered","reason":"server_restarted"}"#
        );
        assert_eq!(
            serde_json::from_str::<DeliveryStatus>(&json).unwrap(),
            status
        );
        assert!(!DeliveryStatus::Queued { deadline_ms: 1 }.is_final());
    }
}
