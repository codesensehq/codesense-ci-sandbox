// SPDX-License-Identifier: MIT
//! SW-AGR-3: session timers ("temporary routines"). `schedule_wakeup`,
//! `list_wakeups` and `cancel_wakeup` for an agent, and "Remind this agent…"
//! for the person at this desktop (SPEC §5, §9.1, §9.5, §9.6, §9.9).
//!
//! - **Any interactive session**, saved agent or unnamed, on this Mac or on a
//!   connected machine. Never a headless run (`claude -p`, a remote task).
//! - **Owned by the authenticated caller.** An agent's timer belongs to the
//!   session the transport vouched for ([`super::authority::bind_caller`]);
//!   a person's to the tab they acted on. A timer only ever wakes its own
//!   session (R6) and never launches anything.
//! - **Stored and fired by the terminal server that runs the session**, local
//!   and remote alike (`SessionWakeV1`): it keeps firing while this desktop
//!   is away, and ends with its session. This desktop keeps no copy: a list
//!   is always the server's, so a reconnect shows what the server holds.
//! - **While the machine is unreachable** setting, listing and cancelling
//!   fail with a clear message; the timers keep firing there.
//!
//! The schedule is validated here (§9.9) and travels as durations only.

use chrono::{DateTime, SecondsFormat, Utc};
use codesense_pty_core::session_wake::{
    compose, timer_header, valid_label, TimerInfo, TimerSchedule, MAX_MIN_GAP_MS,
    MAX_WAIT_LIMIT_MS, MIN_TIMER_INTERVAL_MS,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::authority::{self, Caller, SessionRef, TransportIdentity};
use super::{DeliveryError, DeliveryService, TimerRequest};

/// The author a person's timer names ("set by you").
pub(crate) const HUMAN_AUTHOR: &str = "you";

/// Why a headless run gets no timers (§9.1).
pub(crate) const HEADLESS_REFUSAL: &str =
    "schedule_wakeup is not supported for headless runs: a timer wakes an interactive session, and a headless run has none to wake";

/// What a timer should do, as an agent or a person asks for it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WakeupAsk {
    /// First fire after this long: `90s`, `20m`, `2h`, `1h30m`, `1d`.
    /// Exactly one of `in` or `at`.
    #[serde(rename = "in", default)]
    pub in_: Option<String>,
    /// First fire at this RFC 3339 time, with its time zone offset
    /// (`2026-10-05T14:30:00-07:00` or `…Z`). Must be in the future.
    #[serde(default)]
    pub at: Option<String>,
    /// Then repeat this often (at least `1m`). Omit for a one-shot timer.
    #[serde(default)]
    pub every: Option<String>,
    /// Stop repeating after this RFC 3339 time (with offset); after the first
    /// fire. At most one of `until` or `count`.
    #[serde(default)]
    pub until: Option<String>,
    /// Stop after this many fires (at least 1).
    #[serde(default)]
    pub count: Option<u32>,
    /// The least time between two of its deliveries (`30m`, `2h`; 1m to
    /// `7d`): a fire inside it folds into the last one. Optional.
    #[serde(default)]
    pub min_gap: Option<String>,
    /// What the session receives when the timer fires.
    pub prompt: String,
    /// A short name for the marker and the timer list. Default: the first
    /// words of the prompt.
    #[serde(default)]
    pub name: Option<String>,
    /// Removed (§9.9): delivery always waits for the session to be idle.
    /// Accepted only to refuse it by name.
    #[serde(default)]
    #[schemars(skip)]
    pub when_idle_only: Option<serde_json::Value>,
}

/// `schedule_wakeup`: a timer on the caller's own session.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScheduleWakeupParams {
    /// Your own `$CODESENSE_TAB_SESSION_ID`, checked against the terminal
    /// you call from: another session's id is refused.
    #[serde(alias = "session_id")]
    pub session_id: String,
    /// First fire after this long: `90s`, `20m`, `2h`, `1h30m`, `1d`.
    /// Exactly one of `in` or `at`.
    #[serde(rename = "in", default)]
    pub in_: Option<String>,
    /// First fire at this RFC 3339 time with its offset
    /// (`2026-10-05T14:30:00-07:00` or `…Z`); in the future.
    #[serde(default)]
    pub at: Option<String>,
    /// Then repeat this often (at least `1m`). Omit for a one-shot timer.
    #[serde(default)]
    pub every: Option<String>,
    /// Stop repeating after this RFC 3339 time (with offset), after the first
    /// fire. At most one of `until` or `count`.
    #[serde(default)]
    pub until: Option<String>,
    /// Stop after this many fires (at least 1).
    #[serde(default)]
    pub count: Option<u32>,
    /// Optional: the least time between two of its deliveries (`30m`, `2h`;
    /// 1m to `7d`). A fire inside it folds into the last one instead of
    /// waking you again. Set it when each wake starts heavy work.
    #[serde(default)]
    pub min_gap: Option<String>,
    /// What your session receives when the timer fires.
    pub prompt: String,
    /// A short name for the marker and the list (default: the prompt's
    /// first words).
    #[serde(default)]
    pub name: Option<String>,
    /// Removed: accepted only to refuse it by name.
    #[serde(default)]
    #[schemars(skip)]
    pub when_idle_only: Option<serde_json::Value>,
}

impl ScheduleWakeupParams {
    pub(crate) fn ask(self) -> WakeupAsk {
        WakeupAsk {
            in_: self.in_,
            at: self.at,
            every: self.every,
            until: self.until,
            count: self.count,
            min_gap: self.min_gap,
            prompt: self.prompt,
            name: self.name,
            when_idle_only: self.when_idle_only,
        }
    }
}

/// `list_wakeups`: the caller's own timers.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListWakeupsParams {
    /// Your own `$CODESENSE_TAB_SESSION_ID`.
    #[serde(alias = "session_id")]
    pub session_id: String,
}

/// `cancel_wakeup`: stop one of the caller's own timers.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CancelWakeupParams {
    /// Your own `$CODESENSE_TAB_SESSION_ID`.
    #[serde(alias = "session_id")]
    pub session_id: String,
    /// The timer, from schedule_wakeup or list_wakeups.
    #[serde(alias = "timer_id")]
    pub timer_id: String,
}

/// A timer that was set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScheduledWakeup {
    pub timer_id: String,
    /// The first fire, RFC 3339 (UTC) on the terminal server's clock.
    pub next_at: String,
    pub name: String,
    pub machine_id: String,
    pub session_id: String,
}

/// One active timer, as `list_wakeups` and the timer popover show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WakeupRow {
    pub timer_id: String,
    pub name: String,
    pub prompt: String,
    pub next_at: String,
    pub every: Option<String>,
    pub until: Option<String>,
    pub count: Option<u32>,
    /// Fires left under `count`.
    pub remaining: Option<u32>,
    /// Its `min_gap` (SW-AGR-13), when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_gap: Option<String>,
    pub fires: u32,
    pub created_by: String,
    pub machine_id: String,
    pub session_id: String,
}

/// Refuse a headless run: `session` is a headless launch's pseudo-session,
/// or its process is a headless run (`headless`).
pub(crate) fn require_interactive(session: &str, headless: bool) -> Result<(), DeliveryError> {
    if headless || crate::commands::dashboard_launch::is_headless_session_id(session) {
        return Err(DeliveryError::Refused(HEADLESS_REFUSAL.into()));
    }
    Ok(())
}

/// The furthest first fire a terminal server accepts (366 days).
const MAX_FIRST_FIRE_MS: u64 = MAX_WAIT_LIMIT_MS * 366;

/// How many words of the prompt name a timer that was given no name.
const DEFAULT_NAME_WORDS: usize = 4;

/// Turn `ask` into the timer the terminal server keeps, authored by
/// `created_by`, judged at `now_ms` (this desktop's clock; only durations
/// leave it).
pub(crate) fn plan(ask: &WakeupAsk, now_ms: u64, created_by: &str) -> Result<TimerRequest, String> {
    if ask.when_idle_only.is_some() {
        return Err(
            "whenIdleOnly was removed: a timer always waits until the session is idle".into(),
        );
    }
    let after_ms = match (&ask.in_, &ask.at) {
        (Some(after), None) => parse_duration(after).map_err(|e| format!("`in`: {e}"))?,
        (None, Some(at)) => {
            let at = parse_time(at).map_err(|e| format!("`at`: {e}"))?;
            if at <= now_ms {
                return Err("`at` must be in the future".into());
            }
            at - now_ms
        }
        _ => return Err("pass exactly one of `in` or `at`".into()),
    };
    if after_ms > MAX_FIRST_FIRE_MS {
        return Err("the first fire must be within 366 days".into());
    }
    let every_ms = ask
        .every
        .as_deref()
        .map(|every| parse_duration(every).map_err(|e| format!("`every`: {e}")))
        .transpose()?;
    if every_ms.is_some_and(|every| every < MIN_TIMER_INTERVAL_MS) {
        return Err(format!(
            "`every` must be at least {}m",
            MIN_TIMER_INTERVAL_MS / 60_000
        ));
    }
    if ask.until.is_some() && ask.count.is_some() {
        return Err("pass at most one of `until` or `count`".into());
    }
    if every_ms.is_none() && (ask.until.is_some() || ask.count.is_some()) {
        return Err("`until` and `count` need `every`: a one-shot timer fires once".into());
    }
    if ask.count == Some(0) {
        return Err("`count` must be at least 1".into());
    }
    let min_gap_ms = ask
        .min_gap
        .as_deref()
        .map(|gap| parse_duration(gap).map_err(|e| format!("`minGap`: {e}")))
        .transpose()?;
    if min_gap_ms.is_some_and(|gap| !(MIN_TIMER_INTERVAL_MS..=MAX_MIN_GAP_MS).contains(&gap)) {
        return Err("`minGap` must be between 1m and 7d".into());
    }
    let until_ms = match &ask.until {
        None => None,
        Some(until) => {
            let until = parse_time(until).map_err(|e| format!("`until`: {e}"))?;
            let first = now_ms.saturating_add(after_ms);
            if until <= first {
                return Err(format!(
                    "`until` must be after the first fire ({})",
                    rfc3339(first)
                ));
            }
            Some(until - now_ms)
        }
    };
    if ask.prompt.trim().is_empty() {
        return Err("`prompt` is empty".into());
    }
    let name = match &ask.name {
        Some(name) => {
            let name = name.trim();
            if !valid_label(name) {
                return Err("`name` must be 1-256 bytes without control characters".into());
            }
            name.to_string()
        }
        None => default_name(&ask.prompt),
    };
    // What a fire types: the run header, then the prompt (sanitized), within
    // the 8 KiB a delivery may carry. Judged with the widest fire number.
    compose(Some(&timer_header(&name, u32::MAX)), &ask.prompt)
        .map_err(|e| format!("`prompt`: {e}"))?;
    Ok(TimerRequest {
        schedule: TimerSchedule::After { after_ms, every_ms },
        until_ms,
        count: ask.count,
        text: ask.prompt.clone(),
        name,
        created_by: created_by.to_string(),
        // A fire waits as long as a delivery may (24 h) for the session to
        // be idle: a one-shot reminder is not lost to a long turn. A fire
        // still waiting when the next is due absorbs it (coalesced).
        max_wait_ms: MAX_WAIT_LIMIT_MS,
        require_observed: true,
        min_gap_ms,
    })
}

/// The first words of `prompt`, as a label.
fn default_name(prompt: &str) -> String {
    let words: Vec<&str> = prompt.split_whitespace().take(DEFAULT_NAME_WORDS).collect();
    let mut name: String = words
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    while name.len() > 64 {
        name.pop();
    }
    if valid_label(&name) {
        name
    } else {
        "Reminder".into()
    }
}

/// A duration such as `90s`, `20m`, `2h`, `1h30m` or `1d`: one or more
/// `<digits><unit>`, units `s`, `m`, `h`, `d`; more than zero.
fn parse_duration(raw: &str) -> Result<u64, String> {
    let bad = || format!("{raw:?} is not a duration like 90s, 20m, 2h or 1h30m");
    let mut total: u64 = 0;
    let mut digits = String::new();
    for c in raw.trim().chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let unit: u64 = match c {
            's' => 1_000,
            'm' => 60_000,
            'h' => 3_600_000,
            'd' => 86_400_000,
            _ => return Err(bad()),
        };
        let n: u64 = digits.parse().map_err(|_| bad())?;
        digits.clear();
        total = n
            .checked_mul(unit)
            .and_then(|ms| total.checked_add(ms))
            .ok_or_else(bad)?;
    }
    if !digits.is_empty() || total == 0 {
        return Err(bad());
    }
    Ok(total)
}

/// An RFC 3339 time that names its offset, in epoch milliseconds.
fn parse_time(raw: &str) -> Result<u64, String> {
    let at = DateTime::parse_from_rfc3339(raw.trim()).map_err(|_| {
        format!(
            "{raw:?} must be an RFC 3339 time with a time zone offset, like 2026-10-05T14:30:00-07:00 or 2026-10-05T21:30:00Z"
        )
    })?;
    u64::try_from(at.timestamp_millis()).map_err(|_| format!("{raw:?} is before 1970"))
}

fn rfc3339(ms: u64) -> String {
    DateTime::<Utc>::from_timestamp_millis(i64::try_from(ms).unwrap_or(i64::MAX))
        .map(|at| at.to_rfc3339_opts(SecondsFormat::Secs, true))
        .unwrap_or_default()
}

/// `ms` as the shortest duration string [`parse_duration`] reads back.
fn format_duration(ms: u64) -> String {
    let mut seconds = ms / 1_000;
    let mut out = String::new();
    for (unit, size) in [("d", 86_400), ("h", 3_600), ("m", 60), ("s", 1)] {
        if seconds >= size {
            out.push_str(&format!("{}{unit}", seconds / size));
            seconds %= size;
        }
    }
    if out.is_empty() {
        out.push_str("0s");
    }
    out
}

fn row(machine: &str, info: TimerInfo) -> WakeupRow {
    let every = match &info.schedule {
        TimerSchedule::After { every_ms, .. } => every_ms.map(format_duration),
        TimerSchedule::Cron { .. } => None,
    };
    WakeupRow {
        timer_id: info.timer_id,
        name: info.name,
        prompt: info.text,
        next_at: rfc3339(info.next_at_ms),
        every,
        until: info.until_at_ms.map(rfc3339),
        count: info.count,
        remaining: info.count.map(|count| count.saturating_sub(info.fires)),
        min_gap: info.min_gap_ms.map(format_duration),
        fires: info.fires,
        created_by: info.created_by,
        machine_id: machine.to_string(),
        session_id: info.session.as_str().to_string(),
    }
}

/// Set a timer on `target` for `caller`.
pub(crate) fn schedule(
    service: &DeliveryService,
    caller: &Caller,
    target: &SessionRef,
    ask: &WakeupAsk,
    now_ms: u64,
    created_by: &str,
) -> Result<ScheduledWakeup, DeliveryError> {
    let timer = plan(ask, now_ms, created_by).map_err(DeliveryError::Refused)?;
    let info = service.timer_set(caller, target, timer)?;
    Ok(ScheduledWakeup {
        timer_id: info.timer_id,
        next_at: rfc3339(info.next_at_ms),
        name: info.name,
        machine_id: target.machine.clone(),
        session_id: info.session.as_str().to_string(),
    })
}

/// The active timers of `target`, as its terminal server holds them.
pub(crate) fn list(
    service: &DeliveryService,
    caller: &Caller,
    target: &SessionRef,
) -> Result<Vec<WakeupRow>, DeliveryError> {
    let mut rows: Vec<WakeupRow> = service
        .timer_list(caller, target)?
        .into_iter()
        .map(|info| row(&target.machine, info))
        .collect();
    rows.sort_by(|a, b| a.next_at.cmp(&b.next_at));
    Ok(rows)
}

/// Cancel `target`'s timer `timer_id`; whether it was active.
pub(crate) fn cancel(
    service: &DeliveryService,
    caller: &Caller,
    target: &SessionRef,
    timer_id: &str,
) -> Result<bool, DeliveryError> {
    service.timer_cancel(caller, target, timer_id)
}

/// What to tell the agent or the person about `error`.
pub(crate) fn message(error: &DeliveryError) -> String {
    message_on(error, |_| None)
}

/// [`message`], naming a machine by its `label` (as people call it) when
/// one is known.
pub(crate) fn message_on(error: &DeliveryError, label: impl Fn(&str) -> Option<String>) -> String {
    match error {
        DeliveryError::Offline { machine } => format!(
            "{} is offline: its timers keep firing there, and you can see and change them once it is back",
            label(machine).unwrap_or_else(|| machine.clone())
        ),
        other => other.to_string(),
    }
}

/// Who an agent's timer says set it: `agent:<tab name>`. Never
/// [`HUMAN_AUTHOR`], which is the person's alone.
pub(crate) fn agent_author(name: Option<&str>) -> String {
    let name: String = name
        .unwrap_or_default()
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect();
    if name.is_empty() {
        "agent".into()
    } else {
        format!("agent:{name}")
    }
}

/// The session an agent's timer belongs to: the one the transport vouched
/// for (`identity`; `root_of` answers a local session's process), never the
/// `claimed` id alone. A remote task (a reverse connection with no tab) and
/// a local headless run (`headless_pid` says whether the session's process is
/// one) are refused.
pub(crate) fn timer_owner(
    identity: &TransportIdentity,
    claimed: &str,
    root_of: impl FnOnce(&str) -> Option<codesense_pty_core::activity::peer::PeerProcess>,
    headless_pid: impl FnOnce(&str) -> bool,
) -> Result<SessionRef, DeliveryError> {
    if matches!(identity, TransportIdentity::Reverse { session: None, .. }) {
        return Err(DeliveryError::Refused(HEADLESS_REFUSAL.into()));
    }
    let own = authority::bind_caller(identity, claimed, root_of)?;
    let headless =
        matches!(identity, TransportIdentity::LocalPeer(_)) && headless_pid(&own.session);
    require_interactive(&own.session, headless)?;
    Ok(own)
}

/// The tab a person reminds: an interactive session; on another machine,
/// a terminal tab this desktop opened there (`remote_tab` is the machine
/// its record names), never a remote task.
pub(crate) fn require_tab(
    session: &str,
    machine: &str,
    headless: bool,
    remote_tab: Option<&str>,
) -> Result<(), DeliveryError> {
    require_interactive(session, headless)?;
    if machine != crate::commands::machines::LOCAL_MACHINE_ID && remote_tab != Some(machine) {
        return Err(DeliveryError::Refused(format!(
            "{session} is not a terminal tab on {machine}: only an interactive tab can be reminded"
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "wakeup_tests.rs"]
pub(crate) mod tests;
