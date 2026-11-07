// SPDX-License-Identifier: MIT
//! Wall-clock schedules: 5-field cron evaluated in an IANA zone.
//!
//! The `cron` crate is only used to walk the **naive wall clock**. Its
//! schedule iterates over `Utc` standing in for naive local time, and every
//! naive result is then placed in the zone here, so the DST rules are ours and
//! explicit rather than whatever the crate does with a zoned datetime:
//!
//! - a wall time that occurs twice (fall back) fires once, at its first instant;
//! - a wall time that does not exist (spring forward) is skipped that day, never
//!   shifted.
//!
//! The crate numbers weekdays 1 = Sunday, while standard cron uses 0 or 7 =
//! Sunday and 1 = Monday, so the day-of-week field is rewritten to day names
//! before the crate sees it.

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, LocalResult, TimeZone, Utc};
use chrono_tz::{OffsetName, Tz};

/// Schedules may not fire more often than this.
pub const MIN_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// How many upcoming fires the minimum-interval check looks at.
const INTERVAL_SAMPLE: usize = 50;

/// Bounds the naive-time walk for one `next_after` call. Each skipped candidate
/// is a non-existent or already-fired wall time, of which a single DST
/// transition produces at most an hour's worth.
const MAX_CANDIDATES: usize = 1000;

const DAY_NAMES: [&str; 7] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];
const DAY_TITLES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

const ACCEPTED_FORMS: &str =
    "every Nm (N = 5, 10, 15, 20 or 30), every Nh (N divides 24), hourly, \
daily HH:MM, weekdays HH:MM, weekly <Mon..Sun> HH:MM, or a 5-field cron like `0 9 * * 1-5` \
(after \"every\", also hour, day HH:MM, weekday HH:MM or <Mon..Sun> HH:MM)";

/// Why a schedule cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleError {
    InvalidCron {
        cron: String,
        why: String,
    },
    MissingZone,
    UnknownZone(String),
    /// Two consecutive fires are closer than `min`.
    IntervalTooShort {
        min: Duration,
    },
    /// Both the day of the month and the day of the week are restricted, which
    /// standard cron reads as "either"; that reading is not supported.
    BothDayFields(String),
    /// The schedule has no upcoming fire at all.
    NeverFires(String),
    /// `when` text that is neither a preset nor a cron.
    UnrecognizedWhen(String),
}

impl fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCron { cron, why } => write!(f, "cron {cron:?} is not valid: {why}"),
            Self::MissingZone => f.write_str("tz is required (an IANA zone like America/Denver)"),
            Self::UnknownZone(tz) => {
                write!(
                    f,
                    "tz {tz:?} is not an IANA time zone (like America/Denver)"
                )
            }
            Self::IntervalTooShort { min } => write!(
                f,
                "the schedule fires more often than every {}m",
                min.as_secs() / 60
            ),
            Self::BothDayFields(cron) => write!(
                f,
                "cron {cron:?} restricts both the day of the month and the day of the week; \
                 standard cron fires when either matches, which routines do not support, so \
                 restrict only one and set the other to *"
            ),
            Self::NeverFires(cron) => write!(f, "cron {cron:?} never fires"),
            Self::UnrecognizedWhen(text) => {
                write!(f, "{text:?} is not a schedule; use {ACCEPTED_FORMS}")
            }
        }
    }
}

impl std::error::Error for ScheduleError {}

/// A parsed cron + zone, ready to evaluate.
#[derive(Debug, Clone)]
pub struct CompiledSchedule {
    wall: cron::Schedule,
    tz: Tz,
}

impl CompiledSchedule {
    pub fn parse(cron: &str, tz: &str) -> Result<Self, ScheduleError> {
        let tz = parse_zone(tz)?;
        let wall = compile_cron(cron)?;
        Ok(Self { wall, tz })
    }

    /// The first fire strictly after `after`.
    pub fn next_after(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let naive_after = after.with_timezone(&self.tz).naive_local();
        let start = Utc.from_utc_datetime(&naive_after);
        for wall in self.wall.after(&start).take(MAX_CANDIDATES) {
            let instant = match self.tz.from_local_datetime(&wall.naive_utc()) {
                LocalResult::Single(t) => t,
                LocalResult::Ambiguous(first, second) => first.min(second),
                LocalResult::None => continue,
            }
            .with_timezone(&Utc);
            // Inside a repeated hour the earliest instant of a later wall time
            // can still precede `after`: that occurrence already fired.
            if instant > after {
                return Some(instant);
            }
        }
        None
    }

    /// The next `n` fires after `after`, fewer if the schedule runs out.
    pub fn next_n(&self, after: DateTime<Utc>, n: usize) -> Vec<DateTime<Utc>> {
        let mut out = Vec::with_capacity(n);
        let mut cursor = after;
        while out.len() < n {
            let Some(next) = self.next_after(cursor) else {
                break;
            };
            out.push(next);
            cursor = next;
        }
        out
    }
}

/// The first fire of `cron` in `tz` strictly after `after_utc`. `None` when the
/// schedule is invalid or never fires again.
pub fn next_after(cron: &str, tz: &str, after_utc: DateTime<Utc>) -> Option<DateTime<Utc>> {
    CompiledSchedule::parse(cron, tz)
        .ok()?
        .next_after(after_utc)
}

/// The next `n` fires of `cron` in `tz` after `after`. Empty when invalid.
pub fn next_n(cron: &str, tz: &str, after: DateTime<Utc>, n: usize) -> Vec<DateTime<Utc>> {
    CompiledSchedule::parse(cron, tz)
        .map(|s| s.next_n(after, n))
        .unwrap_or_default()
}

/// The full save-time check: the cron and zone parse, the schedule fires at
/// least once, and no two of its next fires are closer than [`MIN_INTERVAL`].
pub fn validate(cron: &str, tz: &str) -> Result<(), ScheduleError> {
    validate_at(cron, tz, Utc::now())
}

pub(crate) fn validate_at(cron: &str, tz: &str, now: DateTime<Utc>) -> Result<(), ScheduleError> {
    if tz.trim().is_empty() {
        return Err(ScheduleError::MissingZone);
    }
    let schedule = CompiledSchedule::parse(cron, tz)?;
    let fires = schedule.next_n(now, INTERVAL_SAMPLE);
    if fires.is_empty() {
        return Err(ScheduleError::NeverFires(cron.to_string()));
    }
    // Measured on the wall clock the cron is written in, so a schedule's
    // verdict does not depend on whether a DST transition happens to fall
    // inside the sample (`0,58 1,3 * * *` is two real minutes apart across a
    // spring-forward, but never closer than an hour on the wall clock).
    let min = chrono::Duration::from_std(MIN_INTERVAL).unwrap_or_default();
    let wall: Vec<_> = fires
        .iter()
        .map(|f| f.with_timezone(&schedule.tz).naive_local())
        .collect();
    if wall.windows(2).any(|w| w[1] - w[0] < min) {
        return Err(ScheduleError::IntervalTooShort { min: MIN_INTERVAL });
    }
    Ok(())
}

/// This machine's IANA zone, or `UTC` when it cannot be determined.
pub fn system_zone() -> String {
    iana_time_zone::get_timezone()
        .ok()
        .filter(|z| z.parse::<Tz>().is_ok())
        .unwrap_or_else(|| "UTC".to_string())
}

fn parse_zone(tz: &str) -> Result<Tz, ScheduleError> {
    let tz = tz.trim();
    if tz.is_empty() {
        return Err(ScheduleError::MissingZone);
    }
    tz.parse::<Tz>()
        .map_err(|_| ScheduleError::UnknownZone(tz.to_string()))
}

/// Turn a user's 5-field cron into the crate's schedule (seconds prepended, the
/// weekday field rewritten to names).
fn compile_cron(cron: &str) -> Result<cron::Schedule, ScheduleError> {
    let invalid = |why: String| ScheduleError::InvalidCron {
        cron: cron.to_string(),
        why,
    };
    let fields: Vec<&str> = cron.split_whitespace().collect();
    let [minute, hour, dom, month, dow] = fields[..] else {
        return Err(invalid(
            "it must have 5 fields (minute hour day-of-month month day-of-week)".into(),
        ));
    };
    // Standard cron fires when EITHER restricted day field matches; the crate
    // requires both. Refuse the combination rather than silently differ. A
    // field starting with `*` (`*/2`) counts as unrestricted, as in standard
    // cron, where both are then required to match exactly as the crate does.
    if !dom.starts_with(['*', '?']) && !dow.starts_with(['*', '?']) {
        return Err(ScheduleError::BothDayFields(cron.to_string()));
    }
    let dow = weekday_field(dow).map_err(invalid)?;
    cron::Schedule::from_str(&format!("0 {minute} {hour} {dom} {month} {dow}"))
        .map_err(|e| invalid(e.to_string()))
}

fn is_any(field: &str) -> bool {
    field == "*" || field == "?"
}

/// Rewrite a standard-cron weekday field (0 or 7 = Sunday) as an explicit list
/// of day names, expanding ranges, lists and steps.
fn weekday_field(field: &str) -> Result<String, String> {
    if is_any(field) {
        return Ok("*".to_string());
    }
    let mut days = BTreeSet::new();
    for item in field.split(',') {
        let (base, step) = match item.split_once('/') {
            Some((base, step)) => {
                let step: u8 = step
                    .parse()
                    .ok()
                    .filter(|s| *s >= 1)
                    .ok_or_else(|| format!("weekday step {step:?} is not a positive number"))?;
                (base, Some(step))
            }
            None => (item, None),
        };
        let (lo, hi) = if base == "*" {
            (0, 6)
        } else if let Some((a, b)) = base.split_once('-') {
            let lo = weekday(a)?;
            let hi = match weekday(b)? {
                // `FRI-SUN` means Friday through Sunday.
                0 if lo > 0 => 7,
                hi => hi,
            };
            (lo, hi)
        } else {
            let day = weekday(base)?;
            // `1/2` is shorthand for `1-7/2`.
            (day, if step.is_some() { 7 } else { day })
        };
        if lo > hi {
            return Err(format!("weekday range {base:?} runs backwards"));
        }
        for day in (lo..=hi).step_by(usize::from(step.unwrap_or(1))) {
            days.insert(day % 7);
        }
    }
    Ok(days
        .into_iter()
        .map(|d| DAY_NAMES[usize::from(d)])
        .collect::<Vec<_>>()
        .join(","))
}

/// A weekday as 0-7 (0 and 7 = Sunday) or a three-letter name.
fn weekday(token: &str) -> Result<u8, String> {
    if let Ok(n) = token.parse::<u8>() {
        return if n <= 7 {
            Ok(n)
        } else {
            Err(format!("weekday {n} is out of range 0-7"))
        };
    }
    day_index(token).ok_or_else(|| format!("{token:?} is not a weekday"))
}

/// `Mon`, `monday`, `MON` … → 0 (Sunday) to 6 (Saturday).
fn day_index(token: &str) -> Option<u8> {
    let lower = token.to_ascii_lowercase();
    DAY_TITLES
        .iter()
        .position(|title| {
            let title = title.to_ascii_lowercase();
            lower == title || (lower.len() == 3 && title.starts_with(&lower))
        })
        .and_then(|i| u8::try_from(i).ok())
}

/// Normalize `when` text to `(cron, tz)`. `tz` defaults to this machine's zone.
///
/// Accepted presets: `every Nm` (N = 5, 10, 15, 20, 30), `every Nh` (N divides
/// 24), `hourly`, `daily HH:MM`, `weekdays HH:MM`, `weekly <Mon..Sun> HH:MM`.
/// The same schedules also read as the words after "every" in a sentence, with
/// or without it: `5m`, `2h`, `hour`, `day HH:MM`, `weekday HH:MM`,
/// `<Mon..Sun> HH:MM`. Anything else must be a valid 5-field cron, optionally
/// after "every".
pub fn parse_when(text: &str, tz: Option<&str>) -> Result<(String, String), ScheduleError> {
    let tz = match tz.map(str::trim).filter(|t| !t.is_empty()) {
        Some(tz) => parse_zone(tz)?.name().to_string(),
        None => system_zone(),
    };
    let tokens: Vec<&str> = text.split_whitespace().collect();
    // A leading "every" reads the rest as the sentence's slot, so "every 5m"
    // and "5m", or "every day 09:00" and "day 09:00", are one schedule.
    let tokens = match tokens[..] {
        [first, ref rest @ ..] if !rest.is_empty() && first.eq_ignore_ascii_case("every") => rest,
        _ => &tokens[..],
    };
    let words: Vec<String> = tokens.iter().map(|t| t.to_ascii_lowercase()).collect();
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let unrecognized = || ScheduleError::UnrecognizedWhen(text.trim().to_string());
    let cron = match words[..] {
        ["hourly" | "hour"] => "0 * * * *".to_string(),
        ["daily" | "day", at] => {
            let (h, m) = clock_time(at).ok_or_else(unrecognized)?;
            format!("{m} {h} * * *")
        }
        ["weekdays" | "weekday", at] => {
            let (h, m) = clock_time(at).ok_or_else(unrecognized)?;
            format!("{m} {h} * * 1-5")
        }
        ["weekly", day, at] | [day, at] => {
            let day = day_index(day).ok_or_else(unrecognized)?;
            let (h, m) = clock_time(at).ok_or_else(unrecognized)?;
            format!("{m} {h} * * {day}")
        }
        [amount] => {
            if let Some(n) = amount.strip_suffix('m').and_then(|n| n.parse::<u32>().ok()) {
                if ![5, 10, 15, 20, 30].contains(&n) {
                    return Err(unrecognized());
                }
                format!("*/{n} * * * *")
            } else if let Some(n) = amount.strip_suffix('h').and_then(|n| n.parse::<u32>().ok()) {
                if n == 0 || 24 % n != 0 {
                    return Err(unrecognized());
                }
                format!("0 */{n} * * *")
            } else {
                return Err(unrecognized());
            }
        }
        [_, _, _, _, _] => {
            // Validated and stored as written; only whitespace is normalized.
            let cron = tokens.join(" ");
            compile_cron(&cron)?;
            cron
        }
        _ => return Err(unrecognized()),
    };
    Ok((cron, tz))
}

/// `HH:MM` (or `H:MM`) on a 24-hour clock.
fn clock_time(text: &str) -> Option<(u32, u32)> {
    let (h, m) = text.split_once(':')?;
    if h.is_empty() || h.len() > 2 || m.len() != 2 {
        return None;
    }
    let h: u32 = h.parse().ok()?;
    let m: u32 = m.parse().ok()?;
    (h < 24 && m < 60).then_some((h, m))
}

/// A plain-English rendering of a schedule, for lists and confirmations.
pub fn describe(cron: &str, tz: &str) -> String {
    describe_at(cron, tz, Utc::now())
}

/// [`describe`], taking the zone abbreviation at the first fire after `now`
/// (so a summer-time zone reads "MDT" or "MST" for the fire that is next).
pub(crate) fn describe_at(cron: &str, tz: &str, now: DateTime<Utc>) -> String {
    let fallback = || format!("cron `{cron}` ({tz})");
    let fields: Vec<&str> = cron.split_whitespace().collect();
    let [minute, hour, dom, month, dow] = fields[..] else {
        return fallback();
    };
    if dom != "*" || month != "*" {
        return fallback();
    }
    if hour == "*" && dow == "*" {
        if let Some(n) = minute
            .strip_prefix("*/")
            .and_then(|n| n.parse::<u32>().ok())
        {
            return format!("every {n} minutes");
        }
        if minute == "0" {
            return "every hour".to_string();
        }
        return fallback();
    }
    if minute == "0" && dow == "*" {
        if let Some(n) = hour.strip_prefix("*/").and_then(|n| n.parse::<u32>().ok()) {
            return format!("every {n} hours");
        }
    }
    let (Some(h), Some(m)) = (
        hour.parse::<u32>().ok().filter(|h| *h < 24),
        minute.parse::<u32>().ok().filter(|m| *m < 60),
    ) else {
        return fallback();
    };
    let Ok(zone) = parse_zone(tz) else {
        return fallback();
    };
    let at = next_after(cron, tz, now).unwrap_or(now);
    let abbrev = zone
        .offset_from_utc_datetime(&at.naive_utc())
        .abbreviation()
        .unwrap_or(zone.name())
        .to_string();
    let time = format!("{h:02}:{m:02} {abbrev}");
    let days = match dow {
        "*" => return format!("daily at {time}"),
        _ => weekday_field(dow),
    };
    match days.as_deref() {
        Ok("MON,TUE,WED,THU,FRI") => format!("weekdays at {time}"),
        Ok(one) if DAY_NAMES.contains(&one) => {
            let i = DAY_NAMES.iter().position(|d| *d == one).unwrap_or(0);
            format!("every {} at {time}", DAY_TITLES[i])
        }
        _ => fallback(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Timelike, Weekday};
    use std::collections::HashSet;

    const DENVER: &str = "America/Denver";

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn local(tz: &str, t: DateTime<Utc>) -> DateTime<Tz> {
        t.with_timezone(&tz.parse::<Tz>().unwrap())
    }

    /// Fires of `cron` in `tz` across the two weeks starting `from`, as local
    /// weekdays.
    fn weekdays_of(cron: &str, from: &str) -> HashSet<Weekday> {
        next_n(cron, DENVER, utc(from), 10)
            .into_iter()
            .map(|t| local(DENVER, t).weekday())
            .collect()
    }

    #[test]
    fn weekday_numbers_follow_standard_cron() {
        let from = "2026-10-04T00:00:00Z";
        let mon_fri: HashSet<_> = [
            Weekday::Mon,
            Weekday::Tue,
            Weekday::Wed,
            Weekday::Thu,
            Weekday::Fri,
        ]
        .into();
        assert_eq!(weekdays_of("0 9 * * 1-5", from), mon_fri);
        assert_eq!(weekdays_of("0 9 * * MON-FRI", from), mon_fri);
        assert_eq!(weekdays_of("0 9 * * mon-fri", from), mon_fri);
        assert_eq!(weekdays_of("0 9 * * 1,2,3,4,5", from), mon_fri);
        let sunday: HashSet<_> = [Weekday::Sun].into();
        assert_eq!(weekdays_of("0 9 * * 0", from), sunday);
        assert_eq!(weekdays_of("0 9 * * 7", from), sunday);
        assert_eq!(weekdays_of("0 9 * * SUN", from), sunday);
        assert_eq!(
            weekdays_of("0 9 * * 5-7", from),
            [Weekday::Fri, Weekday::Sat, Weekday::Sun].into()
        );
        assert_eq!(
            weekdays_of("0 9 * * FRI-SUN", from),
            [Weekday::Fri, Weekday::Sat, Weekday::Sun].into()
        );
        assert_eq!(
            weekdays_of("0 9 * * */2", from),
            [Weekday::Sun, Weekday::Tue, Weekday::Thu, Weekday::Sat].into()
        );
        assert_eq!(
            weekdays_of("0 9 * * 1-5/2", from),
            [Weekday::Mon, Weekday::Wed, Weekday::Fri].into()
        );
        // Every fire is at 09:00 local.
        for t in next_n("0 9 * * 1-5", DENVER, utc(from), 10) {
            let l = local(DENVER, t);
            assert_eq!((l.hour(), l.minute()), (9, 0));
        }
    }

    #[test]
    fn bad_crons_and_zones_are_typed_errors() {
        for cron in [
            "0 9 * *",
            "0 9 * * 8",
            "61 9 * * *",
            "0 9 * * 5-1",
            "0 9 * * xyz",
        ] {
            assert!(
                matches!(
                    CompiledSchedule::parse(cron, DENVER),
                    Err(ScheduleError::InvalidCron { .. })
                ),
                "{cron}"
            );
        }
        let both = CompiledSchedule::parse("0 9 1 * 1", DENVER).unwrap_err();
        assert_eq!(both, ScheduleError::BothDayFields("0 9 1 * 1".into()));
        let message = both.to_string();
        assert!(
            message.contains("both the day of the month and the day of the week")
                && message.contains("restrict only one"),
            "{message}"
        );
        // A stepped wildcard is unrestricted, as in standard cron.
        assert!(CompiledSchedule::parse("0 9 */2 * 1", DENVER).is_ok());
        assert_eq!(
            CompiledSchedule::parse("0 9 * * *", "Mars/Olympus").unwrap_err(),
            ScheduleError::UnknownZone("Mars/Olympus".into())
        );
        assert_eq!(
            validate_at("0 9 * * *", " ", utc("2026-10-01T00:00:00Z")),
            Err(ScheduleError::MissingZone)
        );
        assert_eq!(
            validate_at("0 9 30 2 *", DENVER, utc("2026-10-01T00:00:00Z")),
            Err(ScheduleError::NeverFires("0 9 30 2 *".into()))
        );
        assert_eq!(next_after("0 9 * *", DENVER, Utc::now()), None);
    }

    #[test]
    fn denver_fall_back_fires_a_repeated_wall_time_once_at_its_first_instant() {
        // 2026-11-01: 01:00-01:59 happens twice (MDT, then MST).
        let fires = next_n("30 1 * * *", DENVER, utc("2026-10-31T12:00:00Z"), 3);
        assert_eq!(fires[0], utc("2026-11-01T07:30:00Z"), "01:30 MDT");
        assert_eq!(fires[1], utc("2026-11-02T08:30:00Z"), "next day 01:30 MST");
        // Starting inside the second 01:xx does not fire 01:30 again.
        assert_eq!(
            next_after("30 1 * * *", DENVER, utc("2026-11-01T08:10:00Z")),
            Some(utc("2026-11-02T08:30:00Z"))
        );
        // A five-minute schedule walks through the repeated hour once.
        let fires = next_n("*/5 * * * *", DENVER, utc("2026-11-01T07:50:00Z"), 4);
        assert_eq!(
            fires,
            vec![
                utc("2026-11-01T07:55:00Z"), // 01:55 MDT
                utc("2026-11-01T09:00:00Z"), // 02:00 MST
                utc("2026-11-01T09:05:00Z"),
                utc("2026-11-01T09:10:00Z"),
            ]
        );
    }

    #[test]
    fn denver_spring_forward_skips_a_missing_wall_time() {
        // 2027-03-14: 02:00-02:59 does not exist.
        let fires = next_n("30 2 * * *", DENVER, utc("2027-03-13T12:00:00Z"), 2);
        assert_eq!(fires[0], utc("2027-03-15T08:30:00Z"), "skipped the 14th");
        assert_eq!(local(DENVER, fires[0]).day(), 15);
        assert_eq!(fires[1], utc("2027-03-16T08:30:00Z"));
    }

    #[test]
    fn weekday_nine_stays_nine_across_both_transitions() {
        for (from, days) in [("2026-10-26T00:00:00Z", 10), ("2027-03-08T00:00:00Z", 10)] {
            for t in next_n("0 9 * * 1-5", DENVER, utc(from), days) {
                let l = local(DENVER, t);
                assert_eq!((l.hour(), l.minute()), (9, 0), "{t}");
                assert!(l.weekday().num_days_from_monday() < 5, "{t}");
            }
        }
        // The UTC instant moves by an hour across the change.
        assert_eq!(
            next_after("0 9 * * 1-5", DENVER, utc("2026-10-30T00:00:00Z")),
            Some(utc("2026-10-30T15:00:00Z"))
        );
        assert_eq!(
            next_after("0 9 * * 1-5", DENVER, utc("2026-11-02T00:00:00Z")),
            Some(utc("2026-11-02T16:00:00Z"))
        );
    }

    #[test]
    fn london_is_a_second_zone() {
        const LONDON: &str = "Europe/London";
        // 2026-10-25 01:00-01:59 repeats; 2027-03-28 01:00-01:59 does not exist.
        assert_eq!(
            next_after("30 1 * * *", LONDON, utc("2026-10-24T12:00:00Z")),
            Some(utc("2026-10-25T00:30:00Z")),
            "01:30 BST, the first instant"
        );
        assert_eq!(
            next_after("30 1 * * *", LONDON, utc("2026-10-25T00:45:00Z")),
            Some(utc("2026-10-26T01:30:00Z"))
        );
        assert_eq!(
            next_after("30 1 * * *", LONDON, utc("2027-03-27T12:00:00Z")),
            Some(utc("2027-03-29T00:30:00Z")),
            "skipped the 28th"
        );
        for t in next_n("0 9 * * 1-5", LONDON, utc("2026-10-20T00:00:00Z"), 10) {
            assert_eq!(local(LONDON, t).hour(), 9);
        }
    }

    #[test]
    fn minimum_interval_is_five_minutes() {
        let now = utc("2026-10-01T00:00:00Z");
        let too_short = Err(ScheduleError::IntervalTooShort { min: MIN_INTERVAL });
        assert_eq!(validate_at("* * * * *", DENVER, now), too_short);
        assert_eq!(validate_at("*/4 * * * *", DENVER, now), too_short);
        assert_eq!(validate_at("0,3 * * * *", DENVER, now), too_short);
        assert_eq!(validate_at("*/5 * * * *", DENVER, now), Ok(()));
        assert_eq!(validate_at("0 9 * * 1-5", DENVER, now), Ok(()));
        assert_eq!(validate_at("0 0 1 1 *", DENVER, now), Ok(()));
        // Across a fall-back the wall clock repeats, but fires never bunch up.
        assert_eq!(
            validate_at("*/5 * * * *", DENVER, utc("2026-11-01T07:00:00Z")),
            Ok(())
        );
        // Accepted or refused the same way whether or not a DST transition is
        // in the sample: 01:58 MST to 03:00 MDT is two real minutes.
        for at in ["2026-10-01T00:00:00Z", "2027-03-10T00:00:00Z"] {
            assert_eq!(
                validate_at("0,58 1,3 * * *", DENVER, utc(at)),
                Ok(()),
                "{at}"
            );
        }
    }

    #[test]
    fn when_presets_normalize_to_cron() {
        let ok = |text: &str| parse_when(text, Some(DENVER)).unwrap().0;
        assert_eq!(ok("every 5m"), "*/5 * * * *");
        assert_eq!(ok("every 30m"), "*/30 * * * *");
        assert_eq!(ok("every 6h"), "0 */6 * * *");
        assert_eq!(ok("every 24h"), "0 */24 * * *");
        assert_eq!(ok("hourly"), "0 * * * *");
        assert_eq!(ok("daily 07:05"), "5 7 * * *");
        assert_eq!(ok("Weekdays 9:00"), "0 9 * * 1-5");
        assert_eq!(ok("weekly Mon 09:30"), "30 9 * * 1");
        assert_eq!(ok("weekly sunday 18:00"), "0 18 * * 0");
        assert_eq!(ok("  0  9 * *   MON-FRI "), "0 9 * * MON-FRI");
        // The words after "every" in the editor's sentence, with or without it.
        assert_eq!(ok("5m"), "*/5 * * * *");
        assert_eq!(ok("2h"), "0 */2 * * *");
        assert_eq!(ok("hour"), "0 * * * *");
        assert_eq!(ok("every hour"), "0 * * * *");
        assert_eq!(ok("day 09:00"), "0 9 * * *");
        assert_eq!(ok("Every day 9:00"), "0 9 * * *");
        assert_eq!(ok("weekday 08:30"), "30 8 * * 1-5");
        assert_eq!(ok("mon 17:00"), "0 17 * * 1");
        assert_eq!(ok("every friday 16:00"), "0 16 * * 5");
        assert_eq!(ok("every 0 9 * * 1-5"), "0 9 * * 1-5");
        assert_eq!(
            parse_when("hourly", Some(" Europe/London ")).unwrap().1,
            "Europe/London"
        );
        assert_eq!(parse_when("hourly", None).unwrap().1, system_zone());

        for bad in [
            "every 7m",
            "every 5h",
            "every 0h",
            "daily 25:00",
            "daily 9",
            "weekly Funday 09:00",
            "whenever",
            "7m",
            "every",
            "funday 09:00",
            "day",
            "",
        ] {
            let err = parse_when(bad, Some(DENVER)).unwrap_err();
            assert_eq!(err, ScheduleError::UnrecognizedWhen(bad.trim().into()));
            assert!(err.to_string().contains("weekdays HH:MM"), "{err}");
        }
        assert!(matches!(
            parse_when("0 9 * * 9", Some(DENVER)),
            Err(ScheduleError::InvalidCron { .. })
        ));
        assert_eq!(
            parse_when("hourly", Some("Nowhere/Land")),
            Err(ScheduleError::UnknownZone("Nowhere/Land".into()))
        );
    }

    #[test]
    fn every_preset_passes_the_save_check() {
        let now = utc("2026-10-01T00:00:00Z");
        for text in [
            "every 5m",
            "every 10m",
            "every 15m",
            "every 20m",
            "every 30m",
            "every 1h",
            "every 8h",
            "hourly",
            "daily 00:00",
            "weekdays 23:59",
            "weekly Sat 12:00",
        ] {
            let (cron, tz) = parse_when(text, Some(DENVER)).unwrap();
            assert_eq!(validate_at(&cron, &tz, now), Ok(()), "{text}");
        }
    }

    #[test]
    fn describe_renders_the_common_forms() {
        let summer = utc("2026-07-01T00:00:00Z");
        let winter = utc("2026-12-01T00:00:00Z");
        assert_eq!(
            describe_at("*/15 * * * *", DENVER, summer),
            "every 15 minutes"
        );
        assert_eq!(describe_at("0 */6 * * *", DENVER, summer), "every 6 hours");
        assert_eq!(describe_at("0 * * * *", DENVER, summer), "every hour");
        assert_eq!(
            describe_at("5 7 * * *", DENVER, summer),
            "daily at 07:05 MDT"
        );
        assert_eq!(
            describe_at("5 7 * * *", DENVER, winter),
            "daily at 07:05 MST"
        );
        assert_eq!(
            describe_at("0 9 * * 1-5", DENVER, summer),
            "weekdays at 09:00 MDT"
        );
        assert_eq!(
            describe_at("0 9 * * MON-FRI", "Europe/London", winter),
            "weekdays at 09:00 GMT"
        );
        assert_eq!(
            describe_at("30 18 * * 0", DENVER, summer),
            "every Sunday at 18:30 MDT"
        );
        assert_eq!(
            describe_at("30 18 * * 7", DENVER, summer),
            "every Sunday at 18:30 MDT"
        );
        assert_eq!(
            describe_at("0 9 * * Wed", DENVER, summer),
            "every Wednesday at 09:00 MDT"
        );
        for other in ["0 9 1 * *", "0 9 * * 1,3", "0 9-17 * * *", "bad"] {
            assert_eq!(
                describe_at(other, DENVER, summer),
                format!("cron `{other}` ({DENVER})")
            );
        }
        // The describe/parse pair round-trips the presets.
        let (cron, tz) = parse_when("weekly Fri 16:00", Some(DENVER)).unwrap();
        assert_eq!(describe_at(&cron, &tz, summer), "every Friday at 16:00 MDT");
    }
}
