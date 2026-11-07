// SPDX-License-Identifier: MIT
//! Cron in an IANA zone, evaluated on the host's own clock with the tzdb
//! compiled into this binary (no zone files on the machine are read). The
//! same wall-clock rules as the app's routine schedules:
//!
//! - a wall time that occurs twice (fall back) fires once, at its first instant;
//! - a wall time that does not exist (spring forward) is skipped that day.
//!
//! The `cron` crate walks the naive wall clock (its `Utc` stands in for naive
//! local time); every naive result is then placed in the zone here. It numbers
//! weekdays 1 = Sunday, so the standard weekday field (0 or 7 = Sunday) is
//! rewritten to day names first.

use std::collections::BTreeSet;
use std::str::FromStr;

use chrono::{DateTime, LocalResult, TimeZone, Utc};
use chrono_tz::Tz;

/// Bounds the naive-time walk of one `next_after`.
const MAX_CANDIDATES: usize = 1000;
const DAY_NAMES: [&str; 7] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];

/// A parsed cron and zone.
#[derive(Debug, Clone)]
pub(crate) struct CronSchedule {
    wall: cron::Schedule,
    tz: Tz,
}

impl CronSchedule {
    pub(crate) fn parse(cron: &str, tz: &str) -> Result<Self, String> {
        let tz: Tz = tz
            .trim()
            .parse()
            .map_err(|_| format!("tz {tz:?} is not an IANA time zone (like America/Denver)"))?;
        Ok(Self {
            wall: compile(cron)?,
            tz,
        })
    }

    /// The first fire strictly after `after_ms` (host clock, ms since epoch).
    pub(crate) fn next_after_ms(&self, after_ms: u64) -> Option<u64> {
        let after = Utc
            .timestamp_millis_opt(i64::try_from(after_ms).ok()?)
            .single()?;
        self.next_after(after)
            .and_then(|at| u64::try_from(at.timestamp_millis()).ok())
    }

    fn next_after(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
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
}

/// A user's 5-field cron as the crate's schedule (seconds prepended, the
/// weekday field rewritten to names).
fn compile(cron: &str) -> Result<cron::Schedule, String> {
    let invalid = |why: String| format!("cron {cron:?} is not valid: {why}");
    let fields: Vec<&str> = cron.split_whitespace().collect();
    let [minute, hour, dom, month, dow] = fields[..] else {
        return Err(invalid(
            "it must have 5 fields (minute hour day-of-month month day-of-week)".into(),
        ));
    };
    if !dom.starts_with(['*', '?']) && !dow.starts_with(['*', '?']) {
        return Err(invalid(
            "restrict only one of day-of-month and day-of-week".into(),
        ));
    }
    let dow = weekday_field(dow).map_err(invalid)?;
    cron::Schedule::from_str(&format!("0 {minute} {hour} {dom} {month} {dow}"))
        .map_err(|e| invalid(e.to_string()))
}

fn weekday_field(field: &str) -> Result<String, String> {
    if field == "*" || field == "?" {
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
                0 if lo > 0 => 7,
                hi => hi,
            };
            (lo, hi)
        } else {
            let day = weekday(base)?;
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

fn weekday(token: &str) -> Result<u8, String> {
    if let Ok(n) = token.parse::<u8>() {
        return if n <= 7 {
            Ok(n)
        } else {
            Err(format!("weekday {n} is out of range 0-7"))
        };
    }
    let lower = token.to_ascii_lowercase();
    DAY_NAMES
        .iter()
        .position(|name| name.eq_ignore_ascii_case(&lower))
        .and_then(|i| u8::try_from(i).ok())
        .ok_or_else(|| format!("{token:?} is not a weekday"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(rfc3339: &str) -> u64 {
        DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .timestamp_millis() as u64
    }

    #[test]
    fn nine_am_denver_follows_dst_in_both_directions() {
        let schedule = CronSchedule::parse("0 9 * * *", "America/Denver").unwrap();
        // MDT before the fall-back of 2026-11-01, MST after.
        let mdt = schedule.next_after_ms(ms("2026-10-30T16:00:00Z")).unwrap();
        assert_eq!(mdt, ms("2026-10-31T15:00:00Z"));
        let mst = schedule.next_after_ms(mdt).unwrap();
        assert_eq!(
            mst,
            ms("2026-11-01T16:00:00Z"),
            "25 hours later on the wall"
        );
        assert_eq!(
            schedule.next_after_ms(mst).unwrap(),
            ms("2026-11-02T16:00:00Z")
        );
        // Spring forward 2027-03-14: 09:00 MDT is 15:00Z again.
        let spring = schedule.next_after_ms(ms("2027-03-14T00:00:00Z")).unwrap();
        assert_eq!(spring, ms("2027-03-14T15:00:00Z"));
    }

    #[test]
    fn a_skipped_wall_time_is_skipped_and_a_repeated_one_fires_once() {
        // 02:30 does not exist in Denver on 2027-03-14.
        let skipped = CronSchedule::parse("30 2 * * *", "America/Denver").unwrap();
        let next = skipped.next_after_ms(ms("2027-03-14T07:00:00Z")).unwrap();
        assert_eq!(next, ms("2027-03-15T08:30:00Z"));
        // 01:30 happens twice on 2026-11-01: only the first (MDT) instant.
        let repeated = CronSchedule::parse("30 1 * * *", "America/Denver").unwrap();
        let first = repeated.next_after_ms(ms("2026-11-01T06:00:00Z")).unwrap();
        assert_eq!(first, ms("2026-11-01T07:30:00Z"));
        let second = repeated.next_after_ms(first).unwrap();
        assert_eq!(second, ms("2026-11-02T08:30:00Z"));
    }

    #[test]
    fn invalid_zone_and_cron_are_refused() {
        assert!(CronSchedule::parse("0 9 * * *", "Mars/Olympus").is_err());
        assert!(CronSchedule::parse("0 9 * *", "UTC").is_err());
        assert!(CronSchedule::parse("0 9 1 * 1", "UTC").is_err());
        assert!(CronSchedule::parse("0 9 * * 1-5", "Europe/Berlin").is_ok());
    }
}
